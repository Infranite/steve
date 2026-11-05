//! Keeping originals only as long as the user chose (job
//! `flow.retention`): after `keepRaw` days an event's payload is dropped, and so are the contexts and
//! step inputs/outputs of the runs that handled it (nodes, branches and timings stay); the event row
//! itself — summary and triage result — goes after a year.

use std::sync::Arc;
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, ToSql, params, params_from_iter};
use serde_json::{Value, json};
use steve_store::Tx;

use crate::{FlowCx, FlowResult};

/// The setting key (shared with channel messages).
pub const KEEP_RAW: &str = "keepRaw";
/// Allowed values in days; the first is the default.
pub const KEEP_RAW_DAYS: [i64; 3] = [7, 30, 90];
/// Event rows are deleted after this long.
pub const KEEP_EVENT_MS: i64 = 365 * 24 * 3_600_000;
/// What replaces purged run data.
pub const PURGED: &str = r#"{"purged":true}"#;
/// How often the job runs.
pub const EVERY: Duration = Duration::from_secs(3600);

/// The entry steve-core's `SETTINGS.data` gets (integrator adds it to the constant).
pub fn keep_raw_setting() -> Value {
    json!({
        "key": KEEP_RAW,
        "label": "事件与消息原文保留",
        "values": ["7", "30", "90"],
        "def": "7",
        "live": true,
        "labels": {"7": "7 天", "30": "30 天", "90": "90 天"},
    })
}

/// The workspace's `keepRaw` in days (a JSON string or number, or the bare text; anything else →
/// 7).
pub fn keep_raw_days(c: &Connection, ws: &str) -> FlowResult<i64> {
    let v: Option<String> = c
        .query_row(
            "SELECT value FROM settings WHERE workspace_id = ? AND key = ?",
            params![ws, KEEP_RAW],
            |r| r.get(0),
        )
        .optional()?;
    let days = v.and_then(|v| match serde_json::from_str::<Value>(&v) {
        Ok(Value::String(s)) => s.parse::<i64>().ok(),
        Ok(Value::Number(n)) => n.as_i64(),
        _ => v.trim().parse::<i64>().ok(),
    });
    Ok(days
        .filter(|d| KEEP_RAW_DAYS.contains(d))
        .unwrap_or(KEEP_RAW_DAYS[0]))
}

/// What one pass did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Swept {
    pub payloads: usize,
    pub runs: usize,
    pub events_deleted: usize,
}

/// Events purged per transaction; the writer is let go between batches.
pub const PURGE_BATCH: usize = 500;
/// Event rows deleted per transaction.
pub const DELETE_BATCH: usize = 1000;

/// One pass over all workspaces at `now`, in short transactions: per workspace, batches of
/// [`PURGE_BATCH`] events lose their payload together with the data of the runs that handled them,
/// then year-old rows go [`DELETE_BATCH`] at a time. Synchronous; [`run`] calls it off the runtime.
pub fn sweep(cx: &FlowCx, now: i64) -> FlowResult<Swept> {
    let wss: Vec<String> = cx.store().reader(|c| -> FlowResult<_> {
        let mut st = c.prepare("SELECT id FROM workspaces")?;
        let rows = st
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?;
        Ok(rows)
    })?;
    let mut out = Swept::default();
    for ws in wss {
        let cut = now - cx.store().reader(|c| keep_raw_days(c, &ws))? * 24 * 3_600_000;
        loop {
            let (runs, payloads) = cx
                .store()
                .transaction(|tx| purge_batch(tx, &ws, cut, now))?;
            out.runs += runs;
            out.payloads += payloads;
            if payloads < PURGE_BATCH {
                break;
            }
        }
    }
    loop {
        let n = cx.store().transaction(|tx| -> FlowResult<usize> {
            Ok(tx.execute(
                "DELETE FROM ext_events WHERE rowid IN (SELECT rowid FROM ext_events WHERE received < ? LIMIT ?)",
                params![now - KEEP_EVENT_MS, DELETE_BATCH as i64],
            )?)
        })?;
        out.events_deleted += n;
        if n < DELETE_BATCH {
            break;
        }
    }
    Ok(out)
}

/// Purges up to [`PURGE_BATCH`] of `ws`'s events received before `cut`: their payload, and the
/// contexts and step data of their runs, in the caller's transaction so the three stay consistent.
/// Returns `(runs, events)` purged.
fn purge_batch(tx: &Tx, ws: &str, cut: i64, now: i64) -> FlowResult<(usize, usize)> {
    let ids: Vec<String> = {
        let mut st = tx.prepare_cached(
            "SELECT id FROM ext_events WHERE workspace_id = ? AND received < ? AND payload_purged IS NULL LIMIT ?",
        )?;
        st.query_map(params![ws, cut, PURGE_BATCH as i64], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    if ids.is_empty() {
        return Ok((0, 0));
    }
    let marks = vec!["?"; ids.len()].join(", ");
    // the first parameter, then the ids
    let with = |first: &'static dyn ToSql| -> Vec<&dyn ToSql> {
        std::iter::once(first)
            .chain(ids.iter().map(|i| i as &dyn ToSql))
            .collect()
    };
    tx.execute(
        &format!(
            "UPDATE flow_run_steps SET input = ?1, output = ?1 WHERE run_id IN (
               SELECT id FROM flow_runs WHERE event_id IN ({marks}))"
        ),
        params_from_iter(with(&PURGED)),
    )?;
    let runs = tx.execute(
        &format!("UPDATE flow_runs SET context = ? WHERE event_id IN ({marks})"),
        params_from_iter(with(&PURGED)),
    )?;
    let mut with_now: Vec<&dyn ToSql> = vec![&now];
    with_now.extend(ids.iter().map(|i| i as &dyn ToSql));
    let events = tx.execute(
        &format!("UPDATE ext_events SET payload = NULL, payload_purged = ? WHERE id IN ({marks})"),
        params_from_iter(with_now),
    )?;
    Ok((runs, events))
}

/// The hourly job; each pass runs on the blocking pool.
pub async fn run(cx: Arc<FlowCx>) {
    loop {
        let now = cx.now();
        let pass = {
            let cx = cx.clone();
            cx.clone().blocking(move |_| sweep(&cx, now)).await
        };
        match pass {
            Ok(s) if s.payloads + s.events_deleted > 0 => tracing::info!(?s, "flow.retention"),
            Ok(_) => {}
            Err(e) => tracing::warn!("flow.retention: {e}"),
        }
        tokio::time::sleep(EVERY).await;
    }
}
