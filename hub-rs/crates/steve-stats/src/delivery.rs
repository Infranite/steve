//! General delivery and response statistics: closing invalidations, PR merge latency,
//! delivery rejections, sessions per completed item, channel reply latency, and session first-tool delay.

use std::collections::BTreeMap;

use serde::Serialize;
use steve_store::statistics::{
    ConversationMeasures, ItemStats, MarkReads, ReportScope, SessionReads,
};
use steve_store::{Read, Result};

use crate::metrics::{P, Query, report_scope};

#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Delivery {
    /// Sessions per item done in the range.
    pub sessions_per_item: P,
    /// Closings invalidated, by reason (`steve_core::cycle::void`).
    pub invalidations: BTreeMap<String, i64>,
    /// Pull requests seen merged, and accepted -> merged for those with an acceptance.
    pub merged: i64,
    pub accept_to_merge: P,
    /// Deliveries Steve refused, by reason, and refused / (refused + delivered).
    pub rejected: i64,
    pub rejects: BTreeMap<String, i64>,
    pub delivered: i64,
    pub reject_rate: Option<f64>,
    /// Channel messages answered in the range: stored -> the first reply sent.
    pub reply_latency: P,
    /// Sessions started in the range: start -> first tool call, and how many made none.
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

/// General delivery and response metrics for `q`, within `[lo, hi)`.
pub fn of(c: &mut impl Read, q: &Query, lo: i64, hi: i64) -> Result<Delivery> {
    let scope = report_scope(q, lo, hi);
    let mut d = Delivery::default();

    let sessions = SessionReads(c).by_completed_item(scope)?;
    d.sessions_per_item = P::of(&sessions);

    d.invalidations = by_value(&marks(c, scope, "invalidate")?);
    let merges = marks(c, scope, "merge")?;
    d.merged = merges.len() as i64;
    let mut to_merge = Vec::new();
    for (_, ts, item) in &merges {
        let accepted = ItemStats(c).accepted(item)?;
        if let Some(a) = accepted.filter(|a| a <= ts) {
            to_merge.push(ts - a);
        }
    }
    d.accept_to_merge = P::of(&to_merge);
    let rejects = marks(c, scope, "reject")?;
    d.rejected = rejects.len() as i64;
    d.rejects = by_value(&rejects);
    d.delivered = marks(c, scope, "deliver")?.len() as i64;
    d.reject_rate = ratio(d.rejected, d.rejected + d.delivered);

    let latency = ConversationMeasures(c).reply_delays(scope)?;
    d.reply_latency = P::of(&latency.into_iter().filter(|v| *v >= 0).collect::<Vec<_>>());

    let starts = SessionReads(c).first_tool_delays(scope)?;
    d.no_tool = starts.iter().filter(|s| s.is_none()).count() as i64;
    d.first_tool = P::of(
        &starts
            .into_iter()
            .flatten()
            .map(|v| v.max(0))
            .collect::<Vec<_>>(),
    );
    Ok(d)
}
