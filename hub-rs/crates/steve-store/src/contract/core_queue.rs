//! Durable work on a thread's routing lane and the held messages it owns.

use sea_query::{Condition, Expr, ExprTrait, OnConflict, Order, Query, SubQueryStatement};

use crate::contract::Dialect;
use crate::{Read, Result, W};

const PREFIX: &str = "thread.pending.";

#[derive(Debug)]
pub struct PendingJob {
    pub workspace: String,
    pub key: String,
    pub payload: String,
}

pub fn pending_key(message: &str) -> String {
    format!("{PREFIX}{message}")
}

fn pending_keys() -> Condition {
    // A case-sensitive index range on the key, also when the caller filters by workspace.
    Condition::all()
        .add(Expr::col("key").gte(PREFIX))
        .add(Expr::col("key").lt("thread.pending/"))
}

fn valid_json_field(dialect: Dialect, value: Expr, field: &'static str) -> Expr {
    Expr::cust_with_exprs(
        "CASE WHEN json_valid(?) THEN ? END",
        [value.clone(), dialect.json_field(value, field)],
    )
}

pub fn pending(c: &mut impl Read, ws: &str, message: &str) -> Result<Option<String>> {
    c.one(
        Query::select()
            .column("value")
            .from("settings")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("key").eq(pending_key(message))),
        |row| row.get(0),
    )
}

pub fn has_pending_in_space(c: &mut impl Read, ws: &str, space: &str) -> Result<bool> {
    let space_of = valid_json_field(c.db().dialect(), Expr::col("value"), "conv.space");
    c.one(
        Query::select()
            .column("key")
            .from("settings")
            .and_where(Expr::col("workspace_id").eq(ws))
            .cond_where(pending_keys())
            .and_where(space_of.eq(space))
            .limit(1),
        |_| Ok(()),
    )
    .map(|row| row.is_some())
}

pub fn pending_jobs(c: &mut impl Read) -> Result<Vec<PendingJob>> {
    c.all(
        Query::select()
            .columns(["workspace_id", "key", "value"])
            .from("settings")
            .cond_where(pending_keys())
            .order_by("updated", Order::Asc),
        |row| {
            Ok(PendingJob {
                workspace: row.get(0)?,
                key: row.get(1)?,
                payload: row.get(2)?,
            })
        },
    )
}

pub fn enqueue(w: &mut W<'_>, ws: &str, message: &str, payload: &str, at: i64) -> Result<()> {
    w.exec(
        Query::insert()
            .into_table("settings")
            .columns(["workspace_id", "key", "value", "updated"])
            .values_panic([
                ws.into(),
                pending_key(message).into(),
                payload.into(),
                at.into(),
            ])
            .on_conflict(
                OnConflict::columns(["workspace_id", "key"])
                    .do_nothing()
                    .to_owned(),
            ),
    )?;
    Ok(())
}

pub fn replace_pending(w: &mut W<'_>, ws: &str, message: &str, payload: &str) -> Result<()> {
    w.exec(
        Query::update()
            .table("settings")
            .value("value", payload)
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("key").eq(pending_key(message))),
    )?;
    Ok(())
}

pub fn remove_pending(w: &mut W<'_>, ws: &str, message: &str) -> Result<()> {
    w.exec(
        Query::delete()
            .from_table("settings")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("key").eq(pending_key(message))),
    )?;
    Ok(())
}

pub fn release_held(w: &mut W<'_>, ws: &str, message: &str, at: i64) -> Result<bool> {
    let dialect = w.dialect();
    let held = valid_json_field(dialect, Expr::col("meta"), "held");
    let latest: SubQueryStatement = Query::select()
        .expr(Expr::cust_with_expr(
            "MAX(?) + 1",
            Expr::col(("m", "created")),
        ))
        .from_as("messages", "m")
        .and_where(Expr::col(("m", "workspace_id")).eq(Expr::col(("messages", "workspace_id"))))
        .and_where(Expr::col(("m", "space")).eq(Expr::col(("messages", "space"))))
        .take()
        .into();
    Ok(w.exec(
        Query::update()
            .table("messages")
            .value(
                "created",
                dialect.greatest(Expr::val(at), Expr::from(latest)),
            )
            .value(
                "meta",
                dialect.json_merge_patch(Expr::col("meta"), Expr::val(r#"{"held":null}"#)),
            )
            .and_where(Expr::col("id").eq(message))
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(held.eq(1_i64)),
    )? > 0)
}

pub fn edit_message(
    w: &mut W<'_>,
    ws: &str,
    message: &str,
    text: &str,
    meta: Option<&str>,
) -> Result<()> {
    let mut update = Query::update();
    update
        .table("messages")
        .value("text", text)
        .and_where(Expr::col("id").eq(message))
        .and_where(Expr::col("workspace_id").eq(ws));
    if let Some(meta) = meta {
        update.value("meta", meta);
    }
    w.exec(&update)?;
    Ok(())
}

pub fn remove_message(w: &mut W<'_>, ws: &str, message: &str) -> Result<()> {
    w.exec(
        Query::delete()
            .from_table("messages")
            .and_where(Expr::col("id").eq(message))
            .and_where(Expr::col("workspace_id").eq(ws)),
    )?;
    Ok(())
}
