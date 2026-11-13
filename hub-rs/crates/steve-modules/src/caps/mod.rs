//! 能力, the capabilities library (migration 65, `steve_store::caps`): MCP servers ([`mcp`]), skills
//! ([`skills`]) and remote A2A agents ([`a2a`]) are defined once per workspace and assigned per Bot.
//! A Bot's sessions get the entries it is assigned and has switched on, the same way for every
//! harness: the servers through ACP's `mcpServers`, the skills where the harness adapter declares it
//! reads them, each remote agent as one Steve tool.
//!
//! The library:
//! - `GET|POST /capabilities/mcp-servers`, `PATCH|DELETE /capabilities/mcp-servers/:id`
//! - `GET|POST /capabilities/skills`, `PATCH|DELETE /capabilities/skills/:id`
//! - `GET|POST /capabilities/a2a-agents`, `PATCH|DELETE /capabilities/a2a-agents/:id`,
//!   `POST /capabilities/a2a-agents/preview {card_url}` (fetches a card without keeping it)
//!
//! Every entry is answered with `bots: [{id, enabled}]`, the Bots that use it. A `POST` with
//! `bot: <id>` also assigns the new entry to that Bot (the quick add of a Bot's 能力 tab).
//!
//! A Bot's side:
//! - `GET /bots/:id/capabilities` → `{mcp: [...], skills: [...], a2a: [...]}`, each entry with its
//!   `enabled` for this Bot
//! - `PUT /bots/:id/capabilities/:kind/:entry {enabled?}` assigns (or switches) one, kind
//!   `mcp|skill|a2a`; `DELETE` the same path takes it off the Bot (the entry stays in the library).
//!
//! A Bot is assigned at most 32 entries of a kind; the names a harness sees stay distinct among a
//! Bot's entries; a Bot's switched-on skills hold at most 128 KiB together. Every change is audited
//! (names only) and publishes the change event of each Bot it touches. A change reaches a session
//! the next time its harness opens (an A2A tool: its next tool list).

pub mod a2a;
pub mod mcp;
pub mod skills;

use axum::extract::{Path, State};
use axum::routing::{get, patch, post, put};
use axum::{Json, Router};
use rusqlite::{Connection, OptionalExtension};
use serde::Deserialize;
use serde_json::{Value, json};
use steve_store::caps as repo;
use steve_store::{ChangeEvent, Tx, base, now};

use crate::vault::{Vault, VaultError, VaultResult};
use crate::{ApiError, Body, Caller};

/// Entries of a kind one Bot may be assigned.
pub const MAX_PER_BOT: usize = 32;
/// Entries of a kind in one workspace's library.
pub const MAX_LIBRARY: usize = 256;

/// The service: the store through the vault (which keeps every secret value), and the client that
/// fetches Agent Cards and talks to remote agents.
#[derive(Clone)]
pub struct Capabilities {
    vault: Vault,
    http: reqwest::Client,
}

pub fn routes<S: Clone + Send + Sync + 'static>(svc: Capabilities) -> Router<S> {
    Router::new()
        .route(
            "/capabilities/mcp-servers",
            get(mcp::list_h).post(mcp::create_h),
        )
        .route(
            "/capabilities/mcp-servers/{id}",
            patch(mcp::update_h).delete(mcp::remove_h),
        )
        .route(
            "/capabilities/skills",
            get(skills::list_h).post(skills::create_h),
        )
        .route(
            "/capabilities/skills/{id}",
            patch(skills::update_h).delete(skills::remove_h),
        )
        .route(
            "/capabilities/a2a-agents",
            get(a2a::list_h).post(a2a::create_h),
        )
        .route("/capabilities/a2a-agents/preview", post(a2a::preview_h))
        .route(
            "/capabilities/a2a-agents/{id}",
            patch(a2a::update_h).delete(a2a::remove_h),
        )
        .route("/bots/{id}/capabilities", get(bot_list_h))
        .route(
            "/bots/{id}/capabilities/{kind}/{entry}",
            put(assign_h).delete(unassign_h),
        )
        .with_state(svc)
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct AssignIn {
    #[serde(default)]
    pub enabled: Option<bool>,
}

async fn bot_list_h(
    State(s): State<Capabilities>,
    who: Caller,
    Path(id): Path<String>,
) -> VaultResult<Json<Value>> {
    let svc = s.clone();
    s.vault
        .blocking(move |_| svc.bot_capabilities(&who.ws, &id))
        .await
        .map(Json)
}

async fn assign_h(
    State(s): State<Capabilities>,
    who: Caller,
    Path((id, kind, entry)): Path<(String, String, String)>,
    Body(b): Body<AssignIn>,
) -> VaultResult<Json<Value>> {
    let svc = s.clone();
    s.vault
        .blocking(move |_| {
            svc.assign(
                &who.ws,
                &who.me,
                &id,
                &kind,
                &entry,
                b.enabled.unwrap_or(true),
            )
        })
        .await
        .map(Json)
}

async fn unassign_h(
    State(s): State<Capabilities>,
    who: Caller,
    Path((id, kind, entry)): Path<(String, String, String)>,
) -> VaultResult<Json<Value>> {
    let svc = s.clone();
    s.vault
        .blocking(move |_| svc.unassign(&who.ws, &who.me, &id, &kind, &entry))
        .await?;
    Ok(Json(json!({ "ok": true })))
}

/// The Bot, if `ws` sees it (its home or one it serves), not deleted.
pub(crate) fn own_bot(c: &Connection, ws: &str, bot: &str) -> Result<(), ApiError> {
    let seen: Option<i64> = c
        .query_row(
            "SELECT 1 FROM bots b WHERE b.id = ?1 AND b.deleted = 0 AND (b.workspace_id = ?2
               OR EXISTS (SELECT 1 FROM bot_workspaces w WHERE w.bot_id = b.id AND w.workspace_id = ?2))",
            [bot, ws],
            |r| r.get(0),
        )
        .optional()
        .map_err(ApiError::from)?;
    seen.map(|_| ()).ok_or_else(|| ApiError::not_found("Bot"))
}

/// The Bots that use an entry, as its answers list them.
pub(crate) fn users(c: &Connection, kind: &str, id: &str) -> Result<Value, ApiError> {
    Ok(Value::Array(
        repo::of_entry(c, kind, id)?
            .into_iter()
            .map(|a| json!({"id": a.bot_id, "enabled": a.enabled}))
            .collect(),
    ))
}

/// Each Bot using an entry hears that it changed.
pub(crate) fn publish_users(tx: &Tx, kind: &str, id: &str) -> Result<(), ApiError> {
    for a in repo::of_entry(tx, kind, id)? {
        tx.publish(ChangeEvent::Bot { id: a.bot_id });
    }
    Ok(())
}

/// A library entry of any kind, as far as assignment needs it: its workspace and the name its
/// harness sees.
fn entry_of(c: &Connection, kind: &str, id: &str) -> Result<(String, String), ApiError> {
    let found = match kind {
        "mcp" => steve_store::caps::mcp::get(c, id)?.map(|s| (s.workspace_id, s.name)),
        "skill" => steve_store::caps::skills::get(c, id)?.map(|s| (s.workspace_id, s.name)),
        "a2a" => steve_store::caps::a2a::get(c, id)?.map(|s| (s.workspace_id, s.name)),
        _ => None,
    };
    found.ok_or_else(|| ApiError::not_found(noun(kind)))
}

fn noun(kind: &str) -> &'static str {
    match kind {
        "mcp" => "MCP 服务器",
        "skill" => "技能",
        _ => "远程 Agent",
    }
}

/// Whether two names a harness sees are the same: MCP server names in any case, others exactly.
fn same_name(kind: &str, a: &str, b: &str) -> bool {
    if kind == "mcp" {
        a.eq_ignore_ascii_case(b)
    } else {
        a == b
    }
}

/// The name `name` of entry `entry` of `kind` would clash with another entry `bot` uses.
pub(crate) fn clashes(
    c: &Connection,
    bot: &str,
    kind: &str,
    entry: &str,
    name: &str,
) -> Result<bool, ApiError> {
    for a in repo::of_bot(c, bot, kind)? {
        if a.entry_id == entry {
            continue;
        }
        if let Ok((_, other)) = entry_of(c, kind, &a.entry_id)
            && same_name(kind, &other, name)
        {
            return Ok(true);
        }
    }
    Ok(false)
}

/// Assigns (or switches) entry `entry` of `kind` to `bot` in `tx`, with every check.
pub(crate) fn assign_in(
    tx: &Tx,
    ws: &str,
    me: &str,
    bot: &str,
    kind: &str,
    entry: &str,
    enabled: bool,
) -> Result<(), ApiError> {
    if !repo::KINDS.contains(&kind) {
        return Err(ApiError::not_found(noun(kind)));
    }
    own_bot(tx, ws, bot)?;
    let (entry_ws, name) = entry_of(tx, kind, entry)?;
    if entry_ws != ws {
        return Err(ApiError::not_found(noun(kind)));
    }
    let old = repo::get(tx, bot, kind, entry)?;
    if old.is_none() && repo::of_bot(tx, bot, kind)?.len() >= MAX_PER_BOT {
        return Err(ApiError::rule(steve_i18n::t!(
            "modules.caps.too_many",
            max = MAX_PER_BOT
        )));
    }
    if clashes(tx, bot, kind, entry, &name)? {
        return Err(ApiError::conflict(steve_i18n::t!(
            "modules.caps.name_used",
            name = name
        )));
    }
    if kind == "skill" && enabled && !old.as_ref().is_some_and(|a| a.enabled) {
        skills::room_for(tx, bot, entry, None)?;
    }
    repo::set(tx, bot, kind, entry, enabled, now())?;
    let verb = match (&old, enabled) {
        (None, _) => "bot_capability.assign",
        (Some(_), true) => "bot_capability.enable",
        (Some(_), false) => "bot_capability.disable",
    };
    base::audit(
        tx,
        ws,
        me,
        verb,
        entry,
        &json!({"bot": bot, "kind": kind, "name": name, "enabled": enabled}),
    )?;
    tx.publish(ChangeEvent::Bot { id: bot.into() });
    Ok(())
}

impl Capabilities {
    pub fn new(vault: Vault) -> Self {
        let http = reqwest::Client::builder()
            .connect_timeout(std::time::Duration::from_secs(10))
            .user_agent(concat!("steve-hub/", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_default();
        Capabilities { vault, http }
    }

    pub fn vault(&self) -> &Vault {
        &self.vault
    }

    /// What a Bot uses, kind by kind, each entry with its `enabled` for this Bot.
    pub fn bot_capabilities(&self, ws: &str, bot: &str) -> VaultResult<Value> {
        self.vault.store().reader(|c| {
            own_bot(c, ws, bot)?;
            let mcp: Vec<Value> = steve_store::caps::mcp::of_bot(c, bot)?
                .iter()
                .map(|(s, on)| {
                    let mut v = mcp::out(s);
                    v["enabled"] = json!(on);
                    v
                })
                .collect();
            let skills: Vec<Value> = steve_store::caps::skills::of_bot(c, bot)?
                .iter()
                .map(|(s, on)| {
                    let mut v = skills::out(s);
                    v["enabled"] = json!(on);
                    v
                })
                .collect();
            let a2a: Vec<Value> = steve_store::caps::a2a::of_bot(c, bot)?
                .iter()
                .map(|(s, on)| {
                    let mut v = a2a::out(s);
                    v["enabled"] = json!(on);
                    v
                })
                .collect();
            Ok::<_, VaultError>(json!({"mcp": mcp, "skills": skills, "a2a": a2a}))
        })
    }

    pub fn assign(
        &self,
        ws: &str,
        me: &str,
        bot: &str,
        kind: &str,
        entry: &str,
        enabled: bool,
    ) -> VaultResult<Value> {
        self.vault.store().transaction(|tx| {
            assign_in(tx, ws, me, bot, kind, entry, enabled)?;
            Ok::<_, VaultError>(
                json!({"bot": bot, "kind": kind, "entry": entry, "enabled": enabled}),
            )
        })
    }

    pub fn unassign(
        &self,
        ws: &str,
        me: &str,
        bot: &str,
        kind: &str,
        entry: &str,
    ) -> VaultResult<()> {
        self.vault.store().transaction(|tx| {
            own_bot(tx, ws, bot)?;
            let name = entry_of(tx, kind, entry)
                .map(|(_, n)| n)
                .unwrap_or_default();
            if !repo::remove(tx, bot, kind, entry)? {
                return Err(ApiError::not_found(noun(kind)).into());
            }
            base::audit(
                tx,
                ws,
                me,
                "bot_capability.unassign",
                entry,
                &json!({"bot": bot, "kind": kind, "name": name}),
            )?;
            tx.publish(ChangeEvent::Bot { id: bot.into() });
            Ok::<_, VaultError>(())
        })
    }
}
