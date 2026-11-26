//! Durable settlement for model and research jobs on the Node, including cancellation,
//! accounting, replay and Hub restart.
#![cfg(unix)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};
use steve_runtime::testkit::{eventually, eventually_sync};
use steve_store::testkit::hub::node_checks;

use serde_json::{Value, json};
use steve_core::SingleModelGateway;
use steve_core::gateway::{ModelGateway, ThinkTools, Tier};
use steve_core::stats::{RecordingGateway, tagged};
use steve_hub::wire::node_files::NodeFiles;
use steve_hub::wire::node_runtime::{NodeDomain, NodePrint, NodeRuntime, Sink};
use steve_node_api::testkit::{FakeNode, build};
use steve_node_api::*;
use steve_store::{FnPublisher, Store};

/// The offline ACP adapter generates persisted usage for a Node job.
const ADAPTER: &str = include_str!("../../steve-node/tests/usage-meter.py");

struct NoDomain;
impl NodeDomain for NoDomain {
    fn turn_ended(&self, _: &str, _: Option<&str>, _: &str, _: Option<String>) {}
}

/// The Node's events go to whichever coordinator is current (a restarted Hub's).
#[derive(Default)]
struct Gate(Mutex<Option<Weak<NodeRuntime>>>, Mutex<Vec<EventBatch>>);

#[async_trait::async_trait]
impl EventSink for Gate {
    async fn commit(&self, batch: EventBatch) -> ApiResult<EventAck> {
        self.1.lock().unwrap().push(batch.clone());
        let to = self.0.lock().unwrap().clone();
        match to {
            Some(w) => Sink(w).commit(batch).await,
            None => Err(Error::new(Code::Unavailable, "no hub")),
        }
    }
}

fn open_store(dir: &Path) -> Store {
    let store = Store::open(dir.join("steve.sqlite3"), FnPublisher(|_| {})).unwrap();
    store.ensure_workspace().unwrap();
    store
}

/// The statistics rows of the gateway's calls, without their ids and times.

/// A Hub with the local Node running think jobs on the stand-in claude, and the in-process
/// gateway on the very same script, side by side.
struct Local {
    _dir: tempfile::TempDir,
    root: PathBuf,
    store: Store,
    rt: Arc<NodeRuntime>,
    node_api: Arc<steve_node::LocalNode>,
    gate: Arc<Gate>,
}

impl Local {
    async fn new() -> Local {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let log = root.join("fake");
        let adapter = root.join("adapter.py");
        std::fs::write(
            &adapter,
            // ACP may prepend system instructions as another text block. The scenario is the
            // user message, for research as well as Think.
            ADAPTER
                .replace("FAKE_LOG", &log.display().to_string())
                .replace(
                    "msg['params']['prompt'][0]['text']",
                    "msg['params']['prompt'][-1]['text']",
                ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&adapter, std::fs::Permissions::from_mode(0o755)).unwrap();
        for d in ["scratch", "data"] {
            std::fs::create_dir_all(root.join(d)).unwrap();
        }
        let store = open_store(&root);
        let node = Arc::new(
            steve_node::LocalNode::open(steve_node::NodeConfig {
                ceiling: steve_node::PolicyCeiling::unlimited(),
                node: NodeId::new("local").unwrap(),
                home: root.join("node"),
                acp: Self::acp(&root),
                roots: vec![],
                capacity: Capacity {
                    sessions: 0,
                    jobs: steve_hub::wire::LOCAL_JOB_CAPACITY,
                    execs: 0,
                },
                storage: steve_node::Storage::default(),
            })
            .unwrap(),
        );
        let mut research = steve_research::ResearchConfig::new(root.join("research"));
        research.home = root.join("home");
        research.files_root = root.join("files");
        research.sandbox = steve_research::SandboxMode::Off;
        node.enable_research(research);
        let rt = NodeRuntime::new(store.clone(), node.clone(), Arc::new(NoDomain));
        let gate = Arc::new(Gate::default());
        *gate.0.lock().unwrap() = Some(Arc::downgrade(&rt));
        node.connect(
            gate.clone(),
            Some(Arc::new(NodeFiles::new(
                store.clone(),
                NodeId::new("local").unwrap(),
                root.join("data"),
            ))),
        );
        rt.start();
        eventually_sync("reconciled", || rt.is_reconciled()).await;
        Local {
            _dir: dir,
            root,
            store,
            rt,
            node_api: node,
            gate,
        }
    }

    /// The adapter as Steve's default one, keeping its own sandbox (so its log reaches the test).
    fn acp(root: &Path) -> steve_acp::AcpConfig {
        let id = steve_harness_spec::DEFAULT_ADAPTER.0.to_string();
        steve_acp::AcpConfig {
            declared: HashMap::from([(
                id.clone(),
                serde_json::from_value(json!({"properties": {
                    "nested_sandbox": false, "self_read_only": true, "usage_fallback":"codex_rollout", "state":{"env":"CODEX_HOME","default":"unused"}}}))
                .unwrap(),
            )]),
            adapters: HashMap::from([(id, root.join("adapter.py"))]),
            node: "node".into(),
            claude_executable: "claude".into(),
                claude_config_dir: None,
            steve_home: root.join("steve"),
            codex_home: root.join("codex-home"),
            user_home: root.to_path_buf(),
            version: "test".into(),
        }
    }

    fn smart(models: &[&str]) -> Vec<String> {
        models.iter().map(|m| m.to_string()).collect()
    }

    /// Steve's gateway over the local Node.
    fn node(&self, models: &[&str]) -> RecordingGateway {
        RecordingGateway::new(
            Arc::new(SingleModelGateway {
                one: NodePrint {
                    runtime: self.rt.clone(),
                    data_dir: self.root.join("data"),
                },
                fast: vec![],
                smart: Self::smart(models),
                models: None,
            }),
            self.store.clone(),
        )
    }
}

fn seed_item(s: &Store) -> String {
    use steve_store::{NewBot, NewItem, WS};
    s.bots()
        .seed(
            "review",
            false,
            &NewBot {
                name: "Review".into(),
                harness: "test".into(),
                ..Default::default()
            },
            WS,
        )
        .unwrap();
    s.items()
        .create(
            WS,
            &NewItem {
                tags: vec![
                    s.tags()
                        .kinds(WS)
                        .unwrap()
                        .into_iter()
                        .find(|tag| tag.name == "feat")
                        .unwrap()
                        .id,
                ],
                title: "Job accounting review".into(),
                goal: "contract".into(),
                bot_id: "review".into(),
                ..Default::default()
            },
        )
        .unwrap()
        .unwrap()
        .id
}
fn snapshot(s: &Store, item: &str) -> Value {
    let (summary, detail) = s
        .read::<_, steve_store::StoreError>(|c| {
            let day = steve_stats::day(steve_store::now(), None);
            let query = steve_stats::metrics::Query {
                workspace_id: "default".into(),
                from: day.clone(),
                to: day,
                ..Default::default()
            };
            let summary = steve_stats::metrics::summary(c, &query)?.unwrap();
            let detail = steve_stats::views::item_detail(c, item, steve_store::now(), false)?
                .unwrap()
                .1;
            Ok((summary, detail))
        })
        .unwrap();
    let (applied, ledger) = steve_store::testkit::hub::node_usage::token_report_totals(s).unwrap();
    json!({"applied_token_usage":applied,"ledger_rows":ledger,"item":detail.usage,"summary":summary.usage})
}
fn save(name: &str, value: Value) {
    println!("REVIEW_JOB {name}={value}");
    if let Ok(out) = std::env::var("ST117_OUTPUTS") {
        std::fs::write(
            Path::new(&out).join(format!("{name}.json")),
            serde_json::to_vec_pretty(&value).unwrap(),
        )
        .unwrap();
    }
}
fn reports(g: &Gate) -> Vec<UsageReport> {
    g.1.lock()
        .unwrap()
        .iter()
        .flat_map(|b| b.events.iter())
        .filter_map(|e| match &e.body {
            EventBody::Usage { report } => Some(report.clone()),
            _ => None,
        })
        .collect()
}
fn exec(s: &Store) -> ExecutionRef {
    let e = serde_json::from_str(&node_checks::latest_runtime_job_attempts_execution(s).unwrap())
        .unwrap();
    assert!(matches!(e, ExecutionRef::Job { .. }));
    e
}
fn spawn_call(
    h: &Local,
    item: &str,
    message: &str,
    timeout: Duration,
) -> tokio::task::JoinHandle<steve_core::gateway::Attempt> {
    let gw = h.node(&["meter-model"]);
    let item = item.to_string();
    let message = message.to_string();
    tokio::spawn(async move {
        tagged(
            "review-job",
            "default",
            steve_core::stats::for_item(
                &item,
                gw.attempt(&message, Tier::Smart, timeout, &ThinkTools::None, None),
            ),
        )
        .await
    })
}
async fn applied_live(h: &Local) {
    eventually_sync("Cumulative tokens applied at Hub", || {
        node_checks::cumulative_gateway_usage_report_count(&h.store).unwrap() > 0
    })
    .await;
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_local_job_live_final_replay() {
    let h = Local::new().await;
    let item = seed_item(&h.store);
    let task = spawn_call(&h, &item, "native", Duration::from_secs(15));
    applied_live(&h).await;
    let e = exec(&h.store);
    let live = snapshot(&h.store, &item);
    let running = node_checks::runtime_job_attempts_state(&h.store).unwrap();
    let result = task.await.unwrap();
    assert!(result.result.is_ok(), "{:?}", result.failures);
    let final_ = snapshot(&h.store, &item);
    let batches = h.gate.1.lock().unwrap().clone();
    for batch in batches {
        Sink(Arc::downgrade(&h.rt)).commit(batch).await.unwrap();
    }
    tokio::time::sleep(Duration::from_millis(150)).await;
    let replay = snapshot(&h.store, &item);
    save(
        "job-live-final",
        json!({"execution":e,"running_state":running,"live":live,"final":final_,"replay":replay,"reports":reports(&h.gate)}),
    );
    h.rt.stop("review done");
    h.node_api.shutdown().await;
    assert_eq!(final_["item"]["total"], 127);
    assert_eq!(replay["summary"]["total"], 127);
    assert_eq!(
        live["item"]["total"], 120,
        "Job running usage must reach item before completion"
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_local_job_future_loss_restart_replay() {
    let h = Local::new().await;
    let item = seed_item(&h.store);
    let task = spawn_call(&h, &item, "hang", Duration::from_secs(30));
    applied_live(&h).await;
    let e = exec(&h.store);
    let live = snapshot(&h.store, &item);
    // The coordinator goes offline while the same real Node retains its journal.
    *h.gate.0.lock().unwrap() = None;
    h.rt.stop("independent review restart");
    task.abort();
    let _ = task.await;
    let rt2 = NodeRuntime::new(h.store.clone(), h.node_api.clone(), Arc::new(NoDomain));
    *h.gate.0.lock().unwrap() = Some(Arc::downgrade(&rt2));
    rt2.start();
    eventually_sync("new Hub reconciled", || rt2.is_reconciled()).await;
    eventually_sync("orphan job ended", || {
        node_checks::runtime_job_attempts_count_by_state(&h.store, "finished").unwrap() == 1
    })
    .await;
    eventually_sync("Final token event applied after restart", || {
        node_checks::final_gateway_usage_report_count(&h.store).unwrap() > 0
    })
    .await;
    let recovered = snapshot(&h.store, &item);
    let batches = h.gate.1.lock().unwrap().clone();
    for batch in batches {
        Sink(Arc::downgrade(&rt2)).commit(batch).await.unwrap();
    }
    tokio::time::sleep(Duration::from_millis(150)).await;
    let replay = snapshot(&h.store, &item);
    save(
        "job-future-loss-restart",
        json!({"execution":e,"live":live,"recovered":recovered,"replay":replay,"reports":reports(&h.gate)}),
    );
    rt2.stop("review done");
    h.node_api.shutdown().await;
    assert_eq!(
        recovered["summary"]["total"], 120,
        "persisted Job usage must survive loss of waiter"
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_job_timeout_late_final() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path());
    let item = seed_item(&store);
    let node = FakeNode::new("n1");
    let rt = NodeRuntime::new(store.clone(), node.clone(), Arc::new(NoDomain));
    node.connect(Arc::new(Sink(Arc::downgrade(&rt))));
    rt.start();
    eventually_sync("ready", || rt.is_reconciled()).await;
    let gw = RecordingGateway::new(
        Arc::new(SingleModelGateway {
            one: NodePrint {
                runtime: rt.clone(),
                data_dir: dir.path().into(),
            },
            fast: vec![],
            smart: vec!["m".into()],
            models: None,
        }),
        store.clone(),
    );
    let iid = item.clone();
    let task = tokio::spawn(async move {
        tagged(
            "review-job",
            "default",
            steve_core::stats::for_item(
                &iid,
                gw.attempt(
                    "test",
                    Tier::Smart,
                    Duration::from_millis(1000),
                    &ThinkTools::None,
                    None,
                ),
            ),
        )
        .await
    });
    let e = wait_for_fake_job_started(&node, &store).await;
    let r = UsageReport {
        report_id: "live".into(),
        meter_epoch: Epoch::new("e").unwrap(),
        model: Some("m".into()),
        scope: UsageScope::Main,
        measurement: Measurement::Cumulative,
        tokens: Tokens {
            input: Some(10),
            output: Some(0),
            cache_read: Some(0),
            cache_write: Some(0),
            reasoning: None,
        },
        context_used: None,
        context_limit: None,
        cost: None,
    };
    node.emit(
        Subject::Execution(e.clone()),
        EventBody::Usage { report: r.clone() },
    )
    .await
    .unwrap();
    let result = task.await.unwrap();
    assert!(result.result.is_err());
    let timeout = snapshot(&store, &item);
    let mut tail = r;
    tail.report_id = "final".into();
    tail.measurement = Measurement::Final;
    tail.tokens.input = Some(20);
    node.emit(
        Subject::Execution(e.clone()),
        EventBody::Usage { report: tail },
    )
    .await
    .unwrap();
    let mut done = build::done();
    done.outcome = Outcome::Cancelled;
    done.stop_reason = "cancelled".into();
    node.finish(&e, done).await.unwrap();
    eventually_sync("ended", || {
        node_checks::runtime_job_attempts_count_by_state(&store, "finished").unwrap() == 1
    })
    .await;
    let final_ = snapshot(&store, &item);
    save(
        "job-timeout-tail",
        json!({"execution":e,"timeout":timeout,"final":final_}),
    );
    rt.stop("review done");
    assert_eq!(timeout["summary"]["total"], 10);
    assert_eq!(
        final_["summary"]["total"], 20,
        "late final must replace timeout partial bill"
    );
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_local_job_disconnect_then_replay_with_waiter() {
    let h = Local::new().await;
    let item = seed_item(&h.store);
    let task = spawn_call(&h, &item, "native", Duration::from_secs(20));
    applied_live(&h).await;
    let e = exec(&h.store);
    *h.gate.0.lock().unwrap() = None;
    let op = OperationId::new(node_checks::runtime_job_attempts_operation_id(&h.store).unwrap())
        .unwrap();
    eventually(
        "The Node finishes the job while the Hub is away.",
        || async {
            h.node_api
                .operation(op.clone())
                .await
                .unwrap()
                .state
                .is_finished()
        },
    )
    .await;
    let disconnected = snapshot(&h.store, &item);
    *h.gate.0.lock().unwrap() = Some(Arc::downgrade(&h.rt));
    h.node_api.connect(h.gate.clone(), None);
    let result = task.await.unwrap();
    assert!(result.result.is_ok(), "{:?}", result.failures);
    let recovered = snapshot(&h.store, &item);
    let batches = h.gate.1.lock().unwrap().clone();
    for batch in batches {
        Sink(Arc::downgrade(&h.rt)).commit(batch).await.unwrap();
    }
    tokio::time::sleep(Duration::from_millis(150)).await;
    let replay = snapshot(&h.store, &item);
    save(
        "job-disconnect-waiter-survives",
        json!({"execution":e,"disconnected":disconnected,"recovered":recovered,"replay":replay}),
    );
    h.rt.stop("review done");
    h.node_api.shutdown().await;
    assert_eq!(recovered["item"]["total"], 127);
    assert_eq!(replay["summary"]["total"], 127);
}

fn fake_report(id: &str, model: &str, n: u64, measurement: Measurement) -> UsageReport {
    UsageReport {
        report_id: id.into(),
        model: Some(model.into()),
        meter_epoch: Epoch::new("meter").unwrap(),
        scope: UsageScope::Main,
        measurement,
        tokens: Tokens {
            input: Some(n),
            output: Some(0),
            cache_read: Some(0),
            cache_write: Some(0),
            reasoning: None,
        },
        cost: None,
        context_used: None,
        context_limit: None,
    }
}
/// Forwards to the Hub and keeps what went through, to send it again.
struct Sent(Sink, Mutex<Vec<NodeEvent>>);

#[async_trait::async_trait]
impl EventSink for Sent {
    async fn commit(&self, batch: EventBatch) -> ApiResult<EventAck> {
        self.1.lock().unwrap().extend(batch.events.iter().cloned());
        self.0.commit(batch).await
    }
}

impl Sent {
    /// The last event sent of `execution`.
    fn last_of(&self, execution: &ExecutionRef) -> NodeEvent {
        self.1
            .lock()
            .unwrap()
            .iter()
            .rev()
            .find(|e| e.subject == Subject::Execution(execution.clone()))
            .unwrap()
            .clone()
    }
}

async fn fake_gateway(
    dir: &Path,
    store: &Store,
    models: Vec<String>,
) -> (Arc<FakeNode>, Arc<NodeRuntime>, RecordingGateway, Arc<Sent>) {
    let node = FakeNode::new("n1");
    let rt = NodeRuntime::new(store.clone(), node.clone(), Arc::new(NoDomain));
    let sent = Arc::new(Sent(Sink(Arc::downgrade(&rt)), Mutex::new(vec![])));
    node.connect(sent.clone());
    rt.start();
    eventually_sync("ready", || rt.is_reconciled()).await;
    let gw = RecordingGateway::new(
        Arc::new(SingleModelGateway {
            one: NodePrint {
                runtime: rt.clone(),
                data_dir: dir.into(),
            },
            fast: vec![],
            smart: models,
            models: None,
        }),
        store.clone(),
    );
    (node, rt, gw, sent)
}
/// Per-call gateway deadline used by fake-Node job tests.
const FAKE_CALL_TIMEOUT: Duration = Duration::from_secs(15);

/// Bound for a failed gateway call to settle once `AttemptEnded` is durably applied.
/// Strictly shorter than [`FAKE_CALL_TIMEOUT`] so an unwanted second job attempt fails fast
/// instead of silently hitting the call deadline, while still giving ample margin on slow CI.
const FAILED_PLAN_SETTLE_TIMEOUT: Duration = Duration::from_secs(10);

/// Waits for `FakeNode` to accept `start_job` AND for the Hub to durably apply and acknowledge
/// its `AttemptStarted` event (`seq = 1`). Polling only `node.log()` races with
/// `FakeNode::finish_accept`'s in-flight commit, allowing a subsequent `node.emit` (`seq = 2`)
/// to reach `NodeRuntime::receive` ahead of `seq = 1` and get dropped by strict in-order stream
/// application (leaving later `AttemptEnded` events unapplied as well).
async fn wait_for_fake_job_started(node: &FakeNode, store: &Store) -> ExecutionRef {
    eventually_sync("job started and acknowledged", || {
        node.log().iter().filter(|(m, _)| *m == "start_job").count() == 1
            && node_checks::runtime_job_attempts_count_by_state(store, "running").unwrap() == 1
            && node.unacked() == 0
    })
    .await;
    exec(store)
}

fn start_fake_call(
    gw: RecordingGateway,
    item: String,
) -> tokio::task::JoinHandle<steve_core::gateway::Attempt> {
    tokio::spawn(async move {
        tagged(
            "test",
            "default",
            steve_core::stats::for_item(
                &item,
                gw.attempt(
                    "test",
                    Tier::Smart,
                    FAKE_CALL_TIMEOUT,
                    &ThinkTools::None,
                    None,
                ),
            ),
        )
        .await
    })
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn dropped_waiter_keeps_late_final_on_the_same_failed_bill() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path());
    let item = seed_item(&store);
    let (node, rt, gw, sent) = fake_gateway(dir.path(), &store, vec!["m".into()]).await;
    let task = start_fake_call(gw, item.clone());
    let e = wait_for_fake_job_started(&node, &store).await;
    node.emit(
        Subject::Execution(e.clone()),
        EventBody::Usage {
            report: fake_report("live", "m", 10, Measurement::Cumulative),
        },
    )
    .await
    .unwrap();
    eventually_sync("live bill", || {
        snapshot(&store, &item)["item"]["total"] == 10
    })
    .await;
    let call = node_checks::usage_records_call_id(&store).unwrap();
    // Hold FakeNode's immediate cancellation receipt until its buffered tail is sent. Real Node
    // journal settlement is asynchronous; do not ask FakeNode.emit to invent a new finished run.
    let allow_cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let ready = allow_cancel.clone();
    node.set_hook(move |p| {
        if matches!(p, steve_node_api::testkit::Point::BeforeAccept("cancel")) {
            let deadline = Instant::now() + Duration::from_secs(2);
            while !ready.load(std::sync::atomic::Ordering::SeqCst) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    });
    task.abort();
    let _ = task.await;
    node.emit(
        Subject::Execution(e.clone()),
        EventBody::Usage {
            report: fake_report("tail", "m", 20, Measurement::Final),
        },
    )
    .await
    .unwrap();
    let mut done = build::done();
    done.outcome = Outcome::Cancelled;
    done.stop_reason = "cancelled".into();
    node.finish(&e, done.clone()).await.unwrap();
    eventually_sync("tail bill", || {
        snapshot(&store, &item)["summary"]["total"] == 20
    })
    .await;
    allow_cancel.store(true, std::sync::atomic::Ordering::SeqCst);
    let replay = EventBatch {
        events: sent.1.lock().unwrap().clone(),
    };
    for _ in 0..2 {
        Sink(Arc::downgrade(&rt))
            .commit(replay.clone())
            .await
            .unwrap();
    }
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert_eq!(node_checks::usage_records_count(&store).unwrap(), 1);
    assert_eq!(node_checks::usage_records_call_id(&store).unwrap(), call);
    assert_eq!(node_checks::usage_records_ok(&store).unwrap(), 0);
    assert_eq!(snapshot(&store, &item)["summary"]["total"], 20);
    rt.stop("done");
}
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn failed_gateway_job_keeps_one_billed_attempt_without_reissuing() {
    for second in ["first", "second"] {
        let dir = tempfile::tempdir().unwrap();
        let store = open_store(dir.path());
        let item = seed_item(&store);
        let (node, rt, gw, _) =
            fake_gateway(dir.path(), &store, vec!["first".into(), second.into()]).await;
        let task = start_fake_call(gw, item.clone());
        let first = wait_for_fake_job_started(&node, &store).await;
        let call = node_checks::gateway_calls_call_id(&store).unwrap();
        node.emit(
            Subject::Execution(first.clone()),
            EventBody::Usage {
                report: fake_report("final", "first", 20, Measurement::Final),
            },
        )
        .await
        .unwrap();
        let mut failed = build::done();
        failed.outcome = Outcome::Failed;
        failed.stop_reason = "error".into();
        failed.error = Some(Error::new(Code::Internal, "503 busy"));
        node.finish(&first, failed).await.unwrap();
        assert_eq!(
            node.unacked(),
            0,
            "Usage and AttemptEnded must be durably applied and acknowledged"
        );
        assert_eq!(
            node_checks::runtime_job_attempts_count_by_state(&store, "finished").unwrap(),
            1,
            "AttemptEnded must finish the job attempt before the caller settles"
        );
        let result = tokio::time::timeout(FAILED_PLAN_SETTLE_TIMEOUT, task)
            .await
            .expect("the failed plan settles without waiting for another job")
            .unwrap();
        assert!(result.result.is_err());
        assert_eq!(result.attempts, 1);
        assert_eq!(
            node.log().iter().filter(|(m, _)| *m == "start_job").count(),
            1
        );
        assert_eq!(result.failures.len(), 1);
        assert_eq!(
            node_checks::gateway_attempts_distinct_call_id_count(&store).unwrap(),
            1
        );
        assert_eq!(
            node_checks::gateway_attempts_count_with_previous_attempt(&store).unwrap(),
            0
        );
        assert_eq!(node_checks::first_usage_call_id(&store).unwrap(), call);
        assert_eq!(snapshot(&store, &item)["summary"]["total"], 20);
        assert_eq!(node_checks::usage_records_ok(&store).unwrap(), 0);
        assert_eq!(
            node_checks::first_usage_records_attempts(&store).unwrap(),
            1
        );
        assert_eq!(node_checks::usage_records_count(&store).unwrap(), 1);
        rt.stop("done");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn research_job_run_keeps_observed_consumption_after_drop() {
    use steve_hub::wire::node_runtime::ResearchAttempt;
    let h = Local::new().await;
    let item = seed_item(&h.store);
    let a = ResearchAttempt {
        run: "research-owner".into(),
        workspace: "default".into(),
        harness: steve_harness_spec::DEFAULT_ADAPTER.0.into(),
        model: "meter-model".into(),
        effort: None,
        prompt: "hang".into(),
        resume: None,
        files: vec![],
        cwd: None,
        readable: vec![],
        budget: ResearchBudget {
            max_turns: Some(32),
            max_tools: Some(32),
            idle_ms: 15000,
        },
        sandbox: SandboxRequirement::None,
        full: false,
        steve_tools: false,
        deadline: steve_store::now() + 20000,
    };
    let job = tagged(
        "research",
        "default",
        steve_core::stats::for_item(&item, h.rt.start_research(a)),
    )
    .await
    .unwrap();
    eventually_sync("research live before caller returns", || {
        let usage = snapshot(&h.store, &item);
        if usage["item"]["total"] == 120 {
            return true;
        }
        if node_checks::runtime_job_attempts_count_by_state(&h.store, "finished").unwrap() > 0 {
            panic!(
                "research ended before live usage: {} completion={} reports={:?}",
                usage,
                node_checks::runtime_job_attempts_coalesce_completion_none(&h.store).unwrap(),
                reports(&h.gate)
            );
        }
        false
    })
    .await;
    let call = node_checks::gateway_calls_call_id(&h.store).unwrap();
    drop(job);
    // The existing research producer drops its ACP future on cancellation, so its last observed
    // cumulative report is the evidence boundary. Do not fabricate a sampler Final or tail count.
    eventually_sync("research cancelled", || {
        node_checks::runtime_job_attempts_count_by_state(&h.store, "finished").unwrap() == 1
    })
    .await;
    assert_eq!(snapshot(&h.store, &item)["summary"]["total"], 120);
    assert_eq!(node_checks::usage_records_call_id(&h.store).unwrap(), call);
    assert_eq!(node_checks::usage_records_ok(&h.store).unwrap(), 0);
    assert!(matches!(exec(&h.store), ExecutionRef::Job { .. }));
    save(
        "research-drop-observed",
        json!({"kind":"actual LocalNode research protocol fixture","call_id":call,"bill":snapshot(&h.store,&item),"reports":reports(&h.gate),"completion":node_checks::runtime_job_attempts_coalesce_completion_none(&h.store).unwrap()}),
    );
    h.rt.stop("done");
    h.node_api.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rolled_back_completion_does_not_wake_core_or_settle_the_call() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path());
    let item = seed_item(&store);
    let (node, rt, gw, sent) = fake_gateway(dir.path(), &store, vec!["m".into()]).await;
    let task = start_fake_call(gw, item);
    let execution = wait_for_fake_job_started(&node, &store).await;
    node.emit(
        Subject::Execution(execution.clone()),
        EventBody::Usage {
            report: fake_report("live", "m", 10, Measurement::Cumulative),
        },
    )
    .await
    .unwrap();
    eventually_sync("live applied", || {
        node_checks::gateway_usage_reports_count(&store).unwrap() == 1
    })
    .await;
    let mut end = sent.last_of(&execution);
    end.id = EventId::new("rollback-end").unwrap();
    end.cursor.seq += 1;
    end.body = EventBody::AttemptEnded {
        completion: build::done(),
    };
    steve_store::testkit::hub::node_usage::refuse_command_apply(&store).unwrap();
    assert!(
        Sink(Arc::downgrade(&rt))
            .commit(EventBatch {
                events: vec![end.clone()],
            })
            .await
            .is_err()
    );
    tokio::time::sleep(Duration::from_millis(120)).await;
    assert!(
        !task.is_finished(),
        "a rolled-back completion must not escape to the caller"
    );
    assert_eq!(
        node_checks::gateway_calls_count_with_ended(&store).unwrap(),
        0
    );
    assert_eq!(
        node_checks::runtime_job_attempts_count_by_state(&store, "finished").unwrap(),
        0
    );
    steve_store::testkit::hub::hub_fixtures::remove_fail_commit(&store).unwrap();
    Sink(Arc::downgrade(&rt))
        .commit(EventBatch { events: vec![end] })
        .await
        .unwrap();
    assert!(task.await.unwrap().result.is_ok());
    assert_eq!(
        node_checks::gateway_calls_count_with_ended(&store).unwrap(),
        1
    );
    assert_eq!(node_checks::usage_records_sum_input(&store).unwrap(), 10);
    rt.stop("done");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn research_job_run_dropped_waiter_applies_late_final_to_the_same_failed_call() {
    use steve_hub::wire::node_runtime::ResearchAttempt;
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path());
    let item = seed_item(&store);
    let (node, rt, _gw, _) = fake_gateway(dir.path(), &store, vec![]).await;
    let a = ResearchAttempt {
        run: "research-tail".into(),
        workspace: "default".into(),
        harness: steve_harness_spec::DEFAULT_ADAPTER.0.into(),
        model: "m".into(),
        effort: None,
        prompt: "controlled research".into(),
        resume: None,
        files: vec![],
        cwd: None,
        readable: vec![],
        budget: ResearchBudget {
            max_turns: Some(32),
            max_tools: Some(32),
            idle_ms: 15000,
        },
        sandbox: SandboxRequirement::None,
        full: false,
        steve_tools: false,
        deadline: steve_store::now() + 20000,
    };
    let job = tagged(
        "research",
        "default",
        steve_core::stats::for_item(&item, rt.start_research(a)),
    )
    .await
    .unwrap();
    let execution = wait_for_fake_job_started(&node, &store).await;
    node.emit(
        Subject::Execution(execution.clone()),
        EventBody::Usage {
            report: fake_report("live", "m", 10, Measurement::Cumulative),
        },
    )
    .await
    .unwrap();
    eventually_sync("research live10", || {
        snapshot(&store, &item)["item"]["total"] == 10
    })
    .await;
    let call = node_checks::gateway_calls_call_id(&store).unwrap();
    let allow_cancel = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let ready = allow_cancel.clone();
    node.set_hook(move |p| {
        if matches!(p, steve_node_api::testkit::Point::BeforeAccept("cancel")) {
            let deadline = Instant::now() + Duration::from_secs(2);
            while !ready.load(std::sync::atomic::Ordering::SeqCst) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    });
    drop(job);
    eventually_sync("research result cancelled", || {
        node_checks::gateway_calls_count_with_ended(&store).unwrap() == 1
    })
    .await;
    node.emit(
        Subject::Execution(execution.clone()),
        EventBody::Usage {
            report: fake_report("tail", "m", 20, Measurement::Final),
        },
    )
    .await
    .unwrap();
    let mut end = build::done();
    end.outcome = Outcome::Cancelled;
    end.stop_reason = "cancelled".into();
    node.finish(&execution, end).await.unwrap();
    allow_cancel.store(true, std::sync::atomic::Ordering::SeqCst);
    eventually_sync("research final20", || {
        snapshot(&store, &item)["summary"]["total"] == 20
    })
    .await;
    assert_eq!(node_checks::usage_records_count(&store).unwrap(), 1);
    assert_eq!(node_checks::usage_records_ok(&store).unwrap(), 0);
    assert_eq!(node_checks::usage_records_call_id(&store).unwrap(), call);
    save(
        "research-late-final",
        json!({"kind":"controlled Node protocol with real ResearchJobRun","call_id":call,"bill":snapshot(&store,&item),"ok":node_checks::usage_records_ok(&store).unwrap()}),
    );
    rt.stop("done");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn retained_final_repairs_the_same_bill_after_runtime_job_pruning() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path());
    let item = seed_item(&store);
    let (node, rt, gw, sent) = fake_gateway(dir.path(), &store, vec!["m".into()]).await;
    let task = start_fake_call(gw, item.clone());
    let execution = wait_for_fake_job_started(&node, &store).await;
    node.emit(
        Subject::Execution(execution.clone()),
        EventBody::Usage {
            report: fake_report("live", "m", 10, Measurement::Cumulative),
        },
    )
    .await
    .unwrap();
    eventually_sync("live10", || snapshot(&store, &item)["item"]["total"] == 10).await;
    let call = node_checks::gateway_calls_call_id(&store).unwrap();
    task.abort();
    let _ = task.await;
    // Dropping the caller schedules cancellation asynchronously. A finished attempt is
    // not prunable while its cancel command can still reach the Node.
    eventually_sync("cancel receipt applied", || {
        steve_store::testkit::hub::node_usage::has_cancelled_job(
            &store,
            execution.attempt().as_str(),
        )
        .unwrap()
    })
    .await;
    let mut done = build::done();
    done.outcome = Outcome::Cancelled;
    done.stop_reason = "cancelled".into();
    node.finish(&execution, done).await.unwrap();
    eventually_sync("end applied", || {
        node_checks::runtime_job_attempts_count_by_state(&store, "finished").unwrap() > 0
    })
    .await;
    let mut final_ = sent.last_of(&execution);
    final_.id = EventId::new("retained-final").unwrap();
    final_.cursor.seq += 1;
    final_.body = EventBody::Usage {
        report: fake_report("final", "m", 20, Measurement::Final),
    };
    assert_eq!(
        store
            .retention_slice(
                steve_store::retention::Kind::Jobs,
                steve_store::now() + 8 * 86_400_000,
                steve_store::retention::DEFAULT_BUDGET,
                None,
            )
            .unwrap()
            .affected,
        1
    );
    assert_eq!(node_checks::runtime_job_attempts_count(&store).unwrap(), 0);
    assert!(
        store
            .stats()
            .gateway_job_owner(execution.attempt().as_str())
            .unwrap()
            .is_some()
    );
    let batch = EventBatch {
        events: vec![final_],
    };
    for _ in 0..2 {
        Sink(Arc::downgrade(&rt))
            .commit(batch.clone())
            .await
            .unwrap();
    }
    eventually_sync("retained Final20", || {
        snapshot(&store, &item)["summary"]["total"] == 20
    })
    .await;
    assert_eq!(node_checks::usage_records_count(&store).unwrap(), 1);
    assert_eq!(node_checks::usage_records_ok(&store).unwrap(), 0);
    assert_eq!(node_checks::usage_records_call_id(&store).unwrap(), call);
    save(
        "job-pruned-recovery",
        json!({"call_id":call,"bill":snapshot(&store,&item),"kind":"retained protocol Final after actual runtime Job pruning"}),
    );
    rt.stop("done");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_research_cancel_must_collect_persisted_tail() {
    use steve_hub::wire::node_runtime::ResearchAttempt;
    let h = Local::new().await;
    let item = seed_item(&h.store);
    let a = ResearchAttempt {
        run: "research-owner".into(),
        workspace: "default".into(),
        harness: steve_harness_spec::DEFAULT_ADAPTER.0.into(),
        model: "meter-model".into(),
        effort: None,
        prompt: "hang".into(),
        resume: None,
        files: vec![],
        cwd: None,
        readable: vec![],
        budget: ResearchBudget {
            max_turns: Some(32),
            max_tools: Some(32),
            idle_ms: 15000,
        },
        sandbox: SandboxRequirement::None,
        full: false,
        steve_tools: false,
        deadline: steve_store::now() + 20000,
    };
    let job = tagged(
        "research",
        "default",
        steve_core::stats::for_item(&item, h.rt.start_research(a)),
    )
    .await
    .unwrap();
    eventually_sync("research live before caller returns", || {
        let usage = snapshot(&h.store, &item);
        if usage["item"]["total"] == 120 {
            return true;
        }
        if node_checks::runtime_job_attempts_count_by_state(&h.store, "finished").unwrap() > 0 {
            panic!(
                "research ended before live usage: {} completion={} reports={:?}",
                usage,
                node_checks::runtime_job_attempts_coalesce_completion_none(&h.store).unwrap(),
                reports(&h.gate)
            );
        }
        false
    })
    .await;
    let call = node_checks::gateway_calls_call_id(&h.store).unwrap();
    // Write a new source count before cancellation, after live120 was observed.
    // This is provider-shaped persisted usage, not a bill/UsageReport injected into the Hub.
    let folder = h.root.join("codex-home/sessions/2026/01/01");
    let path = std::fs::read_dir(&folder)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let before = std::fs::read_to_string(&path).unwrap();
    let mut tail: Value = serde_json::from_str(before.lines().last().unwrap()).unwrap();
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
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    writeln!(file, "{}", tail).unwrap();
    file.sync_all().unwrap();
    drop(job);
    // The existing research producer drops its ACP future on cancellation, so its last observed
    // cumulative report is the evidence boundary. Do not fabricate a sampler Final or tail count.
    eventually_sync("research cancelled", || {
        node_checks::runtime_job_attempts_count_by_state(&h.store, "finished").unwrap() == 1
    })
    .await;
    tokio::time::sleep(Duration::from_millis(3500)).await;
    assert_eq!(node_checks::usage_records_call_id(&h.store).unwrap(), call);
    assert_eq!(node_checks::usage_records_ok(&h.store).unwrap(), 0);
    assert!(matches!(exec(&h.store), ExecutionRef::Job { .. }));
    assert_eq!(node_checks::usage_records_count(&h.store).unwrap(), 1);
    let finals: Vec<_> = reports(&h.gate)
        .into_iter()
        .filter(|r| r.measurement == Measurement::Final && r.tokens.input.is_some())
        .collect();
    assert_eq!(finals.len(), 1, "producer emits one token Final");
    assert_eq!(
        finals[0].tokens,
        Tokens {
            input: Some(70),
            cache_read: Some(80),
            output: Some(30),
            reasoning: Some(7),
            cache_write: None
        }
    );
    let batches = h.gate.1.lock().unwrap().clone();
    for batch in batches {
        Sink(Arc::downgrade(&h.rt)).commit(batch).await.unwrap();
    }
    assert_eq!(snapshot(&h.store, &item)["summary"]["total"], 180);
    assert_eq!(
        node_checks::usage_records_count(&h.store).unwrap(),
        1,
        "journal replay updates the original call once"
    );
    assert_eq!(node_checks::usage_records_ok(&h.store).unwrap(), 0);
    save(
        "independent-research-tail",
        json!({"kind":"actual LocalNode research protocol fixture","call_id":call,"bill":snapshot(&h.store,&item),"reports":reports(&h.gate),"source_before_cancel":before,"persisted_tail":tail,"expected_total":180,"completion":node_checks::runtime_job_attempts_coalesce_completion_none(&h.store).unwrap()}),
    );
    let actual = snapshot(&h.store, &item)["summary"]["total"].clone();
    h.rt.stop("done");
    h.node_api.shutdown().await;
    assert_eq!(
        actual, 180,
        "research cancellation must collect already-persisted +60 tail"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_research_persisted_tail_is_readable_before_cancel() {
    use steve_hub::wire::node_runtime::ResearchAttempt;
    let h = Local::new().await;
    let item = seed_item(&h.store);
    let a = ResearchAttempt {
        run: "research-owner".into(),
        workspace: "default".into(),
        harness: steve_harness_spec::DEFAULT_ADAPTER.0.into(),
        model: "meter-model".into(),
        effort: None,
        prompt: "hang".into(),
        resume: None,
        files: vec![],
        cwd: None,
        readable: vec![],
        budget: ResearchBudget {
            max_turns: Some(32),
            max_tools: Some(32),
            idle_ms: 15000,
        },
        sandbox: SandboxRequirement::None,
        full: false,
        steve_tools: false,
        deadline: steve_store::now() + 20000,
    };
    let job = tagged(
        "research",
        "default",
        steve_core::stats::for_item(&item, h.rt.start_research(a)),
    )
    .await
    .unwrap();
    eventually_sync("research live before caller returns", || {
        let usage = snapshot(&h.store, &item);
        if usage["item"]["total"] == 120 {
            return true;
        }
        if node_checks::runtime_job_attempts_count_by_state(&h.store, "finished").unwrap() > 0 {
            panic!(
                "research ended before live usage: {} completion={} reports={:?}",
                usage,
                node_checks::runtime_job_attempts_coalesce_completion_none(&h.store).unwrap(),
                reports(&h.gate)
            );
        }
        false
    })
    .await;
    let call = node_checks::gateway_calls_call_id(&h.store).unwrap();
    // Write a new source count before cancellation, after live120 was observed.
    // This is provider-shaped persisted usage, not a bill/UsageReport injected into the Hub.
    let folder = h.root.join("codex-home/sessions/2026/01/01");
    let path = std::fs::read_dir(&folder)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let before = std::fs::read_to_string(&path).unwrap();
    let mut tail: Value = serde_json::from_str(before.lines().last().unwrap()).unwrap();
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
    use std::io::Write;
    let mut file = std::fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .unwrap();
    writeln!(file, "{}", tail).unwrap();
    file.sync_all().unwrap();
    eventually_sync(
        "tail is readable by the unmodified Research sampler",
        || snapshot(&h.store, &item)["summary"]["total"] == 180,
    )
    .await;
    drop(job);
    // The existing research producer drops its ACP future on cancellation, so its last observed
    // cumulative report is the evidence boundary. Do not fabricate a sampler Final or tail count.
    eventually_sync("research cancelled", || {
        node_checks::runtime_job_attempts_count_by_state(&h.store, "finished").unwrap() == 1
    })
    .await;
    tokio::time::sleep(Duration::from_millis(3500)).await;
    assert_eq!(node_checks::usage_records_call_id(&h.store).unwrap(), call);
    assert_eq!(node_checks::usage_records_ok(&h.store).unwrap(), 0);
    assert!(matches!(exec(&h.store), ExecutionRef::Job { .. }));
    save(
        "independent-research-tail-control",
        json!({"kind":"actual LocalNode research protocol fixture","call_id":call,"bill":snapshot(&h.store,&item),"reports":reports(&h.gate),"source_before_cancel":before,"persisted_tail":tail,"expected_total":180,"completion":node_checks::runtime_job_attempts_coalesce_completion_none(&h.store).unwrap()}),
    );
    let actual = snapshot(&h.store, &item)["summary"]["total"].clone();
    h.rt.stop("done");
    h.node_api.shutdown().await;
    assert_eq!(
        actual, 180,
        "research cancellation must collect already-persisted +60 tail"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn idle_hub_worker_recovers_healthy_result_behind_128_failed_calls_without_new_usage() {
    use steve_store::gateway_ledger::CallBegin;
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path());
    for i in 0..129 {
        let id = format!("result-{i:03}");
        store
            .stats()
            .gateway_begin(&CallBegin {
                id: id.clone(),
                workspace: "default".into(),
                purpose: "test".into(),
                started: 1_793_491_200_000,
                day: "2026-11-01".into(),
                ..Default::default()
            })
            .unwrap();
        store
            .stats()
            .gateway_attempt(&id, &id, "fixture", "m")
            .unwrap();
        store
            .stats()
            .gateway_report(
                &id,
                &fake_report("live", "m", 10, Measurement::Cumulative),
                1_793_491_200_001,
            )
            .unwrap();
    }
    steve_store::testkit::hub::mutations::create_fail_results(&store).unwrap();
    for i in 0..129 {
        assert!(
            store
                .stats()
                .gateway_cancel(&format!("result-{i:03}"), "cancelled")
                .is_err()
        );
    }
    // Simulate restart with the fault still present: startup must admit the periodic worker.
    assert_eq!(store.stats().gateway_recover().unwrap(), 0);
    steve_store::testkit::hub::mutations::remove_fail_results(&store).unwrap();
    let node = FakeNode::new("n1");
    let rt = NodeRuntime::new(store.clone(), node.clone(), Arc::new(NoDomain));
    node.connect(Arc::new(Sink(Arc::downgrade(&rt))));
    rt.start();
    eventually_sync("healthy result on idle worker", || {
        node_checks::gateway_calls_projection_dirty_by_call(&store, "result-128").unwrap() == 0
    })
    .await;
    assert_eq!(
        node_checks::usage_records_ok_by_live_call(&store, "result-128").unwrap(),
        0
    );
    assert_eq!(
        node_checks::usage_records_pending_result_128(&store).unwrap(),
        0
    );
    assert_eq!(node_checks::usage_records_sum_input(&store).unwrap(), 1290);
    assert_eq!(node_checks::usage_records_count(&store).unwrap(), 129);
    assert_eq!(
        node_checks::gateway_result_ready_count(&store).unwrap(),
        128
    );
    assert!(node.log().iter().all(|(method, _)| *method != "start_job"));
    rt.stop("result recovery verified");
}
