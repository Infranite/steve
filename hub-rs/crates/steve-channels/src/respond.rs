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

use async_trait::async_trait;
use rusqlite::{OptionalExtension, params};
use steve_store::base::{self, DecisionKind};
use steve_store::{NewDecision, Tx, uid};

use crate::rules::Act;
use crate::spaces::{self, Mode};
use crate::{ChannelResult, Channels, bindings, messages, personas};

/// `subject_type` of decisions about one channel message (C6 registers its handler).
pub const SUBJECT_MESSAGE: &str = "chan_message";
/// The card's choice “发这条草稿”: its candidate draft goes out as you approved it.
pub const SEND_DRAFT: &str = "send_draft";
/// “告诉它怎么回”: your words go to the composer as the instruction; its reply is a draft as usual.
pub const INSTRUCT: &str = "instruct";
/// “按原文发”: your words go out as they are.
pub const VERBATIM: &str = "verbatim";
/// The card's choice before the cards had a candidate draft: your words become a draft to approve
/// (still accepted for cards opened then).
pub const DRAFT_REPLY: &str = "draft_reply";
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
            .reader(|c| crate::presence::stop(c, &plan.ws, &plan.message_id, now))
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
        self.store().reader(|c| {
            let starred = crate::decide::starred(c, &plan.ws, plan.sender_party_id.as_deref())?;
            crate::decide::gate(c, &plan.ws, &plan.message_id, starred, now)
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
pub fn open_handoff(tx: &Tx, plan: &Plan) -> ChannelResult<String> {
    let starred = plan.mode == Mode::Own
        && crate::decide::starred(tx, &plan.ws, plan.sender_party_id.as_deref())?;
    crate::decide::open_card(tx, plan, &crate::decide::Card::rule(starred))
}

/// A pending outbound draft replying to the message, and its `approve` decision: “以你本人
/// 发到「<空间>」” on your own account, “以<身份>发到「<空间>」” on a bot. The body is `text`
/// (empty until a composer writes one; you can write it in the inbox). Returns `(message, decision)`.
pub fn open_draft(tx: &Tx, plan: &Plan, text: &str, now: i64) -> ChannelResult<(String, String)> {
    open_draft_to(tx, &crate::deliver::OutTarget::reply(plan), text, now)
}

/// [`open_draft`] for any target: a reply (`reply_to`) or a new message in a space, as the
/// target's persona (explicit, else the space's effective one; none on your own accounts).
pub fn open_draft_to(
    tx: &Tx,
    t: &crate::deliver::OutTarget,
    text: &str,
    now: i64,
) -> ChannelResult<(String, String)> {
    open_draft_as(tx, t, text, now, uid("cm"))
}

/// [`open_draft_to`] stored as message `id` (an operation's: [`crate::deliver::operation_message_id`]).
pub(crate) fn open_draft_as(
    tx: &Tx,
    t: &crate::deliver::OutTarget,
    text: &str,
    now: i64,
    id: String,
) -> ChannelResult<(String, String)> {
    let space =
        spaces::get(tx, &t.space_id)?.ok_or_else(|| steve_modules::ApiError::not_found("空间"))?;
    let (account, _) = bindings::connection_account(tx, &t.ws, &t.connection_id)?;
    let persona = match (t.mode, &t.persona_id) {
        (Mode::Own, _) => None,
        (Mode::Bot, Some(id)) => Some(
            personas::get(tx, id)?
                .filter(|p| p.workspace_id == t.ws && !p.deleted)
                .ok_or_else(|| steve_modules::ApiError::not_found("对外身份"))?,
        ),
        (Mode::Bot, None) => bindings::effective(tx, &space)?.map(|e| e.persona),
    };
    tx.execute(
        // seen by the identity it goes out as (`seen_by`, migration 88)
        "INSERT INTO chan_messages (id, workspace_id, space_id, thread_key, reply_to, direction, sender_account_id,
            persona_id, on_behalf, text, status, created, seen_by)
         VALUES (?1, ?2, ?3, ?4, ?5, 'out', ?6, ?7, ?8, ?9, 'draft', ?10, coalesce((SELECT sum(1 << i.ordinal) FROM space_identities i JOIN connections c ON c.id = i.connection_id WHERE i.space_id = ?3 AND c.account_id = ?6 AND i.ordinal IS NOT NULL), 0))",
        params![
            id,
            t.ws,
            t.space_id,
            t.thread_key,
            t.reply_to,
            account,
            persona.as_ref().map(|p| p.id.clone()),
            if persona.is_some() { "persona" } else { "none" },
            text,
            now
        ],
    )?;
    let name = spaces::display_title(tx, &space)?;
    let question = match (&persona, t.mode) {
        (Some(p), _) => format!("以{}发到「{name}」", p.name),
        (None, Mode::Own) => format!("以你本人发到「{name}」"),
        (None, Mode::Bot) => {
            // no persona yet (no Bot to seed “Steve” with): the bot account's own name
            let account_name: String = tx.query_row(
                "SELECT coalesce(nullif(display_name, ''), handle_display) FROM accounts WHERE id = ?",
                [&account],
                |r| r.get(0),
            )?;
            format!("以{account_name}发到「{name}」")
        }
    };
    let d = base::decisions::open(
        tx,
        &NewDecision {
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
        },
    )?;
    tx.execute(
        "UPDATE chan_messages SET decision_id = ? WHERE id = ?",
        params![d.id, id],
    )?;
    messages::event(tx, &t.ws, &t.space_id, &id);
    Ok((id, d.id))
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
            label: plan.act.label().into(),
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
                .reader(|c| spaces::can_send_as_you(c, &plan.connection_id))?
        {
            r.act = Act::Record;
            r.label = NO_SEND_AS_YOU.into();
            if let Some(i) = &plan.trigger {
                self.react_ack(&plan.message_id).await;
                match self.converse_web(plan, i).await {
                    Ok(_) => r.label = NO_SEND_AS_YOU_COMMAND.into(),
                    Err(e) => {
                        tracing::warn!(
                            space = plan.space_id,
                            "command to the web conversation: {e}"
                        );
                        self.react_finish(&plan.message_id, crate::reactions::Finish::Failed)
                            .await;
                        self.conv_failed(plan, &not_answered(&format!("{NO_SEND_AS_YOU}（{e}）")));
                    }
                }
            }
            return Ok(r);
        }
        // you are there yourself, or the sender's later message took this one into its turn: nothing
        // is drafted or asked in your name (crate::presence, gate 1)
        if let Some(e) = self.owner_stop(plan)? {
            tracing::info!(
                space = plan.space_id,
                reason = e.reason(),
                "not drafted in your name"
            );
            r.act = Act::Record;
            r.label = e.label().into();
            return Ok(r);
        }
        // nothing to answer, or a nudge onto the card still open (crate::decide, G4 and G5)
        if let Some(g) = self.owner_gate(plan)? {
            r.act = Act::Record;
            match g {
                crate::decide::Gate::NoContent => r.label = crate::decide::LABEL_NO_CONTENT.into(),
                crate::decide::Gate::Nudge { decision } => {
                    self.store()
                        .transaction(|tx| crate::decide::nudge(tx, &decision, &plan.text))?;
                    r.label = crate::decide::LABEL_NUDGE.into();
                    r.decision_id = Some(decision);
                }
            }
            return Ok(r);
        }
        let now = self.now();
        if plan.act == Act::Handoff {
            let d = self.store().transaction(|tx| open_handoff(tx, plan))?;
            r.decision_id = Some(d);
            if !plan.muted {
                let owner = self
                    .store()
                    .reader(|c| personas::owner_name_for_space(c, &plan.ws, &plan.space_id))?;
                let text = crate::rules::handoff_ack(&owner);
                match self.responder().acknowledge(self, plan, &text).await {
                    Ok(_) => {}
                    Err(e) => tracing::warn!(space = plan.space_id, "handoff ack not sent: {e}"),
                }
            }
            return Ok(r);
        }
        // Steve responds: “收到” first, before anything is generated (best-effort, bounded)
        self.react_ack(&plan.message_id).await;
        let command = plan.trigger.is_some();
        // why a trigger word got no answer, in the words of what failed
        let mut failed: Vec<String> = vec![];
        if matches!(plan.act, Act::Reply | Act::Draft) {
            match self.converse(plan, command).await {
                Ok(Some(_)) => {
                    r.label = if command {
                        "你的指令，交给对话回答".into()
                    } else {
                        "交给对话回答".into()
                    };
                    return Ok(r);
                }
                Ok(None) => {}
                Err(e) => {
                    tracing::warn!(space = plan.space_id, "conversation hand-off failed: {e}");
                    failed.push(e.to_string());
                }
            }
        }
        match self.responder().act(self, plan).await {
            Ok(Some(a)) => {
                if !a.label.is_empty() {
                    r.label = a.label;
                }
                let said = match &a.out_message_id {
                    Some(id) => self.store().reader(|c| -> ChannelResult<String> {
                        Ok(c.query_row(
                            "SELECT coalesce(text, '') FROM chan_messages WHERE id = ?",
                            [id],
                            |x| x.get(0),
                        )?)
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
                return Ok(r);
            }
            Ok(None) => {}
            Err(e) => {
                // the responder failed: fall back below, so the message still reaches you
                tracing::warn!(space = plan.space_id, "channel responder failed: {e}");
                failed.push(e.to_string());
            }
        }
        if plan.trigger.is_some() {
            r.label = "触发语未接入，只记录".into();
            r.act = Act::Record;
            self.react_finish(&plan.message_id, crate::reactions::Finish::Failed)
                .await;
            let why = if !failed.is_empty() {
                failed.join("；")
            } else if self.thread().is_none() {
                "渠道模块没有接上 Steve 的对话".to_string()
            } else {
                "渠道的回复器没有接手这条触发词".to_string()
            };
            let said = self.conv_failed(plan, &not_answered(&why));
            // said in its thread too, as you, like any failed trigger word answered as you
            if plan.mode == Mode::Own
                && let Some(m) = said
                && let Some(out) = self.responder().outbound()
            {
                let op = crate::conversations::conv_operation(
                    &m,
                    m.meta["origin"].as_str().unwrap_or_default(),
                    crate::conversations::OP_TRIGGER_FAILED,
                );
                match self.trigger_failed_note(&plan.ws, plan, out, op).await {
                    Ok(d) => {
                        if let Err(e) = self.mark_out(&m, &d) {
                            tracing::warn!(space = plan.space_id, "failure note not marked: {e}");
                        }
                    }
                    Err(e) => tracing::warn!(space = plan.space_id, "failure note not sent: {e}"),
                }
            }
            return Ok(r);
        }
        match plan.act {
            Act::Reply | Act::Draft => {
                let (m, d) = self.store().transaction(|tx| {
                    let (m, d) = open_draft(tx, plan, "", now)?;
                    // the reply you write and approve answers the message
                    crate::reactions::expect_answer(tx, &plan.message_id, &m)?;
                    Ok::<_, crate::ChannelError>((m, d))
                })?;
                r.act = Act::Draft;
                r.label = "等你确认回复".into();
                r.out_message_id = Some(m);
                r.decision_id = Some(d);
            }
            Act::CreateItem(_) => {
                let d = self.store().transaction(|tx| open_handoff(tx, plan))?;
                r.act = Act::Handoff;
                r.label = "转给你，可以转成事项".into();
                r.decision_id = Some(d);
                self.react_finish(&plan.message_id, crate::reactions::Finish::Clear)
                    .await;
            }
            Act::Record | Act::Handoff => {}
        }
        Ok(r)
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
pub(crate) fn mark_event(tx: &Tx, event_id: &str, r: &Response) -> ChannelResult<()> {
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
        tx,
        event_id,
        outcome,
        &r.label,
        &reason,
        r.decision_id.as_deref(),
        r.item_id.as_deref(),
    )
}

/// [`mark_event`] on the event of inbound message `message`, when it has one.
pub(crate) fn mark_message_event(tx: &Tx, message: &str, r: &Response) -> ChannelResult<()> {
    match event_of(tx, message)? {
        Some(e) => mark_event(tx, &e, r),
        None => Ok(()),
    }
}

fn event_of(tx: &Tx, message: &str) -> ChannelResult<Option<String>> {
    Ok(tx
        .query_row(
            "SELECT event_id FROM chan_messages WHERE id = ?",
            [message],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten())
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
    tx: &Tx,
    event_id: &str,
    outcome: &str,
    label: &str,
    reason: &str,
    decision: Option<&str>,
    item: Option<&str>,
) -> ChannelResult<()> {
    tx.execute(
        "UPDATE ext_events SET outcome = ?, outcome_label = ?, decision_id = coalesce(?, decision_id),
            work_item_id = coalesce(?, work_item_id), reason = ?
          WHERE id = ? AND outcome = 'recorded' AND flow_id IS NULL AND run_id IS NULL",
        params![outcome, label, decision, item, reason, event_id],
    )?;
    Ok(())
}

/// Adds `label` to what the event of inbound message `message` says happened (once): something
/// done about it later, apart from what the rules did (a starred contact's notification).
pub(crate) fn append_label(tx: &Tx, message: &str, label: &str) -> ChannelResult<()> {
    tx.execute(
        "UPDATE ext_events SET outcome_label = CASE WHEN coalesce(outcome_label, '') = '' THEN ?2
                                                    ELSE outcome_label || ' · ' || ?2 END
          WHERE id = (SELECT event_id FROM chan_messages WHERE id = ?1)
            AND instr(coalesce(outcome_label, ''), ?2) = 0",
        params![message, label],
    )?;
    Ok(())
}

/// [`note_event`] for the event of inbound message `message`, when it has one.
pub(crate) fn note_message_event(
    tx: &Tx,
    message: &str,
    outcome: &str,
    label: &str,
    reason: &str,
    decision: Option<&str>,
) -> ChannelResult<()> {
    match event_of(tx, message)? {
        Some(e) => note_event(tx, &e, outcome, label, reason, decision, None),
        None => Ok(()),
    }
}
