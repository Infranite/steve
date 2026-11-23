//! Usage and audit observations for Core's gateway and model settings tests.

use sea_query::{Asterisk, Expr, ExprTrait, Func, Order, Query};
use serde_json::Value;

use crate::contract::{Json, Q};
use crate::{Store, Tokens};

#[derive(Debug)]
pub struct Usage {
    pub ws: String,
    pub purpose: String,
    pub model: String,
    pub tokens: Tokens,
    pub cost: Option<f64>,
    pub ok: bool,
    pub attempts: i64,
    pub duration_ms: Option<i64>,
    pub item: Option<String>,
    pub detail: Value,
    pub source: String,
    pub harness: String,
}

pub fn usage(store: &Store, source: Option<&str>) -> Vec<Usage> {
    let mut query = Query::select();
    query
        .columns([
            "workspace_id",
            "purpose",
            "model",
            "input",
            "output",
            "cache_read",
            "cache_write",
            "reasoning",
            "cost_reported",
            "ok",
            "attempts",
            "item_id",
            "detail",
            "source",
            "harness",
            "duration_ms",
        ])
        .from("usage_records")
        .order_by("id", Order::Asc);
    if let Some(source) = source {
        query.and_where(Expr::col("source").eq(source));
    }
    store
        .read(|r| {
            r.all(&query, |row| {
                Ok(Usage {
                    ws: row.get(0)?,
                    purpose: row.get(1)?,
                    model: row.get(2)?,
                    tokens: Tokens {
                        input: row.get(3)?,
                        output: row.get(4)?,
                        cache_read: row.get(5)?,
                        cache_write: row.get(6)?,
                        reasoning: row.get(7)?,
                    },
                    cost: row.get(8)?,
                    ok: row.get::<i64>(9)? != 0,
                    attempts: row.get(10)?,
                    item: row.get(11)?,
                    detail: row.get::<Json<Value>>(12)?.0,
                    source: row.get(13)?,
                    harness: row.get(14)?,
                    duration_ms: row.get(15)?,
                })
            })
        })
        .unwrap()
}

pub fn model_audit(store: &Store, actor: &str) -> (Vec<Value>, i64) {
    store
        .read::<_, crate::StoreError>(|r| {
            let details = r.all(
                Query::select()
                    .column("detail")
                    .from("audit")
                    .and_where(Expr::col("actor").eq(actor))
                    .and_where(Expr::col("action").eq("settings.models"))
                    .order_by("id", Order::Asc),
                |row| Ok(row.get::<Json<Value>>(0)?.0),
            )?;
            let revisions = r.one(
                Query::select()
                    .expr(Func::count(Expr::col(Asterisk)))
                    .from("revisions")
                    .and_where(Expr::col("object_type").eq("settings"))
                    .and_where(Expr::col("author").eq(actor)),
                |row| row.get(0),
            )?;
            Ok((details, revisions.unwrap_or_default()))
        })
        .unwrap()
}

pub fn reject_model_settings(store: &Store) {
    store
        .write_locked(|c| {
            c.execute_batch(
                "CREATE TRIGGER reject_model_settings BEFORE INSERT ON settings
                         BEGIN SELECT RAISE(ABORT, 'test settings failure'); END;",
            )?;
            Ok(())
        })
        .unwrap();
}

pub struct Daily {
    pub calls: i64,
    pub failures: i64,
    pub cache_read: i64,
    pub hit_base: i64,
    pub output: i64,
    pub reasoning: i64,
}

pub fn gateway_daily(store: &Store, purpose: Option<&str>) -> Daily {
    let mut query = Query::select();
    for col in [
        "calls",
        "failed_calls",
        "cache_read",
        "hit_base",
        "output",
        "reasoning",
    ] {
        query.expr(Func::sum(Expr::col(col)));
    }
    query
        .from("stat_daily")
        .and_where(Expr::col("source").eq("gateway"));
    if let Some(purpose) = purpose {
        query.and_where(Expr::col("purpose").eq(purpose));
    }
    store
        .read(|r| {
            r.one(&query, |r| {
                Ok(Daily {
                    calls: r.get(0)?,
                    failures: r.get(1)?,
                    cache_read: r.get(2)?,
                    hit_base: r.get(3)?,
                    output: r.get(4)?,
                    reasoning: r.get(5)?,
                })
            })
        })
        .unwrap()
        .unwrap()
}

pub fn started_gateway_call(store: &Store, id: &str) -> (String, Option<String>, i64) {
    store
        .read::<_, crate::StoreError>(|r| {
            let (workspace, bot) = r
                .one(
                    Query::select()
                        .columns(["workspace_id", "bot_id"])
                        .from("gateway_calls")
                        .and_where(Expr::col("call_id").eq(id)),
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?
                .unwrap();
            let attempts = r
                .one(
                    Query::select()
                        .expr(Func::count(Expr::col(Asterisk)))
                        .from("gateway_attempts"),
                    |r| r.get(0),
                )?
                .unwrap();
            Ok((workspace, bot, attempts))
        })
        .unwrap()
}

pub fn gateway_groups(store: &Store) -> Vec<(String, i64, i64, i64, i64)> {
    store
        .read(|r| {
            let dialect = r.db().dialect();
            let call = dialect.json_field(Expr::col("detail"), "call_id");
            let failures = dialect
                .json_field(Expr::col("detail"), "failures")
                .is_not_null();
            r.all(
                Query::select()
                    .expr(call.clone())
                    .expr(Func::count(Expr::col(Asterisk)))
                    .expr(Func::sum(Expr::col("attempts")))
                    .expr(Func::sum(Expr::col("duration_ms")))
                    .expr(Func::sum(Expr::cust_with_expr(
                        "CASE WHEN ? THEN 1 ELSE 0 END",
                        failures,
                    )))
                    .from("usage_records")
                    .and_where(Expr::col("source").eq("gateway"))
                    .add_group_by([call]),
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )
        })
        .unwrap()
}
