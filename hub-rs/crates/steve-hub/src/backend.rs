//! What the HTTP layer needs from storage (steve-store) and the domain (steve-core).
//!
//! steve-store and steve-core are written in parallel, so the binary codes against these traits and the
//! integrator implements them with adapters. Entities cross the boundary as JSON objects already shaped like
//! the TS row mappers (column order, booleans, parsed JSON columns, removed keys), because the
//! API's job is to pass those shapes through byte-for-byte; typed structs on this side would only add a
//! second place where the shape can drift.
//!
//! Concurrency: [`Backend::reader`] is a read-pool connection used by read-only endpoints
//! without the core lock; [`Backend::writer`] holds steve-core's single lock for the duration of one
//! synchronous closure, so a handler's read-check-write sequence is as atomic as it was in Node.

use async_trait::async_trait;
use serde_json::{Map, Value};

/// A JSON object as produced by the store's row mappers.
pub type Obj = Map<String, Value>;
pub type BResult<T> = Result<T, BackendError>;

/// An error from storage or the domain. `status` is 500 for everything the TS Hub threw as a plain `Error`
/// (the message is shown to the user verbatim), 404 for `own()`-style misses.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message}")]
pub struct BackendError {
    pub status: u16,
    pub message: String,
}

impl BackendError {
    /// A thrown `Error(message)`: served as `500 {"error": message}`.
    pub fn msg(message: impl Into<String>) -> Self {
        BackendError {
            status: 500,
            message: message.into(),
        }
    }

    pub fn not_found(message: impl Into<String>) -> Self {
        BackendError {
            status: 404,
            message: message.into(),
        }
    }

    /// A trait method the integrator hasn't wired yet.
    pub fn unimplemented(what: &str) -> Self {
        Self::msg(format!("{UNIMPLEMENTED}{what}"))
    }
}

/// Message prefix of [`BackendError::unimplemented`].
pub const UNIMPLEMENTED: &str = "not implemented in the Rust Hub yet: ";

macro_rules! todo_backend {
    ($name:literal) => {
        Err(BackendError::unimplemented($name))
    };
}

/// A project to insert (repo.ts `projects.create`).
#[derive(Debug, Clone, PartialEq)]
pub struct NewProject {
    pub name: String,
    pub description: String,
    pub device_id: String,
    pub path: String,
    /// `git` or `dir`.
    pub kind: String,
    pub worktree: bool,
    pub default_bot: Option<String>,
    pub release: Option<Value>,
}

/// A read-only session copied from local harness history (system.ts `importLocal`); the store generates the
/// id and the throwaway token hash.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportedSession {
    pub workspace_id: String,
    /// `claude-code` or `codex`.
    pub harness: String,
    pub cwd: String,
    pub title: String,
    /// `本机 Claude Code` / `本机 Codex`.
    pub source: String,
    pub import_key: String,
    /// File mtime in ms; a float in Node, so it may carry a fraction.
    pub created: f64,
}

#[derive(Default)]
pub struct SummaryFacts {
    pub workspace_name: String,
    pub pending: bool,
    pub devices_online: usize,
    pub devices_total: usize,
}

pub type ItemExtras = steve_store::state_batches::ItemExtras<steve_core::pull::Closeout>;

/// Per-bot extras of `botOut` and `allBots` for one workspace.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BotExtras {
    /// bot id → workspace ids
    pub workspaces: std::collections::HashMap<String, Vec<String>>,
    /// bot id → its own MCP servers ([`Repo::bot_mcp_servers`])
    pub mcp: std::collections::HashMap<String, Vec<Value>>,
}

/// Storage operations, one per repo.ts function the API layer calls. Writes publish their bus events
/// inside the store, exactly where repo.ts does. Every method has a default that reports
/// "not implemented" so the binary compiles before every adapter method exists; the contract test
/// shows which ones are still missing.
#[allow(unused_variables)]
pub trait Repo {
    fn item_closeout(&self, id: &str) -> BResult<Option<steve_core::pull::Closeout>> {
        Ok(None)
    }
    fn bot_extras(&self) -> BResult<Option<BotExtras>> {
        Ok(None)
    }
    /// The sessions whose next turn waits because its device is full, keyed by session id, each
    /// with its device's room (`{running, max, waiting}`).
    fn waiting_for_room(&self) -> BResult<std::collections::HashMap<String, Value>> {
        Ok(Default::default())
    }
    // ── workspaces, users ──
    fn ensure_workspace(&self) -> BResult<()> {
        todo_backend!("ensure_workspace")
    }
    fn workspace_get(&self, id: &str) -> BResult<Option<Obj>> {
        todo_backend!("workspace_get")
    }
    /// `workspaces.ofUser`: Workspace rows plus the member `role`, ordered `sort, created`.
    fn workspaces_of_user(&self, user: &str) -> BResult<Vec<Obj>> {
        todo_backend!("workspaces_of_user")
    }
    fn workspaces_all(&self) -> BResult<Vec<Obj>> {
        todo_backend!("workspaces_all")
    }
    /// Creates `ws_<hex>` with `sort = max+1` and an owner membership; publishes `workspace`.
    fn workspace_create(
        &self,
        name: &str,
        description: Option<&str>,
        item_prefix: Option<&str>,
        owner: &str,
    ) -> BResult<Obj> {
        todo_backend!("workspace_create")
    }
    /// Patches the present keys (`name`, `description`, `item_prefix`); always publishes `workspace`.
    fn workspace_patch(&self, id: &str, patch: &Obj) -> BResult<Obj> {
        todo_backend!("workspace_patch")
    }
    fn workspace_is_member(&self, id: &str, user: &str) -> BResult<bool> {
        todo_backend!("workspace_is_member")
    }
    /// The `users` row of a live auth session (cookie `steve_session` token, hashed by the store).
    fn user_by_session(&self, token: &str) -> BResult<Option<Obj>> {
        todo_backend!("user_by_session")
    }
    fn audit(
        &self,
        actor: &str,
        action: &str,
        target: &str,
        detail: &Value,
        ws: &str,
    ) -> BResult<()> {
        todo_backend!("audit")
    }
    /// A page of the workspace's audit trail, newest first, with the names of the ids in it as
    /// `reader` may see them (`steve_store::AuditPage`).
    fn audit_page(
        &self,
        ws: &str,
        reader: &str,
        before: Option<i64>,
        limit: i64,
    ) -> BResult<Value> {
        todo_backend!("audit_page")
    }

    // ── settings ──
    /// Stored settings of a workspace, values JSON-parsed (`json(v, null)`), in row order.
    fn settings_all(&self, ws: &str) -> BResult<Obj> {
        todo_backend!("settings_all")
    }
    /// One stored setting, JSON-parsed; `None` when absent.
    fn settings_get(&self, ws: &str, key: &str) -> BResult<Option<Value>> {
        todo_backend!("settings_get")
    }
    /// Upserts every key (`JSON.stringify` of the value), writes a revision, publishes `settings`.
    fn settings_set(&self, ws: &str, values: &Obj, author: &str) -> BResult<()> {
        todo_backend!("settings_set")
    }

    // ── projects ──
    /// Non-archived projects ordered by name.
    fn projects_list(&self, ws: &str) -> BResult<Vec<Obj>> {
        todo_backend!("projects_list")
    }
    fn project_get(&self, id: &str) -> BResult<Option<Obj>> {
        todo_backend!("project_get")
    }
    fn project_by_path(&self, ws: &str, path: &str) -> BResult<Option<Obj>> {
        todo_backend!("project_by_path")
    }
    /// UNIQUE violations surface as `BackendError::msg(<sqlite message>)` (500), as in TS.
    fn project_create(&self, ws: &str, p: &NewProject) -> BResult<Obj> {
        todo_backend!("project_create")
    }
    fn project_patch(&self, id: &str, patch: &Obj, author: &str) -> BResult<Obj> {
        todo_backend!("project_patch")
    }

    // ── bots ──
    /// `bots.list(ws)`; `None` = every non-deleted bot.
    fn bots_list(&self, ws: Option<&str>) -> BResult<Vec<Obj>> {
        todo_backend!("bots_list")
    }
    /// A non-deleted bot.
    fn bot_get(&self, id: &str) -> BResult<Option<Obj>> {
        todo_backend!("bot_get")
    }
    /// Workspace ids of the bot's memberships (`bot_workspaces` row order).
    fn bot_workspaces(&self, id: &str) -> BResult<Vec<String>> {
        todo_backend!("bot_workspaces")
    }
    /// The Bot's own MCP servers as its view shows them: `{id, name, transport, enabled}` by name.
    fn bot_mcp_servers(&self, id: &str) -> BResult<Vec<Value>> {
        Ok(vec![])
    }
    /// Membership, deleted bots included.
    fn bot_in_workspace(&self, id: &str, ws: &str) -> BResult<bool> {
        todo_backend!("bot_in_workspace")
    }
    fn bot_set_workspaces(&self, id: &str, ws: &[String]) -> BResult<()> {
        todo_backend!("bot_set_workspaces")
    }
    /// `bots.create`: seed insert (only the seed columns are taken from `fields`), membership, audit, publish.
    fn bot_create(&self, ws: &str, fields: &Obj, author: &str) -> BResult<Obj> {
        todo_backend!("bot_create")
    }
    /// `bots.patch` with the allowed keys present in `patch`; missing bot → `没有这个 Bot` (500).
    fn bot_patch(&self, id: &str, patch: &Obj, author: &str) -> BResult<Obj> {
        todo_backend!("bot_patch")
    }
    /// REST selection edits: read the current Bot, validate the merged selection with
    /// `steve_core::models::validate_bot_patch`, and patch in one write transaction. The catalogue
    /// is already read; no network or probing belongs inside this transaction.
    fn bot_patch_checked(
        &self,
        id: &str,
        patch: &Obj,
        author: &str,
        harnesses: &[steve_core::harnesses::Harness],
    ) -> BResult<Obj> {
        todo_backend!("bot_patch_checked")
    }
    fn bot_restore(&self, id: &str) -> BResult<()> {
        todo_backend!("bot_restore")
    }

    // ── memories ──
    fn memory_get(&self, id: &str) -> BResult<Option<Obj>> {
        todo_backend!("memory_get")
    }
    /// Memories of (bot, ws), created DESC.
    fn memories_list(&self, bot: &str, ws: &str) -> BResult<Vec<Obj>> {
        todo_backend!("memories_list")
    }
    fn memories_count(&self, bot: &str, ws: &str) -> BResult<i64> {
        todo_backend!("memories_count")
    }
    fn memory_add(&self, bot: &str, ws: &str, text: &str, source: &str) -> BResult<String> {
        todo_backend!("memory_add")
    }
    fn memory_edit(&self, id: &str, text: &str) -> BResult<()> {
        todo_backend!("memory_edit")
    }
    fn memory_remove(&self, id: &str) -> BResult<()> {
        todo_backend!("memory_remove")
    }
    fn memories_clear(&self, ws: &str) -> BResult<()> {
        todo_backend!("memories_clear")
    }
    /// `DELETE FROM bot_memories WHERE bot_id = ?` (bot deletion with `memory: delete`).
    fn memories_delete_of_bot(&self, bot: &str) -> BResult<()> {
        todo_backend!("memories_delete_of_bot")
    }

    // ── tags ──
    fn tags_list(&self, ws: &str) -> BResult<Vec<Obj>> {
        todo_backend!("tags_list")
    }
    fn tag_get(&self, id: &str) -> BResult<Option<Obj>> {
        todo_backend!("tag_get")
    }
    fn tag_by_name(&self, ws: &str, name: &str) -> BResult<Option<Obj>> {
        todo_backend!("tag_by_name")
    }
    /// Returns the existing tag of that name instead of inserting a duplicate.
    fn tag_create(
        &self,
        ws: &str,
        name: &str,
        cat: &str,
        description: &str,
        by: &str,
    ) -> BResult<Obj> {
        todo_backend!("tag_create")
    }
    fn tag_patch(&self, id: &str, patch: &Obj) -> BResult<Option<Obj>> {
        todo_backend!("tag_patch")
    }
    fn tag_remove(&self, id: &str) -> BResult<()> {
        todo_backend!("tag_remove")
    }
    fn tag_merge(&self, from: &str, into: &str) -> BResult<()> {
        todo_backend!("tag_merge")
    }
    /// `{tagId: count}` for every tag of the workspace.
    fn tag_uses(&self, ws: &str) -> BResult<Obj> {
        todo_backend!("tag_uses")
    }
    fn tags_of_item(&self, item: &str) -> BResult<Vec<String>> {
        todo_backend!("tags_of_item")
    }
    /// A task item's set must hold exactly one category (409 otherwise); returns the ids kept.
    fn tags_set_item(&self, item: &str, tags: &[String]) -> BResult<Vec<String>> {
        todo_backend!("tags_set_item")
    }

    // ── items ──
    /// Every task item of the workspace, updated DESC.
    fn item_get(&self, id: &str) -> BResult<Option<Obj>> {
        todo_backend!("item_get")
    }
    /// `items.patch`: present keys only (booleans → 0/1, `plan`/`snooze` JSON-encoded), bumps `updated` and
    /// publishes `item` unless `patch` is empty. Returns the item as read back.
    fn item_patch(&self, id: &str, patch: &Obj) -> BResult<Option<Obj>> {
        todo_backend!("item_patch")
    }
    /// Switches the item's design gate (never part of `item_patch`); the item as read back.
    fn item_set_design_gate(&self, id: &str, on: bool) -> BResult<Option<Obj>> {
        todo_backend!("item_set_design_gate")
    }
    fn item_remove(&self, id: &str) -> BResult<()> {
        todo_backend!("item_remove")
    }
    /// `[{version, text, created}]` by version.
    fn item_text(&self, ws: &str, id: &str) -> BResult<Option<Obj>> {
        todo_backend!("item_text")
    }
    fn item_goals(&self, id: &str) -> BResult<Vec<Obj>> {
        todo_backend!("item_goals")
    }
    /// Earlier plans `[{round, steps, end, created}]`, oldest first.
    fn item_plan_rounds(&self, id: &str) -> BResult<Vec<Obj>> {
        todo_backend!("item_plan_rounds")
    }
    /// Ended cycles `[{cycle, revision, outcome, acceptance, reason, closed, source}]`, oldest first.
    fn item_cycles(&self, id: &str) -> BResult<Vec<Obj>> {
        Ok(vec![])
    }
    /// Latest goal text, `""` when none.
    fn item_goal(&self, id: &str) -> BResult<String> {
        todo_backend!("item_goal")
    }
    /// Dependency ids of an item, by created.
    fn deps_of(&self, item: &str) -> BResult<Vec<String>> {
        todo_backend!("deps_of")
    }
    fn deps_dependents(&self, item: &str) -> BResult<Vec<String>> {
        todo_backend!("deps_dependents")
    }
    /// `{follow, origin_msg}` of an item made from the conversation, else `None`.
    fn thread_item(&self, item: &str) -> BResult<Option<Value>> {
        Ok(None)
    }
    fn timeline_add(
        &self,
        item: &str,
        actor: &str,
        kind: &str,
        text: &str,
        session: Option<&str>,
    ) -> BResult<()> {
        todo_backend!("timeline_add")
    }
    /// `[{id, ts, actor, kind, text, session_id}]` by id.
    fn timeline_list(&self, item: &str) -> BResult<Vec<Obj>> {
        todo_backend!("timeline_list")
    }
    fn artifacts_list(&self, item: &str) -> BResult<Vec<Obj>> {
        todo_backend!("artifacts_list")
    }
    /// `{artifact id: "record" | "result"}` of the outputs the agent said a role for.
    fn artifact_roles(&self, item: &str) -> BResult<Obj> {
        todo_backend!("artifact_roles")
    }
    /// The user's notes on the item's outputs, drafts and sent (ST-38).
    fn annotations_list(&self, item: &str) -> BResult<Vec<Obj>> {
        todo_backend!("annotations_list")
    }
    fn annotation_add(
        &self,
        item: &str,
        art: &str,
        path: &str,
        anchor: &Value,
        note: &str,
    ) -> BResult<Obj> {
        todo_backend!("annotation_add")
    }
    /// `None`: no such draft.
    fn annotation_edit(&self, item: &str, id: &str, note: &str) -> BResult<Option<Obj>> {
        todo_backend!("annotation_edit")
    }
    /// false: no such draft.
    fn annotation_remove(&self, item: &str, id: &str) -> BResult<bool> {
        todo_backend!("annotation_remove")
    }
    /// The drafts `ids` taken for the send `key`; `(true, taken then)` when the key was seen before.
    fn annotations_claim(
        &self,
        item: &str,
        ids: &[String],
        key: &str,
    ) -> BResult<(bool, Vec<Obj>)> {
        todo_backend!("annotations_claim")
    }
    fn annotations_settle(
        &self,
        item: &str,
        key: &str,
        sent: bool,
        timeline_id: Option<i64>,
    ) -> BResult<()> {
        todo_backend!("annotations_settle")
    }

    // ── sessions, events, queue ──
    /// Raw session row (no conversion; `readonly` stays 0/1).
    fn session_get(&self, id: &str) -> BResult<Option<Obj>> {
        todo_backend!("session_get")
    }
    /// Sessions of an item ordered by `no`.
    fn sessions_of_item(&self, item: &str) -> BResult<Vec<Obj>> {
        todo_backend!("sessions_of_item")
    }
    fn session_patch(&self, id: &str, patch: &Obj) -> BResult<()> {
        todo_backend!("session_patch")
    }
    /// `SELECT DISTINCT e.session_id … e.data LIKE '%q%' LIMIT 200` (no escaping, as in TS).
    fn sessions_search(&self, ws: &str, q: &str) -> BResult<Vec<String>> {
        todo_backend!("sessions_search")
    }
    /// Inserts an event, publishes it, returns its seq.
    fn events_add(&self, session: &str, kind: &str, data: &Value) -> BResult<i64> {
        todo_backend!("events_add")
    }
    /// Full queued rows of a session by created.
    fn queued_list(&self, session: &str) -> BResult<Vec<Obj>> {
        todo_backend!("queued_list")
    }
    /// Every non-null `sessions.import_key`.
    fn import_keys(&self) -> BResult<Vec<String>> {
        todo_backend!("import_keys")
    }
    /// The raw INSERT of system.ts `importLocal`; returns the new id. No publish: the caller publishes
    /// after the events.
    fn session_insert_imported(&self, s: &ImportedSession) -> BResult<String> {
        todo_backend!("session_insert_imported")
    }
    /// Deletes events, queued rows and the session in one transaction, then publishes `session` (no item_id).
    fn session_delete(&self, id: &str) -> BResult<()> {
        todo_backend!("session_delete")
    }

    // ── attachments (migration 3) ──
    fn file_get(&self, id: &str) -> BResult<Option<Obj>> {
        todo_backend!("file_get")
    }
    /// `files.same`: a row of this workspace with these bytes and this (clean) name.
    fn file_same(&self, ws: &str, sha256: &str, name: &str) -> BResult<Option<Obj>> {
        todo_backend!("file_same")
    }
    fn file_create(&self, f: &crate::files::NewFile) -> BResult<Obj> {
        todo_backend!("file_create")
    }

    // ── decisions, messages, devices ──
    fn decision_get(&self, id: &str) -> BResult<Option<Obj>> {
        todo_backend!("decision_get")
    }
    fn decisions_of_item(&self, item: &str) -> BResult<Vec<Obj>> {
        todo_backend!("decisions_of_item")
    }
    /// An item's decisions as views (`steve_types::DecisionView`), oldest first.
    fn decision_views_of_item(
        &self,
        ws: &str,
        item: &str,
    ) -> BResult<Vec<steve_types::DecisionView>> {
        todo_backend!("decision_views_of_item")
    }
    /// `messages.list(ws, space, limit)`: the latest `limit` of a space, ascending.
    fn messages_in(&self, ws: &str, space: &str, limit: i64) -> BResult<Vec<Obj>> {
        todo_backend!("messages_in")
    }
    /// Up to `limit` messages of a space created in `[after, before)`, the newest of them, oldest first.
    fn messages_search(&self, ws: &str, space: &str, q: &str, limit: i64) -> BResult<Vec<Obj>> {
        todo_backend!("messages_search")
    }
    /// `messages.add`: `meta` stored only when truthy; publishes `message`. Returns the id.
    fn message_add(
        &self,
        ws: &str,
        author: &str,
        text: &str,
        item: Option<&str>,
        meta: &Value,
        space: &str,
    ) -> BResult<String> {
        todo_backend!("message_add")
    }
    /// The raw `settings.value` text of one key (no JSON parsing).
    fn setting_raw(&self, ws: &str, key: &str) -> BResult<Option<String>> {
        todo_backend!("setting_raw")
    }
    /// `principal`'s variables on `node` (`node_env`), if `me` in `ws` may see them (403 otherwise).
    fn node_env_list(&self, me: &str, ws: &str, node: &str, principal: &str) -> BResult<Value> {
        todo_backend!("node_env_list")
    }
    /// Sets one of them (`value`), or removes it (`None`).
    #[allow(clippy::too_many_arguments)]
    fn node_env_put(
        &self,
        me: &str,
        ws: &str,
        node: &str,
        principal: &str,
        key: &str,
        value: Option<&str>,
        note: &str,
    ) -> BResult<()> {
        todo_backend!("node_env_put")
    }
    /// Upserts one raw `settings` value (JSON text) without a revision row and without publishing.
    fn setting_put_raw(&self, ws: &str, key: &str, value: &str) -> BResult<()> {
        todo_backend!("setting_put_raw")
    }
    fn summary_facts(&self, ws: &str) -> BResult<SummaryFacts> {
        Ok(SummaryFacts::default())
    }
    fn devices_list(&self) -> BResult<Vec<Obj>> {
        todo_backend!("devices_list")
    }
    /// Insert sets `seen, created`; conflict updates `os, version, seen`. No publish.
    fn device_upsert(
        &self,
        id: &str,
        name: &str,
        roles: &[String],
        os: &str,
        version: &str,
    ) -> BResult<()> {
        todo_backend!("device_upsert")
    }
    /// Patches present keys; always publishes `device`.
    fn device_patch(&self, id: &str, patch: &Obj) -> BResult<()> {
        todo_backend!("device_patch")
    }
}

/// Storage handles. Both return a short-lived borrow used inside one `spawn_blocking` closure.
pub trait Backend: Send + Sync + 'static {
    /// A read-pool connection (no core lock).
    fn reader(&self) -> BResult<Box<dyn Repo + '_>>;
    /// The write connection under steve-core's single lock; released when the box is dropped.
    fn writer(&self) -> BResult<Box<dyn Repo + '_>>;
}

/// `createItem` input (orchestrator.ts).
#[derive(Debug, Clone, PartialEq)]
pub struct CreateItem {
    pub ws: String,
    pub goal: String,
    pub title: String,
    pub bot: Option<String>,
    pub team: Option<String>,
    pub node: Option<String>,
    pub required_specialties: Vec<String>,
    pub deps: Vec<String>,
    /// `None` → the bot's default project; `Some(None)` → explicitly no project.
    pub project: Option<Option<String>>,
    pub tag_ids: Vec<String>,
    /// e.g. `["手动新建", "manual"]`.
    pub source: [String; 2],
    /// `start` or `propose`.
    pub mode: String,
    pub actor: String,
    pub opening: Option<String>,
}

/// A tag the router proposes creating.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewTag {
    pub name: String,
    pub cat: String,
    pub description: String,
}

/// gateway.ts `route()` result (already validated against the workspace).
#[derive(Debug, Clone, PartialEq)]
pub struct Route {
    pub kind: String,
    pub title: String,
    pub project: Option<String>,
    pub tag_ids: Vec<String>,
    pub new_tags: Vec<NewTag>,
    pub reply: String,
}

/// Model tier of `claudePrint`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Fast,
    Smart,
}

#[derive(serde::Serialize)]
pub struct BotNodeCandidates {
    pub bindings: Vec<String>,
    pub unavailable_bindings: usize,
    pub candidates: Vec<BotNodeCandidate>,
}

#[derive(serde::Serialize)]
pub struct BotNodeCandidate {
    pub id: String,
    pub name: Option<String>,
    pub state: BotNodeState,
    pub reason: Option<steve_types::PlacementRejection>,
    pub checked_at: Option<i64>,
    pub provider_verified: bool,
}

#[derive(PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BotNodeState {
    Candidate,
    Pending,
    Unavailable,
}

/// Domain operations owned by steve-core (orchestrator, thread, changes, gateway, acp). Mutations take the
/// core lock internally. Results are the JSON the TS function returned.
#[async_trait]
#[allow(unused_variables)]
pub trait Domain: Send + Sync + 'static {
    // ── boot / shutdown (main.ts order: ensureWorkspace, seed, registerHub, recover, watchReleases) ──
    async fn seed(&self) -> BResult<()> {
        todo_backend!("seed")
    }
    async fn recover(&self) -> BResult<()> {
        todo_backend!("recover")
    }
    async fn upgrade_drain(&self, action: &str) -> BResult<Obj> {
        todo_backend!("upgrade_drain")
    }
    /// Listener bound: begin the prepared durable recovery.
    async fn ready(&self) -> BResult<()> {
        Ok(())
    }
    /// orchestrator `reportRelease` (boot and on every `last.json` change).
    async fn report_release(&self) -> BResult<()> {
        todo_backend!("report_release")
    }
    /// SIGTERM every live adapter process.
    async fn shutdown(&self) {}
    /// Task lifetimes shared by assembly, app boot and domain shutdown.
    fn background_tasks(&self) -> Option<std::sync::Arc<crate::jobs::Tasks>> {
        None
    }
    /// Starts the domain's maintenance jobs, when this domain provides them.
    fn spawn_background(&self) {}

    // ── items ──
    async fn create_item(&self, p: CreateItem) -> Result<Obj, crate::error::ApiError> {
        Err(BackendError::unimplemented("create_item").into())
    }
    async fn confirm_bot_proposal(
        &self,
        _ws: String,
        _actor: String,
        _id: String,
        _edit: steve_types::ConfirmBotProposal,
    ) -> Result<String, crate::error::ApiError> {
        Err(BackendError::unimplemented("confirm_bot_proposal").into())
    }
    async fn route_for_team(&self, ws: &str, text: &str, _team: Option<&str>) -> BResult<Route> {
        self.route(ws, text).await
    }
    async fn bot_nodes(&self, _ws: &str, _bot: &str) -> BResult<BotNodeCandidates> {
        todo_backend!("bot_nodes")
    }
    async fn route(&self, ws: &str, text: &str) -> BResult<Route> {
        todo_backend!("route")
    }
    async fn item_action(&self, id: &str, action: &str, actor: &str) -> BResult<Obj> {
        todo_backend!("item_action")
    }
    /// The durable work graph beneath one item; `null` when no graph has been planned.
    async fn workflow_get(&self, id: &str) -> BResult<Value> {
        Ok(Value::Null)
    }
    async fn dependency_get(&self, id: &str) -> BResult<Value> {
        Ok(Value::Null)
    }
    async fn dependency_wait(
        &self,
        id: &str,
        expected_cycle: i64,
        resume: &str,
        reason: &str,
    ) -> BResult<Value> {
        todo_backend!("dependency_wait")
    }
    /// Plan, pause, resume or change a work unit under an optimistic revision check.
    async fn workflow_change(&self, id: &str, args: Value, actor: &str) -> BResult<Value> {
        todo_backend!("workflow_change")
    }
    /// The lifecycle commands (`accept` | `cancel` | `fail` | `new_cycle`), checked against the cycle
    /// and revision the caller saw; returns the item.
    #[allow(clippy::too_many_arguments)]
    async fn lifecycle_command(
        &self,
        id: &str,
        command: &str,
        expected_cycle: i64,
        expected_revision: i64,
        reason: Option<String>,
        goal: Option<String>,
        actor: &str,
    ) -> BResult<Obj> {
        todo_backend!("lifecycle_command")
    }
    /// The lifecycle projection of one item (`None` without a lifecycle-aware domain).
    async fn lifecycle_view(&self, id: &str) -> BResult<Option<Value>> {
        Ok(None)
    }
    /// The derived state of one item; `None` without a domain
    /// that derives it, or for a conversation item. Read-only.
    async fn derived_state(&self, id: &str) -> BResult<Option<Value>> {
        Ok(None)
    }
    /// Read-only diagnostic projection; authorization is performed by the caller.
    async fn diagnostic_snapshot(&self, id: &str) -> BResult<Value> {
        todo_backend!("diagnostic_snapshot")
    }
    /// Commits the closings whose gate holds now (the periodic tick).
    async fn lifecycle_tick(&self) -> BResult<usize> {
        Ok(0)
    }
    async fn revise_goal(&self, id: &str, text: &str) -> BResult<()> {
        todo_backend!("revise_goal")
    }
    /// `files`: FileRefs (`{id, name, mime, size}`) of the current workspace, already checked.
    async fn supplement(&self, id: &str, text: &str, mode: &str, files: Vec<Value>) -> BResult<()> {
        todo_backend!("supplement")
    }
    async fn deliver_queued(&self, id: &str) -> BResult<()> {
        todo_backend!("deliver_queued")
    }
    async fn reassign(&self, id: &str, bot: &str, actor: &str) -> BResult<Obj> {
        todo_backend!("reassign")
    }
    async fn add_dep(&self, id: &str, dep: &str) -> BResult<()> {
        todo_backend!("add_dep")
    }
    async fn rm_dep(&self, id: &str, dep: &str) -> BResult<()> {
        todo_backend!("rm_dep")
    }

    // ── sessions ──
    /// `"sent"` or `"queued"`.
    async fn session_message(
        &self,
        session: &str,
        text: &str,
        mode: &str,
        files: Vec<Value>,
    ) -> BResult<String> {
        todo_backend!("session_message")
    }
    async fn cancel(&self, session: &str) -> BResult<bool> {
        todo_backend!("cancel")
    }
    /// Edit a row still in the session's queue; the row after. 409 once a turn took it.
    async fn queued_edit(&self, session: &str, id: &str, text: &str) -> BResult<Obj> {
        todo_backend!("queued_edit")
    }
    /// Withdraw a row from the session's queue; the row as it was. 409 once a turn took it.
    async fn queued_remove(&self, session: &str, id: &str) -> BResult<Obj> {
        todo_backend!("queued_remove")
    }
    /// `{result: "interrupted" | "sent" | "waiting", reason?}`. 409 once a turn took it.
    async fn queued_send_now(&self, session: &str, id: &str) -> BResult<Obj> {
        todo_backend!("queued_send_now")
    }
    /// `{item, running, since}` while the Hub holds new turns for a release, else `null`.
    async fn hold_status(&self) -> BResult<Value> {
        Ok(Value::Null)
    }
    /// `acp.isBusy`: a turn is in flight (adapter start, prompt, retry wait). In-memory, never blocks.
    fn is_busy(&self, session: &str) -> bool {
        false
    }
    async fn end_session(&self, session: &str) -> BResult<()> {
        todo_backend!("end_session")
    }
    /// The session's item goes on in a new session; the new session's id.
    async fn continue_in_new_session(&self, session: &str, actor: &str) -> BResult<String> {
        let _ = (session, actor);
        todo_backend!("continue_in_new_session")
    }
    /// `{session: id}` or `{item: id}`.
    async fn fork(
        &self,
        ws: &str,
        session: &str,
        seq: f64,
        goal: &str,
        as_session: bool,
        actor: &str,
    ) -> BResult<Obj> {
        todo_backend!("fork")
    }

    // ── decisions ──
    async fn resolve(&self, id: &str, choice: &str, text: &str, actor: &str) -> BResult<Obj> {
        todo_backend!("resolve")
    }
    async fn undo_decision(&self, id: &str, actor: &str) -> BResult<Obj> {
        todo_backend!("undo_decision")
    }
    /// orchestrator `dismissDecision` (withdraw an open decision).
    async fn dismiss_decision(&self, id: &str, actor: &str) -> BResult<Obj> {
        todo_backend!("dismiss_decision")
    }

    // ── release hand-off and housekeeping ──
    /// orchestrator `drain(token, start)`: `{idle, running}` for the current release's token, else `None`.
    async fn drain(&self, token: &str, start: bool) -> BResult<Option<Obj>> {
        todo_backend!("drain")
    }
    /// orchestrator `tidyBranch(item)`: give back the worktree/branch of a finished item.
    async fn tidy_branch(&self, item: &str) -> BResult<()> {
        todo_backend!("tidy_branch")
    }
    /// The items' worktrees, their sizes and the disk they are on (`steve_core::workspace::DiskReport`).
    async fn disk_report(&self) -> BResult<Value> {
        todo_backend!("disk_report")
    }
    /// One reclaim pass now (`steve_core::workspace::Reclaimed`).
    async fn reclaim_workspaces(&self) -> BResult<Value> {
        todo_backend!("reclaim_workspaces")
    }

    // ── bots ──
    /// `botPrompt(b, ws)`; the caller already set `prompt` to null for the generated variant.
    fn bot_prompt(&self, bot: &Obj, ws: &str) -> BResult<String> {
        todo_backend!("bot_prompt")
    }
    /// `{moved: [itemIds]}`.
    async fn delete_bot(&self, id: &str, to: &str, actor: &str) -> BResult<Obj> {
        todo_backend!("delete_bot")
    }

    // ── conversation (thread.ts) ──
    /// Inserts the user's message (publishing `message`) before returning, then handles it on the
    /// workspace lane in the background.
    /// `user`: who wrote it (module contexts are per user). `mentions`: the checked `@` mentions.
    async fn say(
        &self,
        ws: &str,
        user: &str,
        text: &str,
        files: Vec<Value>,
        mentions: Vec<steve_modules::contacts::item_people::Mention>,
    ) -> BResult<()> {
        todo_backend!("say")
    }
    /// [`Domain::say`] sent straight to an item (`item`, the focus bar) or quoting a message (`reply_to`).
    #[allow(clippy::too_many_arguments)]
    async fn say_to(
        &self,
        ws: &str,
        user: &str,
        text: &str,
        files: Vec<Value>,
        mentions: Vec<steve_modules::contacts::item_people::Mention>,
        item: Option<String>,
        reply_to: Option<String>,
    ) -> BResult<()> {
        if item.is_some() || reply_to.is_some() {
            return Err(BackendError::unimplemented("say_to"));
        }
        self.say(ws, user, text, files, mentions).await
    }
    async fn stop(&self, ws: &str) -> BResult<bool> {
        todo_backend!("stop")
    }
    /// Whether Steve is on a turn in the main conversation (the page's stop button after a reload).
    fn thread_active(&self, ws: &str) -> bool {
        false
    }
    /// Rewrites a main-conversation message still waiting its turn; `false` once it is being handled.
    async fn thread_queued_edit(&self, ws: &str, message: &str, text: &str) -> BResult<bool> {
        todo_backend!("thread_queued_edit")
    }
    /// Withdraws a message still waiting its turn: `{text, files}` as it was, `None` once it is being handled.
    async fn thread_queued_withdraw(&self, ws: &str, message: &str) -> BResult<Option<Obj>> {
        todo_backend!("thread_queued_withdraw")
    }
    /// `by`: the member acting, whose access an item made here runs with (as for the next three).
    async fn promote(&self, ws: &str, message: &str, by: &str) -> BResult<Obj> {
        todo_backend!("promote")
    }
    /// Thread routing: the result `{message_id, item?}` and whether it had been
    /// handled before (the API answers 409 then). `to`: `inline` | `item` | `item:<ST-n>`.
    async fn reroute(&self, ws: &str, message: &str, to: &str, by: &str) -> BResult<(Obj, bool)> {
        todo_backend!("reroute")
    }
    async fn offer(&self, ws: &str, message: &str, take: bool, by: &str) -> BResult<(Obj, bool)> {
        todo_backend!("offer")
    }
    /// `choice`: `here` | `item` | `new`.
    async fn suggest(
        &self,
        ws: &str,
        message: &str,
        choice: &str,
        by: &str,
    ) -> BResult<(Obj, bool)> {
        todo_backend!("suggest")
    }
    /// An A2UI button in Steve's reply `message` (ST-46): `{already, message_id?, action}`.
    async fn a2ui_action(
        &self,
        ws: &str,
        user: &str,
        message: &str,
        action: steve_core::thread::A2uiAction,
    ) -> BResult<Obj> {
        todo_backend!("a2ui_action")
    }
    /// Stop a running research run, or take a queued one off the queue.
    async fn stop_run(&self, ws: &str, run: &str) -> BResult<bool> {
        todo_backend!("stop_run")
    }
    /// Whether an item made from the conversation reports back to it (`thread_items.follow`).
    async fn thread_follow(&self, ws: &str, item: &str, follow: bool) -> BResult<bool> {
        todo_backend!("thread_follow")
    }

    // ── gateway ──
    /// A one-off model call booked (and its models chosen, 设置 · 模型) under `purpose` of workspace `ws`.
    async fn claude_print(
        &self,
        purpose: &'static str,
        ws: &str,
        prompt: &str,
        tier: Tier,
        timeout_ms: u64,
    ) -> Result<String, crate::error::ApiError> {
        Err(BackendError::unimplemented("claude_print").into())
    }

    // ── 设置 · 模型 ──
    /// The harness adapters the Hub knows, with what its Node said of them (`fresh`: asks again).
    async fn harnesses(&self, fresh: bool) -> BResult<Vec<steve_core::harnesses::Harness>> {
        let _ = fresh;
        Ok(steve_core::harnesses::builtin())
    }
    /// Offers of the Nodes this Bot targets, rather than the Hub-wide discovery catalogue.
    async fn bot_harnesses(
        &self,
        bot: Option<&str>,
    ) -> BResult<Vec<steve_core::harnesses::Harness>> {
        let _ = bot;
        self.harnesses(false).await
    }
    /// Every decision purpose with its default and chosen models, the agent defaults, the known models.
    async fn models_view(&self, ws: &str) -> BResult<Value> {
        todo_backend!("models_view")
    }
    /// The models `harness` ran with this past week, per model (calls, last success, last failure).
    async fn models_used(&self, ws: &str, harness: &str) -> BResult<Value> {
        todo_backend!("models_used")
    }
    /// Stores settings keys checked by `steve_core::models::validate` (revision + audit).
    async fn models_write(&self, ws: &str, values: &Obj, actor: &str) -> BResult<()> {
        todo_backend!("models_write")
    }

    // ── first meeting ──
    /// Explicit model choice from setup or chat; independent of model availability.
    async fn models_select(
        &self,
        ws: &str,
        choice: &steve_core::models::SteveSelection,
        actor: &str,
    ) -> BResult<Value> {
        todo_backend!("models_select")
    }

    /// Begins the first meeting (`steve_core::thread::onboard`): the id of its welcome message.
    async fn onboard_start(&self, ws: &str, actor: &str) -> BResult<String> {
        todo_backend!("onboard_start")
    }
    /// Ends the first meeting, whether or not it ran.
    async fn onboard_end(&self, ws: &str, actor: &str) -> BResult<()> {
        todo_backend!("onboard_end")
    }

    // ── conversational configuration (changes.ts) ──
    async fn config_draft(&self, ws: &str, target: &Obj, text: &str) -> BResult<Obj> {
        todo_backend!("config_draft")
    }
    async fn config_apply(&self, id: &str, actor: &str, ws: &str) -> BResult<Obj> {
        todo_backend!("config_apply")
    }
    async fn config_undo(&self, id: &str, actor: &str, ws: &str) -> BResult<Obj> {
        todo_backend!("config_undo")
    }
    /// Throws an in-memory draft away; its conversation card shows it dropped.
    async fn config_drop(&self, id: &str, ws: &str) -> BResult<Obj> {
        todo_backend!("config_drop")
    }
    /// A module changeset was applied (`applied`), dropped (`dropped`) or undone (`None`): its
    /// conversation card, if it has one, shows so.
    async fn config_mark(&self, ws: &str, id: &str, state: Option<&str>) -> BResult<()> {
        todo_backend!("config_mark")
    }
}

/// A domain with nothing wired: every operation reports "not implemented". Lets the binary boot (healthz,
/// static web, read endpoints) before steve-core lands.
pub struct NoDomain;

#[async_trait]
impl Domain for NoDomain {}
