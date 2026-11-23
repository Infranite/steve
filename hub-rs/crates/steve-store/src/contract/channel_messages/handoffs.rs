//! Inbound message identities and open handoff cards in a space.

use sea_query::{Expr, ExprTrait, Func, JoinType, Query};

use crate::{Read, Result};

pub struct Card {
    pub decision: String,
    pub message: String,
    pub question: String,
    pub sender: String,
    pub thread: Option<String>,
}

fn sender() -> Expr {
    Func::coalesce([
        Expr::col(("m", "sender_party_id")),
        Expr::col(("m", "sender_account_id")),
        Expr::Constant("".into()),
    ])
    .into()
}

pub fn sender_thread(r: &mut impl Read, message: &str) -> Result<Option<(String, Option<String>)>> {
    r.one(
        Query::select()
            .expr(sender())
            .column(("m", "thread_key"))
            .from_as("chan_messages", "m")
            .and_where(Expr::col(("m", "id")).eq(message)),
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
}

pub fn open_cards(
    r: &mut impl Read,
    ws: &str,
    space: &str,
    subject: &str,
    keep: &str,
) -> Result<Vec<Card>> {
    r.all(
        Query::select()
            .columns([("d", "id"), ("m", "id"), ("d", "question")])
            .expr(sender())
            .column(("m", "thread_key"))
            .from_as("decisions", "d")
            .join_as(
                JoinType::InnerJoin,
                "chan_messages",
                "m",
                Expr::col(("m", "id"))
                    .equals(("d", "subject_id"))
                    .and(Expr::col(("m", "direction")).eq("in")),
            )
            .and_where(Expr::col(("d", "workspace_id")).eq(ws))
            .and_where(Expr::col(("d", "subject_type")).eq(subject))
            .and_where(Expr::col(("d", "kind")).eq("ask"))
            .and_where(Expr::col(("d", "status")).eq("open"))
            .and_where(Expr::col(("m", "space_id")).eq(space))
            .and_where(Expr::col(("d", "id")).ne(keep)),
        |row| {
            Ok(Card {
                decision: row.get(0)?,
                message: row.get(1)?,
                question: row.get(2)?,
                sender: row.get(3)?,
                thread: row.get(4)?,
            })
        },
    )
}
