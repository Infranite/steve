use sea_query::{Cond, Expr, ExprTrait, Func, OnConflict, Order, Query, SelectStatement};
use serde::Serialize;

use super::Row;
use crate::{Read, Result, W, uid};

pub struct Alarms<'a, C>(pub &'a mut C);

#[derive(Serialize)]
pub struct Alarm {
    pub id: String,
    #[serde(skip)]
    pub ws: String,
    pub owner: String,
    pub kind: String,
    #[serde(skip)]
    pub subject: String,
    pub planned_for: i64,
    pub fire_at: i64,
    pub stale_at: Option<i64>,
    pub hold_until: Option<i64>,
    pub state: String,
    pub run_ref: Option<String>,
}

fn alarms() -> SelectStatement {
    Query::select()
        .columns([
            "id",
            "workspace_id",
            "owner_user",
            "kind",
            "subject",
            "planned_for",
            "fire_at",
            "stale_at",
            "hold_until",
            "state",
            "run_ref",
        ])
        .from("time_alarms")
        .take()
}

fn alarm(row: &Row<'_>) -> Result<Alarm> {
    Ok(Alarm {
        id: row.get(0)?,
        ws: row.get(1)?,
        owner: row.get(2)?,
        kind: row.get(3)?,
        subject: row.get(4)?,
        planned_for: row.get(5)?,
        fire_at: row.get(6)?,
        stale_at: row.get(7)?,
        hold_until: row.get(8)?,
        state: row.get(9)?,
        run_ref: row.get(10)?,
    })
}

fn planned() -> Expr {
    Expr::col("state").eq("planned")
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
                    .and_where(planned()),
                |r| r.get::<Option<i64>>(0),
            )?
            .flatten())
    }

    pub fn of_subject(&mut self, ws: &str, subject: &str) -> Result<Vec<Alarm>> {
        self.0.all(
            alarms()
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("subject").eq(subject))
                .order_by_columns([("planned_for", Order::Asc), ("kind", Order::Asc)]),
            alarm,
        )
    }

    pub fn planned(&mut self, id: &str) -> Result<Option<Alarm>> {
        self.0.one(
            alarms()
                .and_where(Expr::col("id").eq(id))
                .and_where(planned()),
            alarm,
        )
    }

    pub fn due(&mut self, now: i64, limit: u64) -> Result<Vec<(String, String)>> {
        let invalid = self
            .0
            .db()
            .dialect()
            .invalid_integer_columns("fire_at", &["hold_until", "stale_at"]);
        self.0.all(
            Query::select()
                .columns(["id", "workspace_id"])
                .from("time_alarms")
                .and_where(planned())
                .cond_where(
                    Cond::any().add(invalid).add(
                        Cond::all().add(Expr::col("fire_at").lte(now)).add(
                            Cond::any()
                                .add(Expr::col("hold_until").is_null())
                                .add(Expr::col("hold_until").lte(now))
                                .add(Expr::col("stale_at").lte(now)),
                        ),
                    ),
                )
                .order_by_columns([("fire_at", Order::Asc), ("id", Order::Asc)])
                .limit(limit),
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
    }

    pub fn deadline(&mut self, now: i64) -> Result<Option<i64>> {
        let dialect = self.0.db().dialect();
        let invalid = dialect.invalid_integer_columns("fire_at", &["hold_until", "stale_at"]);
        let has_invalid = self
            .0
            .one(
                Query::select()
                    .column("id")
                    .from("time_alarms")
                    .and_where(planned())
                    .cond_where(invalid.clone())
                    .limit(1),
                |_| Ok(()),
            )?
            .is_some();
        let fire = Expr::col("fire_at");
        let held = Func::coalesce([Expr::col("hold_until"), fire.clone()]);
        let stale = Func::coalesce([Expr::col("stale_at"), Expr::Constant(i64::MAX.into())]);
        let until = dialect.least(held.into(), stale.into());
        let deadline = self
            .0
            .one(
                Query::select()
                    .expr(Func::min(dialect.greatest(fire, until)))
                    .from("time_alarms")
                    .and_where(planned())
                    .cond_where(invalid.not()),
                |row| row.get::<Option<i64>>(0),
            )?
            .flatten();
        Ok(if has_invalid {
            Some(deadline.map_or(now, |at| std::cmp::min(at, now)))
        } else {
            deadline
        })
    }

    pub fn dirty(&mut self, limit: u64) -> Result<Vec<(String, String)>> {
        let order = self.0.db().dialect().insertion_order(None);
        self.0.all(
            Query::select()
                .columns(["workspace_id", "subject"])
                .from("time_alarm_dirty")
                .order_by_expr(order, Order::Asc)
                .limit(limit),
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
    }

    pub fn needs_plan(&mut self, ws: &str, subject: &str) -> Result<bool> {
        self.0
            .one(
                Query::select()
                    .column("subject")
                    .from("time_alarm_dirty")
                    .and_where(Expr::col("workspace_id").eq(ws))
                    .and_where(Expr::col("subject").eq(subject)),
                |_| Ok(()),
            )
            .map(|row| row.is_some())
    }

    pub fn held_owners(&mut self) -> Result<Vec<(String, String, i64)>> {
        self.0.all(
            Query::select()
                .columns(["workspace_id", "owner_user"])
                .expr(Func::min(Expr::col("updated")))
                .from("time_alarms")
                .and_where(planned())
                .and_where(Expr::col("hold_until").is_not_null())
                .group_by_columns(["workspace_id", "owner_user"]),
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
    }
}

pub struct NewAlarm<'a> {
    pub ws: &'a str,
    pub owner: &'a str,
    pub kind: &'a str,
    pub subject: &'a str,
    pub planned_for: i64,
    pub fire_at: i64,
    pub stale_at: Option<i64>,
    pub now: i64,
}

impl Alarms<'_, W<'_>> {
    pub fn add(&mut self, alarm: &NewAlarm<'_>) -> Result<()> {
        self.0.exec(
            Query::insert()
                .into_table("time_alarms")
                .columns([
                    "id",
                    "workspace_id",
                    "owner_user",
                    "kind",
                    "subject",
                    "planned_for",
                    "fire_at",
                    "stale_at",
                    "created",
                    "updated",
                ])
                .values_panic([
                    uid("alm").into(),
                    alarm.ws.into(),
                    alarm.owner.into(),
                    alarm.kind.into(),
                    alarm.subject.into(),
                    alarm.planned_for.into(),
                    alarm.fire_at.into(),
                    alarm.stale_at.into(),
                    alarm.now.into(),
                    alarm.now.into(),
                ]),
        )?;
        Ok(())
    }

    pub fn plan(
        &mut self,
        id: &str,
        fire_at: i64,
        stale_at: Option<i64>,
        hold_until: Option<i64>,
        now: i64,
    ) -> Result<()> {
        self.0.exec(
            Query::update()
                .table("time_alarms")
                .value("state", "planned")
                .value("fire_at", fire_at)
                .value("stale_at", stale_at)
                .value("hold_until", hold_until)
                .value("updated", now)
                .and_where(Expr::col("id").eq(id)),
        )?;
        Ok(())
    }

    pub fn cancel(&mut self, id: &str, now: i64) -> Result<bool> {
        Ok(self.0.exec(
            Query::update()
                .table("time_alarms")
                .value("state", "cancelled")
                .value("hold_until", Option::<i64>::None)
                .value("updated", now)
                .and_where(planned())
                .and_where(Expr::col("id").eq(id)),
        )? == 1)
    }

    pub fn cancel_subject(&mut self, ws: &str, subject: &str, now: i64) -> Result<()> {
        self.0.exec(
            Query::update()
                .table("time_alarms")
                .value("state", "cancelled")
                .value("hold_until", Option::<i64>::None)
                .value("updated", now)
                .and_where(planned())
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("subject").eq(subject)),
        )?;
        Ok(())
    }

    pub fn stale(&mut self, id: &str, now: i64) -> Result<()> {
        self.0.exec(
            Query::update()
                .table("time_alarms")
                .value("state", "stale")
                .value("hold_until", Option::<i64>::None)
                .value("updated", now)
                .and_where(planned())
                .and_where(Expr::col("id").eq(id)),
        )?;
        Ok(())
    }

    pub fn hold(&mut self, id: &str, until: i64, now: i64) -> Result<()> {
        self.0.exec(
            Query::update()
                .table("time_alarms")
                .value("hold_until", until)
                .value("updated", now)
                .and_where(planned())
                .and_where(Expr::col("id").eq(id)),
        )?;
        Ok(())
    }

    pub fn fire(&mut self, id: &str, event: &str, now: i64) -> Result<bool> {
        Ok(self.0.exec(
            Query::update()
                .table("time_alarms")
                .value("state", "fired")
                .value("fired_at", now)
                .value("run_ref", event)
                .value("hold_until", Option::<i64>::None)
                .value("updated", now)
                .and_where(planned())
                .and_where(Expr::col("id").eq(id)),
        )? == 1)
    }

    pub fn clear_holds(&mut self, owner: Option<(&str, &str, i64)>) -> Result<()> {
        let mut stmt = Query::update();
        stmt.table("time_alarms")
            .value("hold_until", Option::<i64>::None)
            .and_where(planned())
            .and_where(Expr::col("hold_until").is_not_null());
        if let Some((ws, user, until)) = owner {
            stmt.and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("owner_user").eq(user))
                .and_where(Expr::col("updated").lt(until));
        }
        self.0.exec(&stmt)?;
        Ok(())
    }

    pub fn mark(&mut self, ws: &str, subject: &str) -> Result<()> {
        self.0.exec(
            Query::insert()
                .into_table("time_alarm_dirty")
                .columns(["workspace_id", "subject"])
                .values_panic([ws.into(), subject.into()])
                .on_conflict(
                    OnConflict::columns(["workspace_id", "subject"])
                        .do_nothing()
                        .to_owned(),
                ),
        )?;
        Ok(())
    }

    pub fn clear_dirty(&mut self, ws: &str, subject: &str) -> Result<()> {
        self.0.exec(
            Query::delete()
                .from_table("time_alarm_dirty")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("subject").eq(subject)),
        )?;
        Ok(())
    }
}
