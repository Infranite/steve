use crate::contract::{Json, Row};
use crate::{Decision, Read, Result};
use sea_query::{Expr, ExprTrait, Order, Query};

pub fn failed_item_count(
    c: &mut impl Read,
    ws: &str,
    since: i64,
    reasons: [&str; 2],
) -> Result<i64> {
    let starts_with = |prefix: &str| {
        Expr::cust_with_exprs(
            "substr(?, 1, length(?))",
            [Expr::col("question"), Expr::val(prefix)],
        )
        .eq(prefix)
    };
    Ok(c.one(
        Query::select()
            .expr(Expr::cust("count(DISTINCT item_id)"))
            .from("decisions")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("kind").eq("fail"))
            .and_where(Expr::col("created").gte(since))
            .and_where(starts_with(reasons[0]).or(starts_with(reasons[1]))),
        |r| r.get(0),
    )?
    .unwrap_or_default())
}

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

/// The latest resolved decision about a draft whose answer was `send` (the approval `execute` needs).
pub fn send_approval(c: &mut impl Read, ws: &str, draft: &str) -> Result<Option<crate::Decision>> {
    let dialect = c.db().dialect();
    let id = c.one(
        Query::select()
            .columns(["id"])
            .from("decisions")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("subject_type").eq("mail_draft"))
            .and_where(Expr::col("subject_id").eq(draft))
            .and_where(Expr::col("kind").eq("approve"))
            .and_where(Expr::col("status").eq("resolved"))
            .and_where(dialect.json_field(Expr::col("result"), "choice").eq("send"))
            .order_by("resolved", Order::Desc)
            .order_by("created", Order::Desc)
            .limit(1),
        |r| r.get::<String>(0),
    )?;
    match id {
        Some(id) => get(c, &id),
        None => Ok(None),
    }
}

/// Every open decision about a draft (approval or failure).
pub fn open_draft_decisions(c: &mut impl Read, ws: &str, draft: &str) -> Result<Vec<String>> {
    c.all(
        Query::select()
            .columns(["id"])
            .from("decisions")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("subject_type").eq("mail_draft"))
            .and_where(Expr::col("subject_id").eq(draft))
            .and_where(Expr::col("status").eq("open")),
        |r| r.get(0),
    )
}

pub fn open_mail_draft_ids(c: &mut impl Read) -> Result<Vec<String>> {
    c.all(
        Query::select()
            .column("subject_id")
            .from("decisions")
            .and_where(Expr::col("subject_type").eq("mail_draft"))
            .and_where(Expr::col("status").eq("open")),
        |r| r.get(0),
    )
}

pub fn reopen(w: &mut crate::W<'_>, id: &str) -> Result<()> {
    w.exec(
        Query::update()
            .table("decisions")
            .value("status", "open")
            .value("result", None::<String>)
            .value("resolved", None::<i64>)
            .value("undo", None::<String>)
            .and_where(Expr::col("id").eq(id)),
    )?;
    if let Some(d) = get(w, id)? {
        w.emit(steve_types::DomainEvent::from(
            steve_types::WorkEvent::Decision {
                ws: d.workspace_id.into(),
                id: id.to_owned().into(),
                item_id: Some(d.item_id.map(Into::into)),
            },
        ));
    }
    Ok(())
}

pub fn open_count(c: &mut impl Read, ws: &str) -> Result<i64> {
    Ok(c.one(
        Query::select()
            .expr(Expr::cust("count(*)"))
            .from("decisions")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("status").eq("open")),
        |r| r.get(0),
    )?
    .unwrap_or_default())
}

pub fn retry_count(c: &mut impl Read, kind: &str, id: &str, actor: &str) -> Result<u32> {
    let dialect = c.db().dialect();
    let count: i64 = c
        .one(
            Query::select()
                .expr(Expr::cust("count(*)"))
                .from("decisions")
                .and_where(Expr::col("subject_type").eq(kind))
                .and_where(Expr::col("subject_id").eq(id))
                .and_where(Expr::col("kind").eq("fail"))
                .and_where(Expr::col("status").eq("resolved"))
                .and_where(
                    dialect
                        .json_field(Expr::col("result"), "choice")
                        .eq("retry"),
                )
                .and_where(
                    dialect
                        .json_field(Expr::col("result"), "decided_by")
                        .eq(actor),
                ),
            |r| r.get(0),
        )?
        .unwrap_or_default();
    u32::try_from(count).map_err(|e| crate::StoreError::Rejected(e.to_string()))
}

pub fn latest_approved_release(c: &mut impl Read, ws: &str) -> Result<Option<String>> {
    let dialect = c.db().dialect();
    c.one(
        Query::select()
            .column("id")
            .from("decisions")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("kind").eq("approve"))
            .and_where(Expr::col("subject_type").is_in(["release", "delivery_batch"]))
            .and_where(Expr::col("status").eq("resolved"))
            .and_where(
                dialect
                    .json_field(Expr::col("result"), "approved")
                    .eq(1_i64),
            )
            .order_by("resolved", Order::Desc)
            .order_by_expr(dialect.insertion_order("decisions"), Order::Desc)
            .limit(1),
        |r| r.get(0),
    )
}
