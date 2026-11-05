//! Signals = events + their triage result (signals-automation.md §2.3, §4.1 `Signal`, §4.2
//! `/signals`, §7 SSE): the list with its stats, one signal with its runs, the original, the outcome
//! rule over several runs, and the coalesced `signal` SSE event.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};
use rusqlite::types::Value as Sql;
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde_json::{Value, json};
use steve_modules::ApiError;
use steve_store::base::Viewer;
use steve_store::{ChangeEvent, Store, Tx};

use crate::FlowResult;
use crate::cron::{DEFAULT_TZ, parse_tz};

/// `GET /signals` returns at most this many.
pub const MAX_LIST: i64 = 500;
/// `410` of `GET /signals/:id/raw` after the retention period.
pub const RAW_GONE: &str = "原文已过保留期限，只剩处理结果和摘要";
/// SSE `signal` events of one workspace within this window collapse into the last one.
pub const COALESCE: Duration = Duration::from_millis(500);

/// Outcomes from heaviest to lightest (§2.3); `failed` only when nothing else came out.
pub const OUTCOME_ORDER: [&str; 6] = ["need", "item", "linked", "digest", "ignored", "recorded"];

/// Chinese name of a source code, for `q` and item sources (“邮件”).
pub fn source_label(source: &str) -> &str {
    match source {
        "mail" => "邮件",
        "cal" => "日历",
        "im" => "消息",
        "github" => "GitHub",
        "webhook" => "Webhook",
        "time" => "定时",
        "item" => "事项",
        "manual" => "手动",
        s => s,
    }
}

/// Coalesces `signal` events: the first of a workspace starts a 500 ms window, later ones only
/// replace the id; at the end the last id is published. Without a tokio runtime it publishes at once.
#[derive(Default)]
pub struct SignalDebounce(Arc<Mutex<HashMap<String, String>>>);

impl SignalDebounce {
    pub fn signal(&self, store: &Store, ws: &str, id: &str) {
        let Ok(rt) = tokio::runtime::Handle::try_current() else {
            store.publish(ChangeEvent::Signal {
                id: id.into(),
                ws: ws.into(),
            });
            return;
        };
        let mut m = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let first = m.insert(ws.into(), id.into()).is_none();
        drop(m);
        if first {
            let pending = self.0.clone();
            let store = store.clone();
            let ws = ws.to_string();
            rt.spawn(async move {
                tokio::time::sleep(COALESCE).await;
                let last = pending
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .remove(&ws);
                if let Some(id) = last {
                    store.publish(ChangeEvent::Signal { id, ws });
                }
            });
        }
    }
}

/// Recomputes an event's triage result from its (non-dry) runs (§2.3). Called after a run ends (the
/// executor, S2's engine after waits). Returns the event's workspace when it changed.
pub fn settle(tx: &Tx, event_id: &str) -> FlowResult<Option<String>> {
    let ev = tx
        .query_row(
            "SELECT workspace_id, outcome, outcome_label FROM ext_events WHERE id = ?",
            [event_id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                ))
            },
        )
        .optional()?;
    let Some((ws, old_outcome, old_label)) = ev else {
        return Ok(None);
    };
    let mut st = tx.prepare_cached(
        "SELECT id, flow_id, status, outcome, outcome_label, work_item_id, context FROM flow_runs
          WHERE event_id = ? AND dry_run = 0 ORDER BY started, rowid",
    )?;
    let runs = st
        .query_map([event_id], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, Option<String>>(3)?,
                r.get::<_, String>(4)?,
                r.get::<_, Option<String>>(5)?,
                r.get::<_, String>(6)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if runs.is_empty() {
        return Ok(None);
    }
    let rank = |o: &Option<String>| {
        o.as_deref()
            .and_then(|o| OUTCOME_ORDER.iter().position(|x| *x == o))
    };
    let best = runs
        .iter()
        .filter(|r| rank(&r.3).is_some())
        .min_by_key(|r| rank(&r.3));
    let going = runs.iter().any(|r| r.2 == "running" || r.2 == "waiting");
    let (outcome, run) = match best {
        Some(r) => (r.3.clone().unwrap_or_default(), Some(r)),
        None if going => ("pending".to_string(), None),
        None => match runs.iter().rev().find(|r| r.2 == "failed") {
            Some(r) => ("failed".to_string(), Some(r)),
            None => ("recorded".to_string(), runs.last()),
        },
    };
    let ctx: Value = run
        .and_then(|r| serde_json::from_str(&r.6).ok())
        .unwrap_or(Value::Null);
    let label = match (outcome.as_str(), run) {
        ("pending", _) => String::new(),
        ("recorded", Some(r)) if r.4.is_empty() => crate::ingest::LABEL_RECORD.to_string(),
        (_, Some(r)) => r.4.clone(),
        _ => String::new(),
    };
    let s = |k: &str| ctx.get(k).and_then(Value::as_str).map(String::from);
    tx.execute(
        "UPDATE ext_events SET outcome = ?, outcome_label = ?, reason = ?, handled_by = ?, flow_id = ?, run_id = ?, work_item_id = ?, decision_id = ? WHERE id = ?",
        params![
            outcome,
            label,
            s("reason").unwrap_or_default(),
            s("handled_by"),
            run.map(|r| r.1.clone()),
            run.map(|r| r.0.clone()),
            run.and_then(|r| r.5.clone()),
            s("decision_id"),
            event_id
        ],
    )?;
    Ok((outcome != old_outcome || label != old_label).then_some(ws))
}

/// `range` of `GET /signals`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Range {
    /// Since local midnight (Hub time zone).
    Today,
    SevenDays,
}

impl Range {
    pub fn parse(s: Option<&str>) -> Option<Self> {
        match s.unwrap_or("today") {
            "today" => Some(Self::Today),
            "7d" => Some(Self::SevenDays),
            _ => None,
        }
    }

    /// The first `received` included.
    pub fn since(self, now: i64, tz: &str) -> i64 {
        let tz = parse_tz(tz)
            .or_else(|_| parse_tz(DEFAULT_TZ))
            .unwrap_or(chrono_tz::UTC);
        let local = DateTime::<Utc>::from_timestamp_millis(now)
            .unwrap_or_default()
            .with_timezone(&tz);
        let midnight = local
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .and_then(|d| tz.from_local_datetime(&d).earliest())
            .map_or(now - crate::sources::DAY_MS, |d| d.timestamp_millis());
        match self {
            Self::Today => midnight,
            Self::SevenDays => midnight - 6 * crate::sources::DAY_MS,
        }
    }
}

/// Filters of `GET /signals`.
#[derive(Debug, Clone, Default)]
pub struct Filter {
    pub since: i64,
    pub q: Option<String>,
    pub source: Option<String>,
    pub outcome: Option<String>,
}

/// The visibility condition on `e` (events) joined with `c` (their connection): `private` events
/// belong to the connection's creator; `space:`/`team:` to members of those scopes.
fn visible(viewer: &Viewer, params: &mut Vec<Sql>) -> String {
    params.push(Sql::Text(viewer.user.clone()));
    let mut sql = "(e.visibility = 'workspace' OR coalesce(c.created_by, '') = ?".to_string();
    if !viewer.scopes.is_empty() {
        sql.push_str(&format!(
            " OR e.visibility IN ({})",
            vec!["?"; viewer.scopes.len()].join(", ")
        ));
        params.extend(viewer.scopes.iter().cloned().map(Sql::Text));
    }
    sql.push(')');
    sql
}

/// Every `ext_events` column but `payload` (the raw body is only read by [`raw`]).
const SELECT: &str = "SELECT e.id, e.workspace_id, e.connection_id, e.source, e.type, e.external_id, e.dedupe_key,
    e.thread_key, e.group_key, e.scope_ext_id, e.space_id, e.message_id, e.actor_account_id, e.actor_party_id,
    e.occurred, e.received, e.summary, e.payload_purged, e.visibility, e.outcome, e.outcome_label, e.reason,
    e.handled_by, e.flow_id, e.run_id, e.work_item_id, e.decision_id, (e.payload IS NOT NULL) AS raw_available,
    coalesce(nullif(a.handle_display, ''), a.handle) AS conn_name, f.name AS flow_name, f.kind AS flow_kind, p.name AS actor_name
  FROM ext_events e LEFT JOIN connections c ON c.id = e.connection_id LEFT JOIN accounts a ON a.id = c.account_id
  LEFT JOIN flows f ON f.id = e.flow_id LEFT JOIN parties p ON p.id = e.actor_party_id";

/// `Signal` of §4.1.
fn signal(r: &Row) -> rusqlite::Result<Value> {
    let conn: Option<String> = r.get("connection_id")?;
    let flow: Option<String> = r.get("flow_id")?;
    let party: Option<String> = r.get("actor_party_id")?;
    let group: Option<String> = r.get("group_key")?;
    Ok(json!({
        "id": r.get::<_, String>("id")?,
        "source": r.get::<_, String>("source")?,
        "type": r.get::<_, String>("type")?,
        "summary": r.get::<_, String>("summary")?,
        "group_key": group,
        "group_label": group,
        "thread_key": r.get::<_, Option<String>>("thread_key")?,
        "connection": conn.map(|id| -> rusqlite::Result<Value> {
            Ok(json!({"id": id, "name": r.get::<_, Option<String>>("conn_name")?.unwrap_or_default()}))
        }).transpose()?,
        "actor": party.map(|id| -> rusqlite::Result<Value> {
            Ok(json!({"party_id": id, "name": r.get::<_, Option<String>>("actor_name")?.unwrap_or_default()}))
        }).transpose()?,
        "outcome": r.get::<_, String>("outcome")?,
        "outcome_label": r.get::<_, String>("outcome_label")?,
        "reason": r.get::<_, String>("reason")?,
        "handled_by": r.get::<_, Option<String>>("handled_by")?,
        "flow": flow.map(|id| -> rusqlite::Result<Value> {
            Ok(json!({
                "id": id,
                "name": r.get::<_, Option<String>>("flow_name")?.unwrap_or_default(),
                "kind": r.get::<_, Option<String>>("flow_kind")?.unwrap_or_else(|| "flow".into()),
            }))
        }).transpose()?,
        "work_item_id": r.get::<_, Option<String>>("work_item_id")?,
        "decision_id": r.get::<_, Option<String>>("decision_id")?,
        "raw_available": r.get::<_, bool>("raw_available")?,
        "occurred": r.get::<_, i64>("occurred")?,
        "received": r.get::<_, i64>("received")?,
    }))
}

/// `GET /signals`: `{stats:{total, need, item, quiet}, items}` (newest first, ≤500). `quiet` =
/// ignored, digested or only recorded (the prototype's “忽略或进汇报”). Stats ignore the `outcome`
/// filter, so the counters stay put while you switch tabs.
pub fn list(c: &Connection, viewer: &Viewer, f: &Filter) -> FlowResult<Value> {
    let mut params = vec![Sql::Text(viewer.ws.clone()), Sql::Integer(f.since)];
    let mut cond = format!(
        "e.workspace_id = ? AND e.received >= ? AND {}",
        visible(viewer, &mut params)
    );
    if let Some(s) = f.source.as_deref().filter(|s| !s.is_empty()) {
        cond.push_str(" AND e.source = ?");
        params.push(Sql::Text(s.into()));
    }
    if let Some(q) = f.q.as_deref().map(str::trim).filter(|q| !q.is_empty()) {
        let q = q.to_lowercase();
        // sources whose Chinese name matches
        let codes: Vec<&str> = [
            "mail", "cal", "im", "github", "webhook", "time", "item", "manual",
        ]
        .into_iter()
        .filter(|s| source_label(s).to_lowercase().contains(&q) || s.contains(&q))
        .collect();
        cond.push_str(
            " AND (instr(lower(e.summary), ?) > 0 OR instr(lower(coalesce(e.group_key, '')), ?) > 0 OR instr(lower(e.outcome_label), ?) > 0",
        );
        params.extend([Sql::Text(q.clone()), Sql::Text(q.clone()), Sql::Text(q)]);
        if !codes.is_empty() {
            cond.push_str(&format!(
                " OR e.source IN ({})",
                vec!["?"; codes.len()].join(", ")
            ));
            params.extend(codes.into_iter().map(|s| Sql::Text(s.into())));
        }
        cond.push(')');
    }
    let counts: Vec<(String, i64)> = {
        let sql = format!(
            "SELECT e.outcome, COUNT(*) FROM ext_events e LEFT JOIN connections c ON c.id = e.connection_id WHERE {cond} GROUP BY e.outcome"
        );
        let mut st = c.prepare(&sql)?;
        st.query_map(rusqlite::params_from_iter(params.iter()), |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?
        .collect::<rusqlite::Result<_>>()?
    };
    let n = |ks: &[&str]| -> i64 {
        counts
            .iter()
            .filter(|(k, _)| ks.contains(&k.as_str()))
            .map(|(_, n)| n)
            .sum()
    };
    let total: i64 = counts.iter().map(|(_, n)| n).sum();
    if let Some(o) = f.outcome.as_deref().filter(|o| !o.is_empty()) {
        cond.push_str(" AND e.outcome = ?");
        params.push(Sql::Text(o.into()));
    }
    params.push(Sql::Integer(MAX_LIST));
    let sql = format!("{SELECT} WHERE {cond} ORDER BY e.received DESC, e.rowid DESC LIMIT ?");
    let mut st = c.prepare(&sql)?;
    let items = st
        .query_map(rusqlite::params_from_iter(params.iter()), signal)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(json!({
        "stats": {"total": total, "need": n(&["need"]), "item": n(&["item"]), "quiet": n(&["ignored", "digest", "recorded"])},
        "items": items,
    }))
}

/// One visible signal, or `404 没有这个信号`.
pub fn own(c: &Connection, viewer: &Viewer, id: &str) -> FlowResult<Value> {
    let mut params = vec![Sql::Text(id.into()), Sql::Text(viewer.ws.clone())];
    let cond = visible(viewer, &mut params);
    let sql = format!("{SELECT} WHERE e.id = ? AND e.workspace_id = ? AND {cond}");
    c.query_row(&sql, rusqlite::params_from_iter(params.iter()), signal)
        .optional()?
        .ok_or_else(|| ApiError::not_found("信号"))
}

fn parse_json(s: Option<String>) -> Value {
    s.and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Null)
}

/// `Run` of §4.1 with its steps.
pub fn run_view(c: &Connection, id: &str) -> FlowResult<Option<Value>> {
    let run = c
        .query_row(
            "SELECT id, flow_id, flow_version, status, dry_run, event_id, outcome, outcome_label, started, ended FROM flow_runs WHERE id = ?",
            [id],
            |r| {
                Ok(json!({
                    "id": r.get::<_, String>(0)?,
                    "flow_id": r.get::<_, String>(1)?,
                    "flow_version": r.get::<_, i64>(2)?,
                    "status": r.get::<_, String>(3)?,
                    "dry_run": r.get::<_, bool>(4)?,
                    "event_id": r.get::<_, Option<String>>(5)?,
                    "outcome": r.get::<_, Option<String>>(6)?,
                    "outcome_label": r.get::<_, String>(7)?,
                    "started": r.get::<_, i64>(8)?,
                    "ended": r.get::<_, Option<i64>>(9)?,
                }))
            },
        )
        .optional()?;
    let Some(mut run) = run else {
        return Ok(None);
    };
    let mut st = c.prepare_cached(
        "SELECT seq, node_id, status, branch, input, output, ms, error FROM flow_run_steps WHERE run_id = ? ORDER BY seq",
    )?;
    let steps = st
        .query_map([id], |r| {
            Ok(json!({
                "seq": r.get::<_, i64>(0)?,
                "node_id": r.get::<_, String>(1)?,
                "status": r.get::<_, String>(2)?,
                "branch": r.get::<_, Option<String>>(3)?,
                "input": parse_json(r.get(4)?),
                "output": parse_json(r.get(5)?),
                "ms": r.get::<_, f64>(6)?,
                "error": r.get::<_, Option<String>>(7)?,
            }))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    run["steps"] = Value::Array(steps);
    Ok(Some(run))
}

/// `GET /signals/:id`: `Signal` + `runs`.
pub fn detail(c: &Connection, viewer: &Viewer, id: &str) -> FlowResult<Value> {
    let mut s = own(c, viewer, id)?;
    let mut st = c.prepare_cached(
        "SELECT id FROM flow_runs WHERE event_id = ? AND dry_run = 0 ORDER BY started, rowid",
    )?;
    let ids = st
        .query_map([id], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let runs = ids
        .iter()
        .map(|r| run_view(c, r))
        .collect::<FlowResult<Vec<_>>>()?;
    s["runs"] = Value::Array(runs.into_iter().flatten().collect());
    Ok(s)
}

/// `GET /signals/:id/raw`: `Ok(Some(payload))`, or `Ok(None)` when the retention period removed it
/// (`410`).
pub fn raw(c: &Connection, viewer: &Viewer, id: &str) -> FlowResult<Option<Value>> {
    own(c, viewer, id)?;
    let p: Option<String> =
        c.query_row("SELECT payload FROM ext_events WHERE id = ?", [id], |r| {
            r.get(0)
        })?;
    Ok(p.map(|p| serde_json::from_str(&p).unwrap_or(Value::String(p))))
}
