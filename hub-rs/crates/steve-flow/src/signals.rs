//! Signals = events + their triage result (`/signals`,
//! SSE): the list with its stats, one signal with its runs, the original, the outcome
//! rule over several runs, and the coalesced `signal` SSE event.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::{Value, json};
use steve_store::base::Viewer;
use steve_store::{
    Connections, Contacts, FlowRows, Read, RunRows, SignalFilter, SignalRow, SignalRows,
    SignalTriage, Store, StoreError, W,
};

use crate::runs;

/// `GET /signals` returns at most this many.
pub const MAX_LIST: i64 = 500;
/// `410` of `GET /signals/:id/raw` after the retention period (zh-CN; the answer is `flow.signal.raw_gone`).
pub const RAW_GONE: &str = "原文已过保留期限，只剩处理结果和摘要";
/// SSE `signal` events of one workspace within this window collapse into the last one.
pub const COALESCE: Duration = Duration::from_millis(500);

/// Outcomes from heaviest to lightest; `failed` only when nothing else came out.
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

/// Whether an event of `source` carries somebody else's text (a mail, a message, an issue, a webhook…)
/// rather than the user's own: everything but the clock, a manual run and an item of theirs.
pub fn external_source(source: &str) -> bool {
    !matches!(source, "time" | "manual" | "item")
}

/// Coalesces `signal` events: the first of a workspace starts a 500 ms window, later ones only
/// replace the id; at the end the last id is published. Without a tokio runtime it publishes at once.
#[derive(Default)]
pub struct SignalDebounce(Arc<Mutex<HashMap<String, String>>>);

impl SignalDebounce {
    pub fn signal(&self, store: &Store, ws: &str, id: &str) {
        let Ok(rt) = tokio::runtime::Handle::try_current() else {
            store.publish(steve_types::DomainEvent::from(
                steve_types::AutomationEvent::Signal {
                    id: id.into(),
                    ws: ws.into(),
                },
            ));
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
                    store.publish(steve_types::DomainEvent::from(
                        steve_types::AutomationEvent::Signal {
                            id: id.into(),
                            ws: ws.into(),
                        },
                    ));
                }
            });
        }
    }
}

/// Recomputes an event's triage result from its (non-dry) runs. Called after a run ends (the
/// executor, S2's engine after waits). Returns the event's workspace when it changed.
pub fn settle(w: &mut W<'_>, event_id: &str) -> steve_store::Result<Option<String>> {
    let Some((ws, old_outcome, old_label)) = SignalRows(w).current_outcome(event_id)? else {
        return Ok(None);
    };
    let runs = RunRows(w).triage_runs(&ws, event_id)?;
    if runs.is_empty() {
        return Ok(None);
    }
    let best = runs
        .iter()
        .filter_map(|run| {
            let outcome = run.outcome.as_deref()?;
            let rank = OUTCOME_ORDER.iter().position(|&key| key == outcome)?;
            Some((rank, outcome, run))
        })
        .min_by_key(|(rank, _, _)| *rank);
    let going = runs
        .iter()
        .any(|r| matches!(r.status.as_str(), "running" | "waiting"));
    let (outcome, run) = match best {
        Some((_, outcome, run)) => (outcome, Some(run)),
        None if going => ("pending", None),
        None => match runs.iter().rev().find(|run| run.status == "failed") {
            Some(run) => ("failed", Some(run)),
            None => ("recorded", runs.last()),
        },
    };
    let label = match (outcome, run) {
        ("pending", _) => String::new(),
        ("recorded", Some(r)) if r.label.is_empty() => crate::ingest::LABEL_RECORD.to_string(),
        (_, Some(r)) => r.label.clone(),
        _ => String::new(),
    };
    let text = |key: &str| {
        run.and_then(|run| run.context.get(key))
            .and_then(Value::as_str)
            .map(String::from)
    };
    let changed = outcome != old_outcome || label != old_label;
    SignalRows(w).settle(
        &ws,
        event_id,
        &SignalTriage {
            outcome: outcome.into(),
            label,
            reason: text("reason").unwrap_or_default(),
            handled_by: text("handled_by"),
            flow: run.map(|run| run.flow.clone()),
            run: run.map(|run| run.id.clone()),
            item: run.and_then(|run| run.item.clone()),
            decision: text("decision_id"),
        },
    )?;
    Ok(changed.then_some(ws))
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
        let zone = steve_runtime::tz::Zone::parse(tz)
            .unwrap_or_else(|_| steve_runtime::tz::Zone::system());
        let midnight = zone.day(now).map_or(now - crate::sources::DAY_MS, |d| d.0);
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

fn projections(
    c: &mut impl Read,
    ws: &str,
    rows: Vec<SignalRow>,
) -> steve_store::Result<Vec<Value>> {
    let connection_names =
        Connections(c).display_names(ws, rows.iter().filter_map(|r| r.connection.as_deref()))?;
    let names = Contacts::new(c).party_names(rows.iter().filter_map(|r| r.party.as_deref()))?;
    let labels = FlowRows(c).labels(ws, rows.iter().filter_map(|r| r.flow.as_deref()))?;
    Ok(rows.into_iter().map(|row| {
        let connection = row.connection.map(|id| json!({
            "id": id, "name": connection_names.get(&id).map_or("", String::as_str)
        }));
        let actor = row.party.map(|id| json!({
            "party_id": id, "name": names.get(&id).map_or("", String::as_str)
        }));
        let flow = row.flow.map(|id| {
            let (name, kind) = labels.get(&id).map_or(("", "flow"), |(name, kind, _)| (name.as_str(), kind.as_str()));
            json!({"id": id, "name": name, "kind": kind})
        });
        json!({
            "id": row.id, "source": row.source, "type": row.kind, "summary": row.summary,
            "group_key": row.group, "group_label": row.group, "thread_key": row.thread,
            "connection": connection, "actor": actor, "outcome": row.outcome, "outcome_label": row.label,
            "reason": row.reason, "handled_by": row.handled_by, "flow": flow,
            "work_item_id": row.item, "decision_id": row.decision, "raw_available": row.raw_available,
            "occurred": row.occurred, "received": row.received,
        })
    }).collect())
}

/// Counts ignore the outcome filter, so tabs share the same totals.
pub fn list(c: &mut impl Read, viewer: &Viewer, f: &Filter) -> steve_store::Result<Value> {
    let text =
        f.q.as_deref()
            .map(str::trim)
            .filter(|q| !q.is_empty())
            .map(str::to_lowercase);
    let codes = text.as_ref().map_or_else(Vec::new, |text| {
        [
            "mail", "cal", "im", "github", "webhook", "time", "item", "manual",
        ]
        .into_iter()
        .filter(|src| source_label(src).to_lowercase().contains(text) || src.contains(text))
        .collect()
    });
    let (counts, rows) = SignalRows(c).list(
        viewer,
        &SignalFilter {
            since: f.since,
            source: f.source.as_deref().filter(|s| !s.is_empty()),
            outcome: f.outcome.as_deref().filter(|s| !s.is_empty()),
            text: text.as_deref(),
            matching_sources: &codes,
        },
        MAX_LIST as u64,
    )?;
    let sum = |ks: &[&str]| -> i64 {
        counts
            .iter()
            .filter(|(k, _)| ks.contains(&k.as_str()))
            .map(|(_, n)| n)
            .sum()
    };
    let total: i64 = counts.iter().map(|(_, n)| n).sum();
    Ok(json!({
        "stats": {"total": total, "need": sum(&["need"]), "item": sum(&["item"]), "quiet": sum(&["ignored", "digest", "recorded"])},
        "items": projections(c, &viewer.ws, rows)?,
    }))
}

const NOT_FOUND: StoreError = StoreError::NotFound("没有这个信号");

/// One visible signal.
pub fn own(c: &mut impl Read, viewer: &Viewer, id: &str) -> steve_store::Result<SignalRow> {
    SignalRows(c).get(viewer, id)?.ok_or(NOT_FOUND)
}

fn run_projection(c: &mut impl Read, row: &runs::RunRow) -> steve_store::Result<Value> {
    let text = runs::display_text(row);
    let steps = RunRows(c).steps(&row.id)?;
    Ok(json!({
        "id": row.id, "flow_id": row.flow_id, "flow_version": row.flow_version,
        "status": row.status, "dry_run": row.dry_run, "event_id": row.event_id,
        "outcome": row.outcome, "outcome_label": text.outcome_label,
        "started": row.started, "ended": row.ended, "steps": steps,
    }))
}

/// `Run` with its steps.
pub fn run_view(c: &mut impl Read, id: &str) -> steve_store::Result<Option<Value>> {
    RunRows(c)
        .get(id)?
        .map(|r| run_projection(c, &r))
        .transpose()
}

/// A signal's summary and ordered runs.
pub fn detail(c: &mut impl Read, viewer: &Viewer, id: &str) -> steve_store::Result<Value> {
    let signal = own(c, viewer, id)?;
    let mut signal = projections(c, &viewer.ws, vec![signal])?.swap_remove(0);
    let rows = RunRows(c).by_event(&viewer.ws, id)?;
    signal["runs"] = Value::Array(
        rows.iter()
            .map(|r| run_projection(c, r))
            .collect::<steve_store::Result<_>>()?,
    );
    Ok(signal)
}

/// Raw payload. `None` means its retention period expired.
pub fn raw(c: &mut impl Read, viewer: &Viewer, id: &str) -> steve_store::Result<Option<Value>> {
    SignalRows(c).payload(viewer, id)?.ok_or(NOT_FOUND)
}

#[cfg(test)]
mod timezone_tests {
    use super::*;
    use steve_runtime::tz::Zone;

    #[test]
    fn signal_midnight_uses_runtime_even_when_midnight_is_missing() {
        for (name, text) in [
            ("America/New_York", "2026-03-08 02:15"),
            ("Europe/London", "2026-10-25 01:30"),
            ("America/Sao_Paulo", "2018-11-04 00:15"),
        ] {
            let z = Zone::parse(name).unwrap();
            let wall = chrono::NaiveDateTime::parse_from_str(text, "%Y-%m-%d %H:%M").unwrap();
            let now = z.instant(wall).unwrap();
            assert_eq!(Range::Today.since(now, name), z.day(now).unwrap().0);
        }
        let now = 1_800_000_000_000;
        for name in ["", "system", "invalid"] {
            assert_eq!(
                Range::Today.since(now, name),
                Zone::system().day(now).unwrap().0
            );
        }
    }
}
