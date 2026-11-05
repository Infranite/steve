//! What a Node can do and what harness history it has.

use serde::{Deserialize, Serialize};

use crate::events::NodeEvent;
use crate::ids::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessCapability {
    /// `claude-code`, `codex`.
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

/// The Node writes a secret to a temporary file for a command ([`crate::SecretDelivery::File`]).
/// Only the Hub's own machine offers it; a remote Node's claim is dropped.
pub const FEATURE_SECRET_FILE: &str = "secret-file";

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
}

/// Where a history entry is, for people and for resuming. Only a key the Node listed itself is
/// located; any other (a key made up by a client) is refused.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryLocation {
    pub source: HistoryRef,
    /// What to show ("~/.claude/projects/…/abc.jsonl"), never an address.
    pub display: String,
    /// The harness session it continues, when the harness can resume it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume: Option<ResumeHandle>,
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
