//! Turn context budget & native compaction integration tests on a real LocalNode + fake ACP
//! subprocess (no real models, no credentials, isolated temp home).
#![cfg(unix)]

use std::collections::HashMap;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Value, json};
use steve_acp::AcpConfig;
use steve_node::{LocalNode, NodeConfig, PolicyCeiling, Root, Storage};
use steve_node_api::testkit::{RecordingSink, build};
use steve_node_api::*;
use steve_runtime::testkit::{eventually, on_timeout};

const FAKE_ACP_PY: &str = r#"#!/usr/bin/env python3
import json, os, select, sys, time

log_path = os.environ.get("FAKE_CTX_LOG")
def log(obj):
    if log_path and os.path.basename(os.getcwd()) != "probe":
        with open(log_path, "a", encoding="utf-8") as f:
            f.write(json.dumps(obj) + "\n")

log({
    "spawn": {
        "argv": sys.argv[1:],
        "env": {
            "FAKE_COMPACT_PCT": os.environ.get("FAKE_COMPACT_PCT"),
            "FAKE_COMPACT_WINDOW": os.environ.get("FAKE_COMPACT_WINDOW"),
        },
    }
})

def send(obj):
    sys.stdout.write(json.dumps({"jsonrpc": "2.0", **obj}) + "\n")
    sys.stdout.flush()

def update(sid, u):
    send({"method": "session/update", "params": {"sessionId": sid, "update": u}})

def wait_for_cancel(timeout_s=5.0):
    deadline = time.monotonic() + timeout_s
    while time.monotonic() < deadline:
        r, _, _ = select.select([sys.stdin], [], [], 0.05)
        if not r:
            continue
        line = sys.stdin.readline()
        if not line:
            return False
        msg = json.loads(line)
        log(msg)
        if msg.get("method") == "session/cancel":
            return True
    return False

sid = ""
for raw in sys.stdin:
    if not raw.strip():
        continue
    msg = json.loads(raw)
    log(msg)
    method = msg.get("method")
    mid = msg.get("id")
    if method == "initialize":
        send({"id": mid, "result": {"protocolVersion": 1, "agentCapabilities": {"loadSession": True}}})
    elif method == "session/new":
        sid = "fake-ctx-1"
        send({"id": mid, "result": {
            "sessionId": sid,
            "modes": {"currentModeId": "default", "availableModes": [{"id": "default", "name": "default"}]},
            "models": {"currentModelId": "fake-model", "availableModels": []},
        }})
    elif method == "session/prompt":
        text = "\n".join(p.get("text", "") for p in msg["params"]["prompt"])
        if "SCENARIO:NATIVE" in text:
            # 1. Cross budget threshold (85k / 100k > 70%): must NOT be cancelled on native harness.
            update(sid, {"sessionUpdate": "usage_update", "used": 85000, "size": 100000})
            time.sleep(0.08)
            # 2. Dual-counter oscillation (85k -> 15k -> 90k): must be ignored, not counted as compaction.
            update(sid, {"sessionUpdate": "usage_update", "used": 15000, "size": 100000})
            update(sid, {"sessionUpdate": "usage_update", "used": 90000, "size": 100000})
            # 3. Explicit `compact` think tool + drop (90k -> 12k): confirmed native compaction #1.
            update(sid, {"sessionUpdate": "tool_call", "toolCallId": "cmp-1", "kind": "think", "status": "pending", "title": "compact", "_meta": {"claudeCode": {"toolName": "compact"}}})
            update(sid, {"sessionUpdate": "tool_call_update", "toolCallId": "cmp-1", "status": "completed"})
            update(sid, {"sessionUpdate": "usage_update", "used": 12000, "size": 100000})
            # 4. Climb to 92k then sustained implicit drop (92k -> 14k -> 16k): confirmed native compaction #2.
            update(sid, {"sessionUpdate": "usage_update", "used": 92000, "size": 100000})
            update(sid, {"sessionUpdate": "usage_update", "used": 14000, "size": 100000})
            update(sid, {"sessionUpdate": "usage_update", "used": 16000, "size": 100000})
            update(sid, {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "native done"}})
            send({"id": mid, "result": {
                "stopReason": "end_turn",
                "usage": {
                    "inputTokens": 1200,
                    "outputTokens": 300,
                    "cachedReadTokens": 400,
                    "cachedWriteTokens": 100,
                    "totalTokens": 2000,
                },
            }})
        elif "SCENARIO:CLI_CHECK" in text:
            update(sid, {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "cli ok"}})
            send({"id": mid, "result": {"stopReason": "end_turn"}})
        elif "SCENARIO:BUDGET_STOP" in text:
            update(sid, {"sessionUpdate": "usage_update", "used": 5000, "size": 10000})
            update(sid, {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "halfway"}})
            update(sid, {"sessionUpdate": "usage_update", "used": 8200, "size": 10000})
            cancelled = wait_for_cancel(5.0)
            log({"budget_cancelled": cancelled})
            send({"id": mid, "result": {
                "stopReason": "cancelled" if cancelled else "end_turn",
                "usage": {
                    "inputTokens": 800,
                    "outputTokens": 150,
                    "cachedReadTokens": 200,
                    "cachedWriteTokens": 0,
                    "totalTokens": 1150,
                },
            }})
        else:
            send({"id": mid, "result": {"stopReason": "end_turn"}})
    elif mid is not None:
        send({"id": mid, "result": {}})
"#;

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn command<T>(id: &str, body: T) -> Command<T> {
    let mut c = build::cmd(id, body);
    c.issued_at = Timestamp(now());
    c.accept_before = Timestamp(now() + 60_000);
    c
}

struct Harness {
    dir: tempfile::TempDir,
    node: LocalNode,
    sink: Arc<RecordingSink>,
    log: PathBuf,
}

impl Harness {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("work")).unwrap();
        let script = root.join("fake-ctx-acp.py");
        std::fs::write(&script, FAKE_ACP_PY).unwrap();
        std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o700)).unwrap();
        let log = root.join("acp.log");
        let adapter = root.join("adapter.sh");
        std::fs::write(
            &adapter,
            format!(
                "#!/bin/sh\nexport FAKE_CTX_LOG='{}'\nexec python3 '{}' \"$@\"\n",
                log.display(),
                script.display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&adapter, std::fs::Permissions::from_mode(0o700)).unwrap();

        let declared = HashMap::from([
            (
                "native_env".into(),
                serde_json::from_value(json!({
                    "properties": {
                        "nested_sandbox": false,
                        "context_limit": 100000,
                        "context_budget_pct": 80,
                        "native_compaction": {
                            "kind": "env_percent",
                            "var": "FAKE_COMPACT_PCT",
                            "window_var": "FAKE_COMPACT_WINDOW"
                        }
                    }
                }))
                .unwrap(),
            ),
            (
                "native_cli".into(),
                serde_json::from_value(json!({
                    "properties": {
                        "nested_sandbox": false,
                        "context_limit": 200000,
                        "context_budget_pct": 75,
                        "native_compaction": {
                            "kind": "cli_args",
                            "args": ["-c", "auto_compact_limit={tokens}"]
                        }
                    }
                }))
                .unwrap(),
            ),
            (
                "plain".into(),
                serde_json::from_value(json!({
                    "properties": {
                        "nested_sandbox": false,
                        "context_limit": 10000,
                        "context_budget_pct": 80
                    }
                }))
                .unwrap(),
            ),
        ]);

        let sink = Arc::new(RecordingSink::default());
        let cfg = NodeConfig {
            node: NodeId::new("node").unwrap(),
            home: root.join("node"),
            ceiling: PolicyCeiling::unlimited(),
            acp: AcpConfig {
                adapters: HashMap::from([
                    ("native_env".into(), adapter.clone()),
                    ("native_cli".into(), adapter.clone()),
                    ("plain".into(), adapter),
                ]),
                declared,
                node: "node".into(),
                claude_executable: "/usr/bin/false".into(),
                steve_home: root.join("steve"),
                codex_home: root.join("codex"),
                user_home: root.into(),
                version: "test".into(),
            },
            roots: vec![Root {
                id: RootId::new("work").unwrap(),
                path: root.join("work"),
                display: "work".into(),
                access: RootAccess::ReadWrite,
            }],
            capacity: Capacity {
                sessions: 4,
                jobs: 1,
                execs: 0,
            },
            storage: Storage::default(),
        };
        let node = LocalNode::open(cfg).unwrap();
        node.connect(sink.clone(), None);
        Self {
            dir,
            node,
            sink,
            log,
        }
    }

    fn workspace(sid: &str) -> WorkspaceRef {
        build::workspace("node", &format!("item:{sid}"))
    }

    async fn finished(&self, op: &str) -> Observed {
        let out = std::sync::Mutex::new(None);
        eventually(
            on_timeout(|| async { format!("waiting for op {op}") }),
            || async {
                let state = self
                    .node
                    .operation(OperationId::new(op).unwrap())
                    .await
                    .unwrap()
                    .state;
                let done_event = op.starts_with("prep-")
                    || op.starts_with("open-")
                    || self.sink.events().iter().any(|e| {
                        matches!(&e.subject, Subject::Execution(x) if x.attempt().as_str() == op)
                            && matches!(e.body, EventBody::AttemptEnded { .. })
                    });
                if state.is_finished() && done_event {
                    *out.lock().unwrap() = Some(state);
                    return true;
                }
                false
            },
        )
        .await;
        out.into_inner().unwrap().unwrap()
    }

    async fn open_session(
        &self,
        sid: &str,
        harness: &str,
        budget: Option<ContextBudget>,
        env: Vec<(String, String)>,
    ) {
        std::fs::create_dir_all(self.dir.path().join("work").join(sid)).unwrap();
        self.node
            .prepare_workspace(command(
                &format!("prep-{sid}"),
                Prepare {
                    resource: format!("item:{sid}"),
                    kind: PrepareKind::Existing {
                        dir: RootPath {
                            root: RootId::new("work").unwrap(),
                            rel: RelPath::new(sid).unwrap(),
                        },
                    },
                },
            ))
            .await
            .unwrap();
        self.finished(&format!("prep-{sid}")).await;
        self.node
            .open_session(command(
                &format!("open-{sid}"),
                OpenSession {
                    session: SessionId::new(sid).unwrap(),
                    workspace: Self::workspace(sid),
                    choice: HarnessChoice {
                        harness: harness.into(),
                        model: None,
                        effort: None,
                    },
                    policy: build::policy(),
                    resume: None,
                    env,
                    state: None,
                    artifact_view: None,
                    skills: vec![],
                    context_budget: budget,
                },
            ))
            .await
            .unwrap();
        let state = self.finished(&format!("open-{sid}")).await;
        assert!(
            matches!(
                state,
                Observed::Finished {
                    outcome: Outcome::Succeeded,
                    ..
                }
            ),
            "open failed: {state:?}"
        );
    }

    async fn run_turn(
        &self,
        sid: &str,
        harness: &str,
        attempt: &str,
        prompt: &str,
        budget: Option<ContextBudget>,
    ) -> Completion {
        self.node
            .start_turn(command(
                attempt,
                TurnSpec {
                    session: SessionId::new(sid).unwrap(),
                    turn: TurnId::new(attempt).unwrap(),
                    run: RunSpec {
                        attempt: AttemptId::new(attempt).unwrap(),
                        choice: HarnessChoice {
                            harness: harness.into(),
                            model: None,
                            effort: None,
                        },
                        directory: RunDirectory::Workspace {
                            workspace: Self::workspace(sid),
                        },
                        prompt: vec![Part::Text {
                            text: prompt.into(),
                        }],
                        policy: build::policy(),
                        limits: Limits {
                            deadline: Timestamp(now() + 60_000),
                            offline_until: Timestamp(now() + 60_000),
                            context_budget: budget,
                            ..build::limits()
                        },
                        continuation: None,
                    },
                },
            ))
            .await
            .unwrap();
        match self.finished(attempt).await {
            Observed::Finished {
                result: Some(OperationResult::Execution(c)),
                ..
            } => c,
            other => panic!("unexpected terminal state: {other:?}"),
        }
    }

    fn log_lines(&self) -> Vec<Value> {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| serde_json::from_str(l).ok())
            .collect()
    }

    fn compacted_events(&self, attempt: &str) -> Vec<EventBody> {
        self.sink
            .events()
            .into_iter()
            .filter(
                |e| matches!(&e.subject, Subject::Execution(x) if x.attempt().as_str() == attempt),
            )
            .map(|e| e.body)
            .filter(|b| matches!(b, EventBody::ContextCompacted { .. }))
            .collect()
    }

    fn final_usage_reports(&self, attempt: &str) -> Vec<UsageReport> {
        self.sink
            .events()
            .into_iter()
            .filter(
                |e| matches!(&e.subject, Subject::Execution(x) if x.attempt().as_str() == attempt),
            )
            .filter_map(|e| match e.body {
                EventBody::Usage { report } if report.measurement == Measurement::Final => {
                    Some(report)
                }
                _ => None,
            })
            .collect()
    }
}

#[tokio::test]
async fn native_compaction_env_cli_and_observation_without_cutoff() {
    let h = Harness::new();

    // 1. Native env_percent harness with Hub override (70%) -> receives env vars, is NOT cut off
    //    at 85k/100k, ignores oscillation, and observes both explicit and sustained drops.
    let budget = Some(ContextBudget {
        ratio_pct: Some(70),
        max_tokens: None,
    });
    h.open_session("s_native", "native_env", budget, vec![])
        .await;
    let completion = h
        .run_turn(
            "s_native",
            "native_env",
            "a_native",
            "SCENARIO:NATIVE",
            budget,
        )
        .await;
    assert_eq!(completion.outcome, Outcome::Succeeded);
    assert_eq!(completion.stop_reason, "end_turn");
    assert_eq!(completion.context_peak, Some(92_000));

    let compacted = h.compacted_events("a_native");
    assert_eq!(
        compacted,
        vec![
            EventBody::ContextCompacted {
                mode: CompactionMode::Native,
                reason: CompactionReason::NativeCompaction,
                before_tokens: 90_000,
                after_tokens: 12_000,
                limit_tokens: Some(100_000),
            },
            EventBody::ContextCompacted {
                mode: CompactionMode::Native,
                reason: CompactionReason::NativeCompaction,
                before_tokens: 92_000,
                after_tokens: 14_000,
                limit_tokens: Some(100_000),
            },
        ]
    );

    let finals = h.final_usage_reports("a_native");
    assert_eq!(finals.len(), 1, "final usage must be reported once");
    assert_eq!(
        finals[0].tokens,
        Tokens {
            input: Some(1200),
            output: Some(300),
            cache_read: Some(400),
            cache_write: Some(100),
            reasoning: None,
        }
    );

    // 2. Explicit operator env overrides declarative native compaction env, while an explicit
    //    `max_tokens` ceiling populates `window_var`.
    h.open_session(
        "s_override",
        "native_env",
        Some(ContextBudget {
            ratio_pct: Some(70),
            max_tokens: Some(120_000),
        }),
        vec![("FAKE_COMPACT_PCT".into(), "55".into())],
    )
    .await;

    // 3. Native cli_args harness receives rendered token threshold flags (75% of 200_000 = 150_000).
    h.open_session("s_cli", "native_cli", None, vec![]).await;
    let cli_done = h
        .run_turn("s_cli", "native_cli", "a_cli", "SCENARIO:CLI_CHECK", None)
        .await;
    assert_eq!(cli_done.outcome, Outcome::Succeeded);

    let spawns: Vec<Value> = h
        .log_lines()
        .into_iter()
        .filter_map(|v| v.get("spawn").cloned())
        .collect();
    assert_eq!(spawns.len(), 3, "{spawns:?}");
    assert_eq!(spawns[0]["env"]["FAKE_COMPACT_PCT"], "70");
    assert_eq!(spawns[0]["env"]["FAKE_COMPACT_WINDOW"], Value::Null);
    assert_eq!(spawns[1]["env"]["FAKE_COMPACT_PCT"], "55");
    assert_eq!(spawns[1]["env"]["FAKE_COMPACT_WINDOW"], "120000");
    assert_eq!(
        spawns[2]["argv"],
        json!(["-c", "auto_compact_limit=150000"])
    );

    h.node.shutdown().await;
}

#[tokio::test]
async fn non_native_harness_stops_deterministically_at_context_budget() {
    let h = Harness::new();
    h.open_session("s_plain", "plain", None, vec![]).await;
    let completion = h
        .run_turn("s_plain", "plain", "a_plain", "SCENARIO:BUDGET_STOP", None)
        .await;

    assert_eq!(completion.outcome, Outcome::Cancelled);
    assert_eq!(completion.stop_reason, CONTEXT_BUDGET);
    assert_eq!(
        completion.error.as_ref().and_then(NodeStop::of),
        Some(NodeStop::ContextBudget)
    );
    assert_eq!(completion.context_peak, Some(8_200));

    let compacted = h.compacted_events("a_plain");
    assert_eq!(
        compacted,
        vec![EventBody::ContextCompacted {
            mode: CompactionMode::Handoff,
            reason: CompactionReason::BudgetReached,
            before_tokens: 8_200,
            after_tokens: 0,
            limit_tokens: Some(10_000),
        }]
    );

    let finals = h.final_usage_reports("a_plain");
    assert_eq!(finals.len(), 1, "cancelled budget turn settles usage once");
    assert_eq!(
        finals[0].tokens,
        Tokens {
            input: Some(800),
            output: Some(150),
            cache_read: Some(200),
            cache_write: Some(0),
            reasoning: None,
        }
    );
    assert!(
        h.log_lines()
            .iter()
            .any(|v| v.get("budget_cancelled") == Some(&json!(true))),
        "Node must send session/cancel to the harness once context hits budget"
    );

    h.node.shutdown().await;
}
