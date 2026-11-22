use crate::{Read, Result};
use sea_query::{Expr, ExprTrait, Order, Query};

pub fn subject_for_object(
    c: &mut impl Read,
    ws: &str,
    kind: &str,
    predicate: &str,
    object_kind: &str,
    object: &str,
) -> Result<Option<String>> {
    c.one(
        Query::select()
            .columns(["subject_id"])
            .from("links")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("subject_type").eq(kind))
            .and_where(Expr::col("predicate").eq(predicate))
            .and_where(Expr::col("object_type").eq(object_kind))
            .and_where(Expr::col("object_id").eq(object))
            .limit(1),
        |r| r.get(0),
    )
}

pub fn item_event_exists(
    c: &mut impl Read,
    ws: &str,
    item: &str,
    kind: &str,
    id: &str,
) -> Result<bool> {
    c.one(
        Query::select()
            .expr(Expr::cust("1"))
            .from("links")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("subject_type").eq("item"))
            .and_where(Expr::col("subject_id").eq(item))
            .and_where(Expr::col("predicate").eq("from"))
            .and_where(Expr::col("object_type").eq(kind))
            .and_where(Expr::col("object_id").eq(id))
            .limit(1),
        |_| Ok(()),
    )
    .map(|row| row.is_some())
}

pub fn item_props(
    c: &mut impl Read,
    ws: &str,
    item: &str,
    predicate: &str,
) -> Result<Option<serde_json::Value>> {
    c.one(
        Query::select()
            .columns(["props"])
            .from("links")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("subject_type").eq("item"))
            .and_where(Expr::col("subject_id").eq(item))
            .and_where(Expr::col("predicate").eq(predicate))
            .order_by("created", Order::Asc)
            .order_by("object_id", Order::Asc)
            .limit(1),
        |r| Ok(r.get::<crate::contract::Json<serde_json::Value>>(0)?.0),
    )
}
