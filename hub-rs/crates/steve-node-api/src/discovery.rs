//! What a Node can do and what harness history it has.

use serde::{Deserialize, Serialize};

use crate::events::NodeEvent;
use crate::ids::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessCapability {
    /// `claude-code`, `codex`.
    pub name: String,
    pub version: String,
    /// Sessions (multi-turn, permissions) and/or jobs (one-shot).
    pub sessions: bool,
    pub jobs: bool,
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
