//! Steve Hub domain logic: the item lifecycle (orchestrator.ts, lifecycle.ts), the main conversation
//! (thread.ts), Steve's own model calls (gateway.ts), conversational configuration (changes.ts) and
//! housekeeping (housekeeping.ts).
//!
//! Concurrency: every state-changing operation runs synchronously under one lock ([`Core`]'s state mutex),
//! which keeps the ordering the TS Hub gets from Node's single thread ("write, publish, update memory" never
//! interleaves). The lock is never held across an `.await`: model calls happen outside it and re-enter.
//! Agent turns run inside the [`AgentRuntime`], which reports back through [`Core::on_turn_end`] and the
//! `tool_*` methods from its own tasks. Git commands run inside the critical section, like `execFileSync`
//! in TS, because their result decides what the same operation does next — except for an item on a
//! Node, whose git is gathered off the lock ([`node_git`]).

steve_i18n::init!();

mod agent_questions;
pub mod approval;
pub mod attachments;
pub mod changes;
pub mod config;
mod context_handoff;
pub mod cycle;
mod dependencies;
mod diagnostics;
pub mod error;
pub mod gateway;
pub mod git;
pub mod harnesses;
pub mod housekeeping;
pub mod kinds;
pub mod lifecycle;
mod marks;
pub mod models;
mod modules;
pub mod node_git;
pub mod notice_line;
mod occupancy;
mod orchestrator;
pub mod outbox_work;
mod placement;
pub mod proof;
mod quota;
pub mod release;
pub mod repo;
pub mod runtime;
pub mod stall;
pub mod stats;
pub mod takeover;
pub mod thread;
mod turn_receipts;
mod turn_wait;
pub mod upstream;
pub mod util;
pub mod workflow_run;
pub mod workspace;
mod worktrees;

#[cfg(test)]
mod testkit;
#[cfg(test)]
mod tests;

pub use config::CoreConfig;
pub use context_handoff::{
    BotDefaultSnapshot, BotDefaultUpdated, HANDOFF_UNAVAILABLE_SUBJECT, HandoffOutcome,
    HandoffReason, HandoffTarget,
};
pub use error::{CoreError, HandoffError, Result};
pub use gateway::{ModelGateway, OneModel, SharedGateway, SingleModelGateway, Tier};
pub use orchestrator::{
    CreateItem, DecisionCommand, Forked, ItemAction, SendNow, SupplementMode, design_pending,
};
pub use quota::has_quota_reset;
pub use runtime::{
    AgentRuntime, Answered, Check, Flushed, McpEndpoint, ModeState, Output, PermissionAction,
    PermissionAnswer, Prompted, Released, Running, Stop, TurnEnd,
};
pub use workflow_run::{
    ChildResultError, MAX_BUDGET_ALLOWANCE_DELTA, MAX_CONCURRENT_WORKFLOW_STEPS,
    MAX_STEP_CAS_RETRIES, MAX_STEP_INFRA_RETRIES, WF_RUN_SUBJECT, WfBudget, WfDecisionChoice,
    WfDeliveryBlocker, WfRunAction, WfRunSnapshot, WfToolOutput, WorkflowStepExecutor,
    validate_child_result,
};
pub use worktrees::{CLOSED_WORKTREE_BACKFILL_KEY, WorkspaceFailure, WorktreeBackfillReport};

use repo::Db;
use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, OnceLock};

/// The domain layer. Cheap to clone; all clones share one state.
#[derive(Clone)]
pub struct Core(Arc<Inner>);

pub(crate) struct Inner {
    store: Db,
    runtime: Arc<dyn AgentRuntime>,
    gateway: Arc<dyn ModelGateway>,
    config: CoreConfig,
    /// 设置 · 模型 (the same resolver the gateway's [`SingleModelGateway`] uses).
    models: models::Models,
    state: Mutex<State>,
    thread: thread::ThreadRuntime,
    /// The B1 registries (item hooks, prompt contexts, decision handlers); unset = no modules.
    modules: OnceLock<steve_modules::Modules>,
    hook_queue: modules::HookQueue,
    /// What Steve has to tell a session with its next message (a delivery that went void meanwhile).
    steve_notes: Mutex<HashMap<String, String>>,
    /// A reclaim pass is running (`workspace`): one at a time.
    reclaiming: std::sync::atomic::AtomicBool,
    /// Last observed free bytes on the worktrees volume (reported by Node housekeeping; `u64::MAX` = unknown).
    last_free_bytes: std::sync::atomic::AtomicU64,
    /// The way to items' Nodes, once wired (`node_git`).
    node_git: OnceLock<Arc<dyn node_git::NodeGit>>,
    /// Workspace disk inspection and reclamation on the colocated Node (`workspace`).
    housekeeping: OnceLock<Arc<dyn workspace::NodeHousekeeping>>,
    /// Out-of-process JS workflow step executor (`StepRunner` wired by `steve-hub`).
    workflow_executor: OnceLock<Arc<dyn workflow_run::WorkflowStepExecutor>>,
    /// Per-run async locks serializing `step_workflow_run` executions for the same `run_id`.
    workflow_run_locks: Mutex<HashMap<String, workflow_run::WorkflowRunLockEntry>>,
    /// Global concurrency cap across simultaneous `step_workflow_run` executions.
    workflow_step_permits: Arc<tokio::sync::Semaphore>,
    /// Per-run cache of `(script_version, call_id)` pairs already verified for cross-version
    /// hydration so unused completed calls are not re-verified on every step; cleared on terminal.
    workflow_hydration_verified: Mutex<HashMap<String, std::collections::HashSet<(i64, String)>>>,
}

/// What a session's last Steve tool call left behind, consumed at its turn end.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Mark {
    Asked,
    Delivered,
    Approval,
}

#[derive(Debug, Clone)]
pub(crate) struct Releasing {
    token: String,
    item: String,
    operation: Option<String>,
    /// Bumped per release so a stale hold timer does nothing.
    generation: u64,
    /// When the drain began holding new turns (ms).
    hold_since: Option<i64>,
}

/// In-memory state of the TS module globals (lost on restart there too).
#[derive(Default)]
pub(crate) struct State {
    /// The session's last Steve tool call, with the turn it was made in (`None`: none ran then).
    pending: HashMap<String, (Mark, Option<runtime::Running>)>,
    tokens: HashMap<String, String>,
    /// Items whose turn failed on a model quota that is back later (`quota`).
    quota_waits: HashMap<String, quota::QuotaWait>,
    releasing: Option<Releasing>,
    release_generation: u64,
    /// Item hook calls collected in this critical section, run once the lock is gone.
    hooks: Vec<modules::HookEvent>,
    /// Items the user marked done, with what to restore if they undo it from the toast.
    /// Turns to stop once the lock is gone `(turn, item)`: a cancel can call back into the core
    /// (the gateway-retry wait ends with a turn end), so a lifecycle stop never cancels under the lock.
    /// The turn is frozen when the stop is asked for, so a turn started meanwhile is left alone.
    cancels: Vec<(runtime::Running, String)>,
    /// Why new turns wait now (`occupancy`).
    holds: std::collections::BTreeSet<occupancy::Hold>,
    /// Items handing over to a new session while the old one's release is on its way: the
    /// release (`Released::Pending`) the item's next session waits for, taken when it starts.
    handoffs: HashMap<String, String>,
    /// Sessions created by handoff waiting to record their first-turn outcome metric.
    handoff_outcomes: HashMap<String, &'static str>,
    /// Execution surfaces currently known to be quota-exhausted or failing `(harness, model) -> until_ms`.
    surface_cooldowns: HashMap<context_handoff::SurfaceKey, i64>,
    /// Per-item consecutive surface error streaks and surfaces already failed during this outage.
    surface_errors: HashMap<String, context_handoff::SurfaceFailureState>,
    /// Metadata for open coordinator surface-unavailable cards (`decision_id -> meta`).
    handoff_cards: HashMap<String, context_handoff::HandoffCardMeta>,
    /// Git facts of items on a Node, gathered off the lock (`node_git`).
    git: node_git::Slots,
    /// Critical sections entered so far (facts taken in one may be read again in the same one).
    tx_seq: u64,
    /// The periodic closing check is running: what it asks of a Node is no new event, so it
    /// doesn't outdate a gathering of the same facts already running (`node_git`).
    git_polling: bool,
    /// The periodic closing check is running: it fingerprints a worktree only when nothing else
    /// holds the closing (`cycle.rs`).
    evaluating_closing_cycles: bool,
    /// Script-workflow `run_id`s whose next replay step should run asynchronously after the lock drops.
    wf_step_wakeups: Vec<String>,
}

/// One critical section: the core plus the held state lock.
pub(crate) struct Tx<'a> {
    c: &'a Core,
    // After Tx::drop's hook handling, record hold time before the state guard unlocks.
    _timing: Option<steve_store::instrumentation::LockTiming>,
    st: MutexGuard<'a, State>,
}

impl Core {
    pub fn new(
        store: Db,
        runtime: Arc<dyn AgentRuntime>,
        gateway: Arc<dyn ModelGateway>,
        config: CoreConfig,
    ) -> Core {
        let models = models::Models::new(store.store().clone(), models::Defaults::of(&config));
        Core(Arc::new(Inner {
            store,
            runtime,
            gateway,
            models,
            config,
            state: Mutex::new(State::default()),
            thread: thread::ThreadRuntime::default(),
            modules: OnceLock::new(),
            hook_queue: OnceLock::new(),
            steve_notes: Default::default(),
            reclaiming: Default::default(),
            last_free_bytes: std::sync::atomic::AtomicU64::new(u64::MAX),
            node_git: OnceLock::new(),
            housekeeping: OnceLock::new(),
            workflow_executor: OnceLock::new(),
            workflow_run_locks: Default::default(),
            workflow_step_permits: Arc::new(tokio::sync::Semaphore::new(
                workflow_run::MAX_CONCURRENT_WORKFLOW_STEPS,
            )),
            workflow_hydration_verified: Default::default(),
        }))
    }

    /// Puts `note` in front of the next message session `sid` gets (several notes: in order).
    pub(crate) fn steve_note(&self, sid: &str, note: &str) {
        let mut n = self.0.steve_notes.lock().unwrap_or_else(|e| e.into_inner());
        n.entry(sid.into())
            .and_modify(|x| {
                if !x.contains(note) {
                    x.push_str("\n\n");
                    x.push_str(note);
                }
            })
            .or_insert_with(|| note.into());
    }

    /// The text for session `sid` with Steve's waiting note in front of it (once).
    pub(crate) fn with_steve_note(&self, sid: &str, text: &str) -> String {
        let n = self
            .0
            .steve_notes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(sid);
        match n {
            Some(n) => format!("{n}\n\n{text}"),
            None => text.to_string(),
        }
    }

    /// The database (the domain layer's adapter; `.store()` on it gives the raw `steve_store::Store`).
    pub fn store(&self) -> &Db {
        &self.0.store
    }
    /// Interrupt the session's running turn on whichever backend runs it (the user's stop);
    /// false if nothing was running; an error when the backend cannot tell. The user's Stop: a
    /// backend may also retire what holds the session ([`AgentRuntime::user_stop`]).
    pub fn cancel_turn(&self, session_id: &str) -> Result<bool> {
        /* your Stop is kept before it goes out (its turn end may come after a restart): how that
         * turn ends is yours to go on with, never Steve's to continue (`crate::turn_wait::stopped`) */
        let sessions = self.0.store.store().sessions();
        let asked = self.0.runtime.running(session_id).map(|r| r.attempt);
        sessions.stop_asked(session_id, Some(asked.as_deref().unwrap_or("")))?;
        /* only a backend that says the turn was over withdraws your Stop. One that fails to answer
         * (its Node unreachable, its store failing) leaves the outcome unknown: the turn may still
         * stop or end, so your Stop stays kept and that end, or the idle session, is yours to go on
         * with. Steve never weakens a Stop it could not confirm into an ordinary stop it continues;
         * your next message starts another turn and lifts it (`Tx::on_turn_end`) */
        let stopped = self.0.runtime.user_stop(session_id)? == Stop::Requested;
        if !stopped {
            sessions.stop_asked(session_id, None)?;
        }
        Ok(stopped)
    }
    pub fn config(&self) -> &CoreConfig {
        &self.0.config
    }
    /// Which models Steve's own decisions and new agent sessions use (`models::for_purpose`).
    pub fn models(&self) -> &models::Models {
        &self.0.models
    }
    pub(crate) fn gw(&self) -> &dyn ModelGateway {
        &*self.0.gateway
    }
    pub(crate) fn rt(&self) -> &dyn AgentRuntime {
        &*self.0.runtime
    }

    /// Enter the critical section.
    #[track_caller]
    pub(crate) fn tx(&self) -> Tx<'_> {
        let timing =
            steve_store::instrumentation::LockTiming::start("core", std::panic::Location::caller());
        let mut st = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        st.tx_seq += 1;
        Tx {
            c: self,
            st,
            _timing: timing.map(steve_store::instrumentation::LockTiming::acquired),
        }
    }
}

/// Run `f` off the current critical section inside the caller's tokio runtime blocking pool, if
/// there is one (or a bare thread in synchronous unit tests with no runtime). Using the runtime's
/// blocking pool instead of a raw thread with `Handle::enter` keeps `block_in_place` / `block_on`
/// valid inside detached work such as `disk_low -> reclaim_workspaces` and git gathers.
pub(crate) fn detach(f: impl FnOnce() + Send + 'static) {
    match tokio::runtime::Handle::try_current() {
        Ok(rt) => {
            rt.spawn_blocking(f);
        }
        Err(_) => {
            std::thread::spawn(f);
        }
    }
}

impl Tx<'_> {
    pub(crate) fn db(&self) -> &Db {
        &self.c.0.store
    }
    pub(crate) fn rt(&self) -> &dyn AgentRuntime {
        &*self.c.0.runtime
    }
    pub(crate) fn cfg(&self) -> &CoreConfig {
        &self.c.0.config
    }
    /// Interrupt the session's running turn right away (the turn looked at now and stopped now);
    /// false if none runs. A backend that cannot tell is logged and counts as nothing stopped.
    pub(crate) fn stop_now(&self, sid: &str) -> bool {
        match self.rt().cancel(sid) {
            Ok(stop) => stop == Stop::Requested,
            Err(e) => {
                tracing::error!(session = sid, "stopping the turn: {e}");
                false
            }
        }
    }
}

#[cfg(test)]
mod lock_metrics_tests {
    use super::*;

    struct Sink(
        Arc<Mutex<Vec<(metrics::Key, f64)>>>,
        std::sync::Weak<Inner>,
        Arc<std::sync::atomic::AtomicBool>,
    );
    struct Sample(
        metrics::Key,
        Arc<Mutex<Vec<(metrics::Key, f64)>>>,
        std::sync::Weak<Inner>,
        Arc<std::sync::atomic::AtomicBool>,
    );
    impl metrics::HistogramFn for Sample {
        fn record(&self, value: f64) {
            if self.0.name() == "steve_lock_hold_seconds"
                && let Some(core) = self.2.upgrade()
            {
                self.3.store(
                    core.state.try_lock().is_err(),
                    std::sync::atomic::Ordering::Relaxed,
                );
            }
            self.1.lock().unwrap().push((self.0.clone(), value));
        }
    }
    impl metrics::Recorder for Sink {
        fn describe_counter(
            &self,
            _: metrics::KeyName,
            _: Option<metrics::Unit>,
            _: metrics::SharedString,
        ) {
        }
        fn describe_gauge(
            &self,
            _: metrics::KeyName,
            _: Option<metrics::Unit>,
            _: metrics::SharedString,
        ) {
        }
        fn describe_histogram(
            &self,
            _: metrics::KeyName,
            _: Option<metrics::Unit>,
            _: metrics::SharedString,
        ) {
        }
        fn register_counter(
            &self,
            _: &metrics::Key,
            _: &metrics::Metadata<'_>,
        ) -> metrics::Counter {
            metrics::Counter::noop()
        }
        fn register_gauge(&self, _: &metrics::Key, _: &metrics::Metadata<'_>) -> metrics::Gauge {
            metrics::Gauge::noop()
        }
        fn register_histogram(
            &self,
            key: &metrics::Key,
            _: &metrics::Metadata<'_>,
        ) -> metrics::Histogram {
            metrics::Histogram::from_arc(Arc::new(Sample(
                key.clone(),
                self.0.clone(),
                self.1.clone(),
                self.2.clone(),
            )))
        }
    }

    #[test]
    fn core_lock_records_wait_then_hold_on_release_at_the_external_callsite() {
        let core = crate::testkit::Kit::new();
        let sink = Sink(
            Arc::new(Mutex::new(vec![])),
            Arc::downgrade(&core.core.0),
            Arc::new(std::sync::atomic::AtomicBool::new(false)),
        );
        steve_store::instrumentation::enable_lock_metrics();
        metrics::with_local_recorder(&sink, || {
            let line = line!() + 1;
            let tx = core.core.tx();
            {
                let samples = sink.0.lock().unwrap();
                assert_eq!(samples.len(), 1);
                assert_eq!(samples[0].0.name(), "steve_lock_wait_seconds");
            }
            drop(tx);
            let samples = sink.0.lock().unwrap();
            assert_eq!(samples.len(), 2);
            assert_eq!(samples[1].0.name(), "steve_lock_hold_seconds");
            assert!(
                sink.2.load(std::sync::atomic::Ordering::Relaxed),
                "hold timestamp must be taken before unlocking state"
            );
            assert!(samples.iter().all(|(key, value)| {
                *value >= 0.0
                    && key.labels().any(|label| {
                        label.key() == "caller" && label.value() == format!("{}:{line}", file!())
                    })
                    && key
                        .labels()
                        .any(|label| label.key() == "lock" && label.value() == "core")
            }));
        });
    }
}
