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

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use steve_acp::normalize::{Fact, Normalizer};
use steve_acp::permission::{self, Asked};
use steve_acp::rpc::{Conn, Incoming, RpcError, Spawn};
use steve_acp::session::{self as acp_session, OpenConfig, Reports, Strictness};
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
    },
}

/// Why an attempt's harness is stopped at its ingress. Being behind is not one of them: the reader
/// waits for room on the data queue ([`enqueue`]), so the harness blocks on its stdout instead.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ingress {
    /// One message over the 1 MiB frame cap.
    Frame,
    /// More bytes than the attempt's output budget.
    Output,
    /// The Node took none of its messages for [`crate::Storage::ingress_stall`].
    Stalled,
}

impl Ingress {
    fn stop(self, stall: Duration) -> (Outcome, ApiError) {
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

#[derive(Clone)]
pub(crate) struct LiveNew {
    conn: Arc<Conn>,
    retire: Arc<watch::Sender<bool>>,
    generation: u64,
    acp_id: String,
    image: bool,
    pgid: i64,
    started: Option<u64>,
    view: SessionView,
}

struct Live {
    conn: Arc<Conn>,
    /// Ends a wait of its reader on a full data queue once this harness is not the session's.
    retire: Arc<watch::Sender<bool>>,
    generation: u64,
    acp_id: String,
    image: bool,
    pgid: i64,
    started: Option<u64>,
}

impl Drop for Live {
    fn drop(&mut self) {
        self.retire.send_replace(true);
    }
}

impl From<LiveNew> for Live {
    fn from(l: LiveNew) -> Live {
        Live {
            conn: l.conn,
            retire: l.retire,
            generation: l.generation,
            acp_id: l.acp_id,
            image: l.image,
            pgid: l.pgid,
            started: l.started,
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

struct Run {
    token: u64,
    exec: ExecutionRef,
    op: OperationId,
    spec: Box<TurnSpec>,
    norm: Normalizer,
    phase: Phase,
    acted: bool,
    cancel: bool,
    /// Why the Node itself stopped it.
    stopped: Option<(Outcome, ApiError)>,
    /// The end, computed and waiting to be committed (kept while the journal fails).
    ending: Option<Completion>,
    closing: Option<Vec<EventBody>>,
    final_usage: Vec<(Subject, EventBody)>,
    unknown: Option<String>,
}

/// What a harness start is for.
enum Opening {
    /// The open/configure command.
    Command(OperationId),
    /// The attempt with this token.
    Attempt(u64),
}

/// How long text deltas of one message block may be held back to go out as one event, and how
/// much text at most. The Hub stores, sends and renders one event where the harness sent many
/// small chunks; the final text is the same bytes (increments that have no sequence number yet may
/// be merged in memory).
const HOLD_FOR: Duration = Duration::from_millis(50);
const HOLD_BYTES: usize = 4096;

/// Text deltas held back: contiguous text of one block, from `utf8_offset` on.
struct Held {
    subject: Subject,
    message: String,
    block: String,
    utf8_offset: u64,
    text: String,
    since: tokio::time::Instant,
}

impl Held {
    fn event(self) -> (Subject, EventBody) {
        (
            self.subject,
            EventBody::TextDelta {
                message: self.message,
                block: self.block,
                utf8_offset: self.utf8_offset,
                text: self.text,
            },
        )
    }
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
    pending: Vec<Msg>,
    sending: usize,
    deferred_prompt: Option<Result<Value, RpcError>>,
    stage_cancel: Option<watch::Sender<bool>>,
    stage_task: Option<tokio::task::JoinHandle<()>>,
    /// Text not journaled yet ([`HOLD_FOR`]): it goes before anything else the actor journals.
    held: std::sync::Mutex<Option<Held>>,
    /// Held text the journal refused, so the attempt cannot go on as if it were said.
    held_lost: std::sync::Mutex<Option<ApiError>>,
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
            pending: vec![],
            sending: 0,
            deferred_prompt: None,
            stage_cancel: None,
            stage_task: None,
            held: std::sync::Mutex::new(None),
            held_lost: std::sync::Mutex::new(None),
        };
        tokio::spawn(a.run_loop());
        tx
    }

    async fn run_loop(mut self) {
        let mut tick = tokio::time::interval(Duration::from_millis(500));
        loop {
            let due = self
                .held
                .lock()
                .unwrap()
                .as_ref()
                .map(|h| h.since + HOLD_FOR);
            tokio::select! {
                biased;
                m = self.rx.recv() => {
                    let Some(m) = m else {
                        let _ = self.flush_held();
                        return;
                    };
                    if let Msg::Shutdown { done } = m {
                        let _ = self.flush_held();
                        if let Some(cancel) = self.stage_cancel.take() { let _ = cancel.send(true); }
                        if let Some(task) = self.stage_task.take() { let _ = task.await; }
                        self.stop_process().await;
                        drop(self);
                        let _ = done.send(());
                        return;
                    }
                    self.handle(m).await;
                }
                // before the adapter's data: a flood never keeps held text back
                _ = tokio::time::sleep_until(due.unwrap_or_else(tokio::time::Instant::now)), if due.is_some() => {
                    let _ = self.flush_held();
                }
                _ = tick.tick() => self.check_limits(),
                Some(message) = self.data_rx.recv() => self.handle(message).await,
            }
            self.held_refused();
        }
    }

    /// Journals in one transaction, after the text held so far (in a transaction of its own).
    /// Held text the journal cannot take fails this too: nothing said later goes before it.
    fn tx<R>(&self, f: impl FnOnce(&Tx) -> ApiResult<R>) -> ApiResult<R> {
        self.flush_held()?;
        self.tx_now(f)
    }

    /// Journals the held text now, if any. Refused for its budget it is dropped, as an update the
    /// journal refuses is; otherwise it is kept for the next try and the error returned. Either
    /// way the attempt stops ([`Self::held_refused`]).
    fn flush_held(&self) -> ApiResult<()> {
        let Some(h) = self.held.lock().unwrap().take() else {
            return Ok(());
        };
        let ev = h.event();
        let Err(e) = self.tx_now(|t| crate::emit_all(&self.inner, t, vec![ev.clone()])) else {
            return Ok(());
        };
        self.held_lost.lock().unwrap().get_or_insert(e.clone());
        if e.code == ErrorCode::Exhausted {
            return Ok(());
        }
        let (
            subject,
            EventBody::TextDelta {
                message,
                block,
                utf8_offset,
                text,
            },
        ) = ev
        else {
            unreachable!("held text is a text delta")
        };
        *self.held.lock().unwrap() = Some(Held {
            subject,
            message,
            block,
            utf8_offset,
            text,
            // tried again a second from now (or before the next journal write)
            since: tokio::time::Instant::now() + Duration::from_secs(1),
        });
        Err(e)
    }

    /// What the attempt does once its held text could not be journaled: as for an update.
    fn held_refused(&mut self) {
        let Some(e) = self.held_lost.lock().unwrap().take() else {
            return;
        };
        let Some(token) = self.run.as_ref().map(|r| r.token) else {
            return;
        };
        if e.code == ErrorCode::Exhausted {
            self.stopping(token, Outcome::Cancelled, NodeStop::JournalBudget.error());
            let _ = self.tx.send(Msg::Kill { token });
        } else {
            self.stopping(token, Outcome::Failed, NodeStop::NotRecorded.error());
        }
    }

    /// Holds back the text deltas of `evs` that can wait ([`HOLD_FOR`]); what is to be journaled
    /// now comes back, in order, held text first where it was said first.
    fn hold(&self, evs: Vec<(Subject, EventBody)>) -> Vec<(Subject, EventBody)> {
        let mut held = self.held.lock().unwrap();
        let mut out = vec![];
        for (subject, body) in evs {
            match body {
                EventBody::TextDelta {
                    message,
                    block,
                    utf8_offset,
                    text,
                } => {
                    if let Some(h) = held.as_mut()
                        && h.subject == subject
                        && h.message == message
                        && h.block == block
                        && h.utf8_offset + h.text.len() as u64 == utf8_offset
                        && h.text.len() + text.len() <= HOLD_BYTES
                    {
                        h.text.push_str(&text);
                        continue;
                    }
                    out.extend(held.take().map(Held::event));
                    *held = Some(Held {
                        subject,
                        message,
                        block,
                        utf8_offset,
                        text,
                        since: tokio::time::Instant::now(),
                    });
                }
                body => {
                    out.extend(held.take().map(Held::event));
                    out.push((subject, body));
                }
            }
        }
        if held.as_ref().is_some_and(|h| h.text.len() >= HOLD_BYTES) {
            out.extend(held.take().map(Held::event));
        }
        out
    }

    /// Journals in one transaction as it is (what is held stays held).
    fn tx_now<R>(&self, f: impl FnOnce(&Tx) -> ApiResult<R>) -> ApiResult<R> {
        let r = self.inner.j.tx(f);
        match &r {
            Ok(_) => self.inner.wake.notify_one(),
            Err(e) => {
                tracing::error!("node journal ({}): {e}", self.session);
                if e.code == ErrorCode::Internal {
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
        tokio::spawn(async move {
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
                            update,
                        } => self.update(generation, token, &update),
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
                update,
            } => self.update(generation, token, &update),
            Msg::Permission {
                generation,
                token,
                rpc_id,
                params,
            } => self.permission(generation, token, rpc_id, &params),
            Msg::Exited { generation } => self.exited(generation).await,
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
        let (pgid, started) = match self.live.take() {
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

    async fn exited(&mut self, generation: u64) {
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
                        return Err(ApiError::new(ErrorCode::Stale, "obsolete open result"));
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
                            if e.code == ErrorCode::OutcomeUnknown {
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
                                .ok_or_else(|| ApiError::new(ErrorCode::Stale, "session gone"))?;
                            if row.view.active.as_ref() != Some(&exec) {
                                return Err(ApiError::new(
                                    ErrorCode::Stale,
                                    "obsolete start result",
                                ));
                            }
                            row.acp_id = Some(l.acp_id.clone());
                            row.view.effective = l.view.effective.clone();
                            row.view.effective_permissions = l.view.effective_permissions;
                            row.view.resume = l.view.resume.clone();
                            row.view.revision = Revision::new(t.revision()?).map_err(internal)?;
                            t.put_session(&row)
                        });
                        self.live = Some(l.into());
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
                        if e.code == ErrorCode::OutcomeUnknown {
                            return self.unknown_end(&e.safe_detail);
                        }
                        let outcome = if e.code == ErrorCode::Offline {
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
        let meter = Epoch::new(derived_id("m", &format!("{}.{token}", spec.run.attempt)))
            .unwrap_or_else(|_| Epoch::new("m").expect("valid"));
        self.run = Some(Run {
            token,
            exec,
            op,
            norm: Normalizer::new(root, spec.run.attempt.as_str(), meter),
            spec,
            phase: Phase::Starting,
            acted: false,
            cancel: false,
            stopped: None,
            ending: None,
            closing: None,
            final_usage: vec![],
            unknown: None,
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
                _ = cancelled.changed() => Err(ApiError::new(ErrorCode::Expired, "preparation cancelled")),
                result = tokio::time::timeout(left, async {
                    let root = tokio::task::spawn_blocking(move || context?.dir()).await.map_err(internal)??;
                    crate::prompt(&inner, &exec, &root, &parts, image).await
                }) => {
                    result.unwrap_or_else(|_| Err(ApiError::new(ErrorCode::Expired, "past deadline preparing attachments")))
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
        let prompt = match result {
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
        let Some(l) = self.live.as_ref() else {
            return self.end(failure(
                Outcome::Interrupted,
                NodeStop::HarnessGone.error(),
                false,
            ));
        };
        let (conn, acp_id) = (l.conn.clone(), l.acp_id.clone());
        if let Some(r) = self.run.as_mut() {
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
                .max_output_bytes
                .min(self.inner.cfg.storage.quota / 4),
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
        tokio::spawn(async move {
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

    fn update(&mut self, generation: u64, token: u64, u: &Value) {
        let session = self.session.clone();
        let mut evs: Vec<(Subject, EventBody)> = vec![];
        let mut config: Vec<Fact> = vec![];
        if self.attributed(generation, token) {
            let run = self.run.as_mut().expect("checked");
            for f in run.norm.update(u) {
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
                    Fact::Event(e) => {
                        if matches!(e, EventBody::ToolChanged { .. }) {
                            run.acted = true;
                        }
                        evs.push((Subject::Execution(run.exec.clone()), e));
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
        let mut evs = self.hold(evs);
        if evs.is_empty() && config.is_empty() {
            return;
        }
        // what was held came out in `evs` where it belongs; what is held now was said after them
        let r = self.tx_now(|t| {
            // cost reports are numbered on the process's meter (its running total)
            for (_, b) in evs.iter_mut() {
                if let EventBody::Usage { report } = b
                    && report.measurement == Measurement::Cumulative
                    && let Some((meter, n)) = t.next_report(&session)?
                {
                    report.report_id = derived_id("u", &format!("{meter}.{n}"));
                    report.meter_epoch = Epoch::new(derived_id("m", &meter)).map_err(internal)?;
                }
            }
            if !config.is_empty()
                && let Some(mut row) = t.session(&session)?
            {
                for f in &config {
                    match f {
                        Fact::Model(m) => row.view.effective.model = Some(m.clone()),
                        Fact::Effort(e) => row.view.effective.effort = Some(e.clone()),
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
        });
        if r.as_ref().is_err_and(|e| e.code == ErrorCode::Exhausted) {
            if let Some(token) = self.run.as_ref().map(|r| r.token) {
                self.stopping(token, Outcome::Cancelled, NodeStop::JournalBudget.error());
                let _ = self.tx.send(Msg::Kill { token });
            }
            return;
        }
        if r.is_err() && !evs.is_empty() {
            // tried again in order: what is held now was said after them
            evs.extend(self.held.lock().unwrap().take().map(Held::event));
            self.later(Msg::Facts { events: evs });
        }
        if r.is_err()
            && let Some(token) = self.run.as_ref().map(|r| r.token)
        {
            // what the harness said is lost: the attempt cannot go on as if it were not
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
        tokio::spawn(async move {
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
        // the entry to Steve's own tools: the Hub authorizes each call itself (HubTools)
        if asked.steve_tool.is_some() {
            if !evs.is_empty() {
                let _ = self.tx(|t| crate::emit_all(&self.inner, t, evs));
            }
            conn.respond(
                rpc_id,
                permission::answer(&asked, Some(PermissionDecision::AllowOnce)),
            );
            return;
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
                Err(ApiError::new(
                    ErrorCode::Stale,
                    "nobody waits for this answer",
                )),
            );
        };
        let context = crate::permission_context(&self.inner, &self.session);
        let tx = self.tx.clone();
        let pre = req.preconditions.clone();
        let check = res.decision == PermissionDecision::AllowOnce;
        tokio::spawn(async move {
            let result = if check {
                match tokio::time::timeout(
                    Duration::from_secs(10),
                    tokio::task::spawn_blocking(move || crate::preconditions_hold(&context?, &pre)),
                )
                .await
                {
                    Ok(Ok(r)) => r,
                    _ => Err(ApiError::new(
                        ErrorCode::Stale,
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
                Err(ApiError::new(
                    ErrorCode::Stale,
                    "nobody waits for this answer",
                )),
            );
        };
        let live = self.run.as_ref().is_some_and(|r| {
            r.op == attempt_op && !r.cancel && r.phase == Phase::Prompting && r.ending.is_none()
        });
        if !live || req.expires_at.0 <= now() {
            return self.answer_done(
                &op,
                Err(ApiError::new(ErrorCode::Stale, "that attempt is over")),
            );
        }
        let Some((rpc_id, asked)) = self.waits.get(&res.id).cloned() else {
            return self.answer_done(
                &op,
                Err(ApiError::new(
                    ErrorCode::Stale,
                    "nobody waits for this answer",
                )),
            );
        };
        let Some(conn) = self.live.as_ref().map(|l| l.conn.clone()) else {
            return self.answer_done(
                &op,
                Err(ApiError::new(ErrorCode::Stale, "the harness is gone")),
            );
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
                tokio::spawn(async move {
                    let result = match sent {
                        Ok(rx) => match tokio::time::timeout(Duration::from_secs(10), rx).await {
                            Ok(Ok(Ok(()))) => Ok(()),
                            _ => Err(ApiError::new(
                                ErrorCode::OutcomeUnknown,
                                "permission write not confirmed",
                            )),
                        },
                        Err(e) => Err(ApiError::new(ErrorCode::OutcomeUnknown, e.message)),
                    };
                    let _ = tx.send(Msg::AnswerSent {
                        op,
                        req: Box::new(req),
                        res,
                        result,
                    });
                });
            }
            Err(e) if e.code == ErrorCode::Stale => self.answer_done(&op, Err(e)),
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
                let usage = final_usage(run, &resp);
                if let Some(run) = self.run.as_mut() {
                    run.final_usage = usage;
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
                match stopped {
                    Some((outcome, err)) => failure(outcome, err, acted),
                    None => Completion {
                        outcome: if gone {
                            Outcome::Interrupted
                        } else {
                            Outcome::Failed
                        },
                        stop_reason: "error".into(),
                        transient: steve_acp::is_transient(&e.message),
                        acted,
                        error: Some(ApiError::new(ErrorCode::Internal, e.message)),
                        result: vec![],
                        continuation: None,
                        job: None,
                    },
                }
            }
        };
        self.end(completion);
    }

    /// Ends the running attempt: its waits expire, its last events, its final state,
    /// `attempt_ended`, and the session free again, in one transaction; only then is it gone here.
    fn end(&mut self, completion: Completion) {
        let Some(run) = self.run.as_mut() else { return };
        if run.ending.is_none() {
            run.ending = Some(completion);
        }
        self.commit_end();
    }

    fn commit_end(&mut self) {
        if self.sending != 0 || !self.pending.is_empty() {
            self.arm_retry();
            return;
        }
        let Some(run) = self.run.as_mut() else { return };
        let Some(completion) = run.ending.clone() else {
            return;
        };
        let closing = run
            .closing
            .get_or_insert_with(|| {
                run.norm
                    .finish()
                    .into_iter()
                    .filter_map(|f| match f {
                        Fact::Event(e) => Some(e),
                        _ => None,
                    })
                    .collect()
            })
            .clone();
        let usage = run.final_usage.clone();
        let (exec, op) = (run.exec.clone(), run.op.clone());
        let session = self.session.clone();
        let r = self.tx(|t| {
            let mut evs = usage.clone();
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
    fn stopping(&mut self, token: u64, outcome: Outcome, error: ApiError) {
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
        tokio::spawn(async move {
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
        } else if self
            .inner
            .j
            .read(|t| self.inner.over_limit(t))
            .unwrap_or(false)
        {
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
fn harness_state(
    inner: &Inner,
    s: &HarnessState,
    sd: steve_acp::StateDir,
) -> ApiResult<std::path::PathBuf> {
    use std::os::unix::fs::DirBuilderExt;
    if !s.valid() {
        return Err(ApiError::new(
            ErrorCode::Invalid,
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
        match std::fs::symlink_metadata(&dir) {
            Ok(m) if m.file_type().is_dir() => {}
            Ok(_) => return Err(internal("harness state: not a directory")),
            Err(_) => std::fs::DirBuilder::new()
                .mode(0o700)
                .create(&dir)
                .map_err(|e| internal(format!("harness state: {e}")))?,
        }
    }
    if s.import_owner {
        let own = std::env::var_os(sd.env)
            .filter(|v| !v.is_empty())
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|| inner.cfg.acp.user_home.join(sd.default));
        for f in sd.import {
            let (from, to) = (own.join(f), dir.join(f));
            if !from.is_file() || std::fs::symlink_metadata(&to).is_ok() {
                continue;
            }
            // a new file of its own: never through a link left there
            let copied = std::fs::read(&from).and_then(|bytes| {
                use std::io::Write as _;
                use std::os::unix::fs::OpenOptionsExt as _;
                let mut out = std::fs::OpenOptions::new()
                    .write(true)
                    .create_new(true)
                    .mode(0o600)
                    .open(&to)?;
                out.write_all(&bytes)
            });
            copied.map_err(|e| internal(format!("harness state: {f}: {e}")))?;
        }
    }
    Ok(dir)
}

/// The environment a session's harness gets over the Node's: the principal's own variables
/// (`node_env`; a key the Node refuses is dropped, `PATH_PREPEND` goes in front of `PATH`), then
/// what Steve sets itself.
fn harness_env(
    user: &[(String, String)],
    claude_executable: std::ffi::OsString,
) -> Vec<(std::ffi::OsString, std::ffi::OsString)> {
    use steve_node_api::env::{PATH_PREPEND, user_env_refusal};
    let mut env: Vec<(std::ffi::OsString, std::ffi::OsString)> = Vec::new();
    for (k, v) in user {
        if let Some(why) = user_env_refusal(k) {
            tracing::warn!("node: {k} is not set for the harness: {why}");
            continue;
        }
        if k == PATH_PREPEND {
            let path = std::env::var_os("PATH").unwrap_or_default();
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
    env.push(("CLAUDE_CODE_EXECUTABLE".into(), claude_executable));
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
        .ok_or_else(|| ApiError::new(ErrorCode::Invalid, "no such session"))?;
    let cwd = crate::workspace_dir(inner, &row.open.workspace)?;
    let cwd = cwd.to_string_lossy().to_string();
    let harness = row.open.choice.harness.clone();
    let (program, args) = inner
        .cfg
        .acp
        .adapter_command(&harness)
        .map_err(|e| ApiError::new(ErrorCode::Unsupported, e))?;
    // the principal's own harness directory, when the Hub asks for one and the adapter can move it
    let state = match (
        &row.open.state,
        inner.cfg.acp.adapter_properties(&harness).state,
    ) {
        (Some(s), Some(sd)) => Some((sd.env, harness_state(inner, s, sd)?)),
        _ => None,
    };
    // in the OS sandbox, unless the harness sandboxes its own shell (macOS nests none)
    let (program, args) = if inner.cfg.acp.adapter_properties(&harness).nested_sandbox {
        inner.confine.wrap_in(
            program,
            args,
            &[],
            &state.iter().map(|(_, d)| d.clone()).collect::<Vec<_>>(),
            Some(std::path::Path::new(&cwd)),
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
    let conn = Conn::spawn_registered(
        Spawn {
            program,
            args,
            cwd: cwd.clone().into(),
            env: {
                let mut env = harness_env(
                    &row.open.env,
                    inner.cfg.acp.claude_executable.clone().into_os_string(),
                );
                if let Some((var, dir)) = &state {
                    env.push(((*var).into(), dir.clone().into_os_string()));
                }
                // its toolchains' outputs and caches in the item's and the person's own places
                env.extend(crate::toolchain::env(
                    &inner.cfg.home,
                    std::path::Path::new(&cwd),
                    row.open.state.as_ref(),
                ));
                env
            },
            log_tag: format!("[node {session}]"),
        },
        1 << 20,
        move |bytes| {
            let token = admission_gate.0.load(Ordering::SeqCst);
            if token == 0 {
                let total = counter
                    .fetch_add(bytes as u64, Ordering::SeqCst)
                    .saturating_add(bytes as u64);
                return bytes <= 1 << 20 && total <= 8 << 20;
            }
            let total = counter
                .fetch_add(bytes as u64, Ordering::SeqCst)
                .saturating_add(bytes as u64);
            let why = if bytes > 1 << 20 {
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
                    // counted as it arrives (admission): a flood is stopped before it is all kept
                    let u = params.get("update")?;
                    let update = Msg::Update {
                        generation,
                        token,
                        update: u.clone(),
                    };
                    return enqueue(&data_tx, update, token, &ctl);
                }
                Incoming::Notification { .. } => {}
                Incoming::Request { id, method, params }
                    if method == "session/request_permission" =>
                {
                    if token == 0 {
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
        move || {
            let _ = tx2.send(Msg::Exited { generation });
        },
    )
    .map_err(|e| ApiError::new(ErrorCode::Unsupported, e.message))?;
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
    let mode = crate::mode(&harness, row.open.policy.permissions);
    let meta = crate::harness_meta(&harness, &inner.cfg.acp.steve_home);
    let handshake = acp_session::open_with_meta(
        &conn,
        OpenConfig {
            client_version: &inner.cfg.acp.version,
            cwd: &cwd,
            resume: row.acp_id.as_deref(),
            mcp_servers: crate::mcp_servers(inner, session),
            mode,
            model: row.open.choice.model.as_deref(),
            effort: row.open.choice.effort.as_deref(),
        },
        meta,
        &seen,
        Strictness::Strict,
    );
    let opened = tokio::select! {
        _ = cancelled.changed() => Err(RpcError {message: "startup cancelled".into(), data: None}),
        result = handshake => result,
    };
    let opened = match opened {
        Ok(o) => o,
        Err(e) => {
            if !procs::stop(pgid, started).await.gone() {
                return Err(ApiError::new(
                    ErrorCode::OutcomeUnknown,
                    "failed handshake did not stop",
                ));
            }
            inner.j.tx(|t| t.drop_proc(session, pgid))?;
            return Err(ApiError::new(ErrorCode::Unsupported, e.message));
        }
    };
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
            return Err(ApiError::new(
                ErrorCode::OutcomeUnknown,
                "exited adapter group not confirmed stopped",
            ));
        }
        inner.j.tx(|t| t.drop_proc(session, pgid))?;
        return Err(NodeStop::AdapterExited.error());
    }
    Ok(LiveNew {
        conn,
        retire,
        generation,
        acp_id: opened.acp_id,
        image: opened.image,
        pgid,
        started,
        view,
    })
}

/// The token figures of the prompt's answer: the main loop's, and per model everything.
fn final_usage(run: &Run, resp: &Value) -> Vec<(Subject, EventBody)> {
    let exec = run.exec.clone();
    let attempt = exec.attempt().clone();
    let tok = |u: &Value| Tokens {
        input: u.get("inputTokens").and_then(Value::as_u64),
        output: u.get("outputTokens").and_then(Value::as_u64),
        cache_read: u
            .get("cachedReadTokens")
            .or_else(|| u.get("cachedInputTokens"))
            .and_then(Value::as_u64),
        cache_write: u.get("cachedWriteTokens").and_then(Value::as_u64),
        reasoning: None,
    };
    let mut reports = vec![];
    if let Some(u) = resp.get("usage").filter(|u| u.is_object()) {
        reports.push((None, UsageScope::Main, tok(u)));
    }
    if let Some(m) = resp
        .pointer("/_meta/quota/model_usage")
        .and_then(Value::as_array)
    {
        for x in m {
            if let Some(tc) = x.get("token_count") {
                reports.push((
                    x.get("model").and_then(Value::as_str).map(String::from),
                    UsageScope::All,
                    tok(tc),
                ));
            }
        }
    }
    let Ok(meter) = Epoch::new(derived_id("f", attempt.as_str())) else {
        return vec![];
    };
    reports
        .into_iter()
        .enumerate()
        .map(|(i, (model, scope, tokens))| {
            (
                Subject::Execution(exec.clone()),
                EventBody::Usage {
                    report: UsageReport {
                        report_id: derived_id("f", &format!("{attempt}.{i}")),
                        model,
                        meter_epoch: meter.clone(),
                        scope,
                        measurement: Measurement::Final,
                        tokens,
                        context_used: None,
                        context_limit: None,
                        cost: None,
                    },
                },
            )
        })
        .collect()
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

fn failed_state(e: ApiError) -> Observed {
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

fn failure(outcome: Outcome, e: ApiError, acted: bool) -> Completion {
    Completion {
        outcome,
        stop_reason: "error".into(),
        transient: steve_acp::is_transient(&e.safe_detail),
        acted,
        error: Some(e),
        result: vec![],
        continuation: None,
        job: None,
    }
}
