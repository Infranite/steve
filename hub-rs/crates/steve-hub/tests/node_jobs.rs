//! Steve's think calls on the local Node: streamed drafts and durable accounting, cancellation
//! that reaps the harness, and Hub restarts that reconcile jobs without replaying them.
#![cfg(unix)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};
use steve_runtime::testkit::eventually_sync;
use steve_store::testkit::hub::node_checks;

use serde_json::{Value, json};
use steve_core::SingleModelGateway;
use steve_core::gateway::{ModelGateway, Readable, ThinkTools, Tier, with_images};
use steve_core::stats::{RecordingGateway, tagged};
use steve_hub::wire::node_files::NodeFiles;
use steve_hub::wire::node_runtime::{NodeDomain, NodePrint, NodeRuntime, Sink};
use steve_node_api::testkit::{FakeNode, build};
use steve_node_api::*;
use steve_store::{FnPublisher, Store, StoreError};

/// The stand-in ACP adapter for a Node job. One line
/// per process to `<log>.spawns`, `session/new` to `<log>.new`, the prompt to `<log>.prompt`.
/// Model `m-busy` is a 503 (its cost reported); `@hang` writes its pid to `<log>.pid` and waits;
/// `@read` reads the attachment (an ACP read) and answers with it; `@slow` answers after 3 s;
/// otherwise two messages, the second the answer.
const ADAPTER: &str = r#"#!/usr/bin/env python3
import glob, json, os, sys, time
log = "FAKE_LOG"
def put(what, text, mode="w"):
    with open(log + "." + what, mode) as f: f.write(text)
put("spawns", "%d\n" % os.getpid(), "a")
def send(v):
    print(json.dumps(v, ensure_ascii=False), flush=True)
def update(sid, u):
    send({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": sid, "update": u}})
def chunk(sid, text, mid):
    update(sid, {"sessionUpdate": "agent_message_chunk", "messageId": mid, "content": {"type": "text", "text": text}})
state = {"model": "default"}
def options():
    return [{"id": "model", "category": "model", "type": "select", "currentValue": state["model"], "options": []}]
for line in sys.stdin:
    msg = json.loads(line)
    m, p = msg.get("method"), msg.get("params") or {}
    result = {}
    if m == "initialize":
        result = {"protocolVersion": 1, "agentCapabilities": {"promptCapabilities": {"image": True},
            "sessionCapabilities": {"systemPrompt": {"mode": {"override": {}, "append": {}}}}}}
    elif m == "session/new":
        put("new", json.dumps(p, ensure_ascii=False))
        result = {"sessionId": "s1", "modes": {"currentModeId": "default", "availableModes": [{"id": "plan"}]}, "configOptions": options()}
    elif m == "session/set_config_option":
        state[p["configId"]] = p["value"]
        result = {"configOptions": options()}
    elif m == "session/prompt":
        put("prompt", json.dumps(p["prompt"], ensure_ascii=False))
        sid = p["sessionId"]
        text = " ".join(b.get("text", "") for b in p["prompt"])
        if "@slow" in text:
            time.sleep(3)
            chunk(sid, "慢答", "a")
        elif "@hang" in text:
            put("pid", str(os.getpid()))
            chunk(sid, "写到一半", "a")
            time.sleep(60)
        elif state["model"] == "m-busy":
            update(sid, {"sessionUpdate": "usage_update", "used": 4, "size": 100, "cost": {"amount": 0.01, "currency": "USD"}})
            send({"jsonrpc": "2.0", "id": msg["id"], "error": {"code": -32603, "message": "API Error: 503 overloaded"}})
            continue
        elif "@read" in text:
            path = os.path.abspath(glob.glob(".steve/attachments/*-notes.md")[0])
            update(sid, {"sessionUpdate": "tool_call", "toolCallId": "t1", "kind": "read", "title": "Read",
                         "status": "pending", "locations": [{"path": path}], "rawInput": {"file_path": path}})
            body = open(path).read()
            update(sid, {"sessionUpdate": "tool_call_update", "toolCallId": "t1", "status": "completed"})
            chunk(sid, body, "a")
        else:
            chunk(sid, "先看", "a"); chunk(sid, "一下", "a"); chunk(sid, "答案", "b")
            update(sid, {"sessionUpdate": "usage_update", "used": 10, "size": 100, "cost": {"amount": 0.25, "currency": "USD"}})
            result = {"stopReason": "end_turn", "usage": {"inputTokens": 3, "outputTokens": 2, "cachedReadTokens": 5}}
        result = result or {"stopReason": "end_turn"}
    if "id" in msg:
        send({"jsonrpc": "2.0", "id": msg["id"], "result": result})
"#;

struct NoDomain;
impl NodeDomain for NoDomain {
    fn turn_ended(&self, _: &str, _: Option<&str>, _: &str, _: Option<String>) {}
}

/// The Node's events go to whichever coordinator is current (a restarted Hub's).
#[derive(Default)]
struct Gate(Mutex<Option<Weak<NodeRuntime>>>);

#[async_trait::async_trait]
impl EventSink for Gate {
    async fn commit(&self, batch: EventBatch) -> ApiResult<EventAck> {
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

/// A Hub with the local Node running think jobs on the stand-in adapter.
struct Local {
    _dir: tempfile::TempDir,
    root: PathBuf,
    log: PathBuf,
    store: Store,
    rt: Arc<NodeRuntime>,
}

impl Local {
    async fn new() -> Local {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let log = root.join("fake");
        let adapter = root.join("adapter.py");
        std::fs::write(
            &adapter,
            ADAPTER.replace("FAKE_LOG", &log.display().to_string()),
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
        let rt = NodeRuntime::new(store.clone(), node.clone(), Arc::new(NoDomain));
        node.connect(
            Arc::new(Sink(Arc::downgrade(&rt))),
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
            log,
            store,
            rt,
        }
    }

    /// The adapter as Steve's default one, keeping its own sandbox (so its log reaches the test).
    fn acp(root: &Path) -> steve_acp::AcpConfig {
        let id = steve_harness_spec::DEFAULT_ADAPTER.0.to_string();
        steve_acp::AcpConfig {
            declared: HashMap::from([(
                id.clone(),
                serde_json::from_value(json!({"properties": {
                    "nested_sandbox": false, "self_read_only": true, "probe_initialize": false}}))
                .unwrap(),
            )]),
            adapters: HashMap::from([(id, root.join("adapter.py"))]),
            node: "node".into(),
            claude_executable: "claude".into(),
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

    fn read(&self, what: &str) -> String {
        std::fs::read_to_string(format!("{}.{what}", self.log.display())).unwrap_or_default()
    }

    fn spawns(&self) -> usize {
        self.read("spawns").lines().count()
    }
}

fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks that the process exists
    unsafe { libc::kill(pid, 0) == 0 }
}

/// A background model request must not start a harness behind a persisted upgrade drain.
/// Reopening admission releases that same request exactly once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_calls_wait_for_maintenance_before_starting() {
    let h = Local::new().await;
    {
        let gw = h.node(&["m1"]);
        let spawns = h.spawns();
        let jobs: i64 = node_checks::job_count(&h.store).unwrap();
        h.store.maintenance_set(true).unwrap();
        let call = gw.call(
            "维护后再回答",
            Tier::Smart,
            Duration::from_secs(10),
            &ThinkTools::None,
        );
        tokio::pin!(call);
        assert!(
            tokio::time::timeout(Duration::from_millis(200), &mut call)
                .await
                .is_err(),
            "new model work waits for the drain to reopen"
        );
        assert_eq!(h.spawns(), spawns, "no harness starts behind the drain");
        assert_eq!(
            node_checks::job_count(&h.store).unwrap(),
            jobs,
            "no Node job is queued behind the drain"
        );
        assert!(h.store.maintenance_seal(), "waiting calls hold no permit");
        h.store.maintenance_set(false).unwrap();
        let out = tokio::time::timeout(Duration::from_secs(15), call)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(out.text, "答案");
        assert_eq!(h.spawns(), spawns + 1, "the waiting call runs once");
        assert!(h.store.activities().is_empty());
    }
}

/// A conversation admitted before maintenance may finish its nested model call.
/// The drain still waits for that conversation's permit before sealing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admitted_model_calls_finish_during_maintenance() {
    let h = Local::new().await;
    let gw = h.node(&["m1", "m-busy"]);
    let permit = h.store.try_activity("thread_answer").unwrap();
    h.store.maintenance_set(true).unwrap();
    let out = tokio::time::timeout(
        Duration::from_secs(15),
        h.store.admitted(gw.attempt(
            "继续已开始的回答",
            Tier::Smart,
            Duration::from_secs(10),
            &ThinkTools::None,
            None,
        )),
    )
    .await
    .unwrap();
    assert_eq!(out.result.unwrap().text, "答案");
    assert_eq!(out.attempts, 1);
    assert_eq!(h.spawns(), 1);
    assert!(
        !h.store.maintenance_seal(),
        "the conversation still owns its permit"
    );
    drop(permit);
    assert!(h.store.activities().is_empty());
    assert!(h.store.maintenance_seal());
}

/// More think calls at once than any small job bound (the local Node used to stop at 16): all of
/// them run at once and answer within their timeout, as in process, none refused as exhausted,
/// backed off and timed out into a fallback.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_burst_of_calls_all_answer_within_their_timeout() {
    let h = Local::new().await;
    let gw = h.node(&["m1"]);
    let t0 = Instant::now();
    let calls = (0..24).map(|_| {
        gw.attempt(
            "@slow",
            Tier::Smart,
            Duration::from_secs(8),
            &ThinkTools::None,
            None,
        )
    });
    let out = futures::future::join_all(calls).await;
    for a in &out {
        assert_eq!(
            a.result.as_ref().map(|p| p.text.as_str()).ok(),
            Some("慢答"),
            "failures: {:?}",
            a.failures
        );
        assert_eq!(a.attempts, 1);
    }
    assert!(t0.elapsed() < Duration::from_secs(8), "{:?}", t0.elapsed());
    assert_eq!(h.spawns(), 24);
}

/// A 503 is terminal for the plan, but its reported cost still reaches the bill.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_busy_model_stops_after_one_request_and_books_the_failure() {
    let h = Local::new().await;
    let models = ["m-busy", "haiku"];
    let call = |gw: RecordingGateway| async move {
        tagged(
            "answer",
            "default",
            gw.attempt(
                "想一想",
                Tier::Smart,
                Duration::from_secs(20),
                &ThinkTools::None,
                None,
            ),
        )
        .await
    };
    let attempt = call(h.node(&models)).await;
    assert_eq!(h.spawns(), 1);
    assert_eq!(attempt.attempts, 1);
    assert_eq!(attempt.model, "m-busy");
    assert_eq!(attempt.failures, ["m-busy: API Error: 503 overloaded"]);
    assert!(attempt.result.is_err());
    assert!(
        attempt
            .usage
            .iter()
            .any(|u| u.model == "m-busy" && u.cost_reported == Some(0.01))
    );
    assert_eq!(
        node_checks::usage_records_distinct_json_extract_detail_call_id_count(&h.store).unwrap(),
        1
    );
    assert_eq!(
        node_checks::job_count_finished_think_answer(&h.store).unwrap(),
        1
    );
    assert_eq!(
        node_checks::runtime_job_attempts_count_finished_with_ordinal(&h.store).unwrap(),
        1
    );
}

/// The draft reaches `on_text` as the current message's whole text each time it
/// grows, a new message starting over.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_draft_streams_until_the_final_answer() {
    let h = Local::new().await;
    let stream = |gw: RecordingGateway| async move {
        let seen = Mutex::new(vec![]);
        let on = |t: &str| seen.lock().unwrap().push(t.to_string());
        let p = gw
            .call_streaming(
                "写吧",
                Tier::Smart,
                Duration::from_secs(20),
                &ThinkTools::None,
                &on,
            )
            .await
            .unwrap();
        (p.text, seen.into_inner().unwrap())
    };
    let (answer, seen) = stream(h.node(&["m1"])).await;
    assert_eq!(answer, "答案");
    assert_eq!(seen, ["先看", "先看一下", "答案"]);
}

/// Images go to the adapter as ACP image blocks, and a chat answer's attachments are where the
/// prompt says.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn images_and_attachments_reach_the_run() {
    use base64::Engine;
    let h = Local::new().await;
    let png = b"png-bytes".to_vec();
    let b64 = base64::engine::general_purpose::STANDARD.encode(&png);
    let block =
        json!({"type":"image","source":{"type":"base64","media_type":"image/png","data":b64}});
    let gw = h.node(&["m1"]);
    let p = tagged(
        "chat",
        "default",
        with_images(
            vec![block],
            gw.call(
                "看图",
                Tier::Smart,
                Duration::from_secs(20),
                &ThinkTools::None,
            ),
        ),
    )
    .await
    .unwrap();
    assert_eq!(p.text, "答案");
    assert!(
        h.read("prompt").contains(&b64),
        "the image went in the prompt"
    );
    assert_eq!(
        node_checks::files_created_by_matching_text(&h.store).unwrap(),
        "job"
    );

    // an attachment held by the Hub
    use sha2::Digest as _;
    let body = "笔记内容";
    let sha = hex::encode(sha2::Sha256::digest(body.as_bytes()));
    let id = "fil_notes1";
    let blob = h.root.join("data/files/default").join(&sha[..2]).join(id);
    std::fs::create_dir_all(blob.parent().unwrap()).unwrap();
    std::fs::write(&blob, body).unwrap();
    h.store
        .files()
        .create(&steve_store::NewFile {
            id: id.into(),
            workspace_id: "default".into(),
            sha256: sha,
            name: "notes.md".into(),
            mime: "text/markdown".into(),
            size: body.len() as i64,
            created_by: "u".into(),
        })
        .unwrap();
    let rel = format!(".steve/attachments/{id}-notes.md");
    let tools = ThinkTools::Read(vec![Readable {
        file_id: id.into(),
        rel_path: rel,
    }]);
    let node = h
        .node(&["m1"])
        .call("@read", Tier::Smart, Duration::from_secs(20), &tools)
        .await
        .unwrap();
    assert_eq!(node.text, body);
    let new: Value = serde_json::from_str(&h.read("new")).unwrap();
    assert_eq!(new["systemPrompt"], Value::Null);
    assert!(new.pointer("/_meta/claudeCode/options/tools").is_none());
}

/// A call given up (its future dropped: the stop button, a `select!`) cancels its job on the
/// Node, and the harness is gone.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_dropped_call_cancels_its_job_and_the_process_dies() {
    let h = Local::new().await;
    let gw = Arc::new(h.node(&["m1"]));
    let g = gw.clone();
    let task = tokio::spawn(async move {
        g.call(
            "@hang",
            Tier::Smart,
            Duration::from_secs(60),
            &ThinkTools::None,
        )
        .await
    });
    eventually_sync("the harness runs", || !h.read("pid").trim().is_empty()).await;
    let pid: i32 = h.read("pid").trim().parse().unwrap();
    assert!(alive(pid));
    task.abort();
    let _ = task.await;
    eventually_sync("the harness gone", || !alive(pid)).await;
    eventually_sync("the attempt ended cancelled", || {
        node_checks::runtime_job_attempts_outcome(&h.store)
            .unwrap()
            .as_deref()
            == Some("cancelled")
    })
    .await;
    assert_eq!(
        node_checks::command_count_cancel_with_target_job_attempt(&h.store).unwrap(),
        1
    );
    assert_eq!(h.spawns(), 1);
}

/// A timed-out Node job returns its phase and reaps its harness.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_execution_timeout_stops_the_job_without_a_retry() {
    let h = Local::new().await;
    let call = h
        .node(&["m1", "m2"])
        .attempt(
            "@hang",
            Tier::Smart,
            Duration::from_millis(1_500),
            &ThinkTools::None,
            None,
        )
        .await;
    assert!(matches!(
        call.result,
        Err(steve_core::CoreError::Gateway(
            steve_core::gateway::GatewayError::Timeout(TimeoutPhase::Execution)
        ))
    ));
    assert_eq!(call.attempts, 1);
    assert_eq!(call.failures, ["m1: 网关在 execution 阶段超时"]);
    let pid: i32 = h.read("pid").trim().parse().unwrap();
    eventually_sync("the harness gone", || !alive(pid)).await;
}

/// The same calls over the fake Node, driven by hand: the draft as it is written, the answer from
/// the completion, its usage booked.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_fake_node_contract_streams_and_answers() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path());
    let node = FakeNode::new("n1");
    let rt = NodeRuntime::new(store.clone(), node.clone(), Arc::new(NoDomain));
    node.connect(Arc::new(Sink(Arc::downgrade(&rt))));
    rt.start();
    eventually_sync("reconciled", || rt.is_reconciled()).await;
    let gw = Arc::new(RecordingGateway::new(
        Arc::new(SingleModelGateway {
            one: NodePrint {
                runtime: rt.clone(),
                data_dir: dir.path().to_path_buf(),
            },
            fast: vec![],
            smart: vec!["sonnet".into()],
            models: None,
        }),
        store.clone(),
    ));
    let seen = Arc::new(Mutex::new(vec![]));
    let (g, s) = (gw.clone(), seen.clone());
    let task = tokio::spawn(async move {
        let on = move |t: &str| s.lock().unwrap().push(t.to_string());
        g.call_streaming(
            "想",
            Tier::Smart,
            Duration::from_secs(20),
            &ThinkTools::None,
            &on,
        )
        .await
    });
    eventually_sync("the job started", || {
        node.log().iter().any(|(m, _)| *m == "start_job")
    })
    .await;
    let exec: ExecutionRef =
        serde_json::from_str(&node_checks::runtime_job_attempts_execution(&store).unwrap())
            .unwrap();
    let spec = node.job_spec(exec.attempt()).unwrap();
    assert_eq!(spec.run.choice.model.as_deref(), Some("sonnet"));
    assert_eq!(spec.run.prompt, build::text("想"));
    node.draft(&exec, "m1", &["先", "看"]).await.unwrap();
    node.draft(&exec, "m2", &["答案"]).await.unwrap();
    let report = UsageReport {
        report_id: "r1".into(),
        model: Some("sonnet".into()),
        meter_epoch: Epoch::new("e1").unwrap(),
        scope: UsageScope::All,
        measurement: Measurement::Final,
        tokens: Tokens {
            input: Some(7),
            output: Some(3),
            ..Default::default()
        },
        context_used: None,
        context_limit: None,
        cost: Some(Money {
            decimal: "0.5".into(),
            currency: "USD".into(),
        }),
    };
    node.emit(
        Subject::Execution(exec.clone()),
        EventBody::Usage { report },
    )
    .await
    .unwrap();
    let mut done = build::done();
    done.result = build::text("答案");
    node.finish(&exec, done).await.unwrap();
    let p = task.await.unwrap().unwrap();
    assert_eq!((p.text.as_str(), p.cost_usd), ("答案", Some(0.5)));
    assert_eq!(*seen.lock().unwrap(), vec!["先", "先看", "答案"]);
    assert_eq!(p.usage.len(), 1);
    assert_eq!(p.usage[0].tokens.input, Some(7));
    assert_eq!(node_checks::usage_records_count(&store).unwrap(), 1);
    assert_eq!(
        node_checks::event_count(&store).unwrap(),
        0,
        "a think call is no transcript"
    );
}

/// A job attempt an earlier Hub process left open, as it wrote it.
fn orphan(store: &Store, job: &str, attempt: &str) -> (OperationId, ExecutionRef) {
    use steve_store::jobs::*;
    let op = format!("op_{attempt}");
    let spec = build::job(job, attempt, "想");
    let exec = build::exec_job(job, attempt);
    let c = Command {
        id: OperationId::new(op.clone()).unwrap(),
        issued_at: Timestamp(steve_store::now()),
        accept_before: Timestamp(steve_store::now() + 600_000),
        grant: GrantId::new("hub").unwrap(),
        body: spec.clone(),
    };
    let req = serde_json::to_value(&c).unwrap();
    let digest = digest_of(&req).unwrap().to_string();
    store
        .write::<_, StoreError>(|tx| {
            steve_store::node_jobs::create_job_in(
                tx,
                &NewJob {
                    id: job,
                    kind: JobKind::Think,
                    purpose: "answer",
                    workspace: "default",
                    node: "n1",
                    thread_run: None,
                },
                &NewJobAttempt {
                    id: attempt,
                    ordinal: 0,
                    model: "sonnet",
                    operation: &op,
                    execution: &serde_json::to_value(&exec).unwrap(),
                    spec: &serde_json::to_value(&spec).unwrap(),
                    digest: &digest,
                },
                &StartJob {
                    request: &req,
                    digest: &digest,
                },
                steve_store::now(),
            )
        })
        .unwrap();
    (OperationId::new(op).unwrap(), exec)
}

fn outcome(store: &Store, attempt: &str) -> Option<String> {
    node_checks::runtime_job_attempts_outcome_by_attempt(store, &attempt).unwrap()
}

/// After the Hub restarts without callers, a never-sent job settles locally, a running job can
/// finish on the Node, and an unknown job remains held.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hub_restart_reconciles_open_jobs_without_new_node_mutations() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path());
    let node = FakeNode::new("n1");
    let gate = Arc::new(Gate::default());
    node.connect(gate.clone());
    let rt = NodeRuntime::new(store.clone(), node.clone(), Arc::new(NoDomain));
    *gate.0.lock().unwrap() = Some(Arc::downgrade(&rt));
    rt.start();
    eventually_sync("reconciled", || rt.is_reconciled()).await;
    // written by this process after its own boot (as its callers would)
    let (op_a, exec_a) = orphan(&store, "job_a", "ja_a");
    let (op_b, exec_b) = orphan(&store, "job_b", "ja_b");
    let starts = || node.log().iter().filter(|(m, _)| *m == "start_job").count();
    eventually_sync("both started", || starts() == 2).await;
    eventually_sync("both running", || {
        node_checks::runtime_job_attempts_count_by_state(&store, "running").unwrap() == 2
    })
    .await;
    node.lose_track(&exec_b).unwrap();
    // the Hub goes down; one more job was written but never sent
    let old = Arc::downgrade(&rt);
    drop(rt);
    eventually_sync("the old coordinator gone", || old.upgrade().is_none()).await;
    orphan(&store, "job_c", "ja_c");

    let rt = NodeRuntime::new(store.clone(), node.clone(), Arc::new(NoDomain));
    *gate.0.lock().unwrap() = Some(Arc::downgrade(&rt));
    rt.start();
    eventually_sync("reconciled", || rt.is_reconciled()).await;
    eventually_sync("the unsent one ended here", || {
        outcome(&store, "ja_c").as_deref() == Some("cancelled")
    })
    .await;
    assert_eq!(
        node_checks::command_state_by_operation(&store, "op_ja_c").unwrap(),
        "rejected"
    );
    assert!(!node.cancel_requested(&op_a));
    node.finish(&exec_a, build::done()).await.unwrap();
    eventually_sync("the running one ended", || {
        outcome(&store, "ja_a").as_deref() == Some("succeeded")
    })
    .await;
    assert_eq!(
        node_checks::command_state_by_operation(&store, op_b.as_str()).unwrap(),
        "unknown"
    );
    assert!(outcome(&store, "ja_b").is_none());
    assert!(
        !node
            .log()
            .iter()
            .any(|(m, _)| matches!(*m, "cancel" | "abandon"))
    );
    assert_eq!(starts(), 2, "nothing started again");
    assert_eq!(node_checks::job_count_by_state(&store, "open").unwrap(), 1);
}

/// A fake clock plus the real Hub gateway, job coordinator and durable usage ledger.
#[tokio::test]
async fn gateway_deadline_is_classified_from_phase_evidence_and_failure_usage_is_replayed() {
    use steve_core::{CoreError, gateway::GatewayError};
    use steve_store::core_fixture::usage::usage;
    let budget = Duration::from_secs(60);
    for (expected, evidence) in [
        (TimeoutPhase::Queue, None),
        (TimeoutPhase::FirstByte, Some(GatewayPhase::Submitted)),
        (TimeoutPhase::Execution, Some(GatewayPhase::Responding)),
        (TimeoutPhase::Unknown, None),
    ] {
        for run in 1..=2 {
            let dir = tempfile::tempdir().unwrap();
            let store = open_store(dir.path());
            let node = FakeNode::new("n1");
            let rt = NodeRuntime::new(store.clone(), node.clone(), Arc::new(NoDomain));
            node.connect(Arc::new(Sink(Arc::downgrade(&rt))));
            rt.start();
            eventually_sync("reconciled", || rt.is_reconciled()).await;
            let gw = RecordingGateway::new(
                Arc::new(SingleModelGateway {
                    one: NodePrint {
                        runtime: rt.clone(),
                        data_dir: dir.path().to_path_buf(),
                    },
                    fast: vec!["first".into(), "must-not-run".into()],
                    smart: vec![],
                    models: None,
                }),
                store.clone(),
            );
            tokio::time::pause();
            if expected == TimeoutPhase::Queue {
                store.maintenance_set(true).unwrap();
            }
            let call = gw.attempt("test", Tier::Fast, budget, &ThinkTools::None, None);
            tokio::pin!(call);
            let mut exec = None;
            let inject = async {
                if expected == TimeoutPhase::Queue {
                    // Drive admission to Pending without advancing time.
                    tokio::task::yield_now().await;
                } else {
                    eventually_sync("fake Node accepted the think job", || {
                        node.log().iter().any(|(m, _)| *m == "start_job")
                    })
                    .await;
                    let execution: ExecutionRef = serde_json::from_str(
                        &node_checks::runtime_job_attempts_execution(&store).unwrap(),
                    )
                    .unwrap();
                    let subject = Subject::Execution(execution.clone());
                    for phase in [
                        GatewayPhase::Submitting,
                        GatewayPhase::Submitted,
                        GatewayPhase::Responding,
                    ] {
                        if evidence.is_some_and(|last| phase <= last)
                            || (expected == TimeoutPhase::Unknown
                                && run == 2
                                && phase == GatewayPhase::Submitting)
                        {
                            node.emit(subject.clone(), EventBody::GatewayProgress { phase })
                                .await
                                .unwrap();
                        }
                    }
                    if evidence.is_some() {
                        node.emit(
                            subject,
                            EventBody::Usage {
                                report: gateway_test_report(
                                    "observed",
                                    Measurement::Cumulative,
                                    10 + run,
                                ),
                            },
                        )
                        .await
                        .unwrap();
                    }
                    exec = Some(execution);
                }
                tokio::time::advance(budget).await;
            };
            // Poll the actual deadline and hold on to the injector until its advance returns.
            let (a, ()) = tokio::join!(biased; &mut call, inject);
            assert!(
                matches!(a.result, Err(CoreError::Gateway(GatewayError::Timeout(p))) if p == expected),
                "{expected:?} #{run}: {:?}",
                a.result
            );
            assert_eq!(a.attempts, u32::from(expected != TimeoutPhase::Queue));
            assert_eq!(
                node.log().iter().filter(|(m, _)| *m == "start_job").count(),
                a.attempts as usize
            );
            if evidence.is_some() {
                assert_eq!(a.usage[0].tokens.output, Some(10 + run as i64));
                let execution = exec.unwrap();
                let report = gateway_test_report("settled", Measurement::Final, 20 + run);
                // A Final can reach the Hub after its caller timed out. Replays do not bill twice.
                for _ in 0..2 {
                    node.emit(
                        Subject::Execution(execution.clone()),
                        EventBody::Usage {
                            report: report.clone(),
                        },
                    )
                    .await
                    .unwrap();
                }
                let rows = usage(&store, Some("gateway"));
                assert_eq!(rows.len(), 1);
                assert!(!rows[0].ok);
                assert_eq!(rows[0].tokens.output, Some(20 + run as i64));
                assert_eq!(rows[0].tokens.input, Some(7));
                assert_eq!(rows[0].tokens.cache_read, Some(5));
            }
            tokio::time::resume();
        }
    }
}

fn gateway_test_report(id: &str, measurement: Measurement, output: u64) -> UsageReport {
    UsageReport {
        report_id: id.into(),
        model: Some("first".into()),
        meter_epoch: Epoch::new("test_meter").unwrap(),
        scope: UsageScope::All,
        measurement,
        tokens: Tokens {
            input: Some(7),
            output: Some(output),
            cache_read: Some(5),
            ..Default::default()
        },
        context_used: None,
        context_limit: None,
        cost: None,
    }
}

#[tokio::test]
async fn missing_capability_and_missing_authorization_stop_before_any_job_or_model_request() {
    use steve_core::{CoreError, gateway::GatewayError};
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path());
    let node = FakeNode::new("n1");
    let rt = NodeRuntime::new(store.clone(), node.clone(), Arc::new(NoDomain));
    let gw = RecordingGateway::new(
        Arc::new(SingleModelGateway {
            one: NodePrint {
                runtime: rt,
                data_dir: dir.path().to_path_buf(),
            },
            fast: vec!["first".into(), "must-not-run".into()],
            smart: vec![],
            models: None,
        }),
        store.clone(),
    );
    store
        .files()
        .create(&steve_store::NewFile {
            id: "fil_allowed".into(),
            workspace_id: steve_store::WS.into(),
            sha256: "a".repeat(64),
            name: "notes.md".into(),
            mime: "text/markdown".into(),
            size: 1,
            created_by: "owner".into(),
        })
        .unwrap();
    for (id, expected) in [
        ("fil_allowed", GatewayError::MissingCapability),
        ("fil_ungranted", GatewayError::ToolDenied),
    ] {
        let tools = ThinkTools::Read(vec![Readable {
            file_id: id.into(),
            rel_path: format!(".steve/attachments/{id}-notes.md"),
        }]);
        let a = gw
            .attempt("test", Tier::Fast, Duration::from_secs(5), &tools, None)
            .await;
        assert!(
            matches!(a.result, Err(CoreError::Gateway(ref e)) if *e == expected),
            "{:?}",
            a.result
        );
        assert!(
            !expected.can_handoff(),
            "refuse explicitly rather than selecting another Bot"
        );
        assert_eq!(a.attempts, 0);
        assert!(a.usage.is_empty());
        assert_eq!(node_checks::job_count(&store).unwrap(), 0);
        assert!(!node.log().iter().any(|(m, _)| *m == "start_job"));
    }
}
