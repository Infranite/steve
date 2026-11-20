//! Captured wire bytes pin the public event contract, independently of the Rust declarations.
use serde::Serialize;
use std::{collections::BTreeSet, fs, path::Path};
use steve_types::*;
use strum::EnumCount;

fn without_ws(text: &str) -> String {
    text.replace(",\"ws\":\"workspace-a\"", "")
        .replace("\"ws\":\"workspace-a\",", "")
}

// Exercise each child and its actual aggregate/envelope without reading bus events back in Rust.
macro_rules! golden_check {
    ($name:ident, $aggregate:ty) => {
        fn $name<E>(value: E, name: &str, expected: Scope)
        where
            E: Serialize + Scoped + Into<$aggregate>,
        {
            let golden = fs::read_to_string(
                Path::new(env!("CARGO_MANIFEST_DIR"))
                    .join("tests/fixtures/wire")
                    .join(format!("{name}.json")),
            )
            .unwrap();
            assert_eq!(value.scope(), expected, "{name}");
            // Straight from the struct, never through a JSON map: the bytes must not depend on
            // whether some other crate in the build enables serde_json/preserve_order.
            let bytes = serde_json::to_string(&value).unwrap();
            assert_eq!(without_ws(&bytes), without_ws(&golden), "{name}");
            let event: $aggregate = value.into();
            assert_eq!(event.scope(), expected, "{name}");
            assert_eq!(serde_json::to_string(&event).unwrap(), bytes, "{name}");
            let envelope = Envelope {
                event,
                rev: Rev(47),
            };
            assert_eq!(envelope.scope(), expected, "{name}");
            assert_eq!(
                serde_json::to_string(&envelope).unwrap(),
                format!("{},\"rev\":47}}", &bytes[..bytes.len() - 1]),
                "{name}"
            );
        }
    };
}
golden_check!(check, DomainEvent);
golden_check!(check_live, LiveEvent);

#[test]
fn every_wire_variant_has_golden_bytes_and_scope() {
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
    );
    fixtures.insert("session-value.json".to_owned());
    workevent.insert("session");
    check_live(
        WorkLiveEvent::Event {
            session_id: "session-1".into(),
            seq: Seq(7),
            kind: "append".into(),
            data: RawJson::from_str(r#"{"text":"hello"}"#).unwrap(),
            ws: "workspace-a".into(),
        },
        "event",
        Scope::Ws("workspace-a".into()),
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
    );
    fixtures.insert("message.json".to_owned());
    workevent.insert("message");
    check_live(
        WorkLiveEvent::SteveThinking {
            on: true,
            ws: "workspace-a".into(),
            phase: None,
        },
        "steve.thinking",
        Scope::Ws("workspace-a".into()),
    );
    fixtures.insert("steve.thinking.json".to_owned());
    workliveevent.insert("steve.thinking");
    check_live(
        WorkLiveEvent::SteveThinking {
            on: true,
            ws: "workspace-a".into(),
            phase: Some("answer".into()),
        },
        "steve.thinking-full",
        Scope::Ws("workspace-a".into()),
    );
    fixtures.insert("steve.thinking-full.json".to_owned());
    workliveevent.insert("steve.thinking");
    check_live(
        WorkLiveEvent::SteveDraft {
            ws: "workspace-a".into(),
            mid: "message-1".into(),
            thought: "thinking".into(),
            text: "answer".into(),
        },
        "steve.draft",
        Scope::Ws("workspace-a".into()),
    );
    fixtures.insert("steve.draft.json".to_owned());
    workliveevent.insert("steve.draft");
    check_live(
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
    );
    fixtures.insert("steve.tool.json".to_owned());
    workliveevent.insert("steve.tool");
    check_live(
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
    );
    fixtures.insert("thread.run.json".to_owned());
    workevent.insert("thread.run");
    check(
        DevicesEvent::ReleaseHold { hold: None },
        "release.hold",
        Scope::Hub,
    );
    fixtures.insert("release.hold.json".to_owned());
    devicesevent.insert("release.hold");
    check(
        DevicesEvent::ReleaseHold {
            hold: Some(RawJson::from_str(r#"{"item":"item-1","running":2,"since":10}"#).unwrap()),
        },
        "release.hold-full",
        Scope::Hub,
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
    );
    fixtures.insert("settings.json".to_owned());
    configevent.insert("settings");
    check(
        DevicesEvent::Device {
            id: "entity-1".into(),
        },
        "device",
        Scope::Hub,
    );
    fixtures.insert("device.json".to_owned());
    devicesevent.insert("device");
    check(
        DevicesEvent::Node {
            id: "entity-1".into(),
            user: "user-a".into(),
        },
        "node",
        Scope::Hub,
    );
    fixtures.insert("node.json".to_owned());
    devicesevent.insert("node");
    check(
        DevicesEvent::Client {
            id: "entity-1".into(),
            user: "user-a".into(),
        },
        "client",
        Scope::Hub,
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
    );
    fixtures.insert("channel.json".to_owned());
    channelsevent.insert("channel");
    check(
        AutonomyEvent::Yolo {
            ws: "workspace-a".into(),
        },
        "yolo",
        Scope::Ws("workspace-a".into()),
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
    let covered: BTreeSet<&str> = [
        &workevent,
        &workliveevent,
        &devicesevent,
        &configevent,
        &peopleevent,
        &channelsevent,
        &vaultevent,
        &automationevent,
        &autonomyevent,
        &statsevent,
        &knowledgeevent,
        &integrationsevent,
    ]
    .into_iter()
    .flatten()
    .copied()
    .collect();
    assert_eq!(
        covered,
        WIRE_TYPES.into_iter().collect::<BTreeSet<_>>(),
        "WIRE_TYPES must list exactly the variants' wire types"
    );
}

#[test]
fn opaque_payload_bytes_survive_envelope_serialization() {
    let raw = r#"{"b": [1,  2], "a":1e+02, "escaped":"\u0061"}"#;
    let event: LiveEvent = WorkLiveEvent::Event {
        session_id: "s".into(),
        seq: Seq(7),
        kind: "append".into(),
        data: RawJson::from_str(raw).unwrap(),
        ws: "workspace-a".into(),
    }
    .into();
    let envelope = Envelope {
        event,
        rev: Rev(47),
    };
    let expected = format!(
        r#"{{"type":"event","session_id":"s","seq":7,"kind":"append","data":{raw},"ws":"workspace-a","rev":47}}"#
    );
    assert_eq!(serde_json::to_string(&envelope).unwrap(), expected);
    assert_eq!(envelope.scope(), Scope::Ws("workspace-a".into()));
}
