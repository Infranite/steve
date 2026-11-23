//! Message identity and timestamps for operations on a provider.

use sea_query::{Expr, ExprTrait, Order, Query};

use crate::{Read, Result};

pub struct Message {
    pub space: String,
    pub external_id: Option<String>,
    pub created: i64,
    pub created_exact: bool,
    pub sender_account: Option<String>,
    pub text: Option<String>,
    pub thread_key: Option<String>,
}

pub fn message(r: &mut impl Read, id: &str) -> Result<Option<Message>> {
    r.one(
        Query::select()
            .columns([
                "space_id",
                "external_id",
                "created",
                "ext_created",
                "direction",
                "sender_account_id",
                "text",
                "thread_key",
            ])
            .from("chan_messages")
            .and_where(Expr::col("id").eq(id)),
        |row| {
            let sent_at: Option<i64> = row.get(3)?;
            Ok(Message {
                space: row.get(0)?,
                external_id: row.get(1)?,
                created: sent_at.unwrap_or(row.get(2)?),
                created_exact: row.get::<String>(4)? == "in" || sent_at.is_some(),
                sender_account: row.get(5)?,
                text: row.get(6)?,
                thread_key: row.get(7)?,
            })
        },
    )
}

pub fn source_id(r: &mut impl Read, id: &str, connection: &str) -> Result<Option<String>> {
    r.one(
        Query::select()
            .column("source_id")
            .from("chan_message_seen")
            .and_where(Expr::col("message_id").eq(id))
            .and_where(Expr::col("connection_id").eq(connection))
            .limit(1),
        |row| row.get(0),
    )
}

pub fn in_reply_to(r: &mut impl Read, space: &str, external: &str) -> Result<Option<String>> {
    r.one(
        Query::select()
            .column("id")
            .from("chan_messages")
            .and_where(Expr::col("space_id").eq(space))
            .and_where(Expr::col("external_id").eq(external))
            .order_by_expr(Expr::col("direction").eq("in"), Order::Desc)
            .order_by("created", Order::Asc)
            .limit(1),
        |row| row.get(0),
    )
}
