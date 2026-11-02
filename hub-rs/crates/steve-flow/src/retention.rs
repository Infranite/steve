//! Keeping originals only as long as the user chose (signals-automation.md §2.4, job
//! `flow.retention`): after `keepRaw` days an event's payload is dropped, and so are the contexts and
//! step inputs/outputs of the runs that handled it (nodes, branches and timings stay); the event row
//! itself — summary and triage result — goes after a year.

use std::sync::Arc;
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};

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

/// One pass over all workspaces at `now`.
pub fn sweep(cx: &FlowCx, now: i64) -> FlowResult<Swept> {
    cx.store().transaction(|tx| -> FlowResult<Swept> {
        let wss: Vec<String> = {
            let mut st = tx.prepare("SELECT DISTINCT workspace_id FROM ext_events")?;
            st.query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?
        };
        let mut out = Swept::default();
        for ws in wss {
            let cut = now - keep_raw_days(tx, &ws)? * 24 * 3_600_000;
            tx.execute(
                "UPDATE flow_run_steps SET input = ?1, output = ?1 WHERE run_id IN (
                   SELECT r.id FROM flow_runs r JOIN ext_events e ON e.id = r.event_id
                    WHERE e.workspace_id = ?2 AND e.received < ?3 AND e.payload_purged IS NULL)",
                params![PURGED, ws, cut],
            )?;
            out.runs += tx.execute(
                "UPDATE flow_runs SET context = ?1 WHERE event_id IN (
                   SELECT id FROM ext_events WHERE workspace_id = ?2 AND received < ?3 AND payload_purged IS NULL)",
                params![PURGED, ws, cut],
            )?;
            out.payloads += tx.execute(
                "UPDATE ext_events SET payload = NULL, payload_purged = ?1 WHERE workspace_id = ?2 AND received < ?3 AND payload_purged IS NULL",
                params![now, ws, cut],
            )?;
        }
        out.events_deleted = tx.execute(
            "DELETE FROM ext_events WHERE received < ?",
            [now - KEEP_EVENT_MS],
        )?;
        Ok(out)
    })
}

/// The hourly job.
pub async fn run(cx: Arc<FlowCx>) {
    loop {
        match sweep(&cx, cx.now()) {
            Ok(s) if s.payloads + s.events_deleted > 0 => tracing::info!(?s, "flow.retention"),
            Ok(_) => {}
            Err(e) => tracing::warn!("flow.retention: {e}"),
        }
        tokio::time::sleep(EVERY).await;
    }
}
