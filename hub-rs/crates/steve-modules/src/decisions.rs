//! Settlement for registered decision subjects. The domain handler, choice receipt,
//! card transition, and audit share one write.

use serde_json::{Value, json};
use steve_store::base::{CONFIRM_OPTIONS, DecisionKind};
use steve_store::decision_repo::{DecisionChange, choice_receipt};
use steve_store::views::{ABOUT_CHAT_MESSAGE, Views};
use steve_store::{Decision, DecisionRows, NewDecision, W};
use steve_types::DecisionTier;

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

/// A domain handler can discover that the subject went out of scope while answering it.
pub enum DecisionOutcome {
    Resolved(Value),
    Expired(Value),
}

impl From<Value> for DecisionOutcome {
    fn from(result: Value) -> Self {
        Self::Resolved(result)
    }
}

/// The module side of one `subject_type`.
pub trait DecisionHandler: Send + Sync {
    /// Applies the domain effect or records that its subject has expired. A handler failure
    /// rolls back its writes and leaves the card open.
    fn resolve(&self, tx: &mut W<'_>, cx: &DecisionCx, d: &Decision) -> ApiResult<DecisionOutcome>;

    /// Called in `undoDecision`'s transaction before a resolved decision is reopened: undo this
    /// module's side of the answer, or refuse with an error (the undo is rolled back). Default: nothing.
    fn before_undo(&self, tx: &mut W<'_>, d: &Decision) -> ApiResult<()> {
        let _ = (tx, d);
        Ok(())
    }

    /// Non-authorizing metadata to retain when undo reopens a card (for example, a required
    /// human review). Never retain a choice, approval, content hash or send schedule here.
    /// Other modules keep the original empty-result behavior by default.
    fn reopen_result(&self, _d: &Decision) -> Value {
        Value::Null
    }

    /// This card was opened to act on an already completed result, rather than to finish the item.
    /// Keep it open on Hub recovery for completed (never dropped) items. The normal answer and
    /// effect checks still apply; this does not approve or retry anything.
    fn retain_on_completed_item_recovery(&self, _d: &Decision) -> bool {
        false
    }

    /// Called when a job expires the decision (e.g. 30-day candidates); default does nothing.
    fn expire(&self, tx: &mut W<'_>, d: &Decision) -> ApiResult<()> {
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
        question_template: None,
        source: None,
    }
}

/// Resolve a fixture's card using the current revision. Production callers carry
/// the revision from the decision they present or open.
#[doc(hidden)]
pub fn dispatch(
    m: &Modules,
    caller: &Caller,
    id: &str,
    choice: &str,
    text: &str,
) -> Option<ApiResult<Decision>> {
    let d = m.store.read(|r| DecisionRows(r).get(id)).ok()??;
    m.registry.handlers.get(&d.subject_type)?;
    Some(resolve(m, caller, id, d.revision, choice, text))
}

pub fn resolve(
    m: &Modules,
    caller: &Caller,
    id: &str,
    revision: i64,
    choice: &str,
    text: &str,
) -> ApiResult<Decision> {
    m.store.write(|w| {
        let d = DecisionRows(w)
            .get(id)?
            .filter(|d| d.workspace_id == caller.ws)
            .ok_or_else(|| ApiError::not_found("决定"))?;
        let handler = m
            .registry
            .handlers
            .get(&d.subject_type)
            .ok_or_else(|| ApiError::not_found("决定"))?;
        answer(w, caller, handler.as_ref(), &d, revision, choice, text)
    })
}

/// Ignore the supplied quiet drafts. Check each tier under the same write lock that discards the
/// draft: a contact starred since the inbox was rendered must keep their decision.
pub fn dismiss_drafts(m: &Modules, caller: &Caller, ids: &[String]) -> ApiResult<Vec<String>> {
    let handler = m
        .registry
        .handlers
        .get(ABOUT_CHAT_MESSAGE)
        .ok_or_else(|| ApiError::not_found("决定"))?;
    m.store.write(|w| {
        let views = w.decision_views().of(&caller.ws, ids)?;
        let mut dismissed = Vec::new();
        for v in views {
            if v.status != steve_types::DecisionStatus::Open
                || v.subject_type != ABOUT_CHAT_MESSAGE
                || v.kind != "approve"
                || v.tier != DecisionTier::Draft
            {
                continue;
            }
            let id = v.id.as_str();
            let d = DecisionRows(w)
                .get(id)?
                .ok_or_else(|| ApiError::not_found("决定"))?;
            answer(w, caller, handler.as_ref(), &d, d.revision, "no", "")?;
            dismissed.push(id.to_owned());
        }
        Ok(dismissed)
    })
}

fn answer(
    w: &mut W<'_>,
    caller: &Caller,
    handler: &dyn DecisionHandler,
    d: &Decision,
    revision: i64,
    choice: &str,
    text: &str,
) -> ApiResult<Decision> {
    if replay(w, d, revision, &caller.me, choice, text)? {
        return Ok(d.clone());
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
    match handler.resolve(w, &cx, d)? {
        DecisionOutcome::Resolved(result) => {
            complete_answer(w, d, &cx, &result, &undo_marker(&d.subject_type))
        }
        DecisionOutcome::Expired(result) => {
            expire_in(w, d, &result)?;
            DecisionRows(w)
                .get(&d.id)?
                .ok_or_else(|| ApiError::not_found("决定"))
        }
    }
}

pub fn replay(
    r: &mut impl steve_store::Read,
    decision: &Decision,
    revision: i64,
    actor: &str,
    choice: &str,
    text: &str,
) -> ApiResult<bool> {
    if revision != decision.revision {
        return Err(already_handled());
    }
    if decision.status == steve_types::DecisionStatus::Open {
        return Ok(false);
    }
    if choice_receipt(r, &decision.id, revision)?
        .is_some_and(|receipt| receipt.matches(actor, choice, text))
    {
        return Ok(true);
    }
    Err(already_handled())
}

pub fn complete_answer(
    w: &mut W<'_>,
    d: &Decision,
    cx: &DecisionCx<'_>,
    result: &Value,
    undo: &Value,
) -> ApiResult<Decision> {
    let mut rows = DecisionRows(w);
    if !rows.resolve_choice(d, cx.actor, cx.choice, cx.text, result, undo)? {
        return Err(already_handled());
    }
    DecisionRows(w)
        .get(&d.id)?
        .ok_or_else(|| ApiError::not_found("决定"))
}

fn already_handled() -> ApiError {
    ApiError::conflict(steve_i18n::t!("modules.decision.already_handled"))
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
    let d = m.store.read(|r| DecisionRows(r).get(id)).ok()??;
    let handler = m.registry.handlers.get(&d.subject_type)?;
    Some(m.store.write(|w| {
        let d = DecisionRows(w)
            .get(id)?
            .filter(|d| undoable(d))
            .ok_or_else(|| ApiError::rule(steve_i18n::t!("modules.decision.not_undoable")))?;
        reverse(w, &d, actor, handler.as_ref())?;
        Ok(d)
    }))
}

/// Reverse a registered decision as part of a domain command's transaction.
pub fn undo_in(m: &Modules, w: &mut W<'_>, caller: &Caller, decision: &Decision) -> ApiResult<()> {
    let d = DecisionRows(w)
        .get(&decision.id)?
        .filter(|d| d.workspace_id == caller.ws)
        .ok_or_else(|| ApiError::not_found("决定"))?;
    if d.revision != decision.revision || !undoable(&d) {
        return Err(ApiError::rule(steve_i18n::t!(
            "modules.decision.not_undoable"
        )));
    }
    let handler = m
        .registry
        .handlers
        .get(&d.subject_type)
        .ok_or_else(|| ApiError::not_found("决定"))?;
    reverse(w, &d, &caller.me, handler.as_ref())
}

pub fn undoable(d: &Decision) -> bool {
    d.undo_until()
        .is_some_and(|until| steve_store::now() <= until)
}

fn reverse(
    w: &mut W<'_>,
    d: &Decision,
    actor: &str,
    handler: &dyn DecisionHandler,
) -> ApiResult<()> {
    handler.before_undo(w, d)?;
    let metadata = handler.reopen_result(d);
    DecisionRows(w).reopen(d, &metadata)?;
    w.audit(
        &d.workspace_id,
        actor,
        "decision.undo",
        &d.id,
        &json!({ "revision": d.revision }),
    )?;
    Ok(())
}

/// Expires an open module decision (jobs): the handler's `expire`, then `status=expired`. Returns
/// whether it was open.
pub fn expire(m: &Modules, id: &str) -> ApiResult<bool> {
    m.store.write(|w| {
        let Some(d) = DecisionRows(w)
            .get(id)?
            .filter(|d| d.status == steve_types::DecisionStatus::Open)
        else {
            return Ok(false);
        };
        let handler = m
            .registry
            .handlers
            .get(&d.subject_type)
            .ok_or_else(|| ApiError::not_found("决定"))?;
        handler.expire(w, &d)?;
        expire_in(w, &d, &json!({ "expired": true }))?;
        Ok(true)
    })
}

fn expire_in(w: &mut W<'_>, d: &Decision, result: &Value) -> ApiResult<()> {
    if !DecisionRows(w).apply(d, DecisionChange::Expire, result, &Value::Null)? {
        return Err(already_handled());
    }
    Ok(())
}
