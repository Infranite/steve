//! How each Node's machine is doing (B3): about once a minute the Hub asks every Node it can
//! reach (its own, and each linked remote one) for its vitals, notes how long the answer took,
//! and keeps a day of them (`node_vitals`). The devices page shows them; choosing where work goes
//! can read them. A Node that cannot say (an earlier release) is just not shown.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::response::Response;
use serde_json::{Value, json};
use steve_node_api::{Node, NodeId};
use steve_store::Store;

use super::NodeService;
use crate::error::json_response;

pub const EVERY: Duration = Duration::from_secs(60);
/// An answer later than this is not a sample.
const ASK: Duration = Duration::from_secs(15);
/// The most points of a day the page gets.
const POINTS: usize = 288;

/// Samples until the Hub stops. `local`: the Node in the Hub's own process (`"local"`).
pub async fn sample(
    store: Store,
    local: Option<Arc<dyn Node>>,
    svc: Option<Arc<NodeService>>,
    low: u64,
) {
    loop {
        sample_once(&store, local.as_ref(), svc.as_ref()).await;
        warn_low_disks(&store, low).await;
        tokio::time::sleep(EVERY).await;
    }
}

/// One round: every Node reachable now asked at once, what answers kept.
pub async fn sample_once(
    store: &Store,
    local: Option<&Arc<dyn Node>>,
    svc: Option<&Arc<NodeService>>,
) {
    {
        let mut targets: Vec<(String, Arc<dyn Node>, bool)> = vec![];
        if let Some(n) = local {
            targets.push(("local".into(), n.clone(), false));
        }
        if let Some(svc) = svc {
            for id in svc.registry.linked() {
                if let Ok(node) = NodeId::new(id.clone()) {
                    // The explicitly colocated service is still this machine, not a remote RTT.
                    let remote = id != "local";
                    targets.push((id, svc.fleet.remote(&node), remote));
                }
            }
        }
        let asks = targets.into_iter().map(|(id, node, remote)| async move {
            let t0 = Instant::now();
            let r = tokio::time::timeout(ASK, node.vitals()).await;
            let rtt = remote.then(|| t0.elapsed().as_millis() as i64);
            (id, r, rtt)
        });
        for (id, r, rtt) in futures::future::join_all(asks).await {
            // the link's latency, for the devices page, whatever the Node answered
            if let (Some(ms), Some(svc), Ok(Ok(_))) = (rtt, svc, &r) {
                svc.registry.set_rtt(&id, ms, super::now());
            }
            let v = match r {
                Ok(Ok(v)) => v,
                Ok(Err(e)) => {
                    tracing::debug!(node = %id, "vitals: {}", e.safe_detail);
                    continue;
                }
                Err(_) => {
                    tracing::debug!(node = %id, "vitals: no answer");
                    continue;
                }
            };
            let Ok(data) = serde_json::to_string(&v) else {
                continue;
            };
            let store = store.clone();
            let at = super::now();
            let saved = tokio::task::spawn_blocking(move || {
                store.transaction(|tx| steve_store::nodes::vitals_put(tx, &id, at, rtt, &data))
            })
            .await;
            if !matches!(saved, Ok(Ok(()))) {
                tracing::warn!("keeping vitals: {saved:?}");
            }
        }
    }
}

/// Remote Nodes whose roots are short of space now, said once per episode (F1): the Hub's own
/// machine has its own notice (the worktree watch). `low`: `STEVE_DISK_LOW_GB`.
static WARNED: std::sync::LazyLock<std::sync::Mutex<std::collections::HashSet<String>>> =
    std::sync::LazyLock::new(Default::default);

pub async fn warn_low_disks(store: &Store, low: u64) {
    let store = store.clone();
    let _ = tokio::task::spawn_blocking(move || {
        let since = super::now() - 5 * 60 * 1000;
        let rows: Vec<(String, String, String)> = store
            .reader(|c| {
                let mut st = c.prepare(
                    "SELECT v.node_id, n.name, v.data FROM node_vitals v JOIN nodes n ON n.node_id = v.node_id
                      WHERE n.local = 0 AND v.at >= ?
                        AND v.at = (SELECT MAX(at) FROM node_vitals w WHERE w.node_id = v.node_id)",
                )?;
                let v = st
                    .query_map([since], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok::<_, steve_store::StoreError>(v)
            })
            .unwrap_or_default();
        let mut warned = WARNED.lock().unwrap();
        for (id, name, data) in rows {
            let Ok(v) = serde_json::from_str::<Value>(&data) else { continue };
            let short = v["roots"]
                .as_array()
                .into_iter()
                .flatten()
                .filter_map(|r| r["free"].as_u64())
                .min()
                .filter(|free| *free < low);
            match short {
                Some(free) if warned.insert(id.clone()) => {
                    let text = steve_i18n::t!(
                        "hub.node.disk_low",
                        name = name,
                        free = steve_core::workspace::human(free)
                    );
                    let db = steve_core::repo::Db::new(store.clone());
                    if let Err(e) =
                        steve_core::notify::notify(&db, steve_store::WS, "disk", "info", &text, None)
                    {
                        tracing::error!("node disk notice: {e}");
                    }
                }
                Some(_) => {}
                None => {
                    warned.remove(&id);
                }
            }
        }
    })
    .await;
}

/// One Node's day as the page draws it: the latest sample whole, and at most [`POINTS`] points
/// of load (1 min, per CPU), memory in use (a fraction) and round trip.
pub fn day(rows: &[(i64, Option<i64>, String)]) -> Option<Value> {
    let parsed: Vec<(i64, Option<i64>, Value)> = rows
        .iter()
        .filter_map(|(at, rtt, d)| Some((*at, *rtt, serde_json::from_str(d).ok()?)))
        .collect();
    let (at, rtt, last) = parsed.last()?.clone();
    let step = parsed.len().div_ceil(POINTS).max(1);
    let series: Vec<Value> = parsed
        .chunks(step)
        .map(|chunk| {
            let (at, rtt, v) = chunk.last().unwrap();
            let cpus = v["cpus"].as_f64().filter(|c| *c > 0.0);
            let load = cpus.and_then(|c| v["load"][0].as_f64().map(|l| l / c));
            let mem = match (v["mem_total"].as_f64(), v["mem_available"].as_f64()) {
                (Some(t), Some(a)) if t > 0.0 && a >= 0.0 && a <= t => Some(1.0 - a / t),
                _ => None,
            };
            json!({ "at": at, "load": load, "mem": mem, "rtt": rtt })
        })
        .collect();
    // Missing raw sampling intervals survive decimation. The UI breaks paths across these ranges.
    let gaps: Vec<Value> = parsed
        .windows(2)
        .filter(|w| w[1].0 - w[0].0 > (EVERY + ASK).as_millis() as i64)
        .map(|w| json!({"from": w[0].0, "to": w[1].0}))
        .collect();
    let mut latest = last;
    latest["sampled_at"] = latest.get("at").cloned().unwrap_or(json!(at));
    latest["at"] = json!(at);
    latest["rtt_ms"] = json!(rtt);
    Some(json!({ "latest": latest, "series": series, "gaps": gaps,
        "sampling": {"interval_ms": EVERY.as_millis() as u64,
            "stale_after_ms": (EVERY * 2 + ASK).as_millis() as u64,
            "now": super::now()} }))
}

/// `GET /api/vitals`: the Hub's own Node, and the remote Nodes the caller paired.
pub async fn get(
    State(st): State<crate::api::AppState>,
    caller: steve_modules::Caller,
) -> Response {
    let Some(store) = st.modules.as_ref().map(|m| m.store.clone()) else {
        return json_response(200, &json!({ "nodes": {} }));
    };
    let r = tokio::task::spawn_blocking(move || {
        store.reader(|c| {
            let mut ids = vec!["local".to_string()];
            ids.extend(
                steve_store::nodes::list_for_owner(c, &caller.me)?
                    .into_iter()
                    .filter(|n| n.state == "active" || n.state == "paused")
                    .map(|n| n.node_id),
            );
            let since = super::now() - steve_store::nodes::VITALS_KEEP_MS;
            let mut out = serde_json::Map::new();
            for id in ids {
                let rows = steve_store::nodes::vitals_since(c, &id, since)?;
                if let Some(d) = day(&rows) {
                    out.insert(id, d);
                }
            }
            Ok::<_, steve_store::StoreError>(Value::Object(out))
        })
    })
    .await;
    match r {
        Ok(Ok(nodes)) => json_response(200, &json!({ "nodes": nodes })),
        Ok(Err(e)) => {
            tracing::error!("vitals: {e}");
            json_response(500, &json!({ "error": "internal" }))
        }
        Err(_) => json_response(500, &json!({ "error": "internal" })),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_day_is_the_latest_and_a_bounded_series() {
        let row = |at: i64, load: f64, avail: u64| {
            (
                at,
                Some(12),
                json!({"at": at, "cpus": 4, "load": [load, 0, 0], "mem_total": 1000, "mem_available": avail, "sessions": 1})
                    .to_string(),
            )
        };
        let rows: Vec<_> = (0..1000).map(|i| row(i, 2.0, 250)).collect();
        let d = day(&rows).unwrap();
        assert!(d["series"].as_array().unwrap().len() <= POINTS);
        assert_eq!(d["series"][0]["load"], 0.5);
        assert_eq!(d["series"][0]["mem"], 0.75);
        assert_eq!(d["latest"]["at"], 999);
        assert_eq!(d["latest"]["rtt_ms"], 12);
        assert!(day(&[]).is_none());
    }
}

#[cfg(test)]
mod evidence_tests {
    use super::*;
    #[test]
    fn missing_cpu_stays_unknown_and_sampling_gaps_survive() {
        let d = day(&[
            (1, None, json!({"at":1,"load":[2,1,0]}).to_string()),
            (
                180001,
                None,
                json!({"at":180000,"cpus":4,"load":[8,4,2]}).to_string(),
            ),
        ])
        .unwrap();
        assert!(d["series"][0]["load"].is_null());
        assert_eq!(d["series"][1]["load"], 2.0);
        assert_eq!(d["gaps"][0], json!({"from":1,"to":180001}));
        assert_eq!(d["sampling"]["stale_after_ms"], 135000);
        assert_eq!(d["latest"]["sampled_at"], 180000);
        assert!(d["latest"]["rtt_ms"].is_null());
    }
}
