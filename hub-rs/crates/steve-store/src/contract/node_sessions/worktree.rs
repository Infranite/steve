//! Bindings and durable worktree operations for a Node session.

use sea_query::{Expr, ExprTrait, JoinType, Order, Query};
use steve_node_api::{
    Command, Observed, OperationResult, Prepare, Receipt, WorkspaceRef, WorkspaceView,
};

use crate::{Read, Result};

use super::super::Json;

pub struct Binding {
    pub workspace: WorkspaceRef,
    pub item: Option<String>,
}

pub fn binding(r: &mut impl Read, session: &str, node: &str) -> Result<Option<Binding>> {
    r.one(
        Query::select()
            .columns([("r", "workspace_ref"), ("s", "item_id")])
            .from_as("runtime_sessions", "r")
            .join_as(
                JoinType::InnerJoin,
                "sessions",
                "s",
                Expr::col(("s", "id")).eq(Expr::col(("r", "session_id"))),
            )
            .and_where(Expr::col(("r", "session_id")).eq(session))
            .and_where(Expr::col(("r", "node_id")).eq(node)),
        |row| {
            Ok(Binding {
                workspace: row.get::<Json<_>>(0)?.0,
                item: row.get(1)?,
            })
        },
    )
}

pub fn initial_prepare(r: &mut impl Read, session: &str, node: &str) -> Result<Option<Prepare>> {
    Ok(r.one(
        Query::select()
            .column("request")
            .from("node_commands")
            .and_where(Expr::col("operation_id").eq(format!("op_prep_{session}")))
            .and_where(Expr::col("node_id").eq(node))
            .and_where(Expr::col("method").eq("prepare_workspace")),
        |row| Ok(row.get::<Json<Command<Prepare>>>(0)?.0.body),
    )?)
}

pub struct LastPrepare {
    pub rowid: i64,
    pub body: Prepare,
    pub state: String,
    pub success: bool,
    pub view: Option<WorkspaceView>,
}

#[derive(serde::Deserialize)]
struct PrepareReceipt {
    #[serde(default)]
    success: bool,
    payload: Option<serde_json::Value>,
}

pub fn last_prepare(r: &mut impl Read, node: &str, resource: &str) -> Result<Option<LastPrepare>> {
    let d = r.db().dialect();
    let position = d.insertion_order(None);
    r.one(
        Query::select()
            .expr(position.clone())
            .columns(["request", "state", "receipt"])
            .from("node_commands")
            .and_where(Expr::col("node_id").eq(node))
            .and_where(Expr::col("method").eq("prepare_workspace"))
            .and_where(
                d.json_path(Expr::col("request"), "$.body.resource")
                    .eq(resource),
            )
            .order_by_expr(position, Order::Desc)
            .limit(1),
        |row| {
            let receipt = row.get::<Option<Json<PrepareReceipt>>>(3)?.map(|v| v.0);
            let success = receipt.as_ref().is_some_and(|v| v.success);
            let view = receipt
                .and_then(|r| r.payload)
                .and_then(|payload| serde_json::from_value::<Receipt>(payload).ok())
                .and_then(|receipt| match receipt.state {
                    Observed::Finished {
                        result: Some(OperationResult::Workspace(view)),
                        ..
                    } => Some(view),
                    _ => None,
                });
            Ok(LastPrepare {
                rowid: row.get(0)?,
                body: row.get::<Json<Command<Prepare>>>(1)?.0.body,
                state: row.get(2)?,
                success,
                view,
            })
        },
    )
}

#[derive(PartialEq, Eq)]
pub enum Tidy {
    Removed,
    Kept,
}

pub fn tidy_since(
    r: &mut impl Read,
    node: &str,
    workspace: &str,
    rowid: i64,
) -> Result<Option<Tidy>> {
    let d = r.db().dialect();
    let mut commands = Query::select();
    commands
        .column("operation_id")
        .from("node_commands")
        .and_where(Expr::col("node_id").eq(node))
        .and_where(Expr::col("method").eq("tidy_workspace"))
        .and_where(
            d.json_path(Expr::col("request"), "$.body.workspace.id")
                .eq(workspace),
        )
        .and_where(d.insertion_order(None).gt(rowid))
        .limit(1);
    let success = d.json_field(Expr::col("receipt"), "success");
    let finished = Expr::col("state").eq("finished");
    let removed = r
        .one(
            commands
                .clone()
                .and_where(finished.clone().and(success.clone().eq(true))),
            |_| Ok(()),
        )?
        .is_some();
    if removed {
        return Ok(Some(Tidy::Removed));
    }
    let refused = Expr::col("state").eq("rejected").and(
        d.json_path(Expr::col("receipt"), "$.payload.retry")
            .eq("never"),
    );
    let failed = finished.and(success.eq(false)).and(
        d.json_path(Expr::col("receipt"), "$.payload.state.error.retry")
            .eq("never"),
    );
    let kept = r
        .one(
            commands.and_where(
                Expr::col("state")
                    .is_not_in(["finished", "rejected"])
                    .or(refused)
                    .or(failed),
            ),
            |_| Ok(()),
        )?
        .is_some();
    Ok(kept.then_some(Tidy::Kept))
}

pub fn item_paused(r: &mut impl Read, session: &str) -> Result<bool> {
    Ok(r.one(
        Query::select()
            .column(("i", "id"))
            .from_as("sessions", "s")
            .join_as(
                JoinType::InnerJoin,
                "items",
                "i",
                Expr::col(("i", "id")).eq(Expr::col(("s", "item_id"))),
            )
            .and_where(Expr::col(("s", "id")).eq(session))
            .and_where(Expr::col(("i", "status")).eq("pause")),
        |_| Ok(()),
    )?
    .is_some())
}

pub fn latest_item_session(r: &mut impl Read, node: &str, item: &str) -> Result<Option<String>> {
    r.one(
        Query::select()
            .column(("s", "id"))
            .from_as("sessions", "s")
            .join_as(
                JoinType::InnerJoin,
                "runtime_sessions",
                "r",
                Expr::col(("s", "id")).eq(Expr::col(("r", "session_id"))),
            )
            .and_where(Expr::col(("s", "item_id")).eq(item))
            .and_where(Expr::col(("r", "node_id")).eq(node))
            .order_by(("s", "created"), Order::Desc)
            .order_by(("s", "no"), Order::Desc)
            .limit(1),
        |row| row.get(0),
    )
}
