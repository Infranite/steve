//! Steve's event schema: the facts a Node reports, translated by its drivers from whatever the
//! harness speaks. Each stream (one per session or job) numbers its events without gaps; the numbers
//! never restart on a reconnect, only with a new epoch.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::common::ExecutionRef;
use crate::common::{Completion, Receipt, Subject};
use crate::ids::*;
use crate::run::{ArtifactRef, Money, NetworkScope, Part, PathRef, SessionView, WorkspaceRef};

/// Position in a stream. Acknowledged only once the Hub committed everything up to it.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Watermark {
    pub stream: StreamId,
    pub epoch: Epoch,
    pub seq: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeEvent {
    pub schema: u16,
    pub id: EventId,
    pub at: Timestamp,
    pub cursor: Watermark,
    pub subject: Subject,
    pub body: EventBody,
}

#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct EventBatch {
    pub events: Vec<NodeEvent>,
}

/// The contiguous positions the Hub has durably committed, per stream. Delivery to any client does not
/// matter for it.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct EventAck {
    pub durable: Vec<Watermark>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    User,
    Assistant,
    Thought,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum EventBody {
    SessionChanged {
        view: SessionView,
    },
    AttemptStarted,
    /// The attempt is over; whether the turn is, the Hub decides.
    AttemptEnded {
        completion: Completion,
    },
    OperationChanged {
        receipt: Receipt,
    },
    MessageStarted {
        message: String,
        block: String,
        role: Role,
    },
    /// Appended text; `utf8_offset` is the byte offset in the block, so a resent delta is not appended twice.
    TextDelta {
        message: String,
        block: String,
        utf8_offset: u64,
        text: String,
    },
    MessageCompleted {
        message: String,
    },
    /// The whole current view of one tool invocation (replaces the previous one).
    ToolChanged {
        tool: ToolView,
    },
    PermissionRequested {
        request: PermissionRequest,
    },
    PermissionResolved {
        resolution: PermissionResolution,
    },
    PermissionExpired {
        id: PermissionId,
    },
    Usage {
        report: UsageReport,
    },
    WorkspaceChanged {
        workspace: WorkspaceRef,
        revision: Revision,
    },
    ArtifactAvailable {
        artifact: ArtifactRef,
    },
    /// Something worth showing that has no other place (a harness notice, an unsupported update).
    Diagnostic {
        severity: Severity,
        code: String,
        safe_text: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Severity {
    Info,
    Warning,
    Error,
}

/// What a tool call or a permission request would do, in Steve's terms, structured enough for the
/// Hub (and YOLO) to judge. What a driver cannot normalize is `Unknown` and goes by explicit policy.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Action {
    Exec {
        command: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        cwd: Option<PathRef>,
        network: NetworkScope,
    },
    Write {
        paths: Vec<PathRef>,
        change_summary: String,
    },
    Delete {
        paths: Vec<PathRef>,
    },
    Move {
        from: PathRef,
        to: PathRef,
    },
    Read {
        paths: Vec<PathRef>,
    },
    Search {
        query: String,
    },
    Fetch {
        url: String,
    },
    /// A Steve tool (its arguments are Steve's, not a harness's).
    Tool {
        name: String,
        args: Value,
    },
    Unknown {
        summary: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionDecision {
    AllowOnce,
    Reject,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PermissionRequest {
    pub id: PermissionId,
    pub execution: ExecutionRef,
    pub invocation: CallId,
    /// Short title for the card (`git push`, `编辑 src/main.rs`).
    pub title: String,
    pub action: Action,
    /// Hash of `action`: an answer only applies to the very action it was given for.
    pub digest: Digest,
    pub choices: Vec<PermissionDecision>,
    pub expires_at: Timestamp,
}

/// The Hub's answer, bound to the request, its attempt, its action and the decision's revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionResolution {
    pub id: PermissionId,
    pub execution: ExecutionRef,
    pub action_digest: Digest,
    pub decision_revision: Revision,
    pub decision: PermissionDecision,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolOrigin {
    /// One of Steve's tools (reached through the Node's local tool endpoint).
    Steve,
    /// The harness's own (shell, edit, search…).
    Harness,
    /// Another tool server the harness was given.
    External,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCategory {
    Read,
    Edit,
    Delete,
    Move,
    Search,
    Exec,
    Think,
    Fetch,
    Steve,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolPhase {
    Pending,
    Running,
    Succeeded,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolView {
    pub id: CallId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent: Option<CallId>,
    pub origin: ToolOrigin,
    /// The tool's own name (`Bash`, `steve_deliver`).
    pub name: String,
    pub category: ToolCategory,
    pub title: String,
    pub phase: ToolPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<Action>,
    /// Bounded; anything large is an artifact.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub result: Vec<Part>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub duration_ms: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Measurement {
    /// Add it.
    Delta,
    /// The running total of `meter_epoch`; replaces the previous figure of the same epoch.
    Cumulative,
    /// The attempt's final figure; replaces its deltas, never added to them.
    Final,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageScope {
    /// The main loop only.
    Main,
    /// Everything (sub-agents, compaction): the accounting figure. Never added to `main`.
    All,
    /// Work the harness did on its own, outside our attempts (the subject is the session).
    Background,
}

/// Unknown is `None`, never 0.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct Tokens {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub input: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub output: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_read: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_write: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageReport {
    /// Booked once: a resent report with the same id changes nothing.
    pub report_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    /// A harness restart begins a new epoch (its totals start at zero again).
    pub meter_epoch: Epoch,
    pub scope: UsageScope,
    pub measurement: Measurement,
    pub tokens: Tokens,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_used: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_limit: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<Money>,
}

impl Action {
    /// The digest a permission answer is bound to.
    pub fn digest(&self) -> Digest {
        Digest::sha256(serde_json::to_string(self).unwrap_or_default().as_bytes())
    }
}
