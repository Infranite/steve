//! Golden bytes were captured from the pre-migration enums, not from these new types.
use serde::{Serialize, de::DeserializeOwned};
use std::{collections::BTreeSet, fs, path::Path};
use steve_types::*;
use strum::EnumCount;

fn without_ws(text: &str) -> String {
    text.replace(",\"ws\":\"workspace-a\"", "")
        .replace("\"ws\":\"workspace-a\",", "")
}

fn check<E>(value: E, name: &str, expected: Scope, live: bool, object_order: bool)
where
    E: Serialize + DeserializeOwned + Scoped + std::fmt::Debug + PartialEq + Clone,
{
    let golden = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/wire")
            .join(format!("{name}.json")),
    )
    .unwrap();
    assert_eq!(value.scope(), expected, "{name}");
    let bytes = if object_order {
        serde_json::to_string(&serde_json::to_value(&value).unwrap()).unwrap()
    } else {
        serde_json::to_string(&value).unwrap()
    };
    assert_eq!(without_ws(&bytes), without_ws(&golden), "{name}");
    let mut input = golden.clone();
    if matches!(expected, Scope::Ws(_)) && !input.contains("\"ws\":") {
        input.pop();
        input.push_str(",\"ws\":\"workspace-a\"}");
    }
    assert_eq!(serde_json::from_str::<E>(&input).unwrap(), value, "{name}");
    if live {
        let event: LiveEvent = serde_json::from_str(&bytes).unwrap();
        assert_eq!(event.scope(), expected);
        assert_eq!(
            serde_json::to_string(&event).unwrap(),
            serde_json::to_string(&value).unwrap()
        );
        let env = Envelope {
            event,
            rev: Rev(47),
        };
        assert_eq!(env.scope(), expected);
        assert_eq!(
            serde_json::from_str::<Envelope<LiveEvent>>(&serde_json::to_string(&env).unwrap())
                .unwrap(),
            env
        );
    } else {
        let event: DomainEvent = serde_json::from_str(&bytes).unwrap();
        assert_eq!(event.scope(), expected);
        assert_eq!(
            serde_json::to_string(&event).unwrap(),
            serde_json::to_string(&value).unwrap()
        );
        let env = Envelope {
            event,
            rev: Rev(47),
        };
        assert_eq!(env.scope(), expected);
        assert_eq!(
            serde_json::from_str::<Envelope<DomainEvent>>(&serde_json::to_string(&env).unwrap())
                .unwrap(),
            env
        );
    }
}

#[test]
fn every_wire_variant_has_bytes_roundtrip_and_scope() {
    let mut fixtures = BTreeSet::new();
    let mut workevent = BTreeSet::new();
    let mut workliveevent = BTreeSet::new();
    let mut devicesevent = BTreeSet::new();
    let mut configevent = BTreeSet::new();
    let mut peopleevent = BTreeSet::new();
    let mut channelsevent = BTreeSet::new();
    let mut vaultevent = BTreeSet::new();
    let mut automationevent = BTreeSet::new();
    let mut autonomyevent = BTreeSet::new();
    let mut statsevent = BTreeSet::new();
    let mut knowledgeevent = BTreeSet::new();
    let mut integrationsevent = BTreeSet::new();
    check(
        WorkEvent::Item {
            id: "entity-1".into(),
            ws: "workspace-a".into(),
        },
        "item",
        Scope::Ws("workspace-a".into()),
        false,
        false,
    );
    fixtures.insert("item.json".to_owned());
    workevent.insert("item");
    check(
        WorkEvent::Decision {
            id: "entity-1".into(),
            item_id: None,
            ws: "workspace-a".into(),
        },
        "decision",
        Scope::Ws("workspace-a".into()),
        false,
        false,
    );
    fixtures.insert("decision.json".to_owned());
    workevent.insert("decision");
    check(
        WorkEvent::Decision {
            id: "entity-1".into(),
            item_id: Some(None),
            ws: "workspace-a".into(),
        },
        "decision-null",
        Scope::Ws("workspace-a".into()),
        false,
        false,
    );
    fixtures.insert("decision-null.json".to_owned());
    workevent.insert("decision");
    check(
        WorkEvent::Decision {
            id: "entity-1".into(),
            item_id: Some(Some("item-1".into())),
            ws: "workspace-a".into(),
        },
        "decision-value",
        Scope::Ws("workspace-a".into()),
        false,
        false,
    );
    fixtures.insert("decision-value.json".to_owned());
    workevent.insert("decision");
    check(
        WorkEvent::Session {
            id: "entity-1".into(),
            item_id: None,
            ws: "workspace-a".into(),
        },
        "session",
        Scope::Ws("workspace-a".into()),
        false,
        false,
    );
    fixtures.insert("session.json".to_owned());
    workevent.insert("session");
    check(
        WorkEvent::Session {
            id: "entity-1".into(),
            item_id: Some(None),
            ws: "workspace-a".into(),
        },
        "session-null",
        Scope::Ws("workspace-a".into()),
        false,
        false,
    );
    fixtures.insert("session-null.json".to_owned());
    workevent.insert("session");
    check(
        WorkEvent::Session {
            id: "entity-1".into(),
            item_id: Some(Some("item-1".into())),
            ws: "workspace-a".into(),
        },
        "session-value",
        Scope::Ws("workspace-a".into()),
        false,
        false,
    );
    fixtures.insert("session-value.json".to_owned());
    workevent.insert("session");
    check(
        WorkLiveEvent::Event {
            session_id: "session-1".into(),
            seq: Seq(7),
            kind: "append".into(),
            data: RawJson::from_str(r#"{"text":"hello"}"#).unwrap(),
            ws: "workspace-a".into(),
        },
        "event",
        Scope::Ws("workspace-a".into()),
        true,
        false,
    );
    fixtures.insert("event.json".to_owned());
    workliveevent.insert("event");
    check(
        WorkEvent::Message {
            id: "entity-1".into(),
            ws: "workspace-a".into(),
        },
        "message",
        Scope::Ws("workspace-a".into()),
        false,
        false,
    );
    fixtures.insert("message.json".to_owned());
    workevent.insert("message");
    check(
        WorkLiveEvent::SteveThinking {
            on: true,
            ws: "workspace-a".into(),
            phase: None,
        },
        "steve.thinking",
        Scope::Ws("workspace-a".into()),
        true,
        false,
    );
    fixtures.insert("steve.thinking.json".to_owned());
    workliveevent.insert("steve.thinking");
    check(
        WorkLiveEvent::SteveThinking {
            on: true,
            ws: "workspace-a".into(),
            phase: Some("answer".into()),
        },
        "steve.thinking-full",
        Scope::Ws("workspace-a".into()),
        true,
        false,
    );
    fixtures.insert("steve.thinking-full.json".to_owned());
    workliveevent.insert("steve.thinking");
    check(
        WorkLiveEvent::SteveDraft {
            ws: "workspace-a".into(),
            mid: "message-1".into(),
            thought: "thinking".into(),
            text: "answer".into(),
        },
        "steve.draft",
        Scope::Ws("workspace-a".into()),
        true,
        false,
    );
    fixtures.insert("steve.draft.json".to_owned());
    workliveevent.insert("steve.draft");
    check(
        WorkLiveEvent::SteveTool {
            ws: "workspace-a".into(),
            mid: "message-1".into(),
            run: "run-1".into(),
            name: "search".into(),
            brief: "lookup".into(),
            ok: true,
            kind: String::new(),
            running: false,
            call: None,
            started_at: None,
            at: None,
        },
        "steve.tool",
        Scope::Ws("workspace-a".into()),
        true,
        false,
    );
    fixtures.insert("steve.tool.json".to_owned());
    workliveevent.insert("steve.tool");
    check(
        WorkLiveEvent::SteveTool {
            ws: "workspace-a".into(),
            mid: "message-1".into(),
            run: "run-1".into(),
            name: "search".into(),
            brief: "lookup".into(),
            ok: true,
            kind: "search".into(),
            running: true,
            call: Some(2),
            started_at: Some(Millis(10)),
            at: Some(Millis(20)),
        },
        "steve.tool-full",
        Scope::Ws("workspace-a".into()),
        true,
        false,
    );
    fixtures.insert("steve.tool-full.json".to_owned());
    workliveevent.insert("steve.tool");
    check(
        WorkEvent::ThreadRun {
            id: "entity-1".into(),
            ws: "workspace-a".into(),
        },
        "thread.run",
        Scope::Ws("workspace-a".into()),
        false,
        false,
    );
    fixtures.insert("thread.run.json".to_owned());
    workevent.insert("thread.run");
    check(
        DevicesEvent::ReleaseHold { hold: None },
        "release.hold",
        Scope::Hub,
        false,
        false,
    );
    fixtures.insert("release.hold.json".to_owned());
    devicesevent.insert("release.hold");
    check(
        DevicesEvent::ReleaseHold {
            hold: Some(RawJson::from_str(r#"{"item":"item-1","running":2,"since":10}"#).unwrap()),
        },
        "release.hold-full",
        Scope::Hub,
        false,
        false,
    );
    fixtures.insert("release.hold-full.json".to_owned());
    devicesevent.insert("release.hold");
    check(
        ConfigEvent::Bot {
            id: "entity-1".into(),
            ws: "workspace-a".into(),
        },
        "bot",
        Scope::Ws("workspace-a".into()),
        false,
        false,
    );
    fixtures.insert("bot.json".to_owned());
    configevent.insert("bot");
    check(
        ConfigEvent::Project {
            id: "entity-1".into(),
            ws: "workspace-a".into(),
        },
        "project",
        Scope::Ws("workspace-a".into()),
        false,
        false,
    );
    fixtures.insert("project.json".to_owned());
    configevent.insert("project");
    check(
        ConfigEvent::Tag {
            id: "entity-1".into(),
            ws: "workspace-a".into(),
        },
        "tag",
        Scope::Ws("workspace-a".into()),
        false,
        false,
    );
    fixtures.insert("tag.json".to_owned());
    configevent.insert("tag");
    check(
        ConfigEvent::Workspace {
            id: "entity-1".into(),
            ws: "workspace-a".into(),
        },
        "workspace",
        Scope::Ws("workspace-a".into()),
        false,
        false,
    );
    fixtures.insert("workspace.json".to_owned());
    configevent.insert("workspace");
    check(
        ConfigEvent::Settings {
            id: "entity-1".into(),
            ws: "workspace-a".into(),
        },
        "settings",
        Scope::Ws("workspace-a".into()),
        false,
        false,
    );
    fixtures.insert("settings.json".to_owned());
    configevent.insert("settings");
    check(
        DevicesEvent::Device {
            id: "entity-1".into(),
        },
        "device",
        Scope::Hub,
        false,
        false,
    );
    fixtures.insert("device.json".to_owned());
    devicesevent.insert("device");
    check(
        DevicesEvent::Node {
            id: "entity-1".into(),
            user: "user-a".into(),
        },
        "node",
        Scope::User("user-a".into()),
        false,
        false,
    );
    fixtures.insert("node.json".to_owned());
    devicesevent.insert("node");
    check(
        DevicesEvent::Client {
            id: "entity-1".into(),
            user: "user-a".into(),
        },
        "client",
        Scope::User("user-a".into()),
        false,
        false,
    );
    fixtures.insert("client.json".to_owned());
    devicesevent.insert("client");
    check(
        PeopleEvent::Party {
            id: "entity-1".into(),
            ws: "workspace-a".into(),
        },
        "party",
        Scope::Ws("workspace-a".into()),
        false,
        false,
    );
    fixtures.insert("party.json".to_owned());
    peopleevent.insert("party");
    check(
        ConfigEvent::Mm {
            id: "entity-1".into(),
            ws: "workspace-a".into(),
        },
        "mm",
        Scope::Ws("workspace-a".into()),
        false,
        false,
    );
    fixtures.insert("mm.json".to_owned());
    configevent.insert("mm");
    check(
        ConfigEvent::Changeset {
            id: "entity-1".into(),
            ws: "workspace-a".into(),
        },
        "changeset",
        Scope::Ws("workspace-a".into()),
        false,
        false,
    );
    fixtures.insert("changeset.json".to_owned());
    configevent.insert("changeset");
    check(
        PeopleEvent::Account {
            id: "entity-1".into(),
            ws: "workspace-a".into(),
        },
        "account",
        Scope::Ws("workspace-a".into()),
        false,
        false,
    );
    fixtures.insert("account.json".to_owned());
    peopleevent.insert("account");
    check(
        ChannelsEvent::Connection {
            id: "entity-1".into(),
            ws: "workspace-a".into(),
        },
        "connection",
        Scope::Ws("workspace-a".into()),
        false,
        false,
    );
    fixtures.insert("connection.json".to_owned());
    channelsevent.insert("connection");
    check(
        VaultEvent::Vault {
            id: "entity-1".into(),
            ws: "workspace-a".into(),
        },
        "vault",
        Scope::Ws("workspace-a".into()),
        false,
        false,
    );
    fixtures.insert("vault.json".to_owned());
    vaultevent.insert("vault");
    check(
        AutomationEvent::Signal {
            id: "entity-1".into(),
            ws: "workspace-a".into(),
        },
        "signal",
        Scope::Ws("workspace-a".into()),
        false,
        false,
    );
    fixtures.insert("signal.json".to_owned());
    automationevent.insert("signal");
    check(
        AutomationEvent::Flow {
            id: "entity-1".into(),
            ws: "workspace-a".into(),
        },
        "flow",
        Scope::Ws("workspace-a".into()),
        false,
        false,
    );
    fixtures.insert("flow.json".to_owned());
    automationevent.insert("flow");
    check(
        AutomationEvent::Run {
            id: "entity-1".into(),
            flow_id: "flow-1".into(),
            ws: "workspace-a".into(),
        },
        "run",
        Scope::Ws("workspace-a".into()),
        false,
        false,
    );
    fixtures.insert("run.json".to_owned());
    automationevent.insert("run");
    check(
        ChannelsEvent::Space {
            id: "entity-1".into(),
            ws: "workspace-a".into(),
        },
        "space",
        Scope::Ws("workspace-a".into()),
        false,
        false,
    );
    fixtures.insert("space.json".to_owned());
    channelsevent.insert("space");
    check(
        ChannelsEvent::ChanMessage {
            id: "entity-1".into(),
            space_id: "space-1".into(),
            ws: "workspace-a".into(),
        },
        "chan_message",
        Scope::Ws("workspace-a".into()),
        false,
        false,
    );
    fixtures.insert("chan_message.json".to_owned());
    channelsevent.insert("chan_message");
    check(
        ChannelsEvent::Persona {
            id: "entity-1".into(),
            ws: "workspace-a".into(),
        },
        "persona",
        Scope::Ws("workspace-a".into()),
        false,
        false,
    );
    fixtures.insert("persona.json".to_owned());
    channelsevent.insert("persona");
    check(
        ChannelsEvent::Digest {
            id: "entity-1".into(),
            ws: "workspace-a".into(),
        },
        "digest",
        Scope::Ws("workspace-a".into()),
        false,
        false,
    );
    fixtures.insert("digest.json".to_owned());
    channelsevent.insert("digest");
    check(
        AutomationEvent::Calendar {
            id: "entity-1".into(),
            ws: "workspace-a".into(),
        },
        "calendar",
        Scope::Ws("workspace-a".into()),
        false,
        false,
    );
    fixtures.insert("calendar.json".to_owned());
    automationevent.insert("calendar");
    check(
        ChannelsEvent::Channel {
            id: "entity-1".into(),
            ws: "workspace-a".into(),
        },
        "channel",
        Scope::Ws("workspace-a".into()),
        false,
        false,
    );
    fixtures.insert("channel.json".to_owned());
    channelsevent.insert("channel");
    check(
        AutonomyEvent::Yolo {
            ws: "workspace-a".into(),
        },
        "yolo",
        Scope::Ws("workspace-a".into()),
        false,
        false,
    );
    fixtures.insert("yolo.json".to_owned());
    autonomyevent.insert("yolo");
    check(
        AutonomyEvent::YoloAction {
            id: "entity-1".into(),
            decision_id: None,
            ws: "workspace-a".into(),
        },
        "yolo_action",
        Scope::Ws("workspace-a".into()),
        false,
        false,
    );
    fixtures.insert("yolo_action.json".to_owned());
    autonomyevent.insert("yolo_action");
    check(
        AutonomyEvent::YoloAction {
            id: "entity-1".into(),
            decision_id: Some("decision-1".into()),
            ws: "workspace-a".into(),
        },
        "yolo_action-full",
        Scope::Ws("workspace-a".into()),
        false,
        false,
    );
    fixtures.insert("yolo_action-full.json".to_owned());
    autonomyevent.insert("yolo_action");
    check(
        StatsEvent::Stats {
            ws: "workspace-a".into(),
            day: None,
        },
        "stats",
        Scope::Ws("workspace-a".into()),
        false,
        false,
    );
    fixtures.insert("stats.json".to_owned());
    statsevent.insert("stats");
    check(
        StatsEvent::Stats {
            ws: "workspace-a".into(),
            day: Some("2026-11-19".into()),
        },
        "stats-full",
        Scope::Ws("workspace-a".into()),
        false,
        false,
    );
    fixtures.insert("stats-full.json".to_owned());
    statsevent.insert("stats");
    check(
        KnowledgeEvent::Kb {
            id: "entity-1".into(),
            ws: "workspace-a".into(),
        },
        "kb",
        Scope::Ws("workspace-a".into()),
        false,
        true,
    );
    fixtures.insert("kb.json".to_owned());
    knowledgeevent.insert("kb");
    check(
        IntegrationsEvent::Gh {
            id: "entity-1".into(),
            ws: "workspace-a".into(),
        },
        "gh",
        Scope::Ws("workspace-a".into()),
        false,
        true,
    );
    fixtures.insert("gh.json".to_owned());
    integrationsevent.insert("gh");
    check(
        IntegrationsEvent::Mail {
            id: "entity-1".into(),
            ws: "workspace-a".into(),
        },
        "mail",
        Scope::Ws("workspace-a".into()),
        false,
        true,
    );
    fixtures.insert("mail.json".to_owned());
    integrationsevent.insert("mail");
    assert_eq!(
        workevent.len(),
        WorkEvent::COUNT,
        "new WorkEvent variant needs a scope/golden case"
    );
    assert_eq!(
        workliveevent.len(),
        WorkLiveEvent::COUNT,
        "new WorkLiveEvent variant needs a scope/golden case"
    );
    assert_eq!(
        devicesevent.len(),
        DevicesEvent::COUNT,
        "new DevicesEvent variant needs a scope/golden case"
    );
    assert_eq!(
        configevent.len(),
        ConfigEvent::COUNT,
        "new ConfigEvent variant needs a scope/golden case"
    );
    assert_eq!(
        peopleevent.len(),
        PeopleEvent::COUNT,
        "new PeopleEvent variant needs a scope/golden case"
    );
    assert_eq!(
        channelsevent.len(),
        ChannelsEvent::COUNT,
        "new ChannelsEvent variant needs a scope/golden case"
    );
    assert_eq!(
        vaultevent.len(),
        VaultEvent::COUNT,
        "new VaultEvent variant needs a scope/golden case"
    );
    assert_eq!(
        automationevent.len(),
        AutomationEvent::COUNT,
        "new AutomationEvent variant needs a scope/golden case"
    );
    assert_eq!(
        autonomyevent.len(),
        AutonomyEvent::COUNT,
        "new AutonomyEvent variant needs a scope/golden case"
    );
    assert_eq!(
        statsevent.len(),
        StatsEvent::COUNT,
        "new StatsEvent variant needs a scope/golden case"
    );
    assert_eq!(
        knowledgeevent.len(),
        KnowledgeEvent::COUNT,
        "new KnowledgeEvent variant needs a scope/golden case"
    );
    assert_eq!(
        integrationsevent.len(),
        IntegrationsEvent::COUNT,
        "new IntegrationsEvent variant needs a scope/golden case"
    );
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/wire");
    let actual: BTreeSet<_> = fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(
        fixtures, actual,
        "every fixture must have a construction and scope assertion"
    );
    assert_eq!(DomainEvent::COUNT, 11);
    assert_eq!(LiveEvent::COUNT, 1);
}

#[test]
fn workspace_scope_is_required_and_opaque_bytes_survive_envelopes() {
    for bad in [
        r#"{"type":"item","id":"i"}"#,
        r#"{"type":"item","id":"i","ws":null}"#,
        r#"{"type":"steve.thinking","on":true}"#,
        r#"{"type":"plugin.custom","ws":"workspace-a"}"#,
    ] {
        assert!(serde_json::from_str::<DomainEvent>(bad).is_err());
        assert!(serde_json::from_str::<LiveEvent>(bad).is_err());
    }
    let bytes = r#"{"rev":47,"data":{"b": [1,  2], "a":1e+02, "escaped":"\u0061"},"kind":"append","seq":7,"session_id":"s","ws":"workspace-a","type":"event"}"#;
    let envelope: Envelope<LiveEvent> = serde_json::from_str(bytes).unwrap();
    let output = serde_json::to_string(&envelope).unwrap();
    assert!(output.contains(r#"{"b": [1,  2], "a":1e+02, "escaped":"\u0061"}"#));
    assert_eq!(
        serde_json::from_str::<Envelope<LiveEvent>>(&output).unwrap(),
        envelope
    );
}
