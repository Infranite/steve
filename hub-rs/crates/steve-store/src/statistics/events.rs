use sea_query::{Expr, ExprTrait, Func, JoinType, Order, Query, SelectStatement};

use super::items::decision_row;
use super::reports::{dimensions, range};
use super::{DecisionReads, DecisionRow, MarkReads, ReportScope, TurnReads, UsageReads};
use crate::{Read, Result};

pub struct TurnEvent {
    pub id: String,
    pub session: String,
    pub started: i64,
    pub ended: Option<i64>,
    pub reason: Option<String>,
    pub transient: bool,
    pub tools: i64,
    pub tool_errors: i64,
    pub item: EventItem,
}

pub struct DecisionEvent {
    pub decision: DecisionRow,
    pub session: Option<String>,
    pub item: EventItem,
}

pub struct MarkEvent {
    pub id: i64,
    pub at: i64,
    pub kind: String,
    pub to: Option<String>,
    pub actor: String,
    pub item: EventItem,
}

pub struct EventItem {
    pub id: Option<String>,
    pub no: Option<i64>,
    pub title: Option<String>,
}

fn read_item(r: &crate::contract::Row<'_>, idx: usize) -> Result<EventItem> {
    Ok(EventItem {
        id: r.get(idx)?,
        no: r.get(idx + 1)?,
        title: r.get(idx + 2)?,
    })
}

fn select_events(
    table: &'static str,
    alias: &'static str,
    at: &'static str,
    scope: ReportScope<'_>,
    item: Option<&str>,
    filtered_by_item: bool,
    limit: u64,
) -> SelectStatement {
    let mut q = Query::select();
    q.from_as(table, alias)
        .join_as(
            if table == "item_marks" {
                JoinType::InnerJoin
            } else {
                JoinType::LeftJoin
            },
            "items",
            "i",
            Expr::col(("i", "id")).equals((alias, "item_id")),
        )
        .and_where(Expr::col((alias, "workspace_id")).eq(scope.ws))
        .and_where(range(Expr::col((alias, at)), scope))
        .order_by((alias, at), Order::Desc)
        .limit(limit);
    dimensions(&mut q, if filtered_by_item { "i" } else { alias }, scope);
    if let Some(id) = item {
        q.and_where(Expr::col((alias, "item_id")).eq(id));
    }
    q
}

impl<C: Read> TurnReads<'_, C> {
    pub fn events(
        &mut self,
        scope: ReportScope<'_>,
        item: Option<&str>,
        limit: u64,
    ) -> Result<Vec<TurnEvent>> {
        self.0.all(
            select_events("agent_turns", "t", "started", scope, item, false, limit)
                .columns(
                    [
                        "id",
                        "session_id",
                        "started",
                        "ended",
                        "stop_reason",
                        "transient",
                        "tools",
                        "tool_errors",
                        "item_id",
                    ]
                    .map(|c| ("t", c)),
                )
                .columns([("i", "no"), ("i", "title")]),
            |r| {
                Ok(TurnEvent {
                    id: r.get(0)?,
                    session: r.get(1)?,
                    started: r.get(2)?,
                    ended: r.get(3)?,
                    reason: r.get(4)?,
                    transient: r.get(5)?,
                    tools: r.get(6)?,
                    tool_errors: r.get(7)?,
                    item: read_item(r, 8)?,
                })
            },
        )
    }
}

impl<C: Read> DecisionReads<'_, C> {
    pub fn events(
        &mut self,
        scope: ReportScope<'_>,
        item: Option<&str>,
        limit: u64,
    ) -> Result<Vec<DecisionEvent>> {
        self.0.all(
            select_events("decisions", "d", "created", scope, item, true, limit)
                .columns(
                    [
                        "id",
                        "kind",
                        "status",
                        "created",
                        "resolved",
                        "result",
                        "session_id",
                        "item_id",
                    ]
                    .map(|c| ("d", c)),
                )
                .columns([("i", "no"), ("i", "title")]),
            |r| {
                Ok(DecisionEvent {
                    decision: decision_row(r)?,
                    session: r.get(6)?,
                    item: read_item(r, 7)?,
                })
            },
        )
    }
}

impl<C: Read> MarkReads<'_, C> {
    pub fn events(
        &mut self,
        scope: ReportScope<'_>,
        item: Option<&str>,
        limit: u64,
    ) -> Result<Vec<MarkEvent>> {
        self.0.all(
            select_events("item_marks", "m", "ts", scope, item, true, limit)
                .columns(["id", "ts", "kind", "to_v", "actor", "item_id"].map(|c| ("m", c)))
                .columns([("i", "no"), ("i", "title")]),
            |r| {
                Ok(MarkEvent {
                    id: r.get(0)?,
                    at: r.get(1)?,
                    kind: r.get(2)?,
                    to: r.get(3)?,
                    actor: r.get(4)?,
                    item: read_item(r, 5)?,
                })
            },
        )
    }
}

pub(super) fn sum_known_tokens() -> Expr {
    let component = |field| {
        Expr::from(Func::coalesce([
            Expr::col(field),
            Expr::Constant(0_i64.into()),
        ]))
    };
    Func::sum(
        component("input")
            .add(component("output"))
            .add(component("cache_read"))
            .add(component("cache_write")),
    )
    .into()
}

impl<C: Read> UsageReads<'_, C> {
    pub fn tokens_for_turn(&mut self, session: &str, turn: &str) -> Result<Option<i64>> {
        Ok(self
            .0
            .one(
                Query::select()
                    .expr(sum_known_tokens())
                    .from("usage_records")
                    .and_where(Expr::col("session_id").eq(session))
                    .and_where(Expr::col("turn_id").eq(turn)),
                |r| r.get::<Option<i64>>(0),
            )?
            .flatten())
    }
}
