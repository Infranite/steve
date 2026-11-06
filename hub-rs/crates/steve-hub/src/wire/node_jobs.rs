//! Steve's think calls as streaming ACP jobs on the local Node.
//!
//! The Hub keeps everything that is policy: core's [`steve_core::Fallback`] chooses the models
//! (设置 · 模型), tries them in turn, counts attempts and failures and books the statistics rows.
//! [`NodePrint`] is its [`OneModel`]: each try of one model is one job with one attempt, run by
//! the Node with exactly that model and never retried there.
//!
//! - **Start**: the job, its attempt and the `start_job` command commit in one transaction; the
//!   coordinator's send worker sends it and merges the receipt like any other command.
//! - **Facts**: the Node's events of the job come through the same inbox and apply worker as a
//!   session's, without `events` rows (think calls never were transcripts). The attempt's row moves
//!   forward, and the caller's waiter hears the draft (whole text of the current message), the usage
//!   and the end.
//! - **Cancel**: a dropped call (the stop button, a `select!`, the Hub's own timer) asks the Node to
//!   cancel by operation id; a start that provably never left the Hub ends here instead.
//! - **Restart**: the callers are gone, so open attempts of an earlier process are cancelled (or
//!   abandoned when the Node cannot tell their end) once the Hub has reconciled, and never run again.

use std::collections::HashSet;
use std::path::PathBuf;
use std::time::Duration;

use steve_core::gateway::{OnText, OneCall, OneModel, ThinkTools, Tried, split_static};
use steve_store::jobs::{self, AttemptState};
use tokio::sync::mpsc;

use super::*;

/// What a job attempt's caller hears, in the order the Node said it.
#[derive(Debug)]
pub(super) enum News {
    /// The whole text of the model's current message, each time it grew.
    Text(String),
    Tool(ToolView),
    Usage(UsageReport),
    End(Completion),
}

/// Convert one attempt's meter reports into accounting rows. A cumulative snapshot replaces
/// earlier figures, and a final figure replaces deltas field by field (missing is unknown,
/// not zero). Report ids remain the replay boundary.
pub(super) fn normalized_usage(
    reports: impl IntoIterator<Item = UsageReport>,
    fallback_model: &str,
) -> Vec<UsageReport> {
    fn add(a: Option<u64>, b: Option<u64>) -> Option<u64> {
        match (a, b) {
            (Some(a), Some(b)) => Some(a.saturating_add(b)),
            (a, b) => a.or(b),
        }
    }
    let mut seen = HashSet::new();
    let mut totals: Vec<UsageReport> = vec![];
    for mut report in reports {
        if !seen.insert(report.report_id.clone()) {
            continue;
        }
        if report.model.as_deref().is_none_or(str::is_empty) {
            report.model = Some(fallback_model.to_string());
        }
        let Some(total) = totals.iter_mut().find(|t| {
            t.meter_epoch == report.meter_epoch
                && t.model == report.model
                && t.scope == report.scope
        }) else {
            totals.push(report);
            continue;
        };
        if total.measurement == Measurement::Final && report.measurement != Measurement::Final {
            continue;
        }
        let delta = report.measurement == Measurement::Delta;
        let merge = |a, b| if delta { add(a, b) } else { b.or(a) };
        total.tokens = Tokens {
            input: merge(total.tokens.input, report.tokens.input),
            output: merge(total.tokens.output, report.tokens.output),
            cache_read: merge(total.tokens.cache_read, report.tokens.cache_read),
            cache_write: merge(total.tokens.cache_write, report.tokens.cache_write),
            reasoning: merge(total.tokens.reasoning, report.tokens.reasoning),
        };
        if let Some(cost) = report.cost {
            total.cost = Some(if delta {
                match total
                    .cost
                    .as_ref()
                    .filter(|old| old.currency == cost.currency)
                {
                    Some(old) => Money {
                        decimal: (steve_print::cost_f64(old).unwrap_or(0.0)
                            + steve_print::cost_f64(&cost).unwrap_or(0.0))
                        .to_string(),
                        currency: cost.currency,
                    },
                    None => cost,
                }
            } else {
                cost
            });
        }
        total.context_used = report.context_used.or(total.context_used);
        total.context_limit = report.context_limit.or(total.context_limit);
        total.measurement = report.measurement;
        total.report_id = report.report_id;
    }
    // `all` already includes the main loop. Keep a main-only dollar figure if no all figure
    // reported dollars, but never count the main tokens twice.
    let all_epochs: HashSet<_> = totals
        .iter()
        .filter(|r| r.scope == UsageScope::All)
        .map(|r| r.meter_epoch.clone())
        .collect();
    let priced_epochs: HashSet<_> = totals
        .iter()
        .filter(|r| r.scope == UsageScope::All && r.cost.is_some())
        .map(|r| r.meter_epoch.clone())
        .collect();
    totals.retain_mut(|r| {
        if r.scope == UsageScope::Main && all_epochs.contains(&r.meter_epoch) {
            r.tokens = Tokens::default();
            if priced_epochs.contains(&r.meter_epoch) {
                r.cost = None;
            }
            r.cost.is_some()
        } else {
            true
        }
    });
    totals
}

/// The callers waiting on their job attempts in this process, by attempt.
#[derive(Default)]
pub(super) struct Waiters(Mutex<HashMap<String, Waiter>>);

struct Waiter {
    /// The message being written and its text so far.
    message: Option<String>,
    text: String,
    tx: mpsc::UnboundedSender<News>,
}

impl Waiters {
    pub(super) fn add(&self, attempt: &str) -> mpsc::UnboundedReceiver<News> {
        let (tx, rx) = mpsc::unbounded_channel();
        self.0.lock().unwrap().insert(
            attempt.to_string(),
            Waiter {
                message: None,
                text: String::new(),
                tx,
            },
        );
        rx
    }

    fn remove(&self, attempt: &str) {
        self.0.lock().unwrap().remove(attempt);
    }

    pub(super) fn has(&self, attempt: &str) -> bool {
        self.0.lock().unwrap().contains_key(attempt)
    }

    /// Tells the attempt's caller, if it still waits, what `f` makes of the event.
    fn tell(&self, attempt: &str, f: impl FnOnce(&mut Waiter) -> Option<News>) {
        if let Some(w) = self.0.lock().unwrap().get_mut(attempt)
            && let Some(news) = f(w)
        {
            let _ = w.tx.send(news);
        }
    }
}

/// One try of one model as a think job: what [`NodeRuntime::start_think`] freezes into its spec.
pub(super) struct Think {
    /// The attempt's id, minted by its caller (whose waiter is registered under it first).
    attempt: String,
    /// The call's purpose (`CallTag`), kept with the job.
    purpose: &'static str,
    /// The call's workspace; empty: the default one.
    workspace: String,
    head: Option<String>,
    message: String,
    tools: ThinkTools,
    images: Vec<steve_print::Image>,
    codex: bool,
    model: String,
    effort: Option<String>,
    timeout: Duration,
}

/// The completion the Hub writes for an attempt it ends on its own word.
fn ended(outcome: Outcome, stop_reason: &str, error: Option<ApiError>) -> Completion {
    Completion {
        outcome,
        stop_reason: stop_reason.into(),
        error,
        result: vec![],
        continuation: None,
        transient: false,
        acted: false,
        job: None,
    }
}

/// The job attempt a command is about, if it is one.
pub(super) fn job_attempt_of(tx: &Tx, op: &str) -> Result<Option<String>, StoreError> {
    Ok(tx
        .query_row(
            "SELECT target_job_attempt FROM node_commands WHERE operation_id=?",
            [op],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten())
}

/// The `start_job` of an attempt: its operation, state and whether it may have left the Hub.
fn start_of(tx: &Tx, attempt: &str) -> Result<(String, String, bool), StoreError> {
    Ok(tx.query_row(
        "SELECT c.operation_id, c.state, c.maybe_sent FROM runtime_job_attempts a JOIN node_commands c ON c.operation_id=a.operation_id WHERE a.attempt_id=?",
        [attempt],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?)
}

impl NodeRuntime {
    /// Commits a think job (one attempt, `t.model` only) with its `start_job`. Its caller's waiter
    /// is registered first; a caller gone before the commit (no waiter any more) has its job
    /// stopped right away. Its files are the attachments' own; its images become job inputs of the
    /// workspace, so every part of the prompt is an artifact the Node fetches.
    pub(super) fn start_think(
        &self,
        data_dir: &std::path::Path,
        t: &Think,
    ) -> Result<(), StoreError> {
        self.running()?;
        let mut files = vec![];
        if let ThinkTools::Read(readable) = &t.tools {
            for r in readable {
                let row = self
                    .store
                    .files()
                    .get(&r.file_id)?
                    .ok_or_else(|| conflict(format!("attachment {} is gone", r.file_id)))?;
                files.push((row, r.rel_path.clone()));
            }
        }
        // the attachments' workspace, else the call's (they read nothing of another)
        let workspace = files
            .first()
            .map(|(row, _)| row.workspace_id.clone())
            .unwrap_or_else(|| match t.workspace.as_str() {
                "" => steve_store::WS.to_string(),
                w => w.to_string(),
            });
        let mut prompt = vec![Part::Text {
            text: t.message.to_string(),
        }];
        for img in &t.images {
            let artifact = crate::wire::node_files::job_input(
                &self.store,
                data_dir,
                &workspace,
                &img.mime,
                &img.bytes,
            )
            .map_err(|e| conflict(e.safe_detail))?;
            prompt.push(Part::Image { artifact });
        }
        for (row, name) in &files {
            if row.workspace_id != workspace || row.size < 0 {
                return Err(conflict("attachments of another workspace"));
            }
            prompt.push(Part::File {
                artifact: ArtifactRef {
                    id: idv(row.id.clone())?,
                    sha256: idv(format!("sha256:{}", row.sha256))?,
                    size: row.size as u64,
                    mime: row.mime.clone(),
                },
                name: name.clone(),
            });
        }
        let job = uid("job");
        let attempt = t.attempt.clone();
        let op = format!("op_{attempt}");
        let deadline = now() + i64::try_from(t.timeout.as_millis()).unwrap_or(i64::MAX / 2);
        let spec = JobSpec {
            job: idv(job.clone())?,
            run: RunSpec {
                attempt: idv(attempt.clone())?,
                choice: HarnessChoice {
                    harness: if t.codex { "codex" } else { "claude-code" }.into(),
                    model: (!t.model.is_empty()).then(|| t.model.to_string()),
                    effort: t.effort.clone(),
                },
                directory: RunDirectory::Scratch,
                prompt,
                policy: ExecutionPolicy {
                    revision: idv("think")?,
                    permissions: PermissionMode::Ask,
                    sandbox: SandboxRequirement::AllowFallback {
                        preferred: vec!["fs-readonly".into()],
                        acceptable: vec![],
                    },
                    readable: vec![],
                    network: NetworkScope::Unrestricted,
                    tools: idv("hub")?,
                },
                limits: Limits {
                    deadline: Timestamp(deadline),
                    offline_until: Timestamp(deadline),
                    max_output_bytes: 256 << 20,
                    max_tokens: None,
                    max_cost: None,
                },
                continuation: None,
            },
            work: JobWork::Think(ThinkJob {
                instructions: t.head.clone(),
                tools: if t.tools.any() {
                    ThinkToolMode::ReadAttached
                } else {
                    ThinkToolMode::None
                },
            }),
        };
        let exec = ExecutionRef::Job {
            job: spec.job.clone(),
            attempt: spec.run.attempt.clone(),
        };
        let (req, digest) = command(&op, spec.clone())?;
        self.tx(|tx| {
            jobs::create_job_in(
                tx,
                &jobs::NewJob {
                    id: &job,
                    kind: jobs::JobKind::Think,
                    purpose: t.purpose,
                    workspace: &workspace,
                    node: self.node_id.as_str(),
                    thread_run: None,
                },
                &jobs::NewJobAttempt {
                    id: &attempt,
                    ordinal: 0,
                    model: &t.model,
                    operation: &op,
                    execution: &serde_json::to_value(&exec).map_err(conflict)?,
                    spec: &serde_json::to_value(&spec).map_err(conflict)?,
                    digest: &digest,
                },
                &jobs::StartJob {
                    request: &req,
                    digest: &digest,
                },
                now(),
            )
        })?;
        if !self.jobs.has(&attempt) {
            self.stop_job(&attempt);
        }
        self.wake_send.notify_one();
        Ok(())
    }

    /// A Node event of job `attempt` (the apply worker's transaction): the attempt's row moves
    /// forward and its caller hears the draft, the usage and the end. An attempt the Hub has no
    /// record of is passed over.
    pub(super) fn job_event_in(
        &self,
        tx: &Tx,
        ev: &NodeEvent,
        attempt: &str,
    ) -> Result<(), StoreError> {
        let Some(row) = jobs::attempt_in(tx, attempt)? else {
            return Ok(());
        };
        if row.node != self.node_id.as_str() {
            return Ok(());
        }
        match &ev.body {
            EventBody::AttemptStarted => {
                jobs::apply_attempt_in(tx, attempt, AttemptState::Running, None, None, now())?;
            }
            EventBody::MessageStarted { message, .. } => self.jobs.tell(attempt, |w| {
                w.message = Some(message.clone());
                w.text.clear();
                None
            }),
            EventBody::TextDelta {
                message,
                utf8_offset,
                text,
                ..
            } => self.jobs.tell(attempt, |w| {
                if w.message.as_deref() != Some(message.as_str()) {
                    w.message = Some(message.clone());
                    w.text.clear();
                }
                // placed at its offset: the same delta applied again (a replay) changes nothing
                let at = *utf8_offset as usize;
                if at <= w.text.len() && w.text.is_char_boundary(at) {
                    w.text.truncate(at);
                }
                w.text.push_str(text);
                (!text.is_empty()).then(|| News::Text(w.text.clone()))
            }),
            EventBody::Usage { report } => self
                .jobs
                .tell(attempt, |_| Some(News::Usage(report.clone()))),
            EventBody::ToolChanged { tool } => {
                self.jobs.tell(attempt, |_| Some(News::Tool(tool.clone())))
            }
            EventBody::AttemptEnded { completion } => self.job_ended_in(tx, attempt, completion)?,
            _ => {}
        }
        Ok(())
    }

    /// Job `attempt` is over: its row, its job and its `start_job` take the end (the first end
    /// stays: a later one changes nothing), and its caller hears it.
    fn job_ended_in(&self, tx: &Tx, attempt: &str, c: &Completion) -> Result<(), StoreError> {
        let Some(row) = jobs::attempt_in(tx, attempt)? else {
            return Ok(());
        };
        if row.state == AttemptState::Finished {
            return Ok(());
        }
        let completion = serde_json::to_value(c).map_err(conflict)?;
        jobs::apply_attempt_in(
            tx,
            attempt,
            AttemptState::Finished,
            None,
            Some(&completion),
            now(),
        )?;
        jobs::finish_job_in(tx, &row.job, now())?;
        let terminal: bool = tx.query_row(
            "SELECT state IN ('finished','rejected') FROM node_commands WHERE operation_id=?",
            [&row.operation],
            |r| r.get(0),
        )?;
        if !terminal {
            let receipt = Receipt {
                operation: idv(row.operation.clone())?,
                state: Observed::Finished {
                    outcome: c.outcome,
                    result: Some(OperationResult::Execution(c.clone())),
                    error: c.error.clone(),
                },
            };
            let (state, success) = receipt_state(&receipt.state);
            rt::merge_receipt_in(
                tx,
                &row.operation,
                state,
                success,
                &serde_json::to_value(&receipt).map_err(conflict)?,
                now(),
            )?;
        }
        self.jobs.tell(attempt, |_| Some(News::End(c.clone())));
        Ok(())
    }

    /// What the Node says about a command of job `attempt` (merged already): a start that runs, a
    /// start whose end the Node cannot tell (given up, never sent again), an end, an abandon the
    /// Node has no record of.
    pub(super) fn job_receipt_in(
        &self,
        tx: &Tx,
        op: &str,
        method: &str,
        attempt: &str,
        receipt: &Receipt,
    ) -> Result<(), StoreError> {
        let (state, _) = receipt_state(&receipt.state);
        let value = serde_json::to_value(receipt).map_err(conflict)?;
        match (method, state) {
            ("start_job", ReceiptState::Accepted | ReceiptState::Active) => {
                let s = if state == ReceiptState::Accepted {
                    AttemptState::Accepted
                } else {
                    AttemptState::Running
                };
                jobs::apply_attempt_in(tx, attempt, s, Some(&value), None, now())?;
                // a cancel held back while the start's send was uncertain goes now
                if jobs::attempt_in(tx, attempt)?
                    .is_some_and(|a| a.cancel_requested && a.state != AttemptState::Unknown)
                {
                    self.job_stop_in(tx, attempt, op, "cancel")?;
                }
            }
            ("start_job", ReceiptState::Unknown) => {
                jobs::apply_attempt_in(
                    tx,
                    attempt,
                    AttemptState::Unknown,
                    Some(&value),
                    None,
                    now(),
                )?;
                self.job_stop_in(tx, attempt, op, "abandon")?;
            }
            ("start_job", ReceiptState::Finished) => {
                let c = match &receipt.state {
                    Observed::Finished {
                        result: Some(OperationResult::Execution(c)),
                        ..
                    } => c.clone(),
                    Observed::Finished { outcome, error, .. } => {
                        ended(*outcome, "error", error.clone())
                    }
                    _ => return Ok(()),
                };
                self.job_ended_in(tx, attempt, &c)?;
            }
            ("abandon", ReceiptState::Finished) => {
                if let Observed::Finished { error: Some(e), .. } = &receipt.state {
                    self.job_abandon_failed_in(tx, attempt, e)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// The Node refused a command of job `attempt` before accepting it: a start ends failed (it
    /// did not run), an abandon of what the Node has no record of ends the attempt.
    pub(super) fn job_refused_in(
        &self,
        tx: &Tx,
        method: &str,
        attempt: &str,
        e: &ApiError,
    ) -> Result<(), StoreError> {
        match method {
            "start_job" => {
                let cancelled = jobs::attempt_in(tx, attempt)?.is_some_and(|a| a.cancel_requested);
                let c = if cancelled {
                    ended(Outcome::Cancelled, "cancelled", None)
                } else {
                    ended(Outcome::Failed, "refused", Some(e.clone()))
                };
                self.job_ended_in(tx, attempt, &c)
            }
            "abandon" => self.job_abandon_failed_in(tx, attempt, e),
            _ => Ok(()),
        }
    }

    /// An abandon that did not go through. The Node has no record of the attempt: it did not run
    /// there, and it ends interrupted. Otherwise it stays as it is (a person's Stop is not needed:
    /// its caller is gone either way).
    fn job_abandon_failed_in(
        &self,
        tx: &Tx,
        attempt: &str,
        e: &ApiError,
    ) -> Result<(), StoreError> {
        let unknown_to_node =
            e.code == ErrorCode::Invalid && e.safe_detail.contains("no such operation");
        if unknown_to_node
            && jobs::attempt_in(tx, attempt)?.is_some_and(|a| a.state == AttemptState::Unknown)
        {
            let c = ended(
                Outcome::Interrupted,
                "error",
                Some(ApiError::new(
                    ErrorCode::Invalid,
                    "the Node holds no record of this attempt: it did not run there",
                )),
            );
            return self.job_ended_in(tx, attempt, &c);
        }
        tracing::warn!("abandon of job attempt {attempt}: {}", e.safe_detail);
        Ok(())
    }

    /// Queues a `cancel` or `abandon` of job `attempt` (whose `start_job` is `target`), one at a
    /// time.
    fn job_stop_in(
        &self,
        tx: &Tx,
        attempt: &str,
        target: &str,
        method: &str,
    ) -> Result<(), StoreError> {
        let (n, live): (i64, bool) = tx.query_row(
            "SELECT COUNT(*), COALESCE(MAX(state NOT IN ('finished','rejected') OR json_extract(receipt,'$.success')=1), 0) FROM node_commands WHERE method=? AND target_job_attempt=?",
            [method, attempt],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if live {
            return Ok(());
        }
        let op = if n == 0 {
            format!("op_{method}_{attempt}")
        } else {
            format!("op_{method}_{attempt}_{n}")
        };
        let target = idv(target.to_string())?;
        let (req, digest) = if method == "abandon" {
            command(
                &op,
                Abandon {
                    target,
                    reason: "the Hub gives up a job attempt nobody can tell the end of".into(),
                },
            )?
        } else {
            command(
                &op,
                Cancel {
                    target,
                    reason: "the Hub cancels the job".into(),
                },
            )?
        };
        jobs::insert_job_command_in(
            tx,
            &jobs::NewJobCommand {
                operation: &op,
                attempt,
                method,
                request: &req,
                digest: &digest,
            },
            now(),
        )?;
        self.wake_send.notify_one();
        Ok(())
    }

    /// The Hub stops job `attempt` (its caller gave it up, or is gone): the intent is kept first.
    /// A start that never left the Hub ends here, cancelled; one that may be on its way waits for
    /// the Node to hold it ([`Self::job_receipt_in`]); one the Node cannot tell the end of is
    /// abandoned; a running one is cancelled.
    pub(super) fn stop_job_in(&self, tx: &Tx, attempt: &str) -> Result<(), StoreError> {
        let Some(row) = jobs::attempt_in(tx, attempt)? else {
            return Ok(());
        };
        if row.state == AttemptState::Finished {
            return Ok(());
        }
        jobs::cancel_attempt_in(tx, attempt, now())?;
        let (op, state, maybe_sent) = start_of(tx, attempt)?;
        match state.as_str() {
            "pending" if !maybe_sent => self.job_unsent_in(tx, attempt, &op),
            "pending" => Ok(()),
            "unknown" => self.job_stop_in(tx, attempt, &op, "abandon"),
            _ if row.state == AttemptState::Unknown => {
                self.job_stop_in(tx, attempt, &op, "abandon")
            }
            _ => self.job_stop_in(tx, attempt, &op, "cancel"),
        }
    }

    /// A start that is ended before the Node ran it: rejected here, the attempt cancelled.
    fn job_unsent_in(&self, tx: &Tx, attempt: &str, op: &str) -> Result<(), StoreError> {
        rt::merge_receipt_in(
            tx,
            op,
            ReceiptState::Rejected,
            false,
            &json!({ "reason": "cancelled before it was sent" }),
            now(),
        )?;
        self.job_ended_in(tx, attempt, &ended(Outcome::Cancelled, "cancelled", None))
    }

    /// [`Self::stop_job_in`] in its own transaction, then the send worker is woken.
    pub(super) fn stop_job(&self, attempt: &str) {
        if let Err(e) = self.tx(|tx| self.stop_job_in(tx, attempt)) {
            tracing::error!("cancel job attempt {attempt}: {e}");
        }
        self.wake_send.notify_one();
    }

    /// After the Hub's restart and its reconcile with the Node: the open job attempts of an earlier
    /// process have no caller any more. They are stopped, never run again: a start the Node holds
    /// no record of is ended here (it never ran), a running one cancelled, an unknown one
    /// abandoned. Jobs left open with nothing open in them are closed.
    pub(super) fn settle_orphan_jobs(&self) -> usize {
        let r = self.tx(|tx| {
            let open = jobs::open_attempts_in(tx, self.node_id.as_str())?;
            let mut n = 0;
            for a in open {
                if self.jobs.has(&a.id) {
                    continue;
                }
                n += 1;
                jobs::cancel_attempt_in(tx, &a.id, now())?;
                let (op, state, _) = start_of(tx, &a.id)?;
                match state.as_str() {
                    "pending" => self.job_unsent_in(tx, &a.id, &op)?,
                    "finished" | "rejected" => {
                        // its end came on the receipt but not on the stream
                        let receipt: Option<String> = tx.query_row(
                            "SELECT json_extract(receipt,'$.payload') FROM node_commands WHERE operation_id=?",
                            [&op],
                            |r| r.get(0),
                        )?;
                        let end = receipt
                            .and_then(|r| serde_json::from_str::<Receipt>(&r).ok())
                            .and_then(|r| match r.state {
                                Observed::Finished {
                                    result: Some(OperationResult::Execution(c)),
                                    ..
                                } => Some(c),
                                _ => None,
                            })
                            .unwrap_or_else(|| ended(Outcome::Interrupted, "error", None));
                        self.job_ended_in(tx, &a.id, &end)?;
                    }
                    "unknown" => self.job_stop_in(tx, &a.id, &op, "abandon")?,
                    _ if a.state == AttemptState::Unknown => {
                        self.job_stop_in(tx, &a.id, &op, "abandon")?
                    }
                    _ => self.job_stop_in(tx, &a.id, &op, "cancel")?,
                }
            }
            let empty: Vec<String> = {
                let mut st = tx.prepare(
                    "SELECT j.job_id FROM runtime_jobs j WHERE j.node_id=? AND j.state='open' AND NOT EXISTS(SELECT 1 FROM runtime_job_attempts a WHERE a.job_id=j.job_id AND a.state<>'finished')",
                )?;
                st.query_map([self.node_id.as_str()], |r| r.get(0))?
                    .collect::<Result<_, _>>()?
            };
            for j in empty {
                jobs::finish_job_in(tx, &j, now())?;
            }
            Ok(n)
        });
        match r {
            Ok(n) => {
                if n > 0 {
                    tracing::info!(
                        "node {}: {n} job attempts of an earlier run stopped",
                        self.node_id
                    );
                    self.wake_send.notify_one();
                }
                n
            }
            Err(e) => {
                tracing::error!("node {}: stop earlier job attempts: {e}", self.node_id);
                0
            }
        }
    }

    /// Finished jobs older than `before` go, with their attempts and commands.
    pub(super) fn prune_jobs(&self, before: i64) {
        loop {
            match self.tx(|tx| jobs::prune_jobs_in(tx, before, PRUNE_BATCH)) {
                Ok(n) if n as i64 == PRUNE_BATCH => continue,
                Ok(_) => return,
                Err(e) => {
                    tracing::error!("prune node jobs: {e}");
                    return;
                }
            }
        }
    }
}

/// Core's [`OneModel`] on the local Node: one think job per try of one model. Core's fallback
/// around it keeps choosing, counting and booking exactly as it does in process.
pub struct NodePrint {
    pub runtime: Arc<NodeRuntime>,
    /// Where job-input images are kept (with the attachments, under their retention).
    pub data_dir: PathBuf,
}

/// Stops its attempt unless disarmed, and forgets its waiter either way.
pub(super) struct Waiting {
    pub(super) runtime: Arc<NodeRuntime>,
    pub(super) attempt: String,
    pub(super) armed: bool,
}

impl Drop for Waiting {
    fn drop(&mut self) {
        self.runtime.jobs.remove(&self.attempt);
        if !self.armed {
            return;
        }
        let (rt, attempt) = (self.runtime.clone(), std::mem::take(&mut self.attempt));
        match tokio::runtime::Handle::try_current() {
            Ok(h) => {
                h.spawn_blocking(move || rt.stop_job(&attempt));
            }
            Err(_) => rt.stop_job(&attempt),
        }
    }
}

/// A print error as a Node's completion tells it (`stop_reason` and the harness's words), so the
/// caller sees the very error the in-process run gives.
fn print_error(c: &Completion, timeout: Duration) -> steve_print::PrintError {
    use steve_print::PrintError as P;
    if c.error
        .as_ref()
        .is_some_and(|e| e.code == ErrorCode::Exhausted)
        && c.error
            .as_ref()
            .is_some_and(|e| e.safe_detail == "the output limit was reached")
    {
        return P::OutputLimit;
    }
    let detail = c
        .error
        .as_ref()
        .map(|e| e.safe_detail.clone())
        .unwrap_or_default();
    match (c.outcome, c.stop_reason.as_str()) {
        (Outcome::TimedOut, _) | (_, "timeout") => P::Timeout(timeout),
        (_, "failed") => P::Failed(detail),
        (_, "harness") => P::Harness {
            text: detail,
            transient: c.transient,
        },
        (_, "codex_failed") => P::CodexFailed,
        (_, "no_result") => P::NoResult,
        (_, "no_output") => P::NoOutput,
        (_, "io") => P::Io(std::io::Error::other(detail)),
        (_, why) => P::Failed(if detail.is_empty() {
            why.to_string()
        } else {
            detail
        }),
    }
}

#[async_trait]
impl OneModel for NodePrint {
    async fn once(
        &self,
        call: &OneCall<'_>,
        model: &str,
        effort: Option<&str>,
        on_text: Option<OnText<'_>>,
    ) -> Tried {
        // the gateway's errors are stored with the answer: zh-CN, as core's own
        let fail = |e: String| Tried::failed(steve_core::CoreError::Msg(e));
        let tag = steve_core::stats::current_tag();
        let (head, message) = split_static(call.prompt);
        let attempt = uid("ja");
        let mut news = self.runtime.jobs.add(&attempt);
        let mut waiting = Waiting {
            runtime: self.runtime.clone(),
            attempt: attempt.clone(),
            armed: true,
        };
        let t = Think {
            attempt,
            purpose: tag.purpose,
            workspace: tag.workspace_id,
            head: head.map(String::from),
            message: message.to_string(),
            tools: call.tools.clone(),
            images: call.images.to_vec(),
            codex: call.codex,
            model: model.to_string(),
            effort: effort.map(String::from),
            timeout: call.timeout,
        };
        // a few SQLite writes, and image files when there are any
        let (rt, dir) = (self.runtime.clone(), self.data_dir.clone());
        match tokio::task::spawn_blocking(move || rt.start_think(&dir, &t)).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => {
                waiting.armed = false;
                return fail(format!("Node 没能接下这次判断：{e}"));
            }
            Err(e) => return fail(format!("Node 没能接下这次判断：{e}")),
        }
        let mut usage: Vec<UsageReport> = vec![];
        let mut shown: Option<String> = None;
        // the Hub's own timer, as the in-process run's: the Node's deadline is the same moment
        let timer = tokio::time::sleep(call.timeout);
        tokio::pin!(timer);
        let end = loop {
            tokio::select! {
                n = news.recv() => match n {
                    Some(News::Text(t)) => {
                        if shown.as_deref() != Some(t.as_str()) {
                            if let Some(f) = on_text {
                                f(&t);
                            }
                            shown = Some(t);
                        }
                    }
                    Some(News::Usage(r)) => {
                        usage.push(r);
                    }
                    Some(News::Tool(_)) => {}
                    Some(News::End(c)) => break c,
                    None => return fail("Node 没有回报这次判断的结果".into()),
                },
                _ = &mut timer => {
                    // dropping `waiting` cancels the job on the Node
                    return Tried::of(
                        steve_print::Ran {
                            reply: Err(steve_print::PrintError::Timeout(call.timeout)),
                            usage: normalized_usage(usage, model),
                        },
                        call.codex,
                    );
                }
            }
        };
        waiting.armed = false;
        drop(waiting);
        let usage = normalized_usage(usage, model);
        let reply = if end.outcome == Outcome::Succeeded {
            let text = end
                .result
                .iter()
                .filter_map(|p| match p {
                    Part::Text { text } => Some(text.as_str()),
                    _ => None,
                })
                .collect::<Vec<_>>()
                .join("");
            // the run's dollars: each model's, as the harness's total was booked to them
            let costs: Vec<f64> = usage
                .iter()
                .filter_map(|r| r.cost.as_ref().and_then(steve_print::cost_f64))
                .collect();
            let cost = (!costs.is_empty()).then(|| costs.iter().sum::<f64>());
            Ok(steve_print::Reply {
                text,
                cost: cost.map(steve_print::money),
            })
        } else {
            Err(print_error(&end, call.timeout))
        };
        Tried::of(steve_print::Ran { reply, usage }, call.codex)
    }
}

#[cfg(test)]
mod usage_tests {
    use super::*;

    fn report(
        id: &str,
        measurement: Measurement,
        output: Option<u64>,
        cost: Option<&str>,
    ) -> UsageReport {
        UsageReport {
            report_id: id.into(),
            model: None,
            meter_epoch: Epoch::new("meter").unwrap(),
            scope: UsageScope::Main,
            measurement,
            tokens: Tokens {
                output,
                ..Default::default()
            },
            context_used: None,
            context_limit: None,
            cost: cost.map(|decimal| Money {
                decimal: decimal.into(),
                currency: "USD".into(),
            }),
        }
    }

    #[test]
    fn cumulative_cost_is_replaced_and_final_tokens_keep_that_cost() {
        let end = report("end", Measurement::Final, Some(20), None);
        let rows = normalized_usage(
            [
                report("one", Measurement::Cumulative, None, Some("0.1")),
                report("two", Measurement::Cumulative, None, Some("0.2")),
                end.clone(),
                end,
                report("stale", Measurement::Cumulative, Some(2), Some("0.1")),
            ],
            "model",
        );
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].model.as_deref(), Some("model"));
        assert_eq!(rows[0].tokens.output, Some(20));
        assert_eq!(rows[0].cost.as_ref().unwrap().decimal, "0.2");
    }

    #[test]
    fn deltas_add_but_final_usage_replaces_them_and_other_epochs_remain() {
        let mut restart = report("restart", Measurement::Final, Some(3), Some("0.03"));
        restart.meter_epoch = Epoch::new("next").unwrap();
        let rows = normalized_usage(
            [
                report("d1", Measurement::Delta, Some(4), Some("0.1")),
                report("d2", Measurement::Delta, Some(6), Some("0.2")),
                report("f", Measurement::Final, Some(12), Some("0.25")),
                restart,
            ],
            "model",
        );
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].tokens.output, Some(12));
        assert_eq!(rows[0].cost.as_ref().unwrap().decimal, "0.25");
        assert_eq!(rows[1].tokens.output, Some(3));
        let deltas = normalized_usage(
            [
                report("d1", Measurement::Delta, Some(4), None),
                report("d2", Measurement::Delta, Some(6), None),
            ],
            "model",
        );
        assert_eq!(deltas[0].tokens.output, Some(10));
    }

    #[test]
    fn all_scope_does_not_double_the_main_loop() {
        let mut all = report("all", Measurement::Final, Some(25), None);
        all.scope = UsageScope::All;
        let rows = normalized_usage(
            [
                report("main", Measurement::Final, Some(20), Some("0.2")),
                all,
            ],
            "model",
        );
        assert_eq!(rows.iter().filter_map(|r| r.tokens.output).sum::<u64>(), 25);
        assert_eq!(rows.iter().filter_map(|r| r.cost.as_ref()).count(), 1);
    }
}
