//! Typed cache invalidation contract. Generation changes are synchronous.
mod bus_cases;
use bus_cases::typed;
use steve_hub::bus::Bus;
#[derive(Clone, Copy, Debug)]
enum Counter {
    Global,
    Workspace,
    None,
    Stats,
}

// All 39 typed wire tags, including the thinking/client cache exceptions.
const CURRENT: &[(&str, bool, Counter)] = &[
    ("item", true, Counter::Workspace),
    ("decision", true, Counter::Workspace),
    ("session", true, Counter::Workspace),
    ("event", false, Counter::None),
    ("message", true, Counter::Workspace),
    ("steve.thinking", true, Counter::Workspace),
    ("steve.draft", true, Counter::None),
    ("steve.tool", true, Counter::None),
    ("thread.run", true, Counter::Workspace),
    ("release.hold", false, Counter::Global),
    ("bot", true, Counter::Workspace),
    ("project", true, Counter::Workspace),
    ("tag", true, Counter::Workspace),
    ("workspace", true, Counter::Workspace),
    ("settings", true, Counter::Workspace),
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
fn typed_wire_type_generation_table() {
    assert_eq!(
        CURRENT.iter().map(|(ty, _, _)| *ty).collect::<Vec<_>>(),
        steve_types::WIRE_TYPES,
        "the table covers exactly the shared wire type list"
    );
    let mut unique = std::collections::BTreeSet::new();
    for &(ty, _, counter) in CURRENT {
        assert!(unique.insert(ty));
        let bus = Bus::new();
        assert_eq!(counts(&bus), (0, 0, 0, 0));
        bus.publish(typed(ty, "a"));
        assert_eq!(counts(&bus), expected(counter), "{ty}");
    }
}
#[test]
fn typed_publish_accumulates_workspace_and_global_generations() {
    let bus = Bus::new();
    bus.publish(typed("item", "a"));
    bus.publish(typed("message", "b"));
    bus.publish(typed("node", "a"));
    assert_eq!(counts(&bus), (3, 2, 2, 0));
    bus.publish(typed("client", "a"));
    bus.publish(typed("event", "a"));
    assert_eq!(counts(&bus), (3, 2, 2, 0));
    bus.publish(typed("steve.thinking", "a"));
    assert_eq!(counts(&bus), (4, 3, 2, 0));
}
#[test]
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
        bus.publish(typed(ty, "a"));
        assert_eq!(counts(&bus), (1, 1, 0, 0), "{ty}");
        bus.publish(typed(ty, "b"));
        assert_eq!(counts(&bus), (2, 1, 1, 0), "{ty}");
    }
}

#[test]
fn s1_hub_and_user_events_invalidate_globally() {
    // Client presence is stateless even though its audience is User.
    for ty in ["device", "release.hold", "node"] {
        let bus = Bus::new();
        bus.publish(typed(ty, "a"));
        assert_eq!(counts(&bus), (1, 1, 1, 0), "{ty}");
    }
}

#[test]
fn s1_stateless_events_never_invalidate_state() {
    // Thinking remains in /api/state and is covered by the workspace invalidation test.
    for ty in ["event", "steve.draft", "steve.tool", "stats", "client"] {
        let bus = Bus::new();
        bus.publish(typed(ty, "a"));
        assert_eq!(bus.generation_for("a"), 0, "{ty}");
        assert_eq!(bus.generation_for("b"), 0, "{ty}");
        assert_eq!(bus.stats_generation(), u64::from(ty == "stats"), "{ty}");
    }
}

#[test]
fn an_event_without_a_workspace_never_invalidates_a_state_cache() {
    for ty in ["item", "party", "steve.thinking"] {
        let bus = Bus::new();
        bus.publish(typed(ty, ""));
        assert_eq!(counts(&bus), (1, 0, 0, 0), "{ty}");
        assert_eq!(bus.generation_for(""), 0, "{ty}");
    }
}
