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

pub struct OperationAttempt {
    pub id: String,
    pub revision: i64,
    pub turn: String,
    pub session: String,
    pub state: String,
    pub cancelled: bool,
}

fn operation_attempt(
    c: &mut impl Read,
    op: &str,
    target: bool,
) -> Result<Option<OperationAttempt>> {
    let mut q = Query::select();
    let join = sea_query::JoinType::InnerJoin;
    q.columns([
        ("a", "attempt_id"),
        ("a", "revision"),
        ("a", "turn_id"),
        ("t", "session_id"),
        ("a", "state"),
        ("a", "cancel_requested"),
    ])
    .from_as("runtime_attempts", "a")
    .join_as(
        join,
        "runtime_turns",
        "t",
        Expr::col(("a", "turn_id")).eq(Expr::col(("t", "turn_id"))),
    );
    if target {
        q.join_as(
            join,
            "node_commands",
            "c",
            Expr::col(("a", "attempt_id")).eq(Expr::col(("c", "target_attempt"))),
        )
        .and_where(Expr::col(("c", "operation_id")).eq(op));
    } else {
        q.and_where(Expr::col(("a", "operation_id")).eq(op));
    }
    c.one(&q, |r| {
        Ok(OperationAttempt {
            id: r.get(0)?,
            revision: r.get(1)?,
            turn: r.get(2)?,
            session: r.get(3)?,
            state: r.get(4)?,
            cancelled: r.get(5)?,
        })
    })
}

pub fn started_by(c: &mut impl Read, operation: &str) -> Result<Option<OperationAttempt>> {
    operation_attempt(c, operation, false)
}

pub fn targeted_by(c: &mut impl Read, operation: &str) -> Result<Option<OperationAttempt>> {
    operation_attempt(c, operation, true)
}

pub fn waiting_to_open(c: &mut impl Read, session: &str) -> Result<Option<String>> {
    c.one(
        Query::select()
            .column("turn_id")
            .from("runtime_turns")
            .and_where(Expr::col("session_id").eq(session))
            .and_where(Expr::col("state").ne("finished"))
            .and_where(Expr::col("active_attempt").is_null())
            .order_by("created", Order::Asc)
            .limit(1),
        |r| r.get(0),
    )
}

pub fn latest_spec(c: &mut impl Read, turn: &str) -> Result<(steve_node_api::TurnSpec, i64)> {
    c.one(
        Query::select()
            .columns(["spec", "ordinal"])
            .from("runtime_attempts")
            .and_where(Expr::col("turn_id").eq(turn))
            .order_by("ordinal", Order::Desc)
            .limit(1),
        |r| Ok((r.get::<super::Json<_>>(0)?.0, r.get(1)?)),
    )?
    .ok_or_else(|| StoreError::Rejected(format!("turn {turn} has no attempt")))
}

pub fn was_released(c: &mut impl Read, turn: &str) -> Result<bool> {
    let reason = c
        .db()
        .dialect()
        .json_field(Expr::col("completion"), "stop_reason");
    Ok(c.one(
        Query::select()
            .column("attempt_id")
            .from("runtime_attempts")
            .and_where(Expr::col("turn_id").eq(turn))
            .and_where(reason.eq("released"))
            .limit(1),
        |_| Ok(()),
    )?
    .is_some())
}

pub fn retry_ordinal(c: &mut impl Read, turn: &str) -> Result<i64> {
    c.one(
        Query::select()
            .column("retry_ordinal")
            .from("runtime_turns")
            .and_where(Expr::col("turn_id").eq(turn)),
        |r| r.get(0),
    )?
    .ok_or_else(|| StoreError::Rejected(format!("missing turn {turn}")))
}

pub fn ordinal(c: &mut impl Read, attempt: &str) -> Result<i64> {
    c.one(
        Query::select()
            .column("ordinal")
            .from("runtime_attempts")
            .and_where(Expr::col("attempt_id").eq(attempt)),
        |r| r.get(0),
    )?
    .ok_or_else(|| StoreError::Rejected(format!("missing attempt {attempt}")))
}

pub fn retry_state(c: &mut impl Read, turn: &str) -> Result<(i64, bool)> {
    let cancelled = Query::select()
        .column("turn_id")
        .from("runtime_attempts")
        .and_where(Expr::col("turn_id").eq(turn))
        .and_where(Expr::col("cancel_requested").eq(true))
        .take();
    c.one(
        Query::select()
            .column("retry_ordinal")
            .expr(Expr::exists(cancelled))
            .from("runtime_turns")
            .and_where(Expr::col("turn_id").eq(turn)),
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?
    .ok_or_else(|| StoreError::Rejected(format!("missing turn {turn}")))
}

pub fn for_attempt(c: &mut impl Read, attempt: &str) -> Result<(String, String)> {
    c.one(
        Query::select()
            .columns([("t", "turn_id"), ("t", "session_id")])
            .from_as("runtime_attempts", "a")
            .join_as(
                sea_query::JoinType::InnerJoin,
                "runtime_turns",
                "t",
                Expr::col(("a", "turn_id")).eq(Expr::col(("t", "turn_id"))),
            )
            .and_where(Expr::col(("a", "attempt_id")).eq(attempt)),
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?
    .ok_or_else(|| StoreError::Rejected(format!("missing attempt {attempt}")))
}
