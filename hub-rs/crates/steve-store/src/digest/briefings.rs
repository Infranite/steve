use sea_query::{Expr, ExprTrait, Func, Query};
use serde_json::Value;

use crate::contract::Json;
use crate::{Read, Result, W};

pub struct Briefings<'a, C>(pub &'a mut C);

pub struct Briefing {
    pub material: Value,
    pub pushed: bool,
    pub created: i64,
}

pub struct NewBriefing<'a> {
    pub ws: &'a str,
    pub owner: &'a str,
    pub day: &'a str,
    pub schedule: &'a str,
    pub event: &'a str,
    pub material: &'a Value,
    pub pushed: bool,
    pub created: i64,
}

fn owned(ws: &str, user: &str) -> Expr {
    Expr::col("workspace_id")
        .eq(ws)
        .and(Expr::col("user_id").eq(user))
}

impl<C: Read> Briefings<'_, C> {
    pub fn previous_at(&mut self, ws: &str, user: &str, before: &str) -> Result<Option<i64>> {
        Ok(self
            .0
            .one(
                Query::select()
                    .expr(Func::max(Expr::col("created")))
                    .from("briefings")
                    .and_where(owned(ws, user))
                    .and_where(Expr::col("day").lt(before)),
                |r| r.get::<Option<i64>>(0),
            )?
            .flatten())
    }

    pub fn get(&mut self, ws: &str, user: &str, day: &str) -> Result<Option<Briefing>> {
        self.0.one(
            Query::select()
                .columns(["material", "pushed", "created"])
                .from("briefings")
                .and_where(owned(ws, user))
                .and_where(Expr::col("day").eq(day)),
            |r| {
                Ok(Briefing {
                    material: r.get::<Json<_>>(0)?.0,
                    pushed: r.get(1)?,
                    created: r.get(2)?,
                })
            },
        )
    }
}

impl Briefings<'_, W<'_>> {
    pub fn insert(&mut self, b: NewBriefing<'_>) -> Result<()> {
        self.0.exec(
            Query::insert()
                .into_table("briefings")
                .columns([
                    "id",
                    "workspace_id",
                    "user_id",
                    "day",
                    "schedule_id",
                    "event_id",
                    "material",
                    "pushed",
                    "created",
                ])
                .values_panic([
                    crate::uid("brf").into(),
                    b.ws.into(),
                    b.owner.into(),
                    b.day.into(),
                    b.schedule.into(),
                    b.event.into(),
                    b.material.to_string().into(),
                    i64::from(b.pushed).into(),
                    b.created.into(),
                ]),
        )?;
        Ok(())
    }

    pub fn prune(&mut self, ws: &str, user: &str, before: i64) -> Result<()> {
        self.0.exec(
            Query::delete()
                .from_table("briefings")
                .and_where(owned(ws, user))
                .and_where(Expr::col("created").lt(before)),
        )?;
        Ok(())
    }
}
