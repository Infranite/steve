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
//!   payload of their `ext_events` → NULL. Rows, summaries and links stay, so items and decisions
//!   keep working.
//! - `all` (删除这个接入带来的群、消息和自动识别出的联系人): the spaces and everything that only
//!   exists for them (members, settings, bindings, messages, reply basis, route logs, signals), and
//!   the contacts `identify` created for their people that nobody confirmed, edited or used.
//!
//! Kept, always, with the reason in `kept`: a message an item, decision, thread, memory or link
//! refers to (its text too); a space with such a message, or one a memory, knowledge doc, team,
//! 负责人, link or an item's decision refers to (its other messages lose their text); a contact you
//! confirmed or edited, “我”, one with accounts from elsewhere, seen in other spaces or mail, or
//! referenced by items, memories, knowledge, 负责人, teams, merges or merge proposals.
//!
//! “Only seen through this identity” (one space per chat, seen by one or more identities:
//! `space_identities`, migration 019): a space another live identity still sees is left alone, and
//! deleting one removes its identities, attention and voice (`space_prefs`) with it.

use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::json;
use steve_modules::{ApiError, ApiResult, Body, Caller, Modules};
use steve_store::base::{self, Viewer};

use crate::{ChannelResult, Channels, spaces};

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
    /// At most 200 of the kept things; `kept_count` is all of them.
    pub kept: Vec<Kept>,
    pub kept_count: usize,
    /// The backup file name taken before a real cleanup (`pre-purge-<ms>.sqlite3`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backup: Option<String>,
}

/// A disconnected identity that still has spaces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Leftover {
    pub account_id: String,
    pub name: String,
    pub system_id: String,
    pub spaces: i64,
    pub messages: i64,
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
    Ok(Json(ch.store().reader(|c| -> ApiResult<Summary> {
        Ok(plan(c, &v, &t, r.level)?.summary(r.level, &t.name))
    })?))
}

async fn run_route(
    Extension(ch): Extension<Channels>,
    who: Caller,
    Body(r): Body<Request>,
) -> ApiResult<Json<Summary>> {
    if !r.confirm {
        return Err(ApiError::rule("清理要确认：请求里带上 confirm: true"));
    }
    let v = ch.modules().viewer(&who)?;
    let t = target(&ch, &who, &r)?;
    Ok(Json(run(&ch, &v, &t, r.level).await?))
}

async fn leftovers_route(
    Extension(ch): Extension<Channels>,
    who: Caller,
) -> ApiResult<Json<Vec<Leftover>>> {
    let v = ch.modules().viewer(&who)?;
    Ok(Json(ch.store().reader(|c| leftovers(c, &v))?))
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
            let name = ch
                .store()
                .reader(|db| account_name(db, &ws, &c.account_id))?;
            Ok(Target {
                connection_id: Some(c.id),
                account_id: c.account_id,
                name: name.unwrap_or_default(),
            })
        }
        (_, Some(acc)) if !acc.is_empty() => {
            let name = ch
                .store()
                .reader(|db| account_name(db, &ws, acc))?
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

fn account_name(c: &Connection, ws: &str, id: &str) -> ChannelResult<Option<String>> {
    Ok(c.query_row(
        "SELECT CASE WHEN display_name <> '' THEN display_name WHEN handle_display <> '' THEN handle_display ELSE handle END
           FROM accounts WHERE id = ? AND workspace_id = ?",
        params![id, ws],
        |r| r.get(0),
    )
    .optional()?)
}

/// A live identity other than `?4` still sees space `s` (`space_identities`, migration 019).
const SEEN_BY_OTHERS: &str =
    "EXISTS (SELECT 1 FROM space_identities oi JOIN connections oc ON oc.id = oi.connection_id
      WHERE oi.space_id = s.id AND oi.left_at IS NULL AND oi.connection_id IS NOT ?4)";

/// The spaces of the target the viewer sees, only seen through it: `(id, name)`. A connection's
/// spaces are the ones it is an identity of (`space_identities`; a row from before 019 without
/// any: the one it was first seen through); a disconnected account's are the left spaces that
/// remember it (`spaces.account_id`, set when its last identity disconnected). Either way a space
/// another live identity still sees is left alone.
fn target_spaces(c: &Connection, v: &Viewer, t: &Target) -> ChannelResult<Vec<(String, String)>> {
    let own = match &t.connection_id {
        Some(_) => {
            "(EXISTS (SELECT 1 FROM space_identities ti WHERE ti.space_id = s.id AND ti.connection_id = ?3)
              OR (s.connection_id = ?3 AND NOT EXISTS (SELECT 1 FROM space_identities ti WHERE ti.space_id = s.id)))"
        }
        None => "s.connection_id IS NULL AND s.account_id = ?3",
    };
    let key = t
        .connection_id
        .clone()
        .unwrap_or_else(|| t.account_id.clone());
    let sql = format!(
        "SELECT s.id, s.name FROM spaces s WHERE s.workspace_id = ?1 AND {own} AND {} AND NOT {SEEN_BY_OTHERS} ORDER BY s.id",
        spaces::visible_sql("?2")
    );
    let mut st = c.prepare(&sql)?;
    let rows = st
        .query_map(params![v.ws, v.user, key, t.connection_id], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

// ---------------------------------------------------------------- references

/// Why message `m` must stay, or NULL.
const MESSAGE_REF: &str = "CASE
  WHEN m.work_item_id IS NOT NULL THEN '被事项引用'
  WHEN m.decision_id IS NOT NULL THEN '有相关的决定'
  WHEN EXISTS (SELECT 1 FROM ext_events e WHERE e.message_id = m.id AND e.work_item_id IS NOT NULL) THEN '被事项引用'
  WHEN EXISTS (SELECT 1 FROM ext_events e WHERE e.message_id = m.id AND e.decision_id IS NOT NULL) THEN '有相关的决定'
  WHEN EXISTS (SELECT 1 FROM thread_items t WHERE t.origin_msg = m.id OR t.reply_msg = m.id) THEN '被事项引用'
  WHEN EXISTS (SELECT 1 FROM decisions d WHERE d.subject_type = 'chan_message' AND d.subject_id = m.id) THEN '有相关的决定'
  WHEN EXISTS (SELECT 1 FROM mems x WHERE x.source_type = 'chan_message' AND x.source_id = m.id) THEN '记忆引用了它'
  WHEN EXISTS (SELECT 1 FROM links l WHERE l.workspace_id = m.workspace_id
        AND ((l.subject_type = 'chan_message' AND l.subject_id = m.id) OR (l.object_type = 'chan_message' AND l.object_id = m.id))) THEN '被其他对象关联'
  END";

/// Why space `s` must stay regardless of its messages, or NULL.
const SPACE_REF: &str = "CASE
  WHEN EXISTS (SELECT 1 FROM mem_about a WHERE a.workspace_id = s.workspace_id AND a.object_type = 'space' AND a.object_id = s.id) THEN '有关于它的记忆'
  WHEN EXISTS (SELECT 1 FROM mem_candidates a WHERE a.origin = 'space' AND a.origin_id = s.id) THEN '有关于它的记忆'
  WHEN EXISTS (SELECT 1 FROM kb_doc_about a WHERE a.workspace_id = s.workspace_id AND a.object_type = 'space' AND a.object_id = s.id) THEN '知识库引用了它'
  WHEN EXISTS (SELECT 1 FROM team_details t WHERE t.space_id = s.id) THEN '是一个团队的群'
  WHEN EXISTS (SELECT 1 FROM stewards w WHERE w.workspace_id = s.workspace_id AND w.object_type = 'space' AND w.object_id = s.id) THEN '设了负责人'
  WHEN EXISTS (SELECT 1 FROM decisions d WHERE d.subject_type = 'space' AND d.subject_id = s.id AND d.item_id IS NOT NULL) THEN '被事项引用'
  WHEN EXISTS (SELECT 1 FROM links l WHERE l.workspace_id = s.workspace_id
        AND ((l.subject_type = 'space' AND l.subject_id = s.id) OR (l.object_type = 'space' AND l.object_id = s.id))) THEN '被其他对象关联'
  END";

/// Why a contact `p` must stay, or NULL (`?1`: the ids of the spaces being deleted, as a JSON
/// array; `?3`: the target's account). `-`: not listed at all (“我”, the identity's own account,
/// Steve's own bots).
const CONTACT_REF: &str = "CASE
  WHEN p.user_id IS NOT NULL OR EXISTS (SELECT 1 FROM accounts a WHERE a.party_id = p.id AND a.id = ?3)
    OR (p.contact_kind = 'bot' AND p.source <> 'identify') THEN '-'
  WHEN p.source <> 'identify' OR coalesce(p.contact_kind, '') NOT IN ('unconfirmed', 'bot') THEN '你确认过'
  WHEN p.status <> 'active' THEN '已合并或归档'
  WHEN p.revision > 1
    OR EXISTS (SELECT 1 FROM revisions r WHERE r.object_type = 'party' AND r.object_id = p.id)
    OR EXISTS (SELECT 1 FROM changesets x WHERE x.target_id = p.id AND x.status = 'applied')
    OR EXISTS (SELECT 1 FROM audit u WHERE u.target = p.id AND u.action <> 'party.create')
    OR EXISTS (SELECT 1 FROM attr_values av WHERE av.party_id = p.id) THEN '你改过'
  WHEN EXISTS (SELECT 1 FROM accounts a WHERE a.party_id = p.id AND a.source <> 'identify') THEN '有别的来源的账号'
  WHEN EXISTS (SELECT 1 FROM merges g WHERE g.from_id = p.id OR g.into_id = p.id) THEN '合并过'
  WHEN EXISTS (SELECT 1 FROM merge_proposals g WHERE g.from_id = p.id OR g.into_id = p.id) THEN '有合并建议'
  WHEN EXISTS (SELECT 1 FROM stewards w WHERE w.person_id = p.id OR (w.object_type = 'party' AND w.object_id = p.id)) THEN '是负责人或设了负责人'
  WHEN EXISTS (SELECT 1 FROM team_members tm WHERE tm.contact_id = p.id OR tm.team_id = p.id) THEN '在团队里'
  WHEN EXISTS (SELECT 1 FROM mem_about a WHERE a.object_type = 'party' AND a.object_id = p.id) THEN '有关于他的记忆'
  WHEN EXISTS (SELECT 1 FROM kb_doc_about a WHERE a.object_type = 'party' AND a.object_id = p.id) THEN '知识库引用了他'
  WHEN EXISTS (SELECT 1 FROM links l WHERE l.workspace_id = p.workspace_id AND l.source <> 'infer'
        AND ((l.subject_type = 'party' AND l.subject_id = p.id) OR (l.object_type = 'party' AND l.object_id = p.id))) THEN '被其他对象关联'
  WHEN EXISTS (SELECT 1 FROM space_members sm JOIN accounts a ON a.id = sm.account_id
        WHERE a.party_id = p.id AND sm.space_id NOT IN (SELECT value FROM json_each(?1))) THEN '在别的群里'
  WHEN EXISTS (SELECT 1 FROM chan_messages cm WHERE cm.sender_party_id = p.id
        AND cm.space_id NOT IN (SELECT value FROM json_each(?1))) THEN '在别的群里说过话'
  WHEN EXISTS (SELECT 1 FROM ext_events e WHERE e.actor_party_id = p.id
        AND (e.space_id IS NULL OR e.space_id NOT IN (SELECT value FROM json_each(?1)))) THEN '在别处出现过'
  WHEN EXISTS (SELECT 1 FROM mail_messages mm WHERE mm.from_party_id = p.id) THEN '有邮件往来'
  END";

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
            kept: self.kept.iter().take(200).cloned().collect(),
            kept_count: self.kept.len(),
            backup: None,
        }
    }
}

fn strings(c: &Connection, sql: &str, p: impl rusqlite::Params) -> ChannelResult<Vec<String>> {
    let mut st = c.prepare(sql)?;
    let rows = st
        .query_map(p, |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Messages of space `sid`: `(id, reason to keep)`; for `text`, only those that still have text or
/// attachments.
fn messages_of(
    c: &Connection,
    sid: &str,
    only_unpurged: bool,
) -> ChannelResult<Vec<(String, Option<String>)>> {
    let filter = if only_unpurged {
        "AND (m.text IS NOT NULL OR m.attachments <> '[]')"
    } else {
        ""
    };
    let mut st = c.prepare(&format!(
        "SELECT m.id, {MESSAGE_REF} FROM chan_messages m WHERE m.space_id = ? {filter} ORDER BY m.created, m.id"
    ))?;
    let rows = st
        .query_map([sid], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

fn plan(c: &Connection, v: &Viewer, t: &Target, level: Level) -> ChannelResult<Plan> {
    let mut p = Plan::default();
    let list = target_spaces(c, v, t)?;
    for (sid, name) in &list {
        let msgs = messages_of(c, sid, level == Level::Text)?;
        let refd: Vec<&(String, Option<String>)> = msgs.iter().filter(|m| m.1.is_some()).collect();
        for (mid, why) in &refd {
            p.kept.push(Kept {
                kind: "message",
                id: mid.clone(),
                name: name.clone(),
                reason: why.clone().unwrap_or_default(),
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
                let why: Option<String> = c.query_row(
                    &format!("SELECT {SPACE_REF} FROM spaces s WHERE s.id = ?"),
                    [sid],
                    |r| r.get(0),
                )?;
                let why =
                    why.or_else(|| (!refd.is_empty()).then(|| "里面有被引用的消息".to_string()));
                match why {
                    Some(reason) => {
                        p.kept.push(Kept {
                            kind: "space",
                            id: sid.clone(),
                            name: name.clone(),
                            reason,
                        });
                        let text = messages_of(c, sid, true)?;
                        p.purge_in_kept
                            .extend(text.into_iter().filter(|m| m.1.is_none()).map(|m| m.0));
                    }
                    None => {
                        p.spaces.push(sid.clone());
                        p.messages.extend(free);
                    }
                }
            }
        }
    }
    if level == Level::All && !p.spaces.is_empty() {
        let gone = serde_json::to_string(&p.spaces).unwrap_or_else(|_| "[]".into());
        // the people of the deleted spaces: members and senders
        let people = strings(
            c,
            "SELECT DISTINCT party_id FROM (
               SELECT a.party_id FROM space_members sm JOIN accounts a ON a.id = sm.account_id
                WHERE sm.space_id IN (SELECT value FROM json_each(?1))
               UNION SELECT cm.sender_party_id FROM chan_messages cm
                WHERE cm.space_id IN (SELECT value FROM json_each(?1)) AND cm.sender_party_id IS NOT NULL)
             ORDER BY 1",
            [&gone],
        )?;
        let mut st = c.prepare(&format!(
            "SELECT p.id, p.name, {CONTACT_REF} FROM parties p WHERE p.id = ?2 AND p.type = 'contact'"
        ))?;
        for pid in people {
            let row: Option<(String, String, Option<String>)> = st
                .query_row(params![gone, pid, t.account_id], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })
                .optional()?;
            let Some((id, name, why)) = row else { continue };
            match why {
                Some(reason) if reason == "-" => {}
                Some(reason) => p.kept.push(Kept {
                    kind: "contact",
                    id,
                    name,
                    reason,
                }),
                None => {
                    p.accounts.extend(strings(
                        c,
                        "SELECT id FROM accounts WHERE party_id = ? ORDER BY id",
                        [&id],
                    )?);
                    p.contacts.push(id);
                }
            }
        }
    }
    Ok(p)
}

// ---------------------------------------------------------------- doing it

fn json_ids(xs: &[String]) -> String {
    serde_json::to_string(xs).unwrap_or_else(|_| "[]".into())
}

/// Clears message text and attachments, and the raw payload of their signals.
fn purge_text(tx: &Connection, ids: &[String], now: i64) -> ChannelResult<()> {
    let ids = json_ids(ids);
    tx.execute(
        "UPDATE chan_messages SET text = NULL, attachments = '[]', purged = ?2
          WHERE id IN (SELECT value FROM json_each(?1))",
        params![ids, now],
    )?;
    tx.execute(
        "UPDATE ext_events SET payload = NULL, payload_purged = ?2
          WHERE message_id IN (SELECT value FROM json_each(?1)) AND payload IS NOT NULL",
        params![ids, now],
    )?;
    Ok(())
}

/// Tables whose rows exist only for a space (`space_id`).
const SPACE_OWNED: [&str; 5] = [
    "space_members",
    "space_settings",
    "persona_bindings",
    "space_prefs",
    "space_identities",
];

fn delete_all(tx: &Connection, p: &Plan, now: i64) -> ChannelResult<()> {
    let spaces = json_ids(&p.spaces);
    let msgs = json_ids(&p.messages);
    // signals of the deleted messages / spaces: gone unless something still points at them
    let unused_event = "e.work_item_id IS NULL AND e.decision_id IS NULL
        AND NOT EXISTS (SELECT 1 FROM flow_runs fr WHERE fr.event_id = e.id)
        AND NOT EXISTS (SELECT 1 FROM digest_entries de WHERE de.event_id = e.id)
        AND NOT EXISTS (SELECT 1 FROM signal_corrections sc WHERE sc.event_id = e.id)";
    let of_target = "(e.message_id IN (SELECT value FROM json_each(?1)) OR e.space_id IN (SELECT value FROM json_each(?2)))";
    tx.execute(
        &format!("DELETE FROM ext_events WHERE id IN (SELECT e.id FROM ext_events e WHERE {of_target} AND {unused_event})"),
        params![msgs, spaces],
    )?;
    tx.execute(
        &format!(
            "UPDATE ext_events SET payload = NULL, payload_purged = ?3, message_id = NULL, space_id = NULL
              WHERE id IN (SELECT e.id FROM ext_events e WHERE {of_target})"
        ),
        params![msgs, spaces, now],
    )?;
    for table in ["reply_basis", "route_log", "thread_runs", "chan_reactions"] {
        tx.execute(
            &format!("DELETE FROM {table} WHERE message_id IN (SELECT value FROM json_each(?1))"),
            [&msgs],
        )?;
    }
    tx.execute(
        "DELETE FROM chan_messages WHERE id IN (SELECT value FROM json_each(?1))",
        [&msgs],
    )?;
    // a space keeps only referenced messages, so nothing else is left in a deleted one
    tx.execute(
        "DELETE FROM decisions WHERE subject_type = 'space' AND item_id IS NULL
            AND subject_id IN (SELECT value FROM json_each(?1))",
        [&spaces],
    )?;
    for table in SPACE_OWNED {
        tx.execute(
            &format!("DELETE FROM {table} WHERE space_id IN (SELECT value FROM json_each(?1))"),
            [&spaces],
        )?;
    }
    tx.execute(
        "DELETE FROM entity_tags WHERE object_type = 'space' AND object_id IN (SELECT value FROM json_each(?1))",
        [&spaces],
    )?;
    tx.execute(
        "DELETE FROM spaces WHERE id IN (SELECT value FROM json_each(?1))",
        [&spaces],
    )?;
    // the contacts identify made for them, with their accounts
    let accounts = json_ids(&p.accounts);
    let contacts = json_ids(&p.contacts);
    tx.execute(
        "DELETE FROM space_members WHERE account_id IN (SELECT value FROM json_each(?1))",
        [&accounts],
    )?;
    tx.execute(
        "DELETE FROM account_handles WHERE account_id IN (SELECT value FROM json_each(?1))",
        [&accounts],
    )?;
    tx.execute(
        "UPDATE chan_messages SET sender_account_id = NULL WHERE sender_account_id IN (SELECT value FROM json_each(?1))",
        [&accounts],
    )?;
    tx.execute(
        "DELETE FROM accounts WHERE id IN (SELECT value FROM json_each(?1))",
        [&accounts],
    )?;
    tx.execute(
        "DELETE FROM links WHERE (subject_type = 'party' AND subject_id IN (SELECT value FROM json_each(?1)))
            OR (object_type = 'party' AND object_id IN (SELECT value FROM json_each(?1)))",
        [&contacts],
    )?;
    tx.execute(
        "DELETE FROM entity_tags WHERE object_type = 'party' AND object_id IN (SELECT value FROM json_each(?1))",
        [&contacts],
    )?;
    tx.execute(
        "DELETE FROM parties WHERE id IN (SELECT value FROM json_each(?1))",
        [&contacts],
    )?;
    if !p.purge_in_kept.is_empty() {
        purge_text(tx, &p.purge_in_kept, now)?;
    }
    Ok(())
}

/// Where backups go: `<STEVE_HOME>/backups` for a database at `<STEVE_HOME>/data/…`, else a
/// `backups` folder next to the database.
fn backup_dir(db: &std::path::Path) -> std::path::PathBuf {
    let parent = db.parent().unwrap_or_else(|| std::path::Path::new("."));
    match parent.file_name().and_then(|n| n.to_str()) {
        Some("data") => parent.parent().unwrap_or(parent).join("backups"),
        _ => parent.join("backups"),
    }
}

/// A full copy of the database (`VACUUM INTO`, a consistent snapshot under the write lock) before
/// anything is purged. Returns the file name.
fn backup(ch: &Channels, now: i64) -> ApiResult<String> {
    let dir = backup_dir(ch.store().path());
    std::fs::create_dir_all(&dir)
        .map_err(|e| ApiError::Internal(format!("备份目录建不了：{e}")))?;
    let name = format!("pre-purge-{now}.sqlite3");
    let file = dir.join(&name);
    let path = file
        .to_str()
        .ok_or_else(|| ApiError::Internal("备份路径不是 UTF-8".into()))?
        .to_string();
    ch.store().with_conn(|c| {
        c.execute("VACUUM INTO ?", [&path])?;
        Ok(())
    })?;
    Ok(name)
}

/// Backup, then the cleanup in one transaction, audited.
pub async fn run(ch: &Channels, v: &Viewer, t: &Target, level: Level) -> ApiResult<Summary> {
    let (ch2, v2, t2) = (ch.clone(), v.clone(), t.clone());
    tokio::task::spawn_blocking(move || -> ApiResult<Summary> {
        let (ch, v, t) = (ch2, v2, t2);
        // fail before the backup when there is nothing to clean
        ch.store().reader(|c| plan(c, &v, &t, level).map(|_| ()))?;
        let now = ch.now();
        let file = backup(&ch, now)?;
        let mut out = ch.store().transaction(|tx| -> ApiResult<Summary> {
            let p = plan(tx, &v, &t, level)?;
            match level {
                Level::Text => purge_text(tx, &p.messages, now)?,
                Level::All => delete_all(tx, &p, now)?,
            }
            let s = p.summary(level, &t.name);
            base::audit(
                tx,
                &v.ws,
                &v.user,
                "channel.cleanup",
                t.connection_id.as_deref().unwrap_or(&t.account_id),
                &json!({
                    "level": level, "account": t.account_id, "spaces": s.spaces,
                    "messages": s.messages, "messages_purged": s.messages_purged,
                    "contacts": s.contacts, "accounts": s.accounts, "kept": s.kept_count,
                    "backup": file,
                }),
            )?;
            for id in p.spaces.iter().chain(p.kept.iter().filter(|k| k.kind == "space").map(|k| &k.id)) {
                spaces::event(tx, &v.ws, id);
            }
            Ok(s)
        })?;
        out.backup = Some(file);
        tracing::info!(level = ?level, spaces = out.spaces, messages = out.messages, contacts = out.contacts, "channels: cleanup done");
        Ok(out)
    })
    .await
    .map_err(|_| ApiError::Internal("任务失败".into()))?
}

/// Disconnected identities that still have spaces the viewer sees.
pub fn leftovers(c: &Connection, v: &Viewer) -> ChannelResult<Vec<Leftover>> {
    let mut st = c.prepare(&format!(
        "SELECT s.account_id,
                coalesce((SELECT CASE WHEN a.display_name <> '' THEN a.display_name WHEN a.handle_display <> '' THEN a.handle_display ELSE a.handle END
                            FROM accounts a WHERE a.id = s.account_id), ''),
                min(s.system_id), count(*),
                (SELECT count(*) FROM chan_messages m JOIN spaces x ON x.id = m.space_id
                  WHERE x.workspace_id = ?1 AND x.connection_id IS NULL AND x.account_id = s.account_id)
           FROM spaces s
          WHERE s.workspace_id = ?1 AND s.connection_id IS NULL AND s.account_id IS NOT NULL AND {}
          GROUP BY s.account_id ORDER BY 2, 1",
        spaces::visible_sql("?2")
    ))?;
    let rows = st
        .query_map(params![v.ws, v.user], |r| {
            Ok(Leftover {
                account_id: r.get(0)?,
                name: r.get(1)?,
                system_id: r.get(2)?,
                spaces: r.get(3)?,
                messages: r.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}
