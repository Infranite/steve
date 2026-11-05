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
//! On start, before anything is sent, the Hub reconciles with the Node (its ledger, its sessions,
//! its waiting permissions): what the Node cannot tell the end of becomes `unknown` and keeps its
//! session until a person gives it up; it is never sent again.
//!
//! What it does not do yet (tidy chains, set_mode) is refused explicitly, never reported as done.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use rusqlite::OptionalExtension;
use serde_json::{Value, json};
use steve_node_api::*;
use steve_store::runtime::{self as rt, ReceiptState, StreamKey};
use steve_store::{FileRef, Store, StoreError, Tx};

mod config;
mod perm;

pub use config::{ConfigChange, Configured};

/// Retries of a turn after a passing failure (gateway 5xx) with no tool run, and their waits.
const RETRY_DELAYS_MS: [i64; 2] = [20_000, 60_000];
/// How long a command may wait to be accepted by the Node.
const ACCEPT_WINDOW_MS: i64 = 10 * 60_000;
/// A worker's hold on a command it sends.
const LEASE_MS: i64 = 60_000;

/// A turn's end for the business side (session, stop reason, error), told after its commit.
type End = (String, String, Option<String>);

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
    /// An attempt began running (committed), once.
    /// `user_seq`: the `user` entry the attempt answers (the first attempt of a turn only).
    fn attempt_started(
        &self,
        session: &str,
        attempt: &str,
        ordinal: i64,
        user_seq: Option<i64>,
        at: i64,
    ) {
        let _ = (session, attempt, ordinal, user_seq, at);
    }
    /// An attempt is over (committed), once, with what it did.
    fn attempt_finished(
        &self,
        session: &str,
        attempt: &str,
        completion: &Completion,
        facts: &AttemptFacts,
        at: i64,
    ) {
        let _ = (session, attempt, completion, facts, at);
    }
    /// The session's cost went up (committed): budgets may need a look.
    fn cost_changed(&self, session: &str) {
        let _ = session;
    }
    /// A permission card is being made for a Node's request, inside the transaction that makes
    /// it and before anyone can see it: in-memory preparation only (it may be rolled back).
    fn permission_arming(&self, session: &str, decision: &str, request: &PermissionRequest) {
        let _ = (session, decision, request);
    }
    /// A permission card was made for a Node's request (committed): YOLO may answer it now.
    fn permission_asked(&self, session: &str, decision: &str, request: &PermissionRequest) {
        let _ = (session, decision, request);
    }
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
    /// Shared with the workers, which wait on them without keeping the runtime alive.
    wake_send: Arc<tokio::sync::Notify>,
    wake_apply: Arc<tokio::sync::Notify>,
    holding: AtomicBool,
    /// The owner paused the Node ([`Self::set_paused`]).
    paused: AtomicBool,
    /// Set once the Hub has reconciled with the Node since it started: nothing is planned before.
    reconciled: AtomicBool,
    /// Turn ends settled outside the apply worker (a local cancel), told by it after the commit.
    ended_later: Mutex<Vec<(String, String, Option<String>)>>,
    /// Streams with events to apply.
    dirty: Mutex<HashSet<(String, String)>>,
    /// Why this Node cannot run anything (its host failed to start): its sessions then refuse
    /// work instead of queueing commands nobody sends.
    stopped: Mutex<Option<String>>,
    /// How long an attempt (and so a permission it asks) may take, in ms. 6 h unless the host
    /// sets it ([`Self::set_turn_limit`]).
    turn_limit: std::sync::atomic::AtomicI64,
}

/// How long applied Node events stay in the inbox (what is still needed stays longer).
const INBOX_KEEP_MS: i64 = 7 * 24 * 3600 * 1000;
/// How often the apply worker prunes, and how many rows at most per transaction.
const PRUNE_EVERY: Duration = Duration::from_secs(600);
const PRUNE_BATCH: i64 = 500;
/// An attempt's deadline by default.
const TURN_LIMIT_MS: i64 = 6 * 3_600_000;
/// ?1 node, ?2 received before, ?3 at most. The json_extract terms match migration 036's indexes.
const PRUNE_INBOX_SQL: &str = "DELETE FROM node_event_inbox WHERE rowid IN (
  SELECT e.rowid FROM node_event_inbox e JOIN node_streams s USING(node_id, stream, epoch)
   WHERE e.node_id=?1 AND e.received < ?2 AND e.seq <= s.applied
     AND NOT EXISTS (SELECT 1 FROM runtime_attempts a
           WHERE a.attempt_id = json_extract(e.payload,'$.subject.attempt') AND a.state <> 'finished')
     AND NOT (json_extract(e.payload,'$.body.type')='permission_requested' AND EXISTS (
           SELECT 1 FROM decisions d WHERE d.subject_type='tool_call' AND d.status='open'
              AND d.subject_id = json_extract(e.payload,'$.body.request.id')))
     AND NOT (json_extract(e.payload,'$.body.type')='session_changed'
           AND json_extract(e.payload,'$.body.view.resume') IS NOT NULL
           AND e.seq = (SELECT MAX(x.seq) FROM node_event_inbox x
                 WHERE x.node_id=e.node_id AND x.stream=e.stream AND x.epoch=e.epoch
                   AND json_extract(x.payload,'$.body.type')='session_changed'
                   AND json_extract(x.payload,'$.body.view.id')=json_extract(e.payload,'$.body.view.id')
                   AND json_extract(x.payload,'$.body.view.resume') IS NOT NULL))
     AND NOT (json_extract(e.payload,'$.body.type')='session_changed'
           AND e.seq = (SELECT MAX(x.seq) FROM node_event_inbox x
                 WHERE x.node_id=e.node_id AND x.stream=e.stream AND x.epoch=e.epoch
                   AND json_extract(x.payload,'$.body.type')='session_changed'
                   AND json_extract(x.payload,'$.body.view.id')=json_extract(e.payload,'$.body.view.id')))
     AND NOT (json_extract(e.payload,'$.body.type')='session_changed'
           AND json_extract(e.payload,'$.body.view.revision') = (
                 SELECT json_extract(c.receipt,'$.payload.state.result.revision') FROM node_commands c
                  WHERE c.method='open_session' AND c.state='finished' AND json_extract(c.receipt,'$.success')=1
                    AND json_extract(c.request,'$.body.session')=json_extract(e.payload,'$.body.view.id')
                  ORDER BY c.created DESC, c.rowid DESC LIMIT 1))
     AND NOT (json_extract(e.payload,'$.body.type')='usage'
           AND json_extract(e.payload,'$.body.report.cost') IS NOT NULL
           AND e.seq = (SELECT MAX(x.seq) FROM node_event_inbox x
                 WHERE x.node_id=e.node_id AND x.stream=e.stream AND x.epoch=e.epoch
                   AND json_extract(x.payload,'$.body.type')='usage'
                   AND json_extract(x.payload,'$.body.report.meter_epoch')=json_extract(e.payload,'$.body.report.meter_epoch')
                   AND json_extract(x.payload,'$.body.report.cost') IS NOT NULL))
   LIMIT ?3)";

/// The attempt (and its session) of a `start_turn` the user stopped, while it is not over.
fn cancelled_start_in(tx: &Tx, op: &str) -> Result<Option<(String, String)>, StoreError> {
    Ok(tx
        .query_row(
            "SELECT a.attempt_id, t.session_id FROM runtime_attempts a JOIN runtime_turns t USING(turn_id) WHERE a.operation_id=? AND a.cancel_requested=1 AND a.state<>'finished'",
            [op],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?)
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
            wake_send: Arc::new(tokio::sync::Notify::new()),
            wake_apply: Arc::new(tokio::sync::Notify::new()),
            holding: AtomicBool::new(false),
            paused: AtomicBool::new(false),
            reconciled: AtomicBool::new(false),
            ended_later: Mutex::new(vec![]),
            dirty: Mutex::new(HashSet::new()),
            stopped: Mutex::new(None),
            turn_limit: std::sync::atomic::AtomicI64::new(TURN_LIMIT_MS),
        })
    }

    /// Attempts planned from now on get `ms` as their deadline (and their permissions with it).
    pub fn set_turn_limit(&self, ms: i64) {
        self.turn_limit
            .store(ms.max(1), std::sync::atomic::Ordering::Relaxed);
    }

    /// The Node cannot run anything (its host did not come up): from now on its sessions refuse
    /// prompts, cancels, answers and releases with `why`, and nothing is sent.
    pub fn stop(&self, why: &str) {
        *self.stopped.lock().unwrap() = Some(why.to_string());
    }

    /// An error if [`Self::stop`] was called.
    fn running(&self) -> Result<(), StoreError> {
        match &*self.stopped.lock().unwrap() {
            Some(why) => Err(conflict(format!("the Node is not running: {why}"))),
            None => Ok(()),
        }
    }

    /// The worker that sends commands and the one that applies events, until the runtime is dropped.
    pub fn start(self: &Arc<Self>) {
        // events taken in before a restart but not applied yet
        match self.store.reader(|c| {
            let mut st = c.prepare(
                "SELECT stream, epoch FROM node_streams WHERE node_id=? AND durable > applied",
            )?;
            let v = st
                .query_map([self.node_id.as_str()], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok::<_, StoreError>(v)
        }) {
            Ok(v) => self.dirty.lock().unwrap().extend(v),
            Err(e) => tracing::error!("node streams to catch up on: {e}"),
        }
        // cards that fell due and configures refused while the Hub was down
        self.expire_due();
        self.settle_configuring();
        // each worker holds the runtime only while it works, never while it waits: dropping the
        // last handle ends both. Nothing is sent before the Hub has reconciled with the Node.
        let (weak, wake) = (Arc::downgrade(self), self.wake_send.clone());
        tokio::spawn(async move {
            loop {
                let Some(me) = weak.upgrade() else { return };
                match me.reconcile().await {
                    Ok(n) => {
                        if n > 0 {
                            tracing::info!("node {}: reconciled, {n} changed", me.node_id);
                        }
                        me.reconciled.store(true, Ordering::SeqCst);
                        me.plan_waiting();
                        break;
                    }
                    Err(e) => tracing::warn!("node {}: reconcile: {e}; trying again", me.node_id),
                }
                drop(me);
                tokio::time::sleep(Duration::from_secs(5)).await;
            }
            loop {
                let Some(me) = weak.upgrade() else { return };
                let sent = me.send_one().await;
                drop(me);
                if !sent {
                    let _ = tokio::time::timeout(Duration::from_secs(1), wake.notified()).await;
                }
            }
        });
        // applying is SQLite writes and the Hub's callbacks: on a blocking thread, never on an
        // async worker
        let (weak, wake) = (Arc::downgrade(self), self.wake_apply.clone());
        tokio::spawn(async move {
            let mut pruned = std::time::Instant::now() - PRUNE_EVERY;
            loop {
                let Some(me) = weak.upgrade() else { return };
                let prune = pruned.elapsed() >= PRUNE_EVERY;
                let applied = tokio::task::spawn_blocking(move || {
                    let applied = me.apply_some();
                    me.expire_due();
                    if !applied && prune {
                        me.prune_inbox(now());
                    }
                    applied
                })
                .await
                .unwrap_or(false);
                if !applied {
                    if prune {
                        pruned = std::time::Instant::now();
                    }
                    let _ = tokio::time::timeout(Duration::from_secs(1), wake.notified()).await;
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

    /// The session a decision belongs to.
    pub fn decision_session(&self, decision: &str) -> Option<String> {
        decision_session(&self.store, decision)
    }

    /// The session's harness is gone on its Node (released, or abandoned there): open it again,
    /// continuing the same conversation where the Node can.
    fn reopen_in(&self, tx: &Tx, session: &str) -> Result<(), StoreError> {
        let mut open = self.open_of(tx, session)?;
        self.fold_desired_in(tx, session, &mut open)?;
        open.resume = resume_of_in(tx, &self.node_id, session)?;
        let op = uid(&format!("op_open_{session}"));
        let (req, digest) = command(&op, open)?;
        rt::insert_command_in(
            tx,
            &rt::NewCommand {
                operation: &op,
                node: self.node_id.as_str(),
                method: "open_session",
                request: &req,
                digest: &digest,
                target_attempt: None,
                dependency: None,
            },
            now(),
        )?;
        let rev = session_revision(tx, session)?;
        rt::session_cas_in(
            tx,
            session,
            rev,
            &rt::SessionUpdate {
                phase: "opening",
                active_attempt: None,
                release_pending: false,
            },
            now(),
        )?;
        Ok(())
    }

    /// Lets the session's harness go while it is idle (the next message opens it again, continuing
    /// the conversation). `false`: it is busy, or not open.
    pub fn release(&self, session: &str) -> Result<bool, StoreError> {
        self.running()?;
        let r = self.tx(|tx| self.release_in(tx, session))?;
        if r {
            self.wake_send.notify_one();
        }
        Ok(r)
    }

    /// [`Self::release`] inside `tx`.
    fn release_in(&self, tx: &Tx, session: &str) -> Result<bool, StoreError> {
        {
            let Some((phase, active)) = phase_in(tx, session)? else {
                return Ok(false);
            };
            if phase != "ready" || active.is_some() {
                return Ok(false);
            }
            let op = uid(&format!("op_release_{session}"));
            let (req, digest) = command(
                &op,
                SessionId::try_from(session.to_string()).map_err(conflict)?,
            )?;
            rt::insert_command_in(
                tx,
                &rt::NewCommand {
                    operation: &op,
                    node: self.node_id.as_str(),
                    method: "release_session",
                    request: &req,
                    digest: &digest,
                    target_attempt: None,
                    dependency: None,
                },
                now(),
            )?;
            let rev = session_revision(tx, session)?;
            rt::session_cas_in(
                tx,
                session,
                rev,
                &rt::SessionUpdate {
                    phase: "releasing",
                    active_attempt: None,
                    release_pending: false,
                },
                now(),
            )
        }
    }

    /// Whether `session` runs on a Node.
    pub fn owns(&self, session: &str) -> bool {
        self.store
            .reader(|c| {
                Ok::<_, StoreError>(c.query_row(
                    "SELECT EXISTS(SELECT 1 FROM runtime_sessions WHERE session_id=? AND node_id=?)",
                    [session, self.node_id.as_str()],
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
        files: &[FileRef],
    ) -> Result<Planned, StoreError> {
        self.running()?;
        let planned = self.tx(|tx| {
            queue_in(tx, session, text, via, files, now())?;
            self.plan_in(tx, session)
        })?;
        if planned == Planned::Created {
            self.wake_send.notify_one();
        }
        Ok(planned)
    }

    /// Plans a turn from what is queued, if the session can take one (T1), led by Steve's own
    /// message when there is one (identical texts go once); otherwise nothing changes.
    pub fn flush(&self, session: &str, lead: Option<(&str, &str)>) -> Result<Planned, StoreError> {
        self.running()?;
        let planned = self.tx(|tx| {
            // a released session takes the lead too: once something is queued it opens again
            let released = self.may_plan()
                && !self.is_holding()
                && phase_in(tx, session)?
                    .is_some_and(|(p, a)| p == "released" && a.is_none());
            let p = self.plan_state_in(tx, session)?;
            if !released && matches!(p, Planned::Busy | Planned::Held) {
                return Ok(p);
            }
            if let Some((text, via)) = lead {
                let (first, waiting): (Option<i64>, bool) = tx.query_row(
                    "SELECT MIN(created), EXISTS(SELECT 1 FROM queued WHERE session_id=?1 AND text=?2) FROM queued WHERE session_id=?1",
                    rusqlite::params![session, text],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?;
                if !waiting {
                    queue_in(tx, session, text, via, &[], first.map_or(now(), |t| t - 1))?;
                }
            }
            self.plan_in(tx, session)
        })?;
        if planned == Planned::Created {
            self.wake_send.notify_one();
        }
        Ok(planned)
    }

    /// [`Self::plan_state_in`], and a released session with messages waiting is opened again
    /// (the open is what it waits for).
    fn state_in(&self, tx: &Tx, session: &str) -> Result<Planned, StoreError> {
        let p = self.plan_state_in(tx, session)?;
        if p == Planned::Busy {
            let (phase, active, any): (String, Option<String>, bool) = tx.query_row(
                "SELECT phase, active_attempt, EXISTS(SELECT 1 FROM queued WHERE session_id=?1) FROM runtime_sessions WHERE session_id=?1",
                [session],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?;
            if phase == "released" && active.is_none() && any {
                self.reopen_in(tx, session)?;
            }
        }
        Ok(p)
    }

    /// What [`Self::plan_in`] would answer, without planning (and without writing anything).
    fn plan_state_in(&self, tx: &Tx, session: &str) -> Result<Planned, StoreError> {
        // the persisted upgrade maintenance holds too: after a restart mid-drain nobody calls hold
        if self.is_holding() {
            return Ok(Planned::Held);
        }
        if !self.may_plan() {
            // not reconciled with the Node yet: it waits in the queue
            return Ok(Planned::Busy);
        }
        let (phase, active): (String, Option<String>) = tx.query_row(
            "SELECT phase, active_attempt FROM runtime_sessions WHERE session_id=?",
            [session],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if phase != "ready" || active.is_some() {
            return Ok(Planned::Busy);
        }
        // a config change goes before the next turn, at this idle boundary
        if self.boundary_in(tx, session)? {
            return Ok(Planned::Busy);
        }
        let any: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM queued WHERE session_id=?)",
            [session],
            |r| r.get(0),
        )?;
        Ok(if any {
            Planned::Created
        } else {
            Planned::Empty
        })
    }

    /// T1 inside `tx`: claim, attempt, reservation, command.
    fn plan_in(&self, tx: &Tx, session: &str) -> Result<Planned, StoreError> {
        let p = self.state_in(tx, session)?;
        if p != Planned::Created {
            return Ok(p);
        }
        let turn = uid("t");
        let msgs = rt::claim_in(tx, session, &turn, now())?;
        if msgs.is_empty() {
            return Ok(Planned::Empty);
        }
        let mut prompt = vec![Part::Text {
            text: msgs
                .iter()
                .map(|m| m.text.clone())
                .collect::<Vec<_>>()
                .join("\n\n"),
        }];
        for m in &msgs {
            rt::add_event_in(
                tx,
                session,
                "user",
                message_event(&m.text, &m.via, &m.files),
                V1,
                None,
                now(),
            )?;
            for f in &m.files {
                prompt.push(file_part(tx, f)?);
            }
        }
        self.attempt_in(tx, session, &turn, 0, prompt)?;
        Ok(Planned::Created)
    }

    /// [`Self::plan_in`] while applying a Node fact: a turn that cannot be planned (a bad
    /// attachment, say) must not keep the fact from being applied. Its messages stay queued.
    fn plan_after_in(&self, tx: &Tx, session: &str) -> Result<Planned, StoreError> {
        tx.execute_batch("SAVEPOINT plan")?;
        match self.plan_in(tx, session) {
            Ok(p) => {
                tx.execute_batch("RELEASE plan")?;
                Ok(p)
            }
            Err(e) => {
                tracing::error!("plan a turn of {session}: {e}");
                tx.execute_batch("ROLLBACK TO plan; RELEASE plan")?;
                Ok(Planned::Empty)
            }
        }
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
        let limit = self.turn_limit.load(std::sync::atomic::Ordering::Relaxed);
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
                    deadline: Timestamp(now() + limit),
                    offline_until: Timestamp(now() + limit),
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

    /// The session's open request as in force: as last opened or configured (the turn must match
    /// it exactly).
    fn open_of(&self, tx: &Tx, session: &str) -> Result<OpenSession, StoreError> {
        self.config_of_in(tx, session)
    }

    /// A turn of `session` is in flight (planned, running, waiting, retrying or unknown), or the
    /// session is opening or releasing.
    pub fn is_busy(&self, session: &str) -> bool {
        self.store
            .reader(|c| {
                Ok::<_, StoreError>(c.query_row(
                    "SELECT EXISTS(SELECT 1 FROM runtime_sessions WHERE session_id=?1 AND (active_attempt IS NOT NULL OR phase IN ('opening','releasing','active','configuring')))
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
                    // a harness restarting for a configure counts: a drain waits for it
                    "SELECT (SELECT COUNT(*) FROM runtime_turns t JOIN runtime_sessions s USING(session_id) WHERE s.node_id=?1 AND t.state<>'finished') + (SELECT COUNT(*) FROM runtime_sessions WHERE node_id=?1 AND phase='configuring')",
                    [self.node_id.as_str()],
                    |r| r.get::<_, i64>(0),
                )?)
            })
            .unwrap_or(0) as usize
    }

    pub fn hold(&self, on: bool) {
        self.holding.store(on, Ordering::SeqCst);
    }

    /// The owner paused this Node: nothing new is planned for it until it is resumed. Kept apart
    /// from [`Self::hold`] (the Hub's release hold), so that one never lifts the other.
    pub fn set_paused(self: &Arc<Self>, on: bool) {
        if self.paused.swap(on, Ordering::SeqCst) == on || on {
            return;
        }
        // resumed: what waited in the queue meanwhile is planned now (once reconciled)
        if self.reconciled.load(Ordering::SeqCst) {
            let me = self.clone();
            match tokio::runtime::Handle::try_current() {
                Ok(h) => {
                    h.spawn_blocking(move || me.plan_waiting());
                }
                Err(_) => me.plan_waiting(),
            }
        }
    }

    pub fn is_holding(&self) -> bool {
        self.holding.load(Ordering::SeqCst)
            || self.paused.load(Ordering::SeqCst)
            || self.store.maintenance()
    }

    /// The Node this coordinator drives.
    pub fn node_id(&self) -> &NodeId {
        &self.node_id
    }

    /// The session's open turn has an outcome nobody can tell yet (reconciliation decides).
    /// A read error counts as uncertain: waiting is the safe answer.
    pub fn is_uncertain(&self, session: &str) -> bool {
        self.store
            .reader(|c| {
                Ok::<_, StoreError>(c.query_row(
                    "SELECT EXISTS(SELECT 1 FROM runtime_turns t LEFT JOIN runtime_attempts a ON a.turn_id=t.turn_id AND a.state<>'finished' WHERE t.session_id=?1 AND t.state<>'finished' AND (t.state='unknown' OR a.state='unknown'))",
                    [session],
                    |r| r.get::<_, bool>(0),
                )?)
            })
            .unwrap_or(true)
    }

    /// Asks the Node to cancel the attempt running now (frozen as the target here, never looked up
    /// again when the command goes out). `true`: there was one, the cancel is on its way.
    pub fn cancel(&self, session: &str) -> Result<bool, StoreError> {
        self.running()?;
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
            let state: String = tx.query_row(
                "SELECT state FROM runtime_attempts WHERE attempt_id=?",
                [&attempt],
                |r| r.get(0),
            )?;
            rt::cancel_attempt_in(tx, &attempt, rev, now())?;
            let start: Option<(String, bool)> = tx
                .query_row(
                    "SELECT state, maybe_sent FROM node_commands WHERE operation_id=? AND method='start_turn'",
                    [&target],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .optional()?;
            match start {
                // every send of it so far was refused before acceptance (or none was made): it
                // provably never ran, so it is settled here and nothing is sent
                Some((st, false)) if st == "pending" => {
                    return self.cancel_unsent_in(tx, session, &attempt, &target);
                }
                // it may be on its way or at the Node: a cancel now could reach the Node first and
                // be refused for a target it does not know yet. The cancel goes once the Node is
                // known to hold it (receipt_in); a send refused before acceptance settles it here
                // (send_one); a refusal ends it anyway. Once it started on the Node (its stream
                // said so), the Node holds it and the cancel goes now
                Some((st, true)) if st == "pending" && state == "planned" => return Ok(true),
                _ => {}
            }
            // what nobody can tell is running is given up, once the Node proves it stopped
            let method = if state == "unknown" { "abandon" } else { "cancel" };
            self.stop_command_in(tx, &attempt, &target, method)?;
            Ok(true)
        })?;
        if r {
            self.wake_send.notify_one();
            // a turn settled here is told from the apply worker, never under the caller's lock
            self.wake_apply.notify_one();
        }
        Ok(r)
    }

    /// Queues a stop of `attempt` (whose `start_turn` is `target`): one at a time; after one that
    /// failed or was refused, a new one (a new intent of the user, never the old one sent again).
    fn stop_command_in(
        &self,
        tx: &Tx,
        attempt: &str,
        target: &str,
        method: &str,
    ) -> Result<(), StoreError> {
        let (n, live): (i64, bool) = tx.query_row(
            "SELECT COUNT(*), COALESCE(MAX(state NOT IN ('finished','rejected') OR json_extract(receipt,'$.success')=1), 0) FROM node_commands WHERE method=? AND target_attempt=?",
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
                    reason: "the Hub gives up an attempt nobody can tell the end of".into(),
                },
            )?
        } else {
            command(
                &op,
                Cancel {
                    target,
                    reason: "the Hub cancels the turn".into(),
                },
            )?
        };
        rt::insert_command_in(
            tx,
            &rt::NewCommand {
                operation: &op,
                node: self.node_id.as_str(),
                method,
                request: &req,
                digest: &digest,
                target_attempt: Some(attempt),
                dependency: None,
            },
            now(),
        )?;
        self.wake_send.notify_one();
        Ok(())
    }

    /// [`Self::cancel`] of an attempt whose `start_turn` never left the Hub: the command is
    /// rejected locally, the attempt ends cancelled, its turn with it, and what is queued goes next.
    fn cancel_unsent_in(
        &self,
        tx: &Tx,
        session: &str,
        attempt: &str,
        op: &str,
    ) -> Result<bool, StoreError> {
        rt::merge_receipt_in(
            tx,
            op,
            ReceiptState::Rejected,
            false,
            &json!({ "reason": "cancelled before it was sent" }),
            now(),
        )?;
        let completion = Completion {
            outcome: Outcome::Cancelled,
            stop_reason: "cancelled".into(),
            error: None,
            result: vec![],
            continuation: None,
            transient: false,
            acted: false,
        };
        self.settle_in(tx, session, attempt, &completion)?;
        Ok(true)
    }

    /// Ends an attempt the Node provably never ran (or no longer knows) on the Hub's own word: its
    /// turn ends with it (never sent again), and what is queued goes next.
    fn settle_in(
        &self,
        tx: &Tx,
        session: &str,
        attempt: &str,
        completion: &Completion,
    ) -> Result<(), StoreError> {
        let (rev, turn): (i64, String) = tx.query_row(
            "SELECT revision, turn_id FROM runtime_attempts WHERE attempt_id=?",
            [attempt],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        rt::attempt_cas_in(
            tx,
            attempt,
            rev,
            "finished",
            None,
            Some(&serde_json::to_value(completion).map_err(conflict)?),
            now(),
        )?;
        self.close_cards_of_attempt_in(tx, attempt)?;
        self.turn_over_in(tx, session, &turn, completion)?;
        if self.plan_after_in(tx, session)? != Planned::Created {
            self.ended_later.lock().unwrap().push((
                session.to_string(),
                completion.stop_reason.clone(),
                completion.error.as_ref().map(|e| e.safe_detail.clone()),
            ));
        }
        Ok(())
    }

    // ── T2: sending ──

    /// Leases one command, sends it and merges what came back. False: nothing to send.
    async fn send_one(&self) -> bool {
        let lease = match self.tx(|tx| {
            rt::lease_command_in(
                tx,
                self.node_id.as_str(),
                &self.owner,
                now(),
                now() + LEASE_MS,
            )
        }) {
            Ok(Some(l)) => l,
            Ok(None) => return false,
            Err(e) => {
                tracing::error!("node command lease: {e}");
                return false;
            }
        };
        let sent = self.send(&lease).await;
        // a turn end to report, and a session whose configure was refused for a stale revision
        let r = self.tx(|tx| match &sent {
            Ok(receipt) => self.receipt_in(tx, receipt).map(|end| (end, None)),
            Err(e)
                if e.retry == RetryClass::SameOperation || e.code == ErrorCode::OutcomeUnknown =>
            {
                // not refused for good, or maybe accepted: the very same command again later
                let uncertain = e.retry != RetryClass::SameOperation;
                rt::retry_command_in(tx, &lease, now(), now() + 1_000, uncertain)?;
                // a start_turn the user stopped meanwhile, now refused before acceptance: it
                // provably never ran, so it ends here instead of going out again
                if !uncertain
                    && lease.method == "start_turn"
                    && let Some((attempt, session)) = cancelled_start_in(tx, &lease.operation)?
                {
                    self.cancel_unsent_in(tx, &session, &attempt, &lease.operation)?;
                }
                Ok((None, None))
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
                // refused before it was accepted: it certainly did not run
                let end = match lease.method.as_str() {
                    "start_turn" => self.refused_attempt_in(tx, &lease.operation, e)?,
                    "release_session" => {
                        let s = command_session_in(tx, &lease.operation)?;
                        self.released_in(tx, &s, false)?;
                        None
                    }
                    "open_session" => {
                        let s = command_session_in(tx, &lease.operation)?;
                        self.config_opened_in(tx, &s, &lease.operation, false)?;
                        self.opened_in(tx, &s, false, &e.safe_detail)?
                    }
                    "abandon" => self.abandon_failed_in(tx, &lease.operation, Some(e))?,
                    "resolve_permission" => {
                        self.answer_receipt_in(tx, &lease.operation, false, Some(e))?;
                        None
                    }
                    "configure_session" => {
                        return Ok((None, self.refused_configure_in(tx, &lease.operation, e)?));
                    }
                    _ => None,
                };
                Ok((end, None))
            }
        });
        match r {
            Ok((end, stale)) => {
                if let Some((s, stop, error)) = end {
                    self.domain.turn_ended(&s, &stop, error);
                }
                if let Some(session) = stale {
                    // a configure refused for a stale revision: sent again with the Node's own
                    let view = match SessionId::try_from(session.clone()) {
                        Ok(sid) => self.node.session(sid).await.ok(),
                        Err(_) => None,
                    };
                    if let Err(e) = self.tx(|tx| {
                        self.refresh_configure_in(tx, &session, &lease.operation, view.clone())
                    }) {
                        tracing::error!("refresh configure of {session}: {e}");
                    }
                }
            }
            Err(e) => tracing::error!("node command {}: {e}", lease.operation),
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
            "configure_session" => n.configure_session(parse(&l.request)?).await,
            "resolve_permission" => n.resolve_permission(parse(&l.request)?).await,
            "abandon" => n.abandon(parse(&l.request)?).await,
            other => Err(ApiError::new(
                ErrorCode::Unsupported,
                format!("the coordinator does not send {other}"),
            )),
        }
    }

    /// What the Node says about a command, from its answer or its stream (the same either way):
    /// merged forward, and an open that finished makes the session ready (or released).
    fn receipt_in(&self, tx: &Tx, receipt: &Receipt) -> Result<Option<End>, StoreError> {
        let (state, success) = receipt_state(&receipt.state);
        let op = receipt.operation.as_str();
        let Some(method) = tx
            .query_row(
                "SELECT method FROM node_commands WHERE operation_id=? AND node_id=?",
                [op, self.node_id.as_str()],
                |r| r.get::<_, String>(0),
            )
            .ok()
        else {
            return Ok(None);
        };
        let moved = rt::merge_receipt_in(
            tx,
            op,
            state,
            success,
            &serde_json::to_value(receipt).map_err(conflict)?,
            now(),
        )?;
        if !moved {
            return Ok(None);
        }
        if method == "resolve_permission" && state == ReceiptState::Finished {
            let error = match &receipt.state {
                Observed::Finished { error, .. } => error.clone(),
                _ => None,
            };
            self.answer_receipt_in(tx, op, success, error.as_ref())?;
        }
        if method == "configure_session" {
            self.configure_receipt_in(tx, op, state, success, receipt)?;
        }
        let detail = match &receipt.state {
            Observed::Finished { error: Some(e), .. } => e.safe_detail.clone(),
            Observed::Unknown { reason } => reason.clone(),
            _ => String::new(),
        };
        match (method.as_str(), state) {
            ("release_session", ReceiptState::Finished) => {
                let s = command_session_in(tx, op)?;
                self.released_in(tx, &s, success)?;
            }
            ("open_session", ReceiptState::Finished) => {
                let s = command_session_in(tx, op)?;
                self.config_opened_in(tx, &s, op, success)?;
                return self.opened_in(tx, &s, success, &detail);
            }
            ("abandon", ReceiptState::Finished) if !success => {
                let error = match &receipt.state {
                    Observed::Finished { error, .. } => error.clone(),
                    _ => None,
                };
                return self.abandon_failed_in(tx, op, error.as_ref());
            }
            ("start_turn", ReceiptState::Accepted | ReceiptState::Active) => {
                // a cancel held back while the start_turn's send was uncertain goes now
                if let Some((attempt, _)) = cancelled_start_in(tx, op)? {
                    let state: String = tx.query_row(
                        "SELECT state FROM runtime_attempts WHERE attempt_id=?",
                        [&attempt],
                        |r| r.get(0),
                    )?;
                    if state != "unknown" {
                        self.stop_command_in(tx, &attempt, op, "cancel")?;
                    }
                }
            }
            ("start_turn", ReceiptState::Unknown) => {
                // the Node cannot tell what the attempt did: it keeps its session until it is
                // settled, and is never sent again
                match tx.query_row(
                    "SELECT a.attempt_id, a.revision, t.session_id FROM runtime_attempts a JOIN runtime_turns t USING(turn_id) WHERE a.operation_id=? AND a.state<>'finished'",
                    [op],
                    |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)?, r.get::<_, String>(2)?)),
                ) {
                    Ok((attempt, rev, s)) => {
                        rt::attempt_cas_in(tx, &attempt, rev, "unknown", None, None, now())?;
                        system_in(
                            tx,
                            &s,
                            &format!(
                                "Node 无法确定这次执行的结果（{detail}），等待对账，不会自动重跑。点停止可放弃这次执行。"
                            ),
                        )?;
                    }
                    Err(rusqlite::Error::QueryReturnedNoRows) => {}
                    Err(e) => return Err(e.into()),
                }
            }
            ("open_session" | "release_session", ReceiptState::Unknown) => {
                // never sent again: the session waits (busy) until it is reconciled
                let s = command_session_in(tx, op)?;
                system_in(
                    tx,
                    &s,
                    &format!(
                        "Node 无法确定{}是否完成（{detail}），会话等待对账，不会自动重发。",
                        if method == "open_session" {
                            "打开会话"
                        } else {
                            "释放会话"
                        }
                    ),
                )?;
            }
            _ => {}
        }
        Ok(None)
    }

    /// A release is over: the session is let go, or (refused, failed) still open as it was. What
    /// came in meanwhile goes now (opening it again if it was let go).
    fn released_in(&self, tx: &Tx, s: &str, success: bool) -> Result<(), StoreError> {
        if phase_in(tx, s)?.is_none_or(|(p, _)| p != "releasing") {
            return Ok(());
        }
        let rev = session_revision(tx, s)?;
        rt::session_cas_in(
            tx,
            s,
            rev,
            &rt::SessionUpdate {
                phase: if success { "released" } else { "ready" },
                active_attempt: None,
                release_pending: false,
            },
            now(),
        )?;
        self.plan_after_in(tx, s)?;
        Ok(())
    }

    /// An open is over. Opened: a turn that waited for it (its attempt was refused because the
    /// Node had let the session go) goes as a new attempt; otherwise what is queued goes. Not
    /// opened: such a turn ends as failed, and the session stays let go (the next message tries
    /// again; nothing is retried by itself).
    fn opened_in(
        &self,
        tx: &Tx,
        s: &str,
        ok: bool,
        detail: &str,
    ) -> Result<Option<End>, StoreError> {
        if phase_in(tx, s)?.is_none_or(|(p, _)| p != "opening") {
            return Ok(None);
        }
        let waiting: Option<String> = tx
            .query_row(
                "SELECT turn_id FROM runtime_turns WHERE session_id=? AND state<>'finished' AND active_attempt IS NULL ORDER BY created LIMIT 1",
                [s],
                |r| r.get(0),
            )
            .optional()?;
        if ok {
            if let Some(turn) = waiting {
                let (spec, ordinal): (String, i64) = tx.query_row(
                    "SELECT spec, ordinal FROM runtime_attempts WHERE turn_id=? ORDER BY ordinal DESC LIMIT 1",
                    [&turn],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?;
                let spec: TurnSpec = serde_json::from_str(&spec)?;
                self.attempt_in(tx, s, &turn, ordinal + 1, spec.run.prompt)?;
                return Ok(None);
            }
            let rev = session_revision(tx, s)?;
            rt::session_cas_in(
                tx,
                s,
                rev,
                &rt::SessionUpdate {
                    phase: "ready",
                    active_attempt: None,
                    release_pending: false,
                },
                now(),
            )?;
            self.plan_after_in(tx, s)?;
            return Ok(None);
        }
        // the user hears why even when no turn waited (the queued message stays for the next try)
        let mut end = Some((
            s.to_string(),
            "error".to_string(),
            Some(format!(
                "the Node could not open the session again: {detail}"
            )),
        ));
        if let Some(turn) = waiting {
            let c = Completion {
                outcome: Outcome::Failed,
                stop_reason: "error".into(),
                error: Some(ApiError::new(
                    ErrorCode::Invalid,
                    format!("the Node could not open the session again: {detail}"),
                )),
                result: vec![],
                continuation: None,
                transient: false,
                acted: false,
            };
            self.turn_over_in(tx, s, &turn, &c)?;
            end = Some((
                s.to_string(),
                c.stop_reason.clone(),
                c.error.as_ref().map(|e| e.safe_detail.clone()),
            ));
        }
        let rev = session_revision(tx, s)?;
        rt::session_cas_in(
            tx,
            s,
            rev,
            &rt::SessionUpdate {
                phase: "released",
                active_attempt: None,
                release_pending: false,
            },
            now(),
        )?;
        system_in(
            tx,
            s,
            &format!("Node 没能打开这个会话（{detail}），下一条消息会再试。"),
        )?;
        Ok(end)
    }

    /// An abandon the Node refused or could not finish: the attempt stays unknown, its session
    /// held; the user may give it up again (a new abandon). Refused because the Node has no such
    /// operation: the Node holds no record of the attempt's start_turn, so it never ran there (the
    /// reconciliation that made it unknown saw the same); it ends as interrupted, never sent again.
    fn abandon_failed_in(
        &self,
        tx: &Tx,
        op: &str,
        e: Option<&ApiError>,
    ) -> Result<Option<End>, StoreError> {
        let row: Option<(String, String, String)> = tx
            .query_row(
                "SELECT t.session_id, a.attempt_id, a.state FROM node_commands c JOIN runtime_attempts a ON a.attempt_id=c.target_attempt JOIN runtime_turns t ON t.turn_id=a.turn_id WHERE c.operation_id=?",
                [op],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()?;
        let Some((s, attempt, state)) = row else {
            return Ok(None);
        };
        let detail = e.map(|e| e.safe_detail.clone()).unwrap_or_default();
        let unknown_to_node = e.is_some_and(|e| {
            e.code == ErrorCode::Invalid && e.safe_detail.contains("no such operation")
        });
        if unknown_to_node && state == "unknown" {
            let completion = Completion {
                outcome: Outcome::Interrupted,
                stop_reason: "error".into(),
                error: Some(ApiError::new(
                    ErrorCode::Invalid,
                    "the Node holds no record of this attempt: it did not run there",
                )),
                result: vec![],
                continuation: None,
                transient: false,
                acted: false,
            };
            self.settle_in(tx, &s, &attempt, &completion)?;
            system_in(
                tx,
                &s,
                "Node 没有这次执行的记录（它没有在 Node 上运行），已放弃，不会自动重跑。",
            )?;
            return Ok(None);
        }
        system_in(
            tx,
            &s,
            &format!("放弃这次执行没有成功（{detail}），会话仍在等待对账；可以再点一次停止。"),
        )?;
        Ok(None)
    }

    /// The Node refused an attempt before running it. Refused because the Node had let the
    /// session go: nothing ran, the session opens again and the same turn goes as a new attempt
    /// (once). Otherwise the attempt ends as failed, its turn with it.
    fn refused_attempt_in(
        &self,
        tx: &Tx,
        op: &str,
        e: &ApiError,
    ) -> Result<Option<End>, StoreError> {
        let row: Option<(String, i64, String, String)> = tx
            .query_row(
                "SELECT a.attempt_id, a.revision, a.turn_id, t.session_id FROM runtime_attempts a JOIN runtime_turns t USING(turn_id) WHERE a.operation_id=?",
                [op],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .optional()?;
        let Some((attempt, rev, turn, session)) = row else {
            return Ok(None);
        };
        let gone = e.code == ErrorCode::Invalid
            && (e.safe_detail.contains("open it again") || e.safe_detail.contains("not open"));
        let reopened_before: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM runtime_attempts WHERE turn_id=? AND json_extract(completion,'$.stop_reason')='released')",
            [&turn],
            |r| r.get(0),
        )?;
        let cancelled: bool = tx.query_row(
            "SELECT cancel_requested FROM runtime_attempts WHERE attempt_id=?",
            [&attempt],
            |r| r.get(0),
        )?;
        let reopen = gone && !reopened_before && !cancelled;
        let completion = Completion {
            outcome: Outcome::Failed,
            stop_reason: if reopen { "released" } else { "error" }.into(),
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
        self.close_cards_of_attempt_in(tx, &attempt)?;
        if reopen {
            // the turn keeps its messages and waits for the open (see opened_in)
            let trev = turn_revision(tx, &turn)?;
            let ordinal: i64 = tx.query_row(
                "SELECT retry_ordinal FROM runtime_turns WHERE turn_id=?",
                [&turn],
                |r| r.get(0),
            )?;
            rt::turn_cas_in(
                tx,
                &turn,
                trev,
                &rt::TurnUpdate {
                    state: "running",
                    active_attempt: None,
                    retry_ordinal: ordinal,
                    applied: false,
                },
                now(),
            )?;
            self.reopen_in(tx, &session)?;
            return Ok(None);
        }
        self.after_attempt_in(tx, &session, &turn, &completion)
    }

    // ── reconciling with the Node (Hub start) ──

    /// Whether turns may be planned: only once the Hub has reconciled with the Node.
    fn may_plan(&self) -> bool {
        self.reconciled.load(Ordering::SeqCst)
    }

    /// Whether the Hub has reconciled with its Node since it started (turns are planned).
    pub fn is_reconciled(&self) -> bool {
        self.may_plan()
    }

    /// The Node restarted (a new incarnation) while this Hub ran: nothing new is planned until the
    /// Hub has reconciled with it again (what it cannot tell the end of becomes `unknown`, never
    /// re-run); then what waited in the queue goes.
    pub async fn reconcile_again(self: &Arc<Self>) -> ApiResult<usize> {
        self.reconciled.store(false, Ordering::SeqCst);
        let n = self.reconcile().await?;
        if n > 0 {
            tracing::info!(
                "node {}: reconciled after its restart, {n} changed",
                self.node_id
            );
        }
        self.reconciled.store(true, Ordering::SeqCst);
        let me = self.clone();
        let _ = tokio::task::spawn_blocking(move || me.plan_waiting()).await;
        Ok(n)
    }

    /// What waited in the queue while the Hub was not reconciled goes now.
    fn plan_waiting(&self) {
        let sessions: Vec<String> = self
            .store
            .reader(|c| {
                let mut st = c.prepare(
                    "SELECT DISTINCT q.session_id FROM queued q JOIN runtime_sessions s ON s.session_id=q.session_id WHERE s.node_id=?",
                )?;
                let v = st
                    .query_map([self.node_id.as_str()], |r| r.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok::<_, StoreError>(v)
            })
            .unwrap_or_default();
        for s in sessions {
            if let Err(e) = self.tx(|tx| self.plan_after_in(tx, &s)) {
                tracing::error!("plan {s} after reconciling: {e}");
            }
        }
        self.wake_send.notify_one();
    }

    /// Compares the Hub's view with what the Node holds now (its ledger as of one moment, its
    /// sessions, its waiting permissions), before anything is sent. The Node may have restarted
    /// meanwhile (a local Node restarts with the Hub): what it cannot tell the end of becomes
    /// `unknown` (never sent again; a person gives it up), a session it let go is `released` (the
    /// next message opens it again), a card nobody waits for any more expires. Returns how many
    /// things changed.
    pub async fn reconcile(&self) -> ApiResult<usize> {
        let internal = |e: StoreError| ApiError::new(ErrorCode::Internal, e.to_string());
        // the Node's ledger, all pages of one snapshot
        let mut entries: HashMap<String, SnapshotEntry> = HashMap::new();
        let (mut cursor, mut restarts) = (None, 0);
        loop {
            match self
                .node
                .snapshot(PageRequest {
                    cursor: cursor.clone(),
                    limit: 200,
                })
                .await
            {
                Ok(page) => {
                    for e in page.entries {
                        entries.insert(e.receipt.operation.to_string(), e);
                    }
                    match page.next {
                        Some(c) => cursor = Some(c),
                        None => break,
                    }
                }
                Err(e) if e.code == ErrorCode::Expired && restarts < 3 => {
                    restarts += 1;
                    entries.clear();
                    cursor = None;
                }
                Err(e) => return Err(e),
            }
        }
        // the sessions as the Node holds them (None: it has no such session)
        let sessions: Vec<String> = self
            .store
            .reader(|c| {
                let mut st =
                    c.prepare("SELECT session_id FROM runtime_sessions WHERE node_id=?")?;
                let v = st
                    .query_map([self.node_id.as_str()], |r| r.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok::<_, StoreError>(v)
            })
            .map_err(internal)?;
        let mut views: Vec<(String, Option<SessionView>)> = vec![];
        for s in sessions {
            let id: SessionId = match SessionId::try_from(s.clone()) {
                Ok(x) => x,
                Err(_) => continue,
            };
            match self.node.session(id).await {
                Ok(v) => views.push((s, Some(v))),
                Err(e) if e.code == ErrorCode::Invalid => views.push((s, None)),
                Err(e) => return Err(e),
            }
        }
        let waiting: HashSet<String> = entries
            .values()
            .flat_map(|e| e.pending.iter().map(|r| r.id.to_string()))
            .collect();
        let mut ends: Vec<End> = vec![];
        let changed = self
            .tx(|tx| {
                let mut changed = 0;
                // 1. commands: what the Node says of each one in flight
                let cmds: Vec<(String, String, String, String)> = {
                    let mut st = tx.prepare(
                        "SELECT operation_id, state, method, request FROM node_commands WHERE node_id=? AND state IN ('pending','accepted','active','unknown') ORDER BY created, rowid",
                    )?;
                    st.query_map([self.node_id.as_str()], |r| {
                        Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
                    })?
                    .collect::<Result<Vec<_>, _>>()?
                };
                for (op, state, method, request) in cmds {
                    // the Node keeps the command's content hash as it computes it (method and
                    // business fields, not the id), not the Hub's hash of the stored envelope
                    let digest = serde_json::from_str::<Command<Value>>(&request)
                        .map_err(conflict)?
                        .digest(&method)
                        .map_err(conflict)?;
                    let unknown = |reason: &str| -> Result<Receipt, StoreError> {
                        Ok(Receipt {
                            operation: idv(op.clone())?,
                            state: Observed::Unknown {
                                reason: reason.into(),
                            },
                        })
                    };
                    let receipt = match entries.get(&op) {
                        Some(e) if e.digest == digest && e.method == method => e.receipt.clone(),
                        Some(_) => unknown("the Node holds another command under this id")?,
                        // never got there: the sender sends the very same command
                        None if state == "pending" => continue,
                        None => unknown("the Node has no record of it")?,
                    };
                    let before: String = tx.query_row(
                        "SELECT state FROM node_commands WHERE operation_id=?",
                        [&op],
                        |r| r.get(0),
                    )?;
                    if let Some(end) = self.receipt_in(tx, &receipt)? {
                        ends.push(end);
                    }
                    let after: String = tx.query_row(
                        "SELECT state FROM node_commands WHERE operation_id=?",
                        [&op],
                        |r| r.get(0),
                    )?;
                    if before != after {
                        changed += 1;
                    }
                }
                // 2. sessions the Node let go (a restart lets every one go, silently)
                for (s, view) in &views {
                    let gone = view
                        .as_ref()
                        .is_none_or(|v| v.phase == SessionPhase::Released);
                    if gone
                        && phase_in(tx, s)?.is_some_and(|(p, a)| p == "ready" && a.is_none())
                    {
                        let rev = session_revision(tx, s)?;
                        rt::session_cas_in(
                            tx,
                            s,
                            rev,
                            &rt::SessionUpdate {
                                phase: "released",
                                active_attempt: None,
                                release_pending: false,
                            },
                            now(),
                        )?;
                        changed += 1;
                    }
                }
                // 3. cards nobody waits for any more
                let cards: Vec<(String, String)> = {
                    let mut st = tx.prepare(
                        "SELECT d.id, d.subject_id FROM decisions d JOIN runtime_sessions s ON s.session_id=d.session_id WHERE s.node_id=? AND d.subject_type='tool_call' AND d.status='open'",
                    )?;
                    st.query_map([self.node_id.as_str()], |r| Ok((r.get(0)?, r.get(1)?)))?
                        .collect::<Result<Vec<_>, _>>()?
                };
                for (d, perm) in cards {
                    if waiting.contains(&perm) {
                        continue;
                    }
                    // a card with its row closes through it (an answer on its way was never applied)
                    if let Some(row) = rt::permission_of_decision_in(tx, &d)? {
                        if matches!(row.state.as_str(), "waiting" | "answering") {
                            self.close_in(tx, &row, "Node 已不再等待这个授权")?;
                            changed += 1;
                        }
                    } else if steve_store::decision_resolve_in(
                        tx,
                        &d,
                        &json!({"superseded": "Node 已不再等待这个授权"}),
                        "expired",
                    )? {
                        changed += 1;
                    }
                }
                Ok(changed)
            })
            .map_err(internal)?;
        for (s, stop, error) in ends {
            self.domain.turn_ended(&s, &stop, error);
        }
        self.wake_apply.notify_one();
        Ok(changed)
    }

    /// A `Released` view from before the Hub's last open of the session (that open's own view
    /// has not come on the stream yet): it says nothing of the session open now.
    fn stale_release_in(&self, tx: &Tx, session: &str, ev: &NodeEvent) -> Result<bool, StoreError> {
        let r: Option<String> = tx
            .query_row(
                "SELECT json_extract(receipt,'$.payload') FROM node_commands WHERE method='open_session' AND json_extract(request,'$.body.session')=? AND state='finished' AND json_extract(receipt,'$.success')=1 ORDER BY created DESC, rowid DESC LIMIT 1",
                [session],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        let Some(Ok(receipt)) = r.map(|r| serde_json::from_str::<Receipt>(&r)) else {
            return Ok(false);
        };
        let Observed::Finished {
            result: Some(OperationResult::Session(opened)),
            ..
        } = receipt.state
        else {
            return Ok(false);
        };
        let seen: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM node_event_inbox WHERE node_id=?1 AND stream=?2 AND (epoch<>?3 OR seq<?4)
               AND json_extract(payload,'$.body.type')='session_changed' AND json_extract(payload,'$.body.view.id')=?5
               AND json_extract(payload,'$.body.view.revision')=?6)",
            rusqlite::params![
                self.node_id.as_str(),
                ev.cursor.stream.as_str(),
                ev.cursor.epoch.as_str(),
                ev.cursor.seq as i64,
                session,
                opened.revision.as_str()
            ],
            |r| r.get(0),
        )?;
        Ok(!seen)
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

    /// Applies what is ready on every dirty stream. False: nothing was. Each event is one
    /// transaction; what the Hub does about it (statistics, YOLO, the turn's end) follows that
    /// event's commit, not the end of the whole pass.
    fn apply_some(&self) -> bool {
        let streams: Vec<(String, String)> = self.dirty.lock().unwrap().iter().cloned().collect();
        let node = self.node_id.to_string();
        let mut any = false;
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
                    let mut ask: Vec<After> =
                        self.permission_event_in(tx, &ev)?.into_iter().collect();
                    for a in &ask {
                        // in memory only, and before the card's commit is seen: the YOLO
                        // engine has the tool's input from the first look at the card
                        if let After::Asked(a) = a {
                            self.domain
                                .permission_arming(&a.session, &a.decision, &a.request);
                        }
                    }
                    ask.extend(self.facts_in(tx, &ev)?);
                    let out = self.apply_in(tx, &ev)?;
                    rt::mark_applied_in(tx, k, p.seq)?;
                    Ok(Some((out, ask)))
                });
                match r {
                    Ok(Some((end, ask))) => {
                        any = true;
                        self.after(ask);
                        if let Some((session, stop, error)) = end {
                            self.domain.turn_ended(&session, &stop, error);
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
        // turn ends settled outside this worker (a local cancel), told after their commit
        let later: Vec<_> = std::mem::take(&mut *self.ended_later.lock().unwrap());
        for (session, stop, error) in later {
            self.domain.turn_ended(&session, &stop, error);
        }
        if any {
            self.wake_send.notify_one();
        }
        any
    }

    /// The Hub's side of applied events, after their commit.
    fn after(&self, asked: Vec<After>) {
        for a in asked {
            match a {
                After::Asked(a) => {
                    self.domain
                        .permission_asked(&a.session, &a.decision, &a.request)
                }
                After::Started {
                    session,
                    attempt,
                    ordinal,
                    user_seq,
                    at,
                } => self
                    .domain
                    .attempt_started(&session, &attempt, ordinal, user_seq, at),
                After::Finished {
                    session,
                    attempt,
                    completion,
                    at,
                } => {
                    let facts = self.attempt_facts(&session, &attempt);
                    self.domain
                        .attempt_finished(&session, &attempt, &completion, &facts, at);
                }
                After::Cost { session } => self.domain.cost_changed(&session),
            }
        }
    }

    /// Drops applied Node events past [`INBOX_KEEP_MS`] that nothing reads again: kept are those
    /// of an attempt still open, a permission request whose card is open, and per stream the last
    /// resume handle of a session and the last cumulative cost of a harness process.
    pub fn prune_inbox(&self, at: i64) {
        loop {
            let r = self.tx(|tx| {
                Ok(tx.execute(
                    PRUNE_INBOX_SQL,
                    rusqlite::params![self.node_id.as_str(), at - INBOX_KEEP_MS, PRUNE_BATCH],
                )?)
            });
            match r {
                Ok(n) if n as i64 == PRUNE_BATCH => continue,
                Ok(_) => return,
                Err(e) => {
                    tracing::error!("prune the node inbox: {e}");
                    return;
                }
            }
        }
    }

    /// What an event means for the session row and for statistics, in `tx` (before it is applied,
    /// so an attempt's start or end counts once): the context and cost it reports, the model in
    /// force, and the attempts that begin and end.
    fn facts_in(&self, tx: &Tx, ev: &NodeEvent) -> Result<Vec<After>, StoreError> {
        let Some(session) = ev_session(ev) else {
            return Ok(vec![]);
        };
        let mut out = vec![];
        match &ev.body {
            EventBody::Usage { report } if report.scope != UsageScope::Background => {
                let cost = report
                    .cost
                    .as_ref()
                    .and_then(|m| m.decimal.parse::<f64>().ok());
                let delta = match cost {
                    Some(c) if report.measurement == Measurement::Cumulative => {
                        let before = earlier_cost_in(tx, &self.node_id, ev, &report.meter_epoch)?;
                        Some(match before {
                            Some(b) if b <= c => c - b,
                            // a lower total: the harness counted from zero again
                            _ => c,
                        })
                    }
                    Some(c) => Some(c),
                    None => None,
                };
                tx.execute(
                    "UPDATE sessions SET used = COALESCE(?, used), size = COALESCE(?, size), cost = CASE WHEN ? IS NULL THEN cost ELSE COALESCE(cost, 0) + ? END, updated = ? WHERE id = ?",
                    rusqlite::params![
                        report.context_used.map(|x| x as i64),
                        report.context_limit.map(|x| x as i64),
                        delta,
                        delta,
                        now(),
                        session
                    ],
                )?;
                tx.publish(steve_store::ChangeEvent::Session {
                    id: session.clone(),
                    item_id: None,
                });
                if delta.is_some_and(|d| d > 0.0) {
                    out.push(After::Cost { session });
                }
            }
            EventBody::SessionChanged { view } => {
                // the Node's revision of the session, kept here (the inbox it came in is pruned)
                // keyed on the event's subject and this Node, never on the id the body gives
                if view.id.as_str() != session {
                    tracing::warn!(session, view = %view.id, "session view of another session");
                    return Ok(out);
                }
                tx.execute(
                    "UPDATE runtime_sessions SET node_revision=? WHERE session_id=? AND node_id=?",
                    [
                        view.revision.as_str(),
                        session.as_str(),
                        self.node_id.as_str(),
                    ],
                )?;
                if view.phase == SessionPhase::Released
                    && !self.stale_release_in(tx, &session, ev)?
                {
                    let (phase, active): (String, Option<String>) = tx.query_row(
                        "SELECT phase, active_attempt FROM runtime_sessions WHERE session_id=?",
                        [&session],
                        |r| Ok((r.get(0)?, r.get(1)?)),
                    )?;
                    if phase == "ready" && active.is_none() {
                        let rev = session_revision(tx, &session)?;
                        rt::session_cas_in(
                            tx,
                            &session,
                            rev,
                            &rt::SessionUpdate {
                                phase: "released",
                                active_attempt: None,
                                release_pending: false,
                            },
                            now(),
                        )?;
                    }
                }
                tx.execute(
                    "UPDATE sessions SET model = COALESCE(?, model), reasoning_effort = COALESCE(?, reasoning_effort), updated = ? WHERE id = ?",
                    rusqlite::params![view.effective.model, view.effective.effort, now(), session],
                )?;
                tx.publish(steve_store::ChangeEvent::Session {
                    id: session,
                    item_id: None,
                });
            }
            EventBody::AttemptStarted => {
                if let Subject::Execution(x) = &ev.subject
                    && let Some((_, state)) = attempt_row(tx, x.attempt().as_str())?
                    && state == "planned"
                {
                    let ordinal: i64 = tx.query_row(
                        "SELECT ordinal FROM runtime_attempts WHERE attempt_id=?",
                        [x.attempt().as_str()],
                        |r| r.get(0),
                    )?;
                    // the turn's first message: the first `user` entry since the last turn ended
                    let user_seq: Option<i64> = tx.query_row(
                        "SELECT MIN(seq) FROM events WHERE session_id=?1 AND kind='user' AND seq > (SELECT COALESCE(MAX(seq), 0) FROM events WHERE session_id=?1 AND kind='turn_end')",
                        [&session],
                        |r| r.get(0),
                    )?;
                    out.push(After::Started {
                        session,
                        attempt: x.attempt().to_string(),
                        ordinal,
                        user_seq: user_seq.filter(|_| ordinal == 0),
                        at: ev.at.0,
                    });
                }
            }
            EventBody::AttemptEnded { completion } => {
                if let Subject::Execution(x) = &ev.subject
                    && let Some((_, state)) = attempt_row(tx, x.attempt().as_str())?
                    && state != "finished"
                {
                    out.push(After::Finished {
                        session,
                        attempt: x.attempt().to_string(),
                        completion: completion.clone(),
                        at: ev.at.0,
                    });
                }
            }
            _ => {}
        }
        Ok(out)
    }

    /// What one attempt did, for its statistics row: from the Node's own events of it.
    fn attempt_facts(&self, session: &str, attempt: &str) -> AttemptFacts {
        self.store
            .reader(|c| self.attempt_facts_on(c, session, attempt))
            .unwrap_or_else(|e| {
                tracing::error!("facts of attempt {attempt}: {e}");
                AttemptFacts::default()
            })
    }

    fn attempt_facts_on(
        &self,
        conn: &rusqlite::Connection,
        session: &str,
        attempt: &str,
    ) -> Result<AttemptFacts, StoreError> {
        let rows: Vec<String> = {
            let mut st = conn.prepare(
                "SELECT payload FROM node_event_inbox WHERE node_id=? AND json_extract(payload,'$.subject.attempt')=? AND json_extract(payload,'$.subject.session')=? ORDER BY stream, seq",
            )?;
            st.query_map(
                rusqlite::params![self.node_id.as_str(), attempt, session],
                |r| r.get::<_, String>(0),
            )?
            .collect::<Result<Vec<_>, _>>()?
        };
        let mut f = AttemptFacts::default();
        let mut tools: std::collections::HashMap<String, ToolView> = Default::default();
        // the cumulative cost per harness process: what it stood at before this attempt, then now
        let mut last: std::collections::HashMap<String, f64> = Default::default();
        for r in rows {
            let Ok(ev) = serde_json::from_str::<NodeEvent>(&r) else {
                continue;
            };
            if let EventBody::Usage { report } = &ev.body
                && report.scope != UsageScope::Background
                && let Some(c) = report
                    .cost
                    .as_ref()
                    .and_then(|m| m.decimal.parse::<f64>().ok())
            {
                let delta = if report.measurement == Measurement::Cumulative {
                    let epoch = report.meter_epoch.to_string();
                    let before = match last.get(&epoch) {
                        Some(b) => Some(*b),
                        None => earlier_cost_in(conn, &self.node_id, &ev, &report.meter_epoch)?,
                    };
                    last.insert(epoch, c);
                    match before {
                        Some(b) if b <= c => c - b,
                        _ => c,
                    }
                } else {
                    c
                };
                f.cost = Some(f.cost.unwrap_or(0.0) + delta);
            }
            match ev.body {
                EventBody::ToolChanged { tool } => {
                    tools.insert(tool.id.to_string(), tool);
                }
                EventBody::PermissionRequested { .. } => f.permissions += 1,
                EventBody::Usage { report } if report.scope != UsageScope::Background => {
                    if report.model.is_some() {
                        f.model = report.model.clone();
                    }
                    f.usage.push(report);
                }
                _ => {}
            }
        }
        for t in tools.values() {
            f.tools += 1;
            if t.phase == ToolPhase::Failed {
                f.tool_errors += 1;
            }
            let bucket = match t.name.strip_prefix("mcp__steve__") {
                Some(n) => format!("mcp:{n}"),
                None => serde_json::to_value(t.category)
                    .ok()
                    .and_then(|v| v.as_str().map(String::from))
                    .unwrap_or_else(|| "other".into()),
            };
            *f.tool_kinds.entry(bucket).or_default() += 1;
            if !t.name.is_empty() && !t.name.starts_with("mcp__steve__") {
                *f.tool_kinds.entry(format!("tool:{}", t.name)).or_default() += 1;
            }
        }
        Ok(f)
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
            EventBody::OperationChanged { receipt } => end = self.receipt_in(tx, receipt)?,
            EventBody::AttemptStarted => {
                if let Subject::Execution(x) = &ev.subject
                    && let Some((rev, state)) = attempt_row(tx, x.attempt().as_str())?
                    // planned -> running only: an attempt reconciled as unknown meanwhile stays
                    // unknown until it ends or is settled (Stop abandons it, never cancels)
                    && state == "planned"
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
                self.close_cards_of_attempt_in(tx, attempt)?;
                let (turn, s): (String, String) = tx.query_row(
                    "SELECT a.turn_id, t.session_id FROM runtime_attempts a JOIN runtime_turns t USING(turn_id) WHERE a.attempt_id=?",
                    [attempt],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )?;
                // the receipt the Node gives this start_turn from now on (unless the Node's own
                // final receipt came first: terminal receipts are never rewritten)
                let op = op_of_attempt(tx, attempt)?;
                let terminal: bool = tx.query_row(
                    "SELECT state IN ('finished','rejected') FROM node_commands WHERE operation_id=?",
                    [&op],
                    |r| r.get(0),
                )?;
                if !terminal {
                    self.receipt_in(
                        tx,
                        &Receipt {
                            operation: idv(op)?,
                            state: Observed::Finished {
                                outcome: completion.outcome,
                                result: Some(OperationResult::Execution(completion.clone())),
                                error: completion.error.clone(),
                            },
                        },
                    )?;
                }
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
        let role = match &ev.body {
            EventBody::TextDelta { message, .. } => role_of_in(tx, session, message)?,
            _ => None,
        };
        let Some(entry) = steve_transcript::of_node(&ev.body, role) else {
            return Ok(());
        };
        rt::add_event_in(
            tx,
            session,
            &entry.kind,
            entry.data,
            V1,
            Some(rt::EventSource {
                node: self.node_id.as_str(),
                stream: ev.cursor.stream.as_str(),
                epoch: ev.cursor.epoch.as_str(),
                seq: ev.cursor.seq as i64,
            }),
            ev.at.0,
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
        // a retry carries the config of its first attempt: never once a change is waiting
        let waits = retry && config::desired_in(tx, session)?.is_some();
        if waits {
            rt::add_event_in(
                tx,
                session,
                "system",
                json!({ "text": "配置变更待生效，本次失败不自动重试" }),
                V1,
                None,
                now(),
            )?;
        }
        if retry && !waits {
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
            let text = format!(
                "推理网关暂时不可用，{} 秒后自动重试（第 {} 次）",
                delay / 1000,
                ordinal + 1
            );
            rt::add_event_in(
                tx,
                session,
                "system",
                json!({ "text": text }),
                V1,
                None,
                now(),
            )?;
            return Ok(None);
        }
        self.turn_over_in(tx, session, turn, c)?;
        // what is queued meanwhile goes next; only when nothing does is the turn over for the item
        if self.plan_after_in(tx, session)? == Planned::Created {
            return Ok(None);
        }
        // a session the business side ended is let go once its last turn is over (the release it
        // asked for while the turn ran could not be done then)
        let ended: bool = tx
            .query_row(
                "SELECT status='ended' FROM sessions WHERE id=?",
                [session],
                |r| r.get(0),
            )
            .optional()?
            .unwrap_or(false);
        if ended {
            self.release_in(tx, session)?;
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
        rt::add_event_in(
            tx,
            session,
            "turn_end",
            json!({ "stopReason": c.stop_reason }),
            V1,
            None,
            now(),
        )?;
        // an abandoned attempt takes its harness with it: the Node has let the session go, so
        // the next message opens it again rather than starting a turn on a released session
        let abandoned: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM node_commands c JOIN runtime_attempts a ON a.attempt_id = c.target_attempt WHERE a.turn_id=? AND c.method='abandon')",
            [turn],
            |r| r.get(0),
        )?;
        let srev = session_revision(tx, session)?;
        rt::session_cas_in(
            tx,
            session,
            srev,
            &rt::SessionUpdate {
                phase: if abandoned { "released" } else { "ready" },
                active_attempt: None,
                release_pending: false,
            },
            now(),
        )?;
        Ok(())
    }
}

/// The option ids of a Node permission card.
const ALLOW: &str = "allow_once";
const REJECT: &str = "reject";

/// What one attempt did, as its statistics row counts it.
#[derive(Debug, Clone, Default)]
pub struct AttemptFacts {
    pub tools: i64,
    pub tool_errors: i64,
    /// Per tool category (`exec`, `edit`…) and `mcp:<tool>` for Steve's own; besides those
    /// `tool:<name>` per harness tool name.
    pub tool_kinds: std::collections::BTreeMap<String, i64>,
    pub permissions: i64,
    /// The attempt's usage reports (main scope), in order.
    pub usage: Vec<UsageReport>,
    pub model: Option<String>,
    /// The attempt's share of the harness's own dollar figure, if it reported one.
    pub cost: Option<f64>,
}

/// What the business side hears after a commit.
enum After {
    Asked(Box<Asked>),
    Started {
        session: String,
        attempt: String,
        ordinal: i64,
        user_seq: Option<i64>,
        at: i64,
    },
    Finished {
        session: String,
        attempt: String,
        completion: Completion,
        at: i64,
    },
    Cost {
        session: String,
    },
}

/// The cumulative cost the same harness process reported last before `ev` in its session.
fn earlier_cost_in(
    tx: &rusqlite::Connection,
    node: &NodeId,
    ev: &NodeEvent,
    epoch: &Epoch,
) -> Result<Option<f64>, StoreError> {
    let r: Option<String> = match tx.query_row(
        "SELECT json_extract(payload,'$.body.report.cost.decimal') FROM node_event_inbox
           WHERE node_id=? AND stream=? AND epoch=? AND seq<? AND json_extract(payload,'$.body.type')='usage'
             AND json_extract(payload,'$.body.report.meter_epoch')=? AND json_extract(payload,'$.body.report.cost') IS NOT NULL
           ORDER BY seq DESC LIMIT 1",
        rusqlite::params![
            node.as_str(),
            ev.cursor.stream.as_str(),
            ev.cursor.epoch.as_str(),
            ev.cursor.seq as i64,
            epoch.as_str()
        ],
        |r| r.get(0),
    ) {
        Ok(x) => x,
        Err(rusqlite::Error::QueryReturnedNoRows) => None,
        Err(e) => return Err(e.into()),
    };
    Ok(r.and_then(|d| d.parse().ok()))
}

/// A permission card made while applying, for the business side (YOLO) after the commit.
struct Asked {
    session: String,
    decision: String,
    request: PermissionRequest,
}

fn ev_session(ev: &NodeEvent) -> Option<String> {
    match &ev.subject {
        Subject::Session { session } => Some(session.to_string()),
        Subject::Execution(x) => exec_session(x),
        _ => None,
    }
}

fn exec_session(x: &ExecutionRef) -> Option<String> {
    match x {
        ExecutionRef::Turn { session, .. } => Some(session.to_string()),
        _ => None,
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

/// The format of the events this coordinator writes: Steve's own, never a harness protocol.
const V1: &str = "steve.v1";

/// Queues a message (with its `queued` event) at `created`.
fn queue_in(
    tx: &Tx,
    session: &str,
    text: &str,
    via: &str,
    files: &[FileRef],
    created: i64,
) -> Result<(), StoreError> {
    tx.execute(
        "INSERT INTO queued (id, session_id, text, via, created, files) VALUES (?, ?, ?, ?, ?, ?)",
        rusqlite::params![
            uid("q"),
            session,
            text,
            via,
            created,
            serde_json::to_string(files)?
        ],
    )?;
    rt::add_event_in(
        tx,
        session,
        "queued",
        message_event(text, via, files),
        V1,
        None,
        now(),
    )?;
    tx.publish(steve_store::ChangeEvent::Session {
        id: session.into(),
        item_id: None,
    });
    Ok(())
}

/// A message as the session's events show it (the same shape on every backend).
fn message_event(text: &str, via: &str, files: &[FileRef]) -> Value {
    let mut v = json!({ "text": text, "via": via });
    if !files.is_empty() {
        v["files"] = json!(files);
    }
    v
}

/// An attachment for the Node to fetch from the Hub by hash.
fn file_part(tx: &Tx, f: &FileRef) -> Result<Part, StoreError> {
    let (sha, size, mime): (String, i64, String) = tx.query_row(
        "SELECT sha256, size, mime FROM files WHERE id=?",
        [&f.id],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    Ok(Part::File {
        artifact: ArtifactRef {
            id: idv(f.id.clone())?,
            sha256: idv(if sha.starts_with("sha256:") {
                sha
            } else {
                format!("sha256:{sha}")
            })?,
            size: size.max(0) as u64,
            mime,
        },
        name: f.name.clone(),
    })
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

/// The session's phase and active attempt; `None`: not bound.
fn phase_in(tx: &Tx, session: &str) -> Result<Option<(String, Option<String>)>, StoreError> {
    Ok(tx
        .query_row(
            "SELECT phase, active_attempt FROM runtime_sessions WHERE session_id=?",
            [session],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?)
}

/// The session an open or a release command is for.
fn command_session_in(tx: &Tx, op: &str) -> Result<String, StoreError> {
    Ok(tx.query_row(
        "SELECT CASE method WHEN 'release_session' THEN json_extract(request,'$.body') ELSE json_extract(request,'$.body.session') END FROM node_commands WHERE operation_id=?",
        [op],
        |r| r.get(0),
    )?)
}

/// A `system` entry of the session, for the people reading it.
fn system_in(tx: &Tx, session: &str, text: &str) -> Result<(), StoreError> {
    rt::add_event_in(
        tx,
        session,
        "system",
        json!({ "text": text }),
        V1,
        None,
        now(),
    )?;
    Ok(())
}

/// The session a decision belongs to.
pub fn decision_session(store: &Store, decision: &str) -> Option<String> {
    store
        .reader(|c| {
            Ok::<_, StoreError>(c.query_row(
                "SELECT session_id FROM decisions WHERE id=?",
                [decision],
                |r| r.get::<_, Option<String>>(0),
            )?)
        })
        .ok()
        .flatten()
}

/// Whether `session` is bound to a Node (whether or not that Node is enabled in this process).
pub fn bound(store: &Store, session: &str) -> bool {
    // when unsure, as bound: never hand it to another backend
    bound_strict(store, session).unwrap_or(true)
}

/// [`bound`], with a read error as an error: for a decision that must not rest on a guess.
pub fn bound_strict(store: &Store, session: &str) -> Result<bool, StoreError> {
    store.reader(|c| {
        Ok::<_, StoreError>(c.query_row(
            "SELECT EXISTS(SELECT 1 FROM runtime_sessions WHERE session_id=?)",
            [session],
            |r| r.get::<_, bool>(0),
        )?)
    })
}

/// A message for a Node-bound session while its Node is not enabled here (STEVE_NODE_MODE off): it is
/// queued, never given to another backend, and goes once the Node is back and reconciled.
pub fn park(
    store: &Store,
    session: &str,
    text: &str,
    via: &str,
    files: &[FileRef],
) -> Result<(), StoreError> {
    store.transaction(|tx| {
        queue_in(tx, session, text, via, files, now())?;
        let told: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM events WHERE session_id=?1 AND kind='system' AND json_extract(data,'$.parked')=1 AND seq > (SELECT COALESCE(MAX(seq),0) FROM events WHERE session_id=?1 AND kind IN ('user','turn_end')))",
            [session],
            |r| r.get(0),
        )?;
        if !told {
            rt::add_event_in(
                tx,
                session,
                "system",
                json!({ "text": "该会话绑定在 Node 上，但 Node 未启用（STEVE_NODE_MODE），消息已排队，不会交给旧运行器。", "parked": true }),
                V1,
                None,
                now(),
            )?;
        }
        Ok(())
    })
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

/// The role of a message, from the `message` entry that began it.
fn role_of_in(tx: &Tx, session: &str, message: &str) -> Result<Option<Role>, StoreError> {
    let r: Option<String> = match tx.query_row(
        "SELECT json_extract(data,'$.role') FROM events WHERE session_id=? AND kind='message' AND format='steve.v1' AND json_extract(data,'$.message')=? ORDER BY seq DESC LIMIT 1",
        [session, message],
        |r| r.get(0),
    ) {
        Ok(x) => x,
        Err(rusqlite::Error::QueryReturnedNoRows) => None,
        Err(e) => return Err(e.into()),
    };
    Ok(r.and_then(|r| serde_json::from_value(Value::String(r)).ok()))
}

/// Where the session's conversation continues: the resume handle of the Node's last view of it.
fn resume_of_in(tx: &Tx, node: &NodeId, session: &str) -> Result<Option<ResumeHandle>, StoreError> {
    let r: Option<String> = match tx.query_row(
        "SELECT json_extract(payload,'$.body.view.resume') FROM node_event_inbox
           WHERE node_id=? AND json_extract(payload,'$.body.type')='session_changed' AND json_extract(payload,'$.body.view.id')=?
             AND json_extract(payload,'$.body.view.resume') IS NOT NULL
           ORDER BY received DESC, seq DESC LIMIT 1",
        [node.as_str(), session],
        |r| r.get(0),
    ) {
        Ok(x) => x,
        Err(rusqlite::Error::QueryReturnedNoRows) => None,
        Err(e) => return Err(e.into()),
    };
    Ok(r.and_then(|h| ResumeHandle::try_from(h).ok()))
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
