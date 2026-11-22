//! Audit entries and revision history written with their domain changes.

use sea_query::{InsertStatement, Query};
use serde::Serialize;
use serde_json::{Value, json};

use crate::repo::to_json;
use crate::{Result, W, now};

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
