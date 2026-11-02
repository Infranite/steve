//! What G3 needs from steve-core to wrap an item up (github.md §4.1, §4.2): set its stage and
//! `now`, finish it, fail it. A small trait the integrator implements over the orchestrator
//! (`item_patch`, `finish`, `fail`); never called while a store transaction is open.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;

/// steve-core, as far as the GitHub wrap-up needs it. Errors are messages for the log.
#[async_trait]
pub trait ItemCore: Send + Sync {
    /// `item_patch(stage, now)`; the item stays open (`run`) while Steve waits for you or for CI.
    async fn progress(&self, ws: &str, item_id: &str, stage: &str, now: &str)
    -> Result<(), String>;
    /// `finish(item, now)`: done, plan ticked, `on_finished` hooks.
    async fn finish(&self, ws: &str, item_id: &str, now: &str) -> Result<(), String>;
    /// `fail(item, why)`.
    async fn fail(&self, ws: &str, item_id: &str, why: &str) -> Result<(), String>;
}

/// Refuses everything, so an unwired Hub logs the missing wrap-up instead of pretending.
pub struct NoCore;

#[async_trait]
impl ItemCore for NoCore {
    async fn progress(&self, _: &str, _: &str, _: &str, _: &str) -> Result<(), String> {
        Err("steve-core 还没有接上".into())
    }
    async fn finish(&self, _: &str, _: &str, _: &str) -> Result<(), String> {
        Err("steve-core 还没有接上".into())
    }
    async fn fail(&self, _: &str, _: &str, _: &str) -> Result<(), String> {
        Err("steve-core 还没有接上".into())
    }
}

/// One call to [`RecordingCore`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreCall {
    Progress {
        item: String,
        stage: String,
        now: String,
    },
    Finish {
        item: String,
        now: String,
    },
    Fail {
        item: String,
        why: String,
    },
}

/// For tests: records the calls and applies them to the `items` row like steve-core would
/// (`status`, `stage`, `now`).
pub struct RecordingCore {
    store: steve_store::Store,
    calls: Mutex<Vec<CoreCall>>,
}

impl RecordingCore {
    pub fn new(store: steve_store::Store) -> Arc<Self> {
        Arc::new(Self {
            store,
            calls: Mutex::default(),
        })
    }

    pub fn calls(&self) -> Vec<CoreCall> {
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn record(&self, c: CoreCall, sql: &str, p: [&str; 3]) -> Result<(), String> {
        self.calls.lock().unwrap_or_else(|e| e.into_inner()).push(c);
        self.store
            .with_conn(|c| Ok(c.execute(sql, p)?))
            .map(|_| ())
            .map_err(|e| e.to_string())
    }
}

#[async_trait]
impl ItemCore for RecordingCore {
    async fn progress(&self, _: &str, item_id: &str, stage: &str, now: &str) -> Result<(), String> {
        self.record(
            CoreCall::Progress {
                item: item_id.into(),
                stage: stage.into(),
                now: now.into(),
            },
            "UPDATE items SET status = 'run', stage = ?, now = ? WHERE id = ?",
            [stage, now, item_id],
        )
    }
    async fn finish(&self, _: &str, item_id: &str, now: &str) -> Result<(), String> {
        self.record(
            CoreCall::Finish {
                item: item_id.into(),
                now: now.into(),
            },
            "UPDATE items SET status = 'done', stage = ?, now = ? WHERE id = ?",
            ["收尾", now, item_id],
        )
    }
    async fn fail(&self, _: &str, item_id: &str, why: &str) -> Result<(), String> {
        self.record(
            CoreCall::Fail {
                item: item_id.into(),
                why: why.into(),
            },
            "UPDATE items SET status = 'wait', stage = ?, now = ? WHERE id = ?",
            ["执行", why, item_id],
        )
    }
}
