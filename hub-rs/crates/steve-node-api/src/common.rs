//! Commands, receipts, observed states and errors shared by every face of the protocol.

use serde::{Deserialize, Serialize};
pub(crate) use steve_types::{Code, Error};

use crate::ids::*;
use crate::run::{Part, SessionView};
use crate::workspace::{GitFacts, WorkspaceView};

pub type ApiResult<T> = Result<T, Error>;

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
                Error::new(Code::Invalid, "accept_before is before issued_at")
                    .with_param("operation", self.id.to_string()),
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
        .map_err(|e| Error::new(Code::Invalid, format!("not serializable: {e}")))?;
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
        error: Option<Error>,
    },
    Unknown {
        reason: String,
    },
}

impl Observed {
    pub fn state_name(&self) -> &'static str {
        match self {
            Self::Accepted => "accepted",
            Self::Running => "running",
            Self::Waiting { .. } => "waiting",
            Self::Finished { .. } => "finished",
            Self::Unknown { .. } => "unknown",
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
    pub error: Option<Error>,
    /// The final answer of a job; empty for a session turn (its text is in the events).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub result: Vec<Part>,
    /// Where the harness session continues (a research run's follow-up, a session after release).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation: Option<ResumeHandle>,
    /// The failure may pass by itself (an inference gateway's 503, a rate limit): a fact for the Hub's
    /// retry policy, never a retry by the Node.
    #[serde(default)]
    pub transient: bool,
    /// Tools ran in this attempt: their effects happened, a retry must not redo them blindly.
    #[serde(default)]
    pub acted: bool,
    /// How a research job ended besides its outcome. Every finished research attempt carries one,
    /// a failed one too (`JobEnd::Failed`, the error above); `None` for turns and think jobs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub job: Option<crate::run::JobReport>,
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

/// One page of a snapshot. All pages of one cursor chain show the same moment; entries and stream
/// heads are both paged (`limit=0`: the Node's default page size). Replaying from the collected
/// heads misses nothing since that snapshot moment.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Snapshot {
    pub entries: Vec<SnapshotEntry>,
    pub heads: Vec<StreamHead>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<Cursor>,
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

/// The Hub gives up on an execution whose outcome is unknown (after a Node restart): the Node makes
/// sure its processes are gone (stopping them if they still run), then ends it as interrupted and
/// frees what it held. Only for `unknown` targets; a live one is cancelled instead.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Abandon {
    pub target: OperationId,
    pub reason: String,
}

/// Retire an unknown *control* operation, preserving any filesystem effects. The Node must prove
/// its processes stopped, and CAS the exact target digest; no external action is replayed. The
/// target becomes Interrupted (not Succeeded). Executions use Abandon instead. A control whose
/// processes cannot be proven stopped stays OutcomeUnknown.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Recover {
    pub target: OperationId,
    pub expected: Digest,
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
