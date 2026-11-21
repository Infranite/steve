use super::*;

#[test]
fn node_events_map_onto_the_same_entries() {
    let t = of_node(
        &EventBody::TextDelta {
            message: "m1".into(),
            block: "b1".into(),
            utf8_offset: 0,
            text: "hi".into(),
        },
        Some(Role::Thought),
    )
    .unwrap();
    assert_eq!(t.kind, TEXT);
    assert_eq!(t.data["role"], "thought");
    assert!(of_node(&EventBody::AttemptStarted, None).is_none());
}

#[test]
fn node_tool_actions_keep_their_description_and_bound_the_preview() {
    let body: EventBody = serde_json::from_value(json!({
        "type": "tool_changed",
        "tool": {
            "id": "t1", "origin": "harness", "name": "Bash", "category": "exec",
            "title": "Run", "description": "Write the file", "phase": "pending",
            "action": {"type": "exec", "command": "x".repeat(TEXT_MAX * 2), "network": {"type":"denied"}}
        }
    })).unwrap();
    let entry = of_node(&body, None).unwrap();
    assert_eq!(entry.kind, TOOL);
    assert_eq!(entry.data["tool"]["description"], "Write the file");
    assert_eq!(
        entry.data["tool"]["action"]["command"]
            .as_str()
            .unwrap()
            .chars()
            .count(),
        TEXT_MAX + 1
    );
}

#[test]
fn assistant_text_only_reads_assistant_transcript_entries() {
    let data = json!({"role": "assistant", "text": "answer"});
    assert_eq!(assistant_text(TEXT, &data), Some("answer"));
    assert_eq!(
        assistant_text(TEXT, &json!({"role":"thought", "text":"thinking"})),
        None
    );
    assert_eq!(assistant_text("user", &data), None);
    assert_eq!(
        assistant_text("agent_message_chunk", &json!({"content":{"text":"old"}})),
        None
    );
}
