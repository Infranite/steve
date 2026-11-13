//! The event explorer (`GET /stats/events`): every agent turn, decision card and item mark of a workspace
//! in a span, as a histogram over the whole span and a list of the newest events in a window of it.
//!
//! The web client draws the histogram, lets the user drag a window over it (the list follows the window)
//! and zooms in by asking again with the window as the span. Events come from the recorded tables
//! (`agent_turns`, `decisions`, `item_marks`), so the counts agree with the stats page and the item drill-down.

use rusqlite::{Connection, params};
use serde::Serialize;
use serde_json::Value;
use steve_store::Result;

/// `kind` of `GET /stats/events`.
pub const KINDS: &[&str] = &["turn", "decision", "mark"];
/// Most events in a list.
pub const MAX_EVENTS: usize = 200;
/// Fewest and most histogram buckets.
pub const BUCKETS: (usize, usize) = (12, 240);
/// Most events read for one answer: a span with more is counted from what was read (`capped`).
const READ_MAX: i64 = 50_000;

/// What narrows the events: the workspace, and optionally a project, a Bot, a kind and an item.
#[derive(Debug, Clone, Default)]
pub struct Scope {
    pub workspace_id: String,
    pub project: Option<String>,
    pub bot: Option<String>,
    pub kind: Option<String>,
    pub item: Option<String>,
}

/// One event of the list.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Event {
    pub ts: i64,
    /// `turn`, `decision` or `mark`.
    pub kind: &'static str,
    /// A turn's stop reason (`running` while it runs), a decision's kind, a mark's kind (`status`, `deliver`, `release`, `fail`).
    pub what: String,
    /// A decision's status, a mark's new value (`done`, `released`, `stall`…).
    pub to: Option<String>,
    /// Went wrong: a turn that failed, a failure card or mark, a release that did not go out.
    pub bad: bool,
    /// Still going: a running turn, an open decision.
    pub open: bool,
    pub item_id: Option<String>,
    pub no: Option<i64>,
    pub title: Option<String>,
    pub session_id: Option<String>,
    /// A turn's or a resolved decision's duration.
    pub dur_ms: Option<i64>,
    pub tools: Option<i64>,
    pub tool_errors: Option<i64>,
    /// A turn's tokens (input, output and cache), when the turn reported them.
    pub tokens: Option<i64>,
    /// Who acted: `yolo` or `me` for a resolved decision, a mark's actor.
    pub by: Option<String>,
    /// A turn's id, a decision's id, a mark's id: unique within its kind.
    pub id: String,
}

/// Events per bucket, `start + i * step` to the next, per kind.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Histogram {
    pub start: i64,
    pub step: i64,
    pub turn: Vec<i64>,
    pub decision: Vec<i64>,
    pub mark: Vec<i64>,
    /// Of all three, the ones that went wrong.
    pub bad: Vec<i64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Counts {
    pub turn: usize,
    pub decision: usize,
    pub mark: usize,
    pub bad: usize,
}

/// `GET /stats/events`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Events {
    /// The span the histogram covers.
    pub lo: i64,
    pub hi: i64,
    /// The window the list and the counts cover (the span when none was asked).
    pub a: i64,
    pub b: i64,
    pub histogram: Histogram,
    pub counts: Counts,
    /// The newest events of the window, at most [`MAX_EVENTS`].
    pub rows: Vec<Event>,
    /// Events in the window (the list may show fewer).
    pub total: usize,
    /// The span held more events than one answer reads: the figures cover the newest [`READ_MAX`] of each kind.
    pub capped: bool,
}

fn tokens_of(c: &Connection, session: &str, turn: &str) -> Result<Option<i64>> {
    Ok(c.prepare_cached(
        "SELECT SUM(COALESCE(input, 0) + COALESCE(output, 0) + COALESCE(cache_read, 0) + COALESCE(cache_write, 0))
         FROM usage_records WHERE session_id = ? AND turn_id = ?",
    )?
    .query_row([session, turn], |r| r.get(0))?)
}

fn turns(c: &Connection, s: &Scope, lo: i64, hi: i64, now: i64) -> Result<Vec<Event>> {
    Ok(c.prepare_cached(
        "SELECT t.id, t.started, t.ended, t.stop_reason, t.transient, t.tools, t.tool_errors, t.item_id, i.no, i.title, t.session_id
         FROM agent_turns t LEFT JOIN items i ON i.id = t.item_id
         WHERE t.workspace_id = ?1 AND t.started >= ?2 AND t.started < ?3
           AND (?4 IS NULL OR t.project_id = ?4) AND (?5 IS NULL OR t.bot_id = ?5) AND (?6 IS NULL OR t.item_id = ?6)
         ORDER BY t.started DESC LIMIT ?7",
    )?
    .query_map(params![s.workspace_id, lo, hi, s.project, s.bot, s.item, READ_MAX], |r| {
        let started: i64 = r.get(1)?;
        let ended: Option<i64> = r.get(2)?;
        let stop: Option<String> = r.get(3)?;
        let transient: bool = r.get(4)?;
        Ok(Event {
            id: r.get(0)?,
            ts: started,
            kind: "turn",
            bad: transient || stop.as_deref() == Some("error"),
            open: ended.is_none(),
            what: stop.unwrap_or_else(|| if ended.is_none() { "running" } else { "" }.into()),
            to: None,
            dur_ms: Some(ended.unwrap_or(now).max(started) - started),
            tools: r.get(5)?,
            tool_errors: r.get(6)?,
            item_id: r.get(7)?,
            no: r.get(8)?,
            title: r.get(9)?,
            session_id: r.get(10)?,
            tokens: None,
            by: None,
        })
    })?
    .collect::<rusqlite::Result<_>>()?)
}

fn decisions(c: &Connection, s: &Scope, lo: i64, hi: i64) -> Result<Vec<Event>> {
    Ok(c.prepare_cached(
        "SELECT d.id, d.created, d.resolved, d.kind, d.status, d.result, d.item_id, i.no, i.title, d.session_id
         FROM decisions d LEFT JOIN items i ON i.id = d.item_id
         WHERE d.workspace_id = ?1 AND d.created >= ?2 AND d.created < ?3
           AND (?4 IS NULL OR i.project_id = ?4) AND (?5 IS NULL OR i.bot_id = ?5) AND (?6 IS NULL OR d.item_id = ?6)
         ORDER BY d.created DESC LIMIT ?7",
    )?
    .query_map(params![s.workspace_id, lo, hi, s.project, s.bot, s.item, READ_MAX], |r| {
        let created: i64 = r.get(1)?;
        let resolved: Option<i64> = r.get(2)?;
        let kind: String = r.get(3)?;
        let status: String = r.get(4)?;
        let result: Value = r
            .get::<_, Option<String>>(5)?
            .and_then(|x| serde_json::from_str(&x).ok())
            .unwrap_or(Value::Null);
        let yolo = result.get("decided_by").and_then(Value::as_str) == Some("yolo");
        Ok(Event {
            id: r.get(0)?,
            ts: created,
            kind: "decision",
            bad: kind == "fail",
            open: status == "open",
            by: (status == "resolved").then(|| if yolo { "yolo" } else { "me" }.to_string()),
            what: kind,
            to: Some(status),
            dur_ms: resolved.map(|x| (x - created).max(0)),
            tools: None,
            tool_errors: None,
            tokens: None,
            item_id: r.get(6)?,
            no: r.get(7)?,
            title: r.get(8)?,
            session_id: r.get(9)?,
        })
    })?
    .collect::<rusqlite::Result<_>>()?)
}

fn marks(c: &Connection, s: &Scope, lo: i64, hi: i64) -> Result<Vec<Event>> {
    Ok(c.prepare_cached(
        "SELECT m.id, m.ts, m.kind, m.to_v, m.actor, m.item_id, i.no, i.title
         FROM item_marks m JOIN items i ON i.id = m.item_id
         WHERE m.workspace_id = ?1 AND m.ts >= ?2 AND m.ts < ?3
           AND (?4 IS NULL OR i.project_id = ?4) AND (?5 IS NULL OR i.bot_id = ?5) AND (?6 IS NULL OR m.item_id = ?6)
         ORDER BY m.ts DESC LIMIT ?7",
    )?
    .query_map(params![s.workspace_id, lo, hi, s.project, s.bot, s.item, READ_MAX], |r| {
        let kind: String = r.get(2)?;
        let to: Option<String> = r.get(3)?;
        let actor: String = r.get(4)?;
        Ok(Event {
            id: r.get::<_, i64>(0)?.to_string(),
            ts: r.get(1)?,
            kind: "mark",
            bad: kind == "fail" || (kind == "release" && to.as_deref() != Some("released")),
            open: false,
            what: kind,
            to,
            by: (!actor.is_empty()).then_some(actor),
            dur_ms: None,
            tools: None,
            tool_errors: None,
            tokens: None,
            item_id: r.get(5)?,
            no: r.get(6)?,
            title: r.get(7)?,
            session_id: None,
        })
    })?
    .collect::<rusqlite::Result<_>>()?)
}

/// The events of `scope` in `[lo, hi)`: a histogram of `buckets` over the span, and the newest events in
/// `[a, b)` (clamped to the span). `None` when the span is empty.
pub fn events(
    c: &Connection,
    scope: &Scope,
    lo: i64,
    hi: i64,
    window: Option<(i64, i64)>,
    buckets: usize,
    now: i64,
) -> Result<Option<Events>> {
    if hi <= lo {
        return Ok(None);
    }
    let (a, b) = window.map_or((lo, hi), |(a, b)| (a.clamp(lo, hi), b.clamp(lo, hi)));
    let (a, b) = if b > a { (a, b) } else { (lo, hi) };
    let want = |k: &str| scope.kind.as_deref().is_none_or(|x| x == k);
    let mut all = Vec::new();
    if want("turn") {
        all.push(turns(c, scope, lo, hi, now)?);
    }
    if want("decision") {
        all.push(decisions(c, scope, lo, hi)?);
    }
    if want("mark") {
        all.push(marks(c, scope, lo, hi)?);
    }
    let capped = all.iter().any(|x| x.len() as i64 >= READ_MAX);
    let all: Vec<Event> = all.into_iter().flatten().collect();
    let n = buckets.clamp(BUCKETS.0, BUCKETS.1);
    let step = ((hi - lo) as f64 / n as f64).ceil().max(1.0) as i64;
    let mut h = Histogram {
        start: lo,
        step,
        turn: vec![0; n],
        decision: vec![0; n],
        mark: vec![0; n],
        bad: vec![0; n],
    };
    let mut counts = Counts::default();
    for e in &all {
        let i = (((e.ts - lo) / step) as usize).min(n - 1);
        match e.kind {
            "turn" => h.turn[i] += 1,
            "decision" => h.decision[i] += 1,
            _ => h.mark[i] += 1,
        }
        if e.bad {
            h.bad[i] += 1;
        }
        if e.ts >= a && e.ts < b {
            match e.kind {
                "turn" => counts.turn += 1,
                "decision" => counts.decision += 1,
                _ => counts.mark += 1,
            }
            counts.bad += e.bad as usize;
        }
    }
    let mut rows: Vec<Event> = all.into_iter().filter(|e| e.ts >= a && e.ts < b).collect();
    let total = rows.len();
    rows.sort_by(|x, y| {
        y.ts.cmp(&x.ts)
            .then(y.kind.cmp(x.kind))
            .then(y.id.cmp(&x.id))
    });
    rows.truncate(MAX_EVENTS);
    for e in rows.iter_mut().filter(|e| e.kind == "turn") {
        if let Some(sid) = e.session_id.clone() {
            e.tokens = tokens_of(c, &sid, &e.id)?.filter(|t| *t > 0);
        }
    }
    Ok(Some(Events {
        lo,
        hi,
        a,
        b,
        histogram: h,
        counts,
        rows,
        total,
        capped,
    }))
}
