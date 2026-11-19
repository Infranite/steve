//! Cache dependencies, at the same synchronous publication boundary as state revisions.
use crate::bus::BusEvent;
use steve_runtime::cache::{Tag, Tags};
use steve_types::*;

pub fn scope(scope: &Scope) -> Tag {
    match scope {
        Scope::Ws(ws) => Tag::of("state", ws.as_str()),
        Scope::Hub | Scope::User(_) => Tag::global("state"),
    }
}
pub fn state_event(event: &BusEvent) -> bool {
    match event {
        BusEvent::Domain(event) => domain_state(event),
        BusEvent::Live(LiveEvent::Work(event)) => match event {
            WorkLiveEvent::Event { .. }
            | WorkLiveEvent::SteveDraft { .. }
            | WorkLiveEvent::SteveTool { .. } => false,
            WorkLiveEvent::SteveThinking { .. } => true,
        },
    }
}
fn domain_state(event: &DomainEvent) -> bool {
    match event {
        DomainEvent::Work(event) => match event {
            WorkEvent::Item { .. } => true,
            WorkEvent::Decision { .. } => true,
            WorkEvent::Session { .. } => true,
            WorkEvent::Message { .. } => true,
            WorkEvent::ThreadRun { .. } => true,
        },
        DomainEvent::Config(event) => match event {
            ConfigEvent::Bot { .. } => true,
            ConfigEvent::Project { .. } => true,
            ConfigEvent::Tag { .. } => true,
            ConfigEvent::Workspace { .. } => true,
            ConfigEvent::Settings { .. } => true,
            ConfigEvent::Mm { .. } => true,
            ConfigEvent::Changeset { .. } => true,
        },
        DomainEvent::Devices(event) => match event {
            DevicesEvent::ReleaseHold { .. } => true,
            DevicesEvent::Device { .. } => true,
            DevicesEvent::Node { .. } => true,
            DevicesEvent::Client { .. } => false,
        },
        DomainEvent::Channels(event) => match event {
            ChannelsEvent::Connection { .. } => true,
            ChannelsEvent::Space { .. } => true,
            ChannelsEvent::ChanMessage { .. } => true,
            ChannelsEvent::Persona { .. } => true,
            ChannelsEvent::Digest { .. } => true,
            ChannelsEvent::Channel { .. } => true,
        },
        DomainEvent::People(event) => match event {
            PeopleEvent::Party { .. } => true,
            PeopleEvent::Account { .. } => true,
        },
        DomainEvent::Vault(event) => match event {
            VaultEvent::Vault { .. } => true,
        },
        DomainEvent::Automation(event) => match event {
            AutomationEvent::Signal { .. } => true,
            AutomationEvent::Flow { .. } => true,
            AutomationEvent::Run { .. } => true,
            AutomationEvent::Calendar { .. } => true,
        },
        DomainEvent::Autonomy(event) => match event {
            AutonomyEvent::Yolo { .. } => true,
            AutonomyEvent::YoloAction { .. } => true,
        },
        DomainEvent::Stats(event) => match event {
            StatsEvent::Stats { .. } => false,
        },
        DomainEvent::Knowledge(event) => match event {
            KnowledgeEvent::Kb { .. } => true,
        },
        DomainEvent::Integrations(event) => match event {
            IntegrationsEvent::Gh { .. } => true,
            IntegrationsEvent::Mail { .. } => true,
        },
    }
}
pub fn tags(event: &BusEvent) -> Tags {
    let mut tags = Vec::new();
    if state_event(event) {
        tags.push(scope(&event.scope()));
        tags.push(Tag::global("state-events"));
    }
    match event {
        BusEvent::Domain(event) => match event {
            DomainEvent::Stats(StatsEvent::Stats { ws, .. }) => {
                tags.push(Tag::of("stats", ws.as_str()));
                tags.push(Tag::global("stats-events"));
            }
            DomainEvent::Devices(event) => match event {
                DevicesEvent::Node { id, .. } => {
                    tags.push(Tag::of("node", id.as_str()));
                    tags.push(Tag::global("hub"));
                }
                DevicesEvent::Device { .. } | DevicesEvent::ReleaseHold { .. } => {
                    tags.push(Tag::global("node"))
                }
                DevicesEvent::Client { .. } => tags.push(Tag::global("hub")),
            },
            DomainEvent::Channels(event) => match event {
                ChannelsEvent::Connection { id, .. } | ChannelsEvent::Channel { id, .. } => {
                    tags.push(Tag::of("connection", id.as_str()))
                }
                ChannelsEvent::Space { .. }
                | ChannelsEvent::ChanMessage { .. }
                | ChannelsEvent::Persona { .. }
                | ChannelsEvent::Digest { .. } => {}
            },
            DomainEvent::Config(_) => tags.push(Tag::global("hub")),
            // Vault use-audit events can be emitted by a cache loader itself. Server edits
            // publish Config::Bot and carry server.updated; audit writes are not metadata edits.
            DomainEvent::Vault(_) => {}
            DomainEvent::Work(_)
            | DomainEvent::People(_)
            | DomainEvent::Automation(_)
            | DomainEvent::Autonomy(_)
            | DomainEvent::Knowledge(_)
            | DomainEvent::Integrations(_) => {}
        },
        BusEvent::Live(LiveEvent::Work(_)) => {}
    }
    tags
}
