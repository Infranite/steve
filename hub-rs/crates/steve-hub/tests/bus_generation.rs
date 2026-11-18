//! Cache invalidation contract before S1, plus the explicitly pending S1 contract.
//! These use only public Bus APIs; generation changes are synchronous, not dispatcher timing.
use serde_json::{Value, json};
use steve_hub::bus::{Bus, HubEvent};

#[derive(Clone, Copy, Debug)]
enum Counter {
    Global,
    Workspace,
    None,
    Stats,
}

// All 36 ChangeEvent wire types plus the three plain-JSON integration notices.
// The second column is the producer's current ws shape (not the dispatcher's inferred scope).
// The third column pins the current cache behavior, including Thinking and Client exceptions.
const CURRENT: &[(&str, bool, Counter)] = &[
    ("item", false, Counter::Global),
    ("decision", false, Counter::Global),
    ("session", false, Counter::Global),
    ("event", false, Counter::None),
    ("message", false, Counter::Global),
    ("steve.thinking", true, Counter::Workspace),
    ("steve.draft", true, Counter::None),
    ("steve.tool", true, Counter::None),
    ("thread.run", true, Counter::Workspace),
    ("release.hold", false, Counter::Global),
    ("bot", false, Counter::Global),
    ("project", false, Counter::Global),
    ("tag", false, Counter::Global),
    ("workspace", false, Counter::Global),
    ("settings", false, Counter::Global),
    ("device", false, Counter::Global),
    ("node", false, Counter::Global),
    ("client", false, Counter::None),
    ("party", true, Counter::Workspace),
    ("mm", true, Counter::Workspace),
    ("changeset", true, Counter::Workspace),
    ("account", true, Counter::Workspace),
    ("connection", true, Counter::Workspace),
    ("vault", true, Counter::Workspace),
    ("signal", true, Counter::Workspace),
    ("flow", true, Counter::Workspace),
    ("run", true, Counter::Workspace),
    ("space", true, Counter::Workspace),
    ("chan_message", true, Counter::Workspace),
    ("persona", true, Counter::Workspace),
    ("digest", true, Counter::Workspace),
    ("calendar", true, Counter::Workspace),
    ("channel", true, Counter::Workspace),
    ("yolo", true, Counter::Workspace),
    ("yolo_action", true, Counter::Workspace),
    ("stats", true, Counter::Stats),
    ("kb", true, Counter::Workspace),
    ("gh", true, Counter::Workspace),
    ("mail", true, Counter::Workspace),
];

fn frame(ty: &str, ws: bool) -> Value {
    let mut value = json!({"type": ty, "id": "entity", "user": "user", "session_id": "session",
        "seq": 1, "kind": "text", "data": {}, "on": true, "mid": "message", "run": "run",
        "name": "tool", "brief": "test", "ok": true, "thought": "", "text": "test",
        "hold": null, "flow_id": "flow", "space_id": "space", "decision_id": null});
    if ws {
        value["ws"] = json!("a");
    }
    value
}

fn counts(bus: &Bus) -> (u64, u64, u64, u64) {
    (
        bus.generation(),
        bus.generation_for("a"),
        bus.generation_for("b"),
        bus.stats_generation(),
    )
}

fn expected(counter: Counter) -> (u64, u64, u64, u64) {
    match counter {
        Counter::Global => (1, 1, 1, 0),
        Counter::Workspace => (1, 1, 0, 0),
        Counter::None => (0, 0, 0, 0),
        Counter::Stats => (0, 0, 0, 1),
    }
}

#[test]
fn current_wire_type_generation_table() {
    assert_eq!(CURRENT.len(), 39);
    let mut unique = std::collections::BTreeSet::new();
    for &(ty, ws, counter) in CURRENT {
        assert!(unique.insert(ty), "duplicate row: {ty}");
        let bus = Bus::new();
        assert_eq!(counts(&bus), (0, 0, 0, 0));
        bus.publish_value(frame(ty, ws));
        assert_eq!(counts(&bus), expected(counter), "{ty}");
    }
}

#[test]
fn typed_publish_obeys_the_same_table_and_accumulates() {
    let bus = Bus::new();
    for (index, event) in [
        HubEvent::Item { id: "i".into() },
        HubEvent::Decision {
            id: "d".into(),
            item_id: Some(None),
        },
        HubEvent::Session {
            id: "s".into(),
            item_id: None,
        },
        HubEvent::Message { id: "m".into() },
        HubEvent::Bot { id: "b".into() },
        HubEvent::Project { id: "p".into() },
        HubEvent::Tag { id: "t".into() },
        HubEvent::Workspace { id: "a".into() },
        HubEvent::Settings { id: "a".into() },
        HubEvent::Device { id: "dev".into() },
        HubEvent::Node {
            id: "n".into(),
            user: "u".into(),
        },
    ]
    .into_iter()
    .enumerate()
    {
        bus.publish(event);
        let n = index as u64 + 1;
        assert_eq!(counts(&bus), (n, n, n, 0));
    }
    bus.publish(HubEvent::Client {
        id: "c".into(),
        user: "u".into(),
    });
    bus.publish(HubEvent::Event {
        session_id: "s".into(),
        seq: 1,
        kind: "text".into(),
        data: json!({}),
    });
    assert_eq!(counts(&bus), (11, 11, 11, 0));
    bus.publish(HubEvent::Thinking {
        on: true,
        ws: Some("a".into()),
        phase: None,
    });
    assert_eq!(counts(&bus), (12, 12, 11, 0));
    // Optional ws=None currently increments the overall count, but invalidates no workspace.
    bus.publish(HubEvent::Thinking {
        on: false,
        ws: None,
        phase: None,
    });
    assert_eq!(counts(&bus), (13, 12, 11, 0));
}

#[test]
fn current_invalid_ws_never_invalidates_a_state_cache() {
    for ws in [json!(null), json!(""), json!(4)] {
        let bus = Bus::new();
        let mut value = frame("item", false);
        value["ws"] = ws;
        bus.publish_value(value);
        assert_eq!(counts(&bus), (1, 0, 0, 0));
    }
    let bus = Bus::new();
    bus.publish_value(frame("party", false));
    assert_eq!(counts(&bus), (1, 0, 0, 0));
}

#[test]
#[ignore = "W1.3a: enable when events carry ws"]
fn s1_workspace_events_only_invalidate_their_workspace() {
    for &(ty, _, _) in CURRENT {
        if matches!(
            ty,
            "device"
                | "release.hold"
                | "node"
                | "client"
                | "event"
                | "steve.draft"
                | "steve.tool"
                | "stats"
        ) {
            continue;
        }
        let bus = Bus::new();
        bus.publish_value(frame(ty, true));
        assert_eq!(counts(&bus), (1, 1, 0, 0), "{ty}");
        let mut other = frame(ty, true);
        other["ws"] = json!("b");
        bus.publish_value(other);
        assert_eq!(counts(&bus), (2, 1, 1, 0), "{ty}");
    }
}

#[test]
#[ignore = "W1.3a: enable when events carry ws"]
fn s1_hub_and_user_events_invalidate_globally() {
    // Client presence is stateless even though its audience is User.
    for ty in ["device", "release.hold", "node"] {
        let bus = Bus::new();
        bus.publish_value(frame(ty, false));
        assert_eq!(counts(&bus), (1, 1, 1, 0), "{ty}");
    }
}

#[test]
#[ignore = "W1.3a: enable when events carry ws"]
fn s1_stateless_events_never_invalidate_state() {
    // Thinking remains in /api/state and is covered by the workspace invalidation test.
    for ty in ["event", "steve.draft", "steve.tool", "stats", "client"] {
        let bus = Bus::new();
        bus.publish_value(frame(ty, ty != "client"));
        assert_eq!(bus.generation_for("a"), 0, "{ty}");
        assert_eq!(bus.generation_for("b"), 0, "{ty}");
        assert_eq!(bus.stats_generation(), u64::from(ty == "stats"), "{ty}");
    }
}
