//! Owner-only component maintenance, routed to the named Node. Persist intent before sending;
//! receipt polling never resubmits work, including after a link or Hub restart.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    Extension, Json,
    extract::{Path, Query, State},
};
use rusqlite::OptionalExtension;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use steve_node_api as na;
use steve_store::{Store, StoreError};

use super::{AppState, Ctx};
use crate::error::ApiError;

#[derive(Clone, Default, Serialize, Deserialize)]
struct Saved {
    report: Option<na::HarnessUpdateReport>,
    at: i64,
    runs: BTreeMap<String, Run>,
}

#[derive(Clone, Serialize, Deserialize)]
struct Run {
    command: na::Command<na::UpgradeHarness>,
    receipt: na::Receipt,
    #[serde(default)]
    reconciled: bool,
}

fn internal(e: impl std::fmt::Display) -> ApiError {
    ApiError::internal(e.to_string())
}
fn key(node: &str) -> String {
    format!("harness.updates.{node}")
}

fn authorized(
    db: &rusqlite::Connection,
    me: &str,
    node: &str,
    write: bool,
) -> Result<bool, StoreError> {
    let owner: Option<String> = db
        .query_row(
            "SELECT id FROM users WHERE role='owner' ORDER BY created,rowid LIMIT 1",
            [],
            |r| r.get(0),
        )
        .optional()?;
    if owner.as_deref() != Some(me) {
        return Ok(false);
    }
    if node == "local" {
        return Ok(true);
    }
    Ok(steve_store::nodes::get(db, node)?.is_some_and(|n| {
        n.owner == me
            && if write {
                n.state == "active"
            } else {
                matches!(n.state.as_str(), "active" | "paused")
            }
    }))
}

fn load(c: &rusqlite::Connection, node: &str) -> Result<Saved, StoreError> {
    let v: Option<String> = c
        .query_row(
            "SELECT value FROM settings WHERE workspace_id='default' AND key=?",
            [key(node)],
            |r| r.get(0),
        )
        .optional()?;
    v.map(|v| {
        serde_json::from_str(&v)
            .map_err(|_| StoreError::Conflict("invalid saved harness update state".into()))
    })
    .transpose()
    .map(Option::unwrap_or_default)
}

fn save(c: &rusqlite::Connection, node: &str, s: &Saved) -> Result<(), StoreError> {
    let v = serde_json::to_string(s).map_err(|e| StoreError::Conflict(e.to_string()))?;
    c.execute("INSERT INTO settings(workspace_id,key,value,updated) VALUES ('default',?,?,?) ON CONFLICT(workspace_id,key) DO UPDATE SET value=excluded.value,updated=excluded.updated", rusqlite::params![key(node), v, crate::nodes::now()])?;
    Ok(())
}

async fn target(
    st: &AppState,
    c: &Ctx,
    id: &str,
    write: bool,
) -> Result<(Store, Arc<dyn na::Node>), ApiError> {
    let store = st
        .modules
        .as_ref()
        .map(|m| m.store.clone())
        .ok_or_else(|| ApiError::Json(503, "no store".into()))?;
    let (s, me, node) = (store.clone(), c.me.clone(), id.to_string());
    let local = id == "local";
    let allowed = tokio::task::spawn_blocking(move || -> Result<bool, StoreError> {
        s.reader(|db| authorized(db, &me, &node, write))
    })
    .await
    .map_err(internal)?
    .map_err(internal)?;
    if !allowed {
        return Err(ApiError::Json(
            403,
            "Only the instance owner may maintain their active Nodes".into(),
        ));
    }
    if local {
        let node = st
            .node
            .as_ref()
            .filter(|n| n.node_id().as_str() == id)
            .ok_or_else(|| ApiError::Json(409, "local Node is disabled".into()))?
            .node();
        return Ok((store, node));
    }
    let node = na::NodeId::new(id).map_err(internal)?;
    let svc = st
        .nodes
        .as_ref()
        .ok_or_else(|| ApiError::Json(409, "remote Nodes are disabled".into()))?;
    Ok((store, svc.fleet.remote(&node)))
}

fn node_error(e: na::Error) -> ApiError {
    let status = match e.code {
        na::Code::Forbidden => 403,
        na::Code::Invalid => 400,
        na::Code::Unavailable => 503,
        na::Code::Unsupported => 409,
        _ => 409,
    };
    ApiError::Json(status, e.safe_detail)
}

async fn persist_receipt(
    store: &Store,
    node: &str,
    harness: &str,
    receipt: na::Receipt,
    reconciled: bool,
) -> Result<(), ApiError> {
    let (store, node, harness) = (store.clone(), node.to_string(), harness.to_string());
    tokio::task::spawn_blocking(move || {
        store.transaction(|tx| {
            let mut s = load(tx, &node)?;
            if let Some(run) = s.runs.get_mut(&harness) {
                // A late poll of an earlier retry must not replace the newer command.
                if run.command.id == receipt.operation {
                    if run.receipt.state.is_finished() {
                        return Ok(());
                    }
                    run.receipt = receipt;
                    run.reconciled = reconciled;
                }
            }
            save(tx, &node, &s)
        })
    })
    .await
    .map_err(internal)?
    .map_err(internal)
}

async fn inspect(node: &dyn na::Node, run: &Run) -> na::ApiResult<na::Receipt> {
    tokio::time::timeout(
        Duration::from_secs(10),
        node.operation(run.command.id.clone()),
    )
    .await
    .unwrap_or_else(|_| {
        Err(na::error(
            na::Code::OutcomeUnknown,
            "Receipt read timed out; reconnect and inspect the same operation",
        ))
    })
}

#[derive(Deserialize, Default)]
pub struct Fresh {
    #[serde(default)]
    fresh: Option<String>,
}

/// Also returns the last good report when the Node is offline, marked with an error and its age.
pub async fn get(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    Query(q): Query<Fresh>,
) -> Result<Json<Value>, ApiError> {
    let (store, node) = target(&st, &c, &id, false).await?;
    let (s, n) = (store.clone(), id.clone());
    let mut saved = tokio::task::spawn_blocking(move || s.reader(|db| load(db, &n)))
        .await
        .map_err(internal)?
        .map_err(internal)?;
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        node.harness_updates(q.fresh.as_deref() == Some("1")),
    )
    .await
    .unwrap_or_else(|_| {
        Err(na::error(
            na::Code::Unavailable,
            "Node update inventory timed out",
        ))
    });
    let mut error = None;
    let mut code = None;
    match result {
        Ok(report) if report.node.as_str() == id => {
            saved.report = Some(report.clone());
            saved.at = crate::nodes::now();
            let (s, n, at) = (store.clone(), id.clone(), saved.at);
            tokio::task::spawn_blocking(move || {
                s.transaction(|tx| {
                    let mut current = load(tx, &n)?;
                    current.report = Some(report);
                    current.at = at;
                    save(tx, &n, &current)
                })
            })
            .await
            .map_err(internal)?
            .map_err(internal)?;
            for (h, run) in &mut saved.runs {
                if run.receipt.state.is_finished() {
                    continue;
                }
                let receipt = match inspect(node.as_ref(), run).await {
                    Ok(receipt) => Some(receipt),
                    Err(e)
                        if e.reason
                            .as_ref()
                            .is_some_and(|r| r.is(na::NodeReason::NoSuchOperation))
                            && crate::nodes::now() > run.command.accept_before.0 =>
                    {
                        Some(na::Receipt {
                            operation: run.command.id.clone(),
                            state: na::Observed::Finished {
                                outcome: na::Outcome::Failed,
                                result: None,
                                error: Some(na::error(
                                    na::Code::Stale,
                                    "The Node has no receipt and the acceptance deadline passed; explicitly retry with a new operation",
                                )),
                            },
                        })
                    }
                    Err(_) => None,
                };
                if let Some(receipt) = receipt {
                    run.receipt = receipt.clone();
                    run.reconciled = true;
                    persist_receipt(&store, &id, h, receipt, true).await?;
                }
            }
        }
        Ok(_) => {
            error = Some("Node returned another Node's update inventory".into());
            code = Some(na::Code::Invalid);
        }
        Err(e) => {
            error = Some(e.safe_detail);
            code = Some(e.code);
        }
    }
    if let Some(why) = &error {
        for run in saved
            .runs
            .values_mut()
            .filter(|r| !r.receipt.state.is_finished())
        {
            run.receipt.state = na::Observed::Unknown {
                reason: why.clone(),
            };
            run.reconciled = false;
        }
    }
    let retryable_unknown: Vec<_> = saved
        .runs
        .iter()
        .filter(|(_, r)| r.reconciled && matches!(r.receipt.state, na::Observed::Unknown { .. }))
        .map(|(h, _)| h.clone())
        .collect();
    let runs: BTreeMap<_, _> = saved
        .runs
        .into_iter()
        .map(|(h, r)| (h, r.receipt))
        .collect();
    Ok(Json(
        json!({"report":saved.report,"at":saved.at,"runs":runs,"retryable_unknown":retryable_unknown,"error":error,"code":code}),
    ))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Start {
    operation: na::OperationId,
    versions: BTreeMap<String, String>,
    #[serde(default)]
    retry_unknown: bool,
}

pub async fn start(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path((id, harness)): Path<(String, String)>,
    Json(body): Json<Start>,
) -> Result<Json<Value>, ApiError> {
    let (store, node) = target(&st, &c, &id, true).await?;
    if harness.len() > 120
        || body.versions.is_empty()
        || body.versions.len() > 16
        || body
            .versions
            .iter()
            .any(|(k, v)| k.len() > 120 || v.len() > 128)
    {
        return Err(ApiError::bad_request("Invalid component versions"));
    }
    let now = crate::nodes::now();
    let retry_unknown = body.retry_unknown;
    let command = na::Command {
        id: body.operation,
        issued_at: na::Timestamp(now),
        accept_before: na::Timestamp(now + 30_000),
        grant: na::GrantId::new("instance_owner").expect("static grant"),
        body: na::UpgradeHarness {
            harness: harness.clone(),
            versions: body.versions,
        },
    };
    let (s, n, h, cmd) = (store.clone(), id.clone(), harness.clone(), command.clone());
    let me = c.me.clone();
    // Durable single intent per harness. Different clicks while unfinished return the same
    // operation; same-id/different-content requests are conflicts before anything is sent.
    let run = tokio::task::spawn_blocking(move || {
        s.transaction(|tx| {
            // Re-read authority under the writer lock: a stale Ctx or a concurrent pause,
            // revocation or role change cannot admit a new intent.
            if !authorized(tx, &me, &n, true)? {
                return Err(StoreError::Conflict("harness_owner_required".into()));
            }
            let mut saved = load(tx, &n)?;
            if let Some(prior) = saved.runs.get(&h) {
                if prior.command.id == cmd.id {
                    if prior.command.body != cmd.body {
                        return Err(StoreError::Conflict(
                            "same operation, other component versions".into(),
                        ));
                    }
                    return Ok(prior.clone());
                }
                if !retry_unknown
                    && !prior.receipt.state.is_finished()
                    && prior.command.body == cmd.body
                {
                    return Ok(prior.clone());
                }
                if !prior.receipt.state.is_finished()
                    && !(retry_unknown
                        && prior.reconciled
                        && matches!(prior.receipt.state, na::Observed::Unknown { .. }))
                {
                    return Err(StoreError::Conflict(
                        "Previous upgrade is running or unknown; read its receipt before retrying"
                            .into(),
                    ));
                }
            }
            let run = Run {
                receipt: na::Receipt {
                    operation: cmd.id.clone(),
                    state: na::Observed::Unknown {
                        reason: "Upgrade intent saved; acceptance has not been confirmed".into(),
                    },
                },
                command: cmd,
                reconciled: false,
            };
            saved.runs.insert(h, run.clone());
            save(tx, &n, &saved)?;
            Ok(run)
        })
    })
    .await
    .map_err(internal)?
    .map_err(|e| {
        ApiError::Json(
            if matches!(&e,StoreError::Conflict(s) if s=="harness_owner_required") {
                403
            } else {
                409
            },
            e.to_string(),
        )
    })?;
    if run.receipt.state.is_finished() {
        return Ok(Json(json!({"receipt":run.receipt})));
    }
    // Observe first, even after an HTTP retry or Hub restart. Invalid means the Node knows no
    // such operation; resending this exact saved command then cannot repeat an accepted effect.
    let result = match inspect(node.as_ref(), &run).await {
        Ok(r) => Ok(r),
        Err(e)
            if e.reason
                .as_ref()
                .is_some_and(|r| r.is(na::NodeReason::NoSuchOperation)) =>
        {
            tokio::time::timeout(
                Duration::from_secs(10),
                node.upgrade_harness(run.command.clone()),
            )
            .await
            .unwrap_or_else(|_| {
                Err(na::error(
                    na::Code::OutcomeUnknown,
                    "Acceptance timed out; inspect this operation after reconnecting",
                ))
            })
        }
        Err(e) => Err(e),
    };
    let reconciled = result.is_ok();
    let receipt = match result {
        Ok(r) => r,
        Err(e) if !matches!(e.code, na::Code::OutcomeUnknown | na::Code::Unavailable) => {
            let receipt = na::Receipt {
                operation: run.command.id.clone(),
                state: na::Observed::Finished {
                    outcome: na::Outcome::Failed,
                    result: None,
                    error: Some(e.clone()),
                },
            };
            persist_receipt(&store, &id, &harness, receipt, true).await?;
            return Err(node_error(e));
        }
        Err(e) => na::Receipt {
            operation: run.command.id.clone(),
            state: na::Observed::Unknown {
                reason: e.safe_detail,
            },
        },
    };
    persist_receipt(&store, &id, &harness, receipt.clone(), reconciled).await?;
    if !receipt.state.is_finished() {
        let (store, id, harness, run) = (store.clone(), id.clone(), harness.clone(), run.clone());
        tokio::spawn(async move {
            for _ in 0..(16 * 60) {
                tokio::time::sleep(Duration::from_secs(1)).await;
                match inspect(node.as_ref(), &run).await {
                    Ok(r) => {
                        let done = r.state.is_finished()
                            || matches!(r.state, na::Observed::Unknown { .. });
                        if persist_receipt(&store, &id, &harness, r, true)
                            .await
                            .is_err()
                            || done
                        {
                            break;
                        }
                    }
                    Err(_) => break,
                }
            }
        });
    }
    Ok(Json(json!({"receipt":receipt})))
}

/// Routes mounted behind the shared API login and body-limit layers.
pub(crate) fn routes() -> axum::Router<super::AppState> {
    use super::*;
    Router::new()
        .route("/node-harness-updates/{node}", get(harness_updates::get))
        .route(
            "/node-harness-updates/{node}/{harness}",
            post(harness_updates::start),
        )
}
