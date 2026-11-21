//! Read-only device observations. Never use the Hub's merged model catalogue to describe a
//! particular machine. Calls use the target Node's existing probe/cache, without changing its
//! provider or login. Cached failures retain the last success for display, never for admission.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

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

#[derive(Clone)]
struct ProbedReport {
    report: Report,
    connection: Option<u64>,
    fresh: bool,
}
pub struct Inventory {
    cache: steve_runtime::cache::Cached<String, ProbedReport>,
    caches: Arc<steve_runtime::cache::Caches>,
}
static INVENTORY_SPACE: steve_runtime::cache::CacheSpace<String, ProbedReport> =
    steve_runtime::cache::CacheSpace {
        name: "node-harnesses",
        ttl: MAX_AGE,
        // Preserve the last report during a slow/offline refresh; never use it for admission.
        stale_for: Duration::from_secs(300),
        share_pct: 10,
        tags: |key| {
            vec![
                steve_runtime::cache::Tag::of("node", key),
                steve_runtime::cache::Tag::of("node-inventory", key),
            ]
        },
        weigh: |value| {
            serde_json::to_vec(&value.report).map_or(1, |b| b.len().min(u32::MAX as usize) as u32)
        },
    };
impl Inventory {
    pub fn new(caches: Arc<steve_runtime::cache::Caches>) -> Self {
        Self {
            cache: caches.space(&INVENTORY_SPACE),
            caches,
        }
    }
}

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
    fn connection(t: &Target) -> Option<u64> {
        t.remote.as_ref().map_or(t.generation, |n| n.generation())
    }
    async fn view(&self, t: &Target) -> Report {
        let cached = self.cache.peek(&t.id).await;
        let (mut report, current) = cached.map_or_else(
            || (Report::default(), false),
            |(v, current)| {
                (
                    v.report.clone(),
                    current && v.connection == Self::connection(t),
                )
            },
        );
        let linked = t.node.is_some() && t.current();
        report.stale =
            report.checked_at.is_some() && (!linked || !current || report.state != "reported");
        if !linked {
            report.state = "offline";
        } else if !current {
            report.state = "unreported";
        }
        report
    }
    async fn refresh(&self, t: &Target, fresh: bool) -> Report {
        let Some(node) = &t.node else {
            return self.view(t).await;
        };
        let old = self.cache.peek(&t.id).await.map(|(v, _)| v);
        let tag = steve_runtime::cache::Tag::of("node-inventory", &t.id);
        if old
            .as_ref()
            .is_some_and(|v| fresh || v.connection != t.generation)
        {
            self.caches.invalidate(std::slice::from_ref(&tag));
        }
        // A manual refresh that joined automatic discovery gets one genuinely fresh probe.
        // This is request semantics, not a second cache or an unbounded publication loop.
        for attempt in 0..2 {
            let result = self
                .cache
                .get_or_load(t.id.clone(), || async {
                    let mut report = old.as_ref().map(|v| v.report.clone()).unwrap_or_default();
                    let result = tokio::time::timeout(PROBE_TIMEOUT, node.probe(fresh)).await;
                    if !t.current() {
                        report.state = "offline";
                        report.stale = report.checked_at.is_some();
                        return Ok::<_, ()>(ProbedReport {
                            report,
                            connection: t.generation,
                            fresh,
                        });
                    }
                    report.attempted_at = Some(steve_store::now());
                    report.refreshing = false;
                    let (state, error) = match result {
                        Ok(Ok(c)) if c.node.as_str() == t.id => {
                            report.checked_at = report.attempted_at;
                            report.harnesses = c.harnesses.into_iter().map(harness).collect();
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
                    report.state = state;
                    report.error = error;
                    report.stale = state != "reported" && report.checked_at.is_some();
                    Ok(ProbedReport {
                        report,
                        connection: t.generation,
                        fresh,
                    })
                })
                .await
                .expect("inventory loaders return report values");
            if !t.current()
                || (result.connection == t.generation && (!fresh || result.fresh))
                || attempt == 1
            {
                return result.report.clone();
            }
            self.caches.invalidate(std::slice::from_ref(&tag));
        }
        unreachable!("bounded refresh always returns")
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
        // Without retained entries the list must return the direct observation; a
        // background probe alone would leave every later diagnostic list cold too.
        if st.cfg.cache.mode == steve_runtime::cache::CacheMode::Off {
            nodes.insert(t.id.clone(), st.node_harnesses.refresh(&t, false).await);
            continue;
        }
        let due = t.node.is_some()
            && st
                .node_harnesses
                .cache
                .peek(&t.id)
                .await
                .is_none_or(|(value, current)| !current || value.connection != t.generation);
        let mut report = st.node_harnesses.view(&t).await;
        report.refreshing = due;
        nodes.insert(t.id.clone(), report);
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
    let report = st
        .node_harnesses
        .refresh(&t, q.fresh.as_deref() == Some("1"))
        .await;
    Ok(Json(json!({ "node_id": id, "report": report })))
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

pub(crate) const CACHE_SPEC: (&str, u8) = (INVENTORY_SPACE.name, INVENTORY_SPACE.share_pct);

#[cfg(test)]
pub(crate) fn register_cache_spaces(caches: &steve_runtime::cache::Caches) {
    let _ = caches.space(&INVENTORY_SPACE);
}
