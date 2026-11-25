//! One fact row per item (`item_stats`): when it was done, how long it took and where
//! the time went, how its deliveries were judged, and what it cost in tokens. [`compute`] is a pure
//! function of the item's marks, turns, decisions and token records; [`refresh_in`] loads those by
//! `item_id` (index lookups only) and writes the row back.

use std::collections::HashSet;

use steve_store::statistics::{DecisionReads, ItemHistory, ItemStats, TurnReads, UsageReads};
use steve_store::{Read, Result, W};

/// Release marks that mean a release was attempted (D7); `not_released` never started.
pub const ATTEMPTED: &[&str] = &["released", "failed", "rolled_back", "unknown"];

pub use steve_store::statistics::{
    DecisionRow, DecisionSettler, HUMAN_KINDS, ItemFacts, ItemInput, MarkRow, ModelTokens, TurnRow,
};

/// How a delivery was judged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    Pass,
    Rework,
    /// Replaced by a newer delivery (or no card at all): not a verdict, the next delivery counts.
    Superseded,
    /// Still waiting for the user.
    Pending,
}

fn verdict(d: &DecisionRow) -> Verdict {
    match (d.status.as_str(), d.choice()) {
        ("open", _) => Verdict::Pending,
        (_, Some("pass")) => Verdict::Pass,
        (_, Some("rework")) => Verdict::Rework,
        _ => Verdict::Superseded,
    }
}

/// A delivery with its verdict: (delivered at, verdict, accepted at).
type Delivery = (i64, Verdict, Option<i64>);

/// The deliveries in order. A manual delivery is judged by its accept card (`ref`, or for marks without
/// one the first unused accept card created from shortly before it up to the next delivery); an
/// automatic one passed by itself. Items without deliver marks fall back to their accept cards.
fn deliveries(marks: &[&MarkRow], decisions: &[DecisionRow]) -> Vec<Delivery> {
    let accepts: Vec<&DecisionRow> = decisions.iter().filter(|d| d.kind == "accept").collect();
    if marks.is_empty() {
        return accepts
            .iter()
            .map(|d| (d.created, verdict(d), d.resolved))
            .collect();
    }
    let mut used = HashSet::new();
    let mut out = Vec::with_capacity(marks.len());
    for (i, m) in marks.iter().enumerate() {
        if m.to() == "auto" {
            out.push((m.ts, Verdict::Pass, Some(m.ts)));
            continue;
        }
        let next = marks.get(i + 1).map_or(i64::MAX, |n| n.ts);
        let card = m
            .r#ref
            .as_deref()
            .and_then(|r| accepts.iter().find(|d| d.id == r))
            .or_else(|| {
                accepts.iter().find(|d| {
                    !used.contains(&d.id) && d.created >= m.ts - 2_000 && d.created < next
                })
            });
        match card {
            Some(d) => {
                used.insert(d.id.clone());
                out.push((m.ts, verdict(d), d.resolved));
            }
            None => out.push((m.ts, Verdict::Superseded, None)),
        }
    }
    out
}

/// Total length of the union of `[a, b)` intervals, each clipped to `[lo, hi]`.
pub fn union_ms(intervals: impl IntoIterator<Item = (i64, i64)>, lo: i64, hi: i64) -> i64 {
    let mut v: Vec<(i64, i64)> = intervals
        .into_iter()
        .map(|(a, b)| (a.max(lo), b.min(hi)))
        .filter(|(a, b)| b > a)
        .collect();
    v.sort_unstable();
    let mut total = 0;
    let mut cur: Option<(i64, i64)> = None;
    for (a, b) in v {
        match cur {
            Some((ca, cb)) if a <= cb => cur = Some((ca, cb.max(b))),
            Some((ca, cb)) => {
                total += cb - ca;
                cur = Some((a, b));
            }
            None => cur = Some((a, b)),
        }
    }
    if let Some((a, b)) = cur {
        total += b - a;
    }
    total
}

/// The item's facts as of `now` (open cards and running turns count up to `now`).
pub fn compute(i: &ItemInput, now: i64) -> ItemFacts {
    let mut marks: Vec<&MarkRow> = i.marks.iter().collect();
    marks.sort_by_key(|m| m.ts);
    let of = |k: &'static str| marks.iter().copied().filter(move |m| m.kind == k);

    let last_status = of("status").next_back();
    let done_at = last_status.filter(|m| m.to() == "completed").map(|m| m.ts);
    let dropped_at = last_status.filter(|m| m.to() == "cancelled").map(|m| m.ts);
    let end = done_at.or(dropped_at).unwrap_or(now).max(i.created);

    let delivers: Vec<&MarkRow> = of("deliver").collect();
    let ds = deliveries(&delivers, &i.decisions);
    let first_pass = ds
        .iter()
        .find(|d| d.1 != Verdict::Superseded)
        .and_then(|d| match d.1 {
            Verdict::Pass => Some(true),
            Verdict::Rework => Some(false),
            _ => None,
        });
    let accepted = ds.iter().rev().find(|d| d.1 == Verdict::Pass);

    let first_turn = i.turns.iter().map(|t| t.started).min();
    let cycle_ms = match (accepted, first_turn) {
        (Some(a), Some(f)) if a.0 >= f => Some(a.0 - f),
        _ => None,
    };
    let wait_ms = union_ms(
        i.decisions
            .iter()
            .filter(|d| d.human())
            .map(|d| (d.created, d.resolved.unwrap_or(now))),
        i.created,
        end,
    );
    let agent_ms = union_ms(
        i.turns.iter().map(|t| (t.started, t.ended.unwrap_or(now))),
        i.created,
        end,
    );

    let releases = of("release").filter(|m| ATTEMPTED.contains(&m.to()));
    let mut f = ItemFacts {
        created: i.created,
        first_turn,
        first_deliver: delivers.first().map(|m| m.ts),
        last_deliver: delivers.last().map(|m| m.ts),
        accepted: accepted.and_then(|a| a.2),
        done_at,
        dropped_at,
        lead_ms: done_at.map(|d| (d - i.created).max(0)),
        cycle_ms,
        wait_ms: Some(wait_ms),
        agent_ms: Some(agent_ms),
        turns: i.turns.iter().filter(|t| t.attempt == 0).count() as i64,
        delivers: delivers.len() as i64,
        reworks: i
            .decisions
            .iter()
            .filter(|d| d.kind == "accept" && d.choice() == Some("rework"))
            .count() as i64,
        first_pass,
        fails: of("fail").count() as i64,
        stalls: of("fail").filter(|m| m.to() == "stall").count() as i64,
        releases: releases.clone().count() as i64,
        release_fails: releases.filter(|m| m.to() != "released").count() as i64,
        decisions: i.decisions.len() as i64,
        ..ItemFacts::default()
    };
    for u in &i.usage {
        let t = &u.tokens;
        let n = [t.input, t.output, t.cache_read, t.cache_write].map(|v| v.unwrap_or(0));
        f.input += n[0];
        f.output += n[1];
        f.cache_read += n[2];
        f.cache_write += n[3];
        f.cost_reported += u.cost_reported;
        let e = f.models.entry(u.model.clone()).or_default();
        for k in 0..4 {
            e[k] += n[k];
        }
    }
    f
}

/// Reads what [`compute`] needs about `item_id`; `None` if the item does not exist. Returns the
/// workspace and the input.
pub fn decisions_of(c: &mut impl Read, id: &str) -> Result<Vec<DecisionRow>> {
    DecisionReads(c).for_item(id)
}

pub fn load(c: &mut impl Read, id: &str) -> Result<Option<(String, ItemInput)>> {
    let Some((ws, created)) = ItemHistory(c).created(id)? else {
        return Ok(None);
    };
    Ok(Some((
        ws,
        ItemInput {
            created,
            marks: ItemHistory(c).marks(id)?,
            turns: TurnReads(c).for_item(id)?,
            decisions: DecisionReads(c).for_item(id)?,
            usage: UsageReads(c).totals_by_model(id)?,
        },
    )))
}

/// Recomputes and stores one item's row, clearing `dirty` in the same transaction as its reads.
pub fn refresh_in(w: &mut W<'_>, id: &str, now: i64) -> Result<bool> {
    let Some((ws, input)) = load(w, id)? else {
        ItemStats(w).remove(id)?;
        return Ok(false);
    };
    ItemStats(w).put(&ws, id, &compute(&input, now), now)?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use steve_store::Tokens;

    fn mark(ts: i64, kind: &str, to: &str, r: Option<&str>) -> MarkRow {
        MarkRow {
            ts,
            kind: kind.into(),
            from_v: None,
            to_v: Some(to.into()),
            r#ref: r.map(Into::into),
        }
    }

    fn dec(
        id: &str,
        kind: &str,
        status: &str,
        created: i64,
        resolved: Option<i64>,
        result: Value,
    ) -> DecisionRow {
        DecisionRow {
            id: id.into(),
            kind: kind.into(),
            status: status.into(),
            created,
            resolved,
            result,
        }
    }

    #[test]
    fn union_does_not_count_overlap_twice() {
        assert_eq!(union_ms([(0, 10), (5, 20), (30, 40)], 0, 100), 30);
        assert_eq!(union_ms([(0, 10), (5, 20)], 8, 15), 7);
        assert_eq!(union_ms([], 0, 100), 0);
        assert_eq!(union_ms([(10, 5)], 0, 100), 0);
    }

    #[test]
    fn two_overlapping_cards_wait_once() {
        let i = ItemInput {
            created: 0,
            marks: vec![
                mark(0, "status", "active", None),
                mark(1000, "status", "completed", None),
            ],
            decisions: vec![
                dec("a", "ask", "resolved", 100, Some(400), json!({})),
                dec("b", "fail", "resolved", 300, Some(500), json!({})),
                // settled automatically by Steve: not the user's wait time
                dec(
                    "c",
                    "approve",
                    "resolved",
                    600,
                    Some(900),
                    json!({"decided_by": "steve"}),
                ),
            ],
            ..Default::default()
        };
        let f = compute(&i, 5000);
        assert_eq!(f.wait_ms, Some(400));
        assert_eq!(f.lead_ms, Some(1000));
        assert_eq!(f.decisions, 3);
    }

    #[test]
    fn first_pass_skips_superseded_deliveries() {
        let i = ItemInput {
            created: 0,
            marks: vec![
                mark(100, "deliver", "manual", Some("d1")),
                mark(200, "deliver", "manual", Some("d2")),
                mark(300, "deliver", "manual", Some("d3")),
            ],
            decisions: vec![
                dec(
                    "d1",
                    "accept",
                    "expired",
                    100,
                    Some(200),
                    json!({"superseded": "Bot 已经交付"}),
                ),
                dec(
                    "d2",
                    "accept",
                    "resolved",
                    200,
                    Some(250),
                    json!({"choice": "rework"}),
                ),
                dec(
                    "d3",
                    "accept",
                    "resolved",
                    300,
                    Some(350),
                    json!({"choice": "pass"}),
                ),
            ],
            turns: vec![TurnRow {
                started: 50,
                ended: Some(90),
                attempt: 0,
            }],
            ..Default::default()
        };
        let f = compute(&i, 1000);
        assert_eq!(f.first_pass, Some(false));
        assert_eq!(f.reworks, 1);
        assert_eq!(f.accepted, Some(350));
        assert_eq!(f.cycle_ms, Some(250));
        assert_eq!(f.delivers, 3);
        assert_eq!(f.first_deliver, Some(100));
        assert_eq!(f.last_deliver, Some(300));
    }

    #[test]
    fn auto_delivery_passes_and_open_card_is_no_verdict_yet() {
        let auto = ItemInput {
            marks: vec![mark(10, "deliver", "auto", None)],
            ..Default::default()
        };
        assert_eq!(compute(&auto, 100).first_pass, Some(true));
        let open = ItemInput {
            marks: vec![mark(10, "deliver", "manual", Some("d"))],
            decisions: vec![dec("d", "accept", "open", 10, None, Value::Null)],
            ..Default::default()
        };
        assert_eq!(compute(&open, 100).first_pass, None);
        // the open card waits until now
        assert_eq!(compute(&open, 100).wait_ms, Some(90));
    }

    #[test]
    fn reopened_item_is_not_done() {
        let i = ItemInput {
            marks: vec![
                mark(10, "status", "completed", None),
                mark(20, "status", "active", None),
            ],
            ..Default::default()
        };
        assert_eq!(compute(&i, 100).done_at, None);
        let again = ItemInput {
            marks: vec![
                mark(10, "status", "completed", None),
                mark(20, "status", "active", None),
                mark(30, "status", "completed", None),
            ],
            ..Default::default()
        };
        assert_eq!(compute(&again, 100).done_at, Some(30));
    }

    #[test]
    fn releases_and_tokens() {
        let i = ItemInput {
            marks: vec![
                mark(1, "release", "not_released", None),
                mark(2, "release", "rolled_back", None),
                mark(3, "release", "released", None),
                mark(4, "fail", "stall", None),
                mark(5, "fail", "error", None),
            ],
            usage: vec![
                ModelTokens {
                    model: "m".into(),
                    tokens: Tokens {
                        input: Some(1),
                        output: Some(2),
                        cache_read: Some(3),
                        cache_write: None,
                        reasoning: None,
                    },
                    cost_reported: 0.5,
                },
                ModelTokens {
                    model: "".into(),
                    cost_reported: 1.0,
                    ..Default::default()
                },
            ],
            ..Default::default()
        };
        let f = compute(&i, 10);
        assert_eq!((f.releases, f.release_fails), (2, 1));
        assert_eq!((f.fails, f.stalls), (2, 1));
        assert_eq!(f.tokens(), 6);
        assert_eq!(f.cost_reported, 1.5);
        assert_eq!(f.models.get("m"), Some(&[1, 2, 3, 0]));
    }
}
