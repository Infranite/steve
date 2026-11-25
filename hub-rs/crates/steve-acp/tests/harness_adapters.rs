//! Contract, unit, real-recording replay, and ignored live smoke tests for `steve_acp::adapter`.

use std::collections::HashMap;
use std::path::Path;
use std::time::Duration;

use serde_json::{Value, json};
use steve_acp::adapter::{capabilities_for, for_harness};
use steve_acp::adapters::ProbeStatus;
use steve_acp::normalize::{Fact, Normalizer, call_id};
use steve_node_api::{
    BackgroundTaskCapability, Epoch, EventBody, ResumeCapability, SubagentCapability,
    SubtaskOrigin, SubtaskStatus, SubtaskView, ToolView, UpgradeCapability, UsageAttribution,
    UsageReport, UsageScope, merged_usage,
};

fn meter() -> Epoch {
    Epoch::new("wf1.acp").unwrap()
}

fn bill(reports: &[UsageReport], fallback_model: &str) -> Vec<UsageReport> {
    merged_usage(reports.to_vec(), fallback_model)
}

struct ReplayOutput {
    root_session: String,
    subtasks: HashMap<String, SubtaskView>,
    subtask_events: Vec<SubtaskView>,
    tools: HashMap<String, ToolView>,
    usage: Vec<UsageReport>,
}

fn replay_fixture(harness: &str, fixture_name: &str) -> ReplayOutput {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/wf1")
        .join(fixture_name);
    let raw = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read fixture {}: {e}", path.display()));
    let mut norm = Normalizer::new("/tmp/probe", "wf1", meter()).for_harness(harness);
    let mut root_session = String::new();
    let mut subtasks = HashMap::new();
    let mut subtask_events = Vec::new();
    let mut tools = HashMap::new();
    let mut usage = Vec::new();

    let mut collect = |facts: Vec<Fact>| {
        for fact in facts {
            let Fact::Event(body) = fact else { continue };
            match body {
                EventBody::SubtaskChanged { subtask } => {
                    subtask_events.push(subtask.clone());
                    subtasks.insert(subtask.id.as_str().to_string(), subtask);
                }
                EventBody::ToolChanged { tool } => {
                    tools.insert(tool.id.as_str().to_string(), tool);
                }
                EventBody::Usage { report } => usage.push(report),
                _ => {}
            }
        }
    };

    for (idx, line) in raw.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let frame: Value = serde_json::from_str(line)
            .unwrap_or_else(|e| panic!("{fixture_name}:{}: invalid json: {e}", idx + 1));
        match frame["kind"].as_str().unwrap_or("") {
            "session_new" => {
                root_session = frame["sessionId"].as_str().unwrap_or("").to_string();
                norm.set_root_session(&root_session);
            }
            "update" => {
                let sid = frame["sessionId"].as_str();
                collect(norm.update_for_session(sid, &frame["update"]));
            }
            "prompt_response" => {
                collect(norm.prompt_response(&frame["result"]));
            }
            other => panic!("unexpected fixture frame kind {other} in {fixture_name}"),
        }
    }
    collect(norm.finish());

    ReplayOutput {
        root_session,
        subtasks,
        subtask_events,
        tools,
        usage,
    }
}

/* ───────────────────────── Capabilities & Handshake ───────────────────────── */

#[test]
fn verified_capabilities_match_empirical_matrix() {
    let claude = capabilities_for("claude-code");
    assert!(claude.verified);
    assert_eq!(claude.subagents, SubagentCapability::AcpRfd);
    assert!(claude.subagent_cancel);
    assert_eq!(
        claude.background_tasks,
        BackgroundTaskCapability::ToolFallback
    );
    assert!(claude.child_tool_nesting);
    assert_eq!(claude.usage_attribution, UsageAttribution::TurnAndModelAll);
    assert_eq!(claude.resume, ResumeCapability::SessionLoad);
    assert_eq!(claude.upgrade, UpgradeCapability::ManagedNpm);

    let codex = capabilities_for("codex");
    assert!(codex.verified);
    assert_eq!(codex.subagents, SubagentCapability::AcpRfd);
    assert!(codex.subagent_cancel);
    assert_eq!(codex.background_tasks, BackgroundTaskCapability::None);
    assert!(codex.child_tool_nesting);
    assert_eq!(codex.usage_attribution, UsageAttribution::TurnAndModelAll);
    assert_eq!(codex.resume, ResumeCapability::SessionLoad);
    assert_eq!(codex.upgrade, UpgradeCapability::ManagedNpm);

    let agy = capabilities_for("antigravity");
    assert!(agy.verified);
    assert_eq!(agy.subagents, SubagentCapability::ToolFallback);
    assert!(!agy.subagent_cancel);
    assert_eq!(agy.background_tasks, BackgroundTaskCapability::ToolFallback);
    assert!(!agy.child_tool_nesting);
    assert_eq!(agy.usage_attribution, UsageAttribution::TurnLevel);
    assert_eq!(agy.resume, ResumeCapability::SessionLoad);
    assert_eq!(agy.upgrade, UpgradeCapability::ExternalManaged);

    let kimi = capabilities_for("kimi-code");
    assert!(kimi.verified);
    assert_eq!(kimi.subagents, SubagentCapability::ToolFallback);
    assert!(!kimi.subagent_cancel);
    assert_eq!(
        kimi.background_tasks,
        BackgroundTaskCapability::ToolFallback
    );
    assert!(!kimi.child_tool_nesting);
    assert_eq!(kimi.usage_attribution, UsageAttribution::ContextOnly);
    assert_eq!(kimi.resume, ResumeCapability::SessionLoad);
    assert_eq!(kimi.upgrade, UpgradeCapability::ExternalManaged);

    let pi = capabilities_for("pi");
    assert!(pi.verified);
    assert_eq!(pi.subagents, SubagentCapability::ToolFallback);
    assert!(!pi.subagent_cancel);
    assert_eq!(pi.background_tasks, BackgroundTaskCapability::ToolFallback);
    assert!(!pi.child_tool_nesting);
    assert_eq!(pi.usage_attribution, UsageAttribution::SubtaskScoped);
    assert_eq!(pi.resume, ResumeCapability::SessionLoad);
    assert_eq!(pi.upgrade, UpgradeCapability::ManagedNpm);

    for unverified in ["gemini", "opencode", "qwen-code", "custom-harness"] {
        let g = capabilities_for(unverified);
        assert!(!g.verified, "{unverified} must be unverified");
        assert_eq!(g.subagents, SubagentCapability::None);
        assert!(!g.subagent_cancel);
        assert_eq!(g.background_tasks, BackgroundTaskCapability::None);
        assert!(!g.child_tool_nesting);
        assert_eq!(g.upgrade, UpgradeCapability::Manual);
    }

    // Adapter update_components hook delegates to steve_acp::adapters::update_components (single source of truth)
    let cfg = steve_acp::AcpConfig::from_env();
    let claude_components: Vec<&str> = for_harness("claude-code")
        .update_components(&cfg)
        .into_iter()
        .map(|c| c.id)
        .collect();
    assert!(claude_components.contains(&"cli"));
    assert!(claude_components.contains(&"adapter"));
}

#[test]
fn client_capabilities_negotiate_rfd_without_jetbrains_air() {
    for id in ["claude-code", "codex"] {
        let mut caps = json!({});
        for_harness(id).configure_client_capabilities(&mut caps);
        assert_eq!(
            caps["subagents"],
            json!({}),
            "{id} must negotiate subagents"
        );
        assert!(
            caps.pointer("/_meta/jetbrains/air").is_none(),
            "{id} must not set _meta.jetbrains.air (disables per-child cancel)"
        );
    }
    for id in ["antigravity", "kimi-code", "pi", "gemini"] {
        let mut caps = json!({});
        for_harness(id).configure_client_capabilities(&mut caps);
        assert!(
            caps.get("subagents").is_none(),
            "{id} should not advertise RFD subagents"
        );
    }
}

/* ───────────────────────── Unit & Contract Paths ───────────────────────── */

#[test]
fn claude_code_rfd_cancel_fallback_and_background_tasks() {
    let mut norm = Normalizer::new("/tmp/repo", "t1", meter())
        .for_harness("claude-code")
        .root_session("root-1");

    // 1. Standard ACP RFD subagent + cancel request + child tool nesting
    let facts = norm.update_for_session(
        Some("root-1"),
        &json!({
            "sessionUpdate": "subagent_update",
            "sessionId": "child-a",
            "title": "Check auth",
            "description": "Explore",
            "capabilities": { "cancel": {} },
            "state": { "state": "running" }
        }),
    );
    let sub = facts
        .iter()
        .find_map(|f| match f {
            Fact::Event(EventBody::SubtaskChanged { subtask }) => Some(subtask.clone()),
            _ => None,
        })
        .expect("RFD subagent_update emits SubtaskChanged");
    assert_eq!(sub.id.as_str(), "child-a");
    assert_eq!(sub.origin, SubtaskOrigin::Subagent);
    assert_eq!(sub.status, SubtaskStatus::Running);
    assert!(sub.can_cancel);
    assert_eq!(sub.actor.as_deref(), Some("Explore"));

    let cancel = norm
        .cancel_subtask_request(&sub.id)
        .expect("cancel request");
    assert_eq!(cancel.0, "session/cancel");
    assert_eq!(cancel.1, json!({ "sessionId": "child-a" }));

    let child_facts = norm.update_for_session(
        Some("child-a"),
        &json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "tc-read-1",
            "name": "Read",
            "kind": "read",
            "title": "Read lib.rs",
            "status": "completed",
            "rawInput": { "file_path": "/tmp/repo/lib.rs" }
        }),
    );
    let child_tool = child_facts
        .iter()
        .find_map(|f| match f {
            Fact::Event(EventBody::ToolChanged { tool }) => Some(tool),
            _ => None,
        })
        .expect("child tool emitted");
    assert_eq!(
        child_tool.parent.as_ref().map(|p| p.as_str()),
        Some("child-a")
    );

    // 2. Background Bash + Monitor + TaskStop
    let bg_facts = norm.update_for_session(
        Some("root-1"),
        &json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "tc-bash-bg",
            "name": "Bash",
            "kind": "execute",
            "title": "cargo test",
            "status": "completed",
            "rawInput": { "command": "cargo test", "description": "Run tests", "run_in_background": true },
            "_meta": { "claudeCode": { "toolName": "Bash", "toolResponse": { "backgroundTaskId": "bg-42" } } }
        }),
    );
    let bg = bg_facts
        .iter()
        .find_map(|f| match f {
            Fact::Event(EventBody::SubtaskChanged { subtask }) => Some(subtask),
            _ => None,
        })
        .expect("background Bash emits SubtaskChanged");
    assert_eq!(bg.origin, SubtaskOrigin::BackgroundTask);
    assert_eq!(bg.status, SubtaskStatus::Running);

    let stop_facts = norm.update_for_session(
        Some("root-1"),
        &json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "tc-stop",
            "name": "TaskStop",
            "kind": "other",
            "title": "Stop background task",
            "status": "completed",
            "rawInput": { "task_id": "bg-42" }
        }),
    );
    let stopped = stop_facts
        .iter()
        .find_map(|f| match f {
            Fact::Event(EventBody::SubtaskChanged { subtask }) => Some(subtask),
            _ => None,
        })
        .expect("TaskStop transitions background task");
    assert_eq!(stopped.status, SubtaskStatus::Cancelled);

    // Monitor tool emits background task
    let mon_facts = norm.update_for_session(
        Some("root-1"),
        &json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "tc-mon",
            "name": "Monitor",
            "kind": "execute",
            "title": "Watch build",
            "status": "in_progress",
            "rawInput": { "command": "tail -f build.log", "description": "Watch build log" }
        }),
    );
    let mon = mon_facts
        .iter()
        .find_map(|f| match f {
            Fact::Event(EventBody::SubtaskChanged { subtask }) => Some(subtask),
            _ => None,
        })
        .expect("Monitor emits BackgroundTask");
    assert_eq!(mon.origin, SubtaskOrigin::BackgroundTask);
    assert_eq!(mon.status, SubtaskStatus::Running);
}

#[test]
fn codex_rfd_incremental_patches_terminal_delta_and_collab_fallback() {
    let mut norm = Normalizer::new("/tmp/repo", "t2", meter())
        .for_harness("codex")
        .root_session("root-c");

    // Incremental RFD patch sequence
    norm.update_for_session(
        Some("root-c"),
        &json!({
            "sessionUpdate": "subagent_update",
            "sessionId": "root-c:subagent:thr-1",
            "title": "Count lines",
            "capabilities": {}
        }),
    );
    norm.update_for_session(
        Some("root-c"),
        &json!({
            "sessionUpdate": "subagent_update",
            "sessionId": "root-c:subagent:thr-1",
            "state": { "state": "running" }
        }),
    );
    let cap_facts = norm.update_for_session(
        Some("root-c"),
        &json!({
            "sessionUpdate": "subagent_update",
            "sessionId": "root-c:subagent:thr-1",
            "capabilities": { "cancel": {} }
        }),
    );
    let sub = cap_facts
        .iter()
        .find_map(|f| match f {
            Fact::Event(EventBody::SubtaskChanged { subtask }) => Some(subtask),
            _ => None,
        })
        .expect("subagent updated with cancel cap");
    assert_eq!(sub.title, "Count lines");
    assert!(sub.can_cancel);
    assert!(norm.cancel_subtask_request(&sub.id).is_some());

    // Child tool call with _meta.terminal_output_delta.data
    let child_facts = norm.update_for_session(
        Some("root-c:subagent:thr-1"),
        &json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "exec-1",
            "name": "exec_command",
            "kind": "execute",
            "title": "wc -l hello.txt",
            "status": "completed",
            "_meta": { "terminal_output_delta": { "data": "4 hello.txt\n" } }
        }),
    );
    let tool = child_facts
        .iter()
        .find_map(|f| match f {
            Fact::Event(EventBody::ToolChanged { tool }) => Some(tool),
            _ => None,
        })
        .expect("child exec tool");
    assert_eq!(
        tool.parent.as_ref().map(|p| p.as_str()),
        Some("root-c:subagent:thr-1")
    );
    assert!(
        format!("{:?}", tool.result).contains("4 hello.txt"),
        "expected terminal_output_delta in tool result: {:?}",
        tool.result
    );

    // Child usage_update updates subtask context without emitting root UsageReport
    let u_facts = norm.update_for_session(
        Some("root-c:subagent:thr-1"),
        &json!({ "sessionUpdate": "usage_update", "used": 8000, "size": 258400 }),
    );
    assert!(
        u_facts
            .iter()
            .all(|f| !matches!(f, Fact::Event(EventBody::Usage { .. }))),
        "child usage_update must not emit root UsageReport"
    );
    let updated = norm.subtask(&call_id("root-c:subagent:thr-1")).unwrap();
    assert_eq!(
        updated.usage.as_ref().and_then(|u| u.context_used),
        Some(8000)
    );
    assert_eq!(
        updated.usage.as_ref().and_then(|u| u.context_limit),
        Some(258400)
    );

    // Collab fallback (agentsStates)
    let wait_facts = norm.update_for_session(
        Some("root-c"),
        &json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "call-wait-1",
            "kind": "other",
            "title": "wait",
            "status": "completed",
            "rawInput": {
                "agentsStates": {
                    "thr-fallback": { "status": "completed", "message": "Found 3 matches" }
                }
            }
        }),
    );
    let fb = wait_facts
        .iter()
        .find_map(|f| match f {
            Fact::Event(EventBody::SubtaskChanged { subtask }) => Some(subtask),
            _ => None,
        })
        .expect("agentsStates emits fallback subtask");
    assert_eq!(fb.status, SubtaskStatus::Done);
    assert_eq!(fb.summary.as_deref(), Some("Found 3 matches"));
}

#[test]
fn antigravity_subagent_async_command_and_workflow() {
    let mut norm = Normalizer::new("/tmp/repo", "t3", meter())
        .for_harness("antigravity")
        .root_session("agy-1");

    let f1 = norm.update_for_session(
        Some("agy-1"),
        &json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "call-wf",
            "name": "run_workflow",
            "kind": "other",
            "title": "Run run_workflow",
            "status": "in_progress",
            "rawInput": { "Description": "Fan-out security audit" }
        }),
    );
    let wf = f1
        .iter()
        .find_map(|f| match f {
            Fact::Event(EventBody::SubtaskChanged { subtask }) => Some(subtask),
            _ => None,
        })
        .expect("run_workflow emits Workflow subtask");
    assert_eq!(wf.origin, SubtaskOrigin::NativeWorkflow);
    assert_eq!(wf.title, "Fan-out security audit");
    assert!(!wf.can_cancel);

    let f2 = norm.update_for_session(
        Some("agy-1"),
        &json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "call-daemon",
            "name": "run_command",
            "kind": "execute",
            "title": "npm run dev",
            "status": "completed",
            "rawInput": { "CommandLine": "npm run dev", "IsDaemon": true, "toolSummary": "Start dev server" },
            "rawOutput": { "output": "Tool is running as a background task with task id: conv/task-99" }
        }),
    );
    let bg = f2
        .iter()
        .find_map(|f| match f {
            Fact::Event(EventBody::SubtaskChanged { subtask }) => Some(subtask),
            _ => None,
        })
        .expect("async run_command emits BackgroundTask");
    assert_eq!(bg.origin, SubtaskOrigin::BackgroundTask);
    assert_eq!(bg.status, SubtaskStatus::Running);
    assert!(!bg.can_cancel);
}

#[test]
fn pi_parallel_fanout_and_bare_pi_emits_no_subtasks() {
    // Bare Pi without pi-subagents emits zero subtasks
    let mut bare = Normalizer::new("/tmp/repo", "t4a", meter())
        .for_harness("pi")
        .root_session("pi-bare");
    let bare_facts = bare.update_for_session(
        Some("pi-bare"),
        &json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "c-read",
            "name": "read",
            "kind": "read",
            "title": "Read hello.txt",
            "status": "completed"
        }),
    );
    assert!(
        bare_facts
            .iter()
            .all(|f| !matches!(f, Fact::Event(EventBody::SubtaskChanged { .. }))),
        "bare Pi ordinary tools must not emit subtasks"
    );

    // Parallel fan-out with per-child usage + anti-double-counting
    let mut norm = Normalizer::new("/tmp/repo", "t4b", meter())
        .for_harness("pi")
        .root_session("pi-ext");
    let fanout_facts = norm.update_for_session(
        Some("pi-ext"),
        &json!({
            "sessionUpdate": "tool_call",
            "toolCallId": "call-par",
            "name": "subagent",
            "kind": "other",
            "title": "subagent",
            "status": "completed",
            "rawInput": { "tasks": [{ "agent": "scout", "task": "Check A" }, { "agent": "reviewer", "task": "Check B" }] },
            "rawOutput": {
                "details": {
                    "results": [
                        {
                            "agent": "scout",
                            "task": "Check A",
                            "model": "gpt-5.4-mini",
                            "exitCode": 0,
                            "finalOutput": "A ok",
                            "progressSummary": { "durationMs": 1100 },
                            "usage": { "input": 500, "output": 100, "cacheRead": 50, "cacheWrite": 0, "cost": 0.002 }
                        },
                        {
                            "agent": "reviewer",
                            "task": "Check B",
                            "model": "gpt-5.4-mini",
                            "exitCode": 1,
                            "error": "lint failed",
                            "progressSummary": { "durationMs": 900 },
                            "usage": { "input": 400, "output": 80, "cacheRead": 0, "cacheWrite": 0, "cost": 0.001 }
                        }
                    ]
                }
            }
        }),
    );
    let mut subs = Vec::new();
    let mut reports = Vec::new();
    for f in fanout_facts {
        match f {
            Fact::Event(EventBody::SubtaskChanged { subtask }) => subs.push(subtask),
            Fact::Event(EventBody::Usage { report }) => reports.push(report),
            _ => {}
        }
    }
    assert_eq!(subs.len(), 2);
    assert_eq!(subs[0].status, SubtaskStatus::Done);
    assert_eq!(subs[0].duration_ms, Some(1100));
    assert_eq!(subs[1].status, SubtaskStatus::Failed);
    assert_eq!(reports.len(), 2);
    assert!(reports.iter().all(|r| r.scope == UsageScope::Subtask));

    // Parent prompt response emits Main usage; accounting_usage sums Main + Subtask without double-counting
    for f in norm.prompt_response(&json!({
        "stopReason": "end_turn",
        "usage": { "inputTokens": 2000, "outputTokens": 200 }
    })) {
        if let Fact::Event(EventBody::Usage { report }) = f {
            reports.push(report);
        }
    }
    let accounted = bill(&reports, "gpt-5.4-mini");
    let sum_in: u64 = accounted.iter().filter_map(|r| r.tokens.input).sum();
    let sum_out: u64 = accounted.iter().filter_map(|r| r.tokens.output).sum();
    let sum_cache: u64 = accounted.iter().filter_map(|r| r.tokens.cache_read).sum();
    assert_eq!(sum_in, 2900, "2000 Main + 500 + 400 Subtask");
    assert_eq!(sum_out, 380, "200 Main + 100 + 80 Subtask");
    assert_eq!(sum_cache, 50);
}

#[test]
fn generic_adapter_is_unverified_with_forward_compat_rfd() {
    let mut norm = Normalizer::new("/tmp/repo", "t5", meter())
        .for_harness("unknown-future-harness")
        .root_session("g-1");
    assert!(!norm.adapter().capabilities().verified);

    // Ordinary tool produces no subtask
    let f1 = norm.update_for_session(
        Some("g-1"),
        &json!({ "sessionUpdate": "tool_call", "toolCallId": "tc1", "name": "Agent", "title": "Agent", "status": "completed" }),
    );
    assert!(
        f1.iter()
            .all(|f| !matches!(f, Fact::Event(EventBody::SubtaskChanged { .. })))
    );

    // Forward-compat RFD subagent_update still works if emitted proactively
    let f2 = norm.update_for_session(
        Some("g-1"),
        &json!({
            "sessionUpdate": "subagent_update",
            "sessionId": "g-child",
            "title": "Future agent",
            "state": { "state": "running" }
        }),
    );
    assert!(f2.iter().any(|f| matches!(f, Fact::Event(EventBody::SubtaskChanged { subtask }) if subtask.id.as_str() == "g-child")));
}

/* ───────────────────────── Real Recorded Fixture Replays ───────────────────────── */

#[test]
fn replay_claude_rfd_fixture() {
    let out = replay_fixture("claude-code", "claude_rfd.ndjson");
    assert!(!out.root_session.is_empty());

    // RFD subagent registered, cancellable while running, transitions to done on idle/end_turn
    let rfd_events: Vec<&SubtaskView> = out
        .subtask_events
        .iter()
        .filter(|s| s.id.as_str() == "a1be12d322d8d0297")
        .collect();
    assert!(
        rfd_events.len() >= 2,
        "expected running -> done transitions"
    );
    assert!(
        rfd_events
            .iter()
            .any(|s| s.status == SubtaskStatus::Running && s.can_cancel)
    );
    let final_rfd = out.subtasks.get("a1be12d322d8d0297").expect("RFD subagent");
    assert_eq!(final_rfd.origin, SubtaskOrigin::Subagent);
    assert_eq!(final_rfd.status, SubtaskStatus::Done);
    assert_eq!(final_rfd.title, "Count lines in hello.txt");
    assert!(
        !final_rfd.can_cancel,
        "settled subagent is no longer cancellable"
    );

    // Child tool on child session carries parent = child session id
    let child_read = out
        .tools
        .get("toolu_01453urjUhoHDZW8vtruGxQ2")
        .expect("child Read tool");
    assert_eq!(
        child_read.parent.as_ref().map(|p| p.as_str()),
        Some("a1be12d322d8d0297")
    );

    // Background Bash task tracked and completed via .output Read
    let bg = out
        .subtasks
        .values()
        .find(|s| s.origin == SubtaskOrigin::BackgroundTask)
        .expect("background Bash task");
    assert_eq!(bg.status, SubtaskStatus::Done);

    // Usage includes All scope from PromptResponse without double-counting
    assert!(out.usage.iter().any(|r| r.scope == UsageScope::All));
    let accounted = bill(&out.usage, "claude-sonnet-4-6");
    let total_tokens: u64 = accounted
        .iter()
        .map(|r| {
            r.tokens.input.unwrap_or(0)
                + r.tokens.output.unwrap_or(0)
                + r.tokens.cache_read.unwrap_or(0)
        })
        .sum();
    assert!(total_tokens > 0);
}

#[test]
fn replay_claude_fallback_fixture() {
    let out = replay_fixture("claude-code", "claude_fallback.ndjson");
    let sub = out
        .subtasks
        .get("toolu_012gPuu2v371fScRww1fCbnA")
        .expect("fallback Agent subtask");
    assert_eq!(sub.origin, SubtaskOrigin::Subagent);
    assert_eq!(sub.actor.as_deref(), Some("Explore"));
    assert_eq!(sub.model.as_deref(), Some("claude-haiku-4-5-20251001"));
    assert!(!sub.can_cancel);

    // Child tools emitted on root session carry parentToolUseId -> ToolView.parent
    let child_bash = out
        .tools
        .get("toolu_01SR1xzLoHQt6QXbWR7oDW4W")
        .expect("child Bash tool");
    assert_eq!(
        child_bash.parent.as_ref().map(|p| p.as_str()),
        Some("toolu_012gPuu2v371fScRww1fCbnA")
    );
    let child_read = out
        .tools
        .get("toolu_01VgwmeCa3TRn8dX83GWjz2m")
        .expect("child Read tool");
    assert_eq!(
        child_read.parent.as_ref().map(|p| p.as_str()),
        Some("toolu_012gPuu2v371fScRww1fCbnA")
    );
}

#[test]
fn replay_codex_rfd_fixture() {
    let out = replay_fixture("codex", "codex_rfd.ndjson");
    let sub_id =
        "00000000-0000-4000-8000-000000000001:subagent:00000000-0000-4000-8000-000000000002";
    let events: Vec<&SubtaskView> = out
        .subtask_events
        .iter()
        .filter(|s| s.id.as_str() == sub_id)
        .collect();
    assert!(
        events
            .iter()
            .any(|s| s.status == SubtaskStatus::Running && s.can_cancel),
        "Codex RFD advertises cancel capability while running"
    );
    let final_sub = out.subtasks.get(sub_id).expect("Codex RFD subagent");
    assert_eq!(final_sub.origin, SubtaskOrigin::Subagent);
    assert_eq!(final_sub.title, "Count lines");
    assert_eq!(final_sub.status, SubtaskStatus::Done);
    assert!(!final_sub.can_cancel, "cancel: null revokes cancellability");
    assert!(
        final_sub
            .usage
            .as_ref()
            .and_then(|u| u.context_used)
            .is_some(),
        "child usage_update records child context meter"
    );

    // Child tool call nested under sub_id
    let child_tool = out
        .tools
        .get("exec-00000000-0000-4000-8000-000000000004")
        .expect("child exec tool");
    assert_eq!(child_tool.parent.as_ref().map(|p| p.as_str()), Some(sub_id));
}

#[test]
fn replay_codex_fallback_fixture() {
    let out = replay_fixture("codex", "codex_fallback.ndjson");
    let sub = out
        .subtasks
        .get("00000000-0000-4000-8000-000000000002")
        .expect("fallback agentThreadId subtask");
    assert_eq!(sub.origin, SubtaskOrigin::Subagent);
    assert_eq!(sub.status, SubtaskStatus::Done);
    assert!(!sub.can_cancel);
    assert!(
        out.subtask_events
            .iter()
            .any(|s| s.id == sub.id && s.status == SubtaskStatus::Running),
        "expected started -> completed lifecycle"
    );
}

#[test]
fn replay_antigravity_fixture() {
    let out = replay_fixture("antigravity", "agy.ndjson");
    let sub = out
        .subtasks
        .values()
        .find(|s| s.origin == SubtaskOrigin::Subagent)
        .expect("invoke_subagent subtask");
    assert!(!sub.can_cancel);

    let bg = out
        .subtasks
        .values()
        .find(|s| s.origin == SubtaskOrigin::BackgroundTask)
        .expect("async run_command background task");
    assert!(!bg.can_cancel);
    assert!(
        out.usage.iter().any(|r| r.tokens.output.unwrap_or(0) > 0),
        "agy PromptResponse usage normalized"
    );
}

#[test]
fn replay_kimi_fixture() {
    let out = replay_fixture("kimi-code", "kimi.ndjson");
    let sub = out
        .subtasks
        .values()
        .find(|s| s.origin == SubtaskOrigin::Subagent)
        .expect("Kimi Agent subtask recovered despite omitted tool name");
    assert_eq!(sub.status, SubtaskStatus::Done);
    assert!(!sub.can_cancel);

    let bg = out
        .subtasks
        .values()
        .find(|s| s.origin == SubtaskOrigin::BackgroundTask)
        .expect("Kimi background Bash task");
    assert_eq!(bg.status, SubtaskStatus::Done);
    assert!(!bg.can_cancel);
}

#[test]
fn replay_pi_fixture() {
    let out = replay_fixture("pi", "pi.ndjson");
    let sub = out
        .subtasks
        .values()
        .find(|s| s.origin == SubtaskOrigin::Subagent)
        .expect("Pi subagent result");
    assert_eq!(sub.status, SubtaskStatus::Done);
    assert!(!sub.can_cancel);
    assert!(sub.duration_ms.unwrap_or(0) > 0, "Pi reports durationMs");
    let sub_usage = sub.usage.as_ref().expect("Pi reports per-child usage");
    assert!(sub_usage.input.unwrap_or(0) > 0);
    assert!(sub_usage.output.unwrap_or(0) > 0);

    // Pi emits Subtask-scoped usage separately from parent Main usage; accounting_usage sums both
    let sub_reports: Vec<&UsageReport> = out
        .usage
        .iter()
        .filter(|r| r.scope == UsageScope::Subtask)
        .collect();
    let main_reports: Vec<&UsageReport> = out
        .usage
        .iter()
        .filter(|r| r.scope == UsageScope::Main)
        .collect();
    assert!(
        !sub_reports.is_empty(),
        "expected UsageScope::Subtask report"
    );
    assert!(!main_reports.is_empty(), "expected UsageScope::Main report");
    let main_in: u64 = main_reports.iter().filter_map(|r| r.tokens.input).sum();
    let sub_in: u64 = sub_reports.iter().filter_map(|r| r.tokens.input).sum();
    let accounted = bill(&out.usage, "gpt-5.4");
    let billed_in: u64 = accounted.iter().filter_map(|r| r.tokens.input).sum();
    assert_eq!(billed_in, main_in + sub_in);
}

/* ───────────────────────── Manual Live Smoke Test (#[ignore]) ───────────────────────── */

/// Manual end-to-end smoke test against locally installed harness CLIs.
///
/// Run explicitly by a maintainer / coordinator:
/// `cargo test --manifest-path hub-rs/Cargo.toml -p steve-acp --test harness_adapters live_smoke_local_harnesses -- --ignored --nocapture`
///
/// Uses isolated temporary directories only and never touches `~/.steve*`.
#[tokio::test]
#[ignore = "invokes locally installed harness CLIs over real model APIs; run manually with --ignored"]
async fn live_smoke_local_harnesses() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let work = tmp.path().join("work");
    std::fs::create_dir_all(&work).unwrap();
    std::fs::write(work.join("hello.txt"), "one\ntwo\nthree\n").unwrap();

    let mut cfg = steve_acp::AcpConfig::from_env();
    cfg.steve_home = tmp.path().join("steve-isolated");
    cfg.codex_home = tmp.path().join("codex-isolated");
    cfg.user_home = tmp.path().join("home-isolated");
    std::fs::create_dir_all(&cfg.steve_home).unwrap();
    std::fs::create_dir_all(&cfg.codex_home).unwrap();
    std::fs::create_dir_all(&cfg.user_home).unwrap();

    let mut tested = 0usize;
    for harness in ["claude-code", "codex", "antigravity", "kimi-code", "pi"] {
        let probe = steve_acp::adapters::probe(&cfg, harness, &work, Duration::from_secs(15)).await;
        if probe.status != ProbeStatus::Ready {
            eprintln!(
                "[live-smoke] skipping {harness}: {:?} ({:?})",
                probe.status, probe.error
            );
            continue;
        }
        tested += 1;
        let caps = capabilities_for(harness);
        assert!(caps.verified, "{harness} must have verified capabilities");
        eprintln!(
            "[live-smoke] {harness} ready: version={:?} caps={caps:?}",
            probe.version
        );
    }
    assert!(
        tested > 0,
        "expected at least one locally installed harness in live smoke test"
    );
}
