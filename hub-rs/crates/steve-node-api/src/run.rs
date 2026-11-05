//! What the Hub asks a Node to run: a session turn or a job, each one attempt with one model.
//! No tier, no fallback list, no CLI argument and no MCP endpoint: those are Hub decisions or driver
//! details.

use serde::{Deserialize, Serialize};

use crate::ids::*;

/// The harness and the one model of this attempt. Choosing it (tier, fallback) is the Hub's job.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessChoice {
    /// A harness the Node reported (`claude-code`, `codex`).
    pub harness: String,
    /// `None` leaves it to the harness's default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
}

/// A piece of a prompt or a result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Part {
    Text {
        text: String,
    },
    /// Shown to the model inline.
    Image {
        artifact: ArtifactRef,
    },
    /// Put where the harness can read it and named in the prompt.
    File {
        artifact: ArtifactRef,
        name: String,
    },
}

/// A blob the Hub keeps; a Node fetches its bytes ([`crate::ports::HubFiles`]) and checks the hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArtifactRef {
    pub id: ArtifactId,
    pub sha256: Digest,
    pub size: u64,
    pub mime: String,
}

/// A working directory on one Node.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct WorkspaceRef {
    pub node: NodeId,
    pub id: WorkspaceId,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PathRef {
    pub workspace: WorkspaceRef,
    pub rel: RelPath,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum RunDirectory {
    Workspace {
        workspace: WorkspaceRef,
    },
    /// A scratch directory the Node creates and cleans up (a Steve model call).
    Scratch,
}

/// A guarantee the Hub requires; names are Steve's capability names (`fs-readonly`, `no-network`…),
/// never vendor flags. A Node that cannot give it refuses (`unsupported`); a fallback is only what
/// the Hub listed, and it is recorded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum SandboxRequirement {
    None,
    Required {
        guarantees: Vec<String>,
    },
    AllowFallback {
        preferred: Vec<String>,
        acceptable: Vec<String>,
    },
}

/// How a harness's own tools (shell, edit…) are allowed: Steve's names for the bot setting. The
/// driver maps it to the harness's modes; a harness that cannot honor it refuses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    /// Read and plan only.
    Plan,
    /// Ask the Hub for anything that changes something.
    Ask,
    /// Edits in the workspace go through; the rest asks.
    AcceptEdits,
    /// Nothing asks. The Hub chose it and records it. It only means "no questions" within the policy:
    /// roots, network, sandbox, lease and the Hub's check of Steve's tools all still hold.
    FullAccess,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum NetworkScope {
    Denied,
    Hosts { hosts: Vec<String> },
    Unrestricted,
}

/// The policy of one execution, decided by the Hub, enforced by the Node. Writes are confined to the
/// execution's own workspace; `readable` directories are never writable. A driver that cannot
/// enforce a part of it refuses (`unsupported`) instead of mapping it to the nearest flag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionPolicy {
    pub revision: Revision,
    pub permissions: PermissionMode,
    pub sandbox: SandboxRequirement,
    /// Directories the run may read besides its own (a research run over several projects of one Node).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub readable: Vec<PathRef>,
    pub network: NetworkScope,
    /// Steve's tools this execution may see and call (the Hub checks again on every call).
    pub tools: GrantId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Money {
    /// A decimal string, never a float.
    pub decimal: String,
    pub currency: String,
}

/// Limits the Node enforces. `deadline` stays fixed across resends; `offline_until` is how long the
/// work may go on while the Hub cannot be reached (the lease).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Limits {
    pub deadline: Timestamp,
    pub offline_until: Timestamp,
    pub max_output_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tokens: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_cost: Option<Money>,
}

/// One attempt with one model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunSpec {
    pub attempt: AttemptId,
    pub choice: HarnessChoice,
    pub directory: RunDirectory,
    pub prompt: Vec<Part>,
    pub policy: ExecutionPolicy,
    pub limits: Limits,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub continuation: Option<ResumeHandle>,
}

/// Open (or reopen after a release) the harness context of a Steve session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenSession {
    pub session: SessionId,
    pub workspace: WorkspaceRef,
    pub choice: HarnessChoice,
    pub policy: ExecutionPolicy,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume: Option<ResumeHandle>,
}

/// One attempt of a turn. At most one attempt per session runs at a time: a busy session refuses
/// (`busy`), the queue is the Hub's.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TurnSpec {
    pub session: SessionId,
    pub turn: TurnId,
    pub run: RunSpec,
}

impl TurnSpec {
    /// A turn runs in its session's context: in the session's workspace, with exactly the session's
    /// current choice and policy (`open` as last configured). A model or policy change goes through
    /// [`ConfigureSession`] first; a continuation belongs to the session.
    pub fn check_against(&self, open: &OpenSession) -> crate::common::ApiResult<()> {
        use crate::common::{ApiError, ErrorCode};
        let bad = |why: &str| Err(ApiError::new(ErrorCode::Invalid, why));
        if self.session != open.session {
            return bad("turn of another session");
        }
        match &self.run.directory {
            RunDirectory::Workspace { workspace } if *workspace == open.workspace => {}
            _ => return bad("a turn runs in its session's workspace"),
        }
        if self.run.choice.harness != open.choice.harness {
            return bad("another harness needs another session");
        }
        if self.run.choice != open.choice || self.run.policy != open.policy {
            return bad("the session is configured otherwise: configure it first");
        }
        if self.run.continuation.is_some() {
            return bad("a session turn has no continuation of its own");
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    /// One Steve model call (routing, an answer, a draft).
    Think,
    /// A read-only research run.
    Research,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobSpec {
    pub job: JobId,
    pub kind: JobKind,
    pub run: RunSpec,
}

/// Change a session's model or policy between attempts. Applies only while idle (or at a boundary the
/// driver supports); never silently to a running attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConfigureSession {
    pub session: SessionId,
    pub expected: Revision,
    pub choice: HarnessChoice,
    pub policy: ExecutionPolicy,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionPhase {
    Opening,
    Ready,
    Active,
    Releasing,
    /// The harness process is gone; the Steve session is not (open it again to continue).
    Released,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionView {
    pub id: SessionId,
    pub revision: Revision,
    pub phase: SessionPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active: Option<crate::common::ExecutionRef>,
    /// What the harness actually uses (it may differ from what was asked).
    pub effective: HarnessChoice,
    /// The permission mode and guarantees actually in force (what the audit shows).
    pub effective_permissions: PermissionMode,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub guarantees: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume: Option<ResumeHandle>,
}
