//! Architectural fence: interactive session permissions have one ingress and one Hub ledger
//! writer (`PermissionResolver`), and one-shot ACP calls never run a second approval rule engine
//! or self-approve `session/request_permission`.

use std::path::Path;

fn strip_tests(src: &str) -> &str {
    src.split("#[cfg(test)]").next().unwrap_or(src)
}

fn scan_rs_files(dir: &Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap() {
        let p = entry.unwrap().path();
        if p.is_dir() {
            scan_rs_files(&p, out);
        } else if p.extension().is_some_and(|e| e == "rs") {
            out.push(p);
        }
    }
}

#[test]
fn session_permissions_have_one_ingress_and_one_hub_ledger_writer() {
    let crates = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let node = std::fs::read_to_string(crates.join("steve-node/src/actor.rs")).unwrap();
    assert_eq!(node.matches("\"session/request_permission\"").count(), 1);
    assert_eq!(node.matches("EventBody::PermissionRequested {").count(), 1);
    // The only selected ACP answer uses the committed, attempt-bound Hub resolution.
    assert_eq!(node.matches("permission::answer(").count(), 1);
    assert!(node.contains("permission::answer(&asked, Some(res.decision))"));

    // One-shot ACP calls refuse `session/request_permission` except for an explicit, static
    // Steve-origin read-only MCP tool allowlist (`allow_steve_mcp_tools`, used only for dispatch
    // and research read-only tools), and never run a second approval rule engine or persist
    // AllowAlways.
    let oneshot = std::fs::read_to_string(crates.join("steve-acp/src/oneshot.rs")).unwrap();
    let oneshot_prod = strip_tests(&oneshot);
    assert_eq!(
        oneshot_prod
            .matches("PermissionDecision::AllowOnce")
            .count(),
        1,
        "oneshot.rs may only AllowOnce in the narrow Steve MCP tool check"
    );
    assert!(
        oneshot_prod.contains("asked.fact.origin == ToolOrigin::Steve")
            && oneshot_prod.contains("asked.fact.category == ToolCategory::Steve")
            && (oneshot_prod.contains("req\n                    .allow_steve_mcp_tools")
                || oneshot_prod.contains("req.allow_steve_mcp_tools")),
        "oneshot.rs AllowOnce must remain gated to Steve-origin MCP tools in allow_steve_mcp_tools"
    );
    assert!(
        !oneshot_prod.contains("PermissionDecision::AllowAlways"),
        "oneshot.rs must never self-approve PermissionDecision::AllowAlways"
    );
    assert!(oneshot_prod.contains("PermissionDecision::Reject"));
    assert!(oneshot_prod.contains("steve_permission_refused_total"));
    assert!(oneshot_prod.contains("OneShotError::ToolRefused"));

    // Dispatch and research one-shot jobs may only allowlist their exact static read-only Steve
    // MCP tools (never mutating tools like `steve_onboard_*`, `steve_deliver`, or `steve_tool_call`).
    let node_job = std::fs::read_to_string(crates.join("steve-node/src/job.rs")).unwrap();
    assert!(strip_tests(&node_job).contains(
        r#"const DISPATCH_STEVE_TOOLS: &[&str] = &["steve_bots_list", "steve_nodes_list"];"#
    ));
    let research_acp =
        std::fs::read_to_string(crates.join("steve-node/src/research_acp.rs")).unwrap();
    let research_prod = strip_tests(&research_acp);
    let expected_research_tools = [
        "steve_knowledge_search",
        "steve_knowledge_read",
        "steve_debug",
        "steve_items_search",
        "steve_item_read",
        "steve_sessions_list",
        "steve_bots_list",
        "steve_nodes_list",
        "steve_im_thread",
        "steve_im_media",
    ];
    let research_const_block = research_prod
        .split("const RESEARCH_READ_ONLY_STEVE_TOOLS: &[&str] = &[")
        .nth(1)
        .and_then(|rest| rest.split("];").next())
        .expect("research_acp.rs must declare static RESEARCH_READ_ONLY_STEVE_TOOLS");
    let parsed_research_tools: Vec<&str> =
        research_const_block.split('"').skip(1).step_by(2).collect();
    assert_eq!(
        parsed_research_tools, expected_research_tools,
        "RESEARCH_READ_ONLY_STEVE_TOOLS must stay strictly limited to Steve's read-only research tools"
    );
    // Verify every entry matches Hub's read-only research / IM catalogs and never includes write tools.
    let hub_read_only_tools: std::collections::HashSet<String> =
        steve_hub::wire::research_mcp::descriptors()
            .into_iter()
            .filter(|t| t.read_only)
            .map(|t| t.name)
            .chain(
                steve_channels::im_conv_mcp::tools()
                    .into_iter()
                    .filter_map(|t| t["name"].as_str().map(str::to_owned)),
            )
            .collect();
    for tool in &parsed_research_tools {
        assert!(
            hub_read_only_tools.contains(*tool),
            "research allowlist tool {tool:?} is not a Hub read-only research tool"
        );
    }
    let hub_dispatch_tools: std::collections::HashSet<String> =
        steve_hub::wire::research_mcp::dispatch_descriptors()
            .into_iter()
            .filter(|t| t.read_only)
            .map(|t| t.name)
            .collect();
    assert_eq!(
        hub_dispatch_tools,
        std::collections::HashSet::from([
            "steve_bots_list".to_string(),
            "steve_nodes_list".to_string(),
        ])
    );
    for forbidden in [
        "steve_onboard_",
        "steve_deliver",
        "steve_tool_call",
        "steve_publish",
        "steve_secret_use",
    ] {
        assert!(
            !research_const_block.contains(forbidden),
            "RESEARCH_READ_ONLY_STEVE_TOOLS must never include mutating tool {forbidden}"
        );
    }

    // Across every crate's production source, `allow_tool` is completely gone and
    // `session/request_permission` only exists in the interactive session actor and oneshot rejector.
    let mut prod_files = Vec::new();
    for entry in std::fs::read_dir(crates).unwrap() {
        let src_dir = entry.unwrap().path().join("src");
        if src_dir.is_dir() {
            scan_rs_files(&src_dir, &mut prod_files);
        }
    }
    let mut req_perm_files = Vec::new();
    for file in &prod_files {
        let rel = file
            .strip_prefix(crates)
            .unwrap()
            .to_string_lossy()
            .replace('\\', "/");
        let text = std::fs::read_to_string(file).unwrap();
        let prod = strip_tests(&text);
        assert!(
            !text.contains("allow_tool"),
            "{rel} still references removed allow_tool rule engine"
        );
        if prod.contains("\"session/request_permission\"") {
            req_perm_files.push(rel);
        }
    }
    req_perm_files.sort();
    assert_eq!(
        req_perm_files,
        vec![
            "steve-acp/src/oneshot.rs".to_string(),
            "steve-node/src/actor.rs".to_string(),
        ],
        "unexpected production handler for session/request_permission"
    );

    fn writers(dir: &Path, call: &str) -> usize {
        let mut files = Vec::new();
        scan_rs_files(dir, &mut files);
        files
            .into_iter()
            .map(|p| std::fs::read_to_string(p).unwrap().matches(call).count())
            .sum()
    }
    let hub = crates.join("steve-hub/src");
    let perm = std::fs::read_to_string(hub.join("wire/node_runtime/perm.rs")).unwrap();
    assert_eq!(writers(&hub, "node_write::insert_permission_in("), 1);
    assert_eq!(writers(&hub, "steve_core::approval::resolve("), 1);
    assert!(perm.contains("Request::Permission(request)"));
    assert!(perm.contains("steve_core::approval::resolve("));
    assert!(perm.contains("node_write::insert_permission_in("));
}
