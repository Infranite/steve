//! LocalNode against the scripted ACP adapter of steve-acp (`fake-acp.js`, run by node): the
//! contract's rules with a real process behind them.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use steve_acp::AcpConfig;
use steve_node::{LocalNode, NodeConfig, Root, Storage};
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
                access: RootAccess::ReadWrite,
            }],
            capacity: Capacity {
                sessions: 4,
                jobs: 0,
                execs: 0,
            },
            storage: Storage::default(),
        }
    }

    /// A Node whose filesystem has as many free bytes as `free` says (the D6 defaults otherwise).
    fn node_with_free(&self, free: &Arc<AtomicU64>) -> LocalNode {
        let free = free.clone();
        let mut cfg = self.config();
        cfg.storage.free_space = Some(Arc::new(move || free.load(Ordering::SeqCst)));
        let n = LocalNode::open(cfg).unwrap();
        n.connect(self.sink.clone(), None);
        n
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

/// The adapter's `session/set_mode` calls, in order.
fn modes_sent(t: &T) -> Vec<String> {
    std::fs::read_to_string(&t.log)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| serde_json::from_str::<serde_json::Value>(l).ok())
        .filter(|m| m["method"] == "session/set_mode")
        .filter_map(|m| m["params"]["modeId"].as_str().map(str::to_string))
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn auto_is_a_mode_of_its_own_at_open_and_configure() {
    let t = T::new(&[]);
    let n = t.node();
    n.prepare_workspace(now_cmd("op_prep", prepare_proj()))
        .await
        .unwrap();
    assert!(ok(&finished(&n, "op_prep").await));
    let mut o = open_body("s_1");
    o.policy.permissions = PermissionMode::Auto;
    n.open_session(now_cmd("op_open", o)).await.unwrap();
    let s = finished(&n, "op_open").await;
    assert!(ok(&s), "{s:?}");
    let v = n.session(SessionId::new("s_1").unwrap()).await.unwrap();
    assert_eq!(v.effective_permissions, PermissionMode::Auto);
    assert_eq!(modes_sent(&t), ["auto"], "Claude Code's own auto mode");
    // away and back: Ask is set explicitly, then auto again
    for (op, p) in [
        ("op_cfg_ask", PermissionMode::Ask),
        ("op_cfg_auto", PermissionMode::Auto),
    ] {
        let v = n.session(SessionId::new("s_1").unwrap()).await.unwrap();
        let mut policy = policy();
        policy.permissions = p;
        n.configure_session(now_cmd(
            op,
            ConfigureSession {
                session: SessionId::new("s_1").unwrap(),
                expected: v.revision,
                choice: choice(),
                policy,
            },
        ))
        .await
        .unwrap();
        let s = finished(&n, op).await;
        assert!(ok(&s), "{op}: {s:?}");
        let v = n.session(SessionId::new("s_1").unwrap()).await.unwrap();
        assert_eq!(v.effective_permissions, p);
    }
    assert_eq!(modes_sent(&t), ["auto", "default", "auto"]);
    // a turn runs in it
    let mut tu = turn("s_1", "t_1", "a_1", "@say auto");
    tu.run.policy.permissions = PermissionMode::Auto;
    n.start_turn(now_cmd("op_t1", tu)).await.unwrap();
    t.until("the end", |t| !t.ended().is_empty()).await;
    assert_eq!(t.text(), "auto");
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

fn prepare_in(resource: &str, root: &str, rel: &str) -> Prepare {
    Prepare {
        resource: resource.into(),
        kind: PrepareKind::Existing {
            dir: RootPath {
                root: RootId::new(root).unwrap(),
                rel: RelPath::new(rel).unwrap(),
            },
        },
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn roots_change_without_a_restart_and_a_read_only_root_takes_no_workspace() {
    let t = T::new(&[]);
    std::fs::create_dir_all(t.root.join("later/p")).unwrap();
    std::fs::create_dir_all(t.root.join("kb/notes")).unwrap();
    let n = t.node();
    // not a root yet
    let e = n
        .prepare_workspace(now_cmd("op_later0", prepare_in("item:l", "later", "p")))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Denied);
    let work = t.config().roots.remove(0);
    n.set_roots(vec![
        work.clone(),
        Root {
            id: RootId::new("later").unwrap(),
            path: t.root.join("later"),
            display: "~/later".into(),
            access: RootAccess::ReadWrite,
        },
        Root {
            id: RootId::new("kb").unwrap(),
            path: t.root.join("kb"),
            display: "~/kb".into(),
            access: RootAccess::ReadOnly,
        },
    ]);
    // a root added after open: prepared with no restart
    n.prepare_workspace(now_cmd("op_later", prepare_in("item:l", "later", "p")))
        .await
        .unwrap();
    assert!(ok(&finished(&n, "op_later").await));
    // a read-only root is shown as such and takes no workspace
    let caps = n.probe(false).await.unwrap();
    let kb = caps.roots.iter().find(|r| r.id.as_str() == "kb").unwrap();
    assert_eq!(kb.access, RootAccess::ReadOnly);
    let e = n
        .prepare_workspace(now_cmd("op_kb", prepare_in("item:k", "kb", "notes")))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Denied);
    // a root taken away (an archived project): refused for new work
    n.set_roots(vec![work]);
    let e = n
        .prepare_workspace(now_cmd("op_later2", prepare_in("item:m", "later", "p")))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Denied);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_node_with_no_session_capacity_takes_workspace_work_only() {
    let t = T::new(&[]);
    let mut cfg = t.config();
    cfg.capacity.sessions = 0;
    let n = LocalNode::open(cfg).unwrap();
    n.connect(t.sink.clone(), None);
    n.prepare_workspace(now_cmd("op_prep", prepare_proj()))
        .await
        .unwrap();
    assert!(ok(&finished(&n, "op_prep").await));
    let e = n
        .open_session(now_cmd("op_open", open_body("s1")))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Unsupported);
    assert_eq!(t.sent("initialize"), 0, "no harness started");
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

/// For `kill <pid>` the Node says which processes are the harness's own (the adapter's children, not
/// the adapter): a fact for the Hub's judgement, outside the digest. Any other command gets none.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_kill_permission_carries_the_harness_processes() {
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("grandchild.pid");
    let t = T::new(&[("FAKE_ACP_GRANDCHILD", pidfile.to_str().unwrap())]);
    let n = t.node();
    ready(&n, "s_1").await;
    let mut child = String::new();
    for _ in 0..200 {
        child = std::fs::read_to_string(&pidfile).unwrap_or_default();
        if !child.trim().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    let child: u32 = child.trim().parse().unwrap();
    n.start_turn(now_cmd(
        "op_t",
        turn("s_1", "t_1", "a_1", &format!("@permexec kill {child}")),
    ))
    .await
    .unwrap();
    t.until("the request", |t| t.request().is_some()).await;
    let req = t.request().unwrap();
    assert!(
        matches!(&req.action, Action::Exec { command, .. } if command == &format!("kill {child}")),
        "{:?}",
        req.action
    );
    assert!(req.own_pids.contains(&child), "{:?}", req.own_pids);
    let adapter = std::fs::read_to_string(&t.log)
        .unwrap()
        .lines()
        .find_map(|l| serde_json::from_str::<serde_json::Value>(l).ok()?["spawn"]["pid"].as_u64())
        .unwrap() as u32;
    assert!(
        !req.own_pids.contains(&adapter),
        "the adapter is not its own child"
    );
    // the digest binds the action and preconditions only
    assert_eq!(
        permission_digest(&req.action, &req.preconditions).unwrap(),
        req.digest
    );
    n.resolve_permission(now_cmd("op_r", answer(&req, PermissionDecision::Reject)))
        .await
        .unwrap();
    t.until("the end", |t| !t.ended().is_empty()).await;

    n.start_turn(now_cmd(
        "op_t2",
        turn("s_1", "t_2", "a_2", "@permexec echo hi"),
    ))
    .await
    .unwrap();
    t.until("the second request", |t| {
        t.sink.events().into_iter().any(|e| matches!(e.body, EventBody::PermissionRequested { ref request } if request.title == "echo hi"))
    })
    .await;
    let second = t
        .sink
        .events()
        .into_iter()
        .find_map(|e| match e.body {
            EventBody::PermissionRequested { request } if request.title == "echo hi" => {
                Some(request)
            }
            _ => None,
        })
        .unwrap();
    assert!(second.own_pids.is_empty());
    unsafe { libc::kill(child as i32, libc::SIGKILL) };
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn handshake_permission_is_answered_without_a_live_session() {
    let t = T::new(&[("FAKE_ACP_OPEN_PERMISSION", "1")]);
    let n = t.node();
    tokio::time::timeout(Duration::from_secs(5), ready(&n, "s_1"))
        .await
        .unwrap();
    assert!(
        std::fs::read_to_string(&t.log)
            .unwrap()
            .contains("cancelled")
    );
    n.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn terminal_commit_retries_preserve_closing_and_release() {
    let t = T::new(&[]);
    let n = t.node();
    ready(&n, "s_1").await;
    let db = rusqlite::Connection::open(t.home.join("node.sqlite3")).unwrap();
    db.execute_batch("CREATE TRIGGER fail_end BEFORE UPDATE ON ops WHEN NEW.id='op_t1' AND NEW.receipt LIKE '%finished%' BEGIN SELECT RAISE(ABORT,'injected disk failure'); END;").unwrap();
    n.start_turn(now_cmd("op_t1", turn("s_1", "t_1", "a_1", "@say hello")))
        .await
        .unwrap();
    t.until("text before failed closing", |t| t.text().contains("hello"))
        .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !n.operation(OperationId::new("op_t1").unwrap())
            .await
            .unwrap()
            .state
            .is_finished()
    );
    db.execute_batch("DROP TRIGGER fail_end").unwrap();
    assert!(ok(&finished(&n, "op_t1").await));
    t.until("closing", |t| {
        t.sink
            .events()
            .iter()
            .any(|e| matches!(e.body, EventBody::MessageCompleted { .. }))
    })
    .await;
    assert_eq!(
        t.sink
            .events()
            .iter()
            .filter(|e| matches!(e.body, EventBody::MessageCompleted { .. }))
            .count(),
        1
    );
    // A faulted Node still accepts cancellation/abandon; it never re-runs the attempt.
    n.cancel(now_cmd(
        "cancel_after_fault",
        Cancel {
            target: OperationId::new("op_t1").unwrap(),
            reason: "cleanup".into(),
        },
    ))
    .await
    .unwrap();
    assert_eq!(t.sent("session/prompt"), 1);
    n.shutdown().await;
}

struct EmptyAck(std::sync::atomic::AtomicUsize);
#[async_trait::async_trait]
impl EventSink for EmptyAck {
    async fn commit(&self, _batch: EventBatch) -> ApiResult<EventAck> {
        self.0.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Ok(EventAck { durable: vec![] })
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disconnected_lease_and_empty_ack_backoff() {
    let t = T::new(&[]);
    let n = LocalNode::open(t.config()).unwrap();
    ready(&n, "s_1").await;
    let mut spec = turn("s_1", "t_1", "a_1", "@sleep 20000");
    spec.run.limits.offline_until = Timestamp(wall() + 200);
    n.start_turn(now_cmd("op_t1", spec)).await.unwrap();
    assert!(matches!(
        finished(&n, "op_t1").await,
        Observed::Finished {
            outcome: Outcome::TimedOut,
            ..
        }
    ));
    let sink = Arc::new(EmptyAck(std::sync::atomic::AtomicUsize::new(0)));
    n.connect(sink.clone(), None);
    tokio::time::sleep(Duration::from_millis(800)).await;
    let calls = sink.0.load(std::sync::atomic::Ordering::SeqCst);
    assert!((1..=5).contains(&calls), "empty ack hot loop: {calls}");
    n.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn unknown_workspace_controls_recover_via_public_port() {
    let t = T::new(&[]);
    let n = t.node();
    let db = rusqlite::Connection::open(t.home.join("node.sqlite3")).unwrap();
    db.execute_batch("CREATE TRIGGER fail_prepare BEFORE UPDATE ON ops WHEN NEW.id='prep_crash' AND NEW.receipt LIKE '%finished%' BEGIN SELECT RAISE(ABORT,'crash before final receipt'); END").unwrap();
    let c = now_cmd(
        "prep_crash",
        Prepare {
            resource: "recover_scratch".into(),
            kind: PrepareKind::Scratch,
        },
    );
    n.prepare_workspace(c.clone()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    n.shutdown().await;
    drop(n);
    db.execute_batch("DROP TRIGGER fail_prepare").unwrap();
    let n = t.node();
    assert!(matches!(
        n.operation(c.id.clone()).await.unwrap().state,
        Observed::Unknown { .. }
    ));
    let recover = now_cmd(
        "recover_prep",
        Recover {
            target: c.id.clone(),
            expected: c.digest("prepare_workspace").unwrap(),
            reason: "retire interrupted prepare".into(),
        },
    );
    n.recover(recover.clone()).await.unwrap();
    assert!(ok(&finished(&n, "recover_prep").await));
    assert!(matches!(
        n.operation(c.id.clone()).await.unwrap().state,
        Observed::Finished {
            outcome: Outcome::Interrupted,
            ..
        }
    ));
    assert!(ok(&n.recover(recover).await.unwrap().state));
    n.prepare_workspace(now_cmd("prep_retry", c.body))
        .await
        .unwrap();
    let Observed::Finished {
        result: Some(OperationResult::Workspace(w)),
        ..
    } = finished(&n, "prep_retry").await
    else {
        panic!("prepare")
    };
    db.execute_batch("CREATE TRIGGER fail_tidy BEFORE UPDATE ON ops WHEN NEW.id='tidy_crash' AND NEW.receipt LIKE '%finished%' BEGIN SELECT RAISE(ABORT,'crash after removal'); END").unwrap();
    let tidy = now_cmd(
        "tidy_crash",
        Tidy {
            workspace: w.workspace.clone(),
            expected: w.revision,
            keep_branch: true,
        },
    );
    n.tidy_workspace(tidy.clone()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    n.shutdown().await;
    drop(n);
    db.execute_batch("DROP TRIGGER fail_tidy").unwrap();
    let n = t.node();
    n.recover(now_cmd(
        "recover_tidy",
        Recover {
            target: tidy.id.clone(),
            expected: tidy.digest("tidy_workspace").unwrap(),
            reason: "retire after removal".into(),
        },
    ))
    .await
    .unwrap();
    assert!(ok(&finished(&n, "recover_tidy").await));
    n.tidy_workspace(now_cmd("tidy_retry", tidy.body))
        .await
        .unwrap();
    assert!(ok(&finished(&n, "tidy_retry").await));
    n.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn all_ingress_frames_are_bounded_and_control_stays_responsive() {
    for prompt in ["@flood", "@bigperm", "@bigreply"] {
        let t = T::new(&[]);
        let n = t.node();
        ready(&n, "s_1").await;
        let mut spec = turn("s_1", "t_1", "a_1", prompt);
        spec.run.limits.max_output_bytes = 20_000;
        n.start_turn(now_cmd("op_t1", spec)).await.unwrap();
        let state = tokio::time::timeout(Duration::from_secs(5), finished(&n, "op_t1"))
            .await
            .unwrap();
        assert!(
            matches!(
                state,
                Observed::Finished {
                    outcome: Outcome::Cancelled,
                    ..
                }
            ),
            "{prompt}: {state:?}"
        );
        assert!(t.text().len() <= 20_000);
        n.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn uncertain_stop_retries_unknown_event_and_expires_permissions() {
    let dir = tempfile::tempdir().unwrap();
    let pidfile = dir.path().join("descendant");
    let t = T::at(
        dir.path(),
        &[("FAKE_ACP_GRANDCHILD", pidfile.to_str().unwrap())],
    );
    let n = t.node();
    ready(&n, "s_1").await;
    let descendant: i32 = std::fs::read_to_string(&pidfile).unwrap().parse().unwrap();
    struct Cleanup(i32);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            unsafe {
                libc::kill(self.0, libc::SIGKILL);
            }
        }
    }
    let cleanup = Cleanup(descendant);
    n.start_turn(now_cmd(
        "op_t1",
        turn("s_1", "t_1", "a_1", "@permtool read absent"),
    ))
    .await
    .unwrap();
    t.until("waiting", |t| t.request().is_some()).await;
    let db = rusqlite::Connection::open(t.home.join("node.sqlite3")).unwrap();
    db.execute_batch("CREATE TRIGGER fail_unknown BEFORE UPDATE ON ops WHEN NEW.id='op_t1' AND NEW.receipt LIKE '%unknown%' BEGIN SELECT RAISE(ABORT,'unknown commit failure'); END").unwrap();
    let leader: i32 = std::fs::read_to_string(&t.log)
        .unwrap()
        .lines()
        .filter_map(|line| serde_json::from_str::<serde_json::Value>(line).ok())
        .find_map(|v| v.pointer("/spawn/pid").and_then(|v| v.as_i64()))
        .unwrap() as i32;
    unsafe {
        libc::kill(leader, libc::SIGKILL);
    }
    tokio::time::sleep(Duration::from_millis(800)).await;
    db.execute_batch("DROP TRIGGER fail_unknown").unwrap();
    t.until("unknown event", |t| t.sink.events().iter().any(|e| matches!(&e.body, EventBody::OperationChanged {receipt} if receipt.operation.as_str()=="op_t1" && matches!(receipt.state, Observed::Unknown {..})))).await;
    assert!(
        t.sink
            .events()
            .iter()
            .any(|e| matches!(&e.body, EventBody::PermissionExpired { .. }))
    );
    assert!(
        t.ended().is_empty(),
        "uncertain is not a terminal execution"
    );
    drop(cleanup);
    tokio::time::sleep(Duration::from_millis(100)).await;
    n.abandon(now_cmd(
        "abandon_unknown",
        Abandon {
            target: OperationId::new("op_t1").unwrap(),
            reason: "descendant externally stopped".into(),
        },
    ))
    .await
    .unwrap();
    assert!(ok(&finished(&n, "abandon_unknown").await));
    n.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn prepare_and_release_retry_after_storage_recovers() {
    let t = T::new(&[]);
    let n = t.node();
    let db = rusqlite::Connection::open(t.home.join("node.sqlite3")).unwrap();
    db.execute_batch("CREATE TRIGGER fail_prepare BEFORE UPDATE ON ops WHEN NEW.id='op_prep' AND NEW.receipt LIKE '%finished%' BEGIN SELECT RAISE(ABORT,'prepare failure'); END").unwrap();
    n.prepare_workspace(now_cmd("op_prep", prepare_proj()))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    db.execute_batch("DROP TRIGGER fail_prepare").unwrap();
    assert!(ok(&finished(&n, "op_prep").await));
    // Faulted stays fail-closed for new work. Restart for the release half of the test.
    n.shutdown().await;
    drop(n);
    let n = t.node();
    n.open_session(now_cmd("open_after_restart", open_body("s_1")))
        .await
        .unwrap();
    assert!(ok(&finished(&n, "open_after_restart").await));
    db.execute_batch("CREATE TRIGGER fail_release BEFORE UPDATE ON ops WHEN NEW.id='rel' AND NEW.receipt LIKE '%finished%' BEGIN SELECT RAISE(ABORT,'release failure'); END").unwrap();
    n.release_session(now_cmd("rel", SessionId::new("s_1").unwrap()))
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    db.execute_batch("DROP TRIGGER fail_release").unwrap();
    assert!(ok(&finished(&n, "rel").await));
    assert_eq!(
        n.session(SessionId::new("s_1").unwrap())
            .await
            .unwrap()
            .phase,
        SessionPhase::Released
    );
    n.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn journal_budget_stops_running_work_and_reserves_terminal_space() {
    let t = T::new(&[]);
    let n = t.node();
    ready(&n, "s_1").await;
    n.start_turn(now_cmd("op_t1", turn("s_1", "t_1", "a_1", "@sleep 20000")))
        .await
        .unwrap();
    t.until("prompt before quota", |t| t.sent("session/prompt") == 1)
        .await;
    let db = rusqlite::Connection::open(t.home.join("node.sqlite3")).unwrap();
    // Storage fault fixture: retained history reaches the data budget while an attempt runs.
    // Embedded NULs deliberately distinguish UTF-8 bytes from SQLite length(TEXT).
    db.execute(
        "INSERT INTO command_specs VALUES ('quota_fixture', CAST(zeroblob(268435456) AS TEXT))",
        [],
    )
    .unwrap();
    let state = tokio::time::timeout(Duration::from_secs(5), finished(&n, "op_t1"))
        .await
        .unwrap();
    assert!(
        matches!(
            state,
            Observed::Finished {
                outcome: Outcome::Cancelled,
                ..
            }
        ),
        "{state:?}"
    );
    assert!(matches!(
        n.start_turn(now_cmd(
            "new_over_quota",
            turn("s_1", "t_2", "a_2", "hello")
        ))
        .await,
        Err(ApiError {
            code: ErrorCode::Exhausted,
            ..
        })
    ));
    n.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_process_registration_never_executes_adapter() {
    let t = T::new(&[]);
    let n = t.node();
    n.prepare_workspace(now_cmd("prep", prepare_proj()))
        .await
        .unwrap();
    assert!(ok(&finished(&n, "prep").await));
    let db = rusqlite::Connection::open(t.home.join("node.sqlite3")).unwrap();
    db.execute_batch("CREATE TRIGGER fail_proc BEFORE INSERT ON procs BEGIN SELECT RAISE(ABORT,'registration failure'); END").unwrap();
    n.open_session(now_cmd("open", open_body("s_1")))
        .await
        .unwrap();
    assert!(matches!(
        finished(&n, "open").await,
        Observed::Finished {
            outcome: Outcome::Failed,
            ..
        }
    ));
    assert!(
        !t.log.exists(),
        "adapter must not execute before process ownership is durable"
    );
    db.execute_batch("DROP TRIGGER fail_proc").unwrap();
    n.open_session(now_cmd("open_retry", open_body("s_1")))
        .await
        .unwrap();
    assert!(ok(&finished(&n, "open_retry").await));
    assert_eq!(t.sent("initialize"), 1);
    n.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn worktree_git_is_registered_before_effects_and_reaped_after() {
    let t = T::new(&[]);
    let repo = t.root.join("work/proj");
    for args in [
        vec!["init", "-b", "main"],
        vec![
            "-c",
            "user.name=Test",
            "-c",
            "user.email=test@example.invalid",
            "-c",
            "core.hooksPath=/dev/null",
            "commit",
            "--allow-empty",
            "-m",
            "initial",
        ],
    ] {
        let out = std::process::Command::new("git")
            .args(args)
            .current_dir(&repo)
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let n = t.node();
    let db = rusqlite::Connection::open(t.home.join("node.sqlite3")).unwrap();
    db.execute_batch("CREATE TRIGGER fail_git BEFORE INSERT ON procs BEGIN SELECT RAISE(ABORT,'git registration failure'); END").unwrap();
    let prepare = Prepare {
        resource: "worktree_resource".into(),
        kind: PrepareKind::Worktree {
            repository: RootPath {
                root: RootId::new("work").unwrap(),
                rel: RelPath::new("proj").unwrap(),
            },
            branch: "topic".into(),
            base: None,
        },
    };
    n.prepare_workspace(now_cmd("git_rejected", prepare.clone()))
        .await
        .unwrap();
    assert!(matches!(
        finished(&n, "git_rejected").await,
        Observed::Finished {
            outcome: Outcome::Failed,
            ..
        }
    ));
    assert!(
        !std::process::Command::new("git")
            .args(["rev-parse", "--verify", "topic"])
            .current_dir(&repo)
            .output()
            .unwrap()
            .status
            .success()
    );
    db.execute_batch("DROP TRIGGER fail_git").unwrap();
    n.prepare_workspace(now_cmd("git_retry", prepare))
        .await
        .unwrap();
    let state = finished(&n, "git_retry").await;
    assert!(ok(&state), "{state:?}");
    let Observed::Finished {
        result: Some(OperationResult::Workspace(w)),
        ..
    } = state
    else {
        panic!("workspace")
    };
    n.tidy_workspace(now_cmd(
        "git_tidy",
        Tidy {
            workspace: w.workspace,
            expected: w.revision,
            keep_branch: true,
        },
    ))
    .await
    .unwrap();
    assert!(ok(&finished(&n, "git_tidy").await));
    assert_eq!(
        db.query_row("SELECT COUNT(*) FROM procs", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        0
    );
    n.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn permission_receipt_retries_without_sending_allow_twice() {
    let t = T::new(&[]);
    let n = t.node();
    ready(&n, "s_1").await;
    n.start_turn(now_cmd(
        "op_t1",
        turn("s_1", "t_1", "a_1", "@permtool read absent"),
    ))
    .await
    .unwrap();
    t.until("permission", |t| t.request().is_some()).await;
    let req = t.request().unwrap();
    let db = rusqlite::Connection::open(t.home.join("node.sqlite3")).unwrap();
    db.execute_batch("CREATE TRIGGER fail_answer BEFORE UPDATE ON ops WHEN NEW.id='answer' AND NEW.receipt LIKE '%finished%' BEGIN SELECT RAISE(ABORT,'answer final commit failure'); END").unwrap();
    n.resolve_permission(now_cmd(
        "answer",
        answer(&req, PermissionDecision::AllowOnce),
    ))
    .await
    .unwrap();
    t.until("approval written", |t| {
        std::fs::read_to_string(&t.log)
            .unwrap_or_default()
            .contains("\"optionId\":\"once\"")
    })
    .await;
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        !n.operation(OperationId::new("answer").unwrap())
            .await
            .unwrap()
            .state
            .is_finished()
    );
    db.execute_batch("DROP TRIGGER fail_answer").unwrap();
    assert!(ok(&finished(&n, "answer").await));
    finished(&n, "op_t1").await;
    assert_eq!(
        std::fs::read_to_string(&t.log)
            .unwrap()
            .matches("\"optionId\":\"once\"")
            .count(),
        1
    );
    n.shutdown().await;
}

/// The host bridge tells a same-machine Hub where a bound workspace is; another Node's or an
/// unknown workspace has no directory here.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_bound_workspace_has_a_local_dir_for_the_host_only() {
    let t = T::new(&[]);
    let n = t.node();
    ready(&n, "s_1").await;
    let dir = n.local_dir(&ws()).unwrap();
    assert_eq!(dir.path, t.root.join("work/proj").canonicalize().unwrap());
    assert!(
        !dir.owned,
        "an existing directory is adopted, not the Node's own"
    );
    let other = WorkspaceRef {
        node: NodeId::new("elsewhere").unwrap(),
        ..ws()
    };
    assert!(n.local_dir(&other).is_none());
    let unknown = WorkspaceRef {
        id: WorkspaceId::new("w_unknown").unwrap(),
        ..ws()
    };
    assert!(n.local_dir(&unknown).is_none());
    std::fs::remove_dir_all(t.root.join("work/proj")).unwrap();
    assert!(
        n.local_dir(&ws()).is_none(),
        "a gone directory is not given"
    );
}

/// Opens the home again once the last Node has let go of it.
async fn reopen(t: &T, sink: Arc<RecordingSink>) -> LocalNode {
    let t0 = Instant::now();
    let n = loop {
        match LocalNode::open(t.config()) {
            Ok(x) => break x,
            Err(e) => {
                assert_eq!(e.code, ErrorCode::Conflict, "{e:?}");
                assert!(t0.elapsed() < Duration::from_secs(10), "home never freed");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
        }
    };
    n.connect(sink, None);
    n
}

async fn all_acknowledged(n: &LocalNode) {
    let t0 = Instant::now();
    loop {
        let heads = n.streams().await.unwrap();
        if !heads.is_empty() && heads.iter().all(|h| h.acknowledged == h.produced) {
            return;
        }
        assert!(t0.elapsed() < Duration::from_secs(15), "{heads:?}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A deleted journal (the node home made again) is a new journal: its streams get another epoch
/// and its events other ids, so the Hub that holds the old ones takes the new ones without a clash.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_recreated_journal_never_repeats_positions_or_ids() {
    let t = T::new(&[]);
    let n = t.node();
    ready(&n, "s_1").await;
    all_acknowledged(&n).await;
    let first = t.sink.events();
    n.shutdown().await;
    drop(n);
    for f in ["node.sqlite3", "node.sqlite3-wal", "node.sqlite3-shm"] {
        let _ = std::fs::remove_file(t.home.join(f));
    }

    let n = reopen(&t, t.sink.clone()).await;
    ready(&n, "s_1").await;
    // the same Hub (sink) holds both journals' events: it refuses a clash, so all must arrive
    all_acknowledged(&n).await;
    let second: Vec<NodeEvent> = t
        .sink
        .events()
        .into_iter()
        .filter(|e| !first.contains(e))
        .collect();
    assert!(!second.is_empty());
    let session = StreamId::new(derived_id("session", "s_1")).unwrap();
    let epoch_of = |evs: &[NodeEvent]| {
        let mut v: Vec<Epoch> = evs
            .iter()
            .filter(|e| e.cursor.stream == session)
            .map(|e| e.cursor.epoch.clone())
            .collect();
        v.dedup();
        v
    };
    let (old, new) = (epoch_of(&first), epoch_of(&second));
    assert_eq!(old.len(), 1, "{old:?}");
    assert_eq!(new.len(), 1, "{new:?}");
    assert_ne!(old, new, "the session stream starts again in a new epoch");
    assert!(new[0].as_str().starts_with('j'), "{new:?}");
    let old_ids: std::collections::HashSet<_> = first.iter().map(|e| e.id.clone()).collect();
    assert!(
        second.iter().all(|e| !old_ids.contains(&e.id)),
        "no event id is reused"
    );
    let old_positions: std::collections::HashSet<_> = first
        .iter()
        .map(|e| (e.cursor.stream.clone(), e.cursor.epoch.clone()))
        .collect();
    assert!(
        second
            .iter()
            .all(|e| !old_positions.contains(&(e.cursor.stream.clone(), e.cursor.epoch.clone()))),
        "no (stream, epoch) of the old journal is written again"
    );
    // nor a revision: the Hub matches views by revision, the same steps must not give the same one
    let revisions = |evs: &[NodeEvent]| -> std::collections::HashSet<String> {
        evs.iter()
            .filter_map(|e| match &e.body {
                EventBody::SessionChanged { view } => Some(view.revision.to_string()),
                EventBody::WorkspaceChanged { revision, .. } => Some(revision.to_string()),
                _ => None,
            })
            .collect()
    };
    let (old_revs, new_revs) = (revisions(&first), revisions(&second));
    assert!(!new_revs.is_empty());
    assert!(
        old_revs.is_disjoint(&new_revs),
        "no revision is handed out again: {old_revs:?} {new_revs:?}"
    );
    n.shutdown().await;
}

/// A journal from before journal identity (v2) keeps the epoch of the streams it has: what the
/// Hub acknowledged stays valid and numbering goes on where it was.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_v2_journal_keeps_its_e1_streams() {
    let t = T::new(&[]);
    let n = t.node();
    ready(&n, "s_1").await;
    all_acknowledged(&n).await;
    n.shutdown().await;
    drop(n);
    let session = derived_id("session", "s_1");
    let path = t.home.join("node.sqlite3");
    let produced: i64 = {
        // as a v2 Node left it: no journal id, every stream in epoch e1
        let db = rusqlite::Connection::open(&path).unwrap();
        db.execute_batch(
            "UPDATE streams SET epoch='e1'; DELETE FROM meta WHERE key='journal_id'; PRAGMA user_version=2;",
        )
        .unwrap();
        db.query_row(
            "SELECT produced FROM streams WHERE stream=?1",
            [&session],
            |r| r.get(0),
        )
        .unwrap()
    };
    assert!(produced > 0);

    let sink = Arc::new(RecordingSink::default());
    let n = reopen(&t, sink.clone()).await;
    n.open_session(now_cmd("op_open_again", open_body("s_1")))
        .await
        .unwrap();
    assert!(ok(&finished(&n, "op_open_again").await));
    let head = n
        .streams()
        .await
        .unwrap()
        .into_iter()
        .find(|h| h.stream.as_str() == session)
        .unwrap();
    assert_eq!(head.epoch.as_str(), "e1");
    assert!(head.produced as i64 > produced, "{head:?}");
    let t0 = Instant::now();
    while sink.events().is_empty() {
        assert!(t0.elapsed() < Duration::from_secs(15));
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let on_session: Vec<NodeEvent> = sink
        .events()
        .into_iter()
        .filter(|e| e.cursor.stream.as_str() == session)
        .collect();
    assert_eq!(on_session[0].cursor.epoch.as_str(), "e1");
    assert_eq!(
        on_session[0].cursor.seq as i64,
        produced + 1,
        "numbering goes on"
    );
    // an ack in e1 is taken: the old stream is still the stream the Node keeps
    n.acknowledge(EventAck {
        durable: vec![on_session[0].cursor.clone()],
    })
    .await
    .unwrap();
    n.shutdown().await;
    drop(n);
    let db = rusqlite::Connection::open(&path).unwrap();
    let version: i64 = db
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, 3);
    let ids: i64 = db
        .query_row(
            "SELECT COUNT(*) FROM meta WHERE key='journal_id'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(ids, 1, "the upgrade gave the journal its identity");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_journal_from_a_newer_node_is_refused() {
    let t = T::new(&[]);
    let n = t.node();
    n.shutdown().await;
    drop(n);
    let path = t.home.join("node.sqlite3");
    rusqlite::Connection::open(&path)
        .unwrap()
        .execute_batch("PRAGMA user_version=4;")
        .unwrap();
    let t0 = Instant::now();
    let e = loop {
        match LocalNode::open(t.config()) {
            Ok(_) => panic!("a newer journal was opened"),
            Err(e) if e.code == ErrorCode::Conflict => {
                assert!(t0.elapsed() < Duration::from_secs(10), "home never freed");
                tokio::time::sleep(Duration::from_millis(100)).await;
            }
            Err(e) => break e,
        }
    };
    assert_eq!(e.code, ErrorCode::Unsupported, "{e:?}");
    let version: i64 = rusqlite::Connection::open(&path)
        .unwrap()
        .pragma_query_value(None, "user_version", |r| r.get(0))
        .unwrap();
    assert_eq!(version, 4, "left as it was");
}

// ── disk quota (N1-4): a free-space high-water with hysteresis, compaction on the ack ──

const RESERVE: u64 = 512 << 20;

/// The journal file and its live pages, in bytes (as the Node counts them).
fn journal_sizes(db: &rusqlite::Connection) -> (u64, u64) {
    let p = |name: &str| -> u64 {
        db.pragma_query_value(None, name, |r| r.get::<_, i64>(0))
            .unwrap() as u64
    };
    let (page, count, free) = (p("page_size"), p("page_count"), p("freelist_count"));
    (count * page, (count - free) * page)
}

fn spec_ops(db: &rusqlite::Connection) -> Vec<String> {
    let mut st = db
        .prepare("SELECT op FROM command_specs ORDER BY op")
        .unwrap();
    st.query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn exhausted<T: std::fmt::Debug>(r: &ApiResult<T>) -> bool {
    matches!(r, Err(e) if e.code == ErrorCode::Exhausted)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_full_disk_stops_new_work_but_not_stopping_or_the_end() {
    let t = T::new(&[]);
    let free = Arc::new(AtomicU64::new(1 << 40));
    let n = t.node_with_free(&free);
    ready(&n, "s_1").await;
    n.start_turn(now_cmd("op_t1", turn("s_1", "t_1", "a_1", "@sleep 20000")))
        .await
        .unwrap();
    t.until("the prompt", |t| t.sent("session/prompt") == 1)
        .await;
    // the disk has less free space than the reserve: the limit is gone
    free.store(RESERVE / 2, Ordering::SeqCst);
    let r = n
        .start_turn(now_cmd("op_t2", turn("s_1", "t_2", "a_2", "hello")))
        .await;
    assert!(exhausted(&r), "{r:?}");
    let r = n
        .cancel(now_cmd(
            "op_c",
            Cancel {
                target: OperationId::new("op_t1").unwrap(),
                reason: "disk full".into(),
            },
        ))
        .await;
    assert!(!exhausted(&r), "a cancel is still taken: {r:?}");
    // the running attempt ends (cancelled), and its end is written and delivered
    t.until("the end", |t| !t.ended().is_empty()).await;
    assert_eq!(t.ended()[0].outcome, Outcome::Cancelled);
    finished(&n, "op_t1").await;
    n.release_session(now_cmd("op_rel", SessionId::new("s_1").unwrap()))
        .await
        .unwrap();
    assert!(ok(&finished(&n, "op_rel").await));
    // still full: nothing new, not even opening again
    let r = n.open_session(now_cmd("op_open2", open_body("s_1"))).await;
    assert!(exhausted(&r), "{r:?}");
    n.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_ack_compacts_the_journal_under_low_water_and_work_is_taken_again() {
    let t = T::new(&[]);
    let free = Arc::new(AtomicU64::new(1 << 40));
    // the Hub takes nothing for now: events and command bodies stay in the journal
    t.sink.set_failing(true);
    let n = t.node_with_free(&free);
    ready(&n, "s_1").await;
    let pad = "x".repeat(900_000);
    for i in 1..=4 {
        let op = format!("op_t{i}");
        let text = format!("@say {i}\n{pad}");
        n.start_turn(now_cmd(
            &op,
            turn("s_1", &format!("t_{i}"), &format!("a_{i}"), &text),
        ))
        .await
        .unwrap();
        assert!(ok(&finished(&n, &op).await));
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    let db = rusqlite::Connection::open(t.home.join("node.sqlite3")).unwrap();
    let (file, used) = journal_sizes(&db);
    assert!(used > 3_600_000, "{used}");
    // high-water: the filesystem's room past the reserve is just what the journal uses
    free.store(RESERVE - (file - used), Ordering::SeqCst);
    let r = n
        .start_turn(now_cmd("op_t5", turn("s_1", "t_5", "a_5", "hello")))
        .await;
    assert!(exhausted(&r), "{r:?}");
    let r = n
        .cancel(now_cmd(
            "op_c",
            Cancel {
                target: OperationId::new("op_t4").unwrap(),
                reason: "x".into(),
            },
        ))
        .await;
    assert!(!exhausted(&r), "control keeps its margin: {r:?}");
    // a little room again, but usage is still above 80% of the limit: still refused
    free.store(RESERVE - (file - used) + used / 8, Ordering::SeqCst);
    let r = n
        .start_turn(now_cmd("op_t5", turn("s_1", "t_5", "a_5", "hello")))
        .await;
    assert!(exhausted(&r), "hysteresis: {r:?}");
    // the Hub takes the events: the ack drops them and the finished turns' bodies
    t.sink.set_failing(false);
    all_acknowledged(&n).await;
    let specs = spec_ops(&db);
    // the cancel finished at accept with no stream of its own: its body goes too
    assert!(
        specs.is_empty(),
        "finished and acknowledged bodies are compacted: {specs:?}"
    );
    // the limit is used * 9/8 now, so low water (80%) is 0.9 * used
    let (_, after) = journal_sizes(&db);
    assert!(after * 10 < used * 9, "{after} of {used}");
    n.start_turn(now_cmd("op_t5", turn("s_1", "t_5", "a_5", "@say again")))
        .await
        .unwrap();
    assert!(ok(&finished(&n, "op_t5").await));
    n.shutdown().await;
}

/// Counts every position the Hub took (to see each delivered once).
#[derive(Default)]
struct Counting {
    hub: RecordingSink,
    taken: Mutex<Vec<(String, String, u64)>>,
}

#[async_trait::async_trait]
impl EventSink for Counting {
    async fn commit(&self, batch: EventBatch) -> ApiResult<EventAck> {
        let keys: Vec<_> = batch
            .events
            .iter()
            .map(|e| {
                (
                    e.cursor.stream.as_str().to_string(),
                    e.cursor.epoch.as_str().to_string(),
                    e.cursor.seq,
                )
            })
            .collect();
        let ack = self.hub.commit(batch).await?;
        self.taken.lock().unwrap().extend(keys);
        Ok(ack)
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sqlite_full_mid_turn_keeps_numbered_events_and_skips_no_seq() {
    let t = T::new(&[]);
    t.sink.set_failing(true);
    let n = t.node();
    ready(&n, "s_1").await;
    let mut tu = turn("s_1", "t_1", "a_1", "@flood");
    tu.run.limits.max_output_bytes = 64 << 20;
    n.start_turn(now_cmd("op_t1", tu)).await.unwrap();
    let db = rusqlite::Connection::open(t.home.join("node.sqlite3")).unwrap();
    let count = || -> i64 {
        db.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
            .unwrap()
    };
    t.until("output journaled", |_| count() > 100).await;
    // the disk is full: the next page the journal needs is refused (SQLITE_FULL)
    n.cap_journal_pages(0).unwrap();
    tokio::time::sleep(Duration::from_millis(500)).await;
    n.shutdown().await;
    drop(n);
    let mut st = db.prepare("SELECT event FROM events").unwrap();
    let before: Vec<NodeEvent> = st
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .map(|s| serde_json::from_str(&s.unwrap()).unwrap())
        .collect();
    drop(st);
    let deltas = before
        .iter()
        .filter(|e| matches!(e.body, EventBody::TextDelta { .. }))
        .count();
    assert!(
        (50..2000).contains(&deltas),
        "the flood was cut short by the full journal: {deltas}"
    );

    // started again (the disk has room): what was numbered is delivered, each position once
    let hub = Arc::new(Counting::default());
    let n = loop {
        match LocalNode::open(t.config()) {
            Ok(n) => break n,
            Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    };
    n.connect(hub.clone(), None);
    all_acknowledged(&n).await;
    let taken = hub.taken.lock().unwrap().clone();
    let mut seen = std::collections::HashSet::new();
    for k in &taken {
        assert!(seen.insert(k.clone()), "delivered twice: {k:?}");
    }
    let held = hub.hub.events();
    for e in &before {
        assert!(held.contains(e), "lost: {:?}", e.cursor);
    }
    for h in n.streams().await.unwrap() {
        let mut seqs: Vec<u64> = taken
            .iter()
            .filter(|(s, e, _)| s == h.stream.as_str() && e == h.epoch.as_str())
            .map(|k| k.2)
            .collect();
        seqs.sort();
        assert_eq!(seqs, (1..=h.produced).collect::<Vec<_>>(), "{}", h.stream);
    }
    n.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acknowledged_bodies_go_but_an_unknown_prepare_keeps_its_own_for_recover() {
    let t = T::new(&[]);
    let n = t.node();
    let db = rusqlite::Connection::open(t.home.join("node.sqlite3")).unwrap();
    n.prepare_workspace(now_cmd("op_prep", prepare_proj()))
        .await
        .unwrap();
    assert!(ok(&finished(&n, "op_prep").await));
    db.execute_batch("CREATE TRIGGER fail_prepare BEFORE UPDATE ON ops WHEN NEW.id='prep_crash' AND NEW.receipt LIKE '%finished%' BEGIN SELECT RAISE(ABORT,'crash before final receipt'); END").unwrap();
    let c = now_cmd(
        "prep_crash",
        Prepare {
            resource: "recover_scratch".into(),
            kind: PrepareKind::Scratch,
        },
    );
    n.prepare_workspace(c.clone()).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    n.shutdown().await;
    drop(n);
    db.execute_batch("DROP TRIGGER fail_prepare").unwrap();
    let n = reopen(&t, t.sink.clone()).await;
    all_acknowledged(&n).await;
    assert_eq!(spec_ops(&db), ["prep_crash"]);
    n.recover(now_cmd(
        "recover_prep",
        Recover {
            target: c.id.clone(),
            expected: c.digest("prepare_workspace").unwrap(),
            reason: "retire interrupted prepare".into(),
        },
    ))
    .await
    .unwrap();
    assert!(ok(&finished(&n, "recover_prep").await));
    all_acknowledged(&n).await;
    assert!(spec_ops(&db).is_empty(), "{:?}", spec_ops(&db));
    n.shutdown().await;
}
