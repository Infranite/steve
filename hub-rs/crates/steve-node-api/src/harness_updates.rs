//! Optional, Node-owned component maintenance. No paths, packages, commands or credentials are
//! accepted from the Hub. A successful receipt means the requested versions were read back.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
pub use steve_types::devices::{
    ComponentInstallSource, ComponentUpdateReason, ComponentUpdateState, HarnessUpgradeBlockReason,
    UpgradeMethod,
};

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
    /// Active sessions currently running on this Node using this harness.
    #[serde(default)]
    pub active_sessions: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_reason: Option<HarnessUpgradeBlockReason>,
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
    #[serde(default)]
    pub source: ComponentInstallSource,
    #[serde(default)]
    pub upgrade_method: UpgradeMethod,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upgrade_command: Option<String>,
    #[serde(default)]
    pub can_upgrade: bool,
    pub state: ComponentUpdateState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub installed: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub checked: Option<Timestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<ComponentUpdateReason>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UpgradeHarness {
    pub harness: String,
    /// Component id → exact version from this Node's check; never npm package specifications.
    #[serde(default)]
    pub versions: BTreeMap<String, String>,
    /// Component ids whose verified official self-updater runs without a pre-checked registry version.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub self_update: Vec<String>,
}
