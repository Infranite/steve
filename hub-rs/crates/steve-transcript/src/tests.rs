use super::*;

fn chunk(kind: &str, text: &str) -> Value {
    json!({"sessionUpdate": kind, "content": {"type": "text", "text": text}})
}

#[test]
fn old_text_reads_as_text_of_its_role() {
    let mut r = LegacyReader::new("/w");
    let a = r.read("agent_message_chunk", &chunk("agent_message_chunk", "hi"));
    assert_eq!(a.len(), 1);
    assert_eq!(a[0].kind, TEXT);
    assert_eq!(a[0].data["role"], "assistant");
    assert_eq!(a[0].data["text"], "hi");
    let t = r.read("agent_thought_chunk", &chunk("agent_thought_chunk", "hmm"));
    assert_eq!(t[0].data["role"], "thought");
    // the next text of the same role keeps its message
    let b = r.read(
        "agent_message_chunk",
        &chunk("agent_message_chunk", " there"),
    );
    assert_eq!(b[0].data["role"], "assistant");
    assert_ne!(
        b[0].data["message"], a[0].data["message"],
        "a thought came between"
    );
    assert_eq!(assistant_text(&b[0].kind, &b[0].data), Some(" there"));
    assert_eq!(
        assistant_text("agent_message_chunk", &chunk("agent_message_chunk", "x")),
        Some("x")
    );
}

#[test]
fn an_old_tool_reads_as_its_whole_view_and_updates_keep_what_came_before() {
    let mut r = LegacyReader::new("/w");
    let call = json!({"sessionUpdate": "tool_call", "toolCallId": "t1", "title": "`ls -la`", "kind": "execute", "status": "pending",
        "rawInput": {"command": "ls -la", "description": "List"}, "_meta": {"claudeCode": {"toolName": "Bash"}}});
    let e = r.read("tool_call", &call);
    assert_eq!(e.len(), 1);
    assert_eq!(e[0].kind, TOOL);
    let tool = &e[0].data["tool"];
    assert_eq!(tool["name"], "Bash");
    assert_eq!(tool["category"], "exec");
    assert_eq!(tool["phase"], "pending");
    assert_eq!(tool["action"]["command"], "ls -la");
    assert!(e[0].data.to_string().find("_meta").is_none(), "no ACP left");

    let upd = json!({"sessionUpdate": "tool_call_update", "toolCallId": "t1", "status": "completed",
        "content": [{"type": "content", "content": {"type": "text", "text": "a\nb"}}]});
    assert_eq!(r.missing_tool("tool_call_update", &upd), None);
    let e = r.read("tool_call_update", &upd);
    let tool = &e[0].data["tool"];
    assert_eq!(tool["name"], "Bash", "kept from the call");
    assert_eq!(tool["phase"], "succeeded");
    assert_eq!(tool["result"][0]["text"], "a\nb");

    // read alone, an update names the tool it needs; primed, it reads whole
    let mut alone = LegacyReader::new("/w");
    assert_eq!(
        alone.missing_tool("tool_call_update", &upd).as_deref(),
        Some("t1")
    );
    alone.prime("tool_call", &call);
    assert_eq!(alone.missing_tool("tool_call_update", &upd), None);
    let e = alone.read("tool_call_update", &upd);
    assert_eq!(e[0].data["tool"]["name"], "Bash");
}

#[test]
fn edits_read_as_diffs_relative_to_the_session() {
    let mut r = LegacyReader::new("/w");
    let call = json!({"sessionUpdate": "tool_call", "toolCallId": "e1", "title": "Edit /w/src/a.rs", "kind": "edit", "status": "completed",
        "rawInput": {"file_path": "/w/src/a.rs", "old_string": "a", "new_string": "b\nc"}, "_meta": {"claudeCode": {"toolName": "Edit"}}});
    let e = r.read("tool_call", &call);
    let tool = &e[0].data["tool"];
    assert_eq!(tool["target"], "src/a.rs");
    let diff = tool["result"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["type"] == "diff")
        .expect("a diff");
    assert_eq!(diff["added"], 2);
    assert_eq!(diff["removed"], 1);
}

#[test]
fn the_hubs_own_rows_pass_and_v1_rows_are_entries_already() {
    let mut r = LegacyReader::new("/w");
    let u = json!({"text": "hello", "via": "me"});
    assert_eq!(r.read("user", &u), vec![Entry::new("user", u.clone())]);
    let t = json!({"role": "assistant", "message": "m", "text": "x"});
    assert_eq!(r.read(TEXT, &t), vec![Entry::new(TEXT, t)]);
    // what no transcript shows
    assert!(
        r.read("user_message_chunk", &chunk("user_message_chunk", "x"))
            .is_empty()
    );
}

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
