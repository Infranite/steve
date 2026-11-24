//! Group 4 owns events and reasons for devices, nodes, clients and release holds.
//! Associated ID labels (DeviceId, NodeId, ClientId, UserId) are all defined centrally in `id.rs`.

use crate::*;
use serde::Serialize;

reasons! {
    pub enum NodeReason = "node" {
        Downgrade = downgrade [Invalid] (),
        OperationNotFound = operation_not_found [Invalid] (),
        SessionNotOpen = session_not_open [Invalid] (),
        WorkspaceBindingConflict = workspace_binding_conflict [Conflict] (),
        ToolRefused = tool_refused [Internal] (),
    }
    retired { }
}
reasons! {
    pub enum NodeStopReason = "node_stop" {
        Deadline = deadline [Stale] (),
        JobDeadline = job_deadline [Stale] (),
        HubUnreachable = hub_unreachable [Unavailable] (),
        OutputLimit = output_limit [Exhausted] (),
        FrameTooLarge = frame_too_large [Exhausted] (),
        IngressStalled = ingress_stalled [Busy] (secs),
        JournalBudget = journal_budget [Exhausted] (),
        JournalFails = journal_fails [Internal] (),
        NotRecorded = not_recorded [Internal] (),
        HarnessGone = harness_gone [Unavailable] (),
        AdapterExited = adapter_exited [Unavailable] (),
        StoppedByNode = stopped_by_node [Internal] (),
        HarnessIdle = harness_idle [Stale] (),
        ResearchBudget = research_budget [Exhausted] (),
    }
    retired { }
}
pub const REASONS: &[ReasonSet] = &[NodeReason::SET, NodeStopReason::SET];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum NodeOfflineReason {
    AwaitingConfirmation,
    Revoked,
    ProtocolMismatch,
    HandshakeFailed,
    NeverConnected,
    Unreachable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct NodeOffline {
    pub reason: NodeOfflineReason,
    /// Unix milliseconds of the last observation (or when the link became expected).
    pub since: i64,
    pub duration_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum HarnessDegradedReason {
    AuthenticationRequired,
    AdapterMissing,
    InitializeFailed,
    SessionUnavailable,
}

pub const NODE_FILE_ACCESS_SOCKET: &str = "services/node-file-access.sock";
pub const NODE_BOOT_SERVICE_ENV: &str = "STEVE_NODE_BOOT_SERVICE";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum FileAccessStatus {
    Granted,
    Missing,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum ProtectedLocation {
    Desktop,
    Documents,
    Downloads,
    Volume,
    ProtectedLibrary,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct ProtectedAccess {
    pub location: ProtectedLocation,
    pub status: FileAccessStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct FileAccessReport {
    pub status: FileAccessStatus,
    pub locations: Vec<ProtectedAccess>,
}

impl FileAccessReport {
    pub fn unknown() -> Self {
        Self {
            status: FileAccessStatus::Unknown,
            locations: Vec::new(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, strum::EnumCount)]
#[serde(tag = "type")]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum DevicesEvent {
    #[serde(rename = "release.hold")]
    ReleaseHold { hold: Option<RawJson> },
    #[serde(rename = "device")]
    Device { id: DeviceId },
    #[serde(rename = "node")]
    Node { id: NodeId, user: UserId },
    #[serde(rename = "client")]
    Client { id: ClientId, user: UserId },
}

impl Scoped for DevicesEvent {
    fn scope(&self) -> Scope {
        // The owner's devices and presence are installation-wide state on a single-user Hub.
        Scope::Hub
    }
}

impl DevicesEvent {
    /// The public SSE tag for this domain event.
    pub fn wire_type(&self) -> &'static str {
        match self {
            Self::ReleaseHold { .. } => "release.hold",
            Self::Device { .. } => "device",
            Self::Node { .. } => "node",
            Self::Client { .. } => "client",
        }
    }
}

impl DevicesEvent {
    pub fn entity_id(&self) -> Option<&str> {
        match self {
            Self::ReleaseHold { .. } => None,
            Self::Device { id, .. } => Some(id.as_str()),
            Self::Node { id, .. } => Some(id.as_str()),
            Self::Client { id, .. } => Some(id.as_str()),
        }
    }
}

/// Machine observations from the Hub's in-memory telemetry recorder.
#[derive(Debug, Default, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct VitalsResponse {
    pub nodes: std::collections::BTreeMap<String, NodeVitalsDay>,
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct NodeVitalsDay {
    pub latest: NodeVitalsLatest,
    pub series: Vec<NodeVitalsPoint>,
    pub gaps: Vec<VitalsGap>,
    pub sampling: VitalsSampling,
    pub tokens: VitalsTokens,
}

/// `at` is the Hub receipt time. `sampled_at` is the Node's local sample time.
#[derive(Debug, Clone, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct NodeVitalsLatest {
    pub at: i64,
    pub sampled_at: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub cpus: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub load: Option<[f64; 3]>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub mem_total: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub mem_available: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub network: Option<NetworkRate>,
    pub roots: Vec<VitalsRoot>,
    pub sessions: u32,
    pub rtt_ms: Option<i64>,
}

#[derive(Debug, Clone, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct VitalsRoot {
    pub root: String,
    pub display: String,
    pub free: u64,
    pub total: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub inodes_free: Option<u64>,
}

#[derive(Debug, Clone, Copy, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct NodeVitalsPoint {
    pub at: i64,
    pub load: Option<f64>,
    pub mem: Option<f64>,
    pub rtt: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub network: Option<NetworkRate>,
}

#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct NetworkRate {
    pub rx_bytes_per_second: f64,
    pub tx_bytes_per_second: f64,
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct VitalsGap {
    pub from: i64,
    pub to: i64,
}

#[derive(Debug, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct VitalsSampling {
    pub interval_ms: i64,
    pub stale_after_ms: i64,
    pub now: i64,
}

#[derive(Debug, Default, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct VitalsTokens {
    pub per_minute: i64,
    pub today: i64,
}

/// How a harness component binary was installed on a Node, determined from its resolved path.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum ComponentInstallSource {
    SteveManaged,
    HomebrewCask,
    HomebrewFormula,
    NpmGlobal,
    Standalone,
    #[default]
    External,
}

/// Official upgrade mechanism resolved for a component on a Node.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum UpgradeMethod {
    SteveManaged,
    HomebrewCask,
    HomebrewFormula,
    NpmGlobal,
    SelfUpdate,
    #[default]
    Unsupported,
}

/// Version comparison state of one harness component on a Node.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum ComponentUpdateState {
    Unchecked,
    Current,
    Available,
    /// Official self-updater is supported, but the harness provides no separate check-only registry.
    CheckOnUpgrade,
    CheckFailed,
    Unsupported,
    NotInstalled,
}

/// How a per-harness auto-upgrade override relates to the Node-level default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum HarnessAutoUpgradeMode {
    Inherit,
    On,
    Off,
}

/// Hub-owned automatic harness upgrade settings for one Node (owner-only, default off).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, serde::Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct HarnessAutoUpgradePolicy {
    #[serde(default)]
    pub enabled: bool,
    /// Harness id -> explicit override; absent inherits `enabled`.
    #[serde(default)]
    pub harnesses: std::collections::BTreeMap<String, bool>,
}

impl HarnessAutoUpgradePolicy {
    /// Effective auto-upgrade setting for `harness`.
    pub fn effective(&self, harness: &str) -> bool {
        self.harnesses.get(harness).copied().unwrap_or(self.enabled)
    }

    /// Apply a mode change for one harness (`Inherit` removes its explicit override).
    pub fn set_harness_mode(&mut self, harness: String, mode: HarnessAutoUpgradeMode) {
        match mode {
            HarnessAutoUpgradeMode::Inherit => {
                self.harnesses.remove(&harness);
            }
            HarnessAutoUpgradeMode::On => {
                self.harnesses.insert(harness, true);
            }
            HarnessAutoUpgradeMode::Off => {
                self.harnesses.insert(harness, false);
            }
        }
    }
}

/// Request body for `PUT /api/node-harness-updates/{node}/auto-upgrade`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct SetHarnessAutoUpgrade {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub enabled: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub harness: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub mode: Option<HarnessAutoUpgradeMode>,
}

/// Who triggered a harness upgrade run on the Hub.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum HarnessUpgradeTrigger {
    #[default]
    Manual,
    Auto,
}

/// Safe error attached to a finished harness upgrade receipt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct HarnessUpgradeError {
    pub code: String,
    pub safe_detail: String,
}

/// Observed state of a harness upgrade operation receipt over HTTP.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum HarnessUpgradeReceiptState {
    Accepted,
    Running,
    Waiting,
    Unknown {
        reason: String,
    },
    Finished {
        outcome: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[cfg_attr(test, ts(optional))]
        error: Option<HarnessUpgradeError>,
    },
}

/// Operation receipt for one harness upgrade, mirrored from `steve_node_api::Receipt`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct HarnessUpgradeReceipt {
    pub operation: String,
    pub state: HarnessUpgradeReceiptState,
}

/// HTTP view of one harness component's update state on a Node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct HarnessComponentUpdateView {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub package: Option<String>,
    pub managed: bool,
    #[serde(default)]
    pub source: ComponentInstallSource,
    #[serde(default)]
    pub upgrade_method: UpgradeMethod,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub upgrade_command: Option<String>,
    #[serde(default)]
    pub can_upgrade: bool,
    pub state: ComponentUpdateState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub installed: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub latest: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub checked: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub reason: Option<String>,
}

/// HTTP view of one harness's update state on a Node.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct HarnessUpdateView {
    pub harness: String,
    pub name: String,
    pub components: Vec<HarnessComponentUpdateView>,
    pub can_upgrade: bool,
    #[serde(default)]
    pub active_sessions: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub operation: Option<HarnessUpgradeReceipt>,
}

/// HTTP view of a Node's harness update report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct HarnessUpdateReportView {
    pub node: String,
    pub checking: bool,
    pub harnesses: Vec<HarnessUpdateView>,
}

/// Response body of `GET /api/node-harness-updates/{node}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct HarnessUpdatesResponse {
    pub report: Option<HarnessUpdateReportView>,
    pub at: i64,
    pub runs: std::collections::BTreeMap<String, HarnessUpgradeReceipt>,
    #[serde(default)]
    pub retryable_unknown: Vec<String>,
    #[serde(default)]
    pub auto_upgrade: HarnessAutoUpgradePolicy,
    pub error: Option<String>,
    pub code: Option<String>,
}

/// Response body of `POST /api/node-harness-updates/{node}/{harness}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct UpgradeHarnessResponse {
    pub receipt: HarnessUpgradeReceipt,
}
