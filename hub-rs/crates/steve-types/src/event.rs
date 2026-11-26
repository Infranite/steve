//! Typed bus envelopes and domain dispatch; revisions are data, never SSE cursors.

use crate::{Rev, WorkspaceId};
use serde::Serialize;

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct Envelope<E> {
    #[serde(flatten)]
    pub event: E,
    /// Data, never the SSE cursor: frames of one transaction share it and live events reuse it.
    pub rev: Rev,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum Scope {
    Ws(WorkspaceId),
    Hub,
}

pub trait Scoped {
    fn scope(&self) -> Scope;
}

impl<E: Scoped> Scoped for Envelope<E> {
    fn scope(&self) -> Scope {
        self.event.scope()
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, strum::EnumCount)]
#[serde(untagged)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum DomainEvent {
    Work(crate::WorkEvent),
    Config(crate::ConfigEvent),
    Devices(crate::DevicesEvent),
    Channels(crate::ChannelsEvent),
    People(crate::PeopleEvent),
    Vault(crate::VaultEvent),
    Automation(crate::AutomationEvent),
    Autonomy(crate::AutonomyEvent),
    Stats(crate::StatsEvent),
    Knowledge(crate::KnowledgeEvent),
    Integrations(crate::IntegrationsEvent),
}
impl Scoped for DomainEvent {
    fn scope(&self) -> Scope {
        match self {
            Self::Work(event) => event.scope(),
            Self::Config(event) => event.scope(),
            Self::Devices(event) => event.scope(),
            Self::Channels(event) => event.scope(),
            Self::People(event) => event.scope(),
            Self::Vault(event) => event.scope(),
            Self::Automation(event) => event.scope(),
            Self::Autonomy(event) => event.scope(),
            Self::Stats(event) => event.scope(),
            Self::Knowledge(event) => event.scope(),
            Self::Integrations(event) => event.scope(),
        }
    }
}
impl From<crate::WorkEvent> for DomainEvent {
    fn from(event: crate::WorkEvent) -> Self {
        Self::Work(event)
    }
}
impl From<crate::ConfigEvent> for DomainEvent {
    fn from(event: crate::ConfigEvent) -> Self {
        Self::Config(event)
    }
}
impl From<crate::DevicesEvent> for DomainEvent {
    fn from(event: crate::DevicesEvent) -> Self {
        Self::Devices(event)
    }
}
impl From<crate::ChannelsEvent> for DomainEvent {
    fn from(event: crate::ChannelsEvent) -> Self {
        Self::Channels(event)
    }
}
impl From<crate::PeopleEvent> for DomainEvent {
    fn from(event: crate::PeopleEvent) -> Self {
        Self::People(event)
    }
}
impl From<crate::VaultEvent> for DomainEvent {
    fn from(event: crate::VaultEvent) -> Self {
        Self::Vault(event)
    }
}
impl From<crate::AutomationEvent> for DomainEvent {
    fn from(event: crate::AutomationEvent) -> Self {
        Self::Automation(event)
    }
}
impl From<crate::AutonomyEvent> for DomainEvent {
    fn from(event: crate::AutonomyEvent) -> Self {
        Self::Autonomy(event)
    }
}
impl From<crate::StatsEvent> for DomainEvent {
    fn from(event: crate::StatsEvent) -> Self {
        Self::Stats(event)
    }
}
impl From<crate::KnowledgeEvent> for DomainEvent {
    fn from(event: crate::KnowledgeEvent) -> Self {
        Self::Knowledge(event)
    }
}
impl From<crate::IntegrationsEvent> for DomainEvent {
    fn from(event: crate::IntegrationsEvent) -> Self {
        Self::Integrations(event)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, strum::EnumCount)]
#[serde(untagged)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum LiveEvent {
    Work(crate::WorkLiveEvent),
}
impl Scoped for LiveEvent {
    fn scope(&self) -> Scope {
        match self {
            Self::Work(event) => event.scope(),
        }
    }
}
impl From<crate::WorkLiveEvent> for LiveEvent {
    fn from(event: crate::WorkLiveEvent) -> Self {
        Self::Work(event)
    }
}

impl DomainEvent {
    /// Stable SSE tag, used without inspecting serialized JSON.
    pub fn wire_type(&self) -> &'static str {
        match self {
            Self::Work(event) => event.wire_type(),
            Self::Devices(event) => event.wire_type(),
            Self::Config(event) => event.wire_type(),
            Self::People(event) => event.wire_type(),
            Self::Channels(event) => event.wire_type(),
            Self::Vault(event) => event.wire_type(),
            Self::Automation(event) => event.wire_type(),
            Self::Autonomy(event) => event.wire_type(),
            Self::Stats(event) => event.wire_type(),
            Self::Knowledge(event) => event.wire_type(),
            Self::Integrations(event) => event.wire_type(),
        }
    }
}

impl LiveEvent {
    /// Stable SSE tag, used without inspecting serialized JSON.
    pub fn wire_type(&self) -> &'static str {
        match self {
            Self::Work(event) => event.wire_type(),
        }
    }
}

/// One typed publication. Domain and live events share ordering through the bus and its taps.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(untagged)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum BusEvent {
    Domain(DomainEvent),
    Live(LiveEvent),
    /// What changed in a workspace's views, published by the Hub's projection (never by a store
    /// write): it takes the revision it is as of, not a new one.
    Patch(crate::Patch),
}
impl From<DomainEvent> for BusEvent {
    fn from(event: DomainEvent) -> Self {
        Self::Domain(event)
    }
}
impl From<LiveEvent> for BusEvent {
    fn from(event: LiveEvent) -> Self {
        Self::Live(event)
    }
}
impl Scoped for BusEvent {
    fn scope(&self) -> Scope {
        match self {
            Self::Domain(e) => e.scope(),
            Self::Live(e) => e.scope(),
            Self::Patch(p) => p.scope(),
        }
    }
}
impl BusEvent {
    pub fn as_domain(&self) -> Option<&DomainEvent> {
        match self {
            Self::Domain(e) => Some(e),
            Self::Live(_) | Self::Patch(_) => None,
        }
    }
    pub fn as_live(&self) -> Option<&LiveEvent> {
        match self {
            Self::Live(e) => Some(e),
            Self::Domain(_) | Self::Patch(_) => None,
        }
    }
    pub fn wire_type(&self) -> &'static str {
        match self {
            Self::Domain(e) => e.wire_type(),
            Self::Live(e) => e.wire_type(),
            Self::Patch(_) => "patch",
        }
    }
    /// The same logical key as the wire notice: transcript entries must never coalesce.
    pub fn coalesce_key(&self) -> Option<(&'static str, Option<WorkspaceId>, String)> {
        if matches!(
            self,
            Self::Live(crate::LiveEvent::Work(crate::WorkLiveEvent::Event { .. })) | Self::Patch(_)
        ) {
            return None;
        }
        let ws = match self.scope() {
            Scope::Ws(ws) => Some(ws),
            _ => None,
        };
        let id = match self {
            Self::Domain(e) => e.entity_id().unwrap_or_default(),
            Self::Live(_) | Self::Patch(_) => "",
        };
        Some((self.wire_type(), ws, id.to_owned()))
    }
}

impl DomainEvent {
    pub fn entity_id(&self) -> Option<&str> {
        match self {
            Self::Work(event) => event.entity_id(),
            Self::Devices(event) => event.entity_id(),
            Self::Config(event) => event.entity_id(),
            Self::People(event) => event.entity_id(),
            Self::Channels(event) => event.entity_id(),
            Self::Vault(event) => event.entity_id(),
            Self::Automation(event) => event.entity_id(),
            Self::Autonomy(event) => event.entity_id(),
            Self::Stats(event) => event.entity_id(),
            Self::Knowledge(event) => event.entity_id(),
            Self::Integrations(event) => event.entity_id(),
        }
    }
}

/// Shared immutable notification delivered to internal subscribers before boundary encoding.
pub type Notification = std::sync::Arc<Envelope<BusEvent>>;
impl From<crate::WorkEvent> for BusEvent {
    fn from(event: crate::WorkEvent) -> Self {
        Self::Domain(event.into())
    }
}
impl From<crate::ConfigEvent> for BusEvent {
    fn from(event: crate::ConfigEvent) -> Self {
        Self::Domain(event.into())
    }
}
impl From<crate::DevicesEvent> for BusEvent {
    fn from(event: crate::DevicesEvent) -> Self {
        Self::Domain(event.into())
    }
}
impl From<crate::ChannelsEvent> for BusEvent {
    fn from(event: crate::ChannelsEvent) -> Self {
        Self::Domain(event.into())
    }
}
impl From<crate::PeopleEvent> for BusEvent {
    fn from(event: crate::PeopleEvent) -> Self {
        Self::Domain(event.into())
    }
}
impl From<crate::VaultEvent> for BusEvent {
    fn from(event: crate::VaultEvent) -> Self {
        Self::Domain(event.into())
    }
}
impl From<crate::AutomationEvent> for BusEvent {
    fn from(event: crate::AutomationEvent) -> Self {
        Self::Domain(event.into())
    }
}
impl From<crate::AutonomyEvent> for BusEvent {
    fn from(event: crate::AutonomyEvent) -> Self {
        Self::Domain(event.into())
    }
}
impl From<crate::StatsEvent> for BusEvent {
    fn from(event: crate::StatsEvent) -> Self {
        Self::Domain(event.into())
    }
}
impl From<crate::KnowledgeEvent> for BusEvent {
    fn from(event: crate::KnowledgeEvent) -> Self {
        Self::Domain(event.into())
    }
}
impl From<crate::IntegrationsEvent> for BusEvent {
    fn from(event: crate::IntegrationsEvent) -> Self {
        Self::Domain(event.into())
    }
}
impl From<crate::WorkLiveEvent> for BusEvent {
    fn from(event: crate::WorkLiveEvent) -> Self {
        Self::Live(event.into())
    }
}

/// Every SSE wire type a bus event can have, one per variant across all domain and live sub-enums.
/// Tests on both sides of the wire iterate this list (steve-types `tests/wire.rs` checks it against the
/// variant counts), so a new variant cannot be missed by one of them.
pub const WIRE_TYPES: [&str; 39] = [
    "item",
    "decision",
    "session",
    "event",
    "message",
    "steve.thinking",
    "steve.draft",
    "steve.tool",
    "workflow.run",
    "release.hold",
    "local_node.updating",
    "bot",
    "project",
    "tag",
    "workspace",
    "settings",
    "device",
    "node",
    "client",
    "party",
    "mm",
    "changeset",
    "account",
    "connection",
    "vault",
    "signal",
    "flow",
    "run",
    "space",
    "chan_message",
    "persona",
    "digest",
    "calendar",
    "channel",
    "approval_policy",
    "stats",
    "kb",
    "gh",
    "mail",
];
