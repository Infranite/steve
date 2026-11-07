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
    /// A change to one file, as shown to people: `path` relative to the workspace when it is inside,
    /// the texts bounded (a preview, not the file).
    Diff {
        path: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        old: Option<String>,
        new: String,
        added: u32,
        removed: u32,
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
    /// The harness's own judgement decides (Claude Code's `auto`: routine work goes through, the risky
    /// rest asks). Wider than AcceptEdits, narrower than FullAccess.
    Auto,
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
    /// The principal's own variables on this Node (`node_env`): set in the harness's environment
    /// over the Node's, `PATH_PREPEND` in front of `PATH`; a key [`crate::env::user_env_refusal`]
    /// refuses is dropped. An older Node ignores them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<(String, String)>,
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

/// Which kind of job a [`JobWork`] is (for records and capacity; the work itself carries its fields).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobKind {
    /// One Steve model call (routing, an answer, a draft).
    Think,
    /// A read-only research run.
    Research,
}

/// One attempt of a job: one model, in a scratch directory the Node makes for it. The Hub keeps the
/// fallback (another model is another attempt) and builds the prompt. A job never reads through
/// `run.policy.readable`: what it may read is in its work.
///
/// While it runs, its draft arrives as `MessageStarted`/`TextDelta` (a new message replaces the
/// draft so far) and its tool calls as `ToolChanged`; its answer is the completion's `result` text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobSpec {
    pub job: JobId,
    pub run: RunSpec,
    pub work: JobWork,
}

/// What a job does, by kind (never a kind plus optional fields).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JobWork {
    Think(ThinkJob),
    Research(ResearchJob),
}

/// One Steve model call. It never continues a harness session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ThinkJob {
    /// The stable head of the prompt, kept apart so the harness can cache it (a system prompt, an
    /// instructions file). `None`: the whole prompt is the message.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub instructions: Option<String>,
    pub tools: ThinkToolMode,
}

/// The harness's own tools in a think job.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ThinkToolMode {
    /// No tool at all.
    None,
    /// Read only, for the prompt's [`Part::File`]s (placed at their `name` in the scratch directory,
    /// always `.steve/attachments/<file>`).
    ReadAttached,
}

/// A read-only research run over directories under the Node's roots. Its sandbox is
/// `run.policy.sandbox`, its hard time limit `run.limits.deadline` (one for all attempts of a run),
/// and `run.continuation` resumes an earlier run of the same model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResearchJob {
    /// Where the harness runs; `None`: in the job's scratch directory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<crate::workspace::RootPath>,
    /// The other directories it may read (never write).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub readable: Vec<crate::workspace::RootPath>,
    pub budget: ResearchBudget,
    /// Steve's read-only tools (the grant `run.policy.tools`) are offered to the harness.
    #[serde(default)]
    pub steve_tools: bool,
    /// The file parts (by name) attached to the message the question follows up on, not to the
    /// question itself: the prompt's note says so.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub earlier: Vec<String>,
}

/// Limits of one research attempt besides the deadline. `None`: no limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResearchBudget {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_turns: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_tools: Option<u32>,
    /// No output from the harness for this long: it is stuck and is ended.
    pub idle_ms: u64,
}

impl JobWork {
    pub fn kind(&self) -> JobKind {
        match self {
            JobWork::Think(_) => JobKind::Think,
            JobWork::Research(_) => JobKind::Research,
        }
    }
}

impl JobSpec {
    /// The shape every Node checks before it accepts a job (what it can enforce it checks after).
    pub fn check(&self) -> crate::common::ApiResult<()> {
        use crate::common::{ApiError, ErrorCode};
        let bad = |why: &str| Err(ApiError::new(ErrorCode::Invalid, why));
        let run = &self.run;
        if run.directory != RunDirectory::Scratch {
            return bad("a job runs in its own scratch directory");
        }
        if !run.policy.readable.is_empty() {
            return bad("a job names what it reads in its work");
        }
        let mut files = std::collections::HashSet::new();
        for p in &run.prompt {
            match p {
                Part::Text { .. } | Part::Image { .. } => {}
                Part::File { name, .. } => {
                    if !is_attachment_name(name) {
                        return bad("a file part is named .steve/attachments/<file>");
                    }
                    if !files.insert(name.as_str()) {
                        return bad("two file parts have the same name");
                    }
                }
                Part::Diff { .. } => return bad("a job prompt has no diff"),
            }
        }
        match &self.work {
            JobWork::Think(t) => {
                if run.continuation.is_some() {
                    return bad("a think job does not continue a session");
                }
                if !files.is_empty() && t.tools == ThinkToolMode::None {
                    return bad("files in a think job need read_attached");
                }
            }
            JobWork::Research(r) => {
                let b = &r.budget;
                if b.idle_ms == 0 || b.max_turns == Some(0) || b.max_tools == Some(0) {
                    return bad("a research budget limit is never zero");
                }
            }
        }
        Ok(())
    }
}

/// Where a job's files go in its scratch directory: `.steve/attachments/<file>`, one plain segment.
/// Keeping them there means a file can never be (or overwrite) the harness's own configuration
/// (`.claude/settings.json`, `.mcp.json`, `CLAUDE.md`) in the directory the harness runs from.
pub const ATTACHMENTS_DIR: &str = ".steve/attachments/";

fn is_attachment_name(name: &str) -> bool {
    name.strip_prefix(ATTACHMENTS_DIR).is_some_and(|f| {
        !f.is_empty() && f != "." && !f.contains('/') && RelPath::new(name).is_ok()
    })
}

/// How a research attempt ended, besides its [`crate::common::Outcome`] (in `Completion::job`).
/// Every finished research attempt carries one, a failed one too, so its turns and reads are kept.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct JobReport {
    pub end: JobEnd,
    #[serde(default)]
    pub turns: u32,
    /// Files it read, relative to its cwd when inside it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files_read: Vec<String>,
    /// It ran under an OS sandbox.
    #[serde(default)]
    pub sandboxed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum JobEnd {
    /// It finished with an answer (outcome `succeeded`).
    Done,
    /// The harness reported an error or ended without a result (outcome `failed`; the error is in
    /// `Completion::error`).
    Failed,
    /// Stopped by a cancel; the answer is what was written until then.
    Stopped,
    Budget {
        hit: BudgetHit,
    },
    /// A tool call broke the read-only rules and the run was killed.
    Violated {
        rule: ViolationRule,
        tool: String,
        /// The path it tried, when the rule is about one.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<String>,
    },
}

impl JobEnd {
    /// The end does not contradict the attempt's outcome: `done` comes only with success and
    /// `failed` only with failure (the other ends say why it stopped and leave the outcome to the
    /// Node).
    pub fn fits(&self, outcome: crate::common::Outcome) -> bool {
        use crate::common::Outcome as O;
        match self {
            JobEnd::Done => outcome == O::Succeeded,
            JobEnd::Failed => outcome == O::Failed,
            _ => true,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetHit {
    /// The deadline.
    Time,
    Turns,
    Tools,
    /// Silent for `idle_ms`.
    Idle,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ViolationRule {
    /// A tool not on the read-only allowlist.
    Tool,
    /// A path outside the readable directories.
    OutOfScope,
    /// A secret (keys, credentials, `.env`).
    Sensitive,
    /// A file tool without a usable path.
    BadInput,
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
