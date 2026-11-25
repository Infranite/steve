use std::collections::BTreeMap;

use sea_query::{Expr, ExprTrait, Func, OnConflict, Order, Query};
use serde::Serialize;
use serde_json::Value;

use crate::contract::{Json, Row};
use crate::{Read, Result, Tokens, W};

/// An `item_marks` row.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MarkRow {
    pub ts: i64,
    pub kind: String,
    pub from_v: Option<String>,
    pub to_v: Option<String>,
    pub r#ref: Option<String>,
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

/// Decision kinds that wait for a person (D4 "等你的时间").
pub const HUMAN_KINDS: &[&str] = &["accept", "ask", "approve", "fail", "confirm"];

impl MarkRow {
    pub fn to(&self) -> &str {
        self.to_v.as_deref().unwrap_or_default()
    }
}

/// Who settled a resolved decision (strongly-typed contract across store, stats and UI).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum DecisionSettler {
    /// Settled by a human user.
    Me,
    /// Settled automatically by Steve.
    Steve,
}

impl DecisionSettler {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Me => "me",
            Self::Steve => "steve",
        }
    }
}

impl DecisionRow {
    /// Settled automatically by Steve (not time a person spent waiting).
    pub fn by_steve(&self) -> bool {
        self.result.get("decided_by").and_then(Value::as_str)
            == Some(DecisionSettler::Steve.as_str())
    }

    /// Who settled a resolved decision (`None` while open or expired).
    pub fn settled_by(&self) -> Option<DecisionSettler> {
        (self.status == "resolved").then(|| {
            if self.by_steve() {
                DecisionSettler::Steve
            } else {
                DecisionSettler::Me
            }
        })
    }

    pub fn choice(&self) -> Option<&str> {
        self.result.get("choice").and_then(Value::as_str)
    }

    /// A card that waited for a person.
    pub fn human(&self) -> bool {
        HUMAN_KINDS.contains(&self.kind.as_str()) && !self.by_steve()
    }
}

pub struct ItemStats<'a, C>(pub &'a mut C);
pub struct ItemHistory<'a, C>(pub &'a mut C);
pub struct TurnReads<'a, C>(pub &'a mut C);
pub struct DecisionReads<'a, C>(pub &'a mut C);
pub struct UsageReads<'a, C>(pub &'a mut C);

impl<C: Read> ItemHistory<'_, C> {
    pub fn created(&mut self, id: &str) -> Result<Option<(String, i64)>> {
        self.0.one(
            Query::select()
                .columns(["workspace_id", "created"])
                .from("items")
                .and_where(Expr::col("id").eq(id)),
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
    }

    pub fn marks(&mut self, id: &str) -> Result<Vec<MarkRow>> {
        self.0.all(
            Query::select()
                .columns(["ts", "kind", "from_v", "to_v", "ref"])
                .from("item_marks")
                .and_where(Expr::col("item_id").eq(id))
                .order_by("ts", Order::Asc)
                .order_by("id", Order::Asc),
            |r| {
                Ok(MarkRow {
                    ts: r.get(0)?,
                    kind: r.get(1)?,
                    from_v: r.get(2)?,
                    to_v: r.get(3)?,
                    r#ref: r.get(4)?,
                })
            },
        )
    }

    pub fn tasks(&mut self, ws: &str) -> Result<Vec<(String, i64)>> {
        self.0.all(
            Query::select()
                .columns(["id", "created"])
                .from("items")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("kind").eq("task")),
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
    }

    pub fn task(&mut self, id: &str) -> Result<Option<(String, i64)>> {
        self.0.one(
            Query::select()
                .columns(["workspace_id", "created"])
                .from("items")
                .and_where(Expr::col("id").eq(id))
                .and_where(Expr::col("kind").eq("task")),
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
    }
}

impl<C: Read> TurnReads<'_, C> {
    pub fn for_item(&mut self, id: &str) -> Result<Vec<TurnRow>> {
        self.0.all(
            Query::select()
                .columns(["started", "ended", "attempt"])
                .from("agent_turns")
                .and_where(Expr::col("item_id").eq(id))
                .order_by("started", Order::Asc),
            |r| {
                Ok(TurnRow {
                    started: r.get(0)?,
                    ended: r.get(1)?,
                    attempt: r.get(2)?,
                })
            },
        )
    }
}

pub(super) fn decision_row(r: &Row<'_>) -> Result<DecisionRow> {
    Ok(DecisionRow {
        id: r.get(0)?,
        kind: r.get(1)?,
        status: r.get(2)?,
        created: r.get(3)?,
        resolved: r.get(4)?,
        result: r.get::<Option<Json<_>>>(5)?.map_or(Value::Null, |v| v.0),
    })
}

impl<C: Read> DecisionReads<'_, C> {
    pub fn for_item(&mut self, id: &str) -> Result<Vec<DecisionRow>> {
        self.0.all(
            Query::select()
                .columns(["id", "kind", "status", "created", "resolved", "result"])
                .from("decisions")
                .and_where(Expr::col("item_id").eq(id))
                .order_by("created", Order::Asc)
                .order_by("id", Order::Asc),
            decision_row,
        )
    }

    pub fn has_item_activity(&mut self, since: i64) -> Result<bool> {
        for column in ["created", "resolved"] {
            if self
                .0
                .one(
                    Query::select()
                        .column("item_id")
                        .from("decisions")
                        .and_where(Expr::col("item_id").is_not_null())
                        .and_where(Expr::col(column).gte(since))
                        .limit(1),
                    |_| Ok(()),
                )?
                .is_some()
            {
                return Ok(true);
            }
        }
        Ok(false)
    }

    pub fn items_changed(&mut self, since: i64) -> Result<Vec<String>> {
        self.0.all(
            Query::select()
                .distinct()
                .column("item_id")
                .from("decisions")
                .and_where(Expr::col("item_id").is_not_null())
                .and_where(
                    Expr::col("created")
                        .gte(since)
                        .or(Expr::col("resolved").gte(since)),
                ),
            |r| r.get(0),
        )
    }
}

impl<C: Read> UsageReads<'_, C> {
    pub fn totals_by_model(&mut self, item: &str) -> Result<Vec<ModelTokens>> {
        let mut q = Query::select();
        q.column("model").from("usage_records");
        for column in [
            "input",
            "output",
            "cache_read",
            "cache_write",
            "cost_reported",
        ] {
            q.expr(Func::sum(Expr::col(column)));
        }
        self.0.all(
            q.and_where(Expr::col("item_id").eq(item))
                .group_by_col("model")
                .order_by("model", Order::Asc),
            |r| {
                Ok(ModelTokens {
                    model: r.get(0)?,
                    tokens: Tokens {
                        input: r.get(1)?,
                        output: r.get(2)?,
                        cache_read: r.get(3)?,
                        cache_write: r.get(4)?,
                        reasoning: None,
                    },
                    cost_reported: r.get::<Option<f64>>(5)?.unwrap_or_default(),
                })
            },
        )
    }
}

impl<C: Read> ItemStats<'_, C> {
    pub fn has_dirty(&mut self) -> Result<bool> {
        Ok(self
            .0
            .one(
                Query::select()
                    .column("item_id")
                    .from("item_stats")
                    .and_where(Expr::col("dirty").eq(1_i64))
                    .limit(1),
                |_| Ok(()),
            )?
            .is_some())
    }

    pub fn dirty(&mut self, limit: usize) -> Result<Vec<(String, String)>> {
        self.0.all(
            Query::select()
                .columns(["item_id", "workspace_id"])
                .from("item_stats")
                .and_where(Expr::col("dirty").eq(1_i64))
                .limit(limit as u64),
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
    }
}

const COLUMNS: [&str; 29] = [
    "workspace_id",
    "created",
    "first_turn",
    "first_deliver",
    "last_deliver",
    "accepted",
    "done_at",
    "dropped_at",
    "lead_ms",
    "cycle_ms",
    "wait_ms",
    "agent_ms",
    "turns",
    "delivers",
    "reworks",
    "first_pass",
    "fails",
    "stalls",
    "releases",
    "release_fails",
    "decisions",
    "input",
    "output",
    "cache_read",
    "cache_write",
    "cost_reported",
    "models",
    "dirty",
    "updated",
];

impl ItemStats<'_, W<'_>> {
    pub fn flag(&mut self, ws: &str, items: &[(String, i64)], now: i64) -> Result<()> {
        for batch in items.chunks(150) {
            let mut q = Query::insert();
            q.into_table("item_stats").columns([
                "item_id",
                "workspace_id",
                "created",
                "dirty",
                "updated",
            ]);
            for (id, created) in batch {
                q.values_panic([
                    id.as_str().into(),
                    ws.into(),
                    (*created).into(),
                    1_i64.into(),
                    now.into(),
                ]);
            }
            self.0.exec(
                q.on_conflict(
                    OnConflict::column("item_id")
                        .value("dirty", 1_i64)
                        .to_owned(),
                ),
            )?;
        }
        Ok(())
    }

    pub fn put(&mut self, ws: &str, id: &str, f: &ItemFacts, now: i64) -> Result<()> {
        self.0.exec(
            Query::insert()
                .into_table("item_stats")
                .columns(std::iter::once("item_id").chain(COLUMNS))
                .values_panic([
                    id.into(),
                    ws.into(),
                    f.created.into(),
                    f.first_turn.into(),
                    f.first_deliver.into(),
                    f.last_deliver.into(),
                    f.accepted.into(),
                    f.done_at.into(),
                    f.dropped_at.into(),
                    f.lead_ms.into(),
                    f.cycle_ms.into(),
                    f.wait_ms.into(),
                    f.agent_ms.into(),
                    f.turns.into(),
                    f.delivers.into(),
                    f.reworks.into(),
                    f.first_pass.map(i64::from).into(),
                    f.fails.into(),
                    f.stalls.into(),
                    f.releases.into(),
                    f.release_fails.into(),
                    f.decisions.into(),
                    f.input.into(),
                    f.output.into(),
                    f.cache_read.into(),
                    f.cache_write.into(),
                    f.cost_reported.into(),
                    serde_json::to_string(&f.models)?.into(),
                    0_i64.into(),
                    now.into(),
                ])
                .on_conflict(
                    OnConflict::column("item_id")
                        .update_columns(COLUMNS)
                        .to_owned(),
                ),
        )?;
        Ok(())
    }

    pub fn remove(&mut self, id: &str) -> Result<()> {
        self.0.exec(
            Query::delete()
                .from_table("item_stats")
                .and_where(Expr::col("item_id").eq(id)),
        )?;
        Ok(())
    }
}
