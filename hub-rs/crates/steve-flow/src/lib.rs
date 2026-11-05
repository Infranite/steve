//! Signals, automation flows and schedules
//!
//! Spec: steve-rs-plan/modules/signals-automation.md, schedules.md. So far: S7a (cron, Chinese time
//! phrases, missed/overlap policy) and S7 (scheduled tasks: storage, scheduler, run history, the
//! `action` node, budget, REST, conversational config), S1 (events and signals: [`EventAdapter`],
//! [`ingest()`], `/signals`, `/sources`, retention) and S2 (the flow engine: [`graph`] validation,
//! [`GraphEngine`], the node registry [`nodes`], versions and the trigger index [`flows`], dry runs,
//! restart recovery, the `flow_step` / `flow_run` / `signal` decisions, `/flows` and `/runs`), S3
//! ([`nodes::script`], [`nodes::cmd`], [`nodes::http`]: QuickJS sandbox, internal-address guard,
//! `{{secrets}}` from the vault, `cmd.run` / `http.effect` grants), S4 (the `bot` node [`nodes::bot`], `POST /flows/draft`, `POST /flows/:id/edit-draft`), S8
//! ([`corrections`]: `POST /signals/:id/correct`, the `correction.flow` / `correction.rule` change
//! operations) and S9 (conversational config of sources and flows: the `source` and `flow` targets
//! in [`drafts`]) and S10 (Bots and items: `GET /bots/:id/flows` and [`on_bot_deleted`] in
//! [`bots`], [`ingest_item_event`], [`digest::take_now`], the MCP tool `steve_event_get` in [`mcp`]).
//!
//! Integration (integrator), once at startup:
//! ```ignore
//! let setup = steve_flow::FlowSetup::new(core);                    // ports::Core over steve-core
//! setup.register(&mut registry);                                   // `schedule`/`source`/`flow` draft targets + ops, `correction.*` ops, `flow_run`, `flow_step`, `signal` decisions
//! let modules = Modules::with_clock(store, gateway, registry, clock);
//! let setup = setup
//!     .connections(conns.clone())                                  // A2 `Connections`: ConnCx for adapters
//!     .event_adapter(gmail.clone())                                // each `EventAdapter` (S5, S6…)
//!     .node(Arc::new(steve_flow::nodes::http::Http::new(vault.clone())))  // S3: `http` with `{{secrets}}` (built in without the vault)
//!     .node(Arc::new(steve_flow::nodes::cmd::Cmd::new(&steve_home)))      // S3: `cmd` (built in with the temp dir as home)
//!     .node(Arc::new(steve_flow::nodes::bot::BotNode::new(guard)))  // S4: `bot` with the vault's `guard::scan` (built in without a guard)
//!     .action(reply_as.clone())                                    // channel actions (effectful)
//!     .item_finder("branch", github_branches.clone());             // better `item.link` lookups
//! let flow = setup.build(modules, Arc::new(steve_flow::GraphEngine));
//! // steve-core's `Core` port: implement `supplement` too (item.link); the default refuses
//! api_router.merge(steve_flow::routes(flow.clone()));              // /schedules…, behind the Caller layer
//! tokio::spawn(steve_flow::Scheduler::new(flow.clone()).run());    // `flow.scheduler` (recover + missed runs first)
//! tokio::spawn(steve_flow::Poller::new(flow.clone()).run());       // `flow.poll`
//! tokio::spawn(steve_flow::retention::run(flow.clone()));          // `flow.retention`
//! // other modules (mail E1, GitHub G2, channels F1) get the entry point as a trait object:
//! let sink: Arc<dyn steve_flow::EventSink> = Arc::new(flow.clone());
//! // steve-core SETTINGS.data: append steve_flow::retention::keep_raw_setting()
//! // steve-core, after every `sessions.cost` update:
//! steve_flow::on_session_cost(&flow, &session_id).await;
//! ```
//!
//! S10 call sites in steve-core / steve-mcp (integrator):
//! ```ignore
//! let setup = setup.guard(vault_guard.clone());                    // `guard::scan` for steve_event_get (default finds nothing)
//! // Orchestrator::delete_bot(id, to): after it succeeded, for every workspace `w` the bot served
//! // (`bot_workspaces` rows survive the soft delete), with the heir it used there (`to`, or the main
//! // bot where `to` is not a member):
//! flow.store().transaction(|tx| steve_flow::on_bot_deleted(tx, &w, &id, &heir, flow.now()))?;
//! // after finish(item) → status done / drop(item) → status drop (outside any transaction, may be
//! // spawned; errors only logged):
//! steve_flow::ingest_item_event(&flow, &ws, &item_id, steve_flow::ItemEvent::Done).await;   // or ::Dropped
//! // the away-report and `/汇报`: entries noted since `since`, marked taken
//! let notes = steve_flow::digest::take_now(&flow, &ws, since)?;
//! // steve-mcp: list steve_flow::mcp::tools() after the Steve tools; for names where
//! // steve_flow::mcp::handles(name): steve_flow::mcp::call(&flow, &mcp_cx, name, &args).await
//! ```

pub mod adapter;
pub mod api;
pub mod bots;
pub mod budget;
pub mod corrections;
pub mod cron;
pub mod decisions;
pub mod digest;
pub mod drafts;
pub mod engine;
mod events_api;
pub mod exec;
pub mod expr;
mod flow_drafts;
pub mod flows;
mod flows_api;
pub mod graph;
pub mod ingest;
pub mod item_events;
pub mod link;
pub mod mcp;
pub mod nodes;
pub mod policy;
pub mod ports;
pub mod retention;
pub mod runs;
pub mod scheduler;
pub mod schedules;
pub mod signals;
pub mod sources;
pub mod testkit;
pub mod when;

use std::sync::{Arc, OnceLock, Weak};

use steve_modules::{ApiError, Clock, Modules, Registry};
use steve_store::Store;
use tokio::sync::{Mutex, Notify, mpsc};

pub use adapter::{
    ActorRef, AdapterError, Batch, DropReason, EventAdapter, EventSink, EventTypeDef, IngestMode,
    Ingested, PushRequest, RawEvent, ScopeDef,
};
pub use api::routes;
pub use bots::on_bot_deleted;
pub use budget::on_session_cost;
pub use engine::GraphEngine;
pub use exec::{LinearExecutor, RunExecutor, RunStatus};
pub use ingest::{Poller, ingest};
pub use item_events::{ItemEvent, ItemTrigger, ingest_item_event};
pub use link::ItemFinder;
pub use nodes::action::Action;
pub use nodes::{NodeKind, NodeOutcome, RunCx};
pub use ports::{Core, ItemRequest};
pub use scheduler::Scheduler;

/// Errors of this crate: the module API error (`400/404/409` with Chinese text, `500` otherwise).
pub type FlowError = ApiError;
pub type FlowResult<T> = Result<T, FlowError>;

/// Work that must happen after a decision's transaction commits (decision handlers run inside it
/// and cannot call steve-core).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Followup {
    /// `continue_once` on a budget decision: resume the paused item.
    Resume {
        ws: String,
        item_id: String,
        decision_id: String,
    },
    /// A run a decision woke (`flow_step` answered, `flow_run` retried): drive it.
    Drive { run_id: String, decision_id: String },
    /// `to_item` on a `notify` decision: make the item.
    ToItem {
        ws: String,
        event_id: String,
        decision_id: String,
    },
}

/// Everything the flow module runs on. Built by [`FlowSetup::build`]; shared as `Arc<FlowCx>`.
/// Time and the model gateway are the modules' (`modules.clock`, `modules.gateway`).
pub struct FlowCx {
    pub modules: Modules,
    pub core: Arc<dyn Core>,
    /// [`GraphEngine`] (or the old `LinearExecutor`, which only knows schedule graphs).
    pub exec: Arc<dyn RunExecutor>,
    /// Node kinds (built-ins plus registered ones).
    pub nodes: nodes::NodeRegistry,
    /// `item.link` lookups.
    pub finders: link::ItemFinders,
    /// Registered event sources.
    pub events: sources::EventAdapters,
    /// Dropped events of the last 24 hours (the source page's counts).
    pub drops: ingest::DropStats,
    /// SSE `signal` coalescing.
    pub signals: signals::SignalDebounce,
    /// The vault's secret scanner, for text handed to agents (`steve_event_get`).
    pub guard: Arc<dyn nodes::bot::Guard>,
    wake: Arc<Notify>,
    followups: Mutex<mpsc::UnboundedReceiver<Followup>>,
    /// `true` once the scheduler's boot recovered the interrupted runs ([`Self::recovered`]).
    recovered: tokio::sync::watch::Sender<bool>,
}

impl FlowCx {
    pub fn store(&self) -> &Store {
        &self.modules.store
    }

    pub fn now(&self) -> i64 {
        self.modules.clock.now()
    }

    /// Makes the scheduler look at its triggers now (a schedule was created or changed).
    pub fn wake(&self) {
        self.wake.notify_one();
    }

    pub(crate) fn notified(&self) -> &Notify {
        &self.wake
    }

    /// Resolves once [`Scheduler::boot`] has recovered the runs a restart interrupted (or at once
    /// after it). Whatever starts runs from stored state on its own (the outbox's
    /// `automation.item_trigger`) waits for it, so `engine::recover` and it never drive one run twice.
    pub async fn recovered(&self) {
        let mut rx = self.recovered.subscribe();
        let _ = rx.wait_for(|done| *done).await;
    }

    pub(crate) fn mark_recovered(&self) {
        self.recovered.send_replace(true);
    }

    /// Runs synchronous store work on the blocking pool (the shape of `Modules::blocking`), so a
    /// transaction waiting for the writer does not hold up a runtime worker.
    pub async fn blocking<R: Send + 'static>(
        &self,
        f: impl FnOnce(&Store) -> FlowResult<R> + Send + 'static,
    ) -> FlowResult<R> {
        let store = self.store().clone();
        tokio::task::spawn_blocking(move || f(&store))
            .await
            .map_err(|e| ApiError::Internal(format!("task failed: {e}")))?
    }

    /// Runs the queued [`Followup`]s one after another and returns when they are done (tests, and
    /// callers that want the work finished).
    pub async fn run_followups(&self) {
        let mut rx = self.followups.lock().await;
        while let Ok(f) = rx.try_recv() {
            decisions::followup(self, f).await;
        }
    }

    /// Starts each queued [`Followup`] in its own task, so a resumed run does not hold up the
    /// scheduler round. Each task holds its own activity permit and runs admitted; a follow-up is
    /// only taken off the queue once it has a permit, so none is lost while maintenance waits.
    pub async fn spawn_followups(self: &Arc<Self>) -> Vec<tokio::task::JoinHandle<()>> {
        let mut rx = self.followups.lock().await;
        let mut out = Vec::new();
        while let Some(permit) = self.store().try_activity("flow_followup") {
            let Ok(f) = rx.try_recv() else {
                break;
            };
            let cx = self.clone();
            out.push(tokio::spawn(async move {
                let _permit = permit;
                cx.store().admitted(decisions::followup(&cx, f)).await;
            }));
        }
        out
    }
}

/// The clock of [`Modules`], for registry entries created before it exists (the draft
/// operations). Real time until [`FlowSetup::build`] binds it.
#[derive(Default)]
pub struct LateClock(OnceLock<Arc<dyn Clock>>);

impl Clock for LateClock {
    fn now(&self) -> i64 {
        self.0.get().map_or_else(steve_store::now, |c| c.now())
    }
}

/// The built module, for registry entries made before it exists (the `source` and `flow` draft
/// targets need the event adapters and the node registry). A `Weak` so the registry inside
/// [`Modules`] does not keep its own [`FlowCx`] alive.
#[derive(Default)]
pub struct LateFlow(OnceLock<Weak<FlowCx>>);

impl LateFlow {
    /// The module, or `500` before [`FlowSetup::build`] (or after it was dropped).
    pub fn get(&self) -> FlowResult<Arc<FlowCx>> {
        self.0
            .get()
            .and_then(Weak::upgrade)
            .ok_or_else(|| ApiError::Internal("flow module not built".into()))
    }
}

/// The two-step construction the [`Registry`] forces: decision handlers and draft operations are
/// registered before [`Modules`] exists, but need its clock and the follow-up queue.
pub struct FlowSetup {
    core: Arc<dyn Core>,
    clock: Arc<LateClock>,
    late: Arc<LateFlow>,
    events: sources::EventAdapters,
    actions: nodes::action::Actions,
    nodes: Vec<Arc<dyn NodeKind>>,
    finders: link::ItemFinders,
    guard: Arc<dyn nodes::bot::Guard>,
    wake: Arc<Notify>,
    tx: mpsc::UnboundedSender<Followup>,
    rx: mpsc::UnboundedReceiver<Followup>,
}

impl FlowSetup {
    pub fn new(core: Arc<dyn Core>) -> Self {
        let (tx, rx) = mpsc::unbounded_channel();
        Self {
            core,
            clock: Arc::default(),
            late: Arc::default(),
            events: sources::EventAdapters::default(),
            actions: nodes::action::Actions::default(),
            nodes: Vec::new(),
            finders: link::ItemFinders::default(),
            guard: Arc::new(nodes::bot::NoGuard),
            wake: Arc::new(Notify::new()),
            tx,
            rx,
        }
    }

    /// Registers the `schedule`, `source` and `flow` draft targets, their operations
    /// (`schedule.create`, `schedule.patch`, `source.patch`, `flow.save`, `flow.patch`) and the `flow_run`, `flow_step` and `signal` decision handlers.
    pub fn register(&self, r: &mut Registry) {
        drafts::register(r, self.clock.clone(), self.wake.clone());
        corrections::register(r, self.clock.clone());
        drafts::source::register(r, self.late.clone());
        drafts::flow::register(r, self.late.clone());
        r.decision(
            budget::SUBJECT,
            Box::new(budget::FlowRunDecisions::new(
                self.tx.clone(),
                self.wake.clone(),
            )),
        );
        r.decision(
            nodes::approve::SUBJECT,
            Box::new(decisions::FlowStepDecisions::new(
                self.tx.clone(),
                self.wake.clone(),
            )),
        );
        r.decision(
            nodes::action::SIGNAL_SUBJECT,
            Box::new(decisions::SignalDecisions::new(
                self.tx.clone(),
                self.wake.clone(),
            )),
        );
    }

    /// Registers a node kind (`script`, `cmd`, `http`, `bot`, plugin kinds); replaces a built-in of
    /// the same name.
    pub fn node(mut self, k: Arc<dyn NodeKind>) -> Self {
        self.nodes.push(k);
        self
    }

    /// Registers an action of the `action` node (`reply_as`, `send_to_space`, …).
    pub fn action(mut self, a: Arc<dyn Action>) -> Self {
        self.actions.add(a);
        self
    }

    /// Registers an `item.link` lookup for `by` (`branch`, `ref`, `thread`); asked before the
    /// built-in one.
    pub fn item_finder(mut self, by: &str, f: Arc<dyn ItemFinder>) -> Self {
        self.finders.add(by, f);
        self
    }

    /// The vault's `guard::scan` for event text handed to agents (`steve_event_get`); without it
    /// nothing is redacted.
    pub fn guard(mut self, g: Arc<dyn nodes::bot::Guard>) -> Self {
        self.guard = g;
        self
    }

    /// Registers an event source (its `id()` is the `connections.adapter` it serves).
    pub fn event_adapter(mut self, a: Arc<dyn EventAdapter>) -> Self {
        self.events.add(a);
        self
    }

    /// The connections service adapters get their [`ConnCx`](steve_modules::accounts::connections::ConnCx) from.
    pub fn connections(mut self, c: steve_modules::accounts::connections::Connections) -> Self {
        self.events.set_connections(c);
        self
    }

    pub fn build(self, modules: Modules, exec: Arc<dyn RunExecutor>) -> Arc<FlowCx> {
        let _ = self.clock.0.set(modules.clock.clone());
        let mut registry = nodes::NodeRegistry::builtin(self.actions);
        for k in self.nodes {
            registry.add(k);
        }
        let cx = Arc::new(FlowCx {
            modules,
            core: self.core,
            exec,
            nodes: registry,
            finders: self.finders,
            events: self.events,
            drops: ingest::DropStats::default(),
            signals: signals::SignalDebounce::default(),
            guard: self.guard,
            wake: self.wake,
            followups: Mutex::new(self.rx),
            recovered: tokio::sync::watch::Sender::new(false),
        });
        let _ = self.late.0.set(Arc::downgrade(&cx));
        cx
    }
}
