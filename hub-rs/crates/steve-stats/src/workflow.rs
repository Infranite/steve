//! The "工作流" section of the summary (#242): how work graphs and deliveries go, the figures later
//! workflow changes are measured against.
//!
//! Work graphs are read from their append-only history (`workflow_history`): a `claim` is one attempt of
//! a unit (its `detail.attempt`, 1 for the first), a `deliver` its verdict, and the plan in force says
//! the unit's kind. A graph counts in a range when its history has something in it. The other figures
//! read item marks (`invalidate`, `reject`, `merge`, `wake`, `deliver`), sessions and channel messages.
//!
//! Filters: `project` and `bot` apply through the item (the graph's item for graph figures); the
//! channel reply latency has neither and follows the workspace only.

use std::collections::{BTreeMap, BTreeSet, HashMap};

use serde::Serialize;
use steve_store::statistics::{
    ConversationMeasures, ItemStats, MarkReads, ReportScope, SessionReads, TurnReads, UsageReads,
    WorkflowReads,
};
use steve_store::{Read, Result};

use crate::metrics::{P, Query, report_scope};

/// A coordinator turn longer than this is a long one: the coordinator waited in its turn instead of
/// ending it after planning (Steve wakes it when a unit settles).
pub const LONG_TURN_MS: i64 = 30 * 60_000;

/// Unit kinds (`UnitSpec.kind`), in display order.
pub const UNIT_KINDS: &[&str] = &["work", "review", "integrate"];

/// First attempts of one unit kind that delivered, and how many of them passed.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct FirstTry {
    pub delivered: i64,
    pub passed: i64,
    pub rate: Option<f64>,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Workflow {
    /// Work graphs with history in the range.
    pub graphs: i64,
    /// Units with an attempt that started in the range.
    pub units: i64,
    /// Attempts started in the range (claims).
    pub attempts: i64,
    /// Attempts that were not their unit's first ("单元重试").
    pub retries: i64,
    /// `retries ÷ attempts`.
    pub retry_rate: Option<f64>,
    /// Attempts per unit (each unit's attempts started in the range).
    pub attempts_per_unit: P,
    /// Units reset (their result and their downstream's invalidated), e.g. after a failed review.
    pub resets: i64,
    /// Deliveries of units in the range (passed or failed).
    pub unit_deliveries: i64,
    /// First attempts by unit kind: how many delivered and passed ("首轮通过率").
    pub first_try: BTreeMap<String, FirstTry>,
    /// Sessions per item done in the range: its own and its units'.
    pub sessions_per_item: P,
    /// Tokens of the graphs' own items (the coordinator) and of their units, in the range.
    pub coordinator_tokens: i64,
    pub unit_tokens: i64,
    /// `coordinator_tokens ÷ (coordinator_tokens + unit_tokens)`.
    pub coordinator_share: Option<f64>,
    /// Coordinator turns that started in the range and ended.
    pub coordinator_turn_ms: P,
    /// Of those, the ones longer than [`LONG_TURN_MS`] (given as `long_turn_ms`).
    pub coordinator_long_turns: i64,
    pub long_turn_ms: i64,
    /// Coordinator wakes about a settled unit, by the unit's status.
    pub wakes: BTreeMap<String, i64>,
    /// Closings invalidated, by reason (`steve_core::cycle::void`).
    pub invalidations: BTreeMap<String, i64>,
    /// Pull requests seen merged, and accepted → merged for those with an acceptance.
    pub merged: i64,
    pub accept_to_merge: P,
    /// Deliveries Steve refused, by reason, and refused ÷ (refused + delivered).
    pub rejected: i64,
    pub rejects: BTreeMap<String, i64>,
    pub delivered: i64,
    pub reject_rate: Option<f64>,
    /// Channel messages answered in the range: stored → the first reply sent.
    pub reply_latency: P,
    /// Sessions started in the range: start → first tool call, and how many made none.
    pub first_tool: P,
    pub no_tool: i64,
}

fn ratio(a: i64, b: i64) -> Option<f64> {
    (b > 0).then(|| a as f64 / b as f64)
}

fn marks(
    c: &mut impl Read,
    scope: ReportScope<'_>,
    kind: &str,
) -> Result<Vec<(String, i64, String)>> {
    Ok(MarkReads(c)
        .changes(scope, kind)?
        .into_iter()
        .map(|(id, at, _, to)| (to.unwrap_or_default(), at, id))
        .collect())
}

fn by_value(rows: &[(String, i64, String)]) -> BTreeMap<String, i64> {
    let mut m = BTreeMap::new();
    for (v, _, _) in rows {
        *m.entry(v.clone()).or_default() += 1;
    }
    m
}

/// The section for `q`, within `[lo, hi)` (the summary's bounds).
pub fn of(c: &mut impl Read, q: &Query, lo: i64, hi: i64) -> Result<Workflow> {
    let scope = report_scope(q, lo, hi);
    let mut w = Workflow {
        long_turn_ms: LONG_TURN_MS,
        ..Workflow::default()
    };

    /* graphs: their whole history, read in order, for those with something in the range */
    let history = WorkflowReads(c).history(scope)?;
    for k in UNIT_KINDS {
        w.first_try.insert((*k).into(), FirstTry::default());
    }
    let mut graphs: BTreeSet<&str> = BTreeSet::new();
    let mut kinds: HashMap<(&str, String), String> = HashMap::new();
    let mut attempt: HashMap<(&str, String), i64> = HashMap::new();
    let mut per_unit: BTreeMap<(&str, String), i64> = BTreeMap::new();
    for h in &history {
        graphs.insert(&h.graph);
        let inside = h.created >= lo && h.created < hi;
        match h.action.as_str() {
            "plan" | "replan" => {
                for u in h.detail["units"].as_array().into_iter().flatten() {
                    if let Some(id) = u["id"].as_str() {
                        let kind = u["kind"].as_str().unwrap_or("work");
                        kinds.insert((&h.graph, id.into()), kind.into());
                    }
                }
            }
            "claim" => {
                let Some(unit) = &h.unit else { continue };
                let n = h.detail["attempt"].as_i64().unwrap_or(1);
                attempt.insert((&h.graph, unit.clone()), n);
                if inside {
                    w.attempts += 1;
                    w.retries += i64::from(n > 1);
                    *per_unit.entry((&h.graph, unit.clone())).or_default() += 1;
                }
            }
            "deliver" if inside => {
                let Some(unit) = &h.unit else { continue };
                w.unit_deliveries += 1;
                let key = (h.graph.as_str(), unit.clone());
                if attempt.get(&key) == Some(&1) {
                    let kind = kinds.get(&key).map_or("work", String::as_str);
                    let f = w.first_try.entry(kind.into()).or_default();
                    f.delivered += 1;
                    f.passed += i64::from(h.detail["status"] == "succeeded");
                }
            }
            "reset" if inside => w.resets += 1,
            _ => {}
        }
    }
    for f in w.first_try.values_mut() {
        f.rate = ratio(f.passed, f.delivered);
    }
    w.graphs = graphs.len() as i64;
    w.units = per_unit.len() as i64;
    w.retry_rate = ratio(w.retries, w.attempts);
    w.attempts_per_unit = P::of(&per_unit.values().copied().collect::<Vec<_>>());

    /* the coordinator: the graph's own item, against its units' items */
    let mut coordinator_turns = Vec::new();
    for g in &graphs {
        let children = WorkflowReads(c).children(g)?;
        w.coordinator_tokens += UsageReads(c).tokens_for_items(&[(*g).to_owned()], scope)?;
        w.unit_tokens += UsageReads(c).tokens_for_items(&children, scope)?;
        coordinator_turns.extend(TurnReads(c).item_durations(g, scope)?);
    }
    w.coordinator_share = ratio(w.coordinator_tokens, w.coordinator_tokens + w.unit_tokens);
    w.coordinator_long_turns = coordinator_turns
        .iter()
        .filter(|t| **t > LONG_TURN_MS)
        .count() as i64;
    w.coordinator_turn_ms = P::of(&coordinator_turns);

    /* sessions per item done in the range (units are counted with their graph's item) */
    let sessions = SessionReads(c).by_completed_item(scope)?;
    w.sessions_per_item = P::of(&sessions);

    /* marks */
    w.wakes = by_value(&marks(c, scope, "wake")?);
    w.invalidations = by_value(&marks(c, scope, "invalidate")?);
    let merges = marks(c, scope, "merge")?;
    w.merged = merges.len() as i64;
    let mut to_merge = Vec::new();
    for (_, ts, item) in &merges {
        let accepted = ItemStats(c).accepted(item)?;
        if let Some(a) = accepted.filter(|a| a <= ts) {
            to_merge.push(ts - a);
        }
    }
    w.accept_to_merge = P::of(&to_merge);
    let rejects = marks(c, scope, "reject")?;
    w.rejected = rejects.len() as i64;
    w.rejects = by_value(&rejects);
    w.delivered = marks(c, scope, "deliver")?.len() as i64 + w.unit_deliveries;
    w.reject_rate = ratio(w.rejected, w.rejected + w.delivered);

    /* channels: an inbound message → the first reply to it that was sent */
    let latency = ConversationMeasures(c).reply_delays(scope)?;
    w.reply_latency = P::of(&latency.into_iter().filter(|v| *v >= 0).collect::<Vec<_>>());

    let starts = SessionReads(c).first_tool_delays(scope)?;
    w.no_tool = starts.iter().filter(|s| s.is_none()).count() as i64;
    w.first_tool = P::of(
        &starts
            .into_iter()
            .flatten()
            .map(|v| v.max(0))
            .collect::<Vec<_>>(),
    );
    Ok(w)
}
