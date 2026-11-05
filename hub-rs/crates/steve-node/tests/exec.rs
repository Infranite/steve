//! LocalNode's commands ([`WorkspaceExecutor::exec`]) with real processes: exit codes and output,
//! the deadline that takes what the command started with it, output that never blocks the
//! command, and a Node that dies while one runs (unknown, never run again).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use steve_acp::AcpConfig;
use steve_node::{LocalNode, NodeConfig, Root, Storage};
use steve_node_api::testkit::RecordingSink;
use steve_node_api::testkit::build::{cmd, limits, policy};
use steve_node_api::*;

struct T {
    _dir: Option<tempfile::TempDir>,
    root: PathBuf,
}

impl T {
    fn new() -> T {
        let dir = tempfile::tempdir().unwrap();
        let mut t = T::at(dir.path());
        t._dir = Some(dir);
        t
    }

    fn at(root: &Path) -> T {
        std::fs::create_dir_all(root.join("work/proj/sub")).unwrap();
        T {
            root: root.to_path_buf(),
            _dir: None,
        }
    }

    fn config(&self, execs: u32) -> NodeConfig {
        let root = self.root.clone();
        NodeConfig {
            node: NodeId::new("hub").unwrap(),
            home: root.join("node"),
            acp: AcpConfig {
                adapters: HashMap::new(),
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
                sessions: 0,
                jobs: 0,
                execs,
            },
            storage: Storage::default(),
        }
    }

    fn node(&self) -> LocalNode {
        let n = LocalNode::open(self.config(4)).unwrap();
        n.connect(Arc::new(RecordingSink::default()), None);
        n
    }
}

fn wall() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn now_cmd<B>(op: &str, body: B) -> Command<B> {
    let mut c = cmd(op, body);
    c.issued_at = Timestamp(wall());
    c.accept_before = Timestamp(wall() + 60_000);
    c
}

async fn finished(n: &LocalNode, op: &str) -> Observed {
    let t0 = Instant::now();
    loop {
        let r = n.operation(OperationId::new(op).unwrap()).await.unwrap();
        if r.state.is_finished() {
            return r.state;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(20),
            "{op}: {:?}",
            r.state
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

const RESOURCE: &str = "project:p1";

/// The project directory prepared as a workspace.
async fn workspace(n: &LocalNode) -> WorkspaceRef {
    n.prepare_workspace(now_cmd(
        "op_prep",
        Prepare {
            resource: RESOURCE.into(),
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
    match finished(n, "op_prep").await {
        Observed::Finished {
            result: Some(OperationResult::Workspace(v)),
            ..
        } => v.workspace,
        other => panic!("prepare: {other:?}"),
    }
}

fn spec(w: &WorkspaceRef, command: &str, within: Duration) -> ExecSpec {
    ExecSpec {
        workspace: w.clone(),
        command: command.into(),
        cwd: None,
        policy: policy(),
        limits: Limits {
            deadline: Timestamp(wall() + within.as_millis() as i64),
            max_output_bytes: 64 * 1024,
            ..limits()
        },
        env: vec![],
        secrets: vec![],
    }
}

/// Runs `s` as `op` and waits for its end: the outcome and the result.
async fn run(n: &LocalNode, op: &str, s: ExecSpec) -> (Outcome, ExecResult) {
    n.exec(now_cmd(op, s)).await.unwrap();
    match finished(n, op).await {
        Observed::Finished {
            outcome,
            result: Some(OperationResult::Exec(r)),
            ..
        } => (outcome, r),
        other => panic!("{op}: {other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn exit_code_and_output() {
    let t = T::new();
    let n = t.node();
    let w = workspace(&n).await;
    let (o, r) = run(
        &n,
        "op_x1",
        spec(
            &w,
            "echo out; echo err >&2; pwd; exit 3",
            Duration::from_secs(10),
        ),
    )
    .await;
    assert_eq!(o, Outcome::Succeeded, "it ran to its end");
    assert_eq!(r.exit_code, Some(3));
    let proj = t.root.join("work/proj").canonicalize().unwrap();
    assert_eq!(r.stdout, format!("out\n{}\n", proj.display()));
    assert_eq!(r.stderr, "err\n");
    assert!(!r.truncated);

    // plain env goes in, the Hub's own control variables never do, stdin is empty
    let mut s = spec(
        &w,
        "echo \"$FOO ${STEVE_UPGRADE_ID-none}\"; cat; pwd",
        Duration::from_secs(10),
    );
    s.env = vec![
        ("FOO".into(), "bar".into()),
        ("STEVE_UPGRADE_ID".into(), "u1".into()),
    ];
    s.cwd = Some(RelPath::new("sub").unwrap());
    let (_, r) = run(&n, "op_x2", s).await;
    assert_eq!(
        r.stdout,
        format!("bar none\n{}\n", proj.join("sub").display())
    );
    // the same command again is the same operation: its answer, not a second run
    let count = t.root.join("count");
    let c = now_cmd(
        "op_x3",
        spec(
            &w,
            &format!("echo once >> '{}'", count.display()),
            Duration::from_secs(10),
        ),
    );
    n.exec(c.clone()).await.unwrap();
    finished(&n, "op_x3").await;
    assert!(n.exec(c).await.unwrap().state.is_finished());
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(std::fs::read_to_string(&count).unwrap(), "once\n");

    // a directory that leads out of the workspace is refused
    std::os::unix::fs::symlink("/", t.root.join("work/proj/sub/out")).unwrap();
    let mut s = spec(&w, "true", Duration::from_secs(10));
    s.cwd = Some(RelPath::new("sub/out").unwrap());
    assert_eq!(
        n.exec(now_cmd("op_x4", s)).await.unwrap_err().code,
        ErrorCode::Denied
    );
    n.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_deadline_takes_what_the_command_started() {
    let t = T::new();
    let n = t.node();
    let w = workspace(&n).await;
    let mark = t.root.join("marks");
    std::fs::create_dir_all(&mark).unwrap();
    let (a, b, c) = (mark.join("a"), mark.join("b"), mark.join("c"));
    // a wrapper that leaves a child holding its output open, and one that hangs itself with a
    // grandchild of its own
    let t0 = Instant::now();
    let (o, r) = run(
        &n,
        "op_d1",
        spec(
            &w,
            &format!("(sleep 2; touch '{}') & echo started", a.display()),
            Duration::from_millis(800),
        ),
    )
    .await;
    assert_eq!((o, r.exit_code), (Outcome::Succeeded, Some(0)));
    assert_eq!(r.stdout, "started\n");
    let (o, r) = run(
        &n,
        "op_d2",
        spec(
            &w,
            &format!(
                "(sleep 2; touch '{}') & sleep 2; touch '{}'",
                b.display(),
                c.display()
            ),
            Duration::from_millis(500),
        ),
    )
    .await;
    assert_eq!((o, r.exit_code), (Outcome::TimedOut, None));
    assert!(t0.elapsed() < Duration::from_secs(2), "{:?}", t0.elapsed());
    tokio::time::sleep(Duration::from_secs(3)).await;
    for f in [&a, &b, &c] {
        assert!(!f.exists(), "{} was killed with its command", f.display());
    }
    n.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_lot_of_stderr_does_not_block() {
    let t = T::new();
    let n = t.node();
    let w = workspace(&n).await;
    let (o, r) = run(
        &n,
        "op_s1",
        spec(
            &w,
            "head -c 300000 /dev/zero | tr '\\0' x >&2; echo done",
            Duration::from_secs(10),
        ),
    )
    .await;
    assert_eq!((o, r.exit_code), (Outcome::Succeeded, Some(0)));
    assert_eq!(r.stdout, "done\n");
    assert_eq!(r.stderr.len(), 64 * 1024, "kept up to the bound");
    assert!(r.stderr.bytes().all(|b| b == b'x'));
    assert!(r.truncated);
    n.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn what_the_node_cannot_do_is_refused_before_it_is_taken() {
    // no command capacity: commands are not this Node's
    let none = T::new();
    let n = LocalNode::open(none.config(0)).unwrap();
    n.connect(Arc::new(RecordingSink::default()), None);
    let w = workspace(&n).await;
    let e = n
        .exec(now_cmd("op_r1", spec(&w, "true", Duration::from_secs(5))))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Unsupported);
    n.shutdown().await;
    let t = T::new();
    let n = t.node();
    // another Node's workspace, a past deadline, a workspace never prepared
    let mut other = w.clone();
    other.node = NodeId::new("n2").unwrap();
    let e = n
        .exec(now_cmd(
            "op_r2",
            spec(&other, "true", Duration::from_secs(5)),
        ))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Denied);
    let mut s = spec(&w, "true", Duration::from_secs(5));
    s.limits.deadline = Timestamp(wall() - 1);
    assert_eq!(
        n.exec(now_cmd("op_r3", s)).await.unwrap_err().code,
        ErrorCode::Expired
    );
    let mut ghost = w.clone();
    ghost.id = WorkspaceId::new(derived_id("w", "project:none")).unwrap();
    assert_eq!(
        n.exec(now_cmd(
            "op_r4",
            spec(&ghost, "true", Duration::from_secs(5))
        ))
        .await
        .unwrap_err()
        .code,
        ErrorCode::Invalid
    );
    // a running command keeps its workspace: no tidy meanwhile
    let w = workspace(&n).await;
    n.exec(now_cmd(
        "op_r5",
        spec(&w, "sleep 2", Duration::from_secs(10)),
    ))
    .await
    .unwrap();
    let view = n.workspace(w.clone()).await.unwrap();
    let e = n
        .tidy_workspace(now_cmd(
            "op_tidy",
            Tidy {
                workspace: w.clone(),
                expected: view.revision,
                keep_branch: true,
            },
        ))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Busy);
    finished(&n, "op_r5").await;
    n.shutdown().await;
}

// ── a real crash: the Node is a child process killed with SIGKILL ──

/// Runs in a child process (see `a_node_that_dies_while_a_command_runs_never_runs_it_again`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn child_exec_process() {
    let Ok(root) = std::env::var("STEVE_EXEC_CHILD_ROOT") else {
        return;
    };
    let t = T::at(Path::new(&root));
    let n = t.node();
    let w = workspace(&n).await;
    let runs = Path::new(&root).join("runs");
    n.exec(now_cmd(
        "op_crash",
        spec(
            &w,
            &format!("echo ran >> '{}'; sleep 60", runs.display()),
            Duration::from_secs(120),
        ),
    ))
    .await
    .unwrap();
    let t0 = Instant::now();
    while !runs.exists() {
        assert!(t0.elapsed() < Duration::from_secs(10));
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    std::fs::write(Path::new(&root).join("ready"), "").unwrap();
    tokio::time::sleep(Duration::from_secs(120)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_node_that_dies_while_a_command_runs_never_runs_it_again() {
    let t = T::new();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "child_exec_process", "--nocapture"])
        .env("STEVE_EXEC_CHILD_ROOT", &t.root)
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
    let runs = || std::fs::read_to_string(t.root.join("runs")).unwrap();
    assert_eq!(runs(), "ran\n");

    // the command's group outlived the Node: after the restart it is unknown, and stays so
    let n = t.node();
    let op = OperationId::new("op_crash").unwrap();
    let r = n.operation(op.clone()).await.unwrap();
    assert_eq!(r.state.tag(), StateTag::Unknown, "{:?}", r.state);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        n.operation(op.clone()).await.unwrap().state.tag(),
        StateTag::Unknown
    );
    assert_eq!(runs(), "ran\n", "never run a second time");
    // asking again with the same operation gives what the Node knows, nothing new runs
    let w = WorkspaceRef {
        node: NodeId::new("hub").unwrap(),
        id: WorkspaceId::new(derived_id("w", RESOURCE)).unwrap(),
    };
    let r = n
        .exec(now_cmd(
            "op_crash",
            spec(&w, "echo again", Duration::from_secs(5)),
        ))
        .await;
    assert!(r.is_err(), "other content under the same id is a conflict");

    // recover stops what is left; it ends interrupted, and the workspace is free again
    let stored = rusqlite::Connection::open(t.root.join("node/node.sqlite3"))
        .unwrap()
        .query_row("SELECT digest FROM ops WHERE id='op_crash'", [], |r| {
            r.get::<_, String>(0)
        })
        .unwrap();
    n.recover(now_cmd(
        "op_recover",
        Recover {
            target: op.clone(),
            expected: Digest::new(stored).unwrap(),
            reason: "the node died".into(),
        },
    ))
    .await
    .unwrap();
    let s = finished(&n, "op_recover").await;
    assert!(
        matches!(
            s,
            Observed::Finished {
                outcome: Outcome::Succeeded,
                ..
            }
        ),
        "{s:?}"
    );
    assert!(matches!(
        n.operation(op).await.unwrap().state,
        Observed::Finished {
            outcome: Outcome::Interrupted,
            ..
        }
    ));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(runs(), "ran\n");
    let (o, _) = run(&n, "op_after", spec(&w, "true", Duration::from_secs(5))).await;
    assert_eq!(o, Outcome::Succeeded);
    n.shutdown().await;
}
