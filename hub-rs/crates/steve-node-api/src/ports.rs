//! The ports. Hub → Node: one trait per face, so a Hub module gets only the face it needs (the
//! knowledge sync gets [`WorkspaceExecutor`], never [`SessionExecutor`]). Node → Hub: [`EventSink`],
//! [`HubTools`], [`HubFiles`], [`HubSecrets`], each bound by the wiring to one authenticated Node.
//!
//! Every argument is owned and serializable, every method async. An instance is bound to one Node: a
//! port never routes by what is inside its arguments. Commands are idempotent by their operation id.
//!
//! Dropping a returned future: before its first poll nothing was sent; after the receipt came back it
//! only stops waiting; in between the outcome is unknown, so ask ([`ExecutionControl::operation`])
//! instead of guessing. An implementation never ties the work it starts to the caller's future.

use async_trait::async_trait;

use crate::blob::*;
use crate::common::*;
use crate::discovery::*;
use crate::events::*;
use crate::ids::*;
use crate::run::*;
use crate::tools::*;
use crate::workspace::*;

#[async_trait]
pub trait SessionExecutor: Send + Sync {
    /// Start (or restart after a release) the harness context. Finished with
    /// [`OperationResult::Session`].
    async fn open_session(&self, c: Command<OpenSession>) -> ApiResult<Receipt>;
    async fn configure_session(&self, c: Command<ConfigureSession>) -> ApiResult<Receipt>;
    /// Run one attempt of a turn. `busy` while another attempt of the session runs; the same attempt
    /// under another operation id is a `conflict`, never a second run.
    async fn start_turn(&self, c: Command<TurnSpec>) -> ApiResult<Receipt>;
    async fn session(&self, id: SessionId) -> ApiResult<SessionView>;
    /// Stop an idle harness process; the Steve session goes on (open it again to continue).
    async fn release_session(&self, c: Command<SessionId>) -> ApiResult<Receipt>;
}

#[async_trait]
pub trait JobExecutor: Send + Sync {
    /// Finished with [`OperationResult::Execution`].
    async fn start_job(&self, c: Command<JobSpec>) -> ApiResult<Receipt>;
}

/// Lifecycle of anything a Node accepted, by its operation id. The Hub's control plane only.
#[async_trait]
pub trait ExecutionControl: Send + Sync {
    async fn operation(&self, id: OperationId) -> ApiResult<Receipt>;
    /// An intent: the answer says it was taken; the end comes as the operation's own final state
    /// (which may be a normal finish that came first).
    async fn cancel(&self, c: Command<Cancel>) -> ApiResult<Receipt>;
    /// Give up an `unknown` execution once its processes are confirmed gone ([`Abandon`]).
    async fn abandon(&self, c: Command<Abandon>) -> ApiResult<Receipt>;
    /// Explicitly retire an unknown control operation; never implicitly re-executes it.
    /// Default preserves compatibility with Nodes that do not implement control recovery.
    async fn recover(&self, _c: Command<Recover>) -> ApiResult<Receipt> {
        Err(ApiError::new(
            ErrorCode::Unsupported,
            "control recovery unsupported",
        ))
    }
    /// Applies only to the request still waiting with this attempt and action digest; anything else
    /// is `stale`.
    async fn resolve_permission(&self, c: Command<PermissionResolution>) -> ApiResult<Receipt>;
    /// Every accepted operation with its state, and the stream heads, as of one moment: a page with
    /// no cursor takes a new snapshot, the following pages continue that same one.
    async fn snapshot(&self, page: PageRequest) -> ApiResult<Snapshot>;
    /// Where every stream stands.
    async fn streams(&self) -> ApiResult<Vec<StreamHead>>;
    /// Events after `from` (exclusive), at most `max_bytes` encoded; `gap` past retention.
    async fn replay(&self, from: Watermark, max_bytes: u32) -> ApiResult<EventBatch>;
    async fn acknowledge(&self, ack: EventAck) -> ApiResult<()>;
}

#[async_trait]
pub trait WorkspaceExecutor: Send + Sync {
    /// Finished with [`OperationResult::Workspace`]. Same key, same workspace.
    async fn prepare_workspace(&self, c: Command<Prepare>) -> ApiResult<Receipt>;
    async fn workspace(&self, w: WorkspaceRef) -> ApiResult<WorkspaceView>;
    /// Refused while an execution uses it or when it changed since `expected`.
    async fn tidy_workspace(&self, c: Command<Tidy>) -> ApiResult<Receipt>;
    async fn git_facts(&self, q: GitQuery) -> ApiResult<GitFacts>;
    /// Applied only if HEAD still is `expected_head`; a push only over `expected_remote`.
    async fn git_apply(&self, c: Command<GitApply>) -> ApiResult<Receipt>;
    async fn exec(&self, c: Command<ExecSpec>) -> ApiResult<Receipt>;
    async fn list_dir(&self, path: PathRef, page: PageRequest) -> ApiResult<Page<FileInfo>>;
    /// Browse an allowed root (choosing a project directory).
    async fn list_root(&self, at: RootPath, page: PageRequest) -> ApiResult<Page<FileInfo>>;
    async fn read_file(&self, q: FileRead) -> ApiResult<ReadChunk>;
    /// Strict, execution-scoped artifact read. Unsupported peers must never fall back to a
    /// general filesystem read (which can follow links and does not bind an execution view).
    async fn read_artifact(&self, _q: ArtifactRead) -> ApiResult<ReadChunk> {
        Err(ApiError::new(
            ErrorCode::Unsupported,
            "execution-scoped artifact reads unsupported",
        ))
    }
    /// Scan a tree; read its pages under the scan's operation id.
    async fn scan(&self, c: Command<ScanSpec>) -> ApiResult<Receipt>;
    async fn scan_page(&self, scan: OperationId, page: PageRequest) -> ApiResult<ScanPage>;
}

#[async_trait]
pub trait Discovery: Send + Sync {
    /// Idempotently replace the active-turn capacity (1–256). Running turns are never interrupted;
    /// lowering it defers new starts until enough finish. The Hub persists the setting and reapplies
    /// it before dispatching turns, including after reconnect/restart. Workspace-only Nodes refuse.
    async fn set_session_capacity(&self, _sessions: u32) -> ApiResult<()> {
        Err(ApiError::new(
            ErrorCode::Unsupported,
            "live session capacity unsupported",
        ))
    }
    async fn probe(&self, fresh: bool) -> ApiResult<Capabilities>;
    async fn history(&self, page: PageRequest) -> ApiResult<Page<HistoryEntry>>;
    async fn import_history(&self, c: Command<HistoryImport>) -> ApiResult<Receipt>;
    async fn import_page(&self, import: OperationId, page: PageRequest) -> ApiResult<ImportedPage>;
    /// Where an entry of [`Discovery::history`] (or `session:<id>`, a harness session) is. A key
    /// the Node did not list is `invalid`.
    async fn locate(&self, source: HistoryRef) -> ApiResult<HistoryLocation>;
    /// How the machine is doing now (B3); a Node that cannot say answers `unsupported`.
    async fn vitals(&self) -> ApiResult<NodeVitals> {
        Err(ApiError::new(ErrorCode::Unsupported, "vitals"))
    }
}

/// All faces of one Node.
pub trait Node:
    SessionExecutor + JobExecutor + ExecutionControl + WorkspaceExecutor + Discovery
{
    fn id(&self) -> &NodeId;
}

// ── Node → Hub ──

/// Where a Node's facts go. Returns once the Hub durably committed them (events, projections and what
/// follows from them in one transaction), with the watermarks to acknowledge.
#[async_trait]
pub trait EventSink: Send + Sync {
    async fn commit(&self, batch: EventBatch) -> ApiResult<EventAck>;
}

#[async_trait]
pub trait HubTools: Send + Sync {
    async fn catalog(&self, execution: ExecutionRef) -> ApiResult<Catalog>;
    async fn call(&self, q: ToolCall) -> ApiResult<ToolResult>;
    async fn call_status(&self, key: ToolKey) -> ApiResult<ToolResult>;
    async fn cancel_call(&self, key: ToolKey) -> ApiResult<ToolResult>;
}

#[async_trait]
pub trait HubFiles: Send + Sync {
    async fn fetch(
        &self,
        access: FileAccess,
        id: ArtifactId,
        offset: u64,
        max: u32,
    ) -> ApiResult<ReadChunk>;
    async fn begin_upload(&self, b: BeginUpload) -> ApiResult<Transfer>;
    /// Checks the offset and the chunk's digest.
    async fn upload(&self, chunk: BlobChunk) -> ApiResult<Transfer>;
    async fn upload_status(&self, transfer: TransferId) -> ApiResult<TransferState>;
    /// Checks size and hash, then keeps it; finishing again answers the same artifact.
    async fn finish_upload(&self, transfer: TransferId) -> ApiResult<ArtifactRef>;
    async fn abort_upload(&self, transfer: TransferId) -> ApiResult<TransferState>;
}

/// Secrets for one operation (a push token, a vault value for a command). The Node takes each
/// [`SecretRef`] of a command it accepted, under that command's operation id, right before using
/// it. One-shot and short-lived: a second take, or one after the operation ended, is `stale`; a
/// secret given to another operation (or to another Node) is `denied`. The bytes are never
/// journaled, logged, put in an event or a result, on either side.
#[async_trait]
pub trait HubSecrets: Send + Sync {
    async fn take(&self, operation: OperationId, id: String) -> ApiResult<SecretBytes>;
}
