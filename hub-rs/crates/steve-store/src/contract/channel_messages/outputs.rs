//! Delivery receipts and the channel preferences learned from a reply.

use sea_query::{Expr, ExprTrait, Func, JoinType, OnConflict, Query, SelectStatement};

use crate::{Read, Result, W};

pub struct RepliedDelivery {
    pub id: String,
    pub requester: String,
}

pub fn replied_deliveries(
    r: &mut impl Read,
    ws: &str,
    space: &str,
    reply: &str,
) -> Result<Vec<RepliedDelivery>> {
    r.all(
        Query::select()
            .from_as("output_deliveries", "d")
            .columns([("d", "id"), ("d", "requester")])
            .join_as(
                JoinType::InnerJoin,
                "chan_messages",
                "m",
                Expr::col(("m", "id")).equals(("d", "message_id")),
            )
            .join_as(
                JoinType::InnerJoin,
                "workspace_members",
                "w",
                Expr::col(("w", "workspace_id"))
                    .equals(("d", "workspace_id"))
                    .and(Expr::col(("w", "user_id")).equals(("d", "requester"))),
            )
            .and_where(Expr::col(("d", "workspace_id")).eq(ws))
            .and_where(Expr::col(("d", "space_id")).eq(space))
            .and_where(Expr::col(("m", "external_id")).eq(reply))
            .and_where(Expr::col(("m", "status")).eq("sent")),
        |row| {
            Ok(RepliedDelivery {
                id: row.get(0)?,
                requester: row.get(1)?,
            })
        },
    )
}

pub fn record_reply(w: &mut W<'_>, delivery: &str, message: &str, now: i64) -> Result<()> {
    w.exec(
        Query::update()
            .table("output_deliveries")
            .value(
                "replied_at",
                Func::coalesce([Expr::col("replied_at"), Expr::val(now)]),
            )
            .value(
                "reply_message_id",
                Func::coalesce([Expr::col("reply_message_id"), Expr::val(message)]),
            )
            .and_where(Expr::col("id").eq(delivery)),
    )?;
    Ok(())
}

pub fn learn_route(
    w: &mut W<'_>,
    ws: &str,
    user: &str,
    party: &str,
    space: &str,
    now: i64,
) -> Result<()> {
    w.exec(
        Query::insert()
            .into_table("output_channel_preferences")
            .columns([
                "workspace_id",
                "user_id",
                "party_id",
                "learned_space_id",
                "updated",
            ])
            .values_panic([
                ws.into(),
                user.into(),
                party.into(),
                space.into(),
                now.into(),
            ])
            .on_conflict(
                OnConflict::columns(["workspace_id", "user_id", "party_id"])
                    .update_columns(["learned_space_id", "updated"])
                    .to_owned(),
            ),
    )?;
    Ok(())
}

pub(in crate::contract) fn for_message(message: &str) -> SelectStatement {
    Query::select()
        .column("id")
        .from("output_deliveries")
        .and_where(Expr::col("message_id").eq(message))
        .take()
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct Delivery {
    pub id: String,
    pub message_id: String,
    pub space_id: String,
    pub connection_id: String,
    pub status: String,
    pub decision_id: Option<String>,
    pub attempts: i64,
    pub last_error: Option<String>,
    pub replied_at: Option<i64>,
    pub reply_message_id: Option<String>,
}

fn deliveries(ws: &str, user: &str) -> SelectStatement {
    Query::select()
        .from_as("output_deliveries", "d")
        .join_as(
            JoinType::InnerJoin,
            "chan_messages",
            "m",
            Expr::col(("m", "id")).equals(("d", "message_id")),
        )
        .columns([
            ("d", "fingerprint"),
            ("d", "id"),
            ("d", "message_id"),
            ("d", "space_id"),
            ("d", "connection_id"),
            ("m", "status"),
            ("m", "decision_id"),
            ("d", "attempts"),
            ("d", "last_error"),
            ("d", "replied_at"),
            ("d", "reply_message_id"),
        ])
        .and_where(Expr::col(("d", "workspace_id")).eq(ws))
        .and_where(Expr::col(("d", "requester")).eq(user))
        .take()
}

fn decode(row: &crate::contract::Row<'_>) -> Result<(String, Delivery)> {
    Ok((
        row.get(0)?,
        Delivery {
            id: row.get(1)?,
            message_id: row.get(2)?,
            space_id: row.get(3)?,
            connection_id: row.get(4)?,
            status: row.get(5)?,
            decision_id: row.get(6)?,
            attempts: row.get(7)?,
            last_error: row.get(8)?,
            replied_at: row.get(9)?,
            reply_message_id: row.get(10)?,
        },
    ))
}

pub fn by_key(
    r: &mut impl Read,
    ws: &str,
    user: &str,
    key: &str,
) -> Result<Option<(String, Delivery)>> {
    r.one(
        deliveries(ws, user).and_where(Expr::col(("d", "idempotency_key")).eq(key)),
        decode,
    )
}

pub fn get(r: &mut impl Read, ws: &str, user: &str, id: &str) -> Result<Option<Delivery>> {
    Ok(r.one(
        deliveries(ws, user).and_where(Expr::col(("d", "id")).eq(id)),
        decode,
    )?
    .map(|(_, delivery)| delivery))
}

pub struct StoredOutput {
    pub ws: String,
    pub requester: String,
    pub connection: String,
    pub space: String,
    pub intent: serde_json::Value,
}

pub fn by_message(r: &mut impl Read, message: &str) -> Result<Option<StoredOutput>> {
    r.one(
        Query::select()
            .columns([
                "workspace_id",
                "requester",
                "connection_id",
                "space_id",
                "intent",
            ])
            .from("output_deliveries")
            .and_where(Expr::col("message_id").eq(message)),
        |row| {
            Ok(StoredOutput {
                ws: row.get(0)?,
                requester: row.get(1)?,
                connection: row.get(2)?,
                space: row.get(3)?,
                intent: row.get::<crate::contract::Json<_>>(4)?.0,
            })
        },
    )
}

pub fn recorded_identity(
    r: &mut impl Read,
    ws: &str,
    user: &str,
    key: &str,
) -> Result<Option<String>> {
    let dialect = r.db().dialect();
    r.one(
        Query::select()
            .expr(dialect.json_field(Expr::col("intent"), "identity"))
            .from("output_deliveries")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("requester").eq(user))
            .and_where(Expr::col("idempotency_key").eq(key)),
        |row| row.get(0),
    )
}

pub struct NewDelivery<'a> {
    pub ws: &'a str,
    pub requester: &'a str,
    pub key: &'a str,
    pub fingerprint: &'a str,
    pub intent: &'a serde_json::Value,
    pub connection: &'a str,
    pub space: &'a str,
    pub source: Option<&'a str>,
    pub message: &'a str,
    pub created: i64,
}

pub fn insert(w: &mut W<'_>, delivery: &NewDelivery<'_>) -> Result<String> {
    let id = crate::uid("od");
    w.exec(
        Query::insert()
            .into_table("output_deliveries")
            .columns([
                "id",
                "workspace_id",
                "requester",
                "idempotency_key",
                "fingerprint",
                "intent",
                "connection_id",
                "space_id",
                "source_message_id",
                "message_id",
                "created",
            ])
            .values_panic([
                id.as_str().into(),
                delivery.ws.into(),
                delivery.requester.into(),
                delivery.key.into(),
                delivery.fingerprint.into(),
                serde_json::to_string(delivery.intent)?.into(),
                delivery.connection.into(),
                delivery.space.into(),
                delivery.source.into(),
                delivery.message.into(),
                delivery.created.into(),
            ]),
    )?;
    Ok(id)
}

pub struct Preference {
    pub manual: Option<String>,
    pub learned: Option<String>,
}

pub fn route_preference(
    r: &mut impl Read,
    ws: &str,
    user: &str,
    party: &str,
) -> Result<Option<Preference>> {
    r.one(
        Query::select()
            .columns(["manual_space_id", "learned_space_id"])
            .from("output_channel_preferences")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("user_id").eq(user))
            .and_where(Expr::col("party_id").eq(party)),
        |row| {
            Ok(Preference {
                manual: row.get(0)?,
                learned: row.get(1)?,
            })
        },
    )
}

pub fn set_manual(
    w: &mut W<'_>,
    ws: &str,
    user: &str,
    party: &str,
    space: Option<&str>,
    now: i64,
) -> Result<()> {
    w.exec(
        Query::insert()
            .into_table("output_channel_preferences")
            .columns([
                "workspace_id",
                "user_id",
                "party_id",
                "manual_space_id",
                "updated",
            ])
            .values_panic([
                ws.into(),
                user.into(),
                party.into(),
                space.into(),
                now.into(),
            ])
            .on_conflict(
                OnConflict::columns(["workspace_id", "user_id", "party_id"])
                    .update_columns(["manual_space_id", "updated"])
                    .to_owned(),
            ),
    )?;
    Ok(())
}

pub struct Source {
    pub space: String,
    pub external: Option<String>,
    pub thread: Option<String>,
}

pub fn source(r: &mut impl Read, ws: &str, message: &str) -> Result<Option<Source>> {
    r.one(
        Query::select()
            .columns(["space_id", "external_id", "thread_key"])
            .from("chan_messages")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("id").eq(message))
            .and_where(Expr::col("direction").eq("in"))
            .and_where(Expr::col("purged").is_null()),
        |row| {
            Ok(Source {
                space: row.get(0)?,
                external: row.get(1)?,
                thread: row.get(2)?,
            })
        },
    )
}

pub fn rebind_message(w: &mut W<'_>, from: &str, to: &str) -> Result<()> {
    w.exec(
        Query::update()
            .table("output_deliveries")
            .value("message_id", to)
            .and_where(Expr::col("message_id").eq(from)),
    )?;
    Ok(())
}
