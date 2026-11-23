//! Hub-side Node session bindings and execution ownership.

use sea_query::{Expr, ExprTrait, Func, JoinType, Query};
use steve_node_api::{ExecutionRef, OperationId, SessionId};

use crate::{Read, Result};

pub struct PrincipalState {
    pub node: String,
    pub principal: Option<String>,
    pub state_dir: Option<String>,
}

pub fn principal_state(c: &mut impl Read, session: &str) -> Result<Option<PrincipalState>> {
    c.one(
        Query::select()
            .columns(["node_id", "principal", "state_dir"])
            .from("runtime_sessions")
            .and_where(Expr::col("session_id").eq(session)),
        |r| {
            Ok(PrincipalState {
                node: r.get(0)?,
                principal: r.get(1)?,
                state_dir: r.get(2)?,
            })
        },
    )
}

pub fn on_node(c: &mut impl Read, node: &str) -> Result<Vec<String>> {
    c.all(
        Query::select()
            .column("session_id")
            .from("runtime_sessions")
            .and_where(Expr::col("node_id").eq(node)),
        |r| r.get(0),
    )
}

pub fn owns_session(c: &mut impl Read, node: &str, session: &SessionId) -> Result<bool> {
    Ok(c.one(
        Query::select()
            .column("session_id")
            .from("runtime_sessions")
            .and_where(Expr::col("session_id").eq(session.as_str()))
            .and_where(Expr::col("node_session_id").eq(session.as_str()))
            .and_where(Expr::col("node_id").eq(node)),
        |_| Ok(()),
    )?
    .is_some())
}

pub fn owns_execution(c: &mut impl Read, node: &str, execution: &ExecutionRef) -> Result<bool> {
    let mut query = Query::select();
    query.column(("a", "attempt_id"));
    match execution {
        ExecutionRef::Turn {
            session,
            turn,
            attempt,
        } => {
            query
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
                .and_where(Expr::col(("a", "attempt_id")).eq(attempt.as_str()))
                .and_where(Expr::col(("a", "turn_id")).eq(turn.as_str()))
                .and_where(Expr::col(("t", "session_id")).eq(session.as_str()))
                .and_where(Expr::col(("s", "node_session_id")).eq(session.as_str()))
                .and_where(Expr::col(("a", "node_id")).eq(node))
                .and_where(Expr::col(("s", "node_id")).eq(node));
        }
        ExecutionRef::Job { job, attempt } => {
            query
                .from_as("runtime_job_attempts", "a")
                .join_as(
                    JoinType::InnerJoin,
                    "runtime_jobs",
                    "j",
                    Expr::col(("j", "job_id")).eq(Expr::col(("a", "job_id"))),
                )
                .and_where(Expr::col(("a", "attempt_id")).eq(attempt.as_str()))
                .and_where(Expr::col(("a", "job_id")).eq(job.as_str()))
                .and_where(Expr::col(("a", "node_id")).eq(node))
                .and_where(Expr::col(("j", "node_id")).eq(node));
        }
    }
    Ok(c.one(&query, |_| Ok(()))?.is_some())
}

/// An unrecorded command may have been sent directly by the Hub.
pub fn operation_is_foreign(
    c: &mut impl Read,
    node: &str,
    operation: &OperationId,
) -> Result<bool> {
    Ok(c.one(
        Query::select()
            .column("operation_id")
            .from("node_commands")
            .and_where(Expr::col("operation_id").eq(operation.as_str()))
            .and_where(Expr::col("node_id").ne(node)),
        |_| Ok(()),
    )?
    .is_some())
}

pub struct HandoverCandidate {
    pub session: String,
    pub node: String,
    pub name: String,
    pub last_seen: i64,
}

pub fn handover_candidates(c: &mut impl Read) -> Result<Vec<HandoverCandidate>> {
    c.all(
        Query::select()
            .columns([("r", "session_id"), ("n", "node_id"), ("n", "name")])
            .expr(Func::coalesce([
                Expr::col(("n", "last_seen")),
                Expr::col(("n", "created")),
            ]))
            .from_as("runtime_sessions", "r")
            .join_as(
                JoinType::InnerJoin,
                "nodes",
                "n",
                Expr::col(("n", "node_id")).eq(Expr::col(("r", "node_id"))),
            )
            .join_as(
                JoinType::InnerJoin,
                "sessions",
                "s",
                Expr::col(("s", "id")).eq(Expr::col(("r", "session_id"))),
            )
            .and_where(Expr::col(("n", "local")).eq(false))
            .and_where(Expr::col(("n", "state")).is_in(["active", "paused"]))
            .and_where(Expr::col(("s", "status")).ne("ended"))
            .and_where(Expr::exists(
                Query::select()
                    .column(("q", "session_id"))
                    .from_as("queued", "q")
                    .and_where(Expr::col(("q", "session_id")).eq(Expr::col(("r", "session_id"))))
                    .take(),
            )),
        |r| {
            Ok(HandoverCandidate {
                session: r.get(0)?,
                node: r.get(1)?,
                name: r.get(2)?,
                last_seen: r.get(3)?,
            })
        },
    )
}
