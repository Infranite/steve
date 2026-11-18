//! Typed bus envelopes and domain dispatch; revisions are data, never SSE cursors.

use crate::{Rev, UserId, WorkspaceId};
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
    User(UserId),
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

/// Migration placeholder: every invocation warns, then panics if accidentally executed.
#[deprecated(note = "W1.3b: fill the workspace from the write transaction")]
pub fn ws_todo() -> WorkspaceId {
    panic!("W1.3b: fill the workspace from the write transaction")
}
