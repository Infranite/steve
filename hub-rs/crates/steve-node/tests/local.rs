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
            ceiling: steve_node::PolicyCeiling::unlimited(),
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
            jobs: Default::default(),
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
        env: Default::default(),
        state: Default::default(),
        artifact_view: None,
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

async fn disk_observations_done(
    n: &LocalNode,
    expected: usize,
) -> steve_node::disk_observation::Page {
    let until = Instant::now() + Duration::from_secs(15);
    loop {
        let page = n.disk_observations(0, 100).unwrap();
        if page.records.len() == expected && page.records.iter().all(|r| r.state.terminal()) {
            return page;
        }
        assert!(
            Instant::now() < until,
            "disk observations did not finish: {page:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disk_statistics_abort_does_not_abort_turn_acceptance_or_completion() {
    use steve_node::disk_observation::Policy;
    for phase in ["started", "finished"] {
        let t = T::new(&[]);
        let n = t.node();
        ready(&n, "disk").await;
        n.configure_disk_observations(Some(Policy::default()))
            .unwrap();
        let db = rusqlite::Connection::open(t.home.join("node.sqlite3")).unwrap();
        db.execute_batch(&format!(
            "CREATE TRIGGER fail_statistics BEFORE INSERT ON disk_observations
             WHEN NEW.phase='{phase}' BEGIN SELECT RAISE(ABORT,'statistics only'); END;"
        ))
        .unwrap();
        n.start_turn(now_cmd(
            "disk_turn",
            turn("disk", "t_disk", "a_disk", "@say business committed"),
        ))
        .await
        .unwrap();
        assert!(ok(&finished(&n, "disk_turn").await), "{phase}");
        t.until("the business end despite statistics failure", |t| {
            t.ended().len() == 1
        })
        .await;
        assert_eq!(t.text(), "business committed", "{phase}");
        let rejected_rows: i64 = db
            .query_row(
                "SELECT count(*) FROM disk_observations WHERE phase=?",
                [phase],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(rejected_rows, 0, "{phase}");
        // A completed actor releases its gate even when the optional end record failed.
        n.start_turn(now_cmd(
            "disk_next",
            turn("disk", "t_next", "a_next", "@say continued"),
        ))
        .await
        .unwrap();
        assert!(ok(&finished(&n, "disk_next").await), "{phase}");
        n.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disk_statistics_rollback_does_not_hide_a_lost_business_transaction() {
    use steve_node::disk_observation::Policy;
    let t = T::new(&[]);
    let n = t.node();
    ready(&n, "disk").await;
    n.configure_disk_observations(Some(Policy::default()))
        .unwrap();
    let db = rusqlite::Connection::open(t.home.join("node.sqlite3")).unwrap();
    let view_before: String = db
        .query_row("SELECT view FROM sessions WHERE id='disk'", [], |r| {
            r.get(0)
        })
        .unwrap();
    db.execute_batch(
        "CREATE TRIGGER rollback_statistics BEFORE INSERT ON disk_observations
         BEGIN SELECT RAISE(ROLLBACK,'the whole journal transaction was lost'); END;",
    )
    .unwrap();
    let command = now_cmd(
        "disk_turn",
        turn("disk", "t_disk", "a_disk", "@say only after repair"),
    );
    let error = n.start_turn(command.clone()).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::Internal);
    let accepted: i64 = db
        .query_row("SELECT count(*) FROM ops WHERE id='disk_turn'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(accepted, 0);
    let view_after: String = db
        .query_row("SELECT view FROM sessions WHERE id='disk'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(view_after, view_before);
    assert_eq!(t.sent("session/prompt"), 0);
    db.execute_batch("DROP TRIGGER rollback_statistics")
        .unwrap();
    n.start_turn(command).await.unwrap();
    assert!(ok(&finished(&n, "disk_turn").await));
    n.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disk_records_follow_real_turns_and_survive_resend_compaction_and_restart() {
    use steve_node::disk_observation::{Interpretation, Phase, Policy, State};
    use steve_node::disk_usage::{Budget, Outcome};
    let t = T::new(&[]);
    let n = t.node();
    n.configure_disk_observations(Some(Policy {
        budget: Budget {
            elapsed: Duration::from_secs(2),
            ..Policy::default().budget
        },
        ..Policy::default()
    }))
    .unwrap();
    ready(&n, "disk").await;
    std::fs::write(t.root.join("work/proj/data"), vec![b'x'; 8192]).unwrap();
    let command = now_cmd(
        "disk_turn",
        turn("disk", "t_disk", "a_disk", "@say finished"),
    );
    n.start_turn(command.clone()).await.unwrap();
    assert!(ok(&finished(&n, "disk_turn").await));
    n.start_turn(command).await.unwrap();
    let page = disk_observations_done(&n, 2).await;
    assert_eq!(
        page.records.iter().map(|r| r.phase).collect::<Vec<_>>(),
        vec![Phase::Started, Phase::Finished]
    );
    assert!(
        page.records
            .iter()
            .all(|r| r.interpretation == Interpretation::AsynchronousUnattributedPathStock)
    );
    let record = &page.records[1];
    assert_eq!(record.resource, "item:x");
    assert_eq!(record.execution.attempt().as_str(), "a_disk");
    let State::Observed(sample) = &record.state else {
        panic!("{record:?}")
    };
    let Outcome::Complete(usage) = sample.outcome else {
        panic!("{sample:?}")
    };
    assert!(usage.apparent_bytes >= 8192);
    assert_eq!(sample.missing_roots, 0);
    // Acknowledging and compacting command bodies cannot erase statistics.
    all_acknowledged(&n).await;
    assert_eq!(n.disk_observations(0, 100).unwrap(), page);
    n.shutdown().await;
    drop(n);
    let n = LocalNode::open(t.config()).unwrap();
    assert_eq!(n.disk_observations(0, 100).unwrap(), page);
    n.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn disk_record_limits_preserve_partial_results_and_disabling_does_not_change_turns() {
    use steve_node::disk_observation::{Phase, Policy, State};
    use steve_node::disk_usage::{Incomplete, Outcome};
    let t = T::new(&[]);
    let n = t.node();
    ready(&n, "disk").await;
    std::fs::write(t.root.join("work/proj/data"), b"some data").unwrap();
    n.configure_disk_observations(Some(Policy {
        budget: steve_node::disk_usage::Budget {
            entries: 1,
            elapsed: Duration::from_secs(2),
            ..Policy::default().budget
        },
        ..Policy::default()
    }))
    .unwrap();
    n.start_turn(now_cmd(
        "disk_partial",
        turn("disk", "t1", "a1", "@say done"),
    ))
    .await
    .unwrap();
    assert!(ok(&finished(&n, "disk_partial").await));
    let page = disk_observations_done(&n, 2).await;
    let sample = &page
        .records
        .iter()
        .find(|r| r.phase == Phase::Finished)
        .unwrap()
        .state;
    assert!(
        matches!(sample, State::Observed(sample) if matches!(sample.outcome, Outcome::Incomplete { reason: Incomplete::EntryBudget, .. }))
    );
    n.configure_disk_observations(None).unwrap();
    n.start_turn(now_cmd(
        "disk_off",
        turn("disk", "t2", "a2", "@say still runs"),
    ))
    .await
    .unwrap();
    assert!(ok(&finished(&n, "disk_off").await));
    assert_eq!(n.disk_observations(0, 100).unwrap(), page);
    n.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn artifact_reads_bind_the_execution_view_and_survive_compaction_and_restart() {
    let t = T::new(&[]);
    let n = t.node();
    n.prepare_workspace(now_cmd("op_prep", prepare_proj()))
        .await
        .unwrap();
    assert!(ok(&finished(&n, "op_prep").await));
    let digest = Digest::sha256(b"principal u1, workspace item:x");
    let mut open = open_body("s_art");
    open.artifact_view = Some(digest.clone());
    n.open_session(now_cmd("op_open_art", open.clone()))
        .await
        .unwrap();
    assert!(ok(&finished(&n, "op_open_art").await));
    std::fs::write(t.root.join("work/proj/result.txt"), b"the result").unwrap();
    let command = now_cmd("op_art", turn("s_art", "t_art", "a_art", "@say done"));
    n.start_turn(command.clone()).await.unwrap();
    assert!(ok(&finished(&n, "op_art").await));
    // A resend does not capture another grant or change the view.
    n.start_turn(command).await.unwrap();
    let query = ArtifactRead {
        execution: ExecutionRef::Turn {
            session: SessionId::new("s_art").unwrap(),
            turn: TurnId::new("t_art").unwrap(),
            attempt: AttemptId::new("a_art").unwrap(),
        },
        view_digest: digest,
        location: RelPath::new("result.txt").unwrap(),
        expected: None,
        offset: 4,
        max: 6,
    };
    assert_eq!(
        n.read_artifact(query.clone()).await.unwrap().bytes,
        b"result"
    );
    let mut wrong = query.clone();
    wrong.view_digest = Digest::sha256(b"another principal");
    assert_eq!(
        n.read_artifact(wrong).await.unwrap_err().code,
        ErrorCode::Denied
    );
    let mut wrong = query.clone();
    let ExecutionRef::Turn { ref mut turn, .. } = wrong.execution else {
        unreachable!()
    };
    *turn = TurnId::new("another-turn").unwrap();
    assert_eq!(
        n.read_artifact(wrong).await.unwrap_err().code,
        ErrorCode::Denied
    );
    let mut legacy = query.clone();
    legacy.execution = ExecutionRef::Job {
        job: JobId::new("unknown").unwrap(),
        attempt: AttemptId::new("unknown").unwrap(),
    };
    assert_eq!(
        n.read_artifact(legacy).await.unwrap_err().code,
        ErrorCode::Unsupported
    );
    // Reopening the session with a new view must not relabel an old execution's artifacts.
    n.release_session(now_cmd("op_rel_art", open.session.clone()))
        .await
        .unwrap();
    assert!(ok(&finished(&n, "op_rel_art").await));
    open.artifact_view = Some(Digest::sha256(b"new view"));
    n.open_session(now_cmd("op_reopen_art", open))
        .await
        .unwrap();
    assert!(ok(&finished(&n, "op_reopen_art").await));
    assert_eq!(
        n.read_artifact(query.clone()).await.unwrap().bytes,
        b"result"
    );
    let heads = n.streams().await.unwrap();
    n.acknowledge(EventAck {
        durable: heads
            .into_iter()
            .map(|h| Watermark {
                stream: h.stream,
                epoch: h.epoch,
                seq: h.produced,
            })
            .collect(),
    })
    .await
    .unwrap();
    n.shutdown().await;
    drop(n);
    let n = t.node();
    assert_eq!(
        n.read_artifact(query.clone()).await.unwrap().bytes,
        b"result"
    );
    // Removing a configured root revokes even a durable grant.
    n.set_roots(vec![]);
    assert!(n.read_artifact(query.clone()).await.is_err());
    n.set_roots(t.config().roots);
    let current = n.workspace(ws()).await.unwrap();
    n.tidy_workspace(now_cmd(
        "op_tidy_art",
        Tidy {
            workspace: ws(),
            expected: current.revision,
            keep_branch: true,
        },
    ))
    .await
    .unwrap();
    assert!(ok(&finished(&n, "op_tidy_art").await));
    assert_eq!(
        n.read_artifact(query.clone()).await.unwrap_err().code,
        ErrorCode::Unsupported
    );
    // Reusing the resource/id cannot resurrect a predecessor's grant.
    n.prepare_workspace(now_cmd("op_reprep_art", prepare_proj()))
        .await
        .unwrap();
    assert!(ok(&finished(&n, "op_reprep_art").await));
    assert_eq!(
        n.read_artifact(query).await.unwrap_err().code,
        ErrorCode::Unsupported
    );
    n.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_legacy_execution_never_falls_back_to_a_general_file_read() {
    let t = T::new(&[]);
    let n = t.node();
    ready(&n, "s_legacy").await;
    n.start_turn(now_cmd(
        "op_legacy",
        turn("s_legacy", "t_l", "a_l", "@say done"),
    ))
    .await
    .unwrap();
    assert!(ok(&finished(&n, "op_legacy").await));
    let query = ArtifactRead {
        execution: ExecutionRef::Turn {
            session: SessionId::new("s_legacy").unwrap(),
            turn: TurnId::new("t_l").unwrap(),
            attempt: AttemptId::new("a_l").unwrap(),
        },
        view_digest: Digest::sha256(b"guessed"),
        location: RelPath::new("result.txt").unwrap(),
        expected: None,
        offset: 0,
        max: 100,
    };
    assert_eq!(
        n.read_artifact(query).await.unwrap_err().code,
        ErrorCode::Unsupported
    );
    n.shutdown().await;
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
    assert_eq!(
        list["result"]["tools"][0]["annotations"]["readOnlyHint"],
        false
    );
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

/// The tool server the adapter was given at session/new: its URL and token.
fn tool_server(t: &T) -> (String, String) {
    let log = std::fs::read_to_string(&t.log).unwrap();
    let new = log
        .lines()
        .find(|l| l.contains("\"method\":\"session/new\""))
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(new).unwrap();
    let server = &v["params"]["mcpServers"][0];
    let token = server["headers"][0]["value"]
        .as_str()
        .unwrap()
        .strip_prefix("Bearer ")
        .unwrap()
        .to_string();
    (server["url"].as_str().unwrap().to_string(), token)
}

/// A harness lists Steve's tools as soon as it connects to the server, which may be before the
/// first turn (Claude Code does, when its connection wins the race with the first prompt; it then
/// keeps the empty list for good). The list waits for the attempt instead of failing, and once the
/// Node knows the catalog it is listed at idle at once. Calls still need the running attempt.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn steves_tools_are_listed_before_the_first_turn_and_at_idle() {
    let t = T::new(&[]);
    let n = t.node();
    let tools = Arc::new(Tools::default());
    n.connect_tools(tools.clone()).await.unwrap();
    ready(&n, "s_1").await;
    let (url, token) = tool_server(&t);
    let list = serde_json::json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"});
    let early = tokio::spawn({
        let (url, token, list) = (url.clone(), token.clone(), list.clone());
        async move { mcp(&url, &token, list).await }
    });
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;
    n.start_turn(now_cmd("op_t", turn("s_1", "t_1", "a_1", "@sleep 3000")))
        .await
        .unwrap();
    let (s, early) = early.await.unwrap();
    assert_eq!(s, 200, "{early}");
    assert_eq!(
        early["result"]["tools"][0]["name"], "steve_update",
        "listed before the first turn: {early}"
    );
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
    let (s, idle) =
        tokio::time::timeout(std::time::Duration::from_secs(2), mcp(&url, &token, list))
            .await
            .expect("answered at once at idle");
    assert_eq!(s, 200, "{idle}");
    assert_eq!(idle["result"]["tools"][0]["name"], "steve_update", "{idle}");
    let (_, call) = mcp(
        &url,
        &token,
        serde_json::json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": {"name": "steve_update", "arguments": {"now": "x"}}}),
    )
    .await;
    assert!(call.get("error").is_some(), "no call at idle: {call}");
    assert!(tools.calls.lock().unwrap().is_empty());
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
    let again = open_again(&t).await;
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
    let again = open_again(&t).await;
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
    let again = open_again(&t).await;
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
    let n = reopen(&t, t.sink.clone()).await;
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
    let n = reopen(&t, t.sink.clone()).await;
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
    let n = reopen(&t, t.sink.clone()).await;
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
            at: None,
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
    let n = open_again(t).await;
    n.connect(sink, None);
    n
}

/// Opens `t`'s home again after a shutdown. The home lock goes with the Node's last internal
/// handle: an actor or the delivery loop that already answered `shutdown` still lets go of it a
/// moment later, so a test that reopens right away waits for that instead of racing it.
async fn open_again(t: &T) -> LocalNode {
    let t0 = Instant::now();
    loop {
        match LocalNode::open(t.config()) {
            Ok(x) => return x,
            Err(e) => {
                assert_eq!(e.code, ErrorCode::Conflict, "{e:?}");
                assert!(t0.elapsed() < Duration::from_secs(30), "home never freed");
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        }
    }
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
                assert!(t0.elapsed() < Duration::from_secs(30), "home never freed");
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
    // gated: the adapter sends a chunk only once fewer than `AHEAD` are not yet journaled, so how
    // far the flood got when the disk fills does not depend on how fast the machine journals.
    const AHEAD: i64 = 24;
    let gate_dir = tempfile::tempdir().unwrap();
    let gate = gate_dir.path().join("flood-gate");
    std::fs::write(&gate, AHEAD.to_string()).unwrap();
    let t = T::new(&[("FAKE_ACP_FLOOD_GATE", gate.to_str().unwrap())]);
    t.sink.set_failing(true);
    let n = t.node();
    ready(&n, "s_1").await;
    let mut tu = turn("s_1", "t_1", "a_1", "@flood 20");
    tu.run.limits.max_output_bytes = 64 << 20;
    n.start_turn(now_cmd("op_t1", tu)).await.unwrap();
    let db = rusqlite::Connection::open(t.home.join("node.sqlite3")).unwrap();
    let count = || -> i64 {
        db.query_row("SELECT COUNT(*) FROM events", [], |r| r.get(0))
            .unwrap()
    };
    t.until("output journaled", |_| {
        let c = count();
        // the gate follows what is journaled (a few events are not chunks: fewer still are ahead)
        std::fs::write(&gate, (c + AHEAD).to_string()).unwrap();
        c > 100
    })
    .await;
    // the disk is full: the next page the journal needs is refused (SQLITE_FULL); the flood goes on
    n.cap_journal_pages(0).unwrap();
    std::fs::write(&gate, "2000").unwrap();
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
    let n = open_again(&t).await;
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

fn git(dir: &Path, args: &[&str]) -> String {
    let o = std::process::Command::new("git")
        .current_dir(dir)
        .args(args)
        .output()
        .unwrap();
    assert!(o.status.success(), "git {args:?}: {o:?}");
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

fn worktree_at(resource: &str, branch: &str, rel: &str) -> Prepare {
    Prepare {
        resource: resource.into(),
        kind: PrepareKind::Worktree {
            repository: RootPath {
                root: RootId::new("work").unwrap(),
                rel: RelPath::new("repo").unwrap(),
            },
            branch: branch.into(),
            base: None,
            at: Some(RootPath {
                root: RootId::new("work").unwrap(),
                rel: RelPath::new(rel).unwrap(),
            }),
        },
    }
}

fn view_of(s: &Observed) -> WorkspaceView {
    match s {
        Observed::Finished {
            result: Some(OperationResult::Workspace(v)),
            ..
        } => v.clone(),
        other => panic!("no workspace: {other:?}"),
    }
}

/// N0e-E11a: an item's worktree goes where the Hub names it, below the disk floor only when its
/// branch is there already, is adopted only when it is that worktree, and comes back after a tidy.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_worktree_goes_where_it_is_named_above_the_disk_floor_and_comes_back_after_a_tidy() {
    let t = T::new(&[]);
    let repo = t.root.join("work/repo");
    std::fs::create_dir_all(&repo).unwrap();
    git(&repo, &["init", "-q", "-b", "main"]);
    git(&repo, &["config", "user.email", "t@t"]);
    git(&repo, &["config", "user.name", "t"]);
    git(&repo, &["config", "commit.gpgsign", "false"]);
    std::fs::write(repo.join("a.txt"), "a\n").unwrap();
    git(&repo, &["add", "."]);
    git(&repo, &["commit", "-q", "-m", "a"]);
    std::fs::create_dir_all(t.root.join("work/wt")).unwrap();
    let free = Arc::new(AtomicU64::new(0));
    let mut cfg = t.config();
    cfg.storage.worktree_floor = 1 << 30;
    cfg.storage.worktree_free = Some({
        let free = free.clone();
        Arc::new(move || free.load(Ordering::SeqCst))
    });
    let n = LocalNode::open(cfg).unwrap();
    n.connect(t.sink.clone(), None);
    let wt = t.root.join("work/wt/ST-1");

    // a new branch below the floor: refused as out of room, nothing made
    n.prepare_workspace(now_cmd(
        "op_w1",
        worktree_at("item:ST-1", "steve/st-1", "wt/ST-1"),
    ))
    .await
    .unwrap();
    match finished(&n, "op_w1").await {
        Observed::Finished {
            outcome: Outcome::Failed,
            error: Some(e),
            ..
        } => assert_eq!(e.code, ErrorCode::Exhausted, "{e:?}"),
        other => panic!("{other:?}"),
    }
    assert!(!wt.exists());

    // room again: made right there, on its branch
    free.store(2 << 30, Ordering::SeqCst);
    n.prepare_workspace(now_cmd(
        "op_w2",
        worktree_at("item:ST-1", "steve/st-1", "wt/ST-1"),
    ))
    .await
    .unwrap();
    let v = view_of(&finished(&n, "op_w2").await);
    assert_eq!(v.branch.as_deref(), Some("steve/st-1"));
    assert_eq!(
        git(&wt, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "steve/st-1"
    );
    assert_eq!(
        n.local_dir(&v.workspace).unwrap().path,
        wt.canonicalize().unwrap()
    );

    // a stale tidy is refused; the current one removes it and keeps the branch
    let stale = Tidy {
        workspace: v.workspace.clone(),
        expected: Revision::new("r_old").unwrap(),
        keep_branch: true,
    };
    let e = n.tidy_workspace(now_cmd("op_t0", stale)).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::Conflict);
    assert!(wt.exists());
    let tidy = Tidy {
        workspace: v.workspace.clone(),
        expected: v.revision.clone(),
        keep_branch: true,
    };
    n.tidy_workspace(now_cmd("op_t1", tidy)).await.unwrap();
    assert!(ok(&finished(&n, "op_t1").await));
    assert!(!wt.exists());
    git(&repo, &["rev-parse", "--verify", "steve/st-1"]);

    // its branch is there: it comes back below the floor too (its sources, megabytes)
    free.store(0, Ordering::SeqCst);
    n.prepare_workspace(now_cmd(
        "op_w3",
        worktree_at("item:ST-1", "steve/st-1", "wt/ST-1"),
    ))
    .await
    .unwrap();
    assert!(ok(&finished(&n, "op_w3").await));
    assert_eq!(
        git(&wt, &["rev-parse", "--abbrev-ref", "HEAD"]),
        "steve/st-1"
    );
    free.store(2 << 30, Ordering::SeqCst);

    // one made before (by git, as the Hub did) is adopted; a plain directory is never replaced
    git(
        &repo,
        &[
            "worktree",
            "add",
            "-b",
            "steve/st-2",
            &t.root.join("work/wt/ST-2").to_string_lossy(),
        ],
    );
    n.prepare_workspace(now_cmd(
        "op_w4",
        worktree_at("item:ST-2", "steve/st-2", "wt/ST-2"),
    ))
    .await
    .unwrap();
    assert!(ok(&finished(&n, "op_w4").await));
    std::fs::create_dir_all(t.root.join("work/wt/ST-3")).unwrap();
    std::fs::write(t.root.join("work/wt/ST-3/mine.txt"), "x").unwrap();
    n.prepare_workspace(now_cmd(
        "op_w5",
        worktree_at("item:ST-3", "steve/st-3", "wt/ST-3"),
    ))
    .await
    .unwrap();
    assert!(!ok(&finished(&n, "op_w5").await));
    assert!(t.root.join("work/wt/ST-3/mine.txt").exists());

    // never outside the roots: a symlink out, a missing parent, the root itself
    std::os::unix::fs::symlink(t.root.join("elsewhere"), t.root.join("work/wt/out")).unwrap();
    std::fs::create_dir_all(t.root.join("elsewhere")).unwrap();
    for (op, rel) in [
        ("op_x1", "wt/out"),
        ("op_x2", "nowhere/ST-4"),
        ("op_x3", ""),
    ] {
        let e = n
            .prepare_workspace(now_cmd(
                op,
                worktree_at(&format!("item:{op}"), "steve/x", rel),
            ))
            .await
            .unwrap_err();
        assert!(
            matches!(e.code, ErrorCode::Denied | ErrorCode::Invalid),
            "{rel}: {e:?}"
        );
    }
}

// ── the files face: scans, listings, reads ──

/// `t` with a read-only root `kb` at `<root>/kb` besides `work`.
fn with_kb(t: &T, n: &LocalNode) -> PathBuf {
    let kb = t.root.join("kb");
    std::fs::create_dir_all(&kb).unwrap();
    let mut roots = t.config().roots;
    roots.push(Root {
        id: RootId::new("kb").unwrap(),
        path: kb.clone(),
        display: "~/kb".into(),
        access: RootAccess::ReadOnly,
    });
    n.set_roots(roots);
    kb.canonicalize().unwrap()
}

fn put(root: &Path, rel: &str, bytes: &[u8]) {
    let f = root.join(rel);
    std::fs::create_dir_all(f.parent().unwrap()).unwrap();
    std::fs::write(f, bytes).unwrap();
}

fn kb_at(rel: &str) -> RootPath {
    RootPath {
        root: RootId::new("kb").unwrap(),
        rel: RelPath::new(rel).unwrap(),
    }
}

fn kb_scan(rel: &str, exclude: &[&str], limits: ScanLimits) -> ScanSpec {
    ScanSpec {
        root: kb_at(rel),
        include: vec![],
        exclude: exclude.iter().map(|s| s.to_string()).collect(),
        limits,
    }
}

/// Scans and reads every page, `limit` entries at a time.
async fn scan_all(n: &LocalNode, op: &str, spec: ScanSpec, limit: u32) -> Vec<ScanPage> {
    n.scan(now_cmd(op, spec)).await.unwrap();
    let s = finished(n, op).await;
    assert!(ok(&s), "{s:?}");
    let scan = OperationId::new(op).unwrap();
    let mut pages: Vec<ScanPage> = vec![];
    let mut cursor = None;
    loop {
        let p = n
            .scan_page(scan.clone(), PageRequest { cursor, limit })
            .await
            .unwrap();
        assert!(p.is_consistent(), "{p:?}");
        cursor = p.next.clone();
        pages.push(p);
        if cursor.is_none() {
            return pages;
        }
    }
}

fn entries(pages: &[ScanPage]) -> Vec<FileInfo> {
    pages.iter().flat_map(|p| p.entries.clone()).collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_scan_sees_what_the_knowledge_walk_sees() {
    use steve_knowledge::scan;
    let t = T::new(&[]);
    let n = t.node();
    let kb = with_kb(&t, &n);
    for p in [
        "b.md",
        "a.md",
        "notes.txt",
        "sub/c.md",
        "sub/deep/d.markdown",
        "sub/img.png",
        ".obsidian/x.md",
        ".hidden/y.md",
        "node_modules/z.md",
        "skip/w.md",
        "草稿/想法.md",
    ] {
        put(&kb, p, p.as_bytes());
    }
    put(
        &kb,
        "big.md",
        &vec![b'a'; scan::MAX_FILE_BYTES as usize + 1],
    );
    put(&t.root, "outside/s.md", b"secret");
    let ln = |to: &Path, at: &str| std::os::unix::fs::symlink(to, kb.join(at)).unwrap();
    ln(&kb.join("a.md"), "linked.md");
    ln(&t.root.join("outside/s.md"), "out.md");
    ln(&t.root.join("outside"), "out-dir");
    ln(&kb.join("sub"), "sub-link");
    ln(&kb.join("missing.md"), "dangling.md");

    let rules = scan::Rules::new(vec![], vec!["skip/**".into()]);
    let w = scan::walk(&kb, &rules).unwrap();
    let pages = scan_all(
        &n,
        "op_scan",
        kb_scan("", &["skip/**"], ScanLimits::default()),
        3,
    )
    .await;
    assert!(pages.len() > 2, "read in pages");
    assert!(pages.iter().all(|p| p.manifest == pages[0].manifest));

    // the same files, sizes and times, in the same order
    let mut walked: Vec<(String, u64, i64, bool)> = w
        .files
        .iter()
        .map(|e| (e.rel.clone(), e.size, e.mtime, false))
        .chain(
            w.too_big
                .iter()
                .map(|e| (e.rel.clone(), e.size, e.mtime, true)),
        )
        .collect();
    let mut scanned: Vec<(String, u64, i64, bool)> = entries(&pages)
        .into_iter()
        .map(|f| {
            (
                f.path.as_str().to_string(),
                f.size,
                f.mtime.unwrap().0,
                f.too_big,
            )
        })
        .collect();
    walked.sort();
    scanned.sort();
    assert_eq!(scanned, walked);
    assert!(scanned.iter().any(|e| e.0 == "linked.md"));
    assert!(scanned.iter().all(|e| !e.0.starts_with("sub-link")));
    // what it held back, and complete as the walk is
    let last = pages.last().unwrap();
    let held: Vec<(String, HeldWhy)> = last
        .issues
        .iter()
        .filter_map(|i| match i {
            ScanIssue::Held { path, why } => Some((path.as_str().to_string(), *why)),
            _ => None,
        })
        .collect();
    assert_eq!(held, [("out.md".to_string(), HeldWhy::Outside)]);
    assert_eq!(w.held, [("out.md".to_string(), scan::Held::Outside)]);
    assert_eq!(last.complete, w.complete());
    assert!(last.complete);

    // a directory below the root: the same files, their paths still relative to the root
    let sub = scan_all(&n, "op_sub", kb_scan("sub", &[], ScanLimits::default()), 10).await;
    let names: Vec<String> = entries(&sub)
        .iter()
        .map(|f| f.path.as_str().to_string())
        .collect();
    assert_eq!(names, ["sub/c.md", "sub/deep/d.markdown"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_scan_keeps_its_manifest_while_files_change() {
    let t = T::new(&[]);
    let n = t.node();
    let kb = with_kb(&t, &n);
    for i in 0..6 {
        put(&kb, &format!("d/{i}.md"), b"x");
    }
    let scan = OperationId::new("op_s1").unwrap();
    let c = now_cmd("op_s1", kb_scan("", &[], ScanLimits::default()));
    n.scan(c.clone()).await.unwrap();
    assert!(ok(&finished(&n, "op_s1").await));
    let first = n.scan_page(scan.clone(), page(2)).await.unwrap();
    // the tree changes between pages
    put(&kb, "d/new.md", b"new");
    put(&kb, "d/5.md", b"changed");
    std::fs::remove_file(kb.join("d/0.md")).unwrap();
    let mut pages = vec![first];
    while let Some(cursor) = pages.last().unwrap().next.clone() {
        let p = n
            .scan_page(
                scan.clone(),
                PageRequest {
                    cursor: Some(cursor),
                    limit: 2,
                },
            )
            .await
            .unwrap();
        pages.push(p);
    }
    assert!(pages.iter().all(|p| p.manifest == pages[0].manifest));
    let names: Vec<String> = entries(&pages)
        .iter()
        .map(|f| f.path.as_str().to_string())
        .collect();
    assert_eq!(
        names,
        ["d/0.md", "d/1.md", "d/2.md", "d/3.md", "d/4.md", "d/5.md"]
    );
    // the first page read again is the same page
    assert_eq!(n.scan_page(scan.clone(), page(2)).await.unwrap(), pages[0]);
    // a new scan sees the new tree, under another manifest
    let again = scan_all(&n, "op_s2", kb_scan("", &[], ScanLimits::default()), 100).await;
    assert_ne!(again[0].manifest, pages[0].manifest);
    assert_eq!(entries(&again).len(), 6);
    // a cursor of one scan does not page another; an unknown scan is invalid
    let e = n
        .scan_page(
            OperationId::new("op_s2").unwrap(),
            PageRequest {
                cursor: pages[0].next.clone(),
                limit: 2,
            },
        )
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Invalid);
    let e = n
        .scan_page(OperationId::new("op_nope").unwrap(), page(1))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Invalid);
    // a resend is the same scan
    n.scan(c).await.unwrap();
    assert_eq!(n.scan_page(scan, page(2)).await.unwrap(), pages[0]);
}

/// More scans than a few at once, all accepted before any is read and paged in turns: none pushes
/// another's manifest out while it is read.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn many_scans_paged_at_once_all_finish() {
    let t = T::new(&[]);
    let n = t.node();
    let kb = with_kb(&t, &n);
    for i in 0..5 {
        put(&kb, &format!("d/{i}.md"), b"x");
    }
    let ops: Vec<String> = (0..12).map(|i| format!("op_many{i}")).collect();
    for op in &ops {
        n.scan(now_cmd(op, kb_scan("", &[], ScanLimits::default())))
            .await
            .unwrap();
    }
    for op in &ops {
        assert!(ok(&finished(&n, op).await));
    }
    let mut cursors: Vec<Option<Cursor>> = vec![None; ops.len()];
    let mut seen = vec![0usize; ops.len()];
    let mut done = vec![false; ops.len()];
    while done.iter().any(|d| !d) {
        for (i, op) in ops.iter().enumerate() {
            if done[i] {
                continue;
            }
            let p = n
                .scan_page(
                    OperationId::new(op).unwrap(),
                    PageRequest {
                        cursor: cursors[i].clone(),
                        limit: 1,
                    },
                )
                .await
                .unwrap();
            seen[i] += p.entries.len();
            cursors[i] = p.next.clone();
            done[i] = p.next.is_none();
        }
    }
    assert!(seen.iter().all(|n| *n == 5), "{seen:?}");
}

fn page(limit: u32) -> PageRequest {
    PageRequest {
        cursor: None,
        limit,
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_scan_that_may_hide_files_is_never_complete() {
    use std::os::unix::fs::PermissionsExt;
    let t = T::new(&[]);
    let n = t.node();
    let kb = with_kb(&t, &n);
    for p in ["a.md", "b.md", "c.md", "locked/x.md"] {
        put(&kb, p, b"x");
    }
    // over the file limit: cut, the count said
    let limits = ScanLimits {
        max_files: 2,
        max_file_bytes: 1024,
    };
    let over = scan_all(&n, "op_over", kb_scan("", &["locked/**"], limits), 10).await;
    let last = over.last().unwrap();
    assert_eq!(entries(&over).len(), 2);
    assert!(!last.complete);
    assert_eq!(last.issues, [ScanIssue::OverLimit { count: 1 }]);

    // an unreadable directory
    let locked = kb.join("locked");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let readable = std::fs::read_dir(&locked).is_ok(); // root ignores the mode
    let pages = scan_all(&n, "op_locked", kb_scan("", &[], ScanLimits::default()), 10).await;
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).unwrap();
    if readable {
        return;
    }
    let last = pages.last().unwrap();
    assert_eq!(entries(&pages).len(), 3);
    assert!(!last.complete);
    assert_eq!(
        last.issues,
        [ScanIssue::Unreadable {
            path: Some(RelPath::new("locked").unwrap())
        }]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn reads_stay_inside_their_root_and_see_a_changed_file() {
    let t = T::new(&[]);
    let n = t.node();
    let kb = with_kb(&t, &n);
    put(&kb, "a.md", b"hello world");
    put(&t.root, "outside/s.md", b"secret");
    std::os::unix::fs::symlink(t.root.join("outside/s.md"), kb.join("out.md")).unwrap();
    std::os::unix::fs::symlink(t.root.join("outside"), kb.join("out-dir")).unwrap();
    let read = |path: FileAddress, expected: Option<Revision>, offset: u64, max: u32| FileRead {
        path,
        expected,
        offset,
        max,
    };
    // in chunks, to the end
    let mut got = vec![];
    let mut offset = 0;
    loop {
        let c = n
            .read_file(read(FileAddress::Root(kb_at("a.md")), None, offset, 4))
            .await
            .unwrap();
        assert_eq!(c.offset, offset);
        assert!(c.bytes.len() <= 4);
        offset += c.bytes.len() as u64;
        got.extend(c.bytes);
        if c.eof {
            break;
        }
    }
    assert_eq!(got, b"hello world");
    // a read asks for no more than a frame carries
    put(&kb, "big.md", &vec![b'a'; 1 << 20]);
    let c = n
        .read_file(read(FileAddress::Root(kb_at("big.md")), None, 0, u32::MAX))
        .await
        .unwrap();
    assert!(c.bytes.len() < 1 << 20 && !c.eof);

    // nothing outside the root: a link out, through a linked directory, another root, `..`
    for rel in ["out.md", "out-dir/s.md"] {
        let e = n
            .read_file(read(FileAddress::Root(kb_at(rel)), None, 0, 100))
            .await
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::Denied, "{rel}");
    }
    let e = n
        .read_file(read(
            FileAddress::Root(RootPath {
                root: RootId::new("elsewhere").unwrap(),
                rel: RelPath::new("a.md").unwrap(),
            }),
            None,
            0,
            100,
        ))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Denied);
    assert!(RelPath::new("../outside/s.md").is_err());
    assert!(RelPath::new("/etc/passwd").is_err());
    let forged = serde_json::json!({
        "path": {"in": "root", "root": "kb", "rel": "../outside/s.md"},
        "offset": 0, "max": 100,
    });
    assert!(serde_json::from_value::<FileRead>(forged).is_err());

    // a version from a scan: read as long as the file is as it was, then a conflict
    let pages = scan_all(&n, "op_scan", kb_scan("", &[], ScanLimits::default()), 10).await;
    let a = entries(&pages)
        .into_iter()
        .find(|f| f.path.as_str() == "a.md")
        .unwrap();
    let c = n
        .read_file(read(
            FileAddress::Root(kb_at("a.md")),
            Some(a.version.clone()),
            0,
            100,
        ))
        .await
        .unwrap();
    assert_eq!(c.bytes, b"hello world");
    std::thread::sleep(Duration::from_millis(5));
    put(&kb, "a.md", b"hello again");
    let e = n
        .read_file(read(
            FileAddress::Root(kb_at("a.md")),
            Some(a.version),
            0,
            100,
        ))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Conflict);

    // in a workspace: the same rules
    put(&t.root, "work/proj/src/lib.rs", b"fn main() {}");
    std::os::unix::fs::symlink(t.root.join("outside/s.md"), t.root.join("work/proj/leak")).unwrap();
    n.prepare_workspace(now_cmd("op_prep", prepare_proj()))
        .await
        .unwrap();
    assert!(ok(&finished(&n, "op_prep").await));
    let in_ws = |rel: &str| {
        FileAddress::Workspace(PathRef {
            workspace: ws(),
            rel: RelPath::new(rel).unwrap(),
        })
    };
    let c = n
        .read_file(read(in_ws("src/lib.rs"), None, 0, 100))
        .await
        .unwrap();
    assert_eq!(c.bytes, b"fn main() {}");
    let e = n
        .read_file(read(in_ws("leak"), None, 0, 100))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Denied);
    let listed = n
        .list_dir(
            PathRef {
                workspace: ws(),
                rel: RelPath::new("").unwrap(),
            },
            page(100),
        )
        .await
        .unwrap();
    let names: Vec<&str> = listed.items.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(names, ["src"], "a link out is not listed");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn listings_page_and_do_not_follow_linked_directories() {
    let t = T::new(&[]);
    let n = t.node();
    let kb = with_kb(&t, &n);
    for p in ["a.md", "b/c.md", "d/e.md"] {
        put(&kb, p, b"x");
    }
    std::os::unix::fs::symlink(kb.join("b"), kb.join("b-link")).unwrap();
    std::os::unix::fs::symlink(kb.join("a.md"), kb.join("a-link.md")).unwrap();
    let mut all = vec![];
    let mut cursor = None;
    loop {
        let p = n
            .list_root(kb_at(""), PageRequest { cursor, limit: 1 })
            .await
            .unwrap();
        assert!(p.items.len() <= 1);
        all.extend(p.items);
        cursor = p.next;
        if cursor.is_none() {
            break;
        }
    }
    let shown: Vec<(&str, bool)> = all.iter().map(|f| (f.path.as_str(), f.is_dir)).collect();
    assert_eq!(
        shown,
        [
            ("a-link.md", false),
            ("a.md", false),
            ("b", true),
            ("d", true)
        ]
    );
    let below = n.list_root(kb_at("b"), page(10)).await.unwrap();
    let names: Vec<&str> = below.items.iter().map(|f| f.path.as_str()).collect();
    assert_eq!(names, ["b/c.md"]);
    // a directory that is not there
    let e = n.list_root(kb_at("nope"), page(10)).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::Invalid);
    // a read-only root is listed, read and scanned, but a bad pattern is refused
    let bad = ScanSpec {
        include: vec!["../*.md".into()],
        ..kb_scan("", &[], ScanLimits::default())
    };
    let e = n.scan(now_cmd("op_bad", bad)).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::Invalid);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_scan_read_after_a_restart_is_scanned_again() {
    let t = T::new(&[]);
    put(&t.root, "work/notes/a.md", b"x");
    let n = t.node();
    let spec = ScanSpec {
        root: RootPath {
            root: RootId::new("work").unwrap(),
            rel: RelPath::new("notes").unwrap(),
        },
        include: vec![],
        exclude: vec![],
        limits: ScanLimits::default(),
    };
    let pages = scan_all(&n, "op_scan", spec, 10).await;
    assert_eq!(entries(&pages).len(), 1);
    n.shutdown().await;
    drop(n);
    let n = reopen(&t, t.sink.clone()).await;
    let e = n
        .scan_page(OperationId::new("op_scan").unwrap(), page(10))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Expired);
}

fn git_in(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output()
        .unwrap();
    assert!(out.status.success(), "git {args:?}");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn git_facts_reads_files_commits_logs_and_diffs_of_a_workspace() {
    let t = T::new(&[]);
    let repo = t.root.join("work/proj");
    git_in(&repo, &["init", "-q", "-b", "main"]);
    put(&t.root, "work/proj/a.txt", b"a\n");
    git_in(&repo, &["add", "."]);
    git_in(&repo, &["commit", "-q", "-m", "a"]);
    git_in(&repo, &["checkout", "-q", "-b", "steve/st-1"]);
    put(
        &t.root,
        "work/proj/docs/方案.md",
        b"# \xe6\x96\xb9\xe6\xa1\x88\n",
    );
    git_in(&repo, &["add", "."]);
    git_in(&repo, &["commit", "-q", "-m", "加方案"]);
    let tip = git_in(&repo, &["rev-parse", "HEAD"]);
    git_in(&repo, &["checkout", "-q", "main"]);
    let n = t.node();
    n.prepare_workspace(now_cmd("op_prep", prepare_proj()))
        .await
        .unwrap();
    assert!(ok(&finished(&n, "op_prep").await));

    let range = |from: Option<&str>, to: &str| RevRange {
        from: from.map(str::to_string),
        to: to.into(),
    };
    let reads = vec![
        GitRead::Show {
            rev: "steve/st-1".into(),
            path: Some(RelPath::new("docs/方案.md").unwrap()),
            max: 1000,
        },
        GitRead::Show {
            rev: "main".into(),
            path: Some(RelPath::new("docs/方案.md").unwrap()),
            max: 1000,
        },
        GitRead::Show {
            rev: tip[..7].into(),
            path: None,
            max: 1000,
        },
        GitRead::Log {
            range: range(Some("main"), "steve/st-1"),
            max: 200,
        },
        GitRead::Diff {
            range: range(Some("main"), "steve/st-1"),
            max: 20,
        },
    ];
    let f = n
        .git_facts(GitQuery {
            workspace: ws(),
            refs: vec!["refs/heads/main".into(), "refs/heads/gone".into()],
            ranges: vec![],
            fetch: false,
            reads: reads.clone(),
            heads_only: false,
        })
        .await
        .unwrap();
    assert_eq!(f.branch.as_deref(), Some("main"));
    assert!(!f.dirty);
    assert_eq!(f.refs[0].1.as_deref(), Some(f.head.as_str()));
    assert_eq!(f.refs[1].1, None);
    let a: Vec<&GitAnswer> = f.reads.iter().map(|r| &r.answer).collect();
    assert_eq!(f.reads[0].observed, tip);
    assert!(
        matches!(a[0], GitAnswer::Blob { found: true, bytes, truncated: false } if bytes == "# 方案\n".as_bytes()),
        "{:?}",
        a[0]
    );
    assert!(matches!(a[1], GitAnswer::Blob { found: false, .. }));
    let GitAnswer::Commit {
        commit,
        stat,
        patch,
        truncated,
    } = a[2]
    else {
        panic!("{:?}", a[2])
    };
    assert_eq!(commit.sha, tip);
    assert_eq!(commit.subject, "加方案");
    assert_eq!(commit.body.as_deref().map(str::trim), Some("加方案"));
    assert!(stat.contains("docs/方案.md"), "{stat}");
    assert!(patch.contains("diff --git a/docs/方案.md"), "{patch}");
    assert!(!truncated);
    let GitAnswer::Log { commits, truncated } = a[3] else {
        panic!("{:?}", a[3])
    };
    assert_eq!((commits.len(), *truncated), (1, false));
    assert_eq!(commits[0].subject, "加方案");
    assert_eq!(commits[0].abbrev.as_deref(), Some(&tip[..7]));
    let GitAnswer::Diff {
        stat,
        patch,
        truncated,
    } = a[4]
    else {
        panic!("{:?}", a[4])
    };
    assert!(stat.contains("docs/方案.md"));
    assert!(patch.len() <= 20 && *truncated, "{patch}");

    // a revision is never an option; a changed tree is another revision
    let q = |rev: &str| GitQuery {
        workspace: ws(),
        refs: vec![],
        ranges: vec![],
        fetch: false,
        reads: vec![GitRead::Show {
            rev: rev.into(),
            path: None,
            max: 10,
        }],
        heads_only: false,
    };
    assert_eq!(
        n.git_facts(q("--output=x")).await.unwrap_err().code,
        ErrorCode::Invalid
    );
    // a revision that names no commit is an answer, not a failure (the checkout is there)
    let none = n.git_facts(q("1234567")).await.unwrap();
    assert!(
        matches!(&none.reads[0].answer, GitAnswer::Blob { found: false, .. }),
        "{:?}",
        none.reads
    );
    put(&t.root, "work/proj/new.txt", b"x");
    let g = n.git_facts(q("HEAD")).await.unwrap();
    assert!(g.dirty);
    assert_ne!(g.revision, f.revision);
    // heads only: no tree scan, a revision of another kind than one with the tree in it
    let h = n
        .git_facts(GitQuery {
            heads_only: true,
            ..q("HEAD")
        })
        .await
        .unwrap();
    assert!(!h.dirty);
    assert_eq!(h.head, g.head);
    assert_ne!(h.revision, g.revision);
    // the hash as git abbreviates it here, which follows core.abbrev
    git_in(&repo, &["config", "core.abbrev", "10"]);
    let h = n
        .git_facts(GitQuery {
            heads_only: true,
            ..q("HEAD")
        })
        .await
        .unwrap();
    let GitAnswer::Commit { commit, .. } = &h.reads[0].answer else {
        panic!("{:?}", h.reads)
    };
    assert_eq!(commit.abbrev.as_deref(), Some(&commit.sha[..10]));
    assert!(!repo.join("x").exists());
}

/// A repository at `work/proj` on `main` with `a.txt`, a branch `steve/st-1` with one commit of
/// its own checked out, an identity for commits the Node makes, and the Node's workspace on it.
async fn branch_repo(t: &T) -> (LocalNode, PathBuf) {
    let repo = t.root.join("work/proj");
    git_in(&repo, &["init", "-q", "-b", "main"]);
    git_in(&repo, &["config", "user.name", "t"]);
    git_in(&repo, &["config", "user.email", "t@example.com"]);
    put(&t.root, "work/proj/a.txt", b"a\n");
    git_in(&repo, &["add", "."]);
    git_in(&repo, &["commit", "-q", "-m", "a"]);
    git_in(&repo, &["checkout", "-q", "-b", "steve/st-1"]);
    put(&t.root, "work/proj/b.txt", b"b\n");
    git_in(&repo, &["add", "."]);
    git_in(&repo, &["commit", "-q", "-m", "b"]);
    let n = t.node();
    n.prepare_workspace(now_cmd("op_prep", prepare_proj()))
        .await
        .unwrap();
    assert!(ok(&finished(&n, "op_prep").await));
    (n, repo)
}

fn query(reads: Vec<GitRead>) -> GitQuery {
    GitQuery {
        workspace: ws(),
        refs: vec![],
        ranges: vec![],
        fetch: false,
        reads,
        heads_only: false,
    }
}

#[tokio::test]
async fn git_facts_answer_the_tree_its_own_change_ranges_and_where_it_landed() {
    let t = T::new(&[]);
    let (n, repo) = branch_repo(&t).await;
    let main = git_in(&repo, &["rev-parse", "main"]);
    put(&t.root, "work/proj/new.txt", b"n\n");
    let mut q = query(vec![
        GitRead::Status,
        GitRead::Fingerprint,
        GitRead::IsAncestor {
            ancestor: "main".into(),
            of: "HEAD".into(),
        },
        GitRead::ChangedPaths {
            from: "main".into(),
            to: "HEAD".into(),
            max: 10,
        },
        GitRead::Landed {
            commit: "HEAD".into(),
            onto: "main".into(),
        },
        GitRead::RemoteUrl {
            remote: "origin".into(),
        },
    ]);
    q.ranges = vec![("main".into(), "HEAD".into())];
    let f = n.git_facts(q.clone()).await.unwrap();
    assert_eq!(f.ranges[0].ahead, 1);
    assert_eq!(f.ranges[0].behind, 0);
    assert_eq!(f.ranges[0].merge_base.as_deref(), Some(main.as_str()));
    let a: Vec<&GitAnswer> = f.reads.iter().map(|r| &r.answer).collect();
    let GitAnswer::Status { entries } = a[0] else {
        panic!("{:?}", a[0])
    };
    assert_eq!(entries[0].code, "??");
    assert_eq!(entries[0].path.as_str(), "new.txt");
    let GitAnswer::Fingerprint {
        tree,
        own: Some(own),
    } = a[1]
    else {
        panic!("{:?}", a[1])
    };
    assert_eq!(own.base, main);
    assert_ne!(own.patch, "-");
    // the untracked file is in the tree; the workspace's own index is untouched
    let listed = git_in(&repo, &["ls-tree", "--name-only", tree]);
    assert!(listed.lines().any(|l| l == "new.txt"), "{listed}");
    assert!(git_in(&repo, &["status", "--porcelain"]).contains("?? new.txt"));
    assert_eq!(a[2], &GitAnswer::IsAncestor { yes: true });
    assert_eq!(
        a[3],
        &GitAnswer::ChangedPaths {
            paths: vec!["b.txt".into()],
            truncated: false
        }
    );
    assert_eq!(a[4], &GitAnswer::Landed { commit: None });
    assert_eq!(a[5], &GitAnswer::RemoteUrl { url: None });

    // squashed onto main, the change has landed there; the URL comes without its credential
    git_in(&repo, &["checkout", "-q", "main"]);
    git_in(&repo, &["merge", "-q", "--squash", "steve/st-1"]);
    git_in(&repo, &["commit", "-q", "-m", "squash"]);
    let squash = git_in(&repo, &["rev-parse", "HEAD"]);
    git_in(&repo, &["checkout", "-q", "steve/st-1"]);
    git_in(
        &repo,
        &[
            "remote",
            "add",
            "origin",
            "https://u:secret@example.com/o/r.git",
        ],
    );
    let f = n.git_facts(q).await.unwrap();
    assert_eq!(
        f.reads[4].answer,
        GitAnswer::Landed {
            commit: Some(squash)
        }
    );
    assert_eq!(
        f.reads[5].answer,
        GitAnswer::RemoteUrl {
            url: Some("https://example.com/o/r.git".into())
        }
    );
}

#[tokio::test]
async fn a_fetch_and_the_remotes_tags_come_from_origin() {
    let t = T::new(&[]);
    let (n, repo) = branch_repo(&t).await;
    let origin = t.root.join("origin.git");
    git_in(&t.root, &["init", "-q", "--bare", origin.to_str().unwrap()]);
    git_in(
        &repo,
        &["remote", "add", "origin", origin.to_str().unwrap()],
    );
    git_in(&repo, &["push", "-q", "origin", "main"]);
    git_in(&repo, &["tag", "-a", "-m", "v", "v1.0.0", "main"]);
    git_in(&repo, &["push", "-q", "origin", "v1.0.0"]);
    git_in(&repo, &["tag", "local-only", "main"]);
    let main = git_in(&repo, &["rev-parse", "main"]);
    let mut q = query(vec![GitRead::RemoteTags {
        remote: "origin".into(),
    }]);
    q.fetch = true;
    q.refs = vec!["refs/remotes/origin/main".into()];
    let f = n.git_facts(q).await.unwrap();
    assert_eq!(f.refs[0].1.as_deref(), Some(main.as_str()));
    assert_eq!(
        f.reads[0].answer,
        GitAnswer::RemoteTags {
            tags: vec![("v1.0.0".into(), main)]
        }
    );
    // a remote that is not there: the fetch says so, nothing else is answered
    git_in(
        &repo,
        &["remote", "set-url", "origin", "/nonexistent/x.git"],
    );
    let mut q = query(vec![]);
    q.fetch = true;
    assert_eq!(n.git_facts(q).await.unwrap_err().code, ErrorCode::Invalid);
}

fn rebase_onto(f: &GitFacts, onto: &str) -> GitApply {
    GitApply {
        workspace: ws(),
        expected: f.revision.clone(),
        expected_head: f.head.clone(),
        action: GitAction::Rebase { onto: onto.into() },
    }
}

#[tokio::test]
async fn a_rebase_applies_only_on_the_facts_it_was_decided_on() {
    let t = T::new(&[]);
    let (n, repo) = branch_repo(&t).await;
    git_in(&repo, &["checkout", "-q", "main"]);
    put(&t.root, "work/proj/c.txt", b"c\n");
    git_in(&repo, &["add", "."]);
    git_in(&repo, &["commit", "-q", "-m", "c"]);
    git_in(&repo, &["checkout", "-q", "steve/st-1"]);
    let f = n.git_facts(query(vec![])).await.unwrap();

    // HEAD moved since the facts: refused, nothing done
    put(&t.root, "work/proj/b.txt", b"b2\n");
    git_in(&repo, &["commit", "-q", "-am", "b2"]);
    let moved = git_in(&repo, &["rev-parse", "HEAD"]);
    let e = n
        .git_apply(now_cmd("op_rb1", rebase_onto(&f, "main")))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Stale);
    assert_eq!(git_in(&repo, &["rev-parse", "HEAD"]), moved);

    // on the facts as they are: rebased, the new facts in the result; the same command again is
    // the same answer, not a second rebase
    let f = n.git_facts(query(vec![])).await.unwrap();
    let rb2 = now_cmd("op_rb2", rebase_onto(&f, "main"));
    n.git_apply(rb2.clone()).await.unwrap();
    let Observed::Finished {
        outcome: Outcome::Succeeded,
        result: Some(OperationResult::Git(after)),
        ..
    } = finished(&n, "op_rb2").await
    else {
        panic!("not rebased")
    };
    assert_ne!(after.head, f.head);
    assert_eq!(git_in(&repo, &["rev-parse", "HEAD"]), after.head);
    git_in(&repo, &["merge-base", "--is-ancestor", "main", "HEAD"]);
    let again = n.git_apply(rb2).await.unwrap();
    assert!(again.state.is_finished());
    assert_eq!(git_in(&repo, &["rev-parse", "HEAD"]), after.head);

    // a conflict is backed out and named
    git_in(&repo, &["checkout", "-q", "main"]);
    put(&t.root, "work/proj/b.txt", b"main\n");
    git_in(&repo, &["add", "."]);
    git_in(&repo, &["commit", "-q", "-m", "main b"]);
    git_in(&repo, &["checkout", "-q", "steve/st-1"]);
    let f = n.git_facts(query(vec![])).await.unwrap();
    n.git_apply(now_cmd("op_rb3", rebase_onto(&f, "main")))
        .await
        .unwrap();
    let Observed::Finished {
        outcome: Outcome::Failed,
        error: Some(e),
        ..
    } = finished(&n, "op_rb3").await
    else {
        panic!("a conflict rebased")
    };
    assert_eq!(
        (e.code, e.safe_detail.as_str()),
        (ErrorCode::Conflict, "b.txt")
    );
    assert_eq!(git_in(&repo, &["rev-parse", "HEAD"]), f.head);
    assert_eq!(git_in(&repo, &["status", "--porcelain"]), "");

    // uncommitted tracked changes: not touched
    put(&t.root, "work/proj/b.txt", b"dirty\n");
    let f = n.git_facts(query(vec![])).await.unwrap();
    n.git_apply(now_cmd("op_rb4", rebase_onto(&f, "main")))
        .await
        .unwrap();
    let Observed::Finished { error: Some(e), .. } = finished(&n, "op_rb4").await else {
        panic!("a dirty tree rebased")
    };
    assert_eq!(e.code, ErrorCode::Busy);
    assert_eq!(std::fs::read(repo.join("b.txt")).unwrap(), b"dirty\n");
}

/// H1: the owner's ceiling holds whatever the Hub asks. The default one takes no FullAccess
/// (`bypassPermissions`), no commands and no jobs, and is refused before anything is accepted;
/// what is under it runs as before. Without the open network allowed nothing opens.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_owners_ceiling_refuses_what_is_over_it() {
    let t = T::new(&[]);
    let mut cfg = t.config();
    cfg.ceiling = steve_node::PolicyCeiling::default();
    cfg.capacity.execs = 2;
    cfg.capacity.jobs = 2;
    let n = LocalNode::open(cfg).unwrap();
    n.connect(t.sink.clone(), None);
    // under the ceiling: as before
    ready(&n, "s_1").await;
    let mut o = open_body("s_2");
    o.policy.permissions = PermissionMode::FullAccess;
    let e = n.open_session(now_cmd("op_o2", o)).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::Denied, "{e:?}");
    assert!(e.safe_detail.contains("full_access"), "{e:?}");
    let v = n.session(SessionId::new("s_1").unwrap()).await.unwrap();
    let mut wide = policy();
    wide.permissions = PermissionMode::FullAccess;
    let e = n
        .configure_session(now_cmd(
            "op_t1",
            ConfigureSession {
                session: SessionId::new("s_1").unwrap(),
                expected: v.revision,
                choice: choice(),
                policy: wide,
            },
        ))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Denied, "{e:?}");
    // nothing of either was taken
    assert!(
        n.operation(OperationId::new("op_o2").unwrap())
            .await
            .is_err()
    );
    assert!(
        n.operation(OperationId::new("op_t1").unwrap())
            .await
            .is_err()
    );
    let e = n
        .exec(now_cmd(
            "op_x",
            ExecSpec {
                workspace: ws(),
                command: "true".into(),
                cwd: None,
                policy: policy(),
                limits: limits(),
                env: vec![],
                secrets: vec![],
            },
        ))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Denied, "{e:?}");
    let e = n
        .start_job(now_cmd(
            "op_j",
            steve_node_api::testkit::build::job("j1", "ja1", "hi"),
        ))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Denied, "{e:?}");
    // a turn under it still runs
    n.start_turn(now_cmd("op_t2", turn("s_1", "t_2", "a_2", "@say ok")))
        .await
        .unwrap();
    t.until("the end", |t| !t.ended().is_empty()).await;
    n.shutdown().await;

    // the owner widens it: FullAccess is taken
    let t = T::new(&[]);
    let mut cfg = t.config();
    cfg.ceiling = steve_node::PolicyCeiling {
        max_permissions: PermissionMode::FullAccess,
        ..Default::default()
    };
    let n = LocalNode::open(cfg).unwrap();
    n.connect(t.sink.clone(), None);
    n.prepare_workspace(now_cmd("op_prep", prepare_proj()))
        .await
        .unwrap();
    assert!(ok(&finished(&n, "op_prep").await));
    let mut o = open_body("s_3");
    o.policy.permissions = PermissionMode::FullAccess;
    n.open_session(now_cmd("op_o3", o)).await.unwrap();
    let s = finished(&n, "op_o3").await;
    assert!(ok(&s), "{s:?}");
    n.shutdown().await;

    // no open network: nothing opens (a harness's network cannot be narrowed yet)
    let t = T::new(&[]);
    let mut cfg = t.config();
    cfg.ceiling = steve_node::PolicyCeiling {
        full_network: false,
        ..Default::default()
    };
    let n = LocalNode::open(cfg).unwrap();
    n.connect(t.sink.clone(), None);
    let e = n
        .open_session(now_cmd("op_o4", open_body("s_4")))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Denied, "{e:?}");
}

/// What the harness says in small chunks goes out as fewer, bigger deltas of the very same text,
/// soon (within the hold, not at the end of the turn), and in order with what follows it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn chunks_said_together_go_out_as_one_delta_soon() {
    let t = T::new(&[]);
    let n = t.node();
    ready(&n, "s_1").await;
    let words: Vec<String> = (1..=20).map(|i| format!("w{i}.")).collect();
    let said: String = words.concat();
    let prompt = format!(
        "{} @sleep 1500 @say delta @tool read Read",
        words
            .iter()
            .map(|w| format!("@say {w}"))
            .collect::<Vec<_>>()
            .join(" ")
    );
    n.start_turn(now_cmd("op_t1", turn("s_1", "t_1", "a_1", &prompt)))
        .await
        .unwrap();
    // seen while the harness still sleeps: held text is not kept until the turn ends
    t.until("the first text", |t| t.text() == said).await;
    assert!(t.ended().is_empty(), "before the end of the turn");
    t.until("the end", |t| !t.ended().is_empty()).await;
    let deltas: Vec<(u64, String)> = t
        .sink
        .events()
        .into_iter()
        .filter_map(|e| match e.body {
            EventBody::TextDelta {
                utf8_offset, text, ..
            } => Some((utf8_offset, text)),
            _ => None,
        })
        .collect();
    // the same bytes, contiguous, in far fewer deltas than the 21 chunks
    let mut at = 0;
    for (off, text) in &deltas {
        assert_eq!(*off, at, "{deltas:?}");
        at += text.len() as u64;
    }
    assert_eq!(t.text(), format!("{said}delta"));
    assert!(deltas.len() <= 4, "{deltas:?}");
    assert_eq!(
        deltas.last().unwrap().1,
        "delta",
        "said after the pause: its own delta"
    );
    // the text came before the tool the harness reported after it
    let kinds = t.kinds();
    let last_text = kinds.iter().rposition(|k| k == "text_delta").unwrap();
    let tool = kinds.iter().position(|k| k == "tool_changed").unwrap();
    assert!(last_text < tool, "{kinds:?}");
    n.shutdown().await;
}

/// A Hub that takes a while per batch, counting how many it is given at once.
#[derive(Default)]
struct Slow {
    hub: RecordingSink,
    now: std::sync::atomic::AtomicUsize,
    most: std::sync::atomic::AtomicUsize,
}

#[async_trait::async_trait]
impl EventSink for Slow {
    async fn commit(&self, batch: EventBatch) -> ApiResult<EventAck> {
        let n = self.now.fetch_add(1, Ordering::SeqCst) + 1;
        self.most.fetch_max(n, Ordering::SeqCst);
        tokio::time::sleep(Duration::from_millis(40)).await;
        let ack = self.hub.commit(batch).await;
        self.now.fetch_sub(1, Ordering::SeqCst);
        ack
    }
}

/// Several batches are in flight at once, and every event still arrives once in its stream's
/// order, all of them acknowledged.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn batches_go_several_at_a_time_and_all_are_acknowledged() {
    let t = T::new(&[]);
    let n = LocalNode::open(t.config()).unwrap();
    let slow = Arc::new(Slow::default());
    n.connect(slow.clone(), None);
    ready(&n, "s_1").await;
    // bursts of 10 calls every 30 ms: on a slow runner the actor falls behind them, and the
    // harness waits for it (they once overflowed its 64-slot queue and cancelled the attempt)
    let group = ["@tool read Read"; 10].join(" ");
    let prompt = vec![format!("{group} @sleep 30"); 60].join(" ");
    let mut spec = turn("s_1", "t_1", "a_1", &prompt);
    spec.run.limits.max_output_bytes = 64 << 20;
    n.start_turn(now_cmd("op_t1", spec)).await.unwrap();
    let t0 = Instant::now();
    while !slow
        .hub
        .events()
        .iter()
        .any(|e| matches!(e.body, EventBody::AttemptEnded { .. }))
    {
        assert!(t0.elapsed() < Duration::from_secs(30), "the end");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    all_acknowledged(&n).await;
    let tools = slow
        .hub
        .events()
        .iter()
        .filter(|e| matches!(e.body, EventBody::ToolChanged { .. }))
        .count();
    let end: Vec<_> = slow
        .hub
        .events()
        .into_iter()
        .filter_map(|e| match e.body {
            EventBody::AttemptEnded { completion } => Some(completion),
            _ => None,
        })
        .collect();
    assert!(tools >= 1200, "{tools}: {end:?}");
    assert!(
        slow.most.load(Ordering::SeqCst) > 1,
        "more than one batch at a time"
    );
    // each stream's positions are contiguous from 1 (the sink holds each once)
    let mut last: HashMap<String, u64> = HashMap::new();
    for e in slow.hub.events() {
        let l = last.entry(e.cursor.stream.to_string()).or_insert(0);
        assert_eq!(e.cursor.seq, *l + 1, "{:?}", e.cursor);
        *l = e.cursor.seq;
    }
    n.shutdown().await;
}

/// A Node whose journal takes `ms` more for every write of data (as a slow disk's fsync does: each
/// one asks for the free space first), blocking the session's actor meanwhile.
fn slow_journal(t: &T, ms: u64) -> NodeConfig {
    let mut cfg = t.config();
    cfg.storage.free_space = Some(Arc::new(move || {
        std::thread::sleep(Duration::from_millis(ms));
        1 << 40
    }));
    cfg
}

/// A slow disk and a busy Hub put the Node behind a harness that writes a burst at once (100
/// chunks, then 150 tool calls: several times what the actor's queue holds): the harness waits for it,
/// and the attempt ends as it would have, every update delivered in order.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_slow_journal_and_hub_hold_the_harness_back_and_cancel_nothing() {
    let t = T::new(&[]);
    let n = LocalNode::open(slow_journal(&t, 2)).unwrap();
    let slow = Arc::new(Slow::default());
    n.connect(slow.clone(), None);
    ready(&n, "s_1").await;
    let prompt = format!("@drip 100 0 {}", ["@tool read Read"; 150].join(" "));
    let mut spec = turn("s_1", "t_1", "a_1", &prompt);
    spec.run.limits.max_output_bytes = 64 << 20;
    n.start_turn(now_cmd("op_t1", spec)).await.unwrap();
    let t0 = Instant::now();
    let end = loop {
        if let Some(c) = slow.hub.events().into_iter().find_map(|e| match e.body {
            EventBody::AttemptEnded { completion } => Some(completion),
            _ => None,
        }) {
            break c;
        }
        assert!(t0.elapsed() < Duration::from_secs(120), "the end");
        tokio::time::sleep(Duration::from_millis(20)).await;
    };
    assert_eq!(end.outcome, Outcome::Succeeded, "{end:?}");
    all_acknowledged(&n).await;
    let events = slow.hub.events();
    let text: String = events
        .iter()
        .filter_map(|e| match &e.body {
            EventBody::TextDelta { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    let want: String = (0..100).map(|i| format!("t{i} ")).collect();
    assert_eq!(text, want, "every chunk, in order");
    let tools = events
        .iter()
        .filter(|e| matches!(e.body, EventBody::ToolChanged { .. }))
        .count();
    assert_eq!(tools, 300, "tool_call and its update, 150 times");
    n.shutdown().await;
}

/// Control is never behind the harness's data: a cancel goes out at once and a shutdown ends
/// while the harness waits on a full queue (the actor slowed by its journal); nothing waits forever.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_cancel_and_a_shutdown_get_past_a_full_ingress_queue() {
    let t = T::new(&[]);
    let n = LocalNode::open(slow_journal(&t, 3)).unwrap();
    n.connect(t.sink.clone(), None);
    ready(&n, "s_1").await;
    let mut spec = turn("s_1", "t_1", "a_1", "@flood 20");
    spec.run.limits.max_output_bytes = 64 << 20;
    n.start_turn(now_cmd("op_t1", spec)).await.unwrap();
    t.until("the flood", |t| t.text().len() >= 16 * 4096).await;
    let asked = Instant::now();
    n.cancel(now_cmd(
        "op_c",
        Cancel {
            target: OperationId::new("op_t1").unwrap(),
            reason: "paused".into(),
        },
    ))
    .await
    .unwrap();
    t.until("the cancel", |t| t.sent("session/cancel") == 1)
        .await;
    assert!(
        asked.elapsed() < Duration::from_secs(3),
        "{:?}",
        asked.elapsed()
    );
    tokio::time::timeout(Duration::from_secs(15), n.shutdown())
        .await
        .expect("shutdown with the harness waiting on a full queue");
    assert!(t.text().len() < 2000 * 4096, "cut short");
}

/// A harness the Node takes nothing from at all (its journal stuck, not slow) is still stopped,
/// with why, once the queue was full for the stall limit; the actor is never wedged by it.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_harness_stuck_behind_a_stuck_journal_is_stopped_with_why() {
    let gates = tempfile::tempdir().unwrap();
    let flood = gates.path().join("flood");
    let closed = gates.path().join("stdout-closed");
    std::fs::write(&flood, "0").unwrap();
    let t = T::new(&[
        ("FAKE_ACP_FLOOD_GATE", flood.to_str().unwrap()),
        ("FAKE_ACP_STDOUT_CLOSED", closed.to_str().unwrap()),
    ]);
    let armed = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let (entered, blocked) = tokio::sync::oneshot::channel();
    let entered = Mutex::new(Some(entered));
    let (release, wait) = std::sync::mpsc::channel();
    let wait = Mutex::new(wait);
    let mut cfg = t.config();
    let arm = armed.clone();
    cfg.storage.free_space = Some(Arc::new(move || {
        if arm.swap(false, Ordering::SeqCst) {
            // Freeze one real journal write, without also starving the RPC reader's runtime.
            // Dropping the release sender on failure always frees the journal again.
            tokio::task::block_in_place(|| {
                let _ = entered.lock().unwrap().take().unwrap().send(());
                let _ = wait.lock().unwrap().recv();
            });
        }
        1 << 40
    }));
    cfg.storage.ingress_stall = Duration::from_secs(1);
    let n = LocalNode::open(cfg).unwrap();
    n.connect(t.sink.clone(), None);
    ready(&n, "s_1").await;
    let mut spec = turn("s_1", "t_1", "a_1", "@flood");
    spec.run.limits.max_output_bytes = 64 << 20;
    n.start_turn(now_cmd("op_t1", spec)).await.unwrap();
    t.until("the prompt", |t| t.sent("session/prompt") == 1)
        .await;
    armed.store(true, Ordering::SeqCst);
    // Let the first chunk reach the journal before releasing enough output to fill ingress.
    // No amount of test-runner delay can let the whole flood finish before the disk is stuck.
    std::fs::write(&flood, "1").unwrap();
    tokio::time::timeout(Duration::from_secs(15), blocked)
        .await
        .expect("journal reached the barrier")
        .unwrap();
    std::fs::write(&flood, "2000").unwrap();
    let (observed, pipe_error) = tokio::sync::oneshot::channel();
    std::thread::spawn(move || {
        let deadline = Instant::now() + Duration::from_secs(15);
        let result = loop {
            if Instant::now() >= deadline {
                break None;
            }
            if let Ok(code) = std::fs::read_to_string(&closed)
                && !code.is_empty()
            {
                break Some(code);
            }
            std::thread::sleep(Duration::from_millis(20));
        };
        // A separate observer also releases the journal if a broken stall gate leaves every
        // runtime worker waiting on it. This deadline is cleanup on failure, not a success signal.
        let _ = release.send(());
        let _ = observed.send(result);
    });
    let pipe_error = pipe_error.await.unwrap();
    if pipe_error.is_none() {
        tokio::time::timeout(Duration::from_secs(15), n.shutdown())
            .await
            .expect("shutdown after releasing the stuck journal");
    }
    assert_eq!(
        pipe_error.expect("stalled ingress closed the reader"),
        "EPIPE"
    );
    t.until("the end", |t| !t.ended().is_empty()).await;
    let end = &t.ended()[0];
    assert_eq!(end.outcome, Outcome::Interrupted, "{end:?}");
    let e = end.error.as_ref().unwrap();
    assert_eq!(e.code, ErrorCode::Busy);
    assert!(
        e.safe_detail
            .contains("took none of the harness's output for 1 s"),
        "{e:?}"
    );
    assert!(t.text().len() < 2000 * 4096);
    n.shutdown().await;
}

/// How many text events a streaming harness costs: chunks at a model's pace (1000 chunks, one
/// every `PERF_DRIP_MS` ms, 10 by default). Run by hand:
/// `cargo test --release -p steve-node --test local drip_cost -- --ignored --nocapture`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore]
async fn drip_cost() {
    let ms: u64 = std::env::var("PERF_DRIP_MS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(10);
    let t = T::new(&[]);
    let n = t.node();
    ready(&n, "s_1").await;
    let mut spec = turn("s_1", "t_1", "a_1", &format!("@drip 1000 {ms}"));
    spec.run.limits.max_output_bytes = 64 << 20;
    let t0 = Instant::now();
    n.start_turn(now_cmd("op_t1", spec)).await.unwrap();
    while t.ended().is_empty() {
        assert!(t0.elapsed() < Duration::from_secs(120));
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let deltas = t
        .sink
        .events()
        .iter()
        .filter(|e| matches!(e.body, EventBody::TextDelta { .. }))
        .count();
    let want: String = (0..1000).map(|i| format!("t{i} ")).collect();
    assert_eq!(t.text(), want);
    println!(
        "1000 chunks, one every {ms} ms: {deltas} text events journaled and sent ({:.1} chunks per event), {} events in all",
        1000.0 / deltas as f64,
        t.sink.events().len()
    );
    n.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn claude_code_is_told_to_deny_the_hubs_secret_files() {
    let t = T::new(&[]);
    let n = t.node();
    ready(&n, "s_1").await;
    n.start_turn(now_cmd("op_t1", turn("s_1", "t_1", "a_1", "@say hello")))
        .await
        .unwrap();
    t.until("the end", |t| !t.ended().is_empty()).await;
    let log = std::fs::read_to_string(&t.log).unwrap();
    let new = log
        .lines()
        .find(|l| l.contains("\"method\":\"session/new\""))
        .unwrap();
    let v: serde_json::Value = serde_json::from_str(new).unwrap();
    let deny: Vec<&str> = v["params"]["_meta"]["claudeCode"]["options"]["settings"]["permissions"]
        ["deny"]
        .as_array()
        .expect("deny rules")
        .iter()
        .filter_map(|r| r.as_str())
        .collect();
    let home = t.root.join("steve");
    for want in [
        format!("Read(/{}/signing/**)", home.display()),
        format!("Read(/{}/secrets/**)", home.display()),
        format!("Read(/{}/steve.env*)", home.display()),
        format!("Read(/{}/backups/**)", home.display()),
    ] {
        assert!(deny.contains(&want.as_str()), "{want} not in {deny:?}");
    }
    // the workspace stays the agent's
    assert!(!deny.iter().any(|r| r.contains("/work")), "{deny:?}");
}

/// A session's harness runs in the OS sandbox (path model S12, first stage): what it reads with
/// its own code (not through the Hub) cannot reach the Hub's keys or the Node's journal, while its
/// workspace stays open.
#[cfg(target_os = "macos")]
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_harness_cannot_read_the_control_plane() {
    let t = T::new(&[]);
    let steve = t.root.join("steve");
    std::fs::create_dir_all(steve.join("signing")).unwrap();
    std::fs::write(steve.join("signing/identity.json"), "k").unwrap();
    let n = t.node();
    if !steve_vault::harness::seatbelt_works() {
        eprintln!("skipped: no seatbelt here (inside a sandbox already?)");
        return;
    }
    assert_eq!(n.confinement(), Ok("seatbelt"));
    ready(&n, "s_1").await;
    let text = format!(
        "@cat {}/signing/identity.json @cat {}/node.sqlite3 @cat {}/work/proj/README",
        steve.display(),
        t.home.display(),
        t.root.display()
    );
    std::fs::write(t.root.join("work/proj/README"), "r").unwrap();
    n.start_turn(now_cmd("op_t1", turn("s_1", "t_1", "a_1", &text)))
        .await
        .unwrap();
    t.until("the end", |t| !t.ended().is_empty()).await;
    assert_eq!(t.text(), "cat:EPERM cat:EPERM cat:ok ", "{:?}", t.ended());
}

/// The principal's own variables on this Node (`node_env`) reach its harness, `PATH_PREPEND` in
/// front of `PATH`; a key Steve sets itself is not theirs to set, whatever the Hub sends.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_users_own_variables_reach_the_harness_and_steves_stay_steves() {
    let t = T::new(&[]);
    let n = t.node();
    n.prepare_workspace(now_cmd("op_prep", prepare_proj()))
        .await
        .unwrap();
    assert!(ok(&finished(&n, "op_prep").await));
    let mut body = open_body("s_1");
    body.env = vec![
        ("NPM_TOKEN".into(), "tok-1".into()),
        ("PATH_PREPEND".into(), "/opt/mine/bin".into()),
        ("HOME".into(), "/elsewhere".into()),
    ];
    n.open_session(now_cmd("op_open_s_1", body)).await.unwrap();
    let s = finished(&n, "op_open_s_1").await;
    assert!(ok(&s), "{s:?}");
    let spawn = std::fs::read_to_string(&t.log)
        .unwrap()
        .lines()
        .find_map(|l| {
            serde_json::from_str::<serde_json::Value>(l).ok()?["spawn"]["env"]
                .as_object()
                .cloned()
        })
        .unwrap();
    assert_eq!(spawn["NPM_TOKEN"], "tok-1");
    assert_eq!(spawn["PATH0"], "/opt/mine/bin");
    assert_ne!(spawn["HOME"], "/elsewhere");
}

/// A principal's own harness directory (path model §4.5.1): the harness gets it through the
/// variable its adapter declares; the Node's owner starts with their own settings, anyone else
/// with nothing of theirs; a path that is not plain is refused.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn each_principal_gets_a_harness_directory_of_their_own() {
    let t = T::new(&[]);
    std::fs::create_dir_all(t.root.join(".claude")).unwrap();
    std::fs::write(t.root.join(".claude/settings.json"), r#"{"owner":true}"#).unwrap();
    let n = t.node();
    n.prepare_workspace(now_cmd("op_prep", prepare_proj()))
        .await
        .unwrap();
    assert!(ok(&finished(&n, "op_prep").await));
    let open = |s: &str, dir: &str, owner: bool| {
        let mut b = open_body(s);
        b.state = Some(HarnessState {
            dir: dir.into(),
            import_owner: owner,
        });
        b
    };
    n.open_session(now_cmd(
        "op_open_s_1",
        open("s_1", "default/owner/dev/claude-code", true),
    ))
    .await
    .unwrap();
    assert!(ok(&finished(&n, "op_open_s_1").await));
    n.open_session(now_cmd(
        "op_open_s_2",
        open("s_2", "default/other/dev/claude-code", false),
    ))
    .await
    .unwrap();
    assert!(ok(&finished(&n, "op_open_s_2").await));
    let dirs: Vec<String> = std::fs::read_to_string(&t.log)
        .unwrap()
        .lines()
        .filter_map(|l| {
            serde_json::from_str::<serde_json::Value>(l).ok()?["spawn"]["env"]["CLAUDE_CONFIG_DIR"]
                .as_str()
                .map(String::from)
        })
        .collect();
    let owner = t.home.join("state/default/owner/dev/claude-code/harness");
    let other = t.home.join("state/default/other/dev/claude-code/harness");
    assert_eq!(
        dirs,
        [owner.display().to_string(), other.display().to_string()]
    );
    assert_eq!(
        std::fs::read_to_string(owner.join("settings.json")).unwrap(),
        r#"{"owner":true}"#
    );
    assert!(
        !other.join("settings.json").exists(),
        "nothing of the owner's"
    );
    // in the sandbox, a session reads its own directory and nobody else's
    if steve_vault::harness::seatbelt_works() && n.confinement().is_ok() {
        std::fs::write(other.join("mine.txt"), "m").unwrap();
        let text = format!(
            "@cat {}/mine.txt @cat {}/settings.json",
            other.display(),
            owner.display()
        );
        n.start_turn(now_cmd("op_t2", turn("s_2", "t_1", "a_1", &text)))
            .await
            .unwrap();
        t.until("the end", |t| !t.ended().is_empty()).await;
        assert_eq!(t.text(), "cat:ok cat:EPERM ");
    }
    n.open_session(now_cmd("op_open_s_3", open("s_3", "../escape", true)))
        .await
        .unwrap();
    assert!(!ok(&finished(&n, "op_open_s_3").await));
    // a link someone left on the way is not followed
    let elsewhere = t.root.join("elsewhere");
    std::fs::create_dir_all(&elsewhere).unwrap();
    std::os::unix::fs::symlink(&elsewhere, t.home.join("state/planted")).unwrap();
    n.open_session(now_cmd(
        "op_open_s_4",
        open("s_4", "planted/x/dev/claude-code", true),
    ))
    .await
    .unwrap();
    assert!(!ok(&finished(&n, "op_open_s_4").await));
    assert_eq!(std::fs::read_dir(&elsewhere).unwrap().count(), 0);
}
