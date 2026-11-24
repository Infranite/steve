use sea_query::{Expr, ExprTrait, Func, JoinType, Query};

use crate::{Read, Result};

use super::reports::{join_items, range};
use super::{ItemStats, ReportScope, SessionReads};

pub struct ConversationMeasures<'a, C>(pub &'a mut C);

impl<C: Read> SessionReads<'_, C> {
    pub fn by_completed_item(&mut self, scope: ReportScope<'_>) -> Result<Vec<i64>> {
        let direct = Query::select()
            .expr(Func::count(Expr::col(("x", "id"))))
            .from_as("sessions", "x")
            .and_where(Expr::col(("x", "item_id")).equals(("s", "item_id")))
            .take();
        let mut q = Query::select();
        q.expr(Expr::SubQuery(None, Box::new(direct.into())))
            .from_as("item_stats", "s")
            .and_where(Expr::col(("s", "workspace_id")).eq(scope.ws))
            .and_where(range(Expr::col(("s", "done_at")), scope));
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
