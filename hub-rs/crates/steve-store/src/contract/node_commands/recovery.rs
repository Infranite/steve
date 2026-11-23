//! Durable recovery state and target lookup for Node control commands.

use sea_query::{Expr, ExprTrait, Func, JoinType, Order, Query};

use crate::{Read, Result, StoreError};

use super::dispatch::retired;

const TABLE: &str = "node_commands";

pub fn live_targets(r: &mut impl Read, node: &str) -> Result<Vec<String>> {
    let d = r.db().dialect();
    r.all(
        Query::select()
            .expr(d.json_path(Expr::col("request"), "$.body.target"))
            .from(TABLE)
            .and_where(Expr::col("node_id").eq(node))
            .and_where(Expr::col("method").eq("recover"))
            .and_where(Expr::col("state").is_in(["pending", "accepted", "active"])),
        |row| row.get(0),
    )
}

pub struct UnknownCommand {
    pub id: String,
    pub method: String,
    pub request: String,
    pub attempt: Option<String>,
}

pub fn unknown(r: &mut impl Read, node: &str) -> Result<Vec<UnknownCommand>> {
    let d = r.db().dialect();
    r.all(
        Query::select()
            .columns([
                ("c", "operation_id"),
                ("c", "method"),
                ("c", "request"),
                ("c", "target_attempt"),
            ])
            .from_as(TABLE, "c")
            .and_where(Expr::col(("c", "node_id")).eq(node))
            .and_where(Expr::col(("c", "state")).eq("unknown"))
            .and_where(retired(d).not())
            .order_by(("c", "created"), Order::Asc)
            .order_by_expr(d.insertion_order(Some("c")), Order::Asc),
        |row| {
            Ok(UnknownCommand {
                id: row.get(0)?,
                method: row.get(1)?,
                request: row.get(2)?,
                attempt: row.get(3)?,
            })
        },
    )
}

fn by_target(d: crate::contract::Dialect, target: &str) -> sea_query::SelectStatement {
    Query::select()
        .from(TABLE)
        .and_where(Expr::col("method").eq("recover"))
        .and_where(
            d.json_path(Expr::col("request"), "$.body.target")
                .eq(target),
        )
        .take()
}

pub fn count(r: &mut impl Read, target: &str) -> Result<i64> {
    let d = r.db().dialect();
    Ok(r.one(
        by_target(d, target).expr(Func::count(Expr::col("operation_id"))),
        |row| row.get(0),
    )?
    .unwrap_or_default())
}

pub fn unknown_exec(r: &mut impl Read, node: &str, op: &str) -> Result<Option<(String, String)>> {
    r.one(
        Query::select()
            .columns(["method", "request"])
            .from(TABLE)
            .and_where(Expr::col("operation_id").eq(op))
            .and_where(Expr::col("node_id").eq(node))
            .and_where(Expr::col("method").is_in(["exec", "git_apply"]))
            .and_where(Expr::col("state").eq("unknown")),
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
}

fn finished(d: crate::contract::Dialect) -> Expr {
    Expr::col("state")
        .eq("finished")
        .and(d.json_field(Expr::col("receipt"), "success").eq(true))
}

pub fn exec_recovers(r: &mut impl Read, node: &str, target: &str) -> Result<(bool, i64)> {
    let d = r.db().dialect();
    let live = Expr::col("state")
        .is_in(["pending", "accepted", "active", "unknown"])
        .or(finished(d));
    Ok(r.one(
        by_target(d, target)
            .expr(Func::coalesce([
                Func::max(live).into(),
                Expr::Constant(false.into()),
            ]))
            .expr(Func::count(Expr::col("operation_id")))
            .and_where(Expr::col("node_id").eq(node)),
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?
    .unwrap_or_default())
}

pub fn retiring(r: &mut impl Read, node: &str, target: &str) -> Result<bool> {
    let d = r.db().dialect();
    let pending = Expr::col("state").eq("pending").and(
        Expr::col("maybe_sent")
            .eq(true)
            .or(Expr::col("lease_owner").is_not_null()),
    );
    Ok(r.one(
        by_target(d, target)
            .column("operation_id")
            .and_where(Expr::col("node_id").eq(node))
            .and_where(
                Expr::col("state")
                    .is_in(["accepted", "active", "unknown"])
                    .or(pending)
                    .or(finished(d)),
            )
            .limit(1),
        |_| Ok(()),
    )?
    .is_some())
}

pub fn target(r: &mut impl Read, op: &str) -> Result<String> {
    let d = r.db().dialect();
    r.one(
        Query::select()
            .expr(d.json_path(Expr::col("request"), "$.body.target"))
            .from(TABLE)
            .and_where(Expr::col("operation_id").eq(op)),
        |row| row.get(0),
    )?
    .ok_or_else(|| StoreError::Rejected(format!("missing recover {op}")))
}

pub fn retired_target(r: &mut impl Read, op: &str) -> Result<(String, String)> {
    let d = r.db().dialect();
    r.one(
        Query::select()
            .columns([("c", "operation_id"), ("c", "method")])
            .from_as(TABLE, "r")
            .join_as(
                JoinType::InnerJoin,
                TABLE,
                "c",
                Expr::col(("c", "operation_id"))
                    .eq(d.json_path(Expr::col(("r", "request")), "$.body.target")),
            )
            .and_where(Expr::col(("r", "operation_id")).eq(op)),
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?
    .ok_or_else(|| StoreError::Rejected(format!("missing target of recover {op}")))
}

pub fn request(r: &mut impl Read, op: &str) -> Result<String> {
    r.one(
        Query::select()
            .column("request")
            .from(TABLE)
            .and_where(Expr::col("operation_id").eq(op)),
        |row| row.get(0),
    )?
    .ok_or_else(|| StoreError::Rejected(format!("missing command {op}")))
}
