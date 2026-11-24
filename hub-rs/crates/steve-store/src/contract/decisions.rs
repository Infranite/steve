use sea_query::{Expr, ExprTrait, Func, OnConflict, Order, Query};
use serde_json::Value;
use steve_types::{DecisionStatus, DomainEvent, WorkEvent};

use super::{Json, Q, Row};
use crate::{Decision, DecisionUndo, NewDecision, Read, Result, W, now, uid};

/// A choice acknowledged by the Hub. Any external operation has its own receipt.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct ChoiceReceipt {
    pub revision: i64,
    pub actor: String,
    pub action: String,
    pub answer: String,
    pub recorded_at: i64,
}

impl ChoiceReceipt {
    pub fn matches(&self, actor: &str, action: &str, answer: &str) -> bool {
        self.actor == actor && self.action == action && self.answer == answer
    }
}

pub fn choice_receipt(r: &mut impl Read, id: &str, revision: i64) -> Result<Option<ChoiceReceipt>> {
    r.one(
        Query::select()
            .from("decision_choice_receipts")
            .columns(["actor", "action", "answer", "recorded_at"])
            .and_where(Expr::col("decision_id").eq(id))
            .and_where(Expr::col("revision").eq(revision)),
        |r| {
            Ok(ChoiceReceipt {
                revision,
                actor: r.get(0)?,
                action: r.get(1)?,
                answer: r.get(2)?,
                recorded_at: r.get(3)?,
            })
        },
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DecisionChange {
    Resolve,
    Dismiss,
    Expire,
    Supersede,
    Reopen,
}

impl DecisionChange {
    fn next(self, current: DecisionStatus) -> Option<(DecisionStatus, &'static str)> {
        use DecisionChange::*;
        use DecisionStatus::{Dismissed, Expired, Open, Resolved, Superseded};
        match current {
            Open => match self {
                Resolve => Some((Resolved, "resolved")),
                Dismiss => Some((Dismissed, "dismissed")),
                Expire => Some((Expired, "expired")),
                Supersede => Some((Superseded, "superseded")),
                Reopen => None,
            },
            Resolved | Dismissed => (self == Reopen).then_some((Open, "reopened")),
            Expired | Superseded => None,
        }
    }
}

pub(crate) fn decode_status(row: &Row<'_>, col: usize) -> Result<DecisionStatus> {
    row.get::<String>(col)?
        .parse()
        .map_err(|_| crate::StoreError::Storage("invalid decision status".into()))
}

pub(crate) fn undo_until(
    status: DecisionStatus,
    resolved: Option<i64>,
    undo: Option<&DecisionUndo>,
) -> Option<i64> {
    resolved
        .filter(|at| {
            *at > 0
                && matches!(status, DecisionStatus::Resolved | DecisionStatus::Dismissed)
                && undo.is_some()
        })
        .map(|at| at + 24 * 3_600_000)
}

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

const COLUMNS: [&str; 19] = [
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
    "revision",
    "source_revision",
    "question_template",
];
fn decode(r: &Row<'_>) -> Result<Decision> {
    Ok(Decision {
        id: r.get(0)?,
        revision: r.get(16)?,
        source_revision: r
            .get::<Option<Json<crate::DecisionSource>>>(17)?
            .map(|key| key.0),
        question_template: r
            .get::<Option<Json<steve_types::DecisionPrompt>>>(18)?
            .map(|key| key.0),
        workspace_id: r.get(1)?,
        kind: r.get(2)?,
        subject_type: r.get(3)?,
        subject_id: r.get(4)?,
        item_id: r.get(5)?,
        session_id: r.get(6)?,
        question: r.get(7)?,
        draft: r.get(8)?,
        options: r.get::<Json<Vec<String>>>(9)?.0,
        status: decode_status(r, 10)?,
        result: r
            .get::<Option<Json<serde_json::Value>>>(11)?
            .map_or(serde_json::Value::Null, |value| value.0),
        requested_by: r.get(12)?,
        created: r.get(13)?,
        resolved: r.get(14)?,
        undo: r
            .get::<Option<Json<DecisionUndo>>>(15)?
            .map(|value| value.0),
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

/// Number of already-settled decisions for `(ws, subject_type, subject_id, kind)`.
/// Used by producers whose subject can legitimately open a new card after an earlier one settled
/// (e.g., re-added space, repeated reauth, re-asked PR/comment, retried send) so duplicate
/// replays while a card is open collide on the same natural key without blocking later occurrences.
pub fn settled_count(
    c: &mut impl Read,
    ws: &str,
    subject_type: &str,
    subject_id: &str,
    kind: &str,
) -> Result<i64> {
    Ok(c.one(
        Query::select()
            .expr(Func::count(Expr::col("id")))
            .from("decisions")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("subject_type").eq(subject_type))
            .and_where(Expr::col("subject_id").eq(subject_id))
            .and_where(Expr::col("kind").eq(kind))
            .and_where(Expr::col("status").ne("open")),
        |r| r.get(0),
    )?
    .unwrap_or(0))
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
    pub fn revise_draft(&mut self, id: &str, revision: i64, text: &str) -> Result<bool> {
        let updated = self.0.exec(
            Query::update()
                .table("decisions")
                .value("draft", text)
                .value("revision", revision + 1)
                .and_where(Expr::col("id").eq(id))
                .and_where(Expr::col("revision").eq(revision))
                .and_where(Expr::col("status").eq("open")),
        )?;
        if updated != 0 {
            let d = crate::repo::must(self.get(id)?)?;
            emit(self.0, &d);
        }
        Ok(updated != 0)
    }

    pub fn open(&mut self, d: &NewDecision) -> Result<Decision> {
        self.open_in(d).map(|(decision, _)| decision)
    }

    /// Open a decision and tell producers whether this command inserted the row.
    pub fn open_in(&mut self, d: &NewDecision) -> Result<(Decision, bool)> {
        let source = d.source.as_ref().map(serde_json::to_string).transpose()?;
        let template = d
            .question_template
            .as_ref()
            .map(serde_json::to_string)
            .transpose()?;
        let started = std::time::Instant::now();
        let id = uid("dec");
        let options = serde_json::to_string(d.options.as_deref().unwrap_or_default())?;
        let mut insert = Query::insert();
        insert
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
                "source_revision",
                "question_template",
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
                source.as_deref().into(),
                template.as_deref().into(),
            ]);
        if source.is_some() {
            insert.on_conflict(
                OnConflict::columns([
                    "workspace_id",
                    "subject_type",
                    "subject_id",
                    "kind",
                    "source_revision",
                ])
                .do_nothing()
                .to_owned(),
            );
        }
        if self.0.exec(&insert)? == 0 {
            let existing = self.0.one(
                Query::select()
                    .columns(COLUMNS)
                    .from("decisions")
                    .and_where(Expr::col("workspace_id").eq(d.workspace_id.as_str()))
                    .and_where(Expr::col("subject_type").eq(d.subject_type.as_str()))
                    .and_where(Expr::col("subject_id").eq(d.subject_id.as_str()))
                    .and_where(Expr::col("kind").eq(d.kind.as_str()))
                    .and_where(Expr::col("source_revision").eq(source.as_deref())),
                decode,
            )?;
            let existing = crate::repo::must(existing)?;
            metrics::counter!("steve_decision_duplicate_prompts_total").increment(1);
            return Ok((existing, false));
        }
        let d = crate::repo::must(self.get(&id)?)?;
        emit(self.0, &d);
        self.0
            .diagnostic(crate::diagnostics::decision_event(&d, "created", None));
        metrics::histogram!("steve_decision_transition_seconds", "operation" => "created")
            .record(started.elapsed());
        Ok((d, true))
    }

    /// Updates the structured prompt template and options of an open decision in place.
    pub fn update_template(
        &mut self,
        id: &str,
        template: &steve_types::DecisionPrompt,
        options: &[String],
    ) -> Result<bool> {
        let updated = self.0.exec(
            Query::update()
                .table("decisions")
                .value("question", "")
                .value("question_template", serde_json::to_string(template)?)
                .value("options", serde_json::to_string(options)?)
                .and_where(Expr::col("id").eq(id))
                .and_where(Expr::col("status").eq("open")),
        )?;
        if updated != 0 {
            let d = crate::repo::must(self.get(id)?)?;
            emit(self.0, &d);
        }
        Ok(updated != 0)
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

    /// The only persisted state transition. A stale snapshot cannot change a newer card.
    pub fn apply(
        &mut self,
        decision: &Decision,
        change: DecisionChange,
        result: &Value,
        undo: Option<&DecisionUndo>,
    ) -> Result<bool> {
        let started = std::time::Instant::now();
        let Some((to, operation)) = change.next(decision.status) else {
            return Ok(false);
        };
        let reopening = to == DecisionStatus::Open;
        let n = self.0.exec(
            Query::update()
                .table("decisions")
                .value("status", to.as_str())
                .value("result", (!result.is_null()).then(|| result.to_string()))
                .value("resolved", (!reopening).then(now))
                .value("undo", undo.map(serde_json::to_string).transpose()?)
                .value("revision", decision.revision + i64::from(reopening))
                .and_where(Expr::col("id").eq(decision.id.as_str()))
                .and_where(Expr::col("revision").eq(decision.revision))
                .and_where(Expr::col("status").eq(decision.status.as_str())),
        )?;
        if n > 0 {
            if matches!(change, DecisionChange::Expire | DecisionChange::Supersede) {
                let action = if change == DecisionChange::Expire {
                    "decision.expire"
                } else {
                    "decision.supersede"
                };
                self.0.audit(
                    &decision.workspace_id,
                    "steve",
                    action,
                    &decision.id,
                    &serde_json::json!({"revision": decision.revision}),
                )?;
            }
            let d = crate::repo::must(self.get(&decision.id)?)?;
            self.0.diagnostic(crate::diagnostics::decision_event(
                &d,
                operation,
                Some(decision.status.as_str()),
            ));
            emit(self.0, &d);
            metrics::histogram!("steve_decision_transition_seconds", "operation" => operation)
                .record(started.elapsed());
        }
        Ok(n > 0)
    }

    /// Settle an answer, retain the receipt, and write its audit in the same savepoint.
    pub fn resolve_choice(
        &mut self,
        decision: &Decision,
        actor: &str,
        action: &str,
        answer: &str,
        result: &Value,
        undo: Option<&DecisionUndo>,
    ) -> Result<bool> {
        if !self.apply(decision, DecisionChange::Resolve, result, undo)? {
            return Ok(false);
        }
        self.record_choice(decision, actor, action, answer)?;
        self.0.audit(
            &decision.workspace_id,
            actor,
            "decision.resolve",
            &decision.id,
            &serde_json::json!({"revision": decision.revision, "choice": action}),
        )?;
        Ok(true)
    }

    pub fn record_choice(
        &mut self,
        decision: &Decision,
        actor: &str,
        action: &str,
        answer: &str,
    ) -> Result<ChoiceReceipt> {
        let recorded_at = now();
        self.0.exec(
            Query::insert()
                .into_table("decision_choice_receipts")
                .columns([
                    "decision_id",
                    "revision",
                    "actor",
                    "action",
                    "answer",
                    "recorded_at",
                ])
                .values_panic([
                    decision.id.as_str().into(),
                    decision.revision.into(),
                    actor.into(),
                    action.into(),
                    answer.into(),
                    recorded_at.into(),
                ]),
        )?;
        Ok(ChoiceReceipt {
            revision: decision.revision,
            actor: actor.into(),
            action: action.into(),
            answer: answer.into(),
            recorded_at,
        })
    }

    pub fn settle(
        &mut self,
        id: &str,
        result: &Value,
        change: DecisionChange,
        undo: Option<&DecisionUndo>,
    ) -> Result<bool> {
        let Some(decision) = self.get(id)? else {
            return Ok(false);
        };
        self.apply(&decision, change, result, undo)
    }

    pub fn reopen(&mut self, decision: &Decision, metadata: &Value) -> Result<()> {
        if !self.apply(decision, DecisionChange::Reopen, metadata, None)? {
            return Err(crate::StoreError::Conflict("decision changed".into()));
        }
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

pub fn reopen(w: &mut W<'_>, id: &str) -> Result<()> {
    let mut decisions = DecisionRows(w);
    if let Some(decision) = decisions.get(id)?
        && matches!(
            decision.status,
            DecisionStatus::Resolved | DecisionStatus::Dismissed
        )
    {
        decisions.reopen(&decision, &Value::Null)?;
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
    let mut count = 0;
    for decision in open_item_decisions(w, item_id)? {
        count +=
            usize::from(DecisionRows(w).apply(&decision, DecisionChange::Expire, result, None)?);
    }
    Ok(count)
}

/// The client through which a decision was answered.
#[derive(serde::Serialize)]
pub struct DecisionVia<'a> {
    pub client: &'a str,
    pub kind: &'a str,
    pub name: &'a str,
    pub route: &'a str,
}

pub fn record_via(
    w: &mut W<'_>,
    ws: &str,
    id: &str,
    revision: i64,
    actor: &str,
    via: &DecisionVia<'_>,
) -> Result<()> {
    let Some(d) = get(w, id)?.filter(|d| {
        d.workspace_id == ws
            && d.revision == revision
            && d.status == DecisionStatus::Resolved
            && d.result.get("via").is_none()
    }) else {
        return Ok(());
    };
    if !choice_receipt(w, id, revision)?.is_some_and(|receipt| receipt.actor == actor) {
        return Ok(());
    }
    let result = w.dialect().json_set_field(
        Func::coalesce([Expr::col("result"), Expr::Constant("{}".into())]).into(),
        "via",
        Expr::val(serde_json::to_string(via)?),
    );
    w.exec(
        Query::update()
            .table("decisions")
            .value("result", result)
            .and_where(Expr::col("id").eq(id))
            .and_where(Expr::col("revision").eq(revision)),
    )?;
    emit(w, &d);
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

/// An unanswered request has gone out of scope. An already recorded choice remains
/// a fact; the Node's separate ledger records whether it applied that choice.
pub fn expire_unapplied(w: &mut W<'_>, id: &str, result: &Value) -> Result<bool> {
    DecisionRows(w).settle(
        id,
        result,
        crate::decision_repo::DecisionChange::Expire,
        None,
    )
}
