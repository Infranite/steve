//! Inline answers and read-only research executed under durable `thread_reply_tasks` leases,
//! with per-space `--resume` isolation, prompt stop cancellation, explicit failure marking,
//! and single-tx fenced receipt commits.

use super::handle::RouteInfo;
use super::{Conv, DRAFT_EVERY, Steps, Stop, decide, handoff, lock, meta_of};
use crate::Core;
use crate::error::Result;
use crate::gateway::{self, Offer, Said};
use crate::repo::{Bot, FileRef, Project};
use crate::util::{js_head, now};
use async_trait::async_trait;
use serde_json::{Value, json};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex};
use steve_store::{CommitOutcome, CommitReplyInput, ThreadTask, thread};

/// Where to look when a chat's answer could not be written.
const FIX_MODEL: &str = "到「设置 · 模型」看看回复用的模型是否可用，或稍后再发一次";
/// `meta` flag of a message Steve's thread writes about its own state rather than as an answer.
pub const SYSTEM: &str = "system";

/// One tool call of a research run, as the executor saw it.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolSeen {
    pub call: Option<u32>,
    pub name: String,
    pub kind: String,
    pub brief: String,
    pub ok: bool,
    pub error: Option<String>,
    pub ms: i64,
}

/// How a research run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RunEnd {
    #[default]
    Done,
    Stopped,
    Budget,
    Violated,
    Failed,
}

impl RunEnd {
    pub fn outcome(self) -> &'static str {
        match self {
            RunEnd::Done => "answered",
            RunEnd::Stopped => "stopped",
            RunEnd::Budget | RunEnd::Violated | RunEnd::Failed => "failed",
        }
    }

    fn note(self) -> Option<&'static str> {
        match self {
            RunEnd::Done | RunEnd::Failed => None,
            RunEnd::Stopped => Some("（已停下）"),
            RunEnd::Budget => Some("（没读完就停了）"),
            RunEnd::Violated => Some("（本次查询在工具检查时停止，尚未完成。）"),
        }
    }
}

/// Everything a research run needs; collected by the thread so the executor touches no state.
#[derive(Clone)]
pub struct ResearchRequest {
    pub run: String,
    pub ws: String,
    pub mid: String,
    pub user: Option<String>,
    pub question: String,
    pub history: Vec<Said>,
    pub main: Bot,
    pub context: String,
    pub project: Option<Project>,
    pub readable: Vec<Project>,
    pub files: Vec<FileRef>,
    pub earlier_files: Vec<FileRef>,
    pub resume: Option<ResumeFrom>,
    pub stop: Arc<AtomicBool>,
    pub mcp: Option<crate::McpEndpoint>,
    pub models: crate::models::Choice,
    pub ui: bool,
    pub full: bool,
    pub brief: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ResumeFrom {
    pub run: String,
    pub session: String,
    pub harness: String,
    pub model: String,
}

/// How long a research answer can be followed up in the same harness session (per space).
pub const RESUME_WINDOW: i64 = 30 * 60 * 1000;

pub trait ResearchSink: Send + Sync {
    fn text(&self, text: &str);
    fn tool(&self, t: &ToolSeen);
    fn tool_running(&self, _call: Option<u32>, _name: &str, _kind: &str, _brief: &str) {}
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ResearchOutcome {
    pub text: String,
    pub offer: Option<Offer>,
    pub end: RunEnd,
    pub model: Option<String>,
    pub session: Option<String>,
    pub turns: i64,
    pub first_text_ms: Option<i64>,
    pub error: Option<String>,
}

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

/// `steve.draft` throttled to [`DRAFT_EVERY`] ms, closed immediately on stop or completion.
struct Drafts<'a> {
    core: &'a Core,
    ws: &'a str,
    mid: &'a str,
    stop: &'a Stop,
    state: Arc<Mutex<DraftState>>,
    quiet: bool,
}

#[derive(Default)]
struct DraftState {
    sent: i64,
    thought: String,
    text: String,
    pending: bool,
    closed: bool,
}

impl<'a> Drafts<'a> {
    fn new(core: &'a Core, ws: &'a str, mid: &'a str, stop: &'a Stop, quiet: bool) -> Self {
        Self {
            core,
            ws,
            mid,
            stop,
            quiet,
            state: Default::default(),
        }
    }

    fn send(&self, thought: &str, text: &str) {
        let mut l = lock(&self.state);
        if self.quiet
            || l.closed
            || self.stop.is_stopped()
            || (l.thought == thought && l.text == text)
        {
            return;
        }
        l.thought = thought.into();
        l.text = text.into();
        let wait = DRAFT_EVERY - (now() - l.sent);
        if l.pending {
            return;
        }
        if wait <= 0 {
            l.sent = now();
            self.core
                .store()
                .store()
                .publish(steve_types::LiveEvent::from(
                    steve_types::WorkLiveEvent::SteveDraft {
                        ws: self.ws.to_owned().into(),
                        mid: self.mid.to_owned().into(),
                        thought: l.thought.clone(),
                        text: l.text.clone(),
                    },
                ));
            return;
        }
        l.pending = true;
        let (core, ws, mid, stop, state) = (
            self.core.clone(),
            self.ws.to_string(),
            self.mid.to_string(),
            self.stop.clone(),
            self.state.clone(),
        );
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(wait as u64)).await;
            let mut l = lock(&state);
            l.pending = false;
            if l.closed || stop.is_stopped() {
                return;
            }
            l.sent = now();
            core.store().store().publish(steve_types::LiveEvent::from(
                steve_types::WorkLiveEvent::SteveDraft {
                    ws: ws.into(),
                    mid: mid.into(),
                    thought: l.thought.clone(),
                    text: l.text.clone(),
                },
            ));
        });
    }
}

impl Drop for Drafts<'_> {
    fn drop(&mut self) {
        lock(&self.state).closed = true;
    }
}

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
        if let Some(call) = t.call {
            step["call"] = json!(call);
        }
        if !t.kind.is_empty() {
            step["kind"] = json!(t.kind);
        }
        if let Some(error) = &t.error {
            step["error"] = json!(error);
        }
        lock(self.steps).tools.push(step);
        let d = &self.drafts;
        if d.quiet {
            return;
        }
        d.core.store().store().publish(steve_types::LiveEvent::from(
            steve_types::WorkLiveEvent::SteveTool {
                ws: d.ws.to_owned().into(),
                mid: d.mid.to_owned().into(),
                run: self.run.to_owned(),
                name: t.name.clone(),
                brief: t.brief.clone(),
                ok: t.ok,
                kind: t.kind.clone(),
                running: false,
                call: t.call,
                started_at: Some(steve_types::Millis(self.t0)),
                at: Some(steve_types::Millis(now())),
            },
        ));
    }

    fn tool_running(&self, call: Option<u32>, name: &str, kind: &str, brief: &str) {
        let d = &self.drafts;
        if d.quiet {
            return;
        }
        d.core.store().store().publish(steve_types::LiveEvent::from(
            steve_types::WorkLiveEvent::SteveTool {
                ws: d.ws.to_owned().into(),
                mid: d.mid.to_owned().into(),
                run: self.run.to_owned(),
                name: name.into(),
                brief: brief.into(),
                ok: true,
                kind: kind.into(),
                running: true,
                call,
                started_at: Some(steve_types::Millis(self.t0)),
                at: Some(steve_types::Millis(now())),
            },
        ));
    }
}

impl Core {
    /// Per-space isolated `--resume` lookup: only resumes a recent `answered` receipt from the
    /// exact same `(ws, space)` whose reply is the last visible Steve message before `mid`.
    pub(crate) fn resume_for(
        &self,
        ws: &str,
        space: &str,
        mid: &str,
    ) -> Result<Option<ResumeFrom>> {
        let Some(r) = self
            .store()
            .store()
            .read(|rd| thread::last_resumable_receipt(rd, ws, space))?
        else {
            return Ok(None);
        };
        let (Some(session), Some(model), Some(reply)) =
            (&r.harness_session, &r.answer_model, &r.reply_message_id)
        else {
            return Ok(None);
        };
        if now() - r.committed_at > RESUME_WINDOW {
            return Ok(None);
        }
        let msgs = self.conv_messages(ws, space)?;
        let at = msgs.iter().position(|m| m.id == mid).unwrap_or(msgs.len());
        let last_steve = msgs[..at]
            .iter()
            .rev()
            .find(|m| m.author == "steve" && !super::hidden(m));
        Ok(match last_steve {
            Some(m) if &m.id == reply => Some(ResumeFrom {
                run: r.id.clone(),
                session: session.clone(),
                harness: r.harness.clone().unwrap_or_default(),
                model: model.clone(),
            }),
            _ => None,
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub(super) async fn execute_inline(
        &self,
        task: &ThreadTask,
        lease: &str,
        stop: &Stop,
        conv: &Conv,
        text: &str,
        files: &[FileRef],
        info: RouteInfo,
        t0: i64,
    ) -> Result<()> {
        let researcher = self.0.thread.researcher.get().cloned();
        let want_research = info.kind == "research";
        match (want_research, researcher) {
            (true, Some(r)) => {
                self.research_now(task, lease, stop, text, files, info, r, t0)
                    .await
            }
            _ => {
                self.answer_now(task, lease, stop, conv, text, files, info, t0)
                    .await
            }
        }
    }

    fn inline_meta(
        &self,
        ws: &str,
        task: &ThreadTask,
        question: &str,
        files: &[FileRef],
        project: Option<&str>,
        offer: Option<&Offer>,
        steps: Value,
    ) -> Result<Value> {
        let mut m = serde_json::Map::new();
        m.insert("chat".into(), json!(question));
        if !files.is_empty() {
            m.insert("chatFiles".into(), json!(files));
        }
        m.insert("origin".into(), json!(task.origin_message_id));
        m.insert("run".into(), json!(task.id));
        m.insert("run_receipt".into(), json!(true));
        if let Some(ref r) = task.reply_to_message_id {
            m.insert("reply_to".into(), json!(r));
        }
        if let Some(o) = offer {
            m.insert("offer".into(), self.offer_meta(ws, o, project)?);
        }
        m.insert("steps".into(), steps);
        Ok(Value::Object(m))
    }

    fn after_inline_commit(
        &self,
        ws: &str,
        task: &ThreadTask,
        reply_id: &str,
        meta: &Value,
    ) -> Result<()> {
        if meta.get("offer").is_some() {
            self.fold_offers(ws, reply_id)?;
        }
        if let Some(ref to) = task.reply_to_message_id
            && let Some(m) = self.store().message(to)?
        {
            let mut mmeta = meta_of(&m);
            mmeta.insert("answer".into(), json!(reply_id));
            self.store().message_patch_meta(to, &Value::Object(mmeta))?;
        }
        self.said(ws, &task.space, reply_id);
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn answer_now(
        &self,
        task: &ThreadTask,
        lease: &str,
        stop: &Stop,
        conv: &Conv,
        text: &str,
        files: &[FileRef],
        info: RouteInfo,
        t0: i64,
    ) -> Result<()> {
        let ws = &task.workspace_id;
        let space = &task.space;
        let mid = &task.origin_message_id;

        // Capability preflight for hands-on spaces before any tool-less answer call (0 wasted probes).
        if conv.hands_on && decide::needs_machine_tools(text) {
            let steps = Mutex::new(Steps {
                route: info.step_json.clone(),
                ..Default::default()
            });
            if conv.items {
                return self
                    .preflight_hands_on_item(task, lease, conv, text, files, &info, &steps, t0);
            }
            let reply_text = if space == super::MAIN {
                handoff::NEEDS_TOOLS_ANSWER
            } else {
                handoff::NEEDS_TOOLS_HERE
            };
            let mut meta = self.inline_meta(
                ws,
                task,
                text,
                files,
                info.project.as_deref(),
                info.offer.as_ref(),
                lock(&steps).json(now() - t0),
            )?;
            meta["error"] = json!(true);
            meta[SYSTEM] = json!(true);
            let committed = self.commit_inline_reply(
                task,
                lease,
                false,
                "failed",
                reply_text,
                meta.clone(),
                &info,
                None,
                None,
                None,
                0,
                0,
                None,
                Some("needs_tools"),
            )?;
            if let CommitOutcome::Committed {
                reply_message_id: Some(ref id),
                ..
            } = committed
            {
                self.after_inline_commit(ws, task, id, &meta)?;
            }
            return Ok(());
        }

        lock(&self.0.thread.running_phase).insert((ws.clone(), space.clone()), "answer");

        if space != super::MAIN
            && let Some(host) = self.conv_host()
        {
            return self
                .guest_answer(task, lease, stop, text, files, info, host, t0)
                .await;
        }

        let db = self.store();
        let main = db.main_bot()?;
        let history = self.history_before(ws, mid)?;
        let earlier = self.earlier_files(ws, space, mid, files, None)?;
        let cf = self.chat_files(files, &earlier);
        self.phase(ws, "answer");
        let a0 = now();
        let cx = self.chat_context(ws, task.user_id.as_deref(), text);
        let extra = cx.as_ref().map(|(_, c)| c.text.as_str()).unwrap_or("");
        let drafts = Drafts::new(self, ws, mid, stop, false);
        let on_draft = |thought: &str, t: &str| drafts.send(thought, t);
        let a = tokio::select! {
            a = crate::stats::tagged(
                "chat",
                ws,
                gateway::answer(
                    self.gw(),
                    &main,
                    text,
                    &history,
                    &cf,
                    extra,
                    &on_draft,
                ),
            ) => Some(a),
            _ = stop.stopped() => None,
        };
        lock(&drafts.state).closed = true;
        let Some(a) = a.filter(|_| !stop.is_stopped()) else {
            let partial = lock(&drafts.state).text.trim().to_string();
            let reply_text = if partial.is_empty() {
                "好，停下了。要我换个方式就直接说。".to_string()
            } else {
                format!("{partial}\n\n（已停下）")
            };
            let mut meta = self.inline_meta(
                ws,
                task,
                text,
                files,
                info.project.as_deref(),
                None,
                json!({"tools": [], "ms": now() - t0}),
            )?;
            meta["stopped"] = json!(true);
            meta[SYSTEM] = json!(true);
            let committed = self.commit_inline_reply(
                task,
                lease,
                true,
                "stopped",
                &reply_text,
                meta.clone(),
                &info,
                None,
                None,
                None,
                0,
                0,
                None,
                None,
            )?;
            if let CommitOutcome::Committed {
                reply_message_id: Some(ref id),
                ..
            } = committed
            {
                self.after_inline_commit(ws, task, id, &meta)?;
            }
            return Ok(());
        };

        let reply_text = if a.needs_tools {
            if space == super::MAIN {
                handoff::NEEDS_TOOLS_ANSWER.to_string()
            } else {
                handoff::NEEDS_TOOLS_HERE.to_string()
            }
        } else {
            a.text.clone()
        };
        let steps = Steps {
            route: info.step_json.clone(),
            answer: Some(
                json!({"thought": a.thought, "model": a.model, "ms": now() - a0, "failed": a.failed}),
            ),
            ..Default::default()
        };
        let offer = if a.failed {
            None
        } else {
            a.offer.clone().or_else(|| info.offer.clone())
        };
        let mut meta = self.inline_meta(
            ws,
            task,
            text,
            files,
            info.project.as_deref(),
            offer.as_ref(),
            steps.json(now() - t0),
        )?;
        let is_err = a.failed || a.needs_tools;
        if is_err {
            meta["error"] = json!(true);
        }
        let outcome = if is_err { "failed" } else { "answered" };
        let committed = self.commit_inline_reply(
            task,
            lease,
            false,
            outcome,
            &reply_text,
            meta.clone(),
            &info,
            a.model.as_deref(),
            None,
            None,
            0,
            1,
            None,
            if a.needs_tools {
                Some("needs_tools")
            } else {
                a.failed.then_some("answer_failed")
            },
        )?;
        if let CommitOutcome::Committed {
            reply_message_id: Some(ref id),
            ..
        } = committed
        {
            self.after_inline_commit(ws, task, id, &meta)?;
            if let Some((provider, c)) = &cx
                && !a.failed
                && let Err(e) = self.chat_done(ws, *provider, c, id, &a.text)
            {
                tracing::error!("chat context of {id}: {e}");
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn guest_answer(
        &self,
        task: &ThreadTask,
        lease: &str,
        stop: &Stop,
        text: &str,
        files: &[FileRef],
        info: RouteInfo,
        host: Arc<dyn super::ConvHost>,
        t0: i64,
    ) -> Result<()> {
        let ws = &task.workspace_id;
        let space = &task.space;
        let mid = &task.origin_message_id;
        let a0 = now();
        // Race host.answer against stop so guest cancellation unblocks promptly.
        let r = tokio::select! {
            r = host.answer(ws, space, mid) => Some(r),
            _ = stop.stopped() => None,
        };
        let Some(r) = r.filter(|_| !stop.is_stopped()) else {
            let mut meta = self.inline_meta(
                ws,
                task,
                text,
                files,
                info.project.as_deref(),
                None,
                json!({"tools": [], "ms": now() - t0}),
            )?;
            meta["stopped"] = json!(true);
            meta[SYSTEM] = json!(true);
            let committed = self.commit_inline_reply(
                task,
                lease,
                true,
                "stopped",
                "好，停下了。要我换个方式就直接说。",
                meta.clone(),
                &info,
                Some("channel"),
                None,
                None,
                0,
                0,
                None,
                None,
            )?;
            if let CommitOutcome::Committed {
                reply_message_id: Some(ref id),
                ..
            } = committed
            {
                self.after_inline_commit(ws, task, id, &meta)?;
            }
            return Ok(());
        };
        let (reply_text, failed) = match r {
            Ok(t) if !t.trim().is_empty() => (t, false),
            Ok(_) => (format!("（没能生成回答。{FIX_MODEL}）"), true),
            Err(e) if e.can_handoff() => (handoff::NEEDS_TOOLS_HERE.to_string(), true),
            Err(e) => (
                format!(
                    "（没能生成回答：{}。{FIX_MODEL}）",
                    js_head(&e.to_string(), 80)
                ),
                true,
            ),
        };
        let steps = Steps {
            route: info.step_json.clone(),
            answer: Some(json!({"model": "channel", "ms": now() - a0, "failed": failed})),
            ..Default::default()
        };
        let mut meta = self.inline_meta(
            ws,
            task,
            text,
            files,
            info.project.as_deref(),
            if failed { None } else { info.offer.as_ref() },
            steps.json(now() - t0),
        )?;
        if failed {
            meta["error"] = json!(true);
            meta[SYSTEM] = json!(true);
        }
        let outcome = if failed { "failed" } else { "answered" };
        let committed = self.commit_inline_reply(
            task,
            lease,
            false,
            outcome,
            &reply_text,
            meta.clone(),
            &info,
            Some("channel"),
            None,
            None,
            0,
            1,
            None,
            failed.then_some("guest_answer_failed"),
        )?;
        if let CommitOutcome::Committed {
            reply_message_id: Some(ref id),
            ..
        } = committed
        {
            self.after_inline_commit(ws, task, id, &meta)?;
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn research_now(
        &self,
        task: &ThreadTask,
        lease: &str,
        stop: &Stop,
        text: &str,
        files: &[FileRef],
        info: RouteInfo,
        r: Arc<dyn Researcher>,
        t0: i64,
    ) -> Result<()> {
        let db = self.store();
        let ws = &task.workspace_id;
        let space = &task.space;
        let mid = &task.origin_message_id;
        let run = task.id.clone();
        let started = now();
        let token = crate::util::token();
        let token_ok = db.store().write(|w| {
            thread::set_task_token(
                w,
                &run,
                lease,
                task.generation,
                Some(&crate::util::sha(&token)),
            )
        })?;
        if !token_ok {
            return Ok(());
        }
        lock(&self.0.thread.running_phase).insert((ws.clone(), space.clone()), "research");
        if space == super::MAIN {
            self.phase(ws, "research");
        }

        let own = own_reader(db, space, mid)?;
        let projects: Vec<Project> = if own {
            db.projects(ws)?
                .into_iter()
                .filter(|p| !p.archived && matches!(p.kind.as_str(), "git" | "dir"))
                .collect()
        } else {
            vec![]
        };
        let repo = self.config().steve_repo.clone();
        let project = info
            .project
            .as_deref()
            .or(task.project_id.as_deref())
            .and_then(|id| projects.iter().find(|p| p.id == id))
            .or_else(|| {
                projects.iter().find(|p| {
                    crate::config::is_steve_project(&p.path, &repo) && p.device_id == "hub"
                })
            })
            .cloned();
        let cx = self.chat_context(ws, task.user_id.as_deref().filter(|_| own), text);
        let guest_frame = match (space != super::MAIN, self.conv_host()) {
            (true, Some(h)) => h.research_context(ws, space, mid),
            _ => String::new(),
        };
        let context = cx.as_ref().map(|(_, c)| c.text.clone()).unwrap_or_default();
        let context = if guest_frame.is_empty() {
            context
        } else {
            format!("{guest_frame}\n\n{context}")
        };
        let history = self.history_before(ws, mid)?;
        let named = if own {
            mentioned_items(db, ws, text, &history)?
        } else {
            String::new()
        };
        let context = if named.is_empty() {
            context
        } else {
            format!("{context}\n\n{named}")
        };
        let models = self.models().for_purpose(ws, "research");
        let resume = self
            .resume_for(ws, space, mid)?
            .filter(|res| res.harness.eq(&models.harness));
        let earlier = self.earlier_files(ws, space, mid, files, resume.as_ref())?;
        let full = own && asked_by_owner(db, task.user_id.as_deref());
        let req = ResearchRequest {
            run: run.clone(),
            ws: ws.clone(),
            mid: mid.clone(),
            user: task.user_id.clone(),
            question: text.to_string(),
            history,
            main: db.main_bot()?,
            context,
            project,
            readable: projects,
            files: files.to_vec(),
            earlier_files: earlier,
            resume,
            stop: stop.flag(),
            mcp: Some(crate::McpEndpoint {
                url: format!("http://127.0.0.1:{}/mcp", self.config().port),
                token,
            }),
            models: models.clone(),
            ui: space == super::MAIN,
            full,
            brief: if full && space == super::MAIN && self.meeting(ws)? {
                super::onboard::BRIEF.trim().to_string()
            } else {
                String::new()
            },
        };
        let harness = models.harness.clone();
        let steps = Mutex::new(Steps {
            route: info.step_json.clone(),
            ..Default::default()
        });
        let sink = Sink {
            drafts: Drafts::new(self, ws, mid, stop, space != super::MAIN),
            run: &run,
            steps: &steps,
            t0: started,
            first_text: Mutex::new(None),
        };
        let mut out = r.research(req, &sink).await;
        lock(&sink.drafts.state).closed = true;
        // Clear MCP token immediately after research finishes.
        let _ = db
            .store()
            .write(|w| thread::set_task_token(w, &run, lease, task.generation, None));

        if stop.is_stopped() && out.end == RunEnd::Done {
            out.end = RunEnd::Stopped;
        }
        let tools = lock(&steps).tools.len() as i64;
        let mut reply_text = out.text.trim().to_string();
        if out.end == RunEnd::Failed {
            if reply_text.is_empty() {
                reply_text = format!(
                    "（没查完：{}）",
                    js_head(out.error.as_deref().unwrap_or("研究失败"), 80)
                );
            } else {
                reply_text = format!("{reply_text}\n\n（研究中断，结果不完整）");
            }
        }
        if let Some(n) = out.end.note().filter(|n| !reply_text.ends_with(n)) {
            reply_text = if reply_text.is_empty() {
                n.to_string()
            } else {
                format!("{reply_text}\n\n{n}")
            };
        }
        // Stopped runs NEVER attach a late offer.
        let mut offer = if stop.is_stopped() || out.end == RunEnd::Stopped {
            None
        } else {
            out.offer.clone().or_else(|| info.offer.clone())
        };
        if out.end == RunEnd::Budget && offer.is_none() {
            offer = Some(Offer {
                title: js_head(&format!("继续深入：{text}"), 24).to_string(),
                goal: text.to_string(),
            });
        }
        let first = (*lock(&sink.first_text)).or(out.first_text_ms);
        drop(sink);
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

        let raw = reply_text.clone();
        let mut host_reply = None;
        if space != super::MAIN {
            if out.end == RunEnd::Done && !reply_text.is_empty() && !stop.is_stopped() {
                let prepared = match self.conv_host() {
                    Some(host) => tokio::select! {
                        res = host.research_answer(ws, space, mid, &raw) => res,
                        _ = stop.stopped() => Err("已停止，研究结果未进入发送流程".into()),
                    },
                    None => Err("对话渠道不可用，未核验研究结果的发送资格".into()),
                };
                match prepared {
                    Ok(t) if !t.trim().is_empty() && !stop.is_stopped() => {
                        reply_text = t;
                        host_reply = Some(json!({"status": "prepared"}));
                    }
                    other => {
                        let why = match other {
                            Err(e) => e,
                            _ => "未获得可发送的正式回复".into(),
                        };
                        host_reply = Some(json!({"status": "ineligible", "reason": why}));
                    }
                }
            } else {
                let why = match out.end {
                    RunEnd::Done => "研究未生成可用结果，未进入发送流程",
                    RunEnd::Stopped => "研究已停止，部分结果仅保留在 Steve 对话中",
                    RunEnd::Budget => "研究预算已用完，部分结果仅保留在 Steve 对话中",
                    RunEnd::Violated => "研究在工具检查时停止，部分结果仅保留在 Steve 对话中",
                    RunEnd::Failed => "研究失败，部分结果仅保留在 Steve 对话中",
                };
                host_reply = Some(json!({"status": "ineligible", "reason": why}));
            }
        }
        if stop.is_stopped() && out.end == RunEnd::Done {
            out.end = RunEnd::Stopped;
            offer = None;
            reply_text = format!("{raw}\n\n（已停下）");
            if space != super::MAIN {
                host_reply = Some(
                    json!({"status": "ineligible", "reason": "已停止，研究结果未进入发送流程"}),
                );
            }
            if let Some(research) = lock(&steps).research.as_mut() {
                research["stopped"] = json!("user");
            }
        }

        let mut meta = self.inline_meta(
            ws,
            task,
            text,
            files,
            info.project.as_deref(),
            offer.as_ref(),
            lock(&steps).json(now() - t0),
        )?;
        if out.end == RunEnd::Stopped {
            meta["stopped"] = json!(true);
        }
        if out.end == RunEnd::Failed {
            meta["error"] = json!(true);
        }
        if let Some(h) = host_reply {
            meta["host_reply"] = h;
            meta["research_result"] = json!(raw);
        }

        let committed = self.commit_inline_reply(
            task,
            lease,
            out.end == RunEnd::Stopped,
            out.end.outcome(),
            &reply_text,
            meta.clone(),
            &info,
            out.model.as_deref(),
            out.session.as_ref().map(|_| harness.as_str()),
            out.session.as_deref(),
            tools,
            out.turns,
            first,
            out.error.as_deref(),
        )?;
        if let CommitOutcome::Committed {
            reply_message_id: Some(ref id),
            ..
        } = committed
        {
            self.after_inline_commit(ws, task, id, &meta)?;
            if let Some((provider, c)) = &cx
                && out.end == RunEnd::Done
                && let Err(e) = self.chat_done(ws, *provider, c, id, &reply_text)
            {
                tracing::error!("chat context of {id}: {e}");
            }
        }
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    fn commit_inline_reply(
        &self,
        task: &ThreadTask,
        lease: &str,
        allow_cancelled_stop: bool,
        outcome: &str,
        reply_text: &str,
        reply_meta: Value,
        info: &RouteInfo,
        answer_model: Option<&str>,
        harness: Option<&str>,
        harness_session: Option<&str>,
        tools: i64,
        turns: i64,
        first_text_ms: Option<i64>,
        error_detail: Option<&str>,
    ) -> Result<CommitOutcome> {
        Ok(self.store().store().write(|w| {
            thread::commit_reply(
                w,
                &CommitReplyInput {
                    task_id: &task.id,
                    lease_token: lease,
                    expected_generation: task.generation,
                    allow_cancelled_stop,
                    outcome,
                    reply_text: Some(reply_text),
                    existing_reply_id: None,
                    reply_item_id: None,
                    reply_meta: &reply_meta,
                    error_code: (outcome == "failed").then_some("inline_failed"),
                    route_kind: &info.kind,
                    model_kind: &info.model_kind,
                    route_guard: info.guard.as_deref(),
                    route_model: info.model.as_deref(),
                    route_confidence: info.confidence,
                    route_signals: &info.signals,
                    route_fallback: info.fallback,
                    route_ms: info.ms,
                    route_outcome: None,
                    item_id: None,
                    dispatch: steve_store::ReceiptDispatch::NoDispatch,
                    delivery_ref_id: None,
                    model_call_id: None,
                    user_id: task.user_id.as_deref(),
                    project_id: info.project.as_deref().or(task.project_id.as_deref()),
                    answer_model,
                    harness,
                    harness_session,
                    tools,
                    turns,
                    first_text_ms,
                    error_detail,
                    record_attention_commit: task.space == super::MAIN && task.user_id.is_some(),
                },
            )
        })?)
    }
}

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
            "{} {}\n状态：{}\n目标：{}\n",
            i.id,
            i.title,
            i.outcome.as_deref().unwrap_or_else(|| i.phase.as_str()),
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

pub(crate) fn asked_by_owner(db: &crate::repo::Db, user: Option<&str>) -> bool {
    let users = db.store().users();
    match user {
        Some(id) => matches!(users.get(id), Ok(Some(u)) if u.role == "owner"),
        None => matches!(users.count(), Ok(n) if n <= 1),
    }
}

pub(crate) fn own_reader(db: &crate::repo::Db, space: &str, origin: &str) -> Result<bool> {
    if space == super::MAIN {
        return Ok(true);
    }
    Ok(db
        .message(origin)?
        .is_some_and(|m| m.meta["chan"]["owner_only"] == json!(true)))
}

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

#[cfg(test)]
mod draft_tests {
    use super::*;
    use crate::repo::WS;
    use crate::testkit::Kit;

    async fn drained(state: &Arc<Mutex<DraftState>>) {
        tokio::time::timeout(std::time::Duration::from_secs(2), async {
            while lock(state).pending {
                tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            }
        })
        .await
        .expect("trailing draft task completed");
    }

    #[tokio::test]
    async fn draft_bursts_publish_the_complete_tail_during_a_pause() {
        let k = Kit::new();
        let stop = super::super::Stop::default();
        let drafts = Drafts::new(&k.core, WS, "question", &stop, false);
        for (head, tail) in [
            ("再交给", " Steve 开发排查。\n"),
            ("中文", "标点。！"),
            ("```rs\nlet n = 1;", "\n```"),
        ] {
            drafts.send("", head);
            lock(&drafts.state).sent = now() + 100;
            drafts.send("", &format!("{head}{}", tail.chars().next().unwrap()));
            drafts.send("", &format!("{head}{tail}"));
            assert!(lock(&drafts.state).pending);
            drained(&drafts.state).await;
            assert_eq!(
                k.events.lock().unwrap().last(),
                Some(&steve_types::BusEvent::Live(steve_types::LiveEvent::Work(
                    steve_types::WorkLiveEvent::SteveDraft {
                        ws: WS.to_owned().into(),
                        mid: "question".to_owned().into(),
                        thought: String::new(),
                        text: format!("{head}{tail}")
                    }
                )))
            );
            let n = k.events.lock().unwrap().len();
            drafts.send("", &format!("{head}{tail}"));
            assert_eq!(
                k.events.lock().unwrap().len(),
                n,
                "unchanged snapshots are not repeated"
            );
        }
    }

    #[tokio::test]
    async fn pending_drafts_do_not_reappear_after_stop_or_completion() {
        for stopped in [true, false] {
            let k = Kit::new();
            let stop = super::super::Stop::default();
            let drafts = Drafts::new(&k.core, WS, "question", &stop, false);
            lock(&drafts.state).sent = now() + 100;
            drafts.send("", "尾字。");
            let state = drafts.state.clone();
            if stopped {
                stop.stop();
            } else {
                drop(drafts);
            }
            drained(&state).await;
            assert!(!k.events.lock().unwrap().iter().any(|e| matches!(
                e,
                steve_types::BusEvent::Live(steve_types::LiveEvent::Work(
                    steve_types::WorkLiveEvent::SteveDraft { .. }
                ))
            )));
            assert_eq!(lock(&state).text, "尾字。");
        }
        let k = Kit::new();
        let stop = super::super::Stop::default();
        let drafts = Drafts::new(&k.core, WS, "im", &stop, true);
        drafts.send("", "private reply");
        assert!(!lock(&drafts.state).pending);
        assert!(!k.events.lock().unwrap().iter().any(|e| matches!(
            e,
            steve_types::BusEvent::Live(steve_types::LiveEvent::Work(
                steve_types::WorkLiveEvent::SteveDraft { .. }
            ))
        )));
    }
}
