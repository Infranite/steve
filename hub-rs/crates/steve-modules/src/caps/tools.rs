//! Tool search, a Bot's side (`steve_store::caps::tools`, migration 69). Every session lists Steve's
//! core tools ([`CORE`]) and the ones its Bot pinned; every other tool it can use is found with
//! `steve_tools_search` and run with `steve_tool_call` (the Hub's tool registry,
//! `steve_hub::wire::tool_registry`). A pin is a built-in tool by name (`tool`), every tool of a library
//! MCP server (`mcp`, the server's id) or a remote agent's tool (`a2a`, the agent's id).
//!
//! - `GET /bots/:id/tools` → `{core: [name], pins: [{kind, target}], usage: [{tool, searched, called}],
//!   days: 30}`: the usage over the last 30 days, most called first
//! - `PUT /bots/:id/tool-pins/:kind/:target` pins one, `DELETE` the same path unpins it
//!
//! An MCP server or remote agent can be pinned only while the Bot uses it; taking it off the Bot or
//! deleting it unpins it. Every change is audited (`bot_tool.pin|unpin`) and publishes the Bot's change
//! event; a session's harness sees it the next time it lists its tools.

use std::sync::LazyLock;

use axum::Json;
use axum::extract::{Path, State};
use regex::Regex;
use serde_json::{Value, json};
use steve_store::caps::tools as repo;
use steve_store::{ChangeEvent, base, now};

use super::{Capabilities, own_bot};
use crate::vault::{VaultError, VaultResult};
use crate::{ApiError, Caller};

/// What every session of every Bot lists, in this order: the turn's own tools (report, ask, approval,
/// delivery), finding items and knowledge, and finding and running every other tool.
pub const CORE: &[&str] = &[
    "steve_update",
    "steve_ask",
    "steve_request_approval",
    "steve_deliver",
    "steve_items_search",
    "steve_knowledge_search",
    SEARCH,
    CALL,
];

/// `steve_tools_search`: finds tools by words and filters.
pub const SEARCH: &str = "steve_tools_search";
/// `steve_tool_call`: runs any tool the session can use, by name.
pub const CALL: &str = "steve_tool_call";

/// Pins one Bot may have.
pub const MAX_PINS: usize = 32;
/// Days the 能力 tab sums usage over.
pub const USAGE_DAYS: i64 = 30;

static TOOL_NAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[A-Za-z0-9_-]{1,128}$").unwrap());

impl Capabilities {
    /// The Bot's core tools, pins and usage.
    pub fn bot_tools(&self, ws: &str, bot: &str) -> VaultResult<Value> {
        self.vault.store().reader(|c| {
            own_bot(c, ws, bot)?;
            let today = repo::day_of(now());
            Ok::<_, VaultError>(json!({
                "core": CORE,
                "pins": repo::pins(c, bot)?,
                "usage": repo::usage(c, bot, today, USAGE_DAYS)?,
                "days": USAGE_DAYS,
            }))
        })
    }

    /// Pins (`on`) or unpins `target` of `kind` for `bot`.
    pub fn pin_tool(
        &self,
        ws: &str,
        me: &str,
        bot: &str,
        kind: &str,
        target: &str,
        on: bool,
    ) -> VaultResult<Value> {
        self.vault.store().transaction(|tx| {
            own_bot(tx, ws, bot)?;
            if !repo::PIN_KINDS.contains(&kind) {
                return Err(ApiError::invalid("kind").into());
            }
            let name = match kind {
                "tool" => {
                    if !TOOL_NAME.is_match(target) {
                        return Err(ApiError::invalid("target").into());
                    }
                    target.to_string()
                }
                _ => {
                    let (entry_ws, name) = super::entry_of(tx, kind, target)?;
                    let used = steve_store::caps::is_enabled(tx, bot, kind, target)?;
                    if entry_ws != ws || (on && !used) {
                        return Err(ApiError::not_found(super::noun(kind)).into());
                    }
                    name
                }
            };
            if on && !CORE.contains(&target) {
                let pins = repo::pins(tx, bot)?;
                let known = pins.iter().any(|p| p.kind == kind && p.target == target);
                if !known && pins.len() >= MAX_PINS {
                    return Err(ApiError::rule(steve_i18n::t!(
                        "modules.caps.too_many_pins",
                        max = MAX_PINS
                    ))
                    .into());
                }
            }
            // a core tool is loaded anyway: nothing to keep
            let changed =
                !CORE.contains(&target) && repo::set_pin(tx, bot, kind, target, on, now())?;
            if changed {
                base::audit(
                    tx,
                    ws,
                    me,
                    if on { "bot_tool.pin" } else { "bot_tool.unpin" },
                    target,
                    &json!({"bot": bot, "kind": kind, "name": name}),
                )?;
                tx.publish(ChangeEvent::Bot { id: bot.into() });
            }
            Ok::<_, VaultError>(json!({"bot": bot, "kind": kind, "target": target, "pinned": on}))
        })
    }
}

pub(super) async fn list_h(
    State(s): State<Capabilities>,
    who: Caller,
    Path(id): Path<String>,
) -> VaultResult<Json<Value>> {
    let svc = s.clone();
    s.vault
        .blocking(move |_| svc.bot_tools(&who.ws, &id))
        .await
        .map(Json)
}

pub(super) async fn pin_h(
    State(s): State<Capabilities>,
    who: Caller,
    Path((id, kind, target)): Path<(String, String, String)>,
) -> VaultResult<Json<Value>> {
    let svc = s.clone();
    s.vault
        .blocking(move |_| svc.pin_tool(&who.ws, &who.me, &id, &kind, &target, true))
        .await
        .map(Json)
}

pub(super) async fn unpin_h(
    State(s): State<Capabilities>,
    who: Caller,
    Path((id, kind, target)): Path<(String, String, String)>,
) -> VaultResult<Json<Value>> {
    let svc = s.clone();
    s.vault
        .blocking(move |_| svc.pin_tool(&who.ws, &who.me, &id, &kind, &target, false))
        .await
        .map(Json)
}
