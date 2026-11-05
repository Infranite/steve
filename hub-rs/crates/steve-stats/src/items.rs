//! One fact row per item (`item_stats`): when it was done, how long it took and where
//! the time went, how its deliveries were judged, and what it cost in tokens. [`compute`] is a pure
//! function of the item's marks, turns, decisions and token records; [`refresh_in`] loads those by
//! `item_id` (index lookups only) and writes the row back.

use std::collections::{BTreeMap, HashSet};

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde_json::Value;
use steve_store::{Result, Tokens};

/// Decision kinds that wait for a person (D4 "等你的时间").
pub const HUMAN_KINDS: &[&str] = &["accept", "ask", "approve", "fail", "confirm"];

/// Release marks that mean a release was attempted (D7); `not_released` never started.
pub const ATTEMPTED: &[&str] = &["released", "failed", "rolled_back", "unknown"];

/// An `item_marks` row.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MarkRow {
    pub ts: i64,
    pub kind: String,
    pub from_v: Option<String>,
    pub to_v: Option<String>,
    pub r#ref: Option<String>,
}

impl MarkRow {
    fn to(&self) -> &str {
        self.to_v.as_deref().unwrap_or_default()
    }
}

/// An `agent_turns` row.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct TurnRow {
    pub started: i64,
    pub ended: Option<i64>,
    pub attempt: i64,
}

/// A `decisions` row (the parts the statistics read).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DecisionRow {
    pub id: String,
    pub kind: String,
    pub status: String,
    pub created: i64,
    pub resolved: Option<i64>,
    pub result: Value,
}

impl DecisionRow {
    /// Decided by YOLO on the user's behalf: not time the user spent.
    pub fn by_yolo(&self) -> bool {
        self.result.get("decided_by").and_then(Value::as_str) == Some("yolo")
    }

    pub fn choice(&self) -> Option<&str> {
        self.result.get("choice").and_then(Value::as_str)
    }

    /// A card that waited for a person.
    pub fn human(&self) -> bool {
        HUMAN_KINDS.contains(&self.kind.as_str()) && !self.by_yolo()
    }
}

/// One model's tokens on an item.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModelTokens {
    pub model: String,
    pub tokens: Tokens,
    pub cost_reported: f64,
}

/// Everything [`compute`] reads about one item.
#[derive(Debug, Clone, Default)]
pub struct ItemInput {
    pub created: i64,
    pub marks: Vec<MarkRow>,
    pub turns: Vec<TurnRow>,
    pub decisions: Vec<DecisionRow>,
    pub usage: Vec<ModelTokens>,
}

/// The `item_stats` row (times in Unix ms, durations in ms).
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct ItemFacts {
    pub created: i64,
    pub first_turn: Option<i64>,
    pub first_deliver: Option<i64>,
    pub last_deliver: Option<i64>,
    /// When the accepted delivery was accepted (the pass, or the delivery itself when automatic).
    pub accepted: Option<i64>,
    /// The last `→done`, if the item is still done.
    pub done_at: Option<i64>,
    pub dropped_at: Option<i64>,
    pub lead_ms: Option<i64>,
    pub cycle_ms: Option<i64>,
    pub wait_ms: Option<i64>,
    pub agent_ms: Option<i64>,
    pub turns: i64,
    pub delivers: i64,
    pub reworks: i64,
    pub first_pass: Option<bool>,
    pub fails: i64,
    pub stalls: i64,
    pub releases: i64,
    pub release_fails: i64,
    pub decisions: i64,
    pub yolo_decided: i64,
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_write: i64,
    pub cost_reported: f64,
    /// `{model: [input, output, cache_read, cache_write]}`, for estimating the cost at query time.
    pub models: BTreeMap<String, [i64; 4]>,
}

impl ItemFacts {
    /// Total tokens (U6) over the item's whole life.
    pub fn tokens(&self) -> i64 {
        self.input + self.output + self.cache_read + self.cache_write
    }
}

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
    let done_at = last_status.filter(|m| m.to() == "done").map(|m| m.ts);
    let dropped_at = last_status.filter(|m| m.to() == "drop").map(|m| m.ts);
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
        yolo_decided: i.decisions.iter().filter(|d| d.by_yolo()).count() as i64,
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
pub fn load(c: &Connection, item_id: &str) -> Result<Option<(String, ItemInput)>> {
    let head: Option<(String, i64)> = c
        .prepare_cached("SELECT workspace_id, created FROM items WHERE id = ?")?
        .query_row([item_id], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?;
    let Some((ws, created)) = head else {
        return Ok(None);
    };
    let marks = c
        .prepare_cached(
            "SELECT ts, kind, from_v, to_v, ref FROM item_marks WHERE item_id = ? ORDER BY ts, id",
        )?
        .query_map([item_id], |r| {
            Ok(MarkRow {
                ts: r.get(0)?,
                kind: r.get(1)?,
                from_v: r.get(2)?,
                to_v: r.get(3)?,
                r#ref: r.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let turns = c
        .prepare_cached(
            "SELECT started, ended, attempt FROM agent_turns WHERE item_id = ? ORDER BY started",
        )?
        .query_map([item_id], |r| {
            Ok(TurnRow {
                started: r.get(0)?,
                ended: r.get(1)?,
                attempt: r.get(2)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let decisions = decisions_of(c, item_id)?;
    let usage = c
        .prepare_cached(
            "SELECT model, SUM(input), SUM(output), SUM(cache_read), SUM(cache_write), COALESCE(SUM(cost_reported), 0)
             FROM usage_records WHERE item_id = ? GROUP BY model ORDER BY model",
        )?
        .query_map([item_id], |r| {
            Ok(ModelTokens {
                model: r.get(0)?,
                tokens: Tokens {
                    input: r.get(1)?,
                    output: r.get(2)?,
                    cache_read: r.get(3)?,
                    cache_write: r.get(4)?,
                    reasoning: None,
                },
                cost_reported: r.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    Ok(Some((
        ws,
        ItemInput {
            created,
            marks,
            turns,
            decisions,
            usage,
        },
    )))
}

/// The item's decisions, oldest first.
pub fn decisions_of(c: &Connection, item_id: &str) -> Result<Vec<DecisionRow>> {
    Ok(c
        .prepare_cached(
            "SELECT id, kind, status, created, resolved, result FROM decisions WHERE item_id = ? ORDER BY created, id",
        )?
        .query_map([item_id], decision_row)?
        .collect::<rusqlite::Result<_>>()?)
}

/// Maps `SELECT id, kind, status, created, resolved, result`.
pub fn decision_row(r: &rusqlite::Row) -> rusqlite::Result<DecisionRow> {
    let result: Option<String> = r.get(5)?;
    Ok(DecisionRow {
        id: r.get(0)?,
        kind: r.get(1)?,
        status: r.get(2)?,
        created: r.get(3)?,
        resolved: r.get(4)?,
        result: result
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(Value::Null),
    })
}

/// Recomputes and stores one item's row (clearing `dirty`); `false` if the item is gone, in which
/// case its row is removed. Call inside the write transaction so no mark slips in between.
pub fn refresh_in(c: &Connection, item_id: &str, now: i64) -> Result<bool> {
    let Some((ws, input)) = load(c, item_id)? else {
        c.execute("DELETE FROM item_stats WHERE item_id = ?", [item_id])?;
        return Ok(false);
    };
    let f = compute(&input, now);
    let models = serde_json::to_string(&f.models)?;
    c.prepare_cached(
        "INSERT INTO item_stats (item_id, workspace_id, created, first_turn, first_deliver, last_deliver, accepted, done_at, dropped_at,
           lead_ms, cycle_ms, wait_ms, agent_ms, turns, delivers, reworks, first_pass, fails, stalls, releases, release_fails,
           decisions, yolo_decided, input, output, cache_read, cache_write, cost_reported, models, dirty, updated)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, 0, ?30)
         ON CONFLICT (item_id) DO UPDATE SET workspace_id = ?2, created = ?3, first_turn = ?4, first_deliver = ?5, last_deliver = ?6,
           accepted = ?7, done_at = ?8, dropped_at = ?9, lead_ms = ?10, cycle_ms = ?11, wait_ms = ?12, agent_ms = ?13, turns = ?14,
           delivers = ?15, reworks = ?16, first_pass = ?17, fails = ?18, stalls = ?19, releases = ?20, release_fails = ?21,
           decisions = ?22, yolo_decided = ?23, input = ?24, output = ?25, cache_read = ?26, cache_write = ?27,
           cost_reported = ?28, models = ?29, dirty = 0, updated = ?30",
    )?
    .execute(params![
        item_id,
        ws,
        f.created,
        f.first_turn,
        f.first_deliver,
        f.last_deliver,
        f.accepted,
        f.done_at,
        f.dropped_at,
        f.lead_ms,
        f.cycle_ms,
        f.wait_ms,
        f.agent_ms,
        f.turns,
        f.delivers,
        f.reworks,
        f.first_pass,
        f.fails,
        f.stalls,
        f.releases,
        f.release_fails,
        f.decisions,
        f.yolo_decided,
        f.input,
        f.output,
        f.cache_read,
        f.cache_write,
        f.cost_reported,
        models,
        now
    ])?;
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

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
                mark(0, "status", "run", None),
                mark(1000, "status", "done", None),
            ],
            decisions: vec![
                dec("a", "ask", "resolved", 100, Some(400), json!({})),
                dec("b", "fail", "resolved", 300, Some(500), json!({})),
                // decided by YOLO: not the user's time
                dec(
                    "c",
                    "approve",
                    "resolved",
                    600,
                    Some(900),
                    json!({"decided_by": "yolo"}),
                ),
            ],
            ..Default::default()
        };
        let f = compute(&i, 5000);
        assert_eq!(f.wait_ms, Some(400));
        assert_eq!(f.lead_ms, Some(1000));
        assert_eq!(f.yolo_decided, 1);
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
                mark(10, "status", "done", None),
                mark(20, "status", "run", None),
            ],
            ..Default::default()
        };
        assert_eq!(compute(&i, 100).done_at, None);
        let again = ItemInput {
            marks: vec![
                mark(10, "status", "done", None),
                mark(20, "status", "run", None),
                mark(30, "status", "done", None),
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
