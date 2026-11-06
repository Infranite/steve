//! The thread's research seam (`steve_core::thread::Researcher`) on the read-only executor of
//! steve-research (thread-routing R2): the prompt is filled from what the thread collected, the run's
//! events become drafts and tool rows, and its end becomes the thread's `RunEnd`.
//!
//! With `STEVE_NODE_JOBS=research` (and the local Node) each attempt of a run is a job on the
//! local Node instead (N0d-9): the Hub keeps the policy (admission, the attempts in order, the
//! prompt, the answer's shape, the statistics row, the audit), the Node runs one model per job with
//! the same three read-only layers, and Steve's research tools reach the Hub through the Node's
//! endpoint with the job's grant.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use steve_core::thread::{ResearchOutcome, ResearchRequest, ResearchSink, RunEnd, ToolSeen};
use steve_node_api as api;
use steve_research as r;

use super::node_runtime::{News, NodeRuntime, ResearchAttempt, ResearchFile};

/// The deadline of a run on the Node when the setting names no wall time. The Node needs one, but
/// such a run has no wall limit in process (a conversation is not cut off), so it is far enough
/// never to end a live run; the idle detector and the turn and tool limits end a stuck one.
/// (Bounded so the harness's own clock can add it.)
pub const NO_WALL_LIMIT: Duration = Duration::from_secs(365 * 24 * 3600);

/// Research on the local Node's jobs.
pub struct NodeResearch {
    pub runtime: Arc<NodeRuntime>,
    /// The local Node's roots now: a run's directories are named under them. An error (the store,
    /// the placement) is logged and the run goes in process, counted as a fallback.
    pub roots: Arc<dyn Fn() -> Result<Vec<steve_node::Root>, String> + Send + Sync>,
}

pub struct ResearchAdapter {
    pub researcher: r::Researcher,
    pub db: steve_core::repo::Db,
    pub data_dir: PathBuf,
    node: Option<NodeResearch>,
    admission: Admission,
}

/// The runs admitted on the Node path: one per workspace, `max_global` at once, none once closed
/// (what [`r::Researcher::start`] enforces in process).
#[derive(Default)]
struct Admission(Mutex<Admitted>);

#[derive(Default)]
struct Admitted {
    closed: bool,
    /// run → (workspace, its shutdown flag)
    runs: HashMap<String, (String, Arc<AtomicBool>)>,
}

/// A run's place in the [`Admission`], given back when dropped.
struct Slot<'a> {
    admission: &'a Admission,
    run: String,
    halt: Arc<AtomicBool>,
}

impl Drop for Slot<'_> {
    fn drop(&mut self) {
        self.admission.0.lock().unwrap().runs.remove(&self.run);
    }
}

impl Admission {
    fn admit(&self, run: &str, ws: &str, max: usize) -> Result<Slot<'_>, r::ResearchError> {
        let mut a = self.0.lock().unwrap();
        if a.closed {
            return Err(r::ResearchError::Closed);
        }
        if let Some((rid, _)) = a.runs.iter().find(|(_, (w, _))| w == ws) {
            return Err(r::ResearchError::Busy {
                run_id: rid.clone(),
            });
        }
        if a.runs.len() >= max {
            return Err(r::ResearchError::Full);
        }
        let halt = Arc::new(AtomicBool::new(false));
        a.runs
            .insert(run.to_string(), (ws.to_string(), halt.clone()));
        Ok(Slot {
            admission: self,
            run: run.to_string(),
            halt,
        })
    }

    /// No new run; the running ones are told to stop.
    fn close(&self) {
        let mut a = self.0.lock().unwrap();
        a.closed = true;
        for (_, halt) in a.runs.values() {
            halt.store(true, Ordering::SeqCst);
        }
    }

    fn is_empty(&self) -> bool {
        self.0.lock().unwrap().runs.is_empty()
    }
}

/// The message for a run that was not admitted (zh-CN, stored with the answer).
fn refused(e: &r::ResearchError) -> String {
    match e {
        r::ResearchError::Busy { .. } => "这个工作空间已经有一个研究在跑".into(),
        r::ResearchError::Closed => "服务正在关闭，研究已停止".into(),
        r::ResearchError::Full => "同时在跑的研究太多，稍后再问".into(),
    }
}

impl ResearchAdapter {
    pub fn new(cfg: r::ResearchConfig, db: steve_core::repo::Db, data_dir: PathBuf) -> Self {
        if let Some(note) = cfg.startup_note() {
            tracing::info!("{note}");
        }
        ResearchAdapter {
            researcher: r::Researcher::new(cfg),
            db,
            data_dir,
            node: None,
            admission: Admission::default(),
        }
    }

    /// Runs research as jobs on the local Node (`STEVE_NODE_JOBS=research`).
    pub fn on_node(mut self, node: NodeResearch) -> Self {
        self.node = Some(node);
        self
    }

    fn request(&self, q: &ResearchRequest) -> r::ResearchRequest {
        let skip = q.history.len().saturating_sub(10);
        let prompt = r::prompt::render(&r::PromptInput {
            bot: q.main.name.clone(),
            style: format!("{}\n{}", q.main.persona, q.main.rules)
                .trim()
                .to_string(),
            context: q.context.clone(),
            dirs: q
                .readable
                .iter()
                .map(|p| r::DirInfo {
                    name: p.name.clone(),
                    path: p.path.clone(),
                    description: p.description.clone(),
                })
                .collect(),
            recent: q.history[skip..]
                .iter()
                .map(|m| {
                    let who = if m.author == "me" { "用户" } else { "Steve" };
                    (who.to_string(), m.text.clone())
                })
                .collect(),
            question: q.question.clone(),
            ui: if q.ui {
                steve_core::gateway::A2UI_HINT.to_string()
            } else {
                String::new()
            },
        });
        let cwd = q.project.as_ref().map(|p| PathBuf::from(&p.path));
        /* a follow-up continues the earlier session: the question alone is the prompt */
        let (prompt, resume) = match &q.resume {
            Some(f) => (
                q.question.clone(),
                Some(r::Resume {
                    claude_session: f.claude_session.clone(),
                    model: f.model.clone(),
                    prior_answer: f.prior_answer.clone(),
                }),
            ),
            None => (prompt, None),
        };
        let mut req = r::ResearchRequest::new(&q.ws, &q.question, prompt);
        req.run_id = Some(q.run.clone());
        req.resume = resume;
        req.mcp = q.mcp.as_ref().map(|m| r::McpAccess {
            url: m.url.clone(),
            token: m.token.clone(),
        });
        req.add_dirs = q
            .readable
            .iter()
            .map(|p| PathBuf::from(&p.path))
            .filter(|p| Some(p) != cwd.as_ref())
            .collect();
        req.cwd = cwd;
        if !q.models.models.is_empty() {
            req.models = Some(q.models.models.clone());
        }
        req.effort = q.models.effort.clone();
        req.harness = q.models.harness.clone();
        /* the question's own files, then the ones of the message it follows up on */
        let own = q.files.iter().map(|f| (f, false));
        let earlier = q.earlier_files.iter().map(|f| (f, true));
        req.attachments = own
            .chain(earlier)
            .filter_map(|(r, earlier)| {
                let f = self.db.file(&r.id).ok().flatten()?;
                Some(r::Attachment {
                    path: steve_core::attachments::blob_path(&self.data_dir, &f),
                    name: f.name.clone(),
                    mime: f.mime.clone(),
                    size: u64::try_from(f.size).unwrap_or(0),
                    earlier,
                })
            })
            .collect();
        req
    }
}

impl ResearchAdapter {
    /// Books the run's tokens as a `research` call (stats.md §2.4), on the model that ran it.
    fn record(
        &self,
        ws: &str,
        out: &r::ResearchOutcome,
        usage: Vec<steve_core::gateway::ModelUsage>,
        codex: bool,
    ) {
        let model = out.model.clone().unwrap_or_default();
        let ok = matches!(out.status, r::RunStatus::Done);
        let a = steve_core::gateway::Attempt {
            result: if ok {
                Ok(Default::default())
            } else {
                Err(steve_core::CoreError::Msg("research".into()))
            },
            attempts: 1,
            duration_ms: out.ms,
            usage,
            model,
            failures: vec![],
            harness: if codex {
                steve_core::gateway::CODEX_HARNESS
            } else {
                ""
            },
        };
        let tag = steve_core::stats::CallTag {
            purpose: "research",
            workspace_id: ws.to_string(),
            item_id: None,
        };
        if let Err(e) = steve_core::stats::record(self.db.store(), &a, &tag) {
            tracing::warn!("stats: research usage not recorded: {e}");
        }
    }

    /// The tokens of an in-process run, from the harness's closing result.
    fn usage_of_result(out: &r::ResearchOutcome) -> (Vec<steve_core::gateway::ModelUsage>, bool) {
        let model = out.model.clone().unwrap_or_default();
        let codex = out.result.as_ref().is_some_and(|d| d["harness"] == "codex");
        let usage = match out.result.as_ref() {
            // Codex reports one usage object for the turn, input counting the cached part
            Some(d) if codex => d
                .get("usage")
                .filter(|u| u.is_object())
                .map(|u| {
                    vec![steve_core::gateway::ModelUsage {
                        model: model.clone(),
                        tokens: steve_stats::normalize(
                            "codex",
                            steve_stats::RawUsage::from_json(u),
                            true,
                        ),
                        cost_reported: None,
                        detail: serde_json::json!({"scope": "main"}),
                    }]
                })
                .unwrap_or_default(),
            Some(d) => steve_core::gateway::usage_of(d, &model),
            None => vec![],
        };
        (usage, codex)
    }

    /// The run's end for the thread: its statistics row, the audit of a violation, and the
    /// thread's outcome.
    fn conclude(
        &self,
        q: &ResearchRequest,
        out: r::ResearchOutcome,
        usage: Vec<steve_core::gateway::ModelUsage>,
        codex: bool,
    ) -> ResearchOutcome {
        self.record(&q.ws, &out, usage, codex);
        let (end, error) = match &out.status {
            r::RunStatus::Done => (RunEnd::Done, None),
            r::RunStatus::Stopped => (RunEnd::Stopped, None),
            r::RunStatus::Budget(b) => (RunEnd::Budget, Some(format!("{b:?}"))),
            r::RunStatus::Violated(v) => {
                let detail = serde_json::json!({"run": out.run_id, "violation": format!("{v:?}")});
                if let Err(e) = self.db.audit(
                    "steve",
                    "thread.research_violation",
                    &out.run_id,
                    &detail,
                    &q.ws,
                ) {
                    tracing::error!("audit research violation: {e}");
                }
                (RunEnd::Violated, Some(format!("{v:?}")))
            }
            r::RunStatus::Failed(e) => (RunEnd::Failed, Some(e.clone())),
        };
        ResearchOutcome {
            text: out.answer,
            offer: out.offer.map(|o| steve_core::gateway::Offer {
                title: o.title,
                goal: o.goal,
            }),
            end,
            model: out.model,
            claude_session: out.claude_session,
            turns: out.turns as i64,
            first_text_ms: out.first_text_ms.map(|m| m as i64),
            error,
        }
    }

    /// A run in this process ([`r::Researcher`]).
    async fn in_process(&self, q: &ResearchRequest, sink: &dyn ResearchSink) -> ResearchOutcome {
        let mut run = match self.researcher.start(self.request(q)) {
            Ok(run) => run,
            Err(e) => {
                return ResearchOutcome {
                    end: RunEnd::Failed,
                    error: Some(refused(&e)),
                    ..Default::default()
                };
            }
        };
        let stopper = run.stopper();
        let mut stopped = false;
        loop {
            tokio::select! {
                ev = run.next_event() => match ev {
                    None => break,
                    Some(r::ResearchEvent::Text { text }) => sink.text(&text),
                    Some(r::ResearchEvent::ToolDone(t)) => sink.tool(&ToolSeen {
                        name: t.name,
                        brief: t.brief,
                        ok: t.ok.unwrap_or(true),
                        error: t.error.map(|e| e.code().to_string()),
                        ms: t.ms as i64,
                    }),
                    Some(_) => {}
                },
                _ = tokio::time::sleep(Duration::from_millis(50)) => {
                    if !stopped && q.stop.load(Ordering::SeqCst) {
                        stopped = true;
                        stopper.stop();
                    }
                }
            }
        }
        let out = run.finish().await;
        let (usage, codex) = Self::usage_of_result(&out);
        self.conclude(q, out, usage, codex)
    }

    /// A run as jobs on the local Node: admitted here, its attempts planned here
    /// ([`r::plan`]) and tried in order ([`r::drive`]), one job per attempt. A run whose
    /// directories are under none of the Node's roots (or whose roots cannot be read) runs in
    /// process, logged and counted ([`NodeRuntime::research_fallbacks`], in `/healthz`).
    async fn on_local_node(
        &self,
        node: &NodeResearch,
        q: &ResearchRequest,
        sink: &dyn ResearchSink,
    ) -> ResearchOutcome {
        let cfg = self.researcher.config();
        let slot = match self.admission.admit(&q.run, &q.ws, cfg.max_global) {
            Ok(s) => s,
            Err(e) => {
                return ResearchOutcome {
                    end: RunEnd::Failed,
                    error: Some(refused(&e)),
                    ..Default::default()
                };
            }
        };
        let req = self.request(q);
        let needs_roots = req.cwd.is_some() || !req.add_dirs.is_empty();
        let roots = if needs_roots {
            (node.roots)().unwrap_or_else(|e| {
                tracing::error!(run = %q.run, "research: the local Node's roots: {e}");
                vec![]
            })
        } else {
            vec![]
        };
        let place = |p: &Path| super::workspaces::within(&roots, p);
        let cwd = req.cwd.as_deref().map(place);
        let readable: Vec<_> = req.add_dirs.iter().map(|p| place(p)).collect();
        if cwd.as_ref().is_some_and(Option::is_none) || readable.iter().any(Option::is_none) {
            tracing::warn!(
                run = %q.run,
                "research: a directory is under none of the local Node's roots; it runs in process"
            );
            node.runtime.research_fell_back();
            let out = self.in_process(q, sink).await;
            drop(slot);
            return out;
        }
        let budget = req.budget.unwrap_or(cfg.budget);
        let deadline = steve_store::now()
            + i64::try_from(budget.hard.unwrap_or(NO_WALL_LIMIT).as_millis())
                .unwrap_or(i64::MAX / 2);
        // the guarantee a research job asks the Node's sandbox for (sandbox-exec, files read-only)
        let fs = || vec!["fs-readonly".to_string()];
        let sandbox = match cfg.sandbox {
            r::SandboxMode::Off => api::SandboxRequirement::None,
            r::SandboxMode::Auto => api::SandboxRequirement::AllowFallback {
                preferred: fs(),
                acceptable: vec![],
            },
            r::SandboxMode::Required => api::SandboxRequirement::Required { guarantees: fs() },
        };
        let base = ResearchAttempt {
            run: q.run.clone(),
            workspace: q.ws.clone(),
            harness: req.harness.clone(),
            model: String::new(),
            effort: req.effort.clone(),
            prompt: String::new(),
            resume: None,
            files: self.files(q),
            cwd: cwd.flatten(),
            readable: readable.into_iter().flatten().collect(),
            budget: api::ResearchBudget {
                max_turns: budget.max_turns,
                max_tools: budget.max_tools,
                idle_ms: u64::try_from(budget.idle.as_millis())
                    .unwrap_or(u64::MAX)
                    .max(1),
            },
            sandbox,
            steve_tools: q.mcp.is_some(),
            deadline,
        };
        let plans = r::plan(&req, cfg);
        let mut runner = OnNode {
            runtime: node.runtime.clone(),
            base,
            sink,
            stop: q.stop.clone(),
            halt: slot.halt.clone(),
            t0: std::time::Instant::now(),
            stopped: false,
            first_text_ms: None,
            model: None,
            last: None,
            usage: vec![],
        };
        let last_err = r::drive(&plans, &mut runner).await;
        let ms = runner.t0.elapsed().as_millis() as u64;
        let codex = req.harness == "codex" && !runner.usage.is_empty();
        let usage = std::mem::take(&mut runner.usage)
            .into_iter()
            .map(steve_core::gateway::model_usage)
            .collect();
        let out = runner.outcome(&q.run, &q.question, last_err, ms);
        let out = self.conclude(q, out, usage, codex);
        drop(slot);
        out
    }

    /// The question's attachments (then those of the message it follows up on) as the job's
    /// files, each under a plain name of its own.
    fn files(&self, q: &ResearchRequest) -> Vec<ResearchFile> {
        let own = q.files.iter().map(|f| (f, false));
        let earlier = q.earlier_files.iter().map(|f| (f, true));
        let mut used: Vec<String> = vec![];
        own.chain(earlier)
            .enumerate()
            .filter_map(|(n, (f, earlier))| {
                let row = self.db.file(&f.id).ok().flatten()?;
                let mut name = file_name(&row.name, n);
                if used.contains(&name) {
                    name = format!("{}-{name}", n + 1);
                }
                used.push(name.clone());
                Some(ResearchFile {
                    id: row.id,
                    name,
                    earlier,
                })
            })
            .collect()
    }
}

/// A name the Node keeps a file under: one plain path segment (separators and leading dots go,
/// as the run's own copies are named), else a neutral one.
fn file_name(name: &str, n: usize) -> String {
    let s: String = name
        .chars()
        .map(|c| {
            if matches!(c, '/' | '\\' | '\0') {
                '_'
            } else {
                c
            }
        })
        .collect();
    let s = s.trim().trim_start_matches('.').to_string();
    let ok = !s.is_empty() && api::RelPath::new(format!("{}{s}", api::ATTACHMENTS_DIR)).is_ok();
    if ok {
        s
    } else {
        format!("attachment-{}", n + 1)
    }
}

/// The attempts of one run on the Node, one job each, into one view of the run.
struct OnNode<'a> {
    runtime: Arc<NodeRuntime>,
    /// What every attempt shares (the plan fills in model, prompt and session).
    base: ResearchAttempt,
    sink: &'a dyn ResearchSink,
    /// The user's stop.
    stop: Arc<AtomicBool>,
    /// The Hub's shutdown.
    halt: Arc<AtomicBool>,
    t0: std::time::Instant,
    /// The Hub asked the Node to stop the attempt.
    stopped: bool,
    first_text_ms: Option<u64>,
    /// The model of the last attempt.
    model: Option<String>,
    /// The last attempt's end.
    last: Option<api::Completion>,
    /// The last attempt's usage (the statistics book the attempt that ended the run).
    usage: Vec<api::UsageReport>,
}

impl r::AttemptRunner for OnNode<'_> {
    async fn run(&mut self, plan: &r::AttemptPlan) -> r::Attempted {
        self.model = Some(plan.model.clone());
        self.last = None;
        self.usage.clear();
        if self.stopped {
            return r::Attempted {
                killed: true,
                ..Default::default()
            };
        }
        let a = ResearchAttempt {
            model: plan.model.clone(),
            prompt: plan.prompt.clone(),
            resume: plan.resume.clone(),
            ..self.base.clone()
        };
        let mut job = match self.runtime.start_research(a).await {
            Ok(j) => j,
            Err(e) => {
                return r::Attempted {
                    error: Some(format!("Node 没能接下这次研究：{e}")),
                    ..Default::default()
                };
            }
        };
        let mut progressed = false;
        let mut ids = std::collections::HashSet::new();
        let mut tick = tokio::time::interval(Duration::from_millis(50));
        let end = loop {
            tokio::select! {
                n = job.next() => match n {
                    Some(News::Text(t)) => {
                        if !t.is_empty() {
                            progressed = true;
                            self.first_text_ms
                                .get_or_insert(self.t0.elapsed().as_millis() as u64);
                        }
                        self.sink.text(&t);
                    }
                    Some(News::Tool(v)) => {
                        progressed = true;
                        let ok = match v.phase {
                            api::ToolPhase::Succeeded => true,
                            api::ToolPhase::Failed => false,
                            _ => continue,
                        };
                        self.sink.tool(&ToolSeen {
                            name: v.name,
                            brief: v.target.unwrap_or_default(),
                            ok,
                            error: v.error,
                            ms: v.duration_ms.unwrap_or(0) as i64,
                        });
                    }
                    Some(News::Usage(u)) => {
                        if ids.insert(u.report_id.clone()) {
                            self.usage.push(u);
                        }
                    }
                    Some(News::End(c)) => break Some(c),
                    None => break None,
                },
                _ = tick.tick() => {
                    if !self.stopped
                        && (self.stop.load(Ordering::SeqCst) || self.halt.load(Ordering::SeqCst))
                    {
                        self.stopped = true;
                        job.cancel();
                    }
                }
            }
        };
        let Some(c) = end else {
            return r::Attempted {
                killed: self.stopped,
                progressed,
                error: Some("Node 没有回报这次研究的结果".into()),
                ..Default::default()
            };
        };
        let report = c.job.as_ref().map(|j| &j.end);
        let killed = self.stopped
            || matches!(
                report,
                Some(
                    api::JobEnd::Stopped
                        | api::JobEnd::Budget { .. }
                        | api::JobEnd::Violated { .. }
                )
            );
        let error = (c.outcome != api::Outcome::Succeeded).then(|| {
            c.error
                .as_ref()
                .map(|e| e.safe_detail.clone())
                .unwrap_or_else(|| c.stop_reason.clone())
        });
        let result = matches!(report, Some(api::JobEnd::Done)).then_some(Ok(()));
        self.last = Some(c);
        r::Attempted {
            killed,
            progressed,
            result,
            error,
        }
    }
}

impl OnNode<'_> {
    /// The run's outcome from its last attempt, shaped as an in-process run's
    /// ([`r::present`]: the answer without its offer line, the end mark, the citations).
    fn outcome(&self, run: &str, question: &str, last_err: String, ms: u64) -> r::ResearchOutcome {
        let c = self.last.as_ref();
        let report = c.and_then(|c| c.job.as_ref());
        let status = match report.map(|j| &j.end) {
            Some(api::JobEnd::Done) => r::RunStatus::Done,
            Some(api::JobEnd::Failed) => r::RunStatus::Failed(last_err),
            Some(api::JobEnd::Stopped) => r::RunStatus::Stopped,
            Some(api::JobEnd::Budget { hit }) => r::RunStatus::Budget(match hit {
                api::BudgetHit::Time => r::BudgetHit::Time,
                api::BudgetHit::Turns => r::BudgetHit::Turns,
                api::BudgetHit::Tools => r::BudgetHit::Tools,
                api::BudgetHit::Idle => r::BudgetHit::Idle,
            }),
            Some(api::JobEnd::Violated { rule, tool, target }) => {
                let (tool, path) = (tool.clone(), target.clone().unwrap_or_default());
                r::RunStatus::Violated(match rule {
                    api::ViolationRule::Tool => r::Violation::Tool { tool },
                    api::ViolationRule::OutOfScope => r::Violation::OutOfScope { tool, path },
                    api::ViolationRule::Sensitive => r::Violation::Sensitive { tool, path },
                    api::ViolationRule::BadInput => r::Violation::BadInput { tool },
                })
            }
            // ended by the Hub before the Node ran it
            None if self.stopped && c.is_some_and(|c| c.outcome == api::Outcome::Cancelled) => {
                r::RunStatus::Stopped
            }
            None => r::RunStatus::Failed(last_err),
        };
        let raw: String = c
            .map(|c| {
                c.result
                    .iter()
                    .filter_map(|p| match p {
                        api::Part::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect()
            })
            .unwrap_or_default();
        let files_read = report.map(|j| j.files_read.clone()).unwrap_or_default();
        let (answer, offer, citations) = r::present(&status, &raw, question, &files_read);
        r::ResearchOutcome {
            run_id: run.to_string(),
            status,
            model: self.model.clone(),
            claude_session: c
                .and_then(|c| c.continuation.as_ref())
                .map(|h| h.to_string()),
            answer,
            offer,
            citations,
            files_read,
            tools: vec![],
            turns: report.map_or(0, |j| j.turns),
            ms,
            first_text_ms: self.first_text_ms,
            cost_usd: None,
            sandboxed: report.is_some_and(|j| j.sandboxed),
            result: None,
            raw,
        }
    }
}

#[async_trait]
impl steve_core::thread::Researcher for ResearchAdapter {
    async fn shutdown(&self, grace: Duration) -> bool {
        if self.node.is_none() {
            return self.researcher.shutdown(grace).await;
        }
        let deadline = tokio::time::Instant::now() + grace;
        self.admission.close();
        let in_process = self.researcher.shutdown(grace);
        let on_node = async {
            // each run ends once the Node said its attempt ended (its processes are gone by then)
            while !self.admission.is_empty() {
                if tokio::time::Instant::now() >= deadline {
                    return false;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            true
        };
        let (a, b) = tokio::join!(in_process, on_node);
        a && b
    }
    async fn research(&self, q: ResearchRequest, sink: &dyn ResearchSink) -> ResearchOutcome {
        match &self.node {
            Some(node) => self.on_local_node(node, &q, sink).await,
            None => self.in_process(&q, sink).await,
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex};
    use steve_core::thread::Researcher as _;

    #[derive(Default)]
    struct Seen {
        text: Mutex<Vec<String>>,
        tools: Mutex<Vec<ToolSeen>>,
    }
    impl ResearchSink for Seen {
        fn text(&self, t: &str) {
            self.text.lock().unwrap().push(t.into());
        }
        fn tool(&self, t: &ToolSeen) {
            self.tools.lock().unwrap().push(t.clone());
        }
    }

    fn bot() -> steve_core::repo::Bot {
        steve_core::repo::Bot {
            id: "steve".into(),
            workspace_id: "default".into(),
            name: "Steve".into(),
            role: "r".into(),
            main: true,
            harness: "claude-code".into(),
            model: None,
            reasoning_effort: None,
            rules: "说重点。".into(),
            accept_mode: "inherit".into(),
            perm_mode: "default".into(),
            enabled: true,
            revision: 1,
            updated: 0,
            persona: String::new(),
            proactive: 1,
            avatar: Default::default(),
            default_project: None,
            prompt: None,
            device: None,
        }
    }

    /// The thread's request reaches the executor (prompt, cwd, run id), its events come back as drafts
    /// and tool rows, and its end and offer map onto the thread's.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_run_through_the_executor() {
        use std::os::unix::fs::PermissionsExt;
        let t = tempfile::tempdir().unwrap();
        let root = t.path().canonicalize().unwrap();
        let proj = root.join("proj");
        std::fs::create_dir_all(proj.join("src")).unwrap();
        std::fs::write(proj.join("src/lib.rs"), "fn add() {}\n").unwrap();
        let lib = proj.join("src/lib.rs").display().to_string();
        let lines = [
            r#"{"type":"system","subtype":"init","session_id":"sess-1","model":"claude-sonnet-5-5"}"#.to_string(),
            r#"{"type":"stream_event","event":{"type":"message_start","message":{"id":"m1"}}}"#.to_string(),
            serde_json::json!({"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","id":"t1","name":"Read","input":{"file_path": lib}}]}}).to_string(),
            serde_json::json!({"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"1\tfn add() {}"}]}}).to_string(),
            r#"{"type":"stream_event","event":{"type":"message_start","message":{"id":"m2"}}}"#.to_string(),
            serde_json::json!({"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"add 什么都不做。"}}}).to_string(),
            serde_json::json!({"type":"result","subtype":"success","is_error":false,"result":"add 什么都不做。\nOFFER: {\"title\":\"实现 add\",\"goal\":\"让 add 返回和\"}","session_id":"sess-1"}).to_string(),
        ];
        let args = root.join("args.txt");
        let body = lines
            .iter()
            .map(|l| format!("printf '%s\\n' '{}'", l.replace('\'', "'\\''")))
            .collect::<Vec<_>>()
            .join("\n");
        let exe = root.join("claude.sh");
        std::fs::write(
            &exe,
            format!(
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > '{}'\n{body}\n",
                args.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&exe, std::fs::Permissions::from_mode(0o755)).unwrap();
        let mut cfg = r::ResearchConfig::new(&exe, root.join("scratch"));
        cfg.models = vec!["claude-sonnet-5-5".into()];
        cfg.home = root.join("home");
        cfg.claude_dir = root.join("claude");
        cfg.sandbox = r::SandboxMode::Off;
        let store =
            steve_store::Store::open(root.join("db.sqlite3"), steve_store::NoPublisher).unwrap();
        let a = ResearchAdapter::new(cfg, steve_core::repo::Db::new(store), root.join("data"));
        let project = steve_core::repo::Project {
            id: "prj".into(),
            workspace_id: "default".into(),
            name: "add".into(),
            description: "加法".into(),
            device_id: "hub".into(),
            path: proj.display().to_string(),
            kind: "dir".into(),
            worktree: false,
            default_bot: None,
            release: None,
            archived: false,
            created: 0,
            updated: 0,
        };
        let seen = Seen::default();
        let req = |run: &str, models: steve_core::models::Choice| ResearchRequest {
            run: run.into(),
            ws: "default".into(),
            mid: "msg_1".into(),
            user: None,
            question: "add 怎么工作？".into(),
            history: vec![],
            main: bot(),
            context: String::new(),
            project: Some(project.clone()),
            readable: vec![project.clone()],
            files: vec![],
            earlier_files: vec![],
            resume: None,
            stop: Arc::new(AtomicBool::new(false)),
            mcp: None,
            models,
            ui: true,
        };
        let out = a.research(req("tr_1", Default::default()), &seen).await;
        assert_eq!(out.end, RunEnd::Done, "{out:?}");
        assert_eq!(out.text, "add 什么都不做。");
        assert_eq!(out.offer.unwrap().title, "实现 add");
        assert_eq!(out.claude_session.as_deref(), Some("sess-1"));
        assert_eq!(out.model.as_deref(), Some("claude-sonnet-5-5"));
        assert_eq!(
            seen.text.lock().unwrap().last().unwrap(),
            "add 什么都不做。"
        );
        let tools = seen.tools.lock().unwrap().clone();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0].name, "Read");
        let argv = std::fs::read_to_string(&args).unwrap();
        assert!(
            argv.contains("add 怎么工作？"),
            "the prompt carries the question"
        );
        assert!(argv.contains("说重点。"), "and the bot's rules");
        let flag = |argv: &str, f: &str| {
            let l: Vec<&str> = argv.lines().collect();
            l.iter().position(|x| *x == f).map(|i| l[i + 1].to_string())
        };
        assert_eq!(flag(&argv, "--model").as_deref(), Some("claude-sonnet-5-5"));
        assert_eq!(flag(&argv, "--effort"), None);

        /* 设置 · 模型 · 就地研究: the request's models and effort reach the command line */
        let out = a
            .research(
                req(
                    "tr_2",
                    steve_core::models::Choice {
                        harness: "claude-code".into(),
                        models: vec!["claude-opus-5-5".into()],
                        effort: Some("max".into()),
                    },
                ),
                &seen,
            )
            .await;
        assert_eq!(out.end, RunEnd::Done, "{out:?}");
        assert_eq!(out.model.as_deref(), Some("claude-opus-5-5"));
        let argv = std::fs::read_to_string(&args).unwrap();
        assert_eq!(flag(&argv, "--model").as_deref(), Some("claude-opus-5-5"));
        assert_eq!(flag(&argv, "--effort").as_deref(), Some("max"));

        /* both runs are in the statistics as research, on the model that ran */
        let rows: Vec<(String, String)> =
            a.db.store()
                .with_conn(|c| {
                    let mut st = c.prepare(
                    "SELECT purpose, model FROM usage_records WHERE source = 'gateway' ORDER BY id",
                )?;
                    let v = st
                        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                        .collect::<rusqlite::Result<Vec<_>>>()?;
                    Ok(v)
                })
                .unwrap();
        assert_eq!(
            rows,
            [
                ("research".to_string(), "claude-sonnet-5-5".to_string()),
                ("research".to_string(), "claude-opus-5-5".to_string())
            ]
        );
    }
}
