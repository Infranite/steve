use crate::{Read, Result};
use sea_query::{Expr, ExprTrait, Order, Query};

pub fn current_employers(c: &mut impl Read, ws: &str, party: &str) -> Result<Vec<String>> {
    let end = c.db().dialect().json_field(Expr::col("props"), "to");
    c.all(
        Query::select()
            .column("object_id")
            .from("links")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("subject_type").eq("party"))
            .and_where(Expr::col("subject_id").eq(party))
            .and_where(Expr::col("predicate").eq("works_at"))
            .and_where(Expr::col("object_type").eq("party"))
            .and_where(end.is_null())
            .order_by_columns([("created", Order::Asc), ("object_id", Order::Asc)]),
        |row| row.get(0),
    )
}

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

pub fn item_subjects(
    c: &mut impl Read,
    ws: &str,
    pred: &str,
    kind: &str,
    object: &str,
    newest: bool,
) -> Result<Vec<String>> {
    let direction = if newest { Order::Desc } else { Order::Asc };
    let tie = c.db().dialect().insertion_order(Some("links"));
    c.all(
        Query::select()
            .column("subject_id")
            .from("links")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("subject_type").eq("item"))
            .and_where(Expr::col("predicate").eq(pred))
            .and_where(Expr::col("object_type").eq(kind))
            .and_where(Expr::col("object_id").eq(object))
            .order_by("created", direction.clone())
            .order_by_expr(tie, direction),
        |r| r.get(0),
    )
}

pub fn related(
    c: &mut impl Read,
    ws: &str,
    object: &crate::base::ObjRef,
    incoming: bool,
) -> Result<Vec<crate::Link>> {
    use crate::Link;
    let (kind, id) = if incoming {
        ("object_type", "object_id")
    } else {
        ("subject_type", "subject_id")
    };
    c.all(
        Query::select()
            .columns([
                "workspace_id",
                "subject_type",
                "subject_id",
                "predicate",
                "object_type",
                "object_id",
                "props",
                "source",
                "confidence",
                "created",
            ])
            .from("links")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col(kind).eq(object.kind.as_str()))
            .and_where(Expr::col(id).eq(object.id.as_str()))
            .order_by("created", Order::Asc),
        |r| {
            Ok(Link {
                workspace_id: r.get(0)?,
                subject_type: r.get(1)?,
                subject_id: r.get(2)?,
                predicate: r.get(3)?,
                object_type: r.get(4)?,
                object_id: r.get(5)?,
                props: r.get::<crate::contract::Json<serde_json::Value>>(6)?.0,
                source: r.get(7)?,
                confidence: r.get(8)?,
                created: r.get(9)?,
            })
        },
    )
}

pub fn channel_sources(
    r: &mut impl Read,
    ws: &str,
    items: &[&str],
) -> Result<Vec<(String, String)>> {
    let ids = r.db().dialect().text_array(items)?;
    r.all(
        Query::select()
            .columns(["subject_id", "object_id"])
            .from("links")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("subject_type").eq("item"))
            .and_where(Expr::col("subject_id").in_subquery(ids))
            .and_where(Expr::col("predicate").eq("from"))
            .and_where(Expr::col("object_type").eq("chan_message")),
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
}

pub fn item_channel_source(r: &mut impl Read, ws: &str, item: &str) -> Result<Option<String>> {
    r.one(
        Query::select()
            .column("object_id")
            .from("links")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("subject_type").eq("item"))
            .and_where(Expr::col("subject_id").eq(item))
            .and_where(Expr::col("predicate").eq("from"))
            .and_where(Expr::col("object_type").eq("chan_message"))
            .order_by("created", Order::Asc)
            .limit(1),
        |row| row.get(0),
    )
}
