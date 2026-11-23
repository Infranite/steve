//! Audit entries and revision history written with their domain changes.

use sea_query::{Expr, ExprTrait, InsertStatement, Order, Query};
use serde::Serialize;
use serde_json::{Value, json};

use crate::repo::to_json;
use crate::{Read, Result, W, now};

pub fn record(
    w: &mut W<'_>,
    ws: &str,
    actor: &str,
    action: &str,
    target: &str,
    detail: &Value,
) -> Result<()> {
    w.exec(&record_statement(ws, actor, action, target, detail)?)?;
    Ok(())
}

pub(crate) fn record_statement(
    ws: &str,
    actor: &str,
    action: &str,
    target: &str,
    detail: &Value,
) -> Result<InsertStatement> {
    Ok(Query::insert()
        .into_table("audit")
        .columns(["workspace_id", "ts", "actor", "action", "target", "detail"])
        .values_panic([
            ws.into(),
            now().into(),
            actor.into(),
            action.into(),
            target.into(),
            to_json(detail)?.into(),
        ])
        .take())
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
    let patch = json!({ "before": before, "after": after });
    w.exec(&revision_statement(ws, kind, id, version, &patch, author)?)?;
    Ok(())
}

pub(crate) fn revision_statement(
    ws: &str,
    kind: &str,
    id: &str,
    version: i64,
    patch: &impl Serialize,
    author: &str,
) -> Result<InsertStatement> {
    Ok(Query::insert()
        .into_table("revisions")
        .columns([
            "workspace_id",
            "object_type",
            "object_id",
            "version",
            "patch",
            "author",
            "created",
        ])
        .values_panic([
            ws.into(),
            kind.into(),
            id.into(),
            version.into(),
            to_json(patch)?.into(),
            author.into(),
            now().into(),
        ])
        .take())
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

/// The version following the latest revision of this object's history.
pub fn next_revision(r: &mut impl Read, ws: &str, kind: &str, id: &str) -> Result<i64> {
    let previous: Option<i64> = r.one(
        Query::select()
            .column("version")
            .from("revisions")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("object_type").eq(kind))
            .and_where(Expr::col("object_id").eq(id))
            .order_by("id", Order::Desc)
            .limit(1),
        |row| row.get(0),
    )?;
    Ok(previous.unwrap_or_default() + 1)
}
