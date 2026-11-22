use crate::contract::{Json, Row};
use crate::{Decision, Read, Result};
use sea_query::{Expr, ExprTrait, Order, Query};

/// The draft of the item's latest acceptance decision: the delivery summary and its checks.
pub fn delivery_summary(c: &mut impl Read, item: &str) -> Result<Option<String>> {
    c.one(
        Query::select()
            .columns(["draft"])
            .from("decisions")
            .and_where(Expr::col("item_id").eq(item))
            .and_where(Expr::col("kind").eq("accept"))
            .order_by("created", Order::Desc)
            .limit(1),
        |r| r.get(0),
    )
}

/// Did you approve the PR Steve offered when accepting this item (`gh_pr_open`, `yes`)? Then
/// the item waits for its PR (`finish_on=merged`).
pub fn wrap_up_by_pr(c: &mut impl Read, ws: &str, item: &str) -> Result<bool> {
    let dialect = c.db().dialect();
    c.one(
        Query::select()
            .expr(Expr::cust("1"))
            .from("decisions")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("subject_type").eq("gh_pr_open"))
            .and_where(Expr::col("item_id").eq(item))
            .and_where(Expr::col("requested_by").eq("steve"))
            .and_where(Expr::col("status").eq("resolved"))
            .and_where(dialect.json_field(Expr::col("result"), "choice").eq("yes"))
            .limit(1),
        |_| Ok(()),
    )
    .map(|row| row.is_some())
}

/// Was merging this PR at `sha` already asked (open, or answered for that head)?
pub fn merge_asked(c: &mut impl Read, ws: &str, pr: &str, sha: &str) -> Result<bool> {
    let dialect = c.db().dialect();
    c.one(
        Query::select()
            .expr(Expr::cust("1"))
            .from("decisions")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("subject_type").eq("gh_pr_merge"))
            .and_where(Expr::col("subject_id").eq(pr))
            .and_where(
                Expr::col("status")
                    .eq("open")
                    .or(dialect.json_field(Expr::col("result"), "sha").eq(sha)),
            )
            .limit(1),
        |_| Ok(()),
    )
    .map(|row| row.is_some())
}

const COLUMNS: [&str; 16] = [
    "id",
    "workspace_id",
    "kind",
    "subject_type",
    "subject_id",
    "item_id",
    "session_id",
    "question",
    "draft",
    "options",
    "status",
    "result",
    "requested_by",
    "created",
    "resolved",
    "undo",
];
fn decode(r: &Row<'_>) -> Result<Decision> {
    Ok(Decision {
        id: r.get(0)?,
        workspace_id: r.get(1)?,
        kind: r.get(2)?,
        subject_type: r.get(3)?,
        subject_id: r.get(4)?,
        item_id: r.get(5)?,
        session_id: r.get(6)?,
        question: r.get(7)?,
        draft: r.get(8)?,
        options: r.get::<Json<Vec<String>>>(9)?.0,
        status: r.get(10)?,
        result: r
            .get::<Option<Json<serde_json::Value>>>(11)?
            .map_or(serde_json::Value::Null, |value| value.0),
        requested_by: r.get(12)?,
        created: r.get(13)?,
        resolved: r.get(14)?,
        undo: r
            .get::<Option<Json<serde_json::Value>>>(15)?
            .map_or(serde_json::Value::Null, |value| value.0),
    })
}

pub fn get(c: &mut impl Read, id: &str) -> Result<Option<Decision>> {
    c.one(
        Query::select()
            .columns(COLUMNS)
            .from("decisions")
            .and_where(Expr::col("id").eq(id)),
        decode,
    )
}

pub fn open_for(
    c: &mut impl Read,
    ws: &str,
    kind: &str,
    subject_id: &str,
) -> Result<Option<Decision>> {
    c.one(
        Query::select()
            .columns(COLUMNS)
            .from("decisions")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("subject_type").eq(kind))
            .and_where(Expr::col("subject_id").eq(subject_id))
            .and_where(Expr::col("status").eq("open"))
            .order_by("created", Order::Asc)
            .limit(1),
        decode,
    )
}

pub fn for_item_actor(
    c: &mut impl Read,
    ws: &str,
    item: &str,
    kind: &str,
    actor: &str,
) -> Result<Option<Decision>> {
    c.one(
        Query::select()
            .columns(COLUMNS)
            .from("decisions")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("item_id").eq(item))
            .and_where(Expr::col("subject_type").eq(kind))
            .and_where(Expr::col("requested_by").eq(actor))
            .order_by("created", Order::Asc)
            .limit(1),
        decode,
    )
}
