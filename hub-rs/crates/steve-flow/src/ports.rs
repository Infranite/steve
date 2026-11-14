//! What the flow module needs from steve-core, as a small trait the integrator implements:
//! [`Core`] = `createItem`, `itemAction(pause|resume)`, `post_steve_message`, `digestText` and
//! `supplement` (the `item.link` action). Async
//! so the integrator can wrap the synchronous orchestrator in `spawn_blocking`; never called while a
//! store transaction is open. Time and the model gateway come from B1 (`Modules::clock`,
//! `Modules::gateway`).

use async_trait::async_trait;
use serde_json::Value;

/// An item a schedule creates (steve-core `CreateItem`).
#[derive(Debug, Clone, PartialEq)]
pub struct ItemRequest {
    /// Explicit requester of a personal schedule; None retains workspace service semantics.
    pub requester: Option<String>,
    pub ws: String,
    pub title: String,
    pub goal: String,
    /// Always an existing, enabled bot of the workspace (the scheduler falls back to the main bot).
    pub bot: String,
    /// `None` = the bot's default project.
    pub project: Option<String>,
    pub tag_ids: Vec<String>,
    /// `["定时", "sched"]`.
    pub source: (String, String),
    /// `mode: "propose"`: start with a 方案 decision instead of a session.
    pub propose: bool,
    /// Set the new item's `accept_mode` to `auto` (mode `item_auto`).
    pub accept_auto: bool,
    /// The flow run that asked for it (for logs; the `from` link is written by this module).
    pub run_id: String,
}

/// steve-core, as far as schedules need it. Errors are messages for the run's `error` column.
#[async_trait]
pub trait Core: Send + Sync {
    /// `createItem(...)` (+ `accept_mode=auto` when asked); returns the item id (`ST-12`).
    async fn create_item(&self, req: ItemRequest) -> Result<String, String>;
    /// `itemAction(id, "pause", "steve")`.
    async fn pause_item(&self, ws: &str, item_id: &str) -> Result<(), String>;
    /// `itemAction(id, "resume", "steve")`.
    async fn resume_item(&self, ws: &str, item_id: &str) -> Result<(), String>;
    /// Steve posts `text` in the main thread with `meta` (`post_steve_message`).
    async fn post_steve_message(&self, ws: &str, text: &str, meta: Value) -> Result<(), String>;
    /// The `/汇报` text (`digestText`).
    async fn digest_text(&self, ws: &str) -> Result<String, String>;
    /// `supplement(item, text, mode)` (`mode`: `auto`), for `item.link`. The default refuses, so an
    /// unwired Hub reports the step as failed instead of dropping the supplement silently.
    async fn supplement(
        &self,
        ws: &str,
        item_id: &str,
        text: &str,
        mode: &str,
    ) -> Result<(), String> {
        let _ = (ws, item_id, text, mode);
        Err("补充事项还没有接上".into())
    }
}
