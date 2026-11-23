//! Message excerpts and attachment descriptions used when composing a reply.

use sea_query::{Expr, ExprTrait, JoinType, Order, Query, SelectStatement};
use serde_json::Value;

use crate::contract::Row;
use crate::{Read, Result, W};

pub(super) const SAID_AT: &str = "coalesce(m.ext_created, CASE WHEN m.direction = 'out' THEN coalesce(m.sent, m.created) ELSE m.created END)";

pub struct Record {
    pub id: String,
    pub at: i64,
    pub direction: String,
    pub account: Option<String>,
    pub party: Option<String>,
    pub persona: Option<String>,
    pub on_behalf: String,
    pub text: String,
    pub attachments: Value,
    pub thread: Option<String>,
    pub external: Option<String>,
    pub reply_to: Option<String>,
    pub display_name: String,
    pub party_name: String,
    pub handle: String,
    pub account_kind: String,
    pub contact_kind: String,
    pub persona_name: String,
    pub created: i64,
}

fn decode(row: &Row<'_>) -> Result<Record> {
    let text = |i| Ok::<_, crate::StoreError>(row.get::<Option<String>>(i)?.unwrap_or_default());
    let raw_attachments = row.get::<Option<String>>(8)?;
    Ok(Record {
        id: row.get(0)?,
        at: row.get(1)?,
        direction: row.get(2)?,
        account: row.get(3)?,
        party: row.get(4)?,
        persona: row.get(5)?,
        on_behalf: row
            .get::<Option<String>>(6)?
            .unwrap_or_else(|| "none".into()),
        text: text(7)?,
        attachments: raw_attachments
            .as_deref()
            .and_then(|raw| serde_json::from_str(raw).ok())
            .unwrap_or_else(|| serde_json::json!([])),
        thread: row.get(9)?,
        external: row.get(10)?,
        reply_to: row.get(11)?,
        display_name: text(12)?,
        party_name: text(13)?,
        handle: text(14)?,
        account_kind: text(15)?,
        contact_kind: text(16)?,
        persona_name: text(17)?,
        created: row.get(18)?,
    })
}

fn records(space: &str) -> SelectStatement {
    Query::select()
        .column(("m", "id"))
        .expr(Expr::cust(SAID_AT))
        .columns([
            ("m", "direction"),
            ("m", "sender_account_id"),
            ("m", "sender_party_id"),
            ("m", "persona_id"),
            ("m", "on_behalf"),
            ("m", "text"),
            ("m", "attachments"),
            ("m", "thread_key"),
            ("m", "external_id"),
            ("m", "reply_to"),
            ("a", "display_name"),
            ("p", "name"),
            ("a", "handle_display"),
            ("a", "kind"),
            ("p", "contact_kind"),
            ("pe", "name"),
            ("m", "created"),
        ])
        .from_as("chan_messages", "m")
        .join_as(
            JoinType::LeftJoin,
            "accounts",
            "a",
            Expr::col(("a", "id")).equals(("m", "sender_account_id")),
        )
        .join_as(
            JoinType::LeftJoin,
            "parties",
            "p",
            Expr::col(("p", "id")).equals(("m", "sender_party_id")),
        )
        .join_as(
            JoinType::LeftJoin,
            "personas",
            "pe",
            Expr::col(("pe", "id")).equals(("m", "persona_id")),
        )
        .and_where(Expr::col(("m", "space_id")).eq(space))
        .take()
}

pub fn window(
    r: &mut impl Read,
    space: &str,
    id: &str,
    before: u64,
    after: u64,
) -> Result<Option<(Record, Vec<Record>)>> {
    let query = records(space);
    let Some(target) = r.one(
        query.clone().and_where(Expr::col(("m", "id")).eq(id)),
        decode,
    )?
    else {
        return Ok(None);
    };
    let attachments = r
        .db()
        .dialect()
        .json_rows(Expr::col(("m", "attachments")), "attachment");
    let visible = query
        .clone()
        .and_where(Expr::col(("m", "id")).ne(id))
        .and_where(Expr::col(("m", "status")).is_in(["received", "sent"]))
        .and_where(Expr::col(("m", "text")).is_not_null())
        .and_where(
            Expr::col(("m", "text"))
                .ne("")
                .or(Expr::exists(attachments)),
        )
        .take();
    let mut nearby = r.all(
        visible
            .clone()
            .and_where(Expr::col(("m", "created")).lte(target.created))
            .order_by_columns([(("m", "created"), Order::Desc), (("m", "id"), Order::Desc)])
            .limit(before),
        decode,
    )?;
    nearby.extend(
        r.all(
            visible
                .clone()
                .and_where(Expr::col(("m", "created")).gt(target.created))
                .order_by_columns([(("m", "created"), Order::Asc), (("m", "id"), Order::Asc)])
                .limit(after),
            decode,
        )?,
    );
    Ok(Some((target, nearby)))
}

pub fn pending(r: &mut impl Read, space: &str, limit: u64) -> Result<Vec<(i64, String)>> {
    r.all(
        Query::select()
            .columns(["created", "text"])
            .from("chan_messages")
            .and_where(Expr::col("space_id").eq(space))
            .and_where(Expr::col("direction").eq("out"))
            .and_where(Expr::col("status").is_in(["draft", "pending"]))
            .and_where(Expr::col("text").ne(""))
            .order_by_columns([("created", Order::Desc), ("id", Order::Desc)])
            .limit(limit),
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
}

pub fn facts(r: &mut impl Read, message: &str) -> Result<Value> {
    let event: Option<Option<String>> = r.one(
        Query::select()
            .column("event_id")
            .from("chan_messages")
            .and_where(Expr::col("id").eq(message)),
        |row| row.get(0),
    )?;
    let Some(event) = event.flatten() else {
        return Ok(Value::Null);
    };
    Ok(crate::SignalRows(r)
        .event(&event)?
        .map(|row| row.payload["facts"].clone())
        .unwrap_or_default())
}

pub fn describable(r: &mut impl Read, message: &str) -> Result<Option<Value>> {
    let raw: Option<Option<String>> = r.one(
        Query::select()
            .column("attachments")
            .from("chan_messages")
            .and_where(Expr::col("id").eq(message))
            .and_where(Expr::col("direction").eq("in"))
            .and_where(Expr::col("purged").is_null()),
        |row| row.get(0),
    )?;
    Ok(raw.flatten().and_then(|s| serde_json::from_str(&s).ok()))
}

pub fn set_attachments(w: &mut W<'_>, message: &str, attachments: &Value) -> Result<()> {
    w.exec(
        Query::update()
            .table("chan_messages")
            .value("attachments", attachments.to_string())
            .and_where(Expr::col("id").eq(message)),
    )?;
    Ok(())
}
