// Deterministic held-snapshot and persistent-disconnect barriers from the review probes.

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use steve_node_api::testkit::FakeNode;
use steve_node_api::*;

use async_trait::async_trait;
pub struct Delayed {
    pub base: Arc<FakeNode>,
    once: AtomicBool,
    disconnected: std::sync::Mutex<Option<ErrorCode>>,
    pub entered: tokio::sync::Notify,
    pub gate: tokio::sync::Semaphore,
}
impl Delayed {
    pub fn new(base: Arc<FakeNode>) -> Arc<Self> {
        Arc::new(Self {
            base,
            once: AtomicBool::new(true),
            disconnected: std::sync::Mutex::new(None),
            entered: tokio::sync::Notify::new(),
            gate: tokio::sync::Semaphore::new(0),
        })
    }
    pub fn disconnect(&self, code: ErrorCode) {
        *self.disconnected.lock().unwrap() = Some(code);
    }
    pub fn reconnect(&self) {
        *self.disconnected.lock().unwrap() = None;
    }
    pub fn arm(&self) {
        self.once.store(false, Ordering::SeqCst);
    }
}
impl Node for Delayed {
    fn id(&self) -> &NodeId {
        self.base.id()
    }
}
#[async_trait]
impl SessionExecutor for Delayed {
    async fn open_session(&self, c: Command<OpenSession>) -> ApiResult<Receipt> {
        self.base.open_session(c).await
    }
    async fn configure_session(&self, c: Command<ConfigureSession>) -> ApiResult<Receipt> {
        self.base.configure_session(c).await
    }
    async fn start_turn(&self, c: Command<TurnSpec>) -> ApiResult<Receipt> {
        self.base.start_turn(c).await
    }
    async fn session(&self, id: SessionId) -> ApiResult<SessionView> {
        self.base.session(id).await
    }
    async fn release_session(&self, c: Command<SessionId>) -> ApiResult<Receipt> {
        self.base.release_session(c).await
    }
}
#[async_trait]
impl JobExecutor for Delayed {
    async fn start_job(&self, c: Command<JobSpec>) -> ApiResult<Receipt> {
        self.base.start_job(c).await
    }
}
#[async_trait]
impl ExecutionControl for Delayed {
    async fn operation(&self, id: OperationId) -> ApiResult<Receipt> {
        self.base.operation(id).await
    }
    async fn cancel(&self, c: Command<Cancel>) -> ApiResult<Receipt> {
        self.base.cancel(c).await
    }
    async fn abandon(&self, c: Command<Abandon>) -> ApiResult<Receipt> {
        self.base.abandon(c).await
    }
    async fn resolve_permission(&self, c: Command<PermissionResolution>) -> ApiResult<Receipt> {
        self.base.resolve_permission(c).await
    }
    async fn snapshot(&self, page: PageRequest) -> ApiResult<Snapshot> {
        self.base.snapshot(page).await
    }
    async fn streams(&self) -> ApiResult<Vec<StreamHead>> {
        self.base.streams().await
    }
    async fn replay(&self, from: Watermark, max_bytes: u32) -> ApiResult<EventBatch> {
        self.base.replay(from, max_bytes).await
    }
    async fn acknowledge(&self, ack: EventAck) -> ApiResult<()> {
        self.base.acknowledge(ack).await
    }
}
#[async_trait]
impl WorkspaceExecutor for Delayed {
    async fn prepare_workspace(&self, c: Command<Prepare>) -> ApiResult<Receipt> {
        self.base.prepare_workspace(c).await
    }
    async fn workspace(&self, w: WorkspaceRef) -> ApiResult<WorkspaceView> {
        self.base.workspace(w).await
    }
    async fn tidy_workspace(&self, c: Command<Tidy>) -> ApiResult<Receipt> {
        self.base.tidy_workspace(c).await
    }
    async fn git_facts(&self, q: GitQuery) -> ApiResult<GitFacts> {
        self.base.git_facts(q).await
    }
    async fn git_apply(&self, c: Command<GitApply>) -> ApiResult<Receipt> {
        self.base.git_apply(c).await
    }
    async fn exec(&self, c: Command<ExecSpec>) -> ApiResult<Receipt> {
        self.base.exec(c).await
    }
    async fn list_dir(&self, path: PathRef, page: PageRequest) -> ApiResult<Page<FileInfo>> {
        self.base.list_dir(path, page).await
    }
    async fn list_root(&self, at: RootPath, page: PageRequest) -> ApiResult<Page<FileInfo>> {
        self.base.list_root(at, page).await
    }
    async fn read_file(&self, q: FileRead) -> ApiResult<ReadChunk> {
        self.base.read_file(q).await
    }
    async fn scan(&self, c: Command<ScanSpec>) -> ApiResult<Receipt> {
        self.base.scan(c).await
    }
    async fn scan_page(&self, scan: OperationId, page: PageRequest) -> ApiResult<ScanPage> {
        self.base.scan_page(scan, page).await
    }
}
#[async_trait]
impl Discovery for Delayed {
    async fn history(&self, page: PageRequest) -> ApiResult<Page<HistoryEntry>> {
        self.base.history(page).await
    }
    async fn import_history(&self, c: Command<HistoryImport>) -> ApiResult<Receipt> {
        self.base.import_history(c).await
    }
    async fn import_page(&self, import: OperationId, page: PageRequest) -> ApiResult<ImportedPage> {
        self.base.import_page(import, page).await
    }
    async fn locate(&self, source: HistoryRef) -> ApiResult<HistoryLocation> {
        self.base.locate(source).await
    }

    async fn probe(&self, _fresh: bool) -> ApiResult<Capabilities> {
        if let Some(code) = *self.disconnected.lock().unwrap() {
            return Err(ApiError::new(code, "persistent disconnect"));
        }
        let snapshot = self.base.probe(false).await?;
        if !self.once.swap(true, Ordering::SeqCst) {
            self.entered.notify_one();
            self.gate.acquire().await.unwrap().forget();
        }
        Ok(snapshot)
    }
    async fn install_harness(&self, c: Command<InstallHarness>) -> ApiResult<Receipt> {
        let mut caps = self.base.probe(false).await?;
        for h in &mut caps.harnesses {
            if let Some(s) = &mut h.setup {
                s.state = SetupState::Installing;
                s.can_install = false;
            }
        }
        self.base.set_harnesses(caps.harnesses);
        Ok(Receipt {
            operation: c.id,
            state: Observed::Running,
        })
    }
}
