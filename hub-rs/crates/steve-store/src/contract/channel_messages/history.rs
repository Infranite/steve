//! A bounded chronological read of already stored messages.

use sea_query::{Expr, ExprTrait, Func, JoinType, Order, Query};
use serde_json::Value;

use crate::{Read, Result};

pub struct Line {
    pub at: i64,
    pub sender: String,
    pub out: bool,
    pub text: Option<String>,
    pub attachments: Vec<Value>,
    pub history: Option<String>,
}

pub fn lines(r: &mut impl Read, space: &str, from: i64, to: i64, limit: u64) -> Result<Vec<Line>> {
    let name = Func::coalesce([
        Expr::cust_with_expr("NULLIF(?, '')", Expr::col(("a", "display_name"))),
        Expr::cust_with_expr("NULLIF(?, '')", Expr::col(("p", "name"))),
        Expr::col(("a", "handle_display")),
        Expr::cust("''"),
    ]);
    r.all(
        Query::select()
            .from_as("chan_messages", "m")
            .column(("m", "created"))
            .expr(name)
            .columns([
                ("m", "direction"),
                ("m", "text"),
                ("m", "attachments"),
                ("m", "history"),
            ])
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
            .and_where(Expr::col(("m", "space_id")).eq(space))
            .and_where(Expr::col(("m", "created")).between(from, to))
            .and_where(
                Expr::col(("m", "direction"))
                    .eq("in")
                    .or(Expr::col(("m", "status")).is_in(["sent", "recalled"])),
            )
            .order_by_columns([(("m", "created"), Order::Asc), (("m", "id"), Order::Asc)])
            .limit(limit),
        |row| {
            let attachments = row
                .get::<Option<String>>(4)?
                .and_then(|raw| serde_json::from_str(&raw).ok())
                .unwrap_or_default();
            Ok(Line {
                at: row.get(0)?,
                sender: row.get(1)?,
                out: row.get::<String>(2)? == "out",
                text: row.get(3)?,
                attachments,
                history: row.get(5)?,
            })
        },
    )
}
