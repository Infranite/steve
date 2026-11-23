//! Durable observation summaries scoped to the currently active execution.

use rusqlite::{OptionalExtension, params};
use serde::{Serialize, de::DeserializeOwned};

use crate::{Result, StoreError, W};

pub struct Active {
    pub turn: String,
    pub session: String,
    pub attempt: String,
    pub operation: String,
    pub revision: i64,
    pub waiting: bool,
}

const ACTIVE: &str = "
 SELECT t.turn_id,t.session_id,a.attempt_id,a.operation_id,a.revision,
        EXISTS(SELECT 1 FROM runtime_permissions p
             WHERE p.attempt_id=a.attempt_id AND p.state IN ('waiting','answering'))
        OR EXISTS(SELECT 1 FROM node_streams ns
             WHERE ns.node_id=a.node_id AND ns.session_id=t.session_id AND ns.fault IS NOT NULL)
        OR EXISTS(SELECT 1 FROM decisions d
             WHERE d.session_id=t.session_id AND d.status='open')
 FROM runtime_turns t
 JOIN runtime_attempts a ON a.attempt_id=t.active_attempt AND a.turn_id=t.turn_id
 JOIN runtime_sessions s ON s.session_id=t.session_id AND s.active_attempt=a.attempt_id
 WHERE t.state='running' AND a.state='running' AND a.cancel_requested=0 AND a.node_id=?1";

fn active(row: &rusqlite::Row<'_>) -> rusqlite::Result<Active> {
    Ok(Active {
        turn: row.get(0)?,
        session: row.get(1)?,
        attempt: row.get(2)?,
        operation: row.get(3)?,
        revision: row.get(4)?,
        waiting: row.get(5)?,
    })
}

pub fn for_attempt(w: &mut W<'_>, node: &str, attempt: &str) -> Result<Option<Active>> {
    Ok(w.conn
        .query_row(
            &format!("{ACTIVE} AND a.attempt_id=?2"),
            params![node, attempt],
            active,
        )
        .optional()?)
}

pub fn running(w: &mut W<'_>, node: &str) -> Result<Vec<Active>> {
    let mut q = w.conn.prepare(ACTIVE)?;
    Ok(q.query_map([node], active)?
        .collect::<rusqlite::Result<_>>()?)
}

pub fn read<T: DeserializeOwned>(w: &mut W<'_>, turn: &str) -> Result<Option<T>> {
    w.conn
        .query_row(
            "SELECT summary FROM runtime_turn_progress WHERE turn_id=?",
            [turn],
            |row| row.get::<_, String>(0),
        )
        .optional()?
        .map(|raw| serde_json::from_str(&raw).map_err(StoreError::from))
        .transpose()
}

pub fn save(w: &mut W<'_>, turn: &str, summary: &impl Serialize) -> Result<()> {
    w.conn.execute(
        "INSERT INTO runtime_turn_progress(turn_id,summary) VALUES (?,?)
          ON CONFLICT(turn_id) DO UPDATE SET summary=excluded.summary",
        params![turn, serde_json::to_string(summary)?],
    )?;
    Ok(())
}

pub fn bind_decision(w: &mut W<'_>, turn: &str, decision: &str) -> Result<()> {
    w.conn.execute(
        "UPDATE runtime_turn_progress SET decision_id=? WHERE turn_id=? AND decision_id IS NULL",
        params![decision, turn],
    )?;
    Ok(())
}

pub fn held(w: &mut W<'_>, session: &str) -> Result<bool> {
    Ok(w.conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM runtime_turn_progress p
          JOIN runtime_turns t ON t.turn_id=p.turn_id
          JOIN decisions d ON d.id=p.decision_id
          WHERE t.session_id=? AND d.status='open')",
        [session],
        |r| r.get(0),
    )?)
}
