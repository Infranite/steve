//! Read-only device observations. Never use the Hub's merged model catalogue to describe a
//! particular machine. Calls use the target Node's existing probe/cache, without changing its
//! provider or login. Cached failures retain the last success for display, never for admission.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::{Path, Query, State};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use steve_modules::Caller;
use steve_node_api::{AdapterStatus, ErrorCode, HarnessCapability, Node, NodeId, SetupState};

use super::AppState;
use crate::error::ApiError;

const MAX_AGE: Duration = Duration::from_secs(60);
const PROBE_TIMEOUT: Duration = Duration::from_secs(45);

#[derive(Clone, Default, Serialize)]
pub struct Report {
    state: &'static str,
    checked_at: Option<i64>,
    attempted_at: Option<i64>,
    stale: bool,
    refreshing: bool,
    error: Option<String>,
    harnesses: Vec<Value>,
}

#[derive(Default)]
struct Observation {
    report: Report,
    attempted: Option<Instant>,
    generation: Option<u64>,
    fresh: bool,
}

#[derive(Default)]
struct Entry {
    value: Mutex<Observation>,
    probing: tokio::sync::Mutex<()>,
}

struct Refreshing(Arc<Entry>);
impl Drop for Refreshing {
    fn drop(&mut self) {
        self.0.value.lock().unwrap().report.refreshing = false;
    }
}

#[derive(Default)]
pub struct Inventory(Mutex<BTreeMap<String, Arc<Entry>>>);

struct Target {
    id: String,
    node: Option<Arc<dyn Node>>,
    // A remote link is checked again after awaiting the probe, before publishing its answer.
    remote: Option<Arc<steve_node_wire::RemoteNode>>,
    generation: Option<u64>,
}

impl Target {
    fn current(&self) -> bool {
        self.remote
            .as_ref()
            .is_none_or(|n| n.generation() == self.generation)
    }
}

fn harness(c: HarnessCapability) -> Value {
    let a = c.adapter.as_ref();
    let available = if c
        .setup
        .as_ref()
        .is_some_and(|s| s.state == SetupState::Unknown)
    {
        None
    } else if c
        .setup
        .as_ref()
        .is_some_and(|s| s.state != SetupState::Ready)
    {
        Some(false)
    } else {
        // Older Nodes do not prove session readiness; a declared capability is not a probe.
        a.and_then(|a| {
            if a.status == AdapterStatus::Ready && a.session.is_none() && a.session_error.is_none()
            {
                None
            } else {
                Some(a.status == AdapterStatus::Ready && a.session.is_some())
            }
        })
    };
    let models: Vec<String> = match a.and_then(|a| a.session.as_ref()) {
        Some(s) => s.models.iter().map(|m| m.id.clone()).collect(),
        None => c.models,
    };
    json!({
        "id": c.name, "name": a.map(|a| a.title.as_str()).filter(|s| !s.is_empty()).unwrap_or(&c.name),
        "available": available, "version": c.version,
        "adapter_version": a.and_then(|a| a.version.as_deref()), "models": models,
        "error": a.and_then(|a| a.session_error.as_deref().or(a.error.as_deref())),
        "setup": c.setup,
    })
}

impl Inventory {
    fn entry(&self, id: &str) -> Arc<Entry> {
        self.0.lock().unwrap().entry(id.into()).or_default().clone()
    }

    fn view(&self, t: &Target) -> Report {
        let e = self.entry(&t.id);
        let v = e.value.lock().unwrap();
        let mut r = v.report.clone();
        let generation = t.remote.as_ref().map_or(t.generation, |n| n.generation());
        let linked = t.node.is_some() && (t.remote.is_none() || generation.is_some());
        r.stale = r.checked_at.is_some()
            && (!linked
                || v.generation != generation
                || v.attempted.is_none_or(|at| at.elapsed() >= MAX_AGE)
                || r.state != "reported");
        if !linked {
            r.state = "offline";
        } else if v.generation != generation || v.attempted.is_none() {
            r.state = "unreported";
        }
        r
    }

    async fn refresh(&self, t: &Target, fresh: bool) {
        let Some(node) = &t.node else { return };
        let requested = Instant::now();
        let e = self.entry(&t.id);
        let _single = e.probing.lock().await;
        {
            let mut v = e.value.lock().unwrap();
            if !t.current()
                || (v.generation == t.generation
                    && v.attempted.is_some_and(|at| {
                        (at >= requested && (!fresh || v.fresh))
                            || (!fresh && at.elapsed() < MAX_AGE)
                    }))
            {
                return;
            }
            v.report.refreshing = true;
        }
        let _refreshing = Refreshing(e.clone());
        let result = tokio::time::timeout(PROBE_TIMEOUT, node.probe(fresh)).await;
        let mut v = e.value.lock().unwrap();
        // A reconnect while a call is held must not certify the new connection.
        if !t.current() {
            return;
        }
        v.generation = t.generation;
        v.attempted = Some(Instant::now());
        v.fresh = fresh;
        let now = steve_store::now();
        v.report.attempted_at = Some(now);
        let (state, error) = match result {
            Ok(Ok(c)) if c.node.as_str() == t.id => {
                v.report.checked_at = Some(now);
                v.report.harnesses = c.harnesses.into_iter().map(harness).collect();
                ("reported", None)
            }
            Ok(Ok(_)) => (
                "failed",
                Some("probe returned a different Node identity".into()),
            ),
            Ok(Err(e)) => (
                if e.code == ErrorCode::Unsupported {
                    "unsupported"
                } else {
                    "failed"
                },
                Some(e.safe_detail),
            ),
            Err(_) => ("failed", Some("Node harness probe timed out".into())),
        };
        v.report.state = state;
        v.report.error = error;
    }
}

async fn targets(st: &AppState, caller: Caller) -> Result<Vec<Target>, ApiError> {
    // A colocated service Node is adopted by the fleet too; the in-process Node is not.
    // Fence its reports by connection generation just as for other machines.
    let local_remote = st.node.as_ref().and_then(|runtime| {
        let svc = st.nodes.as_ref()?;
        svc.fleet
            .runtime_for("local")
            .filter(|r| Arc::ptr_eq(r, runtime))
            .map(|_| svc.fleet.remote(&NodeId::new("local").expect("static id")))
    });
    let local_generation = local_remote.as_ref().and_then(|n| n.generation());
    let mut out = vec![Target {
        id: "local".into(),
        node: st
            .node
            .as_ref()
            .filter(|_| local_remote.is_none() || local_generation.is_some())
            .map(|n| n.node()),
        remote: local_remote,
        generation: local_generation,
    }];
    if let Some(svc) = &st.nodes {
        let store = svc.store.clone();
        let rows = tokio::task::spawn_blocking(move || {
            store.reader(|c| steve_store::nodes::list_for_owner(c, &caller.me))
        })
        .await
        .map_err(|_| ApiError::Json(500, "internal".into()))?
        .map_err(|_| ApiError::Json(500, "internal".into()))?;
        for n in rows
            .into_iter()
            .filter(|n| !n.local && (n.state == "active" || n.state == "paused"))
        {
            let id = NodeId::new(n.node_id.clone())
                .map_err(|_| ApiError::Json(500, "invalid Node identity".into()))?;
            let remote = svc.fleet.remote(&id);
            let generation = remote.generation();
            out.push(Target {
                id: n.node_id,
                node: generation.map(|_| remote.clone() as Arc<dyn Node>),
                remote: Some(remote),
                generation,
            });
        }
    }
    Ok(out)
}

/// Fast cached list; slow probes run independently per machine. Polling cannot queue duplicate
/// work behind a slow Node. Reconnection changes its generation and bypasses the one-minute TTL.
pub async fn list(State(st): State<AppState>, caller: Caller) -> Result<Json<Value>, ApiError> {
    let targets = targets(&st, caller).await?;
    let mut nodes = BTreeMap::new();
    for t in targets {
        let e = st.node_harnesses.entry(&t.id);
        let due = {
            let v = e.value.lock().unwrap();
            t.node.is_some()
                && !v.report.refreshing
                && (v.generation != t.generation
                    || v.attempted.is_none_or(|at| at.elapsed() >= MAX_AGE))
        };
        nodes.insert(t.id.clone(), st.node_harnesses.view(&t));
        if due {
            let st = st.clone();
            tokio::spawn(async move {
                st.node_harnesses.refresh(&t, false).await;
            });
        }
    }
    Ok(Json(json!({ "nodes": nodes })))
}

#[derive(Default, Deserialize)]
pub struct Refresh {
    fresh: Option<String>,
}

/// Explicit single-Node refresh; concurrent requests share the result of the in-flight probe.
pub async fn get(
    State(st): State<AppState>,
    caller: Caller,
    Path(id): Path<String>,
    Query(q): Query<Refresh>,
) -> Result<Json<Value>, ApiError> {
    let t = targets(&st, caller)
        .await?
        .into_iter()
        .find(|t| t.id == id)
        .ok_or_else(|| ApiError::Json(404, "Node not found".into()))?;
    st.node_harnesses
        .refresh(&t, q.fresh.as_deref() == Some("1"))
        .await;
    Ok(Json(
        json!({ "node_id": id, "report": st.node_harnesses.view(&t) }),
    ))
}

#[cfg(test)]
mod tests;

/// Routes mounted behind the shared API login and body-limit layers.
pub(crate) fn routes() -> axum::Router<super::AppState> {
    use super::*;
    Router::new()
        .route("/node-harnesses", get(node_harnesses::list))
        .route("/node-harnesses/{id}", get(node_harnesses::get))
}
