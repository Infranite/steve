use sea_query::{Expr, ExprTrait, Func, Query};
use serde_json::Value;
use steve_types::{DomainEvent, WorkEvent};

use super::{Json, Row};
use crate::{Decision, NewDecision, Read, Result, W, now, uid};

pub struct DecisionRows<'a, C>(pub &'a mut C);

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
}
