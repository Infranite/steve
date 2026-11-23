//! What steps 4–7 of the inbound pipeline hand on: a [`Plan`]
//! for the [`Responder`] — the seam C4 (reply generation, items) and C6 (outbound, trigger words)
//! implement — and the [`Response`] that comes back on [`crate::Inbound::response`].
//!
//! C3 itself never sends. What it does without a responder ([`Fallback`]):
//! - `handoff`: the `ask` decision (always, muted or not); “我请 <主人称呼> 来看。” is the
//!   responder's to send ([`Responder::acknowledge`]) and only when not muted;
//! - `reply` / `draft`: a pending outbound draft (`chan_messages` `direction=out, status=draft`) and
//!   its `approve` decision, so nothing goes out until you approve (C6 sends);
//! - `create_item`: handed to you as a handoff (options include “to_item”), so nothing is lost;
//! - trigger words: recorded, nothing sent.

mod execution;

use std::ops::ControlFlow;

use async_trait::async_trait;
use steve_store::Store;
use steve_store::audit_repo as audit;
use steve_store::base::DecisionKind;
use steve_store::views::Views;
use steve_store::{
    Accounts, DecisionRows, NewDecision, SignalRows, W, channel_message_repo as repo, uid,
};
use steve_types::DecisionTier;

use crate::rules::Act;
use crate::spaces::{self, Mode};
use crate::{ChannelResult, Channels, bindings, personas};

/// `subject_type` of decisions about one channel message (C6 registers its handler).
pub const SUBJECT_MESSAGE: &str = "chan_message";
/// The card's choice “发这条草稿”: its candidate draft goes out as you approved it.
pub const SEND_DRAFT: &str = "send_draft";
/// “告诉它怎么回”: your words go to the composer as the instruction; its reply is a draft as usual.
pub const INSTRUCT: &str = "instruct";
/// “按原文发”: your words go out as they are.
pub const VERBATIM: &str = "verbatim";
/// Options of the handoff decision with a candidate draft ([`crate::decide::open_card`]).
pub const HANDOFF_OPTIONS: [&str; 5] = [SEND_DRAFT, INSTRUCT, VERBATIM, "to_item", "done"];
/// … and without one.
pub const HANDOFF_NO_DRAFT_OPTIONS: [&str; 4] = [INSTRUCT, VERBATIM, "to_item", "done"];
/// Options of the approval of a draft.
pub const DRAFT_OPTIONS: [&str; 3] = ["yes", "no", "to_item"];

/// Direct outbound messages have no inbound request to turn into an item.
pub(crate) fn draft_options(has_source: bool) -> Vec<String> {
    DRAFT_OPTIONS
        .iter()
        .filter(|o| has_source || **o != "to_item")
        .map(|o| o.to_string())
        .collect()
}

impl Channels {
    /// [`crate::presence::stop`] for a plan that would draft or ask in your name about someone
    /// else's message (`reply`, `draft`, `handoff` on your own account, not your `/bot`): you are
    /// there, or a later message of the sender took it into its turn. `None` for any other plan.
    pub(crate) fn owner_stop(&self, plan: &Plan) -> ChannelResult<Option<crate::presence::Stop>> {
        if plan.mode != Mode::Own
            || plan.trigger.is_some()
            || !matches!(plan.act, Act::Reply | Act::Draft | Act::Handoff)
        {
            return Ok(None);
        }
        let now = self.now();
        self.store()
            .read(|c| crate::presence::stop(c, &plan.ws, &plan.message_id, now))
    }
}

impl Channels {
    /// G4 and G5 ([`crate::decide::gate`]) for the same plans as [`Channels::owner_stop`].
    pub(crate) fn owner_gate(&self, plan: &Plan) -> ChannelResult<Option<crate::decide::Gate>> {
        if plan.mode != Mode::Own
            || plan.trigger.is_some()
            || !matches!(plan.act, Act::Reply | Act::Draft | Act::Handoff)
        {
            return Ok(None);
        }
        let now = self.now();
        self.store().read(|r| {
            let starred = self
                .store()
                .read(|r| crate::decide::starred(r, &plan.ws, plan.sender_party_id.as_deref()))?;
            crate::decide::gate(r, &plan.ws, &plan.message_id, starred, now)
        })
    }
}

/// Who decided what happens to a message.
#[derive(Debug, Clone, PartialEq)]
pub enum By {
    /// Not processed (own message, another bot, a duplicate, a paused source).
    Skipped,
    /// Your own trigger words.
    Trigger { instruction: String },
    /// The space's flow (its `message` trigger took the event in step 2).
    Flow { flow_id: String },
    /// The response rules.
    Rules,
    /// A reply in the thread an item came from, by whoever raised it or you: sent to that item.
    Thread { item: String },
}

/// What steps 4–7 did with a message.
#[derive(Debug, Clone, PartialEq)]
pub struct Response {
    pub by: By,
    /// The action (after muting); `record` when skipped or handled by a flow.
    pub act: Act,
    /// The index of the rule that matched.
    pub rule: Option<usize>,
    /// Muting turned the matched rule's action into `record`.
    pub muted: bool,
    /// One line for the signal and logs (“起草，等你批准”).
    pub label: String,
    /// The decision opened (handoff, draft approval).
    pub decision_id: Option<String>,
    /// The outbound message made (a draft; a reply the responder sent).
    pub out_message_id: Option<String>,
    pub item_id: Option<String>,
    /// The composer read the message and chose not to answer it: its reason.
    pub composer_skip: Option<String>,
}

impl Response {
    pub(crate) fn skipped(label: impl Into<String>) -> Self {
        Self {
            by: By::Skipped,
            act: Act::Record,
            rule: None,
            muted: false,
            label: label.into(),
            decision_id: None,
            out_message_id: None,
            item_id: None,
            composer_skip: None,
        }
    }
}

/// Everything a responder needs about the message and what to do.
#[derive(Debug, Clone, PartialEq)]
pub struct Plan {
    pub ws: String,
    pub connection_id: String,
    pub mode: Mode,
    pub space_id: String,
    pub space_name: String,
    /// The inbound `cm_…`.
    pub message_id: String,
    /// Its provider id (reply to it) and thread.
    pub external_id: String,
    pub thread_key: Option<String>,
    pub text: String,
    pub sender_account_id: Option<String>,
    pub sender_party_id: Option<String>,
    pub sender_name: String,
    pub act: Act,
    pub rule: Option<usize>,
    /// The space is muted (a handoff says nothing).
    pub muted: bool,
    /// Trigger words: the instruction after them (the act is then `reply`, as you).
    pub trigger: Option<String>,
}

/// What a responder did.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Acted {
    pub label: String,
    pub decision_id: Option<String>,
    pub out_message_id: Option<String>,
    pub item_id: Option<String>,
    /// The composer chose not to answer: its reason ([`Response::composer_skip`]).
    pub composer_skip: Option<String>,
}

/// Carries out the rules' decision (C4 composes and creates items, C6 checks and sends).
#[async_trait]
pub trait Responder: Send + Sync {
    /// `reply`, `draft`, `create_item`, or trigger words (`plan.trigger`). `Ok(None)` = not taken:
    /// C3's fallback applies (a pending draft; a handoff).
    async fn act(&self, ch: &Channels, plan: &Plan) -> ChannelResult<Option<Acted>> {
        let _ = (ch, plan);
        Ok(None)
    }

    /// An explicit conversion creates the item without sending an acknowledgement to the chat.
    async fn to_item(&self, ch: &Channels, plan: &Plan) -> ChannelResult<Option<Acted>> {
        let _ = (ch, plan);
        Ok(None)
    }

    /// Sends a handoff's “我请 <主人称呼> 来看。” (never called when muted). Returns whether it
    /// was sent. Default: not sent.
    async fn acknowledge(&self, ch: &Channels, plan: &Plan, text: &str) -> ChannelResult<bool> {
        let _ = (ch, plan, text);
        Ok(false)
    }

    /// The reply generator, for conversation answers ([`crate::conversations`]). Default: none.
    fn composer(&self) -> Option<&crate::compose::Composer> {
        None
    }

    /// The outbound path, for what a conversation says back into its space. Default: none.
    fn outbound(&self) -> Option<&dyn crate::deliver::Outbound> {
        None
    }
}

/// The responder until C4/C6 are wired: takes nothing, sends nothing.
#[derive(Debug, Default, Clone, Copy)]
pub struct Fallback;

impl Responder for Fallback {}

/// The handoff card about the plan's message, as the rules open it ([`crate::decide::open_card`]): the
/// message is its summary; marked when a contact you starred sent it on your own account. One per
/// message.
pub fn open_handoff(w: &mut W<'_>, plan: &Plan) -> ChannelResult<String> {
    let starred = plan.mode == Mode::Own
        && crate::decide::starred(w, &plan.ws, plan.sender_party_id.as_deref())?;
    crate::decide::open_card(w, plan, &crate::decide::Card::rule(starred))
}

/// A pending outbound draft replying to the message, and its `approve` decision: “以你本人
/// 发到「<空间>」” on your own account, “以<身份>发到「<空间>」” on a bot. The body is `text`
/// (empty until a composer writes one; you can write it in the inbox). Returns `(message, decision)`.
pub fn open_draft(
    w: &mut W<'_>,
    plan: &Plan,
    text: &str,
    now: i64,
) -> ChannelResult<(String, String)> {
    open_draft_to(w, &crate::deliver::OutTarget::reply(plan), text, now)
}

/// [`open_draft`] for a reply or a new message, with the target's effective persona.
pub fn open_draft_to(
    w: &mut W<'_>,
    target: &crate::deliver::OutTarget,
    text: &str,
    now: i64,
) -> ChannelResult<(String, String)> {
    open_draft_as(w, target, text, now, uid("cm"))
}

/// [`open_draft_to`] stored under the operation's deterministic message id.
pub(crate) fn open_draft_as(
    w: &mut W<'_>,
    t: &crate::deliver::OutTarget,
    text: &str,
    now: i64,
    id: String,
) -> ChannelResult<(String, String)> {
    let space = steve_store::channel_spaces::rows::get_in_workspace(w, &t.ws, &t.space_id)?
        .ok_or_else(|| steve_modules::ApiError::not_found("空间"))?;
    let (account, _) = bindings::connection_account(w, &t.ws, &t.connection_id)?;
    let persona = match (t.mode, &t.persona_id) {
        (Mode::Own, _) => None,
        (Mode::Bot, Some(id)) => Some(
            personas::get(w, id)?
                .filter(|p| p.workspace_id == t.ws && !p.deleted)
                .ok_or_else(|| steve_modules::ApiError::not_found("对外身份"))?,
        ),
        (Mode::Bot, None) => bindings::effective(w, &space)?.map(|e| e.persona),
    };
    repo::outbound::insert(
        w,
        &repo::outbound::NewMessage {
            id: &id,
            ws: &t.ws,
            space: &t.space_id,
            thread: t.thread_key.as_deref(),
            reply_to: t.reply_to.as_deref(),
            account: Some(&account),
            persona: persona.as_ref().map(|persona| persona.id.as_str()),
            on_behalf: if persona.is_some() { "persona" } else { "none" },
            text: Some(text),
            status: "draft",
            created: now,
        },
    )?;
    let name = spaces::display_title(w, &space)?;
    let question = match (&persona, t.mode) {
        (Some(p), _) => format!("以{}发到「{name}」", p.name),
        (None, Mode::Own) => format!("以你本人发到「{name}」"),
        (None, Mode::Bot) => {
            let (_, account_name) = Accounts(w)
                .display(&t.ws, &account)?
                .ok_or_else(|| steve_modules::ApiError::not_found("账号"))?;
            format!("以{account_name}发到「{name}」")
        }
    };
    let d = DecisionRows(w).open(&NewDecision {
        workspace_id: t.ws.clone(),
        kind: DecisionKind::Approve.code().into(),
        subject_type: SUBJECT_MESSAGE.into(),
        subject_id: id.clone(),
        item_id: None,
        session_id: None,
        question,
        draft: Some(text.to_string()),
        options: Some(draft_options(t.source_message_id.is_some())),
        requested_by: "steve".into(),
    })?;
    repo::outbound::link_decision(w, &id, &d.id)?;
    retire_previous_drafts(w, &t.ws, &t.space_id, &d.id)?;
    repo::emit(w, &t.ws, &t.space_id, &id);
    Ok((id, d.id))
}

// Superseded versions keep their original body and decision in history. The decision tier
// protects starred contacts and required reviews from the conversation's ordinary draft slot.
fn retire_previous_drafts(
    w: &mut W<'_>,
    ws: &str,
    space: &str,
    current: &str,
) -> ChannelResult<()> {
    let mut views = w.decision_views();
    let ids = views.open_about_chat(ws, space)?;
    let views = views.of(ws, &ids)?;
    if !views
        .iter()
        .any(|d| d.id.as_str() == current && d.tier == DecisionTier::Draft)
    {
        return Ok(());
    }
    for d in views
        .into_iter()
        .filter(|d| d.id.as_str() != current && d.tier == DecisionTier::Draft)
    {
        let id = &d.subject_id;
        if let Some(row) = repo::outbound::approval(w, ws, id)? {
            repo::delivery::transition(
                w,
                id,
                &["draft"],
                "discarded",
                repo::delivery::SentAt::Keep,
            )?;
            crate::basis::checking_stopped(w, id, crate::outbound::R_DEALT_FIRST)?;
            crate::presence::settle(w, d.id.as_str(), "replaced_by_draft")?;
            repo::emit(w, ws, &row.space, id);
        }
    }
    Ok(())
}

/// Saves an edit to the still-open draft against the version the user read.
pub fn edit_reply_draft(
    store: &Store,
    ws: &str,
    actor: &str,
    id: &str,
    previous: &str,
    text: &str,
) -> ChannelResult<()> {
    use steve_modules::ApiError;

    let text = text.trim();
    if text.is_empty() || text.chars().count() > crate::outbound::TEXT_MAX {
        return Err(ApiError::invalid("text"));
    }
    store.write(|w| {
        let d = w
            .decision_views()
            .of(ws, &[id.to_owned()])?
            .pop()
            .ok_or_else(|| ApiError::not_found("decision"))?;
        if d.tier != DecisionTier::Draft || d.status != "open" || d.draft != previous {
            return Err(ApiError::conflict(steve_i18n::t!("channels.error.handled")));
        }
        if previous == text {
            return Ok(());
        }
        let message = &d.subject_id;
        let row = repo::outbound::approval(w, ws, message)?
            .filter(|row| row.status == "draft" && row.decision.as_deref() == Some(id))
            .ok_or_else(|| ApiError::conflict(steve_i18n::t!("channels.error.handled")))?;
        let version = audit::next_revision(w, ws, SUBJECT_MESSAGE, message)?;
        audit::revise(
            w,
            ws,
            SUBJECT_MESSAGE,
            message,
            version,
            &serde_json::json!({ "text": previous }),
            &serde_json::json!({ "text": text }),
            actor,
        )?;
        crate::basis::checking_stopped(w, message, crate::outbound::R_DEALT_FIRST)?;
        repo::outbound::edit_draft(w, message, text)?;
        if !DecisionRows(w).revise_draft(id, d.revision, text)? {
            return Err(ApiError::conflict(steve_i18n::t!("channels.error.handled")));
        }
        repo::emit(w, ws, &row.space, message);
        Ok(())
    })
}

/// What a trigger word's conversation records when it got no answer: why, and where to look.
pub fn not_answered(why: &str) -> String {
    format!(
        "（这条触发词没有回答：{}。到“接入”里看这个账号的连接和权限，修好后在聊天里再发一次。）",
        why.trim().trim_end_matches('。')
    )
}

/// `Response.label` when your account may not send as you.
pub const NO_SEND_AS_YOU: &str = "没有“以你的名义发送”的权限：只记录，不起草";
/// … and it was your `/bot`.
pub const NO_SEND_AS_YOU_COMMAND: &str = "没有“以你的名义发送”的权限：你的指令在网页对话里回答";

impl Channels {
    /// Step 7 for a decided plan.
    pub(crate) async fn execute(&self, plan: &Plan) -> ChannelResult<Response> {
        let mut r = Response {
            by: if let Some(i) = &plan.trigger {
                By::Trigger {
                    instruction: i.clone(),
                }
            } else {
                By::Rules
            },
            act: plan.act,
            rule: plan.rule,
            muted: false,
            label: crate::rules::action_label(plan.act).into(),
            decision_id: None,
            out_message_id: None,
            item_id: None,
            composer_skip: None,
        };
        if plan.act == Act::Record {
            return Ok(r);
        }
        // Without the grant to send as you, nothing is drafted or sent on your account:
        // your `/bot` is answered in your web conversation instead.
        if plan.mode == Mode::Own
            && matches!(plan.act, Act::Reply | Act::Draft)
            && !self
                .store()
                .read(|r| spaces::can_send_as_you(r, &plan.connection_id))?
        {
            return self.respond_without_grant(plan, r).await;
        }
        if self.apply_owner_gates(plan, &mut r)?.is_break() {
            return Ok(r);
        }
        let now = self.now();
        if plan.act == Act::Handoff {
            return self.respond_handoff(plan, r).await;
        }
        // Steve responds: “收到” first, before anything is generated (best-effort, bounded)
        self.react_ack(&plan.message_id).await;
        let command = plan.trigger.is_some();
        // why a trigger word got no answer, in the words of what failed
        let mut failed: Vec<String> = vec![];
        if self
            .route_conversation(plan, command, &mut r, &mut failed)
            .await
            .is_break()
        {
            return Ok(r);
        }
        if self
            .apply_responder(plan, &mut r, &mut failed)
            .await?
            .is_break()
        {
            return Ok(r);
        }
        if plan.trigger.is_some() {
            return self.respond_failed_trigger(plan, r, failed).await;
        }
        self.respond_fallback(plan, r, now).await
    }

    async fn apply_responder(
        &self,
        plan: &Plan,
        r: &mut Response,
        failed: &mut Vec<String>,
    ) -> ChannelResult<ControlFlow<()>> {
        match self.responder().act(self, plan).await {
            Ok(Some(a)) => {
                if !a.label.is_empty() {
                    r.label = a.label;
                }
                let said = match &a.out_message_id {
                    Some(id) => self.store().read(|r| -> ChannelResult<String> {
                        Ok(repo::projection::get(r, id)?
                            .ok_or_else(|| steve_modules::ApiError::not_found("消息"))?
                            .text
                            .unwrap_or_default())
                    })?,
                    None => String::new(),
                };
                self.conv_note(
                    plan,
                    &said,
                    a.item_id.as_deref(),
                    a.out_message_id.as_deref(),
                );
                r.decision_id = a.decision_id;
                r.out_message_id = a.out_message_id;
                r.item_id = a.item_id;
                r.composer_skip = a.composer_skip;
                return Ok(ControlFlow::Break(()));
            }
            Ok(None) => {}
            Err(e) => {
                // the responder failed: fall back below, so the message still reaches you
                tracing::warn!(space = plan.space_id, "channel responder failed: {e}");
                failed.push(e.to_string());
            }
        }
        Ok(ControlFlow::Continue(()))
    }
}

/// Who let a message go without an answer, at the head of its event's `reason` ([`mark_event`]):
/// the rules (the pipeline's own checks included), or the composer after reading it.
pub const TRAIL_RULES: &str = "规则";
pub const TRAIL_COMPOSER: &str = "起草器";

/// Records on the message's event what the rules did, when no flow took it (the signals page shows
/// “等你确认回复” instead of “没有流程，只记录”). A message let go without an answer keeps the
/// outcome `recorded` (nothing for you to do) and says why: the label, and in `reason` who decided
/// ([`TRAIL_RULES`] with the rule, or [`TRAIL_COMPOSER`] with its reason), so a skip can be told
/// apart from a message nobody looked at.
pub(crate) fn mark_event(w: &mut W<'_>, event_id: &str, r: &Response) -> ChannelResult<()> {
    let rule = match r.rule {
        Some(i) => format!("响应规则第 {} 条", i + 1),
        None => String::new(),
    };
    let (outcome, reason) = match (&r.by, r.act) {
        (By::Flow { .. }, _) => return Ok(()),
        (By::Thread { .. }, _) => ("item", rule),
        (By::Trigger { .. }, _) if r.decision_id.is_some() => {
            ("need", r.composer_skip.clone().unwrap_or_default())
        }
        (By::Trigger { .. }, _) => ("recorded", r.composer_skip.clone().unwrap_or_default()),
        _ if r.composer_skip.is_some() => (
            "recorded",
            trail(
                TRAIL_COMPOSER,
                r.composer_skip.as_deref().unwrap_or_default(),
            ),
        ),
        (By::Skipped, _) | (_, Act::Record) => ("recorded", trail(TRAIL_RULES, &rule)),
        (_, Act::CreateItem(_)) if r.item_id.is_some() => ("item", rule),
        _ if r.decision_id.is_some() => ("need", rule),
        _ => return Ok(()),
    };
    note_event(
        w,
        event_id,
        outcome,
        &r.label,
        &reason,
        r.decision_id.as_deref(),
        r.item_id.as_deref(),
    )
}

/// [`mark_event`] on the event of inbound message `message`, when it has one.
pub(crate) fn mark_message_event(w: &mut W<'_>, message: &str, r: &Response) -> ChannelResult<()> {
    match repo::ingress::linked_event(w, message)? {
        Some(e) => mark_event(w, &e, r),
        None => Ok(()),
    }
}

/// `who：why`, or `who` alone.
pub(crate) fn trail(who: &str, why: &str) -> String {
    match why.trim() {
        "" => who.to_string(),
        w => format!("{who}：{w}"),
    }
}

/// Writes what happened to a message on its event, while nothing else (a flow, a run, an earlier
/// outcome than `recorded`) has.
pub(crate) fn note_event(
    w: &mut W<'_>,
    event_id: &str,
    outcome: &str,
    label: &str,
    reason: &str,
    decision: Option<&str>,
    item: Option<&str>,
) -> ChannelResult<()> {
    Ok(SignalRows(w).channel_outcome(event_id, outcome, label, reason, decision, item)?)
}

/// Adds `label` to what the event of inbound message `message` says happened (once): something
/// done about it later, apart from what the rules did (a starred contact's notification).
pub(crate) fn append_label(w: &mut W<'_>, message: &str, label: &str) -> ChannelResult<()> {
    if let Some(event) = repo::ingress::linked_event(w, message)? {
        SignalRows(w).append_channel_label(&event, label)?;
    }
    Ok(())
}

/// [`note_event`] for the event of inbound message `message`, when it has one.
pub(crate) fn note_message_event(
    w: &mut W<'_>,
    message: &str,
    outcome: &str,
    label: &str,
    reason: &str,
    decision: Option<&str>,
) -> ChannelResult<()> {
    match repo::ingress::linked_event(w, message)? {
        Some(e) => note_event(w, &e, outcome, label, reason, decision, None),
        None => Ok(()),
    }
}
