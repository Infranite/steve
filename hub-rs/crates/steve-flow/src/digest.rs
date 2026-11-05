//! `digest_entries`: things flows noted for the next report. The `digest`
//! action writes them; `report` schedules and steve-core's away-report take them.

use rusqlite::{Connection, params};
use serde::Serialize;
use steve_store::{Tx, uid};

use crate::{FlowCx, FlowResult};

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DigestEntry {
    pub id: String,
    pub event_id: Option<String>,
    pub run_id: Option<String>,
    pub text: String,
    pub created: i64,
}

/// The `digest` action: notes `text` for the next report.
pub fn add(
    tx: &Tx,
    ws: &str,
    text: &str,
    event_id: Option<&str>,
    run_id: Option<&str>,
    now: i64,
) -> FlowResult<String> {
    let id = uid("dig");
    tx.execute(
        "INSERT INTO digest_entries (id, workspace_id, event_id, run_id, text, created) VALUES (?, ?, ?, ?, ?, ?)",
        params![id, ws, event_id, run_id, text, now],
    )?;
    Ok(id)
}

/// Entries not taken yet, created at or after `since`, oldest first. Does not take them.
pub fn pending(c: &Connection, ws: &str, since: i64) -> FlowResult<Vec<DigestEntry>> {
    let mut st = c.prepare_cached(
        "SELECT id, event_id, run_id, text, created FROM digest_entries WHERE workspace_id = ? AND taken IS NULL AND created >= ? ORDER BY created, rowid",
    )?;
    let rows = st
        .query_map(params![ws, since], |r| {
            Ok(DigestEntry {
                id: r.get(0)?,
                event_id: r.get(1)?,
                run_id: r.get(2)?,
                text: r.get(3)?,
                created: r.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Marks entries taken (only those still open). Returns how many.
pub fn mark_taken(tx: &Tx, ids: &[String], now: i64) -> FlowResult<usize> {
    let mut n = 0;
    for id in ids {
        n += tx.execute(
            "UPDATE digest_entries SET taken = ? WHERE id = ? AND taken IS NULL",
            params![now, id],
        )?;
    }
    Ok(n)
}

/// `digest::take(ws, since)`: the pending entries, marked taken in the same transaction.
pub fn take(tx: &Tx, ws: &str, since: i64, now: i64) -> FlowResult<Vec<DigestEntry>> {
    let out = pending(tx, ws, since)?;
    let ids: Vec<String> = out.iter().map(|e| e.id.clone()).collect();
    mark_taken(tx, &ids, now)?;
    Ok(out)
}

/// `steve_flow::digest::take(ws, since)` for steve-core (the away-report, `/汇报`): [`take`] in its
/// own transaction at the module's clock.
pub fn take_now(cx: &FlowCx, ws: &str, since: i64) -> FlowResult<Vec<DigestEntry>> {
    let now = cx.now();
    cx.store().transaction(|tx| take(tx, ws, since, now))
}
