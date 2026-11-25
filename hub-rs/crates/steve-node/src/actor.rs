//! One task per session: everything that touches its harness process goes through it, in order.
//!
//! The journal is the truth of what was accepted and decided; the actor turns it into process work
//! and records what the process did. The actor itself never waits on slow work: starting a harness
//! (spawn and handshake), fetching attachments and checking an answer's preconditions run as tasks
//! that report back, so a cancel, a deadline or a question the adapter asks while it starts are
//! handled at once.
//!
//! Every attempt has a token. The adapter's messages are stamped with the token of the attempt
//! running when they were read ([`Gate`]); a stop the Node schedules names its token too. Nothing is
//! taken for an attempt that is not the one it was meant for. (ACP has no attempt id: a message the
//! adapter still sends for an earlier prompt after the next prompt was written cannot be told apart
//! and counts for the new one. Adapters answer a prompt after its last update, so this is the
//! exception it is documented as.)
//!
//! Memory only moves after the journal did: an attempt's end is committed first, then forgotten
//! here. A journal that fails puts the Node in a faulted state ([`crate::Inner::fault`]): it
//! accepts no new work, stops what runs, and retries what it must record.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use steve_acp::StateDirRuntime;
use steve_acp::normalize::{Fact, Normalizer};
use steve_acp::permission::{self, Asked};
use steve_acp::rpc::{Conn, ExitReport, Incoming, RpcError, Spawn};
use steve_acp::session::{self as acp_session, OpenConfig, Reports};
use steve_acp::usage::{POLL_INTERVAL, SETTLE_WAIT, Sampler};
use steve_node_api::*;
use tokio::sync::{mpsc, oneshot, watch};

use crate::journal::{Tx, internal};
use crate::procs::{self, Stopped};
use crate::{Inner, now};

pub(crate) enum Msg {
    /// Start the harness as configured now and confirm it; finish `op` (open, configure).
    Open {
        op: OperationId,
    },
    /// Run one attempt (accepted and journaled already).
    Start {
        op: OperationId,
        spec: Box<TurnSpec>,
        exec: ExecutionRef,
    },
    Cancel {
        target: OperationId,
    },
    /// Cancel a single active subtask (e.g. ACP RFD child session) without cancelling the parent turn.
    CancelSubtask {
        subtask: CallId,
        done: oneshot::Sender<ApiResult<bool>>,
    },
    /// Give a journaled answer to the harness (`op` is the resolve command, finished here).
    Answer {
        op: OperationId,
    },
    /// Stop the harness and say whether it is gone; finish `op` (release) if given. `abandon`: the
    /// stop is for an unknown execution (only a group left from before is stopped then).
    Stop {
        op: Option<OperationId>,
        abandon: bool,
        done: Option<oneshot::Sender<Stopped>>,
    },
    Shutdown {
        done: oneshot::Sender<()>,
    },
    // results of the actor's own tasks
    Opened {
        token: u64,
        result: ApiResult<LiveNew>,
    },
    Prepared {
        token: u64,
        result: ApiResult<Value>,
    },
    Bound {
        generation: u64,
        token: u64,
        rpc_id: Value,
        asked: Box<Asked>,
        result: ApiResult<PermissionRequest>,
    },
    Checked {
        op: OperationId,
        result: ApiResult<()>,
    },
    AnswerSent {
        op: OperationId,
        req: Box<PermissionRequest>,
        res: PermissionResolution,
        result: ApiResult<()>,
    },
    Prompted {
        token: u64,
        result: Result<Value, RpcError>,
    },
    Usage {
        token: u64,
        facts: Vec<Fact>,
    },
    UsageSettled {
        token: u64,
        facts: Vec<Fact>,
    },
    /// Still running after its grace: its group is stopped.
    Kill {
        token: u64,
    },
    ReleaseDone {
        op: OperationId,
        stopped: Stopped,
    },
    AnswerDone {
        op: OperationId,
        result: ApiResult<()>,
    },
    Facts {
        events: Vec<(Subject, EventBody)>,
    },
    /// Try again what the journal did not take.
    Retry,
    /// The attempt's harness went past what its ingress takes ([`Ingress`]).
    Overflow {
        token: u64,
        why: Ingress,
    },
    // from the adapter
    Update {
        generation: u64,
        token: u64,
        session_id: Option<String>,
        update: Value,
    },
    Permission {
        generation: u64,
        token: u64,
        rpc_id: Value,
        params: Value,
    },
    Exited {
        generation: u64,
        report: ExitReport,
    },
}

/// Why an attempt's harness is stopped at its ingress. Being behind is not one of them: the reader
/// waits for room on the data queue ([`enqueue`]), so the harness blocks on its stdout instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ingress {
    /// One message over the 16 MiB frame cap.
    Frame,
    /// More bytes than the attempt's output budget.
    Output,
    /// The Node took none of its messages for [`crate::Storage::ingress_stall`].
    Stalled,
}

impl Ingress {
    fn stop(self, stall: Duration) -> (Outcome, Error) {
        match self {
            Ingress::Frame => (Outcome::Cancelled, NodeStop::FrameTooLarge.error()),
            Ingress::Output => (Outcome::Cancelled, NodeStop::OutputLimit.error()),
            Ingress::Stalled => (
                Outcome::Interrupted,
                NodeStop::IngressStalled {
                    secs: stall.as_secs(),
                }
                .error(),
            ),
        }
    }
}

/// The token of the attempt whose prompt is out, stamped on each adapter message as it is read;
/// the attempt's output budget; whether its ingress was stopped.
#[derive(Default)]
pub(crate) struct Gate(AtomicU64, AtomicU64, AtomicBool);
impl Gate {
    /// Stops the attempt `token` at its ingress (once; never one that is over already).
    fn overflow(&self, tx: &mpsc::UnboundedSender<Msg>, token: u64, why: Ingress) {
        if token != 0
            && self.0.load(Ordering::SeqCst) == token
            && !self.2.swap(true, Ordering::SeqCst)
        {
            let _ = tx.send(Msg::Overflow { token, why });
        }
    }
}

/// Puts a message the reader read on the actor's data queue, in the order read. With room it goes
/// at once. Without, the reader waits (backpressure: it reads no more, so the harness blocks on
/// its stdout and what is buffered stays within the queue's slots of at most a frame each) for as
/// long as the actor keeps taking messages: a slow journal, sink or Hub never ends an attempt.
/// Only no slot freed in `stall` (the actor takes nothing: a stuck disk) stops the attempt the
/// message is for. The wait ends too when the actor is gone or the harness is retired.
fn enqueue(
    data_tx: &mpsc::Sender<Msg>,
    message: Msg,
    token: u64,
    ctl: &Ctl,
) -> Option<steve_acp::rpc::Backpressure> {
    let message = match data_tx.try_send(message) {
        Ok(()) | Err(mpsc::error::TrySendError::Closed(_)) => return None,
        Err(mpsc::error::TrySendError::Full(m)) => m,
    };
    let data_tx = data_tx.clone();
    let Ctl {
        gate,
        tx,
        mut retired,
        stall,
    } = ctl.clone();
    Some(Box::pin(async move {
        let room = async {
            if token == 0 {
                // nobody's attempt: nothing to stop (its bytes are capped by admission)
                return data_tx.reserve().await.ok();
            }
            // measured, not only timed: a wait the runtime polls late still counts in full
            let since = tokio::time::Instant::now();
            match tokio::time::timeout(stall, data_tx.reserve()).await {
                Ok(Ok(permit)) if since.elapsed() < stall => Some(permit),
                Ok(Err(_)) => None,
                _ => {
                    gate.overflow(&tx, token, Ingress::Stalled);
                    None
                }
            }
        };
        tokio::select! {
            permit = room => {
                if let Some(p) = permit {
                    p.send(message);
                }
            }
            // stopped, released or shut down: its messages are nobody's now
            _ = retired.wait_for(|r| *r) => {}
        }
    }))
}

/// What a harness's reader needs to wait for the actor ([`enqueue`]).
#[derive(Clone)]
struct Ctl {
    gate: Arc<Gate>,
    tx: mpsc::UnboundedSender<Msg>,
    /// True (or its sender gone) once the harness is no longer the session's ([`Live`] dropped).
    retired: watch::Receiver<bool>,
    stall: Duration,
}

/// ACP session ids belonging to one live adapter process: the root session plus any child
/// subagent sessions announced via `subagent_update`. Updated synchronously in the reader thread
/// so follow-up frames on a newly-announced child session are never dropped.
#[derive(Default)]
struct SessionRoutes {
    root: Option<String>,
    children: HashSet<String>,
}

impl SessionRoutes {
    fn accepts(&self, sid: Option<&str>) -> bool {
        let Some(root) = self.root.as_deref() else {
            return true;
        };
        match sid {
            Some(s) if s == root => true,
            Some(s) => self.children.contains(s),
            None => false,
        }
    }

    fn is_root(&self, sid: Option<&str>) -> bool {
        match (self.root.as_deref(), sid) {
            (Some(root), Some(s)) => root == s,
            (None, _) => true,
            (Some(_), None) => false,
        }
    }

    fn register_if_subagent(&mut self, update: &Value) {
        if update.get("sessionUpdate").and_then(Value::as_str) == Some("subagent_update")
            && let Some(child_sid) = update
                .get("sessionId")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
        {
            self.children.insert(child_sid.to_string());
        }
    }
}

#[derive(Clone)]
pub(crate) struct LiveNew {
    conn: Arc<Conn>,
    retire: Arc<watch::Sender<bool>>,
    generation: u64,
    acp_id: String,
    routes: Arc<std::sync::Mutex<SessionRoutes>>,
    usage_home: Option<steve_acp::usage::Source>,
    image: bool,
    pgid: i64,
    started: Option<u64>,
    view: SessionView,
    cumulative_prompt_tokens: Option<(u64, u64, u64, u64, u64)>,
    /// Personal user context that could not be delivered via the adapter's native channel on a
    /// fresh session (e.g. legacy adapter binary); prepended once on the first prompt as
    /// `## User Context`.
    pending_system_context: Option<String>,
}

struct Live {
    conn: Arc<Conn>,
    /// Ends a wait of its reader on a full data queue once this harness is not the session's.
    retire: Arc<watch::Sender<bool>>,
    generation: u64,
    acp_id: String,
    _routes: Arc<std::sync::Mutex<SessionRoutes>>,
    usage_home: Option<steve_acp::usage::Source>,
    image: bool,
    pgid: i64,
    started: Option<u64>,
    cumulative_prompt_tokens: Option<(u64, u64, u64, u64, u64)>,
    cumulative_context_cost: f64,
    pending_system_context: Option<String>,
}

impl Drop for Live {
    fn drop(&mut self) {
        self.retire.send_replace(true);
        if !self.conn.has_exited() {
            self.conn.signal(libc::SIGKILL);
        }
    }
}

impl From<LiveNew> for Live {
    fn from(l: LiveNew) -> Live {
        Live {
            conn: l.conn,
            retire: l.retire,
            generation: l.generation,
            acp_id: l.acp_id,
            _routes: l.routes,
            usage_home: l.usage_home,
            image: l.image,
            pgid: l.pgid,
            started: l.started,
            cumulative_prompt_tokens: l.cumulative_prompt_tokens,
            cumulative_context_cost: 0.0,
            pending_system_context: l.pending_system_context,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Phase {
    /// The harness starts for it.
    Starting,
    /// Its attachments are fetched.
    Preparing,
    Prompting,
}

/// Dropping an actor/run also stops its sampler; sender loss never starts a tail read.
struct UsageTask(tokio::task::JoinHandle<()>);
impl Drop for UsageTask {
    fn drop(&mut self) {
        self.0.abort();
    }
}

struct Run {
    token: u64,
    budget_epoch: u64,
    exec: ExecutionRef,
    op: OperationId,
    spec: Box<TurnSpec>,
    norm: Normalizer,
    phase: Phase,
    acted: bool,
    cancel: bool,
    /// Why the Node itself stopped it.
    stopped: Option<(Outcome, Error)>,
    /// The end, computed and waiting to be committed (kept while the journal fails).
    ending: Option<Completion>,
    closing: Option<Vec<EventBody>>,
    final_usage: Vec<(Subject, EventBody)>,
    usage_stop: Option<oneshot::Sender<Vec<Fact>>>,
    usage_task: Option<UsageTask>,
    usage_pending: bool,
    unknown: Option<String>,
    images: crate::inline_images::Images,
    image_scope: crate::inline_images::Scope,
}

/// What a harness start is for.
enum Opening {
    /// The open/configure command.
    Command(OperationId),
    /// The attempt with this token.
    Attempt(u64),
}

pub(crate) struct Actor {
    inner: Arc<Inner>,
    session: SessionId,
    rx: mpsc::UnboundedReceiver<Msg>,
    data_rx: mpsc::Receiver<Msg>,
    data_tx: mpsc::Sender<Msg>,
    tx: mpsc::UnboundedSender<Msg>,
    gate: Arc<Gate>,
    live: Option<Live>,
    run: Option<Run>,
    opening: Option<(u64, Opening)>,
    waits: HashMap<PermissionId, (Value, Asked)>,
    tokens: u64,
    retry_armed: bool,
    /// RPC continuations and retry/kill timers belong to this actor, including at shutdown.
    children: tokio::task::JoinSet<()>,
    pending: Vec<Msg>,
    sending: usize,
    deferred_prompt: Option<Result<Value, RpcError>>,
    stage_cancel: Option<watch::Sender<bool>>,
    stage_task: Option<tokio::task::JoinHandle<()>>,
}

impl Actor {
    pub(crate) fn spawn(inner: Arc<Inner>, session: SessionId) -> mpsc::UnboundedSender<Msg> {
        let (tx, rx) = mpsc::unbounded_channel();
        let (data_tx, data_rx) = mpsc::channel(64);
        let a = Actor {
            inner,
            session,
            rx,
            data_rx,
            data_tx,
            tx: tx.clone(),
            gate: Arc::default(),
            live: None,
            run: None,
            opening: None,
            waits: HashMap::new(),
            tokens: 0,
            retry_armed: false,
            children: tokio::task::JoinSet::new(),
            pending: vec![],
            sending: 0,
            deferred_prompt: None,
            stage_cancel: None,
            stage_task: None,
        };
        tokio::spawn(a.run_loop());
        tx
    }

    async fn run_loop(mut self) {
        let mut tick = tokio::time::interval(Duration::from_millis(500));
        loop {
            tokio::select! {
                biased;
                m = self.rx.recv() => {
                    let Some(m) = m else {
                        let _ = self.inner.j.flush();
                        return;
                    };
                    if let Msg::Shutdown { done } = m {
                        let _ = self.inner.j.flush();
                        if let Some(cancel) = self.stage_cancel.take() { let _ = cancel.send(true); }
                        if let Some(task) = self.stage_task.take() { let _ = task.await; }
                        self.stop_process().await;
                        if let Some(run) = self.run.as_mut() && let Some(mut task) = run.usage_task.take() {
                            task.0.abort();
                            let _ = (&mut task.0).await;
                        }
                        // A completed process can still leave an RPC waiter or retry timer.
                        // Abort and join these owned continuations before acknowledging shutdown.
                        self.children.shutdown().await;
                        drop(self);
                        let _ = done.send(());
                        return;
                    }
                    self.handle(m).await;
                }
                _ = tick.tick() => self.check_limits(),
                Some(message) = self.data_rx.recv() => self.handle(message).await,
            }
            while self.children.try_join_next().is_some() {}
        }
    }

    /// The connection owner flushes pending stream data in this same state transaction.
    fn tx<R: Send>(&self, f: impl FnOnce(&Tx) -> ApiResult<R> + Send) -> ApiResult<R> {
        let r = self.inner.j.tx(f);
        match &r {
            Ok(_) => self.inner.wake.notify_one(),
            Err(e) => {
                tracing::error!("node journal ({}): {e}", self.session);
                if e.code == Code::Internal {
                    self.inner.fault();
                }
            }
        }
        r
    }

    fn next_token(&mut self) -> u64 {
        self.tokens += 1;
        self.tokens
    }

    fn later(&mut self, message: Msg) {
        self.pending.push(message);
        self.arm_retry();
    }

    fn arm_retry(&mut self) {
        if self.retry_armed {
            return;
        }
        self.retry_armed = true;
        let tx = self.tx.clone();
        self.children.spawn(async move {
            tokio::time::sleep(Duration::from_secs(1)).await;
            let _ = tx.send(Msg::Retry);
        });
    }

    async fn handle(&mut self, m: Msg) {
        match m {
            Msg::Overflow { token, why } => {
                let (outcome, e) = why.stop(self.inner.cfg.storage.ingress_stall);
                self.stopping(token, outcome, e);
                let _ = self.tx.send(Msg::Kill { token });
            }
            Msg::ReleaseDone { op, stopped } => self.finish_release(&op, stopped),
            Msg::AnswerDone { op, result } => self.answer_done(&op, result),
            Msg::Facts { events } => {
                if self
                    .tx(|t| crate::emit_all(&self.inner, t, events.clone()))
                    .is_err()
                {
                    self.later(Msg::Facts { events });
                }
            }
            Msg::Open { op } => self.open(op).await,
            Msg::Start { op, spec, exec } => self.start(op, spec, exec).await,
            Msg::Cancel { target } => self.cancel(&target),
            Msg::CancelSubtask { subtask, done } => self.cancel_subtask(&subtask, done),
            Msg::Answer { op } => self.answer(op),
            Msg::Bound {
                generation,
                token,
                rpc_id,
                asked,
                result,
            } => {
                self.permission_bound(generation, token, rpc_id, *asked, result);
            }
            Msg::Checked { op, result } => self.answer_checked(op, result),
            Msg::AnswerSent {
                op,
                req,
                res,
                result,
            } => {
                let state = match &result {
                    Ok(()) => done_state(OperationResult::Unit),
                    Err(e) => Observed::Unknown {
                        reason: e.safe_detail.clone(),
                    },
                };
                let saved = self.tx(|t| {
                    t.drop_answer(&op)?;
                    t.set_state(&op, &state)?;
                    let mut evs = vec![op_changed(&op, state.clone())];
                    if state.is_finished() {
                        evs.push((
                            Subject::Execution(req.execution.clone()),
                            EventBody::PermissionResolved {
                                resolution: res.clone(),
                            },
                        ));
                    }
                    crate::emit_all(&self.inner, t, evs)
                });
                if saved.is_err() {
                    self.later(Msg::AnswerSent {
                        op,
                        req,
                        res,
                        result,
                    });
                    return;
                }
                self.sending = self.sending.saturating_sub(1);
                if self.sending == 0 {
                    if let Some(result) = self.deferred_prompt.take() {
                        self.finish(result).await;
                    } else if self.run.as_ref().is_some_and(|r| r.ending.is_some()) {
                        self.commit_end();
                    }
                }
            }
            Msg::Stop { op, abandon, done } => {
                let s = if abandon && self.live.is_some() {
                    // a harness of this incarnation runs: not what an abandon is about
                    Stopped::Uncertain
                } else {
                    self.stop_process().await
                };
                if let Some(op) = op {
                    self.finish_release(&op, s);
                }
                if let Some(d) = done {
                    let _ = d.send(s);
                }
            }
            Msg::Shutdown { .. } => {}
            Msg::Opened { token, result } => self.opened(token, result).await,
            Msg::Prepared { token, result } => self.prepared(token, result).await,
            Msg::Prompted { token, result } => {
                // Responses follow updates on ACP, but control has priority here. Drain only the
                // already bounded data prefix before sealing this attempt's final facts.
                while let Ok(message) = self.data_rx.try_recv() {
                    match message {
                        Msg::Update {
                            generation,
                            token,
                            session_id,
                            update,
                        } => {
                            self.update(generation, token, session_id.as_deref(), &update)
                                .await
                        }
                        Msg::Permission {
                            generation,
                            token,
                            rpc_id,
                            params,
                        } => self.permission(generation, token, rpc_id, &params),
                        _ => unreachable!("data messages"),
                    }
                }
                if self.run.as_ref().is_some_and(|r| r.token == token) {
                    if self.sending == 0 {
                        self.finish(result).await;
                    } else {
                        self.deferred_prompt = Some(result);
                    }
                }
            }
            Msg::Usage { token, facts } => {
                if let Some(run) = self.run.as_ref().filter(|r| r.token == token) {
                    let events: Vec<_> = facts
                        .into_iter()
                        .filter_map(|f| match f {
                            Fact::Event(e) => Some((Subject::Execution(run.exec.clone()), e)),
                            _ => None,
                        })
                        .collect();
                    if self.inner.j.events(events.clone(), None).is_err() {
                        self.later(Msg::Facts { events });
                    }
                }
            }
            Msg::UsageSettled { token, facts } => {
                if let Some(run) = self.run.as_mut().filter(|r| r.token == token) {
                    let mut diagnostics = vec![];
                    run.final_usage = facts
                        .into_iter()
                        .filter_map(|f| match f {
                            Fact::Event(e @ EventBody::Usage { .. }) => {
                                Some((Subject::Execution(run.exec.clone()), e))
                            }
                            Fact::Event(e) => {
                                diagnostics.push((Subject::Execution(run.exec.clone()), e));
                                None
                            }
                            _ => None,
                        })
                        .collect();
                    run.usage_pending = false;
                    // Diagnostics use the data budget. A full disk/journal must still admit the
                    // final Usage (unknown fields included) and the attempt's terminal state.
                    if !diagnostics.is_empty() {
                        let _ = self.tx(|t| crate::emit_all(&self.inner, t, diagnostics));
                    }
                    self.commit_end();
                }
            }
            Msg::Kill { token } => {
                if self
                    .run
                    .as_ref()
                    .is_some_and(|r| r.token == token && r.ending.is_none())
                {
                    let s = self.stop_process().await;
                    let (outcome, e) = self
                        .run
                        .as_ref()
                        .and_then(|r| r.stopped.clone())
                        .unwrap_or((Outcome::Interrupted, NodeStop::StoppedByNode.error()));
                    let acted = self.run.as_ref().is_some_and(|r| r.acted);
                    if s.gone() {
                        self.end(failure(outcome, e, acted));
                    } else {
                        self.unknown_end("the harness did not stop");
                    }
                }
            }
            Msg::Retry => {
                self.retry_armed = false;
                if !self.pending.is_empty() {
                    for message in std::mem::take(&mut self.pending) {
                        let _ = self.tx.send(message);
                    }
                    self.arm_retry();
                    return;
                }
                if let Some(why) = self.run.as_ref().and_then(|r| r.unknown.clone()) {
                    self.unknown_end(&why);
                    return;
                }
                if self.run.as_ref().is_some_and(|r| r.ending.is_some()) {
                    self.commit_end();
                }
            }
            Msg::Update {
                generation,
                token,
                session_id,
                update,
            } => {
                self.update(generation, token, session_id.as_deref(), &update)
                    .await
            }
            Msg::Permission {
                generation,
                token,
                rpc_id,
                params,
            } => self.permission(generation, token, rpc_id, &params),
            Msg::Exited { generation, report } => self.exited(generation, report).await,
        }
    }

    // ── the process ──

    /// Starts the harness in a task; the answer comes back as [`Msg::Opened`].
    fn spawn_open(&mut self, token: u64) {
        let inner = self.inner.clone();
        let session = self.session.clone();
        let tx = self.tx.clone();
        let gate = self.gate.clone();
        let data_tx = self.data_tx.clone();
        let (cancel, cancelled) = watch::channel(false);
        self.stage_cancel = Some(cancel);
        self.stage_task = Some(tokio::spawn(async move {
            let result = open_harness(&inner, &session, tx.clone(), data_tx, gate, cancelled).await;
            let _ = tx.send(Msg::Opened { token, result });
        }));
    }

    /// Stops the session's harness (the live one, or a group recorded from before) and says what
    /// became of it; its record goes only when it is gone.
    async fn stop_process(&mut self) -> Stopped {
        self.gate.0.store(0, Ordering::SeqCst);
        crate::revoke_tools(&self.inner, &self.session);
        let live = self.live.take();
        let conn = live.as_ref().map(|l| l.conn.clone());
        let (pgid, started) = match live {
            Some(l) => (l.pgid, l.started),
            None => match self.inner.j.read(|t| t.proc_of(&self.session)) {
                Ok(Some(p)) => (p.pgid, p.started),
                Ok(None) => {
                    return match self
                        .inner
                        .j
                        .read(|t| t.reservation(&format!("spawn:{}", self.session)))
                    {
                        Ok(None) => Stopped::Gone,
                        _ => Stopped::Uncertain,
                    };
                }
                Err(_) => return Stopped::Uncertain,
            },
        };
        let s = procs::stop(pgid, started).await;
        if s.gone()
            && let Some(conn) = conn
            && tokio::time::timeout(Duration::from_secs(1), conn.wait_exit())
                .await
                .is_err()
        {
            // The OS says the group is gone, but its transport has not confirmed cleanup.
            // Keep the process record for explicit reconciliation rather than claim success.
            return Stopped::Uncertain;
        }
        if s.gone() && self.tx(|t| t.drop_proc(&self.session, pgid)).is_err() {
            return Stopped::Uncertain;
        }
        s
    }

    fn finish_release(&mut self, op: &OperationId, s: Stopped) {
        let session = self.session.clone();
        let saved = self.tx(|t| {
            let mut row = t.session(&session)?;
            let state = if s.gone() {
                if let Some(r) = row.as_mut() {
                    r.view.phase = SessionPhase::Released;
                    t.put_session(r)?;
                }
                done_state(OperationResult::Unit)
            } else {
                Observed::Unknown {
                    reason: "the harness did not stop".into(),
                }
            };
            t.set_state(op, &state)?;
            let mut evs = vec![op_changed(op, state)];
            if let Some(r) = row {
                evs.push((
                    Subject::Session {
                        session: session.clone(),
                    },
                    EventBody::SessionChanged { view: r.view },
                ));
            }
            crate::emit_all(&self.inner, t, evs)
        });
        if saved.is_err() {
            self.later(Msg::ReleaseDone {
                op: op.clone(),
                stopped: s,
            });
        }
    }

    async fn exited(&mut self, generation: u64, report: ExitReport) {
        // Observation uses the existing delivery ledger, but its failure must not fault or
        // stop the actor. A session subject avoids attributing a late exit to a newer attempt.
        let evidence = EventBody::Diagnostic {
            severity: if report.exit_code != Some(0) || report.wait_error || report.reader_failed {
                Severity::Error
            } else if report.parse_errors > 0 || report.drain_timed_out {
                Severity::Warning
            } else {
                Severity::Info
            },
            code: "adapter_exited".into(),
            safe_text: "Adapter process exited; structured process evidence is available.".into(),
            details: Some(DiagnosticDetails {
                exit_code: report.exit_code,
                signal: report.signal,
                generation: Some(generation),
                parse_errors: report.parse_errors,
                reader_failed: report.reader_failed,
                drain_timed_out: report.drain_timed_out,
                wait_error: report.wait_error,
            }),
        };
        let saved = self.inner.j.tx(|t| {
            crate::emit_all(
                &self.inner,
                t,
                vec![(
                    Subject::Session {
                        session: self.session.clone(),
                    },
                    evidence,
                )],
            )
        });
        if saved.is_ok() {
            self.inner.wake.notify_one();
        } else {
            tracing::warn!(session = %self.session, generation, "adapter exit diagnostic was not persisted");
        }
        if !self
            .live
            .as_ref()
            .is_some_and(|l| l.generation == generation)
        {
            return;
        }
        let l = self.live.take().expect("checked");
        // the leader is gone; its group may not be (then it stays recorded, and nothing new starts
        // in this session until it is gone)
        if procs::stop(l.pgid, l.started).await.gone() {
            let _ = self.tx(|t| t.drop_proc(&self.session, l.pgid));
        }
    }

    async fn open(&mut self, op: OperationId) {
        if !self.stop_process().await.gone() {
            let session = self.session.clone();
            let _ = self.tx(|t| {
                if let Some(mut row) = t.session(&session)? {
                    row.view.phase = SessionPhase::Released;
                    t.put_session(&row)?;
                }
                let state = Observed::Unknown {
                    reason: "the session's old harness did not stop".into(),
                };
                t.set_state(&op, &state)?;
                crate::emit_all(&self.inner, t, vec![op_changed(&op, state)])
            });
            return;
        }
        let token = self.next_token();
        self.opening = Some((token, Opening::Command(op)));
        self.spawn_open(token);
    }

    async fn opened(&mut self, token: u64, result: ApiResult<LiveNew>) {
        self.stage_cancel = None;
        self.stage_task = None;
        let purpose = match self.opening.take() {
            Some((t, p)) if t == token => p,
            other => {
                self.opening = other;
                // nobody waits for this start anymore: stop what started
                if let Ok(l) = result
                    && procs::stop(l.pgid, l.started).await.gone()
                {
                    let _ = self.tx(|t| t.drop_proc(&self.session, l.pgid));
                }
                return;
            }
        };
        match purpose {
            Opening::Command(op) => {
                let session = self.session.clone();
                let candidate = result.clone();
                let view = match result {
                    Ok(l) => {
                        let view = l.view.clone();
                        Ok(view)
                    }
                    Err(e) => Err(e),
                };
                let saved = self.tx(|t| {
                    let Some(mut row) = t.session(&session)? else {
                        return Ok(());
                    };
                    let current = t.open_ops_of(&session)?.into_iter().any(|(id, _)| id == op);
                    if row.view.phase != SessionPhase::Opening || !current {
                        return Err(Error::new(Code::Stale, "obsolete open result"));
                    }
                    let state = match &view {
                        Ok(v) => {
                            row.view = v.clone();
                            row.acp_id = v.resume.as_ref().map(ToString::to_string);
                            row.view.revision = Revision::new(t.revision()?).map_err(internal)?;
                            row.view.phase = SessionPhase::Ready;
                            done_state(OperationResult::Session(row.view.clone()))
                        }
                        Err(e) => {
                            row.view.phase = SessionPhase::Released;
                            if e.code == Code::OutcomeUnknown {
                                Observed::Unknown {
                                    reason: e.safe_detail.clone(),
                                }
                            } else {
                                failed_state(e.clone())
                            }
                        }
                    };
                    t.put_session(&row)?;
                    t.set_state(&op, &state)?;
                    crate::emit_all(
                        &self.inner,
                        t,
                        vec![
                            (
                                Subject::Session {
                                    session: session.clone(),
                                },
                                EventBody::SessionChanged { view: row.view },
                            ),
                            op_changed(&op, state),
                        ],
                    )
                });
                if saved.is_ok() {
                    if let Ok(l) = candidate {
                        self.live = Some(l.into());
                    }
                } else {
                    self.opening = Some((token, Opening::Command(op)));
                    self.later(Msg::Opened {
                        token,
                        result: candidate,
                    });
                }
            }
            Opening::Attempt(tok) => {
                if !self.run.as_ref().is_some_and(|r| r.token == tok) {
                    if let Ok(l) = result
                        && procs::stop(l.pgid, l.started).await.gone()
                    {
                        let _ = self.tx(|t| t.drop_proc(&self.session, l.pgid));
                    }
                    return;
                }
                if self.persisted_cancel() {
                    if let Ok(l) = result {
                        self.live = Some(l.into());
                    }
                    if self.stop_process().await.gone() {
                        let completion = self
                            .run
                            .as_ref()
                            .and_then(|r| r.stopped.clone())
                            .map(|(o, e)| failure(o, e, false))
                            .unwrap_or_else(cancelled);
                        self.end(completion);
                    } else {
                        self.unknown_end("cancelled startup did not stop");
                    }
                    return;
                }
                match result {
                    Ok(l) => {
                        let exec = self.run.as_ref().expect("checked").exec.clone();
                        let saved = self.tx(|t| {
                            let mut row = t
                                .session(&self.session)?
                                .ok_or_else(|| Error::new(Code::Stale, "session gone"))?;
                            if row.view.active.as_ref() != Some(&exec) {
                                return Err(Error::new(Code::Stale, "obsolete start result"));
                            }
                            row.acp_id = Some(l.acp_id.clone());
                            row.view.effective = l.view.effective.clone();
                            row.view.effective_permissions = l.view.effective_permissions;
                            row.view.resume = l.view.resume.clone();
                            row.view.revision = Revision::new(t.revision()?).map_err(internal)?;
                            t.put_session(&row)
                        });
                        let opened_acp_id = l.acp_id.clone();
                        self.live = Some(l.into());
                        if let Some(run) = self.run.as_mut() {
                            run.norm.set_root_session(&opened_acp_id);
                        }
                        if let Some(baseline) = self
                            .live
                            .as_ref()
                            .and_then(|live| live.cumulative_prompt_tokens)
                            && let Some(run) = self.run.as_mut()
                            && self
                                .inner
                                .cfg
                                .acp
                                .adapter_properties(&run.spec.run.choice.harness)
                                .usage_prompt_cumulative
                        {
                            run.norm.set_prompt_cumulative_baseline(baseline);
                        }
                        if saved.is_err() {
                            if self.stop_process().await.gone() {
                                self.end(failure(
                                    Outcome::Failed,
                                    internal("cannot save opened session"),
                                    false,
                                ));
                            } else {
                                self.unknown_end("cannot save or stop opened session");
                            }
                            return;
                        }
                        self.prepare(tok);
                    }
                    Err(e) => {
                        if e.code == Code::OutcomeUnknown {
                            return self.unknown_end(&e.safe_detail);
                        }
                        let outcome = if e.code == Code::Unavailable {
                            Outcome::Interrupted
                        } else {
                            Outcome::Failed
                        };
                        self.end(failure(outcome, e, false));
                    }
                }
            }
        }
    }

    // ── an attempt ──

    fn root(&self) -> std::path::PathBuf {
        self.inner
            .j
            .read(|t| t.session(&self.session))
            .ok()
            .flatten()
            .and_then(|r| {
                self.inner
                    .j
                    .read(|t| t.workspace(&r.open.workspace.id))
                    .ok()
                    .flatten()
            })
            .map(|w| std::path::PathBuf::from(w.path))
            .unwrap_or_default()
    }

    async fn start(&mut self, op: OperationId, spec: Box<TurnSpec>, exec: ExecutionRef) {
        let root = self.root();
        let token = self.next_token();
        let meter = Epoch::new(derived_id("m", spec.run.attempt.as_str()))
            .unwrap_or_else(|_| Epoch::new("m").expect("valid"));
        let props = self
            .inner
            .cfg
            .acp
            .adapter_properties(&spec.run.choice.harness);
        let image_root = root.canonicalize().unwrap_or_else(|_| root.clone());
        let image_scope = crate::inline_images::Scope::new(
            image_root.clone(),
            vec![root.clone()],
            crate::inline_images::protected(&self.inner.cfg.acp.steve_home, &self.inner.cfg.home),
        );
        let mut norm = Normalizer::new(root, spec.run.attempt.as_str(), meter)
            .for_harness(&spec.run.choice.harness)
            .input_includes_cache(props.usage_input_includes_cache);
        if let Some(live) = self.live.as_ref().filter(|l| !l.conn.has_exited()) {
            norm = norm.root_session(&live.acp_id);
        }
        if props.usage_prompt_cumulative {
            let baseline = self
                .live
                .as_ref()
                .filter(|l| !l.conn.has_exited())
                .and_then(|l| l.cumulative_prompt_tokens)
                .unwrap_or((0, 0, 0, 0, 0));
            norm = norm.prompt_cumulative_baseline(baseline);
        }
        self.run = Some(Run {
            token,
            budget_epoch: self.inner.j.budget_epoch(),
            exec,
            op,
            norm,
            spec,
            phase: Phase::Starting,
            acted: false,
            cancel: false,
            stopped: None,
            ending: None,
            closing: None,
            final_usage: vec![],
            usage_stop: None,
            usage_task: None,
            usage_pending: false,
            unknown: None,
            images: crate::inline_images::Images::for_replies(),
            image_scope,
        });
        self.inner.reset_output(&self.session);
        if self.persisted_cancel() {
            return self.end(cancelled());
        }
        if self.live.as_ref().is_some_and(|l| !l.conn.has_exited()) {
            self.prepare(token);
            return;
        }
        // a harness that is gone, or one left from before: confirmed gone before a new one
        if !self.stop_process().await.gone() {
            return self.unknown_end("the session's old harness did not stop");
        }
        self.opening = Some((token, Opening::Attempt(token)));
        self.spawn_open(token);
    }

    fn persisted_cancel(&self) -> bool {
        let Some(r) = &self.run else { return false };
        if r.cancel {
            return true;
        }
        match self.inner.j.read(|t| t.op(&r.op)) {
            Ok(Some(op)) => op.cancel,
            Ok(None) => true,
            Err(_) => {
                self.inner.fault();
                true
            }
        }
    }

    /// Attachments for the prompt, fetched in a task (bounded by the attempt's deadline).
    fn prepare(&mut self, token: u64) {
        let context = crate::permission_context(&self.inner, &self.session);
        let image = self.live.as_ref().is_some_and(|l| l.image);
        let Some(run) = self.run.as_mut() else { return };
        run.phase = Phase::Preparing;
        let (inner, exec, parts) = (
            self.inner.clone(),
            run.exec.clone(),
            run.spec.run.prompt.clone(),
        );
        let deadline = run.spec.run.limits.deadline;
        let tx = self.tx.clone();
        let (cancel, mut cancelled) = watch::channel(false);
        self.stage_cancel = Some(cancel);
        self.stage_task = Some(tokio::spawn(async move {
            let left = Duration::from_millis((deadline.0 - now()).max(0) as u64);
            let result = tokio::select! {
                _ = cancelled.changed() => Err(Error::new(Code::Stale, "preparation cancelled")),
                result = tokio::time::timeout(left, async {
                    let root = tokio::task::spawn_blocking(move || context?.dir()).await.map_err(internal)??;
                    crate::prompt(&inner, &exec, &root, &parts, image).await
                }) => {
                    result.unwrap_or_else(|_| Err(Error::new(Code::Stale, "past deadline preparing attachments")))
                }
            };
            let _ = tx.send(Msg::Prepared { token, result });
        }));
    }

    async fn prepared(&mut self, token: u64, result: ApiResult<Value>) {
        if !self
            .run
            .as_ref()
            .is_some_and(|r| r.token == token && r.phase == Phase::Preparing)
        {
            return;
        }
        self.stage_cancel = None;
        self.stage_task = None;
        if self.persisted_cancel() {
            let completion = self
                .run
                .as_ref()
                .and_then(|r| r.stopped.clone())
                .map(|(o, e)| failure(o, e, false))
                .unwrap_or_else(cancelled);
            return self.end(completion);
        }
        let mut prompt = match result {
            Ok(p) => p,
            Err(e) => return self.end(failure(Outcome::Failed, e, false)),
        };
        // the last look before the prompt goes out: cancelled (here or in the journal), past due
        if self.persisted_cancel() {
            return self.end(cancelled());
        }
        if self
            .run
            .as_ref()
            .is_some_and(|r| r.spec.run.limits.deadline.0 <= now())
        {
            return self.end(failure(
                Outcome::TimedOut,
                NodeStop::Deadline.error(),
                false,
            ));
        }
        let Some(l) = self.live.as_mut() else {
            return self.end(failure(
                Outcome::Interrupted,
                NodeStop::HarnessGone.error(),
                false,
            ));
        };
        if let Some(ctx) = l.pending_system_context.take() {
            prepend_user_context(&mut prompt, &ctx);
        }
        let (conn, acp_id, home) = (l.conn.clone(), l.acp_id.clone(), l.usage_home.clone());
        let r = self.run.as_ref().expect("run");
        let prefix = r.spec.run.attempt.as_str().to_owned();
        let meter = Epoch::new(derived_id("m", &prefix)).expect("derived epoch");
        let mut sampler = Sampler::start_source(home, &acp_id, &prefix, meter).await;
        if self.persisted_cancel() {
            return self.end(cancelled());
        }
        let (stop, mut stopped) = oneshot::channel();
        self.run.as_mut().expect("run").usage_stop = Some(stop);
        let tx = self.tx.clone();
        let usage_task = tokio::spawn(async move {
            let mut tick = tokio::time::interval(POLL_INTERVAL);
            loop {
                tokio::select! {
                    biased;
                    native = &mut stopped => {
                        let Ok(native) = native else { return; };
                        let facts = sampler.finish(native, SETTLE_WAIT).await;
                        let _ = tx.send(Msg::UsageSettled { token, facts });
                        return;
                    }
                    _ = tick.tick() => {
                        let facts = sampler.poll().await;
                        if !facts.is_empty() && tx.send(Msg::Usage { token, facts }).is_err() { return; }
                    }
                }
            }
        });
        if let Some(r) = self.run.as_mut() {
            r.usage_task = Some(UsageTask(usage_task));
            r.phase = Phase::Prompting;
        }
        // from here the adapter's messages are this attempt's
        self.gate.1.store(
            self.run
                .as_ref()
                .expect("run")
                .spec
                .run
                .limits
                .max_output_bytes,
            Ordering::SeqCst,
        );
        self.gate.2.store(false, Ordering::SeqCst);
        self.gate.0.store(token, Ordering::SeqCst);
        let mut req = Box::pin(async move {
            conn.request(
                "session/prompt",
                json!({"sessionId": acp_id, "prompt": prompt}),
                None,
            )
            .await
        });
        // the first poll writes the prompt, here and now: a cancel this actor sends later always
        // follows it on the transport
        let mut early = None;
        std::future::poll_fn(|cx| {
            if let std::task::Poll::Ready(v) = req.as_mut().poll(cx) {
                early = Some(v);
            }
            std::task::Poll::Ready(())
        })
        .await;
        let tx = self.tx.clone();
        self.children.spawn(async move {
            let result = match early {
                Some(r) => r,
                None => req.await,
            };
            let _ = tx.send(Msg::Prompted { token, result });
        });
    }

    fn cancel(&mut self, target: &OperationId) {
        let Some(run) = self.run.as_mut().filter(|r| r.op == *target) else {
            return; // not the attempt running now: nothing to send
        };
        run.cancel = true;
        match run.phase {
            Phase::Prompting => {
                if let Some(l) = &self.live {
                    l.conn
                        .notify("session/cancel", json!({"sessionId": l.acp_id}));
                }
                self.expire_waits();
            }
            // Keep occupancy until the stage task has acknowledged cancellation. Dropping its
            // result alone cannot stop attachment writes or a concurrent spawn.
            Phase::Starting | Phase::Preparing => {
                if let Some(cancel) = &self.stage_cancel {
                    let _ = cancel.send(true);
                }
            }
        }
    }

    fn cancel_subtask(&mut self, subtask: &CallId, done: oneshot::Sender<ApiResult<bool>>) {
        let Some(run) = self.run.as_ref().filter(|r| r.phase == Phase::Prompting) else {
            let _ = done.send(Ok(false));
            return;
        };
        let Some(live) = self.live.as_ref() else {
            let _ = done.send(Ok(false));
            return;
        };
        if let Some((method, params)) = run.norm.cancel_subtask_request(subtask) {
            live.conn.notify(method, params);
            tracing::info!(
                session = %self.session,
                subtask = %subtask,
                method,
                "node: sent single-subtask cancel notification"
            );
            let _ = done.send(Ok(true));
        } else {
            let _ = done.send(Ok(false));
        }
    }

    /// Answer `cancelled` to every waiting request of the running attempt and journal their expiry.
    fn expire_waits(&mut self) {
        let Some(run) = &self.run else { return };
        let (exec, op) = (run.exec.clone(), run.op.clone());
        let r = self.tx(|t| {
            let mut evs = vec![];
            for w in t.waits_of(&op)? {
                t.drop_wait(&w.id)?;
                evs.push((
                    Subject::Execution(exec.clone()),
                    EventBody::PermissionExpired { id: w.id },
                ));
            }
            if !evs.is_empty() {
                t.set_state(&op, &Observed::Running)?;
            }
            crate::emit_all(&self.inner, t, evs)
        });
        if r.is_ok() {
            for (_, (rpc_id, _)) in self.waits.drain() {
                if let Some(l) = &self.live {
                    l.conn
                        .respond(rpc_id, json!({"outcome": {"outcome": "cancelled"}}));
                }
            }
        }
    }

    fn attributed(&self, generation: u64, token: u64) -> bool {
        token != 0
            && self
                .live
                .as_ref()
                .is_some_and(|l| l.generation == generation)
            && self
                .run
                .as_ref()
                .is_some_and(|r| r.token == token && r.phase == Phase::Prompting)
    }

    async fn update(&mut self, generation: u64, token: u64, session_id: Option<&str>, u: &Value) {
        let session = self.session.clone();
        let mut evs: Vec<(Subject, EventBody)> = vec![];
        let mut config: Vec<Fact> = vec![];
        if self.attributed(generation, token) {
            let is_root = self
                .live
                .as_ref()
                .is_some_and(|l| session_id.is_none_or(|sid| sid == l.acp_id));
            if is_root
                && u.get("sessionUpdate").and_then(Value::as_str) == Some("usage_update")
                && let Some(t) = Normalizer::cumulative_tokens_from_usage_update(u)
                && let Some(live) = self.live.as_mut()
            {
                live.cumulative_prompt_tokens = Some(t);
            }
            let run = self.run.as_mut().expect("checked");
            let process_cost = &mut self.live.as_mut().expect("checked").cumulative_context_cost;
            for f in run.norm.update_for_session(session_id, u) {
                match f {
                    Fact::Event(EventBody::Usage { report })
                        if report.scope == UsageScope::Background =>
                    {
                        evs.push((
                            Subject::Session {
                                session: session.clone(),
                            },
                            EventBody::Usage { report },
                        ));
                    }
                    Fact::Event(EventBody::Usage { report })
                        if report.scope == UsageScope::Subtask =>
                    {
                        // Subtask usage reports are already attempt-local deltas (e.g. per-child
                        // results); never mix them with the parent process cumulative cost meter.
                        evs.push((
                            Subject::Execution(run.exec.clone()),
                            EventBody::Usage { report },
                        ));
                    }
                    Fact::Event(EventBody::Usage { mut report }) => {
                        // ACP's context notification carries a process total. Publish the increment
                        // of this attempt so it can be booked independently of earlier turns.
                        report.measurement = Measurement::Delta;
                        if let Some(cost) = &mut report.cost
                            && let Some(total) = usage::decimal_amount(&cost.decimal)
                        {
                            cost.decimal = (total - *process_cost).max(0.0).to_string();
                            *process_cost = process_cost.max(total);
                        }
                        evs.push((
                            Subject::Execution(run.exec.clone()),
                            EventBody::Usage { report },
                        ));
                    }
                    Fact::Event(mut e) => {
                        if let EventBody::ToolChanged { tool } = &mut e {
                            run.acted = true;
                            if let RunDirectory::Workspace { workspace } = &run.spec.run.directory
                                && let Some(fact) = u
                                    .get("toolCallId")
                                    .and_then(Value::as_str)
                                    .and_then(|id| run.norm.fact(id))
                            {
                                tool.evidence =
                                    crate::tool_evidence(fact, &run.image_scope.cwd, workspace);
                            }
                        } else if matches!(e, EventBody::SubtaskChanged { .. }) {
                            run.acted = true;
                        }
                        let files = self.inner.files.lock().unwrap().clone();
                        for e in run
                            .images
                            .process(&run.image_scope, &run.exec, files, vec![e])
                            .await
                        {
                            evs.push((Subject::Execution(run.exec.clone()), e));
                        }
                    }
                    f @ (Fact::Model(_) | Fact::Effort(_)) => config.push(f),
                    Fact::Nothing => {}
                }
            }
        } else if self
            .live
            .as_ref()
            .is_some_and(|l| l.generation == generation)
        {
            let is_root = self
                .live
                .as_ref()
                .is_some_and(|l| session_id.is_none_or(|sid| sid == l.acp_id));
            if is_root
                && u.get("sessionUpdate").and_then(Value::as_str) == Some("usage_update")
                && let Some(t) = Normalizer::cumulative_tokens_from_usage_update(u)
                && let Some(live) = self.live.as_mut()
            {
                live.cumulative_prompt_tokens = Some(t);
            }
            // outside our attempts only the harness's own background cost counts
            let mut n = Normalizer::new("/", "bg", Epoch::new("bg").expect("valid"));
            for f in n.update(u) {
                if let Fact::Event(EventBody::Usage { report }) = f
                    && report.scope == UsageScope::Background
                {
                    evs.push((
                        Subject::Session {
                            session: session.clone(),
                        },
                        EventBody::Usage { report },
                    ));
                }
            }
        }
        let mut evs = evs;
        if evs.is_empty() && config.is_empty() {
            return;
        }
        let streamed = config.is_empty();
        if let Err(error) = self.record_update(&session, &mut evs, &config) {
            self.update_failed(error, evs, streamed);
        }
    }

    /// Data transfers to the writer's window. Effective configuration is a session state change,
    /// so it must commit immediately with its events and whatever data preceded it.
    fn record_update(
        &self,
        session: &SessionId,
        evs: &mut Vec<(Subject, EventBody)>,
        config: &[Fact],
    ) -> ApiResult<()> {
        if config.is_empty() {
            return self.inner.j.events(evs.clone(), Some(session.clone()));
        }
        self.tx(|t| {
            t.stamp_usage(session, evs)?;
            if let Some(mut row) = t.session(session)? {
                for fact in config {
                    match fact {
                        Fact::Model(model) => row.view.effective.model = Some(model.clone()),
                        Fact::Effort(effort) => row.view.effective.effort = Some(effort.clone()),
                        _ => {}
                    }
                }
                t.put_session(&row)?;
                evs.push((
                    Subject::Session {
                        session: session.clone(),
                    },
                    EventBody::SessionChanged { view: row.view },
                ));
            }
            crate::emit_all(&self.inner, t, evs.clone())
        })
    }

    fn update_failed(&mut self, error: Error, events: Vec<(Subject, EventBody)>, streamed: bool) {
        tracing::error!("node journal ({}): {error}", self.session);
        let exhausted = error.code == Code::Exhausted;
        // A streaming-only update transfers to the window; an update with a causal boundary
        // commits as one caller-owned transaction. Retain its closing facts for terminal retry;
        // rejected data ends this attempt and is never submitted again.
        let retry = if streamed || exhausted {
            events
                .into_iter()
                .filter(|(_, body)| !crate::journal::needs_budget(body))
                .collect()
        } else {
            events
        };
        if !retry.is_empty() {
            self.later(Msg::Facts { events: retry });
        }
        if exhausted {
            if let Some(token) = self.run.as_ref().map(|run| run.token) {
                self.stopping(token, Outcome::Cancelled, NodeStop::JournalBudget.error());
                let _ = self.tx.send(Msg::Kill { token });
            }
            return;
        }
        if error.code == Code::Internal {
            self.inner.fault();
        }
        if let Some(token) = self.run.as_ref().map(|run| run.token) {
            self.stopping(token, Outcome::Failed, NodeStop::NotRecorded.error());
        }
    }

    fn permission(&mut self, generation: u64, token: u64, rpc_id: Value, params: &Value) {
        let cancelled = json!({"outcome": {"outcome": "cancelled"}});
        let Some(conn) = self
            .live
            .as_ref()
            .filter(|l| l.generation == generation)
            .map(|l| l.conn.clone())
        else {
            return; // from a process that is gone
        };
        if !self.attributed(generation, token) || self.run.as_ref().is_some_and(|r| r.cancel) {
            conn.respond(rpc_id, cancelled);
            return;
        }
        let run = self.run.as_mut().expect("checked");
        // merged into the attempt's own tool facts (a request may be the first word of a tool)
        let asked = permission::asked(&mut run.norm, &rpc_id, params);
        let (exec, op, deadline) = (
            run.exec.clone(),
            run.op.clone(),
            run.spec.run.limits.deadline,
        );
        let context = crate::permission_context(&self.inner, &self.session);
        let tx = self.tx.clone();
        self.children.spawn(async move {
            let copy = asked.clone();
            let rpc = rpc_id.clone();
            let result = match context {
                Ok(context) => match tokio::time::timeout(
                    Duration::from_secs(5),
                    tokio::task::spawn_blocking(move || {
                        crate::bind_permission(&context, &exec, &op, &rpc, &copy, deadline)
                    }),
                )
                .await
                {
                    Ok(Ok(result)) => result,
                    _ => Err(internal("permission binding timed out")),
                },
                Err(e) => Err(e),
            };
            let _ = tx.send(Msg::Bound {
                generation,
                token,
                rpc_id,
                asked: Box::new(asked),
                result,
            });
        });
    }

    fn permission_bound(
        &mut self,
        generation: u64,
        token: u64,
        rpc_id: Value,
        asked: Asked,
        result: ApiResult<PermissionRequest>,
    ) {
        let cancelled = json!({"outcome": {"outcome": "cancelled"}});
        let Some(conn) = self
            .live
            .as_ref()
            .filter(|l| l.generation == generation)
            .map(|l| l.conn.clone())
        else {
            return;
        };
        if !self.attributed(generation, token) || self.persisted_cancel() {
            conn.respond(rpc_id, cancelled);
            return;
        }
        let run = self.run.as_mut().expect("attributed");
        let (exec, op) = (run.exec.clone(), run.op.clone());
        let mut evs: Vec<(Subject, EventBody)> = vec![];
        for f in &asked.updates {
            if let Fact::Event(e) = f {
                if matches!(e, EventBody::ToolChanged { .. }) {
                    run.acted = true;
                }
                evs.push((Subject::Execution(exec.clone()), e.clone()));
            }
        }
        let req = match result {
            Ok(req) => req,
            Err(_) => {
                conn.respond(rpc_id, cancelled);
                return;
            }
        };
        let session = self.session.clone();
        let r = self.tx(|t| {
            t.put_wait(&op, &session, &req)?;
            let on: Vec<WaitRef> = t
                .waits_of(&op)?
                .into_iter()
                .map(|w| WaitRef::Permission(w.id))
                .collect();
            t.set_state(&op, &Observed::Waiting { on })?;
            evs.push((
                Subject::Execution(exec.clone()),
                EventBody::PermissionRequested {
                    request: req.clone(),
                },
            ));
            crate::emit_all(&self.inner, t, std::mem::take(&mut evs))
        });
        match r {
            Ok(()) => {
                self.waits.insert(req.id.clone(), (rpc_id, asked));
            }
            Err(_) => conn.respond(rpc_id, cancelled),
        }
    }

    /// A journaled answer: its preconditions are checked again in a task (git and file stats stay
    /// off the journal and off this actor), then [`Msg::Checked`] gives it to the harness.
    fn answer(&mut self, op: OperationId) {
        let found = self.inner.j.read(|t| {
            let Some(res) = t.answer_of(&op)? else {
                return Ok(None);
            };
            Ok(t.wait(&res.id)?.map(|(_, _, req)| (res, req)))
        });
        let Ok(Some((res, req))) = found else {
            return self.answer_done(
                &op,
                Err(Error::new(Code::Stale, "nobody waits for this answer")),
            );
        };
        let context = crate::permission_context(&self.inner, &self.session);
        let tx = self.tx.clone();
        let pre = req.preconditions.clone();
        let check = res.decision == PermissionDecision::AllowOnce;
        self.children.spawn(async move {
            let result = if check {
                match tokio::time::timeout(
                    Duration::from_secs(10),
                    tokio::task::spawn_blocking(move || crate::preconditions_hold(&context?, &pre)),
                )
                .await
                {
                    Ok(Ok(r)) => r,
                    _ => Err(Error::new(
                        Code::Stale,
                        "could not check what the request was about",
                    )),
                }
            } else {
                Ok(())
            };
            let _ = tx.send(Msg::Checked { op, result });
        });
    }

    fn answer_checked(&mut self, op: OperationId, checked: ApiResult<()>) {
        if let Err(e) = checked {
            return self.answer_done(&op, Err(e));
        }
        // the serial use boundary: still this attempt's waiting request, not cancelled, not past due
        let found = self.inner.j.read(|t| {
            let Some(res) = t.answer_of(&op)? else {
                return Ok(None);
            };
            Ok(t.wait(&res.id)?.map(|(aop, _, req)| (res, aop, req)))
        });
        let Ok(Some((res, attempt_op, req))) = found else {
            return self.answer_done(
                &op,
                Err(Error::new(Code::Stale, "nobody waits for this answer")),
            );
        };
        let live = self.run.as_ref().is_some_and(|r| {
            r.op == attempt_op && !r.cancel && r.phase == Phase::Prompting && r.ending.is_none()
        });
        if !live || req.expires_at.0 <= now() {
            return self.answer_done(&op, Err(Error::new(Code::Stale, "that attempt is over")));
        }
        let Some((rpc_id, asked)) = self.waits.get(&res.id).cloned() else {
            return self.answer_done(
                &op,
                Err(Error::new(Code::Stale, "nobody waits for this answer")),
            );
        };
        let Some(conn) = self.live.as_ref().map(|l| l.conn.clone()) else {
            return self.answer_done(&op, Err(Error::new(Code::Stale, "the harness is gone")));
        };
        // Durable send intent first. The final receipt needs writer evidence, not just enqueue.
        let r = self.tx(|t| {
            t.answer_for_send(&op, now())?;
            t.drop_wait(&res.id)?;
            let on: Vec<WaitRef> = t
                .waits_of(&attempt_op)?
                .into_iter()
                .map(|w| WaitRef::Permission(w.id))
                .collect();
            t.set_state(
                &attempt_op,
                &if on.is_empty() {
                    Observed::Running
                } else {
                    Observed::Waiting { on }
                },
            )?;
            Ok(())
        });
        match r {
            Ok(()) => {
                self.waits.remove(&res.id);
                let sent =
                    conn.respond_tracked(rpc_id, permission::answer(&asked, Some(res.decision)));
                self.sending += 1;
                let tx = self.tx.clone();
                self.children.spawn(async move {
                    let result = match sent {
                        Ok(rx) => match tokio::time::timeout(Duration::from_secs(10), rx).await {
                            Ok(Ok(Ok(()))) => Ok(()),
                            _ => Err(Error::new(
                                Code::OutcomeUnknown,
                                "permission write not confirmed",
                            )),
                        },
                        Err(e) => Err(Error::new(Code::OutcomeUnknown, e.message)),
                    };
                    let _ = tx.send(Msg::AnswerSent {
                        op,
                        req: Box::new(req),
                        res,
                        result,
                    });
                });
            }
            Err(e) if e.code == Code::Stale => self.answer_done(&op, Err(e)),
            Err(_) => self.later(Msg::Answer { op }),
        }
    }

    fn answer_done(&mut self, op: &OperationId, r: ApiResult<()>) {
        let state = match &r {
            Ok(()) => done_state(OperationResult::Unit),
            Err(e) => failed_state(e.clone()),
        };
        let saved = self.tx(|t| {
            t.drop_answer(op)?;
            t.set_state(op, &state)?;
            crate::emit_all(&self.inner, t, vec![op_changed(op, state)])
        });
        if saved.is_err() {
            self.later(Msg::AnswerDone {
                op: op.clone(),
                result: r,
            });
        }
    }

    /// The prompt answered: build the attempt's completion and end it.
    async fn finish(&mut self, result: Result<Value, RpcError>) {
        let Some(run) = self.run.as_ref() else { return };
        if run.ending.is_some() {
            return;
        }
        let acted = run.acted;
        let stopped = run.stopped.clone();
        let completion = match result {
            Ok(resp) => {
                let stop = resp
                    .get("stopReason")
                    .and_then(Value::as_str)
                    .unwrap_or("end_turn")
                    .to_string();
                if let Some(run) = self.run.as_mut() {
                    run.final_usage = run
                        .norm
                        .prompt_response(&resp)
                        .into_iter()
                        .filter_map(|f| match f {
                            Fact::Event(e) => Some((Subject::Execution(run.exec.clone()), e)),
                            _ => None,
                        })
                        .collect();
                    if let Some(cum) = run.norm.cumulative_prompt_tokens()
                        && let Some(live) = self.live.as_mut()
                    {
                        live.cumulative_prompt_tokens = Some(cum);
                    }
                }
                match stopped {
                    Some((outcome, e)) => failure(outcome, e, acted),
                    None => Completion {
                        outcome: if stop == "cancelled" {
                            Outcome::Cancelled
                        } else {
                            Outcome::Succeeded
                        },
                        stop_reason: stop,
                        error: None,
                        result: vec![],
                        continuation: None,
                        transient: false,
                        acted,
                        job: None,
                    },
                }
            }
            Err(e) => {
                let gone = self.live.as_ref().is_none_or(|l| l.conn.has_exited())
                    || steve_acp::is_dead_agent(&e.message);
                // the next attempt starts a fresh adapter (it loads the session from history);
                // this one ends only once its group is confirmed gone
                if gone && !self.stop_process().await.gone() {
                    return self.unknown_end("the harness did not stop");
                }
                if !gone
                    && let Some(cum) = self
                        .run
                        .as_ref()
                        .and_then(|r| r.norm.cumulative_prompt_tokens())
                    && let Some(live) = self.live.as_mut()
                {
                    live.cumulative_prompt_tokens = Some(cum);
                }
                match stopped {
                    Some((outcome, err)) => failure(outcome, err, acted),
                    None => {
                        let transient = !gone && !acted && steve_acp::is_transient(&e.message);
                        Completion {
                            outcome: if gone {
                                Outcome::Interrupted
                            } else {
                                Outcome::Failed
                            },
                            stop_reason: "error".into(),
                            transient,
                            acted,
                            error: Some(if transient {
                                Error::later(Code::Unavailable, e.message)
                            } else {
                                Error::new(Code::Internal, e.message)
                            }),
                            result: vec![],
                            continuation: None,
                            job: None,
                        }
                    }
                }
            }
        };
        if let Some(run) = self.run.as_mut() {
            let closing = run
                .norm
                .finish()
                .into_iter()
                .filter_map(|f| match f {
                    Fact::Event(e) => Some(e),
                    _ => None,
                })
                .collect();
            let files = self.inner.files.lock().unwrap().clone();
            let mut closing = run
                .images
                .process(&run.image_scope, &run.exec, files.clone(), closing)
                .await;
            let tail = run
                .images
                .capture(&run.image_scope, &run.exec, files, true)
                .await;
            closing.splice(0..0, tail);
            run.closing = Some(closing);
        }
        self.end(completion);
    }

    /// Ends the running attempt: its waits expire, its last events, its final state,
    /// `attempt_ended`, and the session free again, in one transaction; only then is it gone here.
    fn end(&mut self, completion: Completion) {
        let Some(run) = self.run.as_mut() else { return };
        if run.ending.is_none() {
            run.ending = Some(completion);
            if let Some(stop) = run.usage_stop.take() {
                let native = run
                    .final_usage
                    .iter()
                    .map(|(_, e)| Fact::Event(e.clone()))
                    .collect();
                run.usage_pending = stop.send(native).is_ok();
            }
        }
        self.commit_end();
    }

    fn commit_end(&mut self) {
        if self.sending != 0 || !self.pending.is_empty() {
            self.arm_retry();
            return;
        }
        let Some(run) = self.run.as_mut() else { return };
        if run.usage_pending {
            return;
        }
        let Some(completion) = run.ending.clone() else {
            return;
        };
        let closing = run
            .closing
            .get_or_insert_with(|| {
                let mut closing = run.images.drain_original();
                closing.extend(
                    run.norm
                        .finish()
                        .into_iter()
                        .filter_map(|f| match f {
                            Fact::Event(e) => Some(e),
                            _ => None,
                        })
                        .collect::<Vec<_>>(),
                );
                closing
            })
            .clone();
        let usage = run.final_usage.clone();
        let (exec, op) = (run.exec.clone(), run.op.clone());
        let budget_epoch = run.budget_epoch;
        let session = self.session.clone();
        let unlisted = self.inner.tools.unlisted(&session);
        let r = self.tx(|t| {
            let completion = self
                .inner
                .j
                .recorded_completion(budget_epoch, completion.clone());
            let mut evs = usage.clone();
            if unlisted {
                evs.push((
                    Subject::Session {
                        session: session.clone(),
                    },
                    EventBody::Diagnostic {
                        severity: Severity::Warning,
                        code: STEVE_TOOLS_MISSING.into(),
                        safe_text: "The harness never listed Steve's tools during this attempt."
                            .into(),
                        details: None,
                    },
                ));
            }
            for w in t.waits_of(&op)? {
                t.drop_wait(&w.id)?;
                evs.push((
                    Subject::Execution(exec.clone()),
                    EventBody::PermissionExpired { id: w.id },
                ));
            }
            for e in &closing {
                evs.push((Subject::Execution(exec.clone()), e.clone()));
            }
            let state = Observed::Finished {
                outcome: completion.outcome,
                result: Some(OperationResult::Execution(completion.clone())),
                error: completion.error.clone(),
            };
            t.set_state(&op, &state)?;
            evs.push((
                Subject::Execution(exec.clone()),
                EventBody::AttemptEnded {
                    completion: completion.clone(),
                },
            ));
            if let Some(mut row) = t.session(&session)?
                && row.view.active.as_ref() == Some(&exec)
            {
                row.view.phase = SessionPhase::Ready;
                row.view.active = None;
                t.put_session(&row)?;
            }
            crate::emit_all(&self.inner, t, evs)
        });
        match r {
            Ok(()) => {
                self.gate.0.store(0, Ordering::SeqCst);
                self.inner.reset_output(&self.session);
                self.run = None;
                for (_, (rpc_id, _)) in self.waits.drain() {
                    if let Some(l) = &self.live {
                        l.conn
                            .respond(rpc_id, json!({"outcome": {"outcome": "cancelled"}}));
                    }
                }
            }
            Err(_) => self.arm_retry(),
        }
    }

    /// The attempt may still be running somewhere (its group did not stop): it stays unknown and
    /// keeps its session; the Hub decides (abandon, once the group is gone).
    fn unknown_end(&mut self, why: &str) {
        let Some(run) = &mut self.run else { return };
        run.unknown = Some(why.to_string());
        let op = run.op.clone();
        let exec = run.exec.clone();
        let state = Observed::Unknown { reason: why.into() };
        if self
            .tx(|t| {
                let mut events = vec![op_changed(&op, state.clone())];
                for wait in t.waits_of(&op)? {
                    t.drop_wait(&wait.id)?;
                    events.push((
                        Subject::Execution(exec.clone()),
                        EventBody::PermissionExpired { id: wait.id },
                    ));
                }
                t.set_state(&op, &state)?;
                crate::emit_all(&self.inner, t, events)
            })
            .is_ok()
        {
            self.gate.0.store(0, Ordering::SeqCst);
            self.run = None;
            self.waits.clear();
        } else {
            self.arm_retry();
        }
    }

    /// The Node stops the attempt with `token`: cancel now, its group after a grace.
    fn stopping(&mut self, token: u64, outcome: Outcome, error: Error) {
        let Some(run) = self.run.as_mut().filter(|r| r.token == token) else {
            return;
        };
        if run.stopped.is_some() || run.ending.is_some() {
            return;
        }
        run.stopped = Some((outcome, error));
        let target = run.op.clone();
        let prompting = run.phase == Phase::Prompting;
        self.cancel(&target);
        if !prompting {
            return;
        }
        let tx = self.tx.clone();
        self.children.spawn(async move {
            tokio::time::sleep(Duration::from_secs(10)).await;
            let _ = tx.send(Msg::Kill { token });
        });
    }

    /// Deadline, lost Hub (past the lease), output limit and a failing journal, every tick.
    fn check_limits(&mut self) {
        let Some(run) = &self.run else { return };
        if run.stopped.is_some() || run.ending.is_some() {
            return;
        }
        let now = now();
        let lim = &run.spec.run.limits;
        let hub_down = self.inner.hub_down_since().is_some_and(|since| since < now);
        let why = if now >= lim.deadline.0 {
            Some((Outcome::TimedOut, NodeStop::Deadline.error()))
        } else if hub_down && now >= lim.offline_until.0 {
            Some((Outcome::TimedOut, NodeStop::HubUnreachable.error()))
        } else if self.inner.output(&self.session) > lim.max_output_bytes {
            Some((Outcome::Cancelled, NodeStop::OutputLimit.error()))
        } else if self.inner.over_limit() || self.inner.j.budget_epoch() != run.budget_epoch {
            Some((Outcome::Cancelled, NodeStop::JournalBudget.error()))
        } else if self.inner.faulted() {
            Some((Outcome::Failed, NodeStop::JournalFails.error()))
        } else {
            None
        };
        if let Some((o, e)) = why {
            let token = run.token;
            self.stopping(token, o, e);
        }
    }
}

/// The principal's own directory for a harness (path model §4.5.1), made when missing under the
/// Node's `state/` (0700). A principal who owns this Node starts with what the adapter declares of
/// their own directory here (their settings, their login); a file already there is theirs to keep.
fn valid_rel_path(rel: &str) -> Result<Vec<std::ffi::OsString>, Error> {
    let p = std::path::Path::new(rel);
    if p.is_absolute() {
        return Err(Error::new(
            Code::Invalid,
            format!("harness state: path '{rel}' must be relative"),
        ));
    }
    let mut segs = Vec::new();
    for c in p.components() {
        match c {
            std::path::Component::Normal(seg) if !seg.is_empty() => segs.push(seg.to_os_string()),
            _ => {
                return Err(Error::new(
                    Code::Invalid,
                    format!("harness state: unsafe relative path '{rel}'"),
                ));
            }
        }
    }
    if segs.is_empty() {
        return Err(Error::new(
            Code::Invalid,
            format!("harness state: empty relative path '{rel}'"),
        ));
    }
    Ok(segs)
}

fn read_owner_regular_file(own: &std::path::Path, segs: &[std::ffi::OsString]) -> Option<Vec<u8>> {
    // Reject if `own` itself is a symlink; intermediate segments under `own` must be real dirs.
    let own_meta = std::fs::symlink_metadata(own).ok()?;
    if own_meta.file_type().is_symlink() || !own_meta.is_dir() {
        return None;
    }
    let mut cur = own.to_path_buf();
    for (idx, seg) in segs.iter().enumerate() {
        cur.push(seg);
        let meta = std::fs::symlink_metadata(&cur).ok()?;
        if meta.file_type().is_symlink() {
            return None;
        }
        if idx + 1 < segs.len() {
            if !meta.is_dir() {
                return None;
            }
        } else if !meta.is_file() {
            return None;
        }
    }
    #[cfg(unix)]
    {
        use std::io::Read as _;
        use std::os::unix::fs::OpenOptionsExt as _;
        let mut f = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW)
            .open(&cur)
            .ok()?;
        let meta = f.metadata().ok()?;
        if !meta.is_file() || meta.len() > 4 * 1024 * 1024 {
            return None;
        }
        let mut buf = Vec::with_capacity(meta.len() as usize);
        f.read_to_end(&mut buf).ok()?;
        Some(buf)
    }
    #[cfg(not(unix))]
    {
        std::fs::read(&cur).ok()
    }
}

fn harness_state(
    inner: &Inner,
    s: &HarnessState,
    sd: &steve_harness_spec::StateDir,
    skills: Option<&str>,
) -> ApiResult<std::path::PathBuf> {
    if !s.valid() {
        return Err(Error::new(
            Code::Invalid,
            "a harness state directory that is not a plain path",
        ));
    }
    // every step a real directory made here, never one a run planted as a link
    let mut dir = inner.cfg.home.clone();
    let parts = std::iter::once("state")
        .chain(s.dir.split('/'))
        .chain(std::iter::once("harness"));
    for part in parts {
        dir.push(part);
        crate::attachments::private_dir(&dir)
            .map_err(|e| internal(format!("harness state: {e}")))?;
    }
    if sd.xdg {
        let xdg_root = dir.join("xdg");
        crate::attachments::private_dir(&xdg_root)
            .map_err(|e| internal(format!("harness state: {e}")))?;
        for sub in ["data", "config", "state", "cache"] {
            crate::attachments::private_dir(&xdg_root.join(sub))
                .map_err(|e| internal(format!("harness state: {e}")))?;
        }
    }
    let mut target = dir.clone();
    if sd.home_override && !sd.default.is_empty() {
        for seg in valid_rel_path(&sd.default)? {
            target.push(seg);
            crate::attachments::private_dir(&target)
                .map_err(|e| internal(format!("harness state: {e}")))?;
        }
    } else if let Some(sub) = sd.import_into.as_deref().filter(|s| !s.is_empty()) {
        for seg in valid_rel_path(sub)? {
            target.push(seg);
            crate::attachments::private_dir(&target)
                .map_err(|e| internal(format!("harness state: {e}")))?;
        }
    }
    if s.import_owner
        && let Some(own) = sd.owner_source(&inner.cfg.acp.user_home)
    {
        for f in &sd.import {
            let segs = valid_rel_path(f)?;
            let mut to = target.clone();
            for (idx, seg) in segs.iter().enumerate() {
                to.push(seg);
                if idx + 1 < segs.len() {
                    crate::attachments::private_dir(&to)
                        .map_err(|e| internal(format!("harness state: {f}: {e}")))?;
                }
            }
            if std::fs::symlink_metadata(&to).is_ok() {
                continue;
            }
            let Some(raw) = read_owner_regular_file(&own, &segs) else {
                continue;
            };
            crate::attachments::publish_private_if_absent(&to, &raw)
                .map_err(|e| internal(format!("harness state: {f}: {e}")))?;
        }
    }
    // the harness's sign-in on this Node, for every principal: its credentials linked (never
    // copied) and the sign-in entries of its settings the Node's own (after the owner's import,
    // whose file they go into)
    if let Some(node) = sd.owner_source(&inner.cfg.acp.user_home) {
        match crate::harness_login::place(&node, &target, &sd.login) {
            Ok(left) => {
                for why in left {
                    tracing::info!("harness state: sign-in: {why}");
                }
            }
            Err(why) => {
                return Err(internal(format!("harness state: sign-in: {why}")));
            }
        }
    }
    // the owner's personal instructions and extensions, linked where the harness reads them, when
    // the session is theirs and the Hub did not turn them off; otherwise any links made before go
    let own = s
        .import_owner
        .then(|| sd.owner_source(&inner.cfg.acp.user_home))
        .flatten();
    let specs: &[String] = match &own {
        Some(_) if s.owner_context => &sd.context,
        _ => &[],
    };
    let source = own.as_deref().unwrap_or(&target);
    // the Bot's skills go under the state root itself ([`crate::bot_skills::place`])
    let skills = skills.filter(|_| target == dir);
    match crate::harness_context::link(source, &target, specs, skills) {
        Ok(left) => {
            for why in left {
                tracing::info!("harness state: owner context: {why}");
            }
        }
        Err(why) => tracing::warn!("harness state: the owner's context was not linked: {why}"),
    }
    Ok(dir)
}

/// The Node's own value of `name` for `harness`: what the adapter is configured with, else the
/// Node's environment.
fn node_var(inner: &Inner, harness: &str, name: &str) -> Option<std::ffi::OsString> {
    inner
        .cfg
        .acp
        .adapter_env(harness)
        .into_iter()
        .find(|(k, _)| k == name)
        .map(|(_, v)| v)
        .or_else(|| std::env::var_os(name))
}

/// [`steve_harness_spec::StateDir::node_store`] on this Node.
fn node_store(
    inner: &Inner,
    harness: &str,
    sd: &steve_harness_spec::StateDir,
) -> Option<(std::ffi::OsString, std::ffi::OsString, std::path::PathBuf)> {
    sd.node_store(&inner.cfg.acp.user_home, |n| node_var(inner, harness, n))
}

/// The Node's credential store of `harness` ([`steve_harness_spec::Login`]): in the directory its store
/// variable names when it has one, else in its own directory; `None` when it declares no name
/// there or the Node has no such directory.
fn node_login_store(
    inner: &Inner,
    harness: &str,
    sd: &steve_harness_spec::StateDir,
) -> Option<crate::confine::LoginStore> {
    let names = sd.login.store_names();
    if names.is_empty() {
        return None;
    }
    let dir = match node_store(inner, harness, sd) {
        Some((_, _, dir)) => dir,
        None => sd.owner_source(&inner.cfg.acp.user_home)?,
    };
    Some(crate::confine::LoginStore { dir, names })
}

/// The environment a session's harness gets over the Node's: the principal's own variables
/// (`node_env`; a key the Node refuses is dropped, `PATH_PREPEND` goes in front of `PATH`). What
/// Steve sets itself comes after them.
fn harness_env(
    user: &[(String, String)],
    path: Option<&std::ffi::OsStr>,
) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    use steve_node_api::env::{PATH_PREPEND, user_env_refusal};
    let mut env: Vec<(std::ffi::OsString, std::ffi::OsString)> = Vec::new();
    for (k, v) in user {
        if let Some(why) = user_env_refusal(k) {
            tracing::warn!("node: {k} is not set for the harness: {why}");
            continue;
        }
        if k == PATH_PREPEND {
            let path = path
                .map(std::ffi::OsStr::to_os_string)
                .or_else(|| std::env::var_os("PATH"))
                .unwrap_or_default();
            let mut joined = std::ffi::OsString::from(v);
            if !path.is_empty() {
                joined.push(":");
                joined.push(path);
            }
            env.push(("PATH".into(), joined));
        } else {
            env.push((k.into(), v.into()));
        }
    }
    env
}

/// Starts and opens a harness for `session` (in a task). Its group is recorded before the handshake
/// (never over one not confirmed gone) and stopped again if anything after fails.
async fn open_harness(
    inner: &Arc<Inner>,
    session: &SessionId,
    tx: mpsc::UnboundedSender<Msg>,
    data_tx: mpsc::Sender<Msg>,
    gate: Arc<Gate>,
    mut cancelled: watch::Receiver<bool>,
) -> ApiResult<LiveNew> {
    if *cancelled.borrow() {
        return Err(internal("startup cancelled"));
    }
    let row = inner
        .j
        .read(|t| t.session(session))?
        .ok_or_else(|| Error::new(Code::Invalid, "no such session"))?;
    let cwd = crate::workspace_dir(inner, &row.open.workspace)?;
    let cwd = cwd.to_string_lossy().to_string();
    let harness = row.open.choice.harness.clone();
    let (program, args) = inner
        .cfg
        .acp
        .adapter_command_for_mode(&harness, Some(row.open.policy.permissions))
        .map_err(|e| Error::new(Code::Unsupported, e))?;
    // the principal's own harness directory, when the Hub asks for one and the adapter can move it
    let declared_state = inner.cfg.acp.adapter_properties(&harness).state;
    let state = match (&row.open.state, declared_state.as_ref()) {
        (Some(s), Some(sd)) => {
            let props = inner.cfg.acp.adapter_properties(&harness);
            let dir = harness_state(inner, s, sd, props.skills.as_deref())?;
            let mut env =
                sd.isolation_env_for(&dir, Some(&inner.cfg.acp.user_home), s.import_owner);
            // its credential store where the Node's harness keeps it, apart from the directory
            if let Some((var, value, _)) = node_store(inner, &harness, sd) {
                env.push((var, value));
            }
            Some((dir, env))
        }
        (None, Some(sd)) if sd.home.is_some() => {
            let dir = sd
                .owner_source(&inner.cfg.acp.user_home)
                .unwrap_or_else(|| inner.cfg.acp.user_home.join(&sd.default));
            std::fs::create_dir_all(&dir).map_err(internal)?;
            crate::attachments::private_dir(&dir).map_err(internal)?;
            let env = sd.isolation_env_for(&dir, Some(&inner.cfg.acp.user_home), false);
            Some((dir, env))
        }
        _ => None,
    };
    let adapter_home = match (
        &state,
        declared_state.as_ref().and_then(|s| s.home.as_deref()),
    ) {
        (Some((root, _)), Some(sub)) => {
            if !(HarnessState {
                dir: sub.to_string(),
                import_owner: false,
                owner_context: true,
            })
            .valid()
            {
                return Err(Error::new(
                    Code::Invalid,
                    "adapter state HOME must be a plain relative directory",
                ));
            }
            let dir = root.join(sub);
            crate::attachments::private_dir(&dir).map_err(internal)?;
            Some(dir)
        }
        _ => None,
    };
    // the Bot's skills where the adapter declares it reads them, in that directory
    match (
        &state,
        inner.cfg.acp.adapter_properties(&harness).skills.as_deref(),
    ) {
        (Some((dir, _)), Some(sub)) => {
            if let Err(why) = crate::bot_skills::place(dir, sub, &row.open.skills) {
                tracing::warn!(session = %session, "the Bot's skills were not placed: {why}");
            }
        }
        _ if !row.open.skills.is_empty() => tracing::info!(
            session = %session,
            "the Bot's skills are not given: the harness reads none, or the session has no harness directory of its own"
        ),
        _ => {}
    }
    if state.is_none()
        && let Some(sd) = &declared_state
    {
        for d in sd.ambient_dirs(&inner.cfg.acp.user_home) {
            let _ = std::fs::create_dir_all(d);
        }
    }
    // its toolchains' outputs and caches in the item's and the person's own places
    let tools = crate::toolchain::for_run(
        &inner.cfg.home,
        std::path::Path::new(&cwd),
        row.open.state.as_ref(),
    );
    // level 2 of the sandbox: a fresh login environment, then Steve's own for the run
    let fresh: Option<Vec<(std::ffi::OsString, std::ffi::OsString)>> = inner
        .cfg
        .acp
        .adapter_properties(&harness)
        .nested_sandbox
        .then(|| inner.confine.fresh_env().map(<[_]>::to_vec))
        .flatten();
    // in the OS sandbox, unless the harness sandboxes its own shell (macOS nests none)
    let adapter_props = inner.cfg.acp.adapter_properties(&harness);
    let system_context = row
        .open
        .system_context
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty());
    let mut cli_system_context_applied = false;
    let mut args = args;
    if let (Some(ctx), Some(steve_harness_spec::AppendSystemContext::CliFlag { flag })) =
        (system_context, adapter_props.append_system_context.as_ref())
    {
        args.push(flag.into());
        args.push(ctx.into());
        cli_system_context_applied = true;
    }
    if let (Some((dir, _)), Some(sd)) = (&state, &declared_state) {
        args.extend(sd.isolation_args(dir));
    }
    if adapter_props.nested_sandbox
        && crate::confine::is_outer_sandbox_active(false, Some(&inner.confine))
    {
        for flag in &adapter_props.outer_sandbox_args {
            if !args.iter().any(|a| a == flag.as_str()) {
                args.push(flag.into());
            }
        }
    }
    let hide_cli_creds = row.open.state.as_ref().is_some_and(|st| !st.import_owner);
    // Even reading Pi's linked auth.json takes a lock beside its real target. Allow the
    // declared names where the Node keeps its sign-in, not the whole harness directory.
    let login_store = match (&row.open.state, &state, &declared_state) {
        (Some(_), Some(_), Some(sd))
            if inner.confine.opens_principal_login_store(hide_cli_creds) =>
        {
            node_login_store(inner, &harness, sd)
        }
        _ => None,
    };
    let (program, args) = if adapter_props.nested_sandbox {
        inner.confine.wrap_in_mode(
            program,
            args,
            &[],
            &state
                .iter()
                .map(|(d, _)| d.clone())
                .chain(tools.person.clone())
                .collect::<Vec<_>>(),
            Some(std::path::Path::new(&cwd)),
            state.is_some(),
            hide_cli_creds,
            Some(&inner.cfg.acp.user_home),
            login_store.as_ref(),
        )
    } else {
        (program, args)
    };
    let generation = inner.generation.fetch_add(1, Ordering::SeqCst) + 1;
    let (tx1, tx2) = (tx.clone(), tx);
    let (retire, retired) = watch::channel(false);
    let retire = Arc::new(retire);
    let counter = inner.output_counter(session);
    let meter = derived_id(
        "p",
        &format!("{session}.{}.{generation}", inner.incarnation),
    );
    let mut registered_start = None;
    let admission_gate = gate.clone();
    let admission_tx = tx1.clone();
    let ctl = Ctl {
        gate: gate.clone(),
        tx: tx1,
        retired,
        stall: inner.cfg.storage.ingress_stall,
    };
    type SeenTokens = (u64, u64, u64, u64, u64);
    let seen_tokens: Arc<std::sync::Mutex<Option<SeenTokens>>> =
        Arc::new(std::sync::Mutex::new(None));
    let seen_tokens_reader = seen_tokens.clone();
    let spawn_env = {
        let base_path = fresh
            .as_ref()
            .and_then(|f| f.iter().find(|(k, _)| k == "PATH").map(|(_, v)| v.clone()));
        let mut env = harness_env(&row.open.env, base_path.as_deref());
        // the toolchains', then Steve's own for the adapter last: nothing before overrides
        // them
        env.extend(tools.env);
        env.extend(inner.cfg.acp.adapter_env(&harness));
        if let Some((_, vars)) = &state {
            env.extend(vars.iter().cloned());
        }
        if let Some(home) = &adapter_home {
            env.push(("HOME".into(), home.clone().into_os_string()));
        }
        // level 2: the login environment first, everything above over it
        if let Some(f) = &fresh {
            let mut all = f.clone();
            all.extend(env);
            env = all;
        }
        env
    };
    let usage_home = steve_acp::usage::source_for_spawn(
        &inner.cfg.acp,
        &harness,
        &spawn_env,
        std::path::Path::new(&cwd),
        fresh.is_none(),
    );
    let session_routes = Arc::new(std::sync::Mutex::new(SessionRoutes::default()));
    let incoming_routes = session_routes.clone();
    let conn = Conn::spawn_registered_observed(
        Spawn {
            program,
            args,
            cwd: cwd.clone().into(),
            env: spawn_env,
            log_tag: format!("[node {session}]"),
            clean_env: fresh.is_some(),
            prefix: inner.cfg.acp.adapter_prefix(&harness),
        },
        HARNESS_MAX_FRAME_BYTES,
        move |bytes| {
            let token = admission_gate.0.load(Ordering::SeqCst);
            if token == 0 {
                let total = counter
                    .fetch_add(bytes as u64, Ordering::SeqCst)
                    .saturating_add(bytes as u64);
                return bytes <= HARNESS_MAX_FRAME_BYTES && total <= 32 << 20;
            }
            let total = counter
                .fetch_add(bytes as u64, Ordering::SeqCst)
                .saturating_add(bytes as u64);
            let why = if bytes > HARNESS_MAX_FRAME_BYTES {
                Some(Ingress::Frame)
            } else if total > admission_gate.1.load(Ordering::SeqCst) {
                Some(Ingress::Output)
            } else {
                None
            };
            if let Some(why) = why {
                admission_gate.overflow(&admission_tx, token, why);
                return false;
            }
            !admission_gate.2.load(Ordering::SeqCst)
        },
        |pid| {
            let started = procs::start_time(pid as i64).ok_or_else(|| RpcError {
                message: "cannot identify gated adapter".into(),
                data: None,
            })?;
            inner
                .j
                .tx(|t| {
                    t.put_proc(
                        session,
                        pid as i64,
                        generation,
                        inner.incarnation,
                        &meter,
                        Some(started),
                    )
                })
                .map_err(|e| RpcError {
                    message: e.to_string(),
                    data: None,
                })?;
            registered_start = Some(started);
            Ok(())
        },
        move |c, inc| {
            let token = gate.0.load(Ordering::SeqCst);
            match inc {
                Incoming::Notification { method, params } if method == "session/update" => {
                    let sid = params.get("sessionId").and_then(Value::as_str);
                    let u = params.get("update")?;
                    let is_root = {
                        let mut routes = incoming_routes.lock().unwrap();
                        if !routes.accepts(sid) {
                            return None;
                        }
                        // Register child subagent sessions synchronously on the reader thread so
                        // immediately following child updates are never dropped.
                        routes.register_if_subagent(u);
                        routes.is_root(sid)
                    };
                    // counted as it arrives (admission): a flood is stopped before it is all kept
                    if token == 0
                        && is_root
                        && u.get("sessionUpdate").and_then(Value::as_str) == Some("usage_update")
                        && let Some(t) = Normalizer::cumulative_tokens_from_usage_update(u)
                    {
                        *seen_tokens_reader.lock().unwrap() = Some(t);
                    }
                    let update = Msg::Update {
                        generation,
                        token,
                        session_id: sid.map(String::from),
                        update: u.clone(),
                    };
                    return enqueue(&data_tx, update, token, &ctl);
                }
                Incoming::Notification { .. } => {}
                Incoming::Request { id, method, params }
                    if method == "session/request_permission" =>
                {
                    if token == 0
                        || !incoming_routes
                            .lock()
                            .unwrap()
                            .accepts(params.get("sessionId").and_then(Value::as_str))
                    {
                        c.respond(id, json!({"outcome": {"outcome": "cancelled"}}));
                        return None;
                    }
                    // after the updates read before it (its tool's own facts come first): a full
                    // queue delays it as long as the actor is behind, it is never dropped for that
                    let ask = Msg::Permission {
                        generation,
                        token,
                        rpc_id: id,
                        params,
                    };
                    return enqueue(&data_tx, ask, token, &ctl);
                }
                Incoming::Request { id, method, .. } => {
                    c.respond_error(id, -32601, &format!("{method} not supported by Steve"));
                }
            }
            None
        },
        move |report| {
            let _ = tx2.send(Msg::Exited { generation, report });
        },
    )
    .map_err(|e| Error::new(Code::Unsupported, e.message))?;
    let pgid = conn.pid() as i64;
    let started = registered_start;
    #[derive(Default)]
    struct Seen(std::sync::Mutex<(Option<String>, Option<String>, Option<String>)>);
    impl Reports for Seen {
        fn new_session(&self, acp_id: &str) {
            self.0.lock().unwrap().0 = Some(acp_id.into());
        }
        fn model(&self, m: &str) {
            self.0.lock().unwrap().1 = Some(m.into());
        }
        fn effort(&self, e: &str) {
            self.0.lock().unwrap().2 = Some(e.into());
        }
    }
    let seen = Seen::default();
    // Ask is set explicitly too, so what the adapter remembered never stays
    let mode = Some(
        inner
            .cfg
            .acp
            .mode_choice(&harness, row.open.policy.permissions),
    );
    let meta =
        inner
            .cfg
            .acp
            .session_meta_with_mode(&harness, false, Some(row.open.policy.permissions));
    let mcp_servers = crate::mcp_servers(inner, session);
    let acp_system_prompt = match (system_context, adapter_props.append_system_context.as_ref()) {
        (Some(ctx), Some(steve_harness_spec::AppendSystemContext::AcpSystemPrompt)) => {
            Some(acp_session::SystemPrompt {
                text: ctx,
                mode: acp_session::SystemPromptMode::AcpAppend,
            })
        }
        (Some(ctx), Some(steve_harness_spec::AppendSystemContext::MetaAppend { namespace })) => {
            Some(acp_session::SystemPrompt {
                text: ctx,
                mode: acp_session::SystemPromptMode::MetaAppend(namespace.as_str()),
            })
        }
        _ => None,
    };
    let handshake = acp_session::open(
        &conn,
        OpenConfig {
            harness: &harness,
            client_version: &inner.cfg.acp.version,
            cwd: &cwd,
            resume: row.acp_id.as_deref(),
            mcp_servers,
            mode,
            model: row.open.choice.model.as_deref(),
            effort: row.open.choice.effort.as_deref(),
            system_prompt: acp_system_prompt,
        },
        meta,
        &seen,
    );
    let opened = tokio::select! {
        _ = cancelled.changed() => Err(RpcError {message: "startup cancelled".into(), data: None}),
        result = handshake => result,
    };
    let opened = match opened {
        Ok(o) => o,
        Err(e) => {
            if !procs::stop(pgid, started).await.gone() {
                return Err(Error::new(
                    Code::OutcomeUnknown,
                    "failed handshake did not stop",
                ));
            }
            inner.j.tx(|t| t.drop_proc(session, pgid))?;
            return Err(Error::new(Code::Unsupported, e.message));
        }
    };
    session_routes.lock().unwrap().root = Some(opened.acp_id.clone());
    let (new_id, model, effort) = seen.0.into_inner().unwrap();
    // Tasks return candidates only. The actor commits them against its operation/attempt fence.
    let mut view = row.view;
    view.effective = row.open.choice;
    if model.is_some() {
        view.effective.model = model;
    }
    if effort.is_some() {
        view.effective.effort = effort;
    }
    view.effective_permissions = row.open.policy.permissions;
    view.resume = new_id
        .or(Some(opened.acp_id.clone()))
        .map(ResumeHandle::new)
        .transpose()
        .map_err(internal)?;
    if conn.has_exited() {
        if !procs::stop(pgid, started).await.gone() {
            return Err(Error::new(
                Code::OutcomeUnknown,
                "exited adapter group not confirmed stopped",
            ));
        }
        inner.j.tx(|t| t.drop_proc(session, pgid))?;
        return Err(NodeStop::AdapterExited.error());
    }
    let cumulative_prompt_tokens = *seen_tokens.lock().unwrap();
    let system_context_delivered = cli_system_context_applied || opened.system_prompt;
    let pending_system_context = system_context
        .filter(|_| !system_context_delivered && row.acp_id.is_none())
        .map(str::to_owned);
    Ok(LiveNew {
        conn,
        retire,
        generation,
        acp_id: opened.acp_id,
        routes: session_routes,
        usage_home,
        image: opened.image,
        pgid,
        started,
        view,
        cumulative_prompt_tokens,
        pending_system_context,
    })
}

fn prepend_user_context(prompt: &mut Value, ctx: &str) {
    let header = format!("## User Context\n\n{ctx}");
    let Some(arr) = prompt.as_array_mut() else {
        return;
    };
    if let Some(first) = arr.first_mut()
        && first.get("type").and_then(Value::as_str) == Some("text")
        && let Some(existing) = first.get("text").and_then(Value::as_str)
    {
        first["text"] = Value::String(format!("{header}\n\n{existing}"));
    } else {
        arr.insert(0, json!({ "type": "text", "text": header }));
    }
}

fn op_changed(op: &OperationId, state: Observed) -> (Subject, EventBody) {
    (
        Subject::Operation {
            operation: op.clone(),
        },
        EventBody::OperationChanged {
            receipt: Receipt {
                operation: op.clone(),
                state,
            },
        },
    )
}

fn done_state(result: OperationResult) -> Observed {
    Observed::Finished {
        outcome: Outcome::Succeeded,
        result: Some(result),
        error: None,
    }
}

fn failed_state(e: Error) -> Observed {
    Observed::Finished {
        outcome: Outcome::Failed,
        result: None,
        error: Some(e),
    }
}

fn cancelled() -> Completion {
    Completion {
        outcome: Outcome::Cancelled,
        stop_reason: "cancelled".into(),
        error: None,
        result: vec![],
        continuation: None,
        transient: false,
        acted: false,
        job: None,
    }
}

fn failure(outcome: Outcome, e: Error, acted: bool) -> Completion {
    Completion {
        outcome,
        stop_reason: "error".into(),
        transient: matches!(e.retry, Retry::Later { .. }),
        acted,
        error: Some(e),
        result: vec![],
        continuation: None,
        job: None,
    }
}

#[cfg(test)]
mod state_import_tests {
    use super::*;

    #[test]
    fn valid_rel_path_rejects_traversal_and_absolute() {
        assert!(valid_rel_path("settings.json").is_ok());
        assert!(valid_rel_path("settings/cli.json").is_ok());
        assert!(valid_rel_path(".gemini").is_ok());
        assert!(valid_rel_path("../secret.json").is_err());
        assert!(valid_rel_path("a/../b.json").is_err());
        assert!(valid_rel_path("/etc/passwd").is_err());
        assert!(valid_rel_path("./settings.json").is_err());
        assert!(valid_rel_path("").is_err());
    }

    #[cfg(unix)]
    #[test]
    fn read_owner_regular_file_refuses_symlinks() {
        let t = tempfile::tempdir().unwrap();
        let own = t.path().join("own");
        std::fs::create_dir_all(&own).unwrap();
        let outside = t.path().join("outside.json");
        std::fs::write(&outside, "{}").unwrap();
        std::os::unix::fs::symlink(&outside, own.join("settings.json")).unwrap();
        let segs = valid_rel_path("settings.json").unwrap();
        assert!(read_owner_regular_file(&own, &segs).is_none());

        std::fs::write(own.join("real.json"), "{\"ok\":true}").unwrap();
        let real_segs = valid_rel_path("real.json").unwrap();
        assert!(read_owner_regular_file(&own, &real_segs).is_some());
    }
}

#[cfg(test)]
mod journal_regressions {
    use super::*;
    use crate::{LocalNode, NodeConfig, PolicyCeiling, Storage};
    use std::sync::atomic::AtomicU64;

    fn actor(dir: &std::path::Path) -> Actor {
        let node = LocalNode::open(NodeConfig {
            node: NodeId::new("hub").unwrap(),
            home: dir.join("node"),
            acp: steve_acp::AcpConfig {
                declared: Default::default(),
                adapters: Default::default(),
                node: "node".into(),
                claude_executable: "claude".into(),
                claude_config_dir: None,
                steve_home: dir.join("steve"),
                codex_home: dir.join("codex"),
                user_home: dir.into(),
                version: "test".into(),
            },
            roots: vec![],
            capacity: Capacity {
                sessions: 1,
                jobs: 0,
                execs: 0,
            },
            storage: Storage::default(),
            ceiling: PolicyCeiling::unlimited(),
        })
        .unwrap();
        let (tx, rx) = mpsc::unbounded_channel();
        let (data_tx, data_rx) = mpsc::channel(64);
        let mut actor = Actor {
            inner: node.0,
            session: SessionId::new("session").unwrap(),
            rx,
            tx,
            data_tx,
            data_rx,
            gate: Arc::default(),
            live: None,
            run: None,
            opening: None,
            waits: HashMap::new(),
            tokens: 0,
            retry_armed: false,
            children: tokio::task::JoinSet::new(),
            pending: vec![],
            sending: 0,
            deferred_prompt: None,
            stage_cancel: None,
            stage_task: None,
        };
        let mut spec = steve_node_api::testkit::build::turn("session", "turn", "attempt", "hello");
        spec.run.limits.deadline = Timestamp(now() + 60_000);
        spec.run.limits.offline_until = Timestamp(now() + 60_000);
        let exec = ExecutionRef::Turn {
            session: spec.session.clone(),
            turn: spec.turn.clone(),
            attempt: spec.run.attempt.clone(),
        };
        actor.run = Some(Run {
            token: 1,
            budget_epoch: actor.inner.j.budget_epoch(),
            exec,
            op: OperationId::new("op").unwrap(),
            spec: Box::new(spec),
            norm: Normalizer::new(dir, "attempt", Epoch::new("meter").unwrap()),
            phase: Phase::Starting,
            acted: false,
            cancel: false,
            stopped: None,
            ending: None,
            closing: None,
            final_usage: vec![],
            usage_stop: None,
            usage_task: None,
            usage_pending: false,
            unknown: None,
            images: crate::inline_images::Images::for_replies(),
            image_scope: crate::inline_images::Scope::new(dir.into(), vec![dir.into()], vec![]),
        });
        actor
    }

    #[tokio::test]
    async fn usage_failure_retries_without_stopping_the_attempt() {
        let dir = tempfile::tempdir().unwrap();
        let mut actor = actor(dir.path());
        let db = rusqlite::Connection::open(actor.inner.cfg.home.join("node.sqlite3")).unwrap();
        db.execute_batch("CREATE TRIGGER fail_usage BEFORE INSERT ON events BEGIN SELECT RAISE(ABORT, 'usage failure'); END;").unwrap();
        let body: EventBody = serde_json::from_value(json!({"type":"usage", "report":{
            "report_id":"final", "meter_epoch":"meter", "scope":"main", "measurement":"final", "tokens":{"input":3}
        }})).unwrap();
        actor
            .handle(Msg::Usage {
                token: 1,
                facts: vec![Fact::Event(body.clone())],
            })
            .await;
        assert!(actor.run.as_ref().unwrap().stopped.is_none());
        assert!(!actor.inner.faulted());
        assert_eq!(actor.pending.len(), 1);
        db.execute_batch("DROP TRIGGER fail_usage").unwrap();
        let retry = actor.pending.pop().unwrap();
        actor.run = None; // Settlement must survive the attempt/token leaving the actor.
        actor.handle(retry).await;
        assert!(actor.pending.is_empty());
        let events = actor
            .inner
            .j
            .read(|t| t.unacked_past(&HashMap::new(), 1024))
            .unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].body, body);
    }

    #[tokio::test]
    async fn check_limits_finishes_while_connection_is_locked_without_sampling_space() {
        let dir = tempfile::tempdir().unwrap();
        let mut actor = actor(dir.path());
        let samples = Arc::new(AtomicU64::new(0));
        let counter = samples.clone();
        let mut storage = Storage::default();
        storage.free_space = Some(Arc::new(move || {
            counter.fetch_add(1, Ordering::SeqCst);
            u64::MAX
        }));
        actor
            .inner
            .j
            .configure(storage, actor.inner.wake.clone())
            .unwrap();
        let inner = actor.inner.clone();
        let (entered_tx, entered_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let hold = std::thread::spawn(move || {
            inner.j.read(move |_| {
                entered_tx.send(()).unwrap();
                release_rx.recv_timeout(Duration::from_secs(3)).unwrap();
                Ok(())
            })
        });
        entered_rx.recv_timeout(Duration::from_secs(3)).unwrap();
        let before = samples.load(Ordering::SeqCst);
        actor.check_limits();
        assert_eq!(samples.load(Ordering::SeqCst), before);
        assert!(actor.run.as_ref().unwrap().stopped.is_none());
        release_tx.send(()).unwrap();
        hold.join().unwrap().unwrap();
    }
}
