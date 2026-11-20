//! The outbound path: [`Outbox`] is the
//! [`crate::deliver::Outbound`] every reply, acknowledgement, flow action, trigger reply and
//! return-to-source message goes through, and [`Outbox::send`] is the only place an adapter's
//! `send(` is called. In order:
//!
//! 1. **disclosure** (C5): B1's outbound check for the space's audience with the knowledge the text
//!    was written with (a fixed receipt gets the local checks only); a generated text that may leak and
//!    could go out without a draft is rewritten once by the drafting model; still leaking → a draft with
//!    the suspicious parts named, even when a grant or a rule would have let it out. A text that becomes
//!    a draft anyway is not rewritten: its card offers “改写后再看” ([`REWRITE`]);
//! 2. **authorization**: a bot account's `reply` rule, a flow's `msg.send` grant or the engine's
//!    approval of the step, your own trigger words, or your approval of this very message. Your own
//!    account never sends without your approval, except the trigger words you typed yourself.
//!    Everything else is a draft with an `approve` decision (“以你本人 / 以<身份>发到「<空间>」”);
//! 3. **send** ([`Outbox::send`]): the authorization is checked against the store again, the final
//!    text gets the persona signature or “（由 Steve 代发）”, the message is claimed once (`sent` set while `pending`), the adapter is called with
//!    `uuid = cm_…` (a retry cannot double-send), then `sent` / `failed` + a `fail` decision
//!    (retry / rewrite for review / discard), the basis record and the audit.
//!
//! Approvals wait out a short delay in the queue ([`Outbox::send_due`], every [`QUEUE_EVERY`]): the
//! account's `send_delay_s`, else [`SEND_DELAY_MS`] in your name and [`BOT_SEND_DELAY_MS`] for a bot;
//! undoing the approval within it puts the draft back. YOLO's approval comes after its own cooling-off
//! and goes out at once.
//!
//! Wiring (integrator):
//! ```ignore
//! let link = steve_channels::outbound::OutLink::new();
//! registry.decision(steve_channels::respond::SUBJECT_MESSAGE, link.handler());   // before Modules
//! let late = steve_channels::flow_actions::LateChannels::new();
//! let outbox = Arc::new(steve_channels::outbound::Outbox::new(link.clone())
//!     .act_as_user(feishu_user.clone()));
//! registry.item_hooks(Box::new(steve_channels::outbound::ReturnToSource::new(outbox.clone(), late.clone())));
//! let replies = Arc::new(steve_channels::Replies::new().outbound(outbox.clone()));
//! // … Channels::builder(…).responder(replies.clone()).build(); late.set(&channels);
//! link.bind_clock(modules.clock.clone());
//! let send_job = outbox.clone().spawn_queue(&channels);
//! ```

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;
use steve_store_sqlite::digest::{self, Source as DigestSource};

use async_trait::async_trait;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use steve_flow::AdapterError;
use steve_modules::accounts::connections::{BlockScope, ProviderError, WriteRefusal, may_write_in};
use steve_modules::decisions::{DecisionCx, DecisionHandler, choice_result};
use steve_modules::hooks::ItemHooks;
use steve_modules::outbound::{self as b1, Audience, CheckCx, CheckResult, KnowledgeUse, Verdict};
use steve_modules::{ApiError, ApiResult, Clock, SystemClock, Tier};
use steve_store::base::{self, DecisionKind, mems};
use steve_store::{Decision, Item, NewDecision, Tx, uid};

use crate::adapters::feishu::send::{at_handle, reply_in_thread};
use crate::basis::{self, NewBasis, Rewrite};
use crate::channel::{ActAsUser, ActTarget, MessageRef, OutContent, SendKey, Sent, SpaceKind};
use crate::compose::PinnedModel;
use crate::deliver::{Auth, Delivered, Generation, OutRequest, OutTarget, Outbound, Purpose};
use crate::disclosure::{self, MAX_REWRITES};
use crate::flow_actions::{LateChannels, plan_of};
use crate::personas::{self, PersonaRow};
use crate::respond::{self, DRAFT_OPTIONS, SUBJECT_MESSAGE};
use crate::rules::{self, Act, ItemMode, Trigger};
use crate::spaces::{self, Mode, SpaceRow};
use crate::{
    ChannelResult, Channels, bindings, compose, fallback, messages, send_state, write_blocks,
};

/// How long a message you approved waits before it goes out in your name, or as a bot speaking for you
/// (undo window).
pub const SEND_DELAY_MS: i64 = 10_000;
/// The same for a bot account speaking as itself (it can still be recalled once out).
pub const BOT_SEND_DELAY_MS: i64 = 3_000;
/// How often the queue looks for approved messages whose delay is over.
pub const QUEUE_EVERY: Duration = Duration::from_secs(2);
/// A claim older than this is considered abandoned (the Hub stopped mid-send) and retried; the
/// `uuid` keeps the provider from posting it twice.
pub const CLAIM_STALE_MS: i64 = 10 * 60 * 1000;
/// A direct message (no decision) not claimed this long after it was stored was cut off before its
/// send began (a restart, or it waits for its connection): the queue sends it. Longer than an
/// inline send takes to claim it.
pub const UNCLAIMED_MS: i64 = 30 * 1000;
/// A reply that could not go out within this long is not sent late by itself: you are asked.
pub const LATE_MS: i64 = 60 * 60 * 1000;
/// The choices on a message that may be out (sent, no answer, past the provider's dedupe window).
pub const UNKNOWN_OPTIONS: [&str; 3] = ["sent", "retry", "discard"];
/// Options of the decision after a failed send.
pub const FAIL_OPTIONS: [&str; 3] = ["retry", "rewrite", "discard"];
/// Options of the decision after a blocked one.
pub const BLOCKED_OPTIONS: [&str; 3] = ["retry", "rewrite", "discard"];
/// The choice “改写后再看”: the drafting model rewrites the flagged parts, the result is checked again
/// and comes back as a new draft card.
pub const REWRITE: &str = "rewrite";
/// Options of the approval of a model's draft the check flagged.
pub const REWRITE_OPTIONS: [&str; 3] = ["yes", "no", REWRITE];
/// On the card of a draft on your own account while its disclosure check runs, after the draft was
/// written ([`Outbox::send_due`]); replaced by what the check found when it is done.
pub const CHECKING_NOTE: &str = "（披露检查还在跑）";
/// The basis span of a draft dealt with (approved, discarded, closed) before its check finished.
pub const R_DEALT_FIRST: &str = "检查没跑完（草稿在检查结束前就处理了）";
/// Why a flagged reply was dropped: the rewrite that deleted the flagged parts left no text at all
/// ([`disclosure::Rewrite::Empty`]).
pub const BY_NOTHING_LEFT: &str = "删掉可能泄露的部分以后什么都不剩，不回了";
/// On the card of a flagged draft whose rewrite wrote something the draft did not say.
pub const NOTE_ADDED: &str = "（改写加了原文没有的话，已作废，原文未变）";
/// On the card of a flagged draft whose rewrite left only hedges ([`disclosure::Rewrite::Hollow`]): not
/// used, the original waits for you.
pub const NOTE_HOLLOW: &str = "（改写后只剩空话，没用上，原文未变）";

/// A provider refusal for a missing permission (Feishu 99991672 tenant / 99991679 user scope, or
/// the grant check before the call): retrying can't help.
pub fn missing_permission(error: &str) -> bool {
    [
        "99991672",
        "99991679",
        "没有开通这个权限",
        "授权里没有",
        "没有开启「",
    ]
    .iter()
    .any(|m| error.contains(m))
}

/// The question of the one card about writes a channel refuses for good ([`write_blocks`]): what
/// happened, what Steve does meanwhile, and how to allow sending.
fn blocked_notice(scope: BlockScope, space: &str, error: &str) -> String {
    let hours = write_blocks::MEMORY_MS / (60 * 60 * 1000);
    match scope {
        BlockScope::Connection => format!(
            "发到「{space}」的消息没有发出去：{error}。作用域：整个连接。{hours} 小时内仅抑制重复自动回复和告警，明确请求仍处理。请在渠道服务里允许这个连接发送（检查凭据的发送权限或联系服务管理员）；Steve 没有该服务的授权管理 API，内部具体拒绝规则尚未核实。处理后点重试，仍会重新校验；也可修改后重新审阅或不发送。恢复权限不会重发已放弃的消息"
        ),
        BlockScope::Target => format!(
            "发到「{space}」的消息没有发出去：{error}。作用域：这个会话，其他会话不受此目标级拒绝影响。{hours} 小时内仅抑制重复自动回复和告警，明确请求仍处理。请在渠道服务里允许这个会话（检查目标发送范围或联系服务管理员）；Steve 没有该服务的授权管理 API，内部具体拒绝规则尚未核实。处理后点重试，仍会重新校验；也可修改后重新审阅或不发送。恢复权限不会重发已放弃的消息"
        ),
    }
}

fn scope_code(scope: BlockScope) -> &'static str {
    match scope {
        BlockScope::Connection => "connection",
        BlockScope::Target => "target",
    }
}

/// Said on the approval of a draft that promises something for you (YOLO never sends one).
pub const COMMITS: &str = "（替你答应了事情，要你自己定）";
/// Appended to messages Steve sends as you after your trigger words, verbatim.
pub const PROXY_MARK: &str = "（由 Steve 代发）";
/// The longest text, in characters (like `POST /spaces/:id/messages`).
pub const TEXT_MAX: usize = 4000;

pub const MSG_ALREADY_SENT: &str = "消息已经发出，不能撤销";
pub const ERR_NO_RECALL: &str = "这个渠道不支持撤回";

/// `chan_messages.status` of a draft you said no to.
pub const DISCARDED: &str = "discarded";

/// The `steve_reply_source` answers, verbatim.
pub const SRC_POSTED: &str = "Posted.";
pub const SRC_DRAFTED: &str = "Drafted for the user's approval.";
pub const SRC_NO_SOURCE: &str = "This item did not come from a chat.";
/// … when the disclosure check could not finish (not a leak).
pub const SRC_UNCHECKED: &str =
    "Not posted: the disclosure check did not finish. A draft is waiting for the user.";

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The text's fingerprint an approval is for.
pub fn text_hash(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))
}

// ---------------------------------------------------------------- the link

/// Work a decision answer leaves for later (the handler runs inside a transaction, without the
/// runtime).
#[derive(Debug, Clone, PartialEq)]
enum After {
    /// A draft answer you discarded: the message it answers loses its “收到” reaction.
    Discarded { message_id: String },
}

struct LinkInner {
    clock: OnceLock<Arc<dyn Clock>>,
    after: Mutex<Vec<After>>,
}

/// Shared between the `chan_message` decision handler (registered before `Modules` exists) and the
/// [`Outbox`]. Cheap to clone.
#[derive(Clone)]
pub struct OutLink(Arc<LinkInner>);

impl Default for OutLink {
    fn default() -> Self {
        Self::new()
    }
}

impl OutLink {
    pub fn new() -> Self {
        Self(Arc::new(LinkInner {
            clock: OnceLock::new(),
            after: Mutex::new(Vec::new()),
        }))
    }

    /// The handler to register: `registry.decision(respond::SUBJECT_MESSAGE, link.handler())`.
    pub fn handler(&self) -> Box<dyn DecisionHandler> {
        Box::new(MessageDecisions(self.clone()))
    }

    /// The modules' clock (once; the handler uses the system clock until then).
    pub fn bind_clock(&self, c: Arc<dyn Clock>) {
        let _ = self.0.clock.set(c);
    }

    fn now(&self) -> i64 {
        match self.0.clock.get() {
            Some(c) => c.now(),
            None => SystemClock.now(),
        }
    }

    fn push(&self, a: After) {
        lock(&self.0.after).push(a);
    }

    fn take(&self) -> Vec<After> {
        std::mem::take(&mut *lock(&self.0.after))
    }
}

// ---------------------------------------------------------------- rows

/// One outbound `chan_messages` row as the sender needs it.
#[derive(Debug, Clone, PartialEq)]
pub struct OutRow {
    pub id: String,
    pub ws: String,
    pub space_id: String,
    pub text: String,
    pub reply_to: Option<String>,
    pub thread_key: Option<String>,
    pub sender_account_id: Option<String>,
    pub persona_id: Option<String>,
    pub on_behalf: String,
    pub status: String,
    pub decision_id: Option<String>,
    pub sent: Option<i64>,
}

fn out_row(c: &Connection, id: &str) -> ChannelResult<Option<OutRow>> {
    Ok(c.query_row(
        "SELECT id, workspace_id, space_id, coalesce(text, ''), reply_to, thread_key, sender_account_id, persona_id,
                on_behalf, status, decision_id, sent
           FROM chan_messages WHERE id = ? AND direction = 'out'",
        [id],
        |r| {
            Ok(OutRow {
                id: r.get(0)?,
                ws: r.get(1)?,
                space_id: r.get(2)?,
                text: r.get(3)?,
                reply_to: r.get(4)?,
                thread_key: r.get(5)?,
                sender_account_id: r.get(6)?,
                persona_id: r.get(7)?,
                on_behalf: r.get(8)?,
                status: r.get(9)?,
                decision_id: r.get(10)?,
                sent: r.get(11)?,
            })
        },
    )
    .optional()?)
}

fn own_out(tx: &Tx, ws: &str, id: &str) -> ApiResult<OutRow> {
    out_row(tx, id)?
        .filter(|r| r.ws == ws)
        .ok_or_else(|| ApiError::not_found("消息"))
}

fn set_status(tx: &Tx, id: &str, from: &[&str], to: &str) -> ChannelResult<bool> {
    let list = from.iter().map(|_| "?").collect::<Vec<_>>().join(", ");
    let mut p: Vec<&dyn rusqlite::ToSql> = vec![&to, &id];
    for f in from {
        p.push(f);
    }
    let changed = crate::messages::set_status(
        tx,
        id,
        &format!(
            "UPDATE chan_messages SET status = ?, sent = NULL WHERE id = ? AND status IN ({list})"
        ),
        p.as_slice(),
    )? > 0;
    // a draft dealt with while its check still ran: the record says the check did not finish
    if changed && from.contains(&"draft") && to != "draft" {
        basis::checking_stopped(tx, id, R_DEALT_FIRST)?;
    }
    Ok(changed)
}

/// Everything about where a message goes.
struct Place {
    space: SpaceRow,
    account: String,
    mode: Mode,
    persona: Option<PersonaRow>,
    owner_user: String,
    /// “以你本人” / “以<身份>” / “以<账号>”
    speaker: String,
}

fn place(
    c: &Connection,
    ws: &str,
    conn: &str,
    space_id: &str,
    persona_id: Option<&str>,
) -> ChannelResult<Place> {
    let space = spaces::get(c, space_id)?
        .filter(|s| s.workspace_id == ws)
        .ok_or_else(|| ApiError::not_found("空间"))?;
    let (account, mode) = bindings::connection_account(c, ws, conn)?;
    let persona = match (mode, persona_id) {
        (Mode::Own, _) => None,
        (Mode::Bot, Some(id)) => Some(
            personas::get(c, id)?
                .filter(|p| p.workspace_id == ws && !p.deleted)
                .ok_or_else(|| ApiError::not_found("对外身份"))?,
        ),
        (Mode::Bot, None) => bindings::effective(c, &space)?.map(|e| e.persona),
    };
    let owner_user = compose::owner_user(c, ws, conn)?;
    let speaker = match (&persona, mode) {
        (Some(p), _) => format!("以{}", p.name),
        (None, Mode::Own) => "以你本人".to_string(),
        (None, Mode::Bot) => {
            let name: String = c
                .query_row(
                    "SELECT coalesce(nullif(display_name, ''), handle_display) FROM accounts WHERE id = ?",
                    [&account],
                    |r| r.get(0),
                )
                .optional()?
                .unwrap_or_default();
            format!("以{name}")
        }
    };
    Ok(Place {
        space,
        account,
        mode,
        persona,
        owner_user,
        speaker,
    })
}

/// The knowledge a generated text was written with: layer 7's hits and the memories of layer 6.
/// What the audience itself said in the message this one answers (`CheckCx::audience_said`): only an
/// inbound message of the same workspace and space as the target, still holding its text. Anything
/// else — no source, another space, a purged or outbound message — gives nothing.
fn audience_said(c: &Connection, t: &OutTarget) -> ChannelResult<Option<String>> {
    let Some(src) = &t.source_message_id else {
        return Ok(None);
    };
    Ok(c.query_row(
        "SELECT text FROM chan_messages WHERE id = ? AND workspace_id = ? AND space_id = ?
            AND direction = 'in' AND purged IS NULL AND text IS NOT NULL AND trim(text) <> ''",
        params![src, t.ws, t.space_id],
        |r| r.get::<_, String>(0),
    )
    .optional()?)
}

fn knowledge_of(c: &Connection, g: Option<&Generation>) -> ChannelResult<Vec<KnowledgeUse>> {
    let Some(g) = g else {
        return Ok(Vec::new());
    };
    let mut out: Vec<KnowledgeUse> = g
        .hits
        .iter()
        .map(|h| KnowledgeUse {
            source_type: h.obj_type.clone(),
            source_id: h.obj_id.clone(),
            source_label: h.label.clone(),
            level: h.level,
            visibility: h.visibility.code(),
            text: h.text.clone(),
            seq: Some(h.seq),
        })
        .collect();
    for a in &g.anchors {
        let Some(id) = a.strip_prefix("mem:").or_else(|| a.strip_prefix("mm:")) else {
            continue;
        };
        if out.iter().any(|k| k.source_id == id) {
            continue;
        }
        if let Some(m) = mems::get(c, id)? {
            out.push(KnowledgeUse {
                source_type: "mem".into(),
                source_id: m.id.clone(),
                source_label: if m.source_label.is_empty() {
                    "记忆".into()
                } else {
                    m.source_label.clone()
                },
                level: m.level,
                visibility: m.visibility.code(),
                text: m.text.clone(),
                seq: None,
            });
        }
    }
    Ok(out)
}

/// The insert of an outbound row.
struct NewOut<'a> {
    /// Its id: an operation's ([`crate::deliver::operation_message_id`]), else a new one.
    id: Option<String>,
    t: &'a OutTarget,
    p: &'a Place,
    text: &'a str,
    status: &'a str,
    proxy: bool,
}

fn insert_out(tx: &Tx, n: &NewOut, now: i64) -> ChannelResult<String> {
    let id = n.id.clone().unwrap_or_else(|| uid("cm"));
    let on_behalf = if n.proxy {
        "proxy"
    } else if n.p.persona.is_some() {
        "persona"
    } else {
        "none"
    };
    tx.execute(
        // seen by the identity it goes out as (`seen_by`, migration 88)
        "INSERT INTO chan_messages (id, workspace_id, space_id, thread_key, reply_to, direction, sender_account_id,
            persona_id, on_behalf, text, status, created, seen_by)
         VALUES (?1, ?2, ?3, ?4, ?5, 'out', ?6, ?7, ?8, ?9, ?10, ?11, coalesce((SELECT sum(1 << i.ordinal) FROM space_identities i JOIN connections c ON c.id = i.connection_id WHERE i.space_id = ?3 AND c.account_id = ?6 AND i.ordinal IS NOT NULL), 0))",
        params![
            id,
            n.t.ws,
            n.t.space_id,
            n.t.thread_key,
            n.t.reply_to,
            n.p.account,
            n.p.persona.as_ref().map(|p| p.id.clone()),
            on_behalf,
            n.text,
            n.status,
            now
        ],
    )?;
    digest::sync_message(tx, DigestSource::ChanMessage, &id, None)?;
    Ok(id)
}

/// The suspicious parts, for a card: each span's text, or its reason when it has none. The check's
/// own failure is not a suspicious part ([`check_note`] says it).
fn spans_note(r: &CheckResult) -> String {
    let parts: Vec<String> = r
        .spans
        .iter()
        .filter(|s| !s.reason.starts_with(disclosure::R_MODEL_FAILED))
        .map(|s| {
            if s.text.trim().is_empty() {
                s.reason.clone()
            } else {
                format!("「{}」", s.text)
            }
        })
        .collect();
    parts.join("、")
}

/// What a card says about its draft's check, in brackets: nothing when it passed; `（可能泄露：…）`
/// when it flagged something; `（检查没跑完：…；试过 …）` when its model gave no answer — never
/// “可能泄露” for that. Both when the local checks flagged something and the model did not answer.
pub fn check_note(r: &CheckResult) -> String {
    let failed: Vec<&str> = r
        .spans
        .iter()
        .filter_map(|s| s.reason.strip_prefix(disclosure::R_MODEL_FAILED))
        .map(|w| w.trim_start_matches('（').trim_end_matches('）'))
        .collect();
    let unbound = r.spans.iter().any(|s| s.reason == disclosure::R_UNBOUND);
    let mut parts: Vec<String> = Vec::new();
    if r.verdict == Verdict::Leak {
        let flagged = spans_note(r);
        if !flagged.is_empty() {
            parts.push(format!("可能泄露：{flagged}"));
        }
    }
    if r.verdict != Verdict::Pass && (!failed.is_empty() || unbound || parts.is_empty()) {
        let mut w = if unbound {
            disclosure::R_UNBOUND.to_string()
        } else {
            failed.first().copied().unwrap_or("没有检查").to_string()
        };
        if let Some(m) = r.check_model.as_deref().filter(|m| !m.is_empty()) {
            w.push_str(&format!("；试过 {m}"));
        }
        parts.push(format!("{}：{w}", disclosure::R_MODEL_FAILED));
    }
    if parts.is_empty() {
        String::new()
    } else {
        format!("（{}）", parts.join("；"))
    }
}

/// The `reply_basis.verdict` of a check result: a leak is [`basis::DRAFTED`], a check that did not
/// finish [`basis::UNCHECKED`], a pass [`basis::PASS`] or, after a rewrite, [`basis::REWRITTEN`].
fn verdict_code(r: &CheckResult, rewritten: bool) -> &'static str {
    match r.verdict {
        Verdict::Leak => basis::DRAFTED,
        Verdict::Unchecked => basis::UNCHECKED,
        Verdict::Pass if rewritten => basis::REWRITTEN,
        Verdict::Pass => basis::PASS,
    }
}

/// What the audience can read in the conversation around the inbound message `source` of `space`:
/// the stretches of talk before it (as the composer reads them, [`crate::context::arrange`]), it,
/// and what was said after it, one `- 名字：text` line each. `None` when it is not there.
fn seen_of(
    c: &Connection,
    ws: &str,
    space: &str,
    source: &str,
    mode: Mode,
    now: i64,
) -> ChannelResult<Option<String>> {
    use crate::context::{self as cx, Shown};
    let mine = cx::mine(c, ws, space)?;
    let Some((t, before, after)) = cx::load(c, space, source, &mine, mode)? else {
        return Ok(None);
    };
    let label = |m: &cx::Msg| cx::label(&m.name, m.side, mode, "");
    let clock = cx::Clock {
        now,
        tz: steve_modules::zone::Zone::of_owner(c, ws)?.tz,
    };
    let conv = cx::arrange(
        &t,
        &before,
        &after,
        compose::HISTORY,
        cx::SEGMENTS,
        &label,
        &clock,
    );
    let mut lines: Vec<String> = conv
        .chain
        .iter()
        .chain(&conv.before)
        .map(Shown::render)
        .collect();
    lines.push(format!("- （这条）{}：{}", label(&t), t.text.trim()));
    lines.extend(conv.after.iter().map(Shown::render));
    Ok(Some(lines.join("\n")))
}

/// The inbound message an outbound row answers (by its `reply_to`, in its space).
fn source_of_row(c: &Connection, row: &OutRow) -> ChannelResult<Option<(String, String)>> {
    let Some(src) = &row.reply_to else {
        return Ok(None);
    };
    Ok(c.query_row(
        "SELECT id, coalesce(text, '') FROM chan_messages WHERE external_id = ? AND space_id = ? AND direction = 'in'
            AND purged IS NULL ORDER BY created DESC LIMIT 1",
        params![src, row.space_id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()?)
}

/// Resolve the actual inbound source, including thread targets without a reply_to.
fn draft_source(c: &Connection, row: &OutRow) -> ChannelResult<Option<String>> {
    Ok(c.query_row(
        "SELECT m.id FROM chan_messages m WHERE m.workspace_id = ?1 AND m.space_id = ?2 AND m.direction = 'in'
          AND m.purged IS NULL AND (m.id IN (SELECT message_id FROM chan_reactions WHERE answer_id = ?3)
               OR m.id IN (SELECT source_message_id FROM output_deliveries WHERE message_id = ?3)
               OR m.external_id = ?4) ORDER BY m.created DESC LIMIT 1",
        params![row.ws, row.space_id, row.id, row.reply_to], |r| r.get(0),
    ).optional()?)
}

/// What the composer was shown of how you talk, kept with the draft ([`crate::decide::keep_composed`]),
/// and whether the composer wrote it as its reply.
fn composed_of(c: &Connection, message: &str) -> ChannelResult<(Vec<String>, bool)> {
    let v: Option<String> = c
        .query_row(
            "SELECT composed FROM chan_messages WHERE id = ?",
            [message],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    let v: Value = v
        .and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or(Value::Null);
    let samples = v["style_samples"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    Ok((samples, v["outcome"] == "reply"))
}

/// The same choices on initial review, reopening and an asynchronous disclosure result.
fn approval_options(c: &Connection, row: &OutRow, rewrite: bool) -> ChannelResult<Vec<String>> {
    let has_source = draft_source(c, row)?.is_some();
    let base = if rewrite {
        &REWRITE_OPTIONS[..]
    } else {
        &DRAFT_OPTIONS[..]
    };
    let mut options: Vec<String> = base
        .iter()
        .filter(|o| **o != "to_item" || has_source)
        .map(|o| o.to_string())
        .collect();
    if rewrite && has_source {
        options.push("to_item".into());
    }
    Ok(options)
}

/// The `approve` decision of a draft. Returns its id.
fn open_approval(
    tx: &Tx,
    ws: &str,
    id: &str,
    question: String,
    text: &str,
    rewrite: bool,
) -> ChannelResult<String> {
    // Every reopening of a recovery draft inherits the review boundary. In particular, a failed
    // rewrite or a send-time disclosure/authorization recheck must not turn it into an ordinary
    // YOLO-eligible draft. A new UUID created by a rewrite is marked explicitly below.
    let review: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM chan_messages m JOIN decisions d ON d.id=m.decision_id
           WHERE m.id=? AND m.workspace_id=? AND (d.kind='fail' OR json_extract(d.result,'$.recovery_review')=1))",
        params![id, ws], |r| r.get(0),
    )?;
    let options = approval_options(tx, &own_out(tx, ws, id)?, rewrite)?;
    let d = base::decisions::open(
        tx,
        &NewDecision {
            workspace_id: ws.into(),
            kind: DecisionKind::Approve.code().into(),
            subject_type: SUBJECT_MESSAGE.into(),
            subject_id: id.into(),
            item_id: None,
            session_id: None,
            question,
            draft: Some(text.to_string()),
            options: Some(options),
            requested_by: "steve".into(),
        },
    )?;
    tx.execute(
        "UPDATE chan_messages SET decision_id = ? WHERE id = ?",
        params![d.id, id],
    )?;
    if review {
        mark_recovery_review(tx, &d.id)?;
    }
    Ok(d.id)
}

/// A durable requirement for human approval, not proof of disclosure or channel authorization.
fn mark_recovery_review(tx: &Tx, decision: &str) -> ChannelResult<()> {
    tx.execute(
        "UPDATE decisions SET result=json_set(CASE WHEN json_type(result)='object' THEN result ELSE '{}' END,'$.recovery_review',json('true')) WHERE id=?",
        [decision],
    )?;
    Ok(())
}

fn open_fail(tx: &Tx, row: &OutRow, question: String, options: &[&str]) -> ChannelResult<String> {
    let d = base::decisions::open(
        tx,
        &NewDecision {
            workspace_id: row.ws.clone(),
            kind: DecisionKind::Fail.code().into(),
            subject_type: SUBJECT_MESSAGE.into(),
            subject_id: row.id.clone(),
            item_id: None,
            session_id: None,
            question,
            draft: Some(row.text.clone()),
            options: Some(options.iter().map(|s| s.to_string()).collect()),
            requested_by: "steve".into(),
        },
    )?;
    tx.execute(
        "UPDATE chan_messages SET decision_id = ? WHERE id = ?",
        params![d.id, row.id],
    )?;
    Ok(d.id)
}

/// Recovery facts are additive card data. They describe the refusal, never an authorization.
fn recovery_metadata(
    tx: &Tx,
    decision: &str,
    kind: &str,
    scope: &str,
    conn: &str,
    space: &str,
    error: &str,
) -> ChannelResult<()> {
    let adapter: String =
        tx.query_row("SELECT adapter FROM connections WHERE id=?", [conn], |r| {
            r.get(0)
        })?;
    let code = [
        "session.write_policy=forbidden",
        "target_forbidden",
        "write_forbidden",
        "forbidden",
        "99991672",
        "99991679",
    ]
    .into_iter()
    .find(|code| error.contains(code));
    let rpc_code = if adapter.starts_with("ext_ws") {
        match code {
            Some("target_forbidden") => Some(-32016),
            Some("write_forbidden") => Some(-32006),
            Some("forbidden") => Some(-32009),
            _ => None,
        }
    } else {
        None
    };
    let message: String = tx.query_row(
        "SELECT subject_id FROM decisions WHERE id=?",
        [decision],
        |r| r.get(0),
    )?;
    let managed = kind == "missing_permission" && error.contains("没有开启「msg.send_as_user」");
    let question: String = tx.query_row(
        "SELECT question FROM decisions WHERE id=?",
        [decision],
        |r| r.get(0),
    )?;
    let data = json!({"recovery": {
        "kind": kind, "scope": scope, "source": if managed { "steve.connection_capability" } else { &adapter },
        "provider": adapter, "code": if managed { Some("capability_disabled") } else { code }, "rpc_code": rpc_code, "error": error,
        "connection_id": conn, "space_id": space, "requires_user_action": true,
        "authorization_kind": if managed { "managed" } else { "external" },
        "authorization_url": managed.then(|| format!("/api/chan-messages/{message}/authorization")),
        "settings_href": format!("#/channels/acc/{conn}"), "instructions": question,
        "external_policy_known": false
    }});
    tx.execute(
        "UPDATE decisions SET result=? WHERE id=?",
        params![data.to_string(), decision],
    )?;
    Ok(())
}

// ---------------------------------------------------------------- authorization

/// The undo window of an account with no `send_delay_s` of its own: a bot speaking as itself waits
/// [`BOT_SEND_DELAY_MS`], your own account [`SEND_DELAY_MS`].
pub fn default_send_delay_ms(mode: Mode) -> i64 {
    match mode {
        Mode::Bot => BOT_SEND_DELAY_MS,
        Mode::Own => SEND_DELAY_MS,
    }
}

/// How long `row` waits after you approve it: its account's `send_delay_s`, else the default of its
/// identity; a bot speaking for you (`on_behalf = proxy`) always waits like your own account.
fn send_delay_ms(c: &Connection, row: &OutRow) -> ChannelResult<i64> {
    if row.on_behalf == "proxy" {
        return Ok(SEND_DELAY_MS);
    }
    let space = spaces::get(c, &row.space_id)?.ok_or_else(|| ApiError::not_found("空间"))?;
    let (_, mode) = bindings::connection_account(c, &row.ws, &space.connection_id)?;
    let set =
        spaces::channel_settings(c, &row.ws, &space.connection_id)?.and_then(|s| s.send_delay_s);
    Ok(set.map_or(default_send_delay_ms(mode), |s| s.clamp(0, 60) * 1000))
}

/// One rewrite of `text` by the model that wrote it (`model`; the gateway's smart tier when it was not
/// a pinned one); `None` when it gave no text.
async fn rewrite_text(
    ch: &Channels,
    pinned: Option<&Arc<dyn PinnedModel>>,
    audience: &Audience,
    text: &str,
    r: &CheckResult,
    model: &str,
) -> Option<String> {
    let prompt = disclosure::rewrite_prompt(audience, text, &r.spans);
    let out = match pinned {
        Some(p) if !model.is_empty() => p.run(model, &prompt, compose::TIMEOUT).await,
        _ => {
            steve_modules::gateway::purpose(
                "compose",
                ch.modules()
                    .gateway
                    .run(Tier::Smart, &prompt, compose::TIMEOUT),
            )
            .await
        }
    };
    match out {
        Ok(o) => disclosure::parse_rewrite(&o.text),
        Err(e) => {
            tracing::warn!("channel rewrite failed: {e}");
            None
        }
    }
}

/// What a later check of a draft is given: its row, where it goes, its audience, the knowledge it was
/// written with (read again; `complete`: all of it), its basis, and the check's context.
struct Later {
    row: OutRow,
    p: Place,
    audience: Audience,
    knowledge: Vec<KnowledgeUse>,
    complete: bool,
    b: basis::BasisOut,
    cx: CheckCx,
    /// The composer wrote it as its reply (only such a text is rewritten).
    composed: bool,
}

/// Loads what checking the draft `message_id` again needs ([`Later`]); `None` when it has no basis.
fn later_of(c: &Connection, message_id: &str, now: i64) -> ChannelResult<Option<Later>> {
    let Some(row) = out_row(c, message_id)? else {
        return Ok(None);
    };
    let Some(b) = basis::of_message(c, &row.ws, &row.id)? else {
        return Ok(None);
    };
    // checked as it goes out: through its sender's identity in the chat (one space per chat: the space's
    // own connection may be a bot's while the draft is yours), else the space's voice
    let space = match sending_space(c, &row)? {
        Some(s) => s,
        None => spaces::get(c, &row.space_id)?.ok_or_else(|| ApiError::not_found("空间"))?,
    };
    let p = place(
        c,
        &row.ws,
        &space.connection_id,
        &row.space_id,
        row.persona_id.as_deref(),
    )?;
    let audience = disclosure::space_audience(c, &p.space, &p.account, &p.owner_user)?;
    // the same knowledge the first check had (retrieved pieces and memories), read again
    let entries = b.entries.as_array().cloned().unwrap_or_default();
    let (knowledge, complete) = steve_knowledge::retrieve::knowledge_again(c, &row.ws, &entries)?;
    let source = source_of_row(c, &row)?;
    let said = source
        .as_ref()
        .map(|(_, t)| t.clone())
        .filter(|t| !t.trim().is_empty());
    let seen = match &source {
        Some((id, _)) => seen_of(c, &row.ws, &row.space_id, id, p.mode, now)?,
        None => None,
    };
    let (samples, composed) = composed_of(c, &row.id)?;
    let cx = CheckCx {
        ws: row.ws.clone(),
        actor: match (&p.persona, p.mode) {
            (Some(per), _) => per.bot_id.clone(),
            (None, Mode::Own) => "me".into(),
            (None, Mode::Bot) => "steve".into(),
        },
        draft_model: b.answer_model.clone(),
        audience_said: said,
        audience_seen: seen,
        style_samples: samples,
        skip_model: false,
        // the model may be left out only when the knowledge is all here, as in the first check
        internal_skip: complete && p.mode == Mode::Bot && row.on_behalf != "proxy",
        // a draft waits for you anyway
        patient: true,
    };
    Ok(Some(Later {
        row,
        p,
        audience,
        knowledge,
        complete,
        b,
        cx,
        composed,
    }))
}

/// B1's check of `text` for a later check ([`Later`]): checked against less than it was written with,
/// it does not pass on that.
async fn check_again(ch: &Channels, l: &Later, text: &str) -> CheckResult {
    let mut r = b1::check(ch.modules(), &l.cx, &l.audience, text, &l.knowledge).await;
    if !l.complete && r.verdict == Verdict::Pass {
        r.verdict = Verdict::Leak;
        r.spans.push(steve_modules::outbound::Span {
            text: String::new(),
            reason: steve_modules::outbound::R_BASIS_GONE.into(),
        });
    }
    r
}

/// One rewrite of the flagged `text` ([`rewrite_text`]), held to deleting ([`disclosure::judge_rewrite`]):
/// `None` when the model gave no answer.
async fn rewrite_once(
    ch: &Channels,
    pinned: Option<&Arc<dyn PinnedModel>>,
    audience: &Audience,
    text: &str,
    r: &CheckResult,
    model: &str,
) -> Option<disclosure::Rewrite> {
    let new = rewrite_text(ch, pinned, audience, text, r, model).await?;
    let judged = disclosure::judge_rewrite(text, &new);
    if judged == disclosure::Rewrite::Added {
        tracing::info!("a rewrite wrote what the reply did not say: discarded");
    }
    Some(judged)
}

fn reasons(r: &CheckResult) -> String {
    r.spans
        .iter()
        .map(|s| s.reason.clone())
        .collect::<Vec<_>>()
        .join("；")
}

/// A flagged draft whose rewrite left no text at all: there is nothing to say. The draft is discarded
/// (its card `decision`, when still open, closes saying why).
fn nothing_left(
    tx: &Tx,
    link: &OutLink,
    row: &OutRow,
    decision: Option<&str>,
) -> ChannelResult<()> {
    if !set_status(tx, &row.id, &["draft"], DISCARDED)? {
        return Ok(());
    }
    link.push(After::Discarded {
        message_id: row.id.clone(),
    });
    if let Some(d) = decision {
        crate::presence::settle(tx, d, BY_NOTHING_LEFT)?;
    }
    base::audit(
        tx,
        &row.ws,
        "steve",
        "chan_message.nothing_left",
        &row.id,
        &json!({"decision": decision, "why": BY_NOTHING_LEFT}),
    )?;
    messages::event(tx, &row.ws, &row.space_id, &row.id);
    Ok(())
}

/// The disclosure check of a draft on your own account, after it was written (its basis is
/// [`basis::CHECKING`]): nothing waits on it — the draft waits for your approval anyway — so the
/// check model gets its patient time limit and one more try ([`CheckCx::patient`]), against the
/// knowledge the draft was written with, the conversation the audience can read and the samples of
/// how you talk the composer was shown. Under YOLO (which never approves a flagged text) a flagged
/// reply the composer wrote is rewritten once first, by deleting only; no text left at all: the draft
/// is dropped; only hedges left: not used, the original stays flagged for you. Then the basis and the card say what it found: nothing, `（可能泄露：…）` with
/// “改写后再看”, or `（检查没跑完：…）` — never “可能泄露” for a check that did not finish. A draft
/// dealt with meanwhile keeps its card; its basis already says the check did not finish
/// ([`basis::checking_stopped`]). On a task of its own ([`Outbox::send_due`] finds such drafts in
/// the store, so a restart does not lose one).
async fn check_later(
    ch: &Channels,
    link: &OutLink,
    pinned: Option<&Arc<dyn PinnedModel>>,
    message_id: &str,
) -> ChannelResult<()> {
    let now = ch.now();
    let Some(l) = ch.store().reader(|c| later_of(c, message_id, now))? else {
        return Ok(());
    };
    if l.b.verdict != basis::CHECKING || l.row.status != "draft" {
        return Ok(());
    }
    let mut res = check_again(ch, &l, &l.row.text).await;
    let mut text = l.row.text.clone();
    let mut rewrites: Vec<Rewrite> = Vec::new();
    let mut empty = false;
    let mut unused: Option<&str> = None;
    if res.verdict == Verdict::Leak
        && l.composed
        && !disclosure::not_run(&res)
        && ch.yolo_on(&l.row.ws)
    {
        let model = l.b.answer_model.clone().unwrap_or_default();
        match rewrite_once(ch, pinned, &l.audience, &text, &res, &model).await {
            Some(disclosure::Rewrite::Kept(t)) => {
                rewrites.push(Rewrite {
                    from: text.clone(),
                    to: t.clone(),
                    reason: reasons(&res),
                });
                res = check_again(ch, &l, &t).await;
                text = t;
            }
            Some(disclosure::Rewrite::Empty) => empty = true,
            Some(disclosure::Rewrite::Hollow(_)) => unused = Some(NOTE_HOLLOW),
            Some(disclosure::Rewrite::Added) => unused = Some(NOTE_ADDED),
            None => {}
        }
    }
    let leak = res.verdict == Verdict::Leak;
    ch.store().transaction(|tx| -> ChannelResult<()> {
        let Some(cur) = out_row(tx, &l.row.id)? else {
            return Ok(());
        };
        // edited meanwhile: what was checked is not what waits
        if cur.text != l.row.text {
            basis::checking_stopped(tx, &cur.id, R_DEALT_FIRST)?;
            return Ok(());
        }
        let settled = basis::settle(
            tx,
            &l.b.id,
            &NewBasis {
                ws: l.row.ws.clone(),
                message_id: l.row.id.clone(),
                entries: res.entries.clone(),
                rewrites: rewrites.clone(),
                verdict: verdict_code(&res, !rewrites.is_empty()).into(),
                answer_model: l.b.answer_model.clone(),
                check_model: res.check_model.clone(),
                spans: if res.verdict == Verdict::Pass {
                    Vec::new()
                } else {
                    res.spans.clone()
                },
            },
        )?;
        if !settled || cur.status != "draft" {
            return Ok(());
        }
        let Some(d) = cur.decision_id.as_deref() else {
            return Ok(());
        };
        let open: bool = tx
            .query_row(
                "SELECT status = 'open' FROM decisions WHERE id = ?",
                [d],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(false);
        if !open {
            return Ok(());
        }
        if empty {
            return nothing_left(tx, link, &cur, Some(d));
        }
        let mut note = format!("{}{}", unused.unwrap_or_default(), check_note(&res));
        if !rewrites.is_empty() {
            note = format!("（已改写）{note}");
            tx.execute(
                "UPDATE chan_messages SET text = ? WHERE id = ? AND status = 'draft'",
                params![text, cur.id],
            )?;
            tx.execute(
                "UPDATE decisions SET draft = ? WHERE id = ?",
                params![text, d],
            )?;
        }
        tx.execute(
            "UPDATE decisions SET question = replace(question, ?, ?) WHERE id = ?",
            params![CHECKING_NOTE, note, d],
        )?;
        // a model's text the check flagged: the card can ask for the rewrite
        if leak && l.composed && !disclosure::not_run(&res) {
            tx.execute(
                "UPDATE decisions SET options = ? WHERE id = ?",
                params![json!(approval_options(tx, &cur, true)?).to_string(), d],
            )?;
        }
        base::audit(
            tx,
            &cur.ws,
            "steve",
            "chan_message.checked",
            &cur.id,
            &json!({"decision": d, "verdict": verdict_code(&res, !rewrites.is_empty()),
                    "check_model": res.check_model}),
        )?;
        tx.publish(steve_types::DomainEvent::from(
            steve_types::WorkEvent::Decision {
                ws: cur.ws.clone().into(),
                id: d.into(),
                item_id: Some(None),
            },
        ));
        messages::event(tx, &cur.ws, &cur.space_id, &cur.id);
        Ok(())
    })
}

/// Drafts whose disclosure check runs after they were written ([`check_later`]).
fn checks_waiting(c: &Connection) -> ChannelResult<Vec<String>> {
    let mut st = c.prepare_cached(
        "SELECT m.id FROM chan_messages m JOIN reply_basis b ON b.id = m.basis_id
          WHERE m.direction = 'out' AND m.status = 'draft' AND b.verdict = ?
          ORDER BY m.created LIMIT 20",
    )?;
    let rows = st
        .query_map([basis::CHECKING], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// “改写后再看”: the flagged draft `message_id` is rewritten once by the model that wrote it — by
/// deleting the flagged parts only ([`disclosure::judge_rewrite`]) — checked again and put back as a
/// draft with a new card (still flagged: it says so, and offers another rewrite). A rewrite that
/// wrote something the draft did not say is discarded: the text stays and the card says so; so is one
/// that leaves only hedges. One that leaves no text at all means there is nothing to say: the draft
/// is dropped. Nothing is sent.
/// A failed rewrite keeps the text and says so on the card. Only while the draft still waits on the
/// card `decision_id` the choice closed (an undo reopened it: nothing). Runs on a task of its own
/// ([`Outbox::send_due`] finds the choice in the store, so a restart does not lose it).
async fn rewrite_draft(
    ch: &Channels,
    link: &OutLink,
    pinned: Option<&Arc<dyn PinnedModel>>,
    message_id: &str,
    decision_id: &str,
) -> ChannelResult<()> {
    let now = ch.now();
    let loaded = ch.store().reader(|c| -> ChannelResult<Option<(Later, bool, bool, Option<String>)>> {
        let Some(row) = out_row(c, message_id)? else {
            return Ok(None);
        };
        let d = base::decisions::get(c, decision_id)?;
        let chosen = d.as_ref().is_some_and(|d| {
            d.status == "resolved" && d.result["choice"].as_str() == Some(REWRITE)
        });
        if row.status != "draft" || row.decision_id.as_deref() != Some(decision_id) || !chosen {
            return Ok(None);
        }
        let l = later_of(c, message_id, now)?
            .ok_or_else(|| ApiError::rule("原始披露依据已不可用，无法改写"))?;
        // A previous send attempt used this UUID, even if a failed rewrite reopened an approve card.
        let recovering: bool = c.query_row(
            "SELECT EXISTS(SELECT 1 FROM decisions WHERE workspace_id=? AND subject_type=? AND subject_id=? AND kind='fail')",
            params![row.ws, SUBJECT_MESSAGE, row.id], |r| r.get(0),
        )?;
        let review = recovering || d.as_ref().is_some_and(|d| d.result["recovery_review"] == true);
        let edited = d.as_ref().filter(|d| d.kind == "fail")
            .and_then(|d| d.result["text"].as_str())
            .filter(|t| !t.trim().is_empty()).map(|t| t.trim().to_string());
        Ok(Some((l, recovering, review, edited)))
    })?;
    let Some((l, recovering, review, edited)) = loaded else {
        return Ok(());
    };
    let row = &l.row;
    let spans: Vec<steve_modules::outbound::Span> =
        serde_json::from_value(l.b.spans.clone()).unwrap_or_default();
    let flagged = CheckResult {
        verdict: Verdict::Leak,
        entries: Vec::new(),
        spans,
        check_model: l.b.check_model.clone(),
    };
    let model = l.b.answer_model.clone().unwrap_or_default();
    // A user's replacement is intentional new content; a model rewrite must only delete.
    let new = match edited {
        Some(t) => Some(disclosure::Rewrite::Kept(t)),
        None => rewrite_once(ch, pinned, &l.audience, &row.text, &flagged, &model).await,
    }
    .filter(|r| !matches!(r, disclosure::Rewrite::Kept(t) if t.chars().count() > TEXT_MAX));
    let checked = match &new {
        Some(disclosure::Rewrite::Kept(t)) => Some(check_again(ch, &l, t).await),
        _ => None,
    };
    ch.store().transaction(|tx| -> ChannelResult<()> {
        // answered or edited meanwhile: the rewrite is not wanted any more
        let Some(cur) = out_row(tx, &row.id)? else {
            return Ok(());
        };
        if cur.status != "draft"
            || cur.decision_id.as_deref() != Some(decision_id)
            || cur.text != row.text
        {
            return Ok(());
        }
        if !base::decisions::get(tx, decision_id)?.is_some_and(|d| d.status == "resolved" && d.result["choice"] == REWRITE) {
            return Ok(());
        }
        let mut row = row.clone();
        if recovering && !matches!(new, Some(disclosure::Rewrite::Empty)) {
            replace_recovery_draft(tx, &mut row, decision_id, now)?;
        }
        let mut q = format!("{}发到「{}」", l.p.speaker, spaces::display_title(tx, &l.p.space)?);
        let text = match (&new, &checked) {
            (Some(disclosure::Rewrite::Kept(t)), Some(r)) => {
                let leak = r.verdict != Verdict::Pass;
                let mut rewrites: Vec<Rewrite> =
                    serde_json::from_value(l.b.rewrites.clone()).unwrap_or_default();
                rewrites.push(Rewrite {
                    from: row.text.clone(),
                    to: t.clone(),
                    reason: reasons(&flagged),
                });
                basis::insert(
                    tx,
                    &NewBasis {
                        ws: row.ws.clone(),
                        message_id: row.id.clone(),
                        entries: r.entries.clone(),
                        rewrites,
                        verdict: verdict_code(r, true).into(),
                        answer_model: l.b.answer_model.clone(),
                        check_model: r.check_model.clone(),
                        spans: if leak { r.spans.clone() } else { Vec::new() },
                    },
                    now,
                )?;
                q.push_str("（已改写）");
                q.push_str(&check_note(r));
                tx.execute(
                    "UPDATE chan_messages SET text = ? WHERE id = ? AND status = 'draft'",
                    params![t, row.id],
                )?;
                let again = r.verdict == Verdict::Leak && !disclosure::not_run(r);
                open_approval(tx, &row.ws, &row.id, q, t, again)?;
                t.clone()
            }
            (Some(disclosure::Rewrite::Empty), _) => {
                nothing_left(tx, link, &cur, None)?;
                tx.execute(
                    "UPDATE decisions SET result = json_set(coalesce(result, '{}'), '$.superseded', ?) WHERE id = ?",
                    params![BY_NOTHING_LEFT, decision_id],
                )?;
                return Ok(());
            }
            (Some(disclosure::Rewrite::Added), _) => {
                q.push_str(NOTE_ADDED);
                open_approval(tx, &row.ws, &row.id, q, &row.text, true)?;
                row.text.clone()
            }
            (Some(disclosure::Rewrite::Hollow(_)), _) => {
                q.push_str(NOTE_HOLLOW);
                open_approval(tx, &row.ws, &row.id, q, &row.text, true)?;
                row.text.clone()
            }
            _ => {
                q.push_str("（没能改写，原文未变）");
                open_approval(tx, &row.ws, &row.id, q, &row.text, true)?;
                row.text.clone()
            }
        };
        if review {
            let decision = out_row(tx, &row.id)?.and_then(|r| r.decision_id)
                .ok_or_else(|| ApiError::not_found("审批卡"))?;
            mark_recovery_review(tx, &decision)?;
        }
        base::audit(
            tx,
            &row.ws,
            "steve",
            "chan_message.rewritten",
            &row.id,
            &json!({"decision": decision_id, "changed": text != row.text}),
        )?;
        messages::event(tx, &row.ws, &row.space_id, &row.id);
        Ok(())
    })
}

fn replace_recovery_draft(
    tx: &Tx,
    row: &mut OutRow,
    decision_id: &str,
    now: i64,
) -> ChannelResult<()> {
    // A changed payload must never reuse a UUID that reached the provider. Retain both
    // its disclosure basis and composer provenance when an invalid rewrite reopens it.
    let next = uid("cm");
    tx.execute("INSERT INTO chan_messages (id,workspace_id,space_id,thread_key,reply_to,direction,sender_account_id,persona_id,on_behalf,text,status,created,seen_by,basis_id,composed)
        SELECT ?,workspace_id,space_id,thread_key,reply_to,direction,sender_account_id,persona_id,on_behalf,text,'draft',?,seen_by,basis_id,composed FROM chan_messages WHERE id=?",
        params![next, now, row.id])?;
    digest::sync_message(tx, DigestSource::ChanMessage, &next, None)?;
    set_status(tx, &row.id, &["draft"], DISCARDED)?;
    tx.execute(
        "UPDATE output_deliveries SET message_id=? WHERE message_id=?",
        params![next, row.id],
    )?;
    tx.execute(
        "UPDATE decisions SET result=json_set(result,'$.draft_message',?) WHERE id=?",
        params![next, decision_id],
    )?;
    messages::event(tx, &row.ws, &row.space_id, &row.id);
    row.id = next;
    Ok(())
}

/// The flagged draft could not be rewritten at all (its space or account is gone, the store failed): its
/// text comes back on a card as it was, so it is not left without one.
fn rewrite_failed(ch: &Channels, message_id: &str, decision_id: &str) -> ChannelResult<()> {
    ch.store().transaction(|tx| -> ChannelResult<()> {
        let Some(row) = out_row(tx, message_id)? else {
            return Ok(());
        };
        if row.status != "draft" || row.decision_id.as_deref() != Some(decision_id) {
            return Ok(());
        }
        if !base::decisions::get(tx, decision_id)?
            .is_some_and(|d| d.status == "resolved" && d.result["choice"] == REWRITE)
        {
            return Ok(());
        }
        let name = spaces::get(tx, &row.space_id)?
            .map(|s| spaces::display_title(tx, &s))
            .transpose()?
            .unwrap_or_default();
        open_approval(
            tx,
            &row.ws,
            &row.id,
            format!("发到「{name}」（没能改写，原文未变）"),
            &row.text,
            true,
        )?;
        messages::event(tx, &row.ws, &row.space_id, &row.id);
        Ok(())
    })
}

/// How many stale cards one sweep looks at ([`Outbox::expire_stale`]).
pub const EXPIRY_PAGE: usize = 100;

/// An open card the expiry sweep may close.
#[derive(Debug, Clone)]
struct StaleCard {
    id: String,
    ws: String,
    kind: String,
    subject: String,
    space: String,
    created: i64,
}

/// Open message cards past their account's `card_ttl_h` at `now`, oldest first after `after`: on an
/// account on your own account; a hand-off about someone's message, or a draft in your name (not a
/// persona's, not a proxy's) answering one and still a draft; neither a starred contact's
/// ([`crate::decide::STARRED`]) nor one asking you to commit ([`crate::decide::Reason::Commitment`],
/// [`COMMITS`]).
fn stale_cards(
    c: &Connection,
    now: i64,
    after: Option<&(i64, String)>,
) -> ChannelResult<Vec<StaleCard>> {
    let (after_at, after_id) = after.cloned().unwrap_or((i64::MIN, String::new()));
    let commitment = format!(
        "{}{}",
        crate::decide::WHY_HEAD,
        crate::decide::Reason::Commitment.label()
    );
    let mut st = c.prepare_cached(
        "SELECT d.id, d.workspace_id, d.kind, d.subject_id, m.space_id, d.created
           FROM decisions d INDEXED BY decisions_chan_open
           JOIN chan_messages m ON m.id = d.subject_id
           JOIN spaces s ON s.id = m.space_id
           JOIN channel_settings cs ON cs.connection_id = s.connection_id AND cs.workspace_id = d.workspace_id
          WHERE d.subject_type = 'chan_message' AND d.status = 'open'
            AND cs.mode = 'self' AND cs.card_ttl_h IS NOT NULL
            AND d.created < ?1 - cs.card_ttl_h * 3600000
            AND ((d.kind = 'ask' AND m.direction = 'in')
              OR (d.kind = 'approve' AND m.direction = 'out' AND m.status = 'draft' AND m.reply_to IS NOT NULL
                  AND m.on_behalf = 'none' AND m.persona_id IS NULL))
            AND substr(d.question, 1, length(?2)) <> ?2
            AND instr(d.question, ?3) = 0 AND instr(d.question, ?4) = 0
            AND (d.created > ?5 OR (d.created = ?5 AND d.id > ?6))
          ORDER BY d.created, d.id LIMIT ?7",
    )?;
    let rows = st
        .query_map(
            params![
                now,
                crate::decide::STARRED,
                commitment,
                COMMITS,
                after_at,
                after_id,
                EXPIRY_PAGE as i64
            ],
            |r| {
                Ok(StaleCard {
                    id: r.get(0)?,
                    ws: r.get(1)?,
                    kind: r.get(2)?,
                    subject: r.get(3)?,
                    space: r.get(4)?,
                    created: r.get(5)?,
                })
            },
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Cards answered “告诉它怎么回” whose words the composer has not drafted with yet (`result.composed`
/// unset), resolved within [`LATE_MS`] (a reply asked for longer ago is not drafted late by itself,
/// as a send is not): `(ws, decision, message, your words)`.
fn instructions_asked(
    c: &Connection,
    now: i64,
) -> ChannelResult<Vec<(String, String, String, String)>> {
    let mut st = c.prepare_cached(
        "SELECT workspace_id, id, subject_id, coalesce(json_extract(result, '$.text'), '')
           FROM decisions INDEXED BY decisions_chan_answered
          WHERE subject_type = 'chan_message' AND kind = 'ask' AND status = 'resolved' AND resolved >= ?
            AND json_extract(result, '$.choice') = ? AND json_extract(result, '$.composed') IS NULL
          ORDER BY resolved LIMIT 20",
    )?;
    let rows = st
        .query_map(params![now - LATE_MS, respond::INSTRUCT], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// “告诉它怎么回” on card `decision` about message `message`: the composer writes the reply with your
/// words as its instruction (so neither the checks of writing as you nor the decision rules
/// override it), and it goes the usual way: checked, then a draft for your approval. When it does
/// not come out as a reply, a new card says so. Drafted once (`result.composed`). Through the
/// responder's outbound (the [`Outbox`] in the Hub), on a task of its own.
async fn compose_instructed(
    ch: &Channels,
    ws: &str,
    decision: &str,
    message: &str,
    words: &str,
) -> ChannelResult<()> {
    let plan = ch.store().reader(|c| plan_of(c, ws, message))?;
    let r = match ch.responder().composer() {
        Some(composer) => {
            let opts = compose::ComposeOpts {
                instruction: Some(instruction_of(words)),
                ..Default::default()
            };
            composer
                .generate(ch, &plan, &opts)
                .await
                .map_err(|e| e.to_string())
        }
        None => Err("没有起草器".to_string()),
    };
    match r {
        Ok(r) if r.generation.outcome == crate::deliver::Outcome::Reply => {
            let Some(out) = ch.responder().outbound() else {
                return Err(ApiError::Internal("没有出站".into()));
            };
            let d = out
                .deliver(
                    ch,
                    &OutRequest {
                        target: OutTarget::reply(&plan),
                        text: r.text,
                        purpose: Purpose::Reply,
                        approve: true,
                        generation: Some(r.generation),
                        auth: Auth::None,
                        operation_id: None,
                    },
                )
                .await?;
            let id = d.message_id().unwrap_or("skipped").to_string();
            ch.store()
                .transaction(|tx| mark_composed(tx, decision, &id))?;
        }
        other => {
            if let Err(e) = &other {
                tracing::warn!(decision, "drafting with your words: {e}");
            }
            ch.store().transaction(|tx| -> ChannelResult<()> {
                if mark_composed(tx, decision, "failed")? {
                    crate::decide::open_card(
                        tx,
                        &plan,
                        &crate::decide::Card {
                            summary: String::new(),
                            reason: crate::decide::Reason::Instruction,
                            note: None,
                            draft: String::new(),
                            starred: false,
                        },
                    )?;
                }
                Ok(())
            })?;
        }
    }
    Ok(())
}

/// What the composer is told when you answered a card with how to reply ([`respond::INSTRUCT`]).
pub fn instruction_of(words: &str) -> String {
    format!(
        "我看过这条消息，这样回：{}\n按我的意思写一条回复，用我的口吻，outcome 写 reply。",
        words.trim()
    )
}

/// Marks a card answered with your words as drafted (`result.composed`: the draft's id, or
/// `failed`), so it is drafted once.
fn mark_composed(tx: &Tx, decision: &str, what: &str) -> ChannelResult<bool> {
    Ok(tx.execute(
        "UPDATE decisions SET result = json_set(coalesce(result, '{}'), '$.composed', ?)
          WHERE id = ? AND json_extract(result, '$.composed') IS NULL",
        params![what, decision],
    )? == 1)
}

/// Approves draft `m` (its approval `dd`) as answered from card `card`: pending, sent after the
/// account's delay, undoable within it. Returns when it goes out.
fn approve_draft(
    tx: &Tx,
    ws: &str,
    m: &str,
    dd: &str,
    text: &str,
    card: &str,
    now: i64,
) -> ApiResult<i64> {
    let row = own_out(tx, ws, m)?;
    let send_at = now + send_delay_ms(tx, &row)?;
    crate::messages::set_status(
        tx,
        m,
        "UPDATE chan_messages SET status = 'pending', sent = NULL WHERE id = ? AND status = 'draft'",
        [m],
    )?;
    base::decisions::settle(
        tx,
        dd,
        &json!({"choice": "yes", "hash": text_hash(text), "send_at": send_at, "from_card": card}),
        "resolved",
        &steve_modules::decisions::undo_marker(SUBJECT_MESSAGE),
    )?;
    Ok(send_at)
}

/// A card answered “发这条草稿” ([`drafts_chosen`]).
struct Chosen {
    ws: String,
    decision: String,
    message: String,
    /// The card's draft.
    draft: String,
    /// Who answered.
    by: String,
}

/// Cards answered “发这条草稿” whose draft was not checked yet (`result.composed` unset), resolved
/// within [`LATE_MS`].
fn drafts_chosen(c: &Connection, now: i64) -> ChannelResult<Vec<Chosen>> {
    let mut st = c.prepare_cached(
        "SELECT workspace_id, id, subject_id, draft, coalesce(json_extract(result, '$.by'), 'steve')
           FROM decisions INDEXED BY decisions_chan_answered
          WHERE subject_type = 'chan_message' AND kind = 'ask' AND status = 'resolved' AND resolved >= ?
            AND json_extract(result, '$.choice') = ? AND json_extract(result, '$.composed') IS NULL
          ORDER BY resolved LIMIT 20",
    )?;
    let rows = st
        .query_map(params![now - LATE_MS, respond::SEND_DRAFT], |r| {
            Ok(Chosen {
                ws: r.get(0)?,
                decision: r.get(1)?,
                message: r.get(2)?,
                draft: r.get(3)?,
                by: r.get(4)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// The note on an approval of a card's draft the check flagged, or could not finish checking:
/// `{}` is what the check found ([`check_note`], without its brackets).
pub const CARD_DRAFT_FLAGGED: &str = "（卡片上的草稿{}）";

/// “发这条草稿” on card `decision` about `message`: the draft is a model's text (the composer's
/// candidate, a reply it meant for you, the voice check's last try, a starred contact's reply), so it
/// is checked for disclosure like any reply before it goes out (B1's outbound check for the space's
/// audience). It passes: approved by your click, sent after the usual delay, undoable within it. It
/// does not (or the check could not run): the usual approval, with the suspicious parts marked; you
/// decide again. The card keeps the verdict (`result.check`) and the message (`result.composed`).
/// Once per card.
async fn send_chosen(
    ch: &Channels,
    ws: &str,
    decision: &str,
    message: &str,
    text: &str,
    by: &str,
) -> ChannelResult<()> {
    let plan = ch.store().reader(|c| plan_of(c, ws, message))?;
    let t = OutTarget::reply(&plan);
    let now = ch.now();
    let (p, audience, said, seen) = ch.store().reader(|c| -> ChannelResult<_> {
        let p = place(c, &t.ws, &t.connection_id, &t.space_id, None)?;
        let a = disclosure::space_audience(c, &p.space, &p.account, &p.owner_user)?;
        let seen = seen_of(c, ws, &t.space_id, message, p.mode, now)?;
        Ok((p, a, audience_said(c, &t)?, seen))
    })?;
    let cx = CheckCx {
        ws: ws.into(),
        actor: match (&p.persona, p.mode) {
            (Some(per), _) => per.bot_id.clone(),
            (None, Mode::Own) => "me".into(),
            (None, Mode::Bot) => "steve".into(),
        },
        draft_model: None,
        audience_said: said,
        audience_seen: seen,
        style_samples: Vec::new(),
        skip_model: false,
        internal_skip: false,
        // on a task of its own: nobody waits on it
        patient: true,
    };
    let res = b1::check(ch.modules(), &cx, &audience, text, &[]).await;
    let leak = res.verdict != Verdict::Pass;
    let verdict = verdict_code(&res, false);
    let now = ch.now();
    ch.store().transaction(|tx| -> ChannelResult<()> {
        let pending: bool = tx.query_row(
            "SELECT json_extract(result, '$.composed') IS NULL FROM decisions WHERE id = ?",
            [decision],
            |r| r.get(0),
        )?;
        if !pending {
            return Ok(());
        }
        let (m, dd) = respond::open_draft(tx, &plan, text, now)?;
        crate::reactions::expect_answer(tx, &plan.message_id, &m)?;
        basis::insert(
            tx,
            &NewBasis {
                ws: ws.into(),
                message_id: m.clone(),
                entries: res.entries.clone(),
                rewrites: Vec::new(),
                verdict: verdict.into(),
                answer_model: None,
                check_model: res.check_model.clone(),
                spans: if leak { res.spans.clone() } else { Vec::new() },
            },
            now,
        )?;
        if leak {
            let note = check_note(&res);
            let note = note.trim_start_matches('（').trim_end_matches('）');
            tx.execute(
                "UPDATE decisions SET question = question || ? WHERE id = ?",
                params![CARD_DRAFT_FLAGGED.replace("{}", note), dd],
            )?;
        } else {
            approve_draft(tx, ws, &m, &dd, text, decision, now)?;
        }
        tx.execute(
            "UPDATE decisions SET result = json_set(coalesce(result, '{}'), '$.composed', ?, '$.check', ?)
              WHERE id = ?",
            params![
                m,
                match res.verdict {
                    Verdict::Pass => "pass",
                    Verdict::Leak => "leak",
                    Verdict::Unchecked => "unchecked",
                },
                decision
            ],
        )?;
        base::audit(
            tx,
            ws,
            by,
            if leak {
                "chan_message.draft"
            } else {
                "chan_message.approve"
            },
            &m,
            &json!({"decision": dd, "card": decision, "choice": respond::SEND_DRAFT,
                    "verdict": verdict}),
        )?;
        tx.publish(steve_types::DomainEvent::from(steve_types::WorkEvent::Decision { ws: ws.into(), id: decision.into(),
        item_id: Some(None), }));
        messages::event(tx, ws, &plan.space_id, &m);
        Ok(())
    })
}

/// Flagged drafts whose card was answered “改写后再看” and that wait for the rewrite.
fn rewrites_asked(c: &Connection) -> ChannelResult<Vec<(String, String)>> {
    let mut st = c.prepare_cached(
        "SELECT m.id, d.id FROM chan_messages m JOIN decisions d ON d.id = m.decision_id
          WHERE m.direction = 'out' AND m.status = 'draft' AND d.status = 'resolved' AND d.subject_type = ?
            AND d.subject_id = m.id AND json_extract(d.result, '$.choice') = ?
          ORDER BY m.created LIMIT 20",
    )?;
    let rows = st
        .query_map(params![SUBJECT_MESSAGE, REWRITE], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// A receipt from a fixed template (an item's “收到，已记为 …”, a handoff's “我请 … 来看。” or its failure
/// note): nothing a model wrote, so only the local checks run on it.
fn template(req: &OutRequest) -> bool {
    matches!(req.purpose, Purpose::Item | Purpose::HandoffAck) && req.generation.is_none()
}

/// Whether `auth` may let a message out without a draft on an account in `mode` (step 2). The
/// store check comes again in [`verify`].
pub fn direct_ok(mode: Mode, auth: &Auth) -> bool {
    match (mode, auth) {
        (_, Auth::None) => false,
        (Mode::Bot, Auth::Requester { .. }) => true,
        (Mode::Own, Auth::Requester { .. }) => false,
        (_, Auth::Decision { .. }) => true,
        (Mode::Bot, Auth::Rule { .. } | Auth::Grant { .. } | Auth::Step { .. }) => true,
        (Mode::Bot, Auth::Trigger { .. }) => false,
        (Mode::Own, Auth::Trigger { .. }) => true,
        // your approval of exactly this step (its text was on the card)
        (Mode::Own, Auth::Step { gate, .. }) => *gate,
        (Mode::Own, Auth::Rule { .. } | Auth::Grant { .. }) => false,
    }
}

/// An inbound message: `(external id, sender account, text)`.
type Source = (Option<String>, Option<String>, String);

fn source_in(c: &Connection, row: &OutRow, source: &str) -> ChannelResult<Option<Source>> {
    Ok(c.query_row(
        "SELECT external_id, sender_account_id, coalesce(text, '') FROM chan_messages
          WHERE id = ? AND workspace_id = ? AND space_id = ? AND direction = 'in'",
        params![source, row.ws, row.space_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )
    .optional()?)
}

/// The space as outbound `row` goes out: through the identity of its sender's account there (your
/// own account answering a trigger word, also where a bot is the voice), else through the space's
/// voice. `None` for an answer to a trigger word whose account is no longer in the chat: it goes
/// out as you or not at all.
pub(crate) fn sending_space(c: &Connection, row: &OutRow) -> ChannelResult<Option<SpaceRow>> {
    let mut space = spaces::get(c, &row.space_id)?.ok_or_else(|| ApiError::not_found("空间"))?;
    let conn: Option<String> = match &row.sender_account_id {
        Some(acc) => c
            .query_row(
                "SELECT i.connection_id FROM space_identities i JOIN connections cn ON cn.id = i.connection_id
                  WHERE i.space_id = ? AND cn.account_id = ? AND i.left_at IS NULL ORDER BY i.joined LIMIT 1",
                params![row.space_id, acc],
                |r| r.get(0),
            )
            .optional()?,
        None => None,
    };
    match conn {
        Some(conn) => space.connection_id = conn,
        None if row.on_behalf == "proxy" => return Ok(None),
        None => {}
    }
    Ok(Some(space))
}

/// Checks the authorization of `row` against the store. `Err` carries the reason for the audit.
pub fn verify(
    c: &Connection,
    row: &OutRow,
    auth: &Auth,
    now: i64,
) -> ChannelResult<Result<(), String>> {
    let Some(space) = sending_space(c, row)? else {
        return Ok(Err("not in the chat".into()));
    };
    let (account, mode) = bindings::connection_account(c, &row.ws, &space.connection_id)?;
    if spaces::settings(c, &space.id)?.attention == spaces::Attention::Blocked {
        return Ok(Err("space blocked".into()));
    }
    if row.sender_account_id.as_deref() != Some(account.as_str()) {
        return Ok(Err("sender".into()));
    }
    if let Err(why) = crate::pipeline::verify_route(c, &row.id, now)? {
        return Ok(Err(why));
    }
    if let Some(id) = &row.decision_id
        && let Some(d) = base::decisions::get(c, id)?
        && d.result["recovery_review"] == true
        && !matches!(auth, Auth::Decision { id: approved } if approved == id)
    {
        return Ok(Err("recovery draft requires its human approval".into()));
    }
    if !direct_ok(mode, auth) {
        return Ok(Err(format!("{} on {}", auth.code(), mode.code())));
    }
    let ok = match auth {
        Auth::None => false,
        Auth::Requester { user } => c.query_row(
            "SELECT EXISTS(SELECT 1 FROM output_deliveries d JOIN workspace_members m ON m.workspace_id=d.workspace_id AND m.user_id=d.requester WHERE d.message_id=? AND d.workspace_id=? AND d.requester=?)",
            params![row.id,row.ws,user], |r| r.get(0),
        )?,
        Auth::Decision { id } => {
            let d = base::decisions::get(c, id)?;
            d.is_some_and(|d| {
                let choice = d.result["choice"].as_str().unwrap_or("");
                d.workspace_id == row.ws
                    && d.subject_type == SUBJECT_MESSAGE
                    && d.subject_id == row.id
                    && d.status == "resolved"
                    && row.decision_id.as_deref() == Some(d.id.as_str())
                    && ((d.kind == "approve" && choice == "yes") || (d.kind == "fail" && choice == "retry"))
                    // Also fence approvals scheduled by older YOLO engines before this fix.
                    && !(d.result["recovery_review"] == true && b1::decided_by_yolo(&d.result))
                    && d.result["hash"].as_str() == Some(text_hash(&row.text).as_str())
                    && d.result["send_at"].as_i64().is_some_and(|t| t <= now)
            })
        }
        Auth::Rule { source, act } => {
            let set = spaces::settings(c, &space.id)?;
            let rules_v = match &set.rules {
                Some(r) => r.clone(),
                None => spaces::channel_settings(c, &row.ws, &space.connection_id)?
                    .map(|s| s.rules)
                    .unwrap_or(Value::Null),
            };
            let speaks = matches!(act, Act::Reply | Act::CreateItem(_) | Act::Handoff);
            let replies = speaks
                && rules::load(&rules_v, mode)
                    .iter()
                    .any(|r| r.act.code() == act.code());
            let src = source_in(c, row, source)?;
            !set.muted
                && set.flow_id.is_none()
                && replies
                && src.is_some_and(|(ext, _, _)| ext.is_some() && ext == row.reply_to)
        }
        Auth::Trigger { source } => {
            crate::arbiter::command(c, &row.ws, &space, &space.connection_id, source)?.is_some()
        }
        // only a grant for the flow version the run is running
        Auth::Grant {
            flow_id,
            flow_version,
            ..
        } => c.query_row(
            "SELECT EXISTS (SELECT 1 FROM grants WHERE workspace_id = ? AND subject_type = 'flow' AND subject_id = ?
                AND capability = ? AND resource_type = 'space' AND resource_id IN (?, '*') AND revoked IS NULL AND until > ?
                AND json_extract(scope, '$.flow_version') = ?)",
            params![row.ws, flow_id, crate::flow_actions::CAPABILITY, space.id, now, flow_version],
            |r| r.get(0),
        )?,
        Auth::Step { run_id, node_id, gate } => match steve_flow::runs::get(c, run_id) {
            Ok(Some(run)) if run.workspace_id == row.ws => {
                let ctx = &run.context;
                let by_gate = ctx["approved"][node_id] == json!(true)
                    || (ctx["resume"]["node"] == json!(node_id)
                        && ctx["resume"]["gate"] == json!(steve_flow::engine::GATE_YES));
                if *gate {
                    by_gate
                } else {
                    by_gate || ctx.get("approved_by").is_some_and(|v| !v.is_null())
                }
            }
            _ => false,
        },
    };
    Ok(if ok {
        Ok(())
    } else {
        Err(format!("{} not valid", auth.code()))
    })
}

/// Purposes said by themselves, not asked for by anyone: a rule's acknowledgements. Not drafted
/// where writes are refused for now ([`write_blocks`]); a rule's generated reply is not even
/// generated there ([`crate::Replies`]). Your trigger words, `/bot`, a flow and an agent's update go
/// on and are kept as not sent.
fn automatic(p: Purpose) -> bool {
    matches!(p, Purpose::Item | Purpose::HandoffAck)
}

/// Purposes whose sending answers the inbound message (its reaction turns “done”): a reply, your
/// `/bot`. An item's “收到，已记为…” does not — the item ending does.
fn completes(p: Purpose) -> bool {
    matches!(p, Purpose::Reply | Purpose::Trigger)
}

// ---------------------------------------------------------------- the outbox

/// What [`Outbox::send`] did.
#[derive(Debug, Clone, PartialEq)]
pub enum SendOutcome {
    /// Out; `external_id` is the provider's message id when it gave one.
    Sent { external_id: Option<String> },
    /// The authorization did not hold: back to a draft with its approval.
    Drafted { decision_id: String },
    /// The provider failed: `failed`, a `fail` decision (retry / rewrite for review / discard).
    Failed { error: String, decision_id: String },
    /// The provider refuses writes there for now ([`ProviderError::Blocked`]) and the one notice
    /// about it is already out: `failed`, no card ([`write_blocks`]).
    Blocked { error: String },
    /// Sent without an answer: `unknown`, it may be out (sent again under the same dedupe key
    /// while the provider drops repeats, else you are asked).
    Unknown { error: String },
    /// Not pending, already claimed, gone, or waiting for its connection.
    Skipped,
}

/// An `unknown` message: its id, the dedupe window of the side it went out on, its first try.
type Unknown = (String, Option<Duration>, Option<i64>);

/// C6's outbound. Share one `Arc<Outbox>` between [`crate::Replies::outbound`], the queue and the
/// item hook.
pub struct Outbox {
    background: Mutex<tokio::task::JoinSet<()>>,
    stopped: std::sync::atomic::AtomicBool,
    link: OutLink,
    as_user: BTreeMap<String, Arc<dyn ActAsUser>>,
    pinned: Option<Arc<dyn PinnedModel>>,
    /// Drafts being rewritten now (“改写后再看”), each on a task of its own.
    rewriting: Arc<Mutex<std::collections::HashSet<String>>>,
    /// Item conversions being resumed from their durable decision results.
    converting: Arc<Mutex<std::collections::HashSet<String>>>,
    /// Where the sweep of stale cards goes on (`created`, `id` of the last card it looked at), so
    /// cards it leaves open never hold up the ones after them ([`Outbox::expire_stale`]).
    expiry_cursor: Mutex<Option<(i64, String)>>,
    // Transient per-row deferrals do not change claims or authorization. A restart retries immediately.
    deferred: Mutex<std::collections::HashMap<String, i64>>,
    #[cfg(feature = "fake")]
    before_attempt: Mutex<Option<(Arc<tokio::sync::Barrier>, Arc<tokio::sync::Barrier>)>>,
}

impl Outbox {
    pub fn new(link: OutLink) -> Self {
        Self {
            background: Mutex::new(tokio::task::JoinSet::new()),
            stopped: std::sync::atomic::AtomicBool::new(false),
            link,
            as_user: BTreeMap::new(),
            pinned: None,
            rewriting: Arc::default(),
            converting: Arc::default(),
            expiry_cursor: Mutex::new(None),
            deferred: Mutex::default(),
            #[cfg(feature = "fake")]
            before_attempt: Mutex::new(None),
        }
    }

    fn spawn_background(&self, future: impl std::future::Future<Output = ()> + Send + 'static) {
        let mut tasks = lock(&self.background);
        if self.stopped.load(std::sync::atomic::Ordering::SeqCst) {
            return;
        }
        while tasks.try_join_next().is_some() {}
        tasks.spawn(future);
    }
    fn stop_background(&self) {
        self.stopped
            .store(true, std::sync::atomic::Ordering::SeqCst);
        lock(&self.background).abort_all();
        lock(&self.rewriting).clear();
    }

    /// Test-only barrier after claim, before the final authorization/attempt transaction.
    #[cfg(feature = "fake")]
    pub fn pause_before_attempt(
        &self,
        entered: Arc<tokio::sync::Barrier>,
        release: Arc<tokio::sync::Barrier>,
    ) {
        *lock(&self.before_attempt) = Some((entered, release));
    }

    /// Acts as you (sends, reacts, recalls) on your own connections (`Mode::Own`) of the adapter
    /// `a` serves ([`ActAsUser::adapter_id`]).
    pub fn act_as_user(mut self, a: Arc<dyn ActAsUser>) -> Self {
        self.as_user.insert(a.adapter_id().to_string(), a);
        self
    }

    /// How long the provider drops a repeat of a send through connection `conn` (of `adapter`):
    /// as you on your own account, as the connection's account otherwise.
    fn send_dedup(
        &self,
        ch: &Channels,
        c: &Connection,
        ws: &str,
        conn: &str,
        adapter: &str,
    ) -> Option<Duration> {
        let own = bindings::connection_account(c, ws, conn).is_ok_and(|(_, m)| m == Mode::Own);
        match (own, self.as_user.get(adapter)) {
            (true, Some(u)) => u.send_dedup(),
            _ => ch.adapter(adapter).and_then(|a| a.send_dedup()),
        }
    }

    /// The adapters it sends as you on.
    pub fn as_user_ids(&self) -> Vec<String> {
        self.as_user.keys().cloned().collect()
    }

    /// Rewrites on the drafting model when it was a pinned one.
    pub fn pinned(mut self, p: Arc<dyn PinnedModel>) -> Self {
        self.pinned = Some(p);
        self
    }

    pub fn link(&self) -> &OutLink {
        &self.link
    }

    fn bind(&self, ch: &Channels) {
        self.link.bind_clock(ch.modules().clock.clone());
    }

    /// [`Outbound::deliver`] with the check result (for `steve_reply_source`'s answer).
    pub async fn deliver_checked(
        &self,
        ch: &Channels,
        req: &OutRequest,
    ) -> ChannelResult<(Delivered, Option<CheckResult>)> {
        self.deliver_enveloped(ch, req, None).await
    }

    pub(crate) async fn deliver_enveloped(
        &self,
        ch: &Channels,
        req: &OutRequest,
        envelope: Option<&crate::pipeline::Envelope>,
    ) -> ChannelResult<(Delivered, Option<CheckResult>)> {
        self.bind(ch);
        if req
            .generation
            .as_ref()
            .is_some_and(|g| g.outcome != crate::deliver::Outcome::Reply)
        {
            return Ok((Delivered::Skipped, None));
        }
        if let Some(e) = envelope
            && let Some(d) = ch.store().reader(|c| crate::pipeline::existing(c, e))?
        {
            return Ok((crate::pipeline::outcome(&d), None));
        }
        let t = &req.target;
        // asked again for an operation that has its message: what became of it, nothing new
        if let Some(op) = &req.operation_id
            && let Some(d) = ch
                .store()
                .reader(|c| crate::deliver::operation_delivered(c, &t.ws, op))?
        {
            tracing::info!(
                message = d.message_id(),
                "operation already has its message: not sent again"
            );
            return Ok((d, None));
        }
        // an automatic answer where writes are refused for now is not drafted at all ([`write_blocks`])
        let explicit = ch.store().reader(|c| -> ChannelResult<bool> {
            match &req.auth {
                Auth::Decision { .. } | Auth::Trigger { .. } | Auth::Requester { .. } => Ok(true),
                Auth::Rule { source, .. } => {
                    write_blocks::explicit_request(c, &t.ws, &t.connection_id, &t.space_id, source)
                }
                _ => match &t.source_message_id {
                    Some(source) => write_blocks::explicit_request(
                        c,
                        &t.ws,
                        &t.connection_id,
                        &t.space_id,
                        source,
                    ),
                    None => Ok(false),
                },
            }
        })?;
        if automatic(req.purpose)
            && !explicit
            && let Some(why) = ch
                .store()
                .reader(|c| write_blocks::active(c, &t.connection_id, &t.space_id, ch.now()))?
        {
            tracing::info!(space = %t.space_id, purpose = ?req.purpose, "not drafted: {why}");
            return Ok((Delivered::Skipped, None));
        }
        let mut text = req.text.trim().to_string();
        if text.is_empty() {
            return Err(ApiError::invalid("text"));
        }
        if text.chars().count() > TEXT_MAX {
            text = text.chars().take(TEXT_MAX).collect();
        }
        let now = ch.now();
        let (p, audience, knowledge, said, seen) = ch.store().reader(|c| -> ChannelResult<_> {
            let p = place(
                c,
                &t.ws,
                &t.connection_id,
                &t.space_id,
                t.persona_id.as_deref(),
            )?;
            let a = disclosure::space_audience(c, &p.space, &p.account, &p.owner_user)?;
            let k = knowledge_of(c, req.generation.as_ref())?;
            let said = audience_said(c, t)?;
            let seen = match &t.source_message_id {
                Some(src) => seen_of(c, &t.ws, &t.space_id, src, p.mode, now)?,
                None => None,
            };
            Ok((p, a, k, said, seen))
        })?;
        // only the composer's reply speaks as you, and it waits for you (decided before any check runs)
        if let Some(why) = crate::deliver::not_as_you(p.mode, req) {
            tracing::info!(space = t.space_id, purpose = ?req.purpose, "not said: {why}");
            return Ok((Delivered::Skipped, None));
        }
        let approve = crate::deliver::must_approve(p.mode, req);
        let draft_model = req
            .generation
            .as_ref()
            .map(|g| g.model.clone())
            .filter(|m| !m.is_empty());
        let cx = CheckCx {
            ws: t.ws.clone(),
            actor: match (&p.persona, p.mode) {
                (Some(per), _) => per.bot_id.clone(),
                (None, Mode::Own) => "me".into(),
                (None, Mode::Bot) => "steve".into(),
            },
            draft_model: draft_model.clone(),
            audience_said: said,
            audience_seen: seen,
            style_samples: req
                .generation
                .as_ref()
                .map(|g| g.style_samples.clone())
                .unwrap_or_default(),
            // a fixed receipt (“收到，已记为 …”, “我请 … 来看。”) no model wrote: the rules only
            skip_model: template(req),
            // a bot's own reply whose knowledge was tracked (a generation) may skip the model for the
            // workspace's own people; text without one (an agent's, a flow's) and anything said as you
            // or on your behalf may not
            internal_skip: req.generation.is_some()
                && p.mode == Mode::Bot
                && req.purpose != Purpose::Trigger,
            patient: false,
        };

        // 1. disclosure (and the rewrites)
        let will_draft = approve || !direct_ok(p.mode, &req.auth);
        // On your own account a draft waits for your approval anyway: only the local checks run now (for
        // the basis), the check model runs after the draft is written, without hurry ([`check_later`]),
        // and the card says so until it is done
        let later = p.mode == Mode::Own && will_draft && !cx.skip_model;
        let now_cx = if later {
            CheckCx {
                skip_model: true,
                ..cx.clone()
            }
        } else {
            cx.clone()
        };
        let mut res = b1::check(ch.modules(), &now_cx, &audience, &text, &knowledge).await;
        let mut rewrites: Vec<Rewrite> = Vec::new();
        // a text that waits for the owner whatever the check says is not rewritten: they see the original
        // with the parts marked, and the card offers the rewrite. Unless YOLO decides the card: it never
        // approves a flagged text, so the one rewrite comes first, as for a direct reply (on your own
        // account, after the check that runs later). A rewrite may only delete; one left empty means
        // there is nothing to say (recorded); one that leaves only hedges is not used: the original
        // stays flagged and waits for the owner
        if res.verdict == Verdict::Leak
            && !later
            && req.generation.is_some()
            && !disclosure::not_run(&res)
            && (!will_draft || ch.yolo_on(&t.ws))
        {
            for _ in 0..MAX_REWRITES {
                let Some(judged) = rewrite_once(
                    ch,
                    self.pinned.as_ref(),
                    &audience,
                    &text,
                    &res,
                    draft_model.as_deref().unwrap_or(""),
                )
                .await
                else {
                    break;
                };
                let new = match judged {
                    disclosure::Rewrite::Kept(t) => t,
                    disclosure::Rewrite::Added => break,
                    disclosure::Rewrite::Hollow(_) => {
                        tracing::info!(space = t.space_id, "a rewrite left only hedges: not used");
                        break;
                    }
                    disclosure::Rewrite::Empty => {
                        tracing::info!(space = t.space_id, "not said: {BY_NOTHING_LEFT}");
                        // not said, and not silently: the audit says what was dropped and why
                        let reasons = reasons(&res);
                        ch.store().transaction(|tx| -> ChannelResult<()> {
                            base::audit(
                                tx,
                                &t.ws,
                                "steve",
                                "chan_message.nothing_left",
                                t.source_message_id.as_deref().unwrap_or(&t.space_id),
                                &json!({"space": t.space_id, "purpose": format!("{:?}", req.purpose),
                                        "flagged": reasons, "why": BY_NOTHING_LEFT}),
                            )?;
                            Ok(())
                        })?;
                        if completes(req.purpose)
                            && let Some(src) = &t.source_message_id
                        {
                            ch.react_finish(src, crate::reactions::Finish::Clear).await;
                        }
                        return Ok((Delivered::Skipped, Some(res)));
                    }
                };
                rewrites.push(Rewrite {
                    from: text.clone(),
                    to: new.clone(),
                    reason: reasons(&res),
                });
                text = new;
                res = b1::check(ch.modules(), &cx, &audience, &text, &knowledge).await;
                if res.verdict != Verdict::Leak || disclosure::not_run(&res) {
                    break;
                }
            }
        }
        let now = ch.now();
        // said as you on your trigger word (its answer, its item's ack, progress and end)
        let proxy = req.purpose == Purpose::Trigger || matches!(req.auth, Auth::Trigger { .. });
        // 2. authorization
        let leak = res.verdict != Verdict::Pass;
        let direct = !leak && !approve && direct_ok(p.mode, &req.auth);
        if !direct && req.purpose == Purpose::HandoffAck {
            // the handoff's own decision already reaches you
            return Ok((Delivered::Skipped, Some(res)));
        }
        let verdict = if later {
            basis::CHECKING
        } else {
            verdict_code(&res, !rewrites.is_empty())
        };
        let (id, decision, reused) = ch.store().transaction(|tx| -> ChannelResult<_> {
            // Concurrent submissions serialize here, before either can create a second message.
            if let Some(e) = envelope
                && let Some(d) = crate::pipeline::existing(tx, e)?
            {
                return Ok((String::new(), None, Some(crate::pipeline::outcome(&d))));
            }
            // … and so do two deliveries of one operation: the first to get here claims it
            if let Some(op) = &req.operation_id
                && let Some(d) = crate::deliver::operation_delivered(tx, &t.ws, op)?
            {
                return Ok((String::new(), None, Some(d)));
            }
            let id = insert_out(
                tx,
                &NewOut {
                    id: req
                        .operation_id
                        .as_ref()
                        .map(|op| crate::deliver::operation_message_id(&t.ws, op)),
                    t,
                    p: &p,
                    text: &text,
                    status: if direct { "pending" } else { "draft" },
                    proxy,
                },
                now,
            )?;
            if let Some(e) = envelope {
                crate::pipeline::record(tx, e, req, &id, now)?;
            }
            if let Some(g) = &req.generation {
                crate::decide::keep_composed(tx, &id, g)?;
            }
            // a direct one is found again under this authorization if the Hub stops before it is out
            if direct {
                send_state::keep(tx, &id, &req.auth)?;
            }
            if completes(req.purpose)
                && let Some(src) = &t.source_message_id
            {
                crate::reactions::expect_answer(tx, src, &id)?;
            }
            basis::insert(
                tx,
                &NewBasis {
                    ws: t.ws.clone(),
                    message_id: id.clone(),
                    entries: res.entries.clone(),
                    rewrites: rewrites.clone(),
                    verdict: verdict.into(),
                    answer_model: draft_model.clone(),
                    check_model: res.check_model.clone(),
                    spans: if leak { res.spans.clone() } else { Vec::new() },
                },
                now,
            )?;
            let decision = if direct {
                None
            } else {
                let mut q = format!(
                    "{}发到「{}」",
                    p.speaker,
                    spaces::display_title(tx, &p.space)?
                );
                if later {
                    q.push_str(CHECKING_NOTE);
                } else {
                    q.push_str(&check_note(&res));
                }
                // on your own account: the reply promises something in your name
                if p.mode == Mode::Own
                    && req.generation.as_ref().is_some_and(|g| g.needs_commitment)
                {
                    q.push_str(COMMITS);
                }
                // a model's text the check flagged: the card can ask for the rewrite it did not get
                let rewrite = !later
                    && res.verdict == Verdict::Leak
                    && req.generation.is_some()
                    && !disclosure::not_run(&res);
                Some(open_approval(tx, &t.ws, &id, q, &text, rewrite)?)
            };
            messages::event(tx, &t.ws, &t.space_id, &id);
            Ok((id, decision, None))
        })?;
        if let Some(d) = reused {
            return Ok((d, Some(res)));
        }
        if let Some(d) = decision {
            // waiting for your approval is not “working on it”: the acknowledgement comes off, and
            // the answer's own end (sent ✅ / failed ❌, `cleared → done | failed`) shows later
            if completes(req.purpose)
                && let Some(src) = &t.source_message_id
            {
                ch.react_finish(src, crate::reactions::Finish::Clear).await;
            }
            return Ok((
                Delivered::Drafted {
                    message_id: id,
                    decision_id: d,
                },
                Some(res),
            ));
        }

        // 3. send
        let d = match self.send(ch, &id, &req.auth).await? {
            SendOutcome::Sent { .. } => Delivered::Sent { message_id: id },
            SendOutcome::Drafted { decision_id } => Delivered::Drafted {
                message_id: id,
                decision_id,
            },
            SendOutcome::Failed { decision_id, .. } => Delivered::Failed {
                message_id: id,
                decision_id,
            },
            SendOutcome::Blocked { .. } => Delivered::Blocked { message_id: id },
            SendOutcome::Unknown { .. } | SendOutcome::Skipped => {
                Delivered::Queued { message_id: id }
            }
        };
        Ok((d, Some(res)))
    }

    /// The final text: the persona's signature, or “（由 Steve 代发）” after your trigger words.
    fn final_text(c: &Connection, row: &OutRow) -> ChannelResult<String> {
        let mut text = row.text.trim_end().to_string();
        // a trigger word's answer is marked `[AI] ` where it goes out (`send_as`), not here
        if row.on_behalf != "proxy"
            && let Some(pid) = &row.persona_id
            && let Some(p) = personas::get(c, pid)?
        {
            let sig = p.signature.trim();
            if !sig.is_empty() && !text.ends_with(sig) {
                text.push('\n');
                text.push_str(sig);
            }
        }
        Ok(text)
    }

    /// Sends one `pending` outbound message. The only caller of an adapter's `send(`: checks
    /// `auth` against the store, claims the message once, sends with `uuid = cm_…`, records the
    /// result.
    pub async fn send(&self, ch: &Channels, id: &str, auth: &Auth) -> ChannelResult<SendOutcome> {
        self.send_as(ch, id, auth, false).await
    }

    /// [`Outbox::send`]; `again`: an `unknown` message sent again under its dedupe key (only
    /// [`Outbox::settle_unknown`]). A message tried before goes out again only within the
    /// provider's dedupe window, counted from its first try; after it, it is `unknown` and you are
    /// asked. The claim (`sent` = now) fences the result: a late answer of a claim another took
    /// over changes nothing.
    async fn send_as(
        &self,
        ch: &Channels,
        id: &str,
        auth: &Auth,
        again: bool,
    ) -> ChannelResult<SendOutcome> {
        let Some(_activity) = ch.store().try_activity("channel_send") else {
            return Ok(SendOutcome::Skipped);
        };
        self.bind(ch);
        let now = ch.now();
        // claim, after checking the authorization
        let claimed = ch.store().transaction(|tx| -> ChannelResult<Result<OutRow, SendOutcome>> {
            let Some(row) = out_row(tx, id)? else {
                return Ok(Err(SendOutcome::Skipped));
            };
            let stale = row.sent.is_some_and(|t| t < now - CLAIM_STALE_MS);
            let pending = row.status == "pending" && (row.sent.is_none() || stale);
            if !(pending || (again && row.status == "unknown")) {
                return Ok(Err(SendOutcome::Skipped));
            }
            // a starred contact's notification you answered yourself meanwhile: let go, not sent
            if crate::notifications::answered(tx, &row.id, now)? {
                set_status(tx, &row.id, &["pending", "unknown"], DISCARDED)?;
                messages::event(tx, &row.ws, &row.space_id, &row.id);
                return Ok(Err(SendOutcome::Skipped));
            }
            if crate::notifications::deferred(tx, &row.id, now)? {
                tx.execute("UPDATE personal_notifications SET next_attempt_at=? WHERE delivery_id=(SELECT id FROM output_deliveries WHERE message_id=?)",params![now+10_000,row.id])?;
                return Ok(Err(SendOutcome::Skipped));
            }
            // tried before: again only while the provider drops repeats
            let tried = send_state::get(tx, &row.id)?.and_then(|s| s.first_at);
            if let Some(first) = tried {
                let adapter: Option<(String, String)> = tx
                    .query_row(
                        "SELECT c.id, c.adapter FROM spaces s JOIN connections c ON c.id = s.connection_id WHERE s.id = ?",
                        [&row.space_id],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )
                    .optional()?;
                let window = adapter
                    .and_then(|(conn, a)| self.send_dedup(ch, tx, &row.ws, &conn, &a))
                    .map(|d| d.as_millis() as i64);
                if !window.is_some_and(|w| now < first + w) {
                    crate::messages::set_status(
                        tx,
                        &row.id,
                        "UPDATE chan_messages SET status = 'unknown', sent = NULL WHERE id = ? AND status IN ('pending', 'unknown')",
                        [&row.id],
                    )?;
                    messages::event(tx, &row.ws, &row.space_id, &row.id);
                    return Ok(Err(SendOutcome::Skipped));
                }
            }
            // its space has no live connection any more (disconnected, cleaned up): it can never be
            // sent, so it fails once instead of being picked up by the queue forever
            let live: bool = tx
                .query_row(
                    "SELECT s.status != 'left' AND EXISTS (SELECT 1 FROM connections c
                       WHERE c.id = s.connection_id AND c.status != 'revoked')
                       FROM spaces s WHERE s.id = ?",
                    [&row.space_id],
                    |r| r.get(0),
                )
                .optional()?
                .unwrap_or(false);
            if !live {
                fail_orphaned_message(tx, &row, now)?;
                return Ok(Err(SendOutcome::Skipped));
            }
            // you answered yourself since, the sender's later message took it into its turn, or the
            // space no longer lets Steve reply for you: what YOLO or a rule let out is dropped; what
            // you approved yourself goes (crate::presence, gate 4)
            if !approved_by_you(tx, auth)?
                && let Some(e) = crate::presence::stop_out(tx, &row.ws, &row.id, now)?
            {
                drop_for_you(tx, &row, &["pending", "unknown"], None, &e)?;
                return Ok(Err(SendOutcome::Skipped));
            }
            if let Err(why) = verify(tx, &row, auth, now)? {
                // tried before and may be out: not a draft to approve again (a new text under the
                // same dedupe key would be dropped), it stays unknown and you are asked
                if tried.is_some() {
                    crate::messages::set_status(
                        tx,
                        &row.id,
                        "UPDATE chan_messages SET status = 'unknown', sent = NULL WHERE id = ? AND status IN ('pending', 'unknown')",
                        [&row.id],
                    )?;
                    messages::event(tx, &row.ws, &row.space_id, &row.id);
                    return Ok(Err(SendOutcome::Skipped));
                }
                // not authorized after all: back to a draft, nothing leaves
                set_status(tx, &row.id, &["pending", "unknown"], "draft")?;
                send_state::forget(tx, &row.id)?;
                let space = spaces::get(tx, &row.space_id)?.ok_or_else(|| ApiError::not_found("空间"))?;
                let speaker = place(tx, &row.ws, &space.connection_id, &row.space_id, row.persona_id.as_deref())?.speaker;
                let d = match base::decisions::open_for(tx, &row.ws, SUBJECT_MESSAGE, &row.id)? {
                    Some(d) if d.kind == "approve" => d.id,
                    _ => open_approval(tx, &row.ws, &row.id, format!("{speaker}发到「{}」", spaces::display_title(tx, &space)?), &row.text, false)?,
                };
                base::audit(
                    tx,
                    &row.ws,
                    "steve",
                    "chan_message.send.refused",
                    &row.id,
                    &json!({"auth": auth.code(), "why": why}),
                )?;
                messages::event(tx, &row.ws, &row.space_id, &row.id);
                return Ok(Err(SendOutcome::Drafted { decision_id: d }));
            }
            let n = crate::messages::set_status(
                tx,
                &row.id,
                "UPDATE chan_messages SET status = 'pending', sent = ?1 WHERE id = ?2
                   AND ((status = 'pending' AND (sent IS NULL OR sent < ?3)) OR (status = 'unknown' AND ?4))",
                params![now, row.id, now - CLAIM_STALE_MS, again],
            )?;
            Ok(if n == 1 { Ok(row) } else { Err(SendOutcome::Skipped) })
        })?;
        let row = match claimed {
            Ok(r) => r,
            Err(o) => return Ok(o),
        };
        // the claim's mark (`sent`): what a result must still find to be ours
        let claim = now;

        // what goes out, and whether it may
        let prep = ch.store().reader(|c| -> ChannelResult<_> {
            // as the account it was written for, or not at all (your answer, you no longer there)
            let space = sending_space(c, &row)?
                .ok_or_else(|| ApiError::rule("你的账号已经不在这个聊天里，不再回复"))?;
            let (account, mode) = bindings::connection_account(c, &row.ws, &space.connection_id)?;
            // the answer to a trigger word: `[AI] `-marked and placed as its settings say
            let trigger = (row.on_behalf == "proxy")
                .then(|| spaces::channel_settings(c, &row.ws, &space.connection_id))
                .transpose()?
                .flatten()
                .map(|s| Trigger::from_value(&s.trigger));
            let adapter: String = c.query_row(
                "SELECT adapter FROM connections WHERE id = ?",
                [&space.connection_id],
                |r| r.get(0),
            )?;
            let text = Self::final_text(c, &row)?;
            // `[AI] ` only on what YOLO approved in your name (your account, or your trigger words);
            // anything else loses a mark it brought along, so the mark cannot be faked
            let by_yolo = match auth {
                Auth::Decision { id } => base::decisions::get(c, id)?
                    .is_some_and(|d| b1::decided_by_yolo(&d.result)),
                _ => false,
            };
            let marked = trigger.as_ref().is_some_and(|t| t.mark);
            let text = if marked || (by_yolo && (mode == Mode::Own || row.on_behalf == "proxy")) {
                b1::with_ai_prefix(&text)
            } else {
                b1::strip_ai_prefix(&text).to_string()
            };
            let kind = SpaceKind::parse(&space.kind).unwrap_or(SpaceKind::Group);
            // a private chat has no threads to speak of: a quoting reply there
            let in_thread = match &trigger {
                Some(t) => t.reply == crate::rules::ReplyStyle::Thread && kind != SpaceKind::Dm,
                None => reply_in_thread(kind, row.thread_key.as_deref()),
            };
            // @ the person replied to in a group (bot accounts)
            let mut at = Vec::new();
            if mode == Mode::Bot && kind.is_group()
                && let Some(r) = &row.reply_to
            {
                let sender: Option<String> = c
                    .query_row(
                        "SELECT sender_account_id FROM chan_messages WHERE space_id = ? AND external_id = ? AND direction = 'in'",
                        params![row.space_id, r],
                        |x| x.get(0),
                    )
                    .optional()?
                    .flatten();
                if let Some(s) = sender.filter(|s| *s != account)
                    && let Some(h) = at_handle(c, &row.ws, &space.connection_id, &s)?
                {
                    at.push(h);
                }
            }
            Ok((space, mode, adapter, text, at, in_thread))
        });
        let (space, mode, adapter, text, at, in_thread) = match prep {
            Ok(x) => x,
            Err(e) if again || row.sent.is_some() => {
                return self.unknown(ch, &row, &e.to_string(), claim);
            }
            Err(e) => return self.failed(ch, &row, auth, &e.to_string(), None, claim),
        };
        let retry = ch.store().reader(|c| -> ChannelResult<bool> {
            Ok(
                matches!(auth, Auth::Decision { id } if base::decisions::get(c, id)?
                .is_some_and(|d| (d.kind == "fail" && d.result["choice"] == "retry") || d.result["recovery_review"] == true))
                    && send_state::get(c, &row.id)?.is_none_or(|s| s.first_at.is_none()),
            )
        })?;
        if retry {
            // Permission recovery is not approval of stale disclosure facts. Reload the audience
            // and original knowledge; no automatic rewrite or send when this check no longer passes.
            let l = ch.store().reader(|c| later_of(c, &row.id, ch.now()))?;
            let check = match &l {
                Some(l) => check_again(ch, l, &text).await,
                None => CheckResult {
                    verdict: Verdict::Leak,
                    entries: Vec::new(),
                    spans: vec![b1::Span {
                        text: String::new(),
                        reason: b1::R_BASIS_GONE.into(),
                    }],
                    check_model: None,
                },
            };
            let old = l.as_ref().map(|l| &l.b);
            let decision = ch
                .store()
                .transaction(|tx| -> ChannelResult<Option<String>> {
                    let current = out_row(tx, &row.id)?;
                    if !current.is_some_and(|r| {
                        r.status == "pending" && r.sent == Some(claim) && r.text == row.text
                    }) {
                        return Ok(None);
                    }
                    let pass = check.verdict == Verdict::Pass;
                    basis::insert(
                        tx,
                        &NewBasis {
                            ws: row.ws.clone(),
                            message_id: row.id.clone(),
                            entries: check.entries.clone(),
                            rewrites: old
                                .as_ref()
                                .and_then(|b| serde_json::from_value(b.rewrites.clone()).ok())
                                .unwrap_or_default(),
                            verdict: verdict_code(&check, false).into(),
                            answer_model: old.as_ref().and_then(|b| b.answer_model.clone()),
                            check_model: check.check_model.clone(),
                            spans: check.spans.clone(),
                        },
                        ch.now(),
                    )?;
                    if pass {
                        return Ok(None);
                    }
                    set_status(tx, &row.id, &["pending"], "draft")?;
                    send_state::forget(tx, &row.id)?;
                    let d = open_approval(
                        tx,
                        &row.ws,
                        &row.id,
                        format!(
                            "发到「{}」{}",
                            spaces::display_title(tx, &space)?,
                            check_note(&check)
                        ),
                        &row.text,
                        check.verdict == Verdict::Leak && !disclosure::not_run(&check),
                    )?;
                    messages::event(tx, &row.ws, &row.space_id, &row.id);
                    Ok(Some(d))
                })?;
            if let Some(decision_id) = decision {
                return Ok(SendOutcome::Drafted { decision_id });
            }
        }
        // the adapter, on the connection as it is now: paused, to be authorized again or revoked, or
        // (in your name) sending as you turned off since the approval → not sent
        let cap = (mode == Mode::Own).then_some(spaces::SEND_AS_USER_CAP);
        // paused or to be authorized again: it waits (unclaimed) and goes out once resumed
        let waits = ch
            .store()
            .reader(|c| may_write_in(c, &row.ws, &space.connection_id, cap))
            .ok()
            .is_some_and(|r| matches!(r, Err(WriteRefusal::Later(_))));
        if waits {
            ch.store().transaction(|tx| -> ChannelResult<()> {
                crate::messages::set_status(
                    tx,
                    &row.id,
                    "UPDATE chan_messages SET sent = NULL, status=CASE WHEN EXISTS(SELECT 1 FROM chan_send_state WHERE message_id=?1 AND first_at IS NOT NULL) THEN 'unknown' ELSE 'pending' END WHERE id = ?1 AND status = 'pending' AND sent=?2",
                    params![row.id, claim],
                )?;
                Ok(())
            })?;
            return Ok(SendOutcome::Skipped);
        }
        let cx = match ch.connections().cx_for(&row.ws, &space.connection_id, cap) {
            Ok(cx) => cx,
            Err(e) => {
                if again || row.sent.is_some() {
                    return self.unknown(ch, &row, &e.to_string(), claim);
                }
                return self.failed(ch, &row, auth, &e.to_string(), None, claim);
            }
        };
        #[cfg(feature = "fake")]
        {
            let pause = lock(&self.before_attempt).take();
            if let Some((entered, release)) = pause {
                entered.wait().await;
                release.wait().await;
            }
        }
        // Re-read live consent and the pinned recipient at the actual attempt boundary. No await
        // separates this transaction from the adapter invocation. A completed PUT before this point
        // wins; it cannot cancel an already-started external request.
        let attempted = ch.store().transaction(|tx| -> ChannelResult<Option<(String, Vec<String>)>> {
            let now = ch.now();
            let ours:bool=tx.query_row("SELECT EXISTS(SELECT 1 FROM chan_messages WHERE id=? AND status='pending' AND sent=?)",params![row.id,claim],|r|r.get(0))?;
            if !ours { return Ok(None); }
            if crate::notifications::answered(tx, &row.id, now)? {
                set_status(tx, &row.id, &["pending"], DISCARDED)?;
                messages::event(tx, &row.ws, &row.space_id, &row.id);
                return Ok(None);
            }
            if crate::notifications::deferred(tx, &row.id, now)?
                || verify(tx, &row, auth, now)?.is_err()
                || may_write_in(tx, &row.ws, &space.connection_id, cap)?.is_err()
            {
                crate::messages::set_status(
                    tx,
                    &row.id,
                    "UPDATE chan_messages SET sent=NULL,status=CASE WHEN EXISTS(SELECT 1 FROM chan_send_state WHERE message_id=?1 AND first_at IS NOT NULL) THEN 'unknown' ELSE 'pending' END WHERE id=?1 AND status='pending' AND sent=?2",
                    params![row.id,claim],
                )?;
                tx.execute("UPDATE personal_notifications SET next_attempt_at=? WHERE delivery_id=(SELECT id FROM output_deliveries WHERE message_id=?)",params![now+10_000,row.id])?;
                return Ok(None);
            }
            Ok(Some(send_state::attempt(tx, &row.id, auth, &text, &at, now)?))
        })?;
        let Some((text, at)) = attempted else {
            return Ok(SendOutcome::Skipped);
        };
        let sent: Result<Sent, ProviderError> = match (mode, self.as_user.get(&adapter)) {
            (Mode::Own, Some(u)) => {
                let to = match &row.reply_to {
                    Some(r) => {
                        let to = ch
                            .store()
                            .reader(|c| replied_ref(c, &row.space_id, &space.connection_id, r))
                            .unwrap_or_else(|_| MessageRef::platform(&space.external_id, r));
                        ActTarget::Reply { to, in_thread }
                    }
                    None => ActTarget::Space {
                        ext_id: space.external_id.clone(),
                    },
                };
                let mut content = OutContent::text(text.clone());
                content.uuid = Some(row.id.clone());
                content.space_ext_id = Some(space.external_id.clone());
                u.send(&cx, &to, &content, &SendKey(row.id.clone()))
                    .await
                    .map_err(provider_error)
            }
            _ => match ch.adapter(&adapter) {
                Some(a) => {
                    // Preserve source-only reply targets for adapters that support them. The
                    // default send_message bridge falls back to the space without a platform id.
                    let to = match &row.reply_to {
                        Some(r) => ActTarget::Reply {
                            to: ch
                                .store()
                                .reader(|c| replied_ref(c, &row.space_id, &space.connection_id, r))
                                .unwrap_or_else(|_| MessageRef::platform(&space.external_id, r)),
                            in_thread,
                        },
                        None => ActTarget::Space {
                            ext_id: space.external_id.clone(),
                        },
                    };
                    let mut content = OutContent::text(text.clone());
                    content.uuid = Some(row.id.clone());
                    content.at = at;
                    content.space_ext_id = Some(space.external_id.clone());
                    a.send_message(&cx, &to, &content, &SendKey(row.id.clone()))
                        .await
                        .map_err(provider_error)
                }
                None => Err(ProviderError::Rejected("这个渠道还不能发送消息".into())),
            },
        };
        match sent {
            Ok(out) => {
                let now = ch.now();
                let ext = fallback::platform(out.ext_id.clone());
                let ours = ch.store().transaction(|tx| -> ChannelResult<bool> {
                    // `sent_via`: recalled through the same connection later, whatever the space's
                    // voice is by then. The content key: a copy read back without a provider id,
                    // or of a send without one, is this message.
                    let key = fallback::out_key(ch, tx, &row.ws, &space.connection_id, &text)?;
                    if crate::messages::set_status(
                        tx,
                        &row.id,
                        "UPDATE chan_messages SET status = 'sent', external_id = ?, text = ?, sent = ?,
                                sent_via = ?, ext_created = ?, thread_key = coalesce(thread_key, ?),
                                content_key = ?
                          WHERE id = ? AND status = 'pending' AND sent = ?",
                        params![
                            ext,
                            text,
                            now,
                            space.connection_id,
                            out.created,
                            out.thread_key,
                            key,
                            row.id,
                            claim
                        ],
                    )? == 0
                    {
                        return Ok(false);
                    }
                    if let Some(src) = out.source_id.as_deref().filter(|s| !s.is_empty()) {
                        tx.execute(
                            "INSERT OR IGNORE INTO chan_message_seen (connection_id, source_id, message_id) VALUES (?, ?, ?)",
                            params![space.connection_id, src, row.id],
                        )?;
                    }
                    send_state::forget(tx, &row.id)?;
                    // it went through: neither the connection nor the chat refuses writes now
                    write_blocks::lift_after_send(tx, &space.connection_id, &row.space_id)?;
                    base::audit(
                        tx,
                        &row.ws,
                        "steve",
                        "chan_message.send",
                        &row.id,
                        &json!({"auth": auth.code(), "space": row.space_id, "on_behalf": row.on_behalf}),
                    )?;
                    messages::event(tx, &row.ws, &row.space_id, &row.id);
                    Ok(true)
                })?;
                if !ours {
                    tracing::info!(message = %row.id, "channel send answered after its claim was taken over");
                    return Ok(SendOutcome::Skipped);
                }
                ch.react_answered(&row.id, true).await;
                Ok(SendOutcome::Sent { external_id: ext })
            }
            // sent, no (readable) answer: it may be out. Not failed, not offered for a retry; the
            // queue sends it again under the same dedupe key while the provider drops repeats, or
            // asks you after
            Err(ProviderError::Transient(e)) => self.unknown(ch, &row, &e, claim),
            // A refusal of a later attempt says nothing about an earlier unanswered send. Keep
            // the original UUID/body and uncertainty evidence until it can be reconciled.
            Err(e) if again || row.sent.is_some() => self.unknown(ch, &row, &e.to_string(), claim),
            Err(e) => {
                // refused for good: remembered for the connection or the chat, one notice
                let block = match &e {
                    ProviderError::Blocked(scope, _) => {
                        Some((*scope, space.connection_id.as_str()))
                    }
                    _ => None,
                };
                let r = self.failed(ch, &row, auth, &e.to_string(), block, claim);
                ch.react_answered(&row.id, false).await;
                r
            }
        }
    }

    /// A send without an answer: `unknown`, the try kept ([`send_state`]).
    fn unknown(
        &self,
        ch: &Channels,
        row: &OutRow,
        error: &str,
        claim: i64,
    ) -> ChannelResult<SendOutcome> {
        tracing::info!(message = %row.id, error, "channel message: no answer, may be out");
        ch.store().transaction(|tx| -> ChannelResult<()> {
            if crate::messages::set_status(
                tx,
                &row.id,
                "UPDATE chan_messages SET status = 'unknown', sent = NULL WHERE id = ? AND status = 'pending' AND sent = ?",
                params![row.id, claim],
            )? == 0
            {
                return Ok(());
            }
            send_state::error(tx, &row.id, error)?;
            base::audit(
                tx,
                &row.ws,
                "steve",
                "chan_message.send.unknown",
                &row.id,
                &json!({"space": row.space_id}),
            )?;
            messages::event(tx, &row.ws, &row.space_id, &row.id);
            Ok(())
        })?;
        Ok(SendOutcome::Unknown {
            error: error.into(),
        })
    }

    /// The provider failed: `failed`, a `fail` decision (retry / rewrite for review / discard). A missing permission
    /// requires permission recovery before a human retries; YOLO must leave it to the user.
    /// `block`: the provider refuses writes on that connection (or in this chat). Remember it for
    /// automatic noise suppression, while every explicit request or manual retry keeps a card.
    fn failed(
        &self,
        ch: &Channels,
        row: &OutRow,
        auth: &Auth,
        error: &str,
        block: Option<(BlockScope, &str)>,
        claim: i64,
    ) -> ChannelResult<SendOutcome> {
        let permanent = missing_permission(error);
        tracing::info!(message = %row.id, error, blocked = block.is_some(), "channel message not sent");
        let now = ch.now();
        let d = ch.store().transaction(|tx| -> ChannelResult<Option<Option<String>>> {
            // only on our claim: a late answer of one another took over changes nothing
            if crate::messages::set_status(
                tx,
                &row.id,
                "UPDATE chan_messages SET status = 'failed', sent = NULL WHERE id = ? AND status = 'pending' AND sent = ?",
                params![row.id, claim],
            )? == 0
            {
                return Ok(None);
            }
            // A definitive refusal may clear the attempt only while this result still owns the
            // claim. A late refusal must not erase a newer unknown attempt's dedupe evidence.
            send_state::forget(tx, &row.id)?;
            tx.execute("UPDATE output_deliveries SET last_error=? WHERE message_id=?",params![error,row.id])?;
            let space =
                spaces::get(tx, &row.space_id)?.ok_or_else(|| ApiError::not_found("空间"))?;
            if let Some((scope, conn)) = block {
                let r = write_blocks::record(tx, &row.ws, conn, &row.space_id, scope, error, now)?;
                let explicit = match auth {
                    Auth::Rule { source, .. } => write_blocks::explicit_request(tx, &row.ws, conn, &row.space_id, source)?,
                    _ => true,
                };
                let d = if r.notice || explicit {
                    Some(open_fail(
                        tx,
                        row,
                        blocked_notice(scope, &spaces::display_title(tx, &space)?, error),
                        &BLOCKED_OPTIONS,
                    )?)
                } else {
                    None
                };
                if let Some(id) = &d {
                    recovery_metadata(tx, id, "channel_refusal", scope_code(scope), conn, &row.space_id, error)?;
                }
                base::audit(
                    tx,
                    &row.ws,
                    "steve",
                    "chan_message.send.blocked",
                    &row.id,
                    &json!({"space": row.space_id, "connection": conn, "scope": scope_code(scope),
                            "notice": d}),
                )?;
                messages::event(tx, &row.ws, &row.space_id, &row.id);
                return Ok(Some(d));
            }
            let d = if permanent {
                open_fail(
                    tx,
                    row,
                    format!(
                        "发到「{}」的消息没有发出去：{error}。请先到“接入”核对这项权限：平台缺少 scope 时需管理员开通后重新授权；Steve 本地关闭的代发能力可创建可审阅、可撤销的授权改动。权限处理后点重试；修改会重新审阅；不发送会关闭本条消息。清除拒绝缓存不会授予权限",
                        spaces::display_title(tx, &space)?
                    ),
                    &BLOCKED_OPTIONS,
                )?
            } else {
                open_fail(
                    tx,
                    row,
                    format!("发到「{}」的消息没有发出去：{error}", spaces::display_title(tx, &space)?),
                    &FAIL_OPTIONS,
                )?
            };
            if permanent {
                recovery_metadata(tx, &d, "missing_permission", "connection", &space.connection_id, &row.space_id, error)?;
            }
            base::audit(
                tx,
                &row.ws,
                "steve",
                "chan_message.send.failed",
                &row.id,
                &json!({"space": row.space_id, "error": error}),
            )?;
            messages::event(tx, &row.ws, &row.space_id, &row.id);
            Ok(Some(Some(d)))
        })?;
        Ok(match d {
            Some(Some(decision_id)) => SendOutcome::Failed {
                error: error.into(),
                decision_id,
            },
            Some(None) => SendOutcome::Blocked {
                error: error.into(),
            },
            None => SendOutcome::Skipped,
        })
    }

    /// Closes the cards about your own account still open after the account's `card_ttl_h`
    /// (`result.superseded` = [`crate::decide::BY_EXPIRY`]): a hand-off about someone's message where
    /// your own account speaks, and a draft in your name answering one (discarded). Unset (the
    /// default), nothing expires on time. Never a starred contact's card, nor one that asks you to
    /// commit to something (a commitment hand-off, a draft that promises): those wait for you. Cards
    /// where a bot speaks are left alone.
    ///
    /// The candidates are found in SQL (an account on your own account with a lifetime set, past it,
    /// neither starred nor a commitment), [`EXPIRY_PAGE`] at a time after
    /// [`Outbox::expiry_cursor`]: a card left open (its space's voice is a bot after all) is passed
    /// over, not looked at again on every tick. The checks run on a reader; only what closes is
    /// written. Returns how many closed.
    fn expire_stale(&self, ch: &Channels) -> ChannelResult<usize> {
        let now = ch.now();
        let after = lock(&self.expiry_cursor).clone();
        let (page, due) = ch.store().reader(|c| -> ChannelResult<_> {
            let page = stale_cards(c, now, after.as_ref())?;
            let mut due = Vec::new();
            for card in &page {
                let yours = match card.kind.as_str() {
                    "ask" => spaces::speaks_as_you(c, &card.ws, &card.space, None)?,
                    _ => crate::presence::answered_message(c, &card.ws, &card.subject)?.is_some(),
                };
                if yours {
                    due.push(card.clone());
                }
            }
            Ok((page, due))
        })?;
        *lock(&self.expiry_cursor) = if page.len() < EXPIRY_PAGE {
            None
        } else {
            page.last().map(|c| (c.created, c.id.clone()))
        };
        if due.is_empty() {
            return Ok(0);
        }
        let mut closed = 0;
        let mut discarded = Vec::new();
        ch.store().transaction(|tx| -> ChannelResult<()> {
            for card in &due {
                if card.kind == "approve" {
                    if !set_status(tx, &card.subject, &["draft"], DISCARDED)? {
                        continue;
                    }
                    discarded.push(card.subject.clone());
                }
                if crate::presence::settle(tx, &card.id, crate::decide::BY_EXPIRY)? {
                    closed += 1;
                    base::audit(
                        tx,
                        &card.ws,
                        "steve",
                        "chan_message.expired",
                        &card.subject,
                        &json!({"decision": card.id, "why": crate::decide::BY_EXPIRY}),
                    )?;
                    messages::event(tx, &card.ws, &card.space, &card.subject);
                }
            }
            Ok(())
        })?;
        for m in discarded {
            self.link.push(After::Discarded { message_id: m });
        }
        Ok(closed)
    }

    /// The queue (`channels.retry` and the approvals' delay): approved or retried messages whose
    /// delay is over are sent; handoffs answered “to_item” become items. Returns how many were
    /// sent.
    pub async fn send_due(&self, ch: &Channels) -> usize {
        // An explicit recovery pass must recheck freshly resumed connections immediately.
        // Background DueSource passes alone observe the transient per-row retry deadlines.
        lock(&self.deferred).clear();
        self.maintain(ch).await;
        self.send_ready(ch, ch.now()).await + self.settle_unknown(ch, ch.now()).await
    }

    // Conversions, expiry's cursor and draft checks are maintenance, not send deadlines.
    async fn maintain(&self, ch: &Channels) {
        self.bind(ch);
        for a in self.link.take() {
            match a {
                After::Discarded { message_id } => ch.react_discarded(&message_id).await,
            }
        }
        self.convert_due(ch).await;
        // cards about your own account that waited longer than the account allows close
        if let Err(e) = self.expire_stale(ch) {
            tracing::warn!("stale cards: {e}");
        }
        self.queue_instructed_drafts(ch);
        self.queue_chosen_drafts(ch);
        self.queue_disclosure_checks(ch);
        self.queue_rewrites(ch);
    }

    fn queue_instructed_drafts(&self, ch: &Channels) {
        // “告诉它怎么回”: the composer drafts with your words, each on a task of its own
        match ch.store().reader(|c| instructions_asked(c, ch.now())) {
            Ok(asked) => {
                for (ws, d, m, words) in asked {
                    if !lock(&self.rewriting).insert(d.clone()) {
                        continue;
                    }
                    let (ch, busy) = (ch.clone(), self.rewriting.clone());
                    self.spawn_background(async move {
                        if let Err(e) = compose_instructed(&ch, &ws, &d, &m, &words).await {
                            tracing::warn!(decision = d, "drafting with your words failed: {e}");
                        }
                        lock(&busy).remove(&d);
                    });
                }
            }
            Err(e) => tracing::warn!("cards answered with your words: {e}"),
        }
    }

    fn queue_chosen_drafts(&self, ch: &Channels) {
        // “发这条草稿”: the card's draft is checked first, each on a task of its own
        match ch.store().reader(|c| drafts_chosen(c, ch.now())) {
            Ok(chosen) => {
                for Chosen {
                    ws,
                    decision: d,
                    message: m,
                    draft,
                    by,
                } in chosen
                {
                    if !lock(&self.rewriting).insert(d.clone()) {
                        continue;
                    }
                    let (ch, busy) = (ch.clone(), self.rewriting.clone());
                    self.spawn_background(async move {
                        if let Err(e) = send_chosen(&ch, &ws, &d, &m, &draft, &by).await {
                            tracing::warn!(decision = d, "sending a card's draft failed: {e}");
                        }
                        lock(&busy).remove(&d);
                    });
                }
            }
            Err(e) => tracing::warn!("cards answered with their draft: {e}"),
        }
    }

    fn queue_disclosure_checks(&self, ch: &Channels) {
        // drafts on your own account whose disclosure check runs after they were written, each on a
        // task of its own
        match ch.store().reader(checks_waiting) {
            Ok(waiting) => {
                for m in waiting {
                    let key = format!("check:{m}");
                    if !lock(&self.rewriting).insert(key.clone()) {
                        continue;
                    }
                    let (ch, link, pinned, busy) = (
                        ch.clone(),
                        self.link.clone(),
                        self.pinned.clone(),
                        self.rewriting.clone(),
                    );
                    self.spawn_background(async move {
                        if let Err(e) = check_later(&ch, &link, pinned.as_ref(), &m).await {
                            tracing::warn!(message = m, "checking a draft failed: {e}");
                        }
                        lock(&busy).remove(&key);
                    });
                }
            }
            Err(e) => tracing::warn!("drafts to check: {e}"),
        }
    }

    fn queue_rewrites(&self, ch: &Channels) {
        // “改写后再看”: each on a task of its own, so a slow model does not hold up the sends
        match ch.store().reader(rewrites_asked) {
            Ok(asked) => {
                for (m, d) in asked {
                    if !lock(&self.rewriting).insert(m.clone()) {
                        continue;
                    }
                    let (ch, link, pinned, busy) = (
                        ch.clone(),
                        self.link.clone(),
                        self.pinned.clone(),
                        self.rewriting.clone(),
                    );
                    self.spawn_background(async move {
                        if let Err(e) = rewrite_draft(&ch, &link, pinned.as_ref(), &m, &d).await {
                            tracing::warn!(message = m, "rewriting a draft failed: {e}");
                            if let Err(e) = rewrite_failed(&ch, &m, &d) {
                                tracing::warn!(message = m, "putting the draft back: {e}");
                            }
                        }
                        lock(&busy).remove(&m);
                    });
                }
            }
            Err(e) => tracing::warn!("drafts to rewrite: {e}"),
        }
    }

    fn retry_deadlines(&self, now: i64) -> String {
        let mut deferred = lock(&self.deferred);
        deferred.retain(|_, at| *at > now);
        serde_json::to_string(&*deferred).expect("string deadlines serialize")
    }

    fn defer_row(&self, id: &str, now: i64) {
        lock(&self.deferred).insert(id.to_owned(), now + QUEUE_EVERY.as_millis() as i64);
    }

    async fn send_ready(&self, ch: &Channels, now: i64) -> usize {
        // B3: only due messages are selected before the LIMIT, so approved ones still inside their
        // delay can't fill the batch and starve those that are due
        let due = ch
            .store()
            .reader(|c| -> ChannelResult<Vec<(String, String)>> {
                let mut st = c.prepare_cached(&format!(
                    "{QUEUE_CANDIDATES}
                SELECT id, decision FROM candidates WHERE decision IS NOT NULL AND at<=?5
                ORDER BY at,created,id LIMIT 50"
                ))?;
                Ok(st
                    .query_map(
                        params![
                            CLAIM_STALE_MS,
                            SUBJECT_MESSAGE,
                            UNCLAIMED_MS,
                            self.retry_deadlines(now),
                            now
                        ],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )?
                    .collect::<rusqlite::Result<Vec<_>>>()?)
            });
        let due = match due {
            Ok(d) => d,
            Err(e) => {
                tracing::warn!("channel send queue: {e}");
                return 0;
            }
        };
        let mut n = 0;
        for (m, d) in due {
            self.defer_row(&m, now);
            match self.send(ch, &m, &Auth::Decision { id: d }).await {
                Ok(SendOutcome::Sent { .. }) => n += 1,
                Ok(_) => {}
                Err(e) => tracing::warn!(message = m, "channel send failed: {e}"),
            }
        }
        n + self.resume_direct(ch, now).await
    }

    async fn convert_due(&self, ch: &Channels) {
        let pending = ch
            .store()
            .reader(|c| -> ChannelResult<Vec<(String, String, String)>> {
                let mut st = c.prepare_cached(
                "SELECT id, workspace_id, json_extract(result, '$.source_message') FROM decisions
                  WHERE subject_type = 'chan_message' AND status = 'resolved'
                    AND json_extract(result, '$.choice') = 'to_item'
                    AND json_extract(result, '$.source_message') IS NOT NULL
                    AND json_extract(result, '$.item') IS NULL ORDER BY resolved LIMIT 50",
            )?;
                Ok(st
                    .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                    .collect::<Result<_, _>>()?)
            });
        let pending = match pending {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!("item conversions not read: {e}");
                return;
            }
        };
        for (decision, ws, source) in pending {
            if !lock(&self.converting).insert(decision.clone()) {
                continue;
            }
            let result = async {
                let prior: Option<String> = ch.store().reader(|c| -> ChannelResult<_> { Ok(c.query_row(
                    "SELECT work_item_id FROM chan_messages WHERE id = ? AND workspace_id = ? AND direction = 'in'",
                    params![source, ws], |r| r.get(0),
                )?) })?;
                let item = match prior {
                    Some(item) => item,
                    None => {
                        let mut plan = ch.store().reader(|c| plan_of(c, &ws, &source))?;
                        plan.act = Act::CreateItem(ItemMode::Start);
                        ch.responder().to_item(ch, &plan).await?
                            .and_then(|a| a.item_id)
                            .ok_or_else(|| ApiError::rule("转事项尚未完成"))?
                    }
                };
                ch.store().transaction(|tx| -> ChannelResult<()> {
                    tx.execute("UPDATE decisions SET result = json_set(result, '$.item', ?) WHERE id = ? AND status = 'resolved'", params![item, decision])?;
                    tx.publish(steve_types::DomainEvent::from(steve_types::WorkEvent::Decision { ws: ws.clone().into(), id: decision.clone().into(), item_id: None }));
                    Ok(())
                })
            }.await;
            lock(&self.converting).remove(&decision);
            if let Err(e) = result {
                tracing::warn!(decision, "handoff to item: {e}");
            }
        }
    }

    /// Direct messages (a rule's or a trigger's reply, no decision) whose send was cut off before
    /// it began, or that waited for their connection: sent under the authorization they were stored
    /// with (checked again by [`Outbox::send`]). One older than [`LATE_MS`] is asked about instead.
    async fn resume_direct(&self, ch: &Channels, now: i64) -> usize {
        let rows = ch
            .store()
            .reader(|c| -> ChannelResult<Vec<(String, i64, Option<i64>)>> {
                let mut st = c.prepare_cached(&format!(
                    "{QUEUE_CANDIDATES}
                SELECT id,created,first_at FROM candidates WHERE decision IS NULL AND at<=?5
                ORDER BY at,created,id LIMIT 50"
                ))?;
                Ok(st
                    .query_map(
                        params![
                            CLAIM_STALE_MS,
                            SUBJECT_MESSAGE,
                            UNCLAIMED_MS,
                            self.retry_deadlines(now),
                            now
                        ],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                    )?
                    .collect::<rusqlite::Result<Vec<_>>>()?)
            });
        let rows = match rows {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("channel send queue (direct): {e}");
                return 0;
            }
        };
        let mut n = 0;
        for (m, created, tried) in rows {
            self.defer_row(&m, now);
            let now = ch.now();
            // tried before (the Hub stopped during its send): it may be out, so it is unknown,
            // not late; [`Outbox::settle_unknown`] takes it from there
            if tried.is_some() {
                let r = ch.store().transaction(|tx| -> ChannelResult<()> {
                    crate::messages::set_status(
                        tx,
                        &m,
                        "UPDATE chan_messages SET status = 'unknown', sent = NULL WHERE id = ? AND status = 'pending'",
                        [&m],
                    )?;
                    Ok(())
                });
                if let Err(e) = r {
                    tracing::warn!(message = m, "interrupted send not marked unknown: {e}");
                }
                continue;
            }
            let notification = ch.store().reader(|c| -> ChannelResult<bool> {
                Ok(c.query_row("SELECT EXISTS(SELECT 1 FROM personal_notifications n JOIN output_deliveries d ON d.id=n.delivery_id WHERE d.message_id=?)", [&m], |r| r.get(0))?)
            }).unwrap_or(false);
            if created < now - LATE_MS && !notification {
                if let Err(e) = self.ask_late(ch, &m) {
                    tracing::warn!(message = m, "late reply not asked about: {e}");
                }
                continue;
            }
            let Ok(Some(st)) = ch.store().reader(|c| send_state::get(c, &m)) else {
                continue;
            };
            match self.send(ch, &m, &st.auth).await {
                Ok(SendOutcome::Sent { .. }) => n += 1,
                Ok(_) => {}
                Err(e) => tracing::warn!(message = m, "channel send failed: {e}"),
            }
        }
        n
    }

    /// A direct reply that did not go out in time: `failed`, with a retry / rewrite / discard card.
    fn ask_late(&self, ch: &Channels, id: &str) -> ChannelResult<()> {
        ch.store().transaction(|tx| -> ChannelResult<()> {
            let Some(row) = out_row(tx, id)? else {
                return Ok(());
            };
            if crate::messages::set_status(
                tx,
                id,
                "UPDATE chan_messages SET status = 'failed', sent = NULL WHERE id = ? AND status = 'pending'",
                [id],
            )? == 0
            {
                return Ok(());
            }
            send_state::forget(tx, id)?;
            let space = spaces::get(tx, &row.space_id)?.ok_or_else(|| ApiError::not_found("空间"))?;
            open_fail(
                tx,
                &row,
                format!("发到「{}」的回复中途被打断，没有及时发出；现在还要发吗", spaces::display_title(tx, &space)?),
                &FAIL_OPTIONS,
            )?;
            messages::event(tx, &row.ws, &row.space_id, id);
            Ok(())
        })
    }

    /// `unknown` messages: within the provider's dedupe window ([`ChannelAdapter::send_dedup`],
    /// counted from the first try) sent again as they were, so a repeat is dropped; after it, or
    /// without one, you are asked whether it went out ([`UNKNOWN_OPTIONS`]), once.
    async fn settle_unknown(&self, ch: &Channels, now: i64) -> usize {
        let rows = ch.store().reader(|c| -> ChannelResult<Vec<Unknown>> {
            let mut st = c.prepare_cached(
                "SELECT m.id, m.workspace_id, c.id, c.adapter, s.first_at FROM chan_messages m
                   JOIN spaces sp ON sp.id = m.space_id JOIN connections c ON c.id = sp.connection_id
                   LEFT JOIN chan_send_state s ON s.message_id = m.id
                  WHERE m.direction = 'out' AND m.status = 'unknown'
                    AND NOT EXISTS (SELECT 1 FROM decisions d WHERE d.subject_type = ?1
                                      AND d.subject_id = m.id AND d.status = 'open')
                  ORDER BY coalesce((SELECT n.next_attempt_at FROM personal_notifications n
                    JOIN output_deliveries od ON od.id=n.delivery_id WHERE od.message_id=m.id),0),m.created LIMIT 50",
            )?;
            let v = st
                .query_map([SUBJECT_MESSAGE], |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, String>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, Option<i64>>(4)?,
                    ))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(v
                .into_iter()
                .map(|(m, ws, conn, adapter, first)| {
                    (m, self.send_dedup(ch, c, &ws, &conn, &adapter), first)
                })
                .collect())
        });
        let rows = match rows {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("channel send queue (unknown): {e}");
                return 0;
            }
        };
        let mut n = 0;
        for (m, window, first_at) in rows {
            let window = window.map(|d| d.as_millis() as i64);
            let st = ch.store().reader(|c| send_state::get(c, &m)).ok().flatten();
            let resend = match (window, first_at, &st) {
                (Some(w), Some(t), Some(_)) => now < t + w,
                _ => false,
            };
            if !resend {
                if let Err(e) = self.ask_unknown(ch, &m) {
                    tracing::warn!(message = m, "unknown send not asked about: {e}");
                }
                continue;
            }
            let Some(st) = st else { continue };
            // the window is checked again where it is claimed
            match self.send_as(ch, &m, &st.auth, true).await {
                Ok(SendOutcome::Sent { .. }) => n += 1,
                Ok(_) => {}
                Err(e) => tracing::warn!(message = m, "channel send failed: {e}"),
            }
        }
        n
    }

    /// A message that may be out: a `fail` card with [`UNKNOWN_OPTIONS`].
    fn ask_unknown(&self, ch: &Channels, id: &str) -> ChannelResult<()> {
        ch.store().transaction(|tx| -> ChannelResult<()> {
            let Some(row) = out_row(tx, id)? else {
                return Ok(());
            };
            let space =
                spaces::get(tx, &row.space_id)?.ok_or_else(|| ApiError::not_found("空间"))?;
            open_fail(
                tx,
                &row,
                format!(
                    "发到「{}」的消息可能已经发出（没有收到对方的确认），请到那边看一下",
                    spaces::display_title(tx, &space)?
                ),
                &UNKNOWN_OPTIONS,
            )?;
            messages::event(tx, &row.ws, &row.space_id, id);
            Ok(())
        })
    }

    /// Owns the durable send loop and the independent 2s maintenance pass.
    /// Keep this task until shutdown; dropping it cancels both loops.
    pub fn spawn_queue(self: Arc<Self>, ch: &Channels) -> steve_runtime::Task {
        let source = Arc::new(QueueDue {
            channels: ch.downgrade(),
            outbox: self.clone(),
        });
        let clock = ch.modules().clock.clone();
        let weak = ch.downgrade();
        let wake = ch.store().commit_wake(&[
            "chan_messages",
            "decisions",
            "chan_send_state",
            "personal_notifications",
            "notification_policies",
            "output_deliveries",
        ]);
        let ended = weak.clone().closed();
        steve_runtime::spawn("channels.send", async move {
            struct Scope(Arc<Outbox>);
            impl Drop for Scope {
                fn drop(&mut self) {
                    self.0.stop_background();
                }
            }
            let _scope = Scope(self.clone());
            let _maintenance = steve_runtime::every(
                "channels.send.maintenance",
                clock.clone(),
                QUEUE_EVERY,
                steve_runtime::FirstRun::Immediate,
                move || {
                    let (weak, outbox) = (weak.clone(), self.clone());
                    async move {
                        if let Some(ch) = weak.upgrade() {
                            outbox.maintain(&ch).await;
                            outbox.settle_unknown(&ch, ch.now()).await;
                        }
                    }
                },
            );
            steve_runtime::due::due_loop(
                source,
                clock,
                wake,
                steve_runtime::policy::Backoff {
                    base: QUEUE_EVERY,
                    max: QUEUE_EVERY,
                    factor: 1,
                    jitter: steve_runtime::policy::Jitter::None,
                    attempts: None,
                    reset_after: None,
                },
            )
            .await;
        })
        .until(ended)
    }

    /// Back to the source: an item made from a chat message finished → “<编号> 已完成：<结果>”
    /// in the original thread, through the disclosure check, as a draft for your approval. Dropped
    /// items say nothing. Returns what became of it (`None`: not from a chat).
    pub async fn on_item_finished(
        &self,
        ch: &Channels,
        item: &Item,
    ) -> ChannelResult<Option<Delivered>> {
        if item.status == "drop" {
            return Ok(None);
        }
        let found = ch.store().reader(|c| -> ChannelResult<_> {
            let Some(msg) = source_message(c, &item.workspace_id, &item.id)? else {
                return Ok(None);
            };
            let plan = plan_of(c, &item.workspace_id, &msg)?;
            // the chat's own message was handed to Steve by a reply rule (its conversation turn says so): the
            // report goes where that answer went
            let replied: bool = c.query_row(
                "SELECT EXISTS(SELECT 1 FROM messages WHERE workspace_id = ? AND space = ? AND author = 'me'
                   AND json_extract(meta, '$.chan.message') = ? AND json_extract(meta, '$.chan.act') = ?)",
                params![
                    &plan.ws,
                    crate::conversations::conv_space(&plan.space_id),
                    &plan.message_id,
                    Act::Reply.code()
                ],
                |r| r.get(0),
            )?;
            let mine = plan.mode == Mode::Bot
                && replied
                && match spaces::get(c, &plan.space_id)? {
                    Some(s) => {
                        crate::conversations::owner_only(c, &plan.ws, &s, &plan.connection_id)?
                    }
                    None => false,
                };
            Ok(Some((plan, delivery_summary(c, item)?, mine)))
        })?;
        let Some((plan, summary, mine)) = found else {
            return Ok(None);
        };
        // handed off from your trigger word: its end says nothing, the message's reaction turns to
        // done (`react_item_ended`) — progress and failures were said on the way (trigger_report)
        if ch
            .store()
            .reader(|c| {
                crate::command_skill::trigger_plan(c, &item.workspace_id, &plan.message_id)
            })?
            .is_some()
        {
            return Ok(None);
        }
        // in the owner's private chat with the bot it is Steve reporting to them: sent like a reply there (the
        // reply rule still has to allow it), not held as a draft; anywhere else it waits for the owner
        let req = OutRequest {
            target: OutTarget::reply(&plan),
            text: done_text(&item.id, &summary),
            purpose: Purpose::Done,
            approve: !mine,
            generation: None,
            auth: if mine {
                Auth::Rule {
                    source: plan.message_id.clone(),
                    act: Act::Reply,
                }
            } else {
                Auth::None
            },
            operation_id: None,
        };
        Ok(Some(self.deliver_checked(ch, &req).await?.0))
    }

    /// `steve_reply_source`: an agent's update to the chat its item came from. On a bot account
    /// whose rules reply there it is checked and sent; otherwise (your own account, rules that don't
    /// reply, a possible leak) it waits for you as a draft. The answer is for the agent, verbatim.
    pub async fn reply_source(
        &self,
        ch: &Channels,
        ws: &str,
        item_id: &str,
        text: &str,
    ) -> ChannelResult<String> {
        let plan = ch.store().reader(|c| -> ChannelResult<_> {
            match source_message(c, ws, item_id)? {
                Some(m) => Ok(Some(plan_of(c, ws, &m)?)),
                None => Ok(None),
            }
        })?;
        let Some(plan) = plan else {
            return Ok(SRC_NO_SOURCE.into());
        };
        // progress on an answer to your trigger word: said as you, where the answer went
        let as_trigger = ch
            .store()
            .reader(|c| crate::command_skill::trigger_plan(c, ws, &plan.message_id))?;
        let (plan, req) = match as_trigger {
            Some(tp) => {
                let req = OutRequest {
                    target: crate::command_skill::target(ch, &tp)?,
                    text: text.into(),
                    // progress: the reaction stays until the item ends
                    purpose: Purpose::Source,
                    approve: false,
                    generation: None,
                    auth: Auth::Trigger {
                        source: tp.message_id.clone(),
                    },
                    operation_id: None,
                };
                (tp, Some(req))
            }
            None => (plan, None),
        };
        let req = if let Some(r) = req {
            r
        } else {
            OutRequest {
                target: OutTarget::reply(&plan),
                text: text.into(),
                purpose: Purpose::Source,
                approve: plan.mode == Mode::Own,
                generation: None,
                auth: Auth::Rule {
                    source: plan.message_id.clone(),
                    act: Act::Reply,
                },
                operation_id: None,
            }
        };
        let (d, r) = self.deliver_checked(ch, &req).await?;
        Ok(match d {
            Delivered::Sent { .. } => SRC_POSTED.into(),
            Delivered::Failed { .. } => {
                "Not posted: the chat did not accept it. The user can retry.".into()
            }
            Delivered::Blocked { .. } => {
                "Not posted: this chat does not accept messages from Steve for now (read-only). The user was told how to allow sending.".into()
            }
            Delivered::Drafted { .. } => match r.filter(|r| r.verdict != Verdict::Pass) {
                Some(r) if r.verdict == Verdict::Leak => format!(
                    "Not posted: it may reveal {}. A draft is waiting for the user.",
                    spans_note(&r)
                ),
                Some(_) => SRC_UNCHECKED.into(),
                None => SRC_DRAFTED.into(),
            },
            Delivered::Queued { .. } => {
                "Not confirmed yet: it is on its way and will be checked.".into()
            }
            Delivered::Skipped => SRC_DRAFTED.into(),
        })
    }

    /// Recalls a sent message (`POST /chan-messages/:id/recall`) through the connection it went out
    /// on (`sent_via`; the space's voice for messages sent before it was recorded): as you when it
    /// was sent as you, as the connection's account otherwise. `400 这个渠道不支持撤回` when that
    /// side can't.
    pub async fn recall(
        &self,
        ch: &Channels,
        ws: &str,
        actor: &str,
        id: &str,
    ) -> ChannelResult<()> {
        let (row, conn, adapter, mode, msg) = ch.store().reader(|c| -> ChannelResult<_> {
            let row = out_row(c, id)?
                .filter(|r| r.ws == ws)
                .ok_or_else(|| ApiError::not_found("消息"))?;
            let space =
                spaces::get(c, &row.space_id)?.ok_or_else(|| ApiError::not_found("空间"))?;
            let via: Option<String> = c.query_row(
                "SELECT sent_via FROM chan_messages WHERE id = ?",
                [id],
                |r| r.get(0),
            )?;
            let conn = via.filter(|v| !v.is_empty()).unwrap_or(space.connection_id);
            let adapter: String = c
                .query_row(
                    "SELECT adapter FROM connections WHERE id = ? AND workspace_id = ?",
                    params![conn, ws],
                    |r| r.get(0),
                )
                .optional()?
                .unwrap_or_default();
            let mode = bindings::connection_account(c, ws, &conn)
                .map(|(_, m)| m)
                .unwrap_or(Mode::Bot);
            let msg = crate::messages::message_ref(c, id, &conn)?
                .ok_or_else(|| ApiError::not_found("消息"))?;
            Ok((row, conn, adapter, mode, msg))
        })?;
        let no_recall = || ApiError::rule(steve_i18n::t!("channels.error.no_recall"));
        let not_sent = || ApiError::conflict(steve_i18n::t!("channels.error.not_sent"));
        let recaller = match (mode, self.as_user.get(&adapter)) {
            (Mode::Own, Some(u)) if u.supports_recall() => Recaller::AsUser(u.clone()),
            (Mode::Own, Some(_)) => return Err(no_recall()),
            _ => Recaller::Adapter(
                ch.adapter(&adapter)
                    .filter(|a| a.features().recall)
                    .ok_or_else(no_recall)?,
            ),
        };
        if row.status != "sent" {
            return Err(not_sent());
        }
        let cx = ch
            .connections()
            .cx_for(ws, &conn, None)
            .map_err(|e| ApiError::rule(e.to_string()))?;
        let done = match recaller {
            // without ids the adapter still has the time (`created_exact`) and the text to find it by
            Recaller::AsUser(u) => u.recall(&cx, &msg).await,
            Recaller::Adapter(a) => {
                let ext = msg.ext_id.clone().ok_or_else(not_sent)?;
                a.recall(&cx, &ext).await
            }
        };
        done.map_err(|e| {
            ApiError::rule(steve_i18n::t!("channels.error.recall_failed", error = e))
        })?;
        ch.store().transaction(|tx| -> ChannelResult<()> {
            crate::messages::set_status(
                tx,
                id,
                "UPDATE chan_messages SET status = 'recalled' WHERE id = ?",
                [id],
            )?;
            base::audit(tx, ws, actor, "chan_message.recall", id, &json!({}))?;
            messages::event(tx, ws, &row.space_id, id);
            Ok(())
        })
    }
}

/// Who recalls a message: you, or the connection's account through its channel adapter.
enum Recaller {
    AsUser(Arc<dyn ActAsUser>),
    Adapter(Arc<dyn crate::ChannelAdapter>),
}

/// An adapter's failure as the provider error the send path records.
fn provider_error(e: AdapterError) -> ProviderError {
    match e {
        AdapterError::Provider(p) => p,
        e => ProviderError::Rejected(e.to_string()),
    }
}

/// The message a reply as you answers (`ext` in space `space`), as the adapter of connection
/// `conn` acts on it; only its ids when it is not stored.
fn replied_ref(c: &Connection, space: &str, conn: &str, ext: &str) -> ChannelResult<MessageRef> {
    let id: Option<String> = c
        .query_row(
            "SELECT id FROM chan_messages WHERE space_id = ? AND external_id = ? ORDER BY direction = 'in' DESC, created LIMIT 1",
            params![space, ext],
            |r| r.get(0),
        )
        .optional()?;
    let stored = match id {
        Some(id) => crate::messages::message_ref(c, &id, conn)?,
        None => None,
    };
    match stored {
        Some(m) => Ok(m),
        None => {
            let space_ext: String = c.query_row(
                "SELECT external_id FROM spaces WHERE id = ?",
                [space],
                |r| r.get(0),
            )?;
            let mut m = MessageRef::platform(space_ext, ext);
            m.ext_id = fallback::platform(m.ext_id);
            Ok(m)
        }
    }
}

#[async_trait]
impl Outbound for Outbox {
    async fn deliver(&self, ch: &Channels, req: &OutRequest) -> ChannelResult<Delivered> {
        Ok(self.deliver_checked(ch, req).await?.0)
    }
}

/// “<编号> 已完成：<结果>”.
pub fn done_text(item: &str, summary: &str) -> String {
    if summary.trim().is_empty() {
        format!("{item} 已完成。")
    } else {
        format!("{item} 已完成：{}", summary.trim())
    }
}

/// The chat message an item was made from (`links(item, 'from', 'chan_message', …)`).
pub fn source_message(c: &Connection, ws: &str, item: &str) -> ChannelResult<Option<String>> {
    Ok(c.query_row(
        "SELECT object_id FROM links WHERE workspace_id = ? AND subject_type = 'item' AND subject_id = ?
            AND predicate = 'from' AND object_type = 'chan_message' ORDER BY created LIMIT 1",
        params![ws, item],
        |r| r.get(0),
    )
    .optional()?)
}

/// One sentence of what an item delivered: the summary of its last delivery (the `accept` decision's
/// draft without the checks), else its “现在”, else its title.
pub fn delivery_summary(c: &Connection, item: &Item) -> ChannelResult<String> {
    const MAX: usize = 120;
    let draft: Option<String> = c
        .query_row(
            "SELECT draft FROM decisions WHERE item_id = ? AND kind = 'accept' AND draft IS NOT NULL
              ORDER BY created DESC LIMIT 1",
            [&item.id],
            |r| r.get(0),
        )
        .optional()?
        .flatten();
    let s = draft
        .map(|d| {
            d.split("\n\n检查：")
                .next()
                .unwrap_or("")
                .trim()
                .to_string()
        })
        .filter(|s| !s.is_empty())
        .or_else(|| Some(item.now.trim().to_string()).filter(|s| !s.is_empty()))
        .unwrap_or_else(|| item.title.trim().to_string());
    let flat = s.split_whitespace().collect::<Vec<_>>().join(" ");
    Ok(match flat.char_indices().nth(MAX) {
        Some((i, _)) => format!("{}…", &flat[..i]),
        None => flat,
    })
}

// ---------------------------------------------------------------- item hook

/// The B1 item hook for return-to-source: `registry.item_hooks(Box::new(ReturnToSource::new(…)))`.
/// The message is checked and drafted in a spawned task (hooks must not block the lifecycle).
pub struct ReturnToSource {
    outbox: Arc<Outbox>,
    late: Arc<LateChannels>,
}

impl ReturnToSource {
    pub fn new(outbox: Arc<Outbox>, late: Arc<LateChannels>) -> Self {
        Self { outbox, late }
    }
}

#[async_trait]
impl ItemHooks for ReturnToSource {
    fn name(&self) -> &'static str {
        "channels"
    }

    async fn on_finished(&self, item: &Item) {
        let Some(ch) = self.late.get() else {
            return;
        };
        let outbox = self.outbox.clone();
        let item = item.clone();
        tokio::spawn(async move {
            if let Err(e) = outbox.on_item_finished(&ch, &item).await {
                tracing::warn!(item = %item.id, "back to the chat failed: {e}");
            }
            // the item ended: the message it came from is answered
            ch.react_item_ended(&item.workspace_id, &item.id, true)
                .await;
        });
    }

    async fn on_dropped(&self, item: &Item) {
        let Some(ch) = self.late.get() else {
            return;
        };
        let item = item.clone();
        tokio::spawn(async move {
            ch.react_item_ended(&item.workspace_id, &item.id, false)
                .await;
        });
    }
}

// ---------------------------------------------------------------- not said in your name any more

/// Drops a message in your name that is not to be said any more ([`crate::presence::stop_out`]):
/// discarded from `from`, its open card (and `decision` when given, the one being answered)
/// superseded, audit `chan_message.superseded`.
fn drop_for_you(
    tx: &Tx,
    row: &OutRow,
    from: &[&str],
    decision: Option<&str>,
    stop: &crate::presence::Stop,
) -> ChannelResult<()> {
    set_status(tx, &row.id, from, DISCARDED)?;
    send_state::forget(tx, &row.id)?;
    if let Some(d) = decision {
        crate::presence::settle(tx, d, stop.why())?;
    }
    if let Some(d) = base::decisions::open_for(tx, &row.ws, SUBJECT_MESSAGE, &row.id)? {
        crate::presence::settle(tx, &d.id, stop.why())?;
    }
    base::audit(
        tx,
        &row.ws,
        "steve",
        "chan_message.superseded",
        &row.id,
        &json!({"why": stop.why(), "reason": stop.reason()}),
    )?;
    messages::event(tx, &row.ws, &row.space_id, &row.id);
    Ok(())
}

fn fail_orphaned_message(tx: &Tx, row: &OutRow, now: i64) -> ChannelResult<()> {
    crate::messages::set_status(
        tx,
        &row.id,
        "UPDATE chan_messages SET status = 'failed', sent = NULL WHERE id = ?",
        [&row.id],
    )?;
    if let Some(d) = base::decisions::open_for(tx, &row.ws, SUBJECT_MESSAGE, &row.id)? {
        tx.execute(
            "UPDATE decisions SET status = 'resolved', result = ?, resolved = ? WHERE id = ?",
            params![
                json!({"dismissed": true, "why": "connection_gone"}).to_string(),
                now,
                d.id
            ],
        )?;
    }
    base::audit(
        tx,
        &row.ws,
        "steve",
        "chan_message.send.orphaned",
        &row.id,
        &json!({"space": row.space_id}),
    )?;
    messages::event(tx, &row.ws, &row.space_id, &row.id);
    Ok(())
}

fn approve_message_text(tx: &Tx, message: &str, decision: &str, text: &str) -> ChannelResult<()> {
    crate::messages::set_status(
        tx,
        message,
        "UPDATE chan_messages SET text = ?, status = 'pending', sent = NULL WHERE id = ? AND status = 'draft'",
        params![text, message],
    )?;
    // Undo reopens this card: its formal draft must match the edited outbound body.
    tx.execute(
        "UPDATE decisions SET draft = ? WHERE id = ?",
        params![text, decision],
    )?;
    // approved before its check finished: the record says so
    basis::checking_stopped(tx, message, R_DEALT_FIRST)?;
    Ok(())
}

/// The authorization is your own approval (a decision a person answered, not YOLO): what you
/// approved goes out even after you spoke, it is yours.
fn approved_by_you(c: &Connection, auth: &Auth) -> ChannelResult<bool> {
    let Auth::Decision { id } = auth else {
        return Ok(false);
    };
    Ok(base::decisions::get(c, id)?.is_some_and(|d| {
        d.status == "resolved" && d.result["decided_by"].as_str() != Some(b1::YOLO_ACTOR)
    }))
}

// ---------------------------------------------------------------- decisions

/// The `chan_message` decision handler: a draft's approval (`yes` sends the possibly edited text
/// after the delay, `no` discards), a failed send (`retry` / `rewrite` / `discard`), a handoff (`draft_reply`
/// opens an empty draft to write, `to_item` makes an item, `done`).
pub struct MessageDecisions(OutLink);

impl MessageDecisions {
    fn approve(&self, tx: &Tx, cx: &DecisionCx, d: &Decision) -> ApiResult<Value> {
        let now = self.0.now();
        let row = own_out(tx, cx.ws, &d.subject_id)?;
        if row.status != "draft" || row.decision_id.as_deref() != Some(d.id.as_str()) {
            return Err(ApiError::conflict(steve_i18n::t!("channels.error.handled")));
        }
        let mut result = choice_result(cx);
        // YOLO never approves what you already answered yourself, what a later turn took, or what
        // the space no longer lets Steve say for you (crate::presence, gate 3)
        if cx.choice == "yes"
            && cx.actor == b1::YOLO_ACTOR
            && let Some(e) = crate::presence::stop_out(tx, cx.ws, &row.id, now)?
        {
            drop_for_you(tx, &row, &["draft"], Some(&d.id), &e)?;
            self.0.push(After::Discarded {
                message_id: row.id.clone(),
            });
            result["superseded"] = json!(e.why());
            return Ok(result);
        }
        if d.result["recovery_review"] == true {
            result["recovery_review"] = json!(true);
        }
        // YOLO never approves a draft whose disclosure check did not pass (flagged, still running or
        // not finished): only you do
        if cx.choice == "yes" && cx.actor == b1::YOLO_ACTOR {
            let verdict: Option<String> = tx
                .query_row(
                    "SELECT b.verdict FROM chan_messages m JOIN reply_basis b ON b.id = m.basis_id WHERE m.id = ?",
                    [&row.id],
                    |r| r.get(0),
                )
                .optional()?;
            if !matches!(verdict.as_deref(), Some(basis::PASS | basis::REWRITTEN)) {
                return Err(ApiError::conflict(steve_i18n::t!(
                    "channels.error.not_checked"
                )));
            }
        }
        match cx.choice {
            "yes" => {
                if d.result["recovery_review"] == true && cx.actor == b1::YOLO_ACTOR {
                    return Err(ApiError::rule("恢复后的修改稿需要你明确审阅批准"));
                }
                let edited = !cx.text.trim().is_empty();
                let text = if edited {
                    // your edit on the card is what you approve
                    if cx.text.chars().count() > TEXT_MAX {
                        return Err(ApiError::invalid("text"));
                    }
                    cx.text.trim().to_string()
                } else {
                    row.text.clone()
                };
                if text.trim().is_empty() {
                    return Err(ApiError::rule(steve_i18n::t!("channels.error.reply_empty")));
                }
                // YOLO waited out its own cooling-off before it approved: no second wait
                let send_at = if cx.actor == b1::YOLO_ACTOR {
                    now
                } else {
                    now + send_delay_ms(tx, &row)?
                };
                approve_message_text(tx, &row.id, &d.id, &text)?;
                base::audit(
                    tx,
                    cx.ws,
                    cx.actor,
                    "chan_message.approve",
                    &row.id,
                    &json!({"decision": d.id, "edited": edited}),
                )?;
                result["hash"] = json!(text_hash(&text));
                result["send_at"] = json!(send_at);
                if cx.actor == b1::YOLO_ACTOR {
                    result["decided_by"] = json!(b1::YOLO_ACTOR);
                }
            }
            REWRITE if d.options.iter().any(|o| o == REWRITE) => {
                // the card closes; the queue finds the choice and the rewritten text comes back on a new one
                base::audit(
                    tx,
                    cx.ws,
                    cx.actor,
                    "chan_message.rewrite",
                    &row.id,
                    &json!({"decision": d.id}),
                )?;
            }
            "no" => {
                set_status(tx, &row.id, &["draft"], DISCARDED)?;
                self.0.push(After::Discarded {
                    message_id: row.id.clone(),
                });
                base::audit(
                    tx,
                    cx.ws,
                    cx.actor,
                    "chan_message.reject",
                    &row.id,
                    &json!({"decision": d.id}),
                )?;
            }
            "to_item" if d.options.iter().any(|o| o == "to_item") => {
                let source =
                    draft_source(tx, &row)?.ok_or_else(|| ApiError::not_found("原消息"))?;
                plan_of(tx, cx.ws, &source)?;
                set_status(tx, &row.id, &["draft"], DISCARDED)?;
                result["source_message"] = json!(source);
                self.0.push(After::Discarded {
                    message_id: row.id.clone(),
                });
                base::audit(
                    tx,
                    cx.ws,
                    cx.actor,
                    "chan_message.to_item",
                    &row.id,
                    &json!({"decision": d.id, "source": source}),
                )?;
            }
            _ => return Err(ApiError::invalid("choice")),
        }
        messages::event(tx, cx.ws, &row.space_id, &row.id);
        Ok(result)
    }

    fn failed(&self, tx: &Tx, cx: &DecisionCx, d: &Decision) -> ApiResult<Value> {
        let now = self.0.now();
        let row = own_out(tx, cx.ws, &d.subject_id)?;
        if row.decision_id.as_deref() != Some(d.id.as_str()) {
            return Err(ApiError::conflict(steve_i18n::t!("channels.error.handled")));
        }
        let mut result = choice_result(cx);
        // nor retries it (gate 3)
        if cx.choice == "retry"
            && cx.actor == b1::YOLO_ACTOR
            && let Some(e) = crate::presence::stop_out(tx, cx.ws, &row.id, now)?
        {
            drop_for_you(tx, &row, &["failed", "unknown"], Some(&d.id), &e)?;
            result["superseded"] = json!(e.why());
            return Ok(result);
        }
        result["was"] = json!(row.status);
        match (cx.choice, row.status.as_str()) {
            ("retry", "failed") => {
                if !cx.text.trim().is_empty() {
                    return Err(ApiError::invalid("text: use rewrite to edit and review"));
                }
                if d.result["recovery"]["requires_user_action"] == true
                    && cx.actor == b1::YOLO_ACTOR
                {
                    return Err(ApiError::rule("渠道拒绝需要你处理权限后主动重试"));
                }
                crate::messages::set_status(
                    tx,
                    &row.id,
                    "UPDATE chan_messages SET status = 'pending', sent = NULL WHERE id = ? AND status = 'failed'",
                    [&row.id],
                )?;
                result["hash"] = json!(text_hash(&row.text));
                result["send_at"] = json!(now);
                if cx.actor == b1::YOLO_ACTOR {
                    result["decided_by"] = json!(b1::YOLO_ACTOR);
                }
            }
            (REWRITE, "failed") if d.options.iter().any(|o| o == REWRITE) => {
                if cx.text.chars().count() > TEXT_MAX {
                    return Err(ApiError::invalid("text"));
                }
                set_status(tx, &row.id, &["failed"], "draft")?;
                result["rewrite_mode"] = json!(if cx.text.trim().is_empty() {
                    "model"
                } else {
                    "manual"
                });
            }
            ("discard", "failed") => {
                set_status(tx, &row.id, &["failed"], DISCARDED)?;
                send_state::forget(tx, &row.id)?;
                self.0.push(After::Discarded {
                    message_id: row.id.clone(),
                });
            }
            // may be out (no answer): you looked
            ("sent", "unknown") => {
                set_status(tx, &row.id, &["unknown"], "sent")?;
                send_state::forget(tx, &row.id)?;
            }
            ("retry", "unknown") => {
                if !cx.text.trim().is_empty() {
                    return Err(ApiError::invalid(
                        "text: an uncertain send cannot be edited",
                    ));
                }
                crate::messages::set_status(
                    tx,
                    &row.id,
                    "UPDATE chan_messages SET status = 'pending', sent = NULL WHERE id = ? AND status = 'unknown'",
                    [&row.id],
                )?;
                result["hash"] = json!(text_hash(&row.text));
                result["send_at"] = json!(now);
            }
            ("discard", "unknown") => {
                set_status(tx, &row.id, &["unknown"], DISCARDED)?;
                // Keep the uncertainty evidence while discarded. An explicit undo must not turn
                // an unanswered send into a definitely-unsent failure with a fresh retry window.
            }
            _ => return Err(ApiError::conflict(steve_i18n::t!("channels.error.handled"))),
        }
        if let Some(recovery) = d.result.get("recovery") {
            result["recovery"] = recovery.clone();
        }
        base::audit(
            tx,
            cx.ws,
            cx.actor,
            &format!("chan_message.{}", cx.choice),
            &row.id,
            &json!({"decision": d.id}),
        )?;
        messages::event(tx, cx.ws, &row.space_id, &row.id);
        Ok(result)
    }

    fn handoff(&self, tx: &Tx, cx: &DecisionCx, d: &Decision) -> ApiResult<Value> {
        let now = self.0.now();
        let mut result = choice_result(cx);
        // what speaks for you from a card is your own click: YOLO answers none of these
        if cx.actor == b1::YOLO_ACTOR
            && [respond::SEND_DRAFT, respond::VERBATIM, respond::INSTRUCT].contains(&cx.choice)
        {
            return Err(ApiError::rule(steve_i18n::t!(
                "channels.error.yours_to_answer"
            )));
        }
        match cx.choice {
            respond::SEND_DRAFT => {
                // the card's draft is a model's text: the queue checks it for disclosure first and
                // sends it after the usual delay when it passes, else opens the approval with the
                // suspicious parts marked (send_chosen)
                let text = d.draft.trim();
                if text.is_empty() {
                    return Err(ApiError::rule(steve_i18n::t!("channels.error.reply_empty")));
                }
                if text.chars().count() > TEXT_MAX {
                    return Err(ApiError::invalid("text"));
                }
                plan_of(tx, cx.ws, &d.subject_id)?;
                result["by"] = json!(cx.actor);
            }
            respond::VERBATIM => {
                // your own words as you wrote them: approved by your click, after the usual delay
                let text = cx.text.trim();
                if text.is_empty() {
                    return Err(ApiError::rule(steve_i18n::t!("channels.error.reply_empty")));
                }
                if text.chars().count() > TEXT_MAX {
                    return Err(ApiError::invalid("text"));
                }
                let plan = plan_of(tx, cx.ws, &d.subject_id)?;
                let (m, dd, send_at) = self.approve_as_you(tx, cx, &plan, text, &d.id, now)?;
                result["draft_message"] = json!(m);
                result["draft_decision"] = json!(dd);
                result["send_at"] = json!(send_at);
            }
            respond::INSTRUCT => {
                // your words go to the composer: the queue drafts with them (Outbox::send_due)
                if cx.text.trim().is_empty() {
                    return Err(ApiError::rule(steve_i18n::t!("channels.error.reply_empty")));
                }
                plan_of(tx, cx.ws, &d.subject_id)?;
            }
            respond::DRAFT_REPLY => {
                let plan = plan_of(tx, cx.ws, &d.subject_id)?;
                let (m, dd) = respond::open_draft(tx, &plan, cx.text.trim(), now)?;
                result["draft_message"] = json!(m);
                result["draft_decision"] = json!(dd);
            }
            "to_item" => {
                plan_of(tx, cx.ws, &d.subject_id)?;
                result["source_message"] = json!(d.subject_id);
            }
            "done" => {}
            _ => return Err(ApiError::invalid("choice")),
        }
        Ok(result)
    }
}

impl MessageDecisions {
    /// Your click on a card that sends: `text` goes out as a reply to the plan's message, approved by
    /// you (a draft and its approval resolved `yes` at once, after the usual delay, undoable within
    /// it). Returns `(message, its approval, send_at)`.
    fn approve_as_you(
        &self,
        tx: &Tx,
        cx: &DecisionCx,
        plan: &respond::Plan,
        text: &str,
        card: &str,
        now: i64,
    ) -> ApiResult<(String, String, i64)> {
        let (m, dd) = respond::open_draft(tx, plan, text, now)?;
        crate::reactions::expect_answer(tx, &plan.message_id, &m)?;
        let send_at = approve_draft(tx, cx.ws, &m, &dd, text, card, now)?;
        base::audit(
            tx,
            cx.ws,
            cx.actor,
            "chan_message.approve",
            &m,
            &json!({"decision": dd, "card": card, "choice": cx.choice}),
        )?;
        messages::event(tx, cx.ws, &plan.space_id, &m);
        Ok((m, dd, send_at))
    }
}

impl DecisionHandler for MessageDecisions {
    fn reopen_result(&self, d: &Decision) -> Value {
        // Undo revokes the answer/hash/send_at. Keep only the non-authorizing review/refusal
        // facts: clearing these would let YOLO immediately approve or retry the reopened card.
        let mut result = json!({});
        if d.result["recovery_review"] == true {
            result["recovery_review"] = json!(true);
        }
        if let Some(recovery) = d.result.get("recovery") {
            result["recovery"] = recovery.clone();
        }
        if result == json!({}) {
            Value::Null
        } else {
            result
        }
    }

    fn resolve(&self, tx: &Tx, cx: &DecisionCx, d: &Decision) -> ApiResult<Value> {
        match d.kind.as_str() {
            "approve" => self.approve(tx, cx, d),
            "fail" => self.failed(tx, cx, d),
            "ask" => self.handoff(tx, cx, d),
            _ => Err(ApiError::rule(steve_i18n::t!(
                "channels.error.unknown_decision"
            ))),
        }
    }

    /// An approval nobody answered expired: its draft is not sent and counts as discarded, so the
    /// message it answers loses its “收到” reaction instead of keeping it for good.
    fn expire(&self, tx: &Tx, d: &Decision) -> ApiResult<()> {
        if d.kind != "approve" {
            return Ok(());
        }
        let Some(row) = out_row(tx, &d.subject_id)? else {
            return Ok(());
        };
        if row.status == "draft" && row.decision_id.as_deref() == Some(d.id.as_str()) {
            set_status(tx, &row.id, &["draft"], DISCARDED)?;
            self.0.push(After::Discarded {
                message_id: row.id.clone(),
            });
            base::audit(
                tx,
                &row.ws,
                "steve",
                "chan_message.expired",
                &row.id,
                &json!({"decision": d.id}),
            )?;
            messages::event(tx, &row.ws, &row.space_id, &row.id);
        }
        Ok(())
    }

    /// Undoing an approval within the delay puts the draft back; once it went out, `409`.
    fn before_undo(&self, tx: &Tx, d: &Decision) -> ApiResult<()> {
        let now = self.0.now();
        if d.result["choice"] == "to_item" {
            return Err(ApiError::conflict("转事项不能作为发送批准撤销"));
        }
        if d.kind == "ask" {
            return Ok(());
        }
        let Some(row) = out_row(tx, &d.subject_id)? else {
            return Ok(());
        };
        if row.decision_id.as_deref() != Some(&d.id) {
            return Err(ApiError::conflict(steve_i18n::t!("channels.error.handled")));
        }
        let choice = d.result["choice"].as_str().unwrap_or("");
        let in_window = row.status == "pending"
            && row.sent.is_none()
            && d.result["send_at"].as_i64().is_some_and(|t| now < t);
        match (d.kind.as_str(), choice) {
            ("approve", "yes") if in_window => {
                set_status(tx, &row.id, &["pending"], "draft")?;
            }
            // the rewrite has not replaced this card yet: it stays open as it was
            ("approve", REWRITE)
                if row.status == "draft" && row.decision_id.as_deref() == Some(d.id.as_str()) => {}
            ("fail", REWRITE) if row.status == "draft" => {
                set_status(tx, &row.id, &["draft"], "failed")?;
            }
            ("approve", "no") if row.status == DISCARDED => {
                set_status(tx, &row.id, &[DISCARDED], "draft")?;
            }
            ("fail", "retry") if row.status == "pending" && row.sent.is_none() => {
                let was = if d.result["was"] == "unknown" || d.options.iter().any(|o| o == "sent") {
                    "unknown"
                } else {
                    "failed"
                };
                set_status(tx, &row.id, &["pending"], was)?;
            }
            ("fail", "discard") if row.status == DISCARDED => {
                let uncertain =
                    d.result["was"] == "unknown" || d.options.iter().any(|o| o == "sent");
                if uncertain && send_state::get(tx, &row.id)?.is_none_or(|s| s.first_at.is_none()) {
                    return Err(ApiError::conflict(
                        "原发送结果未知且去重依据已不可用，不能撤销后重新发送",
                    ));
                }
                set_status(
                    tx,
                    &row.id,
                    &[DISCARDED],
                    if uncertain { "unknown" } else { "failed" },
                )?;
            }
            _ => {
                return Err(ApiError::conflict(steve_i18n::t!(
                    "channels.error.already_sent"
                )));
            }
        }
        base::audit(
            tx,
            &row.ws,
            "me",
            "chan_message.undo",
            &row.id,
            &json!({"decision": d.id}),
        )?;
        messages::event(tx, &row.ws, &row.space_id, &row.id);
        Ok(())
    }
}

// ---------------------------------------------------------------- reactions

/// The outbox reacts like it sends: as you on your own account (`/bot`: the reply is yours, so are
/// its “收到 → 完成” marks), as the bot through its channel adapter otherwise.
#[async_trait]
impl crate::reactions::Reactor for Outbox {
    async fn add(
        &self,
        ch: &Channels,
        ws: &str,
        conn: &str,
        msg: &MessageRef,
        kind: crate::channel::ReactionKind,
    ) -> Result<crate::reactions::Added, String> {
        let adapter = crate::reactions::adapter_of(ch, ws, conn)?;
        match self.as_user.get(&adapter) {
            Some(u) => {
                let cx = ch
                    .connections()
                    .cx_for(ws, conn, None)
                    .map_err(|e| e.to_string())?;
                let id = u
                    .add_reaction(&cx, msg, kind)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(crate::reactions::Added {
                    reaction_id: id,
                    emoji: u.reaction_emoji(kind).into(),
                    as_user: true,
                })
            }
            None => {
                crate::reactions::AdapterReactor
                    .add(ch, ws, conn, msg, kind)
                    .await
            }
        }
    }

    async fn remove(
        &self,
        ch: &Channels,
        ws: &str,
        conn: &str,
        msg: &MessageRef,
        reaction_id: &str,
        as_user: bool,
    ) -> Result<(), String> {
        let adapter = crate::reactions::adapter_of(ch, ws, conn)?;
        match (as_user, self.as_user.get(&adapter)) {
            (true, Some(u)) => {
                let cx = ch
                    .connections()
                    .cx_for(ws, conn, None)
                    .map_err(|e| e.to_string())?;
                u.remove_reaction(&cx, msg, reaction_id)
                    .await
                    .map_err(|e| e.to_string())
            }
            _ => {
                crate::reactions::AdapterReactor
                    .remove(ch, ws, conn, msg, reaction_id, as_user)
                    .await
            }
        }
    }
}

#[cfg(test)]
mod permission_tests {
    use super::missing_permission;

    #[test]
    fn a_missing_permission_is_not_retried() {
        assert!(missing_permission(
            "飞书应用没有开通这个权限（99991679）：im:message.send_as_user"
        ));
        assert!(missing_permission("飞书拒绝了请求（99991672）"));
        assert!(missing_permission(
            "你本人的授权里没有“以你本人身份发送”（im:message.send_as_user），请重新授权"
        ));
        assert!(!missing_permission("连接超时"));
        assert!(!missing_permission("飞书限流（99991400）"));
    }
}

// One candidate relation drives discovery and both batch consumers. Strict claim boundaries
// become inclusive deadlines (+1ms); notification deferrals are filters before LIMIT, not ordering hints.
const QUEUE_CANDIDATES: &str = "WITH candidates AS (
    SELECT m.id, d.id AS decision, m.created, NULL AS first_at,
      max(json_extract(d.result,'$.send_at'),coalesce(m.sent+?1+1,0),
        coalesce((SELECT n.next_attempt_at FROM personal_notifications n
          JOIN output_deliveries od ON od.id=n.delivery_id WHERE od.message_id=m.id),0),
        coalesce((SELECT value FROM json_each(?4) WHERE key=m.id),0)) AS at
    FROM chan_messages m JOIN decisions d ON d.id=m.decision_id
    WHERE m.direction='out' AND m.status='pending' AND d.status='resolved'
      AND d.subject_type=?2 AND d.subject_id=m.id AND json_extract(d.result,'$.send_at') IS NOT NULL
    UNION ALL
    SELECT m.id, NULL, m.created, s.first_at,
      max(CASE WHEN m.sent IS NULL THEN m.created+?3+1 ELSE m.sent+?1+1 END,
        CASE WHEN s.first_at IS NOT NULL THEN 0 ELSE coalesce((
          SELECT n.next_attempt_at FROM personal_notifications n JOIN output_deliveries od ON od.id=n.delivery_id
          JOIN notification_policies p ON p.workspace_id=n.workspace_id AND p.user_id=n.user_id
          WHERE od.message_id=m.id AND p.enabled=1),0) END,
        coalesce((SELECT value FROM json_each(?4) WHERE key=m.id),0))
    FROM chan_messages m JOIN chan_send_state s ON s.message_id=m.id
    WHERE m.direction='out' AND m.status='pending' AND m.decision_id IS NULL
      AND (s.first_at IS NOT NULL OR NOT EXISTS (
        SELECT 1 FROM personal_notifications n JOIN output_deliveries od ON od.id=n.delivery_id
        JOIN notification_policies p ON p.workspace_id=n.workspace_id AND p.user_id=n.user_id
        WHERE od.message_id=m.id AND p.enabled=1 AND (p.mode='log'
          OR (p.mode='digest' AND (n.topic<>'contact.starred' OR p.space_id IS NULL)))))
) ";

/// Approved sends and abandoned direct claims: the earliest executable durable deadline.
/// A refused send already enters the existing failed/manual-review state; no migration is needed.
pub struct QueueDue {
    pub channels: crate::WeakChannels,
    pub outbox: Arc<Outbox>,
}
impl steve_runtime::due::DueSource for QueueDue {
    fn name(&self) -> &'static str {
        "channels.send"
    }
    async fn next_due(&self) -> Result<Option<i64>, steve_runtime::due::DueError> {
        let Some(channels) = self.channels.upgrade() else {
            return Ok(None);
        };
        channels
            .store()
            .reader(|c| -> ChannelResult<_> {
                Ok(c.query_row(
                    &format!("{QUEUE_CANDIDATES} SELECT min(at) FROM candidates"),
                    params![
                        CLAIM_STALE_MS,
                        SUBJECT_MESSAGE,
                        UNCLAIMED_MS,
                        self.outbox.retry_deadlines(channels.now())
                    ],
                    |r| r.get(0),
                )?)
            })
            .map_err(|e| steve_runtime::due::DueError(e.to_string()))
    }
    async fn run_due(&self, now: i64) -> Result<(), steve_runtime::due::DueError> {
        let Some(channels) = self.channels.upgrade() else {
            return Ok(());
        };
        self.outbox.bind(&channels);
        self.outbox.send_ready(&channels, now).await;
        Ok(())
    }
}
