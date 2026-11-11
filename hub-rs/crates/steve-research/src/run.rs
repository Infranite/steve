//! A research run's policy, shared by the Hub and the Node: its limits, the attempts it makes in
//! order ([`plan`], [`drive`]), how it ended and what the thread shows of it ([`present`]). Each
//! attempt runs as a one-shot ACP call on a Node (`steve-node`'s research jobs).

use crate::answer::{self, Citation, Offer};
use crate::guard::Violation;
use crate::sandbox::{self, SandboxMode};
use serde_json::Value;
use std::path::PathBuf;
use std::time::Duration;

/// A run whose harness says nothing (no text, no tool call, no event at all) for this long is stuck and is
/// ended. This is not a limit on how long a conversation may take: a run that keeps working never ends by
/// time. It is the same rule for every harness (API retries and context compaction can be silent for
/// minutes); setting `thread.research.idle_seconds` overrides it.
pub const DEFAULT_IDLE: Duration = Duration::from_secs(600);
/// After the harness closed its output it is ending: by default it gets this long to exit before it
/// counts as stuck ([`Budget::exit_wait`]).
pub const EXIT_WAIT: Duration = Duration::from_secs(45);

/// Limits of one run (setting `thread.research`). By default a run is bounded by nothing but its own end,
/// the user's stop and the stall detector; the wall time, turns and tool calls are only limited when the
/// setting names a limit (decided by the user on 2026-11-04: conversations must not be cut off).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Budget {
    /// Wall time; at the limit the process is killed. `None`: none.
    pub hard: Option<Duration>,
    /// After this the thread shows "还在读…" ([`ResearchEvent::StillReading`]); only a notice.
    pub soft: Duration,
    /// Model turns (messages between tool calls). `None`: none.
    pub max_turns: Option<u32>,
    /// Tool calls counted by the Hub. `None`: none.
    pub max_tools: Option<u32>,
    /// No output from the harness for this long: stuck, ended.
    pub idle: Duration,
    /// After the harness closed its output, how long it may take to exit before it counts as stuck.
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
            exit_wait: d.exit_wait,
        }
    }
}

/// How research runs.
#[derive(Debug, Clone)]
pub struct ResearchConfig {
    /// Tried in order until one starts answering, when the request names none. Empty: the harness's
    /// own default model.
    pub models: Vec<String>,
    /// Each run gets `<scratch>/<run id>/`.
    pub scratch: PathBuf,
    /// Whose secret directories (`~/.ssh`, `~/.steve`, …) are off limits.
    pub home: PathBuf,
    /// Where each run's attachment copies go (`<files_root>/<run id>/`): outside `~/.steve`, whose reads
    /// stay denied, so the run can read them. Removed when the run ends.
    pub files_root: PathBuf,
    pub budget: Budget,
    pub sandbox: SandboxMode,
    /// Runs at once across all workspaces (one per workspace always).
    pub max_global: usize,
}

impl ResearchConfig {
    /// Defaults: no model named, `$HOME`, the default budget, sandbox when available.
    pub fn new(scratch: impl Into<PathBuf>) -> Self {
        let home = std::env::var_os("HOME").map_or_else(|| PathBuf::from("/"), PathBuf::from);
        ResearchConfig {
            models: vec![],
            scratch: scratch.into(),
            files_root: std::env::temp_dir().join("steve-research-files"),
            home,
            budget: Budget::default(),
            sandbox: SandboxMode::Auto,
            max_global: 3,
        }
    }

    /// The line to log at startup when the platform has no system sandbox.
    pub fn startup_note(&self) -> Option<&'static str> {
        (self.sandbox != SandboxMode::Off && !sandbox::available()).then_some(
            "research: sandbox-exec not available; read-only relies on the adapter's mode and the guard",
        )
    }
}

/// Steve's MCP endpoint and the run's own token (the MCP side lists only the read-only tools for it).
#[derive(Debug, Clone)]
pub struct McpAccess {
    pub url: String,
    pub token: String,
}

/// Continue an earlier run's session (`session/load` on the same adapter, same model). When that
/// fails, a fresh run starts with `fresh` instead.
#[derive(Debug, Clone)]
pub struct Resume {
    pub session: String,
    pub model: String,
    /// What a run that starts over asks: the whole prompt (rules, recent messages, question), as
    /// for a first question. The request's own `prompt` is the question alone.
    pub fresh: String,
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
    /// Overrides the configured models (设置 · 模型), tried in order; empty: the adapter's default.
    pub models: Option<Vec<String>>,
    /// The reasoning effort; `None` leaves it to the adapter.
    pub effort: Option<String>,
    /// The harness adapter it runs on (设置 · 模型: Steve's harness or the research override).
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
            harness: String::new(),
        }
    }
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
    /// The model of the attempt that answered (the one to continue with).
    pub model: Option<String>,
    /// The adapter's session id, to continue the run with (`session/load`).
    pub session: Option<String>,
    /// What the thread shows: the answer (or the part written before the end) without the `OFFER:` line,
    /// with the end mark of a stopped, cut or violating run.
    pub answer: String,
    pub offer: Option<Offer>,
    pub citations: Vec<Citation>,
    /// Files the run read successfully, relative to its cwd when inside it.
    pub files_read: Vec<String>,
    pub turns: u32,
    pub ms: u64,
    pub first_text_ms: Option<u64>,
    /// Whether the harness ran under the system sandbox.
    pub sandboxed: bool,
    /// What the model wrote before the answer was shaped: the final turn's text, or the part
    /// written until the end ([`present`] turns it into `answer`, `offer` and `citations`).
    pub raw: String,
}

impl ResearchOutcome {
    /// A run that ended before anything ran.
    pub fn failed(run_id: impl Into<String>, err: impl Into<String>) -> Self {
        ResearchOutcome {
            run_id: run_id.into(),
            status: RunStatus::Failed(err.into()),
            model: None,
            session: None,
            answer: String::new(),
            offer: None,
            citations: Vec::new(),
            files_read: Vec::new(),
            turns: 0,
            ms: 0,
            first_text_ms: None,
            sandboxed: false,
            raw: String::new(),
        }
    }
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

/// One attempt of a run: one adapter session with one model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptPlan {
    /// The model; `""` leaves it to the adapter's default.
    pub model: String,
    /// The session to continue (only the first attempt of a follow-up).
    pub resume: Option<String>,
    /// What the attempt asks, before the attachment note.
    pub prompt: String,
}

/// The attempts of a run in the order to try them (the Hub's policy):
/// - the request's models, or the configured ones; none named: the adapter's own default;
/// - a follow-up first continues the earlier session with its model, then starts fresh on each model
///   with the whole prompt ([`Resume::fresh`]);
/// - a follow-up whose earlier model is not among the models to try any more (the setting changed
///   in between) starts fresh at once: what is configured now is what answers.
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

/// What one attempt left behind, as far as the driver cares.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Attempted {
    /// The run was ended on purpose: the user's stop, a budget or a violation.
    pub killed: bool,
    /// It wrote text or called a tool.
    pub progressed: bool,
    /// How the turn ended: `Ok` when it answered, `Err(reason)` when the adapter reported an error.
    pub result: Option<Result<(), String>>,
    /// Why the attempt produced nothing usable (it did not start, crashed, sent no result).
    pub error: Option<String>,
}

/// Runs one [`AttemptPlan`] (one adapter session, one model).
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

    #[test]
    fn no_limit_unless_the_setting_names_one() {
        // by default a run is not cut off by time, turns or tool calls; only the stall detector is on
        let d = Budget::default();
        assert_eq!((d.hard, d.max_turns, d.max_tools), (None, None, None));
        assert_eq!(d.idle, DEFAULT_IDLE);
        assert_eq!(d.exit_wait, EXIT_WAIT);
        assert!((30..=60).contains(&EXIT_WAIT.as_secs()));
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
        let mut c = ResearchConfig::new("/tmp/scratch");
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

    fn follow_up() -> ResearchRequest {
        let mut r = ResearchRequest::new("ws", "q", "再细一点");
        r.resume = Some(Resume {
            session: "sess_1".into(),
            model: "opus".into(),
            fresh: WHOLE.into(),
        });
        r
    }

    const WHOLE: &str = "规则\n最近的对话：\nSteve：前面的答案\n用户的问题：\n再细一点";

    #[test]
    fn plan_without_resume_tries_each_configured_model() {
        let r = ResearchRequest::new("ws", "q", "prompt");
        assert_eq!(
            plan(&r, &cfg()),
            vec![at("sonnet", None, "prompt"), at("opus", None, "prompt")]
        );
    }

    #[test]
    fn a_follow_up_continues_the_session_then_starts_fresh_with_the_whole_prompt() {
        assert_eq!(
            plan(&follow_up(), &cfg()),
            vec![
                at("opus", Some("sess_1"), "再细一点"),
                at("sonnet", None, WHOLE),
                at("opus", None, WHOLE),
            ]
        );
    }

    #[test]
    fn plan_prefers_the_requests_models_and_none_is_the_adapters_default() {
        let mut r = ResearchRequest::new("ws", "q", "prompt");
        r.models = Some(vec!["haiku".into()]);
        assert_eq!(plan(&r, &cfg()), vec![at("haiku", None, "prompt")]);
        r.models = Some(Vec::new());
        assert_eq!(plan(&r, &cfg()), vec![at("", None, "prompt")]);
        // a configuration names no model of its own
        r.models = None;
        let plain = ResearchConfig::new("/tmp/scratch");
        assert_eq!(plan(&r, &plain), vec![at("", None, "prompt")]);
        let mut f = follow_up();
        f.resume.as_mut().unwrap().model = String::new();
        f.models = Some(Vec::new());
        assert_eq!(
            plan(&f, &cfg()),
            vec![at("", Some("sess_1"), "再细一点"), at("", None, WHOLE)]
        );
    }

    /// The research model was changed between the answer and the follow-up: the earlier session
    /// ran on a model that is not to be tried any more, so nothing continues it.
    #[test]
    fn a_follow_up_after_the_models_changed_starts_fresh_on_the_new_ones() {
        let mut f = follow_up();
        f.models = Some(vec!["haiku".into()]);
        assert_eq!(plan(&f, &cfg()), vec![at("haiku", None, WHOLE)]);
        // from a named model back to the adapter's default
        f.models = Some(Vec::new());
        assert_eq!(plan(&f, &cfg()), vec![at("", None, WHOLE)]);
        // from the default to the configured ones
        f.models = None;
        f.resume.as_mut().unwrap().model = String::new();
        assert_eq!(
            plan(&f, &cfg()),
            vec![at("sonnet", None, WHOLE), at("opus", None, WHOLE)]
        );
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
