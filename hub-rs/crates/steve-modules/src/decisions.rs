//! Decisions opened by modules. A module opens one with
//! `steve_store::base::decisions::open(tx, NewDecision)` and registers a [`DecisionHandler`] for its
//! `subject_type`; `POST /api/decisions/:id` reaches [`dispatch`] first.

use serde_json::{Value, json};
use steve_store::base::{self, CONFIRM_OPTIONS, DecisionKind};
use steve_store::{Decision, NewDecision, Tx};

use crate::{ApiError, ApiResult, Caller, Modules};

/// What a handler gets besides the decision.
pub struct DecisionCx<'a> {
    pub ws: &'a str,
    /// The user who answered.
    pub actor: &'a str,
    pub choice: &'a str,
    /// Free text from the card (an edited draft, a rework note); may be empty.
    pub text: &'a str,
}

/// The module side of one `subject_type`.
pub trait DecisionHandler: Send + Sync {
    /// Acts on the answer inside the transaction that settles the decision; returns the `result` JSON
    /// stored on it (default `{"choice": …}` plus `text` when given). An error rolls everything back and
    /// leaves the decision open.
    fn resolve(&self, tx: &Tx, cx: &DecisionCx, d: &Decision) -> ApiResult<Value>;

    /// Called in `undoDecision`'s transaction before a resolved decision is reopened: undo this
    /// module's side of the answer, or refuse with an error (the undo is rolled back). Default: nothing.
    fn before_undo(&self, tx: &Tx, d: &Decision) -> ApiResult<()> {
        let _ = (tx, d);
        Ok(())
    }

    /// Called when a job expires the decision (e.g. 30-day candidates); default does nothing.
    fn expire(&self, tx: &Tx, d: &Decision) -> ApiResult<()> {
        let _ = (tx, d);
        Ok(())
    }
}

/// The default result: `{"choice": c}`, plus `"text"` if non-empty.
pub fn choice_result(cx: &DecisionCx) -> Value {
    if cx.text.is_empty() {
        json!({ "choice": cx.choice })
    } else {
        json!({ "choice": cx.choice, "text": cx.text })
    }
}

/// A `confirm` decision (确认候选, options `yes`/`no`), the one kind the modules add.
pub fn confirm(
    ws: &str,
    subject_type: &str,
    subject_id: &str,
    question: &str,
    requested_by: &str,
) -> NewDecision {
    NewDecision {
        workspace_id: ws.into(),
        kind: DecisionKind::Confirm.code().into(),
        subject_type: subject_type.into(),
        subject_id: subject_id.into(),
        item_id: None,
        session_id: None,
        question: question.into(),
        draft: None,
        options: Some(CONFIRM_OPTIONS.map(String::from).to_vec()),
        requested_by: requested_by.into(),
    }
}

/// `POST /api/decisions/:id {choice, text}` for decisions whose `subject_type` a module registered.
/// `None` = not ours (or no such decision): steve-core runs its own `resolve()`.
///
/// In one transaction: the decision must be in the caller's workspace (`404 没有这个决定`) and open
/// (`409 这个决定已经处理过了`); `choice` must be one of its options when it has any
/// (`400 参数不对：choice`); the handler runs; the decision is settled `resolved` with the handler's
/// result; audit `decision.resolve`. Returns the settled decision. If it has an `item_id`, steve-core
/// does its item-side follow-up (supersede, timeline) afterwards; module decisions usually have none.
pub fn dispatch(
    m: &Modules,
    caller: &Caller,
    id: &str,
    choice: &str,
    text: &str,
) -> Option<ApiResult<Decision>> {
    let d = m.store.reader(|c| base::decisions::get(c, id)).ok()??;
    let handler = m.registry.handlers.get(&d.subject_type)?;
    Some(m.store.transaction(|tx| {
        let d = base::decisions::get(tx, id)?
            .filter(|d| d.workspace_id == caller.ws)
            .ok_or_else(|| ApiError::not_found("决定"))?;
        if d.status != "open" {
            return Err(ApiError::conflict(steve_i18n::t!(
                "modules.decision.already_handled"
            )));
        }
        if !d.options.is_empty() && !d.options.iter().any(|o| o == choice) {
            return Err(ApiError::invalid("choice"));
        }
        let cx = DecisionCx {
            ws: &caller.ws,
            actor: &caller.me,
            choice,
            text,
        };
        let result = handler.resolve(tx, &cx, &d)?;
        base::decisions::settle(tx, id, &result, "resolved", &undo_marker(&d.subject_type))?;
        base::audit(
            tx,
            &caller.ws,
            &caller.me,
            "decision.resolve",
            id,
            &json!({ "choice": choice, "text": text }),
        )?;
        base::decisions::get(tx, id)?.ok_or_else(|| ApiError::not_found("决定"))
    }))
}

/// The `undo` stored on a decision a module settled. Core only undoes decisions with a truthy `undo`
/// (and the web shows 撤销 only for them); core's own records are item patches, a module's undo is its
/// handler's [`DecisionHandler::before_undo`], so the marker just names the module side. Its keys are
/// none of the item columns, so core's item patch from it is empty.
pub fn undo_marker(subject_type: &str) -> Value {
    json!({ "module": subject_type })
}

/// `undoDecision` for a decision whose `subject_type` a module registered. `None` = not ours.
///
/// In one transaction: the decision is re-read and must pass `undoable` (core's rules: resolved,
/// truthy `undo`, within 24 h; else `400 这个决定不能撤销`), the handler's `before_undo` runs, the
/// decision is reopened, audit `decision.undo`. A refusal rolls everything back and is returned as is.
/// Returns the decision as it was before the undo, for core's item-side follow-up.
pub fn undo(
    m: &Modules,
    id: &str,
    actor: &str,
    undoable: impl FnOnce(&Decision) -> bool,
) -> Option<ApiResult<Decision>> {
    let d = m.store.reader(|c| base::decisions::get(c, id)).ok()??;
    let handler = m.registry.handlers.get(&d.subject_type)?;
    Some(m.store.transaction(|tx| {
        let d = base::decisions::get(tx, id)?
            .filter(|d| undoable(d))
            .ok_or_else(|| ApiError::rule(steve_i18n::t!("modules.decision.not_undoable")))?;
        handler.before_undo(tx, &d)?;
        tx.execute(
            "UPDATE decisions SET status = 'open', result = NULL, resolved = NULL, undo = NULL WHERE id = ?",
            [id],
        )?;
        tx.publish(steve_store::ChangeEvent::Decision {
            id: id.into(),
            item_id: Some(d.item_id.clone()),
        });
        base::audit(tx, &d.workspace_id, actor, "decision.undo", id, &json!({}))?;
        Ok(d)
    }))
}

/// Expires an open module decision (jobs): the handler's `expire`, then `status=expired`. Returns
/// whether it was open. Decisions of unregistered subject types are only marked expired.
pub fn expire(m: &Modules, id: &str) -> ApiResult<bool> {
    m.store.transaction(|tx| {
        let Some(d) = base::decisions::get(tx, id)?.filter(|d| d.status == "open") else {
            return Ok(false);
        };
        if let Some(h) = m.registry.handlers.get(&d.subject_type) {
            h.expire(tx, &d)?;
        }
        Ok(base::decisions::settle(
            tx,
            id,
            &json!({ "expired": true }),
            "expired",
            &Value::Null,
        )?)
    })
}
