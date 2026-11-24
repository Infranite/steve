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
            .and_where(Expr::col("draft").is_not_null())
            .order_by("created", Order::Desc)
            .limit(1),
        |r| r.get(0),
    )
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

    pub fn subject_had_failure(
        &mut self,
        ws: &str,
        subject_type: &str,
        subject: &str,
    ) -> Result<bool> {
        Ok(self
            .0
            .one(
                Query::select()
                    .column("id")
                    .from("decisions")
                    .and_where(Expr::col("workspace_id").eq(ws))
                    .and_where(Expr::col("subject_type").eq(subject_type))
                    .and_where(Expr::col("subject_id").eq(subject))
                    .and_where(Expr::col("kind").eq("fail"))
                    .limit(1),
                |_| Ok(()),
            )?
            .is_some())
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
    pub fn set_draft(&mut self, id: &str, text: &str) -> Result<()> {
        self.0.exec(
            Query::update()
                .table("decisions")
                .value("draft", text)
                .and_where(Expr::col("id").eq(id)),
        )?;
        Ok(())
    }

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

    /// Writes the card shown to the user in the same transaction as the checked draft.
    pub fn edit_card(
        &mut self,
        id: &str,
        question: Option<&str>,
        draft: Option<&str>,
        options: Option<&[String]>,
    ) -> Result<()> {
        self.update_card(id, question.map(Expr::val), draft, options)
    }

    /// Replaces a rendered annotation after the caller has settled its disclosure check.
    pub fn replace_card_note(
        &mut self,
        id: &str,
        pending: &str,
        settled: &str,
        draft: Option<&str>,
        options: Option<&[String]>,
    ) -> Result<()> {
        let question = Func::cust("replace").args([
            Expr::col("question"),
            Expr::val(pending),
            Expr::val(settled),
        ]);
        self.update_card(id, Some(question.into()), draft, options)
    }

    fn update_card(
        &mut self,
        id: &str,
        question: Option<Expr>,
        draft: Option<&str>,
        options: Option<&[String]>,
    ) -> Result<()> {
        let mut update = Query::update();
        update.table("decisions").and_where(Expr::col("id").eq(id));
        if let Some(question) = question {
            update.value("question", question);
        }
        if let Some(draft) = draft {
            update.value("draft", draft);
        }
        if let Some(options) = options {
            update.value("options", serde_json::to_string(options)?);
        }
        self.0.exec(&update)?;
        Ok(())
    }

    pub fn set_result(&mut self, id: &str, result: &Value) -> Result<()> {
        self.0.exec(
            Query::update()
                .table("decisions")
                .value("result", result.to_string())
                .and_where(Expr::col("id").eq(id)),
        )?;
        Ok(())
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

    /// Close a card automatically as orphaned, without emitting a second event.
    pub fn close_orphaned(&mut self, id: &str, now: i64) -> Result<()> {
        self.0.exec(
            Query::update()
                .table("decisions")
                .value("status", "resolved")
                .value(
                    "result",
                    serde_json::json!({"dismissed": true, "why": "connection_gone"}).to_string(),
                )
                .value("resolved", now)
                .and_where(Expr::col("id").eq(id))
                .and_where(Expr::col("status").eq("open")),
        )?;
        Ok(())
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
            .order_by_expr(dialect.insertion_order(Some("decisions")), Order::Desc)
            .limit(1),
        |r| r.get(0),
    )
}

pub fn open_item_decisions(c: &mut impl Read, item: &str) -> Result<Vec<Decision>> {
    c.all(
        Query::select()
            .columns(COLUMNS)
            .from("decisions")
            .and_where(Expr::col("item_id").eq(item))
            .and_where(Expr::col("status").eq("open")),
        decode,
    )
}

/// Settles the item's open decisions as `expired` with `result`, inside the caller's transaction
/// (`Decisions::resolve` for each, like a finished item's “nothing left to decide”). Returns how many.
pub fn expire_open_for_item(w: &mut crate::W<'_>, item_id: &str, result: &Value) -> Result<usize> {
    use crate::contract::Q;

    let ids = w.all(
        Query::select()
            .columns(["id", "workspace_id"])
            .from("decisions")
            .and_where(Expr::col("item_id").eq(item_id))
            .and_where(Expr::col("status").eq("open"))
            .order_by("created", Order::Asc),
        |r| Ok((r.get::<String>(0)?, r.get::<String>(1)?)),
    )?;
    let result = result.to_string();
    for (id, ws) in &ids {
        w.exec(
            Query::update()
                .table("decisions")
                .values([
                    ("status", "expired".into()),
                    ("result", result.as_str().into()),
                    ("resolved", now().into()),
                    ("undo", None::<String>.into()),
                ])
                .and_where(Expr::col("id").eq(id.as_str()))
                .and_where(Expr::col("status").eq("open")),
        )?;
        w.emit(steve_types::DomainEvent::from(
            steve_types::WorkEvent::Decision {
                ws: ws.clone().into(),
                id: id.clone().into(),
                item_id: Some(Some(item_id.into())),
            },
        ));
    }
    Ok(ids.len())
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

pub(crate) fn open_for_session_or_item(
    c: &mut impl Read,
    session: &str,
    item: Option<&str>,
) -> Result<Vec<Decision>> {
    let mut target = Expr::col("session_id").eq(session);
    if let Some(item) = item {
        target = target.or(Expr::col("item_id").eq(item));
    }
    c.all(
        Query::select()
            .columns(COLUMNS)
            .from("decisions")
            .and_where(Expr::col("status").eq("open"))
            .and_where(target),
        decode,
    )
}

/// Expire a Node permission even when the Hub had recorded an answer that never reached the Node.
pub fn expire_unapplied(w: &mut W<'_>, id: &str, result: &Value) -> Result<bool> {
    let n = w.exec(
        Query::update()
            .table("decisions")
            .value("status", "expired")
            .value("result", result.to_string())
            .value("resolved", now())
            .and_where(Expr::col("id").eq(id))
            .and_where(Expr::col("status").is_in(["open", "resolved"])),
    )?;
    if let Some(d) = get(w, id)? {
        emit(w, &d);
    }
    Ok(n > 0)
}

pub fn settle_permission(w: &mut W<'_>, id: &str, result: &Value, status: &str) -> Result<bool> {
    let changed = DecisionRows(w).settle(id, result, status, &Value::Null)?;
    if !changed && let Some(d) = get(w, id)? {
        emit(w, &d);
    }
    Ok(changed)
}
