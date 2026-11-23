//! Cleaning up what an identity brought in (the 接入 page's “清理记录”): the spaces seen only
//! through one connection (or through a disconnected one, by the account it was on), their
//! messages, and the contacts `identify` made for their members — without breaking anything that
//! points at them.
//!
//! | route | |
//! | --- | --- |
//! | `POST /channels/cleanup/preview` | `{connection_id? \| account_id?, level: "text"\|"all"}` → [`Summary`] (pure read) |
//! | `POST /channels/cleanup` | the same plus `confirm: true` → [`Summary`] with `backup` (DB backup first, one transaction, audit `channel.cleanup`) |
//! | `GET /channels/cleanup/leftovers` | `[{account_id, name, system_id, spaces, messages}]`: disconnected identities that still have spaces |
//!
//! Levels:
//! - `text` (删除消息正文): message text → NULL, attachments → `[]`, `purged = now`; the raw
//!   payload of their `ext_events` → NULL; output-intent content → `{}` (fingerprints and
//!   receipts stay). Rows, summaries and links stay, so items and decisions keep working.
//! - `all` (删除这个接入带来的群、消息和自动识别出的联系人): the spaces and everything that only
//!   exists for them (members, settings, bindings, messages, reply basis, route logs, signals), and
//!   the contacts `identify` created for their people that nobody confirmed, edited or used.
//!
//! Kept, always, with the reason in `kept`: a message an item, decision, thread, memory or link
//! refers to (its text too), except standalone channel decisions removed together in `all`; a space with such a message, or one a memory, knowledge doc, team,
//! 负责人, link or an item's decision refers to (its other messages lose their text); a contact you
//! confirmed or edited, “我”, one with accounts from elsewhere, seen in other spaces or mail, or
//! referenced by items, memories, knowledge, 负责人, teams, merges or merge proposals.
//!
//! “Only seen through this identity” (one space per chat, seen by one or more identities:
//! `space_identities`, migration 019): a space another live identity still sees is left alone, and
//! deleting one removes its identities, attention and voice (`space_prefs`) with it.

use std::{path::PathBuf, sync::Arc};

use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::json;
use steve_modules::{ApiError, ApiResult, Body, Caller, Modules};
use steve_store::base::Viewer;
use steve_store::channel_spaces::cleanup as repo;
pub use steve_store::channel_spaces::cleanup::selection::Leftover;
use steve_store::channel_spaces::cleanup::selection::{self, Keep};
use steve_store::{Accounts, Read};

use crate::{ChannelResult, Channels};

/// The host must finish a durable backup before cleanup is allowed to mutate data.
/// The returned location is opaque to channels and is echoed in the response/audit.
pub trait PreDestructiveBackup: Send + Sync {
    fn backup(&self, what: &str) -> ChannelResult<PathBuf>;
}

/// What to clean.
#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Request {
    #[serde(default)]
    pub connection_id: Option<String>,
    #[serde(default)]
    pub account_id: Option<String>,
    pub level: Level,
    #[serde(default)]
    pub confirm: bool,
    #[serde(default)]
    pub decision_ids: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    /// 删除消息正文.
    Text,
    /// 删除这个接入带来的群、消息和自动识别出的联系人.
    All,
}

/// One thing that stays, and why (Chinese).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Kept {
    /// `space` | `message` | `contact`.
    pub kind: &'static str,
    pub id: String,
    pub name: String,
    pub reason: String,
}

/// What a cleanup removes (preview) or removed.
#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    pub level: Level,
    /// The identity's name (account display name or handle).
    pub name: String,
    /// `all`: spaces deleted; `text`: spaces whose messages lose their text.
    pub spaces: usize,
    /// `all`: messages deleted; `text`: messages whose text is removed.
    pub messages: usize,
    /// `all`: messages of kept spaces whose text is removed (0 for `text`).
    pub messages_purged: usize,
    /// Contacts deleted (`all` only).
    pub contacts: usize,
    /// Accounts deleted with them (`all` only).
    pub accounts: usize,
    /// Standalone decisions removed together with their channel records (`all` only).
    pub decisions: usize,
    /// Echo these IDs on execution; a changed set requires a fresh preview.
    pub decision_ids: Vec<String>,
    /// At most 200 of the kept things; `kept_count` is all of them.
    pub kept: Vec<Kept>,
    pub kept_count: usize,
    /// The backup file name taken before a real cleanup (`pre-purge-<UTC>-<id>/<database>`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backup: Option<String>,
}

/// The routes, relative to `/api`, behind the `Caller` layer.
pub fn routes(ch: Channels) -> Router<Modules> {
    Router::new()
        .route("/channels/cleanup/preview", post(preview_route))
        .route("/channels/cleanup", post(run_route))
        .route("/channels/cleanup/leftovers", get(leftovers_route))
        .layer(Extension(ch))
}

async fn preview_route(
    Extension(ch): Extension<Channels>,
    who: Caller,
    Body(r): Body<Request>,
) -> ApiResult<Json<Summary>> {
    let v = ch.modules().viewer(&who)?;
    let t = target(&ch, &who, &r)?;
    Ok(Json(ch.store().read(|rdb| -> ApiResult<Summary> {
        Ok(plan(rdb, &v, &t, r.level)?.summary(r.level, &t.name))
    })?))
}

async fn run_route(
    Extension(ch): Extension<Channels>,
    backup: Option<Extension<Arc<dyn PreDestructiveBackup>>>,
    who: Caller,
    Body(r): Body<Request>,
) -> ApiResult<Json<Summary>> {
    if !r.confirm {
        return Err(ApiError::rule(steve_i18n::t!(
            "channels.error.cleanup_confirm"
        )));
    }
    let v = ch.modules().viewer(&who)?;
    let t = target(&ch, &who, &r)?;
    Ok(Json(
        run(
            &ch,
            &v,
            &t,
            r.level,
            r.decision_ids,
            backup.map(|Extension(port)| port),
        )
        .await?,
    ))
}

async fn leftovers_route(
    Extension(ch): Extension<Channels>,
    who: Caller,
) -> ApiResult<Json<Vec<Leftover>>> {
    let v = ch.modules().viewer(&who)?;
    Ok(Json(ch.store().read(|r| leftovers(r, &v))?))
}

// ---------------------------------------------------------------- target

/// Whose spaces: a live connection, or the account a disconnected one was on.
#[derive(Debug, Clone)]
pub struct Target {
    pub connection_id: Option<String>,
    pub account_id: String,
    pub name: String,
}

fn target(ch: &Channels, who: &Caller, r: &Request) -> ApiResult<Target> {
    let ws = who.ws.clone();
    match (r.connection_id.as_deref(), r.account_id.as_deref()) {
        (Some(con), _) if !con.is_empty() => {
            let c = ch
                .connections()
                .get(who, con)
                .map_err(|_| ApiError::not_found("连接"))?;
            let name = ch.store().read(|db| account_name(db, &ws, &c.account_id))?;
            Ok(Target {
                connection_id: Some(c.id),
                account_id: c.account_id,
                name: name.unwrap_or_default(),
            })
        }
        (_, Some(acc)) if !acc.is_empty() => {
            let name = ch
                .store()
                .read(|db| account_name(db, &ws, acc))?
                .ok_or_else(|| ApiError::not_found("账号"))?;
            Ok(Target {
                connection_id: None,
                account_id: acc.to_string(),
                name,
            })
        }
        _ => Err(ApiError::invalid("connection_id")),
    }
}

fn account_name(r: &mut impl Read, ws: &str, id: &str) -> ChannelResult<Option<String>> {
    Ok(Accounts(r)
        .get(id)?
        .filter(|a| a.workspace_id == ws)
        .map(|a| {
            if !a.display_name.is_empty() {
                a.display_name
            } else if !a.handle_display.is_empty() {
                a.handle_display
            } else {
                a.handle
            }
        }))
}

// ---------------------------------------------------------------- the plan

#[derive(Debug, Default)]
struct Plan {
    /// Spaces deleted (`all`) or purged (`text`).
    spaces: Vec<String>,
    /// Messages deleted (`all`) or purged (`text`).
    messages: Vec<String>,
    /// `all`: messages of kept spaces whose text goes.
    purge_in_kept: Vec<String>,
    contacts: Vec<String>,
    accounts: Vec<String>,
    decisions: Vec<String>,
    kept: Vec<Kept>,
}

impl Plan {
    fn summary(&self, level: Level, name: &str) -> Summary {
        Summary {
            level,
            name: name.to_string(),
            spaces: self.spaces.len(),
            messages: self.messages.len(),
            messages_purged: self.purge_in_kept.len(),
            contacts: self.contacts.len(),
            accounts: self.accounts.len(),
            decisions: self.decisions.len(),
            decision_ids: self.decisions.clone(),
            kept: self.kept.iter().take(200).cloned().collect(),
            kept_count: self.kept.len(),
            backup: None,
        }
    }
}

fn reason(kind: &str, why: Keep) -> &'static str {
    match (kind, why) {
        (_, Keep::Item) => "被事项引用",
        (_, Keep::Decision) => "有相关的决定",
        ("message", Keep::Memory) => "记忆引用了它",
        ("contact", Keep::Memory) => "有关于他的记忆",
        (_, Keep::Memory) => "有关于它的记忆",
        (_, Keep::Link) => "被其他对象关联",
        ("contact", Keep::Knowledge) => "知识库引用了他",
        (_, Keep::Knowledge) => "知识库引用了它",
        (_, Keep::TeamSpace) => "是一个团队的群",
        ("contact", Keep::Steward) => "是负责人或设了负责人",
        (_, Keep::Steward) => "设了负责人",
        (_, Keep::Confirmed) => "你确认过",
        (_, Keep::Inactive) => "已合并或归档",
        (_, Keep::Edited) => "你改过",
        (_, Keep::OtherAccount) => "有别的来源的账号",
        (_, Keep::Merge) => "合并过",
        (_, Keep::MergeProposal) => "有合并建议",
        (_, Keep::TeamMember) => "在团队里",
        (_, Keep::OtherMembership) => "在别的群里",
        (_, Keep::OtherMessage) => "在别的群里说过话",
        (_, Keep::OtherSignal) => "在别处出现过",
        (_, Keep::Mail) => "有邮件往来",
        (_, Keep::Hidden) => "",
    }
}

fn plan(r: &mut impl Read, v: &Viewer, t: &Target, level: Level) -> ChannelResult<Plan> {
    let mut p = Plan::default();
    let target = match &t.connection_id {
        Some(conn) => selection::Target::Connection(conn),
        None => selection::Target::Account(&t.account_id),
    };
    let list = selection::spaces(r, &v.ws, target)?;
    for (sid, name) in &list {
        let decisions = if level == Level::All {
            selection::removable_decisions(r, &v.ws, sid)?
        } else {
            vec![]
        };
        let msgs = selection::messages(r, sid, level == Level::Text, &decisions)?;
        let refd: Vec<&(String, Option<Keep>)> = msgs.iter().filter(|m| m.1.is_some()).collect();
        for (mid, why) in &refd {
            p.kept.push(Kept {
                kind: "message",
                id: mid.clone(),
                name: name.clone(),
                reason: why
                    .map(|why| reason("message", why))
                    .unwrap_or_default()
                    .into(),
            });
        }
        let free = msgs.iter().filter(|m| m.1.is_none()).map(|m| m.0.clone());
        match level {
            Level::Text => {
                let free: Vec<String> = free.collect();
                if !free.is_empty() {
                    p.spaces.push(sid.clone());
                    p.messages.extend(free);
                }
            }
            Level::All => {
                let protected = selection::protected_space_decision(r, sid, &decisions)?;
                let why = selection::space_reference(r, sid)?
                    .map(|why| reason("space", why))
                    .or_else(|| protected.then_some("决定仍被其他对象引用"))
                    .or_else(|| (!refd.is_empty()).then_some("里面有被引用的消息"));
                match why {
                    Some(reason) => {
                        p.kept.push(Kept {
                            kind: "space",
                            id: sid.clone(),
                            name: name.clone(),
                            reason: reason.into(),
                        });
                        let text = selection::messages(r, sid, true, &[])?;
                        p.purge_in_kept
                            .extend(text.into_iter().filter(|m| m.1.is_none()).map(|m| m.0));
                    }
                    None => {
                        p.spaces.push(sid.clone());
                        p.messages.extend(free);
                        p.decisions.extend(decisions);
                    }
                }
            }
        }
    }
    if level == Level::All && !p.spaces.is_empty() {
        for pid in selection::contact_ids(r, &p.spaces)? {
            let Some(contact) = selection::contact(r, &pid, &p.spaces, &t.account_id)? else {
                continue;
            };
            match contact.reason {
                Some(Keep::Hidden) => {}
                Some(why) => p.kept.push(Kept {
                    kind: "contact",
                    id: contact.id,
                    name: contact.name,
                    reason: reason("contact", why).into(),
                }),
                None => {
                    p.accounts.extend(selection::account_ids(r, &contact.id)?);
                    p.contacts.push(contact.id);
                }
            }
        }
    }
    Ok(p)
}

/// Backup, then the cleanup in one transaction, audited.
pub async fn run(
    ch: &Channels,
    v: &Viewer,
    t: &Target,
    level: Level,
    decision_ids: Vec<String>,
    backup: Option<Arc<dyn PreDestructiveBackup>>,
) -> ApiResult<Summary> {
    let (ch2, v2, t2) = (ch.clone(), v.clone(), t.clone());
    tokio::task::spawn_blocking(steve_i18n::carry(move || -> ApiResult<Summary> {
        let (ch, v, t) = (ch2, v2, t2);
        // fail before the backup when there is nothing to clean
        ch.store().read(|r| plan(r, &v, &t, level).map(|_| ()))?;
        let now = ch.now();
        let backup = backup.ok_or_else(|| ApiError::Internal("清理前备份能力未配置".into()))?;
        let file = backup.backup("purge")?.to_string_lossy().into_owned();
        let mut out = ch.store().write(|tx| -> ApiResult<Summary> {
            let p = plan(tx, &v, &t, level)?;
            let mut expected = decision_ids.clone(); expected.sort(); expected.dedup();
            let mut actual = p.decisions.clone(); actual.sort();
            if expected != actual {
                return Err(ApiError::rule(steve_i18n::t!("channels.error.cleanup_changed")));
            }
            match level {
                Level::Text => repo::purge_text(tx, &p.messages, now)?,
                Level::All => repo::delete(tx, repo::Deletion {
                    workspace: &v.ws, decisions: &p.decisions, spaces: &p.spaces, messages: &p.messages,
                    parties: &p.contacts, accounts: &p.accounts, messages_to_purge: &p.purge_in_kept,
                }, now)?,
            }
            let s = p.summary(level, &t.name);
            steve_store::audit_repo::record(
                tx,
                &v.ws,
                &v.user,
                "channel.cleanup",
                t.connection_id.as_deref().unwrap_or(&t.account_id),
                &json!({
                    "level": level, "account": t.account_id, "spaces": s.spaces,
                    "messages": s.messages, "messages_purged": s.messages_purged,
                    "decisions": s.decisions, "contacts": s.contacts, "accounts": s.accounts, "kept": s.kept_count,
                    "backup": file,
                }),
            )?;
            for id in p.spaces.iter().chain(p.kept.iter().filter(|k| k.kind == "space").map(|k| &k.id)) {
                tx.emit(steve_types::ChannelsEvent::Space { ws: v.ws.clone().into(), id: id.as_str().into() });
            }
            Ok(s)
        })?;
        out.backup = Some(file);
        tracing::info!(level = ?level, spaces = out.spaces, messages = out.messages, contacts = out.contacts, "channels: cleanup done");
        Ok(out)
    }))
    .await
    .map_err(|_| ApiError::Internal("任务失败".into()))?
}

/// Disconnected identities that still have spaces the viewer sees.
pub fn leftovers(r: &mut impl Read, v: &Viewer) -> ChannelResult<Vec<Leftover>> {
    Ok(selection::leftovers(r, &v.ws)?)
}
