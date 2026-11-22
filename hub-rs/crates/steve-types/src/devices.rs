//! Group 4 owns events and reasons for devices, nodes, clients and release holds.
//! Associated ID labels (DeviceId, NodeId, ClientId, UserId) are all defined centrally in `id.rs`.

use crate::*;
use serde::Serialize;

pub const REASONS: &[ReasonSet] = &[];

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
