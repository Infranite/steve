//! The event explorer (`GET /stats/events`): every agent turn, decision card and item mark of a workspace
//! in a span, as a histogram over the whole span and a list of the newest events in a window of it.
//!
//! The web client draws the histogram, lets the user drag a window over it (the list follows the window)
//! and zooms in by asking again with the window as the span. Events come from the recorded tables
//! (`agent_turns`, `decisions`, `item_marks`), so the counts agree with the stats page and the item drill-down.

use serde::Serialize;
use steve_store::statistics::{DecisionReads, MarkReads, ReportScope, TurnReads, UsageReads};
use steve_store::{Read, Result};

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
    /// Who acted: `steve` or `me` for a resolved decision, a mark's actor.
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

fn turns(
    c: &mut impl Read,
    scope: ReportScope<'_>,
    item: Option<&str>,
    now: i64,
) -> Result<Vec<Event>> {
    let rows = TurnReads(c).events(scope, item, READ_MAX as u64)?;
    Ok(rows
        .into_iter()
        .map(|r| Event {
            id: r.id,
            ts: r.started,
            kind: "turn",
            bad: r.transient || r.reason.as_deref() == Some("error"),
            open: r.ended.is_none(),
            what: r
                .reason
                .unwrap_or_else(|| if r.ended.is_none() { "running" } else { "" }.into()),
            to: None,
            dur_ms: Some(r.ended.unwrap_or(now).max(r.started) - r.started),
            tools: Some(r.tools),
            tool_errors: Some(r.tool_errors),
            item_id: r.item.id,
            no: r.item.no,
            title: r.item.title,
            session_id: Some(r.session),
            tokens: None,
            by: None,
        })
        .collect())
}

fn decisions(c: &mut impl Read, scope: ReportScope<'_>, item: Option<&str>) -> Result<Vec<Event>> {
    let rows = DecisionReads(c).events(scope, item, READ_MAX as u64)?;
    Ok(rows
        .into_iter()
        .map(|r| {
            let d = r.decision;
            let by = d.settled_by().map(|s| s.as_str().into());
            Event {
                id: d.id,
                ts: d.created,
                kind: "decision",
                bad: d.kind == "fail",
                open: d.status == "open",
                by,
                what: d.kind,
                to: Some(d.status),
                dur_ms: d.resolved.map(|ts| (ts - d.created).max(0)),
                tools: None,
                tool_errors: None,
                tokens: None,
                item_id: r.item.id,
                no: r.item.no,
                title: r.item.title,
                session_id: r.session,
            }
        })
        .collect())
}

fn marks(c: &mut impl Read, scope: ReportScope<'_>, item: Option<&str>) -> Result<Vec<Event>> {
    let rows = MarkReads(c).events(scope, item, READ_MAX as u64)?;
    Ok(rows
        .into_iter()
        .map(|r| Event {
            id: r.id.to_string(),
            ts: r.at,
            kind: "mark",
            bad: r.kind == "fail" || (r.kind == "release" && r.to.as_deref() != Some("released")),
            open: false,
            what: r.kind,
            to: r.to,
            by: (!r.actor.is_empty()).then_some(r.actor),
            dur_ms: None,
            tools: None,
            tool_errors: None,
            tokens: None,
            item_id: r.item.id,
            no: r.item.no,
            title: r.item.title,
            session_id: None,
        })
        .collect())
}

/// The events of `scope` in `[lo, hi)`: a histogram of `buckets` over the span, and the newest events in
/// `[a, b)` (clamped to the span). `None` when the span is empty.
pub fn events(
    c: &mut impl Read,
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
    let report = ReportScope {
        ws: &scope.workspace_id,
        lo,
        hi,
        project: scope.project.as_deref(),
        bot: scope.bot.as_deref(),
    };
    let item = scope.item.as_deref();
    let mut all = Vec::new();
    if want("turn") {
        all.push(turns(c, report, item, now)?);
    }
    if want("decision") {
        all.push(decisions(c, report, item)?);
    }
    if want("mark") {
        all.push(marks(c, report, item)?);
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
            e.tokens = UsageReads(c)
                .tokens_for_turn(&sid, &e.id)?
                .filter(|t| *t > 0);
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
