//! One task per session: everything that touches its harness process goes through it, in order.
//!
//! The journal is the truth of what was accepted and decided; the actor turns it into process work
//! and records what the process did. Because a session's prompt, cancel, permission answers, the end
//! of an attempt and the adapter's own messages are all handled here one after the other:
//!
//! - a cancel reaches the attempt it was meant for, never the next one;
//! - an attempt's `attempt_ended` is the last event of that attempt;
//! - updates the adapter replays while loading a session are not taken for the attempt's work
//!   (they arrive before the prompt is written: [`Gate`]).

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
use tokio::sync::mpsc;

use crate::journal::{SessionRow, Tx, internal};
use crate::{Inner, now, stop_group};

pub(crate) enum Msg {
    /// Start the harness for the session as configured now and confirm it; finish `op`.
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
    /// Answer a waiting permission (journaled already).
    Answer {
        id: PermissionId,
        decision: PermissionDecision,
    },
    /// Stop the harness, confirm it is gone; finish `op` (release) or just tell `done`.
    Stop {
        op: Option<OperationId>,
        done: Option<tokio::sync::oneshot::Sender<bool>>,
    },
    // from the adapter
    Update {
        generation: u64,
        early: bool,
        update: Value,
    },
    Permission {
        generation: u64,
        early: bool,
        rpc_id: Value,
        params: Value,
    },
    Exited {
        generation: u64,
    },
    Prompted {
        generation: u64,
        result: Result<Value, RpcError>,
    },
}

/// Which process generation's messages count for the running attempt: the actor opens it right
/// before writing the prompt; the reader stamps each message with whether it came earlier.
#[derive(Default)]
pub(crate) struct Gate(AtomicU64);

struct Live {
    conn: Arc<Conn>,
    generation: u64,
    acp_id: String,
    image: bool,
    pgid: i64,
}

struct Run {
    exec: ExecutionRef,
    op: OperationId,
    norm: Normalizer,
    generation: Option<u64>,
    deadline: Timestamp,
    offline_until: Timestamp,
    max_output: u64,
    output: u64,
    acted: bool,
    cancel: bool,
    /// Why the Node itself stopped it (deadline, lost Hub, output limit).
    stopped: Option<(Outcome, ApiError)>,
}

pub(crate) struct Actor {
    inner: Arc<Inner>,
    session: SessionId,
    rx: mpsc::UnboundedReceiver<Msg>,
    tx: mpsc::UnboundedSender<Msg>,
    gate: Arc<Gate>,
    live: Option<Live>,
    run: Option<Run>,
    waits: HashMap<PermissionId, (Value, Asked)>,
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
            waits: HashMap::new(),
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
                    self.handle(m).await;
                }
                _ = tick.tick() => self.check_limits().await,
            }
        }
    }

    fn tx<R>(&self, f: impl FnOnce(&Tx) -> ApiResult<R>) -> ApiResult<R> {
        let r = self.inner.j.tx(f);
        self.inner.wake.notify_one();
        r
    }

    async fn handle(&mut self, m: Msg) {
        match m {
            Msg::Open { op } => self.open(op).await,
            Msg::Start { op, spec, exec } => self.start(op, *spec, exec).await,
            Msg::Cancel { target } => self.cancel(&target),
            Msg::Answer { id, decision } => {
                if let Some((rpc_id, asked)) = self.waits.remove(&id)
                    && let Some(l) = &self.live
                {
                    l.conn
                        .respond(rpc_id, permission::answer(&asked, Some(decision)));
                }
            }
            Msg::Stop { op, done } => {
                let gone = self.stop_process().await;
                if let Some(op) = op {
                    let r = self.tx(|t| {
                        let mut row = t.session(&self.session)?;
                        if gone && let Some(r) = row.as_mut() {
                            r.view.phase = SessionPhase::Released;
                            t.put_session(r)?;
                        }
                        let state = if gone {
                            done_state(OperationResult::Unit)
                        } else {
                            Observed::Unknown {
                                reason: "the harness did not stop".into(),
                            }
                        };
                        t.set_state(&op, &state)?;
                        let mut evs = vec![(
                            Subject::Operation {
                                operation: op.clone(),
                            },
                            EventBody::OperationChanged {
                                receipt: Receipt {
                                    operation: op.clone(),
                                    state,
                                },
                            },
                        )];
                        if let Some(r) = row {
                            evs.push((
                                Subject::Session {
                                    session: self.session.clone(),
                                },
                                EventBody::SessionChanged { view: r.view },
                            ));
                        }
                        crate::emit_all(&self.inner, t, evs)
                    });
                    if let Err(e) = r {
                        tracing::error!("release {}: {e}", self.session);
                    }
                }
                if let Some(d) = done {
                    let _ = d.send(gone);
                }
            }
            Msg::Update {
                generation,
                early,
                update,
            } => self.update(generation, early, &update),
            Msg::Permission {
                generation,
                early,
                rpc_id,
                params,
            } => self.permission(generation, early, rpc_id, &params),
            Msg::Exited { generation } => {
                if self
                    .live
                    .as_ref()
                    .is_some_and(|l| l.generation == generation)
                {
                    let l = self.live.take().expect("checked");
                    // the leader is gone; its group may not be
                    let pgid = l.pgid;
                    if stop_group(pgid).await {
                        let _ = self.tx(|t| t.drop_proc(&self.session, pgid));
                    }
                }
            }
            Msg::Prompted { generation, result } => {
                if self
                    .run
                    .as_ref()
                    .is_some_and(|r| r.generation == Some(generation))
                {
                    self.finish(Some(generation), result).await;
                }
            }
        }
    }

    // ── the process ──

    /// The running process, or a new one started and opened strictly (a mode, model or effort
    /// that does not take is an error).
    async fn ensure(&mut self) -> ApiResult<()> {
        if self.live.as_ref().is_some_and(|l| !l.conn.has_exited()) {
            return Ok(());
        }
        if let Some(l) = self.live.take() {
            stop_group(l.pgid).await;
        }
        let row = self
            .inner
            .j
            .read(|t| t.session(&self.session))?
            .ok_or_else(|| ApiError::new(ErrorCode::Invalid, "no such session"))?;
        let cwd = crate::workspace_dir(&self.inner, &row.open.workspace)?;
        let cwd = cwd.to_string_lossy().to_string();
        let harness = row.open.choice.harness.clone();
        let (program, args) = self
            .inner
            .cfg
            .acp
            .adapter_command(&harness)
            .map_err(|e| ApiError::new(ErrorCode::Unsupported, e))?;
        let generation = self.inner.generation.fetch_add(1, Ordering::SeqCst) + 1;
        let (tx1, tx2, gate) = (self.tx.clone(), self.tx.clone(), self.gate.clone());
        let conn = Conn::spawn(
            Spawn {
                program,
                args,
                cwd: cwd.clone().into(),
                env: vec![(
                    "CLAUDE_CODE_EXECUTABLE".into(),
                    self.inner
                        .cfg
                        .acp
                        .claude_executable
                        .clone()
                        .into_os_string(),
                )],
                log_tag: format!("[node {}]", self.session),
            },
            move |c, inc| {
                let early = gate.0.load(Ordering::SeqCst) != generation;
                match inc {
                    Incoming::Notification { method, params } if method == "session/update" => {
                        if let Some(u) = params.get("update") {
                            let _ = tx1.send(Msg::Update {
                                generation,
                                early,
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
                            early,
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
        let meter = format!("{}.{}.{generation}", self.session, self.inner.incarnation);
        self.tx(|t| {
            t.put_proc(
                &self.session,
                pgid,
                generation,
                self.inner.incarnation,
                &meter,
            )
        })?;
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
        let opened = acp_session::open(
            &conn,
            OpenConfig {
                client_version: &self.inner.cfg.acp.version,
                cwd: &cwd,
                resume: row.acp_id.as_deref(),
                mcp_servers: crate::mcp_servers(&self.inner, &self.session),
                mode,
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
                if stop_group(pgid).await {
                    let _ = self.tx(|t| t.drop_proc(&self.session, pgid));
                }
                return Err(ApiError::new(ErrorCode::Unsupported, e.message));
            }
        };
        let (new_id, model, effort) = seen.0.into_inner().unwrap();
        self.tx(|t| {
            let Some(mut r) = t.session(&self.session)? else {
                return Ok(());
            };
            if let Some(a) = new_id {
                r.acp_id = Some(a);
            }
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
            t.put_session(&r)
        })?;
        self.live = Some(Live {
            conn,
            generation,
            acp_id: opened.acp_id,
            image: opened.image,
            pgid,
        });
        Ok(())
    }

    /// Stops the process group and waits until it is gone. False: it would not stop.
    async fn stop_process(&mut self) -> bool {
        self.gate.0.store(0, Ordering::SeqCst);
        let Some(l) = self.live.take() else {
            // a group left from before (the leader died, children did not)
            let left = self
                .inner
                .j
                .read(|t| t.proc_of(&self.session))
                .ok()
                .flatten();
            return match left {
                Some(p) => {
                    let gone = stop_group(p.pgid).await;
                    if gone {
                        let _ = self.tx(|t| t.drop_proc(&self.session, p.pgid));
                    }
                    gone
                }
                None => true,
            };
        };
        crate::revoke_tools(&self.inner, &self.session);
        let gone = stop_group(l.pgid).await;
        if gone {
            let _ = self.tx(|t| t.drop_proc(&self.session, l.pgid));
        }
        gone
    }

    async fn open(&mut self, op: OperationId) {
        self.stop_process().await;
        let r = self.ensure().await;
        let res = self.tx(|t| {
            let mut row = t
                .session(&self.session)?
                .ok_or_else(|| ApiError::new(ErrorCode::Invalid, "no such session"))?;
            let state = match &r {
                Ok(()) => {
                    row.view.phase = SessionPhase::Ready;
                    row.view.revision =
                        Revision::new(format!("r{}", t.bump("revision")?)).map_err(internal)?;
                    done_state(OperationResult::Session(row.view.clone()))
                }
                Err(e) => {
                    row.view.phase = SessionPhase::Released;
                    Observed::Finished {
                        outcome: Outcome::Failed,
                        result: None,
                        error: Some(e.clone()),
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
                            session: self.session.clone(),
                        },
                        EventBody::SessionChanged { view: row.view },
                    ),
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
                    ),
                ],
            )
        });
        if let Err(e) = res {
            tracing::error!("open {}: {e}", self.session);
        }
    }

    // ── an attempt ──

    async fn start(&mut self, op: OperationId, spec: TurnSpec, exec: ExecutionRef) {
        let cancelled_before = self
            .inner
            .j
            .read(|t| t.op(&op))
            .ok()
            .flatten()
            .is_some_and(|o| o.cancel);
        let root = self
            .inner
            .j
            .read(|t| t.session(&self.session))
            .ok()
            .flatten()
            .and_then(|r| crate::workspace_dir(&self.inner, &r.open.workspace).ok())
            .unwrap_or_default();
        let meter = Epoch::new(format!("{}.{}", spec.run.attempt, self.inner.incarnation))
            .unwrap_or_else(|_| Epoch::new("m").expect("valid"));
        self.run = Some(Run {
            exec: exec.clone(),
            op: op.clone(),
            norm: Normalizer::new(root.clone(), spec.run.attempt.as_str(), meter),
            generation: None,
            deadline: spec.run.limits.deadline,
            offline_until: spec.run.limits.offline_until,
            max_output: spec.run.limits.max_output_bytes,
            output: 0,
            acted: false,
            cancel: cancelled_before,
            stopped: None,
        });
        if cancelled_before {
            return self
                .finish(None, Ok(json!({"stopReason": "cancelled"})))
                .await;
        }
        if let Err(e) = self.ensure().await {
            let outcome = if e.code == ErrorCode::Offline {
                Outcome::Interrupted
            } else {
                Outcome::Failed
            };
            return self.end(failure(outcome, e, false)).await;
        }
        let (conn, generation, acp_id, image) = {
            let l = self.live.as_ref().expect("ensured");
            (l.conn.clone(), l.generation, l.acp_id.clone(), l.image)
        };
        let prompt = match crate::prompt(&self.inner, &exec, &root, &spec.run.prompt, image).await {
            Ok(p) => p,
            Err(e) => return self.end(failure(Outcome::Failed, e, false)).await,
        };
        // a cancel that came while starting: do not prompt at all
        if self.run.as_ref().is_some_and(|r| r.cancel) {
            return self
                .finish(None, Ok(json!({"stopReason": "cancelled"})))
                .await;
        }
        if let Some(r) = self.run.as_mut() {
            r.generation = Some(generation);
        }
        // from here the adapter's messages are this attempt's
        self.gate.0.store(generation, Ordering::SeqCst);
        let tx = self.tx.clone();
        tokio::spawn(async move {
            let result = conn
                .request(
                    "session/prompt",
                    json!({"sessionId": acp_id, "prompt": prompt}),
                    None,
                )
                .await;
            let _ = tx.send(Msg::Prompted { generation, result });
        });
    }

    fn cancel(&mut self, target: &OperationId) {
        let Some(run) = self.run.as_mut().filter(|r| r.op == *target) else {
            return; // not the attempt running now: nothing to send
        };
        run.cancel = true;
        if let (Some(g), Some(l)) = (run.generation, &self.live)
            && l.generation == g
        {
            l.conn
                .notify("session/cancel", json!({"sessionId": l.acp_id}));
        }
        // whatever it waits on can no longer be answered
        self.expire_waits();
    }

    /// Answer `cancelled` to every waiting request of the running attempt and journal their expiry.
    fn expire_waits(&mut self) {
        let Some(run) = &self.run else { return };
        let exec = run.exec.clone();
        let op = run.op.clone();
        for (_, (rpc_id, _)) in self.waits.drain() {
            if let Some(l) = &self.live {
                l.conn
                    .respond(rpc_id, json!({"outcome": {"outcome": "cancelled"}}));
            }
        }
        let _ = self.inner.j.tx(|t| {
            let mut evs = vec![];
            for r in t.waits_of(&op)? {
                t.drop_wait(&r.id)?;
                evs.push((
                    Subject::Execution(exec.clone()),
                    EventBody::PermissionExpired { id: r.id },
                ));
            }
            if !evs.is_empty() {
                t.set_state(&op, &Observed::Running)?;
            }
            crate::emit_all(&self.inner, t, evs)
        });
        self.inner.wake.notify_one();
    }

    fn update(&mut self, generation: u64, early: bool, u: &Value) {
        let session = self.session.clone();
        let in_attempt = !early
            && self
                .run
                .as_ref()
                .is_some_and(|r| r.generation == Some(generation));
        let mut evs: Vec<(Subject, EventBody)> = vec![];
        let mut config: Vec<Fact> = vec![];
        if in_attempt {
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
                        run.output += output_bytes(&e);
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
                    report.report_id = format!("{meter}.u{n}");
                    report.meter_epoch = Epoch::new(meter).map_err(internal)?;
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
        if let Err(e) = r {
            tracing::error!("node update {session}: {e}");
        }
    }

    fn permission(&mut self, generation: u64, early: bool, rpc_id: Value, params: &Value) {
        let cancelled = json!({"outcome": {"outcome": "cancelled"}});
        let Some(l) = self.live.as_ref().filter(|l| l.generation == generation) else {
            return; // from a process that is gone
        };
        let conn = l.conn.clone();
        let live_run = !early
            && self
                .run
                .as_ref()
                .is_some_and(|r| r.generation == Some(generation) && !r.cancel);
        if !live_run {
            conn.respond(rpc_id, cancelled);
            return;
        }
        let run = self.run.as_ref().expect("checked");
        let root = run_root(&self.inner, &self.session);
        let asked = permission::asked(&rpc_id, params, &root);
        // the entry to Steve's own tools: the Hub authorizes each call itself (HubTools)
        if asked.steve_tool.is_some() {
            conn.respond(
                rpc_id,
                permission::answer(&asked, Some(PermissionDecision::AllowOnce)),
            );
            return;
        }
        let (exec, op, deadline) = (run.exec.clone(), run.op.clone(), run.deadline);
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
                conn.respond(rpc_id, cancelled);
                return;
            }
        };
        let r = self.tx(|t| {
            t.put_wait(&op, &self.session, &req)?;
            let on: Vec<WaitRef> = t
                .waits_of(&op)?
                .into_iter()
                .map(|w| WaitRef::Permission(w.id))
                .collect();
            t.set_state(&op, &Observed::Waiting { on })?;
            crate::emit_all(
                &self.inner,
                t,
                vec![(
                    Subject::Execution(exec.clone()),
                    EventBody::PermissionRequested {
                        request: req.clone(),
                    },
                )],
            )
        });
        match r {
            Ok(()) => {
                self.waits.insert(req.id.clone(), (rpc_id, asked));
            }
            Err(e) => {
                tracing::error!("permission {}: {e}", self.session);
                conn.respond(rpc_id, cancelled);
            }
        }
    }

    /// The prompt answered (or the attempt never got to prompt): build its completion and end it.
    async fn finish(&mut self, generation: Option<u64>, result: Result<Value, RpcError>) {
        let Some(run) = self.run.as_ref() else { return };
        let acted = run.acted;
        let stopped = run.stopped.clone();
        let completion = match result {
            Ok(resp) => {
                let stop = resp
                    .get("stopReason")
                    .and_then(Value::as_str)
                    .unwrap_or("end_turn")
                    .to_string();
                self.final_usage(&resp);
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
                if gone && let Some(l) = self.live.take() {
                    // the next attempt starts a fresh adapter (it loads the session from history)
                    if stop_group(l.pgid).await {
                        let _ = self.tx(|t| t.drop_proc(&self.session, l.pgid));
                    }
                }
                let _ = generation;
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
        self.end(completion).await;
    }

    /// The token figures of the prompt's answer: the main loop's, and per model everything.
    fn final_usage(&mut self, resp: &Value) {
        let Some(run) = &self.run else { return };
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
        let Ok(meter) = Epoch::new(format!("{attempt}.final")) else {
            return;
        };
        let evs: Vec<(Subject, EventBody)> = reports
            .into_iter()
            .enumerate()
            .map(|(i, (model, scope, tokens))| {
                (
                    Subject::Execution(exec.clone()),
                    EventBody::Usage {
                        report: UsageReport {
                            report_id: format!("{attempt}.f{i}"),
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
            .collect();
        if !evs.is_empty() {
            let _ = self.tx(|t| crate::emit_all(&self.inner, t, evs));
        }
    }

    /// Ends the running attempt in one transaction: its waits expire, its last events, its final
    /// state, `attempt_ended`, and the session free again. Only then is it gone from here.
    async fn end(&mut self, completion: Completion) {
        let Some(mut run) = self.run.take() else {
            return;
        };
        self.gate.0.store(0, Ordering::SeqCst);
        for (_, (rpc_id, _)) in self.waits.drain() {
            if let Some(l) = &self.live {
                l.conn
                    .respond(rpc_id, json!({"outcome": {"outcome": "cancelled"}}));
            }
        }
        let closing: Vec<EventBody> = run
            .norm
            .finish()
            .into_iter()
            .filter_map(|f| match f {
                Fact::Event(e) => Some(e),
                _ => None,
            })
            .collect();
        let exec = run.exec.clone();
        let op = run.op.clone();
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
            for e in closing {
                evs.push((Subject::Execution(exec.clone()), e));
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
        if let Err(e) = r {
            tracing::error!("end of {exec:?}: {e}");
        }
    }

    /// Deadline, lost Hub (past the lease) and output limit: the Node stops what it may not go on with.
    async fn check_limits(&mut self) {
        let Some(run) = self.run.as_mut() else { return };
        if run.stopped.is_some() {
            return;
        }
        let now = now();
        let hub_down = self
            .inner
            .hub_down_since
            .lock()
            .unwrap()
            .is_some_and(|since| since < now);
        let why = if now >= run.deadline.0 {
            Some((
                Outcome::TimedOut,
                ApiError::new(ErrorCode::Expired, "past its deadline"),
            ))
        } else if hub_down && now >= run.offline_until.0 {
            Some((
                Outcome::TimedOut,
                ApiError::new(ErrorCode::Offline, "the Hub was unreachable past the lease"),
            ))
        } else if run.output > run.max_output {
            Some((
                Outcome::Cancelled,
                ApiError::new(ErrorCode::Exhausted, "the output limit was reached"),
            ))
        } else {
            None
        };
        let Some(why) = why else { return };
        run.stopped = Some(why);
        let target = run.op.clone();
        let prompting = run.generation.is_some();
        self.cancel(&target);
        if !prompting {
            // still starting: it ends here
            return self
                .finish(None, Ok(json!({"stopReason": "cancelled"})))
                .await;
        }
        // the harness gets a moment to stop by itself; then its group is stopped
        let tx = self.tx.clone();
        let generation = self.run.as_ref().and_then(|r| r.generation);
        let pgid = self.live.as_ref().map(|l| l.pgid);
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(10)).await;
            if let (Some(g), Some(p)) = (generation, pgid) {
                if stop_group(p).await {
                    let _ = tx.send(Msg::Exited { generation: g });
                }
                let _ = tx.send(Msg::Prompted {
                    generation: g,
                    result: Err(RpcError {
                        message: "stopped by the node".into(),
                        data: None,
                    }),
                });
            }
        });
    }
}

fn run_root(inner: &Inner, session: &SessionId) -> std::path::PathBuf {
    inner
        .j
        .read(|t| t.session(session))
        .ok()
        .flatten()
        .and_then(|r: SessionRow| crate::workspace_dir(inner, &r.open.workspace).ok())
        .unwrap_or_default()
}

fn done_state(result: OperationResult) -> Observed {
    Observed::Finished {
        outcome: Outcome::Succeeded,
        result: Some(result),
        error: None,
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

/// What an event adds to the attempt's output (text and tool results).
fn output_bytes(e: &EventBody) -> u64 {
    match e {
        EventBody::TextDelta { text, .. } => text.len() as u64,
        // a tool's result counts once, when it is done (its view is sent again on every update)
        EventBody::ToolChanged { tool }
            if matches!(tool.phase, ToolPhase::Succeeded | ToolPhase::Failed) =>
        {
            tool.result
                .iter()
                .map(|p| match p {
                    Part::Text { text } => text.len() as u64,
                    Part::Diff { new, old, .. } => {
                        (new.len() + old.as_ref().map(String::len).unwrap_or(0)) as u64
                    }
                    _ => 0,
                })
                .sum()
        }
        _ => 0,
    }
}
