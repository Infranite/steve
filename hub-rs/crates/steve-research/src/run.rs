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

/// Hard ceiling of a run's wall time, whatever the setting says.
pub const HARD_TIME_CAP: Duration = Duration::from_secs(240);
/// Hard ceiling of a run's tool calls, whatever the setting says.
pub const HARD_TOOLS_CAP: u32 = 60;
/// Default research models (`STEVE_MODELS_RESEARCH`), non-kiro.
pub const DEFAULT_MODELS: &str = "claude-sonnet-5-5,claude-opus-5-5";

/// Limits of one run (setting `thread.research`). Values above the hard caps are cut to them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    /// Wall time; at the limit the process is killed.
    pub hard: Duration,
    /// After this the thread shows "还在读…" ([`ResearchEvent::StillReading`]).
    pub soft: Duration,
    /// Model turns (`--max-turns`, and counted by the Hub too).
    pub max_turns: u32,
    /// Tool calls counted by the Hub.
    pub max_tools: u32,
}

impl Default for Budget {
    fn default() -> Self {
        Budget {
            hard: HARD_TIME_CAP,
            soft: Duration::from_secs(90),
            max_turns: 24,
            max_tools: 40,
        }
    }
}

impl Budget {
    /// From the `thread.research` setting (`{"seconds","soft_seconds","turns","tools"}`), defaults for the rest.
    pub fn from_setting(v: &Value) -> Self {
        let d = Budget::default();
        let n = |k: &str| v.get(k).and_then(Value::as_u64);
        Budget {
            hard: n("seconds").map_or(d.hard, Duration::from_secs),
            soft: n("soft_seconds").map_or(d.soft, Duration::from_secs),
            max_turns: n("turns").map_or(d.max_turns, |x| x as u32),
            max_tools: n("tools").map_or(d.max_tools, |x| x as u32),
        }
        .clamped()
    }

    /// Cut to the hard caps.
    pub fn clamped(self) -> Self {
        Budget {
            hard: self.hard.min(HARD_TIME_CAP),
            soft: self.soft.min(HARD_TIME_CAP),
            max_turns: self.max_turns.max(1),
            max_tools: self.max_tools.min(HARD_TOOLS_CAP),
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

struct Ctx<'a> {
    processes: Arc<crate::processes::Processes>,
    cfg: &'a ResearchConfig,
    budget: Budget,
    scope: Scope,
    tx: mpsc::UnboundedSender<ResearchEvent>,
    deadline: Instant,
    soft: Instant,
}

async fn execute(
    cfg: &ResearchConfig,
    req: ResearchRequest,
    run_id: String,
    tx: mpsc::UnboundedSender<ResearchEvent>,
    mut stop: watch::Receiver<bool>,
    processes: Arc<crate::processes::Processes>,
) -> ResearchOutcome {
    let t0 = Instant::now();
    let budget = req.budget.unwrap_or(cfg.budget).clamped();
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
        cfg,
        budget,
        scope,
        tx,
        deadline: t0 + budget.hard,
        soft: t0 + budget.soft,
    };
    // (model, --resume session, prompt) in the order to try.
    let mut attempts: Vec<(String, Option<String>, String)> = Vec::new();
    let default_only = vec![String::new()];
    let models = match req.models.as_ref() {
        // Codex with no model named: its configured default
        Some(m) if codex && m.is_empty() => &default_only,
        None if codex => &default_only,
        Some(m) => m,
        None => &cfg.models,
    };
    match &req.resume {
        // a Codex run cannot continue a Claude Code session: a fresh run with the earlier answer
        Some(r) if codex => {
            let fresh = format!(
                "上一轮你的回答：\n{}\n\n用户接着问：\n{}",
                r.prior_answer.trim(),
                req.prompt.trim()
            );
            for m in models {
                attempts.push((m.clone(), None, fresh.clone()));
            }
        }
        Some(r) => {
            attempts.push((
                r.model.clone(),
                Some(r.claude_session.clone()),
                req.prompt.clone(),
            ));
            let fresh = format!(
                "上一轮你的回答：\n{}\n\n用户接着问：\n{}",
                r.prior_answer.trim(),
                req.prompt.trim()
            );
            for m in models {
                attempts.push((m.clone(), None, fresh.clone()));
            }
        }
        None => {
            for m in models {
                attempts.push((m.clone(), None, req.prompt.clone()));
            }
        }
    }

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
    let mut last_err = String::from("no research model configured");
    let mut soft_sent = false;
    let profile = sandboxed.then(|| run_dir.join("sandbox.sb"));
    for (model, resume, prompt) in &attempts {
        st.model = Some(model.clone());
        st.session = None;
        st.result = None;
        let prompt = format!("{prompt}{}", files.note);
        // Claude Code takes the images as blocks of a stream-json message; Codex as `-i <file>`
        let stdin = (!codex && !files.images.is_empty())
            .then(|| crate::attach::stdin_message(&prompt, &files.images));
        let inv = match &codex_exe {
            Some(exe) if codex => command::codex_invocation(&command::CodexPlan {
                executable: exe,
                prompt: &prompt,
                model,
                cwd: &cwd,
                run_dir: &run_dir,
                mcp: req.mcp.as_ref().map(|m| (m.url.as_str(), m.token.as_str())),
                effort: req.effort.as_deref(),
                images: &files.image_files,
            }),
            _ => command::invocation(&CommandPlan {
                executable: &cfg.executable,
                prompt: &prompt,
                stdin_input: stdin.is_some(),
                model,
                max_turns: budget.max_turns,
                cwd: &cwd,
                add_dirs: &add_dirs,
                run_dir: &run_dir,
                with_mcp: req.mcp.is_some(),
                resume: resume.as_deref(),
                sandbox_profile: profile.as_deref(),
                effort: req.effort.as_deref(),
            }),
        };
        let parse: fn(&str) -> Vec<StreamItem> = if codex { parse_codex_line } else { parse_line };
        match attempt(
            &ctx,
            &inv,
            stdin.as_deref(),
            &mut st,
            &mut stop,
            &mut soft_sent,
            parse,
        )
        .await
        {
            Ok(()) => {}
            Err(e) => last_err = e,
        }
        if st.kill.is_some() || st.progressed() {
            break;
        }
        match &st.result {
            Some(r) if !r["is_error"].as_bool().unwrap_or(false) => break,
            Some(r) => {
                last_err = r["result"]
                    .as_str()
                    .or_else(|| r["subtype"].as_str())
                    .unwrap_or("harness error")
                    .to_string()
            }
            None => {}
        }
        tracing::warn!(run = %run_id, model = %model, error = %last_err, "research attempt failed, trying the next");
    }
    let _ = tokio::fs::remove_file(run_dir.join("mcp.json")).await;
    if let Some(d) = &files.dir {
        let _ = tokio::fs::remove_dir_all(d).await;
    }
    finish(run_id, &req, st, last_err, sandboxed)
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
    let (Some(stdout), Some(mut stderr)) = (child.stdout.take(), child.stderr.take()) else {
        return Err("harness 没有输出".into());
    };
    let err_task = tokio::spawn(async move {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s).await;
        s
    });
    let model = st.model.clone().unwrap_or_default();
    let _ = ctx.tx.send(ResearchEvent::Started { model });
    let mut lines = tokio::io::BufReader::new(stdout).lines();
    let mut stop_open = true;
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
            _ = tokio::time::sleep_until(ctx.deadline) => {
                st.kill = Some(Kill::Budget(BudgetHit::Time));
                break;
            }
            _ = tokio::time::sleep_until(ctx.soft), if !*soft_sent => {
                *soft_sent = true;
                let _ = ctx.tx.send(ResearchEvent::StillReading);
            }
            l = lines.next_line() => match l {
                Ok(Some(l)) => {
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
    let status = match tokio::time::timeout_at(ctx.deadline, child.wait()).await {
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
            if st.turns.len() as u32 > ctx.budget.max_turns {
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
                    ms: 0,
                });
                return Some(Kill::Violation(v));
            }
            if st.tools.len() as u32 >= ctx.budget.max_tools {
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
                ms: 0,
            };
            st.started.insert(id, Instant::now());
            st.tools.push(seen.clone());
            let _ = ctx.tx.send(ResearchEvent::ToolStarted(seen));
        }
        StreamItem::ToolResult { id, is_error, .. } => {
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
            Some(r) if r["is_error"].as_bool().unwrap_or(false) => RunStatus::Failed(
                r["result"]
                    .as_str()
                    .or_else(|| r["subtype"].as_str())
                    .unwrap_or("harness error")
                    .to_string(),
            ),
            Some(_) => RunStatus::Done,
            None => RunStatus::Failed(last_err),
        },
    };
    let raw = match (&status, result.and_then(|r| r["result"].as_str())) {
        (RunStatus::Done, Some(t)) if !t.trim().is_empty() => t.to_string(),
        _ => st.partial().to_string(),
    };
    let (mut text, mut offer) = answer::split_offer(&raw);
    match &status {
        RunStatus::Stopped => text = answer::with_mark(&text, answer::MARK_STOPPED),
        RunStatus::Budget(_) => {
            text = answer::with_mark(&text, answer::MARK_BUDGET);
            offer = offer.or_else(|| Some(answer::deeper_offer(&req.question)));
        }
        RunStatus::Violated(_) => text = answer::with_mark(&text, answer::MARK_VIOLATION),
        _ => {}
    }
    let mut tools = st.tools;
    for t in &mut tools {
        if t.ok.is_none() {
            t.ok = Some(false);
        }
    }
    ResearchOutcome {
        citations: answer::citations(&text, &st.files_read),
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn budget_setting_and_caps() {
        let b = Budget::from_setting(&json!({"seconds": 999, "tools": 500, "turns": 10}));
        assert_eq!(b.hard, HARD_TIME_CAP);
        assert_eq!(b.max_tools, HARD_TOOLS_CAP);
        assert_eq!(b.max_turns, 10);
        assert_eq!(Budget::default().max_tools, 40);
        assert_eq!(Budget::default().max_turns, 24);
        assert_eq!(Budget::from_setting(&json!({})), Budget::default());
    }
}
