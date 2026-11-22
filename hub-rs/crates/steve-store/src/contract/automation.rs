use sea_query::{Expr, ExprTrait, Func, Order, Query, SelectStatement};
use serde_json::Value;

use crate::contract::Json;
use crate::{Read, Result};

pub struct Schedules<'a, C>(pub &'a mut C);
pub struct Alarms<'a, C>(pub &'a mut C);

pub struct BriefSchedule {
    pub id: String,
    pub name: String,
    pub enabled: bool,
    pub version: i64,
    pub spec: Value,
    pub created_by: String,
    pub created: i64,
    pub updated: i64,
}

fn schedules(ws: &str) -> SelectStatement {
    Query::select()
        .from("flows")
        .and_where(Expr::col("workspace_id").eq(ws))
        .and_where(Expr::col("kind").eq("schedule"))
        .take()
}

impl<C: Read> Schedules<'_, C> {
    pub fn brief_id(&mut self, ws: &str, user: &str, live: bool) -> Result<Option<String>> {
        let dialect = self.0.db().dialect();
        let mut q = schedules(ws);
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

    pub fn live_brief(&mut self, ws: &str, id: &str) -> Result<Option<BriefSchedule>> {
        self.0.one(
            schedules(ws)
                .columns([
                    "id",
                    "name",
                    "enabled",
                    "version",
                    "schedule",
                    "created_by",
                    "created",
                    "updated",
                ])
                .and_where(Expr::col("id").eq(id))
                .and_where(Expr::col("deleted").eq(0_i64)),
            |r| {
                Ok(BriefSchedule {
                    id: r.get(0)?,
                    name: r.get(1)?,
                    enabled: r.get(2)?,
                    version: r.get(3)?,
                    spec: r.get::<Json<_>>(4)?.0,
                    created_by: r.get(5)?,
                    created: r.get(6)?,
                    updated: r.get(7)?,
                })
            },
        )
    }

    pub fn version(&mut self, ws: &str, id: &str) -> Result<Option<i64>> {
        self.0.one(
            schedules(ws)
                .column("version")
                .and_where(Expr::col("id").eq(id)),
            |r| r.get(0),
        )
    }
}

impl<C: Read> Alarms<'_, C> {
    pub fn next(&mut self, ws: &str, subject: &str) -> Result<Option<i64>> {
        let fire = Expr::col("fire_at");
        let held = Func::coalesce([Expr::col("hold_until"), fire.clone()]);
        let next = self.0.db().dialect().greatest(fire, held.into());
        Ok(self
            .0
            .one(
                Query::select()
                    .expr(Func::min(next))
                    .from("time_alarms")
                    .and_where(Expr::col("workspace_id").eq(ws))
                    .and_where(Expr::col("subject").eq(subject))
                    .and_where(Expr::col("state").eq("planned")),
                |r| r.get::<Option<i64>>(0),
            )?
            .flatten())
    }
}
