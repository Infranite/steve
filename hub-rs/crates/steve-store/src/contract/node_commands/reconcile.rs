//! Commands compared against the Node's reconnect snapshot.

use sea_query::{Expr, ExprTrait, Order, Query};

use crate::{Read, Result, StoreError};

pub struct Command {
    pub operation: String,
    pub state: String,
    pub method: String,
    pub request: steve_node_api::Command<serde_json::Value>,
}

pub fn unsettled(r: &mut impl Read, node: &str) -> Result<Vec<Command>> {
    let order = r.db().dialect().insertion_order(None);
    r.all(
        Query::select()
            .columns(["operation_id", "state", "method", "request"])
            .from("node_commands")
            .and_where(Expr::col("node_id").eq(node))
            .and_where(Expr::col("state").is_in(["pending", "accepted", "active", "unknown"]))
            .order_by("created", Order::Asc)
            .order_by_expr(order, Order::Asc),
        |row| {
            Ok(Command {
                operation: row.get(0)?,
                state: row.get(1)?,
                method: row.get(2)?,
                request: row.get::<crate::contract::Json<_>>(3)?.0,
            })
        },
    )
}

pub fn required_state(r: &mut impl Read, node: &str, op: &str) -> Result<String> {
    super::state(r, node, op)?.ok_or_else(|| StoreError::Rejected(format!("missing command {op}")))
}

pub fn latest_opened(r: &mut impl Read, session: &str) -> Result<Option<steve_node_api::Revision>> {
    let d = r.db().dialect();
    let raw = r
        .one(
            Query::select()
                .expr(d.json_field(Expr::col("receipt"), "payload"))
                .from("node_commands")
                .and_where(Expr::col("method").eq("open_session"))
                .and_where(
                    d.json_path(Expr::col("request"), "$.body.session")
                        .eq(session),
                )
                .and_where(Expr::col("state").eq("finished"))
                .and_where(d.json_field(Expr::col("receipt"), "success").eq(true))
                .order_by("created", Order::Desc)
                .order_by_expr(d.insertion_order(None), Order::Desc)
                .limit(1),
            |row| row.get::<Option<String>>(0),
        )?
        .flatten();
    let revision = raw
        .and_then(|payload| serde_json::from_str::<steve_node_api::Receipt>(&payload).ok())
        .and_then(|receipt| match receipt.state {
            steve_node_api::Observed::Finished {
                result: Some(steve_node_api::OperationResult::Session(view)),
                ..
            } => Some(view.revision),
            _ => None,
        });
    Ok(revision)
}
