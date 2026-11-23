//! Read current state and revisions from the Hub's durable turn and attempt projections.

use crate::{Read, Result, StoreError};
use sea_query::{Expr, ExprTrait, Order, Query};

pub fn revision(c: &mut impl Read, turn: &str) -> Result<i64> {
    c.one(
        Query::select()
            .column("revision")
            .from("runtime_turns")
            .and_where(Expr::col("turn_id").eq(turn)),
        |r| r.get(0),
    )?
    .ok_or_else(|| StoreError::Rejected(format!("missing turn {turn}")))
}

pub fn latest_attempt(c: &mut impl Read, turn: &str) -> Result<String> {
    c.one(
        Query::select()
            .column("attempt_id")
            .from("runtime_attempts")
            .and_where(Expr::col("turn_id").eq(turn))
            .order_by("ordinal", Order::Desc)
            .limit(1),
        |r| r.get(0),
    )?
    .ok_or_else(|| StoreError::Rejected(format!("turn {turn} has no attempt")))
}

pub struct Attempt {
    pub revision: i64,
    pub state: String,
    pub operation: String,
}

pub fn attempt(c: &mut impl Read, attempt: &str) -> Result<Option<Attempt>> {
    c.one(
        Query::select()
            .columns(["revision", "state", "operation_id"])
            .from("runtime_attempts")
            .and_where(Expr::col("attempt_id").eq(attempt)),
        |r| {
            Ok(Attempt {
                revision: r.get(0)?,
                state: r.get(1)?,
                operation: r.get(2)?,
            })
        },
    )
}
