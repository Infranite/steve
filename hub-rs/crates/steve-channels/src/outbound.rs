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
//!    (retry / discard), the basis record and the audit.
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
//! outbox.clone().spawn_queue(&channels);
//! ```

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use steve_flow::AdapterError;
use steve_modules::accounts::connections::{ProviderError, WriteRefusal, may_write_in};
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
use crate::{ChannelResult, Channels, bindings, compose, fallback, messages, send_state};

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
pub const FAIL_OPTIONS: [&str; 2] = ["retry", "discard"];
/// Options of the decision after a blocked one.
pub const BLOCKED_OPTIONS: [&str; 1] = ["discard"];
/// The choice “改写后再看”: the drafting model rewrites the flagged parts, the result is checked again
/// and comes back as a new draft card.
pub const REWRITE: &str = "rewrite";
/// Options of the approval of a model's draft the check flagged.
pub const REWRITE_OPTIONS: [&str; 3] = ["yes", "no", REWRITE];

/// A provider refusal for a missing permission (Feishu 99991672 tenant / 99991679 user scope, or
/// the grant check before the call): retrying can't help.
pub fn missing_permission(error: &str) -> bool {
    ["99991672", "99991679", "没有开通这个权限", "授权里没有"]
        .iter()
        .any(|m| error.contains(m))
}
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
    /// A handoff answered “to_item”.
    ToItem { ws: String, message_id: String },
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
    Ok(tx.execute(
        &format!(
            "UPDATE chan_messages SET status = ?, sent = NULL WHERE id = ? AND status IN ({list})"
        ),
        p.as_slice(),
    )? > 0)
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
    t: &'a OutTarget,
    p: &'a Place,
    text: &'a str,
    status: &'a str,
    proxy: bool,
}

fn insert_out(tx: &Tx, n: &NewOut, now: i64) -> ChannelResult<String> {
    let id = uid("cm");
    let on_behalf = if n.proxy {
        "proxy"
    } else if n.p.persona.is_some() {
        "persona"
    } else {
        "none"
    };
    tx.execute(
        "INSERT INTO chan_messages (id, workspace_id, space_id, thread_key, reply_to, direction, sender_account_id,
            persona_id, on_behalf, text, status, created)
         VALUES (?, ?, ?, ?, ?, 'out', ?, ?, ?, ?, ?, ?)",
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
    Ok(id)
}

fn spans_note(r: &CheckResult) -> String {
    let parts: Vec<String> = r
        .spans
        .iter()
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

/// The `approve` decision of a draft. Returns its id.
fn open_approval(
    tx: &Tx,
    ws: &str,
    id: &str,
    question: String,
    text: &str,
    rewrite: bool,
) -> ChannelResult<String> {
    let options = if rewrite {
        &REWRITE_OPTIONS[..]
    } else {
        &DRAFT_OPTIONS[..]
    };
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
            options: Some(options.iter().map(|s| s.to_string()).collect()),
            requested_by: "steve".into(),
        },
    )?;
    tx.execute(
        "UPDATE chan_messages SET decision_id = ? WHERE id = ?",
        params![d.id, id],
    )?;
    Ok(d.id)
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
            draft: (options.len() > 1).then(|| row.text.clone()),
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

fn space_name(s: &SpaceRow) -> String {
    respond::space_label(s)
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

/// “改写后再看”: the flagged draft `message_id` is rewritten once by the model that wrote it, checked
/// again and put back as a draft with a new card (still flagged: it says so, and offers another
/// rewrite). Nothing is sent. A failed rewrite keeps the text and says so on the card. Only while the
/// draft still waits on the card `decision_id` the choice closed (an undo reopened it: nothing). Runs on
/// a task of its own ([`Outbox::send_due`] finds the choice in the store, so a restart does not lose it).
async fn rewrite_draft(
    ch: &Channels,
    pinned: Option<&Arc<dyn PinnedModel>>,
    message_id: &str,
    decision_id: &str,
) -> ChannelResult<()> {
    type Loaded = (
        OutRow,
        Place,
        Audience,
        Vec<KnowledgeUse>,
        Option<String>,
        basis::BasisOut,
        bool,
    );
    let loaded = ch.store().reader(|c| -> ChannelResult<Option<Loaded>> {
        let Some(row) = out_row(c, message_id)? else {
            return Ok(None);
        };
        let d = base::decisions::get(c, decision_id)?;
        let chosen = d.is_some_and(|d| {
            d.status == "resolved" && d.result["choice"].as_str() == Some(REWRITE)
        });
        if row.status != "draft" || row.decision_id.as_deref() != Some(decision_id) || !chosen {
            return Ok(None);
        }
        let Some(b) = basis::of_message(c, &row.ws, &row.id)? else {
            return Ok(None);
        };
        let space = spaces::get(c, &row.space_id)?.ok_or_else(|| ApiError::not_found("空间"))?;
        let p = place(
            c,
            &row.ws,
            &space.connection_id,
            &row.space_id,
            row.persona_id.as_deref(),
        )?;
        let a = disclosure::space_audience(c, &p.space, &p.account, &p.owner_user)?;
        // the same knowledge the first check had (retrieved pieces and memories), read again
        let entries = b.entries.as_array().cloned().unwrap_or_default();
        let (k, complete) = steve_knowledge::retrieve::knowledge_again(c, &row.ws, &entries)?;
        let said = match &row.reply_to {
            Some(src) => c
                .query_row(
                    "SELECT text FROM chan_messages WHERE external_id = ? AND space_id = ? AND direction = 'in'
                        AND purged IS NULL AND text IS NOT NULL AND trim(text) <> ''",
                    params![src, row.space_id],
                    |r| r.get::<_, String>(0),
                )
                .optional()?,
            None => None,
        };
        Ok(Some((row, p, a, k, said, b, complete)))
    })?;
    let Some((row, p, audience, knowledge, said, b, complete)) = loaded else {
        return Ok(());
    };
    let spans: Vec<steve_modules::outbound::Span> =
        serde_json::from_value(b.spans.clone()).unwrap_or_default();
    let flagged = CheckResult {
        verdict: Verdict::Leak,
        entries: Vec::new(),
        spans,
        check_model: b.check_model.clone(),
    };
    let model = b.answer_model.clone().unwrap_or_default();
    let new = rewrite_text(ch, pinned, &audience, &row.text, &flagged, &model).await;
    let cx = CheckCx {
        ws: row.ws.clone(),
        actor: match (&p.persona, p.mode) {
            (Some(per), _) => per.bot_id.clone(),
            (None, Mode::Own) => "me".into(),
            (None, Mode::Bot) => "steve".into(),
        },
        draft_model: b.answer_model.clone(),
        audience_said: said,
        skip_model: false,
        // the model may be left out only when the knowledge is all here, as in the first check
        internal_skip: complete && p.mode == Mode::Bot && row.on_behalf != "proxy",
    };
    let checked = match &new {
        Some(t) => {
            let mut r = b1::check(ch.modules(), &cx, &audience, t, &knowledge).await;
            // checked against less than it was written with: it does not pass on that
            if !complete && r.verdict == Verdict::Pass {
                r.verdict = Verdict::Leak;
                r.spans.push(steve_modules::outbound::Span {
                    text: String::new(),
                    reason: steve_modules::outbound::R_BASIS_GONE.into(),
                });
            }
            Some(r)
        }
        None => None,
    };
    let now = ch.now();
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
        let mut q = format!("{}发到「{}」", p.speaker, space_name(&p.space));
        let text = match (&new, &checked) {
            (Some(t), Some(r)) => {
                let leak = r.verdict != Verdict::Pass;
                let mut rewrites: Vec<Rewrite> =
                    serde_json::from_value(b.rewrites.clone()).unwrap_or_default();
                rewrites.push(Rewrite {
                    from: row.text.clone(),
                    to: t.clone(),
                    reason: flagged
                        .spans
                        .iter()
                        .map(|s| s.reason.clone())
                        .collect::<Vec<_>>()
                        .join("；"),
                });
                basis::insert(
                    tx,
                    &NewBasis {
                        ws: row.ws.clone(),
                        message_id: row.id.clone(),
                        entries: r.entries.clone(),
                        rewrites,
                        verdict: if leak {
                            basis::DRAFTED
                        } else {
                            basis::REWRITTEN
                        }
                        .into(),
                        answer_model: b.answer_model.clone(),
                        check_model: r.check_model.clone(),
                        spans: if leak { r.spans.clone() } else { Vec::new() },
                    },
                    now,
                )?;
                q.push_str("（已改写");
                if leak {
                    q.push_str(&format!("，仍可能泄露：{}", spans_note(r)));
                }
                q.push('）');
                tx.execute(
                    "UPDATE chan_messages SET text = ? WHERE id = ? AND status = 'draft'",
                    params![t, row.id],
                )?;
                let again = leak && !unchecked(r);
                open_approval(tx, &row.ws, &row.id, q, t, again)?;
                t.clone()
            }
            _ => {
                q.push_str("（没能改写，原文未变）");
                open_approval(tx, &row.ws, &row.id, q, &row.text, true)?;
                row.text.clone()
            }
        };
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
        let name = spaces::get(tx, &row.space_id)?
            .map(|s| space_name(&s))
            .unwrap_or_default();
        open_approval(
            tx,
            &row.ws,
            &row.id,
            format!("发到「{name}」（没能改写，原文未变）"),
            &row.text,
            false,
        )?;
        messages::event(tx, &row.ws, &row.space_id, &row.id);
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

/// A check that could not run found nothing to rewrite: the text waits for the owner as it is.
fn unchecked(r: &CheckResult) -> bool {
    r.spans
        .iter()
        .any(|s| s.reason == disclosure::R_MODEL_FAILED || s.reason == disclosure::R_UNBOUND)
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

/// The inbound message a reply or trigger reply comes from, in the same space.
/// The names mentioned in a stored message (trigger words come after leading mentions).
fn mention_names(c: &Connection, id: &str) -> ChannelResult<Vec<String>> {
    let raw: Option<String> = c
        .query_row(
            "SELECT mentions FROM chan_messages WHERE id = ?",
            [id],
            |r| r.get(0),
        )
        .optional()?;
    Ok(raw
        .and_then(|s| serde_json::from_str::<Vec<Value>>(&s).ok())
        .unwrap_or_default()
        .iter()
        .filter_map(|m| m["name"].as_str().map(String::from))
        .collect())
}

fn source_in(c: &Connection, row: &OutRow, source: &str) -> ChannelResult<Option<Source>> {
    Ok(c.query_row(
        "SELECT external_id, sender_account_id, coalesce(text, '') FROM chan_messages
          WHERE id = ? AND workspace_id = ? AND space_id = ? AND direction = 'in'",
        params![source, row.ws, row.space_id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )
    .optional()?)
}

/// Checks the authorization of `row` against the store. `Err` carries the reason for the audit.
pub fn verify(
    c: &Connection,
    row: &OutRow,
    auth: &Auth,
    now: i64,
) -> ChannelResult<Result<(), String>> {
    let space = spaces::get(c, &row.space_id)?.ok_or_else(|| ApiError::not_found("空间"))?;
    let (account, mode) = bindings::connection_account(c, &row.ws, &space.connection_id)?;
    if row.sender_account_id.as_deref() != Some(account.as_str()) {
        return Ok(Err("sender".into()));
    }
    if let Err(why) = crate::pipeline::verify_route(c, &row.id, now)? {
        return Ok(Err(why));
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
            let t = spaces::channel_settings(c, &row.ws, &space.connection_id)?
                .map(|s| Trigger::from_value(&s.trigger));
            let dm = space.kind == SpaceKind::Dm.code();
            // 屏蔽 wins over trigger words (不提醒 does not: they are your explicit command)
            let blocked = spaces::settings(c, &space.id)
                .is_ok_and(|s| s.attention == spaces::Attention::Blocked);
            let names = mention_names(c, source)?;
            match (t, source_in(c, row, source)?) {
                (Some(t), Some((_, Some(sender), text))) => {
                    !blocked
                        && sender == account
                        && t.instruction(&space.id, dm, &text, &names).is_some()
                }
                _ => false,
            }
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
    /// The provider failed: `failed`, a `fail` decision (retry / discard).
    Failed { error: String, decision_id: String },
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
    link: OutLink,
    as_user: BTreeMap<String, Arc<dyn ActAsUser>>,
    pinned: Option<Arc<dyn PinnedModel>>,
    /// Drafts being rewritten now (“改写后再看”), each on a task of its own.
    rewriting: Arc<Mutex<std::collections::HashSet<String>>>,
    #[cfg(feature = "fake")]
    before_attempt: Mutex<Option<(Arc<tokio::sync::Barrier>, Arc<tokio::sync::Barrier>)>>,
}

impl Outbox {
    pub fn new(link: OutLink) -> Self {
        Self {
            link,
            as_user: BTreeMap::new(),
            pinned: None,
            rewriting: Arc::default(),
            #[cfg(feature = "fake")]
            before_attempt: Mutex::new(None),
        }
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
        if let Some(e) = envelope
            && let Some(d) = ch.store().reader(|c| crate::pipeline::existing(c, e))?
        {
            return Ok((crate::pipeline::outcome(&d), None));
        }
        let t = &req.target;
        let mut text = req.text.trim().to_string();
        if text.is_empty() {
            return Err(ApiError::invalid("text"));
        }
        if text.chars().count() > TEXT_MAX {
            text = text.chars().take(TEXT_MAX).collect();
        }
        let (p, audience, knowledge, said) = ch.store().reader(|c| -> ChannelResult<_> {
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
            Ok((p, a, k, said))
        })?;
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
            // a fixed receipt (“收到，已记为 …”, “我请 … 来看。”) no model wrote: the rules only
            skip_model: template(req),
            // a bot's own reply whose knowledge was tracked (a generation) may skip the model for the
            // workspace's own people; text without one (an agent's, a flow's) and anything said as you
            // or on your behalf may not
            internal_skip: req.generation.is_some()
                && p.mode == Mode::Bot
                && req.purpose != Purpose::Trigger,
        };

        // 1. disclosure (and the rewrites)
        let mut res = b1::check(ch.modules(), &cx, &audience, &text, &knowledge).await;
        let mut rewrites: Vec<Rewrite> = Vec::new();
        // a text that waits for the owner whatever the check says is not rewritten: they see the original
        // with the parts marked, and the card offers the rewrite. Unless YOLO decides the card: it never
        // approves a flagged text, so the one rewrite comes first, as for a direct reply
        let will_draft = req.approve || !direct_ok(p.mode, &req.auth);
        if res.verdict == Verdict::Leak
            && req.generation.is_some()
            && !unchecked(&res)
            && (!will_draft || ch.yolo_on(&t.ws))
        {
            for _ in 0..MAX_REWRITES {
                let Some(new) = rewrite_text(
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
                rewrites.push(Rewrite {
                    from: text.clone(),
                    to: new.clone(),
                    reason: res
                        .spans
                        .iter()
                        .map(|s| s.reason.clone())
                        .collect::<Vec<_>>()
                        .join("；"),
                });
                text = new;
                res = b1::check(ch.modules(), &cx, &audience, &text, &knowledge).await;
                if res.verdict != Verdict::Leak || unchecked(&res) {
                    break;
                }
            }
        }
        let now = ch.now();
        let proxy = req.purpose == Purpose::Trigger;
        // 2. authorization
        let leak = res.verdict != Verdict::Pass;
        let direct = !leak && !req.approve && direct_ok(p.mode, &req.auth);
        if !direct && req.purpose == Purpose::HandoffAck {
            // the handoff's own decision already reaches you
            return Ok((Delivered::Skipped, Some(res)));
        }
        let verdict = if leak {
            basis::DRAFTED
        } else if rewrites.is_empty() {
            basis::PASS
        } else {
            basis::REWRITTEN
        };
        let (id, decision, reused) = ch.store().transaction(|tx| -> ChannelResult<_> {
            // Concurrent submissions serialize here, before either can create a second message.
            if let Some(e) = envelope
                && let Some(d) = crate::pipeline::existing(tx, e)?
            {
                return Ok((d.message_id.clone(), d.decision_id.clone(), Some(d)));
            }
            let id = insert_out(
                tx,
                &NewOut {
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
                let mut q = format!("{}发到「{}」", p.speaker, space_name(&p.space));
                if leak {
                    q.push_str(&format!("（可能泄露：{}）", spans_note(&res)));
                }
                // a model's text the check flagged: the card can ask for the rewrite it did not get
                let rewrite = leak && req.generation.is_some() && !unchecked(&res);
                Some(open_approval(tx, &t.ws, &id, q, &text, rewrite)?)
            };
            messages::event(tx, &t.ws, &t.space_id, &id);
            Ok((id, decision, None))
        })?;
        if let Some(d) = reused {
            return Ok((crate::pipeline::outcome(&d), Some(res)));
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
            SendOutcome::Unknown { .. } | SendOutcome::Skipped => {
                Delivered::Queued { message_id: id }
            }
        };
        Ok((d, Some(res)))
    }

    /// The final text: the persona's signature, or “（由 Steve 代发）” after your trigger words.
    fn final_text(c: &Connection, row: &OutRow, conn: &str) -> ChannelResult<String> {
        let mut text = row.text.trim_end().to_string();
        if row.on_behalf == "proxy" {
            let mark = spaces::channel_settings(c, &row.ws, conn)?
                .map(|s| Trigger::from_value(&s.trigger).mark)
                .unwrap_or(true);
            if mark && !text.ends_with(PROXY_MARK) {
                text.push_str(PROXY_MARK);
            }
        } else if let Some(pid) = &row.persona_id
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
                    tx.execute(
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
                tx.execute(
                    "UPDATE chan_messages SET status = 'failed', sent = NULL WHERE id = ?",
                    [&row.id],
                )?;
                if let Some(d) = base::decisions::open_for(tx, &row.ws, SUBJECT_MESSAGE, &row.id)? {
                    tx.execute(
                        "UPDATE decisions SET status = 'resolved', result = ?, resolved = ? WHERE id = ?",
                        params![json!({"dismissed": true, "why": "connection_gone"}).to_string(), now, d.id],
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
                return Ok(Err(SendOutcome::Skipped));
            }
            if let Err(why) = verify(tx, &row, auth, now)? {
                // tried before and may be out: not a draft to approve again (a new text under the
                // same dedupe key would be dropped), it stays unknown and you are asked
                if tried.is_some() {
                    tx.execute(
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
                    _ => open_approval(tx, &row.ws, &row.id, format!("{speaker}发到「{}」", space_name(&space)), &row.text, false)?,
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
            let n = tx.execute(
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
            let space = spaces::get(c, &row.space_id)?.ok_or_else(|| ApiError::not_found("空间"))?;
            let (account, mode) = bindings::connection_account(c, &row.ws, &space.connection_id)?;
            let adapter: String = c.query_row(
                "SELECT adapter FROM connections WHERE id = ?",
                [&space.connection_id],
                |r| r.get(0),
            )?;
            let text = Self::final_text(c, &row, &space.connection_id)?;
            // `[AI] ` only on what YOLO approved in your name (your account, or your trigger words);
            // anything else loses a mark it brought along, so the mark cannot be faked
            let by_yolo = match auth {
                Auth::Decision { id } => base::decisions::get(c, id)?
                    .is_some_and(|d| b1::decided_by_yolo(&d.result)),
                _ => false,
            };
            let text = if by_yolo && (mode == Mode::Own || row.on_behalf == "proxy") {
                b1::with_ai_prefix(&text)
            } else {
                b1::strip_ai_prefix(&text).to_string()
            };
            let kind = SpaceKind::parse(&space.kind).unwrap_or(SpaceKind::Group);
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
            Ok((space, mode, adapter, text, kind, at))
        });
        let (space, mode, adapter, text, kind, at) = match prep {
            Ok(x) => x,
            Err(e) => return self.failed(ch, &row, &e.to_string(), claim),
        };
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
                tx.execute(
                    "UPDATE chan_messages SET sent = NULL WHERE id = ? AND status = 'pending'",
                    [&row.id],
                )?;
                Ok(())
            })?;
            return Ok(SendOutcome::Skipped);
        }
        let cx = match ch.connections().cx_for(&row.ws, &space.connection_id, cap) {
            Ok(cx) => cx,
            Err(e) => {
                ch.store()
                    .transaction(|tx| send_state::forget(tx, &row.id))?;
                return self.failed(ch, &row, &e.to_string(), claim);
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
            if crate::notifications::deferred(tx, &row.id, now)?
                || verify(tx, &row, auth, now)?.is_err()
            {
                tx.execute("UPDATE chan_messages SET sent=NULL WHERE id=? AND status='pending' AND sent=?",params![row.id,claim])?;
                tx.execute("UPDATE personal_notifications SET next_attempt_at=? WHERE delivery_id=(SELECT id FROM output_deliveries WHERE message_id=?)",params![now+10_000,row.id])?;
                return Ok(None);
            }
            Ok(Some(send_state::attempt(tx, &row.id, auth, &text, &at, now)?))
        })?;
        let Some((text, at)) = attempted else {
            return Ok(SendOutcome::Skipped);
        };
        let in_thread = reply_in_thread(kind, row.thread_key.as_deref());
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
                    if tx.execute(
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
            Err(e) => {
                ch.store()
                    .transaction(|tx| send_state::forget(tx, &row.id))?;
                let r = self.failed(ch, &row, &e.to_string(), claim);
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
            if tx.execute(
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

    /// The provider failed: `failed`, a `fail` decision (retry / discard). A missing permission
    /// can't be fixed by retrying (nor by YOLO): only discard, and the question says where to grant it.
    fn failed(
        &self,
        ch: &Channels,
        row: &OutRow,
        error: &str,
        claim: i64,
    ) -> ChannelResult<SendOutcome> {
        let permanent = missing_permission(error);
        tracing::info!(message = %row.id, error, "channel message not sent");
        let d = ch.store().transaction(|tx| -> ChannelResult<Option<String>> {
            // only on our claim: a late answer of one another took over changes nothing
            if tx.execute(
                "UPDATE chan_messages SET status = 'failed', sent = NULL WHERE id = ? AND status = 'pending' AND sent = ?",
                params![row.id, claim],
            )? == 0
            {
                return Ok(None);
            }
            tx.execute("UPDATE output_deliveries SET last_error=? WHERE message_id=?",params![error,row.id])?;
            let space =
                spaces::get(tx, &row.space_id)?.ok_or_else(|| ApiError::not_found("空间"))?;
            let d = if permanent {
                open_fail(
                    tx,
                    row,
                    format!(
                        "发到「{}」的消息没有发出去：{error}。重试也发不出，先到“接入”补上这项权限",
                        space_name(&space)
                    ),
                    &BLOCKED_OPTIONS,
                )?
            } else {
                open_fail(
                    tx,
                    row,
                    format!("发到「{}」的消息没有发出去：{error}", space_name(&space)),
                    &FAIL_OPTIONS,
                )?
            };
            base::audit(
                tx,
                &row.ws,
                "steve",
                "chan_message.send.failed",
                &row.id,
                &json!({"space": row.space_id}),
            )?;
            messages::event(tx, &row.ws, &row.space_id, &row.id);
            Ok(Some(d))
        })?;
        Ok(match d {
            Some(decision_id) => SendOutcome::Failed {
                error: error.into(),
                decision_id,
            },
            None => SendOutcome::Skipped,
        })
    }

    /// The queue (`channels.retry` and the approvals' delay): approved or retried messages whose
    /// delay is over are sent; handoffs answered “to_item” become items. Returns how many were
    /// sent.
    pub async fn send_due(&self, ch: &Channels) -> usize {
        self.bind(ch);
        for a in self.link.take() {
            match a {
                After::ToItem { ws, message_id } => {
                    let plan = ch.store().reader(|c| plan_of(c, &ws, &message_id));
                    match plan {
                        Ok(mut plan) => {
                            plan.act = Act::CreateItem(ItemMode::Start);
                            if let Err(e) = ch.responder().act(ch, &plan).await {
                                tracing::warn!(message = message_id, "handoff to item failed: {e}");
                            }
                        }
                        Err(e) => tracing::warn!(message = message_id, "handoff to item: {e}"),
                    }
                }
                After::Discarded { message_id } => ch.react_discarded(&message_id).await,
            }
        }
        // “改写后再看”: each on a task of its own, so a slow model does not hold up the sends
        match ch.store().reader(rewrites_asked) {
            Ok(asked) => {
                for (m, d) in asked {
                    if !lock(&self.rewriting).insert(m.clone()) {
                        continue;
                    }
                    let (ch, pinned, busy) =
                        (ch.clone(), self.pinned.clone(), self.rewriting.clone());
                    tokio::spawn(async move {
                        if let Err(e) = rewrite_draft(&ch, pinned.as_ref(), &m, &d).await {
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
        let now = ch.now();
        let due = ch.store().reader(|c| -> ChannelResult<Vec<(String, String)>> {
            let mut st = c.prepare_cached(
                "SELECT m.id, d.id, d.result FROM chan_messages m JOIN decisions d ON d.id = m.decision_id
                  WHERE m.direction = 'out' AND m.status = 'pending' AND (m.sent IS NULL OR m.sent < ?)
                    AND d.status = 'resolved' AND d.subject_type = ? AND d.subject_id = m.id
                  ORDER BY coalesce((SELECT n.next_attempt_at FROM personal_notifications n
                    JOIN output_deliveries od ON od.id=n.delivery_id WHERE od.message_id=m.id),0),m.created LIMIT 50",
            )?;
            let rows = st
                .query_map(params![now - CLAIM_STALE_MS, SUBJECT_MESSAGE], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows
                .into_iter()
                .filter(|(_, _, res)| {
                    serde_json::from_str::<Value>(res)
                        .ok()
                        .and_then(|v| v["send_at"].as_i64())
                        .is_some_and(|t| t <= now)
                })
                .map(|(m, d, _)| (m, d))
                .collect())
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
            match self.send(ch, &m, &Auth::Decision { id: d }).await {
                Ok(SendOutcome::Sent { .. }) => n += 1,
                Ok(_) => {}
                Err(e) => tracing::warn!(message = m, "channel send failed: {e}"),
            }
        }
        n + self.resume_direct(ch, now).await + self.settle_unknown(ch, now).await
    }

    /// Direct messages (a rule's or a trigger's reply, no decision) whose send was cut off before
    /// it began, or that waited for their connection: sent under the authorization they were stored
    /// with (checked again by [`Outbox::send`]). One older than [`LATE_MS`] is asked about instead.
    async fn resume_direct(&self, ch: &Channels, now: i64) -> usize {
        let rows = ch.store().reader(|c| -> ChannelResult<Vec<(String, i64, Option<i64>)>> {
            let mut st = c.prepare_cached(
                "SELECT m.id, m.created, s.first_at FROM chan_messages m JOIN chan_send_state s ON s.message_id = m.id
                  WHERE m.direction = 'out' AND m.status = 'pending' AND m.decision_id IS NULL
                    AND ((m.sent IS NULL AND m.created < ?1) OR m.sent < ?2)
                    AND (s.first_at IS NOT NULL OR NOT EXISTS (
                      SELECT 1 FROM personal_notifications n JOIN output_deliveries od ON od.id=n.delivery_id
                      JOIN notification_policies p ON p.workspace_id=n.workspace_id AND p.user_id=n.user_id
                      WHERE od.message_id=m.id AND p.enabled=1 AND (p.mode<>'now' OR n.next_attempt_at>?3)))
                  ORDER BY m.created LIMIT 50",
            )?;
            let v = st
                .query_map(params![now - UNCLAIMED_MS, now - CLAIM_STALE_MS, now], |r| {
                    Ok((r.get(0)?, r.get(1)?, r.get(2)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(v)
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
            let now = ch.now();
            // tried before (the Hub stopped during its send): it may be out, so it is unknown,
            // not late; [`Outbox::settle_unknown`] takes it from there
            if tried.is_some() {
                let r = ch.store().transaction(|tx| -> ChannelResult<()> {
                    tx.execute(
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

    /// A direct reply that did not go out in time: `failed`, with the usual retry / discard card.
    fn ask_late(&self, ch: &Channels, id: &str) -> ChannelResult<()> {
        ch.store().transaction(|tx| -> ChannelResult<()> {
            let Some(row) = out_row(tx, id)? else {
                return Ok(());
            };
            if tx.execute(
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
                format!("发到「{}」的回复中途被打断，没有及时发出；现在还要发吗", space_name(&space)),
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
                    space_name(&space)
                ),
                &UNKNOWN_OPTIONS,
            )?;
            messages::event(tx, &row.ws, &row.space_id, id);
            Ok(())
        })
    }

    /// Runs [`Outbox::send_due`] every [`QUEUE_EVERY`] until the channel module is gone.
    pub fn spawn_queue(self: Arc<Self>, ch: &Channels) -> tokio::task::JoinHandle<()> {
        let weak = ch.downgrade();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(QUEUE_EVERY);
            // A slow round of sends is not followed by a burst of catch-up rounds.
            tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                tick.tick().await;
                let Some(ch) = weak.upgrade() else { break };
                self.send_due(&ch).await;
            }
        })
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
        let req = OutRequest {
            target: OutTarget::reply(&plan),
            text: text.into(),
            purpose: Purpose::Source,
            approve: plan.mode == Mode::Own,
            generation: None,
            auth: Auth::Rule {
                source: plan.message_id.clone(),
                act: Act::Reply,
            },
        };
        let (d, r) = self.deliver_checked(ch, &req).await?;
        Ok(match d {
            Delivered::Sent { .. } => SRC_POSTED.into(),
            Delivered::Failed { .. } => {
                "Not posted: the chat did not accept it. The user can retry.".into()
            }
            Delivered::Drafted { .. } => match r.filter(|r| r.verdict != Verdict::Pass) {
                Some(r) => format!(
                    "Not posted: it may reveal {}. A draft is waiting for the user.",
                    spans_note(&r)
                ),
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
            tx.execute(
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

// ---------------------------------------------------------------- decisions

/// The `chan_message` decision handler: a draft's approval (`yes` sends the possibly edited text
/// after the delay, `no` discards), a failed send (`retry` / `discard`), a handoff (`draft_reply`
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
        match cx.choice {
            "yes" => {
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
                tx.execute(
                    "UPDATE chan_messages SET text = ?, status = 'pending', sent = NULL WHERE id = ? AND status = 'draft'",
                    params![text, row.id],
                )?;
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
        match (cx.choice, row.status.as_str()) {
            ("retry", "failed") => {
                tx.execute(
                    "UPDATE chan_messages SET status = 'pending', sent = NULL WHERE id = ? AND status = 'failed'",
                    [&row.id],
                )?;
                result["hash"] = json!(text_hash(&row.text));
                result["send_at"] = json!(now);
                if cx.actor == b1::YOLO_ACTOR {
                    result["decided_by"] = json!(b1::YOLO_ACTOR);
                }
            }
            ("discard", "failed") => {
                set_status(tx, &row.id, &["failed"], DISCARDED)?;
            }
            // may be out (no answer): you looked
            ("sent", "unknown") => {
                set_status(tx, &row.id, &["unknown"], "sent")?;
                send_state::forget(tx, &row.id)?;
            }
            ("retry", "unknown") => {
                tx.execute(
                    "UPDATE chan_messages SET status = 'pending', sent = NULL WHERE id = ? AND status = 'unknown'",
                    [&row.id],
                )?;
                result["hash"] = json!(text_hash(&row.text));
                result["send_at"] = json!(now);
            }
            ("discard", "unknown") => {
                set_status(tx, &row.id, &["unknown"], DISCARDED)?;
                send_state::forget(tx, &row.id)?;
            }
            _ => return Err(ApiError::conflict(steve_i18n::t!("channels.error.handled"))),
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
        match cx.choice {
            "draft_reply" => {
                let plan = plan_of(tx, cx.ws, &d.subject_id)?;
                let (m, dd) = respond::open_draft(tx, &plan, cx.text.trim(), now)?;
                result["draft_message"] = json!(m);
                result["draft_decision"] = json!(dd);
            }
            "to_item" => {
                plan_of(tx, cx.ws, &d.subject_id)?;
                self.0.push(After::ToItem {
                    ws: cx.ws.into(),
                    message_id: d.subject_id.clone(),
                });
            }
            "done" => {}
            _ => return Err(ApiError::invalid("choice")),
        }
        Ok(result)
    }
}

impl DecisionHandler for MessageDecisions {
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
        if d.kind == "ask" {
            return Ok(());
        }
        let Some(row) = out_row(tx, &d.subject_id)? else {
            return Ok(());
        };
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
            ("approve", "no") if row.status == DISCARDED => {
                set_status(tx, &row.id, &[DISCARDED], "draft")?;
            }
            ("fail", "retry") if row.status == "pending" && row.sent.is_none() => {
                set_status(tx, &row.id, &["pending"], "failed")?;
            }
            ("fail", "discard") if row.status == DISCARDED => {
                set_status(tx, &row.id, &[DISCARDED], "failed")?;
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
