//! Invalid envelopes retained before acknowledging an external channel feed.

use sea_query::{Expr, ExprTrait, OnConflict, Query};
use serde_json::Value;

use crate::{Read, Result, W};

#[derive(Clone, Copy)]
pub struct DeadLetterKey<'a> {
    pub connection: &'a str,
    pub chat: &'a str,
    pub source: &'a str,
}

pub fn has_dead_letter(r: &mut impl Read, key: DeadLetterKey<'_>) -> Result<bool> {
    Ok(r.one(
        Query::select()
            .column("source_id")
            .from("chan_ext_ws_dead_letters")
            .and_where(Expr::col("connection_id").eq(key.connection))
            .and_where(Expr::col("chat_id").eq(key.chat))
            .and_where(Expr::col("source_id").eq(key.source)),
        |_| Ok(()),
    )?
    .is_some())
}

pub fn retain_dead_letter(
    w: &mut W<'_>,
    key: DeadLetterKey<'_>,
    payload: &Value,
    error: &str,
    now: i64,
) -> Result<bool> {
    Ok(w.exec(
        Query::insert()
            .into_table("chan_ext_ws_dead_letters")
            .columns([
                "connection_id",
                "chat_id",
                "source_id",
                "payload",
                "error",
                "created",
            ])
            .values_panic([
                key.connection.into(),
                key.chat.into(),
                key.source.into(),
                serde_json::to_string(payload)?.into(),
                error.into(),
                now.into(),
            ])
            .on_conflict(
                OnConflict::columns(["connection_id", "chat_id", "source_id"])
                    .do_nothing()
                    .to_owned(),
            ),
    )? > 0)
}
