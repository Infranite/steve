//! The main conversation with Steve (thread.ts), one per workspace: every message goes through the router.
//!
//! Thread routing: the conversation is the default. A message is routed on the
//! workspace lane (a few seconds, one at a time, in order); [`decide`] puts the code's guards over the
//! model's choice; answers and read-only research then run on the workspace's inline queue
//! ([`research`]), so the lane is free for the next message. Items are made only for real work, a
//! suggestion card asks when it is unclear, and the user can correct the routing both ways ([`reroute`]).

mod a2ui;
pub mod decide;
#[cfg(test)]
mod eval;
pub mod flowback;
mod handle;
mod offer;
mod reroute;
pub mod research;
mod suggest;

pub use a2ui::{A2uiActed, A2uiAction};
pub use offer::Handled;

pub use reroute::RerouteTo;
pub use research::{
    RESUME_WINDOW, ResearchOutcome, ResearchRequest, ResearchSink, Researcher, ResumeFrom, RunEnd,
    ToolSeen,
};

use crate::Core;
use crate::error::Result;
use crate::gateway::{self, FocusItem, Route, RouteCtx, Said};
use crate::repo::{FileRef, HubEvent, ItemExt, Message};
use crate::util::{js_head, now};
use chrono::{Local, TimeZone};
use regex::Regex;
use rusqlite::OptionalExtension;
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};
use steve_modules::contacts::item_people::{self, Mention};
use tokio::sync::mpsc;
use tokio_util::sync::CancellationToken;

static REF: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^#?([A-Z]{1,6}-\d+)\b\s*").expect("valid regex"));
static AT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^@(\S+)\s*").expect("valid regex"));

/// Whether “@`at`” names the bot called `name`: “名字” or “名字Bot”, spaces ignored (the composer
/// writes “@Steve开发” for “Steve 开发”).
fn names_bot(name: &str, at: &str) -> bool {
    let squash = |s: &str| s.split_whitespace().collect::<String>();
    let n = squash(name);
    let a = squash(at);
    a == n || a == n.clone() + "Bot"
}

/// A message waiting on its workspace lane.
#[derive(serde::Serialize, serde::Deserialize)]
struct Job {
    mid: String,
    text: String,
    files: Vec<FileRef>,
    /// The sending user, if known.
    user: Option<String>,
    /// `@` mentions sent with it (contacts P3).
    mentions: Vec<Mention>,
    /// where the user sent it: an item (focus bar) or a quoted message
    to: SayTo,
    /// the conversation it was said in
    conv: Conv,
    /// Its way is fixed without the router (`/汇报`, sent to an item, quoting one, `#ST-n`): it goes on
    /// its own lane and does not wait for the turn in progress. Worked out again whenever it is queued.
    #[serde(skip)]
    direct: bool,
    /// Said while Steve was still answering, and it needs those answers: it waits for them on the
    /// conversation's [`after_lane`], so the messages behind it are not held up.
    #[serde(skip)]
    after: bool,
    /// Routed while Steve was still answering, and sure to stand on its own
    /// ([`decide::now_while_answering`]): handled with this route (and how long it took) at once.
    #[serde(skip)]
    ahead: Option<(Route, i64)>,
}

/// The lane of a conversation's messages whose way is fixed ([`Job::direct`]).
pub(crate) fn direct_lane(ws: &str, space: &str) -> String {
    format!("{}\u{1f}direct", lane_key(ws, space))
}

/// A message taken off the [`after_lane`]: no longer waiting once this goes ([`Core::undefer`]).
struct Deferred {
    core: Core,
    ws: String,
    mid: String,
}

impl Drop for Deferred {
    fn drop(&mut self) {
        self.core.undefer(&self.ws, &self.mid);
    }
}

/// The most messages of a lane routed in one call ([`Core::route_batch`]).
const BATCH_MAX: usize = 8;

/// A message routed with others before its turn ([`Core::route_batch`]).
struct Prerouted {
    /// the text it was routed as: edited since, it is routed again
    text: String,
    route: Route,
    /// its share of the call's time
    ms: i64,
    /// the router knew what Steve was still answering ([`RouteCtx::pending`])
    pending: bool,
}

/// Why a waiting message could not be taken off its lane's queue ([`Core::claim`]).
enum Claim {
    /// edited after it was routed ahead: its text now
    Changed(String),
    Failed(crate::error::CoreError),
}

/// The lane of a conversation's messages that wait for the answers Steve is still giving ([`Job::after`]).
pub(crate) fn after_lane(ws: &str, space: &str) -> String {
    format!("{}\u{1f}after", lane_key(ws, space))
}

/// A message as the router reads it: it only sees that there are attachments; the bot that does the
/// work gets the files themselves.
fn routed_text(text: &str, files: &[FileRef]) -> String {
    if files.is_empty() {
        return text.to_string();
    }
    let list = files
        .iter()
        .map(|f| format!("{} · {}", f.name, f.mime.split(';').next().unwrap_or("")))
        .collect::<Vec<_>>()
        .join("、");
    format!("{text}\n（附带 {} 个附件：{list}）", files.len())
}

/// The web conversation's `messages.space`.
pub const MAIN: &str = "steve";
/// Automatic re-answers of one interrupted main-conversation run; past it, an explicit rerun.
pub const MAX_RESUME_ATTEMPTS: i64 = 1;

/// What the listener-ready resume of the main conversation did.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct ThreadReady {
    pub resumed: usize,
    /// Handed back to an explicit rerun (re-answered once already, no longer local).
    pub held: usize,
    /// Could not be resumed now (logged; the intent stays for the next boot).
    pub failed: usize,
}

/// A conversation: the web's (`steve`) or a guest one — an IM space's (`chan:<space>`),
/// whose turns come from people in that space. A guest conversation is routed like the web's,
/// but never changes configuration, never talks to items (`#ST-n`, “送到事项”, progress questions)
/// and never answers `/汇报`; research and items are allowed only when the host says so.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Conv {
    pub space: String,
    /// read-only research may answer here
    pub research: bool,
    /// a turn may become an item here
    pub items: bool,
    /// only the workspace's owner reads it (their private chat with Steve)
    pub owner_only: bool,
    /// the person speaking may start work: the workspace's owner. What anyone else asks for becomes an
    /// item that waits for the owner's yes (a proposal), never work that runs on its own
    pub start: bool,
}

impl Default for Conv {
    fn default() -> Self {
        Self::main()
    }
}

impl Conv {
    pub fn main() -> Self {
        Self {
            space: MAIN.into(),
            research: true,
            items: true,
            owner_only: false,
            start: true,
        }
    }
    /// A conversation whose turns are not the user's own (an IM space).
    pub fn guest(&self) -> bool {
        self.space != MAIN
    }
}

/// What owns the guest conversations (the channel module, through the hub): it answers in its own
/// voice and takes what Steve says there back out to where the turn came from.
#[async_trait::async_trait]
pub trait ConvHost: Send + Sync {
    /// The inline answer to turn `origin` (instead of the main bot's chat answer).
    async fn answer(
        &self,
        ws: &str,
        space: &str,
        origin: &str,
    ) -> std::result::Result<String, String>;
    /// What research in this conversation must know about its audience.
    /// What research answering message `origin` in `space` is told about where it answers (the
    /// conversation card and the rules of the place).
    fn research_context(&self, ws: &str, space: &str, origin: &str) -> String;
    /// Steve stored message `id` in guest conversation `space`.
    fn said(&self, ws: &str, space: &str, id: &str);
}

/// Where a message goes besides the router: straight to an item (the focus bar “正在跟 ST-n 说”), or
/// as a reply quoting a message (a message of an item goes to that item).
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct SayTo {
    pub item: Option<String>,
    pub reply_to: Option<String>,
}

/// How often a chat answer being written goes out (`steve.draft`), in ms.
const DRAFT_EVERY: i64 = 50;

type Promotion = Arc<tokio::sync::OnceCell<std::result::Result<crate::repo::Item, String>>>;

/// `meta.steps` on a Steve reply: the routing decision, the reasoning behind a direct answer, the
/// research run, the tools Steve used.
#[derive(Default)]
pub(crate) struct Steps {
    route: Option<Value>,
    answer: Option<Value>,
    research: Option<Value>,
    tools: Vec<Value>,
}

impl Steps {
    fn json(&self, ms: i64) -> Value {
        let mut m = Map::new();
        if let Some(r) = &self.route {
            m.insert("route".into(), r.clone());
        }
        if let Some(a) = &self.answer {
            m.insert("answer".into(), a.clone());
        }
        if let Some(a) = &self.research {
            m.insert("research".into(), a.clone());
        }
        m.insert("tools".into(), json!(self.tools));
        m.insert("ms".into(), json!(ms));
        Value::Object(m)
    }
    /// A tool starts: recorded right away, so a failing one still shows. Returns (index, start).
    fn begin(steps: &Mutex<Steps>, name: &str, args: Value) -> (usize, i64) {
        let mut s = lock(steps);
        s.tools
            .push(json!({"name": name, "args": args, "status": "ok", "ms": 0}));
        (s.tools.len() - 1, now())
    }
    /// A tool ended: its result, or `failed` with the error (≤200 chars).
    fn end<T>(
        steps: &Mutex<Steps>,
        (i, t0): (usize, i64),
        r: Result<T>,
        result: impl Fn(&T) -> Value,
    ) -> Result<T> {
        let mut s = lock(steps);
        let t = &mut s.tools[i];
        match &r {
            Ok(x) => t["result"] = result(x),
            Err(e) => {
                t["status"] = json!("failed");
                t["error"] = json!(js_head(&e.to_string(), 200));
            }
        }
        t["ms"] = json!(now() - t0);
        r
    }
}

/// `meta.steps.route`: the model's choice and what the guards made of it.
fn route_step(r: &Route, d: Option<&decide::Decision>, ms: i64) -> Value {
    json!({
        "kind": d.map(|d| d.kind.as_str()).unwrap_or(r.kind.as_str()),
        "modelKind": r.kind,
        "confidence": r.confidence,
        "signals": r.signals,
        "guard": d.and_then(|d| d.guard),
        "title": r.title, "bot": r.bot, "project": r.project, "tags": r.tag_ids,
        "newTags": r.new_tags.iter().map(|t| &t.name).collect::<Vec<_>>(),
        "why": r.why, "model": r.model, "fallback": r.fallback, "ms": ms
    })
}

/// The lane of conversation `space` in workspace `ws`: the main conversation's is the workspace id.
pub(crate) fn lane_key(ws: &str, space: &str) -> String {
    if space == MAIN {
        ws.to_string()
    } else {
        format!("{ws}\u{1f}{space}")
    }
}

/// How long a conversation's lane waits for its next message before it goes (a new message starts
/// it again). Short in tests, so they go through it.
const LANE_IDLE: std::time::Duration =
    std::time::Duration::from_millis(if cfg!(test) { 300 } else { 600_000 });

/// The user's stop of a reply or an answer being prepared: set once, never cleared.
#[derive(Clone, Default)]
pub(crate) struct Stop {
    /// what a research executor reads ([`research::ResearchRequest::stop`])
    flag: Arc<AtomicBool>,
    token: CancellationToken,
}

impl Stop {
    pub(crate) fn stop(&self) {
        self.flag.store(true, Ordering::SeqCst);
        self.token.cancel();
    }
    pub(crate) fn is_stopped(&self) -> bool {
        self.flag.load(Ordering::SeqCst)
    }
    /// Resolves once stopped: raced against a model call, the user's stop ends it at once (the
    /// harness process is killed with the dropped call) instead of after the whole reply is written.
    pub(crate) async fn stopped(&self) {
        self.token.cancelled().await
    }
    /// The flag a research executor reads.
    pub(crate) fn flag(&self) -> Arc<AtomicBool> {
        self.flag.clone()
    }
    /// The same stop (the same job), not just an equal one.
    pub(crate) fn same(&self, other: &Stop) -> bool {
        Arc::ptr_eq(&self.flag, &other.flag)
    }
}

/// Per-conversation lanes, the reply being prepared, inline queues and promotions in flight.
/// A conversation message forwarded to an item: `mid`, and whether it `asked` something (a
/// follow-up question, not an instruction), so the turn answering it ends on purpose.
pub(crate) struct Forwarded {
    pub(crate) mid: String,
    pub(crate) asked: bool,
}

#[derive(Default)]
pub(crate) struct Lanes {
    /// routing, one message at a time per conversation ([`lane_key`])
    lanes: Mutex<HashMap<String, mpsc::UnboundedSender<Job>>>,
    admitted: Mutex<HashSet<String>>,
    /// the message being routed, per [`lane_key`]; its flag stops it
    inflight: Mutex<HashMap<String, Stop>>,
    /// per workspace, the main conversation's messages on the [`after_lane`] (id, text), in order
    deferred: Mutex<HashMap<String, Vec<(String, String)>>>,
    promoting: Mutex<HashMap<String, Promotion>>,
    /// answers and research runs, one at a time per workspace
    inline: Mutex<HashMap<String, research::InlineLane>>,
    /// the read-only research executor (R2); without one, research falls back to a direct answer
    researcher: OnceLock<Arc<dyn Researcher>>,
    /// item → the conversation message last forwarded to it, until its turn ends
    forwarded: Mutex<HashMap<String, Forwarded>>,
    /// offers, suggestion cards and corrections are handled one at a time (a double click is one click)
    acting: Mutex<()>,
    /// the owner of guest conversations (IM spaces)
    host: OnceLock<Arc<dyn ConvHost>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// A message the conversation no longer shows (flow-back folded into its item's card, a release
/// notice that belongs to the notifications): kept, not deleted.
pub fn hidden(m: &Message) -> bool {
    m.meta.get("hidden").is_some_and(gateway::truthy)
}

/// The object form of a message's `meta`.
fn meta_of(m: &Message) -> Map<String, Value> {
    m.meta.as_object().cloned().unwrap_or_default()
}

impl Core {
    /// Post the user's message (returns its id) and queue it on the workspace's lane. Needs a tokio runtime.
    pub fn say(&self, ws: &str, text: &str, files: &[FileRef]) -> Result<String> {
        self.say_as(ws, None, text, files, &[])
    }

    /// [`Core::say`] from a known user: module context for the answer (B1 `PromptContext::chat`) is
    /// what this user may see; without one, only what the whole workspace may see.
    ///
    /// `mentions` (contacts P3): `party` mentions are linked to the item the message becomes and named
    /// in the reply; without mentions the people the text names are inferred. Both need a known user.
    pub fn say_as(
        &self,
        ws: &str,
        user: Option<&str>,
        text: &str,
        files: &[FileRef],
        mentions: &[Mention],
    ) -> Result<String> {
        self.say_to(ws, user, text, files, mentions, SayTo::default())
    }

    /// A turn in conversation `conv` from someone who is not a workspace user (an IM space's
    /// sender): stored with `meta`, routed on the workspace lane like web chat. Needs a tokio runtime.
    pub fn say_in(&self, ws: &str, conv: Conv, text: &str, meta: &Value) -> Result<String> {
        let mid = self
            .store()
            .message_add_in(ws, &conv.space, "me", text, None, meta)?;
        self.enqueue(
            ws,
            Job {
                mid: mid.clone(),
                text: text.to_string(),
                files: vec![],
                user: None,
                mentions: vec![],
                to: SayTo::default(),
                conv,
                direct: false,
                after: false,
                ahead: None,
            },
        );
        Ok(mid)
    }

    /// Sets the owner of guest conversations (once; `false` if one was set already).
    pub fn set_conv_host(&self, h: Arc<dyn ConvHost>) -> bool {
        self.0.thread.host.set(h).is_ok()
    }

    pub(crate) fn conv_host(&self) -> Option<Arc<dyn ConvHost>> {
        self.0.thread.host.get().cloned()
    }

    /// The conversation of message `id` (`steve` when it is gone).
    pub(crate) fn conv_of(&self, id: &str) -> String {
        self.store()
            .message(id)
            .ok()
            .flatten()
            .map(|m| m.space)
            .unwrap_or_else(|| MAIN.into())
    }

    /// The last 200 messages of conversation `space`, oldest first.
    pub(crate) fn conv_messages(&self, ws: &str, space: &str) -> Result<Vec<Message>> {
        self.store().messages_in(ws, space, 200)
    }

    /// Steve says `text` in conversation `space`; a guest conversation's host hears of it.
    pub(crate) fn post(
        &self,
        ws: &str,
        space: &str,
        text: &str,
        item: Option<&str>,
        meta: Option<&Value>,
    ) -> Result<String> {
        let id = self.store().message_add_in(
            ws,
            space,
            "steve",
            text,
            item,
            meta.unwrap_or(&Value::Null),
        )?;
        self.said(ws, space, &id);
        Ok(id)
    }

    /// Tells a guest conversation's host about Steve's message `id`.
    pub(crate) fn said(&self, ws: &str, space: &str, id: &str) {
        if space != MAIN
            && let Some(h) = self.conv_host()
        {
            h.said(ws, space, id);
        }
    }

    /// [`Core::say_as`] sent to an item or quoting a message.
    pub fn say_to(
        &self,
        ws: &str,
        user: Option<&str>,
        text: &str,
        files: &[FileRef],
        mentions: &[Mention],
        to: SayTo,
    ) -> Result<String> {
        self.say_with(ws, user, text, files, mentions, to, Map::new())
    }

    /// [`Core::say_to`] with more on the message's meta (an A2UI action says what it was).
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn say_with(
        &self,
        ws: &str,
        user: Option<&str>,
        text: &str,
        files: &[FileRef],
        mentions: &[Mention],
        to: SayTo,
        mut meta: Map<String, Value>,
    ) -> Result<String> {
        if !files.is_empty() {
            meta.insert("files".into(), json!(files));
        }
        if let Some(r) = &to.reply_to {
            meta.insert("reply_to".into(), json!(r));
        }
        if let Some(i) = &to.item {
            meta.insert("to".into(), json!(i));
        }
        /* said while Steve is still on a turn: it waits, visibly, and can be edited or withdrawn until then;
        one whose way is fixed (to an item, `/汇报`) does not wait */
        if !self.direct_way(ws, text, &to, false)? && self.main_busy(ws)? {
            meta.insert("held".into(), json!(true));
        }
        let meta = (!meta.is_empty()).then_some(Value::Object(meta));
        let mid = self
            .store()
            .message_add(ws, "me", text, None, meta.as_ref())?;
        self.enqueue(
            ws,
            Job {
                mid: mid.clone(),
                text: text.to_string(),
                files: files.to_vec(),
                user: user.map(String::from),
                mentions: mentions.to_vec(),
                to,
                conv: Conv::main(),
                direct: false,
                after: false,
                ahead: None,
            },
        );
        Ok(mid)
    }

    /// Whether the main conversation is on a turn: a message being routed, an answer or research
    /// running or waiting, or messages still waiting their turn on the lane.
    pub fn main_busy(&self, ws: &str) -> Result<bool> {
        if self.main_busy_besides(ws) {
            return Ok(true);
        }
        /* a key range rather than `LIKE`, which is case-insensitive and so can't seek the settings key */
        Ok(self.store().store().reader::<_, steve_store::StoreError>(|c| {
            Ok(c.query_row(
                "SELECT EXISTS(SELECT 1 FROM settings WHERE workspace_id=? AND key >= 'thread.pending.' AND key < 'thread.pending/'
                   AND json_valid(value) AND json_extract(value,'$.conv.space')=?)",
                rusqlite::params![ws, MAIN],
                |r| r.get::<_, bool>(0),
            )?)
        })?)
    }

    /// The main conversation's turn is on (a message being routed on its lane, an answer or research) —
    /// what a message with a fixed way passes without waiting.
    pub(crate) fn main_busy_besides(&self, ws: &str) -> bool {
        {
            let inflight = lock(&self.0.thread.inflight);
            if inflight.contains_key(ws) || inflight.contains_key(&after_lane(ws, MAIN)) {
                return true;
            }
        }
        self.answering(ws)
    }

    /// A message of the main conversation is being routed right now.
    #[cfg(test)]
    pub(crate) fn routing(&self, ws: &str) -> bool {
        let inflight = lock(&self.0.thread.inflight);
        inflight.contains_key(ws) || inflight.contains_key(&after_lane(ws, MAIN))
    }

    /// Steve is still answering in the main conversation: an answer or research running or waiting,
    /// or messages waiting for them on the [`after_lane`].
    fn answering(&self, ws: &str) -> bool {
        self.inline_busy(ws)
            || lock(&self.0.thread.deferred)
                .get(ws)
                .is_some_and(|d| !d.is_empty())
    }

    /// While Steve is still answering: routes a message of the main conversation ahead of its turn,
    /// the router told what is being answered. The route when the message is handled at once
    /// ([`decide::now_while_answering`]); `None` when it waits for the answers, as do a plain question
    /// (research, no router asked) and a message the router failed on.
    async fn ahead(
        &self,
        ws: &str,
        mid: &str,
        text: &str,
        files: &[FileRef],
    ) -> Option<(Route, i64)> {
        if decide::without_router(text, !files.is_empty(), false, &[]).is_some() {
            return None;
        }
        let history = self.history_before(ws, mid).ok()?;
        let mut c = self.route_ctx_in(ws, false).ok()?;
        c.pending = self.inline_questions(ws);
        if let Some(d) = lock(&self.0.thread.deferred).get(ws) {
            c.pending.extend(d.iter().map(|(_, t)| t.clone()));
        }
        if c.pending.is_empty() {
            /* the answers came out meanwhile: it is routed in its turn, with them */
            return None;
        }
        let r0 = now();
        let r = crate::stats::tagged(
            "route",
            ws,
            gateway::route(self.gw(), &c, &routed_text(text, files), &history, false),
        )
        .await;
        self.goes_ahead(ws, text, r, now() - r0)
    }

    /// A route made while Steve is still answering ([`Core::ahead`], or a burst's): kept when the
    /// message goes now ([`decide::now_while_answering`]), `None` when it waits.
    fn goes_ahead(&self, ws: &str, text: &str, r: Route, ms: i64) -> Option<(Route, i64)> {
        let named = self.named_bot(ws, text).ok()?;
        let d = decide::decide(
            &r,
            text,
            decide::DecideCtx {
                named_bot: named.is_some(),
            },
        );
        let d = decide::for_main(d, &r);
        decide::now_while_answering(&r, &d).then_some((r, ms))
    }

    /// Routes `first` and the messages waiting behind it on the same lane in one call, at most
    /// [`BATCH_MAX`] (2026-11-09: a burst costs one routing, not one per message). Plain questions,
    /// which need no router, and messages with a fixed way are left out; with fewer than two left,
    /// or when the call fails, nothing is routed here and each goes alone as before. While Steve is
    /// still answering in the main conversation, the router is told what, as for one message.
    async fn route_batch(
        &self,
        ws: &str,
        first: &Job,
        rest: &std::collections::VecDeque<Job>,
        out: &mut HashMap<String, Prerouted>,
    ) {
        let guest = first.conv.guest();
        let mut picked: Vec<(String, String, Vec<FileRef>)> = vec![];
        for j in std::iter::once(first).chain(rest.iter()) {
            if picked.len() >= BATCH_MAX {
                break;
            }
            if j.after || j.direct || j.conv != first.conv || out.contains_key(&j.mid) {
                continue;
            }
            let Ok(Some(text)) = self.waiting_text(ws, &j.mid) else {
                continue;
            };
            if decide::without_router(&text, !j.files.is_empty(), guest, &[]).is_some() {
                continue;
            }
            picked.push((j.mid.clone(), text, j.files.clone()));
        }
        if picked.len() < 2 {
            return;
        }
        let (Ok(history), Ok(mut c)) = (
            self.history_before(ws, &picked[0].0),
            self.route_ctx_in(ws, guest),
        ) else {
            return;
        };
        if !guest && self.answering(ws) {
            c.pending = self.inline_questions(ws);
            if let Some(d) = lock(&self.0.thread.deferred).get(ws) {
                c.pending.extend(d.iter().map(|(_, t)| t.clone()));
            }
        }
        let texts: Vec<String> = picked.iter().map(|(_, t, f)| routed_text(t, f)).collect();
        let r0 = now();
        let Some(rs) = crate::stats::tagged(
            "route",
            ws,
            gateway::route_many(self.gw(), &c, &texts, &history),
        )
        .await
        else {
            return;
        };
        let ms = (now() - r0) / picked.len() as i64;
        let pending = !c.pending.is_empty();
        for ((mid, text, _), route) in picked.into_iter().zip(rs) {
            out.insert(
                mid,
                Prerouted {
                    text,
                    route,
                    ms,
                    pending,
                },
            );
        }
    }

    /// The enabled bot a message's “@名字 …” names.
    pub(super) fn named_bot(&self, ws: &str, text: &str) -> Result<Option<crate::repo::Bot>> {
        let Some(a) = AT.captures(text).map(|c| c[1].to_string()) else {
            return Ok(None);
        };
        Ok(self
            .store()
            .bots(ws)?
            .into_iter()
            .find(|b| b.enabled && names_bot(&b.name, &a)))
    }

    /// Moves a message of the main conversation to its [`after_lane`]: it waits for the answers Steve is
    /// still giving and is routed after them, with them in the history.
    fn defer(&self, ws: &str, mut job: Job) {
        lock(&self.0.thread.deferred)
            .entry(ws.to_string())
            .or_default()
            .push((job.mid.clone(), job.text.clone()));
        job.after = true;
        job.ahead = None;
        let key = after_lane(ws, &job.conv.space);
        let mut lanes = lock(&self.0.thread.lanes);
        let tx = lanes
            .entry(key.clone())
            .or_insert_with(|| self.routing_lane(ws, &key));
        if let Err(mpsc::error::SendError(job)) = tx.send(job) {
            tracing::error!("after lane of {ws} was gone; starting it again");
            let tx = self.routing_lane(ws, &key);
            let _ = tx.send(job);
            lanes.insert(key, tx);
        }
    }

    /// A message off the [`after_lane`]: Steve no longer waits on it.
    fn undefer(&self, ws: &str, mid: &str) {
        let mut d = lock(&self.0.thread.deferred);
        if let Some(v) = d.get_mut(ws) {
            v.retain(|(m, _)| m != mid);
            if v.is_empty() {
                d.remove(ws);
            }
        }
    }

    /// The text of a message still waiting on its lane, as it is now (edited while it waited); `None`
    /// once it was withdrawn or taken.
    fn waiting_text(&self, ws: &str, mid: &str) -> Result<Option<String>> {
        let key = format!("thread.pending.{mid}");
        let raw: Option<String> =
            self.store()
                .store()
                .reader::<_, steve_store::StoreError>(|c| {
                    Ok(c.query_row(
                        "SELECT value FROM settings WHERE workspace_id=? AND key=?",
                        rusqlite::params![ws, key],
                        |r| r.get(0),
                    )
                    .optional()?)
                })?;
        Ok(raw
            .and_then(|r| serde_json::from_str::<Job>(&r).ok())
            .map(|j| j.text))
    }

    /// How many routing and inline lanes are running.
    #[cfg(test)]
    pub(crate) fn lanes_open(&self) -> (usize, usize) {
        (
            lock(&self.0.thread.lanes).len(),
            lock(&self.0.thread.inline).len(),
        )
    }

    /// Waits until the main conversation's answer or research (and those queued behind it) is done:
    /// a message said meanwhile is routed after the turn, with its reply in the history.
    async fn turn_done(&self, ws: &str) {
        loop {
            /* listen before looking, so an end in between is not missed */
            let Some(idle) = self.inline_idle(ws) else {
                return;
            };
            let ended = idle.notified();
            tokio::pin!(ended);
            ended.as_mut().enable();
            if !self.inline_busy(ws) {
                return;
            }
            ended.await;
        }
    }

    /// Takes a waiting message off the lane's durable queue, as it is now (edited while it waited);
    /// `None` when it was withdrawn. A held message moves to now: it comes after the turn it waited on.
    /// With `routed_as`, the text it was routed ahead as: edited since, it stays on the queue
    /// ([`Claim::Changed`]).
    fn claim(
        &self,
        ws: &str,
        mid: &str,
        routed_as: Option<&str>,
    ) -> std::result::Result<Option<Job>, Claim> {
        let key = format!("thread.pending.{mid}");
        let job = self
            .store()
            .store()
            .with_conn(|c| {
                let tx = c.unchecked_transaction()?;
                let raw: Option<String> = tx
                    .query_row(
                        "SELECT value FROM settings WHERE workspace_id=? AND key=?",
                        rusqlite::params![ws, key],
                        |r| r.get(0),
                    )
                    .optional()?;
                let Some(raw) = raw else { return Ok(None) };
                if let Some(was) = routed_as
                    && let Ok(now) = serde_json::from_str::<Job>(&raw)
                    && now.text != was
                {
                    return Ok(Some(Err(now.text)));
                }
                tx.execute(
                    "DELETE FROM settings WHERE workspace_id=? AND key=?",
                    rusqlite::params![ws, key],
                )?;
                // strictly after the last message of its conversation: the turn's last word (a stop note)
                // can be written in the same millisecond, and a tie would list them in either order
                let released = tx.execute(
                    "UPDATE messages SET created=MAX(?1, (SELECT MAX(m.created) + 1 FROM messages m
                   WHERE m.workspace_id=messages.workspace_id AND m.space=messages.space)),
                   meta=json_remove(meta,'$.held')
                  WHERE id=?2 AND json_valid(meta) AND json_extract(meta,'$.held')=1",
                    rusqlite::params![now(), mid],
                )?;
                tx.commit()?;
                Ok(Some(Ok((raw, released > 0))))
            })
            .map_err(|e| Claim::Failed(e.into()))?;
        let (raw, released) = match job {
            None => return Ok(None),
            Some(Err(text)) => return Err(Claim::Changed(text)),
            Some(Ok(j)) => j,
        };
        if released {
            self.store()
                .store()
                .publish(steve_store::ChangeEvent::Message { id: mid.into() });
        }
        serde_json::from_str(&raw)
            .map(Some)
            .map_err(|e| Claim::Failed(e.into()))
    }

    /// Rewrites a message still waiting its turn (`PATCH /api/thread/queued/:id`); `false` once it is
    /// being handled (or gone).
    pub fn thread_queued_edit(&self, ws: &str, mid: &str, text: &str) -> Result<bool> {
        let key = format!("thread.pending.{mid}");
        let done = self.store().store().with_conn(|c| {
            let tx = c.unchecked_transaction()?;
            let raw: Option<String> = tx
                .query_row(
                    "SELECT value FROM settings WHERE workspace_id=? AND key=?",
                    rusqlite::params![ws, key],
                    |r| r.get(0),
                )
                .optional()?;
            let Some(mut job) = raw.and_then(|r| serde_json::from_str::<Job>(&r).ok()) else {
                return Ok(false);
            };
            if job.conv.space != MAIN {
                return Ok(false);
            }
            job.text = text.to_string();
            /* `updated` stays when it was said: the queue keeps its order across a restart */
            tx.execute(
                "UPDATE settings SET value=? WHERE workspace_id=? AND key=?",
                rusqlite::params![serde_json::to_string(&job)?, ws, key],
            )?;
            tx.execute(
                "UPDATE messages SET text=? WHERE id=? AND workspace_id=?",
                rusqlite::params![text, mid, ws],
            )?;
            tx.commit()?;
            Ok(true)
        })?;
        if done {
            self.store()
                .store()
                .publish(steve_store::ChangeEvent::Message { id: mid.into() });
        }
        Ok(done)
    }

    /// Withdraws a message still waiting its turn (`DELETE /api/thread/queued/:id`): it is deleted and
    /// never routed. Its text and files, to put back in the composer; `None` once it is being handled.
    pub fn thread_queued_withdraw(
        &self,
        ws: &str,
        mid: &str,
    ) -> Result<Option<(String, Vec<FileRef>)>> {
        let key = format!("thread.pending.{mid}");
        let job = self.store().store().with_conn(|c| {
            let tx = c.unchecked_transaction()?;
            let raw: Option<String> = tx
                .query_row(
                    "SELECT value FROM settings WHERE workspace_id=? AND key=?",
                    rusqlite::params![ws, key],
                    |r| r.get(0),
                )
                .optional()?;
            let Some(job) = raw.and_then(|r| serde_json::from_str::<Job>(&r).ok()) else {
                return Ok(None);
            };
            if job.conv.space != MAIN {
                return Ok(None);
            }
            tx.execute(
                "DELETE FROM settings WHERE workspace_id=? AND key=?",
                rusqlite::params![ws, key],
            )?;
            tx.execute(
                "DELETE FROM messages WHERE id=? AND workspace_id=?",
                rusqlite::params![mid, ws],
            )?;
            tx.commit()?;
            Ok(Some(job))
        })?;
        let Some(job) = job else { return Ok(None) };
        self.store()
            .store()
            .publish(steve_store::ChangeEvent::Message { id: mid.into() });
        Ok(Some((job.text, job.files)))
    }

    /// Whether a message's way is fixed without the router, as [`Core::handle`] takes it: `/汇报`, sent to an
    /// item of this workspace (focus bar) or quoting one of its messages, or `#ST-n …` (a bare `ST-n …`
    /// unless it asks about it) of an item that exists. Never in a guest conversation.
    fn direct_way(&self, ws: &str, text: &str, to: &SayTo, guest: bool) -> Result<bool> {
        if guest {
            return Ok(false);
        }
        if text.trim() == "/汇报" {
            return Ok(true);
        }
        let db = self.store();
        let here =
            |id: &str| -> Result<bool> { Ok(db.item(id)?.is_some_and(|i| i.workspace_id == ws)) };
        let target = match (&to.item, &to.reply_to) {
            (Some(i), _) => Some(i.clone()),
            (None, Some(r)) => db.message(r)?.and_then(|m| m.item_id),
            _ => None,
        };
        if let Some(id) = target
            && here(&id)?
        {
            return Ok(true);
        }
        if let Some(c) = REF.captures(text)
            && (text.starts_with('#') || !decide::pure_question(text))
        {
            return here(&c[1]);
        }
        Ok(false)
    }

    /// Queues a stored message on its conversation's routing lane (its own lane when its way is fixed).
    fn enqueue(&self, ws: &str, mut job: Job) {
        // Persist only the waiting routing work. Once routing starts, its outcome can be
        // effectful; a crash must not blindly replay it. Planned drain waits for that permit.
        let key = format!("thread.pending.{}", job.mid);
        let payload = serde_json::to_string(&job).expect("serializable thread job");
        if let Err(e) = self.store().store().with_conn(|c| {
            c.execute(
                "INSERT OR IGNORE INTO settings(workspace_id,key,value,updated) VALUES(?,?,?,?)",
                rusqlite::params![ws, key, payload, now()],
            )?;
            Ok(())
        }) {
            tracing::error!("persist thread queue: {e}");
            return;
        }
        if !lock(&self.0.thread.admitted).insert(job.mid.clone()) {
            return;
        }
        job.direct = self
            .direct_way(ws, &job.text, &job.to, job.conv.guest())
            .unwrap_or_else(|e| {
                tracing::warn!("thread lane of {}: {e}", job.mid);
                false
            });
        /* sent under the map's lock: an idle lane only goes under it too, and only with nothing queued */
        let mut lanes = lock(&self.0.thread.lanes);
        let key = if job.direct {
            direct_lane(ws, &job.conv.space)
        } else {
            lane_key(ws, &job.conv.space)
        };
        let tx = lanes
            .entry(key.clone())
            .or_insert_with(|| self.routing_lane(ws, &key));
        if let Err(mpsc::error::SendError(job)) = tx.send(job) {
            /* its worker is gone: a new lane takes the message, so the conversation is not stuck */
            tracing::error!("thread lane of {ws} was gone; starting it again");
            let tx = self.routing_lane(ws, &key);
            let _ = tx.send(job);
            lanes.insert(key, tx);
        }
    }

    /// A routing lane `key` of workspace `ws`: one message at a time, each on a task of its own so a
    /// panic loses that message only. It goes after [`LANE_IDLE`] without messages.
    fn routing_lane(&self, ws: &str, key: &str) -> mpsc::UnboundedSender<Job> {
        let (tx, mut rx) = mpsc::unbounded_channel::<Job>();
        let me = tx.downgrade();
        let core = self.clone();
        let (w, key) = (ws.to_string(), key.to_string());
        tokio::spawn(async move {
            /* messages taken off the channel to be routed together, and the routes they got */
            let mut backlog: std::collections::VecDeque<Job> = Default::default();
            let mut routed: HashMap<String, Prerouted> = HashMap::new();
            loop {
                let next = match backlog.pop_front() {
                    Some(j) => Ok(Some(j)),
                    None => tokio::time::timeout(LANE_IDLE, rx.recv()).await,
                };
                let j = match next {
                    Ok(Some(j)) => j,
                    Ok(None) => return,
                    Err(_) => {
                        let mut lanes = lock(&core.0.thread.lanes);
                        if !rx.is_empty() {
                            continue;
                        }
                        if me
                            .upgrade()
                            .is_some_and(|me| lanes.get(&key).is_some_and(|t| t.same_channel(&me)))
                        {
                            lanes.remove(&key);
                        }
                        return;
                    }
                };
                /* the messages said meanwhile are routed together with this one: one call for a burst */
                while let Ok(more) = rx.try_recv() {
                    backlog.push_back(more);
                }
                if !j.after && !j.direct && !routed.contains_key(&j.mid) && !backlog.is_empty() {
                    core.route_batch(&w, &j, &backlog, &mut routed).await;
                }
                let mut pre = routed.remove(&j.mid);
                let mut ahead = None;
                /* a message off the after lane counts as waiting until it is handled, so one said
                meanwhile is still asked whether it needs this one's answer */
                let _waited = j.after.then(|| Deferred {
                    core: core.clone(),
                    ws: w.clone(),
                    mid: j.mid.clone(),
                });
                if j.after {
                    core.turn_done(&w).await;
                } else if !j.conv.guest() && !j.direct && core.answering(&w) {
                    /* Steve is still answering: what stands on its own goes now, the rest waits for the
                    answers on the after lane, so it does not hold up the messages behind it */
                    let text = match core.waiting_text(&w, &j.mid) {
                        Ok(Some(t)) => t,
                        Ok(None) => {
                            lock(&core.0.thread.admitted).remove(&j.mid);
                            continue;
                        }
                        Err(e) => {
                            tracing::error!("thread queue peek: {e}");
                            j.text.clone()
                        }
                    };
                    /* routed in a burst with what is being answered in view: decided as one alone would be */
                    let r = match pre.take().filter(|p| p.pending && p.text == text) {
                        Some(p) => core.goes_ahead(&w, &text, p.route, p.ms),
                        None => core.ahead(&w, &j.mid, &text, &j.files).await,
                    };
                    match r {
                        Some(r) => ahead = Some((text, r)),
                        None => {
                            core.defer(&w, Job { text, ..j });
                            continue;
                        }
                    }
                }
                if let Some(p) = pre.filter(|p| !p.pending && !j.after && ahead.is_none()) {
                    ahead = Some((p.text, (p.route, p.ms)));
                }
                let _activity = core.store().store().activity("thread_router").await;
                let (direct, after) = (j.direct, j.after);
                let routed_as = ahead.as_ref().map(|(t, _)| t.as_str());
                let j = match core.claim(&w, &j.mid, routed_as) {
                    /* edited after it was routed: the route was for other words, so it is routed again
                    as it is now (and waits, if Steve is still answering and it needs that) */
                    Err(Claim::Changed(text)) => {
                        backlog.push_front(Job { text, ..j });
                        continue;
                    }
                    Err(Claim::Failed(e)) => {
                        tracing::error!("thread queue claim: {e}");
                        lock(&core.0.thread.admitted).remove(&j.mid);
                        continue;
                    }
                    Ok(Some(job)) => Job {
                        direct,
                        after,
                        ahead: ahead.map(|(_, r)| r),
                        ..job
                    },
                    Ok(None) => {
                        /* withdrawn while it waited */
                        lock(&core.0.thread.admitted).remove(&j.mid);
                        continue;
                    }
                };
                let (mid, guest) = (j.mid.clone(), j.conv.guest());
                let (c, w2) = (core.clone(), w.clone());
                let run = tokio::spawn(async move {
                    c.store().store().admitted(c.handle(&w2, &j)).await;
                });
                if let Err(e) = run.await {
                    /* what `handle` would have cleared on its way out */
                    tracing::error!(message = %mid, "routing a message in {w} failed: {e}");
                    lock(&core.0.thread.inflight).remove(&key);
                    if !guest {
                        core.store().publish(HubEvent::SteveThinking {
                            on: false,
                            ws: Some(w.clone()),
                            phase: None,
                        });
                    }
                }
                lock(&core.0.thread.admitted).remove(&mid);
            }
        });
        tx
    }

    /// A message the paste guard caught: stored with the secrets replaced, answered with
    /// the fixed reply (linking the vault), and not routed: no model sees it (`guarded`, so a
    /// restart does not take it for a message the routing lane lost).
    /// A note of Steve's in the main conversation, linking to `href` (the paste guard's, under a
    /// message whose secret was replaced before it went on).
    pub fn say_note(&self, ws: &str, text: &str, href: &str) -> Result<String> {
        self.store()
            .message_add(ws, "steve", text, None, Some(&json!({"href": href})))
    }

    pub fn say_guarded(&self, ws: &str, stored: &str, reply: &str, href: &str) -> Result<String> {
        let mid =
            self.store()
                .message_add(ws, "me", stored, None, Some(&json!({"guarded": true})))?;
        self.store()
            .message_add(ws, "steve", reply, None, Some(&json!({"href": href})))?;
        Ok(mid)
    }

    /// Send a conversation message to an item: its reply flows back when that turn ends.
    pub(crate) fn forward(
        &self,
        item: &str,
        text: &str,
        files: &[FileRef],
        mid: &str,
    ) -> Result<()> {
        self.supplement(item, text, crate::orchestrator::SupplementMode::Auto, files)?;
        let asked = decide::pure_question(text);
        lock(&self.0.thread.forwarded).insert(
            item.into(),
            Forwarded {
                mid: mid.into(),
                asked,
            },
        );
        Ok(())
    }

    /// The forwarded message an item's turn answered, once.
    pub(crate) fn forwarded_take(&self, item: &str) -> Option<Forwarded> {
        lock(&self.0.thread.forwarded).remove(item)
    }

    /// Sets the research executor (once; `false` if one was set already).
    pub fn set_researcher(&self, r: Arc<dyn Researcher>) -> bool {
        self.0.thread.researcher.set(r).is_ok()
    }

    /// What the router may pick from in this workspace.
    pub fn route_ctx(&self, ws: &str) -> Result<RouteCtx> {
        self.route_ctx_in(ws, false)
    }

    /// [`Core::route_ctx`]; a guest conversation sees no items to talk to.
    pub(crate) fn route_ctx_in(&self, ws: &str, guest: bool) -> Result<RouteCtx> {
        let db = self.store();
        let projects = db.projects(ws)?;
        let repo = self.config().steve_repo.clone();
        let items = db.items(ws)?;
        Ok(RouteCtx {
            bots: db.bots(ws)?.into_iter().filter(|b| b.enabled).collect(),
            steve_project: projects
                .iter()
                .find(|p| crate::config::is_steve_project(&p.path, &repo))
                .map(|p| p.id.clone()),
            projects: projects.into_iter().filter(|p| !p.archived).collect(),
            tags: db.tags(ws)?,
            focus: if guest {
                vec![]
            } else {
                self.focus(ws, &items)?
            },
            items: items.into_iter().map(|i| i.id).collect(),
            researching: self.researching(ws),
            pending: vec![],
            debug: self.config().debug_route,
        })
    }

    /// The conversation's open items with something going on in the last 24 hours, latest first (≤3).
    fn focus(&self, ws: &str, items: &[crate::repo::Item]) -> Result<Vec<FocusItem>> {
        let since = now() - 24 * 3600 * 1000;
        let th = self.store().store().thread();
        let mut out: Vec<(i64, FocusItem)> = vec![];
        for t in th.items(ws)? {
            let Some(i) = items.iter().find(|i| i.id == t.item_id) else {
                continue;
            };
            if i.is_finished() || i.updated < since {
                continue;
            }
            let origin = self
                .store()
                .message(&t.origin_msg)?
                .map(|m| m.text)
                .unwrap_or_default();
            out.push((
                i.updated,
                FocusItem {
                    id: i.id.clone(),
                    title: i.title.clone(),
                    status: i.status.clone(),
                    origin,
                },
            ));
        }
        out.sort_by_key(|(t, _)| std::cmp::Reverse(*t));
        Ok(out.into_iter().take(3).map(|(_, f)| f).collect())
    }

    /// Route a message (also used by `POST /api/items` when bot, title or project are missing).
    pub async fn route(
        &self,
        ws: &str,
        text: &str,
        history: &[Said],
        promote: bool,
    ) -> Result<Route> {
        self.route_in(ws, text, history, promote, false).await
    }

    pub(crate) async fn route_in(
        &self,
        ws: &str,
        text: &str,
        history: &[Said],
        promote: bool,
        guest: bool,
    ) -> Result<Route> {
        let c = self.route_ctx_in(ws, guest)?;
        Ok(crate::stats::tagged(
            "route",
            ws,
            gateway::route(self.gw(), &c, text, history, promote),
        )
        .await)
    }

    /// The model gateway, for the API's one-off drafts (`bots/draft`, `projects/describe`).
    pub fn gateway(&self) -> &dyn crate::ModelGateway {
        self.gw()
    }

    /// A chat message's attachments for Steve's own print-mode answer (files.ts `forAgent`): images that
    /// fit go into the message itself; everything (images too) is copied next to the run and named, so
    /// the rest can be read with the Read tool. `earlier` are the files of the message it follows up on.
    fn chat_files(&self, files: &[FileRef], earlier: &[FileRef]) -> gateway::ChatFiles {
        if files.is_empty() && earlier.is_empty() {
            return gateway::ChatFiles::default();
        }
        let cfg = self.config();
        let copy = |fs: &[FileRef]| {
            crate::attachments::for_agent(self.store(), &cfg.data_dir, &cfg.scratch, fs, true)
        };
        let mut out = gateway::ChatFiles {
            count: files.len() + earlier.len(),
            ..Default::default()
        };
        for (fs, before) in [(files, false), (earlier, true)] {
            if fs.is_empty() {
                continue;
            }
            match copy(fs) {
                Ok(a) => {
                    let note = if before {
                        a.note.replacen("用户附带了", "用户在前面的消息里附带了", 1)
                    } else {
                        a.note
                    };
                    out.note.push_str(&note);
                    out.images
                        .extend(a.images.iter().map(crate::attachments::message_image));
                    /* only files that are not inline images need the Read tool */
                    out.readable.extend(a.readable);
                }
                Err(e) => out.note.push_str(&format!("\n（附件没能读取：{e}）")),
            }
        }
        out
    }

    /// The attachments of the message `mid` follows up on, when it has none of its own: the latest user
    /// message with files among the few before it, within half an hour (“这样你也看不出来吗？” right after
    /// a screenshot). Empty when a research session that already saw them is resumed.
    pub(crate) fn earlier_files(
        &self,
        ws: &str,
        mid: &str,
        files: &[FileRef],
        resume: Option<&research::ResumeFrom>,
    ) -> Result<Vec<FileRef>> {
        if !files.is_empty() {
            return Ok(vec![]);
        }
        let db = self.store();
        let msgs = self.conv_messages(ws, &self.conv_of(mid))?;
        let Some(at) = msgs.iter().position(|m| m.id == mid) else {
            return Ok(vec![]);
        };
        let t0 = msgs[at].created;
        let Some(m) = msgs[..at]
            .iter()
            .rev()
            .filter(|m| !hidden(m))
            .take(6)
            .take_while(|m| t0 - m.created <= 30 * 60 * 1000)
            .find(|m| {
                m.author == "me"
                    && m.meta
                        .get("files")
                        .is_some_and(|f| f.as_array().is_some_and(|a| !a.is_empty()))
            })
        else {
            return Ok(vec![]);
        };
        if let Some(r) = resume
            && db
                .store()
                .thread()
                .run(&r.run)?
                .is_some_and(|run| run.message_id == m.id)
        {
            return Ok(vec![]);
        }
        Ok(serde_json::from_value(m.meta["files"].clone()).unwrap_or_default())
    }

    /// `steve.thinking` with the phase of the message being handled; `on: true` repeats per phase like TS.
    fn phase(&self, ws: &str, phase: &str) {
        self.store().publish(HubEvent::SteveThinking {
            on: true,
            ws: Some(ws.into()),
            phase: Some(phase.into()),
        });
    }

    /// The conversation before message `mid`, as the router and answers see it. An open offer on a
    /// Steve message is shown after its text, so “好，改吧” can be read as taking it.
    fn history_before(&self, ws: &str, mid: &str) -> Result<Vec<Said>> {
        let db = self.store();
        let at0 = db.message(mid)?.map(|m| m.created).unwrap_or(i64::MAX);
        Ok(self
            .conv_messages(ws, &self.conv_of(mid))?
            .into_iter()
            .filter(|m| m.id != mid && m.created <= at0 && !hidden(m))
            .map(|m| {
                let offer = m
                    .meta
                    .get("offer")
                    .filter(|o| o["state"] == "open")
                    .map(|o| format!("\n（提议：{}）", gateway::jstr(&o["goal"])))
                    .unwrap_or_default();
                /* the router and the answer know a message came with files, and which */
                let files = m
                    .meta
                    .get("files")
                    .and_then(Value::as_array)
                    .filter(|a| !a.is_empty())
                    .map(|a| {
                        let names: Vec<String> =
                            a.iter().map(|f| gateway::jstr(&f["name"])).collect();
                        format!("\n（附带 {} 个附件：{}）", a.len(), names.join("、"))
                    })
                    .unwrap_or_default();
                Said {
                    author: m.author,
                    text: format!("{}{files}{offer}", m.text),
                }
            })
            .collect())
    }

    /// The people of a new item from its message: `party` mentions are linked and
    /// returned as the reply's note (“关联了 A、B”); without mentions the names in the text are inferred
    /// (no note). Failures are logged: the item stands either way.
    fn item_people(
        &self,
        ws: &str,
        user: Option<&str>,
        item: &str,
        text: &str,
        mentions: &[Mention],
    ) -> Option<String> {
        let (m, user) = (self.modules()?, user?);
        let who = steve_modules::Caller {
            ws: ws.into(),
            me: user.into(),
        };
        if mentions.is_empty() {
            if let Err(e) = item_people::infer_item_people(m, &who, item, text) {
                tracing::warn!(item, "infer item people: {e}");
            }
            return None;
        }
        match item_people::link_mentions(m, &who, item, mentions) {
            Ok(names) => item_people::mention_note(&names),
            Err(e) => {
                tracing::warn!(item, "link mentions: {e}");
                None
            }
        }
    }

    /// Stop what Steve is preparing in this workspace: the reply being routed and the answer or research
    /// being written (its partial text is kept). Messages waiting their turn stay, and go next.
    pub fn stop(&self, ws: &str) -> Result<bool> {
        /* the message being routed in its turn: on the lane, or after the answers it waited for */
        let routing = {
            let inflight = lock(&self.0.thread.inflight);
            inflight
                .get(ws)
                .or_else(|| inflight.get(&after_lane(ws, MAIN)))
                .cloned()
        };
        self.stop_turn(ws, routing.as_ref())
    }

    /// At boot: interrupt unfinished inline work and persist safe local resume intents.
    /// External guest replies retain explicit rerun; local answers wait for `thread_ready`.
    pub fn thread_recover(&self) -> Result<()> {
        let th = self.store().store().thread();
        let resume = !self.config().no_resume;
        for r in th.runs_interrupt(resume)? {
            let space = self.conv_of(&r.message_id);
            // a run interrupted again after its automatic re-answer gets no second “will continue”
            let again = th.resume_attempts(&r.id)? > 0;
            let (text, meta) = if space == MAIN && resume && !again {
                (
                    "（Hub 重启，服务就绪后会继续回答）",
                    json!({"resume_pending": true, "origin": r.message_id, "run": r.id}),
                )
            } else {
                (
                    "（Hub 重启，没答完）",
                    json!({"rerun": r.message_id, "origin": r.message_id, "run": r.id}),
                )
            };
            self.store()
                .message_add_in(&r.workspace_id, &space, "steve", text, None, &meta)?;
        }
        // stopped before routing (the lane is in memory): no route, no run, no reply. Say so, with
        // a rerun, instead of leaving the message silently unanswered; never answered by itself.
        for (ws, mid) in th.unrouted_since(now() - crate::orchestrator::RESUME_MAX_AGE_MS)? {
            let pending: Option<String> = self
                .store()
                .store()
                .reader::<_, steve_store::StoreError>(|c| {
                    Ok(c.query_row(
                        "SELECT value FROM settings WHERE workspace_id=? AND key=?",
                        rusqlite::params![ws, format!("thread.pending.{mid}")],
                        |r| r.get(0),
                    )
                    .optional()?)
                })?;
            // Only a complete job for this message and conversation will be routed by ready.
            // Malformed JSON, incomplete jobs and mismatched identities still need a rerun card.
            if pending
                .as_deref()
                .and_then(|raw| serde_json::from_str::<Job>(raw).ok())
                .is_some_and(|job| job.mid == mid && job.conv.space == MAIN)
            {
                continue;
            }
            self.store().message_add_in(
                &ws,
                MAIN,
                "steve",
                "（Hub 重启，这条消息还没来得及处理，需要时点“重新回答”）",
                None,
                &json!({"rerun": mid, "origin": mid, "interrupted": "route"}),
            )?;
        }
        self.thread_postback()?;
        for w in self.store().store().workspaces().all()? {
            let hid = flowback::cleanup(self.store(), &w.id)?;
            if !hid.is_empty() {
                tracing::info!(ws = w.id, n = hid.len(), "flow-back folded into item cards");
            }
        }
        Ok(())
    }

    /// Listener-ready phase. These runs use the existing read-only researcher, never the router:
    /// recovering an answer therefore cannot create a new item or replay an external message.
    /// Reuse the durable run id so another restart can recover the same hand-off.
    pub fn thread_ready(&self) -> Result<ThreadReady> {
        let mut out = ThreadReady::default();
        if self.config().no_resume || self.store().store().maintenance() {
            return Ok(out);
        }
        /* interrupted answers first: messages that waited on them are routed after, as before the restart */
        let th = self.store().store().thread();
        for (r, attempts) in th.runs_to_resume(now() - crate::orchestrator::RESUME_MAX_AGE_MS)? {
            // one run's trouble never stops the others, nor the Hub: its intent stays for later
            match self.resume_run(&r, attempts) {
                Ok(true) => out.resumed += 1,
                Ok(false) => out.held += 1,
                Err(e) => {
                    tracing::error!(run = %r.id, "main conversation resume: {e}");
                    out.failed += 1;
                }
            }
        }
        let pending: Vec<(String, String, String)> = self.store().store().reader::<_, steve_store::StoreError>(|c| {
            let mut q = c.prepare("SELECT workspace_id,key,value FROM settings WHERE key >= 'thread.pending.' AND key < 'thread.pending/' ORDER BY updated")?;
            Ok(q.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?.collect::<rusqlite::Result<_>>()?)
        })?;
        for (ws, key, data) in pending {
            match serde_json::from_str::<Job>(&data) {
                Ok(job) => {
                    let matches_message = self
                        .store()
                        .message(&job.mid)?
                        .is_some_and(|m| m.workspace_id == ws && m.space == job.conv.space);
                    if key != format!("thread.pending.{}", job.mid) || !matches_message {
                        // Keep the original record for diagnosis; never route a different message.
                        tracing::error!(workspace = %ws, pending_key = %key, "persisted thread job identity mismatch");
                        out.failed += 1;
                        continue;
                    }
                    self.enqueue(&ws, job);
                }
                Err(e) => {
                    tracing::error!("invalid persisted thread job: {e}");
                    out.failed += 1;
                }
            }
        }
        Ok(out)
    }

    /// Re-answers one interrupted local run (`attempts` already made): `false` when it is handed
    /// back to an explicit rerun instead (already re-answered once, or no longer local).
    fn resume_run(&self, r: &steve_store::ThreadRun, attempts: i64) -> Result<bool> {
        let th = self.store().store().thread();
        let Some(m) = self.store().message(&r.message_id)? else {
            th.resume_drop(&r.id)?;
            return Ok(false);
        };
        if m.space != MAIN {
            th.resume_drop(&r.id)?;
            return Ok(false);
        }
        if attempts >= MAX_RESUME_ATTEMPTS {
            // it was re-answered and the Hub stopped again: no loop, a rerun for you
            th.resume_drop(&r.id)?;
            self.store().message_add_in(
                &r.workspace_id,
                MAIN,
                "steve",
                "（Hub 又一次在回答时重启，不再自动继续，需要时点“重新回答”）",
                None,
                &json!({"rerun": r.message_id, "origin": r.message_id, "run": r.id}),
            )?;
            return Ok(false);
        }
        // counted before it starts: a crash during it counts too
        th.resume_attempt(&r.id)?;
        let files = m
            .meta
            .get("files")
            .cloned()
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default();
        let job = research::InlineJob {
            kind: if th.route_of(&m.id)?.is_some_and(|x| x.kind == "answer") {
                research::Inline::Answer
            } else {
                research::Inline::Research
            },
            origin: m.id.clone(),
            question: m.text,
            files,
            earlier: vec![],
            user: r.user_id.clone(),
            project: r.project_id.clone(),
            route: None,
            offer: None,
            reply_to: r.reply_id.clone(),
            run: Some(r.id.clone()),
            stop: Stop::default(),
            t0: now(),
            resume: None,
            space: m.space,
        };
        if !self.enqueue_inline(&r.workspace_id, job)? {
            // Still interrupted in the database; next ready/restart can retry.
            tracing::warn!(run = %r.id, "recovery queue full; durable intent retained");
        }
        Ok(true)
    }

    /// Items whose delivered explanation goes back into the conversation once, at the first boot of
    /// this build (thread-routing “决定” 6: ST-20 answered a question, but only inside the item).
    const POSTBACK: [&'static str; 1] = ["ST-20"];

    /// Posts each [`Self::POSTBACK`] item's longest delivery summary into the conversation, quoting the
    /// message it came from. Once per item (settings `thread.postback.<id>`); items not made from the
    /// conversation, or without a delivery, are left alone. Returns the messages posted.
    pub fn thread_postback(&self) -> Result<Vec<String>> {
        self.postback(&Self::POSTBACK)
    }

    pub(crate) fn postback(&self, items: &[&str]) -> Result<Vec<String>> {
        let db = self.store();
        let mut posted = vec![];
        for &id in items {
            let Some(item) = db.item(id)? else { continue };
            let ws = item.workspace_id.clone();
            let key = format!("thread.postback.{id}");
            if !db.setting(&ws, &key, "").unwrap_or_default().is_empty() {
                continue;
            }
            if item.source != ("对话".to_string(), "chat".to_string()) {
                continue;
            }
            let Some(draft) = db
                .decisions_of_item(id)?
                .into_iter()
                .filter(|d| d.kind == "accept")
                .map(|d| d.draft)
                .filter(|t| !t.trim().is_empty())
                .max_by_key(|t| t.chars().count())
            else {
                continue;
            };
            let msgs = db.messages_in(&ws, "steve", 100_000)?;
            let Some(announce) = msgs
                .iter()
                .find(|m| m.author == "steve" && m.item_id.as_deref() == Some(id))
            else {
                continue;
            };
            let origin = msgs
                .iter()
                .rfind(|m| m.author == "me" && m.created <= announce.created)
                .map(|m| m.id.clone())
                .unwrap_or_else(|| announce.id.clone());
            let mid = db.message_add(
                &ws,
                "steve",
                draft.trim(),
                Some(id),
                Some(&json!({
                    "update": {"kind": "deliver", "item": id, "text": draft.trim(), "full": true, "at": now()},
                    "reply_to": origin,
                    "postback": true,
                })),
            )?;
            let mut set = Map::new();
            set.insert(key, json!(mid));
            db.settings_set(&ws, &set, "steve")?;
            tracing::info!(
                item = id,
                message = mid,
                "posted the delivery back into the conversation"
            );
            posted.push(mid);
        }
        Ok(posted)
    }

    /// `/汇报`: open decisions, running items, items finished today (local midnight).
    pub fn digest_text(&self, ws: &str) -> Result<String> {
        let all = self.store().items(ws)?;
        let open = self.store().decisions_open(ws)?;
        let waiting: HashSet<Option<String>> = open.into_iter().map(|d| d.item_id).collect();
        let today = Local::now()
            .date_naive()
            .and_hms_opt(0, 0, 0)
            .and_then(|t| Local.from_local_datetime(&t).earliest())
            .map(|t| t.timestamp_millis())
            .unwrap_or(0);
        let run = all.iter().filter(|i| i.status == "run").count();
        let done = all
            .iter()
            .filter(|i| i.status == "done" && i.updated >= today)
            .count();
        Ok(format!(
            "现在：{} 件等你，{run} 件在进行，今天完成 {done} 件。",
            waiting.len()
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::names_bot;

    #[test]
    fn at_names_a_bot_with_or_without_spaces() {
        assert!(names_bot("Steve 开发", "Steve开发"));
        assert!(names_bot("Steve 开发", "Steve开发Bot"));
        assert!(names_bot("研究", "研究"));
        assert!(names_bot("研究", "研究Bot"));
        assert!(!names_bot("Steve 开发", "Steve"));
        assert!(!names_bot("研究", "研究员"));
    }
}
