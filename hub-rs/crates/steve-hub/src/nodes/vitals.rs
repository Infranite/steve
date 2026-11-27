//! Sample each reachable machine into the Hub's telemetry recorder.

use std::collections::{BTreeSet, HashSet};
use std::sync::Arc;
use std::time::Instant;

use axum::extract::State;
use axum::response::{IntoResponse, Response};
use steve_node_api::{Node, NodeId};
use steve_store::Store;
use steve_types::devices::VitalsResponse;

use super::NodeService;
use crate::metrics::{
    self,
    nodes::{ASK, EVERY, Observation},
};

pub fn task(
    store: Store,
    local: Option<Arc<dyn Node>>,
    svc: Option<Arc<NodeService>>,
    low: u64,
) -> steve_runtime::Task {
    let warned = Arc::new(tokio::sync::Mutex::new(HashSet::new()));
    steve_runtime::every(
        "nodes.vitals",
        Arc::new(steve_runtime::SystemClock),
        EVERY,
        steve_runtime::FirstRun::Immediate,
        move || {
            let store = store.clone();
            let local = local.clone();
            let svc = svc.clone();
            let warned = warned.clone();
            async move {
                let sample_store = store.clone();
                let approval = tokio::task::spawn_blocking(move || {
                    sample_store
                        .read(|r| steve_store::statistics::approval::applied_sessions_by_mode(r))
                })
                .await;
                if let Ok(Ok(by_mode)) = approval {
                    for (mode, n) in by_mode {
                        ::metrics::gauge!("steve_permission_sessions", "mode" => mode.as_str())
                            .set(n as f64);
                    }
                }
                sample_once(local.as_ref(), svc.as_ref()).await;
                if svc.is_some() {
                    let mut guard = warned.lock().await;
                    let prev = std::mem::take(&mut *guard);
                    *guard = warn_low_disks(store, prev, low).await;
                }
            }
        },
    )
}

pub async fn sample(
    store: Store,
    local: Option<Arc<dyn Node>>,
    svc: Option<Arc<NodeService>>,
    low: u64,
) {
    task(store, local, svc, low).await;
}

/// Ask in parallel, then capture the recorder's minute snapshot.
pub async fn sample_once(local: Option<&Arc<dyn Node>>, svc: Option<&Arc<NodeService>>) {
    let mut targets = Vec::<(String, Arc<dyn Node>, bool)>::new();
    if let Some(svc) = svc {
        for id in svc.registry.linked() {
            if let Ok(node) = NodeId::new(id.clone()) {
                let remote = svc.fleet.remote(&node);
                if !remote.is_upgrade() {
                    targets.push((id, remote, true));
                }
            }
        }
    }
    // The service Node and the in-process jobs Node share a machine. Sample it once.
    if let Some(local) = local
        && !local.is_upgrade_only()
        && !targets.iter().any(|(id, _, _)| id == "local")
    {
        targets.push(("local".into(), local.clone(), false));
    }

    let asks = targets
        .into_iter()
        .map(|(id, node, own_process)| async move {
            let start = Instant::now();
            let vitals = match tokio::time::timeout(ASK, node.vitals()).await {
                Ok(Ok(vitals)) => vitals,
                Ok(Err(e)) => {
                    tracing::debug!(node = %id, "vitals: {}", e.safe_detail);
                    return None;
                }
                Err(_) => {
                    tracing::debug!(node = %id, "vitals: no answer");
                    return None;
                }
            };
            let at = super::now();
            let rtt = (id != "local").then(|| start.elapsed().as_millis() as i64);
            if let (Some(ms), Some(svc)) = (rtt, svc) {
                // RTT also describes the current live link in /api/devices.
                svc.registry.set_rtt(&id, ms, at);
            }
            Some(Observation {
                id,
                at,
                rtt,
                own_process,
                vitals,
            })
        });
    metrics::record_minute(
        futures::future::join_all(asks)
            .await
            .into_iter()
            .flatten()
            .collect(),
    )
    .await;
}

/// Warn once per low-disk episode on a remote Node. The Hub already watches its own disk.
async fn warn_low_disks(store: Store, mut warned: HashSet<String>, low: u64) -> HashSet<String> {
    tokio::task::spawn_blocking(move || {
        let Ok(nodes) = store.read(|r| steve_store::nodes::list_live(r)) else {
            return warned;
        };
        let since = super::now() - 5 * 60 * 1000;
        for node in nodes.into_iter().filter(|node| !node.local) {
            let Some(latest) = metrics::latest_node(&node.node_id, since) else {
                continue;
            };
            let Some(free) = latest
                .roots
                .iter()
                .map(|root| root.free)
                .min()
                .filter(|free| *free < low)
            else {
                warned.remove(&node.node_id);
                continue;
            };
            if warned.insert(node.node_id) {
                let text = steve_i18n::t!(
                    "hub.node.disk_low",
                    name = node.name,
                    free = steve_core::workspace::human(free)
                );
                if let Err(e) = steve_store::notify::notify(
                    &store,
                    steve_store::WS,
                    "disk",
                    "info",
                    &text,
                    None,
                ) {
                    tracing::error!("node disk notice: {e}");
                }
            }
        }
        warned
    })
    .await
    .unwrap_or_default()
}

/// `GET /api/vitals`: the Hub's own Node, and the remote Nodes the caller paired.
pub async fn get(
    State(st): State<crate::api::AppState>,
    caller: steve_modules::Caller,
) -> Response {
    let Some(store) = st.modules.as_ref().map(|m| m.store.clone()) else {
        return axum::Json(VitalsResponse::default()).into_response();
    };
    let result = crate::api::blocking(move || {
        store
            .read(|r| {
                let tz = steve_stats::refresh::timezone(r, &caller.ws)?;
                let mut ids = BTreeSet::from(["local".to_string()]);
                ids.extend(
                    steve_store::nodes::list_for_owner(r, &caller.me)?
                        .into_iter()
                        .filter(|node| node.state == "active" || node.state == "paused")
                        .map(|node| node.node_id),
                );
                let now = super::now();
                let today = steve_stats::day::day(now, tz);
                let minute = now - 60_000;
                let minute_day = steve_stats::day::day(minute, tz);
                let tokens = steve_store::nodes::session_tokens(
                    r,
                    &caller.ws,
                    &today,
                    &minute_day,
                    minute,
                    now,
                )?;
                let costs = steve_stats::metrics::node_costs(r, &caller.ws, &today)?;
                Ok::<_, steve_store::StoreError>((ids, now, tokens, costs))
            })
            .map_err(|e| {
                tracing::error!("vitals: {e}");
                crate::error::ApiError::internal("internal")
            })
    })
    .await;
    match result {
        Ok((ids, now, tokens, costs)) => axum::Json(VitalsResponse {
            nodes: metrics::node_days(ids, now, tokens, costs),
        })
        .into_response(),
        Err(e) => e.into_response(),
    }
}
