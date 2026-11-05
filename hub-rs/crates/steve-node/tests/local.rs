//! LocalNode against the scripted ACP adapter of steve-acp (`fake-acp.js`, run by node): the
//! contract's rules with a real process behind them.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use steve_acp::AcpConfig;
use steve_node::{LocalNode, NodeConfig, Root};
use steve_node_api::testkit::RecordingSink;
use steve_node_api::testkit::build::{answer, choice, cmd, limits, policy};
use steve_node_api::*;

struct T {
    _dir: Option<tempfile::TempDir>,
    root: PathBuf,
    home: PathBuf,
    log: PathBuf,
    adapter: PathBuf,
    sink: Arc<RecordingSink>,
}

impl T {
    /// `env`: extra variables for the fake adapter (`FAKE_ACP_SHAPE=claude099`…).
    fn new(env: &[(&str, &str)]) -> T {
        let dir = tempfile::tempdir().unwrap();
        let mut t = T::at(dir.path(), env);
        t._dir = Some(dir);
        t
    }

    /// A Node home under `root` (made if new; reused by a child process of a test).
    fn at(root: &Path, env: &[(&str, &str)]) -> T {
        let home = root.join("node");
        let log = root.join("acp.log");
        let fake = Path::new(env!("CARGO_MANIFEST_DIR")).join("../steve-acp/tests/fake-acp.js");
        let adapter = root.join("adapter.sh");
        let mut script = format!("#!/bin/sh\nexport FAKE_ACP_LOG='{}'\n", log.display());
        for (k, v) in env {
            script.push_str(&format!("export {k}='{v}'\n"));
        }
        script.push_str(&format!("exec node '{}' \"$@\"\n", fake.display()));
        if !adapter.exists() {
            std::fs::write(&adapter, script).unwrap();
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&adapter, std::fs::Permissions::from_mode(0o755)).unwrap();
        }
        std::fs::create_dir_all(root.join("work/proj")).unwrap();
        T {
            root: root.to_path_buf(),
            home,
            log,
            adapter,
            sink: Arc::new(RecordingSink::default()),
            _dir: None,
        }
    }

    fn config(&self) -> NodeConfig {
        let root = self.root.clone();
        NodeConfig {
            node: NodeId::new("hub").unwrap(),
            home: self.home.clone(),
            acp: AcpConfig {
                adapters: HashMap::from([("claude-code".to_string(), self.adapter.clone())]),
                node: "node".into(),
                claude_executable: "claude".into(),
                steve_home: root.join("steve"),
                codex_home: root.join("codex"),
                user_home: root.clone(),
                version: "test".into(),
                idle_release: Duration::from_secs(600),
                retry_delays: vec![],
            },
            roots: vec![Root {
                id: RootId::new("work").unwrap(),
                path: root.join("work"),
                display: "~/work".into(),
            }],
            capacity: Capacity {
                sessions: 4,
                jobs: 0,
                execs: 0,
            },
        }
    }

    fn node(&self) -> LocalNode {
        let n = LocalNode::open(self.config()).unwrap();
        n.connect(self.sink.clone(), None);
        n
    }

    /// Lines of the adapter's log with this method.
    fn sent(&self, method: &str) -> usize {
        std::fs::read_to_string(&self.log)
            .unwrap_or_default()
            .lines()
            .filter(|l| l.contains(&format!("\"method\":\"{method}\"")))
            .count()
    }

    fn kinds(&self) -> Vec<String> {
        self.sink.kinds()
    }

    async fn until(&self, what: &str, f: impl Fn(&T) -> bool) {
        let t0 = Instant::now();
        while !f(self) {
            assert!(
                t0.elapsed() < Duration::from_secs(15),
                "timed out waiting for {what}: {:?}",
                self.kinds()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    fn ended(&self) -> Vec<Completion> {
        self.sink
            .events()
            .into_iter()
            .filter_map(|e| match e.body {
                EventBody::AttemptEnded { completion } => Some(completion),
                _ => None,
            })
            .collect()
    }

    fn text(&self) -> String {
        self.sink
            .events()
            .into_iter()
            .filter_map(|e| match e.body {
                EventBody::TextDelta { text, .. } => Some(text),
                _ => None,
            })
            .collect()
    }

    fn request(&self) -> Option<PermissionRequest> {
        self.sink.events().into_iter().find_map(|e| match e.body {
            EventBody::PermissionRequested { request } => Some(request),
            _ => None,
        })
    }
}

/// Waits until the operation is finished; returns its final state.
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

fn ok(s: &Observed) -> bool {
    matches!(
        s,
        Observed::Finished {
            outcome: Outcome::Succeeded,
            ..
        }
    )
}

fn ws() -> WorkspaceRef {
    WorkspaceRef {
        node: NodeId::new("hub").unwrap(),
        id: WorkspaceId::new(derived_id("w", "item:x")).unwrap(),
    }
}

fn open_body(session: &str) -> OpenSession {
    OpenSession {
        session: SessionId::new(session).unwrap(),
        workspace: ws(),
        choice: choice(),
        policy: policy(),
        resume: None,
    }
}

fn turn(session: &str, t: &str, attempt: &str, text: &str) -> TurnSpec {
    TurnSpec {
        session: SessionId::new(session).unwrap(),
        turn: TurnId::new(t).unwrap(),
        run: RunSpec {
            attempt: AttemptId::new(attempt).unwrap(),
            choice: choice(),
            directory: RunDirectory::Workspace { workspace: ws() },
            prompt: vec![Part::Text { text: text.into() }],
            policy: policy(),
            limits: Limits {
                deadline: Timestamp(i64::MAX / 2),
                offline_until: Timestamp(i64::MAX / 2),
                ..limits()
            },
            continuation: None,
        },
    }
}

fn wall() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

/// A command that is accepted now (the real clock).
fn now_cmd<T>(op: &str, body: T) -> Command<T> {
    let mut c = cmd(op, body);
    c.issued_at = Timestamp(wall());
    c.accept_before = Timestamp(wall() + 60_000);
    c
}

fn prepare_proj() -> Prepare {
    Prepare {
        resource: "item:x".into(),
        kind: PrepareKind::Existing {
            dir: RootPath {
                root: RootId::new("work").unwrap(),
                rel: RelPath::new("proj").unwrap(),
            },
        },
    }
}

async fn ready(n: &LocalNode, session: &str) {
    n.prepare_workspace(now_cmd("op_prep", prepare_proj()))
        .await
        .unwrap();
    assert!(ok(&finished(n, "op_prep").await));
    let op = format!("op_open_{session}");
    n.open_session(now_cmd(&op, open_body(session)))
        .await
        .unwrap();
    let s = finished(n, &op).await;
    assert!(ok(&s), "{s:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_turn_runs_once_and_reports_in_steves_words() {
    let t = T::new(&[]);
    let n = t.node();
    ready(&n, "s_1").await;
    let c = now_cmd("op_t1", turn("s_1", "t_1", "a_1", "@say hello"));
    let r = n.start_turn(c.clone()).await.unwrap();
    assert_eq!(r.state, Observed::Running);
    // resent: the same receipt, no second prompt
    n.start_turn(c).await.unwrap();
    t.until("the end", |t| !t.ended().is_empty()).await;
    assert_eq!(t.sent("session/prompt"), 1);
    assert_eq!(t.text(), "hello");
    let e = &t.ended()[0];
    assert_eq!(
        (e.outcome, e.stop_reason.as_str()),
        (Outcome::Succeeded, "end_turn")
    );
    // the attempt's last event is its end
    let of_attempt: Vec<String> = t
        .sink
        .events()
        .into_iter()
        .filter(|e| matches!(&e.subject, Subject::Execution(x) if x.attempt().as_str() == "a_1"))
        .map(|e| {
            serde_json::to_value(&e.body).unwrap()["type"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
    assert_eq!(of_attempt.last().map(String::as_str), Some("attempt_ended"));
    // no ACP word reached the Hub
    let all = serde_json::to_string(&t.sink.events()).unwrap();
    for word in [
        "sessionUpdate",
        "agent_message_chunk",
        "_meta",
        "rawInput",
        "optionId",
    ] {
        assert!(!all.contains(word), "{word} leaked");
    }
    // the next attempt reuses the adapter process the open started
    n.start_turn(now_cmd("op_t2", turn("s_1", "t_2", "a_2", "@say again")))
        .await
        .unwrap();
    t.until("the second end", |t| t.ended().len() == 2).await;
    assert_eq!(t.sent("initialize"), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_node_per_home() {
    let t = T::new(&[]);
    let _n = t.node();
    let e = LocalNode::open(t.config()).err().expect("refused");
    assert_eq!(e.code, ErrorCode::Conflict);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cancel_ends_the_attempt_as_cancelled() {
    let t = T::new(&[]);
    let n = t.node();
    ready(&n, "s_1").await;
    n.start_turn(now_cmd("op_t", turn("s_1", "t_1", "a_1", "@sleep 5000")))
        .await
        .unwrap();
    t.until("the prompt", |t| t.sent("session/prompt") == 1)
        .await;
    n.cancel(now_cmd(
        "op_c",
        Cancel {
            target: OperationId::new("op_t").unwrap(),
            reason: "paused".into(),
        },
    ))
    .await
    .unwrap();
    t.until("the end", |t| !t.ended().is_empty()).await;
    assert_eq!(t.ended()[0].outcome, Outcome::Cancelled);
    assert_eq!(t.sent("session/cancel"), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_late_cancel_never_reaches_the_next_attempt() {
    let t = T::new(&[]);
    let n = t.node();
    ready(&n, "s_1").await;
    n.start_turn(now_cmd("op_a", turn("s_1", "t_1", "a_1", "@say one")))
        .await
        .unwrap();
    t.until("the first end", |t| t.ended().len() == 1).await;
    n.start_turn(now_cmd(
        "op_b",
        turn("s_1", "t_2", "a_2", "@sleep 1500 @say two"),
    ))
    .await
    .unwrap();
    t.until("the second prompt", |t| t.sent("session/prompt") == 2)
        .await;
    // a cancel of the first, which is over: nothing goes to the adapter
    n.cancel(now_cmd(
        "op_c",
        Cancel {
            target: OperationId::new("op_a").unwrap(),
            reason: "late".into(),
        },
    ))
    .await
    .unwrap();
    t.until("the second end", |t| t.ended().len() == 2).await;
    assert_eq!(t.sent("session/cancel"), 0);
    assert_eq!(t.ended()[1].outcome, Outcome::Succeeded);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_permission_waits_for_the_hubs_answer() {
    let t = T::new(&[]);
    let n = t.node();
    ready(&n, "s_1").await;
    n.start_turn(now_cmd("op_t", turn("s_1", "t_1", "a_1", "@perm Bash")))
        .await
        .unwrap();
    t.until("the request", |t| t.request().is_some()).await;
    let req = t.request().unwrap();
    assert_eq!(
        req.choices,
        vec![PermissionDecision::AllowOnce, PermissionDecision::Reject]
    );
    assert!(matches!(
        n.operation(OperationId::new("op_t").unwrap())
            .await
            .unwrap()
            .state,
        Observed::Waiting { .. }
    ));
    n.resolve_permission(now_cmd("op_r", answer(&req, PermissionDecision::AllowOnce)))
        .await
        .unwrap();
    t.until("the end", |t| !t.ended().is_empty()).await;
    // the adapter got Allow (once), never Always Allow
    assert!(t.text().contains("perm:selected:once"), "{}", t.text());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_answer_after_the_cancel_is_stale() {
    let t = T::new(&[]);
    let n = t.node();
    ready(&n, "s_1").await;
    n.start_turn(now_cmd("op_t", turn("s_1", "t_1", "a_1", "@perm Bash")))
        .await
        .unwrap();
    t.until("the request", |t| t.request().is_some()).await;
    let req = t.request().unwrap();
    n.cancel(now_cmd(
        "op_c",
        Cancel {
            target: OperationId::new("op_t").unwrap(),
            reason: "stop".into(),
        },
    ))
    .await
    .unwrap();
    t.until("the end", |t| !t.ended().is_empty()).await;
    assert!(t.kinds().contains(&"permission_expired".to_string()));
    let e = n
        .resolve_permission(now_cmd("op_r", answer(&req, PermissionDecision::AllowOnce)))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Stale);
    assert!(!t.text().contains("perm:selected"), "{}", t.text());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_model_that_does_not_take_fails_the_open() {
    let t = T::new(&[("FAKE_ACP_SHAPE", "claude099")]);
    let n = t.node();
    n.prepare_workspace(now_cmd(
        "op_prep",
        Prepare {
            resource: "item:x".into(),
            kind: PrepareKind::Scratch,
        },
    ))
    .await
    .unwrap();
    finished(&n, "op_prep").await;
    let mut o = open_body("s_1");
    o.choice.model = Some("no-such-model".into());
    n.open_session(now_cmd("op_open", o)).await.unwrap();
    let s = finished(&n, "op_open").await;
    let Observed::Finished {
        outcome: Outcome::Failed,
        error: Some(e),
        ..
    } = s
    else {
        panic!("{s:?}")
    };
    assert!(e.safe_detail.contains("no-such-model"), "{}", e.safe_detail);
    let v = n.session(SessionId::new("s_1").unwrap()).await.unwrap();
    assert_eq!(v.phase, SessionPhase::Released);
    assert_eq!(t.sent("session/prompt"), 0, "nothing ran on another model");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_crashed_adapter_interrupts_and_the_next_attempt_starts_fresh() {
    let t = T::new(&[]);
    let n = t.node();
    ready(&n, "s_1").await;
    n.start_turn(now_cmd(
        "op_t",
        turn("s_1", "t_1", "a_1", "@tool execute Bash @crash"),
    ))
    .await
    .unwrap();
    t.until("the end", |t| !t.ended().is_empty()).await;
    let e = &t.ended()[0];
    assert_eq!(e.outcome, Outcome::Interrupted);
    assert!(e.acted, "a tool ran before the crash");
    n.start_turn(now_cmd("op_t2", turn("s_1", "t_2", "a_2", "@say back")))
        .await
        .unwrap();
    t.until("the second end", |t| t.ended().len() == 2).await;
    assert_eq!(t.sent("initialize"), 2);
    assert_eq!(
        t.sent("session/load"),
        1,
        "the fresh adapter loads the session"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_deadline_ends_the_attempt() {
    let t = T::new(&[]);
    let n = t.node();
    ready(&n, "s_1").await;
    let mut tr = turn("s_1", "t_1", "a_1", "@sleep 30000");
    tr.run.limits.deadline = Timestamp(wall() + 1500);
    n.start_turn(now_cmd("op_t", tr)).await.unwrap();
    t.until("the end", |t| !t.ended().is_empty()).await;
    assert_eq!(t.ended()[0].outcome, Outcome::TimedOut);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn what_cannot_be_enforced_is_refused() {
    let t = T::new(&[]);
    let n = t.node();
    ready(&n, "s_1").await;
    let mut tr = turn("s_1", "t_1", "a_1", "@say x");
    tr.run.limits.max_cost = Some(Money {
        decimal: "1".into(),
        currency: "USD".into(),
    });
    let e = n.start_turn(now_cmd("op_t", tr)).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::Unsupported);
    let mut o = open_body("s_2");
    o.policy.network = NetworkScope::Denied;
    let e = n.open_session(now_cmd("op_o", o)).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::Unsupported);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn checking_a_workspace_touches_nothing_there() {
    let t = T::new(&[]);
    let n = t.node();
    let proj = t.root.join("work/proj");
    std::fs::write(proj.join(".steve-node-probe"), "mine").unwrap();
    ready(&n, "s_1").await;
    let v = n.workspace(ws()).await.unwrap();
    assert!(v.usable);
    assert_eq!(
        std::fs::read_to_string(proj.join(".steve-node-probe")).unwrap(),
        "mine"
    );
    let left: Vec<_> = std::fs::read_dir(&proj)
        .unwrap()
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| n.starts_with(".steve-node-probe-"))
        .collect();
    assert!(left.is_empty(), "{left:?}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn release_finishes_once_the_harness_is_gone() {
    let t = T::new(&[]);
    let n = t.node();
    ready(&n, "s_1").await;
    n.release_session(now_cmd("op_rel", SessionId::new("s_1").unwrap()))
        .await
        .unwrap();
    assert!(ok(&finished(&n, "op_rel").await));
    let v = n.session(SessionId::new("s_1").unwrap()).await.unwrap();
    assert_eq!(v.phase, SessionPhase::Released);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn paths_stay_inside_the_roots() {
    let t = T::new(&[]);
    let n = t.node();
    let e = n
        .prepare_workspace(now_cmd(
            "op_other",
            Prepare {
                resource: "item:o".into(),
                kind: PrepareKind::Existing {
                    dir: RootPath {
                        root: RootId::new("elsewhere").unwrap(),
                        rel: RelPath::new("x").unwrap(),
                    },
                },
            },
        ))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Denied);
    std::os::unix::fs::symlink("/", t.root.join("work/out")).unwrap();
    let e = n
        .prepare_workspace(now_cmd(
            "op_link",
            Prepare {
                resource: "item:l".into(),
                kind: PrepareKind::Existing {
                    dir: RootPath {
                        root: RootId::new("work").unwrap(),
                        rel: RelPath::new("out").unwrap(),
                    },
                },
            },
        ))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Denied);
}

// ── a real crash: the Node is a child process killed with SIGKILL ──

/// Runs in a child process (see `a_killed_node_leaves_its_attempt_unknown_until_abandoned`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn child_node_process() {
    let Ok(root) = std::env::var("STEVE_NODE_CHILD_ROOT") else {
        return;
    };
    let t = T::at(Path::new(&root), &[]);
    let n = t.node();
    ready(&n, "s_1").await;
    n.start_turn(now_cmd("op_t", turn("s_1", "t_1", "a_1", "@sleep 60000")))
        .await
        .unwrap();
    t.until("the prompt", |t| t.sent("session/prompt") == 1)
        .await;
    std::fs::write(Path::new(&root).join("ready"), "").unwrap();
    tokio::time::sleep(Duration::from_secs(120)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_killed_node_leaves_its_attempt_unknown_until_abandoned() {
    let t = T::new(&[]);
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "child_node_process", "--nocapture"])
        .env("STEVE_NODE_CHILD_ROOT", &t.root)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let t0 = Instant::now();
    while !t.root.join("ready").exists() {
        assert!(
            t0.elapsed() < Duration::from_secs(20),
            "child never got ready"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    child.kill().unwrap();
    child.wait().unwrap();
    // the adapter's group (its own process group) outlived the Node
    let n = t.node();
    assert!(n.incarnation() >= 2);
    let r = n
        .operation(OperationId::new("op_t").unwrap())
        .await
        .unwrap();
    assert_eq!(r.state.tag(), StateTag::Unknown);
    // nothing new runs in that session, not even after opening it again
    let e = n
        .open_session(now_cmd("op_reopen", open_body("s_1")))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Busy);
    // the Hub gives it up: its processes are stopped first
    n.abandon(now_cmd(
        "op_ab",
        Abandon {
            target: OperationId::new("op_t").unwrap(),
            reason: "node crashed".into(),
        },
    ))
    .await
    .unwrap();
    assert!(ok(&finished(&n, "op_ab").await));
    let r = n
        .operation(OperationId::new("op_t").unwrap())
        .await
        .unwrap();
    assert!(matches!(
        r.state,
        Observed::Finished {
            outcome: Outcome::Interrupted,
            ..
        }
    ));
    n.open_session(now_cmd("op_reopen2", open_body("s_1")))
        .await
        .unwrap();
    assert!(ok(&finished(&n, "op_reopen2").await));
}

// ── Steve's tools through the Node's endpoint ──

#[derive(Default)]
struct Tools {
    calls: std::sync::Mutex<Vec<ToolCall>>,
}

#[async_trait::async_trait]
impl HubTools for Tools {
    async fn catalog(&self, _e: ExecutionRef) -> ApiResult<Catalog> {
        Ok(Catalog {
            revision: Revision::new("c1").unwrap(),
            tools: vec![ToolDescriptor {
                name: "steve_update".into(),
                description: "Update the item.".into(),
                input_schema: serde_json::json!({"type": "object", "properties": {"now": {"type": "string"}}}),
                read_only: false,
            }],
        })
    }
    async fn call(&self, q: ToolCall) -> ApiResult<ToolResult> {
        self.calls.lock().unwrap().push(q);
        Ok(ToolResult::Completed {
            parts: vec![Part::Text {
                text: "Steve updated the item.".into(),
            }],
            structured: None,
            end_turn: false,
        })
    }
    async fn call_status(&self, _k: ToolKey) -> ApiResult<ToolResult> {
        Err(ApiError::new(ErrorCode::Unsupported, "x"))
    }
    async fn cancel_call(&self, _k: ToolKey) -> ApiResult<ToolResult> {
        Err(ApiError::new(ErrorCode::Unsupported, "x"))
    }
}

/// A JSON-RPC POST to an MCP endpoint over plain HTTP/1.1.
async fn mcp(url: &str, token: &str, body: serde_json::Value) -> (u16, serde_json::Value) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let rest = url.strip_prefix("http://").unwrap();
    let (host, path) = rest.split_once('/').unwrap();
    let mut s = tokio::net::TcpStream::connect(host).await.unwrap();
    let b = body.to_string();
    let req = format!(
        "POST /{path} HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nMcp-Protocol-Version: 2025-06-18\r\nAuthorization: Bearer {token}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{b}",
        b.len()
    );
    s.write_all(req.as_bytes()).await.unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).await.unwrap();
    let status: u16 = out.split_whitespace().nth(1).unwrap().parse().unwrap();
    let body = out.split("\r\n\r\n").nth(1).unwrap_or("");
    let v = body
        .find('{')
        .and_then(|i| body.rfind('}').map(|j| &body[i..=j]))
        .and_then(|j| serde_json::from_str(j).ok())
        .unwrap_or(serde_json::Value::Null);
    (status, v)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn steves_tools_reach_the_hub_as_the_running_attempt() {
    let t = T::new(&[]);
    let n = t.node();
    let tools = Arc::new(Tools::default());
    n.connect_tools(tools.clone()).await.unwrap();
    ready(&n, "s_1").await;
    n.start_turn(now_cmd("op_t", turn("s_1", "t_1", "a_1", "@sleep 3000")))
        .await
        .unwrap();
    t.until("the prompt", |t| t.sent("session/prompt") == 1)
        .await;
    // what the adapter was given at session/new
    let log = std::fs::read_to_string(&t.log).unwrap();
    let new = log
        .lines()
        .find(|l| l.contains("\"method\":\"session/new\""))
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(new).unwrap();
    let server = &v["params"]["mcpServers"][0];
    let url = server["url"].as_str().unwrap().to_string();
    let token = server["headers"][0]["value"]
        .as_str()
        .unwrap()
        .strip_prefix("Bearer ")
        .unwrap()
        .to_string();
    assert!(url.starts_with("http://127.0.0.1:"));
    let (s, list) = mcp(
        &url,
        &token,
        serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
    )
    .await;
    assert_eq!(s, 200, "{list}");
    assert_eq!(list["result"]["tools"][0]["name"], "steve_update");
    let (_, called) = mcp(
        &url,
        &token,
        serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": {"name": "steve_update", "arguments": {"now": "写测试"}}}),
    )
    .await;
    assert_eq!(
        called["result"]["content"][0]["text"],
        "Steve updated the item."
    );
    let calls = tools.calls.lock().unwrap().clone();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].execution,
        ExecutionRef::Turn {
            session: SessionId::new("s_1").unwrap(),
            turn: TurnId::new("t_1").unwrap(),
            attempt: AttemptId::new("a_1").unwrap(),
        }
    );
    assert_eq!(calls[0].args["now"], "写测试");
    let (s, _) = mcp(
        &url,
        "nope",
        serde_json::json!({"jsonrpc": "2.0", "id": 3, "method": "tools/list"}),
    )
    .await;
    assert_eq!(s, 401);
    n.cancel(now_cmd(
        "op_c",
        Cancel {
            target: OperationId::new("op_t").unwrap(),
            reason: "x".into(),
        },
    ))
    .await
    .unwrap();
    t.until("the end", |t| !t.ended().is_empty()).await;
    let (_, after) = mcp(
        &url,
        &token,
        serde_json::json!({"jsonrpc": "2.0", "id": 4, "method": "tools/call",
            "params": {"name": "steve_update", "arguments": {"now": "x"}}}),
    )
    .await;
    assert!(after.get("error").is_some(), "{after}");
    assert_eq!(tools.calls.lock().unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_permission_is_bound_to_the_file_as_it_was() {
    let t = T::new(&[]);
    let n = t.node();
    let file = t.root.join("work/proj/a.txt");
    std::fs::write(&file, "one").unwrap();
    ready(&n, "s_1").await;
    let path = file.canonicalize().unwrap();
    n.start_turn(now_cmd(
        "op_t",
        turn(
            "s_1",
            "t_1",
            "a_1",
            &format!("@permtool edit {}", path.display()),
        ),
    ))
    .await
    .unwrap();
    t.until("the request", |t| t.request().is_some()).await;
    let req = t.request().unwrap();
    let Action::Write { paths, .. } = &req.action else {
        panic!("{:?}", req.action)
    };
    assert_eq!(paths[0].rel.as_str(), "a.txt");
    assert!(req.preconditions.iter().any(|p| matches!(
        p,
        Precondition::File {
            version: Some(_),
            ..
        }
    )));
    // the file changes before the answer: the approval no longer applies
    std::thread::sleep(Duration::from_millis(20));
    std::fs::write(&file, "two, longer").unwrap();
    // accepted, then checked right before it would be given: it is not given
    n.resolve_permission(now_cmd("op_r", answer(&req, PermissionDecision::AllowOnce)))
        .await
        .unwrap();
    let s = finished(&n, "op_r").await;
    assert!(
        matches!(&s, Observed::Finished { outcome: Outcome::Failed, error: Some(e), .. } if e.code == ErrorCode::Stale),
        "{s:?}"
    );
    // a rejection still goes through
    n.resolve_permission(now_cmd("op_r2", answer(&req, PermissionDecision::Reject)))
        .await
        .unwrap();
    assert!(ok(&finished(&n, "op_r2").await));
    t.until("the end", |t| !t.ended().is_empty()).await;
    assert!(t.text().contains("perm:selected:reject"), "{}", t.text());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_permission_outside_the_workspace_says_so() {
    let t = T::new(&[]);
    let n = t.node();
    ready(&n, "s_1").await;
    n.start_turn(now_cmd(
        "op_t",
        turn("s_1", "t_1", "a_1", "@permtool edit /etc/hosts"),
    ))
    .await
    .unwrap();
    t.until("the request", |t| t.request().is_some()).await;
    let req = t.request().unwrap();
    assert!(
        matches!(&req.action, Action::Unknown { summary } if summary.contains("工作区外") && summary.contains("/etc/hosts")),
        "{:?}",
        req.action
    );
    // the tool's view and the request name the same call
    let tool_ids: Vec<CallId> = t
        .sink
        .events()
        .into_iter()
        .filter_map(|e| match e.body {
            EventBody::ToolChanged { tool } => Some(tool.id),
            _ => None,
        })
        .collect();
    assert!(
        tool_ids.contains(&req.invocation),
        "{tool_ids:?} {:?}",
        req.invocation
    );
}

/// Attachments that never come (a Hub that hangs).
struct Hanging;

#[async_trait::async_trait]
impl HubFiles for Hanging {
    async fn fetch(
        &self,
        _a: FileAccess,
        _id: ArtifactId,
        _o: u64,
        _m: u32,
    ) -> ApiResult<ReadChunk> {
        tokio::time::sleep(Duration::from_secs(3600)).await;
        Err(ApiError::new(ErrorCode::Offline, "never"))
    }
    async fn begin_upload(&self, _b: BeginUpload) -> ApiResult<Transfer> {
        Err(ApiError::new(ErrorCode::Unsupported, "x"))
    }
    async fn upload(&self, _c: BlobChunk) -> ApiResult<Transfer> {
        Err(ApiError::new(ErrorCode::Unsupported, "x"))
    }
    async fn upload_status(&self, _t: TransferId) -> ApiResult<TransferState> {
        Err(ApiError::new(ErrorCode::Unsupported, "x"))
    }
    async fn finish_upload(&self, _t: TransferId) -> ApiResult<ArtifactRef> {
        Err(ApiError::new(ErrorCode::Unsupported, "x"))
    }
    async fn abort_upload(&self, _t: TransferId) -> ApiResult<TransferState> {
        Err(ApiError::new(ErrorCode::Unsupported, "x"))
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cancel_while_starting_means_no_prompt_at_all() {
    let t = T::new(&[]);
    let n = LocalNode::open(t.config()).unwrap();
    n.connect(t.sink.clone(), Some(Arc::new(Hanging)));
    ready(&n, "s_1").await;
    let mut tr = turn("s_1", "t_1", "a_1", "@say no");
    tr.run.prompt.push(Part::File {
        artifact: ArtifactRef {
            id: ArtifactId::new("f_1").unwrap(),
            sha256: Digest::sha256(b"x"),
            size: 1,
            mime: "text/plain".into(),
        },
        name: "a.txt".into(),
    });
    n.start_turn(now_cmd("op_t", tr)).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    n.cancel(now_cmd(
        "op_c",
        Cancel {
            target: OperationId::new("op_t").unwrap(),
            reason: "x".into(),
        },
    ))
    .await
    .unwrap();
    t.until("the end", |t| !t.ended().is_empty()).await;
    assert_eq!(t.ended()[0].outcome, Outcome::Cancelled);
    assert_eq!(t.sent("session/prompt"), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_frees_the_home() {
    let t = T::new(&[]);
    let n = t.node();
    ready(&n, "s_1").await;
    n.shutdown().await;
    drop(n);
    // the delivery loop and the actors let go of it
    let t0 = Instant::now();
    let again = loop {
        match LocalNode::open(t.config()) {
            Ok(x) => break x,
            Err(e) => {
                assert_eq!(e.code, ErrorCode::Conflict);
                assert!(t0.elapsed() < Duration::from_secs(10), "home never freed");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    };
    assert!(again.incarnation() >= 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reads_that_cannot_be_confined_are_refused() {
    let t = T::new(&[]);
    let n = t.node();
    ready(&n, "s_1").await;
    let mut o = open_body("s_2");
    o.policy.readable = vec![PathRef {
        workspace: ws(),
        rel: RelPath::new("other").unwrap(),
    }];
    let e = n.open_session(now_cmd("op_o", o)).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::Unsupported);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn opening_reserves_session_and_workspace_until_handshake_finishes() {
    let t = T::new(&[("FAKE_ACP_OPEN_DELAY", "400")]);
    let n = t.node();
    n.prepare_workspace(now_cmd("prep", prepare_proj()))
        .await
        .unwrap();
    assert!(ok(&finished(&n, "prep").await));
    n.open_session(now_cmd("open", open_body("s")))
        .await
        .unwrap();
    t.until("handshake blocked", |t| t.sent("session/new") == 1)
        .await;
    assert_eq!(
        n.release_session(now_cmd("release", SessionId::new("s").unwrap()))
            .await
            .unwrap_err()
            .code,
        ErrorCode::Busy
    );
    let w = n.workspace(ws()).await.unwrap();
    assert_eq!(
        n.tidy_workspace(now_cmd(
            "tidy",
            Tidy {
                workspace: ws(),
                expected: w.revision,
                keep_branch: true
            }
        ))
        .await
        .unwrap_err()
        .code,
        ErrorCode::Busy
    );
    assert!(ok(&finished(&n, "open").await));
    n.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_joins_an_inflight_handshake_before_releasing_home() {
    let t = T::new(&[("FAKE_ACP_OPEN_DELAY", "10000")]);
    let n = t.node();
    n.prepare_workspace(now_cmd("prep", prepare_proj()))
        .await
        .unwrap();
    assert!(ok(&finished(&n, "prep").await));
    n.open_session(now_cmd("open", open_body("s")))
        .await
        .unwrap();
    t.until("inflight handshake", |t| t.sent("session/new") == 1)
        .await;
    tokio::time::timeout(Duration::from_secs(3), n.shutdown())
        .await
        .unwrap();
    assert!(matches!(
        n.operation(OperationId::new("open").unwrap())
            .await
            .unwrap()
            .state,
        Observed::Unknown { .. }
    ));
    assert_eq!(
        n.open_session(now_cmd("late", open_body("late")))
            .await
            .unwrap_err()
            .code,
        ErrorCode::Offline
    );
    drop(n);
    let again = LocalNode::open(t.config()).unwrap();
    again.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_cancels_a_hanging_fetch_instead_of_waiting_for_its_deadline() {
    let t = T::new(&[]);
    let n = LocalNode::open(t.config()).unwrap();
    n.connect(t.sink.clone(), Some(Arc::new(Hanging)));
    ready(&n, "s").await;
    let mut tr = turn("s", "t", "a", "@say never");
    tr.run.prompt.push(Part::File {
        artifact: ArtifactRef {
            id: ArtifactId::new("f").unwrap(),
            sha256: Digest::sha256(b"x"),
            size: 1,
            mime: "text/plain".into(),
        },
        name: "x.txt".into(),
    });
    n.start_turn(now_cmd("run", tr)).await.unwrap();
    tokio::task::yield_now().await;
    tokio::time::timeout(Duration::from_secs(3), n.shutdown())
        .await
        .unwrap();
    assert_eq!(t.sent("session/prompt"), 0);
    drop(n);
    let again = LocalNode::open(t.config()).unwrap();
    again.shutdown().await;
}
