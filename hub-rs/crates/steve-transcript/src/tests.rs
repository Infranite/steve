use super::*;
use steve_node_api::{CallId, Money, SubtaskOrigin, SubtaskStatus, SubtaskUsage, SubtaskView};

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

#[test]
fn subtask_changed_maps_to_subtask_transcript_entry() {
    let body = EventBody::SubtaskChanged {
        subtask: SubtaskView {
            id: CallId::new("sub-1").unwrap(),
            parent: None,
            external_id: Some("sub-1".into()),
            origin: SubtaskOrigin::Subagent,
            status: SubtaskStatus::Running,
            title: "Audit auth".into(),
            actor: Some("Explore".into()),
            model: Some("claude-haiku-4-5".into()),
            progress: Some("Reading auth.rs".into()),
            summary: None,
            error: None,
            can_cancel: true,
            duration_ms: Some(420),
            usage: Some(SubtaskUsage {
                input: Some(1200),
                output: Some(300),
                cache_read: None,
                cache_write: None,
                reasoning: None,
                total_tokens: None,
                context_used: None,
                context_limit: None,
                cost: Some(Money {
                    decimal: "0.005".into(),
                    currency: "USD".into(),
                }),
            }),
        },
    };
    let entry = of_node(&body, None).unwrap();
    assert_eq!(entry.kind, SUBTASK);
    assert_eq!(entry.data["subtask"]["id"], "sub-1");
    assert_eq!(entry.data["subtask"]["origin"], "subagent");
    assert_eq!(entry.data["subtask"]["can_cancel"], true);
    assert_eq!(entry.data["subtask"]["usage"]["output"], 300);
}
