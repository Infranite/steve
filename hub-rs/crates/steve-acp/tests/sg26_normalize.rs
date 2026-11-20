//! Old-implementation regression corpus for final accounting and whole tool projections.
use serde_json::{Value, json};
use steve_acp::normalize::{CommandFact, Fact, Normalizer, TEXT_MAX, ToolFact};
use steve_node_api::{Epoch, ToolCategory, ToolPhase};
fn facts(rows: Vec<Fact>) -> Value {
    Value::Array(
        rows.into_iter()
            .map(|f| match f {
                Fact::Event(e) => json!({"event":e}),
                Fact::Model(v) => json!({"model":v}),
                Fact::Effort(v) => json!({"effort":v}),
                Fact::Nothing => json!({"nothing":true}),
            })
            .collect(),
    )
}
fn tool(t: &ToolFact) -> Value {
    json!({"raw_id":t.raw_id,"call":t.call,"name":t.name,"category":t.category,"origin":t.origin,"title":t.title,"phase":t.phase,"paths":t.paths,"cwd":t.cwd,"command":match &t.command{Some(CommandFact::Shell(s))=>json!({"shell":s}),Some(CommandFact::Argv(v))=>json!({"argv":v}),None=>Value::Null},"query":t.query,"url":t.url,"change":t.change,"result":t.result,"error":t.error,"raw":t.raw})
}
fn norm(cache: bool) -> Normalizer {
    Normalizer::new("/work", "sg26-attempt", Epoch::new("sg26-meter").unwrap())
        .input_includes_cache(cache)
}
fn usage_cases() -> Vec<Value> {
    let mut out = Vec::new();
    let variants = vec![
        (
            "full",
            json!({"inputTokens":120,"outputTokens":30,"cachedReadTokens":40,"cachedWriteTokens":5,"reasoningTokens":8}),
        ),
        (
            "snake",
            json!({"input_tokens":120,"output_tokens":30,"cached_read_tokens":40,"cached_write_tokens":5,"reasoning_tokens":8}),
        ),
        (
            "provider-alias",
            json!({"inputTokens":120,"outputTokens":30,"cachedInputTokens":40,"cacheWriteInputTokens":5,"thoughtTokens":8}),
        ),
        (
            "anthropic-alias",
            json!({"inputTokens":120,"outputTokens":30,"cache_read_input_tokens":40,"cache_creation_input_tokens":5,"reasoningOutputTokens":8}),
        ),
        (
            "thought-snake",
            json!({"inputTokens":120,"outputTokens":30,"thought_tokens":8}),
        ),
        (
            "alias-precedence",
            json!({"inputTokens":120,"input_tokens":999,"outputTokens":30,"output_tokens":999,"cachedReadTokens":40,"cachedInputTokens":99,"reasoningTokens":8,"thoughtTokens":99}),
        ),
        (
            "cache-over-input",
            json!({"inputTokens":3,"outputTokens":2,"cachedReadTokens":9,"reasoningTokens":4}),
        ),
        (
            "reasoning-without-output",
            json!({"inputTokens":12,"reasoningTokens":8}),
        ),
        (
            "zero",
            json!({"inputTokens":0,"outputTokens":0,"cachedReadTokens":0,"cachedWriteTokens":0,"reasoningTokens":0}),
        ),
        ("unknown", json!({})),
        (
            "invalid",
            json!({"inputTokens":-1,"outputTokens":2.5,"cachedReadTokens":"7","reasoningTokens":null}),
        ),
    ];
    for cache in [false, true] {
        for (name, u) in &variants {
            let mut n = norm(cache);
            let r = n.prompt_response(&json!({"usage":u}));
            out.push(json!({"name":format!("usage/{name}/cache={cache}"),"facts":facts(r),"baseline":n.cumulative_prompt_tokens()}));
        }
    }
    for (name, r) in [
        (
            "meta",
            json!({"_meta":{"input_tokens":12,"output_tokens":3}}),
        ),
        (
            "usage-over-meta",
            json!({"usage":{},"_meta":{"inputTokens":999}}),
        ),
        (
            "invalid-usage-meta",
            json!({"usage":[],"_meta":{"inputTokens":12}}),
        ),
        ("missing", json!({})),
        (
            "quota",
            json!({"usage":{"inputTokens":3},"_meta":{"quota":{"model_usage":[{"model":"m1","token_count":{"inputTokens":7,"outputTokens":2}},{"model":"m2","token_count":{}},{"model":"no-count"},{"token_count":{"outputTokens":0}}]}}}),
        ),
    ] {
        let mut n = norm(false);
        out.push(json!({"name":name,"facts":facts(n.prompt_response(&r))}));
    }
    for cache in [false, true] {
        for stream in [false, true] {
            let mut n = norm(cache).prompt_cumulative_baseline((100, 20, 40, 3, 2));
            let mut steps = Vec::new();
            if stream {
                steps.push(facts(n.update(&json!({"sessionUpdate":"agent_message_chunk","messageId":"answer","content":{"type":"text","text":"hello"},"_meta":{"usage":{"inputTokens":30,"outputTokens":4,"cachedReadTokens":10,"cachedWriteTokens":1,"reasoningTokens":2}}}))));
            }
            for r in [
                json!({"usage":{"inputTokens":180,"outputTokens":40,"cachedReadTokens":60,"cachedWriteTokens":7,"reasoningTokens":9}}),
                json!({"usage":{"inputTokens":180,"outputTokens":40,"cachedReadTokens":60,"cachedWriteTokens":7,"reasoningTokens":9}}),
                json!({"usage":{"inputTokens":10,"outputTokens":2,"cachedReadTokens":1,"reasoningTokens":1}}),
                json!({"usage":{"inputTokens":200,"outputTokens":44,"cachedReadTokens":65}}),
            ] {
                steps.push(facts(n.prompt_response(&r)));
            }
            out.push(json!({"name":format!("cumulative/cache={cache}/stream={stream}"),"steps":steps,"baseline":n.cumulative_prompt_tokens()}));
        }
    }
    let mut n = norm(false);
    let mut steps = Vec::new();
    for text in ["one", "two"] {
        steps.push(facts(n.update(&json!({"sessionUpdate":"agent_message_chunk","content":{"type":"text","text":text},"_meta":{"usage":{"inputTokens":3,"outputTokens":2,"cachedReadTokens":1}}}))));
    }
    steps.push(facts(n.prompt_response(
        &json!({"usage":{"inputTokens":10,"outputTokens":7,"cachedReadTokens":3}}),
    )));
    steps.push(facts(n.prompt_response(
        &json!({"usage":{"inputTokens":10,"outputTokens":7,"cachedReadTokens":3}}),
    )));
    out.push(json!({"name":"stream-subtraction-resets-after-final","steps":steps}));
    out
}
fn tool_cases() -> Vec<(&'static str, Value)> {
    vec![
        (
            "shell",
            json!({"name":"Bash","kind":"execute","rawInput":{"command":"printf hello","cwd":"../other"},"status":"completed","rawOutput":"ok"}),
        ),
        (
            "argv",
            json!({"name":"Bash","kind":"execute","rawInput":{"command":["printf","%s","a b"],"cwd":"/work"}}),
        ),
        (
            "read",
            json!({"name":"Read","kind":"read","rawInput":{"file_path":"/work/a"},"locations":[{"path":"/work/a"},{"path":"/work/b"}]}),
        ),
        (
            "all-paths",
            json!({"kind":"edit","rawInput":{"path":"a","old_path":"old","new_path":"new","source":"source","destination":"dest","dir_path":"dir","directory":"directory","root":"root","target_directory":"target","file":"file","notebook_path":"note","paths":["a","extra"],"files":["file","last"]},"content":[{"type":"diff","path":"diff","oldText":"old","newText":"new"}]}),
        ),
        (
            "edit-input",
            json!({"name":"Edit","kind":"edit","rawInput":{"file_path":"/work/a","old_string":"a\nb","new_string":"b\nc"}}),
        ),
        (
            "edits",
            json!({"name":"Edit","kind":"edit","rawInput":{"path":"a","edits":[{"old_string":"a","new_string":"b"},{"old_string":"c","new_string":"d"}]}}),
        ),
        (
            "write",
            json!({"name":"Write","kind":"edit","rawInput":{"path":"/work/a","content":"written\n"}}),
        ),
        (
            "failed-content",
            json!({"name":"Read","kind":"read","status":"failed","content":[{"type":"content","content":{"type":"text","text":"first"}},{"type":"content","content":{"type":"text","text":"second"}}],"rawOutput":"ignored"}),
        ),
        (
            "failed-object",
            json!({"kind":"other","status":"failed","rawOutput":{"reason":"bad"}}),
        ),
        (
            "null-output",
            json!({"kind":"read","rawOutput":null,"rawInput":{"old_string":"unused","new_string":"unused"}}),
        ),
        (
            "empty-content-authoritative",
            json!({"kind":"edit","content":[],"rawOutput":"ignored","rawInput":{"old_string":"old","new_string":"new"}}),
        ),
        (
            "diff",
            json!({"name":"Edit","kind":"edit","content":[{"type":"diff","path":"/work/a","oldText":"old\ntext","newText":"new\ntext\nlast"}]}),
        ),
        (
            "bad-diff",
            json!({"kind":"edit","content":[{"type":"diff","path":23,"newText":7},{"type":"unexpected","value":"opaque"}]}),
        ),
        (
            "mcp-steve",
            json!({"name":"unrelated","rawInput":{"server_name":"steve","query":"text"},"kind":"search"}),
        ),
        (
            "mcp-external",
            json!({"name":"mcp__steve__read","rawInput":{"server_name":"outside"},"kind":"fetch"}),
        ),
        (
            "mcp-named",
            json!({"name":"mcp__steve__read","kind":"read"}),
        ),
        (
            "mcp-other",
            json!({"name":"mcp__outside__read","kind":"search","rawInput":{"pattern":"first","query":"second","url":"https://example.invalid/"}}),
        ),
        (
            "claude-meta",
            json!({"_meta":{"claudeCode":{"toolName":"Read"}},"title":"display only","kind":"read"}),
        ),
        (
            "unknown-phase",
            json!({"name":"x","kind":"future","status":"future"}),
        ),
        (
            "long-title-target",
            json!({"title":"t".repeat(201),"locations":[{"path":format!("/work/{}","x".repeat(210))}]}),
        ),
        (
            "long-output-error",
            json!({"status":"failed","rawOutput":"字".repeat(TEXT_MAX+1)}),
        ),
        (
            "long-diff",
            json!({"kind":"edit","content":[{"type":"diff","path":"a","oldText":"old\n".repeat(61),"newText":"new\n".repeat(62)}]}),
        ),
        (
            "long-input-edits",
            json!({"kind":"edit","rawInput":{"path":"a","edits":[{"old_string":"x\n".repeat(61),"new_string":"y".repeat(TEXT_MAX+1)}]}}),
        ),
    ]
}
fn corpus() -> Vec<Value> {
    let mut out = usage_cases();
    for (name, raw) in tool_cases() {
        let mut n = norm(false);
        let mut raw = raw;
        raw["sessionUpdate"] = json!("tool_call");
        raw["toolCallId"] = json!("sg26-tool");
        let rows = n.update(&raw);
        out.push(json!({"name":format!("tool/{name}"),"facts":facts(rows),"tool":n.fact("sg26-tool").map(tool)}));
    }
    out
}
#[test]
fn sg26_normalize_outputs_match_the_old_implementation_corpus() {
    let actual = corpus();
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sg26/normalize.json");
    if std::env::var_os("SG26_GENERATE_GOLDENS").is_some() {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(path)
            .expect("first generation only");
        f.write_all(&serde_json::to_vec_pretty(&actual).unwrap())
            .unwrap();
        return;
    }
    let expected: Vec<Value> = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(actual.len(), expected.len());
    for (a, e) in actual.iter().zip(expected) {
        assert_eq!(a, &e, "{}", a["name"]);
    }
}
#[test]
fn sg26_edit_identity_uses_full_input_and_updates_replace_only_supplied_fields() {
    let mut n = norm(false);
    let base = "line\n".repeat(61);
    let make = |tail: &str| json!({"sessionUpdate":"tool_call_update","toolCallId":"edit","name":"Edit","kind":"edit","rawInput":{"path":"a","old_string":base,"new_string":format!("{base}{tail}")}});
    n.update(&make("one"));
    let one = n.fact("edit").unwrap().clone();
    n.update(&make("two"));
    let two = n.fact("edit").unwrap().clone();
    assert_ne!(one.change, two.change);
    assert_eq!(
        one.result, two.result,
        "the differing tail is outside the preview"
    );
    n.update(&json!({"sessionUpdate":"tool_call_update","toolCallId":"edit","status":"failed","content":[],"locations":[]}));
    let t = n.fact("edit").unwrap();
    assert!(t.result.is_empty());
    assert_eq!(t.raw["rawInput"]["new_string"], format!("{base}two"));
}

#[test]
fn sg26_tool_categories_and_phases_keep_programmatic_routing_boundaries() {
    for (kind, category) in [
        ("read", ToolCategory::Read),
        ("edit", ToolCategory::Edit),
        ("delete", ToolCategory::Delete),
        ("move", ToolCategory::Move),
        ("search", ToolCategory::Search),
        ("execute", ToolCategory::Exec),
        ("think", ToolCategory::Think),
        ("fetch", ToolCategory::Fetch),
        ("future", ToolCategory::Other),
    ] {
        for (status, phase) in [
            ("pending", ToolPhase::Pending),
            ("in_progress", ToolPhase::Running),
            ("completed", ToolPhase::Succeeded),
            ("failed", ToolPhase::Failed),
            ("future", ToolPhase::Pending),
        ] {
            let mut n = norm(false);
            n.update(&json!({"sessionUpdate":"tool_call","toolCallId":"kinds","name":"plain_tool","kind":kind,"status":status,"rawInput":{}}));
            let f = n.fact("kinds").unwrap();
            assert_eq!(f.category, category, "{kind}/{status}");
            assert_eq!(f.phase, phase, "{kind}/{status}");
            assert_eq!(f.origin, steve_node_api::ToolOrigin::Harness);
        }
    }
}
