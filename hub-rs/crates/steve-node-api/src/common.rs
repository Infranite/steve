//! Commands, receipts, observed states and errors shared by every face of the protocol.

use serde::{Deserialize, Serialize};

use crate::ids::*;
use crate::run::{Part, SessionView};
use crate::workspace::{GitFacts, WorkspaceView};

pub use steve_types::devices::NodeReason;
pub use steve_types::{Code, Error, Param, Params, Retry};

/// Construct an ordinary protocol error using the shared retry policy.
pub fn error(code: Code, detail: impl Into<String>) -> Error {
    Error {
        code,
        retry: code.default_retry(),
        reason: None,
        params: Params::default(),
        safe_detail: detail.into(),
    }
}

/// Attach display metadata. Operation identity for decisions belongs to commands and receipts.
pub fn on_operation(mut error: Error, operation: &OperationId) -> Error {
    error
        .params
        .0
        .insert("operation".into(), Param::Str(operation.to_string()));
    error
}

/// Unknown outcomes default to reconciliation. Only proven safe callers opt into retry.
pub fn uncertain(operation: &OperationId, detail: impl Into<String>) -> Error {
    let mut error = on_operation(error(Code::OutcomeUnknown, detail), operation);
    error
        .params
        .0
        .insert("state".into(), Param::Str("unknown".into()));
    error
}

pub type ApiResult<T> = Result<T, Error>;

/// The state of an observed operation; error metadata is carried in display params.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StateTag {
    Accepted,
    Running,
    Waiting,
    Finished,
    Unknown,
}

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
            return Err(crate::on_operation(
                crate::error(Code::Invalid, "accept_before is before issued_at"),
                &self.id,
            ));
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
        .map_err(|e| crate::error(Code::Invalid, format!("not serializable: {e}")))?;
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

/// One page of a snapshot. All pages of one cursor chain show the same moment. A request with
/// `limit=0` starts bounded pages of both entries and stream heads; older Nodes return one entry
/// and the complete heads vector. Replaying from the collected heads misses nothing since that
/// snapshot moment.
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
/// target becomes Interrupted (not Succeeded). Executions use Abandon instead. Legacy controls
/// without sufficient recovery evidence may remain Unsupported/OutcomeUnknown.
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

#[cfg(test)]
mod error_contract_tests {
    use super::*;

    #[test]
    fn ordinary_codes_default_retry_and_wire_shape_follow_the_shared_contract() {
        let later = Retry::Later { after_ms: None };
        let cases = [
            (
                "offline",
                error(Code::Unavailable, "safe"),
                "unavailable",
                later,
            ),
            ("busy", error(Code::Busy, "safe"), "busy", later),
            (
                "unsupported",
                error(Code::Unsupported, "safe"),
                "unsupported",
                Retry::Never,
            ),
            (
                "denied",
                error(Code::Forbidden, "safe"),
                "forbidden",
                Retry::Never,
            ),
            ("expired", error(Code::Stale, "safe"), "stale", Retry::Never),
            ("stale", error(Code::Stale, "safe"), "stale", Retry::Never),
            (
                "conflict",
                error(Code::Conflict, "safe"),
                "conflict",
                Retry::Never,
            ),
            (
                "exhausted",
                error(Code::Exhausted, "safe"),
                "exhausted",
                later,
            ),
            (
                "invalid",
                error(Code::Invalid, "safe"),
                "invalid",
                Retry::Never,
            ),
            (
                "downgrade",
                NodeReason::downgrade_refused("safe"),
                "invalid",
                Retry::Never,
            ),
            (
                "outcome_unknown",
                error(Code::OutcomeUnknown, "safe"),
                "outcome_unknown",
                Retry::Never,
            ),
            ("gap", error(Code::Gap, "safe"), "gap", Retry::Never),
            (
                "snapshot_gone",
                NodeReason::snapshot_gone("safe"),
                "stale",
                Retry::Never,
            ),
            (
                "no_such_operation",
                NodeReason::no_such_operation("missing_op", "safe"),
                "not_found",
                Retry::Never,
            ),
            (
                "internal",
                error(Code::Internal, "safe"),
                "internal",
                Retry::Never,
            ),
        ];
        for (old_name, error, code, retry) in cases {
            let value = serde_json::to_value(&error).unwrap();
            assert_eq!(value["code"], code, "{old_name}");
            assert!(value["retry"].is_object(), "{old_name}");
            assert_eq!(error.retry, retry, "{old_name}");
            let reason = match old_name {
                "downgrade" => Some("node.downgrade_refused"),
                "snapshot_gone" => Some("node.snapshot_gone"),
                "no_such_operation" => Some("node.no_such_operation"),
                _ => None,
            };
            assert_eq!(
                error.reason.as_ref().map(|r| r.as_str()),
                reason,
                "{old_name}"
            );
            if old_name == "no_such_operation" {
                assert_eq!(error.params.0["operation"], Param::Str("missing_op".into()));
            }
            assert_eq!(serde_json::from_value::<Error>(value).unwrap(), error);
        }
    }

    #[test]
    fn unknown_and_gap_default_to_never_and_retry_requires_an_explicit_producer() {
        let operation = OperationId::new("op-1").unwrap();
        let unknown = uncertain(&operation, "reply lost");
        assert_eq!(unknown.retry, Retry::Never);
        assert_eq!(error(Code::OutcomeUnknown, "unknown").retry, Retry::Never);
        assert_eq!(error(Code::Gap, "gap").retry, Retry::Never);
        let retryable = unknown.retry(Retry::Later { after_ms: None });
        let decoded: Error =
            serde_json::from_slice(&serde_json::to_vec(&retryable).unwrap()).unwrap();
        assert_eq!(decoded.retry, Retry::Later { after_ms: None });
        assert_eq!(decoded.params.0["operation"], Param::Str("op-1".into()));
        assert_eq!(decoded.params.0["state"], Param::Str("unknown".into()));
    }

    #[test]
    fn operation_and_every_state_are_display_params() {
        let operation = OperationId::new("op-1").unwrap();
        for state in [
            StateTag::Accepted,
            StateTag::Running,
            StateTag::Waiting,
            StateTag::Finished,
            StateTag::Unknown,
        ] {
            let state = serde_json::to_value(state)
                .unwrap()
                .as_str()
                .unwrap()
                .to_owned();
            let mut error = on_operation(error(Code::Internal, "safe"), &operation);
            error
                .params
                .0
                .insert("state".into(), Param::Str(state.clone()));
            let value = serde_json::to_value(&error).unwrap();
            assert!(value.get("operation").is_none());
            assert!(value.get("state").is_none());
            let decoded: Error = serde_json::from_value(value).unwrap();
            assert_eq!(decoded.params.0["operation"], Param::Str("op-1".into()));
            assert_eq!(decoded.params.0["state"], Param::Str(state));
            assert_eq!(decoded.code, Code::Internal);
            assert_eq!(decoded.retry, Retry::Never);
        }
    }
}
