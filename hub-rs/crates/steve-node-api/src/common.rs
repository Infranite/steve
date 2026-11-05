//! Commands, receipts, observed states and errors shared by every face of the protocol.

use serde::{Deserialize, Serialize};
use std::fmt;

use crate::ids::*;
use crate::run::{Part, SessionView};
use crate::workspace::{GitFacts, WorkspaceView};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    /// The Node (or the Hub) cannot be reached.
    Offline,
    /// Already running something that excludes this (one attempt per session at a time).
    Busy,
    /// This Node cannot do it (no such harness, no sandbox of the required kind).
    Unsupported,
    Denied,
    /// Past its deadline or lease.
    Expired,
    /// Past `accept_before`, or an answer to a request nobody waits for anymore.
    Stale,
    /// Same key, different content.
    Conflict,
    /// Out of capacity, disk or budget.
    Exhausted,
    Invalid,
    /// It may or may not have happened; ask, do not redo.
    OutcomeUnknown,
    /// Events were lost beyond the retention window.
    Gap,
    Internal,
}

/// What the caller may do after an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetryClass {
    Never,
    /// Resend the very same command (same operation id): safe, it is deduplicated.
    SameOperation,
    /// Find out what happened first ([`crate::ports::ExecutionControl::inspect`]).
    AfterReconcile,
}

/// Where an operation stood when an error was raised (flat, so errors and results never nest).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StateTag {
    Accepted,
    Running,
    Waiting,
    Finished,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApiError {
    pub code: ErrorCode,
    pub retry: RetryClass,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<OperationId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<StateTag>,
    /// Safe to show and to log: no secret, no path outside the workspace.
    pub safe_detail: String,
}

impl ApiError {
    /// Refused before anything was accepted: nothing ran. Offline, busy and exhausted may be resent
    /// as the very same command (same operation id, never a new attempt); the rest may not.
    pub fn new(code: ErrorCode, detail: impl Into<String>) -> Self {
        let retry = match code {
            ErrorCode::Offline | ErrorCode::Busy | ErrorCode::Exhausted => {
                RetryClass::SameOperation
            }
            ErrorCode::OutcomeUnknown | ErrorCode::Gap => RetryClass::AfterReconcile,
            _ => RetryClass::Never,
        };
        ApiError {
            code,
            retry,
            operation: None,
            state: None,
            safe_detail: detail.into(),
        }
    }
    /// The command may have been accepted (the answer got lost, the connection broke after sending):
    /// find out with [`crate::ports::ExecutionControl::operation`] before doing anything else.
    pub fn uncertain(op: &OperationId, detail: impl Into<String>) -> Self {
        ApiError {
            code: ErrorCode::OutcomeUnknown,
            retry: RetryClass::AfterReconcile,
            operation: Some(op.clone()),
            state: Some(StateTag::Unknown),
            safe_detail: detail.into(),
        }
    }
    pub fn on(mut self, op: &OperationId) -> Self {
        self.operation = Some(op.clone());
        self
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:?}: {}", self.code, self.safe_detail)
    }
}

impl std::error::Error for ApiError {}

pub type ApiResult<T> = Result<T, ApiError>;

/// A command from the Hub. `id` makes it idempotent; `issued_at` and `accept_before` never change on
/// a retry, so a command older than its window is refused as stale even after its tombstone is gone.
/// The grant is checked against the authenticated connection, never trusted from the payload alone.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Command<T> {
    pub id: OperationId,
    pub issued_at: Timestamp,
    pub accept_before: Timestamp,
    pub grant: GrantId,
    pub body: T,
}

impl<T: Serialize> Command<T> {
    /// The content hash a Node keeps with the operation id: the same id with another hash (or for
    /// another method) is a conflict. Covers the method and every business field, not the
    /// transport's request id; canonical, so key order never changes it.
    pub fn digest(&self, method: &str) -> ApiResult<Digest> {
        digest_of(&serde_json::json!({
            "method": method,
            "issued_at": self.issued_at,
            "accept_before": self.accept_before,
            "grant": self.grant,
            "body": self.body,
        }))
    }
    /// `issued_at` ≤ `accept_before`.
    pub fn check_window(&self) -> ApiResult<()> {
        if self.issued_at > self.accept_before {
            return Err(
                ApiError::new(ErrorCode::Invalid, "accept_before is before issued_at").on(&self.id),
            );
        }
        Ok(())
    }
}

/// JSON with object keys sorted at every level: the same value always gives the same text (the
/// crate's `serde_json` keeps insertion order, which differs between producers).
pub fn canonical_json(v: &serde_json::Value) -> String {
    use serde_json::Value;
    fn walk(v: &Value, out: &mut String) {
        match v {
            Value::Object(m) => {
                let mut keys: Vec<&String> = m.keys().collect();
                keys.sort();
                out.push('{');
                for (i, k) in keys.into_iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    out.push_str(&Value::String(k.clone()).to_string());
                    out.push(':');
                    walk(&m[k], out);
                }
                out.push('}');
            }
            Value::Array(a) => {
                out.push('[');
                for (i, x) in a.iter().enumerate() {
                    if i > 0 {
                        out.push(',');
                    }
                    walk(x, out);
                }
                out.push(']');
            }
            other => out.push_str(&other.to_string()),
        }
    }
    let mut out = String::new();
    walk(v, &mut out);
    out
}

/// sha256 of the canonical JSON of `v`. Fails (rather than hashing nothing) when `v` does not serialize.
pub fn digest_of<T: Serialize + ?Sized>(v: &T) -> ApiResult<Digest> {
    let v = serde_json::to_value(v)
        .map_err(|e| ApiError::new(ErrorCode::Invalid, format!("not serializable: {e}")))?;
    Ok(Digest::sha256(canonical_json(&v).as_bytes()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Outcome {
    Succeeded,
    Failed,
    Cancelled,
    TimedOut,
    /// Cut by a restart or a lost process; what it did up to then is in its events.
    Interrupted,
}

/// What an attempt or operation is waiting for (several at once are possible).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "on", content = "id", rename_all = "snake_case")]
pub enum WaitRef {
    Permission(PermissionId),
    Tool(CallId),
    Dependency(OperationId),
}

/// An operation as its executor sees it. `Unknown` is an observation (after a crash, during a
/// partition), not an end: it does not say the process stopped, so nothing may be freed or redone on it.
// `Finished` carries its result inline: receipts are few and short-lived, boxing buys nothing.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum Observed {
    Accepted,
    Running,
    Waiting {
        on: Vec<WaitRef>,
    },
    Finished {
        outcome: Outcome,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        result: Option<OperationResult>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        error: Option<ApiError>,
    },
    Unknown {
        reason: String,
    },
}

impl Observed {
    pub fn tag(&self) -> StateTag {
        match self {
            Observed::Accepted => StateTag::Accepted,
            Observed::Running => StateTag::Running,
            Observed::Waiting { .. } => StateTag::Waiting,
            Observed::Finished { .. } => StateTag::Finished,
            Observed::Unknown { .. } => StateTag::Unknown,
        }
    }
    pub fn is_finished(&self) -> bool {
        matches!(self, Observed::Finished { .. })
    }
}

/// The result of a finished operation, by kind.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum OperationResult {
    Session(SessionView),
    Execution(Completion),
    Workspace(WorkspaceView),
    Git(GitFacts),
    Exec(crate::workspace::ExecResult),
    /// A scan or an import is read page by page under its own operation id.
    Pages {
        operation: OperationId,
    },
    Unit,
}

/// How an attempt ended: the fact the Hub decides on (end the turn, try again, ask the user).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Completion {
    pub outcome: Outcome,
    /// Steve's stop reason (`end_turn`, `max_tokens`, `refusal`, `cancelled`, `error`…).
    pub stop_reason: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<ApiError>,
    /// The final answer of a job; empty for a session turn (its text is in the events).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub result: Vec<Part>,
    /// Where the harness session continues (a research run's follow-up, a session after release).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation: Option<ResumeHandle>,
}

/// The answer to a command: it was accepted (or had already been) and where it stands. Not
/// necessarily its result.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Receipt {
    pub operation: OperationId,
    pub state: Observed,
}

/// Whom an event or a stream is about.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "of", rename_all = "snake_case")]
pub enum Subject {
    /// Session-level facts with no attempt (a model switch, a harness's own background work).
    Session {
        session: SessionId,
    },
    Execution(ExecutionRef),
    Operation {
        operation: OperationId,
    },
}

/// One attempt of a session turn or of a job.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ExecutionRef {
    Turn {
        session: SessionId,
        turn: TurnId,
        attempt: AttemptId,
    },
    Job {
        job: JobId,
        attempt: AttemptId,
    },
}

impl ExecutionRef {
    pub fn attempt(&self) -> &AttemptId {
        match self {
            ExecutionRef::Turn { attempt, .. } | ExecutionRef::Job { attempt, .. } => attempt,
        }
    }
}

/// One accepted operation as a Node can report it after a reconnect: enough for the Hub to check it
/// against its own ledger (which stays the source of the command itself).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SnapshotEntry {
    pub receipt: Receipt,
    /// The method it was accepted under (`start_turn`, `prepare_workspace`…).
    pub method: String,
    pub digest: Digest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution: Option<ExecutionRef>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace: Option<crate::run::WorkspaceRef>,
    /// Permission requests of this execution still waiting, in full.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub pending: Vec<crate::events::PermissionRequest>,
    /// A cancel was asked for and the end has not come yet.
    #[serde(default)]
    pub cancel_requested: bool,
}

/// Where a stream stands on the Node: what it produced and from where it can still replay.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamHead {
    pub stream: StreamId,
    pub epoch: Epoch,
    /// Highest seq produced (0: none yet).
    pub produced: u64,
    /// Highest seq acknowledged; replay starts after it, anything before is gone.
    pub acknowledged: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cancel {
    pub target: OperationId,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PageRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cursor: Option<Cursor>,
    pub limit: u32,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Page<T> {
    pub items: Vec<T>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<Cursor>,
}
