//! Usage at the real LocalNode/process/journal boundary, independent of any model backend.
#![cfg(unix)]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use steve_runtime::testkit::{eventually, eventually_sync, on_timeout};

use serde_json::json;
use steve_acp::AcpConfig;
use steve_acp::usage::testkit::{PollHold, hold_polls};
use steve_node::{LocalNode, NodeConfig, PolicyCeiling, Root, Storage};
use steve_node_api::testkit::{RecordingSink, build};
use steve_node_api::*;

struct Fixture {
    dir: tempfile::TempDir,
    node: LocalNode,
    sink: Arc<RecordingSink>,
}

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
fn choice() -> HarnessChoice {
    HarnessChoice {
        harness: "meter".into(),
        model: None,
        effort: None,
    }
}
/// The output budget of a stop on output. Every byte the harness writes counts, its keep-alive
/// heartbeats too (one per 50 ms while it waits for the test's control file: at most 180 kB within
/// the 60 s deadline of [`limits`]). The budget is above all of that, so only the message the meter
/// sends after the control file reaches it, however late the sampler's first poll comes.
const OUTPUT_BUDGET: u64 = 256 << 10;
/// The longest a test waits for something it observes (a live report, an attempt's end). It only
/// bounds a failure: a loaded runner, not the code under test, decides how long these take.
/// How far ahead of its start a deadline stop is set: time for the meter to start and report live
/// usage before it (the test then persists its tail under a poll hold, not against this clock).
const DEADLINE_AFTER_MS: i64 = 5_000;
/// The bound on a stop and its settlement, from the moment its condition is armed.
const STOP_WAIT: Duration = Duration::from_secs(8);

fn limits() -> Limits {
    Limits {
        deadline: Timestamp(now() + 60_000),
        offline_until: Timestamp(now() + 60_000),
        ..build::limits()
    }
}

impl Fixture {
    fn new() -> Self {
        Self::for_format(false)
    }
    fn claude() -> Self {
        Self::for_format(true)
    }
    fn for_format(transcript: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        std::fs::create_dir_all(root.join("work/proj")).unwrap();
        let script = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(if transcript {
            "tests/transcript-meter.py"
        } else {
            "tests/usage-meter.py"
        });
        let adapter = root.join("adapter.sh");
        std::fs::write(
            &adapter,
            format!("#!/bin/sh\nexec python3 '{}'\n", script.display()),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&adapter, std::fs::Permissions::from_mode(0o700)).unwrap();
        let sink = Arc::new(RecordingSink::default());
        let cfg=NodeConfig {
            node:NodeId::new("node").unwrap(),home:root.join("node"),ceiling:PolicyCeiling::unlimited(),
            acp:AcpConfig {
                adapters:HashMap::from([("meter".into(),adapter)]),
                declared:HashMap::from([("meter".into(),serde_json::from_value(json!({"properties":{
                    "nested_sandbox":false,"self_read_only":true,"usage_fallback":if transcript { "claude_transcript" } else { "codex_rollout" },
                    "state":{"env":if transcript { "CLAUDE_CONFIG_DIR" } else { "CODEX_HOME" },"default":"unused"}
                }})).unwrap())]),
                node:"node".into(),claude_executable:"unused".into(),
                claude_config_dir:transcript.then(|| root.join("configured-home")),
                steve_home:root.join("unused"),
                codex_home:root.join("configured-home"),user_home:root.into(),version:"test".into(),
            },
            roots:vec![Root { id:RootId::new("work").unwrap(),path:root.join("work"),display:"work".into(),access:RootAccess::ReadWrite }],
            capacity:Capacity { sessions:4,jobs:4,execs:0 },storage:Storage::default(),
        };
        let node = LocalNode::open(cfg).unwrap();
        node.connect(sink.clone(), None);
        Self { dir, node, sink }
    }
    async fn finished(&self, op: &str) -> Observed {
        let observed = std::sync::Mutex::new(None);
        eventually(
            on_timeout(|| async {
                let state = self
                    .node
                    .operation(OperationId::new(op).unwrap())
                    .await
                    .map(|r| r.state);
                format!("operation and terminal event finished: op={op}, state={state:?}")
            }),
            || async {
                let state = self
                    .node
                    .operation(OperationId::new(op).unwrap())
                    .await
                    .unwrap()
                    .state;
                if state.is_finished()
                    && (op.starts_with("prep-")
                        || op.starts_with("open-")
                        || self.sink.events().iter().any(|e| {
                            matches!(&e.subject,Subject::Execution(x) if x.attempt().as_str()==op)
                                && matches!(e.body, EventBody::AttemptEnded { .. })
                        }))
                {
                    *observed.lock().unwrap() = Some(state);
                    return true;
                }
                false
            },
        )
        .await;
        observed
            .into_inner()
            .unwrap()
            .expect("terminal state snapshot")
    }
    fn workspace(sid: &str) -> WorkspaceRef {
        build::workspace("node", &format!("item:{sid}"))
    }
    async fn session(&self, sid: &str, own_home: bool) {
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
                    choice: choice(),
                    policy: build::policy(),
                    resume: None,
                    env: vec![],
                    state: own_home.then(|| HarnessState {
                        dir: format!("principals/{sid}"),
                        import_owner: false,
                        owner_context: true,
                    }),
                    artifact_view: None,
                    skills: vec![],
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
            "{state:?}"
        );
    }
    async fn turn(&self, sid: &str, attempt: &str, scenario: &str) {
        self.node
            .start_turn(command(
                attempt,
                TurnSpec {
                    session: SessionId::new(sid).unwrap(),
                    turn: TurnId::new(attempt).unwrap(),
                    run: RunSpec {
                        attempt: AttemptId::new(attempt).unwrap(),
                        choice: choice(),
                        directory: RunDirectory::Workspace {
                            workspace: Self::workspace(sid),
                        },
                        prompt: vec![Part::Text {
                            text: scenario.into(),
                        }],
                        policy: build::policy(),
                        limits: limits(),
                        continuation: None,
                    },
                },
            ))
            .await
            .unwrap();
    }
    async fn job(&self, attempt: &str, scenario: &str) {
        let mut spec = build::job(attempt, attempt, scenario);
        spec.run.choice = choice();
        spec.run.limits = limits();
        self.node.start_job(command(attempt, spec)).await.unwrap();
    }
    fn reports(&self, attempt: &str) -> Vec<UsageReport> {
        self.sink
            .events()
            .into_iter()
            .filter(|e| matches!(&e.subject,Subject::Execution(x) if x.attempt().as_str()==attempt))
            .filter_map(|e| match e.body {
                EventBody::Usage { report } => Some(report),
                _ => None,
            })
            .collect()
    }
    async fn live(&self, attempt: &str) -> UsageReport {
        let report = std::sync::Mutex::new(None);
        eventually_sync(
            on_timeout(|| async {
                format!(
                    "no live usage: {attempt}; reports={:?}",
                    self.reports(attempt)
                )
            }),
            || {
                if let Some(r) = self
                    .reports(attempt)
                    .into_iter()
                    .find(|r| r.measurement == Measurement::Cumulative && r.tokens.input.is_some())
                {
                    *report.lock().unwrap() = Some(r);
                    return true;
                }
                false
            },
        )
        .await;
        report.into_inner().unwrap().expect("live usage snapshot")
    }
    fn final_report(&self, attempt: &str) -> UsageReport {
        self.reports(attempt)
            .into_iter()
            .find(|r| r.measurement == Measurement::Final && r.scope == UsageScope::Main)
            .expect("final report")
    }
}

fn expected(i: u64, c: u64, o: u64, r: u64) -> Tokens {
    Tokens {
        input: Some(i),
        cache_read: Some(c),
        output: Some(o),
        reasoning: Some(r),
        cache_write: None,
    }
}

#[tokio::test]
async fn regular_live_final_and_next_attempt_never_use_session_lifetime() {
    let f = Fixture::new();
    f.session("s", false).await;
    f.turn("s", "a1", "live").await;
    let live = f.live("a1").await;
    assert_eq!(live.tokens, expected(40, 60, 20, 5));
    assert!(
        !f.node
            .operation(OperationId::new("a1").unwrap())
            .await
            .unwrap()
            .state
            .is_finished()
    );
    f.finished("a1").await;
    assert_eq!(f.final_report("a1").tokens, live.tokens);
    f.turn("s", "a2", "live").await;
    f.finished("a2").await;
    assert_eq!(f.final_report("a2").tokens, live.tokens);
    assert_ne!(
        f.final_report("a1").report_id,
        f.final_report("a2").report_id
    );
    assert!(
        f.reports("a1")
            .iter()
            .any(|r| r.context_used == Some(999999) && r.tokens == Tokens::default())
    );
    assert!(!f.reports("a1").iter().any(|r| r.context_used == Some(777)));
    let context_ids: std::collections::HashSet<_> = f
        .reports("a1")
        .into_iter()
        .filter(|r| r.context_used == Some(999999))
        .map(|r| r.report_id)
        .collect();
    assert_eq!(
        context_ids.len(),
        1,
        "replayed native updates retain their report identity"
    );
    f.node.shutdown().await;
}

#[tokio::test]
async fn native_final_uses_exclusive_cache_and_both_reasoning_field_spellings() {
    let f = Fixture::new();
    f.session("s", false).await;
    f.turn("s", "a", "native").await;
    let live = f.live("a").await;
    f.finished("a").await;
    let mut t = expected(40, 60, 20, 5);
    t.cache_write = Some(7);
    assert_eq!(f.final_report("a").tokens, t);
    assert_eq!(live.meter_epoch, f.final_report("a").meter_epoch);
    assert_eq!(
        f.reports("a")
            .iter()
            .find(|r| r.scope == UsageScope::All)
            .unwrap()
            .tokens,
        t
    );
    f.job("j", "native").await;
    let job_live = f.live("j").await;
    f.finished("j").await;
    assert_eq!(f.final_report("j").tokens, t);
    assert_eq!(job_live.meter_epoch, f.final_report("j").meter_epoch);
    for attempt in ["a", "j"] {
        let reports = f.reports(attempt);
        let mut by_epoch = std::collections::BTreeMap::new();
        for r in reports.iter().filter(|r| r.tokens != Tokens::default()) {
            by_epoch.insert(
                r.meter_epoch.as_str(),
                r.tokens.input.unwrap_or(0)
                    + r.tokens.output.unwrap_or(0)
                    + r.tokens.cache_read.unwrap_or(0)
                    + r.tokens.cache_write.unwrap_or(0),
            );
        }
        assert_eq!(
            by_epoch.values().sum::<u64>(),
            127,
            "live + Main/All Final must settle once"
        );
    }
    assert!(
        !f.reports("a")
            .iter()
            .chain(f.reports("j").iter())
            .any(|r| r.context_used == Some(777))
    );
    f.node.shutdown().await;
}

#[tokio::test]
async fn regular_error_and_late_success_settle_the_async_tail() {
    let f = Fixture::new();
    f.session("s", false).await;
    for (a, scenario) in [("a1", "error"), ("a2", "late")] {
        f.turn("s", a, scenario).await;
        let state = f.finished(a).await;
        assert!(
            matches!(state,Observed::Finished{outcome,..} if outcome==if scenario=="error"{Outcome::Failed}else{Outcome::Succeeded})
        );
        assert_eq!(f.final_report(a).tokens, expected(70, 80, 30, 7));
    }
    f.node.shutdown().await;
}

#[tokio::test]
async fn regular_cancel_includes_tail_before_freeing_the_session() {
    let f = Fixture::new();
    f.session("s", false).await;
    f.turn("s", "a", "cancel").await;
    f.live("a").await;
    f.node
        .cancel(command(
            "cancel",
            Cancel {
                target: OperationId::new("a").unwrap(),
                reason: "test".into(),
            },
        ))
        .await
        .unwrap();
    assert!(matches!(
        f.finished("a").await,
        Observed::Finished {
            outcome: Outcome::Cancelled,
            ..
        }
    ));
    assert_eq!(f.final_report("a").tokens, expected(70, 80, 30, 7));
    f.node.shutdown().await;
}

#[tokio::test]
async fn oneshot_live_cancel_error_and_success_are_durable_without_returning_usage() {
    let f = Fixture::new();
    f.job("cancelled", "hang").await;
    assert_eq!(f.live("cancelled").await.tokens, expected(40, 60, 20, 5));
    f.node
        .cancel(command(
            "cancel",
            Cancel {
                target: OperationId::new("cancelled").unwrap(),
                reason: "test".into(),
            },
        ))
        .await
        .unwrap();
    assert!(matches!(
        f.finished("cancelled").await,
        Observed::Finished {
            outcome: Outcome::Cancelled,
            ..
        }
    ));
    assert_eq!(f.final_report("cancelled").tokens, expected(40, 60, 20, 5));
    for (a, scenario) in [("error", "error"), ("success", "late")] {
        f.job(a, scenario).await;
        f.finished(a).await;
        assert_eq!(f.final_report(a).tokens, expected(70, 80, 30, 7));
    }
    f.node.shutdown().await;
}

#[tokio::test]
async fn concurrent_regular_and_oneshot_principal_home_reset_missing_and_zero() {
    let f = Fixture::new();
    f.session("s1", true).await;
    f.session("s2", false).await;
    f.turn("s1", "a1", "reset").await;
    f.turn("s2", "a2", "live").await;
    f.job("j", "live").await;
    f.finished("a1").await;
    f.finished("a2").await;
    f.finished("j").await;
    assert_eq!(f.final_report("a1").tokens, expected(90, 90, 35, 8));
    assert_eq!(f.final_report("a2").tokens, expected(40, 60, 20, 5));
    assert_eq!(f.final_report("j").tokens, expected(40, 60, 20, 5));
    let own = f
        .dir
        .path()
        .join("node/state/principals/s1/harness/sessions");
    assert!(own.is_dir(), "{}", own.display());
    for (a, scenario) in [("missing", "missing"), ("zero", "zero")] {
        f.turn("s1", a, scenario).await;
        f.finished(a).await;
        assert_eq!(
            f.final_report(a).tokens,
            if scenario == "missing" {
                Tokens::default()
            } else {
                expected(0, 0, 0, 0)
            }
        );
    }
    f.node.shutdown().await;
}

#[tokio::test]
async fn reconnect_replays_the_same_auditable_reports_without_new_token_bookings() {
    let f = Fixture::new();
    f.session("s", false).await;
    let cursor = f
        .sink
        .events()
        .into_iter()
        .find(|e| matches!(&e.subject,Subject::Session{session} if session.as_str()=="s"))
        .unwrap()
        .cursor;
    f.sink.set_failing(true);
    f.turn("s", "a", "live").await;
    eventually(
        on_timeout(|| async {
            let state = f
                .node
                .operation(OperationId::new("a").unwrap())
                .await
                .map(|r| r.state);
            format!("offline-sink operation finished: op=a state={state:?}")
        }),
        || async {
            if f.node
                .operation(OperationId::new("a").unwrap())
                .await
                .unwrap()
                .state
                .is_finished()
            {
                return true;
            }
            false
        },
    )
    .await;
    let from = Watermark {
        seq: cursor.seq,
        ..cursor
    };
    let a = f.node.replay(from.clone(), 64 * 1024).await.unwrap();
    let b = f.node.replay(from, 64 * 1024).await.unwrap();
    assert_eq!(a, b);
    let end = a
        .events
        .iter()
        .find_map(|e| match &e.body {
            EventBody::Usage { report } if report.measurement == Measurement::Final => {
                Some(report.clone())
            }
            _ => None,
        })
        .unwrap();
    assert_eq!(end.tokens, expected(40, 60, 20, 5));
    f.sink.set_failing(false);
    let ack = f.sink.commit(a.clone()).await.unwrap();
    let count = f.sink.events().len();
    assert_eq!(f.sink.commit(b).await.unwrap(), ack);
    assert_eq!(f.sink.events().len(), count);
    f.node.acknowledge(ack).await.unwrap();
    f.node.connect(f.sink.clone(), None);
    f.finished("a").await;
    assert_eq!(f.final_report("a").report_id, end.report_id);
    f.node.shutdown().await;
}

#[tokio::test]
async fn oneshot_final_streaming_write_failure_is_retried_with_the_terminal_state() {
    let f = Fixture::new();
    let db = rusqlite::Connection::open(f.dir.path().join("node/node.sqlite3")).unwrap();
    db.execute_batch("CREATE TRIGGER reject_final BEFORE INSERT ON events WHEN json_extract(NEW.event,'$.body.type')='usage' AND json_extract(NEW.event,'$.body.report.measurement')='final' BEGIN SELECT RAISE(ABORT,'injected final write failure'); END;").unwrap();
    f.job("j", "native").await;
    tokio::time::sleep(Duration::from_millis(3500)).await;
    assert!(
        !f.node
            .operation(OperationId::new("j").unwrap())
            .await
            .unwrap()
            .state
            .is_finished()
    );
    db.execute_batch("DROP TRIGGER reject_final;").unwrap();
    f.finished("j").await;
    assert_eq!(f.final_report("j").tokens.input, Some(40));
    f.node.shutdown().await;
}

fn claude_expected(tail: bool) -> Tokens {
    Tokens {
        input: Some(if tail { 8 } else { 6 }),
        cache_read: Some(23457),
        cache_write: Some(30544),
        output: Some(if tail { 1119 } else { 1112 }),
        reasoning: Some(if tail { 26 } else { 19 }),
    }
}

#[tokio::test]
async fn claude_regular_and_oneshot_live_native_final_and_synthetic_zero() {
    let f = Fixture::claude();
    f.session("s", false).await;
    for (attempt, scenario) in [("native", "native"), ("synthetic", "zero-final")] {
        f.turn("s", attempt, scenario).await;
        let live = f.live(attempt).await;
        assert_eq!(live.tokens, claude_expected(false));
        assert_eq!(live.model.as_deref(), Some("source-model"));
        f.finished(attempt).await;
        for r in f
            .reports(attempt)
            .iter()
            .filter(|r| r.measurement == Measurement::Final)
        {
            assert_eq!(r.tokens, claude_expected(false));
            assert_eq!(r.meter_epoch, live.meter_epoch);
        }
        let job = format!("job-{attempt}");
        f.job(&job, scenario).await;
        let live = f.live(&job).await;
        f.finished(&job).await;
        assert_eq!(live.tokens, claude_expected(false));
        for r in f
            .reports(&job)
            .iter()
            .filter(|r| r.measurement == Measurement::Final)
        {
            assert_eq!(r.tokens, claude_expected(false));
            assert_eq!(r.meter_epoch, live.meter_epoch);
        }
    }
    f.node.shutdown().await;
}

#[tokio::test]
async fn claude_regular_and_oneshot_error_cancel_settle_late_distinct_messages() {
    let f = Fixture::claude();
    f.session("s", false).await;
    for (attempt, scenario) in [("err", "error"), ("cancelled", "cancel")] {
        f.turn("s", attempt, scenario).await;
        if scenario == "cancel" {
            f.live(attempt).await;
            f.node
                .cancel(command(
                    "cancel-turn",
                    Cancel {
                        target: OperationId::new(attempt).unwrap(),
                        reason: "test".into(),
                    },
                ))
                .await
                .unwrap();
        }
        let state = f.finished(attempt).await;
        assert!(
            matches!(state,Observed::Finished{outcome,..} if outcome==if scenario=="error"{Outcome::Failed}else{Outcome::Cancelled})
        );
        assert_eq!(f.final_report(attempt).tokens, claude_expected(true));
        let job = format!("job-{attempt}");
        f.job(&job, scenario).await;
        if scenario == "cancel" {
            f.live(&job).await;
            f.node
                .cancel(command(
                    "cancel-job",
                    Cancel {
                        target: OperationId::new(&job).unwrap(),
                        reason: "test".into(),
                    },
                ))
                .await
                .unwrap();
        }
        f.finished(&job).await;
        assert_eq!(
            f.final_report(&job).tokens,
            if scenario == "error" {
                claude_expected(true)
            } else {
                claude_expected(false)
            },
            "oneshot cancellation kills its producer before a future tail can be flushed"
        );
    }
    f.node.shutdown().await;
}

#[tokio::test]
async fn claude_concurrent_principals_models_children_reconnect_and_next_attempt() {
    let f = Fixture::claude();
    f.session("s1", true).await;
    f.session("s2", false).await;
    f.turn("s1", "a1", "children").await;
    f.turn("s2", "a2", "models").await;
    f.job("j", "live").await;
    f.live("a1").await;
    f.live("a2").await;
    f.live("j").await;
    f.sink.set_failing(true);
    for a in ["a1", "a2", "j"] {
        eventually(
            on_timeout(|| async {
                let state = f
                    .node
                    .operation(OperationId::new(a).unwrap())
                    .await
                    .map(|r| r.state);
                format!("concurrent operation finished: op={a}, state={state:?}")
            }),
            || async {
                if f.node
                    .operation(OperationId::new(a).unwrap())
                    .await
                    .unwrap()
                    .state
                    .is_finished()
                {
                    return true;
                }
                false
            },
        )
        .await;
    }
    let mut batch = EventBatch::default();
    for head in f.node.streams().await.unwrap() {
        let from = Watermark {
            stream: head.stream,
            epoch: head.epoch,
            seq: head.acknowledged,
        };
        let replay = f.node.replay(from.clone(), 1024 * 1024).await.unwrap();
        assert_eq!(replay, f.node.replay(from, 1024 * 1024).await.unwrap());
        batch.events.extend(replay.events);
    }
    f.sink.set_failing(false);
    let ack = f.sink.commit(batch.clone()).await.unwrap();
    let count = f.sink.events().len();
    assert_eq!(f.sink.commit(batch).await.unwrap(), ack);
    assert_eq!(f.sink.events().len(), count);
    f.node.acknowledge(ack).await.unwrap();
    f.node.connect(f.sink.clone(), None);
    for a in ["a1", "a2", "j"] {
        f.finished(a).await;
    }
    let all: Vec<_> = f
        .reports("a1")
        .into_iter()
        .filter(|r| r.measurement == Measurement::Final && r.scope == UsageScope::All)
        .collect();
    assert_eq!(all.len(), 1);
    assert_eq!(all[0].tokens.input, Some(8));
    assert_eq!(all[0].tokens.output, Some(1119));
    let models: std::collections::BTreeSet<_> = f
        .reports("a2")
        .into_iter()
        .filter(|r| r.measurement == Measurement::Final && r.scope == UsageScope::All)
        .filter_map(|r| r.model)
        .collect();
    assert_eq!(
        models,
        std::collections::BTreeSet::from(["source-model".into(), "other-model".into()])
    );
    assert_eq!(f.final_report("j").tokens, claude_expected(false));
    assert!(
        f.dir
            .path()
            .join("node/state/principals/s1/harness/projects")
            .is_dir()
    );
    f.turn("s1", "next", "live").await;
    f.finished("next").await;
    assert_eq!(f.final_report("next").tokens, claude_expected(false));
    for (a, scenario) in [("missing", "missing"), ("zero", "zero")] {
        f.turn("s1", a, scenario).await;
        f.finished(a).await;
        let t = f.final_report(a).tokens;
        assert_eq!(
            t,
            if scenario == "missing" {
                Tokens::default()
            } else {
                Tokens {
                    input: Some(0),
                    output: Some(0),
                    cache_read: Some(0),
                    cache_write: Some(0),
                    reasoning: Some(0),
                }
            }
        );
    }
    f.node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_joins_sampler_and_releases_home_without_three_second_task_tail() {
    let f = Fixture::claude();
    f.session("s", false).await;
    f.turn("s", "hang", "hang").await;
    f.live("hang").await;
    let metrics = tokio::runtime::Handle::current().metrics();
    let start = Instant::now();
    f.node.shutdown().await;
    let elapsed = start.elapsed();
    drop(f.node);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(
        elapsed < Duration::from_secs(2),
        "shutdown took {elapsed:?}"
    );
    assert_eq!(
        metrics.num_alive_tasks(),
        0,
        "shutdown must join sampler instead of detaching a settling task"
    );
    use std::os::fd::AsRawFd;
    let file = std::fs::OpenOptions::new()
        .write(true)
        .open(f.dir.path().join("node/node.lock"))
        .unwrap();
    assert_eq!(
        unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) },
        0
    );
}

// Persist source bytes independently of the ACP events. In particular the timestamp/session id
// and +60 expectation stay unchanged, matching the independent review's already-durable tail.
fn persist_tail(path: &std::path::Path) {
    use std::io::Write;
    let text = std::fs::read_to_string(path).unwrap();
    let mut tail: serde_json::Value = serde_json::from_str(text.lines().last().unwrap()).unwrap();
    let info = &mut tail["payload"]["info"];
    for (field, delta) in [
        ("input_tokens", 50_u64),
        ("cached_input_tokens", 20),
        ("output_tokens", 10),
        ("reasoning_output_tokens", 2),
    ] {
        let old = info["total_token_usage"][field].as_u64().unwrap();
        info["total_token_usage"][field] = json!(old + delta);
        info["last_token_usage"][field] = json!(delta);
    }
    let mut file = std::fs::OpenOptions::new().append(true).open(path).unwrap();
    writeln!(file, "{tail}").unwrap();
    file.sync_all().unwrap();
}

/// Hands the live meter its stop action. The meter acts as soon as the control file exists, so
/// the file appears only complete (written aside, then renamed): a plain write could let it read
/// the file between its creation and its content, act on nothing, and leave the stop to the idle
/// budget or the deadline.
fn arm(source: &std::path::Path, action: &str) {
    let aside = source.with_extension("arming");
    std::fs::write(&aside, action).unwrap();
    std::fs::rename(&aside, source.with_extension("control")).unwrap();
}

impl Fixture {
    fn sources(&self) -> Vec<std::path::PathBuf> {
        std::fs::read_dir(self.dir.path().join("configured-home/sessions/2026/01/01"))
            .into_iter()
            .flatten()
            .map(|e| e.unwrap().path())
            .filter(|p| p.extension().is_some_and(|s| s == "jsonl"))
            .collect()
    }
    async fn stop_job(&self, attempt: &str, research: bool, action: &str) -> std::path::PathBuf {
        let before = self.sources();
        let mut spec = build::job(
            attempt,
            attempt,
            if action == "stubborn" {
                "stop-stubborn"
            } else {
                "stop-controlled"
            },
        );
        spec.run.choice = choice();
        spec.run.limits = limits();
        if action == "output" {
            spec.run.limits.max_output_bytes = OUTPUT_BUDGET;
        }
        if action == "deadline" {
            spec.run.limits.deadline = Timestamp(now() + DEADLINE_AFTER_MS);
        }
        if research {
            spec.run.policy.permissions = PermissionMode::Plan;
            spec.run.policy.sandbox = SandboxRequirement::None;
            let mut cfg = steve_research::ResearchConfig::new(self.dir.path().join("research"));
            cfg.home = self.dir.path().join("research-home");
            cfg.files_root = self.dir.path().join("files");
            cfg.sandbox = steve_research::SandboxMode::Off;
            self.node.enable_research(cfg);
            spec.work = JobWork::Research(ResearchJob {
                cwd: None,
                readable: vec![],
                budget: ResearchBudget {
                    max_turns: Some(if action == "turns" { 1 } else { 32 }),
                    max_tools: Some(if action == "tools" { 1 } else { 32 }),
                    idle_ms: if action == "idle" { 1250 } else { 15000 },
                },
                steve_tools: false,
                earlier: vec![],
            });
        }
        self.node.start_job(command(attempt, spec)).await.unwrap();
        assert_eq!(self.live(attempt).await.tokens, expected(40, 60, 20, 5));
        self.sources()
            .into_iter()
            .find(|p| !before.contains(p))
            .expect("new bound source")
    }
    /// Persists the attempt's tail where no live poll can read it: polls are held (and any poll
    /// in flight has returned) before the bytes are written, and stay held while the guard lives.
    /// Only the final settlement can collect the tail, however the runner schedules the
    /// sampler's ticks around the stop.
    async fn persist_unpolled_tail(&self, attempt: &str, source: &std::path::Path) -> PollHold {
        let hold = hold_polls(attempt).await;
        persist_tail(source);
        hold
    }
    async fn stop_cancel(&self, attempt: &str) {
        self.node
            .cancel(command(
                &format!("cancel-{attempt}"),
                Cancel {
                    target: OperationId::new(attempt).unwrap(),
                    reason: "persisted-tail".into(),
                },
            ))
            .await
            .unwrap();
    }
    fn final_once_before_end(&self, attempt: &str, expected_tokens: Tokens) {
        let reports: Vec<_> = self
            .reports(attempt)
            .into_iter()
            .filter(|r| r.measurement == Measurement::Final && r.tokens.input.is_some())
            .collect();
        assert_eq!(reports.len(), 1, "one Final per meter");
        assert_eq!(reports[0].tokens, expected_tokens);
        if expected_tokens.input == Some(70) {
            assert!(!self.reports(attempt).iter().any(|r| r.measurement == Measurement::Cumulative && r.tokens.input == Some(70)), "tail must be collected by finish, not a normal poll: {attempt}");
        }
        let live = self
            .reports(attempt)
            .into_iter()
            .find(|r| r.measurement == Measurement::Cumulative && r.tokens.input.is_some())
            .unwrap();
        assert_eq!(reports[0].meter_epoch, live.meter_epoch);
        let events: Vec<_> = self
            .sink
            .events()
            .into_iter()
            .filter(
                |e| matches!(&e.subject, Subject::Execution(x) if x.attempt().as_str() == attempt),
            )
            .collect();
        let final_at = events.iter().position(|e| matches!(&e.body, EventBody::Usage { report } if report.measurement == Measurement::Final && report.tokens.input.is_some())).unwrap();
        let end_at = events
            .iter()
            .position(|e| matches!(&e.body, EventBody::AttemptEnded { .. }))
            .unwrap();
        assert!(
            final_at < end_at,
            "Final must precede terminal acknowledgement"
        );
    }
    async fn process_gone(&self, source: &std::path::Path) {
        let name = source.file_stem().unwrap().to_str().unwrap();
        let pid: i32 = name.rsplit('-').next().unwrap().parse().unwrap();
        assert_eq!(
            unsafe { libc::kill(pid, 0) },
            -1,
            "only this fixture's process must be gone"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn think_and_research_cancel_persisted_unpolled_tails_concurrently_and_isolate_next_attempt()
{
    let f = Fixture::new();
    let think = f.stop_job("think-tail", false, "cancel").await;
    let research = f.stop_job("research-tail", true, "cancel").await;
    // Persist both tails only now, under poll holds, then cancel both: no normal sampler tick
    // can read either source write.
    let _think_hold = f.persist_unpolled_tail("think-tail", &think).await;
    let _research_hold = f.persist_unpolled_tail("research-tail", &research).await;
    let started = Instant::now();
    tokio::join!(f.stop_cancel("think-tail"), f.stop_cancel("research-tail"));
    for attempt in ["think-tail", "research-tail"] {
        assert!(matches!(
            f.finished(attempt).await,
            Observed::Finished {
                outcome: Outcome::Cancelled,
                ..
            }
        ));
        f.final_once_before_end(attempt, expected(70, 80, 30, 7));
    }
    assert!(
        started.elapsed() < Duration::from_secs(7),
        "bounded process stop plus settlement"
    );
    f.process_gone(&think).await;
    f.process_gone(&research).await;
    // Fresh attempts cannot inherit either source's lifetime or final report identity.
    for research in [false, true] {
        let attempt = if research {
            "research-next"
        } else {
            "think-next"
        };
        let source = f.stop_job(attempt, research, "cancel").await;
        f.stop_cancel(attempt).await;
        f.finished(attempt).await;
        f.final_once_before_end(attempt, expected(40, 60, 20, 5));
        f.process_gone(&source).await;
    }
    f.node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn research_stop_budgets_and_policy_settle_persisted_tail_without_new_tool_work() {
    let f = Fixture::new();
    for action in ["idle", "output", "tools", "turns", "policy", "deadline"] {
        let attempt = format!("research-{action}");
        let source = f.stop_job(&attempt, true, action).await;
        let _hold = f.persist_unpolled_tail(&attempt, &source).await;
        // For `idle` the meter's heartbeats end only now, after the tail is persisted, so the
        // idle budget expires after it too.
        arm(&source, action);
        let started = Instant::now();
        let state = f.finished(&attempt).await;
        assert!(
            !matches!(
                state,
                Observed::Finished {
                    outcome: Outcome::Succeeded,
                    ..
                }
            ),
            "{action}: {state:?}"
        );
        f.final_once_before_end(&attempt, expected(70, 80, 30, 7));
        // A deadline stop is armed at its deadline, at most DEADLINE_AFTER_MS ahead.
        let armed = if action == "deadline" {
            Duration::from_millis(DEADLINE_AFTER_MS as u64)
        } else {
            Duration::ZERO
        };
        assert!(started.elapsed() < STOP_WAIT + armed, "{action}");
        f.process_gone(&source).await;
        // Permission denial cannot execute the request or authorize a later call.
        assert!(!f.sink.events().iter().any(|e| matches!(&e.subject, Subject::Execution(x) if x.attempt().as_str() == attempt)
            && matches!(&e.body, EventBody::ToolChanged { tool, .. } if tool.phase == ToolPhase::Succeeded)));
    }
    f.node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn think_budget_and_missing_raw_source_keep_bound_final_and_cleanup() {
    let f = Fixture::new();
    for (research, action) in [
        (false, "output"),
        (false, "deadline"),
        (false, "missing"),
        (true, "missing"),
    ] {
        let attempt = format!("{}-{action}", if research { "research" } else { "think" });
        let source = f.stop_job(&attempt, research, action).await;
        let _hold = f.persist_unpolled_tail(&attempt, &source).await;
        if action == "missing" {
            // Raw source becomes unavailable before its next poll: preserve live evidence,
            // diagnose unknown tail; do not fabricate the known but now unreadable +60.
            std::fs::remove_file(&source).unwrap();
            f.stop_cancel(&attempt).await;
        } else {
            arm(&source, action);
        }
        assert!(!matches!(
            f.finished(&attempt).await,
            Observed::Finished {
                outcome: Outcome::Succeeded,
                ..
            }
        ));
        f.final_once_before_end(
            &attempt,
            if action == "missing" {
                expected(40, 60, 20, 5)
            } else {
                expected(70, 80, 30, 7)
            },
        );
        if action == "missing" {
            assert!(f.sink.events().iter().any(
                |e| matches!(&e.subject, Subject::Execution(x) if x.attempt().as_str() == attempt)
                    && matches!(&e.body, EventBody::Diagnostic { .. })
            ));
        }
        f.process_gone(&source).await;
    }
    f.node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn cancellation_kills_only_owned_term_resistant_groups_before_final_and_never_launches_next_work()
 {
    let f = Fixture::new();
    for research in [false, true] {
        let attempt = if research {
            "research-stubborn"
        } else {
            "think-stubborn"
        };
        let source = f.stop_job(attempt, research, "stubborn").await;
        let child: i32 = std::fs::read_to_string(source.with_extension("child"))
            .unwrap()
            .parse()
            .unwrap();
        let _hold = f.persist_unpolled_tail(attempt, &source).await;
        let started = Instant::now();
        f.stop_cancel(attempt).await;
        f.finished(attempt).await;
        assert!(started.elapsed() < Duration::from_secs(7));
        f.final_once_before_end(attempt, expected(70, 80, 30, 7));
        f.process_gone(&source).await;
        // macOS/Linux init may reap the reparented child shortly after the leader; wait only
        // on the exact PID started by this fixture. No process name or global cleanup is used.
        let running = || {
            if unsafe { libc::kill(child, 0) } != 0 {
                return false;
            }
            // A reparented Linux zombie cannot execute or be signalled further; init owns
            // its final reap. macOS normally reaps it before this observation.
            if cfg!(target_os = "linux") {
                if let Ok(stat) = std::fs::read_to_string(format!("/proc/{child}/stat")) {
                    if stat
                        .rsplit_once(')')
                        .is_some_and(|(_, tail)| tail.trim_start().starts_with('Z'))
                    {
                        return false;
                    }
                }
            }
            true
        };
        eventually_sync(format!("owned child still alive: {child}"), || {
            if !running() {
                return true;
            }
            false
        })
        .await;
    }
    f.node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn research_cancellation_cannot_ack_end_before_failed_final_write_is_retried() {
    let f = Fixture::new();
    let source = f.stop_job("research-retry", true, "cancel").await;
    let db = rusqlite::Connection::open(f.dir.path().join("node/node.sqlite3")).unwrap();
    db.execute_batch("CREATE TRIGGER reject_final BEFORE INSERT ON events WHEN json_extract(NEW.event,'$.body.type')='usage' AND json_extract(NEW.event,'$.body.report.measurement')='final' BEGIN SELECT RAISE(ABORT,'injected final write failure'); END;").unwrap();
    let _hold = f.persist_unpolled_tail("research-retry", &source).await;
    f.stop_cancel("research-retry").await;
    tokio::time::sleep(Duration::from_millis(3800)).await;
    assert!(
        !f.node
            .operation(OperationId::new("research-retry").unwrap())
            .await
            .unwrap()
            .state
            .is_finished()
    );
    assert!(!f.sink.events().iter().any(
        |e| matches!(&e.subject, Subject::Execution(x) if x.attempt().as_str() == "research-retry")
            && matches!(e.body, EventBody::AttemptEnded { .. })
    ));
    db.execute_batch("DROP TRIGGER reject_final;").unwrap();
    assert!(matches!(
        f.finished("research-retry").await,
        Observed::Finished {
            outcome: Outcome::Cancelled,
            ..
        }
    ));
    f.final_once_before_end("research-retry", expected(70, 80, 30, 7));
    f.process_gone(&source).await;
    f.node.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_joins_deadline_kill_timer_and_rpc_continuations() {
    let f = Fixture::new();
    f.session("shutdown-timer", false).await;
    let mut lim = limits();
    lim.deadline = Timestamp(now() + 1300);
    f.node
        .start_turn(command(
            "deadline-timer",
            TurnSpec {
                session: SessionId::new("shutdown-timer").unwrap(),
                turn: TurnId::new("deadline-timer").unwrap(),
                run: RunSpec {
                    attempt: AttemptId::new("deadline-timer").unwrap(),
                    choice: choice(),
                    directory: RunDirectory::Workspace {
                        workspace: Fixture::workspace("shutdown-timer"),
                    },
                    prompt: vec![Part::Text {
                        text: "hang".into(),
                    }],
                    policy: build::policy(),
                    limits: lim,
                    continuation: None,
                },
            },
        ))
        .await
        .unwrap();
    f.live("deadline-timer").await;
    let source = f.sources().into_iter().next().unwrap();
    eventually_sync(
        on_timeout(|| async {
            format!(
                "adapter acknowledged deadline cancellation: {}",
                source.with_extension("cancelled").display()
            )
        }),
        || {
            if source.with_extension("cancelled").exists() {
                return true;
            }
            false
        },
    )
    .await;
    // The adapter saw the deadline's cancel, so the actor's ten-second kill timer is pending.
    // Shutdown must join it now, alongside the sampler and RPC continuation, not wait ten seconds.
    let metrics = tokio::runtime::Handle::current().metrics();
    let start = Instant::now();
    f.node.shutdown().await;
    assert!(start.elapsed() < Duration::from_secs(2));
    drop(f.node);
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(
        metrics.num_alive_tasks(),
        0,
        "actor-owned timers/RPC tasks must be joined"
    );
}
