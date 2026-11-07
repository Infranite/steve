//! Public driver API fixtures: ACP partial updates and permission requests share one fact table.
use serde_json::{Value, json};
use steve_acp::normalize::{CommandFact, Fact, Normalizer, TEXT_MAX, call_id};
use steve_acp::permission::{answer, asked};
use steve_node_api::{
    Action, Digest, Epoch, EventBody, Part, PermissionDecision, ToolCategory, ToolOrigin,
    ToolPhase, ToolView,
};

fn norm() -> Normalizer {
    Normalizer::new("/work", "attempt", Epoch::new("process").unwrap())
}
fn update(n: &mut Normalizer, mut v: Value) -> Vec<Fact> {
    v["sessionUpdate"] = json!("tool_call_update");
    n.update(&v)
}
fn view(fs: &[Fact]) -> &ToolView {
    fs.iter()
        .find_map(|f| match f {
            Fact::Event(EventBody::ToolChanged { tool }) => Some(tool),
            _ => None,
        })
        .expect("tool projection")
}
fn has_diagnostic(fs: &[Fact], wanted: &str) -> bool {
    fs.iter()
        .any(|f| matches!(f, Fact::Event(EventBody::Diagnostic { code, .. }) if code == wanted))
}
fn request(tool: Value) -> Value {
    json!({"toolCall": tool, "options": [
        {"optionId":"yes", "kind":"allow_once"}, {"optionId":"no", "kind":"reject_once"}]})
}

#[test]
fn updates_merge_independent_fields_and_replace_opaque_inputs() {
    let mut n = norm();
    update(
        &mut n,
        json!({"toolCallId":"t", "title":"working", "status":"pending",
        "locations":[{"path":"/work/a"}], "_meta":{"claudeCode":{"toolName":"Read"}}}),
    );
    update(&mut n, json!({"toolCallId":"t", "kind":"execute"}));
    update(&mut n, json!({"toolCallId":"t", "name":"Bash"}));
    update(
        &mut n,
        json!({"toolCallId":"t", "rawInput":{"command":["printf", "%s", "a b"], "cwd":"../other"}}),
    );
    let fs = update(
        &mut n,
        json!({"toolCallId":"t", "title":"new", "status":"in_progress"}),
    );
    let f = n.fact("t").unwrap();
    assert_eq!(f.name, "Bash");
    assert_eq!(f.category, ToolCategory::Exec);
    assert_eq!(f.paths, ["/work/a"]);
    assert_eq!(f.cwd.as_deref(), Some("../other"));
    assert_eq!(
        f.command,
        Some(CommandFact::Argv(vec![
            "printf".into(),
            "%s".into(),
            "a b".into()
        ]))
    );
    assert_eq!(f.phase, ToolPhase::Running);
    assert!(
        matches!(view(&fs).action, Some(Action::Unknown { .. })),
        "argv/cwd must be bound by Node, not flattened into Exec"
    );
    update(
        &mut n,
        json!({"toolCallId":"t", "rawInput":{"command":"echo next"}}),
    );
    let f = n.fact("t").unwrap();
    assert_eq!(f.cwd, None, "rawInput replaces the whole object");
    assert_eq!(f.command, Some(CommandFact::Shell("echo next".into())));
    update(
        &mut n,
        json!({"toolCallId":"t", "name":null, "title":null, "rawInput":null}),
    );
    let f = n.fact("t").unwrap();
    assert_eq!(f.name, "Bash");
    assert_eq!(f.title, "new");
    assert_eq!(f.command, None);
}

#[test]
fn empty_collections_clear_but_absent_and_null_collections_keep() {
    let mut n = norm();
    let initial = json!({"toolCallId":"t", "locations":[{"path":"a"}],
        "rawOutput":"obsolete fallback", "content":[{"type":"content", "content":{"type":"text","text":"first"}}]});
    update(&mut n, initial);
    update(&mut n, json!({"toolCallId":"t", "status":"in_progress"}));
    assert_eq!(n.fact("t").unwrap().paths, ["a"]);
    let fs = update(
        &mut n,
        json!({"toolCallId":"t", "content":null, "locations":null}),
    );
    assert_eq!(
        view(&fs).result,
        [Part::Text {
            text: "first".into()
        }]
    );
    let fs = update(
        &mut n,
        json!({"toolCallId":"t", "content":[], "locations":[]}),
    );
    assert!(view(&fs).result.is_empty());
    assert_eq!(view(&fs).target, None);
    assert!(n.fact("t").unwrap().paths.is_empty());
    let fs = update(
        &mut n,
        json!({"toolCallId":"t", "content":[{"type":"content", "content":{"type":"text","text":"second"}}]}),
    );
    assert_eq!(
        view(&fs).result,
        [Part::Text {
            text: "second".into()
        }]
    );
}

#[test]
fn programmatic_names_control_identity_not_titles() {
    let mut n = norm();
    for title in [
        "Read steve_config.rs",
        "mcp__steve__steve_deliver",
        "steve_update",
        "Tool: steve/steve_item_read",
    ] {
        let fs = update(
            &mut n,
            json!({"toolCallId":title, "title":title, "kind":"read"}),
        );
        assert_eq!(view(&fs).origin, ToolOrigin::Harness);
        assert_eq!(view(&fs).category, ToolCategory::Read);
    }
    let fs = update(
        &mut n,
        json!({"toolCallId":"cc", "title":"unrelated", "_meta":{"claudeCode":{"toolName":"mcp__steve__steve_deliver"}}}),
    );
    assert_eq!(view(&fs).origin, ToolOrigin::Steve);
    let fs = update(
        &mut n,
        json!({"toolCallId":"cc", "name":"Read", "kind":"read"}),
    );
    assert_eq!(
        view(&fs).origin,
        ToolOrigin::Harness,
        "public name takes precedence over old metadata"
    );
    let fs = update(
        &mut n,
        json!({"toolCallId":"cx", "rawInput":{"server_name":"steve", "tool_name":"steve_update"}}),
    );
    assert_eq!(view(&fs).origin, ToolOrigin::Steve);
    assert_eq!(view(&fs).name, "steve_update");
    let fs = update(
        &mut n,
        json!({"toolCallId":"cx", "rawInput":{"server_name":"other", "tool_name":"steve_update"}}),
    );
    assert_eq!(view(&fs).origin, ToolOrigin::External);
}

#[test]
fn structured_mcp_invocations_keep_identity_and_arguments_across_updates() {
    // ACP adapters can report the MCP invocation envelope rather than a flattened tool name.
    // This is the codex-acp 0.16 notification shape; the title is display-only.
    let mut n = norm();
    let args = json!({"id":"ST-1"});
    let fs = update(
        &mut n,
        json!({
            "toolCallId":"mcp", "title":"Tool: steve/steve_item_read", "status":"in_progress",
            "rawInput":{"server":"steve", "tool":"steve_item_read", "arguments":args}
        }),
    );
    let f = n.fact("mcp").unwrap();
    assert_eq!(f.origin, ToolOrigin::Steve);
    assert_eq!(f.name, "mcp__steve__steve_item_read");
    assert!(
        matches!(&view(&fs).action, Some(Action::Tool { name, args: actual })
        if name == "steve_item_read" && actual == &args)
    );
    update(
        &mut n,
        json!({"toolCallId":"mcp", "status":"completed", "rawOutput":{"ok":true}}),
    );
    assert_eq!(n.fact("mcp").unwrap().name, "mcp__steve__steve_item_read");

    let fs = update(
        &mut n,
        json!({
            "toolCallId":"external", "title":"Tool: steve/steve_item_read",
            "rawInput":{"server":"other", "tool":"steve_item_read", "arguments":args}
        }),
    );
    assert_eq!(view(&fs).origin, ToolOrigin::External);
    assert!(!matches!(view(&fs).action, Some(Action::Tool { .. })));

    let fs = update(
        &mut n,
        json!({
            "toolCallId":"external-prefix", "title":"Tool: steve/steve_item_read",
            "rawInput":{"server":"other", "tool":"mcp__steve__steve_item_read", "arguments":args}
        }),
    );
    assert_eq!(
        view(&fs).origin,
        ToolOrigin::External,
        "the server controls the origin even if the tool name claims Steve"
    );
    let fs = update(
        &mut n,
        json!({
            "toolCallId":"external-explicit", "name":"mcp__steve__steve_item_read",
            "rawInput":{"server_name":"other", "tool_name":"steve_item_read"}
        }),
    );
    assert_eq!(view(&fs).origin, ToolOrigin::External);

    let fs = update(
        &mut n,
        json!({
            "toolCallId":"named", "name":"Read", "kind":"read",
            "rawInput":{"server":"steve", "tool":"steve_item_read", "arguments":args}
        }),
    );
    assert_eq!(
        view(&fs).origin,
        ToolOrigin::Harness,
        "an explicit tool name takes precedence over argument keys"
    );
    assert_eq!(view(&fs).name, "Read");

    let fs = update(
        &mut n,
        json!({
            "toolCallId":"partial", "title":"Tool: steve/steve_item_read",
            "rawInput":{"server":"steve", "arguments":args}
        }),
    );
    assert_ne!(
        view(&fs).origin,
        ToolOrigin::Steve,
        "an incomplete envelope cannot identify a Steve tool"
    );
}

#[test]
fn call_ids_have_disjoint_plain_and_hash_spaces() {
    assert_eq!(call_id("toolu_01").as_str(), "toolu_01");
    let long = "x".repeat(129);
    let raw_ids = ["", "a/b", "a?b", "a_b", "中文", long.as_str()];
    let mut ids = std::collections::HashSet::new();
    for raw in raw_ids {
        let id = call_id(raw);
        assert!(ids.insert(id.clone()));
        assert_eq!(id, call_id(raw));
        assert!(id.as_str().len() <= 128);
        if id.as_str() != raw {
            assert_ne!(
                call_id(id.as_str()),
                id,
                "a raw id that looks like a hash must be escaped"
            );
        }
    }
    assert_ne!(
        call_id(&format!("{}a", "x".repeat(128))),
        call_id(&format!("{}b", "x".repeat(128)))
    );
}

#[test]
fn partial_permissions_reuse_facts_and_the_exact_call_id() {
    let mut n = norm();
    let fs = update(
        &mut n,
        json!({"toolCallId":"tool /一", "name":"Bash", "kind":"execute", "title":"Run test",
        "rawInput":{"command":["sh","-c","echo a b"], "cwd":"/work/sub"}}),
    );
    let id = view(&fs).id.clone();
    let req = asked(
        &mut n,
        &json!(10),
        &request(json!({"toolCallId":"tool /一"})),
    );
    assert_eq!(req.fact.call, id);
    assert_eq!(view(&req.updates).id, id);
    assert_eq!(req.fact.cwd.as_deref(), Some("/work/sub"));
    assert_eq!(req.title, "Run test");
    assert!(matches!(req.fact.command, Some(CommandFact::Argv(_))));
    let old = req.fact.clone();
    let req = asked(
        &mut n,
        &json!(11),
        &request(json!({"toolCallId":"tool /一", "rawInput":{"command":"next"}})),
    );
    assert_eq!(req.fact.call, id);
    assert_eq!(req.fact.command, Some(CommandFact::Shell("next".into())));
    assert_eq!(
        old.cwd.as_deref(),
        Some("/work/sub"),
        "Asked owns a snapshot"
    );
    let first = asked(
        &mut n,
        &json!(12),
        &request(json!({"toolCallId":"new", "kind":"read", "rawInput":{"path":"relative"}})),
    );
    assert_eq!(first.fact.paths, ["relative"]);
    assert_eq!(n.fact("new"), Some(&first.fact));
}

#[test]
fn persistent_reject_is_never_offered_even_when_first() {
    let mut n = norm();
    let mut p = request(json!({"toolCallId":"t"}));
    p["options"] = json!([
        {"optionId":"forever","kind":"reject_always"},
        {"optionId":"once","kind":"reject_once"}
    ]);
    let req = asked(&mut n, &json!(1), &p);
    assert_eq!(req.choices, [(PermissionDecision::Reject, "once".into())]);
    assert_eq!(
        answer(&req, Some(PermissionDecision::Reject))["outcome"]["optionId"],
        "once"
    );
    p["options"] = json!([{"optionId":"forever","kind":"reject_always"}]);
    let req = asked(&mut n, &json!(2), &p);
    assert!(req.choices.is_empty());
    assert_eq!(
        answer(&req, Some(PermissionDecision::Reject))["outcome"]["outcome"],
        "cancelled"
    );
}

#[test]
fn unsupported_content_is_visible_without_leaking_payloads() {
    let mut n = norm();
    for kind in ["image", "resource", "resource_link", "audio"] {
        let content = json!({"type":kind,"data":"private bytes", "uri":"private uri"});
        let fs = n.update(&json!({"sessionUpdate":"agent_message_chunk", "content":content}));
        assert!(has_diagnostic(&fs, "unsupported_content"));
        let fs = update(
            &mut n,
            json!({"toolCallId":kind, "content":[{"type":"content", "content":content}]}),
        );
        assert!(has_diagnostic(&fs, "unsupported_content"));
        for f in fs {
            if let Fact::Event(EventBody::Diagnostic { safe_text, .. }) = f {
                assert!(!safe_text.contains("private"));
            }
        }
    }
}

#[test]
fn previews_are_marked_but_change_digests_and_paths_are_lossless() {
    let mut n = norm();
    let body = "界".repeat(TEXT_MAX + 100);
    let input = json!({"file_path":"/outside/a", "old_string":"old", "new_string":body});
    let fs = update(
        &mut n,
        json!({"toolCallId":"t", "kind":"edit", "rawInput":input}),
    );
    assert!(has_diagnostic(&fs, "truncated"));
    let fact = n.fact("t").unwrap();
    assert_eq!(fact.paths, ["/outside/a"]);
    assert_eq!(
        fact.change,
        Some(steve_node_api::digest_of(&input).unwrap())
    );
    assert_eq!(fact.raw["rawInput"], input);
    let before = fact.change.clone();
    update(
        &mut n,
        json!({"toolCallId":"t", "rawInput":{"file_path":"/outside/a", "old_string":"old", "new_string":format!("{body}different tail")}}),
    );
    assert_ne!(n.fact("t").unwrap().change, before);
    let fs = update(
        &mut n,
        json!({"toolCallId":"out", "rawOutput":"x".repeat(TEXT_MAX+1)}),
    );
    assert!(has_diagnostic(&fs, "truncated"));
    assert_eq!(
        n.fact("out").unwrap().raw["rawOutput"]
            .as_str()
            .unwrap()
            .len(),
        TEXT_MAX + 1
    );
    // Full content diffs also hash the original texts, not their previews.
    let diffs = json!([{"type":"diff","path":"a","oldText":"before","newText":body}]);
    let fs = update(&mut n, json!({"toolCallId":"diff", "content":diffs}));
    assert!(has_diagnostic(&fs, "truncated"));
    assert_eq!(n.fact("diff").unwrap().paths, ["a"]);
    assert_eq!(
        n.fact("diff").unwrap().change,
        Some(steve_node_api::digest_of(&diffs).unwrap())
    );
}

#[test]
fn raw_input_distinguishes_argv_boundaries_and_shell() {
    let mut n = norm();
    update(
        &mut n,
        json!({"toolCallId":"a", "kind":"execute", "rawInput":{"command":["echo","a b"]}}),
    );
    update(
        &mut n,
        json!({"toolCallId":"b", "kind":"execute", "rawInput":{"command":["echo","a","b"]}}),
    );
    update(
        &mut n,
        json!({"toolCallId":"c", "kind":"execute", "rawInput":{"command":"echo a b"}}),
    );
    let a = n.fact("a").unwrap();
    let b = n.fact("b").unwrap();
    let c = n.fact("c").unwrap();
    assert_ne!(a.command, b.command);
    assert_ne!(b.command, c.command);
    assert_ne!(
        Digest::sha256(a.raw.to_string().as_bytes()),
        Digest::sha256(b.raw.to_string().as_bytes())
    );
}

#[test]
fn cleared_content_clears_failed_output_and_diff_paths() {
    let mut n = norm();
    update(
        &mut n,
        json!({"toolCallId":"t", "status":"failed", "rawOutput":"old error",
        "content":[{"type":"content", "content":{"type":"text", "text":"new error"}},
            {"type":"diff", "path":"/work/file", "oldText":"a", "newText":"b"}]}),
    );
    assert_eq!(n.fact("t").unwrap().error.as_deref(), Some("new error"));
    let fs = update(&mut n, json!({"toolCallId":"t", "content":[]}));
    assert!(view(&fs).result.is_empty());
    assert_eq!(view(&fs).error, None);
    assert_eq!(view(&fs).target, None);
}

#[test]
fn long_targets_are_marked_without_cutting_driver_facts() {
    let mut n = norm();
    let command = "x".repeat(300);
    let fs = update(
        &mut n,
        json!({"toolCallId":"t", "kind":"execute", "rawInput":{"command":command}}),
    );
    assert!(has_diagnostic(&fs, "truncated"));
    assert_eq!(
        n.fact("t").unwrap().command,
        Some(CommandFact::Shell(command))
    );
}

#[test]
fn permission_update_closes_the_message_and_carries_content_diagnostics() {
    let mut n = norm();
    n.update(
        &json!({"sessionUpdate":"agent_message_chunk", "content":{"type":"text", "text":"before"}}),
    );
    let req = asked(
        &mut n,
        &json!(1),
        &request(json!({"toolCallId":"t",
        "content":[{"type":"content", "content":{"type":"image","data":"bytes"}}]})),
    );
    assert!(matches!(
        req.updates.first(),
        Some(Fact::Event(EventBody::MessageCompleted { .. }))
    ));
    assert!(has_diagnostic(&req.updates, "unsupported_content"));
    assert_eq!(req.fact.call, view(&req.updates).id);
    let next = n.update(
        &json!({"sessionUpdate":"agent_message_chunk", "content":{"type":"text", "text":"after"}}),
    );
    assert!(
        matches!(next.first(), Some(Fact::Event(EventBody::MessageStarted { message, .. })) if message == "attempt.m2")
    );
}
