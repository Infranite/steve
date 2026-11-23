use sea_query::{Expr, ExprTrait, Func, JoinType, Order, Query, SelectStatement, UnionType};
use serde_json::Value;

use crate::contract::Json;
use crate::{Read, Result};

use super::events::sum_known_tokens;
use super::reports::{dimensions, join_items, range};
use super::{ItemStats, ReportScope, SessionReads, TurnReads, UsageReads};

pub struct WorkflowReads<'a, C>(pub &'a mut C);
pub struct ConversationMeasures<'a, C>(pub &'a mut C);

pub struct WorkflowHistory {
    pub graph: String,
    pub action: String,
    pub unit: Option<String>,
    pub detail: Value,
    pub created: i64,
}

fn child_items(graph: Expr) -> SelectStatement {
    let attempts = Query::select()
        .column("item_id")
        .from("workflow_attempts")
        .and_where(Expr::col("workflow_id").eq(graph.clone()))
        .take();
    Query::select()
        .column("item_id")
        .from("workflow_units")
        .and_where(Expr::col("workflow_id").eq(graph))
        .and_where(Expr::col("item_id").is_not_null())
        .union(UnionType::Distinct, attempts)
        .take()
}

impl<C: Read> WorkflowReads<'_, C> {
    pub fn history(&mut self, scope: ReportScope<'_>) -> Result<Vec<WorkflowHistory>> {
        let active = Query::select()
            .column("workflow_id")
            .from("workflow_history")
            .and_where(range(Expr::col("created"), scope))
            .take();
        let mut q = Query::select();
        q.columns(["workflow_id", "action", "unit_id", "detail", "created"].map(|c| ("h", c)))
            .from_as("workflow_history", "h")
            .join_as(
                JoinType::InnerJoin,
                "items",
                "i",
                Expr::col(("i", "id")).equals(("h", "workflow_id")),
            )
            .and_where(Expr::col(("i", "workspace_id")).eq(scope.ws))
            .and_where(Expr::col(("h", "workflow_id")).in_subquery(active))
            .order_by(("h", "workflow_id"), Order::Asc)
            .order_by(("h", "revision"), Order::Asc);
        dimensions(&mut q, "i", scope);
        self.0.all(&q, |r| {
            Ok(WorkflowHistory {
                graph: r.get(0)?,
                action: r.get(1)?,
                unit: r.get(2)?,
                detail: r.get::<Json<_>>(3)?.0,
                created: r.get(4)?,
            })
        })
    }

    pub fn children(&mut self, graph: &str) -> Result<Vec<String>> {
        self.0.all(&child_items(Expr::value(graph)), |r| r.get(0))
    }
}

impl<C: Read> UsageReads<'_, C> {
    pub fn tokens_for_items(&mut self, ids: &[String], scope: ReportScope<'_>) -> Result<i64> {
        let mut count = 0;
        for ids in ids.chunks(500) {
            count += self
                .0
                .one(
                    Query::select()
                        .expr(sum_known_tokens())
                        .from("usage_records")
                        .and_where(Expr::col("item_id").is_in(ids.iter().map(String::as_str)))
                        .and_where(range(Expr::col("ts"), scope)),
                    |r| r.get::<Option<i64>>(0),
                )?
                .flatten()
                .unwrap_or_default();
        }
        Ok(count)
    }
}

impl<C: Read> TurnReads<'_, C> {
    pub fn item_durations(&mut self, id: &str, scope: ReportScope<'_>) -> Result<Vec<i64>> {
        self.0.all(
            Query::select()
                .columns(["started", "ended"])
                .from("agent_turns")
                .and_where(Expr::col("item_id").eq(id))
                .and_where(range(Expr::col("started"), scope))
                .and_where(Expr::col("ended").is_not_null()),
            |r| Ok(std::cmp::max(r.get::<i64>(1)? - r.get::<i64>(0)?, 0)),
        )
    }
}

impl<C: Read> SessionReads<'_, C> {
    pub fn by_completed_item(&mut self, scope: ReportScope<'_>) -> Result<Vec<i64>> {
        let session_count = |condition: Expr| {
            let q = Query::select()
                .expr(Func::count(Expr::col(("x", "id"))))
                .from_as("sessions", "x")
                .and_where(condition)
                .take();
            Expr::SubQuery(None, Box::new(q.into()))
        };
        let root = Expr::col(("s", "item_id"));
        let direct = session_count(Expr::col(("x", "item_id")).eq(root.clone()));
        let children = session_count(Expr::col(("x", "item_id")).in_subquery(child_items(root)));
        let dialect = self.0.db().dialect();
        let origin = dialect.json_path(Expr::col(("i", "source")), "$[1]");
        let mut q = Query::select();
        q.expr(direct.add(children))
            .from_as("item_stats", "s")
            .and_where(Expr::col(("s", "workspace_id")).eq(scope.ws))
            .and_where(range(Expr::col(("s", "done_at")), scope))
            .and_where(dialect.null_safe_eq(origin, Expr::value("workflow")).not());
        join_items(&mut q, "s", scope);
        self.0.all(&q, |r| r.get(0))
    }

    pub fn first_tool_delays(&mut self, scope: ReportScope<'_>) -> Result<Vec<Option<i64>>> {
        let event = Query::select()
            .expr(Func::min(Expr::col("ts")))
            .from("events")
            .and_where(Expr::col("session_id").equals(("s", "id")))
            .and_where(Expr::col("kind").eq(Expr::Constant(crate::TOOL_CALL_EVENT.into())))
            .take();
        let created = Expr::col(("s", "created"));
        let first: Expr = Func::coalesce([
            Expr::col(("s", "first_tool_at")),
            Expr::SubQuery(None, Box::new(event.into())),
        ])
        .into();
        let mut q = Query::select();
        q.columns([("s", "created")])
            .expr(first)
            .from_as("sessions", "s")
            .join_as(
                JoinType::LeftJoin,
                "items",
                "i",
                Expr::col(("i", "id")).equals(("s", "item_id")),
            )
            .and_where(Expr::col(("s", "workspace_id")).eq(scope.ws))
            .and_where(Expr::col(("s", "source")).eq("Steve"))
            .and_where(range(created, scope));
        if let Some(id) = scope.project {
            q.and_where(Expr::col(("i", "project_id")).eq(id));
        }
        if let Some(id) = scope.bot {
            q.and_where(Expr::col(("s", "bot_id")).eq(id));
        }
        self.0.all(&q, |r| {
            let started: i64 = r.get(0)?;
            Ok(r.get::<Option<i64>>(1)?.map(|at| at - started))
        })
    }
}

impl<C: Read> ItemStats<'_, C> {
    pub fn accepted(&mut self, id: &str) -> Result<Option<i64>> {
        Ok(self
            .0
            .one(
                Query::select()
                    .column("accepted")
                    .from("item_stats")
                    .and_where(Expr::col("item_id").eq(id)),
                |r| r.get::<Option<i64>>(0),
            )?
            .flatten())
    }
}

impl<C: Read> ConversationMeasures<'_, C> {
    pub fn reply_delays(&mut self, scope: ReportScope<'_>) -> Result<Vec<i64>> {
        let sent: Expr = Func::min(Expr::col(("o", "sent"))).into();
        let q = Query::select()
            .expr(sent.sub(Expr::col(("m", "created"))))
            .from_as("chan_messages", "o")
            .join_as(
                JoinType::InnerJoin,
                "chan_messages",
                "m",
                Expr::col(("m", "space_id"))
                    .equals(("o", "space_id"))
                    .and(Expr::col(("m", "external_id")).equals(("o", "reply_to")))
                    .and(Expr::col(("m", "direction")).eq("in")),
            )
            .and_where(Expr::col(("o", "workspace_id")).eq(scope.ws))
            .and_where(Expr::col(("o", "direction")).eq("out"))
            .and_where(Expr::col(("o", "status")).eq("sent"))
            .and_where(Expr::col(("o", "sent")).is_not_null())
            .and_where(Expr::col(("o", "reply_to")).is_not_null())
            .and_where(range(Expr::col(("o", "created")), scope))
            .group_by_col(("m", "id"))
            .take();
        self.0.all(&q, |r| r.get(0))
    }
}
