//! Hub-side Node session bindings and execution ownership.

use sea_query::{Expr, ExprTrait, Func, JoinType, Query};
use steve_node_api::{ExecutionRef, SessionId};

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

/// The latest in-flight state of a bound session.
pub struct Phase {
    pub name: String,
    pub active_attempt: Option<String>,
}

pub fn phase(c: &mut impl Read, session: &str, node: Option<&str>) -> Result<Option<Phase>> {
    let mut q = Query::select();
    q.columns(["phase", "active_attempt"])
        .from("runtime_sessions")
        .and_where(Expr::col("session_id").eq(session));
    if let Some(node) = node {
        q.and_where(Expr::col("node_id").eq(node));
    }
    c.one(&q, |r| {
        Ok(Phase {
            name: r.get(0)?,
            active_attempt: r.get(1)?,
        })
    })
}

pub fn unfinished(c: &mut impl Read, session: &str) -> Result<bool> {
    Ok(c.one(
        Query::select()
            .column("turn_id")
            .from("runtime_turns")
            .and_where(Expr::col("session_id").eq(session))
            .and_where(Expr::col("state").ne("finished"))
            .limit(1),
        |_| Ok(()),
    )?
    .is_some())
}

pub fn configuring_on(c: &mut impl Read, node: &str) -> Result<Vec<String>> {
    c.all(
        Query::select()
            .column("session_id")
            .from("runtime_sessions")
            .and_where(Expr::col("node_id").eq(node))
            .and_where(Expr::col("phase").eq("configuring")),
        |r| r.get(0),
    )
}

pub fn node_revision(c: &mut impl Read, session: &str) -> Result<Option<steve_node_api::Revision>> {
    Ok(c.one(
        Query::select()
            .column("node_revision")
            .from("runtime_sessions")
            .and_where(Expr::col("session_id").eq(session)),
        |r| r.get::<Option<String>>(0),
    )?
    .flatten()
    .and_then(|s| s.try_into().ok()))
}

/// The desired harness configuration, pending a Node command at an idle boundary.
pub fn desired_config<T: serde::de::DeserializeOwned>(
    c: &mut impl Read,
    session: &str,
) -> Result<Option<T>> {
    Ok(c.one(
        Query::select()
            .column("desired_config")
            .from("runtime_sessions")
            .and_where(Expr::col("session_id").eq(session)),
        |r| r.get::<Option<super::Json<T>>>(0),
    )?
    .flatten()
    .map(|v| v.0))
}

/// Changing desired configuration leaves the session's phase revision alone.
pub fn set_desired_config<T: serde::Serialize>(
    w: &mut crate::W<'_>,
    session: &str,
    config: Option<&T>,
    at: i64,
) -> Result<()> {
    let config = config.map(serde_json::to_string).transpose()?;
    w.exec(
        Query::update()
            .table("runtime_sessions")
            .value("desired_config", config)
            .value("updated", at)
            .and_where(Expr::col("session_id").eq(session)),
    )?;
    Ok(())
}

pub fn ended(c: &mut impl Read, session: &str) -> Result<bool> {
    Ok(c.one(
        Query::select()
            .column("id")
            .from("sessions")
            .and_where(Expr::col("id").eq(session))
            .and_where(Expr::col("status").eq("ended")),
        |_| Ok(()),
    )?
    .is_some())
}

pub fn faulted(c: &mut impl Read, node: &str, session: &str, stream: &str) -> Result<bool> {
    Ok(c.one(
        Query::select()
            .column("stream")
            .from("node_streams")
            .and_where(Expr::col("node_id").eq(node))
            .and_where(Expr::col("fault").is_not_null())
            .and_where(
                Expr::col("session_id")
                    .eq(session)
                    .or(Expr::col("stream").eq(stream)),
            )
            .limit(1),
        |_| Ok(()),
    )?
    .is_some())
}

pub fn uncertain_turn(c: &mut impl Read, session: &str) -> Result<bool> {
    Ok(c.one(
        Query::select()
            .column(("t", "turn_id"))
            .from_as("runtime_turns", "t")
            .join_as(
                JoinType::LeftJoin,
                "runtime_attempts",
                "a",
                Expr::col(("a", "turn_id"))
                    .eq(Expr::col(("t", "turn_id")))
                    .and(Expr::col(("a", "state")).ne("finished")),
            )
            .and_where(Expr::col(("t", "session_id")).eq(session))
            .and_where(Expr::col(("t", "state")).ne("finished"))
            .and_where(
                Expr::col(("t", "state"))
                    .eq("unknown")
                    .or(Expr::col(("a", "state")).eq("unknown")),
            )
            .limit(1),
        |_| Ok(()),
    )?
    .is_some())
}

pub fn revision(c: &mut impl Read, session: &str) -> Result<i64> {
    c.one(
        Query::select()
            .column("revision")
            .from("runtime_sessions")
            .and_where(Expr::col("session_id").eq(session)),
        |r| r.get(0),
    )?
    .ok_or_else(|| crate::StoreError::Rejected(format!("missing runtime session {session}")))
}

/// A session the business side ended stays ended.
pub fn set_status(w: &mut crate::W<'_>, session: &str, status: &str, at: i64) -> Result<()> {
    let changed = w.exec(
        Query::update()
            .table("sessions")
            .value("status", status)
            .value("updated", at)
            .and_where(Expr::col("id").eq(session))
            .and_where(Expr::col("status").is_not_in(["ended", status])),
    )?;
    if changed > 0 {
        changed_event(w, session)?;
    }
    Ok(())
}

pub fn changed_event(w: &mut crate::W<'_>, session: &str) -> Result<()> {
    use super::Q;
    let ws: String = w
        .one(
            Query::select()
                .column("workspace_id")
                .from("sessions")
                .and_where(Expr::col("id").eq(session)),
            |r| r.get(0),
        )?
        .ok_or_else(|| crate::StoreError::Rejected(format!("missing session {session}")))?;
    w.emit(steve_types::DomainEvent::from(
        steve_types::WorkEvent::Session {
            ws: ws.into(),
            id: session.to_string().into(),
            item_id: None,
        },
    ));
    Ok(())
}

pub fn queue(
    w: &mut crate::W<'_>,
    id: &str,
    session: &str,
    text: &str,
    via: &str,
    files: &[crate::FileRef],
    created: i64,
) -> Result<()> {
    let files = serde_json::to_string(files)?;
    w.exec(
        Query::insert()
            .into_table("queued")
            .columns(["id", "session_id", "text", "via", "created", "files"])
            .values_panic([
                id.into(),
                session.into(),
                text.into(),
                via.into(),
                created.into(),
                files.into(),
            ]),
    )?;
    Ok(())
}

pub fn has_current_park_notice(c: &mut impl Read, session: &str) -> Result<bool> {
    let boundary = Query::select()
        .expr(Func::coalesce([
            Func::max(Expr::col("seq")).into(),
            Expr::Constant(0_i64.into()),
        ]))
        .from("events")
        .and_where(Expr::col("session_id").eq(session))
        .and_where(Expr::col("kind").is_in(["user", "turn_end"]))
        .take();
    let parked = c.db().dialect().json_field(Expr::col("data"), "parked");
    Ok(c.one(
        Query::select()
            .column("seq")
            .from("events")
            .and_where(Expr::col("session_id").eq(session))
            .and_where(Expr::col("kind").eq("system"))
            .and_where(parked.eq(true))
            .and_where(Expr::col("seq").gt(Expr::from(boundary)))
            .limit(1),
        |_| Ok(()),
    )?
    .is_some())
}

pub fn clear_queue(w: &mut crate::W<'_>, session: &str) -> Result<()> {
    w.exec(
        Query::delete()
            .from_table("queued")
            .and_where(Expr::col("session_id").eq(session)),
    )?;
    Ok(())
}

pub fn workspace_ref(c: &mut impl Read, session: &str) -> Result<Option<String>> {
    c.one(
        Query::select()
            .column("workspace_ref")
            .from("runtime_sessions")
            .and_where(Expr::col("session_id").eq(session)),
        |r| r.get(0),
    )
}

pub mod observations;
pub mod startup;

pub mod lifecycle;
pub mod planning;
pub mod reconcile;
pub mod worktree;
