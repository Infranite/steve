//! The outbound path (C6, channels-personas.md §2.7, §10): [`Outbox`] is the
//! [`crate::deliver::Outbound`] every reply, acknowledgement, flow action, trigger reply and
//! return-to-source message goes through, and [`Outbox::send`] is the only place an adapter's
//! `send(` is called. In order:
//!
//! 1. **disclosure** (C5): B1's outbound check for the space's audience with the knowledge the text
//!    was written with; a generated text that may leak is rewritten by the drafting model, at most
//!    twice; still leaking → a draft with the suspicious parts named, even when a grant or a rule
//!    would have let it out; a secret value → `blocked`, never sent and never drafted;
//! 2. **authorization**: a bot account's `reply` rule, a flow's `msg.send` grant or the engine's
//!    approval of the step, your own trigger words, or your approval of this very message. Your own
//!    account never sends without your approval, except the trigger words you typed yourself.
//!    Everything else is a draft with an `approve` decision (“以你本人 / 以<身份>发到「<空间>」”);
//! 3. **send** ([`Outbox::send`]): the authorization is checked against the store again, the final
//!    text (persona signature, “（由 Steve 代发）”) passes the vault guard and the redactor's values,
//!    the message is claimed once (`sent` set while `pending`), the adapter is called with
//!    `uuid = cm_…` (a retry cannot double-send), then `sent` / `failed` + a `fail` decision
//!    (retry / discard), the basis record and the audit.
//!
//! Approvals wait out a short delay ([`SEND_DELAY_MS`]) in the queue ([`Outbox::send_due`], every
//! [`QUEUE_EVERY`]); undoing the approval within it puts the draft back.
//!
//! Wiring (integrator):
//! ```ignore
//! let link = steve_channels::outbound::OutLink::new();
//! registry.decision(steve_channels::respond::SUBJECT_MESSAGE, link.handler());   // before Modules
//! let late = steve_channels::flow_actions::LateChannels::new();
//! let outbox = Arc::new(steve_channels::outbound::Outbox::new(link.clone())
//!     .as_user(steve_channels::adapters::feishu::auth::USER_ADAPTER, feishu_user.clone()));
//! registry.item_hooks(Box::new(steve_channels::outbound::ReturnToSource::new(outbox.clone(), late.clone())));
//! let replies = Arc::new(steve_channels::Replies::new().outbound(outbox.clone()));
//! // … Channels::builder(…).responder(replies.clone()).build(); late.set(&channels);
//! link.bind_clock(modules.clock.clone());
//! outbox.clone().spawn_queue(&channels);
//! ```

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use steve_modules::decisions::{DecisionCx, DecisionHandler, choice_result};
use steve_modules::hooks::ItemHooks;
use steve_modules::outbound::{self as b1, Audience, CheckCx, CheckResult, KnowledgeUse, Verdict};
use steve_modules::{ApiError, ApiResult, Clock, SystemClock, Tier};
use steve_store::base::{self, DecisionKind, mems};
use steve_store::{Decision, Item, NewDecision, Tx, uid};

use crate::adapters::feishu::as_user::{SendAsUser, UserMessage};
use crate::adapters::feishu::send::{at_handle, reply_in_thread};
use crate::basis::{self, NewBasis, Rewrite};
use crate::channel::{OutContent, SendTarget, SpaceKind};
use crate::compose::PinnedModel;
use crate::deliver::{Auth, Delivered, Generation, OutRequest, OutTarget, Outbound, Purpose};
use crate::disclosure::{self, MAX_REWRITES, NoSecrets, SecretScan};
use crate::flow_actions::{LateChannels, plan_of};
use crate::personas::{self, PersonaRow};
use crate::respond::{self, DRAFT_OPTIONS, SUBJECT_MESSAGE};
use crate::rules::{self, Act, ItemMode, Trigger};
use crate::spaces::{self, Mode, SpaceRow};
use crate::{ChannelResult, Channels, bindings, compose, messages};

/// How long an approved message waits before it goes out (undo window).
pub const SEND_DELAY_MS: i64 = 10_000;
/// How often the queue looks for approved messages whose delay is over.
pub const QUEUE_EVERY: Duration = Duration::from_secs(2);
/// A claim older than this is considered abandoned (the Hub stopped mid-send) and retried; the
/// `uuid` keeps the provider from posting it twice.
pub const CLAIM_STALE_MS: i64 = 10 * 60 * 1000;
/// Options of the decision after a failed send (§2.7).
pub const FAIL_OPTIONS: [&str; 2] = ["retry", "discard"];
/// Options of the decision after a blocked one.
pub const BLOCKED_OPTIONS: [&str; 1] = ["discard"];

/// A provider refusal for a missing permission (Feishu 99991672 tenant / 99991679 user scope, or
/// the grant check before the call): retrying can't help.
pub fn missing_permission(error: &str) -> bool {
    ["99991672", "99991679", "没有开通这个权限", "授权里没有"]
        .iter()
        .any(|m| error.contains(m))
}
/// Appended to messages Steve sends as you after your trigger words (§2.7), verbatim.
pub const PROXY_MARK: &str = "（由 Steve 代发）";
/// The longest text, in characters (like `POST /spaces/:id/messages`).
pub const TEXT_MAX: usize = 4000;

pub const MSG_ALREADY_SENT: &str = "消息已经发出，不能撤销";
pub const MSG_HANDLED: &str = "这条消息已经处理过了";
pub const ERR_NO_AUTH: &str = "没有有效的批准或授权";
pub const ERR_SECRET: &str = "正文里有机密值，已拦下";
pub const ERR_NO_RECALL: &str = "这个渠道不支持撤回";

/// `chan_messages.status` of a draft you said no to (the §1 list has no word for it).
pub const DISCARDED: &str = "discarded";

/// The `steve_reply_source` answers (§6), verbatim.
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
    delay_ms: AtomicI64,
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
            delay_ms: AtomicI64::new(SEND_DELAY_MS),
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

    /// The undo window after an approval, in ms (0–60 s).
    pub fn set_delay_ms(&self, ms: i64) {
        self.0
            .delay_ms
            .store(ms.clamp(0, 60_000), Ordering::Relaxed);
    }

    fn delay(&self) -> i64 {
        self.0.delay_ms.load(Ordering::Relaxed)
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

/// The `approve` decision of a draft (§2.7). Returns its id.
fn open_approval(
    tx: &Tx,
    ws: &str,
    id: &str,
    question: String,
    text: &str,
) -> ChannelResult<String> {
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
            options: Some(DRAFT_OPTIONS.map(String::from).to_vec()),
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

/// Whether `auth` may let a message out without a draft on an account in `mode` (step 2). The
/// store check comes again in [`verify`].
pub fn direct_ok(mode: Mode, auth: &Auth) -> bool {
    match (mode, auth) {
        (_, Auth::None) => false,
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
    if !direct_ok(mode, auth) {
        return Ok(Err(format!("{} on {}", auth.code(), mode.code())));
    }
    let ok = match auth {
        Auth::None => false,
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
        Auth::Grant { flow_id, .. } => c.query_row(
            "SELECT EXISTS (SELECT 1 FROM grants WHERE workspace_id = ? AND subject_type = 'flow' AND subject_id = ?
                AND capability = ? AND resource_type = 'space' AND resource_id IN (?, '*') AND revoked IS NULL AND until > ?)",
            params![row.ws, flow_id, crate::flow_actions::CAPABILITY, space.id, now],
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
    Sent {
        external_id: String,
    },
    /// The authorization did not hold: back to a draft with its approval.
    Drafted {
        decision_id: String,
    },
    /// A secret value: `blocked`, a `fail` decision with “discard”.
    Blocked {
        decision_id: String,
    },
    /// The provider failed: `failed`, a `fail` decision (retry / discard).
    Failed {
        error: String,
        decision_id: String,
    },
    /// Not pending, already claimed, gone.
    Skipped,
}

/// C6's outbound. Share one `Arc<Outbox>` between [`crate::Replies::outbound`], the queue and the
/// item hook.
pub struct Outbox {
    link: OutLink,
    as_user: BTreeMap<String, Arc<dyn SendAsUser>>,
    secrets: Arc<dyn SecretScan>,
    pinned: Option<Arc<dyn PinnedModel>>,
}

impl Outbox {
    pub fn new(link: OutLink) -> Self {
        Self {
            link,
            as_user: BTreeMap::new(),
            secrets: Arc::new(NoSecrets),
            pinned: None,
        }
    }

    /// Sends as you on connections of `adapter` (`feishu_user`) through F6's [`SendAsUser`].
    pub fn as_user(mut self, adapter: &str, s: Arc<dyn SendAsUser>) -> Self {
        self.as_user.insert(adapter.into(), s);
        self
    }

    /// The redactor's registered values (checked again on the final text).
    pub fn secrets(mut self, s: Arc<dyn SecretScan>) -> Self {
        self.secrets = s;
        self
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

    async fn rewrite(
        &self,
        ch: &Channels,
        audience: &Audience,
        text: &str,
        r: &CheckResult,
        model: &str,
    ) -> Option<String> {
        let prompt = disclosure::rewrite_prompt(audience, text, &r.spans);
        let out = match &self.pinned {
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

    /// [`Outbound::deliver`] with the check result (for `steve_reply_source`'s answer).
    pub async fn deliver_checked(
        &self,
        ch: &Channels,
        req: &OutRequest,
    ) -> ChannelResult<(Delivered, Option<CheckResult>)> {
        self.bind(ch);
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
        };

        // 1. disclosure (and the rewrites)
        let mut res = b1::check(ch.modules(), &cx, &audience, &text, &knowledge).await;
        let mut rewrites: Vec<Rewrite> = Vec::new();
        if res.verdict == Verdict::Leak && req.generation.is_some() {
            for _ in 0..MAX_REWRITES {
                let Some(new) = self
                    .rewrite(
                        ch,
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
                if res.verdict != Verdict::Leak {
                    break;
                }
            }
        }
        let now = ch.now();
        let proxy = req.purpose == Purpose::Trigger;
        if res.verdict == Verdict::Blocked {
            let (id, d) = ch.store().transaction(|tx| -> ChannelResult<_> {
                let shown = steve_vault::guard::redact(&text)
                    .unwrap_or_else(|| steve_vault::guard::REPLACEMENT.to_string());
                let id = insert_out(
                    tx,
                    &NewOut {
                        t,
                        p: &p,
                        text: &shown,
                        status: "blocked",
                        proxy,
                    },
                    now,
                )?;
                let row = out_row(tx, &id)?.ok_or_else(|| ApiError::not_found("消息"))?;
                let d = open_fail(
                    tx,
                    &row,
                    format!(
                        "发到「{}」的消息里有机密值，已拦下，没有发出",
                        space_name(&p.space)
                    ),
                    &BLOCKED_OPTIONS,
                )?;
                base::audit(
                    tx,
                    &t.ws,
                    "steve",
                    "chan_message.send.blocked",
                    &id,
                    &json!({"reason": "secret", "space": t.space_id}),
                )?;
                messages::event(tx, &t.ws, &t.space_id, &id);
                Ok((id, d))
            })?;
            if completes(req.purpose)
                && let Some(src) = &t.source_message_id
            {
                ch.react_finish(src, crate::reactions::Finish::Failed).await;
            }
            return Ok((
                Delivered::Blocked {
                    message_id: id,
                    decision_id: d,
                },
                Some(res),
            ));
        }

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
        let (id, decision) = ch.store().transaction(|tx| -> ChannelResult<_> {
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
                Some(open_approval(tx, &t.ws, &id, q, &text)?)
            };
            messages::event(tx, &t.ws, &t.space_id, &id);
            Ok((id, decision))
        })?;
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
            SendOutcome::Blocked { decision_id } => Delivered::Blocked {
                message_id: id,
                decision_id,
            },
            SendOutcome::Failed { decision_id, .. } => Delivered::Failed {
                message_id: id,
                decision_id,
            },
            SendOutcome::Skipped => Delivered::Skipped,
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

    /// Sends one `pending` outbound message (§10). The only caller of an adapter's `send(`: checks
    /// `auth` against the store, claims the message once, scans the final text for secrets, sends
    /// with `uuid = cm_…`, records the result.
    pub async fn send(&self, ch: &Channels, id: &str, auth: &Auth) -> ChannelResult<SendOutcome> {
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
            if row.status != "pending" || (row.sent.is_some() && !stale) {
                return Ok(Err(SendOutcome::Skipped));
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
                // not authorized after all: back to a draft, nothing leaves
                set_status(tx, &row.id, &["pending"], "draft")?;
                let space = spaces::get(tx, &row.space_id)?.ok_or_else(|| ApiError::not_found("空间"))?;
                let speaker = place(tx, &row.ws, &space.connection_id, &row.space_id, row.persona_id.as_deref())?.speaker;
                let d = match base::decisions::open_for(tx, &row.ws, SUBJECT_MESSAGE, &row.id)? {
                    Some(d) if d.kind == "approve" => d.id,
                    _ => open_approval(tx, &row.ws, &row.id, format!("{speaker}发到「{}」", space_name(&space)), &row.text)?,
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
                "UPDATE chan_messages SET sent = ? WHERE id = ? AND status = 'pending' AND (sent IS NULL OR sent < ?)",
                params![now, row.id, now - CLAIM_STALE_MS],
            )?;
            Ok(if n == 1 { Ok(row) } else { Err(SendOutcome::Skipped) })
        })?;
        let row = match claimed {
            Ok(r) => r,
            Err(o) => return Ok(o),
        };

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
            Err(e) => return self.failed(ch, &row, &e.to_string()),
        };
        let secrets = disclosure::secret_spans(self.secrets.as_ref(), &row.ws, &text);
        if !secrets.is_empty() {
            let d = ch.store().transaction(|tx| -> ChannelResult<String> {
                tx.execute(
                    "UPDATE chan_messages SET status = 'blocked', sent = NULL, text = ? WHERE id = ?",
                    params![
                        steve_vault::guard::redact(&row.text)
                            .unwrap_or_else(|| steve_vault::guard::REPLACEMENT.into()),
                        row.id
                    ],
                )?;
                let d = open_fail(
                    tx,
                    &row,
                    format!("发到「{}」的消息里有机密值，已拦下，没有发出", space_name(&space)),
                    &BLOCKED_OPTIONS,
                )?;
                base::audit(
                    tx,
                    &row.ws,
                    "steve",
                    "chan_message.send.blocked",
                    &row.id,
                    &json!({"reason": "secret"}),
                )?;
                messages::event(tx, &row.ws, &row.space_id, &row.id);
                Ok(d)
            })?;
            ch.react_answered(&row.id, false).await;
            return Ok(SendOutcome::Blocked { decision_id: d });
        }

        // the adapter
        let cx = match ch.connections().cx(&row.ws, &space.connection_id) {
            Ok(cx) => cx,
            Err(e) => return self.failed(ch, &row, &e.to_string()),
        };
        let in_thread = reply_in_thread(kind, row.thread_key.as_deref());
        let sent: Result<String, String> = match (mode, self.as_user.get(&adapter)) {
            (Mode::Own, Some(u)) => {
                let mut m = UserMessage::text(text.clone());
                m.uuid = Some(row.id.clone());
                match &row.reply_to {
                    Some(r) => u.reply_as_user(&cx, r, &m, in_thread).await,
                    None => u.send_as_user(&cx, &space.external_id, &m).await,
                }
                .map(|s| s.message_id)
                .map_err(|e| e.to_string())
            }
            _ => match ch.adapter(&adapter) {
                Some(a) => {
                    let to = match &row.reply_to {
                        Some(r) => SendTarget::Reply {
                            ext_msg_id: r.clone(),
                            in_thread,
                        },
                        None => SendTarget::Space {
                            ext_id: space.external_id.clone(),
                        },
                    };
                    let mut content = OutContent::text(text.clone());
                    content.uuid = Some(row.id.clone());
                    content.at = at;
                    content.space_ext_id = Some(space.external_id.clone());
                    a.send(&cx, &to, &content).await.map_err(|e| e.to_string())
                }
                None => Err("这个渠道还不能发送消息".into()),
            },
        };
        match sent {
            Ok(ext) => {
                let now = ch.now();
                ch.store().transaction(|tx| -> ChannelResult<()> {
                    tx.execute(
                        "UPDATE chan_messages SET status = 'sent', external_id = ?, text = ?, sent = ? WHERE id = ?",
                        params![ext, text, now, row.id],
                    )?;
                    base::audit(
                        tx,
                        &row.ws,
                        "steve",
                        "chan_message.send",
                        &row.id,
                        &json!({"auth": auth.code(), "space": row.space_id, "on_behalf": row.on_behalf}),
                    )?;
                    messages::event(tx, &row.ws, &row.space_id, &row.id);
                    Ok(())
                })?;
                ch.react_answered(&row.id, true).await;
                Ok(SendOutcome::Sent { external_id: ext })
            }
            Err(e) => {
                let r = self.failed(ch, &row, &e);
                ch.react_answered(&row.id, false).await;
                r
            }
        }
    }

    /// The provider failed: `failed`, a `fail` decision (retry / discard). A missing permission
    /// can't be fixed by retrying (nor by YOLO): only discard, and the question says where to grant it.
    fn failed(&self, ch: &Channels, row: &OutRow, error: &str) -> ChannelResult<SendOutcome> {
        let permanent = missing_permission(error);
        tracing::info!(message = %row.id, error, "channel message not sent");
        let d = ch.store().transaction(|tx| -> ChannelResult<String> {
            tx.execute(
                "UPDATE chan_messages SET status = 'failed', sent = NULL WHERE id = ?",
                [&row.id],
            )?;
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
            Ok(d)
        })?;
        Ok(SendOutcome::Failed {
            error: error.into(),
            decision_id: d,
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
        let now = ch.now();
        let due = ch.store().reader(|c| -> ChannelResult<Vec<(String, String)>> {
            let mut st = c.prepare_cached(
                "SELECT m.id, d.id, d.result FROM chan_messages m JOIN decisions d ON d.id = m.decision_id
                  WHERE m.direction = 'out' AND m.status = 'pending' AND (m.sent IS NULL OR m.sent < ?)
                    AND d.status = 'resolved' AND d.subject_type = ? AND d.subject_id = m.id
                  ORDER BY m.created LIMIT 50",
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
        n
    }

    /// Runs [`Outbox::send_due`] every [`QUEUE_EVERY`] until the channel module is gone.
    pub fn spawn_queue(self: Arc<Self>, ch: &Channels) -> tokio::task::JoinHandle<()> {
        let weak = ch.downgrade();
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(QUEUE_EVERY);
            loop {
                tick.tick().await;
                let Some(ch) = weak.upgrade() else { break };
                self.send_due(&ch).await;
            }
        })
    }

    /// Back to the source (§2.7): an item made from a chat message finished → “<编号> 已完成：<结果>”
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
            Ok(Some((plan, delivery_summary(c, item)?)))
        })?;
        let Some((plan, summary)) = found else {
            return Ok(None);
        };
        let req = OutRequest {
            target: OutTarget::reply(&plan),
            text: done_text(&item.id, &summary),
            purpose: Purpose::Done,
            approve: true,
            generation: None,
            auth: Auth::None,
        };
        Ok(Some(self.deliver_checked(ch, &req).await?.0))
    }

    /// `steve_reply_source` (§6): an agent's update to the chat its item came from. On a bot account
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
            Delivered::Blocked { .. } => "Not posted: it contains a secret value.".into(),
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
            Delivered::Skipped => SRC_DRAFTED.into(),
        })
    }

    /// Recalls a sent message (`POST /chan-messages/:id/recall`): `400 这个渠道不支持撤回` when the
    /// adapter can't.
    pub async fn recall(
        &self,
        ch: &Channels,
        ws: &str,
        actor: &str,
        id: &str,
    ) -> ChannelResult<()> {
        let (row, space, adapter) = ch.store().reader(|c| -> ChannelResult<_> {
            let row = out_row(c, id)?
                .filter(|r| r.ws == ws)
                .ok_or_else(|| ApiError::not_found("消息"))?;
            let space =
                spaces::get(c, &row.space_id)?.ok_or_else(|| ApiError::not_found("空间"))?;
            let adapter: String = c.query_row(
                "SELECT adapter FROM connections WHERE id = ?",
                [&space.connection_id],
                |r| r.get(0),
            )?;
            Ok((row, space, adapter))
        })?;
        let a = ch
            .adapter(&adapter)
            .filter(|a| a.features().recall)
            .ok_or_else(|| ApiError::rule(ERR_NO_RECALL))?;
        if row.status != "sent" {
            return Err(ApiError::conflict("这条消息没有发出，不能撤回"));
        }
        let ext: Option<String> = ch.store().reader(|c| {
            c.query_row(
                "SELECT external_id FROM chan_messages WHERE id = ?",
                [id],
                |r| r.get(0),
            )
            .map_err(steve_store::StoreError::from)
        })?;
        let ext = ext.ok_or_else(|| ApiError::conflict("这条消息没有发出，不能撤回"))?;
        let cx = ch
            .connections()
            .cx(ws, &space.connection_id)
            .map_err(|e| ApiError::rule(e.to_string()))?;
        a.recall(&cx, &ext)
            .await
            .map_err(|e| ApiError::rule(format!("没有撤回：{e}")))?;
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
            return Err(ApiError::conflict(MSG_HANDLED));
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
                    return Err(ApiError::rule("要先写好回复的内容"));
                }
                let send_at = now + self.0.delay();
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
            return Err(ApiError::conflict(MSG_HANDLED));
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
            // a blocked message stays blocked; “discard” only closes the card
            ("discard", "blocked") => {}
            _ => return Err(ApiError::conflict(MSG_HANDLED)),
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
            _ => Err(ApiError::rule("不认识这种决定")),
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
            ("approve", "no") if row.status == DISCARDED => {
                set_status(tx, &row.id, &[DISCARDED], "draft")?;
            }
            ("fail", "retry") if row.status == "pending" && row.sent.is_none() => {
                set_status(tx, &row.id, &["pending"], "failed")?;
            }
            ("fail", "discard") if row.status == DISCARDED => {
                set_status(tx, &row.id, &[DISCARDED], "failed")?;
            }
            ("fail", "discard") if row.status == "blocked" => {}
            _ => return Err(ApiError::conflict(MSG_ALREADY_SENT)),
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
        ext_msg_id: &str,
        kind: crate::channel::ReactionKind,
    ) -> Result<crate::reactions::Added, String> {
        let adapter = crate::reactions::adapter_of(ch, ws, conn)?;
        match self.as_user.get(&adapter) {
            Some(u) => {
                let cx = ch.connections().cx(ws, conn).map_err(|e| e.to_string())?;
                let emoji = crate::adapters::feishu::send::emoji_type(kind);
                let id = u
                    .react_as_user(&cx, ext_msg_id, emoji)
                    .await
                    .map_err(|e| e.to_string())?;
                Ok(crate::reactions::Added {
                    reaction_id: id,
                    emoji: emoji.into(),
                    as_user: true,
                })
            }
            None => {
                crate::reactions::AdapterReactor
                    .add(ch, ws, conn, ext_msg_id, kind)
                    .await
            }
        }
    }

    async fn remove(
        &self,
        ch: &Channels,
        ws: &str,
        conn: &str,
        ext_msg_id: &str,
        reaction_id: &str,
        as_user: bool,
    ) -> Result<(), String> {
        let adapter = crate::reactions::adapter_of(ch, ws, conn)?;
        match (as_user, self.as_user.get(&adapter)) {
            (true, Some(u)) => {
                let cx = ch.connections().cx(ws, conn).map_err(|e| e.to_string())?;
                u.unreact_as_user(&cx, ext_msg_id, reaction_id)
                    .await
                    .map_err(|e| e.to_string())
            }
            _ => {
                crate::reactions::AdapterReactor
                    .remove(ch, ws, conn, ext_msg_id, reaction_id, as_user)
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
