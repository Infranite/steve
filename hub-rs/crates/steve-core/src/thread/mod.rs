//! Thread runtime: durable per-space serial reply tasks (`thread_reply_tasks`), immutable reply
//! receipts (`thread_reply_receipts`), durable submit idempotency (`thread_submit_keys`),
//! origin-scoped item dispatch (`thread_dispatch_refs`), and bounded global concurrency.

mod a2ui;
pub mod decide;
#[cfg(test)]
mod eval;
pub mod flowback;
mod handle;
pub mod handoff;
mod offer;
pub mod onboard;
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
use crate::error::{CoreError, Result};
use crate::gateway::{self, FocusItem, Route, RouteCtx, Said};
use crate::repo::{FileRef, Message};
use crate::util::{js_head, now};
use regex::Regex;
use serde_json::{Map, Value, json};
use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, LazyLock, Mutex, OnceLock};
use steve_modules::contacts::item_people::{self, Mention};
use steve_store::{
    EditQueuedOutcome, StopTaskOutcome, SubmitInput, SubmitOutcome, WithdrawOutcome, thread,
};
use tokio::sync::{Notify, Semaphore};
use tokio_util::sync::CancellationToken;

static AT: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"^@(\S+)\s*").expect("valid regex"));

/// Maximum number of spaces executing thread tasks concurrently across the Hub.
pub const MAX_CONCURRENT_SPACES: usize = 4;

/// The web conversation's `messages.space`.
pub const MAIN: &str = thread::MAIN_SPACE;

/// How often a chat answer being written goes out (`steve.draft`), in ms.
const DRAFT_EVERY: i64 = 50;

/// What `thread_ready` woke on startup.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct ThreadReady {
    pub resumed: usize,
    pub held: usize,
    pub failed: usize,
}

/// Whether “@`at`” names the bot called `name`: “名字” or “名字Bot”, spaces ignored.
fn names_bot(name: &str, at: &str) -> bool {
    let squash = |s: &str| s.split_whitespace().collect::<String>();
    let n = squash(name);
    let a = squash(at);
    a == n || a == n.clone() + "Bot"
}

/// A message as the router reads it: attachments are noted; the bot gets the files themselves.
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

/// A conversation: the web's (`steve`) or a guest one — an IM space's (`chan:<space>`).
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Conv {
    pub space: String,
    /// read-only research may answer here
    pub research: bool,
    /// a turn may become an item here
    pub items: bool,
    /// only the workspace's owner reads it (their private chat with Steve)
    pub owner_only: bool,
    /// the person speaking may start work: the workspace's owner
    pub start: bool,
    /// tool-needing requests preflight directly to an item where `items` allows one
    #[serde(default)]
    pub hands_on: bool,
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
            hands_on: false,
        }
    }
    /// A conversation whose turns are not the user's own (an IM space).
    pub fn guest(&self) -> bool {
        self.space != MAIN
    }
}

/// What owns guest conversations (the channel module, through the hub).
#[async_trait::async_trait]
pub trait ConvHost: Send + Sync {
    async fn answer(
        &self,
        ws: &str,
        space: &str,
        origin: &str,
    ) -> std::result::Result<String, steve_modules::gateway::GatewayError>;

    fn research_context(&self, ws: &str, space: &str, origin: &str) -> String;

    async fn research_answer(
        &self,
        _ws: &str,
        _space: &str,
        _origin: &str,
        text: &str,
    ) -> std::result::Result<String, String> {
        Ok(text.to_string())
    }

    fn said(&self, ws: &str, space: &str, id: &str);
}

/// Where a message goes besides the router: straight to an item, or quoting a message.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct SayTo {
    pub item: Option<String>,
    pub reply_to: Option<String>,
}

/// `meta.steps` on a Steve reply.
#[derive(Default)]
pub(crate) struct Steps {
    pub(crate) route: Option<Value>,
    pub(crate) answer: Option<Value>,
    pub(crate) research: Option<Value>,
    pub(crate) tools: Vec<Value>,
}

impl Steps {
    pub(crate) fn json(&self, ms: i64) -> Value {
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

    pub(crate) fn begin(steps: &Mutex<Steps>, name: &str, args: Value) -> (usize, i64) {
        let mut s = lock(steps);
        s.tools
            .push(json!({"name": name, "args": args, "status": "ok", "ms": 0}));
        (s.tools.len() - 1, now())
    }

    pub(crate) fn end<T>(
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
pub(crate) fn route_step(r: &Route, d: Option<&decide::Decision>, ms: i64) -> Value {
    json!({
        "kind": d.map(|d| d.kind.as_str()).unwrap_or(r.kind.as_str()),
        "modelKind": r.kind,
        "confidence": r.confidence,
        "signals": r.signals,
        "guard": d.and_then(|d| d.guard),
        "title": r.title, "project": r.project, "tags": r.tag_ids,
        "newTags": r.new_tags.iter().map(|t| &t.name).collect::<Vec<_>>(),
        "why": r.why, "model": r.model, "fallback": r.fallback, "ms": ms
    })
}

/// In-memory cancellation signal for a running task (paired with durable `cancel_requested_at`).
#[derive(Clone, Default)]
pub(crate) struct Stop {
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
    pub(crate) async fn stopped(&self) {
        self.token.cancelled().await
    }
    pub(crate) fn flag(&self) -> Arc<AtomicBool> {
        self.flag.clone()
    }
}

/// A conversation message forwarded to an item (stored durably in `thread_forwarded`).
pub(crate) struct Forwarded {
    pub(crate) mid: String,
    pub(crate) asked: bool,
}

/// Ephemeral execution runtime; all durable state lives in `steve-store` (`thread_*` tables).
pub(crate) struct ThreadRuntime {
    workers: Mutex<HashMap<(String, String), Arc<Notify>>>,
    budget: Arc<Semaphore>,
    stops: Mutex<HashMap<String, Stop>>,
    running_phase: Mutex<HashMap<(String, String), &'static str>>,
    researcher: OnceLock<Arc<dyn Researcher>>,
    host: OnceLock<Arc<dyn ConvHost>>,
    acting: Mutex<()>,
}

impl Default for ThreadRuntime {
    fn default() -> Self {
        Self {
            workers: Mutex::default(),
            budget: Arc::new(Semaphore::new(MAX_CONCURRENT_SPACES)),
            stops: Mutex::default(),
            running_phase: Mutex::default(),
            researcher: OnceLock::new(),
            host: OnceLock::new(),
            acting: Mutex::new(()),
        }
    }
}

pub(crate) fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// A message hidden from conversation views (folded flow-back, withdrawn tombstone).
pub fn hidden(m: &Message) -> bool {
    m.meta.get("hidden").is_some_and(gateway::truthy)
}

pub(crate) fn meta_of(m: &Message) -> Map<String, Value> {
    m.meta.as_object().cloned().unwrap_or_default()
}

fn submit_digest(text: &str, to: &SayTo, files: &[FileRef], mentions: &[Mention]) -> String {
    let file_ids: Vec<&str> = files.iter().map(|f| f.id.as_str()).collect();
    let payload = json!({
        "text": text,
        "item": to.item,
        "reply_to": to.reply_to,
        "files": file_ids,
        "mentions": mentions,
    });
    steve_store::sha(&payload.to_string())
}

impl Core {
    /// Post the user's message and enqueue a durable reply task for its space.
    pub fn say(&self, ws: &str, text: &str, files: &[FileRef]) -> Result<String> {
        self.say_as(ws, None, text, files, &[])
    }

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

    pub fn say_to(
        &self,
        ws: &str,
        user: Option<&str>,
        text: &str,
        files: &[FileRef],
        mentions: &[Mention],
        to: SayTo,
    ) -> Result<String> {
        self.say_with_key(ws, user, text, files, mentions, to, Map::new(), None)
    }

    /// [`Core::say_to`] with an explicit durable idempotency key (`client_msg_id`).
    #[allow(clippy::too_many_arguments)]
    pub fn say_to_with_key(
        &self,
        ws: &str,
        user: Option<&str>,
        text: &str,
        files: &[FileRef],
        mentions: &[Mention],
        to: SayTo,
        client_key: Option<&str>,
    ) -> Result<String> {
        self.say_with_key(ws, user, text, files, mentions, to, Map::new(), client_key)
    }

    /// A turn in conversation `conv` (guest / channel space).
    pub fn say_in(&self, ws: &str, conv: Conv, text: &str, meta: &Value) -> Result<String> {
        self.say_in_as(ws, conv, text, meta, None)
    }

    pub fn say_in_as(
        &self,
        ws: &str,
        conv: Conv,
        text: &str,
        meta: &Value,
        user: Option<&str>,
    ) -> Result<String> {
        if text.trim().is_empty() {
            return Err(CoreError::Status(
                400,
                steve_i18n::t!("core.error.invalid", fields = "text"),
            ));
        }
        let provider_key = meta
            .get("chan")
            .and_then(|c| c.get("message"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(|id| format!("chan:{id}"));
        let submit_key = provider_key.unwrap_or_else(|| steve_store::uid("sub"));
        let to = SayTo::default();
        let digest = submit_digest(text, &to, &[], &[]);
        let conv_json = serde_json::to_string(&conv)?;
        let actor_key = user.unwrap_or("");
        let outcome = self.store().store().write(|w| {
            thread::submit_message(
                w,
                &SubmitInput {
                    ws,
                    space: &conv.space,
                    actor_key,
                    submit_key: &submit_key,
                    payload_digest: &digest,
                    text,
                    item_id: None,
                    meta,
                    user_id: user,
                    project_id: None,
                    conv_json: Some(&conv_json),
                    to_item: None,
                    to_reply_to: None,
                    file_ids: &[],
                    files_json: None,
                    mentions_json: None,
                    enqueue_task: true,
                    validate_refs: true,
                },
            )
        })?;
        match outcome {
            SubmitOutcome::Accepted { message_id, .. } => {
                self.wake_space(ws, &conv.space);
                Ok(message_id)
            }
            SubmitOutcome::Duplicate { message_id, .. } => Ok(message_id),
            SubmitOutcome::Conflict { reason } => Err(conflict_err(reason)),
        }
    }

    #[allow(clippy::too_many_arguments)]
    pub(crate) fn say_with_key(
        &self,
        ws: &str,
        user: Option<&str>,
        text: &str,
        files: &[FileRef],
        mentions: &[Mention],
        to: SayTo,
        mut meta: Map<String, Value>,
        client_key: Option<&str>,
    ) -> Result<String> {
        if text.trim().is_empty() {
            return Err(CoreError::Status(
                400,
                steve_i18n::t!("core.error.invalid", fields = "text"),
            ));
        }
        if !files.is_empty() {
            meta.insert("files".into(), json!(files));
        }
        if let Some(r) = &to.reply_to {
            meta.insert("reply_to".into(), json!(r));
        }
        if let Some(i) = &to.item {
            meta.insert("to".into(), json!(i));
        }
        let meta_val = if meta.is_empty() {
            Value::Null
        } else {
            Value::Object(meta)
        };
        let submit_key = client_key
            .filter(|s| !s.is_empty())
            .map(String::from)
            .unwrap_or_else(|| steve_store::uid("sub"));
        let digest = submit_digest(text, &to, files, mentions);
        let conv = Conv::main();
        let conv_json = serde_json::to_string(&conv)?;
        let file_ids: Vec<String> = files.iter().map(|f| f.id.clone()).collect();
        let files_json = (!files.is_empty())
            .then(|| serde_json::to_string(files))
            .transpose()?;
        let mentions_json = (!mentions.is_empty())
            .then(|| serde_json::to_string(mentions))
            .transpose()?;
        let actor_key = user.unwrap_or("");
        let outcome = self.store().store().write(|w| {
            thread::submit_message(
                w,
                &SubmitInput {
                    ws,
                    space: MAIN,
                    actor_key,
                    submit_key: &submit_key,
                    payload_digest: &digest,
                    text,
                    item_id: None,
                    meta: &meta_val,
                    user_id: user,
                    project_id: None,
                    conv_json: Some(&conv_json),
                    to_item: to.item.as_deref(),
                    to_reply_to: to.reply_to.as_deref(),
                    file_ids: &file_ids,
                    files_json: files_json.as_deref(),
                    mentions_json: mentions_json.as_deref(),
                    enqueue_task: true,
                    validate_refs: true,
                },
            )
        })?;
        match outcome {
            SubmitOutcome::Accepted { message_id, .. } => {
                self.wake_space(ws, MAIN);
                Ok(message_id)
            }
            SubmitOutcome::Duplicate { message_id, .. } => Ok(message_id),
            SubmitOutcome::Conflict { reason } => Err(conflict_err(reason)),
        }
    }

    pub fn set_conv_host(&self, h: Arc<dyn ConvHost>) -> bool {
        self.0.thread.host.set(h).is_ok()
    }

    pub(crate) fn conv_host(&self) -> Option<Arc<dyn ConvHost>> {
        self.0.thread.host.get().cloned()
    }

    /// The conversation space of message `id`. Fails closed (empty string, never MAIN) when gone.
    pub(crate) fn conv_of(&self, id: &str) -> String {
        self.store()
            .message(id)
            .ok()
            .flatten()
            .map(|m| m.space)
            .unwrap_or_default()
    }

    /// The last 200 messages of conversation `space`, oldest first.
    pub(crate) fn conv_messages(&self, ws: &str, space: &str) -> Result<Vec<Message>> {
        if space.is_empty() {
            return Ok(vec![]);
        }
        self.store().messages_in(ws, space, 200)
    }

    /// Steve says `text` in conversation `space`; its host hears of guest or channel-sourced replies.
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

    /// Tells the host about a guest reply, or a main reply whose origin came from a channel.
    pub(crate) fn said(&self, ws: &str, space: &str, id: &str) {
        let Some(h) = self.conv_host() else {
            return;
        };
        if space == MAIN {
            let sourced = || -> Result<bool> {
                let Some(m) = self.store().message(id)? else {
                    return Ok(false);
                };
                let Some(origin) = m.meta["origin"].as_str() else {
                    return Ok(false);
                };
                Ok(m.workspace_id == ws
                    && m.space == MAIN
                    && self.store().message(origin)?.is_some_and(|o| {
                        o.workspace_id == ws && o.space == MAIN && o.meta["chan"].is_object()
                    }))
            };
            if !sourced().unwrap_or(false) {
                return;
            }
        }
        h.said(ws, space, id);
    }

    /// Whether the main conversation has any queued or claimed reply task.
    pub fn main_busy(&self, ws: &str) -> Result<bool> {
        Ok(self
            .store()
            .store()
            .read(|r| thread::has_active_or_queued(r, ws, MAIN))?)
    }

    /// Whether the main conversation has an active (claimed) reply task right now.
    pub fn thread_active(&self, ws: &str) -> bool {
        self.store()
            .store()
            .read(|r| thread::has_claimed(r, ws, MAIN))
            .unwrap_or(false)
    }

    #[cfg(test)]
    pub(crate) fn routing(&self, ws: &str) -> bool {
        lock(&self.0.thread.running_phase)
            .get(&(ws.to_string(), MAIN.to_string()))
            .is_some_and(|p| *p == "route")
    }

    pub(crate) fn researching(&self, ws: &str) -> bool {
        lock(&self.0.thread.running_phase)
            .get(&(ws.to_string(), MAIN.to_string()))
            .is_some_and(|p| *p == "research")
    }

    /// The bot a message's “@名字 …” names.
    pub(super) fn named_bot(&self, ws: &str, text: &str) -> Result<Option<crate::repo::Bot>> {
        let Some(a) = AT.captures(text).map(|c| c[1].to_string()) else {
            return Ok(None);
        };
        Ok(self
            .store()
            .bots(ws)?
            .into_iter()
            .find(|b| names_bot(&b.name, &a)))
    }

    /// Rewrites a main-conversation message still waiting (`status = 'queued'`).
    pub fn thread_queued_edit(&self, ws: &str, mid: &str, text: &str) -> Result<bool> {
        if text.trim().is_empty() {
            return Err(CoreError::Status(
                400,
                steve_i18n::t!("core.error.invalid", fields = "text"),
            ));
        }
        let Some(m) = self.store().message(mid)? else {
            return Ok(false);
        };
        if m.workspace_id != ws || m.space != MAIN {
            return Ok(false);
        }
        let files: Vec<FileRef> = m
            .meta
            .get("files")
            .cloned()
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default();
        let to = SayTo {
            item: m.meta.get("to").and_then(Value::as_str).map(String::from),
            reply_to: m
                .meta
                .get("reply_to")
                .and_then(Value::as_str)
                .map(String::from),
        };
        let new_digest = submit_digest(text, &to, &files, &[]);
        let outcome = self
            .store()
            .store()
            .write(|w| thread::edit_queued(w, ws, mid, text, &new_digest))?;
        Ok(outcome == EditQueuedOutcome::Updated)
    }

    /// Withdraws a main-conversation message still waiting (`status = 'queued'`), retaining its
    /// durable tombstone so retrying its `client_msg_id` never resurrects it.
    pub fn thread_queued_withdraw(
        &self,
        ws: &str,
        mid: &str,
    ) -> Result<Option<(String, Vec<FileRef>)>> {
        let Some(m) = self.store().message(mid)? else {
            return Ok(None);
        };
        if m.workspace_id != ws || m.space != MAIN {
            return Ok(None);
        }
        let files: Vec<FileRef> = m
            .meta
            .get("files")
            .cloned()
            .and_then(|v| serde_json::from_value(v).ok())
            .unwrap_or_default();
        let outcome = self
            .store()
            .store()
            .write(|w| thread::withdraw_queued(w, ws, mid))?;
        match outcome {
            WithdrawOutcome::Withdrawn { .. } => Ok(Some((m.text, files))),
            WithdrawOutcome::NotQueued => Ok(None),
        }
    }

    /// Wake (or start) the per-space serial worker for `(ws, space)`.
    pub(crate) fn wake_space(&self, ws: &str, space: &str) {
        if self.store().store().maintenance() {
            return;
        }
        let Ok(handle) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let key = (ws.to_string(), space.to_string());
        let mut workers = lock(&self.0.thread.workers);
        if let Some(notify) = workers.get(&key) {
            notify.notify_one();
            return;
        }
        let notify = Arc::new(Notify::new());
        workers.insert(key.clone(), notify.clone());
        let core = self.clone();
        let (w, s) = key;
        handle.spawn(async move {
            core.space_worker(w, s, notify).await;
        });
    }

    async fn space_worker(&self, ws: String, space: String, notify: Arc<Notify>) {
        let key = (ws.clone(), space.clone());
        loop {
            if self.store().store().maintenance() {
                lock(&self.0.thread.workers).remove(&key);
                return;
            }
            // Acquire one global concurrency budget slot before claiming head work.
            let Ok(_permit) = self.0.thread.budget.clone().acquire_owned().await else {
                lock(&self.0.thread.workers).remove(&key);
                return;
            };
            let lease = steve_store::uid("lease");
            let claimed = match self
                .store()
                .store()
                .write(|w| thread::claim_head(w, &ws, &space, &lease, now()))
            {
                Ok(task) => task,
                Err(e) => {
                    tracing::error!(workspace = %ws, space = %space, "thread claim_head: {e}");
                    lock(&self.0.thread.workers).remove(&key);
                    return;
                }
            };
            let Some(task) = claimed else {
                drop(_permit);
                // Re-check under workers lock so a concurrent wake_space is never lost.
                let mut workers = lock(&self.0.thread.workers);
                let still_queued = self
                    .store()
                    .store()
                    .read(|r| thread::has_active_or_queued(r, &ws, &space))
                    .unwrap_or(false);
                if !still_queued {
                    workers.remove(&key);
                    return;
                }
                drop(workers);
                // Another worker currently owns the head claimed row (or a new row arrived);
                // wait briefly for a wake notification before rechecking.
                let _ =
                    tokio::time::timeout(std::time::Duration::from_millis(25), notify.notified())
                        .await;
                continue;
            };

            let _activity = self.store().store().activity("thread_task").await;
            let stop = Stop::default();
            lock(&self.0.thread.stops).insert(task.id.clone(), stop.clone());
            let c = self.clone();
            let t_clone = task.clone();
            let l_clone = lease.clone();
            let s_clone = stop.clone();
            let run = tokio::spawn(async move {
                c.store()
                    .store()
                    .admitted(c.execute_task(&t_clone, &l_clone, &s_clone))
                    .await;
            });
            if let Err(e) = run.await {
                tracing::error!(task = %task.id, "thread task panicked: {e}");
                let _ = self.store().store().write(|w| {
                    thread::commit_reply(
                        w,
                        &steve_store::CommitReplyInput {
                            task_id: &task.id,
                            lease_token: &lease,
                            expected_generation: task.generation,
                            allow_cancelled_stop: true,
                            outcome: "failed",
                            reply_text: Some("（处理异常中断）"),
                            existing_reply_id: None,
                            reply_item_id: None,
                            reply_meta: &json!({
                                "origin": task.origin_message_id,
                                "error": true,
                                research::SYSTEM: true,
                            }),
                            error_code: Some("panic"),
                            route_kind: "unknown",
                            model_kind: "unknown",
                            route_guard: None,
                            route_model: None,
                            route_confidence: None,
                            route_signals: &[],
                            route_fallback: false,
                            route_ms: 0,
                            route_outcome: None,
                            item_id: None,
                            dispatch_ref_id: None,
                            delivery_ref_id: None,
                            model_call_id: None,
                            user_id: task.user_id.as_deref(),
                            project_id: task.project_id.as_deref(),
                            answer_model: None,
                            harness: None,
                            harness_session: None,
                            tools: 0,
                            turns: 0,
                            first_text_ms: None,
                            error_detail: Some(&e.to_string()),
                            record_attention_commit: task.space == MAIN && task.user_id.is_some(),
                        },
                    )
                });
            }
            lock(&self.0.thread.stops).remove(&task.id);
            lock(&self.0.thread.running_phase).remove(&key);
        }
    }

    /// Fence all active/queued tasks for `(ws, origin)` and trigger their in-memory stop signals.
    pub(crate) fn fence_origin(&self, ws: &str, origin: &str, reason: &str) -> Result<()> {
        let fenced = self
            .store()
            .store()
            .write(|w| thread::fence_origin_tasks(w, ws, origin, reason, now()))?;
        let stops = lock(&self.0.thread.stops);
        for id in fenced {
            if let Some(s) = stops.get(&id) {
                s.stop();
            }
        }
        Ok(())
    }

    /// Send a conversation message to an item; durable `thread_forwarded` remembers it until the turn ends.
    pub(crate) fn forward(
        &self,
        item: &str,
        text: &str,
        files: &[FileRef],
        mid: &str,
    ) -> Result<()> {
        self.supplement(item, text, crate::orchestrator::SupplementMode::Auto, files)?;
        let ws = self
            .store()
            .item(item)?
            .map(|i| i.workspace_id)
            .unwrap_or_else(|| crate::repo::WS.to_string());
        let asked = if decide::pure_question(text) {
            "1"
        } else {
            "0"
        };
        self.store()
            .store()
            .write(|w| thread::set_forwarded(w, item, &ws, mid, asked, now()))?;
        Ok(())
    }

    pub(crate) fn forward_open(
        &self,
        item: &str,
        text: &str,
        files: &[FileRef],
        mid: &str,
    ) -> Result<bool> {
        match self.forward(item, text, files, mid) {
            Ok(()) => Ok(true),
            Err(_) if self.store().item(item)?.is_some_and(|i| i.is_closed()) => Ok(false),
            Err(e) => Err(e),
        }
    }

    /// The forwarded message an item's turn answered, once.
    pub(crate) fn forwarded_take(&self, item: &str) -> Option<Forwarded> {
        self.store()
            .store()
            .write(|w| thread::take_forwarded(w, item))
            .ok()
            .flatten()
            .map(|(mid, asked)| Forwarded {
                mid,
                asked: asked == "1",
            })
    }

    pub fn set_researcher(&self, r: Arc<dyn Researcher>) -> bool {
        self.0.thread.researcher.set(r).is_ok()
    }

    pub fn route_ctx(&self, ws: &str) -> Result<RouteCtx> {
        self.route_ctx_in(ws, false)
    }

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
            closed_items: if guest {
                vec![]
            } else {
                items
                    .iter()
                    .filter(|i| i.is_closed())
                    .map(|i| i.id.clone())
                    .collect()
            },
            items: items.into_iter().map(|i| i.id).collect(),
            researching: self.researching(ws),
            pending: vec![],
            debug: self.config().debug_route,
            operator: !guest,
        })
    }

    fn route_ctx_for_team(&self, ws: &str, guest: bool, team: Option<&str>) -> Result<RouteCtx> {
        let mut c = self.route_ctx_in(ws, guest)?;
        let selected = self
            .store()
            .store()
            .read(|db| steve_store::bot_dispatch::selected(db, ws, team))?;
        if let Some(team) = &selected {
            c.bots.retain(|b| team.member(&b.id).is_some());
            c.bots
                .sort_by_key(|b| team.members.iter().position(|m| m.bot == b.id));
        }
        Ok(c)
    }

    fn focus(&self, ws: &str, items: &[crate::repo::Item]) -> Result<Vec<FocusItem>> {
        let since = now() - 24 * 3600 * 1000;
        let dispatches = self
            .store()
            .store()
            .read(|r| thread::dispatches_of_workspace(r, ws))?;
        let mut out: Vec<(i64, FocusItem)> = vec![];
        for t in dispatches {
            let Some(ref item_id) = t.target_item_id else {
                continue;
            };
            let Some(i) = items.iter().find(|i| &i.id == item_id) else {
                continue;
            };
            if i.is_closed() || i.updated < since {
                continue;
            }
            let origin = self
                .store()
                .message(&t.origin_message_id)?
                .map(|m| m.text)
                .unwrap_or_default();
            out.push((
                i.updated,
                FocusItem {
                    id: i.id.clone(),
                    title: i.title.clone(),
                    status: i.outcome.clone().unwrap_or_else(|| i.phase.as_str().into()),
                    origin,
                },
            ));
        }
        out.sort_by_key(|(t, _)| std::cmp::Reverse(*t));
        Ok(out.into_iter().take(3).map(|(_, f)| f).collect())
    }

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
        self.route_for_team(ws, text, history, promote, guest, None)
            .await
    }

    pub async fn route_for_team(
        &self,
        ws: &str,
        text: &str,
        history: &[Said],
        promote: bool,
        guest: bool,
        team: Option<&str>,
    ) -> Result<Route> {
        let c = self.route_ctx_for_team(ws, guest, team)?;
        let route = crate::stats::tagged(
            "route",
            ws,
            gateway::route(self.gw(), &c, text, history, promote),
        )
        .await;
        Ok(route)
    }

    pub fn gateway(&self) -> &dyn crate::ModelGateway {
        self.gw()
    }

    pub(crate) fn chat_files(&self, files: &[FileRef], earlier: &[FileRef]) -> gateway::ChatFiles {
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
                    out.readable.extend(a.readable);
                }
                Err(e) => out.note.push_str(&format!("\n（附件没能读取：{e}）")),
            }
        }
        out
    }

    pub(crate) fn earlier_files(
        &self,
        ws: &str,
        space: &str,
        mid: &str,
        files: &[FileRef],
        resume: Option<&research::ResumeFrom>,
    ) -> Result<Vec<FileRef>> {
        if !files.is_empty() {
            return Ok(vec![]);
        }
        let msgs = self.conv_messages(ws, space)?;
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
            && self
                .store()
                .store()
                .read(|rd| thread::receipt_by_id(rd, &r.run))?
                .is_some_and(|rcpt| rcpt.origin_message_id == m.id)
        {
            return Ok(vec![]);
        }
        Ok(serde_json::from_value(m.meta["files"].clone()).unwrap_or_default())
    }

    pub(crate) fn phase(&self, ws: &str, phase: &str) {
        self.store().store().publish(steve_types::LiveEvent::from(
            steve_types::WorkLiveEvent::SteveThinking {
                on: true,
                ws: ws.to_owned().into(),
                phase: Some(phase.into()),
            },
        ));
    }

    pub(crate) fn history_before(&self, ws: &str, mid: &str) -> Result<Vec<Said>> {
        let db = self.store();
        let Some(origin) = db.message(mid)? else {
            return Ok(vec![]);
        };
        let at0 = origin.created;
        Ok(self
            .conv_messages(ws, &origin.space)?
            .into_iter()
            .filter(|m| m.id != mid && m.created <= at0 && !hidden(m))
            .map(|m| {
                let offer = m
                    .meta
                    .get("offer")
                    .filter(|o| o["state"] == "open")
                    .map(|o| format!("\n（提议：{}）", gateway::jstr(&o["goal"])))
                    .unwrap_or_default();
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

    pub(crate) fn item_people(
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

    /// Atomically request cancellation of the active main-conversation task (idempotent).
    /// Waiting queued tasks remain queued and run next.
    pub fn stop(&self, ws: &str) -> Result<bool> {
        let Some(task_id) = self
            .store()
            .store()
            .write(|w| thread::request_stop_space(w, ws, MAIN, "user", now()))?
        else {
            return Ok(false);
        };
        if let Some(s) = lock(&self.0.thread.stops).get(&task_id) {
            s.stop();
        }
        Ok(true)
    }

    /// Stop a specific task by id: cancels an active claimed task or settles a queued task directly
    /// with 0 execution (idempotent).
    pub fn stop_run(&self, ws: &str, task_id: &str) -> Result<bool> {
        let outcome = self.store().store().write(|w| {
            thread::request_stop_task(
                w,
                ws,
                task_id,
                "user",
                "好，停下了。要我换个方式就直接说。",
                now(),
            )
        })?;
        match outcome {
            StopTaskOutcome::CancelClaimed { task_id, .. } => {
                if let Some(s) = lock(&self.0.thread.stops).get(&task_id) {
                    s.stop();
                }
                Ok(true)
            }
            StopTaskOutcome::StoppedQueued {
                reply_message_id,
                space,
                ..
            } => {
                self.said(ws, &space, &reply_message_id);
                Ok(true)
            }
            StopTaskOutcome::AlreadyStopped => Ok(false),
        }
    }

    /// At boot: settle any crashed `claimed` tasks to an explicit `unknown` receipt (never
    /// auto-replaying side effects or calling `host.said`). Proven `queued` tasks stay queued.
    pub fn thread_recover(&self) -> Result<()> {
        self.store().store().write(|w| {
            thread::recover_claimed_tasks(
                w,
                "（Hub 重启，本次处理状态未知，未自动重跑，需要时请显式重试）",
                now(),
            )
        })?;
        Ok(())
    }

    /// Listener-ready phase: wake per-space workers for any proven `queued` tasks.
    pub fn thread_ready(&self) -> Result<ThreadReady> {
        let mut out = ThreadReady::default();
        if self.config().no_resume || self.store().store().maintenance() {
            return Ok(out);
        }
        let spaces = self
            .store()
            .store()
            .read(|r| thread::list_queued_spaces(r))?;
        for (ws, space) in spaces {
            self.wake_space(&ws, &space);
            out.resumed += 1;
        }
        Ok(out)
    }

    /// `/汇报`: open decisions, running items, items finished today.
    pub fn digest_text(&self, ws: &str) -> Result<String> {
        let all = self.store().items(ws)?;
        let open = self.store().decisions_open(ws)?;
        let waiting: HashSet<Option<String>> = open.into_iter().map(|d| d.item_id).collect();
        let zone = self
            .store()
            .store()
            .read(|r| steve_modules::zone::Zone::for_owner(r, ws))?;
        let today = zone.day_start(steve_store::now()).unwrap_or(0);
        let run = all
            .iter()
            .filter(|i| i.phase == crate::cycle::ACTIVE)
            .count();
        let done = all
            .iter()
            .filter(|i| i.is_completed() && i.updated >= today)
            .count();
        Ok(format!(
            "现在：{} 件等你，{run} 件在进行，今天完成 {done} 件。",
            waiting.len()
        ))
    }
}

fn conflict_err(reason: &'static str) -> CoreError {
    match reason {
        "withdrawn" => CoreError::Status(409, "message was withdrawn".into()),
        "payload_mismatch" => {
            CoreError::Status(409, "client_id reused with a different payload".into())
        }
        "invalid_file" | "invalid_item" | "invalid_reply_to" => {
            CoreError::Status(404, steve_i18n::t!("core.message.not_found"))
        }
        _ => CoreError::Status(409, reason.into()),
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

    #[test]
    fn routing_context_contains_only_visible_team_bots() {
        let k = crate::testkit::Kit::new();
        let ws = crate::repo::WS;
        steve_store::core_fixture::thread::default_team(k.db().store(), ws, "dev");
        let context = k.core.route_ctx_for_team(ws, false, None).unwrap();
        assert_eq!(context.bots.len(), 1);
        assert_eq!(context.bots[0].id, "dev");
    }
}
