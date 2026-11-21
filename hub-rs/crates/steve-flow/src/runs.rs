//! Run records (`flow_runs`, `flow_run_steps`) as schedules use them: insert, steps,
//! the overlap check, history pages and retention. S2's engine reuses the same rows.

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::Serialize;
use serde_json::{Value, json};
use steve_store::{Tx, uid};

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

/// One `flow_runs` row.
#[derive(Debug, Clone, PartialEq)]
pub struct RunRow {
    pub id: String,
    pub workspace_id: String,
    pub flow_id: String,
    pub flow_version: i64,
    pub trigger_kind: String,
    pub status: String,
    pub outcome: Option<String>,
    pub outcome_label: String,
    pub cursor: Option<String>,
    pub context: Value,
    pub error: Option<String>,
    pub retry_at: Option<i64>,
    pub attempts: i64,
    pub work_item_id: Option<String>,
    pub cost: f64,
    pub started: i64,
    pub ended: Option<i64>,
    /// The event that started it (event-triggered runs).
    pub event_id: Option<String>,
    pub dry_run: bool,
    /// The run whose `flow` node called this one, and how deep that nesting is.
    pub parent_run: Option<String>,
    pub depth: i64,
}

impl RunRow {
    fn from_row(r: &Row) -> rusqlite::Result<Self> {
        let ctx: String = r.get("context")?;
        Ok(Self {
            id: r.get("id")?,
            workspace_id: r.get("workspace_id")?,
            flow_id: r.get("flow_id")?,
            flow_version: r.get("flow_version")?,
            trigger_kind: r.get("trigger_kind")?,
            status: r.get("status")?,
            outcome: r.get("outcome")?,
            outcome_label: r.get("outcome_label")?,
            cursor: r.get("cursor")?,
            context: serde_json::from_str(&ctx).unwrap_or_else(|_| json!({})),
            error: r.get("error")?,
            retry_at: r.get("retry_at")?,
            attempts: r.get("attempts")?,
            work_item_id: r.get("work_item_id")?,
            cost: r.get("cost")?,
            started: r.get("started")?,
            ended: r.get("ended")?,
            event_id: r.get("event_id")?,
            dry_run: r.get("dry_run")?,
            parent_run: r.get("parent_run")?,
            depth: r.get("depth")?,
        })
    }

    /// The planned time this run stands for (`context.at`); the start time for older rows.
    pub fn at(&self) -> i64 {
        self.context
            .get("at")
            .and_then(Value::as_i64)
            .unwrap_or(self.started)
    }

    /// Human-facing text only. Persistence, comparisons and budgets use the raw fields.
    /// Call under `stored` when this text becomes part of another persisted/model-facing goal.
    pub fn display_text(&self) -> RunText {
        let startup_error = (self.status == "failed"
            && self.error.as_deref() == Some(START_REJECTED_CODE))
        .then(|| start_failures::error_text(&self.context));
        RunText {
            outcome_label: startup_error
                .clone()
                .unwrap_or_else(|| self.outcome_label.clone()),
            error: startup_error.or_else(|| self.error.clone()),
        }
    }

    /// One entry of `GET /schedules/:id/runs`.
    pub fn view(&self) -> RunView {
        let text = self.display_text();
        RunView {
            id: self.id.clone(),
            at: self.at(),
            started: self.started,
            ended: self.ended,
            trigger: self.trigger_kind.clone(),
            status: self.status.clone(),
            outcome_label: text.outcome_label,
            item_id: self.work_item_id.clone(),
            cost_usd: self.cost,
            error: text.error,
            start_failures: self.context.get("start_failures").and_then(Value::as_i64),
            first_at: self.context.get("first_at").and_then(Value::as_i64),
            startup: start_failures::summary(&self.context),
        }
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

pub fn connector_runs(c: &Connection, ws: &str, event: &str) -> FlowResult<Vec<ConnectorRun>> {
    let mut st = c.prepare_cached(
        "SELECT r.*, f.name AS flow_name, f.deleted AS flow_deleted, i.id AS item_id
           FROM flow_runs r JOIN flows f ON f.id = r.flow_id AND f.workspace_id = r.workspace_id
           LEFT JOIN items i ON i.id = r.work_item_id AND i.workspace_id = r.workspace_id
          WHERE r.workspace_id = ? AND r.event_id = ? AND r.dry_run = 0
          ORDER BY r.started, r.id",
    )?;
    Ok(st
        .query_map(params![ws, event], |r| {
            let run = RunRow::from_row(r)?;
            let text = run.display_text();
            Ok(ConnectorRun {
                id: run.id,
                flow_id: run.flow_id,
                flow_name: r.get("flow_name")?,
                flow_deleted: r.get("flow_deleted")?,
                status: run.status,
                outcome: run.outcome,
                label: text.outcome_label,
                error: text.error,
                item_id: r.get("item_id")?,
            })
        })?
        .collect::<Result<_, _>>()?)
}

/// A row to insert.
#[derive(Debug, Clone)]
pub struct NewRun<'a> {
    pub ws: &'a str,
    pub flow_id: &'a str,
    pub flow_version: i64,
    pub trigger_kind: &'a str,
    /// Initial run status (including a terminal startup failure).
    pub status: &'a str,
    pub outcome_label: &'a str,
    /// A terminal startup rejection is inserted with its error, never patched afterwards.
    pub error: Option<&'a str>,
    /// Next node to run (`None` for skipped rows).
    pub cursor: Option<&'a str>,
    pub context: &'a Value,
    pub now: i64,
}

pub fn insert(tx: &Tx, n: &NewRun) -> FlowResult<String> {
    insert_stored(tx, n).map_err(Into::into)
}

pub(crate) fn insert_stored(tx: &Tx, n: &NewRun) -> steve_store::Result<String> {
    let id = uid("run");
    let ended = (n.status != "running").then_some(n.now);
    tx.execute(
        "INSERT INTO flow_runs (id, workspace_id, flow_id, flow_version, trigger_kind, status, outcome_label, error, cursor, context, started, ended) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            id,
            n.ws,
            n.flow_id,
            n.flow_version,
            n.trigger_kind,
            n.status,
            n.outcome_label,
            n.error,
            n.cursor,
            n.context.to_string(),
            n.now,
            ended
        ],
    )?;
    publish(tx, n.ws, n.flow_id, &id);
    Ok(id)
}

/// `{type:"run"}` and `{type:"flow"}` (the schedule list shows the last run).
pub fn publish(tx: &Tx, ws: &str, flow_id: &str, run_id: &str) {
    tx.publish(steve_types::DomainEvent::from(
        steve_types::AutomationEvent::Run {
            id: run_id.into(),
            flow_id: flow_id.into(),
            ws: ws.into(),
        },
    ));
    tx.publish(steve_types::DomainEvent::from(
        steve_types::AutomationEvent::Flow {
            id: flow_id.into(),
            ws: ws.into(),
        },
    ));
}

/// One step of a run.
#[derive(Debug, Clone)]
pub struct NewStep<'a> {
    pub node_id: &'a str,
    /// `ok | skipped | waiting | failed`
    pub status: &'a str,
    pub input: &'a Value,
    pub output: &'a Value,
    pub ms: f64,
    pub error: Option<&'a str>,
    pub started: i64,
}

pub fn add_step(tx: &Tx, ws: &str, run_id: &str, s: &NewStep) -> FlowResult<i64> {
    let seq: i64 = tx.query_row(
        "SELECT coalesce(max(seq), 0) + 1 FROM flow_run_steps WHERE run_id = ?",
        [run_id],
        |r| r.get(0),
    )?;
    tx.execute(
        "INSERT INTO flow_run_steps (run_id, seq, workspace_id, node_id, status, input, output, ms, error, started) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            run_id,
            seq,
            ws,
            s.node_id,
            s.status,
            s.input.to_string(),
            s.output.to_string(),
            s.ms,
            s.error,
            s.started
        ],
    )?;
    Ok(seq)
}

pub fn get(c: &Connection, id: &str) -> FlowResult<Option<RunRow>> {
    Ok(c.query_row(
        "SELECT * FROM flow_runs WHERE id = ?",
        [id],
        RunRow::from_row,
    )
    .optional()?)
}

/// Sets what [`insert`] leaves at its defaults: the event, dry run, parent run and depth.
pub fn set_origin(
    tx: &Tx,
    id: &str,
    event_id: Option<&str>,
    dry_run: bool,
    parent_run: Option<&str>,
    depth: i64,
) -> FlowResult<()> {
    tx.execute(
        "UPDATE flow_runs SET event_id = ?, dry_run = ?, parent_run = ?, depth = ? WHERE id = ?",
        params![event_id, dry_run, parent_run, depth, id],
    )?;
    Ok(())
}

/// Saves the mutable part of a run (status, result, cursor, context, retry, cost).
pub fn save(tx: &Tx, r: &RunRow) -> FlowResult<()> {
    tx.execute(
        "UPDATE flow_runs SET status = ?, outcome = ?, outcome_label = ?, cursor = ?, context = ?, error = ?, retry_at = ?, attempts = ?, work_item_id = ?, cost = ?, ended = ? WHERE id = ?",
        params![
            r.status,
            r.outcome,
            r.outcome_label,
            r.cursor,
            r.context.to_string(),
            r.error,
            r.retry_at,
            r.attempts,
            r.work_item_id,
            r.cost,
            r.ended,
            r.id
        ],
    )?;
    publish(tx, &r.workspace_id, &r.flow_id, &r.id);
    Ok(())
}

/// Is an earlier run of this flow still `running` or `waiting` (the overlap rule)?
pub fn active_exists(c: &Connection, ws: &str, flow_id: &str) -> FlowResult<bool> {
    Ok(c.query_row(
        "SELECT EXISTS (SELECT 1 FROM flow_runs WHERE workspace_id = ? AND flow_id = ? AND status IN ('running', 'waiting'))",
        params![ws, flow_id],
        |r| r.get(0),
    )?)
}

/// The latest run that is not `skipped` (`Schedule.last`, and “上次结果” in the item goal).
pub fn last_real(
    c: &Connection,
    ws: &str,
    flow_id: &str,
    except: Option<&str>,
) -> FlowResult<Option<RunRow>> {
    Ok(c.query_row(
        "SELECT * FROM flow_runs WHERE workspace_id = ? AND flow_id = ? AND status != 'skipped' AND id != ? ORDER BY started DESC, rowid DESC LIMIT 1",
        params![ws, flow_id, except.unwrap_or("")],
        RunRow::from_row,
    )
    .optional()?)
}

/// A page of history, newest first: `started < before` (when given), at most [`MAX_PAGE`].
pub fn page(
    c: &Connection,
    ws: &str,
    flow_id: &str,
    before: Option<i64>,
    limit: i64,
) -> FlowResult<Vec<RunRow>> {
    page_of(c, ws, flow_id, before, limit, None)
}

/// [`page`], only dry runs (`Some(true)`) or only real ones (`Some(false)`).
pub fn page_of(
    c: &Connection,
    ws: &str,
    flow_id: &str,
    before: Option<i64>,
    limit: i64,
    dry: Option<bool>,
) -> FlowResult<Vec<RunRow>> {
    let mut st = c.prepare_cached(
        "SELECT * FROM flow_runs WHERE workspace_id = ?5 AND flow_id = ?1 AND started < ?2 AND (?4 IS NULL OR dry_run = ?4)
           ORDER BY started DESC, rowid DESC LIMIT ?3",
    )?;
    let rows = st
        .query_map(
            params![
                flow_id,
                before.unwrap_or(i64::MAX),
                limit.clamp(1, MAX_PAGE),
                dry,
                ws
            ],
            RunRow::from_row,
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Runs of `waiting` status whose retry is due.
pub(crate) fn due_retries(c: &Connection, now: i64) -> FlowResult<Vec<i64>> {
    let mut st=c.prepare_cached("SELECT rowid FROM flow_runs WHERE status='waiting' AND retry_at IS NOT NULL AND (typeof(retry_at)<>'integer' OR retry_at<=?1) ORDER BY retry_at,id LIMIT ?2")?;
    Ok(st
        .query_map(params![now, crate::scheduler::BATCH as i64], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?)
}

/// Claims a retry or ends unusable stored state. A true result needs completion bookkeeping.
pub fn claim_retry(tx: &Tx, id: &str, retry_at: i64, now: i64) -> FlowResult<bool> {
    let invalid: Option<bool> = tx.query_row("SELECT cursor IS NULL OR attempts > ?2 OR CASE WHEN json_valid(context) THEN json_type(context)<>'object' ELSE 1 END FROM flow_runs WHERE id=?1 AND status='waiting' AND retry_at=?3", params![id, crate::exec::MAX_RETRIES, retry_at], |r| r.get(0)).optional()?;
    let Some(invalid) = invalid else {
        return Ok(false);
    };
    if invalid {
        return fail_due(tx, id, now, "Invalid stored retry state");
    }
    Ok(tx.execute(
        "UPDATE flow_runs SET status = 'running', retry_at = NULL WHERE id = ? AND status = 'waiting' AND retry_at = ?",
        params![id, retry_at],
    )? == 1)
}

/// An unusable persisted wait/retry is terminal, using the run's existing failed state.
pub(crate) fn fail_due(tx: &Tx, id: &str, now: i64, reason: &str) -> FlowResult<bool> {
    let meta: Option<(String, String)> = tx
        .query_row(
            "SELECT workspace_id,flow_id FROM flow_runs WHERE id=? AND status='waiting'",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((ws, flow)) = meta else {
        return Ok(false);
    };
    tx.execute("UPDATE flow_runs SET status='failed', retry_at=NULL, error=?2, outcome_label=?2, ended=?3 WHERE id=?1 AND status='waiting'", params![id, reason, now])?;
    tx.execute("DELETE FROM flow_waits WHERE run_id=?", [id])?;
    publish(tx, &ws, &flow, id);
    // Dispatching an already failed run performs the engine's normal parent/event completion.
    Ok(true)
}

/// Schedule runs past retention: older than 90 days and beyond the latest 200 of their
/// schedule; never one that is still going. Steps go with them. Returns the number of runs deleted.
pub fn prune(tx: &Tx, now: i64) -> FlowResult<usize> {
    let old = format!(
        "SELECT id FROM (SELECT r.id, r.rowid AS rid, r.started, r.status, ROW_NUMBER() OVER (PARTITION BY r.flow_id ORDER BY r.started DESC, r.rowid DESC) AS rn FROM flow_runs r JOIN flows f ON f.id = r.flow_id WHERE f.kind = 'schedule') WHERE rn > ?1 AND started < ?2 AND status NOT IN ('running', 'waiting') AND rid NOT IN ({})",
        start_failures::ANCHORS
    );
    let cut = now - KEEP_MS;
    tx.execute(
        &format!("DELETE FROM flow_run_steps WHERE run_id IN ({old})"),
        params![KEEP_RUNS, cut],
    )?;
    Ok(tx.execute(
        &format!("DELETE FROM flow_runs WHERE id IN ({old})"),
        params![KEEP_RUNS, cut],
    )?)
}
