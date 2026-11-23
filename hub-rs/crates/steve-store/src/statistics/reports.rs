use std::collections::BTreeMap;

use sea_query::{Expr, ExprTrait, Func, JoinType, Order, Query, SelectStatement};

use crate::contract::Json;
use crate::{Read, Result};

use super::items::decision_row;
use super::{
    DailyFilter, DecisionReads, DecisionRow, ItemHistory, ItemStats, TurnReads, UsageReads,
};

#[derive(Clone, Copy)]
pub struct ReportScope<'a> {
    pub ws: &'a str,
    pub lo: i64,
    pub hi: i64,
    pub project: Option<&'a str>,
    pub bot: Option<&'a str>,
}

impl ReportScope<'_> {
    pub fn has_item_filters(&self) -> bool {
        self.project.is_some() || self.bot.is_some()
    }
}

pub struct MarkReads<'a, C>(pub &'a mut C);
pub struct TimelineReads<'a, C>(pub &'a mut C);
pub struct SessionReads<'a, C>(pub &'a mut C);

pub struct CompletedItem {
    pub id: String,
    pub lead: Option<i64>,
    pub cycle: Option<i64>,
    pub first: Option<i64>,
    pub wait: Option<i64>,
    pub agent: Option<i64>,
    pub accepted: Option<i64>,
    pub tokens: i64,
    pub models: BTreeMap<String, [i64; 4]>,
    pub done_at: i64,
}

pub type MarkChange = (String, i64, Option<String>, Option<String>);

pub struct TurnMeasure {
    pub started: i64,
    pub ended: i64,
    pub reason: Option<String>,
    pub attempt: i64,
    pub transient: bool,
    pub permissions: i64,
}

pub(super) fn range(column: Expr, scope: ReportScope<'_>) -> Expr {
    column.clone().gte(scope.lo).and(column.lt(scope.hi))
}

pub(super) fn dimensions(q: &mut SelectStatement, table: &'static str, scope: ReportScope<'_>) {
    for (field, val) in [("project_id", scope.project), ("bot_id", scope.bot)] {
        if let Some(val) = val {
            q.and_where(Expr::col((table, field)).eq(val));
        }
    }
}

pub(super) fn join_items(q: &mut SelectStatement, table: &'static str, scope: ReportScope<'_>) {
    q.join_as(
        JoinType::InnerJoin,
        "items",
        "i",
        Expr::col(("i", "id")).equals((table, "item_id")),
    );
    dimensions(q, "i", scope);
}

pub(super) fn token_sum(table: &'static str) -> Expr {
    Expr::col((table, "input"))
        .add(Expr::col((table, "output")))
        .add(Expr::col((table, "cache_read")))
        .add(Expr::col((table, "cache_write")))
}

impl<C: Read> ItemHistory<'_, C> {
    pub fn created_sources(&mut self, scope: ReportScope<'_>) -> Result<Vec<Vec<String>>> {
        let mut q = Query::select();
        q.column(("i", "source"))
            .from_as("items", "i")
            .and_where(Expr::col(("i", "workspace_id")).eq(scope.ws))
            .and_where(Expr::col(("i", "kind")).eq("task"))
            .and_where(range(Expr::col(("i", "created")), scope));
        dimensions(&mut q, "i", scope);
        self.0.all(&q, |r| Ok(r.get::<Json<_>>(0)?.0))
    }

    pub fn open_tasks(&mut self, scope: ReportScope<'_>, states: &[&str]) -> Result<i64> {
        let mut q = Query::select();
        q.expr(Func::count(Expr::col(("i", "id"))))
            .from_as("items", "i")
            .and_where(Expr::col(("i", "workspace_id")).eq(scope.ws))
            .and_where(Expr::col(("i", "kind")).eq("task"))
            .and_where(Expr::col(("i", "archived")).eq(0_i64))
            .and_where(Expr::col(("i", "status")).is_in(states.iter().copied()));
        dimensions(&mut q, "i", scope);
        Ok(self.0.one(&q, |r| r.get(0))?.unwrap_or_default())
    }
}

impl<C: Read> MarkReads<'_, C> {
    pub fn changes(&mut self, scope: ReportScope<'_>, kind: &str) -> Result<Vec<MarkChange>> {
        let mut q = Query::select();
        q.columns([
            ("m", "item_id"),
            ("m", "ts"),
            ("m", "from_v"),
            ("m", "to_v"),
        ])
        .from_as("item_marks", "m")
        .and_where(Expr::col(("m", "workspace_id")).eq(scope.ws))
        .and_where(Expr::col(("m", "kind")).eq(kind))
        .and_where(range(Expr::col(("m", "ts")), scope))
        .order_by(("m", "ts"), Order::Asc)
        .order_by(("m", "id"), Order::Asc);
        join_items(&mut q, "m", scope);
        self.0
            .all(&q, |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))
    }
}

impl<C: Read> ItemStats<'_, C> {
    pub fn completed(&mut self, scope: ReportScope<'_>) -> Result<Vec<CompletedItem>> {
        let mut q = Query::select();
        q.columns([("s", "item_id"), ("s", "lead_ms"), ("s", "cycle_ms")])
            .expr(Expr::col(("s", "first_deliver")).sub(Expr::col(("s", "first_turn"))))
            .columns([("s", "wait_ms"), ("s", "agent_ms"), ("s", "accepted")])
            .expr(token_sum("s"))
            .columns([("s", "models"), ("s", "done_at")])
            .from_as("item_stats", "s")
            .and_where(Expr::col(("s", "workspace_id")).eq(scope.ws))
            .and_where(range(Expr::col(("s", "done_at")), scope));
        join_items(&mut q, "s", scope);
        self.0.all(&q, |r| {
            Ok(CompletedItem {
                id: r.get(0)?,
                lead: r.get(1)?,
                cycle: r.get(2)?,
                first: r.get(3)?,
                wait: r.get(4)?,
                agent: r.get(5)?,
                accepted: r.get(6)?,
                tokens: r.get(7)?,
                models: r.get::<Json<_>>(8)?.0,
                done_at: r.get(9)?,
            })
        })
    }

    pub fn first_passes(&mut self, scope: ReportScope<'_>) -> Result<Vec<bool>> {
        let mut q = Query::select();
        q.column(("s", "first_pass"))
            .from_as("item_stats", "s")
            .and_where(Expr::col(("s", "workspace_id")).eq(scope.ws))
            .and_where(range(Expr::col(("s", "first_deliver")), scope))
            .and_where(Expr::col(("s", "first_pass")).is_not_null());
        join_items(&mut q, "s", scope);
        self.0.all(&q, |r| r.get(0))
    }
}

impl<C: Read> DecisionReads<'_, C> {
    pub fn created(&mut self, scope: ReportScope<'_>) -> Result<Vec<(DecisionRow, String)>> {
        let mut q = Query::select();
        q.columns(
            [
                "id",
                "kind",
                "status",
                "created",
                "resolved",
                "result",
                "subject_type",
            ]
            .map(|c| ("d", c)),
        )
        .from_as("decisions", "d")
        .and_where(Expr::col(("d", "workspace_id")).eq(scope.ws))
        .and_where(range(Expr::col(("d", "created")), scope));
        if scope.has_item_filters() {
            join_items(&mut q, "d", scope);
        }
        self.0.all(&q, |r| Ok((decision_row(r)?, r.get(6)?)))
    }

    pub fn reworks(&mut self, scope: ReportScope<'_>) -> Result<i64> {
        let choice = self
            .0
            .db()
            .dialect()
            .json_field(Expr::col(("d", "result")), "choice");
        let mut q = Query::select();
        q.expr(Func::count(Expr::col(("d", "id"))))
            .from_as("decisions", "d")
            .and_where(Expr::col(("d", "workspace_id")).eq(scope.ws))
            .and_where(Expr::col(("d", "kind")).eq("accept"))
            .and_where(range(Expr::col(("d", "resolved")), scope))
            .and_where(choice.eq("rework"));
        join_items(&mut q, "d", scope);
        Ok(self.0.one(&q, |r| r.get(0))?.unwrap_or_default())
    }
}

impl<C: Read> TimelineReads<'_, C> {
    pub fn owner_messages(&mut self, scope: ReportScope<'_>) -> Result<i64> {
        let mut q = Query::select();
        q.expr(Func::count(Expr::col(("t", "item_id"))))
            .from_as("timeline", "t")
            .and_where(Expr::col(("i", "workspace_id")).eq(scope.ws))
            .and_where(Expr::col(("t", "kind")).eq("me"))
            .and_where(Expr::col(("t", "actor")).eq("me"))
            .and_where(range(Expr::col(("t", "ts")), scope));
        join_items(&mut q, "t", scope);
        Ok(self.0.one(&q, |r| r.get(0))?.unwrap_or_default())
    }
}

impl<C: Read> TurnReads<'_, C> {
    pub fn completed_in(&mut self, scope: ReportScope<'_>) -> Result<Vec<TurnMeasure>> {
        let mut q = Query::select();
        q.columns(
            [
                "started",
                "ended",
                "stop_reason",
                "attempt",
                "transient",
                "permissions",
            ]
            .map(|c| ("t", c)),
        )
        .from_as("agent_turns", "t")
        .and_where(Expr::col(("t", "workspace_id")).eq(scope.ws))
        .and_where(range(Expr::col(("t", "started")), scope))
        .and_where(Expr::col(("t", "ended")).is_not_null());
        dimensions(&mut q, "t", scope);
        self.0.all(&q, |r| {
            Ok(TurnMeasure {
                started: r.get(0)?,
                ended: r.get(1)?,
                reason: r.get(2)?,
                attempt: r.get(3)?,
                transient: r.get(4)?,
                permissions: r.get(5)?,
            })
        })
    }
}

impl<C: Read> UsageReads<'_, C> {
    pub fn gateway_outcomes(&mut self, f: DailyFilter<'_>) -> Result<(i64, i64)> {
        let mut q = Query::select();
        q.expr(Func::sum(
            Expr::col("attempts").gt(Expr::Constant(1_i64.into())),
        ))
        .expr(Func::sum(Expr::col("ok").eq(Expr::Constant(0_i64.into()))))
        .from_as("usage_records", "u")
        .and_where(Expr::col("workspace_id").eq(f.ws))
        .and_where(Expr::col("source").eq("gateway"))
        .and_where(Expr::col("day").between(f.from, f.to));
        for (field, value) in [("project_id", f.project), ("bot_id", f.bot)] {
            if let Some(value) = value {
                q.and_where(Expr::col(field).eq(value));
            }
        }
        Ok(self
            .0
            .one(&q, |r| {
                Ok((
                    r.get::<Option<i64>>(0)?.unwrap_or_default(),
                    r.get::<Option<i64>>(1)?.unwrap_or_default(),
                ))
            })?
            .unwrap_or_default())
    }

    pub fn earliest(&mut self, ws: &str, sources: &[&str]) -> Result<Option<String>> {
        Ok(self
            .0
            .one(
                Query::select()
                    .expr(Func::min(Expr::col("day")))
                    .from("usage_records")
                    .and_where(Expr::col("workspace_id").eq(ws))
                    .and_where(Expr::col("source").is_in(sources.iter().copied())),
                |r| r.get::<Option<String>>(0),
            )?
            .flatten())
    }
}

impl<C: Read> SessionReads<'_, C> {
    pub fn close_to_limit(&mut self, ws: &str) -> Result<i64> {
        Ok(self
            .0
            .one(
                Query::select()
                    .expr(Func::count(Expr::col("id")))
                    .from("sessions")
                    .and_where(Expr::col("workspace_id").eq(ws))
                    .and_where(Expr::col("source").eq("Steve"))
                    .and_where(Expr::col("size").gt(0_i64))
                    .and_where(
                        Expr::col("used")
                            .mul(5_i64)
                            .gt(Expr::col("size").mul(4_i64)),
                    )
                    .and_where(Expr::col("status").is_in(["running", "starting", "idle"])),
                |r| r.get(0),
            )?
            .unwrap_or_default())
    }
}
