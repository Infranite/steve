//! The Hub's coordinator for sessions that run on a Node (docs/hub-node-client.md, D3).
//!
//! The Hub decides, the Node executes. Here the Hub's decisions become durable commands and the
//! Node's facts become the Hub's state, each in one store transaction:
//!
//! - **T1, plan**: a message is queued; if the session can take a turn now, its queued messages are
//!   claimed into one logical turn, an attempt with its frozen request is created, the session is
//!   reserved and the `start_turn` command is written. All or nothing.
//! - **T2, send**: a worker leases a command, sends it to the Node outside any lock, and merges the
//!   receipt (it only moves forward; a refusal before acceptance is told from an uncertain send,
//!   which is sent again as the very same command).
//! - **T3, receive**: the Node's events go into the inbox; only the contiguous durable prefix is
//!   acknowledged.
//! - **T4, apply**: events are applied in order, each in the transaction that marks it applied: the
//!   session's Steve events (`steve.v1`), the attempt's and turn's state, a retry as a new attempt
//!   (never a rewrite), the next turn or, once nothing follows, the turn's end for the business
//!   side, exactly once.
//!
//! What the first slice does not do yet (answering permissions, release and tidy chains) is refused
//! explicitly, never reported as done.

use std::collections::HashSet;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use steve_node_api::*;
use steve_store::runtime::{self as rt, ReceiptState, StreamKey};
use steve_store::{Store, StoreError, Tx};

/// Retries of a turn after a passing failure (gateway 5xx) with no tool run, and their waits.
const RETRY_DELAYS_MS: [i64; 2] = [20_000, 60_000];
/// How long a command may wait to be accepted by the Node.
const ACCEPT_WINDOW_MS: i64 = 10 * 60_000;
/// A worker's hold on a command it sends.
const LEASE_MS: i64 = 60_000;

fn now() -> i64 {
    steve_store::now()
}

fn uid(p: &str) -> String {
    let u = uuid::Uuid::new_v4().simple().to_string();
    format!("{p}_{}", &u[..20])
}

fn conflict(e: impl std::fmt::Display) -> StoreError {
    StoreError::Conflict(e.to_string())
}

fn idv<T: TryFrom<String>>(s: impl Into<String>) -> Result<T, StoreError>
where
    T::Error: std::fmt::Display,
{
    T::try_from(s.into()).map_err(conflict)
}

/// What the coordinator tells the business side (the core).
pub trait NodeDomain: Send + Sync + 'static {
    /// The session's logical turn is over and nothing follows it: the turn end of the item's
    /// lifecycle. Called once per turn, after its end is committed.
    fn turn_ended(&self, session: &str, stop_reason: &str, error: Option<String>);
}

/// How a session runs on its Node (fixed when it is bound).
#[derive(Debug, Clone)]
pub struct Binding {
    pub session: String,
    /// The Hub's stable name of what the workspace is for (`item:<id>`).
    pub resource: String,
    pub prepare: PrepareKind,
    pub choice: HarnessChoice,
    pub policy: ExecutionPolicy,
}

pub struct NodeRuntime {
    store: Store,
    node: Arc<dyn Node>,
    node_id: NodeId,
    domain: Arc<dyn NodeDomain>,
    /// The lease owner of this Hub process.
    owner: String,
    wake_send: tokio::sync::Notify,
    wake_apply: tokio::sync::Notify,
    holding: AtomicBool,
    /// Streams with events to apply.
    dirty: Mutex<HashSet<(String, String)>>,
}

/// A command's envelope as the Node gets it, built once and stored verbatim.
fn command<T: serde::Serialize>(op: &str, body: T) -> Result<(Value, String), StoreError> {
    let c = Command {
        id: idv::<OperationId>(op)?,
        issued_at: Timestamp(now()),
        accept_before: Timestamp(now() + ACCEPT_WINDOW_MS),
        grant: idv::<GrantId>("hub")?,
        body,
    };
    let v = serde_json::to_value(&c).map_err(conflict)?;
    let digest = digest_of(&v).map_err(conflict)?;
    Ok((v, digest.to_string()))
}

impl NodeRuntime {
    pub fn new(store: Store, node: Arc<dyn Node>, domain: Arc<dyn NodeDomain>) -> Arc<NodeRuntime> {
        let node_id = node.id().clone();
        Arc::new(NodeRuntime {
            store,
            node,
            node_id,
            domain,
            owner: uid("hub"),
            wake_send: tokio::sync::Notify::new(),
            wake_apply: tokio::sync::Notify::new(),
            holding: AtomicBool::new(false),
            dirty: Mutex::new(HashSet::new()),
        })
    }

    /// The worker that sends commands and the one that applies events, until the runtime is dropped.
    pub fn start(self: &Arc<Self>) {
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            loop {
                let Some(me) = weak.upgrade() else { return };
                let sent = me.send_one().await;
                if !sent {
                    let _ =
                        tokio::time::timeout(Duration::from_secs(1), me.wake_send.notified()).await;
                }
            }
        });
        let weak = Arc::downgrade(self);
        tokio::spawn(async move {
            loop {
                let Some(me) = weak.upgrade() else { return };
                let applied = me.apply_some();
                if !applied {
                    let _ = tokio::time::timeout(Duration::from_secs(1), me.wake_apply.notified())
                        .await;
                }
            }
        });
    }

    fn tx<T>(&self, f: impl FnOnce(&Tx) -> Result<T, StoreError>) -> Result<T, StoreError> {
        self.store.transaction(f)
    }

    /// The session runs on this Node from now on: its workspace is prepared and it is opened, by
    /// two commands (the open waits for the prepare). Idempotent for the same binding.
    pub fn bind(&self, b: &Binding) -> Result<(), StoreError> {
        let workspace = WorkspaceRef {
            node: self.node_id.clone(),
            id: idv(derived_id("w", &b.resource))?,
        };
        let open = OpenSession {
            session: idv(b.session.clone())?,
            workspace: workspace.clone(),
            choice: b.choice.clone(),
            policy: b.policy.clone(),
            resume: None,
        };
        self.tx(|tx| {
            let fresh = rt::bind_session_in(
                tx,
                &rt::SessionBinding {
                    session: &b.session,
                    node: self.node_id.as_str(),
                    workspace: &serde_json::to_value(&workspace).map_err(conflict)?,
                    node_session: &b.session,
                    config_revision: open.policy.revision.as_str(),
                },
                now(),
            )?;
            if !fresh {
                return Ok(());
            }
            let (prep, prep_d) = command(
                &format!("op_prep_{}", b.session),
                Prepare {
                    resource: b.resource.clone(),
                    kind: b.prepare.clone(),
                },
            )?;
            let prep_op = format!("op_prep_{}", b.session);
            rt::insert_command_in(
                tx,
                &rt::NewCommand {
                    operation: &prep_op,
                    node: self.node_id.as_str(),
                    method: "prepare_workspace",
                    request: &prep,
                    digest: &prep_d,
                    target_attempt: None,
                    dependency: None,
                },
                now(),
            )?;
            let open_op = format!("op_open_{}", b.session);
            let (open_v, open_d) = command(&open_op, open.clone())?;
            rt::insert_command_in(
                tx,
                &rt::NewCommand {
                    operation: &open_op,
                    node: self.node_id.as_str(),
                    method: "open_session",
                    request: &open_v,
                    digest: &open_d,
                    target_attempt: None,
                    dependency: Some(&prep_op),
                },
                now(),
            )?;
            let rev = session_revision(tx, &b.session)?;
            rt::session_cas_in(
                tx,
                &b.session,
                rev,
                &rt::SessionUpdate {
                    phase: "opening",
                    active_attempt: None,
                    release_pending: false,
                },
                now(),
            )?;
            Ok(())
        })?;
        self.wake_send.notify_one();
        Ok(())
    }

    /// Whether `session` runs on a Node.
    pub fn owns(&self, session: &str) -> bool {
        self.store
            .reader(|c| {
                Ok::<_, StoreError>(c.query_row(
                    "SELECT EXISTS(SELECT 1 FROM runtime_sessions WHERE session_id=?)",
                    [session],
                    |r| r.get::<_, bool>(0),
                )?)
            })
            .unwrap_or(false)
    }

    // ── what the core asks (AgentRuntime for Node sessions) ──

    /// Queues the message and, if the session can take a turn now, plans it (T1); otherwise it
    /// waits in the queue, and the answer says why.
    pub fn prompt(
        &self,
        session: &str,
        text: &str,
        via: &str,
        files: &[FileRefLite],
    ) -> Result<Planned, StoreError> {
        let planned = self.tx(|tx| {
            let files = serde_json::to_string(files).map_err(conflict)?;
            tx.execute(
                "INSERT INTO queued (id, session_id, text, via, created, files) VALUES (?, ?, ?, ?, ?, ?)",
                rusqlite::params![uid("q"), session, text, via, now(), files],
            )?;
            tx.execute(
                "INSERT INTO events (session_id, ts, kind, data, format) VALUES (?, ?, 'queued', ?, 'steve.v1')",
                rusqlite::params![session, now(), json!({"text": text, "via": via}).to_string()],
            )?;
            self.plan_in(tx, session)
        })?;
        if planned == Planned::Created {
            self.wake_send.notify_one();
        }
        Ok(planned)
    }

    /// Plans a turn from what is queued, if the session can take one (T1); otherwise nothing.
    pub fn flush(&self, session: &str) -> Result<Planned, StoreError> {
        let planned = self.tx(|tx| self.plan_in(tx, session))?;
        if planned == Planned::Created {
            self.wake_send.notify_one();
        }
        Ok(planned)
    }

    /// T1 inside `tx`: claim, attempt, reservation, command.
    fn plan_in(&self, tx: &Tx, session: &str) -> Result<Planned, StoreError> {
        if self.holding.load(Ordering::SeqCst) {
            return Ok(Planned::Held);
        }
        let (phase, active): (String, Option<String>) = tx.query_row(
            "SELECT phase, active_attempt FROM runtime_sessions WHERE session_id=?",
            [session],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if phase != "ready" || active.is_some() {
            return Ok(Planned::Busy);
        }
        let turn = uid("t");
        let msgs = rt::claim_in(tx, session, &turn, now())?;
        if msgs.is_empty() {
            return Ok(Planned::Empty);
        }
        let text = msgs
            .iter()
            .map(|m| m.text.clone())
            .collect::<Vec<_>>()
            .join("\n\n");
        for m in &msgs {
            tx.execute(
                "INSERT INTO events (session_id, ts, kind, data, format) VALUES (?, ?, 'user', ?, 'steve.v1')",
                rusqlite::params![session, now(), json!({"text": m.text, "via": m.via}).to_string()],
            )?;
        }
        self.attempt_in(tx, session, &turn, 0, vec![Part::Text { text }])?;
        Ok(Planned::Created)
    }

    /// A new attempt of `turn` with its own frozen request and its `start_turn` command.
    fn attempt_in(
        &self,
        tx: &Tx,
        session: &str,
        turn: &str,
        ordinal: i64,
        prompt: Vec<Part>,
    ) -> Result<(), StoreError> {
        let open = self.open_of(tx, session)?;
        let attempt = uid("a");
        let op = format!("op_{attempt}");
        let spec = TurnSpec {
            session: open.session.clone(),
            turn: idv(turn.to_string())?,
            run: RunSpec {
                attempt: idv(attempt.clone())?,
                choice: open.choice.clone(),
                directory: RunDirectory::Workspace {
                    workspace: open.workspace.clone(),
                },
                prompt,
                policy: open.policy.clone(),
                limits: Limits {
                    deadline: Timestamp(now() + 6 * 3_600_000),
                    offline_until: Timestamp(now() + 6 * 3_600_000),
                    max_output_bytes: 256 << 20,
                    max_tokens: None,
                    max_cost: None,
                },
                continuation: None,
            },
        };
        let exec = ExecutionRef::Turn {
            session: open.session.clone(),
            turn: spec.turn.clone(),
            attempt: spec.run.attempt.clone(),
        };
        let (req, digest) = command(&op, spec.clone())?;
        rt::insert_attempt_in(
            tx,
            &rt::NewAttempt {
                id: &attempt,
                turn,
                ordinal,
                node: self.node_id.as_str(),
                operation: &op,
                execution: &serde_json::to_value(&exec).map_err(conflict)?,
                spec: &serde_json::to_value(&spec).map_err(conflict)?,
                digest: &digest,
            },
            now(),
        )?;
        rt::insert_command_in(
            tx,
            &rt::NewCommand {
                operation: &op,
                node: self.node_id.as_str(),
                method: "start_turn",
                request: &req,
                digest: &digest,
                target_attempt: Some(&attempt),
                dependency: None,
            },
            now(),
        )?;
        let trev = turn_revision(tx, turn)?;
        rt::turn_cas_in(
            tx,
            turn,
            trev,
            &rt::TurnUpdate {
                state: "running",
                active_attempt: Some(&attempt),
                retry_ordinal: ordinal,
                applied: false,
            },
            now(),
        )?;
        let srev = session_revision(tx, session)?;
        if !rt::session_cas_in(
            tx,
            session,
            srev,
            &rt::SessionUpdate {
                phase: "active",
                active_attempt: Some(&attempt),
                release_pending: false,
            },
            now(),
        )? {
            return Err(conflict("the session changed while planning"));
        }
        Ok(())
    }

    /// The session's open request, as sent (the turn must match it exactly).
    fn open_of(&self, tx: &Tx, session: &str) -> Result<OpenSession, StoreError> {
        let req: String = tx.query_row(
            "SELECT request FROM node_commands WHERE method='open_session' AND json_extract(request,'$.body.session')=? ORDER BY created DESC, rowid DESC LIMIT 1",
            [session],
            |r| r.get(0),
        )?;
        let c: Command<OpenSession> = serde_json::from_str(&req)?;
        Ok(c.body)
    }

    /// A turn of `session` is in flight (planned, running, waiting, retrying or unknown), or the
    /// session is opening or releasing.
    pub fn is_busy(&self, session: &str) -> bool {
        self.store
            .reader(|c| {
                Ok::<_, StoreError>(c.query_row(
                    "SELECT EXISTS(SELECT 1 FROM runtime_sessions WHERE session_id=?1 AND (active_attempt IS NOT NULL OR phase IN ('opening','releasing','active')))
                       OR EXISTS(SELECT 1 FROM runtime_turns WHERE session_id=?1 AND state<>'finished')",
                    [session],
                    |r| r.get::<_, bool>(0),
                )?)
            })
            .unwrap_or(true)
    }

    pub fn running_turns(&self) -> usize {
        self.store
            .reader(|c| {
                Ok::<_, StoreError>(c.query_row(
                    "SELECT COUNT(*) FROM runtime_turns WHERE state<>'finished'",
                    [],
                    |r| r.get::<_, i64>(0),
                )?)
            })
            .unwrap_or(0) as usize
    }

    pub fn hold(&self, on: bool) {
        self.holding.store(on, Ordering::SeqCst);
    }

    pub fn is_holding(&self) -> bool {
        self.holding.load(Ordering::SeqCst)
    }

    /// Asks the Node to cancel the attempt running now (frozen as the target here, never looked up
    /// again when the command goes out). `true`: there was one, the cancel is on its way.
    pub fn cancel(&self, session: &str) -> Result<bool, StoreError> {
        let r = self.tx(|tx| {
            let row: Option<(String, String, i64)> = tx
                .query_row(
                    "SELECT a.attempt_id, a.operation_id, a.revision FROM runtime_sessions s JOIN runtime_attempts a ON a.attempt_id = s.active_attempt WHERE s.session_id=? AND a.state<>'finished'",
                    [session],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                )
                .ok();
            let Some((attempt, target, rev)) = row else {
                return Ok(false);
            };
            rt::cancel_attempt_in(tx, &attempt, rev, now())?;
            let op = format!("op_cancel_{attempt}");
            let (req, digest) = command(
                &op,
                Cancel {
                    target: idv(target)?,
                    reason: "the Hub cancels the turn".into(),
                },
            )?;
            rt::insert_command_in(
                tx,
                &rt::NewCommand {
                    operation: &op,
                    node: self.node_id.as_str(),
                    method: "cancel",
                    request: &req,
                    digest: &digest,
                    target_attempt: Some(&attempt),
                    dependency: None,
                },
                now(),
            )?;
            Ok(true)
        })?;
        if r {
            self.wake_send.notify_one();
        }
        Ok(r)
    }

    // ── T2: sending ──

    /// Leases one command, sends it and merges what came back. False: nothing to send.
    async fn send_one(&self) -> bool {
        let lease =
            match self.tx(|tx| rt::lease_command_in(tx, &self.owner, now(), now() + LEASE_MS)) {
                Ok(Some(l)) => l,
                Ok(None) => return false,
                Err(e) => {
                    tracing::error!("node command lease: {e}");
                    return false;
                }
            };
        let sent = self.send(&lease).await;
        let r = self.tx(|tx| match &sent {
            Ok(receipt) => self.receipt_in(tx, receipt),
            Err(e)
                if e.retry == RetryClass::SameOperation || e.code == ErrorCode::OutcomeUnknown =>
            {
                // not refused for good, or maybe accepted: the very same command again later
                rt::retry_command_in(tx, &lease, now(), now() + 1_000)?;
                Ok(())
            }
            Err(e) => {
                rt::merge_receipt_in(
                    tx,
                    &lease.operation,
                    ReceiptState::Rejected,
                    false,
                    &serde_json::to_value(e).map_err(conflict)?,
                    now(),
                )?;
                if lease.method == "start_turn" {
                    self.refused_attempt_in(tx, &lease.operation, e)?;
                }
                Ok(())
            }
        });
        if let Err(e) = r {
            tracing::error!("node command {}: {e}", lease.operation);
        }
        self.wake_apply.notify_one();
        true
    }

    async fn send(&self, l: &rt::CommandLease) -> ApiResult<Receipt> {
        fn parse<T: serde::de::DeserializeOwned>(v: &Value) -> ApiResult<Command<T>> {
            serde_json::from_value(v.clone())
                .map_err(|e| ApiError::new(ErrorCode::Invalid, format!("stored command: {e}")))
        }
        let n = &self.node;
        match l.method.as_str() {
            "prepare_workspace" => n.prepare_workspace(parse(&l.request)?).await,
            "open_session" => n.open_session(parse(&l.request)?).await,
            "start_turn" => n.start_turn(parse(&l.request)?).await,
            "cancel" => n.cancel(parse(&l.request)?).await,
            "release_session" => n.release_session(parse(&l.request)?).await,
            other => Err(ApiError::new(
                ErrorCode::Unsupported,
                format!("the coordinator does not send {other}"),
            )),
        }
    }

    /// What the Node says about a command, from its answer or its stream (the same either way):
    /// merged forward, and an open that finished makes the session ready (or released).
    fn receipt_in(&self, tx: &Tx, receipt: &Receipt) -> Result<(), StoreError> {
        let (state, success) = receipt_state(&receipt.state);
        let op = receipt.operation.as_str();
        let Some(method) = tx
            .query_row(
                "SELECT method FROM node_commands WHERE operation_id=?",
                [op],
                |r| r.get::<_, String>(0),
            )
            .ok()
        else {
            return Ok(());
        };
        let moved = rt::merge_receipt_in(
            tx,
            op,
            state,
            success,
            &serde_json::to_value(receipt).map_err(conflict)?,
            now(),
        )?;
        if moved && method == "open_session" && state == ReceiptState::Finished {
            let s: String = tx.query_row(
                "SELECT json_extract(request,'$.body.session') FROM node_commands WHERE operation_id=?",
                [op],
                |r| r.get(0),
            )?;
            let rev = session_revision(tx, &s)?;
            rt::session_cas_in(
                tx,
                &s,
                rev,
                &rt::SessionUpdate {
                    phase: if success { "ready" } else { "released" },
                    active_attempt: None,
                    release_pending: false,
                },
                now(),
            )?;
            if success {
                self.plan_in(tx, &s)?;
            }
        }
        Ok(())
    }

    /// The Node refused an attempt before running it: it ends as failed, its turn with it.
    fn refused_attempt_in(&self, tx: &Tx, op: &str, e: &ApiError) -> Result<(), StoreError> {
        let row: Option<(String, i64, String, String)> = tx
            .query_row(
                "SELECT a.attempt_id, a.revision, a.turn_id, t.session_id FROM runtime_attempts a JOIN runtime_turns t USING(turn_id) WHERE a.operation_id=?",
                [op],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .ok();
        let Some((attempt, rev, turn, session)) = row else {
            return Ok(());
        };
        let completion = Completion {
            outcome: Outcome::Failed,
            stop_reason: "error".into(),
            error: Some(e.clone()),
            result: vec![],
            continuation: None,
            transient: false,
            acted: false,
        };
        rt::attempt_cas_in(
            tx,
            &attempt,
            rev,
            "finished",
            None,
            Some(&serde_json::to_value(&completion).map_err(conflict)?),
            now(),
        )?;
        self.turn_over_in(tx, &session, &turn, &completion)
    }

    // ── T3: receiving ──

    /// Takes a batch from the Node into the inbox; acknowledges the contiguous durable prefixes.
    fn receive(&self, batch: &EventBatch) -> Result<EventAck, StoreError> {
        let node = self.node_id.to_string();
        let mut touched: Vec<(String, String)> = vec![];
        self.tx(|tx| {
            for e in &batch.events {
                let k = StreamKey {
                    node: &node,
                    stream: e.cursor.stream.as_str(),
                    epoch: e.cursor.epoch.as_str(),
                };
                rt::inbox_insert_in(
                    tx,
                    k,
                    e.cursor.seq as i64,
                    e.id.as_str(),
                    &serde_json::to_value(e).map_err(conflict)?,
                    now(),
                )?;
                let key = (e.cursor.stream.to_string(), e.cursor.epoch.to_string());
                if !touched.contains(&key) {
                    touched.push(key);
                }
            }
            let mut durable = vec![];
            for (stream, epoch) in &touched {
                let w = rt::watermarks_in(
                    tx,
                    StreamKey {
                        node: &node,
                        stream,
                        epoch,
                    },
                )?;
                if w.durable > 0 {
                    durable.push(Watermark {
                        stream: idv(stream.clone())?,
                        epoch: idv(epoch.clone())?,
                        seq: w.durable as u64,
                    });
                }
            }
            Ok(EventAck { durable })
        })
        .inspect(|_| {
            self.dirty.lock().unwrap().extend(touched.iter().cloned());
            self.wake_apply.notify_one();
        })
    }

    // ── T4: applying ──

    /// Applies what is ready on every dirty stream. False: nothing was.
    fn apply_some(&self) -> bool {
        let streams: Vec<(String, String)> = self.dirty.lock().unwrap().iter().cloned().collect();
        let node = self.node_id.to_string();
        let mut any = false;
        let mut ended: Vec<(String, String, Option<String>)> = vec![];
        for (stream, epoch) in streams {
            loop {
                let k = StreamKey {
                    node: &node,
                    stream: &stream,
                    epoch: &epoch,
                };
                let r = self.tx(|tx| {
                    let Some(p) = rt::next_event_in(tx, k)? else {
                        return Ok(None);
                    };
                    let ev: NodeEvent = serde_json::from_value(p.payload)?;
                    let out = self.apply_in(tx, &ev)?;
                    rt::mark_applied_in(tx, k, p.seq)?;
                    Ok(Some(out))
                });
                match r {
                    Ok(Some(end)) => {
                        any = true;
                        if let Some(e) = end {
                            ended.push(e);
                        }
                    }
                    Ok(None) => {
                        self.dirty
                            .lock()
                            .unwrap()
                            .remove(&(stream.clone(), epoch.clone()));
                        break;
                    }
                    Err(e) => {
                        tracing::error!("apply {stream}: {e}");
                        break;
                    }
                }
            }
        }
        for (session, stop, error) in ended {
            self.domain.turn_ended(&session, &stop, error);
        }
        if any {
            self.wake_send.notify_one();
        }
        any
    }

    /// One event's effects in `tx`. Returns a turn end to tell the business side after the commit.
    fn apply_in(
        &self,
        tx: &Tx,
        ev: &NodeEvent,
    ) -> Result<Option<(String, String, Option<String>)>, StoreError> {
        let session = match &ev.subject {
            Subject::Session { session } => Some(session.to_string()),
            Subject::Execution(ExecutionRef::Turn { session, .. }) => Some(session.to_string()),
            _ => None,
        };
        let mut end = None;
        match &ev.body {
            EventBody::OperationChanged { receipt } => self.receipt_in(tx, receipt)?,
            EventBody::AttemptStarted => {
                if let Subject::Execution(x) = &ev.subject
                    && let Some((rev, state)) = attempt_row(tx, x.attempt().as_str())?
                    && state != "finished"
                {
                    rt::attempt_cas_in(
                        tx,
                        x.attempt().as_str(),
                        rev,
                        "running",
                        None,
                        None,
                        now(),
                    )?;
                }
            }
            EventBody::AttemptEnded { completion } => {
                let Subject::Execution(x) = &ev.subject else {
                    return Ok(None);
                };
                let attempt = x.attempt().as_str();
                let Some((rev, state)) = attempt_row(tx, attempt)? else {
                    return Ok(None);
                };
                if state == "finished" {
                    return Ok(None);
                }
                rt::attempt_cas_in(
                    tx,
                    attempt,
                    rev,
                    "finished",
                    None,
                    Some(&serde_json::to_value(completion).map_err(conflict)?),
                    now(),
                )?;
                let (turn, s): (String, String) = tx.query_row(
                    "SELECT a.turn_id, t.session_id FROM runtime_attempts a JOIN runtime_turns t USING(turn_id) WHERE a.attempt_id=?",
                    [attempt],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?;
                // the receipt the Node gives this start_turn from now on
                self.receipt_in(
                    tx,
                    &Receipt {
                        operation: idv(op_of_attempt(tx, attempt)?)?,
                        state: Observed::Finished {
                            outcome: completion.outcome,
                            result: Some(OperationResult::Execution(completion.clone())),
                            error: completion.error.clone(),
                        },
                    },
                )?;
                self.store_event_in(tx, &s, ev)?;
                end = self.after_attempt_in(tx, &s, &turn, completion)?;
                return Ok(end);
            }
            _ => {}
        }
        if let Some(s) = session
            && !matches!(ev.body, EventBody::OperationChanged { .. })
        {
            self.store_event_in(tx, &s, ev)?;
        }
        Ok(end)
    }

    /// The session's Steve event, once (the Node's stream position is its identity).
    fn store_event_in(&self, tx: &Tx, session: &str, ev: &NodeEvent) -> Result<(), StoreError> {
        let kind = serde_json::to_value(&ev.body).map_err(conflict)?["type"]
            .as_str()
            .unwrap_or("diagnostic")
            .to_string();
        tx.execute(
            "INSERT OR IGNORE INTO events (session_id, ts, kind, data, format, source_node, source_stream, source_epoch, source_seq)
             VALUES (?, ?, ?, ?, 'steve.v1', ?, ?, ?, ?)",
            rusqlite::params![
                session,
                ev.at.0,
                kind,
                serde_json::to_string(ev).map_err(conflict)?,
                self.node_id.as_str(),
                ev.cursor.stream.as_str(),
                ev.cursor.epoch.as_str(),
                ev.cursor.seq as i64
            ],
        )?;
        Ok(())
    }

    /// An attempt is over: the Hub decides. A passing failure with no tool run is tried again as a
    /// new attempt of the same turn (after a wait); otherwise the turn is over.
    fn after_attempt_in(
        &self,
        tx: &Tx,
        session: &str,
        turn: &str,
        c: &Completion,
    ) -> Result<Option<(String, String, Option<String>)>, StoreError> {
        let (ordinal, cancelled): (i64, bool) = tx.query_row(
            "SELECT t.retry_ordinal, EXISTS(SELECT 1 FROM runtime_attempts WHERE turn_id=t.turn_id AND cancel_requested=1) FROM runtime_turns t WHERE t.turn_id=?",
            [turn],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let retry = c.outcome == Outcome::Failed
            && c.transient
            && !c.acted
            && !cancelled
            && (ordinal as usize) < RETRY_DELAYS_MS.len();
        if retry {
            // the same messages, a new attempt (its own id and command), after a wait
            let spec: String = tx.query_row(
                "SELECT spec FROM runtime_attempts WHERE turn_id=? ORDER BY ordinal DESC LIMIT 1",
                [turn],
                |r| r.get(0),
            )?;
            let spec: TurnSpec = serde_json::from_str(&spec)?;
            let srev = session_revision(tx, session)?;
            // the slot is kept by the turn: the attempt is freed only to be replaced
            rt::session_cas_in(
                tx,
                session,
                srev,
                &rt::SessionUpdate {
                    phase: "active",
                    active_attempt: None,
                    release_pending: false,
                },
                now(),
            )?;
            self.attempt_in(tx, session, turn, ordinal + 1, spec.run.prompt)?;
            let delay = RETRY_DELAYS_MS[ordinal as usize];
            tx.execute(
                "UPDATE node_commands SET retry_at=? WHERE target_attempt=(SELECT active_attempt FROM runtime_turns WHERE turn_id=?) AND method='start_turn'",
                rusqlite::params![now() + delay, turn],
            )?;
            tx.execute(
                "INSERT INTO events (session_id, ts, kind, data, format) VALUES (?, ?, 'system', ?, 'steve.v1')",
                rusqlite::params![session, now(), json!({"text": format!("推理网关暂时不可用，{} 秒后自动重试（第 {} 次）", delay / 1000, ordinal + 1)}).to_string()],
            )?;
            return Ok(None);
        }
        self.turn_over_in(tx, session, turn, c)?;
        // what is queued meanwhile goes next; only when nothing does is the turn over for the item
        if self.plan_in(tx, session)? == Planned::Created {
            return Ok(None);
        }
        let stop = c.stop_reason.clone();
        let error = c.error.as_ref().map(|e| e.safe_detail.clone());
        Ok(Some((session.to_string(), stop, error)))
    }

    /// The turn and its dispatch receipts are over; the session is free.
    fn turn_over_in(
        &self,
        tx: &Tx,
        session: &str,
        turn: &str,
        c: &Completion,
    ) -> Result<(), StoreError> {
        let rev = turn_revision(tx, turn)?;
        let ordinal: i64 = tx.query_row(
            "SELECT retry_ordinal FROM runtime_turns WHERE turn_id=?",
            [turn],
            |r| r.get(0),
        )?;
        rt::turn_cas_in(
            tx,
            turn,
            rev,
            &rt::TurnUpdate {
                state: "finished",
                active_attempt: None,
                retry_ordinal: ordinal,
                applied: true,
            },
            now(),
        )?;
        rt::dispatch_receipt_in(
            tx,
            turn,
            match c.outcome {
                Outcome::Succeeded => "completed",
                Outcome::Cancelled => "cancelled",
                Outcome::Interrupted | Outcome::TimedOut => "interrupted",
                Outcome::Failed => "error",
            },
            now(),
        )?;
        let srev = session_revision(tx, session)?;
        rt::session_cas_in(
            tx,
            session,
            srev,
            &rt::SessionUpdate {
                phase: "ready",
                active_attempt: None,
                release_pending: false,
            },
            now(),
        )?;
        Ok(())
    }
}

/// What a plan did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Planned {
    /// A turn was planned: its command is on the way.
    Created,
    /// The session is opening, running or releasing: it waits.
    Busy,
    /// Turns are held (release, maintenance).
    Held,
    /// Nothing is queued.
    Empty,
}

/// A queued attachment as the core hands it over (kept with the message).
#[derive(Debug, Clone, serde::Serialize)]
pub struct FileRefLite {
    pub id: String,
    pub name: String,
    pub mime: String,
    pub size: i64,
}

fn receipt_state(s: &Observed) -> (ReceiptState, bool) {
    match s {
        Observed::Accepted => (ReceiptState::Accepted, false),
        Observed::Running | Observed::Waiting { .. } => (ReceiptState::Active, false),
        Observed::Unknown { .. } => (ReceiptState::Unknown, false),
        Observed::Finished { outcome, .. } => {
            (ReceiptState::Finished, *outcome == Outcome::Succeeded)
        }
    }
}

fn session_revision(tx: &Tx, session: &str) -> Result<i64, StoreError> {
    Ok(tx.query_row(
        "SELECT revision FROM runtime_sessions WHERE session_id=?",
        [session],
        |r| r.get(0),
    )?)
}

fn turn_revision(tx: &Tx, turn: &str) -> Result<i64, StoreError> {
    Ok(tx.query_row(
        "SELECT revision FROM runtime_turns WHERE turn_id=?",
        [turn],
        |r| r.get(0),
    )?)
}

fn op_of_attempt(tx: &Tx, attempt: &str) -> Result<String, StoreError> {
    Ok(tx.query_row(
        "SELECT operation_id FROM runtime_attempts WHERE attempt_id=?",
        [attempt],
        |r| r.get(0),
    )?)
}

fn attempt_row(tx: &Tx, attempt: &str) -> Result<Option<(i64, String)>, StoreError> {
    match tx.query_row(
        "SELECT revision, state FROM runtime_attempts WHERE attempt_id=?",
        [attempt],
        |r| Ok((r.get(0)?, r.get(1)?)),
    ) {
        Ok(x) => Ok(Some(x)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// The Hub's side of the Node's event stream.
pub struct Sink(pub std::sync::Weak<NodeRuntime>);

#[async_trait]
impl EventSink for Sink {
    async fn commit(&self, batch: EventBatch) -> ApiResult<EventAck> {
        let Some(rt) = self.0.upgrade() else {
            return Err(ApiError::new(ErrorCode::Offline, "the hub is gone"));
        };
        tokio::task::spawn_blocking(move || {
            rt.receive(&batch)
                .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))
        })
        .await
        .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?
    }
}
