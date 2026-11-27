//! One explicitly constructed event for each public wire tag. No event deserialization.
use steve_store::BusEvent;
use steve_types::*;
pub fn typed(ty: &str, ws: &str) -> BusEvent {
    match ty {
        "item" => DomainEvent::from(WorkEvent::Item {
            id: "entity".into(),
            ws: ws.into(),
        })
        .into(),
        "decision" => DomainEvent::from(WorkEvent::Decision {
            id: "entity".into(),
            item_id: None,
            ws: ws.into(),
        })
        .into(),
        "session" => DomainEvent::from(WorkEvent::Session {
            id: "entity".into(),
            item_id: None,
            ws: ws.into(),
        })
        .into(),
        "event" => LiveEvent::from(WorkLiveEvent::Event {
            session_id: "entity".into(),
            seq: Seq(1),
            kind: "entity".into(),
            data: RawJson::from_str(r#"{"text":"hello"}"#).unwrap(),
            ws: ws.into(),
        })
        .into(),
        "message" => DomainEvent::from(WorkEvent::Message {
            space: "notify".into(),
            id: "entity".into(),
            ws: ws.into(),
        })
        .into(),
        "steve.thinking" => LiveEvent::from(WorkLiveEvent::SteveThinking {
            on: true,
            ws: ws.into(),
            phase: None,
            started_at: None,
            at: None,
        })
        .into(),
        "steve.draft" => LiveEvent::from(WorkLiveEvent::SteveDraft {
            ws: ws.into(),
            mid: "entity".into(),
            thought: "entity".into(),
            text: "entity".into(),
        })
        .into(),
        "steve.tool" => LiveEvent::from(WorkLiveEvent::SteveTool {
            ws: ws.into(),
            mid: "entity".into(),
            run: "entity".into(),
            name: "entity".into(),
            brief: "entity".into(),
            ok: true,
            kind: "entity".into(),
            running: true,
            call: None,
            started_at: None,
            at: None,
            description: String::new(),
            step_started_at: None,
            ms: None,
            excerpt: None,
            error: None,
        })
        .into(),
        "steve.progress" => LiveEvent::from(WorkLiveEvent::SteveProgress {
            ws: ws.into(),
            mid: "entity".into(),
            run: "entity".into(),
            started_at: None,
            at: None,
            body: steve_types::ThreadProgressKind::Stage {
                stage: steve_types::ThreadResearchStage::Thinking,
            },
        })
        .into(),
        "workflow.run" => DomainEvent::from(WorkEvent::WorkflowRun {
            id: "entity".into(),
            item_id: "item-1".into(),
            ws: ws.into(),
        })
        .into(),
        "release.hold" => DomainEvent::from(DevicesEvent::ReleaseHold { hold: None }).into(),
        "bot" => DomainEvent::from(ConfigEvent::Bot {
            id: "entity".into(),
            ws: ws.into(),
        })
        .into(),
        "project" => DomainEvent::from(ConfigEvent::Project {
            id: "entity".into(),
            ws: ws.into(),
        })
        .into(),
        "tag" => DomainEvent::from(ConfigEvent::Tag {
            id: "entity".into(),
            ws: ws.into(),
        })
        .into(),
        "workspace" => DomainEvent::from(ConfigEvent::Workspace {
            id: "entity".into(),
            ws: ws.into(),
        })
        .into(),
        "settings" => DomainEvent::from(ConfigEvent::Settings {
            id: "entity".into(),
            ws: ws.into(),
        })
        .into(),
        "device" => DomainEvent::from(DevicesEvent::Device {
            id: "entity".into(),
        })
        .into(),
        "node" => DomainEvent::from(DevicesEvent::Node {
            id: "entity".into(),
            user: "user".into(),
        })
        .into(),
        "client" => DomainEvent::from(DevicesEvent::Client {
            id: "entity".into(),
            user: "user".into(),
        })
        .into(),
        "party" => DomainEvent::from(PeopleEvent::Party {
            id: "entity".into(),
            ws: ws.into(),
        })
        .into(),
        "mm" => DomainEvent::from(ConfigEvent::Mm {
            id: "entity".into(),
            ws: ws.into(),
        })
        .into(),
        "changeset" => DomainEvent::from(ConfigEvent::Changeset {
            id: "entity".into(),
            ws: ws.into(),
        })
        .into(),
        "account" => DomainEvent::from(PeopleEvent::Account {
            id: "entity".into(),
            ws: ws.into(),
        })
        .into(),
        "connection" => DomainEvent::from(ChannelsEvent::Connection {
            id: "entity".into(),
            ws: ws.into(),
        })
        .into(),
        "vault" => DomainEvent::from(VaultEvent::Vault {
            id: "entity".into(),
            ws: ws.into(),
        })
        .into(),
        "signal" => DomainEvent::from(AutomationEvent::Signal {
            id: "entity".into(),
            ws: ws.into(),
        })
        .into(),
        "flow" => DomainEvent::from(AutomationEvent::Flow {
            id: "entity".into(),
            ws: ws.into(),
        })
        .into(),
        "run" => DomainEvent::from(AutomationEvent::Run {
            id: "entity".into(),
            flow_id: "entity".into(),
            ws: ws.into(),
        })
        .into(),
        "space" => DomainEvent::from(ChannelsEvent::Space {
            id: "entity".into(),
            ws: ws.into(),
        })
        .into(),
        "chan_message" => DomainEvent::from(ChannelsEvent::ChanMessage {
            id: "entity".into(),
            space_id: "entity".into(),
            ws: ws.into(),
        })
        .into(),
        "persona" => DomainEvent::from(ChannelsEvent::Persona {
            id: "entity".into(),
            ws: ws.into(),
        })
        .into(),
        "digest" => DomainEvent::from(ChannelsEvent::Digest {
            id: "entity".into(),
            ws: ws.into(),
        })
        .into(),
        "calendar" => DomainEvent::from(AutomationEvent::Calendar {
            id: "entity".into(),
            ws: ws.into(),
        })
        .into(),
        "channel" => DomainEvent::from(ChannelsEvent::Channel {
            id: "entity".into(),
            ws: ws.into(),
        })
        .into(),
        "approval_policy" => DomainEvent::from(AutonomyEvent::ApprovalPolicy {
            ws: ws.into(),
            policy: Default::default(),
        })
        .into(),
        "stats" => DomainEvent::from(StatsEvent::Stats {
            ws: ws.into(),
            day: None,
        })
        .into(),
        "kb" => DomainEvent::from(KnowledgeEvent::Kb {
            id: "entity".into(),
            ws: ws.into(),
        })
        .into(),
        "gh" => DomainEvent::from(IntegrationsEvent::Gh {
            id: "entity".into(),
            ws: ws.into(),
        })
        .into(),
        "mail" => DomainEvent::from(IntegrationsEvent::Mail {
            id: "entity".into(),
            ws: ws.into(),
        })
        .into(),
        "local_node.updating" => DomainEvent::from(DevicesEvent::LocalNodeUpdating {
            updating: Some(devices::LocalNodeUpdating { running: 1 }),
        })
        .into(),
        _ => panic!("missing typed fixture: {ty}"),
    }
}
