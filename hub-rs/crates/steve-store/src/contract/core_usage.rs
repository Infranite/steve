//! Usage projections for model selection and the model settings view.

use sea_query::{Asterisk, Expr, ExprTrait, Func, Query};

use crate::{Read, Result};

pub struct RecentUse {
    pub purpose: String,
    pub harness: String,
    pub model: String,
    pub calls: i64,
    pub at: i64,
    pub failed: i64,
}

pub fn recent_gateway_use(
    c: &mut impl Read,
    ws: &str,
    day: &str,
    since: i64,
) -> Result<Vec<RecentUse>> {
    c.all(
        Query::select()
            .columns(["purpose", "harness", "model"])
            .expr(Func::count(Expr::col(Asterisk)))
            .expr(Func::max(Expr::col("ts")))
            .expr(Func::sum(Expr::cust("CASE WHEN ok = 0 THEN 1 ELSE 0 END")))
            .from("usage_records")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("day").gte(day))
            .and_where(Expr::col("source").eq("gateway"))
            .and_where(Expr::col("ts").gte(since))
            .and_where(Expr::col("model").ne(""))
            .group_by_columns(["purpose", "harness", "model"]),
        |r| {
            Ok(RecentUse {
                purpose: r.get(0)?,
                harness: r.get(1)?,
                model: r.get(2)?,
                calls: r.get(3)?,
                at: r.get(4)?,
                failed: r.get(5)?,
            })
        },
    )
}

pub struct ModelUse {
    pub model: String,
    pub calls: i64,
    pub ok_at: Option<i64>,
    pub failed_at: Option<i64>,
}

pub fn harness_model_use(
    c: &mut impl Read,
    ws: &str,
    day: &str,
    harness: &str,
    since: i64,
) -> Result<Vec<ModelUse>> {
    c.all(
        Query::select()
            .column("model")
            .expr(Func::count(Expr::col(Asterisk)))
            .expr(Func::max(Expr::cust("CASE WHEN ok = 1 THEN ts END")))
            .expr(Func::max(Expr::cust("CASE WHEN ok = 0 THEN ts END")))
            .from("usage_records")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("day").gte(day))
            .and_where(Expr::col("harness").eq(harness))
            .and_where(Expr::col("ts").gte(since))
            .and_where(Expr::col("model").ne(""))
            .group_by_col("model"),
        |r| {
            Ok(ModelUse {
                model: r.get(0)?,
                calls: r.get(1)?,
                ok_at: r.get(2)?,
                failed_at: r.get(3)?,
            })
        },
    )
}
