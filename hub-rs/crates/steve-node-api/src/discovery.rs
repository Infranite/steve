//! What a Node can do and what harness history it has.

use serde::{Deserialize, Serialize};

use crate::events::NodeEvent;
use crate::ids::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessCapability {
    /// The harness adapter's id (`claude-code`, `codex`, or any id a manifest declares).
    pub name: String,
    /// What the harness itself reports (`claude --version`), filled by a probe; empty when unknown.
    pub version: String,
    /// Sessions (multi-turn, permissions) and/or jobs (one-shot).
    pub sessions: bool,
    pub jobs: bool,
    /// The models the harness lists, filled by a probe; empty when it lists none.
    pub models: Vec<String>,
    pub resume: bool,
    pub cancel: bool,
    pub permissions: bool,
    pub images: bool,
    /// The selected adapter declares read access to explicitly attached files in a think job.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub read_attached: bool,
    /// Guarantees it can enforce (`fs-readonly`, `no-network`…), Steve's names.
    pub enforcement: Vec<String>,
    /// What its adapter is and said about itself (`initialize`), when the Node asked it; `None`
    /// for an adapter the Node does not probe.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter: Option<AdapterInfo>,
    /// Component discovery and controlled setup; `None` for a harness without managed components.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub setup: Option<HarnessSetup>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SetupState {
    NotInstalled,
    Installing,
    Ready,
    Unavailable,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessComponent {
    pub id: String,
    pub name: String,
    pub package: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub path: Option<String>,
    /// Evidence for this expected component's integrity, independent of session readiness.
    pub status: ComponentStatus,
    /// Another executable discovered on the Node. Read-only evidence: it does not replace
    /// `path`, complete a managed installation, or change the command used for sessions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detected: Option<DetectedComponent>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentStatus {
    Installed,
    Missing,
    Unknown,
}

impl HarnessComponent {
    pub fn installed(&self) -> bool {
        self.status == ComponentStatus::Installed
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DetectedComponent {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessSetup {
    pub state: SetupState,
    pub components: Vec<HarnessComponent>,
    pub can_install: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub help: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Install only the trusted components of a built-in id into the Node's configured prefix.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InstallHarness {
    pub harness: String,
}

/// A harness adapter as its Node knows it (harness parity S1).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdapterInfo {
    /// For people: the declared name, or what the adapter calls itself.
    pub title: String,
    /// One of Steve's built-in entries, not one the Node's manifest added.
    #[serde(default)]
    pub builtin: bool,
    pub status: AdapterStatus,
    /// Why it is not ready (no stderr, no arguments).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// `agentInfo.version`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
    /// `loadSession`: a session can be continued.
    #[serde(default)]
    pub load_session: bool,
    /// `sessionCapabilities.list`: its sessions can be listed.
    #[serde(default)]
    pub list_sessions: bool,
    /// `mcpCapabilities.http`: Steve's tools reach it over HTTP.
    #[serde(default)]
    pub mcp_http: bool,
    /// `promptCapabilities.embeddedContext`.
    #[serde(default)]
    pub embedded_context: bool,
    /// The ways it offers to sign in (`authMethods`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub auth: Vec<AdapterAuth>,
    /// How to install it, for people.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install_hint: Option<String>,
    /// `sessionCapabilities.systemPrompt`: it takes a system prompt of Steve's.
    #[serde(default)]
    pub system_prompt: bool,
    /// What a scratch session offered (no prompt: nothing is asked of a model); `None` when the
    /// Node could not open one (`session_error`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<AdapterOffer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_error: Option<String>,
    /// Only present when the adapter explicitly required authentication during the ACP probe.
    /// Absence is not proof that the provider account or entitlement was verified.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_auth_required: Option<bool>,
    /// Modes the Node's own ceiling allows for a normal unrestricted-network session; `None`
    /// without a session offer. The adapter offer alone cannot prove Node policy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_modes: Option<Vec<crate::PermissionMode>>,
    /// An adapter Steve installed and keeps up to date (`<home>/adapters`): what its last check of
    /// the npm registry found. `None` for one installed otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update: Option<AdapterUpdate>,
    /// Shell template (`{id}` replaced with the session id) to resume a session in the CLI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_command: Option<String>,
    /// Shell template (`{id}` replaced with the session id) to fork a session in the CLI.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fork_command: Option<String>,
}

/// The update state of an adapter Steve keeps up to date ([`AdapterInfo::update`]).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdapterUpdate {
    /// `STEVE_ADAPTER_UPDATES` on the Node: `auto`, `check` or `off`.
    pub policy: String,
    /// The installed release.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed: Option<String>,
    /// The latest release on the registry, as of `checked`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest: Option<String>,
    /// When the registry was asked last (Unix ms).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked: Option<i64>,
    /// Why the last check or upgrade failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// A newer release waits for the Node to have no session running (`auto`).
    #[serde(default)]
    pub pending: bool,
}

/// The permission modes, models and efforts an adapter offers (harness parity S4, S8).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdapterOffer {
    /// Steve's permission modes it can be set to.
    #[serde(default)]
    pub modes: Vec<crate::PermissionMode>,
    #[serde(default)]
    pub models: Vec<AdapterModel>,
    /// What it runs when no model is set.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    /// The reasoning efforts it takes.
    #[serde(default)]
    pub efforts: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdapterModel {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdapterStatus {
    /// `initialize` succeeded.
    Ready,
    /// Its command could not be started.
    AdapterMissing,
    /// It started, but `initialize` failed or did not answer in time.
    InitializeFailed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdapterAuth {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootInfo {
    pub id: RootId,
    /// For people ("~/code"), never an address.
    pub display: String,
    #[serde(default)]
    pub access: RootAccess,
}

/// What a root allows. A readonly root (a folder to browse or index) can be listed, read and
/// scanned; preparing a workspace in it, running a command or a git action there is `denied`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RootAccess {
    #[default]
    ReadWrite,
    ReadOnly,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capacity {
    pub sessions: u32,
    pub jobs: u32,
    pub execs: u32,
}

impl Capacity {
    /// Concurrent turns on a newly started session-capable Node, before the Hub synchronizes its
    /// saved per-device setting. Idle sessions, jobs and execs do not consume these slots.
    pub const SESSIONS_DEFAULT: u32 = 32;
    /// Largest configurable concurrent-turn count.
    pub const SESSIONS_MAX: u32 = 256;
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    pub node: NodeId,
    /// `macOS 27.0 · Apple M5 · 64 GB`.
    pub os: String,
    pub version: String,
    pub harnesses: Vec<HarnessCapability>,
    pub roots: Vec<RootInfo>,
    pub capacity: Capacity,
    /// Optional abilities, by name ([`FEATURE_SECRET_FILE`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub features: Vec<String>,
}

/// A binary's release identity, as `--build-info` reports it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BuildIdentity {
    pub version: String,
    /// The full Git commit.
    pub commit: String,
    /// The Rust target triple (`aarch64-apple-darwin`).
    pub target: String,
}

/// How a Node's machine is doing now (B3): what the Hub samples to show on the devices page and,
/// later, to choose where work goes. Every figure is optional: a platform that cannot say leaves it
/// out rather than guessing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NodeVitals {
    pub at: Timestamp,
    /// Logical CPUs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpus: Option<u32>,
    /// Load average over 1, 5 and 15 minutes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub load: Option<[f64; 3]>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mem_total: Option<u64>,
    /// What can be had without swapping (Linux `MemAvailable`; free, inactive and purgeable pages
    /// on macOS).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mem_available: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub roots: Vec<RootVitals>,
    /// Sessions this Node is running now.
    #[serde(default)]
    pub sessions: u32,
    /// The Node process's own meters; the Hub exports them as `steve_node_*{node}` and does not
    /// keep them with the samples.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub meters: Option<NodeMeters>,
}

/// What the Node process does to its machine and how far its journal is behind. Totals count from
/// the Node process's start: a restart begins them at zero again.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct NodeMeters {
    /// Live journal bytes, and the journal file's size.
    pub journal_bytes: u64,
    pub journal_file_bytes: u64,
    /// Journal write transactions committed. Each is durable on its own (WAL, `synchronous=FULL`):
    /// one commit is one fsync of the log.
    pub journal_commits: u64,
    /// Journaled events the Hub has not acknowledged yet, and their bytes.
    pub event_backlog: u64,
    pub event_backlog_bytes: u64,
    /// Harness process groups the Node runs, and what their live processes use together.
    pub adapters: u32,
    pub adapter_cpu_seconds: f64,
    pub adapter_resident_bytes: u64,
    /// The Node process itself; `None` when the platform cannot say.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process: Option<ProcessMeters>,
    /// Non-loopback interfaces, absent when the host cannot supply counters.
    #[serde(default, skip_serializing_if = "std::collections::BTreeMap::is_empty")]
    pub network: std::collections::BTreeMap<String, NetworkBytes>,
}

/// Cumulative host interface counters, in bytes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct NetworkBytes {
    pub received: u64,
    pub sent: u64,
}

/// One process's use of its machine. A figure the platform does not give is left out.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProcessMeters {
    pub cpu_seconds: f64,
    pub resident_bytes: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub open_fds: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub threads: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_read_bytes: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub disk_written_bytes: Option<u64>,
}

/// What the Node's machine is (H→N `node.host`). The Hub asks once per link and also on demand
/// to recheck file access. It keeps the latest answer for when the Node is away.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostInfo {
    /// The machine's name as its owner sees it (macOS: the computer name; elsewhere the host name).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub hostname: Option<String>,
    /// The operating system family, as Rust names it: `macos`, `linux`, `windows`, …
    pub os: String,
    /// The system's own name for itself: `macOS`, `Ubuntu`, `Debian GNU/Linux`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os_name: Option<String>,
    /// Its version: `26.1`, `24.04`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os_version: Option<String>,
    /// The processor architecture, as Rust names it: `aarch64`, `x86_64`.
    pub arch: String,
    /// Logical CPUs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cpus: Option<u32>,
    /// Bytes of memory.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mem_total: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub file_access: Option<crate::FileAccessReport>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RootVitals {
    pub root: RootId,
    /// How the root is shown (`~/code`).
    #[serde(default)]
    pub display: String,
    pub free: u64,
    pub total: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub inodes_free: Option<u64>,
}

/// The Node writes a secret to a temporary file for a command ([`crate::SecretDelivery::File`]).
/// Only the Hub's own machine offers it; a remote Node's claim is dropped.
pub const FEATURE_SECRET_FILE: &str = "secret-file";
/// Stage a release on the Node (H→N upgrade-v1 `upgrade_stage`), under `operation`: the Node pulls the archive
/// `artifact` from the Hub over its link (`blob.fetch`, [`crate::FileAccess::Operation`]), checks
/// its size and SHA256 and the build identity of what it unpacks, and keeps it next to its current
/// release. Nothing switches yet ([`Discovery::upgrade_switch`]). The same operation again answers
/// its state; another while one is unfinished is `busy`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeUpgrade {
    pub operation: OperationId,
    /// What the unpacked binaries must report: the Hub's own version and commit, for this Node's
    /// target.
    pub build: BuildIdentity,
    pub artifact: ArtifactId,
    pub size: u64,
    /// Lowercase hex.
    pub sha256: String,
    /// How long the new release has to link to the Hub before the Node goes back (seconds).
    pub health_secs: u32,
}

/// Switch to the staged release, or give it up (`abandon`). A switch refuses (`busy`) while
/// the Node still runs work unless `force`: then running turns end with the Node's restart and the
/// Hub continues them as after any restart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UpgradeSwitch {
    pub operation: OperationId,
    #[serde(default)]
    pub abandon: bool,
    #[serde(default)]
    pub force: bool,
}

/// Where a Node upgrade is. Terminal: `succeeded`, `failed` (nothing switched), `rolled_back`
/// (switched, then back on the previous release), `abandoned`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeUpgradeState {
    pub operation: OperationId,
    /// `staging` | `staged` | `switching` | `linked` | `succeeded` | `failed` | `rolled_back` |
    /// `abandoned`.
    pub phase: String,
    pub version: String,
    /// The release the Node ran before, once known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// When it last changed (ms).
    pub updated: Timestamp,
}

impl NodeUpgradeState {
    pub const TERMINAL: &'static [&'static str] =
        &["succeeded", "failed", "rolled_back", "abandoned"];
    pub fn is_terminal(&self) -> bool {
        Self::TERMINAL.contains(&self.phase.as_str())
    }
}

/// The Node can remove itself from its machine when the Hub asks ([`Discovery::remove_begin`]):
/// stop taking work, let what runs end (or cancel it past a grace period), then, once the Hub
/// confirms, stop and uninstall its service and delete its home. Only a Node installed as a
/// service says so: one run by hand has no service to uninstall.
pub const FEATURE_NODE_REMOVE: &str = "node-remove-v1";

/// Begin removing the Node from its machine (H→N `node.remove`), under `operation`. The Node stops
/// taking new work at once and waits up to `grace_secs` for what runs to end; past it, it cancels
/// what still runs. Then it is `ready` and waits for the Hub's [`RemoveCommit`]: nothing is
/// uninstalled or deleted before. The same operation again answers its state; another while one
/// is unfinished is `busy`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeRemove {
    pub operation: OperationId,
    /// How long running turns, jobs and commands may go on before they are cancelled (seconds).
    pub grace_secs: u32,
}

/// The Hub recorded that the Node is `ready` and confirms the removal (`abandon: false`): the
/// Node answers `removing`, drops its link, and a helper outside its service stops and uninstalls
/// the service and deletes the Node's home. `abandon: true` gives the removal up instead: the
/// Node takes work again. Refused (`busy`) while it is still draining.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoveCommit {
    pub operation: OperationId,
    #[serde(default)]
    pub abandon: bool,
}

/// Where a removal is. `draining` (no new work, waiting for what runs) → `cancelling` (past the
/// grace period, what ran was cancelled) → `ready` (nothing runs; waiting for the Hub) →
/// `removing` (confirmed: being uninstalled and deleted). Terminal: `removing`, `abandoned` (the
/// Node takes work again), `failed`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NodeRemoveState {
    pub operation: OperationId,
    pub phase: String,
    /// Turns, jobs and commands still running when it was written.
    #[serde(default)]
    pub running: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// When it last changed (ms).
    pub updated: Timestamp,
}

impl NodeRemoveState {
    pub const TERMINAL: &'static [&'static str] = &["removing", "abandoned", "failed"];
    pub fn is_terminal(&self) -> bool {
        Self::TERMINAL.contains(&self.phase.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryRef {
    pub harness: String,
    /// The Node's key for it.
    pub key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryEntry {
    pub source: HistoryRef,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directory: Option<String>,
    pub updated: Timestamp,
    pub version: Revision,
    /// The transcript's size in bytes, when the Node knows it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size: Option<u64>,
}

/// Where a history entry is, for people and for resuming. Only a key the Node listed itself, or
/// `session:<harness session id>` (looked up in the harness's own session store, never a path),
/// is located; any other (a key made up by a client) is refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryLocation {
    pub source: HistoryRef,
    /// What to show ("~/.claude/projects/…/abc.jsonl"), never an address.
    pub display: String,
    /// The harness session it continues, when the harness can resume it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume: Option<ResumeHandle>,
    /// When the transcript last changed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub updated: Option<Timestamp>,
    /// The session id and working directory the transcript itself records, when it does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recorded_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recorded_cwd: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryImport {
    pub source: HistoryRef,
    pub expected: Revision,
}

/// Already in Steve's schema. History never counts as new usage and never drives live state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImportedPage {
    pub events: Vec<NodeEvent>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next: Option<Cursor>,
}

#[cfg(test)]
mod component_tests {
    use super::*;

    #[test]
    fn component_evidence_round_trips() {
        let v = serde_json::json!({"id":"adapter","name":"ACP","package":"pkg","status":"unknown",
            "detected":{"path":"/opt/example/bin/agent","version":"1.2.3"}});
        let c: HarnessComponent = serde_json::from_value(v.clone()).unwrap();
        assert_eq!(c.status, ComponentStatus::Unknown);
        assert_eq!(serde_json::to_value(c).unwrap(), v);
    }
}
