//! End-to-end Node tests for child-session routing and single-subtask cancellation.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

async fn finished(n: &LocalNode, op: &str) -> Observed {
    let t0 = Instant::now();
    loop {
        let r = n.operation(OperationId::new(op).unwrap()).await.unwrap();
        if r.state.is_finished() {
            return r.state;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(15),
            "{op}: {:?}",
            r.state
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

use steve_acp::AcpConfig;
use steve_node::{LocalNode, NodeConfig, Root, Storage};
use steve_node_api::testkit::RecordingSink;
use steve_node_api::testkit::build::{choice, cmd, limits, policy};
use steve_node_api::*;

const MOCK_RFD_ADAPTER: &str = r#"#!/usr/bin/env python3
import json, os, sys, threading

log_path = os.environ["MOCK_ACP_LOG"]
cancel_event = threading.Event()
lock = threading.Lock()

def log(entry):
    with lock:
        with open(log_path, "a", encoding="utf-8") as f:
            f.write(json.dumps(entry, ensure_ascii=False) + "\n")

def send(obj):
    with lock:
        sys.stdout.write(json.dumps(obj, ensure_ascii=False) + "\n")
        sys.stdout.flush()

def update(sid, upd):
    send({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": sid, "update": upd}})

def run_prompt(req_id, root_sid):
    child_sid = "child-sub-1"
    # 1. Register cancellable RFD subagent on root session
    update(root_sid, {
        "sessionUpdate": "subagent_update",
        "sessionId": child_sid,
        "title": "Inspect auth module",
        "description": "Explore",
        "capabilities": {"cancel": {}},
        "state": {"state": "running"},
    })
    # 2. Emit child tool_call on child_sid (Node must NOT drop it!)
    update(child_sid, {
        "sessionUpdate": "tool_call",
        "toolCallId": "child-tool-1",
        "name": "Read",
        "kind": "read",
        "title": "Read auth.rs",
        "status": "completed",
        "rawInput": {"file_path": "/workspace/auth.rs"},
        "rawOutput": "pub fn verify() {}",
    })
    # 3. Wait for single-subtask session/cancel targeting child_sid
    cancelled = cancel_event.wait(timeout=10.0)
    update(root_sid, {
        "sessionUpdate": "subagent_update",
        "sessionId": child_sid,
        "capabilities": {"cancel": None},
        "state": {"state": "idle", "stopReason": "cancelled" if cancelled else "end_turn"},
    })
    # 4. Parent session stays alive and finishes its own reply cleanly
    update(root_sid, {
        "sessionUpdate": "agent_message_chunk",
        "content": {"type": "text", "text": "Parent completed after child cancel."},
    })
    send({"jsonrpc": "2.0", "id": req_id, "result": {"stopReason": "end_turn"}})

for raw in sys.stdin:
    if not raw.strip():
        continue
    msg = json.loads(raw)
    log(msg)
    method = msg.get("method")
    req_id = msg.get("id")
    if method == "initialize":
        send({
            "jsonrpc": "2.0",
            "id": req_id,
            "result": {
                "protocolVersion": 1,
                "agentInfo": {"name": "mock-rfd", "version": "1.0.0"},
                "agentCapabilities": {"loadSession": True},
            },
        })
    elif method == "session/new":
        send({
            "jsonrpc": "2.0",
            "id": req_id,
            "result": {
                "sessionId": "root-sess-1",
                "modes": {"availableModes": [{"id": "default", "name": "Default"}], "currentModeId": "default"},
                "models": {"availableModels": [{"modelId": "default", "name": "Default"}], "currentModelId": "default"},
            },
        })
    elif method == "session/prompt":
        sid = msg.get("params", {}).get("sessionId", "root-sess-1")
        threading.Thread(target=run_prompt, args=(req_id, sid), daemon=True).start()
    elif method == "session/cancel":
        if msg.get("params", {}).get("sessionId") == "child-sub-1":
            cancel_event.set()
    elif req_id is not None:
        send({"jsonrpc": "2.0", "id": req_id, "result": {}})
"#;

fn wall() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn now_cmd<T>(op: &str, body: T) -> Command<T> {
    let mut c = cmd(op, body);
    c.issued_at = Timestamp(wall());
    c.accept_before = Timestamp(wall() + 60_000);
    c
}

#[tokio::test]
async fn child_session_updates_are_routed_and_single_subtask_cancel_keeps_parent_alive() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    let home = root.join("node");
    let log = root.join("mock-acp.log");
    let py = root.join("mock_rfd.py");
    let adapter = root.join("adapter.sh");

    std::fs::write(&py, MOCK_RFD_ADAPTER).unwrap();
    std::fs::write(
        &adapter,
        format!(
            "#!/bin/sh\nexport MOCK_ACP_LOG='{}'\nexec python3 '{}' \"$@\"\n",
            log.display(),
            py.display()
        ),
    )
    .unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&adapter, std::fs::Permissions::from_mode(0o755)).unwrap();
    }
    std::fs::create_dir_all(root.join("work/proj")).unwrap();

    let sink = Arc::new(RecordingSink::default());
    let cfg = NodeConfig {
        ceiling: steve_node::PolicyCeiling::unlimited(),
        node: NodeId::new("hub").unwrap(),
        home,
        acp: AcpConfig {
            declared: Default::default(),
            adapters: HashMap::from([("claude-code".to_string(), PathBuf::from(&adapter))]),
            node: "node".into(),
            claude_executable: "claude".into(),
            claude_config_dir: None,
            steve_home: root.join("steve"),
            codex_home: root.join("codex"),
            user_home: root.to_path_buf(),
            version: "test".into(),
        },
        roots: vec![Root {
            id: RootId::new("work").unwrap(),
            path: root.join("work"),
            display: "~/work".into(),
            access: RootAccess::ReadWrite,
        }],
        capacity: Capacity {
            sessions: 2,
            jobs: 0,
            execs: 0,
        },
        storage: Storage::default(),
    };

    let node = LocalNode::open(cfg).unwrap();
    node.connect(sink.clone(), None);

    let ws = WorkspaceRef {
        node: NodeId::new("hub").unwrap(),
        id: WorkspaceId::new(derived_id("w", "item:wf1")).unwrap(),
    };
    node.prepare_workspace(now_cmd(
        "op-prep",
        Prepare {
            resource: "item:wf1".into(),
            kind: PrepareKind::Existing {
                dir: RootPath {
                    root: RootId::new("work").unwrap(),
                    rel: RelPath::new("proj").unwrap(),
                },
            },
        },
    ))
    .await
    .unwrap();
    let prep_state = finished(&node, "op-prep").await;
    assert!(
        matches!(
            prep_state,
            Observed::Finished {
                outcome: Outcome::Succeeded,
                ..
            }
        ),
        "prepare failed: {prep_state:?}"
    );

    let mut ch = choice();
    ch.harness = "claude-code".into();
    ch.model = None;
    ch.effort = None;

    node.open_session(now_cmd(
        "op-open",
        OpenSession {
            session: SessionId::new("s-wf1").unwrap(),
            workspace: ws.clone(),
            choice: ch.clone(),
            policy: policy(),
            resume: None,
            env: Default::default(),
            state: Default::default(),
            artifact_view: None,
            skills: vec![],
            system_context: None,
        },
    ))
    .await
    .unwrap();

    let open_state = finished(&node, "op-open").await;
    assert!(
        matches!(
            open_state,
            Observed::Finished {
                outcome: Outcome::Succeeded,
                ..
            }
        ),
        "open_session failed: {open_state:?}"
    );

    node.start_turn(now_cmd(
        "op-turn",
        TurnSpec {
            session: SessionId::new("s-wf1").unwrap(),
            turn: TurnId::new("t-1").unwrap(),
            run: RunSpec {
                attempt: AttemptId::new("a-1").unwrap(),
                choice: ch,
                directory: RunDirectory::Workspace { workspace: ws },
                prompt: vec![Part::Text {
                    text: "delegate check".into(),
                }],
                policy: policy(),
                limits: Limits {
                    deadline: Timestamp(i64::MAX / 2),
                    offline_until: Timestamp(i64::MAX / 2),
                    ..limits()
                },
                continuation: None,
            },
        },
    ))
    .await
    .unwrap();

    // Wait until BOTH the RFD subtask AND its child session's tool call arrive on the sink!
    steve_runtime::testkit::eventually_sync("child tool routed and parented", || {
        let evs = sink.events();
        let has_sub = evs.iter().any(|e| {
            matches!(
                &e.body,
                EventBody::SubtaskChanged { subtask }
                    if subtask.id.as_str() == "child-sub-1" && subtask.can_cancel
            )
        });
        let has_child_tool = evs.iter().any(|e| {
            matches!(
                &e.body,
                EventBody::ToolChanged { tool }
                    if tool.id.as_str() == "child-tool-1"
                        && tool.parent.as_ref().map(|p| p.as_str()) == Some("child-sub-1")
            )
        });
        has_sub && has_child_tool
    })
    .await;

    // Unknown subtask returns Ok(false)
    let unknown = node
        .cancel_subtask(CancelSubtask {
            session: SessionId::new("s-wf1").unwrap(),
            subtask: CallId::new("does-not-exist").unwrap(),
        })
        .await
        .unwrap();
    assert!(!unknown);

    // Cancel ONLY child-sub-1; parent session must stay alive and complete normally
    let cancelled = node
        .cancel_subtask(CancelSubtask {
            session: SessionId::new("s-wf1").unwrap(),
            subtask: CallId::new("child-sub-1").unwrap(),
        })
        .await
        .unwrap();
    assert!(cancelled);

    // Wait for parent turn to complete with Succeeded + reply text
    steve_runtime::testkit::eventually_sync("parent turn completed", || {
        sink.events().iter().any(|e| {
            matches!(
                &e.body,
                EventBody::AttemptEnded {
                    completion: Completion {
                        outcome: Outcome::Succeeded,
                        ..
                    }
                }
            )
        })
    })
    .await;

    let events = sink.events();
    let final_sub = events
        .iter()
        .rev()
        .find_map(|e| match &e.body {
            EventBody::SubtaskChanged { subtask } if subtask.id.as_str() == "child-sub-1" => {
                Some(subtask)
            }
            _ => None,
        })
        .expect("final subtask event");
    assert_eq!(final_sub.status, SubtaskStatus::Cancelled);
    assert!(!final_sub.can_cancel);

    let text: String = events
        .iter()
        .filter_map(|e| match &e.body {
            EventBody::TextDelta { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert!(
        text.contains("Parent completed after child cancel."),
        "expected parent reply text, got: {text:?}"
    );

    // Verify wire log: initialize negotiated subagents without jetbrains.air,
    // and session/cancel targeted ONLY child-sub-1 (never root-sess-1)
    let wire_lines: Vec<serde_json::Value> = std::fs::read_to_string(&log)
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect();
    let init = wire_lines
        .iter()
        .find(|m| m["method"] == "initialize")
        .expect("initialize frame");
    assert_eq!(
        init["params"]["clientCapabilities"]["subagents"],
        serde_json::json!({})
    );
    assert!(
        init["params"]["clientCapabilities"]
            .pointer("/_meta/jetbrains/air")
            .is_none()
    );
    let cancels: Vec<&str> = wire_lines
        .iter()
        .filter(|m| m["method"] == "session/cancel")
        .filter_map(|m| m["params"]["sessionId"].as_str())
        .collect();
    assert_eq!(cancels, vec!["child-sub-1"]);

    tokio::time::timeout(Duration::from_secs(5), node.shutdown())
        .await
        .expect("clean shutdown");
}
