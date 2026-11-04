//! The main conversation with Steve (thread.ts), one per workspace: every message goes through the router.
//!
//! Thread routing (modules/thread-routing.md): the conversation is the default. A message is routed on the
//! workspace lane (a few seconds, one at a time, in order); [`decide`] puts the code's guards over the
//! model's choice; answers and read-only research then run on the workspace's inline queue
//! ([`research`]), so the lane is free for the next message. Items are made only for real work, a
//! suggestion card asks when it is unclear, and the user can correct the routing both ways ([`reroute`]).

pub mod decide;
#[cfg(test)]
mod eval;
pub mod flowback;
mod handle;
mod offer;
mod reroute;
pub mod research;
mod suggest;

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

/// A conversation: the web's (`steve`) or a guest one — an IM space's (`chan:<space>`, channels
/// §2.12), whose turns come from people in that space. A guest conversation is routed like the web's,
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
    fn research_context(&self, ws: &str, space: &str) -> String;
    /// Steve stored message `id` in guest conversation `space`.
    fn said(&self, ws: &str, space: &str, id: &str);
}

/// Where a message goes besides the router (§5.3): straight to an item (the focus bar “正在跟 ST-n 说”), or
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

/// Per-workspace lanes, the reply being prepared, inline queues and promotions in flight.
#[derive(Default)]
pub(crate) struct Lanes {
    lanes: Mutex<HashMap<String, mpsc::UnboundedSender<Job>>>,
    admitted: Mutex<HashSet<String>>,
    inflight: Mutex<HashMap<String, Arc<AtomicBool>>>,
    promoting: Mutex<HashMap<String, Promotion>>,
    /// answers and research runs, one at a time per workspace (§4.6)
    inline: Mutex<HashMap<String, research::InlineLane>>,
    /// the read-only research executor (R2); without one, research falls back to a direct answer
    researcher: OnceLock<Arc<dyn Researcher>>,
    /// item → the conversation message last forwarded to it, until its turn ends (§5.3)
    forwarded: Mutex<HashMap<String, String>>,
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
        let mut meta = Map::new();
        if !files.is_empty() {
            meta.insert("files".into(), json!(files));
        }
        if let Some(r) = &to.reply_to {
            meta.insert("reply_to".into(), json!(r));
        }
        if let Some(i) = &to.item {
            meta.insert("to".into(), json!(i));
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
            },
        );
        Ok(mid)
    }

    /// Queues a stored message on its workspace's routing lane.
    fn enqueue(&self, ws: &str, job: Job) {
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
        let mut lanes = lock(&self.0.thread.lanes);
        let tx = lanes.entry(ws.to_string()).or_insert_with(|| {
            let (tx, mut rx) = mpsc::unbounded_channel::<Job>();
            let core = self.clone();
            let w = ws.to_string();
            tokio::spawn(async move {
                while let Some(j) = rx.recv().await {
                    let _activity = core.store().store().activity("thread_router").await;
                    let key = format!("thread.pending.{}", j.mid);
                    let removed = core.store().store().with_conn(|c| {
                        c.execute(
                            "DELETE FROM settings WHERE workspace_id=? AND key=?",
                            rusqlite::params![w, key],
                        )?;
                        Ok(())
                    });
                    if let Err(e) = removed {
                        tracing::error!("thread queue claim: {e}");
                        continue;
                    }
                    core.store().store().admitted(core.handle(&w, &j)).await;
                    lock(&core.0.thread.admitted).remove(&j.mid);
                }
            });
            tx
        });
        if tx.send(job).is_err() {
            tracing::error!("thread lane of {ws} is gone");
        }
    }

    /// A message the paste guard caught (V4 §4.6): stored with the secrets replaced, answered with
    /// the fixed reply (linking the vault), and not routed: no model sees it (`guarded`, so a
    /// restart does not take it for a message the routing lane lost).
    pub fn say_guarded(&self, ws: &str, stored: &str, reply: &str, href: &str) -> Result<String> {
        let mid =
            self.store()
                .message_add(ws, "me", stored, None, Some(&json!({"guarded": true})))?;
        self.store()
            .message_add(ws, "steve", reply, None, Some(&json!({"href": href})))?;
        Ok(mid)
    }

    /// Send a conversation message to an item (§5.3): its reply flows back when that turn ends.
    pub(crate) fn forward(
        &self,
        item: &str,
        text: &str,
        files: &[FileRef],
        mid: &str,
    ) -> Result<()> {
        self.supplement(item, text, crate::orchestrator::SupplementMode::Auto, files)?;
        lock(&self.0.thread.forwarded).insert(item.into(), mid.into());
        Ok(())
    }

    /// The forwarded message an item's turn answered, once.
    pub(crate) fn forwarded_take(&self, item: &str) -> Option<String> {
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
        let repo = self.config().steve_repo.to_string_lossy().to_string();
        let items = db.items(ws)?;
        Ok(RouteCtx {
            bots: db.bots(ws)?.into_iter().filter(|b| b.enabled).collect(),
            steve_project: projects
                .iter()
                .find(|p| p.path == repo)
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
                    out.readable |= fs.len() > a.images.len();
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

    /// The people of a new item from its message (contacts.md §2.8): `party` mentions are linked and
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
    /// being written (its partial text is kept). Queued questions stay.
    pub fn stop(&self, ws: &str) -> Result<bool> {
        let routing = lock(&self.0.thread.inflight).get(ws).cloned();
        let inline = self.stop_inline(ws);
        if let Some(f) = &routing {
            f.store(true, Ordering::SeqCst);
        }
        if routing.is_none() && !inline {
            return Ok(false);
        }
        if routing.is_some() && !inline {
            self.store().message_add(
                ws,
                "steve",
                "好，停下了。要我换个方式就直接说。",
                None,
                None,
            )?;
        }
        Ok(true)
    }

    /// At boot: interrupt unfinished inline work and persist safe local resume intents.
    /// External guest replies retain explicit rerun; local answers wait for `thread_ready`.
    /// Also performs the one-time `thread_items` backfill.
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
            let pending: Option<String> = self.store().store().with_conn(|c| {
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
            let linked = th.backfill(&w.id)?;
            if !linked.is_empty() {
                tracing::info!(ws = w.id, ?linked, "thread items backfilled");
            }
        }
        /* after the backfill, so backfilled items get their card too */
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
        let pending: Vec<(String, String, String)> = self.store().store().with_conn(|c| {
            let mut q = c.prepare("SELECT workspace_id,key,value FROM settings WHERE key LIKE 'thread.pending.%' ORDER BY updated")?;
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
            stop: Arc::new(AtomicBool::new(false)),
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
