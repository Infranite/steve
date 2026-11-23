//! Relationships between Node jobs and their commands.

use sea_query::{Expr, ExprTrait, Func, JoinType, Query};

use crate::{Read, Result, StoreError};
use steve_node_api::{Completion, Observed, OperationResult, Receipt};

pub fn command_attempt(r: &mut impl Read, op: &str) -> Result<Option<String>> {
    Ok(r.one(
        Query::select()
            .column("target_job_attempt")
            .from("node_commands")
            .and_where(Expr::col("operation_id").eq(op)),
        |row| row.get::<Option<String>>(0),
    )?
    .flatten())
}

pub struct Start {
    pub operation: String,
    pub state: String,
    pub maybe_sent: bool,
}

pub fn start(r: &mut impl Read, attempt: &str) -> Result<Start> {
    r.one(
        Query::select()
            .columns([("c", "operation_id"), ("c", "state"), ("c", "maybe_sent")])
            .from_as("runtime_job_attempts", "a")
            .join_as(
                JoinType::InnerJoin,
                "node_commands",
                "c",
                Expr::col(("c", "operation_id")).eq(Expr::col(("a", "operation_id"))),
            )
            .and_where(Expr::col(("a", "attempt_id")).eq(attempt)),
        |row| {
            Ok(Start {
                operation: row.get(0)?,
                state: row.get(1)?,
                maybe_sent: row.get(2)?,
            })
        },
    )?
    .ok_or_else(|| StoreError::Rejected(format!("missing start command for job attempt {attempt}")))
}

pub fn terminal(r: &mut impl Read, op: &str) -> Result<bool> {
    r.one(
        Query::select()
            .expr(Expr::col("state").is_in(["finished", "rejected"]))
            .from("node_commands")
            .and_where(Expr::col("operation_id").eq(op)),
        |row| row.get(0),
    )?
    .ok_or_else(|| StoreError::Rejected(format!("missing start command {op}")))
}

pub fn stop_commands(r: &mut impl Read, method: &str, attempt: &str) -> Result<(i64, bool)> {
    let d = r.db().dialect();
    let live = Expr::col("state")
        .is_not_in(["finished", "rejected"])
        .or(d.json_field(Expr::col("receipt"), "success").eq(true));
    Ok(r.one(
        Query::select()
            .expr(Func::count(Expr::col("operation_id")))
            .expr(Func::coalesce([
                Func::max(live).into(),
                Expr::Constant(false.into()),
            ]))
            .from("node_commands")
            .and_where(Expr::col("method").eq(method))
            .and_where(Expr::col("target_job_attempt").eq(attempt)),
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?
    .unwrap_or_default())
}

pub fn completion(r: &mut impl Read, op: &str) -> Result<Option<Completion>> {
    let d = r.db().dialect();
    let json: Option<String> = r
        .one(
            Query::select()
                .expr(d.json_field(Expr::col("receipt"), "payload"))
                .from("node_commands")
                .and_where(Expr::col("operation_id").eq(op)),
            |row| row.get(0),
        )?
        .ok_or_else(|| StoreError::Rejected(format!("missing job command {op}")))?;
    Ok(json
        .and_then(|r| serde_json::from_str::<Receipt>(&r).ok())
        .and_then(|r| match r.state {
            Observed::Finished {
                result: Some(OperationResult::Execution(completion)),
                ..
            } => Some(completion),
            _ => None,
        }))
}

pub fn empty(r: &mut impl Read, node: &str) -> Result<Vec<String>> {
    let live = Query::select()
        .column(("a", "attempt_id"))
        .from_as("runtime_job_attempts", "a")
        .and_where(Expr::col(("a", "job_id")).eq(Expr::col(("j", "job_id"))))
        .and_where(Expr::col(("a", "state")).ne("finished"))
        .take();
    r.all(
        Query::select()
            .column(("j", "job_id"))
            .from_as("runtime_jobs", "j")
            .and_where(Expr::col(("j", "node_id")).eq(node))
            .and_where(Expr::col(("j", "state")).eq("open"))
            .and_where(Expr::exists(live).not()),
        |row| row.get(0),
    )
}

pub fn unavailable(r: &mut impl Read, attempt: &str) -> Result<bool> {
    let refused = Query::select()
        .column("operation_id")
        .from("node_commands")
        .and_where(Expr::col("target_job_attempt").eq(attempt))
        .and_where(Expr::col("method").is_in(["cancel", "abandon"]))
        .and_where(Expr::col("state").eq("rejected"))
        .take();
    let state = Expr::col("state");
    r.one(
        Query::select()
            .expr(
                state
                    .clone()
                    .eq("unknown")
                    .or(state.ne("finished").and(Expr::exists(refused))),
            )
            .from("runtime_job_attempts")
            .and_where(Expr::col("attempt_id").eq(attempt)),
        |row| row.get(0),
    )?
    .ok_or_else(|| StoreError::Rejected(format!("missing job attempt {attempt}")))
}
