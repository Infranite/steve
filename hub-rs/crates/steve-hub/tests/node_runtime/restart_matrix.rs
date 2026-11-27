//! W3.1 (#480 / #528) parameterized restart matrix:
//!
//! 10 execution breakpoints (covering plan before/after commit, send before/after,
//! Node accept/persist, effect occurred without receipt, Hub stream/receipt commit
//! before ack, and post-ack/GC) × {Hub restart, Node restart} on a FakeNode + FakeClock.
//!
//! Every cell with a durable receipt must converge to a normalized state identical to an
//! uninterrupted baseline trace; every cell with an unknown outcome must converge to Held
//! (`unknown` / uncertain). Every cell also verifies 0 duplicate side effects, 0 late
//! receipt reversals, 0 unconfirmed operations treated as succeeded, and duplicate-ack
//! idempotence.

use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};

use async_trait::async_trait;
use steve_hub::wire::acp_stats;
use steve_hub::wire::node_runtime::{
    AttemptFacts, Binding, NodeDomain, NodeRuntime, Planned, Sink,
};
use steve_node_api::testkit::{FakeNode, Scripted, build};
use steve_node_api::*;
use steve_runtime::testkit::{eventually_sync, on_timeout};
use steve_runtime::{Clock, FakeClock};
use steve_store::testkit::hub::{mutations, node_checks, node_fixtures as fixtures};
use steve_store::{FnPublisher, Store};

const SESSION: &str = "s1";
const PROMPT: &str = "matrix prompt";
const REPLY: &str = "matrix reply";
/// Fixed virtual epoch (2030-01-01T00:00:00Z): ensures runtime retry, envelope, and
/// deadline logic never silently leak machine wall clock time.
const VIRTUAL_EPOCH: i64 = 1_893_456_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RestartSide {
    Hub,
    Node,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Breakpoint {
    /// Plan transaction aborted/rolled back before committing; message remains queued.
    BeforePlanCommit,
    /// Plan transaction committed (`pending`, `maybe_sent=0`, `lease_generation=0`), before send.
    AfterPlanCommit,
    /// Pre-admission retry-safe refusal (`Unavailable + Retry::Later`) before reaching Node.
    BeforeSendRetryable,
    /// Command leased (`maybe_sent=1`, `lease_generation=1`), interrupted before reaching Node.
    BeforeSendLeased,
    /// Command sent on wire, reply lost (`OutcomeUnknown + Retry::Later`), Node has no record.
    AfterSendUnconfirmed,
    /// Node accepted & persisted (`Running` + `AttemptStarted`), reply dropped & stream stashed.
    NodeAcceptedPersisted,
    /// Send reply + `AttemptStarted` committed on Hub (`active`/`running`), before completion.
    AfterSendRunning,
    /// Effect completed on Node (`TextDelta` + `AttemptEnded` + `Finished`), not yet on Hub.
    EffectOccurredUnreceipted,
    /// Hub committed stream & final receipt (`turn_ended` fired), ack lost before Node applied it.
    HubCommittedBeforeAck,
    /// Node received ack and GC'd journal (`unacked == 0`), restart afterward.
    AfterAckGc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ExpectedOutcome {
    MatchesBaseline,
    Held,
}

/// Business & execution state normalized across ephemeral IDs, revisions, and idle harness
/// release (a Node restart releases idle harnesses while keeping logical state identical).
#[derive(Debug, Clone, PartialEq, Eq)]
struct NormalizedTrace {
    session_status: String,
    session_idle: bool,
    active_attempt: bool,
    uncertain: bool,
    busy: bool,
    working: usize,
    turns: Vec<String>,
    dispatches: Vec<(String, String)>,
    queued: i64,
    final_start_state: String,
    node_executed_starts: usize,
    turn_ends: Vec<(String, String, Option<String>)>,
    folded_text: String,
    transcript: Vec<(String, i64)>,
    duplicate_node_events: i64,
    faulted_streams: i64,
    node_unacked: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct HeldTrace {
    uncertain: bool,
    busy: bool,
    working: usize,
    active_attempt: bool,
    attempt_state: String,
    command_state: String,
    turn_state: String,
    dispatch_state: String,
    turn_ends: usize,
    folded_text: String,
    followup_created: bool,
    duplicate_node_events: i64,
    faulted_streams: i64,
}

impl HeldTrace {
    fn expected() -> Self {
        Self {
            uncertain: true,
            busy: true,
            working: 1,
            active_attempt: true,
            attempt_state: "unknown".into(),
            command_state: "unknown".into(),
            turn_state: "running".into(),
            dispatch_state: "claimed".into(),
            turn_ends: 0,
            folded_text: String::new(),
            followup_created: false,
            duplicate_node_events: 0,
            faulted_streams: 0,
        }
    }
}

#[derive(Default)]
struct DomainLog {
    ends: Mutex<Vec<(String, String, Option<String>)>>,
    store: std::sync::OnceLock<Store>,
}

impl NodeDomain for DomainLog {
    fn turn_ended(&self, session: &str, _: Option<&str>, stop: &str, error: Option<String>) {
        self.ends
            .lock()
            .unwrap()
            .push((session.into(), stop.into(), error));
    }

    fn attempt_started(
        &self,
        session: &str,
        attempt: &str,
        ordinal: i64,
        user_seq: Option<i64>,
        at: i64,
    ) {
        if let Some(store) = self.store.get() {
            acp_stats::node_attempt_started(store, session, attempt, ordinal, user_seq, at);
        }
    }

    fn attempt_finished(
        &self,
        session: &str,
        attempt: &str,
        c: &Completion,
        f: &AttemptFacts,
        at: i64,
    ) {
        if let Some(store) = self.store.get() {
            acp_stats::node_attempt_finished(store, session, attempt, c, f, at);
        }
    }
}

/// Controllable `EventSink` between the FakeNode and the active Hub coordinator.
struct GateSink {
    target: Mutex<Weak<NodeRuntime>>,
    seen: Mutex<Vec<NodeEvent>>,
    batches: Mutex<Vec<EventBatch>>,
    /// While `Some`, incoming batches are stashed instead of reaching the Hub.
    stash: Mutex<Option<Vec<EventBatch>>>,
    /// When true, `commit` fails with `Unavailable` (events stay in Node journal for replay).
    refuse: AtomicBool,
    /// When true, forwards to the Hub (Hub commits) but returns an empty ack so the Node
    /// retains the events unacknowledged (`Point::BeforeAck`).
    lose_acks: AtomicBool,
    lost_acks: AtomicUsize,
}

impl GateSink {
    fn new(rt: &Arc<NodeRuntime>) -> Arc<Self> {
        Arc::new(Self {
            target: Mutex::new(Arc::downgrade(rt)),
            seen: Mutex::new(vec![]),
            batches: Mutex::new(vec![]),
            stash: Mutex::new(None),
            refuse: AtomicBool::new(false),
            lose_acks: AtomicBool::new(false),
            lost_acks: AtomicUsize::new(0),
        })
    }

    fn retarget(&self, rt: &Arc<NodeRuntime>) {
        *self.target.lock().unwrap() = Arc::downgrade(rt);
    }

    fn stash_on(&self) {
        *self.stash.lock().unwrap() = Some(vec![]);
    }

    fn take_stashed(&self) -> Vec<EventBatch> {
        self.stash.lock().unwrap().take().unwrap_or_default()
    }

    fn all_batches(&self) -> Vec<EventBatch> {
        self.batches.lock().unwrap().clone()
    }
}

#[async_trait]
impl EventSink for GateSink {
    async fn commit(&self, batch: EventBatch) -> ApiResult<EventAck> {
        self.seen
            .lock()
            .unwrap()
            .extend(batch.events.iter().cloned());
        self.batches.lock().unwrap().push(batch.clone());
        if self.refuse.load(Ordering::SeqCst) {
            return Err(Error::later(Code::Unavailable, "sink refused by matrix"));
        }
        if let Some(kept) = self.stash.lock().unwrap().as_mut() {
            kept.push(batch);
            return Ok(EventAck { durable: vec![] });
        }
        let weak = self.target.lock().unwrap().clone();
        let ack = Sink(weak).commit(batch).await?;
        if self.lose_acks.load(Ordering::SeqCst) && !ack.durable.is_empty() {
            self.lost_acks.fetch_add(1, Ordering::SeqCst);
            return Ok(EventAck { durable: vec![] });
        }
        Ok(ack)
    }
}

/// Fault-injecting proxy around `FakeNode` for deterministic breakpoint holds before the
/// request ever enters `FakeNode`.
enum StartIntercept {
    /// Fail immediately before touching `FakeNode` with a retry-safe error (`Unavailable`).
    PreAdmissionRefuse(Error),
    /// Fail immediately before touching `FakeNode` with `OutcomeUnknown` (frame lost in flight).
    LostAfterWrite,
    /// Block after `lease_command_in` committed and before `FakeNode` sees the command.
    BlockBeforeNode {
        entered: Arc<tokio::sync::Notify>,
        release: Arc<tokio::sync::Notify>,
    },
}

struct ProxyNode {
    inner: Arc<FakeNode>,
    observes: AtomicBool,
    generation: AtomicU64,
    start_intercept: Mutex<Option<StartIntercept>>,
    start_attempts: AtomicUsize,
}

impl ProxyNode {
    fn new(inner: Arc<FakeNode>) -> Arc<Self> {
        Arc::new(Self {
            inner,
            observes: AtomicBool::new(true),
            generation: AtomicU64::new(1),
            start_intercept: Mutex::new(None),
            start_attempts: AtomicUsize::new(0),
        })
    }

    fn set_observes(&self, on: bool) {
        self.observes.store(on, Ordering::SeqCst);
    }

    fn bump_generation(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
    }

    fn intercept_start(&self, intercept: Option<StartIntercept>) {
        *self.start_intercept.lock().unwrap() = intercept;
    }
}

#[async_trait]
impl SessionExecutor for ProxyNode {
    async fn open_session(&self, c: Command<OpenSession>) -> ApiResult<Receipt> {
        self.inner.open_session(c).await
    }
    async fn configure_session(&self, c: Command<ConfigureSession>) -> ApiResult<Receipt> {
        self.inner.configure_session(c).await
    }
    async fn start_turn(&self, c: Command<TurnSpec>) -> ApiResult<Receipt> {
        self.start_attempts.fetch_add(1, Ordering::SeqCst);
        let intercept = self.start_intercept.lock().unwrap().take();
        match intercept {
            Some(StartIntercept::PreAdmissionRefuse(e)) => {
                Err(e.with_param("operation", c.id.to_string()))
            }
            Some(StartIntercept::LostAfterWrite) => Err(Error::outcome_unknown(
                &c.id,
                "connection dropped after write before node admission",
            )
            .retry(Retry::Later { after_ms: None })),
            Some(StartIntercept::BlockBeforeNode { entered, release }) => {
                entered.notify_one();
                release.notified().await;
                Err(Error::outcome_unknown(
                    &c.id,
                    "interrupted after lease before node admission",
                ))
            }
            None => self.inner.start_turn(c).await,
        }
    }
    async fn session(&self, id: SessionId) -> ApiResult<SessionView> {
        self.inner.session(id).await
    }
    async fn release_session(&self, c: Command<SessionId>) -> ApiResult<Receipt> {
        self.inner.release_session(c).await
    }
}

#[async_trait]
impl JobExecutor for ProxyNode {
    async fn start_job(&self, c: Command<JobSpec>) -> ApiResult<Receipt> {
        self.inner.start_job(c).await
    }
}

#[async_trait]
impl ExecutionControl for ProxyNode {
    async fn operation(&self, id: OperationId) -> ApiResult<Receipt> {
        self.inner.operation(id).await
    }
    async fn cancel(&self, c: Command<Cancel>) -> ApiResult<Receipt> {
        self.inner.cancel(c).await
    }
    async fn abandon(&self, c: Command<Abandon>) -> ApiResult<Receipt> {
        self.inner.abandon(c).await
    }
    async fn recover(&self, c: Command<Recover>) -> ApiResult<Receipt> {
        self.inner.recover(c).await
    }
    async fn resolve_permission(&self, c: Command<PermissionResolution>) -> ApiResult<Receipt> {
        self.inner.resolve_permission(c).await
    }
    async fn snapshot(&self, page: PageRequest) -> ApiResult<Snapshot> {
        self.inner.snapshot(page).await
    }
    async fn streams(&self) -> ApiResult<Vec<StreamHead>> {
        self.inner.streams().await
    }
    async fn replay(&self, from: Watermark, max_bytes: u32) -> ApiResult<EventBatch> {
        self.inner.replay(from, max_bytes).await
    }
    async fn acknowledge(&self, ack: EventAck) -> ApiResult<()> {
        self.inner.acknowledge(ack).await
    }
}

#[async_trait]
impl WorkspaceExecutor for ProxyNode {
    async fn prepare_workspace(&self, c: Command<Prepare>) -> ApiResult<Receipt> {
        self.inner.prepare_workspace(c).await
    }
    async fn workspace(&self, w: WorkspaceRef) -> ApiResult<WorkspaceView> {
        self.inner.workspace(w).await
    }
    async fn tidy_workspace(&self, c: Command<Tidy>) -> ApiResult<Receipt> {
        self.inner.tidy_workspace(c).await
    }
    async fn git_facts(&self, q: GitQuery) -> ApiResult<GitFacts> {
        self.inner.git_facts(q).await
    }
    async fn git_apply(&self, c: Command<GitApply>) -> ApiResult<Receipt> {
        self.inner.git_apply(c).await
    }
    async fn exec(&self, c: Command<ExecSpec>) -> ApiResult<Receipt> {
        self.inner.exec(c).await
    }
    async fn list_dir(&self, path: PathRef, page: PageRequest) -> ApiResult<Page<FileInfo>> {
        self.inner.list_dir(path, page).await
    }
    async fn list_root(&self, at: RootPath, page: PageRequest) -> ApiResult<Page<FileInfo>> {
        self.inner.list_root(at, page).await
    }
    async fn read_file(&self, q: FileRead) -> ApiResult<ReadChunk> {
        self.inner.read_file(q).await
    }
    async fn scan(&self, c: Command<ScanSpec>) -> ApiResult<Receipt> {
        self.inner.scan(c).await
    }
    async fn scan_page(&self, scan: OperationId, page: PageRequest) -> ApiResult<ScanPage> {
        self.inner.scan_page(scan, page).await
    }
}

#[async_trait]
impl Discovery for ProxyNode {
    fn execution_generation(&self) -> Option<u64> {
        self.observes
            .load(Ordering::SeqCst)
            .then(|| self.generation.load(Ordering::SeqCst))
    }
    async fn set_session_capacity(&self, sessions: u32) -> ApiResult<()> {
        self.inner.set_session_capacity(sessions).await
    }
    async fn probe(&self, fresh: bool) -> ApiResult<Capabilities> {
        self.inner.probe(fresh).await
    }
    async fn history(&self, page: PageRequest) -> ApiResult<Page<HistoryEntry>> {
        self.inner.history(page).await
    }
    async fn import_history(&self, c: Command<HistoryImport>) -> ApiResult<Receipt> {
        self.inner.import_history(c).await
    }
    async fn import_page(&self, import: OperationId, page: PageRequest) -> ApiResult<ImportedPage> {
        self.inner.import_page(import, page).await
    }
    async fn locate(&self, source: HistoryRef) -> ApiResult<HistoryLocation> {
        self.inner.locate(source).await
    }
}

impl Node for ProxyNode {
    fn id(&self) -> &NodeId {
        self.inner.id()
    }
}

/// Hosts one Hub `NodeRuntime` inside its own Tokio runtime so a Hub crash terminates
/// in-flight async tasks at the exact `.await` point without post-crash SQLite writes.
struct HubProcess {
    rt: Arc<NodeRuntime>,
    tokio: Option<tokio::runtime::Runtime>,
}

/// Bounded wait when tearing down a simulated Hub Tokio runtime so worker threads and blocking
/// tasks finish dropping their `Store` handles before the next phase opens another store.
const HUB_SHUTDOWN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

impl HubProcess {
    fn spawn(
        store: Store,
        node: Arc<ProxyNode>,
        domain: Arc<DomainLog>,
        clock: Arc<FakeClock>,
    ) -> Self {
        // FakeNode + Store + FakeClock only use timers; omit the unused IO/signal driver so each
        // simulated Hub does not allocate extra kqueue/epoll/socketpair descriptors.
        let tokio = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_time()
            .build()
            .unwrap_or_else(|e| {
                panic!(
                    "HubProcess::spawn tokio runtime failed: {e}; fds={:?}",
                    open_fd_inventory()
                )
            });
        let rt = NodeRuntime::with_clock(store, node, domain, clock);
        {
            let _guard = tokio.enter();
            rt.start();
        }
        Self {
            rt,
            tokio: Some(tokio),
        }
    }

    async fn kill(mut self) {
        let weak = Arc::downgrade(&self.rt);
        let tokio = self.tokio.take();
        drop(self);
        if let Some(tokio) = tokio {
            let _ = tokio::task::spawn_blocking(move || {
                tokio.shutdown_timeout(HUB_SHUTDOWN_TIMEOUT);
            })
            .await;
        }
        eventually_sync("crashed Hub runtime dropped", || weak.upgrade().is_none()).await;
    }
}

impl Drop for HubProcess {
    fn drop(&mut self) {
        if let Some(tokio) = self.tokio.take() {
            tokio.shutdown_background();
        }
    }
}

struct MatrixRig {
    _dir: tempfile::TempDir,
    store: Store,
    clock: Arc<FakeClock>,
    node: Arc<FakeNode>,
    proxy: Arc<ProxyNode>,
    domain: Arc<DomainLog>,
    sink: Arc<GateSink>,
    hub: Option<HubProcess>,
}

impl MatrixRig {
    async fn ready() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("steve.sqlite3"), FnPublisher(|_| {})).unwrap();
        store.ensure_workspace().unwrap();
        fixtures::seed_sessions(&store, SESSION).unwrap();
        let clock = Arc::new(FakeClock::new(VIRTUAL_EPOCH));
        let node = FakeNode::new("n1");
        node.set_now(clock.now());
        let proxy = ProxyNode::new(node.clone());
        let domain = Arc::new(DomainLog::default());
        let _ = domain.store.set(store.clone());
        let hub = HubProcess::spawn(store.clone(), proxy.clone(), domain.clone(), clock.clone());
        let sink = GateSink::new(&hub.rt);
        node.connect(sink.clone());
        let rig = Self {
            _dir: dir,
            store,
            clock,
            node,
            proxy,
            domain,
            sink,
            hub: Some(hub),
        };
        eventually_sync("initial reconcile", || rig.rt().is_reconciled()).await;
        rig.rt()
            .bind(&Binding {
                session: SESSION.into(),
                resource: format!("item:{SESSION}"),
                prepare: PrepareKind::Scratch,
                choice: build::choice(),
                policy: build::policy(),
            })
            .unwrap();
        eventually_sync("session ready", || {
            rig.phase() == "ready" && rig.node.unacked() == 0
        })
        .await;
        rig
    }

    fn rt(&self) -> &Arc<NodeRuntime> {
        &self.hub.as_ref().expect("hub running").rt
    }

    fn phase(&self) -> String {
        node_checks::binding_phase_by_session(&self.store, SESSION).unwrap()
    }

    fn ends(&self) -> Vec<(String, String, Option<String>)> {
        self.domain.ends.lock().unwrap().clone()
    }

    fn node_executed_starts(&self) -> usize {
        self.node
            .log()
            .into_iter()
            .filter(|(m, _)| *m == "start_turn")
            .count()
    }

    fn latest_start_op(&self) -> OperationId {
        OperationId::new(
            node_checks::latest_command_operation_id_by_method(&self.store, "start_turn").unwrap(),
        )
        .unwrap()
    }

    fn latest_execution(&self) -> ExecutionRef {
        let rows = fixtures::ordered_attempt_executions(&self.store).unwrap();
        serde_json::from_str(rows.last().expect("attempt execution")).unwrap()
    }

    fn advance(&self, ms: i64) {
        let at = self.clock.advance(ms);
        self.node.set_now(at);
    }

    async fn restart_hub(&mut self) {
        if let Some(old) = self.hub.take() {
            old.kill().await;
        }
        // Mirror Hub boot closing any cut statistics rows before the new coordinator starts.
        acp_stats::interrupt_open(&self.store, self.clock.now());
        self.proxy.bump_generation();
        let next = HubProcess::spawn(
            self.store.clone(),
            self.proxy.clone(),
            self.domain.clone(),
            self.clock.clone(),
        );
        self.sink.retarget(&next.rt);
        self.hub = Some(next);
        eventually_sync("restarted Hub reconciled", || self.rt().is_reconciled()).await;
    }

    async fn restart_node(&self) {
        self.node.restart();
        self.proxy.bump_generation();
        self.rt().reconcile_again().await.unwrap();
    }

    async fn shutdown(mut self) {
        if let Some(old) = self.hub.take() {
            old.kill().await;
        }
    }

    async fn restart(&mut self, side: RestartSide) {
        match side {
            RestartSide::Hub => self.restart_hub().await,
            RestartSide::Node => self.restart_node().await,
        }
    }

    async fn finish_active_turn(&self) {
        eventually_sync(
            on_timeout(|| async {
                format!(
                    "node accepted active turn; starts={}, reconciled={}, observes={}, snap={:?}",
                    self.node_executed_starts(),
                    self.rt().is_reconciled(),
                    self.proxy.observes.load(Ordering::SeqCst),
                    fixtures::session_runtime_snapshot(&self.store, SESSION),
                )
            }),
            || self.node_executed_starts() == 1,
        )
        .await;
        let exec = self.latest_execution();
        eventually_sync(
            on_timeout(|| async {
                format!(
                    "attempt running before finish; attempt={}, snap={:?}",
                    exec.attempt(),
                    fixtures::session_runtime_snapshot(&self.store, SESSION),
                )
            }),
            || node_checks::attempt_start_committed(&self.store, exec.attempt().as_str()).unwrap(),
        )
        .await;
        self.emit_reply_and_finish(&exec).await;
    }

    async fn emit_reply_and_finish(&self, exec: &ExecutionRef) {
        self.node
            .emit(
                Subject::Execution(exec.clone()),
                EventBody::TextDelta {
                    message: "m1".into(),
                    block: "b1".into(),
                    utf8_offset: 0,
                    text: REPLY.into(),
                },
            )
            .await
            .unwrap();
        self.node.finish(exec, build::done()).await.unwrap();
    }

    fn normalized_trace(&self) -> NormalizedTrace {
        let snap = fixtures::session_runtime_snapshot(&self.store, SESSION).unwrap();
        let final_start_state = snap
            .start_commands
            .last()
            .map(|(s, _, _)| s.clone())
            .unwrap_or_default();
        NormalizedTrace {
            session_status: snap.session_status,
            session_idle: matches!(snap.binding_phase.as_str(), "ready" | "released"),
            active_attempt: snap.active_attempt.is_some(),
            uncertain: self.rt().is_uncertain(SESSION),
            busy: self.rt().is_busy(SESSION),
            working: self.rt().working(),
            turns: snap.turns,
            dispatches: snap.dispatches,
            queued: snap.queued,
            final_start_state,
            node_executed_starts: self.node_executed_starts(),
            turn_ends: self.ends(),
            folded_text: snap.folded_text,
            transcript: snap.transcript,
            duplicate_node_events: snap.duplicate_node_events,
            faulted_streams: snap.faulted_streams,
            node_unacked: self.node.unacked(),
        }
    }

    fn held_trace(&self, followup_created: bool) -> HeldTrace {
        let snap = fixtures::session_runtime_snapshot(&self.store, SESSION).unwrap();
        HeldTrace {
            uncertain: self.rt().is_uncertain(SESSION),
            busy: self.rt().is_busy(SESSION),
            working: self.rt().working(),
            active_attempt: snap.active_attempt.is_some(),
            attempt_state: snap.attempts.last().cloned().unwrap_or_default(),
            command_state: snap
                .start_commands
                .last()
                .map(|(s, _, _)| s.clone())
                .unwrap_or_default(),
            turn_state: snap.turns.last().cloned().unwrap_or_default(),
            dispatch_state: snap
                .dispatches
                .last()
                .map(|(_, s)| s.clone())
                .unwrap_or_default(),
            turn_ends: self.ends().len(),
            folded_text: snap.folded_text,
            followup_created,
            duplicate_node_events: snap.duplicate_node_events,
            faulted_streams: snap.faulted_streams,
        }
    }

    /// Inject out-of-order / late receipts, duplicate acks, and replayed event batches, then
    /// assert that no terminal or unknown state reverses and no duplicate events appear.
    async fn assert_late_receipts_and_duplicate_acks_are_noops(&self, include_unknown: bool) {
        let op = self.latest_start_op();
        let before = fixtures::session_runtime_snapshot(&self.store, SESSION).unwrap();
        let before_ends = self.ends();
        let sink = Sink(Arc::downgrade(self.rt()));

        // Duplicate ack on every stream head is idempotent.
        let heads = self.node.streams().await.unwrap();
        let durable: Vec<Watermark> = heads
            .into_iter()
            .filter(|h| h.acknowledged > 0)
            .map(|h| Watermark {
                stream: h.stream,
                epoch: h.epoch,
                seq: h.acknowledged,
            })
            .collect();
        if !durable.is_empty() {
            self.node
                .acknowledge(EventAck {
                    durable: durable.clone(),
                })
                .await
                .unwrap();
            self.node.acknowledge(EventAck { durable }).await.unwrap();
        }

        // Replay every previously delivered event batch.
        for batch in self.sink.all_batches() {
            sink.commit(batch).await.unwrap();
        }

        // Inject late out-of-order OperationChanged receipts on the op stream.
        let mut states = vec![Observed::Accepted, Observed::Running];
        if include_unknown {
            states.push(Observed::Unknown {
                reason: "late stale unknown".into(),
            });
        }
        let op_stream = StreamId::new(derived_id("op", op.as_str())).unwrap();
        let mut seq = 0u64;
        for (i, state) in states.into_iter().enumerate() {
            seq += 1;
            let batch = EventBatch {
                events: vec![NodeEvent {
                    schema: EVENT_SCHEMA,
                    id: EventId::new(format!("late_receipt_{i}_{seq}")).unwrap(),
                    at: Timestamp(self.clock.now()),
                    cursor: Watermark {
                        stream: op_stream.clone(),
                        epoch: Epoch::new("late_epoch").unwrap(),
                        seq,
                    },
                    subject: Subject::Operation {
                        operation: op.clone(),
                    },
                    body: EventBody::OperationChanged {
                        receipt: Receipt {
                            operation: op.clone(),
                            state,
                        },
                    },
                }],
            };
            sink.commit(batch).await.unwrap();
        }

        let after = fixtures::session_runtime_snapshot(&self.store, SESSION).unwrap();
        assert_eq!(
            after, before,
            "late receipts/replays must never reverse state"
        );
        assert_eq!(
            self.ends(),
            before_ends,
            "late receipts/replays must never duplicate turn_ended"
        );
    }
}

async fn baseline_trace() -> NormalizedTrace {
    let rig = MatrixRig::ready().await;
    assert_eq!(
        rig.rt().prompt(SESSION, PROMPT, "user", &[]).unwrap(),
        Planned::Created
    );
    rig.finish_active_turn().await;
    eventually_sync("baseline settled", || {
        rig.ends().len() == 1 && rig.node.unacked() == 0 && rig.phase() == "ready"
    })
    .await;
    let trace = rig.normalized_trace();
    assert_eq!(trace.node_executed_starts, 1);
    assert_eq!(trace.turns, ["finished"]);
    assert_eq!(trace.dispatches, [(PROMPT.into(), "completed".into())]);
    assert_eq!(trace.folded_text, REPLY);
    assert_eq!(trace.turn_ends, [(SESSION.into(), "end_turn".into(), None)]);
    assert_eq!(trace.duplicate_node_events, 0);
    assert_eq!(trace.faulted_streams, 0);
    assert_eq!(trace.node_unacked, 0);
    rig.shutdown().await;
    trace
}

async fn run_matrix_cell(bp: Breakpoint, side: RestartSide, expected: ExpectedOutcome) {
    let baseline = baseline_trace().await;
    let mut rig = MatrixRig::ready().await;

    match bp {
        Breakpoint::BeforePlanCommit => {
            // Queue input, then abort T1 right before node_commands insert so plan rolls back.
            rig.store
                .queued()
                .add(SESSION, PROMPT, "user", &[])
                .unwrap();
            mutations::create_refuse_push(&rig.store).unwrap();
            assert!(
                rig.rt().flush(SESSION, None).is_err(),
                "plan tx must fail before commit"
            );
            mutations::remove_refuse_push(&rig.store).unwrap();
            let before = fixtures::session_runtime_snapshot(&rig.store, SESSION).unwrap();
            assert_eq!(before.queued, 1);
            assert!(before.turns.is_empty());
            assert!(before.start_commands.is_empty());
            assert_eq!(rig.node_executed_starts(), 0);

            rig.restart(side).await;
            rig.finish_active_turn().await;
        }
        Breakpoint::AfterPlanCommit => {
            // Freeze sender before lease_command_in, commit T1 (`pending`, `maybe_sent=0`, `gen=0`).
            rig.proxy.set_observes(false);
            assert_eq!(
                rig.rt().prompt(SESSION, PROMPT, "user", &[]).unwrap(),
                Planned::Created
            );
            let snap = fixtures::session_runtime_snapshot(&rig.store, SESSION).unwrap();
            assert_eq!(snap.start_commands, [("pending".into(), 0, false)]);
            assert_eq!(rig.node_executed_starts(), 0);

            rig.restart(side).await;
            rig.proxy.set_observes(true);
            // Advance virtual clock slightly to wake sender loop deterministically.
            rig.advance(1_000);
            rig.finish_active_turn().await;
        }
        Breakpoint::BeforeSendRetryable => {
            // Pre-admission retry-safe refusal: leased once (`gen=1`), `maybe_sent` cleared to 0.
            rig.proxy
                .intercept_start(Some(StartIntercept::PreAdmissionRefuse(Error::later(
                    Code::Unavailable,
                    "offline before write",
                ))));
            assert_eq!(
                rig.rt().prompt(SESSION, PROMPT, "user", &[]).unwrap(),
                Planned::Created
            );
            eventually_sync("retry-safe pre-admission refusal recorded", || {
                let s = fixtures::session_runtime_snapshot(&rig.store, SESSION).unwrap();
                s.start_commands == [("pending".into(), 1, false)]
            })
            .await;
            assert_eq!(rig.node_executed_starts(), 0);
            let retry_at =
                node_checks::command_retry_at_by_method(&rig.store, "start_turn").unwrap();
            assert!(retry_at > rig.clock.now());

            rig.restart(side).await;
            rig.advance(retry_at - rig.clock.now());
            rig.finish_active_turn().await;
        }
        Breakpoint::BeforeSendLeased => {
            // Hold after lease_command_in (`maybe_sent=1`, `gen=1`) before request reaches Node.
            let entered = Arc::new(tokio::sync::Notify::new());
            let release = Arc::new(tokio::sync::Notify::new());
            rig.proxy
                .intercept_start(Some(StartIntercept::BlockBeforeNode {
                    entered: entered.clone(),
                    release: release.clone(),
                }));
            assert_eq!(
                rig.rt().prompt(SESSION, PROMPT, "user", &[]).unwrap(),
                Planned::Created
            );
            entered.notified().await;
            let snap = fixtures::session_runtime_snapshot(&rig.store, SESSION).unwrap();
            assert_eq!(snap.start_commands, [("pending".into(), 1, true)]);
            assert_eq!(rig.node_executed_starts(), 0);

            match side {
                RestartSide::Hub => {
                    // Kill the Hub process mid-await before the RPC touches the Node.
                    rig.restart_hub().await;
                }
                RestartSide::Node => {
                    rig.node.restart();
                    rig.proxy.bump_generation();
                    release.notify_one();
                    rig.rt().reconcile_again().await.unwrap();
                }
            }
        }
        Breakpoint::AfterSendUnconfirmed => {
            // Frame left Hub (`maybe_sent=1`, `OutcomeUnknown + Retry::Later`), Node never persisted.
            rig.proxy
                .intercept_start(Some(StartIntercept::LostAfterWrite));
            assert_eq!(
                rig.rt().prompt(SESSION, PROMPT, "user", &[]).unwrap(),
                Planned::Created
            );
            eventually_sync("uncertain start persisted", || {
                node_checks::uncertain_start_count(&rig.store).unwrap() == 1
            })
            .await;
            assert_eq!(rig.node_executed_starts(), 0);

            rig.restart(side).await;
        }
        Breakpoint::NodeAcceptedPersisted => {
            // Node persists Running + AttemptStarted, reply dropped and stream stashed before Hub.
            rig.sink.stash_on();
            rig.node.script_once("start_turn", Scripted::DropReply);
            assert_eq!(
                rig.rt().prompt(SESSION, PROMPT, "user", &[]).unwrap(),
                Planned::Created
            );
            eventually_sync(
                "node persisted acceptance while hub stays uncertain",
                || {
                    rig.node_executed_starts() == 1
                        && rig.node.unacked() >= 1
                        && node_checks::uncertain_start_count(&rig.store).unwrap() == 1
                },
            )
            .await;
            let before = fixtures::session_runtime_snapshot(&rig.store, SESSION).unwrap();
            assert_eq!(before.attempts, ["planned"]);
            assert_eq!(before.start_commands, [("pending".into(), 1, true)]);

            let stashed = rig.sink.take_stashed();
            rig.restart(side).await;
            // Deliver any stale stashed pre-restart batches late to test deduplication.
            let sink = Sink(Arc::downgrade(rig.rt()));
            for batch in stashed {
                sink.commit(batch).await.unwrap();
            }
            if expected == ExpectedOutcome::MatchesBaseline {
                let exec = rig.latest_execution();
                eventually_sync("reconciled running turn", || {
                    node_checks::attempt_start_committed(&rig.store, exec.attempt().as_str())
                        .unwrap()
                })
                .await;
                rig.emit_reply_and_finish(&exec).await;
            }
        }
        Breakpoint::AfterSendRunning => {
            // Hub merged Running receipt & AttemptStarted; turn running on Node before effect.
            assert_eq!(
                rig.rt().prompt(SESSION, PROMPT, "user", &[]).unwrap(),
                Planned::Created
            );
            eventually_sync("start committed on hub", || {
                rig.node_executed_starts() == 1
                    && node_checks::attempt_start_committed(
                        &rig.store,
                        rig.latest_execution().attempt().as_str(),
                    )
                    .unwrap()
                    && rig.node.unacked() == 0
            })
            .await;

            rig.restart(side).await;
            if expected == ExpectedOutcome::MatchesBaseline {
                let exec = rig.latest_execution();
                rig.emit_reply_and_finish(&exec).await;
            }
        }
        Breakpoint::EffectOccurredUnreceipted => {
            // Turn runs and completes on Node, while Hub sink refuses all completion batches.
            assert_eq!(
                rig.rt().prompt(SESSION, PROMPT, "user", &[]).unwrap(),
                Planned::Created
            );
            eventually_sync("start committed before effect", || {
                rig.node_executed_starts() == 1
                    && node_checks::attempt_start_committed(
                        &rig.store,
                        rig.latest_execution().attempt().as_str(),
                    )
                    .unwrap()
                    && rig.node.unacked() == 0
            })
            .await;
            let exec = rig.latest_execution();
            rig.sink.refuse.store(true, Ordering::SeqCst);
            rig.emit_reply_and_finish(&exec).await;
            assert!(rig.node.unacked() >= 2, "unreceipted events kept on Node");
            assert!(rig.ends().is_empty(), "Hub has not received completion");

            rig.sink.refuse.store(false, Ordering::SeqCst);
            rig.restart(side).await;
        }
        Breakpoint::HubCommittedBeforeAck => {
            // Hub commits TextDelta + AttemptEnded + Finished receipt, but ack is dropped.
            assert_eq!(
                rig.rt().prompt(SESSION, PROMPT, "user", &[]).unwrap(),
                Planned::Created
            );
            eventually_sync("start committed before hub commit", || {
                rig.node_executed_starts() == 1
                    && node_checks::attempt_start_committed(
                        &rig.store,
                        rig.latest_execution().attempt().as_str(),
                    )
                    .unwrap()
                    && rig.node.unacked() == 0
            })
            .await;
            let exec = rig.latest_execution();
            rig.sink.lose_acks.store(true, Ordering::SeqCst);
            rig.emit_reply_and_finish(&exec).await;
            eventually_sync("hub committed completion while acks lost", || {
                rig.ends().len() == 1 && rig.sink.lost_acks.load(Ordering::SeqCst) >= 2
            })
            .await;
            assert!(rig.node.unacked() >= 2, "Node still retains unacked events");

            rig.sink.lose_acks.store(false, Ordering::SeqCst);
            rig.restart(side).await;
        }
        Breakpoint::AfterAckGc => {
            // Turn fully completed and Node journal drained/GC'd before restart.
            assert_eq!(
                rig.rt().prompt(SESSION, PROMPT, "user", &[]).unwrap(),
                Planned::Created
            );
            rig.finish_active_turn().await;
            eventually_sync("settled and GC'd", || {
                rig.ends().len() == 1 && rig.node.unacked() == 0 && rig.phase() == "ready"
            })
            .await;

            rig.restart(side).await;
        }
    }

    match expected {
        ExpectedOutcome::MatchesBaseline => {
            eventually_sync(
                on_timeout(|| async {
                    format!(
                        "{bp:?} × {side:?} settles to baseline; snap={:?}, ends={:?}, unacked={}",
                        fixtures::session_runtime_snapshot(&rig.store, SESSION),
                        rig.ends(),
                        rig.node.unacked()
                    )
                }),
                || {
                    rig.ends().len() == 1
                        && rig.node.unacked() == 0
                        && matches!(rig.phase().as_str(), "ready" | "released")
                },
            )
            .await;
            let actual = rig.normalized_trace();
            assert_eq!(
                actual, baseline,
                "{bp:?} × {side:?} diverged from uninterrupted baseline"
            );
            rig.assert_late_receipts_and_duplicate_acks_are_noops(true)
                .await;
            // After virtual clock wakes, no duplicate work starts.
            rig.advance(10_000);
            assert_eq!(rig.node_executed_starts(), 1);
            assert_eq!(rig.ends().len(), 1);
        }
        ExpectedOutcome::Held => {
            eventually_sync(
                on_timeout(|| async {
                    format!(
                        "{bp:?} × {side:?} converges to Held; snap={:?}, uncertain={}",
                        fixtures::session_runtime_snapshot(&rig.store, SESSION),
                        rig.rt().is_uncertain(SESSION)
                    )
                }),
                || {
                    let snap = fixtures::session_runtime_snapshot(&rig.store, SESSION).unwrap();
                    rig.rt().is_uncertain(SESSION)
                        && snap.attempts.last().map(String::as_str) == Some("unknown")
                        && snap.start_commands.last().map(|(s, _, _)| s.as_str()) == Some("unknown")
                },
            )
            .await;
            let starts_after_restart = rig.node_executed_starts();
            let followup = rig
                .rt()
                .prompt(SESSION, "followup while held", "user", &[])
                .unwrap();
            let flush = rig.rt().flush(SESSION, None).unwrap();
            let held = rig.held_trace(followup == Planned::Created || flush == Planned::Created);
            assert_eq!(
                held,
                HeldTrace::expected(),
                "{bp:?} × {side:?} must converge to canonical Held state"
            );
            // Advance virtual clock well past retry/lease windows: must stay Held with 0 resends.
            for _ in 0..5 {
                rig.advance(6 * 60_000);
            }
            assert_eq!(
                rig.node_executed_starts(),
                starts_after_restart,
                "Held operation must never be re-executed"
            );
            assert!(
                rig.ends().is_empty(),
                "unconfirmed op never treated as succeeded"
            );
            assert!(rig.rt().is_uncertain(SESSION));
            rig.assert_late_receipts_and_duplicate_acks_are_noops(false)
                .await;
        }
    }
    rig.shutdown().await;
}

macro_rules! matrix_cells {
    ($( $name:ident => ($bp:expr, $side:expr, $expected:expr); )* ) => {
        $(
            #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
            async fn $name() {
                run_matrix_cell($bp, $side, $expected).await;
            }
        )*
    };
}

matrix_cells! {
    bp1_before_plan_commit_restart_hub => (Breakpoint::BeforePlanCommit, RestartSide::Hub, ExpectedOutcome::MatchesBaseline);
    bp1_before_plan_commit_restart_node => (Breakpoint::BeforePlanCommit, RestartSide::Node, ExpectedOutcome::MatchesBaseline);

    bp2_after_plan_commit_restart_hub => (Breakpoint::AfterPlanCommit, RestartSide::Hub, ExpectedOutcome::MatchesBaseline);
    bp2_after_plan_commit_restart_node => (Breakpoint::AfterPlanCommit, RestartSide::Node, ExpectedOutcome::MatchesBaseline);

    bp3_before_send_retryable_restart_hub => (Breakpoint::BeforeSendRetryable, RestartSide::Hub, ExpectedOutcome::MatchesBaseline);
    bp3_before_send_retryable_restart_node => (Breakpoint::BeforeSendRetryable, RestartSide::Node, ExpectedOutcome::MatchesBaseline);

    bp3b_before_send_leased_restart_hub => (Breakpoint::BeforeSendLeased, RestartSide::Hub, ExpectedOutcome::Held);
    bp3b_before_send_leased_restart_node => (Breakpoint::BeforeSendLeased, RestartSide::Node, ExpectedOutcome::Held);

    bp4_after_send_unconfirmed_restart_hub => (Breakpoint::AfterSendUnconfirmed, RestartSide::Hub, ExpectedOutcome::Held);
    bp4_after_send_unconfirmed_restart_node => (Breakpoint::AfterSendUnconfirmed, RestartSide::Node, ExpectedOutcome::Held);

    bp5_node_accepted_persisted_restart_hub => (Breakpoint::NodeAcceptedPersisted, RestartSide::Hub, ExpectedOutcome::MatchesBaseline);
    bp5_node_accepted_persisted_restart_node => (Breakpoint::NodeAcceptedPersisted, RestartSide::Node, ExpectedOutcome::Held);

    bp6_after_send_running_restart_hub => (Breakpoint::AfterSendRunning, RestartSide::Hub, ExpectedOutcome::MatchesBaseline);
    bp6_after_send_running_restart_node => (Breakpoint::AfterSendRunning, RestartSide::Node, ExpectedOutcome::Held);

    bp7_effect_occurred_unreceipted_restart_hub => (Breakpoint::EffectOccurredUnreceipted, RestartSide::Hub, ExpectedOutcome::MatchesBaseline);
    bp7_effect_occurred_unreceipted_restart_node => (Breakpoint::EffectOccurredUnreceipted, RestartSide::Node, ExpectedOutcome::MatchesBaseline);

    bp8_hub_committed_before_ack_restart_hub => (Breakpoint::HubCommittedBeforeAck, RestartSide::Hub, ExpectedOutcome::MatchesBaseline);
    bp8_hub_committed_before_ack_restart_node => (Breakpoint::HubCommittedBeforeAck, RestartSide::Node, ExpectedOutcome::MatchesBaseline);

    bp9_after_ack_gc_restart_hub => (Breakpoint::AfterAckGc, RestartSide::Hub, ExpectedOutcome::MatchesBaseline);
    bp9_after_ack_gc_restart_node => (Breakpoint::AfterAckGc, RestartSide::Node, ExpectedOutcome::MatchesBaseline);
}

/// Regression test for virtual-clock deadline consistency: a transient turn failure at a
/// virtual clock time far ahead of wall clock must schedule and dispatch its retry instead
/// of expiring against machine wall clock time.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn virtual_clock_transient_turn_retry_uses_injected_clock_not_wall_clock() {
    let rig = MatrixRig::ready().await;
    assert_eq!(
        rig.rt().prompt(SESSION, PROMPT, "user", &[]).unwrap(),
        Planned::Created
    );
    eventually_sync("first attempt running", || {
        rig.node_executed_starts() == 1
            && node_checks::attempt_start_committed(
                &rig.store,
                rig.latest_execution().attempt().as_str(),
            )
            .unwrap()
    })
    .await;
    let first = rig.latest_execution();
    let mut failure = build::done();
    failure.outcome = Outcome::Failed;
    failure.stop_reason = "error".into();
    failure.transient = true;
    failure.error = Some(Error::later(Code::Unavailable, "temporary gateway error"));
    rig.node.finish(&first, failure).await.unwrap();

    eventually_sync("retry attempt planned under virtual clock", || {
        fixtures::ordered_attempt_executions(&rig.store)
            .unwrap()
            .len()
            == 2
    })
    .await;
    let retry_at = node_checks::active_attempt_retry_at(&rig.store, SESSION).unwrap();
    assert_eq!(retry_at, rig.clock.now() + 20_000);
    rig.advance(20_000);
    // Wait for both FakeNode acceptance AND Hub commit of AttemptStarted before emitting
    // TextDelta/AttemptEnded so FakeNode's concurrent deliver() cannot deliver later stream
    // sequence numbers ahead of AttemptStarted (which Hub's in-order receiver would skip).
    eventually_sync(
        on_timeout(|| async {
            format!(
                "retry attempt running on virtual clock; starts={}, snap={:?}, unacked={}",
                rig.node_executed_starts(),
                fixtures::session_runtime_snapshot(&rig.store, SESSION),
                rig.node.unacked(),
            )
        }),
        || {
            rig.node_executed_starts() == 2
                && node_checks::attempt_start_committed(
                    &rig.store,
                    rig.latest_execution().attempt().as_str(),
                )
                .unwrap()
        },
    )
    .await;
    let second = rig.latest_execution();
    rig.emit_reply_and_finish(&second).await;
    eventually_sync(
        on_timeout(|| async {
            format!(
                "retried turn completed; ends={:?}, unacked={}, snap={:?}",
                rig.ends(),
                rig.node.unacked(),
                fixtures::session_runtime_snapshot(&rig.store, SESSION),
            )
        }),
        || rig.ends().len() == 1 && rig.node.unacked() == 0,
    )
    .await;
    assert_eq!(rig.ends(), [(SESSION.into(), "end_turn".into(), None)]);
    rig.shutdown().await;
}

/// Idle 48h virtual-clock stability: an idle session with no business input creates 0 new
/// turns, 0 model calls, and at most 1 idle release across 48 hours and Hub/Node restarts.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_session_across_48h_virtual_clock_plans_zero_new_turns() {
    let mut rig = MatrixRig::ready().await;
    assert_eq!(
        rig.rt().prompt(SESSION, PROMPT, "user", &[]).unwrap(),
        Planned::Created
    );
    rig.finish_active_turn().await;
    eventually_sync("initial turn settled", || {
        rig.ends().len() == 1 && rig.node.unacked() == 0 && rig.phase() == "ready"
    })
    .await;

    // Advance past the 10-minute idle release window and confirm the virtual clock triggers
    // exactly one idle release without waiting wall-clock time.
    rig.advance(10 * 60_000 + 2_000);
    eventually_sync("idle session released on virtual clock", || {
        rig.phase() == "released"
            && node_checks::command_count_by_method(&rig.store, "release_session").unwrap() == 1
    })
    .await;

    // Walk the remaining 48 hours of virtual time, including 2s wake ticks, a Hub restart at
    // 16h, and a Node restart at 32h.
    for _ in 0..5 {
        rig.advance(2_000);
        tokio::task::yield_now().await;
    }
    for hour in 1..=48 {
        rig.advance(60 * 60_000);
        tokio::task::yield_now().await;
        if hour == 16 {
            rig.restart_hub().await;
        } else if hour == 32 {
            rig.restart_node().await;
        }
    }
    assert_eq!(rig.node_executed_starts(), 1, "0 new starts while idle 48h");
    assert_eq!(
        node_checks::command_count_by_method(&rig.store, "start_turn").unwrap(),
        1
    );
    assert_eq!(
        node_checks::command_count_by_method(&rig.store, "start_job").unwrap(),
        0
    );
    assert_eq!(
        node_checks::command_count_by_method(&rig.store, "release_session").unwrap(),
        1,
        "exactly one idle release across 48h"
    );
    assert_eq!(rig.ends().len(), 1);
    rig.shutdown().await;
}

#[derive(Debug, Default)]
struct FdInventory {
    total: usize,
    by_kind: std::collections::BTreeMap<String, usize>,
    paths: Vec<std::path::PathBuf>,
}

impl FdInventory {
    fn count_under(&self, dir: &std::path::Path) -> usize {
        let canon = std::fs::canonicalize(dir).unwrap_or_else(|_| dir.to_path_buf());
        self.paths
            .iter()
            .filter(|p| p.starts_with(dir) || p.starts_with(&canon))
            .count()
    }

    fn non_vnode(&self) -> usize {
        self.by_kind
            .iter()
            .filter(|(k, _)| !k.starts_with("vnode:") && !k.starts_with("file:"))
            .map(|(_, v)| *v)
            .sum()
    }
}

fn open_fd_inventory() -> FdInventory {
    let mut inv = FdInventory::default();
    #[cfg(target_os = "macos")]
    {
        let pid = std::process::id() as libc::c_int;
        let bytes =
            unsafe { libc::proc_pidinfo(pid, libc::PROC_PIDLISTFDS, 0, std::ptr::null_mut(), 0) };
        if bytes <= 0 {
            return inv;
        }
        let elem = std::mem::size_of::<libc::proc_fdinfo>();
        let cap = (bytes as usize / elem) + 32;
        let mut buf: Vec<libc::proc_fdinfo> = vec![unsafe { std::mem::zeroed() }; cap];
        let got = unsafe {
            libc::proc_pidinfo(
                pid,
                libc::PROC_PIDLISTFDS,
                0,
                buf.as_mut_ptr().cast(),
                (buf.len() * elem) as libc::c_int,
            )
        };
        if got <= 0 {
            return inv;
        }
        let count = got as usize / elem;
        inv.total = count;
        for info in &buf[..count] {
            let kind = match info.proc_fdtype as libc::c_int {
                libc::PROX_FDTYPE_VNODE => {
                    let mut path = [0u8; libc::MAXPATHLEN as usize];
                    let ok =
                        unsafe { libc::fcntl(info.proc_fd, libc::F_GETPATH, path.as_mut_ptr()) }
                            == 0;
                    if ok {
                        let end = path.iter().position(|b| *b == 0).unwrap_or(path.len());
                        let s = String::from_utf8_lossy(&path[..end]);
                        inv.paths.push(std::path::PathBuf::from(s.as_ref()));
                        if s.ends_with(".sqlite3") {
                            "vnode:sqlite3".into()
                        } else if s.ends_with("-wal") {
                            "vnode:sqlite3-wal".into()
                        } else if s.ends_with("-shm") {
                            "vnode:sqlite3-shm".into()
                        } else {
                            "vnode:other".into()
                        }
                    } else {
                        "vnode:unknown".into()
                    }
                }
                libc::PROX_FDTYPE_SOCKET => "socket".into(),
                libc::PROX_FDTYPE_PIPE => "pipe".into(),
                libc::PROX_FDTYPE_KQUEUE => "kqueue".into(),
                other => format!("type:{other}"),
            };
            *inv.by_kind.entry(kind).or_insert(0) += 1;
        }
    }
    #[cfg(target_os = "linux")]
    {
        if let Ok(entries) = std::fs::read_dir("/proc/self/fd") {
            for entry in entries.flatten() {
                inv.total += 1;
                let kind = match std::fs::read_link(entry.path()) {
                    Ok(target) => {
                        let s = target.to_string_lossy();
                        if s.starts_with("socket:") {
                            "socket".into()
                        } else if s.starts_with("pipe:") {
                            "pipe".into()
                        } else if s.starts_with("anon_inode:") {
                            s.into_owned()
                        } else {
                            inv.paths.push(target.clone());
                            if s.ends_with(".sqlite3") {
                                "file:sqlite3".into()
                            } else if s.ends_with("-wal") {
                                "file:sqlite3-wal".into()
                            } else if s.ends_with("-shm") {
                                "file:sqlite3-shm".into()
                            } else {
                                "file:other".into()
                            }
                        }
                    }
                    Err(_) => "unreadable".into(),
                };
                *inv.by_kind.entry(kind).or_insert(0) += 1;
            }
        }
    }
    inv
}

/// Contract: repeated Hub and Node restarts must keep open file descriptors bounded (no per-restart
/// leak of SQLite handles, runtimes, or event-loop fds), and shutting down a rig must release all
/// of its store descriptors immediately.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn repeated_restarts_do_not_leak_file_descriptors() {
    const RESTART_ROUNDS: usize = 30;
    /// Lazy reader pool (1 writer + 1 warm reader, plus at most a few concurrent burst readers)
    /// must stay well below the old eager 16-reader footprint (35 fds per Store).
    const MAX_RIG_STORE_FDS: usize = 12;

    let mut rig = MatrixRig::ready().await;
    // Canonicalize before `rig.shutdown()` deletes the TempDir so macOS `/var` -> `/private/var`
    // paths returned by `F_GETPATH` still match even after the directory is removed.
    let rig_dir =
        std::fs::canonicalize(rig._dir.path()).unwrap_or_else(|_| rig._dir.path().to_path_buf());
    let after_ready = open_fd_inventory();
    let ready_store_fds = after_ready.count_under(&rig_dir);
    assert!(
        ready_store_fds > 0 && ready_store_fds <= MAX_RIG_STORE_FDS,
        "expected lazy Store reader pool to stay <= {MAX_RIG_STORE_FDS} fds, got {ready_store_fds}: {after_ready:?}"
    );

    for i in 1..=RESTART_ROUNDS {
        if i % 2 == 0 {
            let entered = Arc::new(tokio::sync::Notify::new());
            let release = Arc::new(tokio::sync::Notify::new());
            rig.proxy
                .intercept_start(Some(StartIntercept::BlockBeforeNode {
                    entered: entered.clone(),
                    release: release.clone(),
                }));
            let _ = rig.rt().prompt(SESSION, "active restart", "user", &[]);
            tokio::select! {
                _ = entered.notified() => {}
                _ = tokio::time::sleep(std::time::Duration::from_millis(50)) => {}
            }
        }
        rig.restart_hub().await;
    }
    for _ in 1..=RESTART_ROUNDS {
        rig.restart_node().await;
    }

    let after_restarts = open_fd_inventory();
    let restarted_store_fds = after_restarts.count_under(&rig_dir);
    assert!(
        restarted_store_fds <= MAX_RIG_STORE_FDS,
        "store fds grew past bound after {RESTART_ROUNDS} Hub + {RESTART_ROUNDS} Node restarts: ready={ready_store_fds}, after={restarted_store_fds}, inv={after_restarts:?}"
    );
    // Even if sibling tests in the same cargo-test binary briefly open runtimes, a per-restart
    // kqueue/pipe/socket leak across 2 * RESTART_ROUNDS restarts would exceed this ceiling.
    assert!(
        after_restarts.non_vnode() <= after_ready.non_vnode() + RESTART_ROUNDS,
        "non-file fds grew unexpectedly across restarts: ready={after_ready:?}, after={after_restarts:?}"
    );

    rig.shutdown().await;
    eventually_sync(
        on_timeout(|| async {
            format!(
                "all Store fds close after MatrixRig::shutdown(); inv={:?}",
                open_fd_inventory()
            )
        }),
        || open_fd_inventory().count_under(&rig_dir) == 0,
    )
    .await;
}
