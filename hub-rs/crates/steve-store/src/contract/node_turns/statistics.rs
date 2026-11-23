//! Statistics recorded for a Node turn and its usage reports.

use sea_query::{Expr, ExprTrait, JoinType, Query};

use crate::{ModelUsage, Read, Result, Tokens};

pub struct AttemptStart {
    pub ordinal: i64,
    pub at: i64,
}

pub fn missing_turn(
    r: &mut impl Read,
    session: &str,
    attempt: &str,
) -> Result<Option<AttemptStart>> {
    let recorded = Query::select()
        .column("id")
        .from("agent_turns")
        .and_where(Expr::col("node_attempt_id").eq(attempt))
        .take();
    r.one(
        Query::select()
            .columns([("a", "ordinal"), ("a", "created")])
            .from_as("runtime_attempts", "a")
            .join_as(
                JoinType::InnerJoin,
                "runtime_turns",
                "t",
                Expr::col(("a", "turn_id")).eq(Expr::col(("t", "turn_id"))),
            )
            .and_where(Expr::col(("a", "attempt_id")).eq(attempt))
            .and_where(Expr::col(("t", "session_id")).eq(session))
            .and_where(Expr::exists(recorded).not()),
        |row| {
            Ok(AttemptStart {
                ordinal: row.get(0)?,
                at: row.get(1)?,
            })
        },
    )
}

pub struct AgentTurn {
    pub id: String,
    pub started: i64,
}

pub fn turn(r: &mut impl Read, attempt: &str) -> Result<Option<AgentTurn>> {
    r.one(
        Query::select()
            .columns(["id", "started"])
            .from("agent_turns")
            .and_where(Expr::col("node_attempt_id").eq(attempt)),
        |row| {
            Ok(AgentTurn {
                id: row.get(0)?,
                started: row.get(1)?,
            })
        },
    )
}

pub fn usage(r: &mut impl Read, turn: &str) -> Result<Vec<ModelUsage>> {
    r.all(
        Query::select()
            .columns([
                "model",
                "input",
                "output",
                "cache_read",
                "cache_write",
                "reasoning",
                "cost_reported",
            ])
            .from("usage_records")
            .and_where(Expr::col("live_turn_id").eq(turn)),
        |row| {
            Ok(ModelUsage {
                model: row.get(0)?,
                tokens: Tokens {
                    input: row.get(1)?,
                    output: row.get(2)?,
                    cache_read: row.get(3)?,
                    cache_write: row.get(4)?,
                    reasoning: row.get(5)?,
                },
                cost_reported: row.get(6)?,
                detail: serde_json::Value::Null,
            })
        },
    )
}
