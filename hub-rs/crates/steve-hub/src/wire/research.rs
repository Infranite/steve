//! The thread's research seam (`steve_core::thread::Researcher`) on the local Node's research jobs
//! (thread-routing R2, N0d-9): the Hub keeps the policy (admission, the attempts in order, the
//! prompt, the answer's shape, the statistics row, the audit), the Node runs each attempt as one
//! job on the chosen harness adapter with the three read-only layers, its events become drafts and
//! tool rows, and Steve's research tools reach the Hub through the Node's endpoint with the job's
//! grant. Without the local Node there is no research.

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
    /// The local Node's roots now: a run's directories are named under them. A directory under
    /// none of them cannot be read: the run fails, counted.
    pub roots: Arc<dyn Fn() -> Result<Vec<steve_node::Root>, String> + Send + Sync>,
}

pub struct ResearchAdapter {
    pub cfg: r::ResearchConfig,
    pub db: steve_core::repo::Db,
    pub data_dir: PathBuf,
    node: Option<NodeResearch>,
    admission: Admission,
}

/// The runs admitted: one per workspace, `max_global` at once, none once closed.
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
            cfg,
            db,
            data_dir,
            node: None,
            admission: Admission::default(),
        }
    }

    /// Runs research as jobs on the local Node.
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
                .filter(|p| p.device_id == "hub")
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
            full: q.full,
            brief: q.brief.clone(),
        });
        let cwd = q.project.as_ref().map(|p| PathBuf::from(&p.path));
        /* a follow-up continues the earlier session: the question alone is the prompt. One that has
        to start over asks the whole prompt, so it keeps the rules, the access and the brief */
        let (prompt, resume) = match &q.resume {
            Some(f) => (
                q.question.clone(),
                Some(r::Resume {
                    session: f.session.clone(),
                    model: f.model.clone(),
                    fresh: prompt,
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
            .filter(|p| p.device_id == "hub")
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
        harness: &str,
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
            harness: harness.to_string(),
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

    /// The run's end for the thread: its statistics row, the audit of a violation, and the
    /// thread's outcome.
    fn conclude(
        &self,
        q: &ResearchRequest,
        out: r::ResearchOutcome,
        usage: Vec<steve_core::gateway::ModelUsage>,
    ) -> ResearchOutcome {
        self.record(&q.ws, &out, usage, &q.models.harness);
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
            session: out.session,
            turns: out.turns as i64,
            first_text_ms: out.first_text_ms.map(|m| m as i64),
            error,
        }
    }

    /// A run as jobs on the local Node: admitted here, its attempts planned here
    /// ([`r::plan`]) and tried in order ([`r::drive`]), one job per attempt. A run whose
    /// directories are under none of the Node's roots (or whose roots cannot be read) fails,
    /// logged and counted ([`NodeRuntime::research_fallbacks`], in `/healthz`).
    async fn on_local_node(
        &self,
        node: &NodeResearch,
        q: &ResearchRequest,
        sink: &dyn ResearchSink,
    ) -> ResearchOutcome {
        if q.project.as_ref().is_some_and(|p| p.device_id != "hub") {
            return ResearchOutcome {
                end: RunEnd::Failed,
                error: Some("这个项目位于另一节点，当前研究不能读取其目录".into()),
                ..Default::default()
            };
        }
        let cfg = &self.cfg;
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
                "research: a directory is under none of the local Node's roots; it cannot be read"
            );
            node.runtime.research_fell_back();
            drop(slot);
            return ResearchOutcome {
                end: RunEnd::Failed,
                error: Some("这个项目的目录不在本机 Node 能读的范围里，研究不能读取它".into()),
                ..Default::default()
            };
        }
        let budget = req.budget.unwrap_or(cfg.budget);
        let deadline = steve_store::now()
            + i64::try_from(budget.hard.unwrap_or(NO_WALL_LIMIT).as_millis())
                .unwrap_or(i64::MAX / 2);
        // the guarantee a research job asks the Node's sandbox for (sandbox-exec, files read-only)
        let fs = || vec!["fs-readonly".to_string()];
        let sandbox = match cfg.sandbox {
            // the owner's own access: nothing is read-only
            _ if q.full => api::SandboxRequirement::None,
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
            full: q.full,
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
            text: String::new(),
            unavailable: false,
        };
        let last_err = r::drive(&plans, &mut runner).await;
        let ms = runner.t0.elapsed().as_millis() as u64;
        let usage = super::node_runtime::normalized_usage(
            std::mem::take(&mut runner.usage),
            runner.model.as_deref().unwrap_or_default(),
        )
        .into_iter()
        .map(steve_core::gateway::model_usage)
        .collect();
        let out = runner.outcome(&q.run, &q.question, last_err, ms);
        let out = self.conclude(q, out, usage);
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
    /// The latest visible draft, kept when the Node disappears before a completion arrives.
    text: String,
    unavailable: bool,
}

impl r::AttemptRunner for OnNode<'_> {
    async fn run(&mut self, plan: &r::AttemptPlan) -> r::Attempted {
        self.model = Some(plan.model.clone());
        self.last = None;
        self.usage.clear();
        self.text.clear();
        if self.stopped || self.stop.load(Ordering::SeqCst) || self.halt.load(Ordering::SeqCst) {
            self.stopped = true;
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
        let mut running = None;
        let mut tick = tokio::time::interval(Duration::from_millis(50));
        let mut health = tokio::time::interval(Duration::from_secs(1));
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
                        self.text = t;
                    }
                    Some(News::Tool(v)) => {
                        progressed = true;
                        let kind = serde_json::to_value(v.category)
                            .ok()
                            .and_then(|c| c.as_str().map(str::to_owned))
                            .unwrap_or_default();
                        let brief = v.target.unwrap_or_default();
                        let ok = match v.phase {
                            api::ToolPhase::Succeeded => true,
                            api::ToolPhase::Failed => false,
                            // what the run is doing now: a call that takes minutes is seen while it runs
                            _ => {
                                let now = (v.id, brief);
                                if running.as_ref() != Some(&now) {
                                    self.sink.tool_running(&v.name, &kind, &now.1);
                                    running = Some(now);
                                }
                                continue;
                            }
                        };
                        running = None;
                        self.sink.tool(&ToolSeen {
                            name: v.name,
                            kind,
                            brief,
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
                _ = health.tick() => {
                    if job.unavailable().unwrap_or(true) {
                        self.unavailable = true;
                        job.cancel();
                        break None;
                    }
                }
            }
        };
        let Some(c) = end else {
            return r::Attempted {
                killed: self.stopped || self.unavailable,
                progressed,
                error: Some(if self.unavailable {
                    "Node 连接中断，已保留当前内容并请求停止；尚未确认执行结束".into()
                } else {
                    "Node 没有回报这次研究的结果".into()
                }),
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
                .map(super::node_runtime::said)
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
            None if self.stopped && c.is_none_or(|c| c.outcome == api::Outcome::Cancelled) => {
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
            .filter(|text: &String| !text.is_empty())
            .unwrap_or_else(|| self.text.clone());
        let files_read = report.map(|j| j.files_read.clone()).unwrap_or_default();
        let (mut answer, offer, citations) = r::present(&status, &raw, question, &files_read);
        if self.unavailable && !answer.is_empty() {
            answer.push_str("\n\n（Node 连接中断，已请求停止，尚未确认执行结束。）");
        }
        r::ResearchOutcome {
            run_id: run.to_string(),
            status,
            model: self.model.clone(),
            session: c
                .and_then(|c| c.continuation.as_ref())
                .map(|h| h.to_string()),
            answer,
            offer,
            citations,
            files_read,
            turns: report.map_or(0, |j| j.turns),
            ms,
            first_text_ms: self.first_text_ms,
            sandboxed: report.is_some_and(|j| j.sandboxed),
            raw,
        }
    }
}

#[async_trait]
impl steve_core::thread::Researcher for ResearchAdapter {
    async fn shutdown(&self, grace: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + grace;
        self.admission.close();
        // each run ends once the Node said its attempt ended (its processes are gone by then)
        while !self.admission.is_empty() {
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        true
    }
    async fn research(&self, q: ResearchRequest, sink: &dyn ResearchSink) -> ResearchOutcome {
        match &self.node {
            Some(node) => self.on_local_node(node, &q, sink).await,
            None => ResearchOutcome {
                end: RunEnd::Failed,
                error: Some("研究在本机 Node 上运行，本机 Node 现在没有开启".into()),
                ..Default::default()
            },
        }
    }
}

#[cfg(all(test, unix))]
#[path = "research_stream_tests.rs"]
mod stream_tests;
