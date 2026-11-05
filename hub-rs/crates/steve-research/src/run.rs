//! The executor: starts a research run, streams its events, enforces the budget and the guard, stops it,
//! and assembles the outcome.

use crate::answer::{self, Citation, Offer};
use crate::command::{self, CommandPlan};
use crate::guard::{self, Scope, Violation};
use crate::sandbox::{self, SandboxMode};
use crate::stream::{StreamItem, parse_codex_line, parse_line};
use serde::Serialize;
use serde_json::Value;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncReadExt};
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;
use tracing::Instrument;

/// A run whose harness says nothing (no text, no tool call, no event at all) for this long is stuck and is
/// ended. This is not a limit on how long a conversation may take: a run that keeps working never ends by
/// time. Setting `thread.research.idle_seconds` overrides it.
pub const DEFAULT_IDLE: Duration = Duration::from_secs(600);
/// Default research models (`STEVE_MODELS_RESEARCH`), non-kiro.
pub const DEFAULT_MODELS: &str = "claude-sonnet-5-5,claude-opus-5-5";

/// Limits of one run (setting `thread.research`). By default a run is bounded by nothing but its own end,
/// the user's stop and the stall detector; the wall time, turns and tool calls are only limited when the
/// setting names a limit (decided by the user on 2026-11-04: conversations must not be cut off).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    /// Wall time; at the limit the process is killed. `None`: none.
    pub hard: Option<Duration>,
    /// After this the thread shows "还在读…" ([`ResearchEvent::StillReading`]); only a notice.
    pub soft: Duration,
    /// Model turns (`--max-turns`, and counted by the Hub too). `None`: none.
    pub max_turns: Option<u32>,
    /// Tool calls counted by the Hub. `None`: none.
    pub max_tools: Option<u32>,
    /// No output from the harness for this long: stuck, ended.
    pub idle: Duration,
}

impl Default for Budget {
    fn default() -> Self {
        Budget {
            hard: None,
            soft: Duration::from_secs(90),
            max_turns: None,
            max_tools: None,
            idle: DEFAULT_IDLE,
        }
    }
}

impl Budget {
    /// From the `thread.research` setting (`{"seconds","soft_seconds","turns","tools","idle_seconds"}`),
    /// defaults for the rest.
    pub fn from_setting(v: &Value) -> Self {
        let d = Budget::default();
        let n = |k: &str| v.get(k).and_then(Value::as_u64).filter(|x| *x > 0);
        Budget {
            hard: n("seconds").map(Duration::from_secs).or(d.hard),
            soft: n("soft_seconds").map_or(d.soft, Duration::from_secs),
            max_turns: n("turns").map(|x| x as u32).or(d.max_turns),
            max_tools: n("tools").map(|x| x as u32).or(d.max_tools),
            idle: n("idle_seconds").map_or(d.idle, Duration::from_secs),
        }
    }
}

/// `STEVE_MODELS_RESEARCH` (comma-separated), or [`DEFAULT_MODELS`].
pub fn models_from_env() -> Vec<String> {
    let v = std::env::var("STEVE_MODELS_RESEARCH").unwrap_or_default();
    let list: Vec<String> = v
        .split(',')
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect();
    if list.is_empty() {
        DEFAULT_MODELS.split(',').map(String::from).collect()
    } else {
        list
    }
}

/// How the Hub runs research.
#[derive(Debug, Clone)]
pub struct ResearchConfig {
    /// The harness (`CLAUDE_CODE_EXECUTABLE`).
    pub executable: PathBuf,
    /// Tried in order until one starts answering (`STEVE_MODELS_RESEARCH`).
    pub models: Vec<String>,
    /// `STEVE_HOME/scratch/research`; each run gets `<scratch>/<run id>/`.
    pub scratch: PathBuf,
    /// Whose secret directories (`~/.ssh`, `~/.steve`, …) are off limits.
    pub home: PathBuf,
    /// The harness's own state (`~/.claude`), the one place outside the run directory it may write.
    pub claude_dir: PathBuf,
    /// Where each run's attachment copies go (`<files_root>/<run id>/`): outside `~/.steve`, whose reads
    /// stay denied, so the run can read them. Removed when the run ends.
    pub files_root: PathBuf,
    pub budget: Budget,
    pub sandbox: SandboxMode,
    /// Runs at once across all workspaces (one per workspace always).
    pub max_global: usize,
    /// Layer 2 switch; only tests of layer 1 or 3 turn it off.
    #[doc(hidden)]
    pub guard: bool,
    /// `codex` for runs on Codex (Steve's harness or the research override); `None`: such runs fail.
    pub codex_executable: Option<PathBuf>,
}

impl ResearchConfig {
    /// Defaults: models from the environment, `$HOME`, `~/.claude`, the default budget, sandbox when available.
    pub fn new(executable: impl Into<PathBuf>, scratch: impl Into<PathBuf>) -> Self {
        let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from);
        ResearchConfig {
            executable: executable.into(),
            models: models_from_env(),
            scratch: scratch.into(),
            claude_dir: home.join(".claude"),
            files_root: std::env::temp_dir().join("steve-research-files"),
            home,
            budget: Budget::default(),
            sandbox: SandboxMode::Auto,
            max_global: 3,
            guard: true,
            codex_executable: None,
        }
    }

    /// The line to log at startup when the platform has no system sandbox.
    pub fn startup_note(&self) -> Option<&'static str> {
        (self.sandbox != SandboxMode::Off && !sandbox::available()).then_some(
            "research: sandbox-exec not available; read-only relies on the harness allowlist and the Hub guard",
        )
    }
}

/// Steve's MCP endpoint and the run's own token (the MCP side lists only the read-only tools for it).
#[derive(Debug, Clone)]
pub struct McpAccess {
    pub url: String,
    pub token: String,
}

/// Continue an earlier run's harness session (`--resume`); same model only. When that model fails, a fresh
/// run starts with `prior_answer` as context instead.
#[derive(Debug, Clone)]
pub struct Resume {
    pub claude_session: String,
    pub model: String,
    pub prior_answer: String,
}

/// One research run to start.
#[derive(Debug, Clone)]
pub struct ResearchRequest {
    /// The `thread_runs.id`; generated (`tr_…`) when absent.
    pub run_id: Option<String>,
    pub workspace: String,
    /// The user's question (for the "go deeper" offer when a budget cuts the run short).
    pub question: String,
    /// The full prompt ([`crate::prompt::render`]), or a follow-up question when resuming.
    pub prompt: String,
    /// The chosen project's directory; `None` runs in the run's scratch directory.
    pub cwd: Option<PathBuf>,
    /// The workspace's other project directories.
    pub add_dirs: Vec<PathBuf>,
    /// The message's attachments (and those of the message it follows up on): copied where the run may
    /// read them, named in the prompt, images inline.
    pub attachments: Vec<crate::attach::Attachment>,
    pub mcp: Option<McpAccess>,
    pub resume: Option<Resume>,
    /// Overrides the configured budget (setting `thread.research`).
    pub budget: Option<Budget>,
    /// Overrides the configured models (设置 · 模型), tried in order.
    pub models: Option<Vec<String>>,
    /// `--effort` (low, medium, high, xhigh, max); `None` leaves it to the harness.
    pub effort: Option<String>,
    /// `claude-code` (default) or `codex` (设置 · 模型: Steve's harness or the research override).
    pub harness: String,
}

impl ResearchRequest {
    pub fn new(
        workspace: impl Into<String>,
        question: impl Into<String>,
        prompt: impl Into<String>,
    ) -> Self {
        ResearchRequest {
            run_id: None,
            workspace: workspace.into(),
            question: question.into(),
            prompt: prompt.into(),
            cwd: None,
            add_dirs: Vec::new(),
            attachments: Vec::new(),
            mcp: None,
            resume: None,
            budget: None,
            models: None,
            effort: None,
            harness: "claude-code".into(),
        }
    }
}

/// One tool call of the run (`meta.steps.tools`, `steve.tool`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ToolSeen {
    pub id: String,
    pub name: String,
    pub brief: String,
    /// `None` while the harness has not answered it.
    pub ok: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<crate::stream::ToolError>,
    pub ms: u64,
}

/// What a run reports while it goes.
#[derive(Debug, Clone, PartialEq)]
pub enum ResearchEvent {
    /// An attempt started with this model.
    Started { model: String },
    /// The harness session id (keep it for `--resume`).
    Session { claude_session: String },
    /// The answer so far (the model's current message, whole). Throttle drafts on the caller side.
    Text { text: String },
    /// A tool call began (it already passed the guard).
    ToolStarted(ToolSeen),
    /// A tool call returned.
    ToolDone(ToolSeen),
    /// The soft time limit passed ("还在读…").
    StillReading,
}

/// Which budget ended a run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetHit {
    Time,
    Turns,
    Tools,
    /// The harness said nothing for [`Budget::idle`]: stuck.
    Idle,
}

/// How a run ended (`thread_runs.status`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunStatus {
    Done,
    Stopped,
    Budget(BudgetHit),
    Violated(Violation),
    Failed(String),
}

impl RunStatus {
    /// The `thread_runs.status` value.
    pub fn as_str(&self) -> &'static str {
        match self {
            RunStatus::Done => "done",
            RunStatus::Stopped => "stopped",
            RunStatus::Budget(_) => "budget",
            RunStatus::Violated(_) => "violated",
            RunStatus::Failed(_) => "failed",
        }
    }

    /// `meta.steps.research.stopped`.
    pub fn stopped(&self) -> Option<&'static str> {
        match self {
            RunStatus::Stopped => Some("user"),
            RunStatus::Budget(_) => Some("budget"),
            RunStatus::Violated(_) => Some("violation"),
            _ => None,
        }
    }
}

/// The result of a run.
#[derive(Debug, Clone)]
pub struct ResearchOutcome {
    pub run_id: String,
    pub status: RunStatus,
    /// The model of the attempt that answered (the one to `--resume` with).
    pub model: Option<String>,
    pub claude_session: Option<String>,
    /// What the thread shows: the answer (or the part written before the end) without the `OFFER:` line,
    /// with the end mark of a stopped, cut or violating run.
    pub answer: String,
    pub offer: Option<Offer>,
    pub citations: Vec<Citation>,
    /// Files the run read successfully, relative to its cwd when inside it.
    pub files_read: Vec<String>,
    pub tools: Vec<ToolSeen>,
    pub turns: u32,
    pub ms: u64,
    pub first_text_ms: Option<u64>,
    pub cost_usd: Option<f64>,
    /// Whether the harness ran under `sandbox-exec`.
    pub sandboxed: bool,
    /// The harness's closing `result` object (tokens per model for the statistics), when it sent one.
    pub result: Option<Value>,
    /// What the model wrote before the answer was shaped: the closing result's text, or the part
    /// written until the end ([`present`] turns it into `answer`, `offer` and `citations`).
    pub raw: String,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ResearchError {
    /// The workspace already has a run (one at a time); the caller queues.
    #[error("workspace already has a research run: {run_id}")]
    Busy { run_id: String },
    /// The global limit is reached.
    #[error("too many research runs at once")]
    Full,
    #[error("research service is shutting down")]
    Closed,
}

/// Stops a run from anywhere (the stop button).
#[derive(Debug, Clone)]
pub struct StopHandle(Arc<watch::Sender<bool>>);

impl StopHandle {
    pub fn stop(&self) {
        self.0.send_replace(true);
    }
}

/// A started run: its events, a way to stop it and its outcome.
pub struct ResearchRun {
    pub id: String,
    pub workspace: String,
    events: mpsc::UnboundedReceiver<ResearchEvent>,
    stop: StopHandle,
    task: tokio::task::JoinHandle<ResearchOutcome>,
}

impl ResearchRun {
    /// The next event; `None` once the run has ended.
    pub async fn next_event(&mut self) -> Option<ResearchEvent> {
        self.events.recv().await
    }

    pub fn stopper(&self) -> StopHandle {
        self.stop.clone()
    }

    /// Stop now; the outcome keeps what was written ([`RunStatus::Stopped`]).
    pub fn stop(&self) {
        self.stop.stop();
    }

    /// Waits for the end (events not yet read are dropped).
    pub async fn finish(self) -> ResearchOutcome {
        let id = self.id.clone();
        match self.task.await {
            Ok(o) => o,
            Err(e) => failed_outcome(id, format!("research task failed: {e}")),
        }
    }
}

struct Active {
    workspace: String,
    stop: StopHandle,
}

/// Runs research: at most one per workspace and `max_global` overall.
#[derive(Clone)]
pub struct Researcher {
    cfg: Arc<ResearchConfig>,
    active: Arc<Mutex<HashMap<String, Active>>>,
    processes: Arc<crate::processes::Processes>,
}

impl Researcher {
    pub fn new(cfg: ResearchConfig) -> Self {
        Researcher {
            cfg: Arc::new(cfg),
            active: Arc::default(),
            processes: Arc::default(),
        }
    }

    pub fn config(&self) -> &ResearchConfig {
        &self.cfg
    }

    /// Starts a run in the background (needs a tokio runtime). Busy when the workspace already has one.
    pub fn start(&self, req: ResearchRequest) -> Result<ResearchRun, ResearchError> {
        let id = req
            .run_id
            .clone()
            .unwrap_or_else(|| format!("tr_{}", uuid::Uuid::new_v4().simple()));
        let (stop_tx, stop_rx) = watch::channel(false);
        let stop = StopHandle(Arc::new(stop_tx));
        self.processes
            .admit(|| -> Result<(), ResearchError> {
                let mut active = self.active.lock().unwrap_or_else(|e| e.into_inner());
                if let Some((rid, _)) = active.iter().find(|(_, a)| a.workspace == req.workspace) {
                    return Err(ResearchError::Busy {
                        run_id: rid.clone(),
                    });
                }
                if active.len() >= self.cfg.max_global {
                    return Err(ResearchError::Full);
                }
                active.insert(
                    id.clone(),
                    Active {
                        workspace: req.workspace.clone(),
                        stop: stop.clone(),
                    },
                );
                Ok(())
            })
            .ok_or(ResearchError::Closed)??;
        let (tx, rx) = mpsc::unbounded_channel();
        let cfg = self.cfg.clone();
        let release = Release {
            active: self.active.clone(),
            id: id.clone(),
        };
        let workspace = req.workspace.clone();
        let rid = id.clone();
        let processes = self.processes.clone();
        let task = tokio::spawn(async move {
            let _release = release;
            execute(&cfg, req, rid, tx, stop_rx, processes).await
        });
        Ok(ResearchRun {
            id,
            workspace,
            events: rx,
            stop,
            task,
        })
    }

    /// Permanently stop admission, stop active runs, and confirm this service's independent
    /// process groups (including descendants) are gone within the caller's deadline.
    pub async fn shutdown(&self, grace: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + grace;
        self.processes.close();
        for a in self
            .active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
        {
            a.stop.stop();
        }
        if !self.processes.finish_shutdown(grace).await {
            return false;
        }
        loop {
            if self
                .active
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_empty()
            {
                return true;
            }
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep_until(
                (tokio::time::Instant::now() + Duration::from_millis(20)).min(deadline),
            )
            .await;
        }
    }

    /// Stops one run; false when it isn't running.
    pub fn stop(&self, run_id: &str) -> bool {
        let active = self.active.lock().unwrap_or_else(|e| e.into_inner());
        match active.get(run_id) {
            Some(a) => {
                a.stop.stop();
                true
            }
            None => false,
        }
    }

    /// Stops the workspace's run (`POST /api/thread/stop`); its id when there was one.
    pub fn stop_workspace(&self, workspace: &str) -> Option<String> {
        let active = self.active.lock().unwrap_or_else(|e| e.into_inner());
        let (id, a) = active.iter().find(|(_, a)| a.workspace == workspace)?;
        a.stop.stop();
        Some(id.clone())
    }

    /// The workspace's running run.
    pub fn running(&self, workspace: &str) -> Option<String> {
        let active = self.active.lock().unwrap_or_else(|e| e.into_inner());
        active
            .iter()
            .find(|(_, a)| a.workspace == workspace)
            .map(|(id, _)| id.clone())
    }
}

/// Frees the run's slot even if its task panics.
struct Release {
    active: Arc<Mutex<HashMap<String, Active>>>,
    id: String,
}

impl Drop for Release {
    fn drop(&mut self) {
        let mut active = self.active.lock().unwrap_or_else(|e| e.into_inner());
        active.remove(&self.id);
    }
}

fn failed_outcome(run_id: String, err: String) -> ResearchOutcome {
    ResearchOutcome {
        run_id,
        status: RunStatus::Failed(err),
        model: None,
        claude_session: None,
        answer: String::new(),
        offer: None,
        citations: Vec::new(),
        files_read: Vec::new(),
        tools: Vec::new(),
        turns: 0,
        ms: 0,
        first_text_ms: None,
        cost_usd: None,
        sandboxed: false,
        result: None,
        raw: String::new(),
    }
}

/// Why the Hub ended the process.
#[derive(Debug, Clone, PartialEq)]
enum Kill {
    User,
    Budget(BudgetHit),
    Violation(Violation),
}

/// What the run has seen so far.
struct State {
    t0: Instant,
    text: String,
    prev_text: String,
    tools: Vec<ToolSeen>,
    started: HashMap<String, Instant>,
    /// Read calls without an answer yet: tool id → path.
    reading: HashMap<String, String>,
    files_read: Vec<String>,
    turns: HashSet<String>,
    session: Option<String>,
    model: Option<String>,
    first_text_ms: Option<u64>,
    result: Option<Value>,
    kill: Option<Kill>,
}

impl State {
    fn progressed(&self) -> bool {
        !self.text.is_empty() || !self.prev_text.is_empty() || !self.tools.is_empty()
    }

    fn partial(&self) -> &str {
        if self.text.trim().is_empty() {
            &self.prev_text
        } else {
            &self.text
        }
    }
}

/// One attempt of a run: one harness process with one model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptPlan {
    /// The model; `""` leaves it to Codex's configured default.
    pub model: String,
    /// The harness session to `--resume` (only the first attempt of a follow-up on Claude Code).
    pub resume: Option<String>,
    /// What the attempt asks, before the attachment note.
    pub prompt: String,
}

/// The attempts of a run in the order to try them (the Hub's policy):
/// - the request's models, or the configured ones; Codex with none named uses its own default;
/// - a follow-up first continues the earlier session with its model, then starts fresh on each model
///   with the earlier answer as context;
/// - Codex never resumes (it cannot continue a Claude Code session): fresh attempts only.
pub fn plan(req: &ResearchRequest, cfg: &ResearchConfig) -> Vec<AttemptPlan> {
    let codex = req.harness == "codex";
    let default_only = [String::new()];
    let models: &[String] = match req.models.as_deref() {
        Some([]) | None if codex => &default_only,
        Some(m) => m,
        None => &cfg.models,
    };
    let fresh = |prompt: String| {
        models.iter().map(move |m| AttemptPlan {
            model: m.clone(),
            resume: None,
            prompt: prompt.clone(),
        })
    };
    match &req.resume {
        None => fresh(req.prompt.clone()).collect(),
        Some(r) => {
            let prior = format!(
                "上一轮你的回答：\n{}\n\n用户接着问：\n{}",
                r.prior_answer.trim(),
                req.prompt.trim()
            );
            let resumed = (!codex).then(|| AttemptPlan {
                model: r.model.clone(),
                resume: Some(r.claude_session.clone()),
                prompt: req.prompt.clone(),
            });
            resumed.into_iter().chain(fresh(prior)).collect()
        }
    }
}

/// What one attempt left behind, as far as the driver cares.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Attempted {
    /// The run was ended on purpose: the user's stop, a budget or a violation.
    pub killed: bool,
    /// It wrote text or called a tool.
    pub progressed: bool,
    /// The harness's closing result: `Ok` when it answered, `Err(reason)` when it reported an error.
    pub result: Option<Result<(), String>>,
    /// Why the process produced nothing usable (it did not start, crashed, sent no result).
    pub error: Option<String>,
}

/// Runs one [`AttemptPlan`] (one harness process, one model).
pub trait AttemptRunner {
    fn run(&mut self, plan: &AttemptPlan) -> impl Future<Output = Attempted> + Send;
}

/// Tries the plans in order. It stops after the first attempt that answered, progressed or was ended
/// on purpose (a later model must not start over a half-written answer or after a violation), and goes
/// on after one that produced nothing. Returns why the last failed attempt failed.
pub async fn drive<R: AttemptRunner>(plans: &[AttemptPlan], runner: &mut R) -> String {
    let mut last_err = String::from("no research model configured");
    for p in plans {
        let a = runner.run(p).await;
        if let Some(e) = a.error {
            last_err = e;
        }
        if a.killed || a.progressed {
            break;
        }
        match a.result {
            Some(Ok(())) => break,
            Some(Err(e)) => last_err = e,
            None => {}
        }
        tracing::warn!(model = %p.model, error = %last_err, "research attempt failed, trying the next");
    }
    last_err
}

/// The reason a closing `result` object gives for an error.
fn result_error(r: &Value) -> String {
    r["result"]
        .as_str()
        .or_else(|| r["subtype"].as_str())
        .unwrap_or("harness error")
        .to_string()
}

struct Ctx<'a> {
    processes: Arc<crate::processes::Processes>,
    cfg: &'a ResearchConfig,
    req: &'a ResearchRequest,
    budget: Budget,
    scope: Scope,
    tx: mpsc::UnboundedSender<ResearchEvent>,
    /// The wall-time limit, when the setting names one.
    deadline: Option<Instant>,
    soft: Instant,
    cwd: PathBuf,
    run_dir: PathBuf,
    add_dirs: Vec<PathBuf>,
    files: crate::attach::Prepared,
    codex: bool,
    codex_exe: Option<PathBuf>,
    /// The `sandbox-exec` profile, when the run is sandboxed.
    profile: Option<PathBuf>,
    on_spawn: Option<SpawnHook>,
}

/// Told the process group id of each harness right after it started; `false` ends it at once (the
/// host could not record it) and the attempt fails.
pub type SpawnHook = Arc<dyn Fn(u32) -> bool + Send + Sync>;

/// One attempt for a host that runs each attempt on its own (a Node job, one model per job).
pub struct OneAttempt {
    /// The run's directory names (`<scratch>/<run_id>`, `<files_root>/<run_id>`).
    pub run_id: String,
    /// What to run: the model, the session to resume and the prompt ([`plan`] made it on the Hub).
    pub plan: AttemptPlan,
    pub on_spawn: Option<SpawnHook>,
}

/// Runs exactly one attempt with all of a run's layers (allowlist and settings, guard, sandbox,
/// budgets, attachments) and returns its outcome. No fallback: another model is another attempt.
/// Admission (one run per workspace, `max_global`) is the caller's. Dropping the future kills the
/// harness's process group.
pub async fn run_attempt(
    cfg: &ResearchConfig,
    req: ResearchRequest,
    one: OneAttempt,
    events: mpsc::UnboundedSender<ResearchEvent>,
    stop: watch::Receiver<bool>,
) -> ResearchOutcome {
    let plans = [one.plan];
    let processes = Arc::new(crate::processes::Processes::default());
    let host = Host {
        plans: &plans,
        on_spawn: one.on_spawn,
    };
    execute_plans(cfg, req, one.run_id, events, stop, processes, host).await
}

/// What a run tries and how its processes are reported.
struct Host<'p> {
    plans: &'p [AttemptPlan],
    on_spawn: Option<SpawnHook>,
}

async fn execute(
    cfg: &ResearchConfig,
    req: ResearchRequest,
    run_id: String,
    tx: mpsc::UnboundedSender<ResearchEvent>,
    stop: watch::Receiver<bool>,
    processes: Arc<crate::processes::Processes>,
) -> ResearchOutcome {
    let plans = plan(&req, cfg);
    let host = Host {
        plans: &plans,
        on_spawn: None,
    };
    execute_plans(cfg, req, run_id, tx, stop, processes, host).await
}

async fn execute_plans(
    cfg: &ResearchConfig,
    req: ResearchRequest,
    run_id: String,
    tx: mpsc::UnboundedSender<ResearchEvent>,
    mut stop: watch::Receiver<bool>,
    processes: Arc<crate::processes::Processes>,
    host: Host<'_>,
) -> ResearchOutcome {
    let t0 = Instant::now();
    let budget = req.budget.unwrap_or(cfg.budget);
    let run_dir = cfg.scratch.join(&run_id);
    if let Err(e) = tokio::fs::create_dir_all(run_dir.join("tmp")).await {
        return failed_outcome(run_id, format!("research dir: {e}"));
    }
    let run_dir = guard::resolve(&run_dir);
    let files_dir = cfg.files_root.join(&run_id);
    let files = match crate::attach::prepare(&files_dir, &req.attachments).await {
        Ok(f) => f,
        Err(e) => {
            let _ = tokio::fs::remove_dir_all(&files_dir).await;
            return failed_outcome(run_id, format!("research attachments: {e}"));
        }
    };
    let cwd = req.cwd.clone().unwrap_or_else(|| run_dir.clone());
    let mut add_dirs = req.add_dirs.clone();
    add_dirs.extend(files.dir.clone());
    let mut readable = add_dirs.clone();
    readable.push(run_dir.clone());
    let scope = Scope::new(&cwd, &readable, &cfg.home);
    let codex = req.harness == "codex";
    // Codex sandboxes its commands with Seatbelt itself, which cannot be nested in sandbox-exec
    let sandboxed = match (codex, sandbox::decide(cfg.sandbox)) {
        (true, None) => {
            return failed_outcome(
                run_id,
                "sandbox-exec is required, and Codex runs cannot be wrapped in it".into(),
            );
        }
        (true, Some(_)) => false,
        (false, Some(s)) => s,
        (false, None) => {
            return failed_outcome(run_id, "sandbox-exec is required but not available".into());
        }
    };
    let codex_exe = match (codex, &cfg.codex_executable) {
        (true, None) => {
            return failed_outcome(
                run_id,
                "没有配置 codex（CODEX_EXECUTABLE），不能用 Codex 研究".into(),
            );
        }
        (_, e) => e.clone(),
    };
    let setup = async {
        if codex {
            return Ok::<_, std::io::Error>(());
        }
        write_json(&run_dir.join("settings.json"), &command::settings(&scope)).await?;
        if let Some(m) = &req.mcp {
            write_json(
                &run_dir.join("mcp.json"),
                &command::mcp_config(&m.url, &m.token),
            )
            .await?;
        }
        if sandboxed {
            let prof = sandbox::profile(&[run_dir.clone(), guard::resolve(&cfg.claude_dir)])
                .ok_or_else(|| std::io::Error::other("unusable path for the sandbox profile"))?;
            tokio::fs::write(run_dir.join("sandbox.sb"), prof).await?;
        }
        Ok::<_, std::io::Error>(())
    };
    if let Err(e) = setup.await {
        if let Some(d) = &files.dir {
            let _ = tokio::fs::remove_dir_all(d).await;
        }
        return failed_outcome(run_id, format!("research setup: {e}"));
    }

    let ctx = Ctx {
        processes,
        on_spawn: host.on_spawn,
        cfg,
        req: &req,
        budget,
        scope,
        tx,
        deadline: budget.hard.map(|h| t0 + h),
        soft: t0 + budget.soft,
        profile: sandboxed.then(|| run_dir.join("sandbox.sb")),
        cwd,
        run_dir,
        add_dirs,
        files,
        codex,
        codex_exe,
    };
    let mut st = State {
        t0,
        text: String::new(),
        prev_text: String::new(),
        tools: Vec::new(),
        started: HashMap::new(),
        reading: HashMap::new(),
        files_read: Vec::new(),
        turns: HashSet::new(),
        session: None,
        model: None,
        first_text_ms: None,
        result: None,
        kill: None,
    };
    let mut local = Local {
        ctx: &ctx,
        st: &mut st,
        stop: &mut stop,
        soft_sent: false,
    };
    let last_err = drive(host.plans, &mut local)
        .instrument(tracing::info_span!("research", run = %run_id))
        .await;
    let _ = tokio::fs::remove_file(ctx.run_dir.join("mcp.json")).await;
    if let Some(d) = &ctx.files.dir {
        let _ = tokio::fs::remove_dir_all(d).await;
    }
    finish(run_id, &req, st, last_err, sandboxed)
}

/// Runs the attempts in this process, all into one [`State`].
struct Local<'a, 'c> {
    ctx: &'a Ctx<'c>,
    st: &'a mut State,
    stop: &'a mut watch::Receiver<bool>,
    /// The soft-limit notice is sent once per run, not per attempt.
    soft_sent: bool,
}

impl AttemptRunner for Local<'_, '_> {
    async fn run(&mut self, plan: &AttemptPlan) -> Attempted {
        let error = run_plan(self.ctx, plan, self.st, self.stop, &mut self.soft_sent)
            .await
            .err();
        Attempted {
            killed: self.st.kill.is_some(),
            progressed: self.st.progressed(),
            result: self.st.result.as_ref().map(|r| {
                if r["is_error"].as_bool().unwrap_or(false) {
                    Err(result_error(r))
                } else {
                    Ok(())
                }
            }),
            error,
        }
    }
}

/// One attempt: the harness invocation for this plan's model, prompt and session, run to its end.
async fn run_plan(
    ctx: &Ctx<'_>,
    plan: &AttemptPlan,
    st: &mut State,
    stop: &mut watch::Receiver<bool>,
    soft_sent: &mut bool,
) -> Result<(), String> {
    st.model = Some(plan.model.clone());
    st.session = None;
    st.result = None;
    let prompt = format!("{}{}", plan.prompt, ctx.files.note);
    let req = ctx.req;
    // Claude Code takes the images as blocks of a stream-json message; Codex as `-i <file>`
    let stdin = (!ctx.codex && !ctx.files.images.is_empty())
        .then(|| crate::attach::stdin_message(&prompt, &ctx.files.images));
    let inv = match &ctx.codex_exe {
        Some(exe) if ctx.codex => command::codex_invocation(&command::CodexPlan {
            executable: exe,
            prompt: &prompt,
            model: &plan.model,
            cwd: &ctx.cwd,
            run_dir: &ctx.run_dir,
            mcp: req.mcp.as_ref().map(|m| (m.url.as_str(), m.token.as_str())),
            effort: req.effort.as_deref(),
            images: &ctx.files.image_files,
        }),
        _ => command::invocation(&CommandPlan {
            executable: &ctx.cfg.executable,
            prompt: &prompt,
            stdin_input: stdin.is_some(),
            model: &plan.model,
            max_turns: ctx.budget.max_turns,
            cwd: &ctx.cwd,
            add_dirs: &ctx.add_dirs,
            run_dir: &ctx.run_dir,
            with_mcp: req.mcp.is_some(),
            resume: plan.resume.as_deref(),
            sandbox_profile: ctx.profile.as_deref(),
            effort: req.effort.as_deref(),
        }),
    };
    let parse: fn(&str) -> Vec<StreamItem> = if ctx.codex {
        parse_codex_line
    } else {
        parse_line
    };
    attempt(ctx, &inv, stdin.as_deref(), st, stop, soft_sent, parse).await
}

async fn write_json(p: &Path, v: &Value) -> std::io::Result<()> {
    let s = serde_json::to_vec_pretty(v).map_err(std::io::Error::other)?;
    tokio::fs::write(p, s).await?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(p, std::fs::Permissions::from_mode(0o600)).await?;
    }
    Ok(())
}

/// How much of the harness's stderr is kept: only its end is ever shown.
const STDERR_TAIL: usize = 16 * 1024;

/// Drains `r` to EOF (a full pipe would block the harness) and keeps the last [`STDERR_TAIL`]
/// bytes, decoded leniently so invalid UTF-8 does not lose the message.
async fn stderr_tail(mut r: impl tokio::io::AsyncRead + Unpin) -> String {
    let mut tail: std::collections::VecDeque<u8> = std::collections::VecDeque::new();
    let mut buf = [0u8; 8192];
    while let Ok(n) = r.read(&mut buf).await {
        if n == 0 {
            break;
        }
        tail.extend(&buf[..n]);
        let over = tail.len().saturating_sub(STDERR_TAIL);
        tail.drain(..over);
    }
    String::from_utf8_lossy(tail.make_contiguous()).into_owned()
}

/// Runs one harness process until it exits or the Hub kills it. Err is why it produced nothing usable.
async fn attempt(
    ctx: &Ctx<'_>,
    inv: &command::Invocation,
    stdin: Option<&str>,
    st: &mut State,
    stop: &mut watch::Receiver<bool>,
    soft_sent: &mut bool,
    parse: fn(&str) -> Vec<StreamItem>,
) -> Result<(), String> {
    if *stop.borrow() {
        st.kill = Some(Kill::User);
        return Ok(());
    }
    let mut cmd = tokio::process::Command::new(&inv.program);
    cmd.args(&inv.args)
        .current_dir(&inv.cwd)
        .envs(inv.env.iter().map(|(k, v)| (k, v)))
        .stdin(if stdin.is_some() {
            std::process::Stdio::piped()
        } else {
            std::process::Stdio::null()
        })
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
    #[cfg(unix)]
    cmd.process_group(0);
    for key in [
        "STEVE_UPGRADE_CONTROL_TOKEN",
        "STEVE_SUPERVISOR_PID",
        "STEVE_UPGRADE_ID",
    ] {
        cmd.env_remove(key);
    }
    let (mut child, _group) = ctx
        .processes
        .spawn(&mut cmd)
        .map_err(|e| format!("spawn {}: {e}", inv.program.display()))?;
    /* the one user message (text and images), then EOF so the harness ends after answering it */
    if let (Some(line), Some(mut w)) = (stdin, child.stdin.take()) {
        let line = format!("{line}\n");
        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            if let Err(e) = w.write_all(line.as_bytes()).await {
                tracing::warn!("research: writing the message to the harness: {e}");
            }
            let _ = w.shutdown().await;
        });
    }
    let pid = child.id();
    if let (Some(hook), Some(p)) = (&ctx.on_spawn, pid)
        && !hook(p)
    {
        kill_tree(&mut child, pid).await;
        return Err("harness process not recorded".into());
    }
    let (Some(stdout), Some(stderr)) = (child.stdout.take(), child.stderr.take()) else {
        return Err("harness 没有输出".into());
    };
    let err_task = tokio::spawn(stderr_tail(stderr));
    let model = st.model.clone().unwrap_or_default();
    let _ = ctx.tx.send(ResearchEvent::Started { model });
    let mut lines = tokio::io::BufReader::new(stdout).lines();
    let mut stop_open = true;
    let mut quiet_until = Instant::now() + ctx.budget.idle;
    loop {
        tokio::select! {
            biased;
            r = stop.changed(), if stop_open => {
                match r {
                    Ok(()) if *stop.borrow() => { st.kill = Some(Kill::User); break; }
                    Ok(()) => {}
                    Err(_) => stop_open = false,
                }
            }
            // without a wall-time limit the branch is off (its instant is then never awaited)
            _ = tokio::time::sleep_until(ctx.deadline.unwrap_or(quiet_until)), if ctx.deadline.is_some() => {
                st.kill = Some(Kill::Budget(BudgetHit::Time));
                break;
            }
            _ = tokio::time::sleep_until(quiet_until) => {
                tracing::warn!(idle = ?ctx.budget.idle, "research: the harness said nothing for too long");
                st.kill = Some(Kill::Budget(BudgetHit::Idle));
                break;
            }
            _ = tokio::time::sleep_until(ctx.soft), if !*soft_sent => {
                *soft_sent = true;
                let _ = ctx.tx.send(ResearchEvent::StillReading);
            }
            l = lines.next_line() => match l {
                Ok(Some(l)) => {
                    quiet_until = Instant::now() + ctx.budget.idle;
                    for item in parse(&l) {
                        if let Some(k) = on_item(ctx, st, item) {
                            st.kill = Some(k);
                            break;
                        }
                    }
                    if st.kill.is_some() { break; }
                }
                Ok(None) | Err(_) => break,
            },
        }
    }
    if let Some(k) = &st.kill {
        tracing::info!(kill = ?k, "research: ending the harness");
        kill_tree(&mut child, pid).await;
        return Ok(());
    }
    // its output is closed: it is ending; a process that does not is as stuck as a silent one
    let wait_until = match ctx.deadline {
        Some(d) => d.min(Instant::now() + ctx.budget.idle),
        None => Instant::now() + ctx.budget.idle,
    };
    let status = match tokio::time::timeout_at(wait_until, child.wait()).await {
        Ok(Ok(s)) => s,
        Ok(Err(e)) => return Err(e.to_string()),
        Err(_) => {
            st.kill = Some(Kill::Budget(BudgetHit::Time));
            kill_tree(&mut child, pid).await;
            return Ok(());
        }
    };
    if st.result.is_none() && !status.success() {
        let e = err_task.await.unwrap_or_default();
        let tail: String = e
            .trim()
            .chars()
            .rev()
            .take(400)
            .collect::<Vec<_>>()
            .into_iter()
            .rev()
            .collect();
        return Err(format!("Command failed: {tail}"));
    }
    if st.result.is_none() {
        return Err("harness 没有返回结果".into());
    }
    Ok(())
}

/// SIGKILL to the whole process group (the harness and anything it started), then reap.
async fn kill_tree(child: &mut tokio::process::Child, pid: Option<u32>) {
    #[cfg(unix)]
    if let Some(pid) = pid.and_then(|p| i32::try_from(p).ok()) {
        // SAFETY: plain syscall on a process group this run created (`process_group(0)`).
        unsafe {
            libc::killpg(pid, libc::SIGKILL);
        }
    }
    #[cfg(not(unix))]
    let _ = pid;
    let _ = child.start_kill();
    let _ = child.wait().await;
}

/// Applies one stream item; `Some` when the run must end now.
fn on_item(ctx: &Ctx<'_>, st: &mut State, item: StreamItem) -> Option<Kill> {
    match item {
        StreamItem::Init { session_id, .. } => {
            if !session_id.is_empty() {
                st.session = Some(session_id.clone());
                let _ = ctx.tx.send(ResearchEvent::Session {
                    claude_session: session_id,
                });
            }
        }
        StreamItem::MessageStart { id } => {
            if !st.text.trim().is_empty() {
                st.prev_text = std::mem::take(&mut st.text);
            } else {
                st.text.clear();
            }
            st.turns.insert(id);
            if ctx
                .budget
                .max_turns
                .is_some_and(|m| st.turns.len() as u32 > m)
            {
                return Some(Kill::Budget(BudgetHit::Turns));
            }
        }
        StreamItem::TextDelta(t) => {
            st.text.push_str(&t);
            if st.first_text_ms.is_none() {
                st.first_text_ms = Some(st.t0.elapsed().as_millis() as u64);
            }
            let _ = ctx.tx.send(ResearchEvent::Text {
                text: st.text.clone(),
            });
        }
        StreamItem::ToolUse {
            id,
            name,
            input,
            message_id,
        } => {
            if st.tools.iter().any(|t| t.id == id) {
                return None;
            }
            if !message_id.is_empty() {
                st.turns.insert(message_id);
            }
            if ctx.cfg.guard
                && let Err(v) = guard::research_guard(&name, &input, &ctx.scope)
            {
                tracing::warn!(violation = %v, "research: out-of-scope tool call");
                st.tools.push(ToolSeen {
                    id,
                    brief: guard::brief(&name, &input, &ctx.scope.cwd),
                    name,
                    ok: Some(false),
                    error: Some(crate::stream::ToolError::GuardViolation),
                    ms: 0,
                });
                return Some(Kill::Violation(v));
            }
            if ctx
                .budget
                .max_tools
                .is_some_and(|m| st.tools.len() as u32 >= m)
            {
                return Some(Kill::Budget(BudgetHit::Tools));
            }
            if name == "Read"
                && let Some(p) = input.get("file_path").and_then(Value::as_str)
            {
                let f = guard::brief(
                    "Read",
                    &serde_json::json!({ "file_path": p }),
                    &ctx.scope.cwd,
                );
                st.reading.insert(id.clone(), f);
            }
            if name == "Shell"
                && let Some(p) = input
                    .get("command")
                    .and_then(Value::as_str)
                    .and_then(guard::shell_read)
            {
                let f = guard::brief(
                    "Read",
                    &serde_json::json!({ "file_path": p }),
                    &ctx.scope.cwd,
                );
                st.reading.insert(id.clone(), f);
            }
            let seen = ToolSeen {
                id: id.clone(),
                brief: guard::brief(&name, &input, &ctx.scope.cwd),
                name,
                ok: None,
                error: None,
                ms: 0,
            };
            st.started.insert(id, Instant::now());
            st.tools.push(seen.clone());
            let _ = ctx.tx.send(ResearchEvent::ToolStarted(seen));
        }
        StreamItem::ToolResult {
            id,
            is_error,
            error,
            ..
        } => {
            let ms = st
                .started
                .remove(&id)
                .map_or(0, |t| t.elapsed().as_millis() as u64);
            // only reads the harness answered count as files read
            if let Some(f) = st.reading.remove(&id)
                && !is_error
                && !st.files_read.contains(&f)
            {
                st.files_read.push(f);
            }
            if let Some(t) = st.tools.iter_mut().find(|t| t.id == id) {
                t.ok = Some(!is_error);
                t.error = error;
                t.ms = ms;
                let _ = ctx.tx.send(ResearchEvent::ToolDone(t.clone()));
            }
        }
        StreamItem::Result(v) => st.result = Some(v),
    }
    None
}

fn finish(
    run_id: String,
    req: &ResearchRequest,
    st: State,
    last_err: String,
    sandboxed: bool,
) -> ResearchOutcome {
    let result = st.result.as_ref();
    let status = match &st.kill {
        Some(Kill::User) => RunStatus::Stopped,
        Some(Kill::Budget(b)) => RunStatus::Budget(*b),
        Some(Kill::Violation(v)) => RunStatus::Violated(v.clone()),
        None => match result {
            Some(r) if r["subtype"] == "error_max_turns" => RunStatus::Budget(BudgetHit::Turns),
            Some(r) if r["is_error"].as_bool().unwrap_or(false) => {
                RunStatus::Failed(result_error(r))
            }
            Some(_) => RunStatus::Done,
            None => RunStatus::Failed(last_err),
        },
    };
    let raw = match (&status, result.and_then(|r| r["result"].as_str())) {
        (RunStatus::Done, Some(t)) if !t.trim().is_empty() => t.to_string(),
        _ => st.partial().to_string(),
    };
    let (text, offer, citations) = present(&status, &raw, &req.question, &st.files_read);
    let mut tools = st.tools;
    for t in &mut tools {
        if t.ok.is_none() {
            t.ok = Some(false);
        }
    }
    ResearchOutcome {
        citations,
        raw,
        run_id,
        status,
        model: st.model,
        claude_session: st.session,
        answer: text,
        offer,
        files_read: st.files_read,
        tools,
        turns: st.turns.len() as u32,
        ms: st.t0.elapsed().as_millis() as u64,
        first_text_ms: st.first_text_ms,
        cost_usd: result.and_then(|r| r["total_cost_usd"].as_f64()),
        sandboxed,
        result: st.result.clone(),
    }
}

/// What the thread shows for a run that ended with `status` after writing `raw`: the answer without
/// its `OFFER:` line and with the end mark of a stopped, cut or violating run, the offer (a cut run
/// offers to go deeper into `question`) and the citations of the files it read.
pub fn present(
    status: &RunStatus,
    raw: &str,
    question: &str,
    files_read: &[String],
) -> (String, Option<Offer>, Vec<Citation>) {
    let (mut text, mut offer) = answer::split_offer(raw);
    match status {
        RunStatus::Stopped => text = answer::with_mark(&text, answer::MARK_STOPPED),
        RunStatus::Budget(_) => {
            text = answer::with_mark(&text, answer::MARK_BUDGET);
            offer = offer.or_else(|| Some(answer::deeper_offer(question)));
        }
        RunStatus::Violated(_) => text = answer::with_mark(&text, answer::MARK_VIOLATION),
        _ => {}
    }
    let citations = answer::citations(&text, files_read);
    (text, offer, citations)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn stderr_keeps_a_bounded_tail_even_with_invalid_utf8() {
        // read_to_string used to drop everything here: one bad byte, and the error came out empty
        let mut e = vec![b'a'; STDERR_TAIL * 3];
        e.extend_from_slice(b"\xff boom: the real reason");
        let t = stderr_tail(e.as_slice()).await;
        assert!(t.ends_with(" boom: the real reason"), "{t:?}");
        assert!(t.len() <= STDERR_TAIL + 3);
    }

    #[test]
    fn no_limit_unless_the_setting_names_one() {
        // by default a run is not cut off by time, turns or tool calls; only the stall detector is on
        let d = Budget::default();
        assert_eq!((d.hard, d.max_turns, d.max_tools), (None, None, None));
        assert_eq!(d.idle, DEFAULT_IDLE);
        assert_eq!(Budget::from_setting(&json!({})), d);
        // a limit in the setting is honoured as given, there is no ceiling above it
        let b = Budget::from_setting(
            &json!({"seconds": 999, "tools": 500, "turns": 10, "idle_seconds": 30}),
        );
        assert_eq!(b.hard, Some(Duration::from_secs(999)));
        assert_eq!((b.max_tools, b.max_turns), (Some(500), Some(10)));
        assert_eq!(b.idle, Duration::from_secs(30));
    }

    fn cfg() -> ResearchConfig {
        let mut c = ResearchConfig::new("/bin/claude", "/tmp/scratch");
        c.models = vec!["sonnet".into(), "opus".into()];
        c
    }

    fn at(model: &str, resume: Option<&str>, prompt: &str) -> AttemptPlan {
        AttemptPlan {
            model: model.into(),
            resume: resume.map(String::from),
            prompt: prompt.into(),
        }
    }

    fn follow_up(harness: &str) -> ResearchRequest {
        let mut r = ResearchRequest::new("ws", "q", "再细一点");
        r.harness = harness.into();
        r.resume = Some(Resume {
            claude_session: "sess_1".into(),
            model: "opus".into(),
            prior_answer: "  前面的答案 \n".into(),
        });
        r
    }

    const PRIOR: &str = "上一轮你的回答：\n前面的答案\n\n用户接着问：\n再细一点";

    #[test]
    fn plan_without_resume_tries_each_configured_model() {
        let r = ResearchRequest::new("ws", "q", "prompt");
        assert_eq!(
            plan(&r, &cfg()),
            vec![at("sonnet", None, "prompt"), at("opus", None, "prompt")]
        );
    }

    #[test]
    fn plan_resumes_on_claude_then_starts_fresh_with_the_prior_answer() {
        assert_eq!(
            plan(&follow_up("claude-code"), &cfg()),
            vec![
                at("opus", Some("sess_1"), "再细一点"),
                at("sonnet", None, PRIOR),
                at("opus", None, PRIOR),
            ]
        );
    }

    #[test]
    fn plan_on_codex_never_resumes() {
        // no model named: Codex's own default, fresh with the earlier answer only
        assert_eq!(plan(&follow_up("codex"), &cfg()), vec![at("", None, PRIOR)]);
        let mut r = follow_up("codex");
        r.models = Some(Vec::new());
        assert_eq!(plan(&r, &cfg()), vec![at("", None, PRIOR)]);
        r.models = Some(vec!["gpt".into()]);
        assert_eq!(plan(&r, &cfg()), vec![at("gpt", None, PRIOR)]);
    }

    #[test]
    fn plan_prefers_the_requests_models_over_the_configured_ones() {
        let mut r = ResearchRequest::new("ws", "q", "prompt");
        r.models = Some(vec!["haiku".into()]);
        assert_eq!(plan(&r, &cfg()), vec![at("haiku", None, "prompt")]);
        // an empty list on Claude Code is no model at all (the run fails as unconfigured)
        r.models = Some(Vec::new());
        assert!(plan(&r, &cfg()).is_empty());
    }

    /// Plays one scripted [`Attempted`] per attempt and records the models it was asked to run.
    struct Scripted {
        script: std::collections::VecDeque<Attempted>,
        ran: Vec<String>,
    }

    impl AttemptRunner for Scripted {
        async fn run(&mut self, plan: &AttemptPlan) -> Attempted {
            self.ran.push(plan.model.clone());
            self.script.pop_front().unwrap_or_default()
        }
    }

    async fn drive_script(script: Vec<Attempted>) -> (Vec<String>, String) {
        let plans = vec![at("a", None, "p"), at("b", None, "p"), at("c", None, "p")];
        let mut s = Scripted {
            script: script.into(),
            ran: Vec::new(),
        };
        let err = drive(&plans, &mut s).await;
        (s.ran, err)
    }

    fn failed(e: &str) -> Attempted {
        Attempted {
            error: Some(e.into()),
            ..Attempted::default()
        }
    }

    #[tokio::test]
    async fn driver_stops_after_progress_a_kill_or_an_answer() {
        let progressed = Attempted {
            progressed: true,
            error: Some("crashed midway".into()),
            ..Attempted::default()
        };
        let (ran, _) = drive_script(vec![failed("busy"), progressed]).await;
        assert_eq!(ran, ["a", "b"]);
        // a violation (or the user's stop, or a budget) ends the run on the spot
        let killed = Attempted {
            killed: true,
            ..Attempted::default()
        };
        let (ran, _) = drive_script(vec![killed]).await;
        assert_eq!(ran, ["a"]);
        let answered = Attempted {
            result: Some(Ok(())),
            ..Attempted::default()
        };
        let (ran, _) = drive_script(vec![failed("busy"), answered]).await;
        assert_eq!(ran, ["a", "b"]);
    }

    #[tokio::test]
    async fn driver_goes_on_after_an_attempt_that_produced_nothing() {
        let harness_error = Attempted {
            result: Some(Err("overloaded".into())),
            ..Attempted::default()
        };
        let (ran, err) = drive_script(vec![failed("spawn x"), harness_error, failed("dead")]).await;
        assert_eq!(ran, ["a", "b", "c"]);
        assert_eq!(err, "dead");
        // the harness's own reason wins over the process error of the same attempt
        let both = Attempted {
            result: Some(Err("rate limited".into())),
            error: Some("exit 1".into()),
            ..Attempted::default()
        };
        let (_, err) = drive_script(vec![failed("x"), failed("y"), both]).await;
        assert_eq!(err, "rate limited");
        // nothing to try: the run fails as unconfigured
        let mut none = Scripted {
            script: Default::default(),
            ran: Vec::new(),
        };
        assert_eq!(drive(&[], &mut none).await, "no research model configured");
        assert!(none.ran.is_empty());
    }
}
