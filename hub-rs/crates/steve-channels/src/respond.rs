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
use rusqlite::params;
use steve_store::base::{self, DecisionKind};
use steve_store::{NewDecision, Tx, uid};

use crate::rules::Act;
use crate::spaces::{self, Mode, SpaceRow};
use crate::{ChannelResult, Channels, bindings, messages, personas};

/// `subject_type` of decisions about one channel message (C6 registers its handler).
pub const SUBJECT_MESSAGE: &str = "chan_message";
/// Options of the handoff decision.
pub const HANDOFF_OPTIONS: [&str; 3] = ["draft_reply", "to_item", "done"];
/// Options of the approval of a draft.
pub const DRAFT_OPTIONS: [&str; 2] = ["yes", "no"];
/// The handoff question's summary of the message, in characters.
const SUMMARY_MAX: usize = 60;

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

fn cut(s: &str, n: usize) -> String {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    match s.char_indices().nth(n) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s,
    }
}

/// A space's name in questions (`私聊` when it has none).
pub(crate) fn space_label(s: &SpaceRow) -> String {
    if s.name.trim().is_empty() {
        "私聊".into()
    } else {
        s.name.clone()
    }
}

/// The handoff decision: `「<空间>」里 <发送人> 找你：<摘要>`. Linked from the inbound
/// message (`decision_id`). One per message.
pub fn open_handoff(tx: &Tx, plan: &Plan) -> ChannelResult<String> {
    open_handoff_asking(tx, plan, None)
}

/// [`open_handoff`] with Steve's question to you under the message (the composer's `ask_owner`).
pub fn open_handoff_asking(tx: &Tx, plan: &Plan, ask: Option<&str>) -> ChannelResult<String> {
    if let Some(d) = base::decisions::open_for(tx, &plan.ws, SUBJECT_MESSAGE, &plan.message_id)? {
        return Ok(d.id);
    }
    let who = if plan.sender_name.trim().is_empty() {
        "有人"
    } else {
        plan.sender_name.as_str()
    };
    let body = if plan.text.trim().is_empty() {
        "[消息]".to_string()
    } else {
        cut(&plan.text, SUMMARY_MAX)
    };
    let d = base::decisions::open(
        tx,
        &NewDecision {
            workspace_id: plan.ws.clone(),
            kind: DecisionKind::Ask.code().into(),
            subject_type: SUBJECT_MESSAGE.into(),
            subject_id: plan.message_id.clone(),
            item_id: None,
            session_id: None,
            question: match ask.map(str::trim).filter(|a| !a.is_empty()) {
                Some(a) => format!(
                    "「{}」里 {who} 找你：{body}\nSteve 想先问你：{}",
                    plan.space_name,
                    cut(a, SUMMARY_MAX)
                ),
                None => format!("「{}」里 {who} 找你：{body}", plan.space_name),
            },
            draft: None,
            options: Some(HANDOFF_OPTIONS.map(String::from).to_vec()),
            requested_by: "steve".into(),
        },
    )?;
    tx.execute(
        "UPDATE chan_messages SET decision_id = ? WHERE id = ?",
        params![d.id, plan.message_id],
    )?;
    messages::event(tx, &plan.ws, &plan.space_id, &plan.message_id);
    Ok(d.id)
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
    let id = uid("cm");
    tx.execute(
        "INSERT INTO chan_messages (id, workspace_id, space_id, thread_key, reply_to, direction, sender_account_id,
            persona_id, on_behalf, text, status, created)
         VALUES (?, ?, ?, ?, ?, 'out', ?, ?, ?, ?, 'draft', ?)",
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
    let question = match (&persona, t.mode) {
        (Some(p), _) => format!("以{}发到「{}」", p.name, t.space_name),
        (None, Mode::Own) => format!("以你本人发到「{}」", t.space_name),
        (None, Mode::Bot) => {
            // no persona yet (no Bot to seed “Steve” with): the bot account's own name
            let name: String = tx.query_row(
                "SELECT coalesce(nullif(display_name, ''), handle_display) FROM accounts WHERE id = ?",
                [&account],
                |r| r.get(0),
            )?;
            format!("以{name}发到「{}」", t.space_name)
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
            options: Some(DRAFT_OPTIONS.map(String::from).to_vec()),
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
                    Err(e) => tracing::warn!(
                        space = plan.space_id,
                        "command to the web conversation: {e}"
                    ),
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
                    tracing::warn!(space = plan.space_id, "conversation hand-off failed: {e}")
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
                self.conv_note(plan, &said, a.item_id.as_deref());
                r.decision_id = a.decision_id;
                r.out_message_id = a.out_message_id;
                r.item_id = a.item_id;
                return Ok(r);
            }
            Ok(None) => {}
            Err(e) => {
                // the responder failed: fall back below, so the message still reaches you
                tracing::warn!(space = plan.space_id, "channel responder failed: {e}");
            }
        }
        if plan.trigger.is_some() {
            r.label = "触发语未接入，只记录".into();
            r.act = Act::Record;
            self.react_finish(&plan.message_id, crate::reactions::Finish::Clear)
                .await;
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

/// Records on the message's event what the rules did, when no flow took it (the signals page shows
/// “等你确认回复” instead of “没有流程，只记录”).
pub(crate) fn mark_event(tx: &Tx, event_id: &str, r: &Response) -> ChannelResult<()> {
    let outcome = match (&r.by, r.act) {
        (By::Thread { .. }, _) => "item",
        (By::Skipped | By::Flow { .. }, _) | (_, Act::Record) => return Ok(()),
        (_, Act::CreateItem(_)) if r.item_id.is_some() => "item",
        _ if r.decision_id.is_some() => "need",
        _ => return Ok(()),
    };
    tx.execute(
        "UPDATE ext_events SET outcome = ?, outcome_label = ?, decision_id = coalesce(?, decision_id),
            work_item_id = coalesce(?, work_item_id), reason = ?
          WHERE id = ? AND outcome = 'recorded' AND flow_id IS NULL AND run_id IS NULL",
        params![
            outcome,
            r.label,
            r.decision_id,
            r.item_id,
            match r.rule {
                Some(i) => format!("响应规则第 {} 条", i + 1),
                None => String::new(),
            },
            event_id
        ],
    )?;
    Ok(())
}
