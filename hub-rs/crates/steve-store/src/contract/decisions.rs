use sea_query::{Expr, ExprTrait, Func, Order, Query};
use serde_json::Value;
use steve_types::{DomainEvent, WorkEvent};

use super::{Json, Row};
use crate::{Decision, NewDecision, Read, Result, W, now, uid};

pub struct DecisionRows<'a, C>(pub &'a mut C);
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

impl<C: Read> DecisionRows<'_, C> {
    pub fn get(&mut self, id: &str) -> Result<Option<Decision>> {
        get(self.0, id)
    }

    pub fn open_confirmations(&mut self, ws: &str, subject: &str) -> Result<i64> {
        Ok(self
            .0
            .one(
                Query::select()
                    .expr(Func::count(Expr::col("id")))
                    .from("decisions")
                    .and_where(Expr::col("workspace_id").eq(ws))
                    .and_where(Expr::col("subject_type").eq(subject))
                    .and_where(Expr::col("kind").eq("confirm"))
                    .and_where(Expr::col("status").eq("open")),
                |r| r.get(0),
            )?
            .unwrap_or(0))
    }
}

fn emit(w: &mut W<'_>, d: &Decision) {
    w.emit(DomainEvent::from(WorkEvent::Decision {
        ws: d.workspace_id.clone().into(),
        id: d.id.clone().into(),
        item_id: Some(d.item_id.clone().map(Into::into)),
    }));
}

impl DecisionRows<'_, W<'_>> {
    pub fn open(&mut self, d: &NewDecision) -> Result<Decision> {
        let id = uid("dec");
        let options = serde_json::to_string(d.options.as_deref().unwrap_or_default())?;
        self.0.exec(
            Query::insert()
                .into_table("decisions")
                .columns([
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
                    "requested_by",
                    "created",
                ])
                .values_panic([
                    id.as_str().into(),
                    d.workspace_id.as_str().into(),
                    d.kind.as_str().into(),
                    d.subject_type.as_str().into(),
                    d.subject_id.as_str().into(),
                    d.item_id.as_deref().into(),
                    d.session_id.as_deref().into(),
                    d.question.as_str().into(),
                    d.draft.as_deref().unwrap_or_default().into(),
                    options.into(),
                    "open".into(),
                    d.requested_by.as_str().into(),
                    now().into(),
                ]),
        )?;
        let d = crate::repo::must(self.get(&id)?)?;
        emit(self.0, &d);
        self.0
            .diagnostic(crate::diagnostics::decision_event(&d, "created", None));
        Ok(d)
    }

    pub fn settle(&mut self, id: &str, result: &Value, status: &str, undo: &Value) -> Result<bool> {
        let undo = crate::types::truthy(undo).then(|| undo.to_string());
        let n = self.0.exec(
            Query::update()
                .table("decisions")
                .value("status", status)
                .value("result", result.to_string())
                .value("resolved", now())
                .value("undo", undo)
                .and_where(Expr::col("id").eq(id))
                .and_where(Expr::col("status").eq("open")),
        )?;
        if n > 0 {
            let d = crate::repo::must(self.get(id)?)?;
            self.0.diagnostic(crate::diagnostics::decision_event(
                &d,
                "resolved",
                Some("open"),
            ));
            emit(self.0, &d);
        }
        Ok(n > 0)
    }

    pub fn reopen(&mut self, decision: &Decision, metadata: &Value) -> Result<()> {
        let value = (!metadata.is_null()).then(|| metadata.to_string());
        self.0.exec(
            Query::update()
                .table("decisions")
                .value("status", "open")
                .value("result", value)
                .value("resolved", Option::<i64>::None)
                .value("undo", Option::<String>::None)
                .and_where(Expr::col("id").eq(decision.id.as_str())),
        )?;
        emit(self.0, decision);
        Ok(())
    }
}

impl<C: Read> DecisionRows<'_, C> {
    pub fn open_for(
        &mut self,
        ws: &str,
        subject_type: &str,
        subject: &str,
    ) -> Result<Option<Decision>> {
        let order = self.0.db().dialect().insertion_order(None);
        let id = self.0.one(
            Query::select()
                .from("decisions")
                .column("id")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("subject_type").eq(subject_type))
                .and_where(Expr::col("subject_id").eq(subject))
                .and_where(Expr::col("status").eq("open"))
                .order_by("created", sea_query::Order::Desc)
                .order_by_expr(order, sea_query::Order::Desc)
                .limit(1),
            |row| row.get::<String>(0),
        )?;
        match id {
            Some(id) => self.get(&id),
            None => Ok(None),
        }
    }
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

/// The client through which a decision was answered.
#[derive(serde::Serialize)]
pub struct DecisionVia<'a> {
    pub client: &'a str,
    pub kind: &'a str,
    pub name: &'a str,
    pub route: &'a str,
}

pub fn record_via(w: &mut W<'_>, id: &str, via: &DecisionVia<'_>) -> Result<()> {
    let via = serde_json::to_string(via)?;
    let result = w.dialect().json_set_field(
        Func::coalesce([Expr::col("result"), Expr::Constant("{}".into())]).into(),
        "via",
        Expr::val(via),
    );
    w.exec(
        Query::update()
            .table("decisions")
            .value("result", result)
            .and_where(Expr::col("id").eq(id))
            .and_where(Expr::col("status").ne("open")),
    )?;
    Ok(())
}
