//! Cache dependencies, at the same synchronous publication boundary as state revisions.
use crate::bus::BusEvent;
use steve_runtime::cache::{Tag, Tags};
use steve_types::*;

pub fn tags(event: &BusEvent) -> Tags {
    let mut tags = Vec::new();
    match event {
        BusEvent::Domain(event) => match event {
            DomainEvent::Stats(StatsEvent::Stats { ws, .. }) => {
                tags.push(Tag::of("stats", ws.as_str()));
                tags.push(Tag::global("stats-events"));
            }
            DomainEvent::Devices(event) => match event {
                DevicesEvent::Node { id, .. } => {
                    tags.push(Tag::of("node", id.as_str()));
                    tags.push(Tag::of("runtimes", id.as_str()));
                }
                DevicesEvent::Device { .. } => tags.push(Tag::global("runtimes")),
                DevicesEvent::ReleaseHold { .. } | DevicesEvent::Client { .. } => {}
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
            DomainEvent::Config(event) => match event {
                ConfigEvent::Bot { .. }
                | ConfigEvent::Project { .. }
                | ConfigEvent::Tag { .. }
                | ConfigEvent::Workspace { .. }
                | ConfigEvent::Settings { .. }
                | ConfigEvent::Mm { .. }
                | ConfigEvent::Changeset { .. } => {}
            },
            DomainEvent::Work(event) => match event {
                WorkEvent::Item { .. }
                | WorkEvent::Decision { .. }
                | WorkEvent::Session { .. }
                | WorkEvent::Message { .. } => {}
            },
            DomainEvent::People(event) => match event {
                PeopleEvent::Party { .. } | PeopleEvent::Account { .. } => {}
            },
            DomainEvent::Vault(event) => match event {
                VaultEvent::Vault { .. } => {}
            },
            DomainEvent::Automation(event) => match event {
                AutomationEvent::Signal { .. }
                | AutomationEvent::Flow { .. }
                | AutomationEvent::Run { .. }
                | AutomationEvent::Calendar { .. } => {}
            },
            DomainEvent::Autonomy(event) => match event {
                AutonomyEvent::ApprovalPolicy { .. } => {}
            },
            DomainEvent::Knowledge(event) => match event {
                KnowledgeEvent::Kb { .. } => {}
            },
            DomainEvent::Integrations(event) => match event {
                IntegrationsEvent::Gh { .. } | IntegrationsEvent::Mail { .. } => {}
            },
        },
        BusEvent::Live(LiveEvent::Work(event)) => match event {
            WorkLiveEvent::Event { .. }
            | WorkLiveEvent::SteveDraft { .. }
            | WorkLiveEvent::SteveTool { .. }
            | WorkLiveEvent::SteveThinking { .. } => {}
        },
        BusEvent::Patch(_) => {}
    }
    tags
}

/// The complete process cache catalogue. Startup and preflight share this validation.
pub(crate) const SPACES: &[(&str, u8)] = &[
    crate::access::CACHE_SPEC,
    crate::system::CACHE_SPEC,
    crate::api::stats::CACHE_SPEC,
    crate::api::NODE_CACHE_SPEC,
    crate::wire::library_mcp::CACHE_SPECS[0],
    crate::wire::library_mcp::CACHE_SPECS[1],
    steve_github::CACHE_SPACES[0],
    steve_github::CACHE_SPACES[1],
];
pub(crate) fn validate_spaces(spaces: &[(&str, u8)]) -> Result<(), crate::config::ConfigError> {
    let mut names = std::collections::HashSet::new();
    let mut share = 0u16;
    for (name, pct) in spaces {
        if !names.insert(*name) || *pct == 0 {
            return Err(crate::config::ConfigError::Invalid {
                var: "STEVE_CACHE",
                msg: "duplicate cache space or zero share".into(),
            });
        }
        share += u16::from(*pct);
    }
    if share > 100 {
        return Err(crate::config::ConfigError::Invalid {
            var: "STEVE_CACHE",
            msg: "cache space shares exceed 100 percent".into(),
        });
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn a_node_change_does_not_revoke_another_nodes_report() {
        let caches = steve_runtime::cache::Caches::new(Default::default());
        let event = BusEvent::Domain(DomainEvent::Devices(DevicesEvent::Node {
            id: "n_a".into(),
            user: "owner".into(),
        }));
        caches.invalidate(&tags(&event));
        assert_eq!(caches.generation(&Tag::of("runtimes", "n_a")), 1);
        assert_eq!(caches.generation(&Tag::of("runtimes", "n_b")), 0);
    }
    #[test]
    fn all_spaces_register_from_the_startup_catalogue() {
        validate_spaces(SPACES).unwrap();
        assert_eq!(SPACES.len(), 8);
        let caches = std::sync::Arc::new(steve_runtime::cache::Caches::new(Default::default()));
        crate::access::register_cache_spaces(&caches);
        crate::system::register_cache_spaces(&caches);
        crate::api::stats::register_cache_spaces(&caches);
        crate::api::register_node_cache_spaces(&caches);
        crate::wire::library_mcp::register_cache_spaces(&caches);
        steve_github::register_cache_spaces_for_test(caches);
        assert!(validate_spaces(&[("same", 1), ("same", 1)]).is_err());
        assert!(validate_spaces(&[("a", 60), ("b", 60)]).is_err());
    }
}
