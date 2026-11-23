use crate::{Read, Result};
use sea_query::{Expr, ExprTrait, Query};

pub type FileInfo = (String, String, String, String, i64);

pub fn file_row(c: &mut impl Read, id: &str) -> Result<Option<FileInfo>> {
    c.one(
        Query::select()
            .columns(["workspace_id", "sha256", "name", "mime", "size"])
            .from("files")
            .and_where(Expr::col("id").eq(id)),
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
    )
}

/// Execution scoped to a live start command and owned by the calling Node.
pub fn operation_execution(
    r: &mut impl Read,
    node: &str,
    op: &str,
) -> Result<Option<steve_node_api::ExecutionRef>> {
    use sea_query::JoinType;

    let start = |method, attempts, target| {
        Query::select()
            .column(("a", "execution"))
            .from_as("node_commands", "c")
            .join_as(
                JoinType::InnerJoin,
                attempts,
                "a",
                Expr::col(("a", "attempt_id"))
                    .eq(Expr::col(("c", target)))
                    .and(Expr::col(("a", "operation_id")).eq(Expr::col(("c", "operation_id")))),
            )
            .and_where(Expr::col(("c", "operation_id")).eq(op))
            .and_where(Expr::col(("c", "node_id")).eq(node))
            .and_where(Expr::col(("a", "node_id")).eq(node))
            .and_where(Expr::col(("c", "method")).eq(method))
            .and_where(Expr::col(("c", "state")).is_not_in(["finished", "rejected", "unknown"]))
            .take()
    };

    if let Some(execution) = r.one(
        &start("start_turn", "runtime_attempts", "target_attempt"),
        |row| row.get::<super::Json<_>>(0),
    )? {
        return Ok(Some(execution.0));
    }
    Ok(r.one(
        &start("start_job", "runtime_job_attempts", "target_job_attempt"),
        |row| row.get::<super::Json<_>>(0),
    )?
    .map(|value| value.0))
}

pub struct JobInputs {
    pub execution: steve_node_api::ExecutionRef,
    pub spec: steve_node_api::JobSpec,
    pub workspace: String,
}

pub fn job_inputs(
    r: &mut impl Read,
    node: &str,
    job: &steve_node_api::JobId,
    attempt: &steve_node_api::AttemptId,
) -> Result<Option<JobInputs>> {
    r.one(
        Query::select()
            .columns([("a", "execution"), ("a", "spec"), ("j", "workspace_id")])
            .from_as("runtime_job_attempts", "a")
            .join_as(
                sea_query::JoinType::InnerJoin,
                "runtime_jobs",
                "j",
                Expr::col(("a", "job_id")).eq(Expr::col(("j", "job_id"))),
            )
            .and_where(Expr::col(("a", "attempt_id")).eq(attempt.as_str()))
            .and_where(Expr::col(("a", "job_id")).eq(job.as_str()))
            .and_where(Expr::col(("a", "node_id")).eq(node))
            .and_where(Expr::col(("j", "node_id")).eq(node))
            .and_where(Expr::col(("j", "state")).eq("open"))
            .and_where(Expr::col(("a", "cancel_requested")).eq(false))
            .and_where(Expr::col(("a", "state")).is_not_in(["finished", "unknown"])),
        |row| {
            Ok(JobInputs {
                execution: row.get::<super::Json<_>>(0)?.0,
                spec: row.get::<super::Json<_>>(1)?.0,
                workspace: row.get(2)?,
            })
        },
    )
}
