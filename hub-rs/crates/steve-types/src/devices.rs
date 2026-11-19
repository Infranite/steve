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
        match self {
            Self::ReleaseHold { .. } => Scope::Hub,
            Self::Device { .. } => Scope::Hub,
            Self::Node { user, .. } => Scope::User(user.clone()),
            Self::Client { user, .. } => Scope::User(user.clone()),
        }
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
