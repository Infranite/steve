//! What the HTTP layer needs from storage (steve-store) and the domain (steve-core).
//!
//! steve-store and steve-core are written in parallel, so the binary codes against these traits and the
//! integrator implements them with adapters. Entities cross the boundary as JSON objects already shaped like
//! the TS row mappers (清单 §3.1: column order, booleans, parsed JSON columns, removed keys), because the
//! API's job is to pass those shapes through byte-for-byte; typed structs on this side would only add a
//! second place where the shape can drift.
//!
//! Concurrency (rust-hub-plan §3): [`Backend::reader`] is a read-pool connection used by read-only endpoints
//! without the core lock; [`Backend::writer`] holds steve-core's single lock for the duration of one
//! synchronous closure, so a handler's read-check-write sequence is as atomic as it was in Node.

use async_trait::async_trait;
use serde_json::{Map, Value};

/// A JSON object as produced by the store's row mappers.
pub type Obj = Map<String, Value>;
pub type BResult<T> = Result<T, BackendError>;
/// `(seq, ts, kind, data as stored JSON text)`.
pub type RawEvent = (i64, i64, String, String);

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

/// What `summary()` adds to one item (清单 §2.2): computed per item by default, or for a whole workspace in
/// a handful of queries by [`Repo::item_extras`].
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ItemExtras {
    pub goal: String,
    pub goals: usize,
    pub tags: Vec<String>,
    pub deps: Vec<String>,
    /// `(id, status)` of the item's sessions by `no`.
    pub sessions: Vec<(String, String)>,
    pub open: Vec<String>,
    /// Queued rows of the last non-ended session.
    pub queued: usize,
}

/// Per-bot extras of `botOut` and `allBots` for one workspace.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct BotExtras {
    /// bot id → workspace ids
    pub workspaces: std::collections::HashMap<String, Vec<String>>,
    /// bot id → memory count in the workspace
    pub memories: std::collections::HashMap<String, i64>,
}

/// Storage operations, one per repo.ts function the API layer calls. Writes publish their bus events
/// (清单 §3.3) inside the store, exactly where repo.ts does. Every method has a default that reports
/// "not implemented" so the binary compiles before every adapter method exists; the contract test
/// shows which ones are still missing.
#[allow(unused_variables)]
pub trait Repo {
    // ── batched reads for /api/state (optional; `None` = the API falls back to per-row calls) ──
    /// `summary()` extras of every task item of `ws`, keyed by item id (items without rows may be absent).
    fn item_extras(
        &self,
        ws: &str,
    ) -> BResult<Option<std::collections::HashMap<String, ItemExtras>>> {
        Ok(None)
    }
    /// Queued rows of the sessions `sessions_list(ws)` returns, keyed by session id.
    fn queued_by_session(
        &self,
        ws: &str,
    ) -> BResult<Option<std::collections::HashMap<String, Vec<Obj>>>> {
        Ok(None)
    }
    fn bot_extras(&self, ws: &str) -> BResult<Option<BotExtras>> {
        Ok(None)
    }
    /// `events_after` with `data` as the stored JSON text, row by row: `(seq, ts, kind, data)`.
    /// `false` = not supported here (the caller uses `events_after`). Rows are handed over one at
    /// a time so a large page is never held twice.
    fn events_raw(
        &self,
        session: &str,
        after: i64,
        each: &mut dyn FnMut(RawEvent),
    ) -> BResult<bool> {
        Ok(false)
    }

    /// Opt-in byte/row bounded event pages. Short pages are not EOF; callers continue
    /// by seq until empty. Backends without this capability fall back to legacy pages.
    fn events_page_raw(
        &self,
        session: &str,
        after: i64,
        each: &mut dyn FnMut(RawEvent),
    ) -> BResult<bool> {
        Ok(false)
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
    fn items_all(&self, ws: &str) -> BResult<Vec<Obj>> {
        todo_backend!("items_all")
    }
    /// Non-archived task items, updated DESC.
    fn items_list(&self, ws: &str) -> BResult<Vec<Obj>> {
        todo_backend!("items_list")
    }
    fn item_get(&self, id: &str) -> BResult<Option<Obj>> {
        todo_backend!("item_get")
    }
    /// `items.patch`: present keys only (booleans → 0/1, `plan`/`snooze` JSON-encoded), bumps `updated` and
    /// publishes `item` unless `patch` is empty. Returns the item as read back.
    fn item_patch(&self, id: &str, patch: &Obj) -> BResult<Option<Obj>> {
        todo_backend!("item_patch")
    }
    fn item_remove(&self, id: &str) -> BResult<()> {
        todo_backend!("item_remove")
    }
    /// `[{version, text, created}]` by version.
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
    /// `{follow, origin_msg}` of an item made from the conversation (thread-routing §5), else `None`.
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
    /// `sessions.list(ws)`: LIMIT 300, updated DESC.
    fn sessions_list(&self, ws: &str) -> BResult<Vec<Obj>> {
        todo_backend!("sessions_list")
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
    /// `[{seq, ts, kind, data}]` with `seq > after`, by seq, LIMIT 5000.
    fn events_after(&self, session: &str, after: i64) -> BResult<Vec<Obj>> {
        todo_backend!("events_after")
    }
    fn queued_get(&self, id: &str) -> BResult<Option<Obj>> {
        todo_backend!("queued_get")
    }
    /// Full queued rows of a session by created.
    fn queued_list(&self, session: &str) -> BResult<Vec<Obj>> {
        todo_backend!("queued_list")
    }
    fn queued_drop(&self, id: &str) -> BResult<()> {
        todo_backend!("queued_drop")
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
    fn decisions_open(&self, ws: &str) -> BResult<Vec<Obj>> {
        todo_backend!("decisions_open")
    }
    /// Resolved/expired with `resolved > since`, resolved DESC, LIMIT 100.
    fn decisions_recent(&self, ws: &str, since: i64) -> BResult<Vec<Obj>> {
        todo_backend!("decisions_recent")
    }
    fn decision_get(&self, id: &str) -> BResult<Option<Obj>> {
        todo_backend!("decision_get")
    }
    fn decisions_of_item(&self, item: &str) -> BResult<Vec<Obj>> {
        todo_backend!("decisions_of_item")
    }
    /// The last 200 thread messages, ascending.
    fn messages_list(&self, ws: &str) -> BResult<Vec<Obj>> {
        self.messages_in(ws, "steve", 200)
    }
    /// `messages.list(ws, space, limit)`: the latest `limit` of a space, ascending.
    fn messages_in(&self, ws: &str, space: &str, limit: i64) -> BResult<Vec<Obj>> {
        todo_backend!("messages_in")
    }
    /// Up to `limit` messages of a space created in `[after, before)`, the newest of them, oldest first.
    fn messages_before(
        &self,
        ws: &str,
        space: &str,
        before: i64,
        after: i64,
        limit: i64,
    ) -> BResult<Vec<Obj>> {
        todo_backend!("messages_before")
    }
    /// Messages of a space whose text contains `q` or whose item is `q`, newest first.
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
    /// Upserts one raw `settings` value (JSON text) without a revision row and without publishing.
    fn setting_put_raw(&self, ws: &str, key: &str, value: &str) -> BResult<()> {
        todo_backend!("setting_put_raw")
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
    pub bot: String,
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
    /// Starts the domain's own background jobs (release watcher, hourly housekeeping); `false` means it has
    /// none and the Hub runs its own ports of them.
    fn spawn_background(&self) -> bool {
        false
    }

    // ── items ──
    async fn create_item(&self, p: CreateItem) -> BResult<Obj> {
        todo_backend!("create_item")
    }
    async fn route(&self, ws: &str, text: &str) -> BResult<Route> {
        todo_backend!("route")
    }
    /// Gives the items without exactly one category theirs (`dry`: only says what it would do).
    async fn kinds_backfill(&self, ws: &str, dry: bool) -> BResult<Value> {
        todo_backend!("kinds_backfill")
    }
    /// The filled-in categories the user should look at.
    async fn kinds_review(&self, ws: &str) -> BResult<Value> {
        todo_backend!("kinds_review")
    }
    /// The user looked at these; returns what is left.
    async fn kinds_reviewed(&self, ws: &str, ids: Vec<String>) -> BResult<Value> {
        todo_backend!("kinds_reviewed")
    }
    async fn item_action(&self, id: &str, action: &str, actor: &str) -> BResult<Obj> {
        todo_backend!("item_action")
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
    /// The lifecycle projections of the task items of `ws`, keyed by item id.
    async fn lifecycle_views(&self, ws: &str) -> BResult<std::collections::HashMap<String, Value>> {
        Ok(Default::default())
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
    /// A message the paste guard caught: stored replaced, answered with `guard::REPLY`, not routed.
    async fn say_guarded(&self, ws: &str, stored: &str) -> BResult<()> {
        todo_backend!("say_guarded")
    }
    async fn stop(&self, ws: &str) -> BResult<bool> {
        todo_backend!("stop")
    }
    /// Whether Steve is on a turn in the main conversation (the page's stop button after a reload).
    async fn thread_busy(&self, ws: &str) -> BResult<bool> {
        Ok(false)
    }
    /// Rewrites a main-conversation message still waiting its turn; `false` once it is being handled.
    async fn thread_queued_edit(&self, ws: &str, message: &str, text: &str) -> BResult<bool> {
        todo_backend!("thread_queued_edit")
    }
    /// Withdraws a message still waiting its turn: `{text, files}` as it was, `None` once it is being handled.
    async fn thread_queued_withdraw(&self, ws: &str, message: &str) -> BResult<Option<Obj>> {
        todo_backend!("thread_queued_withdraw")
    }
    async fn promote(&self, ws: &str, message: &str) -> BResult<Obj> {
        todo_backend!("promote")
    }
    /// Thread routing (thread-routing §3.4–3.5): the result `{message_id, item?}` and whether it had been
    /// handled before (the API answers 409 then). `to`: `inline` | `item` | `item:<ST-n>`.
    async fn reroute(&self, ws: &str, message: &str, to: &str) -> BResult<(Obj, bool)> {
        todo_backend!("reroute")
    }
    async fn offer(&self, ws: &str, message: &str, take: bool) -> BResult<(Obj, bool)> {
        todo_backend!("offer")
    }
    /// `choice`: `here` | `item` | `new`.
    async fn suggest(&self, ws: &str, message: &str, choice: &str) -> BResult<(Obj, bool)> {
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
    ) -> BResult<String> {
        todo_backend!("claude_print")
    }

    // ── 设置 · 模型 ──
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
