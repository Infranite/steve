//! YOLO storage (migration 14): the on/off/inherit rows per workspace, project and
//! item, the windows a workspace switch was on, and the ledger of what YOLO did or left alone.
//!
//! Free functions over a connection (reads) or a [`Tx`] (writes), so steve-core can change a scope, write
//! its audit row and its revision in one transaction. Writes queue the `yolo` / `yolo_action` events.

use rusqlite::{Connection, Row, params};
use serde::Serialize;
use serde_json::{Value, json};

use super::{all, one, to_json};
use crate::types::json_or;
use crate::{ChangeEvent, Result, Tx, uid};

/// `object_type` of the workspace row.
pub const WORKSPACE: &str = "workspace";
pub const PROJECT: &str = "project";
pub const ITEM: &str = "item";
/// `revisions.object_type` of every scope change.
pub const REVISION_TYPE: &str = "yolo_scope";

/// One `yolo_scopes` row.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ScopeRow {
    pub workspace_id: String,
    pub object_type: String,
    pub object_id: String,
    /// `on` | `off` | `inherit`
    pub mode: String,
    /// Breaker state of the workspace row: `{code, reason, at}`.
    pub paused: Option<Value>,
    pub reason: String,
    pub set_by: String,
    pub revision: i64,
    pub updated: i64,
}

impl ScopeRow {
    fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            workspace_id: r.get(0)?,
            object_type: r.get(1)?,
            object_id: r.get(2)?,
            mode: r.get(3)?,
            paused: r
                .get::<_, Option<String>>(4)?
                .map(|s| json_or(Some(s), Value::Null))
                .filter(|v| !v.is_null()),
            reason: r.get(5)?,
            set_by: r.get(6)?,
            revision: r.get(7)?,
            updated: r.get(8)?,
        })
    }

    /// The part a revision records.
    fn snapshot(&self) -> Value {
        json!({ "mode": self.mode, "paused": self.paused, "reason": self.reason })
    }
}

const SCOPE_COLS: &str =
    "workspace_id, object_type, object_id, mode, paused, reason, set_by, revision, updated";

pub fn scope_get(
    c: &Connection,
    ws: &str,
    object_type: &str,
    id: &str,
) -> Result<Option<ScopeRow>> {
    one(
        c,
        &format!(
            "SELECT {SCOPE_COLS} FROM yolo_scopes WHERE workspace_id = ? AND object_type = ? AND object_id = ?"
        ),
        params![ws, object_type, id],
        ScopeRow::from_row,
    )
}

/// Every row of a workspace: the workspace row first, then projects, then items.
pub fn scopes(c: &Connection, ws: &str) -> Result<Vec<ScopeRow>> {
    all(
        c,
        &format!(
            "SELECT {SCOPE_COLS} FROM yolo_scopes WHERE workspace_id = ?
             ORDER BY CASE object_type WHEN 'workspace' THEN 0 WHEN 'project' THEN 1 ELSE 2 END, object_id"
        ),
        [ws],
        ScopeRow::from_row,
    )
}

/// What [`scope_put`] changes; `None` keeps the stored value (or the default for a new row).
#[derive(Debug, Clone, Default)]
pub struct ScopeChange<'a> {
    pub mode: Option<&'a str>,
    /// `Some(None)` clears the breaker state.
    pub paused: Option<Option<&'a Value>>,
    pub reason: Option<&'a str>,
}

/// Inserts or updates one row, bumps its `revision`, writes the `revisions` row (object id
/// `<type>:<id>`, patch `{before, after}`) and queues `{type:"yolo", ws}`. Returns `(before, after)`.
/// A change that changes nothing writes nothing and returns the row twice.
pub fn scope_put(
    tx: &Tx,
    ws: &str,
    object_type: &str,
    id: &str,
    ch: &ScopeChange,
    by: &str,
    now: i64,
) -> Result<(Option<ScopeRow>, ScopeRow)> {
    let before = scope_get(tx, ws, object_type, id)?;
    let mode = ch
        .mode
        .map(str::to_string)
        .or_else(|| before.as_ref().map(|b| b.mode.clone()))
        .unwrap_or_else(|| {
            if object_type == WORKSPACE {
                "off"
            } else {
                "inherit"
            }
            .into()
        });
    let paused = match ch.paused {
        Some(p) => p.cloned(),
        None => before.as_ref().and_then(|b| b.paused.clone()),
    };
    let reason = ch
        .reason
        .map(str::to_string)
        .or_else(|| before.as_ref().map(|b| b.reason.clone()))
        .unwrap_or_default();
    if let Some(b) = &before
        && b.mode == mode
        && b.paused == paused
        && b.reason == reason
    {
        return Ok((before.clone(), b.clone()));
    }
    let revision = before.as_ref().map_or(1, |b| b.revision + 1);
    let paused_s = paused.as_ref().map(to_json).transpose()?;
    tx.prepare_cached(
        "INSERT INTO yolo_scopes (workspace_id, object_type, object_id, mode, paused, reason, set_by, revision, updated)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
         ON CONFLICT(workspace_id, object_type, object_id) DO UPDATE SET mode = excluded.mode,
           paused = excluded.paused, reason = excluded.reason, set_by = excluded.set_by,
           revision = excluded.revision, updated = excluded.updated",
    )?
    .execute(params![ws, object_type, id, mode, paused_s, reason, by, revision, now])?;
    let after = super::must(scope_get(tx, ws, object_type, id)?)?;
    crate::base::revision(
        tx,
        ws,
        REVISION_TYPE,
        &format!("{object_type}:{id}"),
        revision,
        &before.as_ref().map_or(Value::Null, ScopeRow::snapshot),
        &after.snapshot(),
        by,
    )?;
    tx.publish(ChangeEvent::Yolo { ws: ws.into() });
    Ok((before, after))
}

/// One `yolo_windows` row: a stretch during which the workspace switch was on.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WindowRow {
    pub id: String,
    pub workspace_id: String,
    pub started: i64,
    pub ended: Option<i64>,
    /// `off` | `tripped`
    pub ended_why: Option<String>,
    /// The cards open when it started may be handled too.
    pub include_open: bool,
    pub counters: Value,
}

impl WindowRow {
    fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get(0)?,
            workspace_id: r.get(1)?,
            started: r.get(2)?,
            ended: r.get(3)?,
            ended_why: r.get(4)?,
            include_open: r.get::<_, i64>(5)? != 0,
            counters: json_or(r.get(6)?, json!({})),
        })
    }
}

const WINDOW_COLS: &str = "id, workspace_id, started, ended, ended_why, include_open, counters";

/// The window still running in a workspace, if any.
pub fn window_current(c: &Connection, ws: &str) -> Result<Option<WindowRow>> {
    one(
        c,
        &format!(
            "SELECT {WINDOW_COLS} FROM yolo_windows WHERE workspace_id = ? AND ended IS NULL ORDER BY started DESC, rowid DESC LIMIT 1"
        ),
        [ws],
        WindowRow::from_row,
    )
}

pub fn window_get(c: &Connection, id: &str) -> Result<Option<WindowRow>> {
    one(
        c,
        &format!("SELECT {WINDOW_COLS} FROM yolo_windows WHERE id = ?"),
        [id],
        WindowRow::from_row,
    )
}

/// Windows of a workspace, newest first.
pub fn windows(c: &Connection, ws: &str, limit: i64) -> Result<Vec<WindowRow>> {
    all(
        c,
        &format!(
            "SELECT {WINDOW_COLS} FROM yolo_windows WHERE workspace_id = ? ORDER BY started DESC, rowid DESC LIMIT ?"
        ),
        params![ws, limit],
        WindowRow::from_row,
    )
}

/// Starts a window (`yw_…`).
pub fn window_open(tx: &Tx, ws: &str, include_open: bool, now: i64) -> Result<WindowRow> {
    let id = uid("yw");
    tx.prepare_cached(
        "INSERT INTO yolo_windows (id, workspace_id, started, include_open) VALUES (?, ?, ?, ?)",
    )?
    .execute(params![id, ws, now, include_open as i64])?;
    super::must(window_get(tx, &id)?)
}

/// Ends a running window; a window that already ended keeps its first end.
pub fn window_close(tx: &Tx, id: &str, why: &str, now: i64) -> Result<()> {
    tx.prepare_cached(
        "UPDATE yolo_windows SET ended = ?, ended_why = ? WHERE id = ? AND ended IS NULL",
    )?
    .execute(params![now, why, id])?;
    Ok(())
}

/// A new `yolo_actions` row.
#[derive(Debug, Clone, Default)]
pub struct NewAction {
    pub workspace_id: String,
    pub window_id: Option<String>,
    pub decision_id: Option<String>,
    pub item_id: Option<String>,
    pub session_id: Option<String>,
    pub kind: String,
    pub subject_type: String,
    pub rule: String,
    /// `scheduled` | `decided` | `declined` | `muted` | `cancelled` | `failed`
    pub outcome: String,
    pub choice: Option<String>,
    pub text: Option<String>,
    pub reason: String,
    pub checks: Value,
    pub due_at: Option<i64>,
    pub undo: Option<Value>,
}

/// One `yolo_actions` row.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ActionRow {
    pub id: String,
    pub workspace_id: String,
    pub window_id: Option<String>,
    pub decision_id: Option<String>,
    pub item_id: Option<String>,
    pub session_id: Option<String>,
    pub kind: String,
    pub subject_type: String,
    pub rule: String,
    pub outcome: String,
    pub choice: Option<String>,
    pub text: Option<String>,
    pub reason: String,
    pub checks: Value,
    pub cost: Option<f64>,
    pub due_at: Option<i64>,
    pub done_at: Option<i64>,
    pub undo: Option<Value>,
    pub undone: Option<i64>,
    pub created: i64,
}

impl ActionRow {
    fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get(0)?,
            workspace_id: r.get(1)?,
            window_id: r.get(2)?,
            decision_id: r.get(3)?,
            item_id: r.get(4)?,
            session_id: r.get(5)?,
            kind: r.get(6)?,
            subject_type: r.get(7)?,
            rule: r.get(8)?,
            outcome: r.get(9)?,
            choice: r.get(10)?,
            text: r.get(11)?,
            reason: r.get(12)?,
            checks: json_or(r.get(13)?, json!({})),
            cost: r.get(14)?,
            due_at: r.get(15)?,
            done_at: r.get(16)?,
            undo: r
                .get::<_, Option<String>>(17)?
                .map(|s| json_or(Some(s), Value::Null))
                .filter(|v| !v.is_null()),
            undone: r.get(18)?,
            created: r.get(19)?,
        })
    }
}

const ACTION_COLS: &str = "id, workspace_id, window_id, decision_id, item_id, session_id, kind, subject_type, rule, outcome, choice, text, reason, checks, cost, due_at, done_at, undo, undone, created";

pub fn action_get(c: &Connection, id: &str) -> Result<Option<ActionRow>> {
    one(
        c,
        &format!("SELECT {ACTION_COLS} FROM yolo_actions WHERE id = ?"),
        [id],
        ActionRow::from_row,
    )
}

/// Records one action (`ya_…`) and queues `{type:"yolo_action", id, decision_id, ws}`.
pub fn action_add(tx: &Tx, a: &NewAction, now: i64) -> Result<ActionRow> {
    let id = uid("ya");
    let undo = a.undo.as_ref().map(to_json).transpose()?;
    let checks = if a.checks.is_null() {
        "{}".to_string()
    } else {
        to_json(&a.checks)?
    };
    tx.prepare_cached(
        "INSERT INTO yolo_actions (id, workspace_id, window_id, decision_id, item_id, session_id, kind, subject_type,
           rule, outcome, choice, text, reason, checks, due_at, undo, created)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )?
    .execute(params![
        id,
        a.workspace_id,
        a.window_id,
        a.decision_id,
        a.item_id,
        a.session_id,
        a.kind,
        a.subject_type,
        a.rule,
        a.outcome,
        a.choice,
        a.text,
        a.reason,
        checks,
        a.due_at,
        undo,
        now
    ])?;
    tx.publish(ChangeEvent::YoloAction {
        id: id.clone(),
        decision_id: a.decision_id.clone(),
        ws: a.workspace_id.clone(),
    });
    super::must(action_get(tx, &id)?)
}

/// Actions of one window, oldest first.
pub fn actions_of_window(c: &Connection, window: &str) -> Result<Vec<ActionRow>> {
    all(
        c,
        &format!(
            "SELECT {ACTION_COLS} FROM yolo_actions WHERE window_id = ? ORDER BY created, rowid"
        ),
        [window],
        ActionRow::from_row,
    )
}

/// Number of actions per outcome in one window.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Counts {
    pub decided: i64,
    pub declined: i64,
    pub scheduled: i64,
}

pub fn counts(c: &Connection, window: &str) -> Result<Counts> {
    let mut st = c.prepare_cached(
        "SELECT outcome, count(*) FROM yolo_actions WHERE window_id = ? GROUP BY outcome",
    )?;
    let rows = st.query_map([window], |r| {
        Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?))
    })?;
    let mut n = Counts::default();
    for row in rows {
        let (outcome, k) = row?;
        match outcome.as_str() {
            "decided" => n.decided = k,
            "declined" => n.declined = k,
            "scheduled" => n.scheduled = k,
            _ => {}
        }
    }
    Ok(n)
}

/// Cancels every scheduled action of a window (the switch went off or the breaker tripped); returns how many.
pub fn cancel_scheduled(tx: &Tx, ws: &str, window: &str, now: i64) -> Result<usize> {
    let ids: Vec<(String, Option<String>)> = {
        let mut st = tx.prepare_cached(
            "SELECT id, decision_id FROM yolo_actions WHERE window_id = ? AND outcome = 'scheduled'",
        )?;
        let rows = st.query_map([window], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect::<rusqlite::Result<_>>()?
    };
    for (id, decision_id) in &ids {
        tx.prepare_cached(
            "UPDATE yolo_actions SET outcome = 'cancelled', done_at = ? WHERE id = ?",
        )?
        .execute(params![now, id])?;
        tx.publish(ChangeEvent::YoloAction {
            id: id.clone(),
            decision_id: decision_id.clone(),
            ws: ws.into(),
        });
    }
    Ok(ids.len())
}

/// Every action about one decision, oldest first.
pub fn actions_of_decision(c: &Connection, decision: &str) -> Result<Vec<ActionRow>> {
    all(
        c,
        &format!(
            "SELECT {ACTION_COLS} FROM yolo_actions WHERE decision_id = ? ORDER BY created, rowid"
        ),
        [decision],
        ActionRow::from_row,
    )
}

/// The latest `limit` actions about one item, newest first.
pub fn actions_of_item(c: &Connection, item: &str, limit: i64) -> Result<Vec<ActionRow>> {
    all(
        c,
        &format!(
            "SELECT {ACTION_COLS} FROM yolo_actions WHERE item_id = ? ORDER BY created DESC, rowid DESC LIMIT ?"
        ),
        params![item, limit],
        ActionRow::from_row,
    )
}

/// Scheduled actions whose time has come, oldest first.
pub fn due_actions(c: &Connection, now: i64) -> Result<Vec<ActionRow>> {
    all(
        c,
        &format!(
            "SELECT {ACTION_COLS} FROM yolo_actions WHERE outcome = 'scheduled' AND due_at <= ? ORDER BY due_at, rowid"
        ),
        [now],
        ActionRow::from_row,
    )
}

/// Settles an action (`decided` | `cancelled` | `failed`…); `reason` replaces the stored one when given.
/// Only a `scheduled` action changes, so an action is executed once. Returns whether it changed.
pub fn action_finish(
    tx: &Tx,
    id: &str,
    outcome: &str,
    reason: Option<&str>,
    now: i64,
) -> Result<bool> {
    let n = tx
        .prepare_cached(
            "UPDATE yolo_actions SET outcome = ?, reason = coalesce(?, reason), done_at = ?
              WHERE id = ? AND outcome = 'scheduled'",
        )?
        .execute(params![outcome, reason, now, id])?;
    if n == 1
        && let Some(a) = action_get(tx, id)?
    {
        tx.publish(ChangeEvent::YoloAction {
            id: a.id,
            decision_id: a.decision_id,
            ws: a.workspace_id,
        });
    }
    Ok(n == 1)
}

/// The user took back what YOLO decided (the decision was undone): stamps `undone` once.
pub fn action_undone(tx: &Tx, id: &str, now: i64) -> Result<bool> {
    let n = tx
        .prepare_cached("UPDATE yolo_actions SET undone = ? WHERE id = ? AND undone IS NULL")?
        .execute(params![now, id])?;
    if n == 1
        && let Some(a) = action_get(tx, id)?
    {
        tx.publish(ChangeEvent::YoloAction {
            id: a.id,
            decision_id: a.decision_id,
            ws: a.workspace_id,
        });
    }
    Ok(n == 1)
}

/// YOLO's decisions the user undid since `since` (breaker B3).
pub fn undone_since(c: &Connection, ws: &str, since: i64) -> Result<i64> {
    Ok(c.prepare_cached(
        "SELECT count(*) FROM yolo_actions WHERE workspace_id = ? AND undone IS NOT NULL AND undone >= ?",
    )?
    .query_row(params![ws, since], |r| r.get(0))?)
}

/// Moves a scheduled action to a later time (its judgement says wait, not no); false when it is no
/// longer scheduled.
pub fn action_postpone(tx: &Tx, id: &str, due_at: i64, reason: &str) -> Result<bool> {
    let n = tx
        .prepare_cached(
            "UPDATE yolo_actions SET due_at = ?, reason = ? WHERE id = ? AND outcome = 'scheduled'",
        )?
        .execute(params![due_at, reason, id])?;
    Ok(n == 1)
}

/// Which actions [`actions_where`] returns; `None` fields do not filter.
#[derive(Debug, Clone, Default)]
pub struct ActionFilter<'a> {
    pub ws: &'a str,
    pub window: Option<&'a str>,
    pub item: Option<&'a str>,
    pub outcome: Option<&'a str>,
    /// Rules, any of them.
    pub rules: &'a [&'a str],
    /// Created at or after (ms).
    pub since: Option<i64>,
    pub limit: i64,
}

/// Actions of a workspace, newest first.
pub fn actions_where(c: &Connection, f: &ActionFilter) -> Result<Vec<ActionRow>> {
    let mut sql = format!("SELECT {ACTION_COLS} FROM yolo_actions WHERE workspace_id = ?1");
    let mut args: Vec<rusqlite::types::Value> = vec![f.ws.to_string().into()];
    let mut add = |sql: &mut String, cond: &str, v: rusqlite::types::Value| {
        args.push(v);
        sql.push_str(&format!(" AND {cond} ?{}", args.len()));
    };
    if let Some(w) = f.window {
        add(&mut sql, "window_id =", w.to_string().into());
    }
    if let Some(i) = f.item {
        add(&mut sql, "item_id =", i.to_string().into());
    }
    if let Some(o) = f.outcome {
        add(&mut sql, "outcome =", o.to_string().into());
    }
    if let Some(s) = f.since {
        add(&mut sql, "created >=", s.into());
    }
    if !f.rules.is_empty() {
        let mut marks = Vec::new();
        for r in f.rules {
            args.push(r.to_string().into());
            marks.push(format!("?{}", args.len()));
        }
        sql.push_str(&format!(" AND rule IN ({})", marks.join(", ")));
    }
    args.push(if f.limit > 0 { f.limit } else { 1000 }.into());
    sql.push_str(&format!(
        " ORDER BY created DESC, rowid DESC LIMIT ?{}",
        args.len()
    ));
    all(
        c,
        &sql,
        rusqlite::params_from_iter(args),
        ActionRow::from_row,
    )
}

/// Removes a project or item row (back to “no setting”: a real-directory project counts as off
/// again), with its revision and `{type:"yolo", ws}`. False when there was none.
pub fn scope_clear(tx: &Tx, ws: &str, object_type: &str, id: &str, by: &str) -> Result<bool> {
    let Some(before) = scope_get(tx, ws, object_type, id)? else {
        return Ok(false);
    };
    tx.prepare_cached(
        "DELETE FROM yolo_scopes WHERE workspace_id = ? AND object_type = ? AND object_id = ?",
    )?
    .execute(params![ws, object_type, id])?;
    crate::base::revision(
        tx,
        ws,
        REVISION_TYPE,
        &format!("{object_type}:{id}"),
        before.revision + 1,
        &before.snapshot(),
        &Value::Null,
        by,
    )?;
    tx.publish(ChangeEvent::Yolo { ws: ws.into() });
    Ok(true)
}
