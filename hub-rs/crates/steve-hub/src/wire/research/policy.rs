//! Hub-side research policy: budgets, attempt planning/driving, and answer presentation.
//! Execution itself runs exclusively on a Node over `steve-node-api` (`JobExecutor`).

use std::future::Future;
use std::path::PathBuf;
use std::time::Duration;

pub use steve_confine::SandboxMode;

pub use super::answer::{self, Citation, Offer};
pub use super::prompt::{self, DirInfo, PromptInput};

pub const DEFAULT_IDLE: Duration = Duration::from_secs(600);
pub const EXIT_WAIT: Duration = Duration::from_secs(45);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Violation {
    Tool { tool: String },
    OutOfScope { tool: String, path: String },
    Sensitive { tool: String, path: String },
    BadInput { tool: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    pub hard: Option<Duration>,
    pub soft: Duration,
    pub max_turns: Option<u32>,
    pub max_tools: Option<u32>,
    pub idle: Duration,
    pub exit_wait: Duration,
}

impl Default for Budget {
    fn default() -> Self {
        Budget {
            hard: None,
            soft: Duration::from_secs(90),
            max_turns: None,
            max_tools: None,
            idle: DEFAULT_IDLE,
            exit_wait: EXIT_WAIT,
        }
    }
}

#[derive(Debug, Clone)]
pub struct ResearchConfig {
    pub models: Vec<String>,
    pub budget: Budget,
    pub sandbox: SandboxMode,
    pub max_global: usize,
}

impl ResearchConfig {
    pub fn new() -> Self {
        ResearchConfig {
            models: vec![],
            budget: Budget::default(),
            sandbox: SandboxMode::Auto,
            max_global: 3,
        }
    }

    pub fn startup_note(&self) -> Option<&'static str> {
        (self.sandbox != SandboxMode::Off && !steve_confine::available()).then_some(
            "research: system sandbox not available; read-only relies on the adapter's mode and the guard",
        )
    }
}

impl Default for ResearchConfig {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone)]
pub struct Resume {
    pub session: String,
    pub model: String,
    pub fresh: String,
}

#[derive(Debug, Clone)]
pub struct ResearchRequest {
    pub prompt: String,
    pub cwd: Option<PathBuf>,
    pub add_dirs: Vec<PathBuf>,
    pub resume: Option<Resume>,
    pub budget: Option<Budget>,
    pub models: Option<Vec<String>>,
    pub effort: Option<String>,
    pub harness: String,
}

impl ResearchRequest {
    pub fn new(prompt: impl Into<String>) -> Self {
        ResearchRequest {
            prompt: prompt.into(),
            cwd: None,
            add_dirs: Vec::new(),
            resume: None,
            budget: None,
            models: None,
            effort: None,
            harness: String::new(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BudgetHit {
    Time,
    Turns,
    Tools,
    Idle,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RunStatus {
    Done,
    Stopped,
    Budget(BudgetHit),
    Violated(Violation),
    Failed(String),
}

#[derive(Debug, Clone)]
pub struct ResearchOutcome {
    pub run_id: String,
    pub status: RunStatus,
    pub model: Option<String>,
    pub session: Option<String>,
    pub answer: String,
    pub offer: Option<Offer>,
    pub turns: u32,
    pub ms: u64,
    pub first_text_ms: Option<u64>,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ResearchError {
    #[error("workspace already has a research run: {run_id}")]
    Busy { run_id: String },
    #[error("too many research runs at once")]
    Full,
    #[error("research service is shutting down")]
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptPlan {
    pub model: String,
    pub resume: Option<String>,
    pub prompt: String,
}

pub fn plan(req: &ResearchRequest, cfg: &ResearchConfig) -> Vec<AttemptPlan> {
    let default_only = [String::new()];
    let models: &[String] = match req.models.as_deref() {
        Some([]) => &default_only,
        Some(m) => m,
        None if cfg.models.is_empty() => &default_only,
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
        Some(r) if !models.contains(&r.model) => fresh(r.fresh.clone()).collect(),
        Some(r) => {
            let resumed = AttemptPlan {
                model: r.model.clone(),
                resume: Some(r.session.clone()),
                prompt: req.prompt.clone(),
            };
            std::iter::once(resumed)
                .chain(fresh(r.fresh.clone()))
                .collect()
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Attempted {
    pub killed: bool,
    pub progressed: bool,
    pub result: Option<Result<(), String>>,
    pub error: Option<String>,
}

pub trait AttemptRunner {
    fn run(&mut self, plan: &AttemptPlan) -> impl Future<Output = Attempted> + Send;
}

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
