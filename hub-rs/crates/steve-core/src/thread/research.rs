//! Answers and inline research on the workspace's inline queue, and the seam to the
//! research executor (R2, `Researcher`): this module decides when a run starts, records it in
//! `thread_runs`, streams it into the conversation and stores the answer; the executor only reads.
//!
//! One answer or research run at a time per workspace; while one runs, up to [`MAX_WAITING`] more wait
//! (the user's message shows “排队中”), the next is refused with a sentence.

use super::{DRAFT_EVERY, Steps, lane_key, lock, meta_of};
use crate::Core;
use crate::error::Result;
use crate::gateway::{self, Offer, Said};
use crate::repo::{Bot, FileRef, HubEvent, Project};
use crate::util::{js_head, now};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use steve_store::RunPatch;
use tokio::sync::mpsc;

/// How many answers or research runs may wait behind the running one.
pub const MAX_WAITING: usize = 3;
/// The reply when the queue is full.
pub const QUEUE_FULL: &str = "前面还有 3 个问题在排队，先等等或停掉当前的";

/// One tool call of a research run, as the executor saw it.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolSeen {
    /// the harness tool name (`Read`, `Grep`, `mcp__steve__steve_knowledge_search`, …)
    pub name: String,
    /// what it was about: a relative path with lines, a pattern, a query, an item id
    pub brief: String,
    pub ok: bool,
    /// Stable diagnostic code; never a raw tool error or result.
    pub error: Option<String>,
    pub ms: i64,
}

/// How a run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RunEnd {
    #[default]
    Done,
    /// the user stopped it
    Stopped,
    /// time, turns or tool calls ran out
    Budget,
    /// a tool check stopped it; this does not prove that protected data was read
    Violated,
    Failed,
}

impl RunEnd {
    /// The `thread_runs.status` and `meta.steps.research.stopped` value.
    pub fn status(self) -> &'static str {
        match self {
            RunEnd::Done => "done",
            RunEnd::Stopped => "stopped",
            RunEnd::Budget => "budget",
            RunEnd::Violated => "violated",
            RunEnd::Failed => "failed",
        }
    }
    /// The note after a partial answer.
    fn note(self) -> Option<&'static str> {
        match self {
            RunEnd::Done | RunEnd::Failed => None,
            RunEnd::Stopped => Some("（已停下）"),
            RunEnd::Budget => Some("（没读完就停了）"),
            RunEnd::Violated => Some("（本次查询在工具检查时停止，尚未完成。）"),
        }
    }
}

/// Everything a research run needs; collected by the thread, so the executor touches no state.
#[derive(Clone)]
pub struct ResearchRequest {
    /// the `thread_runs` row (status `running` when the executor is called)
    pub run: String,
    pub ws: String,
    /// the user message being answered
    pub mid: String,
    pub user: Option<String>,
    pub question: String,
    /// the conversation before the question, oldest first
    pub history: Vec<Said>,
    /// the main bot: name, persona and rules of the voice that answers
    pub main: Bot,
    /// module context for the question (knowledge snippets, B1 `PromptContext::chat`); may be empty
    pub context: String,
    /// the project to work in (cwd), if the router named one or the question is about Steve
    pub project: Option<Project>,
    /// every project directory of the workspace that may be read
    pub readable: Vec<Project>,
    /// the question's attachments
    pub files: Vec<FileRef>,
    /// attachments of the message the question follows up on (“这样你也看不出来吗？” after a screenshot)
    pub earlier_files: Vec<FileRef>,
    /// the previous round on the same topic, to `--resume`
    pub resume: Option<ResumeFrom>,
    /// set when the user stops the run; the executor ends the process and returns what it has
    pub stop: Arc<AtomicBool>,
    /// Steve's `/mcp` with this run's own token: only the read-only research tools answer it
    pub mcp: Option<crate::McpEndpoint>,
    /// 设置 · 模型 · 就地研究: the models to try in order and the effort (`models::for_purpose`)
    pub models: crate::models::Choice,
    /// answered in the web's main conversation: the answer may draw an A2UI interface ([`crate::gateway::A2UI_HINT`])
    pub ui: bool,
}

/// A follow-up within [`RESUME_WINDOW`] of a research answer continues that run's harness session.
#[derive(Debug, Clone, PartialEq)]
pub struct ResumeFrom {
    /// the earlier `thread_runs` row
    pub run: String,
    pub claude_session: String,
    /// `--resume` needs the same model
    pub model: String,
    /// the earlier answer, the context of a fresh run when resuming is not possible
    pub prior_answer: String,
}

/// How long a research answer can be followed up in the same harness session.
pub const RESUME_WINDOW: i64 = 30 * 60 * 1000;

/// Told about a run while it reads and writes.
pub trait ResearchSink: Send + Sync {
    /// The answer so far (whole, each time it grows).
    fn text(&self, text: &str);
    /// A tool call finished.
    fn tool(&self, t: &ToolSeen);
}

/// What a run produced.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ResearchOutcome {
    /// the answer, without the `OFFER:` line (see [`gateway::split_offer`]); partial when stopped
    pub text: String,
    pub offer: Option<Offer>,
    pub end: RunEnd,
    pub model: Option<String>,
    /// the harness session id, for `--resume`
    pub claude_session: Option<String>,
    pub turns: i64,
    pub first_text_ms: Option<i64>,
    pub error: Option<String>,
}

/// The read-only research executor (thread-routing R2). Implemented with `claude -p` stream-json and a
/// tool whitelist by the integrator; faked in tests.
#[async_trait]
pub trait Researcher: Send + Sync {
    async fn shutdown(&self, _grace: std::time::Duration) -> bool {
        true
    }
    async fn research(&self, req: ResearchRequest, sink: &dyn ResearchSink) -> ResearchOutcome;
}

impl Core {
    pub async fn shutdown_research(&self, grace: std::time::Duration) -> bool {
        match self.0.thread.researcher.get() {
            Some(r) => r.shutdown(grace).await,
            None => true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Inline {
    Answer,
    Research,
}

/// An answer or research run waiting on the inline queue.
pub(crate) struct InlineJob {
    pub kind: Inline,
    /// the user message answered
    pub origin: String,
    pub question: String,
    pub files: Vec<FileRef>,
    /// the files of the message this one follows up on ([`Core::earlier_files`])
    pub earlier: Vec<FileRef>,
    pub user: Option<String>,
    pub project: Option<String>,
    /// `meta.steps.route` of the reply
    pub route: Option<Value>,
    /// offered with the answer unless the answer brings its own (guard rule 4)
    pub offer: Option<Offer>,
    /// the Steve message this answer continues (suggestion card “在这里出方案”, “改成在这里回答”)
    pub reply_to: Option<String>,
    pub run: Option<String>,
    pub stop: super::Stop,
    pub t0: i64,
    /// a follow-up of a recent research answer
    pub resume: Option<ResumeFrom>,
    /// the conversation (`messages.space`) it is answered in
    pub space: String,
}

/// The inline queue of a conversation: the web's is keyed by the workspace (as before), a guest
/// conversation's has its own, so a chat's question never waits behind your research.
struct Active {
    run: Option<String>,
    kind: Inline,
    stop: super::Stop,
}

#[derive(Default)]
struct LaneState {
    waiting: Vec<Active>,
    running: Option<Active>,
}

pub(crate) struct InlineLane {
    tx: mpsc::UnboundedSender<InlineJob>,
    st: Arc<Mutex<LaneState>>,
    /// told whenever an answer or research ends or leaves the queue (`turn_done` waits on it)
    idle: Arc<tokio::sync::Notify>,
}

/// `steve.draft` at most every [`DRAFT_EVERY`] ms, never twice the same.
struct Drafts<'a> {
    core: &'a Core,
    ws: &'a str,
    mid: &'a str,
    stop: &'a super::Stop,
    last: Mutex<(i64, String, String)>,
    /// an IM conversation's answer: nothing streams to the web's conversation
    quiet: bool,
}

impl Drafts<'_> {
    fn send(&self, thought: &str, text: &str) {
        let mut l = lock(&self.last);
        if self.quiet
            || self.stop.is_stopped()
            || now() - l.0 < DRAFT_EVERY
            || (l.1 == thought && l.2 == text)
        {
            return;
        }
        *l = (now(), thought.into(), text.into());
        self.core.store().publish(HubEvent::SteveDraft {
            ws: self.ws.into(),
            mid: self.mid.into(),
            thought: thought.into(),
            text: text.into(),
        });
    }
}

/// The thread's side of a running research: drafts, tool rows, `steve.tool`.
struct Sink<'a> {
    drafts: Drafts<'a>,
    run: &'a str,
    steps: &'a Mutex<Steps>,
    t0: i64,
    first_text: Mutex<Option<i64>>,
}

impl ResearchSink for Sink<'_> {
    fn text(&self, text: &str) {
        lock(&self.first_text).get_or_insert_with(|| now() - self.t0);
        self.drafts.send("", text);
    }
    fn tool(&self, t: &ToolSeen) {
        let mut step = json!({
            "name": t.name,
            "args": {"brief": t.brief},
            "status": if t.ok { "ok" } else { "failed" },
            "ms": t.ms,
        });
        if let Some(error) = &t.error {
            step["error"] = json!(error);
        }
        lock(self.steps).tools.push(step);
        let d = &self.drafts;
        if d.quiet {
            return;
        }
        d.core.store().publish(HubEvent::SteveTool {
            ws: d.ws.into(),
            mid: d.mid.into(),
            run: self.run.into(),
            name: t.name.clone(),
            brief: t.brief.clone(),
            ok: t.ok,
        });
    }
}

impl Core {
    /// The research run a follow-up `mid` continues: the latest finished run, at most
    /// [`RESUME_WINDOW`] old, whose answer is the last Steve message before `mid`.
    pub(crate) fn resume_for(&self, ws: &str, mid: &str) -> Result<Option<ResumeFrom>> {
        let th = self.store().store().thread();
        let Some(r) = th.run_last_done(ws)? else {
            return Ok(None);
        };
        let (Some(session), Some(model), Some(reply)) = (&r.claude_session, &r.model, &r.reply_id)
        else {
            return Ok(None);
        };
        if r.ended.is_none_or(|t| now() - t > RESUME_WINDOW) {
            return Ok(None);
        }
        let msgs = self.conv_messages(ws, &self.conv_of(mid))?;
        let at = msgs.iter().position(|m| m.id == mid).unwrap_or(msgs.len());
        let last_steve = msgs[..at]
            .iter()
            .rev()
            .find(|m| m.author == "steve" && !super::hidden(m));
        Ok(match last_steve {
            Some(m) if &m.id == reply => Some(ResumeFrom {
                run: r.id.clone(),
                claude_session: session.clone(),
                model: model.clone(),
                prior_answer: m.text.clone(),
            }),
            _ => None,
        })
    }

    /// Whether a research run is reading in this workspace right now.
    pub(crate) fn researching(&self, ws: &str) -> bool {
        lock(&self.0.thread.inline).get(ws).is_some_and(|l| {
            lock(&l.st)
                .running
                .as_ref()
                .is_some_and(|a| a.kind == Inline::Research)
        })
    }

    /// Whether the main conversation has an answer or research running or waiting.
    pub(crate) fn inline_busy(&self, ws: &str) -> bool {
        lock(&self.0.thread.inline).get(ws).is_some_and(|l| {
            let st = lock(&l.st);
            st.running.is_some() || !st.waiting.is_empty()
        })
    }

    /// Stop the running answer or research of a workspace; true if there was one.
    pub(crate) fn stop_inline(&self, ws: &str) -> bool {
        let lanes = lock(&self.0.thread.inline);
        let Some(l) = lanes.get(ws) else {
            return false;
        };
        let st = lock(&l.st);
        match &st.running {
            Some(a) => {
                a.stop.stop();
                true
            }
            None => false,
        }
    }

    /// Stop one research run: the running one ends with its partial answer, a queued one is taken off
    /// the queue (`stopped`). False when it is not active.
    pub fn stop_run(&self, ws: &str, run: &str) -> Result<bool> {
        let found = {
            let lanes = lock(&self.0.thread.inline);
            let prefix = format!("{ws}\u{1f}");
            lanes
                .iter()
                .filter(|(k, _)| k.as_str() == ws || k.starts_with(&prefix))
                .find_map(|(_, l)| {
                    let st = lock(&l.st);
                    st.running
                        .iter()
                        .chain(st.waiting.iter())
                        .find(|a| a.run.as_deref() == Some(run))
                        .map(|a| {
                            (
                                a.stop.clone(),
                                st.running
                                    .as_ref()
                                    .is_some_and(|r| r.run.as_deref() == Some(run)),
                            )
                        })
                })
        };
        let Some((stop, running)) = found else {
            return Ok(false);
        };
        stop.stop();
        if !running {
            let th = self.store().store().thread();
            if let Some(r) = th.run(run)? {
                th.run_patch(
                    run,
                    &RunPatch {
                        status: Some("stopped".into()),
                        ended: Some(now()),
                        ..Default::default()
                    },
                )?;
                self.unmark_queued(&r.message_id)?;
            }
            self.store().publish(HubEvent::ThreadRun {
                id: run.into(),
                ws: ws.into(),
            });
        }
        Ok(true)
    }

    /// Queue an answer or research run; `false` when the queue is full (the caller says so).
    pub(crate) fn enqueue_inline(&self, ws: &str, mut job: InlineJob) -> Result<bool> {
        /* held to the end: the job is sent under the map's lock, and an idle lane only goes under it */
        let mut lanes = lock(&self.0.thread.inline);
        let key = lane_key(ws, &job.space);
        if lanes.get(&key).is_some_and(|l| l.tx.is_closed()) {
            /* its worker is gone: a new lane takes over, so the conversation is not stuck */
            tracing::error!("inline lane of {ws} was gone; starting it again");
            lanes.remove(&key);
        }
        let lane = lanes
            .entry(key.clone())
            .or_insert_with(|| self.inline_lane(ws, &key));
        let queued = {
            let mut st = lock(&lane.st);
            let busy = st.running.is_some() || !st.waiting.is_empty();
            if busy && st.waiting.len() >= MAX_WAITING && job.run.is_none() {
                return Ok(false);
            }
            st.waiting.push(Active {
                run: None,
                kind: job.kind,
                stop: job.stop.clone(),
            });
            busy
        };
        {
            // All inline work is durable, including a short direct answer.
            let th = self.store().store().thread();
            let r = if let Some(id) = &job.run {
                th.run_patch(
                    id,
                    &RunPatch {
                        status: Some("queued".into()),
                        ..Default::default()
                    },
                )?
                .ok_or_else(|| crate::error::msg(steve_i18n::t!("core.thread.resume_missing")))?
            } else {
                th.run_add(&steve_store::NewThreadRun {
                    workspace_id: ws.into(),
                    message_id: job.origin.clone(),
                    user_id: job.user.clone(),
                    project_id: job.project.clone(),
                    prev_run: job.resume.as_ref().map(|r| r.run.clone()),
                    ..Default::default()
                })?
            };
            if let Some(a) = lock(&lane.st)
                .waiting
                .iter_mut()
                .find(|a| a.stop.same(&job.stop))
            {
                a.run = Some(r.id.clone());
            }
            self.store().publish(HubEvent::ThreadRun {
                id: r.id.clone(),
                ws: ws.into(),
            });
            job.run = Some(r.id);
        }
        if queued {
            self.mark_queued(&job.origin, job.run.as_deref())?;
        }
        if lane.tx.send(job).is_err() {
            tracing::error!("inline lane of {ws} is gone");
        }
        Ok(true)
    }

    /// The `turn_done` signal of the main conversation's inline lane, if it has one.
    pub(crate) fn inline_idle(&self, ws: &str) -> Option<Arc<tokio::sync::Notify>> {
        lock(&self.0.thread.inline).get(ws).map(|l| l.idle.clone())
    }

    /// An inline lane `key` of workspace `ws`: one answer or research at a time, each on a task of its
    /// own so a panic loses that one only. It goes after [`super::LANE_IDLE`] with nothing to do.
    fn inline_lane(&self, ws: &str, key: &str) -> InlineLane {
        let (tx, mut rx) = mpsc::unbounded_channel::<InlineJob>();
        let st = Arc::new(Mutex::new(LaneState::default()));
        let idle = Arc::new(tokio::sync::Notify::new());
        let me = tx.downgrade();
        let core = self.clone();
        let (w, key) = (ws.to_string(), key.to_string());
        let (s, done) = (st.clone(), idle.clone());
        tokio::spawn(async move {
            loop {
                let j = match tokio::time::timeout(super::LANE_IDLE, rx.recv()).await {
                    Ok(Some(j)) => j,
                    Ok(None) => return,
                    Err(_) => {
                        let mut lanes = lock(&core.0.thread.inline);
                        let busy = {
                            let g = lock(&s);
                            g.running.is_some() || !g.waiting.is_empty()
                        };
                        if busy || !rx.is_empty() {
                            continue;
                        }
                        if me.upgrade().is_some_and(|me| {
                            lanes.get(&key).is_some_and(|l| l.tx.same_channel(&me))
                        }) {
                            lanes.remove(&key);
                        }
                        return;
                    }
                };
                let _activity = core.store().store().activity("thread_answer").await;
                {
                    let mut g = lock(&s);
                    g.waiting.retain(|a| !a.stop.same(&j.stop));
                    if j.stop.is_stopped() {
                        drop(g);
                        done.notify_waiters();
                        continue;
                    }
                    g.running = Some(Active {
                        run: j.run.clone(),
                        kind: j.kind,
                        stop: j.stop.clone(),
                    });
                }
                let (c, w2) = (core.clone(), w.clone());
                let run = tokio::spawn(async move {
                    if let Err(e) = c.store().store().admitted(c.run_inline(&w2, &j)).await {
                        tracing::error!("inline answer in {w2}: {e}");
                        let _ = c.post(
                            &w2,
                            &j.space,
                            &format!("（出错了：{}）", js_head(&e.to_string(), 120)),
                            None,
                            Some(&json!({"origin": j.origin, "error": true})),
                        );
                    }
                });
                if let Err(e) = run.await {
                    tracing::error!("inline answer in {w} failed: {e}");
                }
                lock(&s).running = None;
                done.notify_waiters();
            }
        });
        InlineLane { tx, st, idle }
    }

    /// The user message shows “排队中” while its answer waits.
    fn mark_queued(&self, mid: &str, run: Option<&str>) -> Result<()> {
        if let Some(m) = self.store().message(mid)? {
            let mut meta = meta_of(&m);
            meta.insert("queued".into(), json!({"run": run}));
            self.store().message_patch_meta(mid, &Value::Object(meta))?;
        }
        Ok(())
    }

    fn unmark_queued(&self, mid: &str) -> Result<()> {
        if let Some(m) = self.store().message(mid)?
            && m.meta.get("queued").is_some()
        {
            let mut meta = meta_of(&m);
            meta.remove("queued");
            self.store().message_patch_meta(mid, &Value::Object(meta))?;
        }
        Ok(())
    }

    /// Answer or research one queued question and store the reply.
    async fn run_inline(&self, ws: &str, j: &InlineJob) -> Result<()> {
        self.unmark_queued(&j.origin)?;
        let researcher = self.0.thread.researcher.get().cloned();
        let r = match (j.kind, researcher) {
            (Inline::Research, Some(r)) => self.research_now(ws, j, r).await,
            _ => {
                let th = self.store().store().thread();
                if let Some(id) = &j.run {
                    th.run_patch(
                        id,
                        &RunPatch {
                            status: Some("running".into()),
                            started: Some(now()),
                            ..Default::default()
                        },
                    )?;
                }
                let answer = self.answer_now(ws, j).await;
                if let Some(id) = &j.run {
                    th.run_patch(
                        id,
                        &RunPatch {
                            status: Some(
                                if j.stop.is_stopped() {
                                    "stopped"
                                } else if answer.is_ok() {
                                    "done"
                                } else {
                                    "failed"
                                }
                                .into(),
                            ),
                            ended: Some(now()),
                            ..Default::default()
                        },
                    )?;
                }
                answer
            }
        };
        if j.space == super::MAIN {
            self.store().publish(HubEvent::SteveThinking {
                on: false,
                ws: Some(ws.into()),
                phase: None,
            });
        }
        r
    }

    /// The reply's `meta` common to answers and research.
    fn inline_meta(
        &self,
        ws: &str,
        j: &InlineJob,
        offer: Option<&Offer>,
        steps: Value,
    ) -> Result<Value> {
        let mut m = serde_json::Map::new();
        m.insert("chat".into(), json!(j.question));
        if !j.files.is_empty() {
            m.insert("chatFiles".into(), json!(j.files));
        }
        m.insert("origin".into(), json!(j.origin));
        if let Some(run) = &j.run {
            m.insert("run".into(), json!(run));
            m.insert("run_receipt".into(), json!(true));
        }
        if let Some(r) = &j.reply_to {
            m.insert("reply_to".into(), json!(r));
        }
        if let Some(o) = offer {
            m.insert(
                "offer".into(),
                self.offer_meta(ws, o, j.project.as_deref())?,
            );
        }
        m.insert("steps".into(), steps);
        Ok(Value::Object(m))
    }

    /// Store an inline reply and tie it to its question, the routing and the message it continues.
    fn inline_reply(&self, ws: &str, j: &InlineJob, text: &str, meta: &Value) -> Result<String> {
        let id = self
            .store()
            .message_add_in(ws, &j.space, "steve", text, None, meta)?;
        if meta.get("offer").is_some() {
            self.fold_offers(ws, &id)?;
        }
        let th = self.store().store().thread();
        if th
            .route_of(&j.origin)?
            .is_some_and(|r| r.reply_id.is_none())
        {
            th.route_reply(&j.origin, &id, None)?;
        }
        if let Some(to) = &j.reply_to
            && let Some(m) = self.store().message(to)?
        {
            let mut meta = meta_of(&m);
            meta.insert("answer".into(), json!(id));
            self.store().message_patch_meta(to, &Value::Object(meta))?;
        }
        self.said(ws, &j.space, &id);
        Ok(id)
    }

    async fn answer_now(&self, ws: &str, j: &InlineJob) -> Result<()> {
        if j.space != super::MAIN
            && let Some(host) = self.conv_host()
        {
            return self.guest_answer(ws, j, host).await;
        }
        let db = self.store();
        let main = db.main_bot()?;
        let history = self.history_before(ws, &j.origin)?;
        let cf = self.chat_files(&j.files, &j.earlier);
        self.phase(ws, "answer");
        let a0 = now();
        let cx = self.chat_context(ws, j.user.as_deref(), &j.question);
        let extra = cx.as_ref().map(|(_, c)| c.text.as_str()).unwrap_or("");
        let drafts = Drafts {
            core: self,
            ws,
            mid: &j.origin,
            stop: &j.stop,
            last: Mutex::new((0, String::new(), String::new())),
            quiet: false,
        };
        let on_draft = |thought: &str, t: &str| drafts.send(thought, t);
        /* a stop ends the call right away (its harness is killed), not after the whole answer is written */
        let a = tokio::select! {
            a = crate::stats::tagged(
                "chat",
                ws,
                gateway::answer(
                    self.gw(),
                    &main,
                    &j.question,
                    &history,
                    &cf,
                    extra,
                    &on_draft,
                ),
            ) => Some(a),
            _ = j.stop.stopped() => None,
        };
        let Some(a) = a.filter(|_| !j.stop.is_stopped()) else {
            /* what was written so far stays, marked as stopped */
            let partial = lock(&drafts.last).2.trim().to_string();
            let text = if partial.is_empty() {
                "好，停下了。要我换个方式就直接说。".to_string()
            } else {
                format!("{partial}\n\n（已停下）")
            };
            self.post(
                ws,
                &j.space,
                &text,
                None,
                Some(&json!({"origin": j.origin, "stopped": true})),
            )?;
            return Ok(());
        };
        let steps = Steps {
            route: j.route.clone(),
            answer: Some(
                json!({"thought": a.thought, "model": a.model, "ms": now() - a0, "failed": a.failed}),
            ),
            ..Default::default()
        };
        let offer = a.offer.clone().or_else(|| j.offer.clone());
        let meta = self.inline_meta(ws, j, offer.as_ref(), steps.json(now() - j.t0))?;
        let id = self.inline_reply(ws, j, &a.text, &meta)?;
        if let Some((provider, c)) = &cx
            && !a.failed
            && let Err(e) = self.chat_done(ws, *provider, c, &id, &a.text)
        {
            tracing::error!("chat context of {id}: {e}");
        }
        Ok(())
    }

    /// A guest conversation's inline answer: the host composes it in the voice of the place it goes
    /// to (an IM space: persona, safety rules, knowledge by disclosure level).
    async fn guest_answer(
        &self,
        ws: &str,
        j: &InlineJob,
        host: Arc<dyn super::ConvHost>,
    ) -> Result<()> {
        let a0 = now();
        let r = host.answer(ws, &j.space, &j.origin).await;
        if j.stop.is_stopped() {
            self.post(
                ws,
                &j.space,
                "好，停下了。要我换个方式就直接说。",
                None,
                Some(&json!({"origin": j.origin, "stopped": true})),
            )?;
            return Ok(());
        }
        let (text, failed) = match r {
            Ok(t) if !t.trim().is_empty() => (t, false),
            Ok(_) => ("（没能生成回答）".to_string(), true),
            Err(e) => (format!("（没能生成回答：{}）", js_head(&e, 80)), true),
        };
        let steps = Steps {
            route: j.route.clone(),
            answer: Some(json!({"model": "channel", "ms": now() - a0, "failed": failed})),
            ..Default::default()
        };
        let mut meta = self.inline_meta(ws, j, j.offer.as_ref(), steps.json(now() - j.t0))?;
        if failed {
            meta["error"] = json!(true);
        }
        self.inline_reply(ws, j, &text, &meta)?;
        Ok(())
    }

    async fn research_now(&self, ws: &str, j: &InlineJob, r: Arc<dyn Researcher>) -> Result<()> {
        let db = self.store();
        let th = db.store().thread();
        let run = j.run.clone().unwrap_or_default();
        let started = now();
        let token = crate::util::token();
        th.run_patch(
            &run,
            &RunPatch {
                status: Some("running".into()),
                started: Some(started),
                token_hash: Some(Some(crate::util::sha(&token))),
                ..Default::default()
            },
        )?;
        db.publish(HubEvent::ThreadRun {
            id: run.clone(),
            ws: ws.into(),
        });
        if j.space == super::MAIN {
            self.phase(ws, "research");
        }
        /* where anyone besides the owner reads the answer, the run gets no project directory and no one's
         * personal view: what it may say there it finds through its tools, which check every call */
        let own = own_reader(db, &j.space, &j.origin)?;
        let projects: Vec<Project> = if own {
            db.projects(ws)?
                .into_iter()
                .filter(|p| !p.archived && matches!(p.kind.as_str(), "git" | "dir"))
                .collect()
        } else {
            vec![]
        };
        let repo = self.config().steve_repo.to_string_lossy().to_string();
        let project = j
            .project
            .as_deref()
            .and_then(|id| projects.iter().find(|p| p.id == id))
            .or_else(|| {
                projects
                    .iter()
                    .find(|p| p.path == repo && p.device_id == "hub")
            })
            .cloned();
        let cx = self.chat_context(ws, j.user.as_deref().filter(|_| own), &j.question);
        /* a chat's research knows who hears its answer */
        let guest_frame = match (j.space != super::MAIN, self.conv_host()) {
            (true, Some(h)) => h.research_context(ws, &j.space, &j.origin),
            _ => String::new(),
        };
        let context = cx.as_ref().map(|(_, c)| c.text.clone()).unwrap_or_default();
        let context = if guest_frame.is_empty() {
            context
        } else {
            format!("{guest_frame}\n\n{context}")
        };
        let history = self.history_before(ws, &j.origin)?;
        /* items are read up front only where the owner alone reads the answer; a shared chat asks the tool */
        let named = if own {
            mentioned_items(db, ws, &j.question, &history)?
        } else {
            String::new()
        };
        let context = if named.is_empty() {
            context
        } else {
            format!("{context}\n\n{named}")
        };
        let req = ResearchRequest {
            run: run.clone(),
            ws: ws.into(),
            mid: j.origin.clone(),
            user: j.user.clone(),
            question: j.question.clone(),
            history,
            main: db.main_bot()?,
            context,
            project,
            readable: projects,
            files: j.files.clone(),
            earlier_files: j.earlier.clone(),
            resume: j.resume.clone(),
            stop: j.stop.flag(),
            mcp: Some(crate::McpEndpoint {
                url: format!("http://127.0.0.1:{}/mcp", self.config().port),
                token,
            }),
            models: self.models().for_purpose(ws, "research"),
            ui: j.space == super::MAIN,
        };
        let steps = Mutex::new(Steps {
            route: j.route.clone(),
            ..Default::default()
        });
        let sink = Sink {
            drafts: Drafts {
                core: self,
                ws,
                mid: &j.origin,
                stop: &j.stop,
                last: Mutex::new((0, String::new(), String::new())),
                quiet: j.space != super::MAIN,
            },
            run: &run,
            steps: &steps,
            t0: started,
            first_text: Mutex::new(None),
        };
        let mut out = r.research(req, &sink).await;
        if j.stop.is_stopped() && out.end == RunEnd::Done {
            out.end = RunEnd::Stopped;
        }
        let tools = lock(&steps).tools.len() as i64;
        let mut text = out.text.trim().to_string();
        if out.end == RunEnd::Failed && text.is_empty() {
            text = format!(
                "（没查完：{}）",
                js_head(out.error.as_deref().unwrap_or("研究失败"), 80)
            );
        }
        /* the executor may have marked the end already */
        if let Some(n) = out.end.note().filter(|n| !text.ends_with(n)) {
            text = if text.is_empty() {
                n.to_string()
            } else {
                format!("{text}\n\n{n}")
            };
        }
        let mut offer = out.offer.clone().or_else(|| j.offer.clone());
        if out.end == RunEnd::Budget && offer.is_none() {
            offer = Some(Offer {
                title: js_head(&format!("继续深入：{}", j.question), 24).to_string(),
                goal: j.question.clone(),
            });
        }
        let first = (*lock(&sink.first_text)).or(out.first_text_ms);
        {
            let mut s = lock(&steps);
            let mut research = json!({
                "run": run, "model": out.model, "ms": now() - started,
                "turns": out.turns, "tools": tools,
            });
            if out.end != RunEnd::Done {
                research["stopped"] = json!(match out.end {
                    RunEnd::Stopped => "user",
                    RunEnd::Budget => "budget",
                    RunEnd::Violated => "violation",
                    _ => "failed",
                });
            }
            s.research = Some(research);
        }
        let meta = self.inline_meta(ws, j, offer.as_ref(), lock(&steps).json(now() - j.t0))?;
        let id = self.inline_reply(ws, j, &text, &meta)?;
        th.run_patch(
            &run,
            &RunPatch {
                status: Some(out.end.status().into()),
                reply_id: Some(id.clone()),
                model: out.model.clone(),
                claude_session: out.claude_session.clone(),
                token_hash: Some(None),
                tools: Some(tools),
                turns: Some(out.turns),
                first_text_ms: first,
                error: out.error.clone(),
                ended: Some(now()),
                ..Default::default()
            },
        )?;
        db.publish(HubEvent::ThreadRun {
            id: run,
            ws: ws.into(),
        });
        if let Some((provider, c)) = &cx
            && out.end == RunEnd::Done
            && let Err(e) = self.chat_done(ws, *provider, c, &id, &text)
        {
            tracing::error!("chat context of {id}: {e}");
        }
        Ok(())
    }
}

/// The items the question or the last turns name (`ST-23`), at most 3, read up front for the research
/// prompt: a follow-up like "是的" after "查一下 ST-23" names nothing itself, and the model skips the item
/// tool often enough. The same reading `steve_item_read` gives, shorter.
fn mentioned_items(
    db: &crate::repo::Db,
    ws: &str,
    question: &str,
    history: &[Said],
) -> Result<String> {
    let Some(w) = db.workspace(ws)? else {
        return Ok(String::new());
    };
    let prefix = w.item_prefix.to_uppercase();
    let texts =
        std::iter::once(question).chain(history.iter().rev().take(6).map(|h| h.text.as_str()));
    let mut ids: Vec<String> = vec![];
    for t in texts {
        for id in item_refs(t, &prefix) {
            if !ids.contains(&id) && ids.len() < 3 {
                ids.push(id);
            }
        }
    }
    let mut out = String::new();
    for id in ids {
        let Some(i) = db.item(&id)?.filter(|i| i.workspace_id == ws) else {
            continue;
        };
        let goal = db.goal(&i.id).unwrap_or_default();
        out.push_str(&format!(
            "{} {}\n状态：{} · {}\n现在：{}\n目标：{}\n",
            i.id,
            i.title,
            i.status,
            i.stage,
            i.now,
            js_head(&goal, 300)
        ));
        if !i.plan.is_empty() {
            let done = i.plan.iter().filter(|p| p.done).count();
            out.push_str(&format!("计划：完成 {done}/{}\n", i.plan.len()));
        }
        let tl = db.timeline(&i.id)?;
        for t in tl.iter().rev().take(5).rev() {
            out.push_str(&format!("  {} {}\n", t.actor, js_head(&t.text, 160)));
        }
    }
    Ok(if out.is_empty() {
        out
    } else {
        format!("对话提到的事项（已读取）：\n{out}")
    })
}

/// Only the owner reads the answer to `origin`: Steve's own conversation, or a chat the channel marked
/// `owner_only` (the owner's private chat with Steve).
pub(crate) fn own_reader(db: &crate::repo::Db, space: &str, origin: &str) -> Result<bool> {
    if space == super::MAIN {
        return Ok(true);
    }
    Ok(db
        .message(origin)?
        .is_some_and(|m| m.meta["chan"]["owner_only"] == json!(true)))
}

/// `PREFIX-<digits>` references in `t` (any case), normalised to upper case.
fn item_refs(t: &str, prefix: &str) -> Vec<String> {
    let up = t.to_uppercase();
    let pat = format!("{prefix}-");
    let mut out = vec![];
    let mut from = 0;
    while let Some(at) = up[from..].find(&pat) {
        let start = from + at;
        let before = up[..start].chars().next_back();
        let digits: String = up[start + pat.len()..]
            .chars()
            .take_while(|c| c.is_ascii_digit())
            .collect();
        if !digits.is_empty() && !before.is_some_and(|c| c.is_ascii_alphanumeric()) {
            out.push(format!("{prefix}-{digits}"));
        }
        from = start + pat.len();
    }
    out
}

#[cfg(test)]
mod mention_tests {
    #[test]
    fn item_refs_are_found_in_any_case_and_not_inside_words() {
        assert_eq!(
            super::item_refs("查一下 st-23 和 ST-7，还有 XST-9", "ST"),
            ["ST-23", "ST-7"]
        );
        assert!(super::item_refs("ST- 和 ST-x", "ST").is_empty());
    }
}
