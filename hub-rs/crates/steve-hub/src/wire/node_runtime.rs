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
//! - **T3, receive**: a batch of the Node's events is applied as it comes, in one transaction with
//!   each stream's position; once it committed, what was applied is acknowledged (the Node sends
//!   the rest again).
//! - **T4, apply**: each stream's events in order, each once: the session's Steve events
//!   (`steve.v1`), the attempt's and turn's state, a retry as a new attempt (never a rewrite), the
//!   next turn or, once nothing follows, the turn's end for the business side, exactly once.
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
use serde_json::{Value, json};
use steve_node_api::*;
use steve_store::runtime::{self as rt, ReceiptState};
use steve_store::{
    FileRef, Ingest, Store, StoreError, StreamKey, W, node_sessions, node_turns, node_write,
};

mod apply;
mod config;
mod cost;
mod heads;
mod intent;
// the job half of the coordinator lives next to it (wire/node_jobs.rs), with its fields in reach
mod ledger;
mod lifecycle;
mod meters;
#[path = "node_jobs.rs"]
mod node_jobs;
mod perm;
mod plan;
mod realtime;
#[cfg(test)]
mod realtime_tests;
mod receive;
mod reconcile;
mod recover;
mod send;
mod stall;
#[cfg(test)]
mod tests;
mod workspace;

pub use super::approval::Desired as ApprovalTarget;
pub use apply::AttemptFacts;
pub use config::{ConfigChange, Configured};
pub(crate) use config::{config_in, mode_str};
pub use cost::CostCaps;
pub use steve_runtime::Clock;

pub use heads::{
    CaughtUp, NodeSnapshot, Pull, Receive, ReplayCheck, catch_up_streams, read_snapshot,
};
pub use ledger::LedgerEntry;
pub use lifecycle::{Release, bound, bound_strict, decision_session, health, waiting_card};
pub(crate) use node_jobs::normalized_usage;
pub use node_jobs::{
    News, NodePrint, RESEARCH_GRANT, ResearchAttempt, ResearchFile, ResearchJobRun,
};
pub use plan::{Planned, park};
pub(crate) use plan::{deliver_mid_turn, mid_turn_note, queue_in};
pub use receive::Sink;
pub use workspace::WorkspaceFact;

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
pub(crate) fn said(e: &Error) -> String {
    steve_i18n::stored(|| match NodeStop::of(e) {
        Some(stop) => stop_text(stop),
        None if workspace_binding_conflict(e) => steve_i18n::t!("hub.node.workspace_conflict"),
        None => e.safe_detail.clone(),
    })
}

/// Only the Node's own immutable-workspace conflict; a harness's arbitrary conflict stays intact.
pub(crate) fn workspace_binding_conflict(e: &Error) -> bool {
    e.reason
        .as_ref()
        .is_some_and(|r| r.is(NodeReason::WorkspaceBindingConflict))
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
    fn approval(&self, _session: &str) -> steve_store::Result<Option<ApprovalTarget>> {
        Ok(None)
    }

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
    transport_link_required: AtomicBool,
    /// [`Self::set_configured_roots`].
    configured_roots: OnceLock<Vec<String>>,
    store: Store,
    clock: Arc<dyn Clock>,
    node: Arc<dyn Node>,
    node_id: NodeId,
    domain: Arc<dyn NodeDomain>,
    /// The lease owner of this Hub process.
    owner: String,
    /// Shared with the workers, which wait on them without keeping the runtime alive.
    wake_send: Arc<tokio::sync::Notify>,
    wake_upkeep: Arc<tokio::sync::Notify>,
    holding: AtomicBool,
    /// The owner paused the Node ([`Self::set_paused`]).
    paused: AtomicBool,
    /// Set once the Hub has reconciled with the Node since it started: nothing is planned before.
    reconciled: AtomicBool,
    sender_started: AtomicBool,
    /// Sessions with Node work the Hub has no record of (found by the last reconcile): nothing is
    /// planned for them, and that work is never given up on its own (D7).
    held: Mutex<HashSet<String>>,
    /// What a remote Node's replayed batches pass before they are applied ([`Self::over_link`]).
    replay_check: OnceLock<ReplayCheck>,
    /// What the Node's usage reports may add to a session's cost ([`Self::set_cost_caps`]).
    cost_caps: Mutex<CostCaps>,
    /// Peak prompt context tokens observed per in-flight attempt (read-only metric observation).
    context_peaks: Mutex<HashMap<String, u64>>,
    /// The largest replay page the Node's transport carries (unbounded in process).
    largest_replay: AtomicU32,
    /// What the business side is told of committed transactions, in commit order, by the upkeep
    /// worker ([`Self::tell_later`]): what applied events and the Hub's own settles mean for it.
    later: Mutex<Vec<Tell>>,
    /// What the open transaction will tell (applied events, [`Self::settle_in`]): moved to `later`
    /// only once it committed, so nothing is told earlier (a row the store does not show finished
    /// yet) nor once rolled back.
    staged: Mutex<Vec<Tell>>,
    /// The workspace facts the open transaction found, told once it committed.
    staged_workspace: Mutex<Vec<WorkspaceFact>>,
    /// Job waiters hear only committed events, never a rolled-back Usage or completion.
    staged_job_events: Mutex<Vec<(String, EventBody)>>,
    /// Held across [`Self::tx`]'s transaction and the move of what it settled, so a transaction
    /// that starts after another committed finds that one's settles already queued (turn ends are
    /// told in commit order).
    commit: Mutex<()>,
    /// Why this Node cannot run anything (its host failed to start): its sessions then refuse
    /// work instead of queueing commands nobody sends.
    stopped: Mutex<Option<String>>,
    /// How long an attempt (and so a permission it asks) may take, in ms. No limit in practice unless the host
    /// sets it ([`Self::set_turn_limit`]).
    turn_limit: std::sync::atomic::AtomicI64,
    /// How long an open session may sit idle before its harness is let go, in ms
    /// ([`Self::set_idle_release`]; 10 minutes by default).
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
    /// What the Hub measures of this Node's batches and commands.
    meters: meters::Meters,
}

/// An attempt's deadline by default: none in practice, like research's ([`super::research::NO_WALL_LIMIT`]).
/// A Worker's turn is not cut off for running long, nor its permission cards for waiting on you (they
/// expire with it); it ends when the agent ends it or you stop it (decided by the user on 2026-11-09:
/// Steve coordinates, it does not limit the agent). The Node needs a deadline, so it is far away.
const TURN_LIMIT_MS: i64 = super::research::NO_WALL_LIMIT.as_millis() as i64;
/// How long a session stays open with nothing to do, by default.
const IDLE_RELEASE_MS: i64 = 600_000;
/// How often the upkeep worker looks for idle sessions.
const IDLE_EVERY: Duration = Duration::from_secs(1);
/// The most pending gateway accounting rows the upkeep worker applies at once.
const LOCAL_RUN: usize = 128;

fn command_at<T: serde::Serialize>(
    op: &str,
    body: T,
    at: i64,
) -> Result<(Value, String), StoreError> {
    let c = Command {
        id: idv::<OperationId>(op)?,
        issued_at: Timestamp(at),
        accept_before: Timestamp(at + ACCEPT_WINDOW_MS),
        grant: idv::<GrantId>("hub")?,
        body,
    };
    let v = serde_json::to_value(&c).map_err(conflict)?;
    let digest = digest_of(&v).map_err(conflict)?;
    Ok((v, digest.to_string()))
}

impl NodeRuntime {
    pub fn new(store: Store, node: Arc<dyn Node>, domain: Arc<dyn NodeDomain>) -> Arc<NodeRuntime> {
        Self::with_clock(store, node, domain, Arc::new(steve_runtime::SystemClock))
    }

    /// Assemble the coordinator with its source of retry time. Durable timestamps remain Unix
    /// milliseconds; a replacement clock must use the same epoch across coordinator restarts.
    pub fn with_clock(
        store: Store,
        node: Arc<dyn Node>,
        domain: Arc<dyn NodeDomain>,
        clock: Arc<dyn Clock>,
    ) -> Arc<NodeRuntime> {
        let node_id = node.id().clone();
        Arc::new(NodeRuntime {
            principal_state: AtomicBool::new(false),
            transport_link_required: AtomicBool::new(false),
            configured_roots: OnceLock::new(),
            store,
            clock,
            node,
            domain,
            owner: uid("hub"),
            wake_send: Arc::new(tokio::sync::Notify::new()),
            wake_upkeep: Arc::new(tokio::sync::Notify::new()),
            holding: AtomicBool::new(false),
            paused: AtomicBool::new(false),
            reconciled: AtomicBool::new(false),
            sender_started: AtomicBool::new(false),
            held: Mutex::new(HashSet::new()),
            replay_check: OnceLock::new(),
            cost_caps: Mutex::new(CostCaps::default()),
            context_peaks: Mutex::new(HashMap::new()),
            largest_replay: AtomicU32::new(u32::MAX),
            later: Mutex::new(vec![]),
            staged: Mutex::new(vec![]),
            staged_workspace: Mutex::new(vec![]),
            staged_job_events: Mutex::new(vec![]),
            commit: Mutex::new(()),
            stopped: Mutex::new(None),
            turn_limit: std::sync::atomic::AtomicI64::new(TURN_LIMIT_MS),
            idle_release: std::sync::atomic::AtomicI64::new(IDLE_RELEASE_MS),
            fallbacks: std::sync::atomic::AtomicU64::new(0),
            research_fallbacks: AtomicU64::new(0),
            jobs: node_jobs::Waiters::default(),
            roles: Mutex::new(HashMap::new()),
            meters: meters::Meters::new(node_id.as_str()),
            node_id,
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
    fn owners_work_in(&self, workspace: &str) -> Result<(), StoreError> {
        use super::node_binding::{RemoteRefusal, remote_refusal};
        if !self.is_remote() {
            return Ok(());
        }
        match self
            .store
            .read(|c| remote_refusal(c, self.node_id.as_str(), workspace))?
        {
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

    /// Recheck persisted item gates (paused item or open prerequisite wait) at durable admission.
    fn item_blocker_in(&self, session: &str) -> Result<Option<String>, StoreError> {
        Ok(self
            .store
            .read(|c| steve_store::item_gate::session_blocker(c, session))?
            .map(super::item_gate_reason))
    }

    /// Check the workspace's Node admission for this session.
    fn session_owners_work_in(&self, w: &mut W<'_>, session: &str) -> Result<(), StoreError> {
        if !self.is_remote() {
            return Ok(());
        }
        let s = node_sessions::authority::session(w, session)?
            .ok_or_else(|| conflict(format!("no session {session}")))?;
        self.owners_work_in(&s.workspace)
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

    /// The worker that sends commands and the upkeep worker, until the runtime is dropped.
    pub fn start(self: &Arc<Self>) {
        if self.sender_started.swap(true, Ordering::AcqRel) {
            return;
        }
        let admission = Arc::downgrade(self);
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(2)).await;
                let Some(node) = admission.upgrade() else {
                    break;
                };
                if node.is_reconciled() && node.running().is_ok() {
                    let _ = tokio::task::spawn_blocking(move || node.plan_waiting()).await;
                }
            }
        });

        // configures refused while the Hub was down
        self.settle_configuring();
        // each worker holds the runtime only while it works, never while it waits: dropping the
        // last handle ends both. Nothing is sent before the Hub has reconciled with the Node.
        let (weak, wake) = (Arc::downgrade(self), self.wake_send.clone());
        let clock = self.clock.clone();
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
                        me.reconciled.store(true, Ordering::SeqCst);
                        me.plan_waiting();
                        break;
                    }
                    Err(e) if e.code == Code::Unavailable && !waited => {
                        waited = true;
                        tracing::info!(
                            "node {}: not linked yet; reconciling once it is",
                            me.node_id
                        );
                    }
                    Err(e) => tracing::warn!("node {}: reconcile: {e}; trying again", me.node_id),
                }
                drop(me);
                clock.sleep(Duration::from_secs(5)).await;
            }
            loop {
                let Some(me) = weak.upgrade() else { return };
                // Prepare before sending; use the persisted retry deadline within the one-second fallback.
                // The system timer still starts after the send. Do not retain the runtime asleep.
                let tick = clock.sleep(me.sender_delay());
                let sent = me.send_one().await;
                drop(me);
                if !sent {
                    tokio::select! {
                        _ = tick => {},
                        _ = wake.notified() => {},
                    }
                }
            }
        });
        // upkeep is SQLite writes and the Hub's callbacks: on a blocking thread, never on an
        // async worker
        let (weak, wake) = (Arc::downgrade(self), self.wake_upkeep.clone());
        let clock = self.clock.clone();
        tokio::spawn(async move {
            let mut idled = clock.now();
            loop {
                let Some(me) = weak.upgrade() else { return };
                let at = clock.now();
                let idle = at.saturating_sub(idled) >= IDLE_EVERY.as_millis() as i64;
                if idle {
                    idled = at;
                }
                let tick = clock.sleep(stall::POLL_INTERVAL);
                let more = tokio::task::spawn_blocking(move || me.upkeep(idle))
                    .await
                    .unwrap_or(false);
                if !more {
                    tokio::select! {
                        _ = tick => {},
                        _ = wake.notified() => {},
                    }
                }
            }
        });
    }

    /// What the upkeep worker does: pending usage in the durable gateway inbox, what committed transactions tell the business side,
    /// turn progress and idle sessions. True: more gateway usage may wait.
    fn upkeep(&self, idle: bool) -> bool {
        let more = match self.store.stats().gateway_apply_local(LOCAL_RUN) {
            Ok(n) => n > 0,
            Err(e) => {
                tracing::warn!("apply local gateway usage: {e}");
                false
            }
        };
        self.tell_later();
        if let Err(e) = self.observe_turns() {
            tracing::warn!("node {}: progress observation: {e}", self.node_id);
        }
        if idle {
            self.release_idle();
        }
        more
    }

    /// One write ([`Store::write`]); what the coordinator staged in it is told once it committed.
    fn write<T>(
        &self,
        f: impl FnOnce(&mut W<'_>) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        // transactions are serial (one writer), so what is staged while one is open is its own
        let commit = self.commit.lock().unwrap();
        let (out, told, facts, job_events) = self.store.write(|w| {
            let take = || std::mem::take(&mut *self.staged.lock().unwrap());
            let take_ws = || std::mem::take(&mut *self.staged_workspace.lock().unwrap());
            let take_jobs = || std::mem::take(&mut *self.staged_job_events.lock().unwrap());
            take();
            take_ws();
            take_jobs();
            match f(w) {
                Ok(out) => Ok((out, take(), take_ws(), take_jobs())),
                Err(e) => {
                    take();
                    take_ws();
                    take_jobs();
                    Err(e)
                }
            }
        })?;
        // committed: told from now on, after what committed before
        if !told.is_empty() {
            self.later.lock().unwrap().extend(told);
            self.wake_upkeep.notify_one();
        }
        // Keep this commit's waiter notifications ahead of the next transaction's events.
        for (attempt, body) in job_events {
            self.notify_job(&attempt, &body);
        }
        drop(commit);
        for f in facts {
            self.domain.workspace_changed(f);
        }
        Ok(out)
    }

    /// How much the open transaction staged so far ([`Self::unstage`]).
    fn staged_mark(&self) -> [usize; 3] {
        [
            self.staged.lock().unwrap().len(),
            self.staged_workspace.lock().unwrap().len(),
            self.staged_job_events.lock().unwrap().len(),
        ]
    }

    /// Drops what the open transaction staged after `mark`: the scope that staged it rolled back.
    fn unstage(&self, [told, workspace, jobs]: [usize; 3]) {
        self.staged.lock().unwrap().truncate(told);
        self.staged_workspace.lock().unwrap().truncate(workspace);
        self.staged_job_events.lock().unwrap().truncate(jobs);
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

    /// An explicitly colocated service still needs its transport; identity is not connectivity.
    pub fn require_transport_link(&self) {
        self.transport_link_required.store(true, Ordering::SeqCst);
    }

    pub fn connection_ready(&self, linked: bool) -> bool {
        if self.transport_link_required.load(Ordering::SeqCst) {
            linked
        } else {
            self.is_reconciled()
        }
    }

    /// The Node this coordinator drives.
    pub fn node_id(&self) -> &NodeId {
        &self.node_id
    }

    /// Its faces, for what needs no ledger (discovery: probing, listing and reading history).
    pub fn node(&self) -> Arc<dyn Node> {
        self.node.clone()
    }

    /// Cancel a single active subtask on this Node without cancelling the parent turn.
    pub async fn cancel_subtask(&self, session: &str, subtask: &str) -> ApiResult<bool> {
        let session =
            SessionId::new(session).map_err(|e| Error::new(Code::Invalid, e.to_string()))?;
        let subtask = CallId::new(subtask).map_err(|e| Error::new(Code::Invalid, e.to_string()))?;
        let res = self
            .node
            .cancel_subtask(CancelSubtask { session, subtask })
            .await;
        let outcome = match &res {
            Ok(true) => "cancelled",
            Ok(false) => "noop",
            Err(_) => "error",
        };
        metrics::counter!(
            "steve_subtask_cancels_total",
            "node" => self.node_id.to_string(),
            "outcome" => outcome,
        )
        .increment(1);
        res
    }
}

/// The option ids of a Node permission card.
const ALLOW: &str = "allow_once";
const REJECT: &str = "reject";

/// What the business side is told after a commit ([`NodeRuntime::tell_later`]).
enum Tell {
    After(After),
    End(End),
    Stalled(steve_types::orchestration::StallEvidence),
    StalledUsage(steve_types::orchestration::StallReason, [i64; 4]),
    StalledAttemptStopped(steve_types::orchestration::StallReason, i64),
    /// A diagnostic record of an applied event.
    Diagnostic(Box<steve_store::diagnostics::Record>),
}

/// What the business side hears after a commit.
enum After {
    AutoTurn,
    PermissionIngress {
        resolver: &'static str,
        outcome: &'static str,
    },
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
        facts: AttemptFacts,
        at: i64,
        /// What it was for `steve_model_*`, recorded with the rest once committed.
        measure: Option<steve_store::instrumentation::ModelCall>,
    },
    Cost {
        session: String,
    },
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

/// One of the Node's streams that carries this session's events is faulted.
fn faulted(
    r: &mut impl steve_store::Read,
    node: &NodeId,
    session: &str,
) -> Result<bool, StoreError> {
    node_sessions::faulted(r, node.as_str(), session, &heads::session_stream(session))
}

/// `sessions.status` once a turn is over and nothing follows it: `error` after a failure, else `idle`.
fn settled_status(stop_reason: &str) -> &'static str {
    if stop_reason == "error" || steve_core::runtime::is_model_service_failure(stop_reason) {
        "error"
    } else {
        "idle"
    }
}

/// A `system` entry of the session, for the people reading it.
fn system_in(tx: &mut W<'_>, session: &str, text: &str) -> Result<(), StoreError> {
    event_in(tx, session, "system", json!({ "text": text }))
}

/// A session entry of the Hub's own (not one of a Node's events), in `tx`'s write.
fn event_in(tx: &mut W<'_>, session: &str, kind: &str, data: Value) -> Result<(), StoreError> {
    tx.events().add(session, kind, data, None, now())?;
    Ok(())
}

impl NodeRuntime {
    fn session_revision(&self, session: &str) -> Result<i64, StoreError> {
        self.store.read(|r| node_sessions::revision(r, session))
    }

    fn turn_revision(&self, turn: &str) -> Result<i64, StoreError> {
        self.store.read(|r| node_turns::revision(r, turn))
    }

    fn last_attempt_in(&self, turn: &str) -> Result<String, StoreError> {
        self.store.read(|r| node_turns::latest_attempt(r, turn))
    }

    fn ended_in(&self, session: &str) -> Result<bool, StoreError> {
        self.store.read(|r| node_sessions::ended(r, session))
    }

    fn phase_in(&self, session: &str) -> Result<Option<(String, Option<String>)>, StoreError> {
        self.store.read(|r| {
            Ok(node_sessions::phase(r, session, None)?
                .map(|phase| (phase.name, phase.active_attempt)))
        })
    }

    fn command<T: serde::Serialize>(
        &self,
        op: &str,
        body: T,
    ) -> Result<(Value, String), StoreError> {
        command_at(op, body, self.clock.now())
    }

    fn status_in(&self, session: &str, status: &str) -> Result<(), StoreError> {
        self.store
            .write(|w| node_sessions::set_status(w, session, status, self.clock.now()))
    }

    fn attempt_row(&self, attempt: &str) -> Result<Option<node_turns::Attempt>, StoreError> {
        self.store.read(|r| node_turns::attempt(r, attempt))
    }
}
