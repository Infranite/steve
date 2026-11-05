//! The Hub's handle on a remote Node: every port, over whatever link the Node has right now.
//!
//! The coordinator gets an `Arc<RemoteNode>` exactly as it gets a `LocalNode`. Without a link every
//! call is `offline` (nothing was sent: the same command may be sent again). After a Node restart
//! (a new incarnation) the slot is `reconciling` until the Hub caught up: new work is refused
//! `offline` then, while inspection, replay, acks, cancel and abandon go through.

use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde::Serialize;
use serde::de::DeserializeOwned;
use steve_node_api::*;
use tokio::sync::watch;

use crate::conn::{Conn, from_value, to_value};
use crate::methods::{self as m, *};

struct Link {
    conn: Arc<Conn>,
}

#[derive(Default)]
struct Slot {
    link: Option<Link>,
    /// The Node restarted and the Hub has not caught up yet. A property of the Node, not of one
    /// link: it survives a reconnect until a catch-up on the current link clears it.
    reconciling: bool,
}

pub struct RemoteNode {
    id: NodeId,
    slot: Mutex<Slot>,
    connected: watch::Sender<Option<u64>>,
}

impl RemoteNode {
    pub fn new(id: NodeId) -> Arc<RemoteNode> {
        Arc::new(RemoteNode {
            id,
            slot: Mutex::new(Slot::default()),
            connected: watch::channel(None).0,
        })
    }

    /// Install the link of a newly accepted generation (the registry already fenced the old one).
    /// An older generation than the one held is refused (`false`): two links' attaches may run in
    /// either order, and the fenced one must not take the slot back.
    pub fn attach(&self, conn: Arc<Conn>, restarted: bool) -> bool {
        let generation = conn.generation();
        let mut slot = self.slot.lock().unwrap();
        if slot
            .link
            .as_ref()
            .is_some_and(|l| l.conn.generation() > generation)
        {
            return false;
        }
        slot.reconciling |= restarted;
        let old = slot.link.replace(Link { conn });
        drop(slot);
        drop(old);
        self.connected.send_replace(Some(generation));
        true
    }

    /// Forget the link of `generation` (a newer one stays).
    pub fn detach(&self, generation: u64) {
        let mut slot = self.slot.lock().unwrap();
        if slot.link.as_ref().map(|l| l.conn.generation()) == Some(generation) {
            let old = slot.link.take();
            drop(slot);
            drop(old);
            self.connected.send_replace(None);
        }
    }

    /// The Hub caught up over the link of `generation`: new work may go again. A catch-up of a
    /// link that is no longer the one held clears nothing.
    pub fn set_ready(&self, generation: u64) -> bool {
        let mut slot = self.slot.lock().unwrap();
        let current = slot
            .link
            .as_ref()
            .is_some_and(|l| l.conn.generation() == generation && !l.conn.is_closed());
        if current {
            slot.reconciling = false;
        }
        current
    }

    pub fn is_reconciling(&self) -> bool {
        self.slot.lock().unwrap().reconciling
    }

    /// The generation of the live link, if any.
    pub fn connected(&self) -> watch::Receiver<Option<u64>> {
        self.connected.subscribe()
    }

    pub fn generation(&self) -> Option<u64> {
        *self.connected.borrow()
    }

    fn conn(&self, new_work: bool) -> ApiResult<Arc<Conn>> {
        let slot = self.slot.lock().unwrap();
        match slot.link.as_ref() {
            Some(l) if l.conn.is_closed() => Err(ApiError::new(ErrorCode::Offline, "node offline")),
            Some(_) if new_work && slot.reconciling => Err(ApiError::new(
                ErrorCode::Offline,
                "node restarted, reconciling first",
            )),
            Some(l) => Ok(l.conn.clone()),
            None => Err(ApiError::new(ErrorCode::Offline, "node offline")),
        }
    }

    async fn go<A: Serialize, R: DeserializeOwned>(
        &self,
        method: &str,
        args: &A,
        op: Option<&OperationId>,
        new_work: bool,
    ) -> ApiResult<R> {
        let conn = self.conn(new_work)?;
        let v = conn.call(method, to_value(args)?, op).await?;
        from_value(v)
    }

    async fn cmd<T: Serialize + Send + Sync>(
        &self,
        method: &str,
        c: &Command<T>,
        new_work: bool,
    ) -> ApiResult<Receipt> {
        self.go(method, c, Some(&c.id), new_work).await
    }
}

#[async_trait]
impl SessionExecutor for RemoteNode {
    async fn open_session(&self, c: Command<OpenSession>) -> ApiResult<Receipt> {
        self.cmd(m::SESSION_OPEN, &c, true).await
    }
    async fn configure_session(&self, c: Command<ConfigureSession>) -> ApiResult<Receipt> {
        self.cmd(SESSION_CONFIGURE, &c, false).await
    }
    async fn start_turn(&self, c: Command<TurnSpec>) -> ApiResult<Receipt> {
        self.cmd(TURN_START, &c, true).await
    }
    async fn session(&self, id: SessionId) -> ApiResult<SessionView> {
        self.go(SESSION_INSPECT, &id, None, false).await
    }
    async fn release_session(&self, c: Command<SessionId>) -> ApiResult<Receipt> {
        self.cmd(SESSION_RELEASE, &c, false).await
    }
}

#[async_trait]
impl JobExecutor for RemoteNode {
    /// N2: a remote Node runs no jobs (their sandboxing is local-only so far). Refused here,
    /// nothing sent, so the caller may route it elsewhere.
    async fn start_job(&self, _c: Command<JobSpec>) -> ApiResult<Receipt> {
        Err(ApiError::new(
            ErrorCode::Unsupported,
            "jobs do not run on a remote node yet",
        ))
    }
}

#[async_trait]
impl ExecutionControl for RemoteNode {
    async fn operation(&self, id: OperationId) -> ApiResult<Receipt> {
        self.go(OP_INSPECT, &id, None, false).await
    }
    async fn cancel(&self, c: Command<Cancel>) -> ApiResult<Receipt> {
        self.cmd(EXEC_CANCEL, &c, false).await
    }
    async fn abandon(&self, c: Command<Abandon>) -> ApiResult<Receipt> {
        self.cmd(EXEC_ABANDON, &c, false).await
    }
    async fn recover(&self, c: Command<Recover>) -> ApiResult<Receipt> {
        self.cmd(EXEC_RECOVER, &c, false).await
    }
    async fn resolve_permission(&self, c: Command<PermissionResolution>) -> ApiResult<Receipt> {
        self.cmd(PERMISSION_RESOLVE, &c, false).await
    }
    async fn snapshot(&self, page: PageRequest) -> ApiResult<Snapshot> {
        self.go(NODE_SNAPSHOT, &page, None, false).await
    }
    async fn streams(&self) -> ApiResult<Vec<StreamHead>> {
        self.go(NODE_STREAMS, &(), None, false).await
    }
    async fn replay(&self, from: Watermark, max_bytes: u32) -> ApiResult<EventBatch> {
        self.go(EVENTS_REPLAY, &ReplayArgs { from, max_bytes }, None, false)
            .await
    }
    async fn acknowledge(&self, ack: EventAck) -> ApiResult<()> {
        self.go(EVENTS_ACK, &ack, None, false).await
    }
}

#[async_trait]
impl WorkspaceExecutor for RemoteNode {
    async fn prepare_workspace(&self, c: Command<Prepare>) -> ApiResult<Receipt> {
        self.cmd(WORKSPACE_PREPARE, &c, false).await
    }
    async fn workspace(&self, w: WorkspaceRef) -> ApiResult<WorkspaceView> {
        self.go(WORKSPACE_INSPECT, &w, None, false).await
    }
    async fn tidy_workspace(&self, c: Command<Tidy>) -> ApiResult<Receipt> {
        self.cmd(WORKSPACE_TIDY, &c, false).await
    }
    async fn git_facts(&self, q: GitQuery) -> ApiResult<GitFacts> {
        self.go(GIT_INSPECT, &q, None, false).await
    }
    async fn git_apply(&self, c: Command<GitApply>) -> ApiResult<Receipt> {
        self.cmd(GIT_APPLY, &c, false).await
    }
    /// N2: no execs on a remote Node either (see `start_job`); nothing is sent.
    async fn exec(&self, _c: Command<ExecSpec>) -> ApiResult<Receipt> {
        Err(ApiError::new(
            ErrorCode::Unsupported,
            "execs do not run on a remote node yet",
        ))
    }
    async fn list_dir(&self, path: PathRef, page: PageRequest) -> ApiResult<Page<FileInfo>> {
        self.go(FS_LIST, &ListDirArgs { path, page }, None, false)
            .await
    }
    async fn list_root(&self, at: RootPath, page: PageRequest) -> ApiResult<Page<FileInfo>> {
        self.go(FS_LIST_ROOT, &ListRootArgs { at, page }, None, false)
            .await
    }
    async fn read_file(&self, q: FileRead) -> ApiResult<ReadChunk> {
        self.go(FS_READ, &q, None, false).await
    }
    async fn scan(&self, c: Command<ScanSpec>) -> ApiResult<Receipt> {
        self.cmd(TREE_SCAN, &c, false).await
    }
    async fn scan_page(&self, scan: OperationId, page: PageRequest) -> ApiResult<ScanPage> {
        let a = PagesArgs {
            operation: scan,
            page,
        };
        self.go(SCAN_PAGE, &a, None, false).await
    }
}

#[async_trait]
impl Discovery for RemoteNode {
    /// A remote Node runs no jobs and no execs in N2 (their sandboxing is local-only so far): its
    /// capacity for them is reported as zero whatever it says.
    async fn probe(&self, fresh: bool) -> ApiResult<Capabilities> {
        let mut c: Capabilities = self
            .go(HARNESS_PROBE, &ProbeArgs { fresh }, None, false)
            .await?;
        c.capacity.jobs = 0;
        c.capacity.execs = 0;
        Ok(c)
    }
    async fn history(&self, page: PageRequest) -> ApiResult<Page<HistoryEntry>> {
        self.go(HISTORY_LIST, &page, None, false).await
    }
    async fn import_history(&self, c: Command<HistoryImport>) -> ApiResult<Receipt> {
        self.cmd(HISTORY_IMPORT, &c, false).await
    }
    async fn import_page(&self, import: OperationId, page: PageRequest) -> ApiResult<ImportedPage> {
        let a = PagesArgs {
            operation: import,
            page,
        };
        self.go(IMPORT_PAGE, &a, None, false).await
    }
}

impl Node for RemoteNode {
    fn id(&self) -> &NodeId {
        &self.id
    }
}
