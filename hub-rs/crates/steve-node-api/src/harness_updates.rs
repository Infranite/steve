//! Optional, Node-owned component maintenance. No paths, packages, commands or credentials are
//! accepted from the Hub. A successful receipt means the requested versions were read back.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{NodeId, Receipt, Timestamp};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HarnessUpdateReport {
    pub node: NodeId,
    /// A single bounded registry check runs in the background. Poll without `fresh` to observe it.
    pub checking: bool,
    pub harnesses: Vec<HarnessUpdateInfo>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HarnessUpdateInfo {
    pub harness: String,
    pub name: String,
    pub components: Vec<HarnessComponentUpdate>,
    pub can_upgrade: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Last explicit maintenance receipt, including failure/unknown, from the Node's journal.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub operation: Option<Receipt>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HarnessComponentUpdate {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub package: Option<String>,
    pub managed: bool,
    pub state: ComponentUpdateState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentUpdateState {
    Unchecked,
    Current,
    Available,
    CheckFailed,
    Unsupported,
    NotInstalled,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpgradeHarness {
    pub harness: String,
    /// Component id → exact version from this Node's check; never npm package specifications.
    pub versions: BTreeMap<String, String>,
}
