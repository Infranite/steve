//! Acknowledgment and completion reactions on an inbound message.

use sea_query::{Expr, ExprTrait, OnConflict, Query};
use serde::Serialize;

use crate::{Read, Result, W};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Reaction {
    pub message_id: String,
    pub state: String,
    pub reaction_id: Option<String>,
    pub emoji: Option<String>,
    pub as_user: bool,
    pub answer_id: Option<String>,
    pub error: Option<String>,
    pub updated: i64,
}

pub fn get(r: &mut impl Read, message_id: &str) -> Result<Option<Reaction>> {
    r.one(
        Query::select()
            .columns([
                "message_id",
                "state",
                "reaction_id",
                "emoji",
                "as_user",
                "answer_id",
                "error",
                "updated",
            ])
            .from("chan_reactions")
            .and_where(Expr::col("message_id").eq(message_id)),
        |row| {
            Ok(Reaction {
                message_id: row.get(0)?,
                state: row.get(1)?,
                reaction_id: row.get(2)?,
                emoji: row.get(3)?,
                as_user: row.get::<i64>(4)? != 0,
                answer_id: row.get(5)?,
                error: row.get(6)?,
                updated: row.get(7)?,
            })
        },
    )
}

pub fn discarded_answer_source(r: &mut impl Read, out_id: &str) -> Result<Option<String>> {
    r.one(
        Query::select()
            .column("message_id")
            .from("chan_reactions")
            .and_where(Expr::col("answer_id").eq(out_id))
            .and_where(Expr::exists(
                Query::select()
                    .column("id")
                    .from("chan_messages")
                    .and_where(Expr::col("id").eq(out_id))
                    .and_where(Expr::col("status").eq("discarded"))
                    .take(),
            ))
            .limit(1),
        |row| row.get(0),
    )
}

pub fn for_messages(r: &mut impl Read, ids: &[String]) -> Result<Vec<String>> {
    if ids.is_empty() {
        return Ok(vec![]);
    }
    let ids = r.db().dialect().text_array(ids)?;
    r.all(
        Query::select()
            .column("message_id")
            .from("chan_reactions")
            .and_where(Expr::col("message_id").in_subquery(ids)),
        |row| row.get(0),
    )
}

pub fn expect_answer(w: &mut W<'_>, message_id: &str, out_id: &str) -> Result<bool> {
    w.exec(
        Query::update()
            .table("chan_messages")
            .value("answer_to", message_id)
            .and_where(Expr::col("id").eq(out_id)),
    )?;
    Ok(w.exec(
        Query::update()
            .table("chan_reactions")
            .value("answer_id", out_id)
            .and_where(Expr::col("message_id").eq(message_id))
            .and_where(Expr::col("state").is_in(["ack", "failed"])),
    )? == 1)
}

pub fn acknowledge(w: &mut W<'_>, ws: &str, message: &str, now: i64) -> Result<()> {
    let empty = Option::<String>::None;
    w.exec(
        Query::insert()
            .into_table("chan_reactions")
            .columns([
                "message_id",
                "workspace_id",
                "state",
                "reaction_id",
                "emoji",
                "as_user",
                "answer_id",
                "error",
                "updated",
            ])
            .values_panic([
                message.into(),
                ws.into(),
                "ack".into(),
                empty.clone().into(),
                empty.clone().into(),
                0i64.into(),
                empty.clone().into(),
                empty.into(),
                now.into(),
            ])
            .on_conflict(
                OnConflict::column("message_id")
                    .update_columns([
                        "state",
                        "reaction_id",
                        "emoji",
                        "as_user",
                        "answer_id",
                        "error",
                        "updated",
                    ])
                    .to_owned(),
            ),
    )?;
    Ok(())
}

pub struct Change<'a> {
    pub id: Option<&'a str>,
    pub emoji: Option<&'a str>,
    pub as_user: bool,
    pub error: Option<&'a str>,
}

pub fn save_result(
    w: &mut W<'_>,
    message: &str,
    change: Change<'_>,
    finished: Option<&str>,
    now: i64,
) -> Result<()> {
    let mut update = Query::update();
    update
        .table("chan_reactions")
        .value("reaction_id", change.id)
        .value("emoji", change.emoji)
        .value("as_user", i64::from(change.as_user))
        .value("error", change.error)
        .value("updated", now)
        .and_where(Expr::col("message_id").eq(message));
    match finished {
        Some(state) => {
            update.value("state", state);
        }
        None => {
            update.and_where(Expr::col("state").eq("ack"));
        }
    }
    w.exec(&update)?;
    Ok(())
}

pub struct InboundTarget {
    pub workspace: String,
    pub space: String,
    pub account: Option<String>,
    pub party: Option<String>,
}

pub fn inbound_target(r: &mut impl Read, message: &str) -> Result<Option<InboundTarget>> {
    r.one(
        Query::select()
            .columns([
                "workspace_id",
                "space_id",
                "sender_account_id",
                "sender_party_id",
            ])
            .from("chan_messages")
            .and_where(Expr::col("id").eq(message))
            .and_where(Expr::col("direction").eq("in"))
            .and_where(Expr::col("external_id").ne("")),
        |row| {
            Ok(InboundTarget {
                workspace: row.get(0)?,
                space: row.get(1)?,
                account: row.get(2)?,
                party: row.get(3)?,
            })
        },
    )
}
