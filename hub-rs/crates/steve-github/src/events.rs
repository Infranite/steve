//! Where GitHub changes go: signal events to S1's `steve_flow::ingest`, and
//! the `{type:"gh", id, ws}` SSE notices. Both are small traits here so this crate does not depend
//! on steve-flow or on a `BusEvent` variant that does not exist yet; the integrator forwards them
//! (see the crate docs). `{type:"item", id}` goes straight through the store (`WorkEvent::Item`).

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::Value;

/// One event for the signals pipeline. Field for field `steve_flow::RawEvent` with
/// `source = "github"`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SignalEvent {
    /// `github.check_failed`, `github.pr_merged`, …
    pub type_key: String,
    /// Stable per change, the same whether the webhook or the poll saw it, so S1's dedupe on
    /// `(connection, external_id)` also holds when both paths are live.
    pub external_id: String,
    /// Unix ms.
    pub occurred: i64,
    /// `pr:<ghp>`, `branch:<repo>:<branch>`, `issue:<ghi>`.
    pub thread_key: Option<String>,
    /// `repo:<ghr>`.
    pub group_key: Option<String>,
    /// `owner/name`.
    pub scope_ext_id: Option<String>,
    /// The GitHub user who caused it (`ActorRef::Handle { system: "github", .. }`).
    pub actor: Option<Actor>,
    pub summary: String,
    /// `{repo, number, title, url, head_ref, head_sha, item_id, failed:[{name, job_id, url}]}`.
    pub payload: Value,
}

/// A GitHub account, to be identified by S1 (`family=github_user`; bots become `virtual`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Actor {
    pub login: String,
    /// `User`, `Bot`, `Organization`.
    pub kind: String,
}

/// What G2 needs from S1. The integrator implements it over `steve_flow::EventSink`:
/// `RawEvent { source: "github", type_key, external_id, occurred, thread_key, group_key,
/// scope_ext_id, actor: Handle { system: "github", handle: login, display_name: None,
/// signature_org: None }, summary, payload, .. }` with `conn = Some(connection_id)`.
#[async_trait]
pub trait EventSink: Send + Sync {
    /// `Err` is logged and recorded on the delivery; the state sync it followed is kept.
    async fn ingest(&self, ws: &str, connection_id: &str, ev: SignalEvent) -> Result<(), String>;
}

pub(crate) fn emit(w: &mut steve_store::W<'_>, ws: &str, id: &str) {
    w.emit(steve_types::DomainEvent::from(
        steve_types::IntegrationsEvent::Gh {
            id: id.to_owned(),
            ws: ws.into(),
        },
    ));
}

/// Drops everything (until the integrator wires the real ones).
pub struct NoSink;

#[async_trait]
impl EventSink for NoSink {
    async fn ingest(&self, _: &str, _: &str, _: SignalEvent) -> Result<(), String> {
        Ok(())
    }
}

/// Records what it gets, for tests. Dedupes on `(connection, external_id)` like S1 does, and keeps
/// what it would have dropped in `duplicates`.
#[derive(Default)]
pub struct RecordingSink {
    events: Mutex<Vec<(String, String, SignalEvent)>>,
    duplicates: Mutex<Vec<SignalEvent>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl RecordingSink {
    pub fn new() -> Arc<Self> {
        Arc::new(Self::default())
    }
    /// `(ws, connection, event)` in arrival order.
    pub fn events(&self) -> Vec<(String, String, SignalEvent)> {
        lock(&self.events).clone()
    }
    /// The type keys received, in order.
    pub fn types(&self) -> Vec<String> {
        lock(&self.events)
            .iter()
            .map(|(_, _, e)| e.type_key.clone())
            .collect()
    }
    pub fn duplicates(&self) -> Vec<SignalEvent> {
        lock(&self.duplicates).clone()
    }
    pub fn clear(&self) {
        lock(&self.events).clear();
        lock(&self.duplicates).clear();
    }
}

#[async_trait]
impl EventSink for RecordingSink {
    async fn ingest(&self, ws: &str, connection_id: &str, ev: SignalEvent) -> Result<(), String> {
        let mut all = lock(&self.events);
        if all
            .iter()
            .any(|(_, c, e)| c == connection_id && e.external_id == ev.external_id)
        {
            lock(&self.duplicates).push(ev);
            return Ok(());
        }
        all.push((ws.into(), connection_id.into(), ev));
        Ok(())
    }
}
