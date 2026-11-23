use sea_query::{Expr, ExprTrait, Order, Query, SelectStatement};
use serde_json::Value;

use crate::contract::Json;
use crate::{Read, Result};

pub struct Schedules<'a, C>(pub &'a mut C);

pub struct ScheduleRow {
    pub id: String,
    pub workspace_id: String,
    pub name: String,
    pub enabled: bool,
    pub version: i64,
    pub spec: Value,
    pub created_by: String,
    pub created: i64,
    pub updated: i64,
    pub next_at: Option<i64>,
}

fn schedules() -> SelectStatement {
    Query::select()
        .from("flows")
        .and_where(Expr::col("kind").eq("schedule"))
        .take()
}

fn scope(ws: &str) -> SelectStatement {
    schedules()
        .and_where(Expr::col("workspace_id").eq(ws))
        .take()
}

fn projection(q: &mut SelectStatement, deadline: bool) -> &mut SelectStatement {
    q.columns([
        "id",
        "workspace_id",
        "name",
        "enabled",
        "version",
        "schedule",
        "created_by",
        "created",
        "updated",
    ]);
    if deadline {
        let next = Query::select()
            .column(("t", "next_at"))
            .from_as("flow_triggers", "t")
            .and_where(Expr::col(("t", "flow_id")).equals(("flows", "id")))
            .and_where(Expr::col(("t", "kind")).eq(Expr::Constant("time".into())))
            .limit(1)
            .take();
        q.expr(Expr::case(
            Expr::col("enabled").ne(Expr::Constant(0_i64.into())),
            Expr::SubQuery(None, Box::new(next.into())),
        ));
    }
    q
}

fn decode(r: &super::Row<'_>, deadline: bool) -> Result<ScheduleRow> {
    Ok(ScheduleRow {
        id: r.get(0)?,
        workspace_id: r.get(1)?,
        name: r.get(2)?,
        enabled: r.get(3)?,
        version: r.get(4)?,
        spec: r.get::<Json<_>>(5)?.0,
        created_by: r.get(6)?,
        created: r.get(7)?,
        updated: r.get(8)?,
        next_at: if deadline { r.get(9)? } else { None },
    })
}

impl<C: Read> Schedules<'_, C> {
    pub fn brief_id(&mut self, ws: &str, user: &str, live: bool) -> Result<Option<String>> {
        let dialect = self.0.db().dialect();
        let mut q = scope(ws);
        q.column("id")
            .and_where(dialect.json_valid(Expr::col("schedule")))
            .and_where(
                dialect
                    .json_field(Expr::col("schedule"), "mode")
                    .eq("brief"),
            )
            .and_where(
                dialect
                    .json_field(Expr::col("schedule"), "requester")
                    .eq(user),
            )
            .order_by("deleted", Order::Asc)
            .order_by("created", Order::Desc)
            .limit(1);
        if live {
            q.and_where(Expr::col("deleted").eq(0_i64));
        }
        self.0.one(&q, |r| r.get(0))
    }

    pub fn live(&mut self, ws: &str, id: &str) -> Result<Option<ScheduleRow>> {
        self.live_schedule(ws, id, false)
    }

    pub fn live_schedule(
        &mut self,
        ws: &str,
        id: &str,
        deadline: bool,
    ) -> Result<Option<ScheduleRow>> {
        self.0.one(
            projection(&mut scope(ws), deadline)
                .and_where(Expr::col("id").eq(id))
                .and_where(Expr::col("deleted").eq(0_i64)),
            |r| decode(r, deadline),
        )
    }

    pub fn get(&mut self, id: &str) -> Result<Option<ScheduleRow>> {
        self.0.one(
            projection(&mut schedules(), true).and_where(Expr::col("id").eq(id)),
            |r| decode(r, true),
        )
    }

    pub fn list(&mut self, ws: &str) -> Result<Vec<ScheduleRow>> {
        self.0.all(
            projection(&mut scope(ws), true)
                .and_where(Expr::col("deleted").eq(0_i64))
                .order_by("created", Order::Asc),
            |r| decode(r, true),
        )
    }

    pub fn version(&mut self, ws: &str, id: &str) -> Result<Option<i64>> {
        self.0.one(
            scope(ws)
                .column("version")
                .and_where(Expr::col("id").eq(id)),
            |r| r.get(0),
        )
    }
}
