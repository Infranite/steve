//! Research jobs: one read-only research attempt per job, run by `steve_research` on this machine.
//!
//! The Hub chose the model and built the prompt; this Node resolves the directories through its
//! roots, fetches the prompt's files from the Hub, runs exactly that one attempt with all three
//! read-only layers (harness allowlist and settings, the guard that kills on the first bad call, and
//! `sandbox-exec` when the policy asks for `fs-readonly`) and reports its draft, tool rows, usage
//! and end. Steve's read-only tools reach the Hub through the Node's loopback endpoint with a token
//! of this job only, gone when it ends. Off until the host enables it ([`LocalNode::enable_research`]).

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use steve_node_api::*;
use steve_research as r;
use tokio::sync::{mpsc, watch};

use crate::journal::Tx;
use crate::{Decided, LocalNode, emit_all, job, now, procs, workspace};

/// The guarantee a research job may ask the sandbox for.
pub(crate) const FS_READONLY: &str = "fs-readonly";

/// How often a running job looks for a cancel or a shutdown.
const POLL: Duration = Duration::from_millis(100);

#[derive(Default)]
pub(crate) struct Research {
    cfg: Mutex<Option<Arc<r::ResearchConfig>>>,
}

/// A recorded process group: its id and its leader's start time.
type Group = (i64, Option<u64>);

/// Why the Node stops a job before its harness ends it (the API's limits, a cancel, a shutdown).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stop {
    Cancel,
    Shutdown,
    Deadline,
    /// The Hub was unreachable past the lease (`offline_until`).
    Offline,
    /// The draft and tool rows went past `max_output_bytes`.
    Output,
}

/// What `start_job` checked and resolved before accepting, for the run.
struct Plan {
    cfg: r::ResearchConfig,
    req: r::ResearchRequest,
    plan: r::AttemptPlan,
    codex: bool,
}

impl LocalNode {
    /// Host bridge: run research jobs with this setup (the harness executables, whose home is off
    /// limits, `~/.claude`, where attachment copies go). The job decides the rest: model, budget,
    /// deadline and sandbox. Without it a research job is `unsupported`.
    pub fn enable_research(&self, cfg: r::ResearchConfig) {
        *self.0.research.cfg.lock().unwrap() = Some(Arc::new(cfg));
    }

    pub(crate) async fn start_research(&self, c: Command<JobSpec>) -> ApiResult<Receipt> {
        let spec = &c.body;
        let exec = ExecutionRef::Job {
            job: spec.job.clone(),
            attempt: spec.run.attempt.clone(),
        };
        // a resend answers from the journal (or is a conflict), whatever changed in the setup since
        if self.0.j.read(|t| t.op(&c.id))?.is_some() {
            return self
                .accept("start_job", &c, |_| Ok(Decided::accepted()))
                .map(|(r, _)| r);
        }
        self.0.cfg.ceiling.check(&spec.run.policy)?;
        let plan = self.research_plan(spec)?;
        let jobs = self.0.cfg.capacity.jobs;
        let (r, fresh) = self.accept("start_job", &c, |t| {
            if t.unacked_bytes()? > self.0.cfg.storage.quota {
                return Err(ApiError::new(
                    ErrorCode::Exhausted,
                    "the hub has not taken this node's events: no new work until it does",
                ));
            }
            if running_jobs(t)? >= jobs {
                return Err(ApiError::new(ErrorCode::Exhausted, "no job capacity"));
            }
            if let Some((_, other)) = t.attempt(&spec.run.attempt)?
                && other != c.id
            {
                return Err(ApiError::new(
                    ErrorCode::Conflict,
                    "this attempt already ran under another operation",
                ));
            }
            t.take_attempt(&exec, &c.id)?;
            Ok(Decided {
                state: Observed::Running,
                execution: Some(exec.clone()),
                workspace: None,
                session: None,
                events: vec![(Subject::Execution(exec.clone()), EventBody::AttemptStarted)],
            })
        })?;
        if fresh {
            let me = self.clone();
            let op = c.id.clone();
            let spec = c.body.clone();
            self.spawn_owned(async move { me.run_research(op, exec, spec, plan).await });
        }
        Ok(r)
    }

    /// Everything that can be refused before the job is accepted.
    fn research_plan(&self, spec: &JobSpec) -> ApiResult<Plan> {
        let Some(base) = self.0.research.cfg.lock().unwrap().clone() else {
            return Err(ApiError::new(
                ErrorCode::Unsupported,
                "this node runs no research jobs",
            ));
        };
        if self.0.cfg.capacity.jobs == 0 {
            return Err(ApiError::new(
                ErrorCode::Unsupported,
                "this node runs no jobs",
            ));
        }
        spec.check()?;
        let JobWork::Research(job) = &spec.work else {
            return Err(ApiError::new(ErrorCode::Internal, "not a research job"));
        };
        let run = &spec.run;
        let codex = match run.choice.harness.as_str() {
            "claude-code" => false,
            "codex" if base.codex_executable.is_some() => true,
            h => {
                return Err(ApiError::new(
                    ErrorCode::Unsupported,
                    format!("no research harness {h} on this node"),
                ));
            }
        };
        let model = run.choice.model.clone().unwrap_or_default();
        if model.is_empty() && !codex {
            return Err(ApiError::new(
                ErrorCode::Invalid,
                "a research job on claude-code names its model",
            ));
        }
        if codex && run.continuation.is_some() {
            return Err(ApiError::new(
                ErrorCode::Invalid,
                "codex cannot continue a research session",
            ));
        }
        let p = &run.policy;
        if p.permissions != PermissionMode::Plan {
            return Err(ApiError::new(
                ErrorCode::Invalid,
                "a research job only reads: permissions plan",
            ));
        }
        if p.network != NetworkScope::Unrestricted {
            return Err(ApiError::new(
                ErrorCode::Unsupported,
                "this node cannot restrict a harness's network",
            ));
        }
        let sandbox = sandbox_mode(&p.sandbox, codex)?;
        Self::check_limits(&run.limits)?;
        let roots = self.0.roots();
        let cwd = job
            .cwd
            .as_ref()
            .map(|at| workspace::resolve(&roots, at, false))
            .transpose()?;
        let add_dirs = job
            .readable
            .iter()
            .map(|at| workspace::resolve(&roots, at, false))
            .collect::<ApiResult<Vec<PathBuf>>>()?;
        if job.steve_tools && self.0.tools_url.lock().unwrap().is_none() {
            return Err(ApiError::new(
                ErrorCode::Unsupported,
                "this node serves no Steve tools",
            ));
        }
        let mut cfg = (*base).clone();
        cfg.scratch = self.0.cfg.home.join("jobs");
        cfg.sandbox = sandbox;
        cfg.models = vec![model.clone()];
        // the deadline is the job's (one for all attempts of a run), measured when the harness
        // starts (after the attachments); no "still reading" notice
        cfg.budget = r::Budget {
            hard: None,
            soft: Duration::ZERO,
            max_turns: job.budget.max_turns,
            max_tools: job.budget.max_tools,
            idle: Duration::from_millis(job.budget.idle_ms),
        };
        let prompt = run
            .prompt
            .iter()
            .filter_map(|p| match p {
                Part::Text { text } => Some(text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n\n");
        let mut req = r::ResearchRequest::new(spec.job.as_str(), "", prompt.clone());
        req.cwd = cwd;
        req.add_dirs = add_dirs;
        req.budget = Some(cfg.budget);
        req.models = Some(vec![model.clone()]);
        req.effort = run.choice.effort.clone();
        req.harness = run.choice.harness.clone();
        Ok(Plan {
            plan: r::AttemptPlan {
                model,
                resume: run.continuation.as_ref().map(|h| h.to_string()),
                prompt,
            },
            cfg,
            req,
            codex,
        })
    }

    async fn run_research(self, op: OperationId, exec: ExecutionRef, spec: JobSpec, p: Plan) {
        let Plan {
            mut cfg,
            mut req,
            plan,
            codex,
        } = p;
        let inner = self.0.clone();
        let dir = workspace::dir_name(exec.attempt().as_str());
        let JobWork::Research(job) = &spec.work else {
            return;
        };
        let limits = &spec.run.limits;
        let mut tick = tokio::time::interval(POLL);
        // the prompt's files and images, from the Hub for this execution, while the job is wanted
        for part in &spec.run.prompt {
            let (artifact, name, earlier) = match part {
                Part::File { artifact, name } => (
                    artifact,
                    name.strip_prefix(ATTACHMENTS_DIR)
                        .unwrap_or(name)
                        .to_string(),
                    job.earlier.contains(name),
                ),
                Part::Image { artifact } => (artifact, image_name(artifact), false),
                _ => continue,
            };
            let fetch = crate::fetch(&inner, &exec, artifact);
            tokio::pin!(fetch);
            let fetched = loop {
                tokio::select! {
                    got = &mut fetch => break got,
                    _ = tick.tick() => {
                        if let Some(why) = self.stop_now(&op, limits, 0) {
                            let c = ended(Some(why), &r::RunStatus::Stopped, String::new(), None, 0, &[], false);
                            return self.end_research(&op, &exec, c, vec![]).await;
                        }
                    }
                }
            };
            match fetched {
                Ok(path) => req.attachments.push(r::Attachment {
                    path,
                    name,
                    mime: artifact.mime.clone(),
                    size: artifact.size,
                    earlier,
                }),
                Err(e) => {
                    let failed = r::RunStatus::Failed(format!("research attachments: {e}"));
                    let c = ended(None, &failed, String::new(), None, 0, &[], false);
                    return self.end_research(&op, &exec, c, vec![]).await;
                }
            }
        }
        // what is left of the job's deadline now: the harness cannot outlive it
        let left = Duration::from_millis(u64::try_from(limits.deadline.0 - now()).unwrap_or(0));
        if left.is_zero() {
            let c = ended(
                Some(Stop::Deadline),
                &r::RunStatus::Stopped,
                String::new(),
                None,
                0,
                &[],
                false,
            );
            return self.end_research(&op, &exec, c, vec![]).await;
        }
        cfg.budget.hard = Some(left);
        cfg.budget.soft = left;
        req.budget = Some(cfg.budget);
        if job.steve_tools
            && let Some(url) = inner.tools_url.lock().unwrap().clone()
        {
            req.mcp = Some(r::McpAccess {
                url,
                token: inner.tools.job_token(&exec),
            });
        }
        // the harness's process group is in the journal before it reads anything
        let holder = job::group_key(exec.attempt());
        let group: Arc<Mutex<Option<Group>>> = Arc::default();
        let on_spawn: r::SpawnHook = {
            let (inner, holder, group) = (inner.clone(), holder.clone(), group.clone());
            Arc::new(move |pid| {
                let pgid = i64::from(pid);
                let started = procs::start_time(pgid);
                let ok = inner
                    .j
                    .tx(|t| t.put_proc(&holder, pgid, 0, inner.incarnation, "research", started))
                    .is_ok();
                if ok {
                    *group.lock().unwrap() = Some((pgid, started));
                }
                ok
            })
        };
        let (events_tx, mut events) = mpsc::unbounded_channel();
        let (stop_tx, stop) = watch::channel(false);
        let one = r::OneAttempt {
            run_id: dir.clone(),
            plan,
            on_spawn: Some(on_spawn),
        };
        let run = r::run_attempt(&cfg, req, one, events_tx, stop);
        tokio::pin!(run);
        let mut draft = Draft::new(&exec);
        let mut output = 0u64;
        // the first reason the Node stopped it for
        let mut why: Option<Stop> = None;
        let outcome = loop {
            tokio::select! {
                out = &mut run => break out,
                Some(ev) = events.recv() => {
                    let evs = draft.on(ev);
                    output += output_bytes(&evs);
                    if !evs.is_empty() && self.emit(evs).is_err() {
                        // the journal is at its limit (or failed): running work stops
                        stop_tx.send_replace(true);
                    }
                    if why.is_none() && output > limits.max_output_bytes {
                        why = Some(Stop::Output);
                        stop_tx.send_replace(true);
                    }
                }
                _ = tick.tick() => {
                    if why.is_none() && let Some(w) = self.stop_now(&op, limits, output) {
                        why = Some(w);
                        stop_tx.send_replace(true);
                    }
                }
            }
        };
        // whatever the harness still said before it ended
        while let Ok(ev) = events.try_recv() {
            let evs = draft.on(ev);
            let _ = self.emit(evs);
        }
        inner.tools.revoke_job(&exec);
        let _ = tokio::fs::remove_dir_all(cfg.scratch.join(&dir)).await;
        let recorded = *group.lock().unwrap();
        if let Some((pgid, started)) = recorded {
            if !confirm_gone(pgid, started).await {
                // still running somewhere: it stays unknown, and the Hub decides
                let state = Observed::Unknown {
                    reason: "the research harness would not stop".into(),
                };
                return self.complete(&op, state, |_| Ok(())).await;
            }
            let _ = inner.j.tx(|t| t.drop_proc(&holder, pgid));
        }
        let usage = usage(&exec, &outcome, codex);
        let session = outcome
            .claude_session
            .as_deref()
            .and_then(|s| ResumeHandle::new(s).ok());
        let c = ended(
            why,
            &outcome.status,
            outcome.raw.clone(),
            session,
            outcome.turns,
            &outcome.files_read,
            outcome.sandboxed,
        );
        let mut evs = usage;
        evs.extend(draft.close());
        self.end_research(&op, &exec, c, evs).await
    }

    /// Whether the Node stops the job now, and why: a cancel, a shutdown, its deadline, the Hub
    /// unreachable past the lease, or its output past the cap (the actor's limits for a turn).
    fn stop_now(&self, op: &OperationId, limits: &Limits, output: u64) -> Option<Stop> {
        let inner = &self.0;
        let now = now();
        if inner
            .j
            .read(|t| t.op(op))
            .ok()
            .flatten()
            .is_some_and(|o| o.cancel)
        {
            Some(Stop::Cancel)
        } else if inner.closed.load(Ordering::SeqCst) {
            Some(Stop::Shutdown)
        } else if now >= limits.deadline.0 {
            Some(Stop::Deadline)
        } else if inner.hub_down_since().is_some_and(|since| since < now)
            && now >= limits.offline_until.0
        {
            Some(Stop::Offline)
        } else if output > limits.max_output_bytes {
            Some(Stop::Output)
        } else {
            None
        }
    }

    fn emit(&self, evs: Vec<(Subject, EventBody)>) -> ApiResult<()> {
        let r = self.0.j.tx(|t| emit_all(&self.0, t, evs));
        self.0.wake.notify_one();
        r
    }

    /// The attempt's end: `before` (its usage, the last message's end), then `AttemptEnded`, in the
    /// same transaction as its state.
    async fn end_research(
        &self,
        op: &OperationId,
        exec: &ExecutionRef,
        c: Completion,
        before: Vec<(Subject, EventBody)>,
    ) {
        let inner = self.0.clone();
        loop {
            let r = inner.j.tx(|t| {
                let state = Observed::Finished {
                    outcome: c.outcome,
                    result: Some(OperationResult::Execution(c.clone())),
                    error: c.error.clone(),
                };
                t.set_state(op, &state)?;
                let mut evs = before.clone();
                evs.push((
                    Subject::Execution(exec.clone()),
                    EventBody::AttemptEnded {
                        completion: c.clone(),
                    },
                ));
                emit_all(&inner, t, evs)
            });
            inner.wake.notify_one();
            match r {
                Ok(()) => return,
                Err(e) => {
                    tracing::error!("research job {op}: {e}");
                    inner.fault();
                    if inner.closed.load(Ordering::SeqCst) {
                        return;
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
            }
        }
    }
}

/// Whether the attempt's process group is gone. steve-research has killed it already, so a group
/// whose leader is gone while a member still dies is waited for (briefly); one still running is
/// stopped here.
async fn confirm_gone(pgid: i64, started: Option<u64>) -> bool {
    let until = tokio::time::Instant::now() + Duration::from_secs(3);
    loop {
        match procs::state(pgid, started) {
            procs::Stopped::Gone => return true,
            procs::Stopped::Alive => return procs::stop(pgid, started).await.gone(),
            procs::Stopped::Uncertain if tokio::time::Instant::now() < until => {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            procs::Stopped::Uncertain => return false,
        }
    }
}

/// Jobs this Node runs now (think and research share the capacity).
fn running_jobs(t: &Tx) -> ApiResult<u32> {
    Ok(t.ops()?
        .into_iter()
        .filter(|(_, o)| o.method == "start_job" && !o.receipt.state.is_finished())
        .count() as u32)
}

/// The sandbox a research job asks for, as steve-research's mode: `fs-readonly` required (refused
/// when this machine has no `sandbox-exec`, or for Codex, which sandboxes itself and cannot be
/// wrapped), preferred (when available; Codex never), or none.
fn sandbox_mode(s: &SandboxRequirement, codex: bool) -> ApiResult<r::SandboxMode> {
    let only_fs = |g: &[String]| g.iter().all(|g| g == FS_READONLY);
    let unsupported = |why: &str| Err(ApiError::new(ErrorCode::Unsupported, why));
    match s {
        SandboxRequirement::None => Ok(r::SandboxMode::Off),
        SandboxRequirement::Required { guarantees } if guarantees.is_empty() => {
            Ok(r::SandboxMode::Off)
        }
        SandboxRequirement::Required { guarantees } if only_fs(guarantees) => {
            if codex {
                unsupported("sandbox-exec cannot wrap codex")
            } else if !r::sandbox::available() {
                unsupported("no sandbox-exec on this node")
            } else {
                Ok(r::SandboxMode::Required)
            }
        }
        SandboxRequirement::AllowFallback {
            preferred,
            acceptable,
        } if only_fs(preferred) && acceptable.is_empty() => Ok(if codex {
            r::SandboxMode::Off
        } else {
            r::SandboxMode::Auto
        }),
        _ => unsupported("this node's research sandbox only gives fs-readonly"),
    }
}

/// What the draft and tool rows add to a job's output.
fn output_bytes(evs: &[(Subject, EventBody)]) -> u64 {
    evs.iter()
        .map(|(_, b)| match b {
            EventBody::TextDelta { text, .. } => text.len() as u64,
            EventBody::ToolChanged { tool } => {
                serde_json::to_vec(tool).map_or(0, |v| v.len() as u64)
            }
            _ => 0,
        })
        .sum()
}

/// The name an inline image goes by in the run's files.
fn image_name(a: &ArtifactRef) -> String {
    let ext = match a.mime.split(';').next().unwrap_or("").trim() {
        "image/png" => ".png",
        "image/jpeg" => ".jpg",
        "image/gif" => ".gif",
        "image/webp" => ".webp",
        _ => "",
    };
    let h = a.sha256.as_str().trim_start_matches("sha256:");
    format!("image-{}{ext}", &h[..h.len().min(8)])
}

fn job_report(status: &r::RunStatus, turns: u32, files: &[String], sandboxed: bool) -> JobReport {
    let end = match status {
        r::RunStatus::Done => JobEnd::Done,
        r::RunStatus::Failed(_) => JobEnd::Failed,
        r::RunStatus::Stopped => JobEnd::Stopped,
        r::RunStatus::Budget(b) => JobEnd::Budget {
            hit: match b {
                r::BudgetHit::Time => BudgetHit::Time,
                r::BudgetHit::Turns => BudgetHit::Turns,
                r::BudgetHit::Tools => BudgetHit::Tools,
                r::BudgetHit::Idle => BudgetHit::Idle,
            },
        },
        r::RunStatus::Violated(v) => {
            let (rule, target) = match v {
                r::Violation::Tool { .. } => (ViolationRule::Tool, None),
                r::Violation::OutOfScope { path, .. } => {
                    (ViolationRule::OutOfScope, Some(path.clone()))
                }
                r::Violation::Sensitive { path, .. } => {
                    (ViolationRule::Sensitive, Some(path.clone()))
                }
                r::Violation::BadInput { .. } => (ViolationRule::BadInput, None),
            };
            JobEnd::Violated {
                rule,
                tool: v.tool().to_string(),
                target,
            }
        }
    };
    JobReport {
        end,
        turns,
        files_read: files.to_vec(),
        sandboxed,
    }
}

/// The attempt's end, with the reason the Node stopped it if it did: a harness that ended because
/// it was stopped ends as that reason says (a deadline as the time budget, a shutdown as
/// interrupted, the lease and the output cap with their errors); one that ended otherwise first
/// keeps its own end.
fn ended(
    why: Option<Stop>,
    status: &r::RunStatus,
    raw: String,
    continuation: Option<ResumeHandle>,
    turns: u32,
    files: &[String],
    sandboxed: bool,
) -> Completion {
    let stopped = *status == r::RunStatus::Stopped;
    let status = match why {
        Some(Stop::Deadline) if stopped => r::RunStatus::Budget(r::BudgetHit::Time),
        _ => status.clone(),
    };
    let report = job_report(&status, turns, files, sandboxed);
    let mut c = completion(&status, raw, continuation, report);
    if !stopped {
        return c;
    }
    match why {
        // stopped because this Node closes, not by the Hub: its processes are gone
        Some(Stop::Shutdown) => {
            c.outcome = Outcome::Interrupted;
            c.stop_reason = "node_shutdown".into();
        }
        Some(Stop::Offline) => {
            c.outcome = Outcome::TimedOut;
            c.stop_reason = "offline".into();
            c.error = Some(ApiError::new(
                ErrorCode::Offline,
                "the Hub was unreachable past the lease",
            ));
        }
        Some(Stop::Output) => {
            c.outcome = Outcome::Cancelled;
            c.stop_reason = "max_output".into();
            c.error = Some(ApiError::new(
                ErrorCode::Exhausted,
                "the output limit was reached",
            ));
        }
        Some(Stop::Cancel | Stop::Deadline) | None => {}
    }
    c
}

/// The attempt's end in Steve's terms. The answer is what the model wrote (the Hub shapes it:
/// offer, marks, citations); a stopped, cut or violating run keeps what it wrote until then.
fn completion(
    status: &r::RunStatus,
    raw: String,
    continuation: Option<ResumeHandle>,
    job: JobReport,
) -> Completion {
    let (outcome, stop_reason, error) = match status {
        r::RunStatus::Done => (Outcome::Succeeded, "end_turn", None),
        r::RunStatus::Stopped => (Outcome::Cancelled, "cancelled", None),
        r::RunStatus::Budget(r::BudgetHit::Time) => (
            Outcome::TimedOut,
            "deadline",
            Some(ApiError::new(ErrorCode::Expired, "past the job's deadline")),
        ),
        r::RunStatus::Budget(r::BudgetHit::Idle) => (
            Outcome::TimedOut,
            "idle",
            Some(ApiError::new(
                ErrorCode::Expired,
                "the harness said nothing for too long",
            )),
        ),
        r::RunStatus::Budget(_) => (
            Outcome::Failed,
            "max_turns",
            Some(ApiError::new(
                ErrorCode::Exhausted,
                "research budget reached",
            )),
        ),
        r::RunStatus::Violated(v) => (
            Outcome::Failed,
            "violation",
            Some(ApiError::new(ErrorCode::Denied, v.to_string())),
        ),
        r::RunStatus::Failed(e) => (
            Outcome::Failed,
            "error",
            Some(ApiError::new(ErrorCode::Internal, e.clone())),
        ),
    };
    Completion {
        outcome,
        stop_reason: stop_reason.into(),
        error,
        result: if raw.is_empty() {
            vec![]
        } else {
            vec![Part::Text { text: raw }]
        },
        continuation,
        transient: false,
        acted: false,
        job: Some(job),
    }
}

/// The attempt's tokens and dollars from the harness's closing result, booked to its model, with
/// ids of this attempt (a resent report changes nothing).
fn usage(exec: &ExecutionRef, out: &r::ResearchOutcome, codex: bool) -> Vec<(Subject, EventBody)> {
    let model = out.model.clone().unwrap_or_default();
    let reports = match &out.result {
        Some(d) if codex => d
            .get("usage")
            .filter(|u| u.is_object())
            .map(|u| vec![steve_print::codex_usage(u, &model)])
            .unwrap_or_default(),
        Some(d) => steve_print::claude_usage(d, &model),
        None => vec![],
    };
    let attempt = exec.attempt().as_str();
    let Ok(meter) = Epoch::new(derived_id("research", attempt)) else {
        return vec![];
    };
    reports
        .into_iter()
        .enumerate()
        .map(|(i, mut report)| {
            report.report_id = derived_id("research", &format!("{attempt}.{i}"));
            report.meter_epoch = meter.clone();
            (
                Subject::Execution(exec.clone()),
                EventBody::Usage { report },
            )
        })
        .collect()
}

/// Turns the run's events into Steve's: the draft as messages and deltas (a new message replaces
/// the draft so far), tool calls as their whole current view.
struct Draft {
    exec: ExecutionRef,
    n: u32,
    /// The current message's text as sent.
    sent: String,
}

impl Draft {
    fn new(exec: &ExecutionRef) -> Draft {
        Draft {
            exec: exec.clone(),
            n: 0,
            sent: String::new(),
        }
    }

    fn message(&self) -> String {
        format!("{}.m{}", self.exec.attempt(), self.n)
    }

    fn on(&mut self, ev: r::ResearchEvent) -> Vec<(Subject, EventBody)> {
        let subject = Subject::Execution(self.exec.clone());
        match ev {
            r::ResearchEvent::Text { text } => {
                let mut out = vec![];
                if self.n == 0 || !text.starts_with(&self.sent) {
                    if self.n > 0 {
                        out.push((
                            subject.clone(),
                            EventBody::MessageCompleted {
                                message: self.message(),
                            },
                        ));
                    }
                    self.n += 1;
                    self.sent.clear();
                    out.push((
                        subject.clone(),
                        EventBody::MessageStarted {
                            message: self.message(),
                            block: "text".into(),
                            role: Role::Assistant,
                        },
                    ));
                }
                let new = &text[self.sent.len()..];
                if !new.is_empty() {
                    out.push((
                        subject,
                        EventBody::TextDelta {
                            message: self.message(),
                            block: "text".into(),
                            utf8_offset: self.sent.len() as u64,
                            text: new.to_string(),
                        },
                    ));
                }
                self.sent = text;
                out
            }
            r::ResearchEvent::ToolStarted(t) | r::ResearchEvent::ToolDone(t) => {
                vec![(
                    subject,
                    EventBody::ToolChanged {
                        tool: tool_view(&t),
                    },
                )]
            }
            // the session is in the completion; the attempt started at accept; no notice
            r::ResearchEvent::Started { .. }
            | r::ResearchEvent::Session { .. }
            | r::ResearchEvent::StillReading => vec![],
        }
    }

    /// The end of the last message, if there was one.
    fn close(&mut self) -> Vec<(Subject, EventBody)> {
        if self.n == 0 {
            return vec![];
        }
        vec![(
            Subject::Execution(self.exec.clone()),
            EventBody::MessageCompleted {
                message: self.message(),
            },
        )]
    }
}

fn tool_view(t: &r::ToolSeen) -> ToolView {
    let steve = t.name.starts_with("mcp__steve__");
    let category = if steve {
        ToolCategory::Steve
    } else {
        match t.name.as_str() {
            "Read" => ToolCategory::Read,
            "Grep" | "Glob" => ToolCategory::Search,
            "Shell" => ToolCategory::Exec,
            _ => ToolCategory::Other,
        }
    };
    let id = CallId::new(t.id.clone())
        .or_else(|_| CallId::new(derived_id("tool", &t.id)))
        .expect("derived id");
    ToolView {
        id,
        parent: None,
        origin: if steve {
            ToolOrigin::Steve
        } else {
            ToolOrigin::Harness
        },
        name: t.name.clone(),
        category,
        title: if t.brief.is_empty() {
            t.name.clone()
        } else {
            t.brief.clone()
        },
        description: None,
        target: (!t.brief.is_empty()).then(|| t.brief.clone()),
        phase: match t.ok {
            None => ToolPhase::Running,
            Some(true) => ToolPhase::Succeeded,
            Some(false) => ToolPhase::Failed,
        },
        action: None,
        result: vec![],
        error: t.error.map(|e| e.code().to_string()),
        duration_ms: t.ok.map(|_| t.ms),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn exec() -> ExecutionRef {
        ExecutionRef::Job {
            job: JobId::new("j1").unwrap(),
            attempt: AttemptId::new("a1").unwrap(),
        }
    }

    fn deltas(evs: &[(Subject, EventBody)]) -> Vec<String> {
        evs.iter()
            .map(|(_, b)| match b {
                EventBody::MessageStarted { message, .. } => format!("start {message}"),
                EventBody::MessageCompleted { message } => format!("end {message}"),
                EventBody::TextDelta {
                    message,
                    utf8_offset,
                    text,
                    ..
                } => format!("{message}@{utf8_offset}:{text}"),
                other => format!("{other:?}"),
            })
            .collect()
    }

    #[test]
    fn a_draft_grows_by_deltas_and_a_new_message_replaces_it() {
        let mut d = Draft::new(&exec());
        let text = |t: &str| r::ResearchEvent::Text { text: t.into() };
        let mut all = d.on(text("我先"));
        all.extend(d.on(text("我先看看")));
        all.extend(d.on(text("答案")));
        all.extend(d.close());
        assert_eq!(
            deltas(&all),
            [
                "start a1.m1",
                "a1.m1@0:我先",
                "a1.m1@6:看看",
                "end a1.m1",
                "start a1.m2",
                "a1.m2@0:答案",
                "end a1.m2",
            ]
        );
        assert!(d.on(r::ResearchEvent::StillReading).is_empty());
    }

    #[test]
    fn only_fs_readonly_is_a_research_sandbox() {
        let req = |g: &[&str]| SandboxRequirement::Required {
            guarantees: g.iter().map(|s| s.to_string()).collect(),
        };
        assert_eq!(
            sandbox_mode(&SandboxRequirement::None, false).unwrap(),
            r::SandboxMode::Off
        );
        let prefer = SandboxRequirement::AllowFallback {
            preferred: vec![FS_READONLY.into()],
            acceptable: vec![],
        };
        assert_eq!(sandbox_mode(&prefer, false).unwrap(), r::SandboxMode::Auto);
        // Codex sandboxes itself and cannot be wrapped: preferred is none, required is refused
        assert_eq!(sandbox_mode(&prefer, true).unwrap(), r::SandboxMode::Off);
        assert_eq!(
            sandbox_mode(&req(&[FS_READONLY]), true).unwrap_err().code,
            ErrorCode::Unsupported
        );
        assert_eq!(
            sandbox_mode(&req(&["no-network"]), false).unwrap_err().code,
            ErrorCode::Unsupported
        );
        match sandbox_mode(&req(&[FS_READONLY]), false) {
            Ok(m) => {
                assert!(r::sandbox::available());
                assert_eq!(m, r::SandboxMode::Required);
            }
            Err(e) => {
                assert!(!r::sandbox::available());
                assert_eq!(e.code, ErrorCode::Unsupported);
            }
        }
    }

    #[test]
    fn every_end_fits_its_outcome() {
        let ends = [
            r::RunStatus::Done,
            r::RunStatus::Stopped,
            r::RunStatus::Failed("x".into()),
            r::RunStatus::Budget(r::BudgetHit::Time),
            r::RunStatus::Budget(r::BudgetHit::Tools),
            r::RunStatus::Violated(r::Violation::Sensitive {
                tool: "Read".into(),
                path: "~/.ssh/id_rsa".into(),
            }),
        ];
        for s in ends {
            let c = completion(&s, "t".into(), None, job_report(&s, 1, &[], false));
            let job = c.job.clone().unwrap();
            assert!(job.end.fits(c.outcome), "{s:?} → {:?}", c.outcome);
            assert_eq!(
                c.error.is_none(),
                c.outcome != Outcome::Failed && c.outcome != Outcome::TimedOut
            );
        }
        let v = job_report(
            &r::RunStatus::Violated(r::Violation::OutOfScope {
                tool: "Grep".into(),
                path: "/etc".into(),
            }),
            0,
            &[],
            true,
        );
        assert_eq!(
            v.end,
            JobEnd::Violated {
                rule: ViolationRule::OutOfScope,
                tool: "Grep".into(),
                target: Some("/etc".into()),
            }
        );
    }
}
