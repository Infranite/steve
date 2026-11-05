//! Run records (`flow_runs`, `flow_run_steps`) as schedules use them (schedules.md §7): insert, steps,
//! the overlap check, history pages and retention. S2's engine reuses the same rows.

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::Serialize;
use serde_json::{Value, json};
use steve_store::{ChangeEvent, Tx, uid};

use crate::FlowResult;

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

    /// One entry of `GET /schedules/:id/runs`.
    pub fn view(&self) -> RunView {
        RunView {
            id: self.id.clone(),
            at: self.at(),
            started: self.started,
            ended: self.ended,
            trigger: self.trigger_kind.clone(),
            status: self.status.clone(),
            outcome_label: self.outcome_label.clone(),
            item_id: self.work_item_id.clone(),
            cost_usd: self.cost,
            error: self.error.clone(),
        }
    }
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
}

/// A row to insert.
#[derive(Debug, Clone)]
pub struct NewRun<'a> {
    pub ws: &'a str,
    pub flow_id: &'a str,
    pub flow_version: i64,
    pub trigger_kind: &'a str,
    /// `running` or `skipped`.
    pub status: &'a str,
    pub outcome_label: &'a str,
    /// Next node to run (`None` for skipped rows).
    pub cursor: Option<&'a str>,
    pub context: &'a Value,
    pub now: i64,
}

pub fn insert(tx: &Tx, n: &NewRun) -> FlowResult<String> {
    let id = uid("run");
    let ended = (n.status != "running").then_some(n.now);
    tx.execute(
        "INSERT INTO flow_runs (id, workspace_id, flow_id, flow_version, trigger_kind, status, outcome_label, cursor, context, started, ended) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            id,
            n.ws,
            n.flow_id,
            n.flow_version,
            n.trigger_kind,
            n.status,
            n.outcome_label,
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
    tx.publish(ChangeEvent::Run {
        id: run_id.into(),
        flow_id: flow_id.into(),
        ws: ws.into(),
    });
    tx.publish(ChangeEvent::Flow {
        id: flow_id.into(),
        ws: ws.into(),
    });
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
pub fn due_retries(c: &Connection, now: i64) -> FlowResult<Vec<(String, i64)>> {
    let mut st = c.prepare_cached(
        "SELECT id, retry_at FROM flow_runs WHERE status = 'waiting' AND retry_at IS NOT NULL AND retry_at <= ? ORDER BY retry_at",
    )?;
    let rows = st
        .query_map([now], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Claims a due retry: `waiting` → `running` only if nobody else did. Returns whether it was ours.
pub fn claim_retry(tx: &Tx, id: &str, retry_at: i64) -> FlowResult<bool> {
    Ok(tx.execute(
        "UPDATE flow_runs SET status = 'running', retry_at = NULL WHERE id = ? AND status = 'waiting' AND retry_at = ?",
        params![id, retry_at],
    )? == 1)
}

/// Schedule runs past retention (§7): older than 90 days and beyond the latest 200 of their
/// schedule; never one that is still going. Steps go with them. Returns the number of runs deleted.
pub fn prune(tx: &Tx, now: i64) -> FlowResult<usize> {
    const OLD: &str = "SELECT id FROM (SELECT r.id, r.started, r.status, ROW_NUMBER() OVER (PARTITION BY r.flow_id ORDER BY r.started DESC, r.rowid DESC) AS rn FROM flow_runs r JOIN flows f ON f.id = r.flow_id WHERE f.kind = 'schedule') WHERE rn > ?1 AND started < ?2 AND status NOT IN ('running', 'waiting')";
    let cut = now - KEEP_MS;
    tx.execute(
        &format!("DELETE FROM flow_run_steps WHERE run_id IN ({OLD})"),
        params![KEEP_RUNS, cut],
    )?;
    Ok(tx.execute(
        &format!("DELETE FROM flow_runs WHERE id IN ({OLD})"),
        params![KEEP_RUNS, cut],
    )?)
}
