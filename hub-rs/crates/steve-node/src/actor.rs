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
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use steve_acp::normalize::{Fact, Normalizer};
use steve_acp::permission::{self, Asked};
use steve_acp::rpc::{Conn, Incoming, RpcError, Spawn};
use steve_acp::session::{self as acp_session, OpenConfig, Reports, Strictness};
use steve_node_api::*;
use tokio::sync::{mpsc, oneshot};

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
    Checked {
        op: OperationId,
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
    /// Try again what the journal did not take.
    Retry,
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

/// The token of the attempt whose prompt is out, stamped on each adapter message as it is read.
#[derive(Default)]
pub(crate) struct Gate(AtomicU64);

pub(crate) struct LiveNew {
    conn: Arc<Conn>,
    generation: u64,
    acp_id: String,
    image: bool,
    pgid: i64,
    started: Option<u64>,
    view: SessionView,
}

struct Live {
    conn: Arc<Conn>,
    generation: u64,
    acp_id: String,
    image: bool,
    pgid: i64,
    started: Option<u64>,
}

impl From<LiveNew> for Live {
    fn from(l: LiveNew) -> Live {
        Live {
            conn: l.conn,
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
    tx: mpsc::UnboundedSender<Msg>,
    gate: Arc<Gate>,
    live: Option<Live>,
    run: Option<Run>,
    opening: Option<(u64, Opening)>,
    waits: HashMap<PermissionId, (Value, Asked)>,
    tokens: u64,
    retry_armed: bool,
}

impl Actor {
    pub(crate) fn spawn(inner: Arc<Inner>, session: SessionId) -> mpsc::UnboundedSender<Msg> {
        let (tx, rx) = mpsc::unbounded_channel();
        let a = Actor {
            inner,
            session,
            rx,
            tx: tx.clone(),
            gate: Arc::default(),
            live: None,
            run: None,
            opening: None,
            waits: HashMap::new(),
            tokens: 0,
            retry_armed: false,
        };
        tokio::spawn(a.run_loop());
        tx
    }

    async fn run_loop(mut self) {
        let mut tick = tokio::time::interval(Duration::from_millis(500));
        loop {
            tokio::select! {
                m = self.rx.recv() => {
                    let Some(m) = m else { return };
                    if let Msg::Shutdown { done } = m {
                        self.stop_process().await;
                        let _ = done.send(());
                        return;
                    }
                    self.handle(m).await;
                }
                _ = tick.tick() => self.check_limits(),
            }
        }
    }

    fn tx<R>(&self, f: impl FnOnce(&Tx) -> ApiResult<R>) -> ApiResult<R> {
        let r = self.inner.j.tx(f);
        match &r {
            Ok(_) => self.inner.wake.notify_one(),
            Err(e) => {
                tracing::error!("node journal ({}): {e}", self.session);
                self.inner.fault();
            }
        }
        r
    }

    fn next_token(&mut self) -> u64 {
        self.tokens += 1;
        self.tokens
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
            Msg::Open { op } => self.open(op).await,
            Msg::Start { op, spec, exec } => self.start(op, spec, exec).await,
            Msg::Cancel { target } => self.cancel(&target),
            Msg::Answer { op } => self.answer(op),
            Msg::Checked { op, result } => self.answer_checked(op, result),
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
                if self.run.as_ref().is_some_and(|r| r.token == token) {
                    self.finish(result).await;
                }
            }
            Msg::Kill { token } => {
                if self
                    .run
                    .as_ref()
                    .is_some_and(|r| r.token == token && r.ending.is_none())
                {
                    let s = self.stop_process().await;
                    let (outcome, e) =
                        self.run
                            .as_ref()
                            .and_then(|r| r.stopped.clone())
                            .unwrap_or((
                                Outcome::Interrupted,
                                ApiError::new(ErrorCode::Internal, "stopped by the node"),
                            ));
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
        tokio::spawn(async move {
            let result = open_harness(&inner, &session, tx.clone(), gate).await;
            let _ = tx.send(Msg::Opened { token, result });
        });
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
                Ok(None) => return Stopped::Gone,
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
        let _ = self.tx(|t| {
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
                let state = failed_state(ApiError::new(
                    ErrorCode::Busy,
                    "the session's old harness did not stop",
                ));
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
                let view = match result {
                    Ok(l) => {
                        let view = l.view.clone();
                        self.live = Some(l.into());
                        Ok(view)
                    }
                    Err(e) => Err(e),
                };
                let _ = self.tx(|t| {
                    let Some(mut row) = t.session(&session)? else {
                        return Ok(());
                    };
                    let state = match &view {
                        Ok(v) => {
                            row.view = v.clone();
                            row.view.phase = SessionPhase::Ready;
                            done_state(OperationResult::Session(row.view.clone()))
                        }
                        Err(e) => {
                            row.view.phase = SessionPhase::Released;
                            failed_state(e.clone())
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
                match result {
                    Ok(l) => {
                        self.live = Some(l.into());
                        self.prepare(tok);
                    }
                    Err(e) => {
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
            .and_then(|r| crate::workspace_dir(&self.inner, &r.open.workspace).ok())
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
        r.cancel
            || self
                .inner
                .j
                .read(|t| t.op(&r.op))
                .ok()
                .flatten()
                .is_some_and(|o| o.cancel)
    }

    /// Attachments for the prompt, fetched in a task (bounded by the attempt's deadline).
    fn prepare(&mut self, token: u64) {
        let root = self.root();
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
        tokio::spawn(async move {
            let left = Duration::from_millis((deadline.0 - now()).max(0) as u64);
            let result = match tokio::time::timeout(
                left,
                crate::prompt(&inner, &exec, &root, &parts, image),
            )
            .await
            {
                Ok(r) => r,
                Err(_) => Err(ApiError::new(
                    ErrorCode::Expired,
                    "past its deadline while fetching attachments",
                )),
            };
            let _ = tx.send(Msg::Prepared { token, result });
        });
    }

    async fn prepared(&mut self, token: u64, result: ApiResult<Value>) {
        if !self
            .run
            .as_ref()
            .is_some_and(|r| r.token == token && r.phase == Phase::Preparing)
        {
            return;
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
                ApiError::new(ErrorCode::Expired, "past its deadline"),
                false,
            ));
        }
        let Some(l) = self.live.as_ref() else {
            return self.end(failure(
                Outcome::Interrupted,
                ApiError::new(ErrorCode::Offline, "the harness is gone"),
                false,
            ));
        };
        let (conn, acp_id) = (l.conn.clone(), l.acp_id.clone());
        if let Some(r) = self.run.as_mut() {
            r.phase = Phase::Prompting;
        }
        // from here the adapter's messages are this attempt's
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
            // not prompted yet: it ends without a prompt (a start under way is not waited for)
            Phase::Starting | Phase::Preparing => {
                let stopped = run.stopped.clone();
                let acted = run.acted;
                self.end(match stopped {
                    Some((o, e)) => failure(o, e, acted),
                    None => cancelled(),
                });
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
        if evs.is_empty() && config.is_empty() {
            return;
        }
        let r = self.tx(|t| {
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
            crate::emit_all(&self.inner, t, std::mem::take(&mut evs))
        });
        if r.is_err()
            && let Some(token) = self.run.as_ref().map(|r| r.token)
        {
            // what the harness said is lost: the attempt cannot go on as if it were not
            self.stopping(
                token,
                Outcome::Failed,
                ApiError::new(ErrorCode::Internal, "the node could not record the attempt"),
            );
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
        let req = match crate::bind_permission(
            &self.inner,
            &self.session,
            &exec,
            &op,
            &rpc_id,
            &asked,
            deadline,
        ) {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!("permission of {}: {e}", self.session);
                if !evs.is_empty() {
                    let _ = self.tx(|t| crate::emit_all(&self.inner, t, evs));
                }
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
        let inner = self.inner.clone();
        let tx = self.tx.clone();
        let pre = req.preconditions.clone();
        let check = res.decision == PermissionDecision::AllowOnce;
        tokio::spawn(async move {
            let result = if check {
                match tokio::time::timeout(
                    Duration::from_secs(10),
                    tokio::task::spawn_blocking(move || crate::preconditions_hold(&inner, &pre)),
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
        // journal first, then answer: what the harness got is what was recorded
        let r = self.tx(|t| {
            t.drop_wait(&res.id)?;
            t.drop_answer(&op)?;
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
            let state = done_state(OperationResult::Unit);
            t.set_state(&op, &state)?;
            crate::emit_all(
                &self.inner,
                t,
                vec![
                    (
                        Subject::Execution(req.execution.clone()),
                        EventBody::PermissionResolved {
                            resolution: res.clone(),
                        },
                    ),
                    op_changed(&op, state),
                ],
            )
        });
        if r.is_ok() {
            self.waits.remove(&res.id);
            conn.respond(rpc_id, permission::answer(&asked, Some(res.decision)));
        }
    }

    fn answer_done(&mut self, op: &OperationId, r: ApiResult<()>) {
        let state = match r {
            Ok(()) => done_state(OperationResult::Unit),
            Err(e) => failed_state(e),
        };
        let _ = self.tx(|t| {
            t.drop_answer(op)?;
            t.set_state(op, &state)?;
            crate::emit_all(&self.inner, t, vec![op_changed(op, state)])
        });
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
                if !usage.is_empty() {
                    let _ = self.tx(|t| crate::emit_all(&self.inner, t, usage));
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
        let Some(run) = self.run.as_mut() else { return };
        let Some(completion) = run.ending.clone() else {
            return;
        };
        let closing: Vec<EventBody> = run
            .norm
            .finish()
            .into_iter()
            .filter_map(|f| match f {
                Fact::Event(e) => Some(e),
                _ => None,
            })
            .collect();
        let (exec, op) = (run.exec.clone(), run.op.clone());
        let session = self.session.clone();
        let r = self.tx(|t| {
            let mut evs = vec![];
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
        let Some(run) = &self.run else { return };
        let op = run.op.clone();
        let state = Observed::Unknown { reason: why.into() };
        if self.tx(|t| t.set_state(&op, &state).map(|_| ())).is_ok() {
            self.gate.0.store(0, Ordering::SeqCst);
            self.run = None;
            self.waits.clear();
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
            Some((
                Outcome::TimedOut,
                ApiError::new(ErrorCode::Expired, "past its deadline"),
            ))
        } else if hub_down && now >= lim.offline_until.0 {
            Some((
                Outcome::TimedOut,
                ApiError::new(ErrorCode::Offline, "the Hub was unreachable past the lease"),
            ))
        } else if self.inner.output(&self.session) > lim.max_output_bytes {
            Some((
                Outcome::Cancelled,
                ApiError::new(ErrorCode::Exhausted, "the output limit was reached"),
            ))
        } else if self.inner.faulted() {
            Some((
                Outcome::Failed,
                ApiError::new(ErrorCode::Internal, "the node's journal fails"),
            ))
        } else {
            None
        };
        if let Some((o, e)) = why {
            let token = run.token;
            self.stopping(token, o, e);
        }
    }
}

/// Starts and opens a harness for `session` (in a task). Its group is recorded before the handshake
/// (never over one not confirmed gone) and stopped again if anything after fails.
async fn open_harness(
    inner: &Arc<Inner>,
    session: &SessionId,
    tx: mpsc::UnboundedSender<Msg>,
    gate: Arc<Gate>,
) -> ApiResult<LiveNew> {
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
    let generation = inner.generation.fetch_add(1, Ordering::SeqCst) + 1;
    let (tx1, tx2) = (tx.clone(), tx);
    let counter = inner.output_counter(session);
    let conn = Conn::spawn(
        Spawn {
            program,
            args,
            cwd: cwd.clone().into(),
            env: vec![(
                "CLAUDE_CODE_EXECUTABLE".into(),
                inner.cfg.acp.claude_executable.clone().into_os_string(),
            )],
            log_tag: format!("[node {session}]"),
        },
        move |c, inc| {
            let token = gate.0.load(Ordering::SeqCst);
            match inc {
                Incoming::Notification { method, params } if method == "session/update" => {
                    if let Some(u) = params.get("update") {
                        // counted as it arrives: a flood is stopped before it is all kept
                        counter.fetch_add(u.to_string().len() as u64, Ordering::SeqCst);
                        let _ = tx1.send(Msg::Update {
                            generation,
                            token,
                            update: u.clone(),
                        });
                    }
                }
                Incoming::Notification { .. } => {}
                Incoming::Request { id, method, params }
                    if method == "session/request_permission" =>
                {
                    let _ = tx1.send(Msg::Permission {
                        generation,
                        token,
                        rpc_id: id,
                        params,
                    });
                }
                Incoming::Request { id, method, .. } => {
                    c.respond_error(id, -32601, &format!("{method} not supported by Steve"));
                }
            }
        },
        move || {
            let _ = tx2.send(Msg::Exited { generation });
        },
    )
    .map_err(|e| ApiError::new(ErrorCode::Unsupported, e.message))?;
    let pgid = conn.pid() as i64;
    let started = procs::start_time(pgid);
    let meter = derived_id(
        "p",
        &format!("{session}.{}.{generation}", inner.incarnation),
    );
    if let Err(e) = inner.j.tx(|t| {
        t.put_proc(
            session,
            pgid,
            generation,
            inner.incarnation,
            &meter,
            started,
        )
    }) {
        // not recorded: it must not run on
        procs::stop(pgid, started).await;
        return Err(e);
    }
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
    let opened = acp_session::open(
        &conn,
        OpenConfig {
            client_version: &inner.cfg.acp.version,
            cwd: &cwd,
            resume: row.acp_id.as_deref(),
            mcp_servers: crate::mcp_servers(inner, session),
            mode: crate::mode(&harness, row.open.policy.permissions),
            model: row.open.choice.model.as_deref(),
            effort: row.open.choice.effort.as_deref(),
        },
        &seen,
        Strictness::Strict,
    )
    .await;
    let opened = match opened {
        Ok(o) => o,
        Err(e) => {
            if procs::stop(pgid, started).await.gone() {
                let _ = inner.j.tx(|t| t.drop_proc(session, pgid));
            }
            return Err(ApiError::new(ErrorCode::Unsupported, e.message));
        }
    };
    let (new_id, model, effort) = seen.0.into_inner().unwrap();
    let saved = inner.j.tx(|t| {
        let Some(mut r) = t.session(session)? else {
            return Err(ApiError::new(ErrorCode::Invalid, "the session is gone"));
        };
        if let Some(a) = new_id {
            r.acp_id = Some(a);
        }
        // what was asked and confirmed by the handshake; what the harness reported where it did
        r.view.effective = r.open.choice.clone();
        if model.is_some() {
            r.view.effective.model = model;
        }
        if effort.is_some() {
            r.view.effective.effort = effort;
        }
        r.view.effective_permissions = r.open.policy.permissions;
        r.view.resume = r
            .acp_id
            .clone()
            .map(ResumeHandle::new)
            .transpose()
            .ok()
            .flatten();
        r.view.revision = Revision::new(format!("r{}", t.bump("revision")?)).map_err(internal)?;
        t.put_session(&r)?;
        Ok(r.view)
    });
    let failed_after = match saved {
        Ok(view) if !conn.has_exited() => {
            inner.wake.notify_one();
            return Ok(LiveNew {
                conn,
                generation,
                acp_id: opened.acp_id,
                image: opened.image,
                pgid,
                started,
                view,
            });
        }
        Ok(_) => ApiError::new(ErrorCode::Offline, "adapter exited"),
        Err(e) => e,
    };
    if procs::stop(pgid, started).await.gone() {
        let _ = inner.j.tx(|t| t.drop_proc(session, pgid));
    }
    Err(failed_after)
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
    }
}
