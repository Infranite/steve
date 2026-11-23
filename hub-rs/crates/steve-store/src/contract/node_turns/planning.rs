//! Ownership of a turn's current attempt for cancellation and mid-turn input.

use sea_query::{Expr, ExprTrait, JoinType, Query};

use crate::{Read, Result};

pub fn same_turn(r: &mut impl Read, first: &str, second: &str) -> Result<bool> {
    Ok(r.one(
        Query::select()
            .column(("a", "attempt_id"))
            .from_as("runtime_attempts", "a")
            .join_as(
                JoinType::InnerJoin,
                "runtime_attempts",
                "b",
                Expr::col(("b", "turn_id")).eq(Expr::col(("a", "turn_id"))),
            )
            .and_where(Expr::col(("a", "attempt_id")).eq(first))
            .and_where(Expr::col(("b", "attempt_id")).eq(second)),
        |_| Ok(()),
    )?
    .is_some())
}

pub struct ActiveAttempt {
    pub id: String,
    pub operation: String,
    pub revision: i64,
    pub state: String,
}

pub fn active_for(r: &mut impl Read, session: &str, asked: &str) -> Result<Option<ActiveAttempt>> {
    r.one(
        Query::select()
            .columns([
                ("a", "attempt_id"),
                ("a", "operation_id"),
                ("a", "revision"),
                ("a", "state"),
            ])
            .from_as("runtime_sessions", "s")
            .join_as(
                JoinType::InnerJoin,
                "runtime_attempts",
                "a",
                Expr::col(("a", "attempt_id")).eq(Expr::col(("s", "active_attempt"))),
            )
            .join_as(
                JoinType::InnerJoin,
                "runtime_turns",
                "t",
                Expr::col(("t", "turn_id")).eq(Expr::col(("a", "turn_id"))),
            )
            .join_as(
                JoinType::InnerJoin,
                "runtime_attempts",
                "asked",
                Expr::col(("asked", "turn_id")).eq(Expr::col(("a", "turn_id"))),
            )
            .and_where(Expr::col(("s", "session_id")).eq(session))
            .and_where(Expr::col(("asked", "attempt_id")).eq(asked))
            .and_where(Expr::col(("a", "state")).ne("finished"))
            .and_where(Expr::col(("t", "state")).ne("finished")),
        |row| {
            Ok(ActiveAttempt {
                id: row.get(0)?,
                operation: row.get(1)?,
                revision: row.get(2)?,
                state: row.get(3)?,
            })
        },
    )
}

pub fn session_for_live_attempt(
    r: &mut impl Read,
    node_session: &str,
    turn: &str,
    attempt: &str,
) -> Result<Option<String>> {
    r.one(
        Query::select()
            .column(("s", "session_id"))
            .from_as("runtime_sessions", "s")
            .join_as(
                JoinType::InnerJoin,
                "runtime_turns",
                "t",
                Expr::col(("t", "session_id")).eq(Expr::col(("s", "session_id"))),
            )
            .join_as(
                JoinType::InnerJoin,
                "runtime_attempts",
                "a",
                Expr::col(("a", "turn_id")).eq(Expr::col(("t", "turn_id"))),
            )
            .and_where(Expr::col(("s", "node_session_id")).eq(node_session))
            .and_where(Expr::col(("t", "turn_id")).eq(turn))
            .and_where(Expr::col(("t", "state")).ne("finished"))
            .and_where(Expr::col(("t", "active_attempt")).eq(attempt))
            .and_where(Expr::col(("a", "attempt_id")).eq(attempt))
            .and_where(Expr::col(("a", "cancel_requested")).eq(false)),
        |row| row.get(0),
    )
}

pub fn planned_here(r: &mut impl Read, node: &str, session: &str, attempt: &str) -> Result<bool> {
    Ok(r.one(
        Query::select()
            .column(("a", "attempt_id"))
            .from_as("runtime_attempts", "a")
            .join_as(
                JoinType::InnerJoin,
                "runtime_turns",
                "t",
                Expr::col(("t", "turn_id")).eq(Expr::col(("a", "turn_id"))),
            )
            .join_as(
                JoinType::InnerJoin,
                "runtime_sessions",
                "s",
                Expr::col(("s", "session_id")).eq(Expr::col(("t", "session_id"))),
            )
            .and_where(Expr::col(("a", "attempt_id")).eq(attempt))
            .and_where(Expr::col(("t", "session_id")).eq(session))
            .and_where(Expr::col(("a", "node_id")).eq(node))
            .and_where(Expr::col(("s", "node_id")).eq(node)),
        |_| Ok(()),
    )?
    .is_some())
}
