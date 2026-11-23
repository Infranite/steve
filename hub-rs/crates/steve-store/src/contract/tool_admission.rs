//! Frozen execution identities used to authorize Node tools and attachments.

use sea_query::{Expr, ExprTrait, JoinType, Query};
use steve_node_api::{
    AttemptId, ExecutionRef, JobId, JobSpec, NodeId, SessionId, TurnId, TurnSpec,
};

use super::Json;
use crate::{Read, Result};

pub struct TurnExecution {
    pub execution: ExecutionRef,
    pub spec: TurnSpec,
    pub state: String,
    pub cancelled: bool,
    pub session_attempt: Option<String>,
    pub turn_attempt: Option<String>,
}

pub fn turn_execution(
    r: &mut impl Read,
    node: &NodeId,
    session: &SessionId,
    turn: &TurnId,
    attempt: &AttemptId,
) -> Result<Option<TurnExecution>> {
    r.one(
        Query::select()
            .columns([
                ("a", "execution"),
                ("a", "spec"),
                ("a", "state"),
                ("a", "cancel_requested"),
                ("s", "active_attempt"),
                ("t", "active_attempt"),
            ])
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
            .and_where(Expr::col(("a", "node_id")).eq(node.as_str()))
            .and_where(Expr::col(("s", "node_id")).eq(node.as_str())),
        |row| {
            Ok(TurnExecution {
                execution: row.get::<Json<_>>(0)?.0,
                spec: row.get::<Json<_>>(1)?.0,
                state: row.get(2)?,
                cancelled: row.get(3)?,
                session_attempt: row.get(4)?,
                turn_attempt: row.get(5)?,
            })
        },
    )
}

pub struct JobExecution {
    pub execution: ExecutionRef,
    pub spec: JobSpec,
    pub state: String,
    pub cancelled: bool,
    pub kind: String,
    pub run: Option<String>,
    pub workspace: String,
    pub job_state: String,
}

pub fn job_execution(
    r: &mut impl Read,
    node: &NodeId,
    job: &JobId,
    attempt: &AttemptId,
    live_only: bool,
) -> Result<Option<JobExecution>> {
    let mut q = Query::select();
    q.columns([
        ("a", "execution"),
        ("a", "spec"),
        ("a", "state"),
        ("a", "cancel_requested"),
        ("j", "kind"),
        ("j", "thread_run_id"),
        ("j", "workspace_id"),
        ("j", "state"),
    ])
    .from_as("runtime_job_attempts", "a")
    .join_as(
        JoinType::InnerJoin,
        "runtime_jobs",
        "j",
        Expr::col(("j", "job_id")).eq(Expr::col(("a", "job_id"))),
    )
    .and_where(Expr::col(("a", "attempt_id")).eq(attempt.as_str()))
    .and_where(Expr::col(("a", "job_id")).eq(job.as_str()))
    .and_where(Expr::col(("a", "node_id")).eq(node.as_str()))
    .and_where(Expr::col(("j", "node_id")).eq(node.as_str()));
    if live_only {
        q.and_where(Expr::col(("j", "state")).eq("open"))
            .and_where(Expr::col(("a", "cancel_requested")).eq(false))
            .and_where(Expr::col(("a", "state")).is_not_in(["finished", "unknown"]));
    }
    r.one(&q, |row| {
        Ok(JobExecution {
            execution: row.get::<Json<_>>(0)?.0,
            spec: row.get::<Json<_>>(1)?.0,
            state: row.get(2)?,
            cancelled: row.get(3)?,
            kind: row.get(4)?,
            run: row.get(5)?,
            workspace: row.get(6)?,
            job_state: row.get(7)?,
        })
    })
}
