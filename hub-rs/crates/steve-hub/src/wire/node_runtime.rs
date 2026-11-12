//! The Hub's coordinator for sessions that run on a Node.
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

mod apply;
mod config;
mod cost;
mod heads;
mod inbox;
// the job half of the coordinator lives next to it (wire/node_jobs.rs), with its fields in reach
mod ledger;
mod lifecycle;
#[path = "node_jobs.rs"]
mod node_jobs;
mod perm;
mod plan;
mod reconcile;
mod recover;
mod send;
#[cfg(test)]
mod tests;
mod workspace;

pub use apply::AttemptFacts;
pub use config::{ConfigChange, Configured};
pub use cost::CostCaps;
pub(crate) use cost::tokens as usage_tokens;
pub use heads::{
    CaughtUp, NodeSnapshot, Pull, Receive, ReplayCheck, catch_up_streams, read_snapshot,
};
pub use inbox::Sink;
pub use ledger::LedgerEntry;
#[cfg(test)]
pub(crate) use ledger::PUSH_DEADLINE;
pub use lifecycle::{Release, bound, bound_strict, decision_session, health, waiting_card};
pub(crate) use node_jobs::normalized_usage;
pub use node_jobs::{
    News, NodePrint, RESEARCH_GRANT, ResearchAttempt, ResearchFile, ResearchJobRun,
};
pub use plan::{Planned, park};
pub(crate) use plan::{deliver_mid_turn, mid_turn_note};
pub use workspace::WorkspaceFact;

#[cfg(test)]
use apply::role_lookup_in;
use inbox::PRUNE_BATCH;
use lifecycle::last_release_in;
use plan::cancelled_start_in;
#[cfg(test)]
use send::exhausted_delay;
use send::{command_any_session_in, receipt_state};

/// How long a command may wait to be accepted by the Node.
const ACCEPT_WINDOW_MS: i64 = 10 * 60_000;

/// What the user reads for a Node's error: a cause the Node decided on its own ([`NodeStop`]) in
/// words of ours, anything else as the Node said it. Stored (timeline, fail cards), so zh-CN;
/// that is also the language a model reads it in where it reaches one (a gateway or research
/// error), as the prompts are.
pub(crate) fn said(e: &ApiError) -> String {
    steve_i18n::stored(|| match NodeStop::of(e) {
        Some(stop) => stop_text(stop),
        None => e.safe_detail.clone(),
    })
}

fn stop_text(stop: NodeStop) -> String {
    use steve_i18n::t;
    match stop {
        NodeStop::Deadline => t!("hub.node.stop.deadline"),
        NodeStop::JobDeadline => t!("hub.node.stop.job_deadline"),
        NodeStop::HubUnreachable => t!("hub.node.stop.hub_unreachable"),
        NodeStop::OutputLimit => t!("hub.node.stop.output_limit"),
        NodeStop::FrameTooLarge => t!("hub.node.stop.frame_too_large"),
        NodeStop::IngressStalled { secs } => t!("hub.node.stop.ingress_stalled", secs = secs),
        NodeStop::JournalBudget => t!("hub.node.stop.journal_budget"),
        NodeStop::JournalFails => t!("hub.node.stop.journal_fails"),
        NodeStop::NotRecorded => t!("hub.node.stop.not_recorded"),
        NodeStop::HarnessGone => t!("hub.node.stop.harness_gone"),
        NodeStop::AdapterExited => t!("hub.node.stop.adapter_exited"),
        NodeStop::StoppedByNode => t!("hub.node.stop.stopped_by_node"),
        NodeStop::HarnessIdle => t!("hub.node.stop.harness_idle"),
        NodeStop::ResearchBudget => t!("hub.node.stop.research_budget"),
    }
}

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
    /// A workspace prepare or tidy is over (committed). Must return promptly and never call back
    /// into the coordinator synchronously.
    fn workspace_changed(&self, fact: WorkspaceFact) {
        let _ = fact;
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
    /// [`Self::set_principal_state`].
    principal_state: AtomicBool,
    /// [`Self::set_configured_roots`].
    configured_roots: OnceLock<Vec<String>>,
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
    /// What the Node's usage reports may add to a session's cost ([`Self::set_cost_caps`]).
    cost_caps: Mutex<CostCaps>,
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
    /// The workspace facts the open transaction found, told once it committed.
    staged_workspace: Mutex<Vec<WorkspaceFact>>,
    /// Held across [`Self::tx`]'s transaction and the move of what it settled, so a transaction
    /// that starts after another committed finds that one's settles already queued (turn ends are
    /// told in commit order).
    commit: Mutex<()>,
    /// Streams with events to apply.
    dirty: Mutex<HashSet<(String, String)>>,
    /// Why this Node cannot run anything (its host failed to start): its sessions then refuse
    /// work instead of queueing commands nobody sends.
    stopped: Mutex<Option<String>>,
    /// How long an attempt (and so a permission it asks) may take, in ms. No limit in practice unless the host
    /// sets it ([`Self::set_turn_limit`]).
    turn_limit: std::sync::atomic::AtomicI64,
    /// How long an open session may sit idle before its harness is let go, in ms
    /// ([`Self::set_idle_release`]; legacy's 10 minutes by default).
    idle_release: std::sync::atomic::AtomicI64,
    /// New sessions bound nowhere since boot although this Node was on (no binding could be
    /// planned, or the bind failed), so refused: `/healthz` shows them, so they don't hide.
    fallbacks: std::sync::atomic::AtomicU64,
    /// Research runs that went in process since boot although research runs on this Node (a
    /// directory under none of its roots, or the roots unreadable): `/healthz` shows them too.
    research_fallbacks: AtomicU64,
    /// The callers waiting on think jobs of this Node ([`NodePrint`]).
    jobs: node_jobs::Waiters,
    /// The role of each message seen lately, by (session, message): a text delta takes its role
    /// from the `message` row that began it, and a message's role never changes. What is not
    /// here (after a restart, or once the map was cleared at [`ROLES_KEPT`]) is looked up by index.
    roles: Mutex<HashMap<(String, String), Role>>,
}

/// How long applied Node events stay in the inbox (what is still needed stays longer).
const INBOX_KEEP_MS: i64 = 7 * 24 * 3600 * 1000;
/// How often the apply worker prunes, and how many rows at most per transaction.
const PRUNE_EVERY: Duration = Duration::from_secs(600);
/// An attempt's deadline by default: none in practice, like research's ([`super::research::NO_WALL_LIMIT`]).
/// A Worker's turn is not cut off for running long, nor its permission cards for waiting on you (they
/// expire with it); it ends when the agent ends it or you stop it (decided by the user on 2026-11-09:
/// Steve coordinates, it does not limit the agent). The Node needs a deadline, so it is far away.
const TURN_LIMIT_MS: i64 = super::research::NO_WALL_LIMIT.as_millis() as i64;
/// How long a session stays open with nothing to do, by default.
const IDLE_RELEASE_MS: i64 = 600_000;
/// How often the apply worker looks for idle sessions.
const IDLE_EVERY: Duration = Duration::from_secs(1);
/// How often the apply worker looks for lost commands past their deadline.
const LOST_EXECS_EVERY: Duration = Duration::from_secs(2);

/// A command's envelope as the Node gets it, built once and stored verbatim.
/// The reason of an abandon the Hub sends on its own for an attempt its Node's restart cut (B6).
const RESTART_ABANDON: &str = "the Node restarted while it ran this attempt";

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
            principal_state: AtomicBool::new(false),
            configured_roots: OnceLock::new(),
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
            cost_caps: Mutex::new(CostCaps::default()),
            largest_replay: AtomicU32::new(u32::MAX),
            ended_later: Mutex::new(vec![]),
            settled_later: Mutex::new(vec![]),
            staged: Mutex::new((vec![], vec![])),
            staged_workspace: Mutex::new(vec![]),
            commit: Mutex::new(()),
            dirty: Mutex::new(HashSet::new()),
            stopped: Mutex::new(None),
            turn_limit: std::sync::atomic::AtomicI64::new(TURN_LIMIT_MS),
            idle_release: std::sync::atomic::AtomicI64::new(IDLE_RELEASE_MS),
            fallbacks: std::sync::atomic::AtomicU64::new(0),
            research_fallbacks: AtomicU64::new(0),
            jobs: node_jobs::Waiters::default(),
            roles: Mutex::new(HashMap::new()),
        })
    }

    /// The Node is remote, behind a link: what it replays passes `check` (the link's guard, as
    /// what it pushes does), and no replay page is asked bigger than `largest` (its frame). Set
    /// before [`Self::start`]; once.
    pub fn over_link(&self, check: ReplayCheck, largest: u32) {
        let _ = self.replay_check.set(check);
        self.largest_replay.store(largest, Ordering::Relaxed);
    }

    /// Whether the Node is remote, behind a link ([`Self::over_link`]).
    pub fn is_remote(&self) -> bool {
        self.replay_check.get().is_some()
    }

    /// H1: a remote Node takes only its owner's own work until runs there are sandboxed
    /// ([`super::node_binding::remote_refusal`]): refused (a conflict saying why, in the current
    /// language) when `workspace`'s work may be someone else's. A Node paused or revoked is left
    /// to what pausing does (its work waits). The local Node takes everything.
    fn owners_work_in(&self, c: &rusqlite::Connection, workspace: &str) -> Result<(), StoreError> {
        use super::node_binding::{RemoteRefusal, remote_refusal};
        if !self.is_remote() {
            return Ok(());
        }
        match remote_refusal(c, self.node_id.as_str(), workspace)? {
            None | Some(RemoteRefusal::Inactive) => Ok(()),
            Some(r) => {
                let why = steve_i18n::t!(
                    "hub.node.remote.refused",
                    node = self.node_id.as_str(),
                    why = r.why()
                );
                tracing::warn!(node = %self.node_id, workspace, refusal = ?r, "remote node refused: not its owner's work");
                Err(StoreError::Conflict(why))
            }
        }
    }

    /// [`Self::owners_work_in`] for the workspace of `session`.
    fn session_owners_work_in(
        &self,
        c: &rusqlite::Connection,
        session: &str,
    ) -> Result<(), StoreError> {
        if let Some(why) = steve_store::workflow_gate::session_authority_blocker(c, session)? {
            return Err(conflict(why));
        }
        if !self.is_remote() {
            return Ok(());
        }
        let ws: Option<String> = c
            .query_row(
                "SELECT workspace_id FROM sessions WHERE id=?",
                [session],
                |r| r.get(0),
            )
            .optional()?;
        match ws {
            Some(w) => self.owners_work_in(c, &w),
            None => Err(conflict(format!("no session {session}"))),
        }
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

    /// What one usage report and one attempt may add to a session's cost at most
    /// (`STEVE_NODE_COST_REPORT_MAX`, `STEVE_NODE_COST_ATTEMPT_MAX`); a report past them adds
    /// nothing.
    pub fn set_cost_caps(&self, caps: CostCaps) {
        *self.cost_caps.lock().unwrap() = caps;
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

    /// A new session was bound nowhere although this Node is on (it is refused).
    pub fn fell_back(&self) {
        self.fallbacks.fetch_add(1, Ordering::Relaxed);
    }

    /// How many new sessions [`Self::fell_back`] since boot.
    pub fn fallbacks(&self) -> u64 {
        self.fallbacks.load(Ordering::Relaxed)
    }

    /// A research run went in process although research runs on this Node.
    pub fn research_fell_back(&self) {
        self.research_fallbacks.fetch_add(1, Ordering::Relaxed);
    }

    /// How many research runs [`Self::research_fell_back`] since boot.
    pub fn research_fallbacks(&self) -> u64 {
        self.research_fallbacks.load(Ordering::Relaxed)
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
            // the Node links a moment after the Hub starts: the first try finding it not there is
            // the usual order, a later one is worth a warning
            let mut waited = false;
            loop {
                let Some(me) = weak.upgrade() else { return };
                match me.reconcile().await {
                    Ok(n) => {
                        if n > 0 {
                            tracing::info!("node {}: reconciled, {n} changed", me.node_id);
                        }
                        // job attempts of an earlier run have no caller any more
                        let jobs = me.clone();
                        let _ =
                            tokio::task::spawn_blocking(move || jobs.settle_orphan_jobs()).await;
                        me.reconciled.store(true, Ordering::SeqCst);
                        me.plan_waiting();
                        break;
                    }
                    Err(e) if e.code == ErrorCode::Offline && !waited => {
                        waited = true;
                        tracing::info!(
                            "node {}: not linked yet; reconciling once it is",
                            me.node_id
                        );
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
            let mut swept = std::time::Instant::now() - LOST_EXECS_EVERY;
            loop {
                let Some(me) = weak.upgrade() else { return };
                let prune = pruned.elapsed() >= PRUNE_EVERY;
                let idle = idled.elapsed() >= IDLE_EVERY;
                if idle {
                    idled = std::time::Instant::now();
                }
                // lost commands are known only once the Hub has reconciled with the Node
                let sweep = swept.elapsed() >= LOST_EXECS_EVERY && me.is_reconciled();
                if sweep {
                    swept = std::time::Instant::now();
                }
                let applied = tokio::task::spawn_blocking(move || {
                    let applied = me.apply_some();
                    me.expire_due();
                    if sweep && let Err(e) = me.recover_lost_execs(now()) {
                        tracing::warn!("node {}: lost commands: {e}", me.node_id);
                    }
                    if idle {
                        me.release_idle();
                    }
                    if !applied && prune {
                        me.prune_inbox(now());
                        me.prune_jobs(now() - INBOX_KEEP_MS);
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
        let commit = self.commit.lock().unwrap();
        let (out, (settled, ended), facts) = self.store.transaction(|tx| {
            let take = || std::mem::take(&mut *self.staged.lock().unwrap());
            let take_ws = || std::mem::take(&mut *self.staged_workspace.lock().unwrap());
            take();
            take_ws();
            match f(tx) {
                Ok(out) => Ok((out, take(), take_ws())),
                Err(e) => {
                    take();
                    take_ws();
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
        drop(commit);
        for f in facts {
            self.domain.workspace_changed(f);
        }
        Ok(out)
    }

    /// New sessions get a harness directory of their principal's own on this Node
    /// ([`super::principal::harness_state_in`]).
    pub fn set_principal_state(&self, on: bool) {
        self.principal_state.store(on, Ordering::Relaxed);
    }

    /// The directories the Hub gives the local Node besides its projects, sessions and folders
    /// (the scratch and worktree directories), for [`Self::record_view`]. Once.
    pub fn set_configured_roots(&self, dirs: Vec<std::path::PathBuf>) {
        let _ = self.configured_roots.set(
            dirs.into_iter()
                .map(|d| d.to_string_lossy().into_owned())
                .collect(),
        );
    }

    fn tell_end(&self, e: End) {
        self.domain
            .turn_ended(&e.session, e.attempt.as_deref(), &e.stop, e.error);
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

    /// Its faces, for what needs no ledger (discovery: probing, listing and reading history).
    pub fn node(&self) -> Arc<dyn Node> {
        self.node.clone()
    }
}

/// The option ids of a Node permission card.
const ALLOW: &str = "allow_once";
const REJECT: &str = "reject";

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
