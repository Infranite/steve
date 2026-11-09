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

pub mod attachments;
pub mod changes;
pub mod config;
pub mod cycle;
mod dependencies;
mod diagnostics;
pub mod error;
pub mod gateway;
pub mod git;
pub mod housekeeping;
pub mod kinds;
pub mod lane;
pub mod mq;
pub mod lifecycle;
mod marks;
pub mod models;
mod modules;
pub mod node_git;
pub mod notify;
mod occupancy;
mod orchestrator;
pub mod outbox_work;
pub mod proof;
pub mod release;
pub mod repo;
pub mod runtime;
pub mod stall;
pub mod stats;
pub mod takeover;
pub mod thread;
pub mod upstream;
pub mod util;
pub mod workflow;
pub mod workspace;
mod worktrees;
pub mod yolo;

#[cfg(test)]
mod testkit;
#[cfg(test)]
mod tests;

pub use config::CoreConfig;
pub use error::{CoreError, Result};
pub use gateway::{ClaudeCli, Fallback, LocalPrint, ModelGateway, OneModel, SharedGateway, Tier};
pub use orchestrator::{CreateItem, Forked, ItemAction, RESUMED_NOTE, SendNow, SupplementMode};
pub use runtime::{
    AgentRuntime, Answered, Check, Configured, Flushed, McpEndpoint, Output, Prompted, Released,
    Running, Stop, TurnEnd,
};

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
    /// 设置 · 模型 (the same resolver the gateway's [`ClaudeCli`] uses).
    models: models::Models,
    state: Mutex<State>,
    thread: thread::Lanes,
    /// The B1 registries (item hooks, prompt contexts, decision handlers); unset = no modules.
    modules: OnceLock<steve_modules::Modules>,
    hook_queue: modules::HookQueue,
    /// The YOLO switch, once wired (the protocol and the harness mode depend on it).
    yolo: OnceLock<yolo::Yolo>,
    /// What running sessions were told about YOLO.
    yolo_sessions: yolo::agent::Sessions,
    /// What Steve has to tell a session with its next message (a delivery that went void meanwhile).
    steve_notes: Mutex<HashMap<String, String>>,
    /// A reclaim pass or trash purge is running (`workspace`): one at a time.
    reclaiming: std::sync::atomic::AtomicBool,
    /// The way to items' Nodes, once wired (`node_git`); unset: the Hub's own git everywhere.
    node_git: OnceLock<Arc<dyn node_git::NodeGit>>,
    /// The Node the merge queue's staging is on, once wired (`mq::drive`).
    mq_node: OnceLock<Arc<dyn mq::exec::MqNode>>,
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
    /// The last turn end each session had (its attempt): the same end told again is ignored.
    ended: HashMap<String, String>,
    releasing: Option<Releasing>,
    release_generation: u64,
    /// An acceptance answered with 验收并发布 `(item, who)`: its release requirement starts the
    /// release right away instead of asking the agent (`release.rs` `release_with_accept`).
    release_with_accept: Option<(String, String)>,
    drafts: HashMap<String, changes::Proposal>,
    applied: HashMap<String, changes::Applied>,
    /// Item hook calls collected in this critical section, run once the lock is gone.
    hooks: Vec<modules::HookEvent>,
    /// Items the user marked done, with what to restore if they undo it from the toast.
    marked_done: HashMap<String, orchestrator::MarkedDone>,
    /// Turns to stop once the lock is gone `(turn, item)`: a cancel can call back into the core
    /// (the gateway-retry wait ends with a turn end), so a lifecycle stop never cancels under the lock.
    /// The turn is frozen when the stop is asked for, so a turn started meanwhile is left alone.
    cancels: Vec<(runtime::Running, String)>,
    /// Items whose completion rule is being checked against the remote now (`takeover`).
    probing: HashMap<String, takeover::Looking>,
    /// Numbers the looks in `probing`, so a look that comes back is told from a newer one.
    probe_seq: u64,
    /// The last completion probe of each item.
    probed: HashMap<String, takeover::Probed>,
    /// Followed builds: when each was last looked at.
    build_checked: HashMap<String, i64>,
    /// Why new turns wait now (`occupancy`).
    holds: std::collections::BTreeSet<occupancy::Hold>,
    /// Items handing over to a new session while the old one's release is on its way: the
    /// release (`Released::Pending`) the item's next session waits for, taken when it starts.
    handoffs: HashMap<String, String>,
    /// Git facts of items on a Node, gathered off the lock (`node_git`).
    git: node_git::Slots,
    /// Release reports waiting for an item's facts to prove what they shipped (`node_git`).
    git_proofs: HashMap<String, Vec<node_git::PendingProof>>,
    /// Critical sections entered so far (facts taken in one may be read again in the same one).
    tx_seq: u64,
    /// The periodic closing check is running: what it asks of a Node is no new event, so it
    /// doesn't outdate a gathering of the same facts already running (`node_git`).
    git_polling: bool,
    /// The periodic closing check is running: it fingerprints a worktree only when nothing else
    /// holds the closing (`cycle.rs`).
    lc_ticking: bool,
}

/// One critical section: the core plus the held state lock.
pub(crate) struct Tx<'a> {
    c: &'a Core,
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
            thread: thread::Lanes::default(),
            modules: OnceLock::new(),
            hook_queue: OnceLock::new(),
            yolo: OnceLock::new(),
            yolo_sessions: Default::default(),
            steve_notes: Default::default(),
            reclaiming: Default::default(),
            node_git: OnceLock::new(),
            mq_node: OnceLock::new(),
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

    /// Wires the YOLO switch in (once).
    pub fn set_yolo(&self, y: yolo::Yolo) {
        let _ = self.0.yolo.set(y);
    }

    pub(crate) fn yolo(&self) -> Option<&yolo::Yolo> {
        self.0.yolo.get()
    }

    /// YOLO decides for the user in `ws` now (for `item`'s decisions when given): `false` without YOLO
    /// wired or when the setting can't be read.
    pub fn yolo_applies(&self, ws: &str, item: Option<&str>) -> bool {
        self.yolo()
            .and_then(|y| y.effective(ws, item).ok())
            .is_some_and(|e| e.on)
    }

    /// The database (the domain layer's adapter; `.store()` on it gives the raw `steve_store::Store`).
    pub fn store(&self) -> &Db {
        &self.0.store
    }
    /// Interrupt the session's running turn on whichever backend runs it (the user's stop);
    /// false if nothing was running; an error when the backend cannot tell. The user's Stop: a
    /// backend may also retire what holds the session ([`AgentRuntime::user_stop`]).
    pub fn cancel_turn(&self, session_id: &str) -> Result<bool> {
        Ok(self.0.runtime.user_stop(session_id)? == Stop::Requested)
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
    pub(crate) fn gw_arc(&self) -> Arc<dyn ModelGateway> {
        self.0.gateway.clone()
    }

    /// Enter the critical section.
    pub(crate) fn tx(&self) -> Tx<'_> {
        let mut st = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        st.tx_seq += 1;
        Tx { c: self, st }
    }
}

/// Run `f` on its own thread inside the caller's tokio runtime, if there is one. A TS timer runs on the event loop;
/// a bare thread has no runtime, and the agent runtime starts turns (and the item hooks run) with `tokio::spawn`.
pub(crate) fn detach(f: impl FnOnce() + Send + 'static) {
    let rt = tokio::runtime::Handle::try_current().ok();
    std::thread::spawn(move || {
        let _in = rt.as_ref().map(|h| h.enter());
        f()
    });
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
