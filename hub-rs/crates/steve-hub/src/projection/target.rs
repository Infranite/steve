//! Which view an event makes dirty. Every variant is listed, so a new event has to say.

use steve_types::{
    AutomationEvent, AutonomyEvent, ChannelsEvent, ConfigEvent, DevicesEvent, DomainEvent,
    IntegrationsEvent, KnowledgeEvent, PeopleEvent, StatsEvent, VaultEvent, ViewKind, WorkEvent,
};

pub(super) enum Dirty<'a> {
    One(&'a str, ViewKind, &'a str),
    None,
}

pub(super) fn target(e: &DomainEvent) -> Dirty<'_> {
    match e {
        DomainEvent::Work(e) => match e {
            WorkEvent::Decision { id, ws, .. } => {
                Dirty::One(ws.as_str(), ViewKind::Decision, id.as_str())
            }
            WorkEvent::Item { .. }
            | WorkEvent::Session { .. }
            | WorkEvent::Message { .. }
            | WorkEvent::ThreadRun { .. } => Dirty::None,
        },
        DomainEvent::Config(e) => match e {
            ConfigEvent::Bot { .. }
            | ConfigEvent::Project { .. }
            | ConfigEvent::Tag { .. }
            | ConfigEvent::Workspace { .. }
            | ConfigEvent::Settings { .. }
            | ConfigEvent::Mm { .. }
            | ConfigEvent::Changeset { .. } => Dirty::None,
        },
        DomainEvent::Devices(e) => match e {
            DevicesEvent::ReleaseHold { .. }
            | DevicesEvent::Device { .. }
            | DevicesEvent::Node { .. }
            | DevicesEvent::Client { .. } => Dirty::None,
        },
        DomainEvent::Channels(e) => match e {
            ChannelsEvent::Connection { .. }
            | ChannelsEvent::Space { .. }
            | ChannelsEvent::ChanMessage { .. }
            | ChannelsEvent::Persona { .. }
            | ChannelsEvent::Digest { .. }
            | ChannelsEvent::Channel { .. } => Dirty::None,
        },
        DomainEvent::People(e) => match e {
            PeopleEvent::Party { .. } | PeopleEvent::Account { .. } => Dirty::None,
        },
        DomainEvent::Vault(e) => match e {
            VaultEvent::Vault { .. } => Dirty::None,
        },
        DomainEvent::Automation(e) => match e {
            AutomationEvent::Signal { .. }
            | AutomationEvent::Flow { .. }
            | AutomationEvent::Run { .. }
            | AutomationEvent::Calendar { .. } => Dirty::None,
        },
        DomainEvent::Autonomy(e) => match e {
            AutonomyEvent::Yolo { .. } | AutonomyEvent::YoloAction { .. } => Dirty::None,
        },
        DomainEvent::Stats(e) => match e {
            StatsEvent::Stats { .. } => Dirty::None,
        },
        DomainEvent::Knowledge(e) => match e {
            KnowledgeEvent::Kb { .. } => Dirty::None,
        },
        DomainEvent::Integrations(e) => match e {
            IntegrationsEvent::Gh { .. } | IntegrationsEvent::Mail { .. } => Dirty::None,
        },
    }
}
