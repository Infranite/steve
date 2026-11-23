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

pub struct DeliveryMetadata {
    pub name: String,
    pub mime: String,
    pub entrypoint: Option<String>,
    pub sha256: String,
}

pub fn item_delivery(c: &mut impl Read, item: &str, url: &str) -> Result<Option<DeliveryMetadata>> {
    let Some(ws) = super::items::item_workspace(c, item)? else {
        return Ok(None);
    };
    c.one(
        Query::select()
            .columns(["name", "mime", "entrypoint", "sha256"])
            .from("deliveries")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("item_id").eq(item))
            .and_where(Expr::col("url").eq(url)),
        |r| {
            Ok(DeliveryMetadata {
                name: r.get(0)?,
                mime: r.get(1)?,
                entrypoint: r.get(2)?,
                sha256: r.get(3)?,
            })
        },
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
