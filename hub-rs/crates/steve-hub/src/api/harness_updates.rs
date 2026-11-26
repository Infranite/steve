//! Owner-only component maintenance, routed to the named Node. Persist intent before sending;
//! receipt polling never resubmits work, including after a link or Hub restart.
//! Automatic upgrades follow Hub-owned, owner-only per-Node / per-harness settings (default off)
//! and write finished outcomes into workspace notifications/timeline.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use axum::{
    Extension, Json,
    extract::{Path, Query, State},
};
use serde::{Deserialize, Serialize};
use steve_node_api::{self as na, Discovery as _};
use steve_store::{Read, Store, StoreError, UserRows, W, WS, WorkspaceSettings, node_directory};
use steve_types::devices::{
    ComponentUpdateState, DevicesEvent, HarnessAutoUpgradePolicy, HarnessComponentUpdateView,
    HarnessUpdateReportView, HarnessUpdateView, HarnessUpdatesResponse, HarnessUpgradeError,
    HarnessUpgradeReceipt, HarnessUpgradeReceiptState, HarnessUpgradeTrigger, NodeReason,
    SetHarnessAutoUpgrade, UpgradeHarnessResponse,
};

use super::{AppState, Ctx};
use crate::bus::Bus;
use crate::error::ApiError;
use crate::nodes::NodeService;

/// Timeout for single Node RPC calls (`harness_updates`, `upgrade_harness`, `operation`).
const NODE_RPC_TIMEOUT: Duration = Duration::from_secs(10);
/// Acceptance window for a newly submitted upgrade command.
const ACCEPT_WINDOW_MS: i64 = 30_000;
/// Maximum time the Hub polls an in-flight harness upgrade receipt before yielding to normal polling.
const WATCH_MAX_SECONDS: usize = 16 * 60;
/// Fast initial poll cadence for newly submitted upgrades so quick completions persist promptly.
const WATCH_FAST_POLL_INTERVAL: Duration = Duration::from_millis(100);
const WATCH_FAST_POLL_TICKS: usize = 5;
/// Steady-state poll cadence for long-running harness upgrades.
const WATCH_POLL_INTERVAL: Duration = Duration::from_secs(1);
/// Delay after Hub boot before the background auto-upgrade loop runs its first check.
const AUTO_UPGRADE_INITIAL_DELAY: Duration = Duration::from_secs(60);
/// Cadence between Hub background auto-upgrade evaluation passes.
const AUTO_UPGRADE_POLL_INTERVAL: Duration = Duration::from_secs(5 * 60);
/// Validation bounds on harness / component identifiers and version maps.
const MAX_ID_LEN: usize = 120;
const MAX_VERSION_LEN: usize = 128;
const MAX_COMPONENTS_PER_REQUEST: usize = 16;
/// Maximum number of Nodes kept in the in-process harness update report cache.
const HARNESS_REPORT_CACHE_MAX: usize = 128;

/// Bounded in-process cache of recent per-Node harness update reports so `GET /node-harness-updates/{node}`
/// never writes to SQLite while still returning the last good report when a Node disconnects.
#[derive(Default)]
pub struct ReportCache {
    entries: std::sync::Mutex<std::collections::VecDeque<(String, na::HarnessUpdateReport, i64)>>,
}

impl ReportCache {
    pub(crate) fn get(&self, node: &str) -> Option<(na::HarnessUpdateReport, i64)> {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let idx = entries.iter().position(|(id, _, _)| id == node)?;
        let item = entries.remove(idx).expect("index in bounds");
        let out = (item.1.clone(), item.2);
        entries.push_back(item);
        Some(out)
    }

    pub(crate) fn put(&self, node: &str, report: na::HarnessUpdateReport, at: i64) {
        let mut entries = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(idx) = entries.iter().position(|(id, _, _)| id == node) {
            entries.remove(idx);
        } else if entries.len() >= HARNESS_REPORT_CACHE_MAX {
            entries.pop_front();
        }
        entries.push_back((node.to_owned(), report, at));
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
struct Saved {
    report: Option<na::HarnessUpdateReport>,
    at: i64,
    runs: BTreeMap<String, Run>,
    #[serde(default)]
    auto_upgrade: HarnessAutoUpgradePolicy,
    /// Per-harness version set that failed an automatic upgrade attempt, preventing silent retry
    /// loops until a newer version arrives or the owner explicitly retries.
    #[serde(default)]
    auto_failed: BTreeMap<String, BTreeMap<String, String>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct Run {
    command: na::Command<na::UpgradeHarness>,
    receipt: na::Receipt,
    #[serde(default)]
    reconciled: bool,
    #[serde(default)]
    trigger: HarnessUpgradeTrigger,
    #[serde(default)]
    notified: bool,
}

#[derive(Debug, thiserror::Error)]
enum HarnessUpdatesError {
    #[error("invalid saved harness update state: {0}")]
    InvalidSavedState(String),
    #[error("Only the instance owner may maintain their active Nodes")]
    OwnerRequired,
    #[error("no store")]
    NoStore,
    #[error("local Node is disabled")]
    LocalDisabled,
    #[error("remote Nodes are disabled")]
    RemoteDisabled,
    #[error("Node requires upgrade before checking or updating harnesses")]
    UpgradeRequired,
    #[error("{0}")]
    InvalidRequest(&'static str),
    #[error("{0}")]
    Conflict(&'static str),
    #[error("{0}")]
    Node(na::Error),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("{0}")]
    Task(String),
}

impl From<HarnessUpdatesError> for ApiError {
    fn from(err: HarnessUpdatesError) -> Self {
        match err {
            HarnessUpdatesError::InvalidSavedState(detail) => {
                tracing::warn!(detail = %detail, "invalid saved harness update state");
                ApiError::Contract(NodeReason::invalid_harness_update_state(
                    "invalid saved harness update state",
                ))
            }
            HarnessUpdatesError::OwnerRequired => ApiError::Contract(steve_types::Error::new(
                steve_types::Code::Forbidden,
                "Only the instance owner may maintain their active Nodes",
            )),
            HarnessUpdatesError::NoStore => ApiError::Contract(steve_types::Error::new(
                steve_types::Code::Unavailable,
                "no store",
            )),
            HarnessUpdatesError::LocalDisabled => ApiError::Contract(steve_types::Error::new(
                steve_types::Code::Conflict,
                "local Node is disabled",
            )),
            HarnessUpdatesError::RemoteDisabled => ApiError::Contract(steve_types::Error::new(
                steve_types::Code::Conflict,
                "remote Nodes are disabled",
            )),
            HarnessUpdatesError::UpgradeRequired => {
                ApiError::Contract(NodeReason::upgrade_required(
                    "Node requires upgrade before checking or updating harnesses",
                ))
            }
            HarnessUpdatesError::InvalidRequest(msg) => {
                ApiError::Contract(steve_types::Error::new(steve_types::Code::Invalid, msg))
            }
            HarnessUpdatesError::Conflict(msg) => {
                ApiError::Contract(steve_types::Error::new(steve_types::Code::Conflict, msg))
            }
            HarnessUpdatesError::Node(e) => {
                let code = match e.code {
                    na::Code::Forbidden => steve_types::Code::Forbidden,
                    na::Code::Invalid => steve_types::Code::Invalid,
                    na::Code::Unavailable => steve_types::Code::Unavailable,
                    _ => steve_types::Code::Conflict,
                };
                let mut contract = steve_types::Error::new(code, e.safe_detail);
                contract.reason = e.reason;
                contract.params = e.params;
                ApiError::Contract(contract)
            }
            HarnessUpdatesError::Store(e) => ApiError::internal(e.to_string()),
            HarnessUpdatesError::Task(msg) => ApiError::internal(msg),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoUpgradeCandidate {
    pub harness: String,
    pub versions: BTreeMap<String, String>,
}

fn task_err(e: impl std::fmt::Display) -> HarnessUpdatesError {
    HarnessUpdatesError::Task(e.to_string())
}

fn key(node: &str) -> String {
    format!("harness.updates.{node}")
}

fn code_string(c: na::Code) -> String {
    serde_json::to_string(&c)
        .map(|s| s.trim_matches('"').to_string())
        .unwrap_or_else(|_| "internal".into())
}

fn outcome_string(o: na::Outcome) -> String {
    serde_json::to_string(&o)
        .map(|s| s.trim_matches('"').to_string())
        .unwrap_or_else(|_| "failed".into())
}

fn receipt_view(r: &na::Receipt) -> HarnessUpgradeReceipt {
    let state = match &r.state {
        na::Observed::Accepted => HarnessUpgradeReceiptState::Accepted,
        na::Observed::Running => HarnessUpgradeReceiptState::Running,
        na::Observed::Waiting { .. } => HarnessUpgradeReceiptState::Waiting,
        na::Observed::Unknown { reason } => HarnessUpgradeReceiptState::Unknown {
            reason: reason.clone(),
        },
        na::Observed::Finished { outcome, error, .. } => HarnessUpgradeReceiptState::Finished {
            outcome: outcome_string(*outcome),
            error: error.as_ref().map(|e| HarnessUpgradeError {
                code: code_string(e.code),
                safe_detail: e.safe_detail.clone(),
            }),
        },
    };
    HarnessUpgradeReceipt {
        operation: r.operation.as_str().to_string(),
        state,
    }
}

fn report_view(report: &na::HarnessUpdateReport) -> HarnessUpdateReportView {
    HarnessUpdateReportView {
        node: report.node.as_str().to_string(),
        checking: report.checking,
        harnesses: report
            .harnesses
            .iter()
            .map(|h| HarnessUpdateView {
                harness: h.harness.clone(),
                name: h.name.clone(),
                components: h
                    .components
                    .iter()
                    .map(|c| HarnessComponentUpdateView {
                        id: c.id.clone(),
                        name: c.name.clone(),
                        package: c.package.clone(),
                        managed: c.managed,
                        source: c.source,
                        upgrade_method: c.upgrade_method,
                        upgrade_command: c.upgrade_command.clone(),
                        can_upgrade: c.can_upgrade,
                        state: c.state,
                        installed: c.installed.clone(),
                        latest: c.latest.clone(),
                        checked: c.checked.map(|t| t.0),
                        reason: c.reason.clone(),
                    })
                    .collect(),
                can_upgrade: h.can_upgrade,
                active_sessions: h.active_sessions,
                blocked_reason: h.blocked_reason,
                operation: h.operation.as_ref().map(receipt_view),
            })
            .collect(),
    }
}

fn authorized(db: &mut impl Read, me: &str, node: &str, write: bool) -> Result<bool, StoreError> {
    if UserRows(db).first_owner()?.as_deref() != Some(me) {
        return Ok(false);
    }
    if node == "local" {
        return Ok(true);
    }
    if node_directory::owner_if_connectable(db, node)?.as_deref() != Some(me) {
        return Ok(false);
    }
    Ok(!write || node_directory::link_state(db, node)?.is_some_and(|(_, state)| state == "active"))
}

fn load(c: &mut impl Read, node: &str) -> Result<Saved, HarnessUpdatesError> {
    let v = WorkspaceSettings(c).get(WS, &key(node))?;
    v.map(|v| {
        serde_json::from_str(&v).map_err(|e| HarnessUpdatesError::InvalidSavedState(e.to_string()))
    })
    .transpose()
    .map(Option::unwrap_or_default)
}

fn save(w: &mut W<'_>, node: &str, s: &Saved) -> Result<(), HarnessUpdatesError> {
    let v = serde_json::to_string(s).map_err(|e| HarnessUpdatesError::Task(e.to_string()))?;
    WorkspaceSettings(w).set(WS, &key(node), &v, crate::nodes::now())?;
    Ok(())
}

async fn target(
    st: &AppState,
    c: &Ctx,
    id: &str,
    write: bool,
) -> Result<(Store, Arc<dyn na::Node>), HarnessUpdatesError> {
    let store = st
        .modules
        .as_ref()
        .map(|m| m.store.clone())
        .ok_or(HarnessUpdatesError::NoStore)?;
    let (s, me, node) = (store.clone(), c.me.clone(), id.to_string());
    let local = id == "local";
    let allowed = tokio::task::spawn_blocking(move || -> Result<bool, HarnessUpdatesError> {
        s.read(|db| Ok(authorized(db, &me, &node, write)?))
    })
    .await
    .map_err(task_err)??;
    if !allowed {
        return Err(HarnessUpdatesError::OwnerRequired);
    }
    if local {
        let node = st
            .node
            .as_ref()
            .filter(|n| n.node_id().as_str() == id)
            .ok_or(HarnessUpdatesError::LocalDisabled)?
            .node();
        return Ok((store, node));
    }
    let node = na::NodeId::new(id).map_err(task_err)?;
    let svc = st
        .nodes
        .as_ref()
        .ok_or(HarnessUpdatesError::RemoteDisabled)?;
    Ok((store, svc.fleet.remote(&node)))
}

struct FinishedNotice {
    trigger: HarnessUpgradeTrigger,
    outcome: na::Outcome,
    harness_name: String,
    versions: String,
    error: String,
}

async fn persist_receipt(
    store: &Store,
    node: &str,
    harness: &str,
    receipt: na::Receipt,
    reconciled: bool,
) -> Result<Option<FinishedNotice>, HarnessUpdatesError> {
    let (store_c, node_s, harness_s) = (store.clone(), node.to_string(), harness.to_string());
    let notice = tokio::task::spawn_blocking(move || {
        store_c.write(
            |tx| -> Result<Option<FinishedNotice>, HarnessUpdatesError> {
                let mut s = load(tx, &node_s)?;
                let mut finished_notice = None;
                let harness_name = s
                    .report
                    .as_ref()
                    .and_then(|rep| rep.harnesses.iter().find(|h| harness_s == h.harness))
                    .map(|h| h.name.clone())
                    .unwrap_or_else(|| harness_s.clone());
                let mut failed_auto_versions: Option<BTreeMap<String, String>> = None;
                let mut clear_auto_failed = false;

                if let Some(run) = s.runs.get_mut(&harness_s) {
                    // A late poll of an earlier retry must not replace the newer command.
                    if run.command.id == receipt.operation {
                        if run.receipt.state.is_finished() {
                            return Ok(None);
                        }
                        let just_finished = receipt.state.is_finished();
                        run.receipt = receipt.clone();
                        run.reconciled = reconciled;
                        if just_finished && !run.notified {
                            run.notified = true;
                            if let na::Observed::Finished { outcome, error, .. } = &receipt.state {
                                let versions = if run.command.body.versions.is_empty() {
                                    "latest".to_string()
                                } else {
                                    run.command
                                        .body
                                        .versions
                                        .values()
                                        .cloned()
                                        .collect::<Vec<_>>()
                                        .join(" / ")
                                };
                                if *outcome == na::Outcome::Succeeded {
                                    clear_auto_failed = true;
                                } else if run.trigger == HarnessUpgradeTrigger::Auto
                                    && !run.command.body.versions.is_empty()
                                {
                                    failed_auto_versions = Some(run.command.body.versions.clone());
                                }
                                finished_notice = Some(FinishedNotice {
                                    trigger: run.trigger,
                                    outcome: *outcome,
                                    harness_name,
                                    versions,
                                    error: error
                                        .as_ref()
                                        .map(|e| e.safe_detail.clone())
                                        .unwrap_or_else(|| outcome_string(*outcome)),
                                });
                            }
                        }
                    }
                }
                if clear_auto_failed {
                    s.auto_failed.remove(&harness_s);
                } else if let Some(vers) = failed_auto_versions {
                    s.auto_failed.insert(harness_s.clone(), vers);
                }
                if let Some(ref info) = finished_notice {
                    record_finished_notice_in(tx, &node_s, info);
                }
                save(tx, &node_s, &s)?;
                Ok(finished_notice)
            },
        )
    })
    .await
    .map_err(task_err)??;

    if let Some(ref info) = notice {
        record_finished_metric(info);
    }
    Ok(notice)
}

fn record_finished_metric(info: &FinishedNotice) {
    let trigger_label = match info.trigger {
        HarnessUpgradeTrigger::Manual => "manual",
        HarnessUpgradeTrigger::Auto => "auto",
    };
    let outcome_label = match info.outcome {
        na::Outcome::Succeeded => "succeeded",
        _ => "failed",
    };
    ::metrics::counter!(
        "steve_hub_harness_upgrades_total",
        "trigger" => trigger_label,
        "outcome" => outcome_label
    )
    .increment(1);
}

fn record_finished_notice_in(tx: &mut W<'_>, node_id: &str, info: &FinishedNotice) {
    let node_name = if node_id == "local" {
        "local".to_string()
    } else {
        steve_store::nodes::get(tx, node_id)
            .ok()
            .flatten()
            .map(|n| n.name)
            .unwrap_or_else(|| node_id.to_string())
    };
    let (level, text) = match (info.trigger, info.outcome) {
        (HarnessUpgradeTrigger::Auto, na::Outcome::Succeeded) => (
            "success",
            steve_i18n::t!(
                "hub.node.harness_auto_upgraded",
                node = node_name,
                harness = info.harness_name,
                versions = info.versions
            ),
        ),
        (HarnessUpgradeTrigger::Auto, _) => (
            "fail",
            steve_i18n::t!(
                "hub.node.harness_auto_upgrade_failed",
                node = node_name,
                harness = info.harness_name,
                error = info.error
            ),
        ),
        (HarnessUpgradeTrigger::Manual, na::Outcome::Succeeded) => (
            "success",
            steve_i18n::t!(
                "hub.node.harness_upgraded",
                node = node_name,
                harness = info.harness_name,
                versions = info.versions
            ),
        ),
        (HarnessUpgradeTrigger::Manual, _) => (
            "fail",
            steve_i18n::t!(
                "hub.node.harness_upgrade_failed",
                node = node_name,
                harness = info.harness_name,
                error = info.error
            ),
        ),
    };
    if let Err(e) = steve_store::notify::notify_in(tx, WS, "system", level, &text, None) {
        tracing::warn!("harness upgrade notice failed: {e}");
    }
}

/// Automatically re-probe the Node's harness version & availability after a successful upgrade.
async fn reprobe_after_upgrade(
    st: Option<&AppState>,
    store: &Store,
    node_id: &str,
    node: &Arc<dyn na::Node>,
) {
    if let Some(st) = st {
        st.node_harnesses.invalidate(node_id);
    }
    let _ = tokio::time::timeout(Duration::from_secs(30), node.probe(true)).await;
    if let Ok(Ok(report)) = tokio::time::timeout(NODE_RPC_TIMEOUT, node.harness_updates(true)).await
        && report.node.as_str() == node_id
    {
        let (s, n, at) = (store.clone(), node_id.to_string(), crate::nodes::now());
        let _ = tokio::task::spawn_blocking(move || {
            s.write(|tx| {
                let mut current = load(tx, &n)?;
                current.report = Some(report);
                current.at = at;
                save(tx, &n, &current)
            })
        })
        .await;
    }
    if let Some(st) = st {
        publish_node_changed(&st.bus, store, node_id).await;
    }
}

async fn publish_node_changed(bus: &Bus, store: &Store, node_id: &str) {
    let store = store.clone();
    if let Ok(Ok(Some(owner))) =
        tokio::task::spawn_blocking(move || store.read(|db| UserRows(db).first_owner())).await
    {
        bus.publish(steve_types::DomainEvent::from(DevicesEvent::Node {
            id: steve_types::NodeId::from(node_id.to_string()),
            user: steve_types::UserId::from(owner),
        }));
    }
}

async fn inspect(node: &dyn na::Node, run: &Run) -> na::ApiResult<na::Receipt> {
    tokio::time::timeout(NODE_RPC_TIMEOUT, node.operation(run.command.id.clone()))
        .await
        .unwrap_or_else(|_| {
            Err(na::Error::new(
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
/// Pure read path: never writes to SQLite (caches fresh reports in memory and inspects unfinished
/// runs read-only; durable receipt persistence belongs to `POST` watchers and background tasks).
pub async fn get(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    Query(q): Query<Fresh>,
) -> Result<Json<HarnessUpdatesResponse>, ApiError> {
    let (store, node) = target(&st, &c, &id, false).await?;
    let (s, n) = (store.clone(), id.clone());
    let mut saved = tokio::task::spawn_blocking(move || s.read(|db| load(db, &n)))
        .await
        .map_err(task_err)??;
    if let Some((cached_report, cached_at)) = st.harness_reports.get(&id)
        && cached_at >= saved.at
    {
        saved.report = Some(cached_report);
        saved.at = cached_at;
    }
    let mut error = None;
    let mut code = None;
    if node.is_upgrade_only() {
        error = Some("Node requires upgrade before checking or updating harnesses".into());
        code = Some("upgrade_required".into());
    } else {
        let result = tokio::time::timeout(
            NODE_RPC_TIMEOUT,
            node.harness_updates(q.fresh.as_deref() == Some("1")),
        )
        .await
        .unwrap_or_else(|_| {
            Err(na::Error::new(
                na::Code::Unavailable,
                "Node update inventory timed out",
            ))
        });
        match result {
            Ok(report) if report.node.as_str() == id => {
                let at = crate::nodes::now();
                st.harness_reports.put(&id, report.clone(), at);
                saved.report = Some(report);
                saved.at = at;
            }
            Ok(_) => {
                error = Some("Node returned another Node's update inventory".into());
                code = Some(code_string(na::Code::Invalid));
            }
            Err(e) => {
                error = Some(e.safe_detail);
                code = Some(code_string(e.code));
            }
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
        .map(|(h, r)| (h, receipt_view(&r.receipt)))
        .collect();
    Ok(Json(HarnessUpdatesResponse {
        report: saved.report.as_ref().map(report_view),
        at: saved.at,
        runs,
        retryable_unknown,
        auto_upgrade: saved.auto_upgrade,
        error,
        code,
    }))
}

/// Refresh unfinished saved operations without ever resubmitting an upgrade.
/// Returns true if any operation newly finished with `Outcome::Succeeded`.
async fn reconcile_runs(
    store: &Store,
    id: &str,
    node: &dyn na::Node,
    saved: &mut Saved,
) -> Result<bool, HarnessUpdatesError> {
    let mut any_succeeded = false;
    for (h, run) in &mut saved.runs {
        if run.receipt.state.is_finished() {
            continue;
        }
        let receipt = match inspect(node, run).await {
            Ok(receipt) => Some(receipt),
            Err(e)
                if e.code == na::Code::Invalid
                    && crate::nodes::now() > run.command.accept_before.0 =>
            {
                Some(na::Receipt {
                    operation: run.command.id.clone(),
                    state: na::Observed::Finished {
                        outcome: na::Outcome::Failed,
                        result: None,
                        error: Some(na::Error::new(
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
            if let Some(notice) = persist_receipt(store, id, h, receipt, true).await?
                && notice.outcome == na::Outcome::Succeeded
            {
                any_succeeded = true;
            }
        }
    }
    Ok(any_succeeded)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Start {
    operation: na::OperationId,
    #[serde(default)]
    versions: BTreeMap<String, String>,
    #[serde(default)]
    self_update: Vec<String>,
    #[serde(default)]
    retry_unknown: bool,
}

/// Admit one owner-authorized durable intent under the writer lock, preserving retry identity.
fn admit_upgrade(
    tx: &mut W<'_>,
    me: &str,
    node: &str,
    h: String,
    cmd: na::Command<na::UpgradeHarness>,
    retry_unknown: bool,
    trigger: HarnessUpgradeTrigger,
) -> Result<Run, HarnessUpdatesError> {
    // Re-read authority under the writer lock: a stale Ctx or a concurrent pause,
    // revocation or role change cannot admit a new intent.
    if !authorized(tx, me, node, true)? {
        return Err(HarnessUpdatesError::OwnerRequired);
    }
    let mut saved = load(tx, node)?;
    if let Some(prior) = saved.runs.get(&h) {
        if prior.command.id == cmd.id {
            if prior.command.body != cmd.body {
                return Err(HarnessUpdatesError::Conflict(
                    "same operation, other component versions",
                ));
            }
            return Ok(prior.clone());
        }
        if !retry_unknown && !prior.receipt.state.is_finished() && prior.command.body == cmd.body {
            return Ok(prior.clone());
        }
        if !prior.receipt.state.is_finished()
            && !(retry_unknown
                && prior.reconciled
                && matches!(prior.receipt.state, na::Observed::Unknown { .. }))
        {
            return Err(HarnessUpdatesError::Conflict(
                "Previous upgrade is running or unknown; read its receipt before retrying",
            ));
        }
    }
    if trigger == HarnessUpgradeTrigger::Manual {
        saved.auto_failed.remove(&h);
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
        trigger,
        notified: false,
    };
    saved.runs.insert(h, run.clone());
    save(tx, node, &saved)?;
    Ok(run)
}

/// Observe a saved command before considering its one safe idempotent submission.
async fn observe_or_submit(node: &dyn na::Node, run: &Run) -> na::ApiResult<na::Receipt> {
    // Observe first, even after an HTTP retry or Hub restart. Invalid means the Node knows no
    // such operation; resending this exact saved command then cannot repeat an accepted effect.
    match inspect(node, run).await {
        Ok(r) => Ok(r),
        Err(e) if e.code == na::Code::Invalid => {
            tokio::time::timeout(NODE_RPC_TIMEOUT, node.upgrade_harness(run.command.clone()))
                .await
                .unwrap_or_else(|_| {
                    Err(na::Error::new(
                        na::Code::OutcomeUnknown,
                        "Acceptance timed out; inspect this operation after reconnecting",
                    ))
                })
        }
        Err(e) => Err(e),
    }
}

async fn submit_and_watch(
    st: Option<AppState>,
    store: Store,
    node: Arc<dyn na::Node>,
    id: String,
    harness: String,
    run: Run,
) -> Result<na::Receipt, HarnessUpdatesError> {
    if run.receipt.state.is_finished() {
        return Ok(run.receipt);
    }
    let result = observe_or_submit(node.as_ref(), &run).await;
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
            let _ = persist_receipt(&store, &id, &harness, receipt, true).await?;
            return Err(HarnessUpdatesError::Node(e));
        }
        Err(e) => na::Receipt {
            operation: run.command.id.clone(),
            state: na::Observed::Unknown {
                reason: e.safe_detail,
            },
        },
    };
    let finished_now = persist_receipt(&store, &id, &harness, receipt.clone(), reconciled).await?;
    if finished_now
        .as_ref()
        .is_some_and(|n| n.outcome == na::Outcome::Succeeded)
    {
        let (st_c, store_c, id_c, node_c) = (st.clone(), store.clone(), id.clone(), node.clone());
        tokio::spawn(async move {
            reprobe_after_upgrade(st_c.as_ref(), &store_c, &id_c, &node_c).await;
        });
    } else if !receipt.state.is_finished() {
        tokio::spawn(async move {
            for tick in 0..(WATCH_FAST_POLL_TICKS + WATCH_MAX_SECONDS) {
                let delay = if tick < WATCH_FAST_POLL_TICKS {
                    WATCH_FAST_POLL_INTERVAL
                } else {
                    WATCH_POLL_INTERVAL
                };
                tokio::time::sleep(delay).await;
                match inspect(node.as_ref(), &run).await {
                    Ok(r) => {
                        let done = r.state.is_finished()
                            || matches!(r.state, na::Observed::Unknown { .. });
                        match persist_receipt(&store, &id, &harness, r, true).await {
                            Ok(notice) => {
                                if notice
                                    .as_ref()
                                    .is_some_and(|n| n.outcome == na::Outcome::Succeeded)
                                {
                                    reprobe_after_upgrade(st.as_ref(), &store, &id, &node).await;
                                }
                                if done {
                                    break;
                                }
                            }
                            Err(_) => break,
                        }
                    }
                    Err(e)
                        if matches!(e.code, na::Code::Unavailable | na::Code::OutcomeUnknown) =>
                    {
                        continue;
                    }
                    Err(_) => break,
                }
            }
        });
    }
    Ok(receipt)
}

pub async fn start(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path((id, harness)): Path<(String, String)>,
    Json(body): Json<Start>,
) -> Result<Json<UpgradeHarnessResponse>, ApiError> {
    let (store, node) = target(&st, &c, &id, true).await?;
    if node.is_upgrade_only() {
        return Err(HarnessUpdatesError::UpgradeRequired.into());
    }
    let total = body.versions.len().saturating_add(body.self_update.len());
    if harness.len() > MAX_ID_LEN
        || total == 0
        || total > MAX_COMPONENTS_PER_REQUEST
        || body
            .versions
            .iter()
            .any(|(k, v)| k.len() > MAX_ID_LEN || v.len() > MAX_VERSION_LEN)
        || body.self_update.iter().any(|k| k.len() > MAX_ID_LEN)
    {
        return Err(HarnessUpdatesError::InvalidRequest("Invalid component versions").into());
    }
    let now = crate::nodes::now();
    let retry_unknown = body.retry_unknown;
    let command = na::Command {
        id: body.operation,
        issued_at: na::Timestamp(now),
        accept_before: na::Timestamp(now + ACCEPT_WINDOW_MS),
        grant: na::GrantId::new("instance_owner").expect("static grant"),
        body: na::UpgradeHarness {
            harness: harness.clone(),
            versions: body.versions,
            self_update: body.self_update,
        },
    };
    let (s, n, h, cmd) = (store.clone(), id.clone(), harness.clone(), command.clone());
    let me = c.me.clone();
    // Durable single intent per harness. Different clicks while unfinished return the same
    // operation; same-id/different-content requests are conflicts before anything is sent.
    let run = tokio::task::spawn_blocking(move || {
        s.write(|tx| {
            admit_upgrade(
                tx,
                &me,
                &n,
                h,
                cmd,
                retry_unknown,
                HarnessUpgradeTrigger::Manual,
            )
        })
    })
    .await
    .map_err(task_err)??;
    let receipt = submit_and_watch(Some(st), store, node, id, harness, run).await?;
    Ok(Json(UpgradeHarnessResponse {
        receipt: receipt_view(&receipt),
    }))
}

/// `PUT /api/node-harness-updates/{node}/auto-upgrade`: update Node-level and/or per-harness
/// automatic upgrade settings (instance owner only).
pub async fn set_auto_upgrade(
    State(st): State<AppState>,
    Extension(c): Extension<Ctx>,
    Path(id): Path<String>,
    Json(body): Json<SetHarnessAutoUpgrade>,
) -> Result<Json<HarnessAutoUpgradePolicy>, ApiError> {
    if body.enabled.is_none() && body.harness.is_none() {
        return Err(
            HarnessUpdatesError::InvalidRequest("Provide enabled and/or harness + mode").into(),
        );
    }
    if let Some(ref h) = body.harness {
        if h.is_empty() || h.len() > MAX_ID_LEN || body.mode.is_none() {
            return Err(
                HarnessUpdatesError::InvalidRequest("Invalid harness auto-upgrade mode").into(),
            );
        }
    } else if body.mode.is_some() {
        return Err(
            HarnessUpdatesError::InvalidRequest("mode requires a target harness id").into(),
        );
    }
    let (store, _node) = target(&st, &c, &id, true).await?;
    let (s, me, node_id) = (store.clone(), c.me.clone(), id.clone());
    let policy = tokio::task::spawn_blocking(move || {
        s.write(|tx| {
            if !authorized(tx, &me, &node_id, true)? {
                return Err(HarnessUpdatesError::OwnerRequired);
            }
            let mut saved = load(tx, &node_id)?;
            if let Some(enabled) = body.enabled {
                saved.auto_upgrade.enabled = enabled;
            }
            if let (Some(harness), Some(mode)) = (body.harness, body.mode) {
                saved.auto_upgrade.set_harness_mode(harness, mode);
            }
            save(tx, &node_id, &saved)?;
            Ok(saved.auto_upgrade)
        })
    })
    .await
    .map_err(task_err)??;
    ::metrics::counter!("steve_hub_harness_auto_upgrade_policy_changes_total").increment(1);
    publish_node_changed(&st.bus, &store, &id).await;
    Ok(Json(policy))
}

/// Pure, deterministic auto-upgrade decision over one Node's report, policy, and run history.
pub fn select_auto_upgrades(
    report: &na::HarnessUpdateReport,
    policy: &HarnessAutoUpgradePolicy,
    runs: &BTreeMap<String, na::Receipt>,
    auto_failed: &BTreeMap<String, BTreeMap<String, String>>,
) -> Vec<AutoUpgradeCandidate> {
    if report.checking {
        return Vec::new();
    }
    let mut out = Vec::new();
    for h in &report.harnesses {
        if !policy.effective(&h.harness) {
            continue;
        }
        // Auto-upgrades only run when idle (never interrupt or queue behind active sessions).
        if !h.can_upgrade || h.active_sessions > 0 {
            continue;
        }
        let in_flight_or_unknown = runs.get(&h.harness).is_some_and(|r| !r.state.is_finished())
            || h.operation.as_ref().is_some_and(|r| !r.state.is_finished());
        if in_flight_or_unknown {
            continue;
        }
        let versions: BTreeMap<String, String> = h
            .components
            .iter()
            .filter(|c| c.can_upgrade && c.state == ComponentUpdateState::Available)
            .filter_map(|c| Some((c.id.clone(), c.latest.clone()?)))
            .collect();
        if versions.is_empty() {
            continue;
        }
        if auto_failed.get(&h.harness) == Some(&versions) {
            continue;
        }
        out.push(AutoUpgradeCandidate {
            harness: h.harness.clone(),
            versions,
        });
    }
    out
}

/// Periodic Hub background task: evaluates Nodes that have auto-upgrade enabled (Node-level or
/// per-harness) and triggers idle upgrades through the exact same durable operation path.
pub fn auto_upgrade_task(
    store: Store,
    local: Option<Arc<dyn na::Node>>,
    svc: Option<Arc<NodeService>>,
    bus: Bus,
) -> steve_runtime::Task {
    steve_runtime::every(
        "nodes.harness_auto_upgrade",
        Arc::new(steve_runtime::SystemClock),
        AUTO_UPGRADE_POLL_INTERVAL,
        steve_runtime::FirstRun::After(AUTO_UPGRADE_INITIAL_DELAY),
        move || {
            let store = store.clone();
            let local = local.clone();
            let svc = svc.clone();
            let bus = bus.clone();
            async move {
                run_auto_upgrade_pass(&store, local.as_ref(), svc.as_ref(), Some(&bus)).await;
            }
        },
    )
}

/// Execute one auto-upgrade evaluation pass across all reachable owner Nodes.
pub async fn run_auto_upgrade_pass(
    store: &Store,
    local: Option<&Arc<dyn na::Node>>,
    svc: Option<&Arc<NodeService>>,
    bus: Option<&Bus>,
) {
    let store_c = store.clone();
    let owner_and_nodes = tokio::task::spawn_blocking(move || {
        store_c.read(|db| {
            let Some(owner) = UserRows(db).first_owner()? else {
                return Ok::<_, StoreError>(None);
            };
            let rows = steve_store::nodes::list_for_owner(db, &owner)?;
            Ok(Some((owner, rows)))
        })
    })
    .await;
    let Ok(Ok(Some((owner, rows)))) = owner_and_nodes else {
        return;
    };

    let mut targets: Vec<(String, Arc<dyn na::Node>)> = Vec::new();
    if let Some(local) = local
        && !local.is_upgrade_only()
    {
        targets.push(("local".into(), local.clone()));
    }
    if let Some(svc) = svc {
        for row in rows.into_iter().filter(|n| !n.local && n.state == "active") {
            if svc.registry.current(&row.node_id).is_some()
                && let Ok(nid) = na::NodeId::new(row.node_id.clone())
            {
                let remote = svc.fleet.remote(&nid);
                if !remote.is_upgrade_only() {
                    targets.push((row.node_id, remote));
                }
            }
        }
    }

    for (node_id, node) in targets {
        auto_upgrade_one_node(store, &owner, &node_id, &node, bus).await;
    }
}

async fn auto_upgrade_one_node(
    store: &Store,
    owner: &str,
    node_id: &str,
    node: &Arc<dyn na::Node>,
    bus: Option<&Bus>,
) {
    let (s, n) = (store.clone(), node_id.to_string());
    let Ok(Ok(mut saved)) = tokio::task::spawn_blocking(move || s.read(|db| load(db, &n))).await
    else {
        return;
    };
    let any_auto_enabled =
        saved.auto_upgrade.enabled || saved.auto_upgrade.harnesses.values().any(|&v| v);
    if !any_auto_enabled {
        return;
    }
    let Ok(Ok(report)) = tokio::time::timeout(NODE_RPC_TIMEOUT, node.harness_updates(false)).await
    else {
        return;
    };
    if report.node.as_str() != node_id {
        return;
    }
    saved.report = Some(report.clone());
    saved.at = crate::nodes::now();
    {
        let (s, n, rep, at) = (store.clone(), node_id.to_string(), report.clone(), saved.at);
        let _ = tokio::task::spawn_blocking(move || {
            s.write(|tx| {
                let mut current = load(tx, &n)?;
                current.report = Some(rep);
                current.at = at;
                save(tx, &n, &current)
            })
        })
        .await;
    }
    if let Ok(succeeded) = reconcile_runs(store, node_id, node.as_ref(), &mut saved).await
        && succeeded
    {
        reprobe_after_upgrade(None, store, node_id, node).await;
        if let Some(bus) = bus {
            publish_node_changed(bus, store, node_id).await;
        }
    }
    let receipts: BTreeMap<String, na::Receipt> = saved
        .runs
        .iter()
        .map(|(h, r)| (h.clone(), r.receipt.clone()))
        .collect();
    let candidates =
        select_auto_upgrades(&report, &saved.auto_upgrade, &receipts, &saved.auto_failed);
    for cand in candidates {
        let now = crate::nodes::now();
        let Ok(op_id) = na::OperationId::new(format!("hup_auto_{}", uuid::Uuid::new_v4().simple()))
        else {
            continue;
        };
        let cmd = na::Command {
            id: op_id,
            issued_at: na::Timestamp(now),
            accept_before: na::Timestamp(now + ACCEPT_WINDOW_MS),
            grant: na::GrantId::new("instance_owner").expect("static grant"),
            body: na::UpgradeHarness {
                harness: cand.harness.clone(),
                versions: cand.versions,
                self_update: vec![],
            },
        };
        let (s, me, nid, h) = (
            store.clone(),
            owner.to_string(),
            node_id.to_string(),
            cand.harness.clone(),
        );
        let Ok(Ok(run)) = tokio::task::spawn_blocking(move || {
            s.write(|tx| admit_upgrade(tx, &me, &nid, h, cmd, false, HarnessUpgradeTrigger::Auto))
        })
        .await
        else {
            continue;
        };
        if let Err(e) = submit_and_watch(
            None,
            store.clone(),
            node.clone(),
            node_id.to_string(),
            cand.harness,
            run,
        )
        .await
        {
            tracing::warn!(node = %node_id, "auto-upgrade submission rejected: {e:?}");
        }
        // Submit at most one auto-upgrade per Node per pass so maintenance leases never collide.
        break;
    }
}

/// Routes mounted behind the shared API login and body-limit layers.
pub(crate) fn routes() -> axum::Router<super::AppState> {
    use super::*;
    Router::new()
        .route("/node-harness-updates/{node}", get(harness_updates::get))
        .route(
            "/node-harness-updates/{node}/auto-upgrade",
            put(harness_updates::set_auto_upgrade),
        )
        .route(
            "/node-harness-updates/{node}/{harness}",
            post(harness_updates::start),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use steve_types::devices::HarnessAutoUpgradeMode;

    fn comp(
        id: &str,
        state: ComponentUpdateState,
        latest: Option<&str>,
        can_upgrade: bool,
    ) -> na::HarnessComponentUpdate {
        na::HarnessComponentUpdate {
            id: id.into(),
            name: id.into(),
            package: None,
            managed: false,
            source: na::ComponentInstallSource::HomebrewCask,
            upgrade_method: if can_upgrade {
                na::UpgradeMethod::HomebrewCask
            } else {
                na::UpgradeMethod::Unsupported
            },
            upgrade_command: None,
            can_upgrade,
            state,
            installed: Some("1.0.0".into()),
            latest: latest.map(String::from),
            checked: Some(na::Timestamp(1)),
            reason: None,
        }
    }

    fn report(harnesses: Vec<na::HarnessUpdateInfo>, checking: bool) -> na::HarnessUpdateReport {
        na::HarnessUpdateReport {
            node: na::NodeId::new("local").unwrap(),
            checking,
            harnesses,
        }
    }

    #[test]
    fn auto_upgrade_policy_defaults_off_and_per_harness_overrides_node_level() {
        let mut p = HarnessAutoUpgradePolicy::default();
        assert!(!p.enabled);
        assert!(!p.effective("alpha"));

        // Per-harness ON overrides Node OFF.
        p.set_harness_mode("alpha".into(), HarnessAutoUpgradeMode::On);
        assert!(p.effective("alpha"));
        assert!(!p.effective("beta"));

        // Node ON applies to beta; explicit OFF on alpha overrides Node ON.
        p.enabled = true;
        p.set_harness_mode("alpha".into(), HarnessAutoUpgradeMode::Off);
        assert!(!p.effective("alpha"));
        assert!(p.effective("beta"));

        // Inherit clears the per-harness override.
        p.set_harness_mode("alpha".into(), HarnessAutoUpgradeMode::Inherit);
        assert!(p.effective("alpha"));
        assert!(p.harnesses.is_empty());
    }

    #[test]
    fn select_auto_upgrades_respects_policy_idle_sessions_check_on_upgrade_and_failed_versions() {
        let rep = report(
            vec![
                na::HarnessUpdateInfo {
                    harness: "idle_ready".into(),
                    name: "Idle Ready".into(),
                    components: vec![
                        comp("cli", ComponentUpdateState::Available, Some("2.0.0"), true),
                        comp(
                            "adapter",
                            ComponentUpdateState::Current,
                            Some("1.0.0"),
                            true,
                        ),
                    ],
                    can_upgrade: true,
                    active_sessions: 0,
                    blocked_reason: None,
                    operation: None,
                },
                na::HarnessUpdateInfo {
                    harness: "busy_sessions".into(),
                    name: "Busy".into(),
                    components: vec![comp(
                        "cli",
                        ComponentUpdateState::Available,
                        Some("2.0.0"),
                        true,
                    )],
                    can_upgrade: true,
                    active_sessions: 2,
                    blocked_reason: None,
                    operation: None,
                },
                na::HarnessUpdateInfo {
                    harness: "self_check_only".into(),
                    name: "Self Check".into(),
                    components: vec![comp(
                        "cli",
                        ComponentUpdateState::CheckOnUpgrade,
                        None,
                        true,
                    )],
                    can_upgrade: true,
                    active_sessions: 0,
                    blocked_reason: None,
                    operation: None,
                },
                na::HarnessUpdateInfo {
                    harness: "explicit_off".into(),
                    name: "Explicit Off".into(),
                    components: vec![comp(
                        "cli",
                        ComponentUpdateState::Available,
                        Some("2.0.0"),
                        true,
                    )],
                    can_upgrade: true,
                    active_sessions: 0,
                    blocked_reason: None,
                    operation: None,
                },
            ],
            false,
        );

        // Default policy OFF selects nothing.
        let off = HarnessAutoUpgradePolicy::default();
        assert!(select_auto_upgrades(&rep, &off, &BTreeMap::new(), &BTreeMap::new()).is_empty());

        // Node ON, explicit_off overridden OFF -> only idle_ready qualifies.
        let mut on = HarnessAutoUpgradePolicy {
            enabled: true,
            harnesses: BTreeMap::new(),
        };
        on.set_harness_mode("explicit_off".into(), HarnessAutoUpgradeMode::Off);
        let picked = select_auto_upgrades(&rep, &on, &BTreeMap::new(), &BTreeMap::new());
        assert_eq!(
            picked,
            vec![AutoUpgradeCandidate {
                harness: "idle_ready".into(),
                versions: BTreeMap::from([("cli".into(), "2.0.0".into())]),
            }]
        );

        // Failed auto-upgrade for exact same version set does not retry in a loop.
        let failed = BTreeMap::from([(
            "idle_ready".into(),
            BTreeMap::from([("cli".into(), "2.0.0".into())]),
        )]);
        assert!(select_auto_upgrades(&rep, &on, &BTreeMap::new(), &failed).is_empty());

        // Checking in progress suppresses auto-upgrades.
        let checking = report(rep.harnesses.clone(), true);
        assert!(
            select_auto_upgrades(&checking, &on, &BTreeMap::new(), &BTreeMap::new()).is_empty()
        );

        // In-flight or unknown receipt suppresses auto-upgrades.
        let runs = BTreeMap::from([(
            "idle_ready".into(),
            na::Receipt {
                operation: na::OperationId::new("op1").unwrap(),
                state: na::Observed::Unknown {
                    reason: "link lost".into(),
                },
            },
        )]);
        assert!(select_auto_upgrades(&rep, &on, &runs, &BTreeMap::new()).is_empty());
    }

    /// Frozen on-disk schema of `Saved` in `settings` (`harness.updates.<node>`).
    /// Any serialized shape change MUST break this test and ship with a store migration.
    #[test]
    fn saved_json_shape_is_frozen() {
        let frozen = serde_json::json!({
            "report": {
                "node": "local",
                "checking": false,
                "harnesses": [
                    {
                        "harness": "claude-code",
                        "name": "Claude Code",
                        "components": [
                            {
                                "id": "cli",
                                "name": "CLI",
                                "package": "@anthropic-ai/claude-code",
                                "managed": true,
                                "source": "steve_managed",
                                "upgrade_method": "steve_managed",
                                "upgrade_command": "npm install -g @anthropic-ai/claude-code",
                                "can_upgrade": true,
                                "state": "available",
                                "installed": "1.0.0",
                                "latest": "1.1.0",
                                "checked": 1700000000000i64,
                                "reason": { "code": "tool_missing", "tool": "npm" }
                            },
                            {
                                "id": "adapter",
                                "name": "Adapter",
                                "managed": false,
                                "source": "external",
                                "upgrade_method": "unsupported",
                                "can_upgrade": false,
                                "state": "unsupported",
                                "reason": { "code": "package_manager_unsupported", "manager": "pnpm" }
                            }
                        ],
                        "can_upgrade": false,
                        "active_sessions": 1,
                        "blocked_reason": "node_upgrade_in_progress",
                        "operation": {
                            "operation": "hup_op_1",
                            "state": { "state": "running" }
                        }
                    }
                ]
            },
            "at": 1700000000123i64,
            "runs": {
                "claude-code": {
                    "command": {
                        "id": "hup_op_1",
                        "issued_at": 1700000000000i64,
                        "accept_before": 1700000030000i64,
                        "grant": "instance_owner",
                        "body": {
                            "harness": "claude-code",
                            "versions": { "cli": "1.1.0" },
                            "self_update": ["cli"]
                        }
                    },
                    "receipt": {
                        "operation": "hup_op_1",
                        "state": {
                            "state": "finished",
                            "outcome": "failed",
                            "error": {
                                "code": "unavailable",
                                "retry": { "kind": "later", "after_ms": null },
                                "reason": null,
                                "params": {},
                                "safe_detail": "registry unreachable"
                            }
                        }
                    },
                    "reconciled": true,
                    "trigger": "auto",
                    "notified": true
                }
            },
            "auto_upgrade": {
                "enabled": false,
                "harnesses": { "claude-code": true }
            },
            "auto_failed": {
                "claude-code": { "cli": "1.1.0" }
            }
        });

        let parsed: Saved =
            serde_json::from_value(frozen.clone()).expect("frozen Saved fixture must deserialize");
        assert_eq!(parsed.at, 1_700_000_000_123);
        assert_eq!(
            parsed.report.as_ref().unwrap().harnesses[0].blocked_reason,
            Some(steve_types::devices::HarnessUpgradeBlockReason::NodeUpgradeInProgress)
        );
        assert_eq!(
            serde_json::to_value(&parsed).expect("Saved must serialize"),
            frozen,
            "Saved serialized shape drifted; add a migration and update the frozen fixture intentionally"
        );
    }

    #[test]
    fn corrupt_saved_state_returns_typed_conflict_without_fallback() {
        let dir = tempfile::tempdir().unwrap();
        let store =
            Store::open(dir.path().join("steve.sqlite3"), steve_store::NoPublisher).unwrap();
        store.ensure_workspace().unwrap();
        store
            .write(|tx| {
                WorkspaceSettings(tx).set(
                    WS,
                    &key("local"),
                    r#"{"report":{"node":"local","checking":false,"harnesses":[{"harness":"pi","name":"Pi","components":[],"can_upgrade":false,"blocked_reason":"legacy free-form text"}]},"at":1,"runs":{}}"#,
                    1,
                )
            })
            .unwrap();
        let err = store
            .read(|db| load(db, "local"))
            .expect_err("corrupt saved state must not fall back to default");
        assert!(matches!(err, HarnessUpdatesError::InvalidSavedState(_)));
        let api: ApiError = err.into();
        match api {
            ApiError::Contract(contract) => {
                assert_eq!(contract.code, steve_types::Code::Conflict);
                assert!(
                    contract
                        .reason
                        .is_some_and(|r| r.is(NodeReason::InvalidHarnessUpdateState))
                );
            }
            other => panic!("expected typed contract error, got {other:?}"),
        }
    }
}
