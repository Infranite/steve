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

use crate::conn::{Conn, answer_of, to_value};
use crate::methods::{self as m, *};

struct Link {
    conn: Arc<Conn>,
    since: std::time::Instant,
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
    /// The Hub's own Node in a process of its own (B1): on this machine, so what it says it can do
    /// (jobs, secrets written to a file) stands.
    colocated: std::sync::atomic::AtomicBool,
}

impl RemoteNode {
    /// The Hub's own Node, linked from a process of its own on this machine (B1).
    pub fn set_colocated(&self, on: bool) {
        self.colocated
            .store(on, std::sync::atomic::Ordering::Relaxed);
    }

    fn is_colocated(&self) -> bool {
        self.colocated.load(std::sync::atomic::Ordering::Relaxed)
    }

    pub fn new(id: NodeId) -> Arc<RemoteNode> {
        Arc::new(RemoteNode {
            id,
            slot: Mutex::new(Slot::default()),
            connected: watch::channel(None).0,
            colocated: std::sync::atomic::AtomicBool::new(false),
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
        let old = slot.link.replace(Link {
            conn,
            since: std::time::Instant::now(),
        });
        drop(slot);
        if let Some(old) = old {
            self.link_ended(old);
        }
        let node = self.id.to_string();
        metrics::counter!("steve_node_link_connects_total", "node" => node.clone()).increment(1);
        metrics::gauge!("steve_node_link_up", "node" => node).set(1.0);
        self.connected.send_replace(Some(generation));
        true
    }

    /// The one place a link's end is measured: lost, or replaced by a newer one.
    fn link_ended(&self, old: Link) {
        let node = self.id.to_string();
        metrics::counter!("steve_node_link_disconnects_total", "node" => node.clone()).increment(1);
        metrics::histogram!("steve_node_link_uptime_seconds", "node" => node)
            .record(old.since.elapsed().as_secs_f64());
    }

    /// Forget the link of `generation` (a newer one stays).
    pub fn detach(&self, generation: u64) {
        let mut slot = self.slot.lock().unwrap();
        if slot.link.as_ref().map(|l| l.conn.generation()) == Some(generation) {
            let old = slot.link.take();
            drop(slot);
            if let Some(old) = old {
                self.link_ended(old);
            }
            metrics::gauge!("steve_node_link_up", "node" => self.id.to_string()).set(0.0);
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

    /// A live generation that can take new work, not a closed or reconciling socket.
    pub fn ready_generation(&self) -> Option<u64> {
        self.conn(true).ok().map(|c| c.generation())
    }

    fn live_conn(&self) -> ApiResult<Arc<Conn>> {
        self.slot
            .lock()
            .unwrap()
            .link
            .as_ref()
            .filter(|l| !l.conn.is_closed())
            .map(|l| l.conn.clone())
            .ok_or_else(|| Error::later(Code::Unavailable, "node offline"))
    }

    fn conn(&self, new_work: bool) -> ApiResult<Arc<Conn>> {
        let slot = self.slot.lock().unwrap();
        match slot.link.as_ref() {
            Some(l) if l.conn.is_closed() => Err(Error::later(Code::Unavailable, "node offline")),
            Some(l) if l.conn.is_upgrade() => Err(Error::later(
                Code::Unavailable,
                "ordinary protocol unavailable while upgrading",
            )),
            Some(_) if new_work && slot.reconciling => Err(Error::later(
                Code::Unavailable,
                "node restarted, reconciling first",
            )),
            Some(l) => Ok(l.conn.clone()),
            None => Err(Error::later(Code::Unavailable, "node offline")),
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
        answer_of(method, v, op)
    }

    async fn cmd<T: Serialize + Send + Sync>(
        &self,
        method: &str,
        c: &Command<T>,
        new_work: bool,
    ) -> ApiResult<Receipt> {
        self.go(method, c, Some(&c.id), new_work).await
    }

    async fn upgrade_go<A: Serialize, R: DeserializeOwned>(
        &self,
        method: &str,
        args: A,
        operation: Option<&OperationId>,
    ) -> ApiResult<R> {
        let value = self
            .live_conn()?
            .call_upgrade(method, to_value(&args)?, operation)
            .await?;
        answer_of(method, value, operation)
    }

    pub fn is_upgrade(&self) -> bool {
        self.slot
            .lock()
            .unwrap()
            .link
            .as_ref()
            .is_some_and(|l| l.conn.is_upgrade())
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
    async fn start_job(&self, c: Command<JobSpec>) -> ApiResult<Receipt> {
        if !self.is_colocated() {
            return Err(Error::new(
                Code::Unsupported,
                "jobs do not run on a remote node yet",
            ));
        }
        self.cmd(JOB_START, &c, true).await
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
        // A push to another branch is the merge queue's, on the Hub's own Node for now.
        if let GitAction::Push {
            remote_branch: Some(_),
            ..
        } = &c.body.action
        {
            return Err(Error::new(
                Code::Unsupported,
                "a push to another branch is not on a remote node yet",
            ));
        }
        self.cmd(GIT_APPLY, &c, false).await
    }
    /// N2: no execs on a remote Node either (see `start_job`); nothing is sent.
    /// A command (a flow step, a vault use, a push) on the Node: it runs only when the Node's own
    /// policy lets commands run there (`policy.json` `exec`, B4), the Node refuses it otherwise.
    async fn exec(&self, c: Command<ExecSpec>) -> ApiResult<Receipt> {
        self.cmd(EXEC_START, &c, true).await
    }
    async fn list_dir(&self, path: PathRef, page: PageRequest) -> ApiResult<Page<FileInfo>> {
        self.go(FS_LIST, &ListDirArgs { path, page }, None, false)
            .await
    }
    async fn list_root(&self, at: RootPath, page: PageRequest) -> ApiResult<Page<FileInfo>> {
        self.go(FS_LIST_ROOT, &ListRootArgs { at, page }, None, false)
            .await
    }
    async fn read_file(&self, mut q: FileRead) -> ApiResult<ReadChunk> {
        // hex doubles the bytes: keep a chunk's frame within a bulk frame
        q.max = q.max.min(crate::envelope::CHUNK_BYTES);
        self.go(FS_READ, &q, None, false).await
    }
    async fn read_artifact(&self, mut q: ArtifactRead) -> ApiResult<ReadChunk> {
        q.max = q.max.min(crate::envelope::CHUNK_BYTES);
        self.go(ARTIFACT_READ, &q, None, false).await
    }
    async fn read_execution_delivery(
        &self,
        mut q: ExecutionDeliveryRead,
    ) -> ApiResult<DeliveryChunk> {
        q.max = q.max.min(crate::envelope::CHUNK_BYTES);
        self.go(EXECUTION_DELIVERY_READ, &q, None, false).await
    }
    async fn read_delivery(&self, mut q: FileRead) -> ApiResult<DeliveryChunk> {
        q.max = q.max.min(crate::envelope::CHUNK_BYTES);
        self.go(DELIVERY_READ, &q, None, false).await
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
    async fn set_session_capacity(&self, sessions: u32) -> ApiResult<()> {
        self.go(NODE_SESSION_CAPACITY, &sessions, None, false).await
    }
    async fn probe_build(&self) -> ApiResult<upgrade_v1::ProbeBuildResponse> {
        self.upgrade_go(
            upgrade_v1::PROBE_BUILD,
            upgrade_v1::ProbeBuildRequest {},
            None,
        )
        .await
    }
    async fn upgrade_stage(&self, u: NodeUpgrade) -> ApiResult<NodeUpgradeState> {
        let op = u.operation.clone();
        let state: upgrade_v1::StageResponse = self
            .upgrade_go(
                upgrade_v1::UPGRADE_STAGE,
                upgrade_v1::StageRequest::from(u),
                Some(&op),
            )
            .await?;
        state.try_into().map_err(Error::from)
    }
    async fn upgrade_switch(&self, s: UpgradeSwitch) -> ApiResult<NodeUpgradeState> {
        let op = s.operation.clone();
        let state: upgrade_v1::SwitchResponse = self
            .upgrade_go(
                upgrade_v1::UPGRADE_SWITCH,
                upgrade_v1::SwitchRequest::from(s),
                Some(&op),
            )
            .await?;
        state.try_into().map_err(Error::from)
    }
    async fn upgrade_status(&self, operation: OperationId) -> ApiResult<NodeUpgradeState> {
        let state: upgrade_v1::StatusResponse = self
            .upgrade_go(
                upgrade_v1::UPGRADE_STATUS,
                upgrade_v1::StatusRequest {
                    operation: operation.to_string(),
                },
                None,
            )
            .await?;
        state.try_into().map_err(Error::from)
    }
    async fn remove_begin(&self, r: NodeRemove) -> ApiResult<NodeRemoveState> {
        let op = r.operation.clone();
        self.go(NODE_REMOVE, &r, Some(&op), false).await
    }
    async fn remove_commit(&self, c: RemoveCommit) -> ApiResult<NodeRemoveState> {
        let op = c.operation.clone();
        self.go(NODE_REMOVE_COMMIT, &c, Some(&op), false).await
    }
    async fn remove_status(&self, operation: OperationId) -> ApiResult<NodeRemoveState> {
        self.go(NODE_REMOVE_STATUS, &operation, None, false).await
    }
    async fn install_harness(&self, c: Command<InstallHarness>) -> ApiResult<Receipt> {
        if !self.is_colocated() {
            return Err(Error::new(
                Code::Forbidden,
                "install harnesses on a separate Node with its operator CLI",
            ));
        }
        self.cmd(HARNESS_INSTALL, &c, false).await
    }
    async fn harness_updates(&self, fresh: bool) -> ApiResult<HarnessUpdateReport> {
        self.go(HARNESS_UPDATES, &ProbeArgs { fresh }, None, false)
            .await
    }
    async fn upgrade_harness(&self, c: Command<UpgradeHarness>) -> ApiResult<Receipt> {
        // Separate Nodes use exactly the same owner-authorized command as local.
        self.cmd(HARNESS_UPGRADE, &c, true).await
    }
    /// A remote Node runs no jobs and no execs in N2 (their sandboxing is local-only so far): its
    /// capacity for them is reported as zero whatever it says.
    async fn probe(&self, fresh: bool) -> ApiResult<Capabilities> {
        let mut c: Capabilities = self
            .go(HARNESS_PROBE, &ProbeArgs { fresh }, None, false)
            .await?;
        if !self.is_colocated() {
            // jobs stay with the Hub; commands as the Node's own policy allows (B4)
            c.capacity.jobs = 0;
            // secrets are never written to a remote Node's disk
            c.features.retain(|f| f != FEATURE_SECRET_FILE);
        }
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
    async fn locate(&self, source: HistoryRef) -> ApiResult<HistoryLocation> {
        self.go(HISTORY_LOCATE, &source, None, false).await
    }
    async fn vitals(&self) -> ApiResult<NodeVitals> {
        self.go(NODE_VITALS, &serde_json::json!({}), None, false)
            .await
    }
    async fn host(&self) -> ApiResult<HostInfo> {
        self.go(NODE_HOST, &serde_json::json!({}), None, false)
            .await
    }
}

impl Node for RemoteNode {
    fn id(&self) -> &NodeId {
        &self.id
    }
}
