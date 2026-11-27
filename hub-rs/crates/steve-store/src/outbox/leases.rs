use sea_query::{Expr, ExprTrait, Func, JoinType, OnConflict, Order, Query, SelectStatement};

use super::{Consumer, Event, Since};
use crate::contract::{Q, Row};
use crate::{Read, Result, StoreError, W};

pub(super) fn register(w: &mut W<'_>, consumer: &dyn Consumer) -> Result<()> {
    let known: Option<String> = w.one(
        Query::select()
            .column("topic")
            .from("outbox_consumers")
            .and_where(Expr::col("id").eq(consumer.id())),
        |row| row.get(0),
    )?;
    match known {
        Some(topic) if topic == consumer.topic() => return Ok(()),
        Some(topic) => {
            return Err(StoreError::Hook(format!(
                "outbox consumer {} is registered for {topic}, not {}",
                consumer.id(),
                consumer.topic()
            )));
        }
        None => {}
    }
    let since = match consumer.since() {
        Since::Start => 0,
        Since::Now => super::latest_sequence(w)?,
    };
    w.exec(
        Query::insert()
            .into_table("outbox_consumers")
            .columns(["id", "topic", "since_seq", "enabled", "registered"])
            .values_panic([
                consumer.id().into(),
                consumer.topic().into(),
                since.into(),
                1_i64.into(),
                crate::now().into(),
            ]),
    )?;
    Ok(())
}

fn due(consumer: &dyn Consumer, now: i64) -> SelectStatement {
    let mut query = Query::select();
    query
        .from_as("outbox_events", "e")
        .join_as(
            JoinType::Join,
            "outbox_consumers",
            "c",
            Expr::col(("c", "id"))
                .eq(consumer.id())
                .and(Expr::col(("c", "enabled")).eq(1_i64)),
        )
        .and_where(Expr::col(("e", "topic")).eq(consumer.topic()))
        .and_where(Expr::col(("e", "seq")).gt(Expr::col(("c", "since_seq"))))
        .and_where(
            Expr::exists(
                Query::select()
                    .column(("r", "event_seq"))
                    .from_as("outbox_receipts", "r")
                    .and_where(Expr::col(("r", "event_seq")).eq(Expr::col(("e", "seq"))))
                    .and_where(Expr::col(("r", "consumer_id")).eq(consumer.id()))
                    .take(),
            )
            .not(),
        )
        .and_where(
            Expr::exists(
                Query::select()
                    .column(("l", "event_seq"))
                    .from_as("outbox_leases", "l")
                    .and_where(Expr::col(("l", "event_seq")).eq(Expr::col(("e", "seq"))))
                    .and_where(Expr::col(("l", "consumer_id")).eq(consumer.id()))
                    .and_where(
                        Expr::col(("l", "dead"))
                            .eq(1_i64)
                            .or(Expr::col(("l", "next_at")).gt(now)),
                    )
                    .take(),
            )
            .not(),
        );
    query
}

pub(super) fn next_due(r: &mut impl Read, consumer: &dyn Consumer) -> Result<Option<i64>> {
    let mut query = Query::select();
    query
        .expr(Func::min(Func::coalesce([
            Expr::col(("l", "next_at")),
            Expr::val(0_i64),
        ])))
        .from_as("outbox_events", "e")
        .join_as(
            JoinType::Join,
            "outbox_consumers",
            "c",
            Expr::col(("c", "id"))
                .eq(consumer.id())
                .and(Expr::col(("c", "enabled")).eq(1_i64)),
        )
        .join_as(
            JoinType::LeftJoin,
            "outbox_leases",
            "l",
            Expr::col(("l", "event_seq"))
                .eq(Expr::col(("e", "seq")))
                .and(Expr::col(("l", "consumer_id")).eq(consumer.id())),
        )
        .and_where(Expr::col(("e", "topic")).eq(consumer.topic()))
        .and_where(Expr::col(("e", "seq")).gt(Expr::col(("c", "since_seq"))))
        .and_where(
            Expr::exists(
                Query::select()
                    .column(("r", "event_seq"))
                    .from_as("outbox_receipts", "r")
                    .and_where(Expr::col(("r", "event_seq")).eq(Expr::col(("e", "seq"))))
                    .and_where(Expr::col(("r", "consumer_id")).eq(consumer.id()))
                    .take(),
            )
            .not(),
        )
        .and_where(
            Expr::col(("l", "event_seq"))
                .is_null()
                .or(Expr::col(("l", "dead")).eq(0_i64)),
        );
    Ok(r.one(&query, |row| row.get::<Option<i64>>(0))?.flatten())
}

pub(super) fn has_due(r: &mut impl Read, consumer: &dyn Consumer, now: i64) -> Result<bool> {
    Ok(
        r.one(due(consumer, now).column(("e", "seq")).limit(1), |row| {
            row.get::<i64>(0)
        })?
        .is_some(),
    )
}

pub(super) fn claim(w: &mut W<'_>, consumer: &dyn Consumer, now: i64) -> Result<Vec<Event>> {
    let events = w.all(
        due(consumer, now)
            .columns([
                ("e", "seq"),
                ("e", "id"),
                ("e", "workspace_id"),
                ("e", "topic"),
                ("e", "version"),
                ("e", "payload"),
                ("e", "created"),
            ])
            .order_by(("e", "seq"), Order::Asc)
            .limit(super::BATCH as u64),
        event,
    )?;
    for ev in &events {
        w.exec(
            Query::insert()
                .into_table("outbox_leases")
                .columns(["event_seq", "consumer_id", "attempts", "updated"])
                .values_panic([
                    ev.seq.into(),
                    consumer.id().into(),
                    1_i64.into(),
                    now.into(),
                ])
                .on_conflict(
                    OnConflict::columns(["event_seq", "consumer_id"])
                        .value(
                            "attempts",
                            Expr::col(("outbox_leases", "attempts")).add(1_i64),
                        )
                        .update_column("updated")
                        .to_owned(),
                ),
        )?;
    }
    Ok(events)
}

fn event(row: &Row<'_>) -> Result<Event> {
    let payload: String = row.get(5)?;
    Ok(Event {
        seq: row.get(0)?,
        id: row.get(1)?,
        workspace_id: row.get(2)?,
        topic: row.get(3)?,
        version: row.get(4)?,
        payload: serde_json::from_str(&payload).unwrap_or(serde_json::Value::Null),
        created: row.get(6)?,
    })
}

pub(super) fn fail(
    w: &mut W<'_>,
    consumer: &str,
    ev: &Event,
    now: i64,
    reject: bool,
    message: &str,
) -> Result<(bool, bool)> {
    let (attempts, since) = w
        .one(
            Query::select()
                .columns(["attempts", "failing_since"])
                .from("outbox_leases")
                .and_where(Expr::col("event_seq").eq(ev.seq))
                .and_where(Expr::col("consumer_id").eq(consumer)),
            |row| Ok((row.get::<i64>(0)?, row.get::<Option<i64>>(1)?)),
        )?
        .unwrap_or((1, None));
    let since = since.unwrap_or(now);
    let dead = reject || now - since >= super::DEAD_AFTER_MS;
    let first = dead
        && w.one(
            Query::select()
                .column(("l", "event_seq"))
                .from_as("outbox_leases", "l")
                .join_as(
                    JoinType::Join,
                    "outbox_events",
                    "e",
                    Expr::col(("e", "seq")).eq(Expr::col(("l", "event_seq"))),
                )
                .and_where(Expr::col(("l", "consumer_id")).eq(consumer))
                .and_where(Expr::col(("l", "dead")).eq(1_i64))
                .and_where(Expr::col(("e", "workspace_id")).eq(ev.workspace_id.as_str()))
                .and_where(Expr::col(("l", "event_seq")).ne(ev.seq))
                .limit(1),
            |row| row.get::<i64>(0),
        )?
        .is_none();
    w.exec(
        Query::insert()
            .into_table("outbox_leases")
            .columns([
                "event_seq",
                "consumer_id",
                "attempts",
                "last_error",
                "next_at",
                "dead",
                "failing_since",
                "updated",
            ])
            .values_panic([
                ev.seq.into(),
                consumer.into(),
                attempts.into(),
                message.into(),
                (now + super::backoff(attempts)).into(),
                i64::from(dead).into(),
                since.into(),
                now.into(),
            ])
            .on_conflict(
                OnConflict::columns(["event_seq", "consumer_id"])
                    .update_columns(["last_error", "next_at", "dead", "failing_since", "updated"])
                    .to_owned(),
            ),
    )?;
    Ok((dead, first))
}

pub(super) fn revive(w: &mut W<'_>, consumer: Option<&str>, now: i64) -> Result<usize> {
    let mut query = Query::update();
    query
        .table("outbox_leases")
        .value("dead", 0_i64)
        .value("attempts", 0_i64)
        .value("next_at", 0_i64)
        .value("failing_since", None::<i64>)
        .value("updated", now)
        .and_where(Expr::col("dead").eq(1_i64));
    if let Some(consumer) = consumer {
        query.and_where(Expr::col("consumer_id").eq(consumer));
    }
    let count = w.exec(&query)?;
    if count > 0 {
        w.outbox_changed();
    }
    usize::try_from(count).map_err(|err| StoreError::Hook(err.to_string()))
}

pub(super) fn unregister(w: &mut W<'_>, id: &str) -> Result<()> {
    for table in ["outbox_leases", "outbox_receipts"] {
        w.exec(
            Query::delete()
                .from_table(table)
                .and_where(Expr::col("consumer_id").eq(id)),
        )?;
    }
    w.exec(
        Query::delete()
            .from_table("outbox_consumers")
            .and_where(Expr::col("id").eq(id)),
    )?;
    Ok(())
}
