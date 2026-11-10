//! Research jobs: one read-only research attempt per job, hosted on this machine.
//!
//! The Hub chose the model and built the prompt; this Node resolves the directories through its
//! roots, fetches the prompt's files from the Hub, runs exactly that one attempt through its ACP
//! adapter (read-only mode, the guard that kills on the first bad call, and the policy's sandbox)
//! and reports draft, tool rows, usage and end. Steve's read-only tools reach the Hub through the Node's loopback endpoint with a token
//! of this job only, gone when it ends. Off until the host enables it ([`LocalNode::enable_research`]).

use std::path::PathBuf;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use steve_node_api::*;
use steve_research as r;
use tokio::sync::mpsc;

use crate::journal::Tx;
use crate::{Decided, LocalNode, emit_all, job, now, procs, workspace};

#[path = "research_acp.rs"]
pub(crate) mod acp;

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
}

impl LocalNode {
    /// Host bridge: run research jobs with this setup (whose home is off limits, where attachment
    /// copies go). The job decides the rest: harness, model, budget, deadline and sandbox. Without
    /// it a research job is `unsupported`.
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
        if !self.0.cfg.acp.adapters.contains_key(&run.choice.harness) {
            return Err(ApiError::new(
                ErrorCode::Unsupported,
                format!("no research harness {} on this node", run.choice.harness),
            ));
        }
        let model = run.choice.model.clone().unwrap_or_default();
        let p = &run.policy;
        // a research job only reads (plan), or acts for the user with the user's own access
        // (full_access: no read-only sandbox, no guard; the Node's ceiling was checked on accept)
        let full = match p.permissions {
            PermissionMode::Plan => false,
            PermissionMode::FullAccess if p.sandbox == SandboxRequirement::None => true,
            _ => {
                return Err(ApiError::new(
                    ErrorCode::Invalid,
                    "a research job's permissions are plan, or full_access without a sandbox",
                ));
            }
        };
        if p.network != NetworkScope::Unrestricted {
            return Err(ApiError::new(
                ErrorCode::Unsupported,
                "this node cannot restrict a harness's network",
            ));
        }
        let sandbox = acp::sandbox_mode(
            &p.sandbox,
            &self.0.cfg.acp.adapter_properties(&run.choice.harness),
        )?;
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
            exit_wait: r::EXIT_WAIT,
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
        if full && req.cwd.is_none() {
            // the user's own home, as a terminal opens there
            req.cwd = Some(self.0.cfg.acp.user_home.clone());
        }
        Ok(Plan {
            plan: r::AttemptPlan {
                model,
                resume: run.continuation.as_ref().map(|h| h.to_string()),
                prompt,
            },
            cfg,
            req,
        })
    }

    async fn run_research(self, op: OperationId, exec: ExecutionRef, spec: JobSpec, p: Plan) {
        let Plan {
            mut cfg,
            mut req,
            plan,
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
        let on_spawn = {
            let (inner, holder, group) = (inner.clone(), holder.clone(), group.clone());
            move |pid: u32| {
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
            }
        };
        let (outcome, why, tail) = self
            .research_acp_attempt(&op, &exec, &spec, &cfg, &req, &plan, &on_spawn)
            .await;
        inner.tools.revoke_job(&exec);
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
        // A cancelled ACP future sends TERM before the process is reaped. Keep its directories
        // until the whole group is confirmed gone, so a dying child cannot recreate them.
        let _ = tokio::fs::remove_dir_all(cfg.scratch.join(&dir)).await;
        let _ = tokio::fs::remove_dir_all(cfg.files_root.join(&dir)).await;
        let session = outcome
            .session
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
        self.end_research(&op, &exec, c, tail).await
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
            procs::Stopped::Alive => {
                if procs::stop(pgid, started).await.gone() {
                    return true;
                }
            }
            procs::Stopped::Uncertain => {}
        }
        // The leader can exit between state() and stop()'s ownership check. That is not
        // evidence of a surviving process: wait for the kernel's group-gone proof, just as
        // when the first probe was already uncertain. Never signal an uncertain group.
        if tokio::time::Instant::now() >= until {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Jobs this Node runs now (think and research share the capacity).
fn running_jobs(t: &Tx) -> ApiResult<u32> {
    Ok(t.ops()?
        .into_iter()
        .filter(|(_, o)| o.method == "start_job" && !o.receipt.state.is_finished())
        .count() as u32)
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
            c.error = Some(NodeStop::HubUnreachable.error());
        }
        Some(Stop::Output) => {
            c.outcome = Outcome::Cancelled;
            c.stop_reason = "max_output".into();
            c.error = Some(NodeStop::OutputLimit.error());
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
            Some(NodeStop::JobDeadline.error()),
        ),
        r::RunStatus::Budget(r::BudgetHit::Idle) => (
            Outcome::TimedOut,
            "idle",
            Some(NodeStop::HarnessIdle.error()),
        ),
        r::RunStatus::Budget(_) => (
            Outcome::Failed,
            "max_turns",
            Some(NodeStop::ResearchBudget.error()),
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

#[cfg(test)]
mod tests {
    use super::*;

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
