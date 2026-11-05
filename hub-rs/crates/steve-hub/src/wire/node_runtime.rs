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
//! On start, before anything is sent, the Hub reconciles with the Node (its stream heads, its
//! ledger, its sessions, its waiting permissions): what the Hub missed of the Node's events is
//! replayed and applied first (`heads`), what the Node cannot tell the end of becomes `unknown` and
//! keeps its session until a person gives it up; it is never sent again.
//!
//! What it does not do yet (tidy chains, set_mode) is refused explicitly, never reported as done.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use async_trait::async_trait;
use rusqlite::OptionalExtension;
use serde_json::{Value, json};
use steve_node_api::*;
use steve_store::runtime::{self as rt, ReceiptState, StreamKey};
use steve_store::{FileRef, Store, StoreError, Tx};

mod config;
mod heads;
mod perm;
mod recover;

pub use config::{ConfigChange, Configured};
pub use heads::{
    CaughtUp, NodeSnapshot, Pull, Receive, ReplayCheck, catch_up_streams, read_snapshot,
};

/// Retries of a turn after a passing failure (gateway 5xx) with no tool run, and their waits.
const RETRY_DELAYS_MS: [i64; 2] = [20_000, 60_000];
/// How long a command may wait to be accepted by the Node.
const ACCEPT_WINDOW_MS: i64 = 10 * 60_000;
/// A worker's hold on a command it sends.
const LEASE_MS: i64 = 60_000;

/// A turn's end for the business side, told after its commit.
#[derive(Debug)]
struct End {
    session: String,
    /// The turn's last attempt (after gateway retries, the retry that ended it); `None` for a
    /// failed open with no turn waiting on it.
    attempt: Option<String>,
    stop: String,
    error: Option<String>,
}

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
    /// lifecycle. Called once per turn, after its end is committed. `attempt`: the turn's last
    /// attempt (after gateway retries, the retry that ended it); `None` when an open failed with
    /// no turn waiting on it.
    fn turn_ended(
        &self,
        session: &str,
        attempt: Option<&str>,
        stop_reason: &str,
        error: Option<String>,
    );
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
    /// Sessions with Node work the Hub has no record of (found by the last reconcile): nothing is
    /// planned for them, and that work is never given up on its own (D7).
    held: Mutex<HashSet<String>>,
    /// Sessions whose stream reconcile stopped waiting for ([`heads::APPLY_WAIT`]): nothing is
    /// planned for them until the apply worker has caught up on it.
    lagging: Mutex<HashMap<String, heads::Lag>>,
    /// How long reconcile waits for the Node's heads to be applied, in ms.
    apply_wait_ms: AtomicU64,
    /// What a remote Node's replayed batches pass before the inbox ([`Self::over_link`]).
    replay_check: OnceLock<ReplayCheck>,
    /// The largest replay page the Node's transport carries (unbounded in process).
    largest_replay: AtomicU32,
    /// Turn ends settled outside the apply worker (a local cancel), told by it after the commit.
    ended_later: Mutex<Vec<End>>,
    /// Attempts the Hub settled on its own word (no AttemptEnded comes for them): their
    /// statistics row ends after the commit, as one the Node reported would.
    settled_later: Mutex<Vec<After>>,
    /// What the open transaction settled (see [`Self::settle_in`]): moved to `settled_later` and
    /// `ended_later` only once it committed, so the apply worker never sees it earlier (it would
    /// book a row the store does not show finished yet) and never sees one rolled back.
    staged: Mutex<(Vec<After>, Vec<End>)>,
    /// Held across [`Self::tx`]'s transaction and the move of what it settled, so a transaction
    /// that starts after another committed finds that one's settles already queued (turn ends are
    /// told in commit order).
    commit: Mutex<()>,
    /// Streams with events to apply.
    dirty: Mutex<HashSet<(String, String)>>,
    /// Why this Node cannot run anything (its host failed to start): its sessions then refuse
    /// work instead of queueing commands nobody sends.
    stopped: Mutex<Option<String>>,
    /// How long an attempt (and so a permission it asks) may take, in ms. 6 h unless the host
    /// sets it ([`Self::set_turn_limit`]).
    turn_limit: std::sync::atomic::AtomicI64,
    /// How long an open session may sit idle before its harness is let go, in ms
    /// ([`Self::set_idle_release`]; legacy's 10 minutes by default).
    idle_release: std::sync::atomic::AtomicI64,
    /// New sessions left on the legacy runner since boot although this Node was on (no binding
    /// could be planned, or the bind failed): `/healthz` shows them, so they don't hide.
    fallbacks: std::sync::atomic::AtomicU64,
}

/// What a session with Node work the Hub has no record of reads.
const STRANGER: &str = "Node 上有这个会话尚未结束的工作，但 Hub 没有它的记录（Hub 的数据可能回到了较早的状态）；该会话暂停，不会发出新的指令，那项工作也不会被自动放弃";
/// How long applied Node events stay in the inbox (what is still needed stays longer).
const INBOX_KEEP_MS: i64 = 7 * 24 * 3600 * 1000;
/// How often the apply worker prunes, and how many rows at most per transaction.
const PRUNE_EVERY: Duration = Duration::from_secs(600);
const PRUNE_BATCH: i64 = 500;
/// The `retry_at` of a release queued for when its session's turn is over: never leased until
/// [`NodeRuntime::release_in`] sends it.
const AFTER_TURN: i64 = i64::MAX;
/// An attempt's deadline by default.
const TURN_LIMIT_MS: i64 = 6 * 3_600_000;
/// How long a session stays open with nothing to do, by default (legacy's idle kill).
const IDLE_RELEASE_MS: i64 = 600_000;
/// How often the apply worker looks for idle sessions.
const IDLE_EVERY: Duration = Duration::from_secs(1);
/// The sessions of node ?1 idle since before ?2 (ms), all of them or only ?3: open with nothing
/// running, no turn open or over since then, nothing queued and no open card.
const IDLE_SESSIONS_SQL: &str = "SELECT s.session_id FROM runtime_sessions s
   WHERE s.node_id=?1 AND s.phase='ready' AND s.active_attempt IS NULL AND s.updated<=?2
     AND (?3 IS NULL OR s.session_id=?3)
     AND NOT EXISTS (SELECT 1 FROM runtime_turns t WHERE t.session_id=s.session_id
           AND (t.state<>'finished' OR t.updated>?2))
     AND NOT EXISTS (SELECT 1 FROM queued q WHERE q.session_id=s.session_id)
     AND NOT EXISTS (SELECT 1 FROM decisions d WHERE d.session_id=s.session_id
           AND d.subject_type='tool_call' AND d.status='open')
   ORDER BY s.updated LIMIT 64";
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
            held: Mutex::new(HashSet::new()),
            lagging: Mutex::new(HashMap::new()),
            apply_wait_ms: AtomicU64::new(heads::APPLY_WAIT.as_millis() as u64),
            replay_check: OnceLock::new(),
            largest_replay: AtomicU32::new(u32::MAX),
            ended_later: Mutex::new(vec![]),
            settled_later: Mutex::new(vec![]),
            staged: Mutex::new((vec![], vec![])),
            commit: Mutex::new(()),
            dirty: Mutex::new(HashSet::new()),
            stopped: Mutex::new(None),
            turn_limit: std::sync::atomic::AtomicI64::new(TURN_LIMIT_MS),
            idle_release: std::sync::atomic::AtomicI64::new(IDLE_RELEASE_MS),
            fallbacks: std::sync::atomic::AtomicU64::new(0),
        })
    }

    /// The Node is remote, behind a link: what it replays passes `check` (the link's guard, as
    /// what it pushes does), and no replay page is asked bigger than `largest` (its frame). Set
    /// before [`Self::start`]; once.
    pub fn over_link(&self, check: ReplayCheck, largest: u32) {
        let _ = self.replay_check.set(check);
        self.largest_replay.store(largest, Ordering::Relaxed);
    }

    /// How long reconcile waits for the Node's heads to be applied before it holds the sessions
    /// of the streams still behind and goes on ([`heads::APPLY_WAIT`] unless set).
    pub fn set_apply_wait(&self, d: Duration) {
        self.apply_wait_ms
            .store(d.as_millis() as u64, Ordering::Relaxed);
    }

    fn apply_wait(&self) -> Duration {
        Duration::from_millis(self.apply_wait_ms.load(Ordering::Relaxed))
    }

    /// Attempts planned from now on get `ms` as their deadline (and their permissions with it).
    pub fn set_turn_limit(&self, ms: i64) {
        self.turn_limit
            .store(ms.max(1), std::sync::atomic::Ordering::Relaxed);
    }

    /// An open session idle for `ms` (nothing running, queued or waiting on a card) is let go;
    /// the next message opens it again, resuming the conversation (`STEVE_IDLE_RELEASE_MS`).
    pub fn set_idle_release(&self, ms: i64) {
        self.idle_release.store(ms.max(0), Ordering::Relaxed);
    }

    /// The Node cannot run anything (its host did not come up): from now on its sessions refuse
    /// prompts, cancels, answers and releases with `why`, and nothing is sent.
    pub fn stop(&self, why: &str) {
        *self.stopped.lock().unwrap() = Some(why.to_string());
    }

    /// Whether [`Self::stop`] was called (its reason stays in the log).
    pub fn is_stopped(&self) -> bool {
        self.stopped.lock().unwrap().is_some()
    }

    /// A new session stayed on the legacy runner although this Node is on.
    pub fn fell_back(&self) {
        self.fallbacks.fetch_add(1, Ordering::Relaxed);
    }

    /// How many new sessions [`Self::fell_back`] since boot.
    pub fn fallbacks(&self) -> u64 {
        self.fallbacks.load(Ordering::Relaxed)
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
                "SELECT stream, epoch FROM node_streams WHERE node_id=? AND durable > applied AND fault IS NULL",
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
            let mut idled = std::time::Instant::now();
            loop {
                let Some(me) = weak.upgrade() else { return };
                let prune = pruned.elapsed() >= PRUNE_EVERY;
                let idle = idled.elapsed() >= IDLE_EVERY;
                if idle {
                    idled = std::time::Instant::now();
                }
                let applied = tokio::task::spawn_blocking(move || {
                    let applied = me.apply_some();
                    me.expire_due();
                    if idle {
                        me.release_idle();
                    }
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
        // transactions are serial (one writer), so what is staged while one is open is its own
        let _commit = self.commit.lock().unwrap();
        let (out, (settled, ended)) = self.store.transaction(|tx| {
            let take = || std::mem::take(&mut *self.staged.lock().unwrap());
            take();
            match f(tx) {
                Ok(out) => Ok((out, take())),
                Err(e) => {
                    take();
                    Err(e)
                }
            }
        })?;
        // committed: the apply worker books and tells it from now on
        if !settled.is_empty() || !ended.is_empty() {
            self.settled_later.lock().unwrap().extend(settled);
            self.ended_later.lock().unwrap().extend(ended);
            self.wake_apply.notify_one();
        }
        Ok(out)
    }

    /// The session runs on this Node from now on: its workspace is prepared and it is opened, by
    /// two commands (the open waits for the prepare). Idempotent for the same binding. Refused
    /// once [`Self::stop`] was called: nothing would ever send those commands.
    pub fn bind(&self, b: &Binding) -> Result<(), StoreError> {
        self.bind_after(b, None)
    }

    /// [`Self::bind`] for a session that takes over from another one: its prepare (and so its
    /// open) waits until `after`, that one's `release_session`, is done, so the two harnesses are
    /// never open at once (they share the item's worktree). A release that fails is asked again
    /// and the session waits for that one instead ([`Self::release_again_in`]).
    pub fn bind_after(&self, b: &Binding, after: Option<&str>) -> Result<(), StoreError> {
        self.running()?;
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
                    dependency: after,
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
    /// The session a decision card belongs to (`None`: no such card, or not a session's).
    pub fn decision_session(&self, decision: &str) -> Result<Option<String>, StoreError> {
        decision_session(&self.store, decision)
    }

    /// The session's harness is gone on its Node (released, or abandoned there): open it again,
    /// continuing the same conversation where the Node can.
    fn reopen_in(&self, tx: &Tx, session: &str) -> Result<(), StoreError> {
        self.reopen_after_in(tx, session, None)
    }

    /// [`Self::reopen_in`], the open waiting on `dependency` (a prepare) when there is one.
    fn reopen_after_in(
        &self,
        tx: &Tx,
        session: &str,
        dependency: Option<&str>,
    ) -> Result<(), StoreError> {
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
                dependency,
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
    /// the conversation). A session the business side ended during a turn is let go once that
    /// turn is over: its release is queued now and goes then ([`Release::Pending`] either way).
    pub fn release(&self, session: &str) -> Result<Release, StoreError> {
        self.running()?;
        let r = self.tx(|tx| self.release_in(tx, session))?;
        if matches!(r, Release::Pending(_)) {
            self.wake_send.notify_one();
        }
        Ok(r)
    }

    /// Lets go every session idle for [`Self::set_idle_release`] (once the Hub has reconciled
    /// with the Node). Returns how many releases were planned.
    fn release_idle(&self) -> usize {
        if !self.may_plan() || self.running().is_err() || self.store.maintenance() {
            return 0;
        }
        let before = now() - self.idle_release.load(Ordering::Relaxed);
        let node = self.node_id.as_str();
        let idle = self.store.reader(|c| {
            let mut st = c.prepare(IDLE_SESSIONS_SQL)?;
            let v = st
                .query_map(rusqlite::params![node, before, None::<String>], |r| {
                    r.get::<_, String>(0)
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok::<_, StoreError>(v)
        });
        let idle = match idle {
            Ok(v) => v,
            Err(e) => {
                tracing::error!("node {}: idle sessions: {e}", self.node_id);
                return 0;
            }
        };
        let mut n = 0;
        for s in idle {
            // checked again in the release's own transaction: a message may have come meanwhile
            let r = self.tx(|tx| {
                let still: Option<String> = tx
                    .query_row(IDLE_SESSIONS_SQL, rusqlite::params![node, before, s], |r| {
                        r.get(0)
                    })
                    .optional()?;
                if still.is_none() {
                    return Ok(false);
                }
                self.release_in(tx, &s)
            });
            match r {
                Ok(true) => {
                    tracing::info!(session = %s, "node {}: idle, released", self.node_id);
                    n += 1;
                }
                Ok(false) => {}
                Err(e) => tracing::error!("node {}: release idle {s}: {e}", self.node_id),
            }
        }
        if n > 0 {
            self.wake_send.notify_one();
        }
        n
    }

    /// [`Self::release`] inside `tx`.
    fn release_in(&self, tx: &Tx, session: &str) -> Result<Release, StoreError> {
        let Some((phase, active)) = phase_in(tx, session)? else {
            return Ok(Release::Gone);
        };
        let waiting = after_turn_release_in(tx, session)?;
        match (phase.as_str(), active.is_some()) {
            ("released", false) => {
                // nothing left to let go: a release that waited for the turn is done without
                // going out (what waits for it may go)
                if let Some(op) = waiting {
                    rt::merge_receipt_in(
                        tx,
                        &op,
                        ReceiptState::Finished,
                        true,
                        &json!({ "reason": "the session was let go already" }),
                        now(),
                    )?;
                }
                Ok(Release::Gone)
            }
            ("releasing", _) => Ok(match last_release_in(tx, session)? {
                Some(op) => Release::Pending(op),
                None => Release::Busy,
            }),
            ("ready", false) => {
                let sid = SessionId::try_from(session.to_string()).map_err(conflict)?;
                let op = match waiting {
                    // it never went out: it goes now, issued now
                    Some(op) => {
                        let (req, digest) = command(&op, sid)?;
                        tx.execute(
                            "UPDATE node_commands SET request=?, digest=?, retry_at=?, updated=? WHERE operation_id=? AND state='pending'",
                            rusqlite::params![
                                serde_json::to_string(&req).map_err(conflict)?,
                                digest,
                                now(),
                                now(),
                                op
                            ],
                        )?;
                        op
                    }
                    None => self.release_command_in(tx, session)?,
                };
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
                )?;
                Ok(Release::Pending(op))
            }
            _ if ended_in(tx, session)? => {
                // busy (a turn, an open, a configure): queued for when that is over
                let op = match waiting {
                    Some(op) => op,
                    None => {
                        let op = self.release_command_in(tx, session)?;
                        tx.execute(
                            "UPDATE node_commands SET retry_at=? WHERE operation_id=?",
                            rusqlite::params![AFTER_TURN, op],
                        )?;
                        op
                    }
                };
                Ok(Release::Pending(op))
            }
            _ => Ok(Release::Busy),
        }
    }

    /// A new `release_session` command for `session`, its id.
    fn release_command_in(&self, tx: &Tx, session: &str) -> Result<String, StoreError> {
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
        Ok(op)
    }

    /// An ended session is let go once it is idle (see [`Self::release`]).
    fn release_if_ended_in(&self, tx: &Tx, session: &str) -> Result<(), StoreError> {
        if ended_in(tx, session)? {
            self.release_in(tx, session)?;
        }
        Ok(())
    }

    /// Whether `session` runs on a Node.
    pub fn owns(&self, session: &str) -> bool {
        self.owns_strict(session).unwrap_or(false)
    }

    /// [`Self::owns`], with a read error as an error: for a decision that must not rest on a guess.
    pub fn owns_strict(&self, session: &str) -> Result<bool, StoreError> {
        self.store.reader(|c| {
            Ok::<_, StoreError>(c.query_row(
                "SELECT EXISTS(SELECT 1 FROM runtime_sessions WHERE session_id=? AND node_id=?)",
                [session, self.node_id.as_str()],
                |r| r.get::<_, bool>(0),
            )?)
        })
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
        Ok(self.flush_turn(session, lead)?.0)
    }

    /// [`Self::flush`], with the attempt it planned when it planned one.
    pub fn flush_turn(
        &self,
        session: &str,
        lead: Option<(&str, &str)>,
    ) -> Result<(Planned, Option<String>), StoreError> {
        self.running()?;
        let (planned, attempt) = self.tx(|tx| {
            // a released session takes the lead too: once something is queued it opens again
            let released = self.may_plan()
                && !self.is_holding()
                && phase_in(tx, session)?
                    .is_some_and(|(p, a)| p == "released" && a.is_none());
            let p = self.plan_state_in(tx, session)?;
            if !released && matches!(p, Planned::Busy | Planned::Held) {
                return Ok((p, None));
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
            let p = self.plan_in(tx, session)?;
            let attempt = if p == Planned::Created {
                tx.query_row(
                    "SELECT active_attempt FROM runtime_sessions WHERE session_id=?",
                    [session],
                    |r| r.get(0),
                )?
            } else {
                None
            };
            Ok((p, attempt))
        })?;
        if planned == Planned::Created {
            self.wake_send.notify_one();
        }
        Ok((planned, attempt))
    }

    /// Attempts `a` and `b` belong to the same turn (one is a gateway retry of the other).
    pub fn same_turn(&self, a: &str, b: &str) -> Result<bool, StoreError> {
        self.store.reader(|c| {
            Ok(c.query_row(
                "SELECT EXISTS(SELECT 1 FROM runtime_attempts x JOIN runtime_attempts y ON y.turn_id = x.turn_id WHERE x.attempt_id=?1 AND y.attempt_id=?2)",
                [a, b],
                |r| r.get(0),
            )?)
        })
    }

    fn tell_end(&self, e: End) {
        self.domain
            .turn_ended(&e.session, e.attempt.as_deref(), &e.stop, e.error);
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
        // a faulted stream waits for reconciliation: what the Node does for this session now would
        // never be applied, so nothing new starts there (and a released session is not reopened)
        if faulted(tx, &self.node_id, session)? || self.is_held(session) {
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
        // as the legacy runner's begin(): the turn is under way from the moment it is planned
        // (opening or loading the harness may take long, and a restart may leave the attempt
        // unknown before it starts); `starting` until the harness has a session of its own. A
        // retry of a turn already running stays running.
        let (acp, status): (bool, Option<String>) = tx
            .query_row(
                "SELECT acp_session_id IS NOT NULL, status FROM sessions WHERE id=?",
                [session],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?
            .unwrap_or((false, None));
        let starting = if acp || status.as_deref() == Some("running") {
            "running"
        } else {
            "starting"
        };
        status_in(tx, session, starting)?;
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

    /// What an upgrade drain waits for: every open turn (running, waiting for a permission answer
    /// until it is answered or expires, retrying, or unknown until settled or abandoned), plus each
    /// session whose harness is opening, configuring or releasing with no open turn. A read error
    /// counts as work: the drain then waits rather than seal under a running turn.
    /// Only this Node's sessions count: a remote Node's turns outlive the Hub's restart.
    /// A stopped Node counts nothing: it runs nothing and refuses the Stop that would settle its
    /// open turns, so a restart cuts nothing (its unknown turns stay unknown across it).
    pub fn working(&self) -> usize {
        if self.stopped.lock().unwrap().is_some() {
            return 0;
        }
        self.store
            .reader(|c| {
                Ok::<_, StoreError>(c.query_row(
                    "SELECT (SELECT COUNT(*) FROM runtime_turns t JOIN runtime_sessions s USING(session_id)
                               WHERE s.node_id=?1 AND t.state<>'finished')
                          + (SELECT COUNT(*) FROM runtime_sessions s
                               WHERE s.node_id=?1 AND s.phase IN ('opening','configuring','releasing')
                                 AND NOT EXISTS(SELECT 1 FROM runtime_turns t
                                                WHERE t.session_id=s.session_id AND t.state<>'finished'))",
                    [self.node_id.as_str()],
                    |r| r.get::<_, i64>(0),
                )?)
            })
            .unwrap_or(1) as usize
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

    /// The session's open turn has an outcome nobody can tell yet (reconciliation decides), or its
    /// stream is faulted (its events wait for reconciliation, so what they say is not known).
    /// A read error counts as uncertain: waiting is the safe answer.
    pub fn is_uncertain(&self, session: &str) -> bool {
        self.store
            .reader(|c| {
                Ok::<_, StoreError>(c.query_row(
                    "SELECT EXISTS(SELECT 1 FROM runtime_turns t LEFT JOIN runtime_attempts a ON a.turn_id=t.turn_id AND a.state<>'finished' WHERE t.session_id=?1 AND t.state<>'finished' AND (t.state='unknown' OR a.state='unknown'))",
                    [session],
                    |r| r.get::<_, bool>(0),
                )? || faulted(c, &self.node_id, session)?)
            })
            .unwrap_or(true)
            || self.is_held(session)
    }

    /// The Node holds work of the session the Hub has no record of ([`Self::reconcile`]), or
    /// its stream was not applied in time and has not caught up yet.
    fn is_held(&self, session: &str) -> bool {
        self.held.lock().unwrap().contains(session)
            || self.lagging.lock().unwrap().contains_key(session)
    }

    /// The attempt the session runs now (not finished yet), if any.
    pub fn active_attempt(&self, session: &str) -> Result<Option<String>, StoreError> {
        self.store.reader(|c| {
            Ok(c.query_row(
                "SELECT a.attempt_id FROM runtime_sessions s JOIN runtime_attempts a ON a.attempt_id = s.active_attempt WHERE s.session_id=? AND a.state<>'finished'",
                [session],
                |r| r.get(0),
            )
            .optional()?)
        })
    }

    /// Asks the Node to cancel the attempt running now ([`Self::cancel_attempt`] of it). The
    /// lifecycle's cancels (an item cancelled, done or paused, a steer) come here: they never
    /// retire a control, that is the user's [`Self::user_stop`] only.
    pub fn cancel(&self, session: &str) -> Result<bool, StoreError> {
        match self.active_attempt(session)? {
            Some(a) => self.cancel_attempt(session, &a),
            None => {
                self.running()?;
                Ok(false)
            }
        }
    }

    /// The user's Stop: [`Self::cancel`], and with no attempt to stop, a control whose outcome is
    /// unknown and holds the session is retired by one explicit `recover` ([`recover`]).
    pub fn user_stop(&self, session: &str) -> Result<bool, StoreError> {
        let attempt = self.active_attempt(session)?;
        self.cancel_as(session, attempt.as_deref(), true)
    }

    /// Asks the Node to cancel the turn `attempt` belongs to, only while that turn is still the
    /// session's running one: the stop reaches the attempt active now, which is `attempt` itself or
    /// a gateway retry of it (a retry is a new attempt of the same turn, as the legacy runner keeps
    /// its turn token across one). The target is frozen here, never looked up again when the
    /// command goes out. `true`: the turn ran, the cancel is on its way; `false`: that turn is over
    /// (another one may run now), and nothing is queued.
    pub fn cancel_attempt(&self, session: &str, attempt: &str) -> Result<bool, StoreError> {
        self.cancel_as(session, Some(attempt), false)
    }

    /// [`Self::cancel_attempt`]; `stop`: the user asked, a recover may be planned (also with no
    /// attempt to stop).
    fn cancel_as(
        &self,
        session: &str,
        attempt: Option<&str>,
        stop: bool,
    ) -> Result<bool, StoreError> {
        self.running()?;
        let r = self.tx(|tx| {
            let row: Option<(String, String, i64)> = match attempt {
                Some(attempt) => tx
                    .query_row(
                        "SELECT a.attempt_id, a.operation_id, a.revision FROM runtime_sessions s JOIN runtime_attempts a ON a.attempt_id = s.active_attempt JOIN runtime_turns t ON t.turn_id = a.turn_id JOIN runtime_attempts asked ON asked.turn_id = a.turn_id WHERE s.session_id=?1 AND asked.attempt_id=?2 AND a.state<>'finished' AND t.state<>'finished'",
                        [session, attempt],
                        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
                    )
                    .optional()?,
                None => None,
            };
            let Some((attempt, target, rev)) = row else {
                if !stop {
                    return Ok(false);
                }
                return Ok(self.recover_in(tx, session, None)?.unwrap_or(false));
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
                // (send_one) unless an earlier send was uncertain; a refusal ends it anyway. Once it started on the Node (its stream
                // said so), the Node holds it and the cancel goes now
                Some((st, true)) if st == "pending" && state == "planned" => return Ok(true),
                _ => {}
            }
            // an abandon of it the Node cannot tell the end of is retired first (its slot freed)
            if stop
                && state == "unknown"
                && let Some(r) = self.recover_in(tx, session, Some(&attempt))?
            {
                return Ok(r);
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
        // one retired by a recover is over too, even before its own end came
        let (n, live): (i64, bool) = tx.query_row(
            &format!("SELECT COUNT(*), COALESCE(MAX((c.state NOT IN ('finished','rejected') OR json_extract(c.receipt,'$.success')=1) AND NOT {}), 0) FROM node_commands c WHERE c.method=? AND c.target_attempt=?", recover::RETIRED),
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
        // the attempt's statistics row, if it had started, ends with this outcome; booked by the
        // apply worker once this transaction committed (staged until then, see `Self::tx`)
        let finished = After::Finished {
            session: session.to_string(),
            attempt: attempt.to_string(),
            completion: completion.clone(),
            at: now(),
        };
        let settled = self.plan_after_in(tx, session)? != Planned::Created;
        if settled {
            self.release_if_ended_in(tx, session)?;
            status_in(tx, session, settled_status(&completion.stop_reason))?;
        }
        let ended = settled.then(|| End {
            session: session.to_string(),
            attempt: Some(attempt.to_string()),
            stop: completion.stop_reason.clone(),
            error: completion.error.as_ref().map(|e| e.safe_detail.clone()),
        });
        let mut staged = self.staged.lock().unwrap();
        staged.0.push(finished);
        staged.1.extend(ended);
        Ok(())
    }

    // ── T2: sending ──

    /// Leases one command, sends it and merges what came back. False: nothing to send.
    async fn send_one(&self) -> bool {
        let lease = match self.tx(|tx| {
            match rt::lease_command_in(
                tx,
                self.node_id.as_str(),
                &self.owner,
                now(),
                now() + LEASE_MS,
            )? {
                Some(l) if !l.maybe_sent => fresh_envelope_in(tx, l).map(Some),
                l => Ok(l),
            }
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
                let delay = if e.code == ErrorCode::Exhausted {
                    self.exhausted_in(tx, &lease, e)?
                } else {
                    1_000
                };
                rt::retry_command_in(tx, &lease, now(), now() + delay, uncertain)?;
                // a start_turn the user stopped meanwhile, refused before acceptance every time
                // it went: it provably never ran, so it ends here instead of going out again. One
                // earlier uncertain send may still arrive: the cancel waits for the Node instead
                if !uncertain
                    && !lease.maybe_sent
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
                        self.released_in(tx, &s, &lease.operation, false)?;
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
                    "recover" => {
                        self.recover_failed_in(tx, &lease.operation, &e.safe_detail)?;
                        None
                    }
                    _ => None,
                };
                Ok((end, None))
            }
        });
        match r {
            Ok((end, stale)) => {
                if let Some(end) = end {
                    self.tell_end(end);
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

    /// A command the Node refused because it is out of room: the refusal is kept on the command
    /// with how many came in a row, which sets its next delay (2 s doubling, at most 60 s). The
    /// first one tells the session, once; the command itself stays the same and is not counted
    /// as an attempt again.
    fn exhausted_in(
        &self,
        tx: &Tx,
        lease: &rt::CommandLease,
        e: &ApiError,
    ) -> Result<i64, StoreError> {
        let before: Option<i64> = tx
            .query_row(
                "SELECT json_extract(receipt,'$.exhausted') FROM node_commands WHERE operation_id=? AND state='pending'",
                [&lease.operation],
                |r| r.get(0),
            )
            .optional()?
            .flatten();
        let n = before.unwrap_or(0) + 1;
        let refusal = json!({ "success": false, "payload": e, "exhausted": n });
        tx.execute(
            "UPDATE node_commands SET receipt=? WHERE operation_id=? AND state='pending'",
            rusqlite::params![refusal.to_string(), lease.operation],
        )?;
        if n == 1
            && let Some(session) = command_any_session_in(tx, &lease.operation)?
        {
            system_in(
                tx,
                &session,
                "Node 存储/容量已满，暂停接新工作，恢复后自动继续",
            )?;
        }
        Ok(exhausted_delay(n))
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
            "recover" => n.recover(parse(&l.request)?).await,
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
        let detail = match &receipt.state {
            Observed::Finished { error: Some(e), .. } => e.safe_detail.clone(),
            Observed::Unknown { reason } => reason.clone(),
            _ => String::new(),
        };
        if method == "recover" {
            self.recover_receipt_in(tx, op, state, success, &detail)?;
            return Ok(None);
        }
        // a target a recover retires: what follows comes with the recover's own end
        let interrupted = matches!(
            &receipt.state,
            Observed::Finished {
                outcome: Outcome::Interrupted,
                ..
            }
        );
        if self.retiring_in(tx, op, interrupted)? {
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
        match (method.as_str(), state) {
            ("release_session", ReceiptState::Finished) => {
                let s = command_session_in(tx, op)?;
                self.released_in(tx, &s, op, success)?;
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
                        "Node 无法确定{}是否完成（{detail}），会话等待对账，不会自动重发。点停止可撤回这个操作。",
                        if method == "open_session" {
                            "打开会话"
                        } else {
                            "释放会话"
                        }
                    ),
                )?;
            }
            ("prepare_workspace" | "abandon", ReceiptState::Unknown) => {
                // never sent again: what waits on it waits until it is retired
                if let Some(s) = command_any_session_in(tx, op)? {
                    system_in(
                        tx,
                        &s,
                        &format!(
                            "Node 无法确定{}是否完成（{detail}），会话等待对账，不会自动重发。点停止可撤回这个操作。",
                            if method == "abandon" {
                                "放弃这次执行"
                            } else {
                                "工作区准备"
                            }
                        ),
                    )?;
                }
            }
            _ => {}
        }
        Ok(None)
    }

    /// A release is over: the session is let go, or (refused, failed) still open as it was. What
    /// came in meanwhile goes now (opening it again if it was let go). Only the session's last
    /// release says so: an earlier one's receipt, come late, says nothing of the session now.
    fn released_in(&self, tx: &Tx, s: &str, op: &str, success: bool) -> Result<(), StoreError> {
        if phase_in(tx, s)?.is_none_or(|(p, _)| p != "releasing")
            || last_release_in(tx, s)?.as_deref() != Some(op)
        {
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
        if self.plan_after_in(tx, s)? != Planned::Created && !success {
            self.release_again_in(tx, s, op)?;
        }
        Ok(())
    }

    /// A release of an ended session that a successor waits for came back refused or failed:
    /// the old harness is still open in the shared worktree, so the successor keeps waiting, now
    /// for a new release (after a delay doubling from 1 s up to a minute) that it is moved onto.
    /// The successor is told once. Nothing waits for it: it stays as it is (tidy asks again).
    fn release_again_in(&self, tx: &Tx, s: &str, failed: &str) -> Result<(), StoreError> {
        let waiting: Vec<String> = tx
            .prepare(
                "SELECT operation_id FROM node_commands WHERE dependency_operation=? AND state='pending'",
            )?
            .query_map([failed], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        if waiting.is_empty() || !ended_in(tx, s)? {
            return Ok(());
        }
        let Release::Pending(op) = self.release_in(tx, s)? else {
            return Ok(());
        };
        let failures: i64 = tx.query_row(
            "SELECT COUNT(*) FROM node_commands WHERE method='release_session' AND json_extract(request,'$.body')=? AND state IN ('finished','rejected') AND json_extract(receipt,'$.success')=0",
            [s],
            |r| r.get(0),
        )?;
        tx.execute(
            "UPDATE node_commands SET retry_at=?, updated=? WHERE operation_id=? AND state='pending'",
            rusqlite::params![
                now() + (1_000i64 << (failures - 1).clamp(0, 6)).min(60_000),
                now(),
                op
            ],
        )?;
        for w in &waiting {
            tx.execute(
                "UPDATE node_commands SET dependency_operation=?, updated=? WHERE operation_id=? AND state='pending'",
                rusqlite::params![op, now(), w],
            )?;
            if failures == 1
                && let Some(next) = command_any_session_in(tx, w)?
            {
                system_in(
                    tx,
                    &next,
                    "上一个会话还没能在 Node 上释放，这个会话等它释放后再打开，正在重试",
                )?;
            }
        }
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
            if self.plan_after_in(tx, s)? != Planned::Created {
                self.release_if_ended_in(tx, s)?;
            }
            return Ok(None);
        }
        // the user hears why even when no turn waited (the queued message stays for the next try)
        let mut end = Some(End {
            session: s.to_string(),
            attempt: None,
            stop: "error".to_string(),
            error: Some(format!(
                "the Node could not open the session again: {detail}"
            )),
        });
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
            end = Some(End {
                session: s.to_string(),
                attempt: Some(last_attempt_in(tx, &turn)?),
                stop: c.stop_reason.clone(),
                error: c.error.as_ref().map(|e| e.safe_detail.clone()),
            });
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
        self.release_if_ended_in(tx, s)?;
        if let Some(e) = &end {
            status_in(tx, s, settled_status(&e.stop))?;
        }
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

    /// What waited while the Hub was not reconciled goes now: queued messages, and a desired
    /// config whose idle boundary (a turn's end applied during reconcile) found nothing planned.
    fn plan_waiting(&self) {
        let sessions: Vec<String> = self
            .store
            .reader(|c| {
                let mut st = c.prepare(
                    "SELECT s.session_id FROM runtime_sessions s WHERE s.node_id=?
                       AND (s.desired_config IS NOT NULL OR EXISTS(SELECT 1 FROM queued q WHERE q.session_id=s.session_id))",
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

    /// Compares the Hub's view with what the Node holds now (its ledger and stream heads as of one
    /// moment, its sessions, its waiting permissions), before anything is sent. First the Hub's
    /// inbox is brought up to the Node's heads and applied ([`catch_up_streams`]: replayed what it
    /// lacks, faulted what cannot be reconciled). The Node may have restarted meanwhile (a local
    /// Node restarts with the Hub): what it cannot tell the end of becomes `unknown` (never sent
    /// again; a person gives it up), a session it let go is `released` (the next message opens it
    /// again), a card nobody waits for any more expires, and a session with Node work the Hub has
    /// no record of is held with a note (that work is never given up on its own). Returns how many
    /// things changed.
    pub async fn reconcile(&self) -> ApiResult<usize> {
        let internal = |e: StoreError| ApiError::new(ErrorCode::Internal, e.to_string());
        // the Node's ledger, all pages of one snapshot
        let NodeSnapshot { entries, heads } = read_snapshot(&*self.node, 200).await?;
        // its events up to that moment, applied before the ledger is compared
        let caught = self.catch_up(&heads).await?;
        if caught.replayed > 0 || caught.faulted > 0 {
            tracing::info!(
                "node {}: reconcile replayed {} events, faulted {} streams",
                self.node_id,
                caught.replayed,
                caught.faulted
            );
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
                // 4. Node work the Hub has no record of (its database went back, say): counted
                // and logged, its session held with a note; never given up on its own (D7)
                let was = self.held.lock().unwrap().clone();
                let mut held = HashSet::new();
                let mut strangers = 0;
                for (op, e) in &entries {
                    if e.receipt.state.is_finished() {
                        continue;
                    }
                    let known: bool = tx.query_row(
                        "SELECT EXISTS(SELECT 1 FROM node_commands WHERE operation_id=? AND node_id=?)",
                        rusqlite::params![op, self.node_id.as_str()],
                        |r| r.get(0),
                    )?;
                    if known {
                        continue;
                    }
                    strangers += 1;
                    let session = e.execution.as_ref().and_then(exec_session);
                    tracing::warn!(
                        "node {}: {} {op} ({:?}) is not the Hub's; held",
                        self.node_id,
                        e.method,
                        e.receipt.state.tag()
                    );
                    let Some(session) = session else { continue };
                    let ours: bool = tx.query_row(
                        "SELECT EXISTS(SELECT 1 FROM runtime_sessions WHERE session_id=? AND node_id=?)",
                        rusqlite::params![session, self.node_id.as_str()],
                        |r| r.get(0),
                    )?;
                    if ours && held.insert(session.clone()) && !was.contains(&session) {
                        system_in(tx, &session, STRANGER)?;
                        changed += 1;
                    }
                }
                if strangers > 0 {
                    tracing::warn!(
                        "node {}: {strangers} operations the Hub has no record of, {} sessions held",
                        self.node_id,
                        held.len()
                    );
                }
                Ok((changed, held))
            })
            .map_err(internal)?;
        let (changed, held) = changed;
        *self.held.lock().unwrap() = held;
        for end in ends {
            self.tell_end(end);
        }
        self.wake_apply.notify_one();
        Ok(changed)
    }

    /// A `Released` view from before the Hub's last open of the session (that open's own view
    /// has not come on the stream yet): it says nothing of the session open now. Only the event's
    /// own epoch counts: a stream keeps its epoch within one journal, and an older journal's views
    /// say nothing of this one's.
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
            "SELECT EXISTS(SELECT 1 FROM node_event_inbox WHERE node_id=?1 AND stream=?2 AND epoch=?3 AND seq<?4
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

    /// Takes a batch from the Node into the inbox; acknowledges the contiguous durable prefixes
    /// (quarantined events included).
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
                let seq = e.cursor.seq as i64;
                let payload = serde_json::to_value(e).map_err(conflict)?;
                // a conflict is set aside on its own stream (D5: acknowledged once it is durably
                // quarantined), so the rest of the batch and every other stream go on
                if let rt::Inboxed::Conflict(why) =
                    rt::inbox_insert_in(tx, k, seq, e.id.as_str(), &payload, now())?
                {
                    let q = rt::quarantine_in(tx, k, seq, e.id.as_str(), &payload, why, now())?;
                    if q.new {
                        tracing::warn!(
                            "node {node}: stream {} epoch {} seq {seq} ({}): {}; quarantined",
                            k.stream,
                            k.epoch,
                            e.id,
                            why.reason()
                        );
                    }
                    if q.faulted {
                        self.faulted_note_in(tx, e)?;
                    }
                }
                let key = (e.cursor.stream.to_string(), e.cursor.epoch.to_string());
                if !touched.contains(&key) {
                    touched.push(key);
                }
            }
            let mut durable = vec![];
            for (stream, epoch) in &touched {
                let acked = rt::acked_in(
                    tx,
                    StreamKey {
                        node: &node,
                        stream,
                        epoch,
                    },
                )?;
                if acked > 0 {
                    durable.push(Watermark {
                        stream: idv(stream.clone())?,
                        epoch: idv(epoch.clone())?,
                        seq: acked as u64,
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

    /// The one note on a session whose stream just faulted: its Node events stop being applied
    /// until reconciliation, and its open turn is never run again meanwhile ([`Self::is_uncertain`]).
    fn faulted_note_in(&self, tx: &Tx, e: &NodeEvent) -> Result<(), StoreError> {
        let Some(session) = ev_session(e) else {
            return Ok(());
        };
        let ours: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM runtime_sessions WHERE session_id=? AND node_id=?)",
            rusqlite::params![session, self.node_id.as_str()],
            |r| r.get(0),
        )?;
        if ours {
            system_in(
                tx,
                &session,
                "Node 发来的事件与已保存的记录冲突，已隔离保存；该会话暂停应用 Node 事件，等待对账，期间不会重新运行",
            )?;
        }
        Ok(())
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
                // turn ends settled outside this worker (a local cancel) before this event's
                // commit are told before its end: taken under the write lock, so every one comes
                // from a transaction that is over, and the item hears its turns in commit order
                let mut earlier = (vec![], vec![]);
                let r = self.tx(|tx| {
                    earlier = self.take_settled();
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
                self.book(earlier);
                match r {
                    Ok(Some((end, ask))) => {
                        any = true;
                        self.after(ask);
                        if let Some(end) = end {
                            self.tell_end(end);
                        }
                    }
                    Ok(None) => {
                        self.dirty
                            .lock()
                            .unwrap()
                            .remove(&(stream.clone(), epoch.clone()));
                        self.caught_up_on(&stream, &epoch);
                        break;
                    }
                    Err(e) => {
                        tracing::error!("apply {stream}: {e}");
                        break;
                    }
                }
            }
        }
        self.book_settled();
        if any {
            self.wake_send.notify_one();
        }
        any
    }

    /// What the Hub settled on its own word (a local cancel, an abandon the Node has no record
    /// of), after the commit: the attempts' statistics rows end, then their turns are told.
    /// Only committed settles are queued here ([`Self::tx`]), so each is booked as it is.
    fn book_settled(&self) {
        self.book(self.take_settled());
    }

    /// The committed settles queued so far ([`Self::book_settled`]).
    fn take_settled(&self) -> (Vec<After>, Vec<End>) {
        (
            std::mem::take(&mut *self.settled_later.lock().unwrap()),
            std::mem::take(&mut *self.ended_later.lock().unwrap()),
        )
    }

    fn book(&self, (settled, ended): (Vec<After>, Vec<End>)) {
        self.after(settled);
        for end in ended {
            self.tell_end(end);
        }
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
                // the resume handle is the harness's own session id (ACP `sessionId`), what ST-28
                // shows and the legacy runner keeps too
                tx.execute(
                    "UPDATE sessions SET model = COALESCE(?, model), reasoning_effort = COALESCE(?, reasoning_effort), acp_session_id = COALESCE(?, acp_session_id), updated = ? WHERE id = ?",
                    rusqlite::params![
                        view.effective.model,
                        view.effective.effort,
                        view.resume.as_ref().map(|r| r.to_string()),
                        now(),
                        session
                    ],
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
                    status_in(tx, &session, "running")?;
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
    fn apply_in(&self, tx: &Tx, ev: &NodeEvent) -> Result<Option<End>, StoreError> {
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
    ) -> Result<Option<End>, StoreError> {
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
        // asked for while the turn ran waited for this)
        self.release_if_ended_in(tx, session)?;
        status_in(tx, session, settled_status(&c.stop_reason))?;
        Ok(Some(End {
            session: session.to_string(),
            attempt: Some(last_attempt_in(tx, turn)?),
            stop: c.stop_reason.clone(),
            error: c.error.as_ref().map(|e| e.safe_detail.clone()),
        }))
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
    /// Turns are held (release, maintenance, or the session's stream is faulted).
    Held,
    /// Nothing is queued.
    Empty,
}

/// What [`NodeRuntime::release`] did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Release {
    /// No harness is open for the session (never opened, or let go already).
    Gone,
    /// This `release_session` lets it go: queued now, or (an ended session that is busy) queued
    /// for when its turn is over.
    Pending(String),
    /// Busy and not ended: kept.
    Busy,
}

/// The format of the events this coordinator writes: Steve's own, never a harness protocol.
const V1: &str = "steve.v1";

/// The turn's latest attempt (the one that ended it, once it is over).
fn last_attempt_in(tx: &Tx, turn: &str) -> Result<String, StoreError> {
    Ok(tx.query_row(
        "SELECT attempt_id FROM runtime_attempts WHERE turn_id=? ORDER BY ordinal DESC LIMIT 1",
        [turn],
        |r| r.get(0),
    )?)
}

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

/// A command that never went out and waited long (behind its dependency, say: a successor's
/// prepare behind its predecessor's turn) is issued again now, so the Node does not refuse it as
/// past `accept_before`. Only one no send can have reached: the Node knows nothing of it yet.
fn fresh_envelope_in(tx: &Tx, mut l: rt::CommandLease) -> Result<rt::CommandLease, StoreError> {
    let mut c: Command<Value> = serde_json::from_value(l.request.clone()).map_err(conflict)?;
    if c.accept_before.0 - now() >= ACCEPT_WINDOW_MS / 2 {
        return Ok(l);
    }
    c.issued_at = Timestamp(now());
    c.accept_before = Timestamp(now() + ACCEPT_WINDOW_MS);
    let v = serde_json::to_value(&c).map_err(conflict)?;
    let digest = digest_of(&v).map_err(conflict)?.to_string();
    tx.execute(
        "UPDATE node_commands SET request=?, digest=?, updated=? WHERE operation_id=? AND state='pending'",
        rusqlite::params![
            serde_json::to_string(&v).map_err(conflict)?,
            digest,
            now(),
            l.operation
        ],
    )?;
    l.request = v;
    l.digest = digest;
    Ok(l)
}

/// The session's last `release_session` command, whatever became of it.
fn last_release_in(tx: &Tx, session: &str) -> Result<Option<String>, StoreError> {
    Ok(tx
        .query_row(
            "SELECT operation_id FROM node_commands WHERE method='release_session' AND json_extract(request,'$.body')=? ORDER BY created DESC, rowid DESC LIMIT 1",
            [session],
            |r| r.get(0),
        )
        .optional()?)
}

/// The session's release queued for when its turn is over (never sent yet), if any.
fn after_turn_release_in(tx: &Tx, session: &str) -> Result<Option<String>, StoreError> {
    Ok(tx
        .query_row(
            "SELECT operation_id FROM node_commands WHERE method='release_session' AND json_extract(request,'$.body')=?1 AND state='pending' AND retry_at=?2",
            rusqlite::params![session, AFTER_TURN],
            |r| r.get(0),
        )
        .optional()?)
}

/// The business side ended the session (`sessions.status`).
fn ended_in(tx: &Tx, session: &str) -> Result<bool, StoreError> {
    Ok(tx
        .query_row(
            "SELECT status='ended' FROM sessions WHERE id=?",
            [session],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or(false))
}

/// The session's phase and active attempt; `None`: not bound.
/// One of this Node's streams that carries `session`'s events (any epoch the Hub holds for it)
/// is faulted: its events wait for reconciliation. The session's own stream counts even when the
/// Hub holds none of its events (a gap found by reconcile).
fn faulted(c: &rusqlite::Connection, node: &NodeId, session: &str) -> Result<bool, StoreError> {
    Ok(c.query_row(
        "SELECT EXISTS(SELECT 1 FROM node_streams s WHERE s.node_id=?2 AND s.fault IS NOT NULL AND (
              s.stream=?3
           OR EXISTS(SELECT 1 FROM node_event_quarantine q WHERE q.node_id=s.node_id AND q.stream=s.stream AND q.epoch=s.epoch AND json_extract(q.payload,'$.subject.session')=?1)
           OR EXISTS(SELECT 1 FROM node_event_inbox e WHERE e.node_id=s.node_id AND e.stream=s.stream AND e.epoch=s.epoch AND json_extract(e.payload,'$.subject.session')=?1)))",
        rusqlite::params![session, node.as_str(), heads::session_stream(session)],
        |r| r.get(0),
    )?)
}

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

/// The delay before the `n`th send after as many Exhausted refusals in a row.
fn exhausted_delay(n: i64) -> i64 {
    (2_000i64 << (n - 1).clamp(0, 5)).min(60_000)
}

/// The session any command is for: its own, its target attempt's, or (a workspace) the open
/// waiting on it. `None`: none of them.
fn command_any_session_in(tx: &Tx, op: &str) -> Result<Option<String>, StoreError> {
    Ok(tx
        .query_row(
            "SELECT COALESCE(
                CASE c.method WHEN 'release_session' THEN json_extract(c.request,'$.body')
                    ELSE json_extract(c.request,'$.body.session') END,
                (SELECT t.session_id FROM runtime_attempts a JOIN runtime_turns t ON t.turn_id=a.turn_id
                  WHERE a.attempt_id=c.target_attempt),
                (SELECT json_extract(d.request,'$.body.session') FROM node_commands d
                  WHERE d.dependency_operation=c.operation_id ORDER BY d.created, d.rowid LIMIT 1))
             FROM node_commands c WHERE c.operation_id=?",
            [op],
            |r| r.get::<_, Option<String>>(0),
        )
        .optional()?
        .flatten())
}

/// `sessions.status` once a turn is over and nothing follows it: the legacy runner's values.
fn settled_status(stop_reason: &str) -> &'static str {
    if stop_reason == "error" {
        "error"
    } else {
        "idle"
    }
}

/// Sets `sessions.status` the way the legacy runner does (`running` from an attempt's start until
/// the turn is over). A session the business side ended stays ended.
fn status_in(tx: &Tx, session: &str, status: &str) -> Result<(), StoreError> {
    let n = tx.execute(
        "UPDATE sessions SET status=?1, updated=?2 WHERE id=?3 AND status NOT IN ('ended', ?1)",
        rusqlite::params![status, now(), session],
    )?;
    if n > 0 {
        tx.publish(steve_store::ChangeEvent::Session {
            id: session.into(),
            item_id: None,
        });
    }
    Ok(())
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

/// `/healthz`'s `node` block: what rollout and rollback decisions read instead of the database.
/// `local`: the local Node's coordinator when `STEVE_NODE_MODE=local`; `remote`: remote Nodes are
/// listened for (`STEVE_NODE_LISTEN=1`). The counts come from the store, so they are there with
/// the Node off too:
///
/// - `sessions` / `open`: sessions bound to any Node / not released by it; `running`: their turns
///   not finished, plus harnesses restarting for a configure (what a drain waits for);
/// - `unknown`: attempts whose outcome nobody can tell (a person gives them up);
/// - `parked`: sessions with queued messages bound to a Node that is not enabled here;
/// - `legacy.sessions`: sessions the legacy runner would still run (not ended, not read-only,
///   bound to no Node); `legacy.busy`: legacy sessions with work in hand (queued messages, a
///   claimed or submitted dispatch, a pending resume). Removing the runner needs both at 0.
///
/// `fallbacks` (since boot) counts new sessions left on the legacy runner although the local Node
/// was on. Public: no reason text, ids or paths, only states and counts.
pub fn health(
    store: &Store,
    local: Option<&NodeRuntime>,
    remote: bool,
) -> Result<Value, StoreError> {
    let (sessions, open, running, unknown, parked, legacy, busy) = store.reader(|c| {
        Ok::<_, StoreError>(c.query_row(
            "SELECT
               (SELECT COUNT(*) FROM runtime_sessions),
               (SELECT COUNT(*) FROM runtime_sessions WHERE phase<>'released'),
               (SELECT COUNT(*) FROM runtime_turns WHERE state<>'finished')
                 + (SELECT COUNT(*) FROM runtime_sessions WHERE phase='configuring'),
               (SELECT COUNT(*) FROM runtime_attempts WHERE state='unknown'),
               (SELECT COUNT(*) FROM runtime_sessions r
                 WHERE ((r.node_id='local' AND ?1=0) OR (r.node_id<>'local' AND ?2=0))
                   AND EXISTS(SELECT 1 FROM queued q WHERE q.session_id=r.session_id)),
               (SELECT COUNT(*) FROM sessions s WHERE s.status<>'ended' AND s.readonly=0
                   AND NOT EXISTS(SELECT 1 FROM runtime_sessions r WHERE r.session_id=s.id)),
               (SELECT COUNT(*) FROM sessions s
                 WHERE NOT EXISTS(SELECT 1 FROM runtime_sessions r WHERE r.session_id=s.id)
                   AND (EXISTS(SELECT 1 FROM queued q WHERE q.session_id=s.id)
                     OR EXISTS(SELECT 1 FROM session_dispatch d WHERE d.session_id=s.id
                                 AND d.backend='legacy' AND d.state IN ('claimed','submitted'))
                     OR EXISTS(SELECT 1 FROM session_resume x WHERE x.session_id=s.id)))",
            [local.is_some(), remote],
            |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, i64>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, i64>(4)?,
                    r.get::<_, i64>(5)?,
                    r.get::<_, i64>(6)?,
                ))
            },
        )?)
    })?;
    Ok(json!({
        "enabled": local.is_some(),
        "remote": remote,
        "reconciled": local.map(|n| n.is_reconciled()),
        "stopped": local.is_some_and(|n| n.is_stopped()),
        "holding": local.is_some_and(|n| n.is_holding()),
        "fallbacks": local.map_or(0, |n| n.fallbacks()),
        "sessions": sessions,
        "open": open,
        "running": running,
        "unknown": unknown,
        "parked": parked,
        "legacy": { "sessions": legacy, "busy": busy },
    }))
}

/// The session a decision belongs to.
pub fn decision_session(store: &Store, decision: &str) -> Result<Option<String>, StoreError> {
    store.reader(|c| {
        Ok::<_, StoreError>(
            c.query_row(
                "SELECT session_id FROM decisions WHERE id=?",
                [decision],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten(),
        )
    })
}

/// `decision` is the card of a Node turn still waiting for its answer.
pub fn waiting_card(store: &Store, decision: &str) -> bool {
    store
        .reader(|c| {
            Ok::<_, StoreError>(c.query_row(
                "SELECT EXISTS(SELECT 1 FROM runtime_permissions WHERE decision_id=? AND state='waiting')",
                [decision],
                |r| r.get::<_, bool>(0),
            )?)
        })
        .unwrap_or(false)
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

#[cfg(test)]
mod tests {
    use super::*;
    use steve_node_api::testkit::{FakeNode, build};

    /// Counts what the runtime books after commits.
    #[derive(Default)]
    struct Seen {
        finished: Mutex<Vec<String>>,
        ended: Mutex<Vec<String>>,
        started: Mutex<Vec<String>>,
    }

    impl NodeDomain for Seen {
        fn turn_ended(&self, session: &str, _: Option<&str>, _: &str, _: Option<String>) {
            self.ended.lock().unwrap().push(session.into());
        }
        fn attempt_started(&self, _: &str, attempt: &str, _: i64, _: Option<i64>, _: i64) {
            self.started.lock().unwrap().push(attempt.into());
        }
        fn attempt_finished(
            &self,
            _: &str,
            attempt: &str,
            _: &Completion,
            _: &AttemptFacts,
            _: i64,
        ) {
            self.finished.lock().unwrap().push(attempt.into());
        }
    }

    async fn until(what: &str, f: impl Fn() -> bool) {
        for _ in 0..1000 {
            if f() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        panic!("timed out waiting for {what}");
    }

    /// Review L4: an attempt the Hub settles on its own word reaches the apply worker only once
    /// its transaction committed. The worker draining its queue while that transaction is still
    /// open (it runs on another thread) used to drop the settle for good, its statistics row
    /// left open forever.
    #[tokio::test]
    async fn a_settle_drained_before_its_commit_still_ends_its_row() {
        const S: &str = "s1";
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(
            dir.path().join("steve.sqlite3"),
            steve_store::FnPublisher(|_| {}),
        )
        .unwrap();
        store.ensure_workspace().unwrap();
        store
            .transaction::<_, StoreError>(|tx| {
                tx.execute(
                    "INSERT INTO sessions(id,workspace_id,harness,cwd,status,created,updated) VALUES (?,'default','claude-code','.','active',1,1)",
                    [S],
                )?;
                Ok(())
            })
            .unwrap();
        let node = FakeNode::new("n1");
        let seen = Arc::new(Seen::default());
        let rt = NodeRuntime::new(store.clone(), node.clone(), seen.clone());
        node.connect(Arc::new(Sink(Arc::downgrade(&rt))));
        rt.start();
        rt.bind(&Binding {
            session: S.into(),
            resource: format!("item:{S}"),
            prepare: PrepareKind::Scratch,
            choice: build::choice(),
            policy: build::policy(),
        })
        .unwrap();
        rt.prompt(S, "first", "user", &[]).unwrap();
        until("started", || !seen.started.lock().unwrap().is_empty()).await;
        let attempt = seen.started.lock().unwrap()[0].clone();

        let completion = Completion {
            outcome: Outcome::Interrupted,
            stop_reason: "error".into(),
            error: None,
            result: vec![],
            continuation: None,
            transient: false,
            acted: false,
        };
        rt.tx(|tx| {
            rt.settle_in(tx, S, &attempt, &completion)?;
            // the apply worker drains between the settle and its commit
            rt.book_settled();
            assert!(
                seen.finished.lock().unwrap().is_empty(),
                "booked before the commit"
            );
            Ok(())
        })
        .unwrap();
        until("row ended", || !seen.finished.lock().unwrap().is_empty()).await;
        until("turn told", || !seen.ended.lock().unwrap().is_empty()).await;
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(
            *seen.finished.lock().unwrap(),
            std::slice::from_ref(&attempt),
            "once"
        );
        assert_eq!(*seen.ended.lock().unwrap(), [S.to_string()], "once");

        // a settle whose transaction rolls back is never booked
        let r = rt.tx(|tx| {
            rt.settle_in(tx, S, &attempt, &completion)?;
            Err::<(), _>(StoreError::Conflict("rolled back".into()))
        });
        assert!(r.is_err());
        rt.book_settled();
        tokio::time::sleep(Duration::from_millis(100)).await;
        assert_eq!(seen.finished.lock().unwrap().len(), 1);
        assert_eq!(seen.ended.lock().unwrap().len(), 1);
    }

    #[test]
    fn exhausted_delays_double_from_two_seconds_up_to_a_minute() {
        let d: Vec<i64> = (1..=8).map(super::exhausted_delay).collect();
        assert_eq!(
            d,
            [2_000, 4_000, 8_000, 16_000, 32_000, 60_000, 60_000, 60_000]
        );
    }
}
