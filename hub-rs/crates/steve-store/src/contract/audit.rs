//! Audit entries and revision history written with their domain changes.

use sea_query::{Expr, ExprTrait, Order, Query};
use serde_json::{Value, json};

use crate::{Read, Result, Revisions, W};

pub fn record(
    w: &mut W<'_>,
    ws: &str,
    actor: &str,
    action: &str,
    target: &str,
    detail: &Value,
) -> Result<()> {
    w.audit(ws, actor, action, target, detail)
}

/// Record the version assigned by the owner of the revised object in the same write.
#[allow(clippy::too_many_arguments)]
pub fn revise(
    w: &mut W<'_>,
    ws: &str,
    kind: &str,
    id: &str,
    version: i64,
    before: &Value,
    after: &Value,
    author: &str,
) -> Result<()> {
    Revisions(w).append(
        ws,
        kind,
        id,
        version,
        &json!({ "before": before, "after": after }),
        author,
    )
}

/// Provider explanation captured by the latest failed send of this message.
pub fn channel_send_error(r: &mut impl Read, ws: &str, message: &str) -> Result<Option<String>> {
    let error = r.db().dialect().json_field(Expr::col("detail"), "error");
    Ok(r.one(
        Query::select()
            .expr(error)
            .from("audit")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("target").eq(message))
            .and_where(Expr::col("action").eq("chan_message.send.failed"))
            .order_by("id", Order::Desc)
            .limit(1),
        |row| row.get(0),
    )?
    .flatten())
}

pub fn channel_send_blocked(r: &mut impl Read, ws: &str, message: &str) -> Result<bool> {
    Ok(r.one(
        Query::select()
            .column("id")
            .from("audit")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("target").eq(message))
            .and_where(Expr::col("action").eq("chan_message.send.blocked"))
            .limit(1),
        |_| Ok(()),
    )?
    .is_some())
}
