//! Run records (`flow_runs`, `flow_run_steps`) as schedules use them: insert, steps,
//! the overlap check, history pages and retention. S2's engine reuses the same rows.

use serde::Serialize;
use serde_json::{Value, json};
use steve_store::{FlowRows, ItemReads, Read, RunRows, W};

use crate::FlowResult;

mod start_failures;
pub(crate) use start_failures::record_start_rejection;
pub use start_failures::{
    FailedStart, START_REJECTED_CODE, StartupCheck, StartupDiagnostic, StartupState, StartupStatus,
    StartupSummary, record_start_failure, record_started, startup_check, startup_diagnostic,
    startup_state,
};

/// Runs kept per schedule regardless of age, and the age below which every run is kept.
pub const KEEP_RUNS: i64 = 200;
pub const KEEP_MS: i64 = 90 * 24 * 3_600_000;

/// Page size cap of `GET /schedules/:id/runs`.
pub const MAX_PAGE: i64 = 50;

pub use steve_store::RunRow;

/// Human-facing text only. Persistence, comparisons and budgets use the raw fields.
/// Call under `stored` when this text becomes part of another persisted/model-facing goal.
pub fn display_text(run: &RunRow) -> RunText {
    let startup_error = (run.status == "failed"
        && run.error.as_deref() == Some(START_REJECTED_CODE))
    .then(|| start_failures::error_text(&run.context));
    RunText {
        outcome_label: startup_error
            .clone()
            .unwrap_or_else(|| run.outcome_label.clone()),
        error: startup_error.or_else(|| run.error.clone()),
    }
}

/// One entry of `GET /schedules/:id/runs`.
pub fn view(run: &RunRow) -> RunView {
    let text = display_text(run);
    RunView {
        id: run.id.clone(),
        at: run.at(),
        started: run.started,
        ended: run.ended,
        trigger: run.trigger_kind.clone(),
        status: run.status.clone(),
        outcome_label: text.outcome_label,
        item_id: run.work_item_id.clone(),
        cost_usd: run.cost,
        error: text.error,
        start_failures: run.context.get("start_failures").and_then(Value::as_i64),
        first_at: run.context.get("first_at").and_then(Value::as_i64),
        startup: start_failures::summary(&run.context),
    }
}

/// Reader-localized label and error shared by all run projections.
#[derive(Debug, Clone, PartialEq)]
pub struct RunText {
    pub outcome_label: String,
    pub error: Option<String>,
}

/// The `Run` entry of the schedule pages.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RunView {
    pub id: String,
    pub at: i64,
    pub started: i64,
    pub ended: Option<i64>,
    pub trigger: String,
    pub status: String,
    pub outcome_label: String,
    pub item_id: Option<String>,
    pub cost_usd: f64,
    pub error: Option<String>,
    /// Consecutive startup rejections represented by this entry, when counted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub start_failures: Option<i64>,
    /// First rejection represented by this entry; started/ended describe its latest occurrence.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_at: Option<i64>,
    /// Historical budget at this run, not the current configuration's liveness.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub startup: Option<StartupSummary>,
}

/// Connector-facing projection of canonical flow runs. A connector stores an event reference
/// alongside its own receipt, then reads these rows; it does not maintain a copy of run status.
#[derive(Debug, Serialize)]
pub struct ConnectorRun {
    pub id: String,
    pub flow_id: String,
    pub flow_name: String,
    pub flow_deleted: bool,
    pub status: String,
    pub outcome: Option<String>,
    pub label: String,
    pub error: Option<String>,
    pub item_id: Option<String>,
}

pub fn connector_runs(
    c: &mut impl Read,
    ws: &str,
    event: &str,
) -> steve_store::Result<Vec<ConnectorRun>> {
    let mut runs = RunRows(c).by_event(ws, event)?;
    runs.sort_by(|a, b| (a.started, &a.id).cmp(&(b.started, &b.id)));
    let flows = FlowRows(c).labels(ws, runs.iter().map(|r| r.flow_id.as_str()))?;
    let items =
        ItemReads(c).existing_ids(ws, runs.iter().filter_map(|r| r.work_item_id.as_deref()))?;
    Ok(runs
        .into_iter()
        .filter_map(|run| {
            let (name, _, deleted) = flows.get(&run.flow_id)?;
            let text = display_text(&run);
            Some(ConnectorRun {
                id: run.id,
                flow_id: run.flow_id,
                flow_name: name.clone(),
                flow_deleted: *deleted,
                status: run.status,
                outcome: run.outcome,
                label: text.outcome_label,
                error: text.error,
                item_id: run.work_item_id.filter(|id| items.contains(id)),
            })
        })
        .collect())
}

pub use steve_store::{NewRun, NewStep};

pub fn get(c: &mut impl Read, id: &str) -> steve_store::Result<Option<RunRow>> {
    RunRows(c).get(id)
}

pub fn save(w: &mut W<'_>, run: &RunRow) -> steve_store::Result<()> {
    RunRows(w).save(run)
}

/// A page of history with the newest run first.
pub fn page(
    c: &mut impl steve_store::Read,
    ws: &str,
    flow_id: &str,
    before: Option<i64>,
    limit: i64,
) -> steve_store::Result<Vec<RunRow>> {
    page_of(c, ws, flow_id, before, limit, None)
}

pub fn page_of(
    c: &mut impl steve_store::Read,
    ws: &str,
    flow_id: &str,
    before: Option<i64>,
    limit: i64,
    dry: Option<bool>,
) -> steve_store::Result<Vec<RunRow>> {
    steve_store::RunRows(c).page(
        ws,
        flow_id,
        before.unwrap_or(i64::MAX),
        limit.clamp(1, MAX_PAGE) as u64,
        dry,
    )
}

/// Schedule runs past retention: older than 90 days and beyond the latest 200 of their
/// schedule; never one that is still going. Steps go with them. Returns the number of runs deleted.
pub fn prune(w: &mut steve_store::W<'_>, now: i64) -> steve_store::Result<usize> {
    steve_store::RunRows(w).prune_schedules(KEEP_RUNS, now - KEEP_MS)
}
