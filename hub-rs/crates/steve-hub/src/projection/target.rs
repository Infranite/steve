//! Which view an event makes dirty. Every variant is listed, so a new event has to say.

use steve_types::{
    AutomationEvent, AutonomyEvent, ChannelsEvent, ConfigEvent, DevicesEvent, DomainEvent,
    IntegrationsEvent, KnowledgeEvent, PeopleEvent, StatsEvent, VaultEvent, ViewKind, WorkEvent,
};

pub(super) enum Dirty<'a> {
    One(&'a str, ViewKind, &'a str),
    /// The decisions about a conversation: a message in space `.1` of workspace `.0` (whether you
    /// spoke there since is part of their tier).
    Chat(&'a str, &'a str),
    /// The decisions about every conversation of a workspace: a contact (starred or not), an
    /// account or a connection changed.
    Chats(&'a str),
    SessionsOfNode(&'a str),
    None,
}

pub(super) fn target(e: &DomainEvent) -> Dirty<'_> {
    match e {
        DomainEvent::Work(e) => match e {
            WorkEvent::Decision { id, ws, .. } => {
                Dirty::One(ws.as_str(), ViewKind::Decision, id.as_str())
            }
            WorkEvent::Item { id, ws } => Dirty::One(ws.as_str(), ViewKind::Item, id.as_str()),
            WorkEvent::Session { id, ws, .. } => {
                Dirty::One(ws.as_str(), ViewKind::Session, id.as_str())
            }
            WorkEvent::Message { .. } | WorkEvent::ThreadRun { .. } => Dirty::None,
        },
        DomainEvent::Config(e) => match e {
            ConfigEvent::Workspace { ws, .. } => Dirty::Chats(ws.as_str()),
            ConfigEvent::Bot { .. }
            | ConfigEvent::Project { .. }
            | ConfigEvent::Tag { .. }
            | ConfigEvent::Settings { .. }
            | ConfigEvent::Mm { .. }
            | ConfigEvent::Changeset { .. } => Dirty::None,
        },
        DomainEvent::Devices(e) => match e {
            DevicesEvent::Node { id, .. } => Dirty::SessionsOfNode(id.as_str()),
            DevicesEvent::Device { id } if id.as_str() == steve_store::runtime::HUB_DEVICE => {
                Dirty::SessionsOfNode(steve_store::runtime::HUB_NODE)
            }
            DevicesEvent::ReleaseHold { .. }
            | DevicesEvent::Device { .. }
            | DevicesEvent::Client { .. } => Dirty::None,
        },
        DomainEvent::Channels(e) => match e {
            ChannelsEvent::ChanMessage { space_id, ws, .. } => {
                Dirty::Chat(ws.as_str(), space_id.as_str())
            }
            ChannelsEvent::Connection { ws, .. }
            | ChannelsEvent::Space { ws, .. }
            | ChannelsEvent::Channel { ws, .. } => Dirty::Chats(ws.as_str()),
            ChannelsEvent::Persona { .. } | ChannelsEvent::Digest { .. } => Dirty::None,
        },
        DomainEvent::People(e) => match e {
            PeopleEvent::Party { ws, .. } | PeopleEvent::Account { ws, .. } => {
                Dirty::Chats(ws.as_str())
            }
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
