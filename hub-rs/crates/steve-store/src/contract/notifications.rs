//! Personal notification consent, inbox and delivery receipts.

use sea_query::{Expr, ExprTrait, Func, JoinType, OnConflict, Order, Query, SelectStatement};
use serde::Serialize;

use crate::contract::Q;
use crate::{Read, Result, W};

pub struct Policy {
    pub enabled: bool,
    pub mode: String,
    pub party: Option<String>,
    pub space: Option<String>,
    pub timezone: String,
    pub quiet_start: Option<i64>,
    pub quiet_end: Option<i64>,
    pub starred_in_quiet: bool,
}

fn policies(ws: &str, user: &str) -> SelectStatement {
    Query::select()
        .from("notification_policies")
        .and_where(Expr::col("workspace_id").eq(ws))
        .and_where(Expr::col("user_id").eq(user))
        .take()
}

pub fn policy(r: &mut impl Read, ws: &str, user: &str) -> Result<Option<Policy>> {
    r.one(
        policies(ws, user).columns([
            "enabled",
            "mode",
            "party_id",
            "space_id",
            "timezone",
            "quiet_start",
            "quiet_end",
            "starred_in_quiet",
        ]),
        |row| {
            Ok(Policy {
                enabled: row.get::<i64>(0)? != 0,
                mode: row.get(1)?,
                party: row.get(2)?,
                space: row.get(3)?,
                timezone: row.get(4)?,
                quiet_start: row.get(5)?,
                quiet_end: row.get(6)?,
                starred_in_quiet: row.get::<i64>(7)? != 0,
            })
        },
    )
}

pub fn set_policy(w: &mut W<'_>, ws: &str, user: &str, policy: &Policy, now: i64) -> Result<()> {
    let previous = w.one(
        policies(ws, user).columns(["enabled", "consented_at", "consent_since_seq"]),
        |row| {
            Ok((
                row.get::<i64>(0)? != 0,
                row.get::<i64>(1)?,
                row.get::<i64>(2)?,
            ))
        },
    )?;
    let (consented_at, consent_since_seq) = match previous {
        Some((enabled, at, seq)) if enabled || !policy.enabled => (at, seq),
        _ => (now, crate::outbox::latest_sequence(w)?),
    };
    w.exec(
        Query::insert()
            .into_table("notification_policies")
            .columns([
                "workspace_id",
                "user_id",
                "enabled",
                "mode",
                "party_id",
                "space_id",
                "timezone",
                "quiet_start",
                "quiet_end",
                "starred_in_quiet",
                "consented_at",
                "consent_since_seq",
                "updated",
            ])
            .values_panic([
                ws.into(),
                user.into(),
                (i64::from(policy.enabled)).into(),
                policy.mode.as_str().into(),
                policy.party.as_deref().into(),
                policy.space.as_deref().into(),
                policy.timezone.as_str().into(),
                policy.quiet_start.into(),
                policy.quiet_end.into(),
                (i64::from(policy.starred_in_quiet)).into(),
                consented_at.into(),
                consent_since_seq.into(),
                now.into(),
            ])
            .on_conflict(
                OnConflict::columns(["workspace_id", "user_id"])
                    .update_columns([
                        "enabled",
                        "mode",
                        "party_id",
                        "space_id",
                        "timezone",
                        "quiet_start",
                        "quiet_end",
                        "starred_in_quiet",
                        "consented_at",
                        "consent_since_seq",
                        "updated",
                    ])
                    .to_owned(),
            ),
    )?;
    Ok(())
}

pub fn recipients(r: &mut impl Read, ws: &str, created: i64, sequence: i64) -> Result<Vec<String>> {
    r.all(
        Query::select()
            .column(("p", "user_id"))
            .from_as("notification_policies", "p")
            .join_as(
                JoinType::InnerJoin,
                "workspace_members",
                "m",
                Expr::col(("m", "workspace_id"))
                    .equals(("p", "workspace_id"))
                    .and(Expr::col(("m", "user_id")).equals(("p", "user_id"))),
            )
            .and_where(Expr::col(("p", "workspace_id")).eq(ws))
            .and_where(Expr::col(("p", "enabled")).eq(1i64))
            .and_where(Expr::col(("p", "user_id")).not_like("svc:%"))
            .and_where(Expr::col(("p", "consented_at")).lte(created))
            .and_where(Expr::col(("p", "consent_since_seq")).lt(sequence)),
        |row| row.get(0),
    )
}

pub struct NewNotice<'a> {
    pub ws: &'a str,
    pub user: &'a str,
    pub event: &'a str,
    pub key: &'a str,
    pub mode: &'a str,
    pub body: &'a str,
    pub state: &'a str,
    pub topic: &'a str,
    pub message: Option<&'a str>,
    pub created: i64,
}

pub fn insert(w: &mut W<'_>, notice: &NewNotice<'_>) -> Result<Option<String>> {
    let id = crate::uid("ntf");
    Ok((w.exec(
        Query::insert()
            .into_table("personal_notifications")
            .columns([
                "id",
                "workspace_id",
                "user_id",
                "event_id",
                "delivery_key",
                "mode",
                "body",
                "state",
                "topic",
                "message_id",
                "created",
            ])
            .values_panic([
                id.as_str().into(),
                notice.ws.into(),
                notice.user.into(),
                notice.event.into(),
                notice.key.into(),
                notice.mode.into(),
                notice.body.into(),
                notice.state.into(),
                notice.topic.into(),
                notice.message.into(),
                notice.created.into(),
            ])
            .on_conflict(
                OnConflict::columns(["workspace_id", "user_id", "event_id"])
                    .do_nothing()
                    .to_owned(),
            ),
    )? != 0)
        .then_some(id))
}

pub struct Pending {
    pub body: Option<String>,
    pub state: String,
    pub topic: String,
    pub message: Option<String>,
}

pub fn pending(r: &mut impl Read, ws: &str, user: &str, key: &str) -> Result<Option<Pending>> {
    r.one(
        Query::select()
            .columns(["body", "state", "topic", "message_id"])
            .from("personal_notifications")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("user_id").eq(user))
            .and_where(Expr::col("delivery_key").eq(key)),
        |row| {
            Ok(Pending {
                body: row.get(0)?,
                state: row.get(1)?,
                topic: row.get(2)?,
                message: row.get(3)?,
            })
        },
    )
}

pub fn attach(w: &mut W<'_>, ws: &str, user: &str, key: &str, delivery: &str) -> Result<()> {
    w.exec(
        Query::update()
            .table("personal_notifications")
            .value("delivery_id", delivery)
            .value("state", "submitted")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("user_id").eq(user))
            .and_where(Expr::col("delivery_key").eq(key)),
    )?;
    Ok(())
}

pub struct Route {
    pub workspace: String,
    pub user: String,
    pub key: String,
    pub topic: String,
    pub message: Option<String>,
}

pub fn by_output(r: &mut impl Read, output: &str) -> Result<Option<Route>> {
    r.one(
        Query::select()
            .columns([
                "workspace_id",
                "user_id",
                "delivery_key",
                "topic",
                "message_id",
            ])
            .from("personal_notifications")
            .and_where(
                Expr::col("delivery_id")
                    .in_subquery(super::channel_messages::outputs::for_message(output)),
            ),
        |row| {
            Ok(Route {
                workspace: row.get(0)?,
                user: row.get(1)?,
                key: row.get(2)?,
                topic: row.get(3)?,
                message: row.get(4)?,
            })
        },
    )
}

pub fn cancel(w: &mut W<'_>, ws: &str, key: &str) -> Result<bool> {
    Ok(w.exec(
        Query::update()
            .table("personal_notifications")
            .value("state", "cancelled")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("delivery_key").eq(key))
            .and_where(Expr::col("state").is_in(["waiting", "held", "submitted"])),
    )? != 0)
}

pub fn update_for_policy(
    w: &mut W<'_>,
    ws: &str,
    user: &str,
    policy: &Policy,
    starred_topic: &str,
) -> Result<()> {
    if !policy.enabled {
        w.exec(
            Query::update()
                .table("personal_notifications")
                .value("state", "cancelled")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("user_id").eq(user))
                .and_where(Expr::col("state").is_in(["waiting", "held", "submitted"])),
        )?;
        return Ok(());
    }
    if policy.mode == "now" {
        return Ok(());
    }
    let mut rows = Query::update();
    rows.table("personal_notifications")
        .value("state", "held")
        .value("mode", policy.mode.as_str())
        .and_where(Expr::col("workspace_id").eq(ws))
        .and_where(Expr::col("user_id").eq(user))
        .and_where(Expr::col("state").eq("waiting"));
    if policy.mode == "digest" && policy.space.is_some() {
        rows.and_where(Expr::col("topic").ne(starred_topic));
    }
    w.exec(&rows)?;
    Ok(())
}

#[derive(Serialize)]
pub struct Notice {
    pub id: String,
    pub event_id: String,
    pub mode: String,
    pub body: Option<String>,
    pub state: String,
    pub delivery_id: Option<String>,
    pub seen_at: Option<i64>,
    pub created: i64,
}

pub fn list(r: &mut impl Read, ws: &str, user: &str) -> Result<Vec<Notice>> {
    r.all(
        Query::select()
            .from_as("personal_notifications", "n")
            .columns([
                ("n", "id"),
                ("n", "event_id"),
                ("n", "mode"),
                ("n", "body"),
                ("n", "state"),
                ("n", "delivery_id"),
                ("r", "read_at"),
                ("n", "created"),
            ])
            .join_as(
                JoinType::InnerJoin,
                "feed_entries",
                "e",
                Expr::col(("e", "notification_id")).equals(("n", "id")),
            )
            .join_as(
                JoinType::LeftJoin,
                "feed_reads",
                "r",
                Expr::col(("r", "entry_id"))
                    .equals(("e", "id"))
                    .and(Expr::col(("r", "workspace_id")).equals(("n", "workspace_id")))
                    .and(Expr::col(("r", "user_id")).equals(("n", "user_id"))),
            )
            .and_where(Expr::col(("n", "workspace_id")).eq(ws))
            .and_where(Expr::col(("n", "user_id")).eq(user))
            .order_by_columns([(("n", "created"), Order::Desc), (("n", "id"), Order::Desc)])
            .limit(100),
        |row| {
            Ok(Notice {
                id: row.get(0)?,
                event_id: row.get(1)?,
                mode: row.get(2)?,
                body: row.get(3)?,
                state: row.get(4)?,
                delivery_id: row.get(5)?,
                seen_at: row.get(6)?,
                created: row.get(7)?,
            })
        },
    )
}

pub struct Due {
    pub workspace: String,
    pub user: String,
    pub key: String,
    pub body: String,
    pub topic: String,
    pub message: Option<String>,
}

fn waiting() -> SelectStatement {
    Query::select()
        .from("personal_notifications")
        .and_where(Expr::col("state").eq("waiting"))
        .and_where(Expr::col("delivery_id").is_null())
        .and_where(Expr::col("body").is_not_null())
        .take()
}

pub fn due(r: &mut impl Read, now: i64, since: i64) -> Result<Vec<Due>> {
    r.all(
        waiting()
            .columns([
                "workspace_id",
                "user_id",
                "delivery_key",
                "body",
                "topic",
                "message_id",
            ])
            .and_where(Expr::col("next_attempt_at").lte(now))
            .and_where(Expr::col("created").gte(since))
            .order_by_columns([("next_attempt_at", Order::Asc), ("created", Order::Asc)])
            .limit(50),
        |row| {
            Ok(Due {
                workspace: row.get(0)?,
                user: row.get(1)?,
                key: row.get(2)?,
                body: row.get(3)?,
                topic: row.get(4)?,
                message: row.get(5)?,
            })
        },
    )
}

pub fn next_due(r: &mut impl Read, keep_ms: i64) -> Result<Option<i64>> {
    let attempt: Option<i64> = r
        .one(
            waiting().expr(Func::min(Expr::col("next_attempt_at"))),
            |row| row.get(0),
        )?
        .flatten();
    let created: Option<i64> = r
        .one(
            Query::select()
                .expr(Func::min(Expr::col("created")))
                .from("personal_notifications")
                .and_where(Expr::col("body").is_not_null()),
            |row| row.get(0),
        )?
        .flatten();
    Ok(attempt
        .into_iter()
        .chain(created.map(|at| at + keep_ms + 1))
        .min())
}

pub enum WaitingChange {
    RetryAt(i64),
    Cancel,
}

pub fn update_waiting(w: &mut W<'_>, key: &str, change: WaitingChange) -> Result<()> {
    let mut rows = Query::update();
    rows.table("personal_notifications")
        .and_where(Expr::col("delivery_key").eq(key))
        .and_where(Expr::col("state").eq("waiting"));
    match change {
        WaitingChange::RetryAt(at) => rows.value("next_attempt_at", at),
        WaitingChange::Cancel => rows.value("state", "cancelled"),
    };
    w.exec(&rows)?;
    Ok(())
}

pub fn prune(w: &mut W<'_>, before: i64) -> Result<()> {
    let expired = Query::select()
        .column("id")
        .from("personal_notifications")
        .and_where(Expr::col("created").lt(before))
        .and_where(Expr::col("body").is_not_null())
        .limit(1_000)
        .take();
    w.exec(
        Query::update()
            .table("personal_notifications")
            .value("body", Option::<String>::None)
            .value("state", "purged")
            .and_where(Expr::col("id").in_subquery(expired)),
    )?;
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn recent_starred(
    r: &mut impl Read,
    ws: &str,
    user: &str,
    space: &str,
    sender: Option<&str>,
    since: i64,
    topic: &str,
) -> Result<Option<i64>> {
    let messages = super::channel_messages::ingress::space_sender_messages(r, space, sender);
    Ok(r.one(
        Query::select()
            .expr(Func::max(Expr::col("created")))
            .from("personal_notifications")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("user_id").eq(user))
            .and_where(Expr::col("topic").eq(topic))
            .and_where(Expr::col("state").ne("cancelled"))
            .and_where(Expr::col("created").gt(since))
            .and_where(Expr::col("message_id").in_subquery(messages)),
        |row| row.get(0),
    )?
    .flatten())
}

pub fn postpone_delivery(w: &mut W<'_>, message: &str, until: i64) -> Result<()> {
    let delivery = Query::select()
        .column("id")
        .from("output_deliveries")
        .and_where(Expr::col("message_id").eq(message))
        .take();
    w.exec(
        Query::update()
            .table("personal_notifications")
            .value("next_attempt_at", until)
            .and_where(Expr::col("delivery_id").in_subquery(delivery)),
    )?;
    Ok(())
}
