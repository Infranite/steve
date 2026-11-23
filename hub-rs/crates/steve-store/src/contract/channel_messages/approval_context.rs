//! Stored messages an outbound approval card shows beside the draft.

use sea_query::{Expr, ExprTrait, Order, Query, SelectStatement};

use crate::contract::Row;
use crate::{Read, Result};

pub struct Draft {
    pub space_id: String,
    pub text: Option<String>,
    pub reply_to: Option<String>,
    pub on_behalf: String,
    pub persona_id: Option<String>,
}

pub fn draft(r: &mut impl Read, id: &str) -> Result<Option<Draft>> {
    r.one(
        Query::select()
            .columns(["space_id", "text", "reply_to", "on_behalf", "persona_id"])
            .from("chan_messages")
            .and_where(Expr::col("id").eq(id))
            .and_where(Expr::col("direction").eq("out")),
        |row| {
            Ok(Draft {
                space_id: row.get(0)?,
                text: row.get(1)?,
                reply_to: row.get(2)?,
                on_behalf: row.get(3)?,
                persona_id: row.get(4)?,
            })
        },
    )
}

pub struct Message {
    pub id: String,
    pub text: Option<String>,
    pub sender_party_id: Option<String>,
    pub sender_account_id: Option<String>,
    pub created: i64,
    pub out: bool,
}

pub fn replied_to(r: &mut impl Read, space: &str, reference: &str) -> Result<Option<Message>> {
    r.one(
        messages(space)
            .and_where(
                Expr::col("external_id")
                    .eq(reference)
                    .or(Expr::col("id").eq(reference)),
            )
            .order_by("created", Order::Asc)
            .limit(1),
        message,
    )
}

pub fn preceding(
    r: &mut impl Read,
    space: &str,
    replied: &Message,
    limit: u64,
) -> Result<Vec<Message>> {
    r.all(
        messages(space)
            .and_where(Expr::col("created").lt(replied.created))
            .and_where(Expr::col("id").ne(replied.id.as_str()))
            .and_where(Expr::col("text").is_not_null())
            .order_by("created", Order::Desc)
            .limit(limit),
        message,
    )
}

fn messages(space: &str) -> SelectStatement {
    Query::select()
        .columns([
            "id",
            "text",
            "sender_party_id",
            "sender_account_id",
            "created",
            "direction",
        ])
        .from("chan_messages")
        .and_where(Expr::col("space_id").eq(space))
        .take()
}

fn message(row: &Row<'_>) -> Result<Message> {
    Ok(Message {
        id: row.get(0)?,
        text: row.get(1)?,
        sender_party_id: row.get(2)?,
        sender_account_id: row.get(3)?,
        created: row.get(4)?,
        out: row.get::<String>(5)? == "out",
    })
}
