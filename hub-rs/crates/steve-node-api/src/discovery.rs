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
    /// Guarantees it can enforce (`fs-readonly`, `no-network`…), Steve's names.
    pub enforcement: Vec<String>,
    /// What its adapter is and said about itself (`initialize`), when the Node asked it. Older
    /// Nodes send none: their two built-in harnesses as they always were.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub adapter: Option<AdapterInfo>,
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
    /// Node could not open one (`session_error`) or is older.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session: Option<AdapterOffer>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_error: Option<String>,
    /// An adapter Steve installed and keeps up to date (`<home>/adapters`): what its last check of
    /// the npm registry found. `None` for one installed otherwise, or a Node that is older.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub update: Option<AdapterUpdate>,
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
/// Strict artifact reads bound to an accepted execution and its recorded view.
pub const FEATURE_ARTIFACT_READ: &str = "artifact-read-v1";

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
