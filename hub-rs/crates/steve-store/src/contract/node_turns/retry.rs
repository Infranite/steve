//! Durable gates shared by every automatic revision of a logical turn.

use crate::{Read, Result, StoreError};
use sea_query::{Expr, ExprTrait, Func, JoinType, Query};

const TURN_RETRY_DEADLINE_MS: i64 = 15 * 60_000;

pub fn before_deadline(r: &mut impl Read, turn: &str, at: i64) -> Result<bool> {
    let d = r.db().dialect();
    let first = r
        .one(
            Query::select()
                .expr(Func::min(Expr::col("updated")))
                .from("node_commands")
                .and_where(Expr::col("intent_purpose").eq("start_turn"))
                .and_where(Expr::col("intent_subject").eq(turn))
                .and_where(Expr::col("state").is_in(["finished", "rejected"]))
                .and_where(d.json_field(Expr::col("receipt"), "success").eq(false)),
            |row| row.get::<Option<i64>>(0),
        )?
        .flatten();
    Ok(first.is_some_and(|first| at <= first.saturating_add(TURN_RETRY_DEADLINE_MS)))
}

pub fn allowed(r: &mut impl Read, session: &str) -> Result<bool> {
    let open = Query::select()
        .column(("d", "id"))
        .from_as("decisions", "d")
        .and_where(Expr::col(("d", "item_id")).eq(Expr::col(("i", "id"))))
        .and_where(Expr::col(("d", "status")).eq("open"))
        .take();
    let allowed = Expr::col(("s", "status"))
        .ne("ended")
        .and(
            Expr::col(("i", "id"))
                .is_null()
                .or(Expr::col(("i", "phase"))
                    .eq("active")
                    .and(Expr::col(("i", "archived")).eq(false))),
        )
        .and(Expr::exists(open).not());
    r.one(
        Query::select()
            .expr(allowed)
            .from_as("sessions", "s")
            .join_as(
                JoinType::LeftJoin,
                "items",
                "i",
                Expr::col(("i", "id")).eq(Expr::col(("s", "item_id"))),
            )
            .and_where(Expr::col(("s", "id")).eq(session)),
        |row| row.get(0),
    )?
    .ok_or_else(|| StoreError::Rejected(format!("missing session {session}")))
}
