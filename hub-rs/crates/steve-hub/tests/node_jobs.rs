//! Steve's think calls as jobs on the local Node (N0d-7): the same
//! answers, drafts, errors and statistics rows as the in-process gateway, a dropped call cancelled
//! on the Node with its process gone, and a Hub restart that stops what an earlier run left open
//! without ever running it again.
#![cfg(unix)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use steve_core::gateway::{ModelGateway, Readable, ThinkTools, Tier, with_images};
use steve_core::stats::{RecordingGateway, tagged};
use steve_core::{Fallback, InProcess, LocalAcp};
use steve_hub::wire::node_files::NodeFiles;
use steve_hub::wire::node_runtime::{NodeDomain, NodePrint, NodeRuntime, Sink};
use steve_node_api::testkit::{FakeNode, build};
use steve_node_api::*;
use steve_store::{FnPublisher, Store, StoreError};

/// The stand-in ACP adapter, for both ways of running it (in process and as a Node job). One line
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
            None => Err(ApiError::new(ErrorCode::Offline, "no hub")),
        }
    }
}

async fn until(what: &str, f: impl Fn() -> bool) {
    let t0 = Instant::now();
    while !f() {
        assert!(
            t0.elapsed() < Duration::from_secs(15),
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn open_store(dir: &Path) -> Store {
    let store = Store::open(dir.join("steve.sqlite3"), FnPublisher(|_| {})).unwrap();
    store.ensure_workspace().unwrap();
    store
}

fn q<T: rusqlite::types::FromSql>(store: &Store, sql: &str) -> T {
    store
        .reader(|c| Ok::<_, StoreError>(c.query_row(sql, [], |r| r.get(0))?))
        .unwrap()
}

/// The statistics rows of the gateway's calls, without their ids and times.
fn rows(store: &Store) -> Vec<Value> {
    store
        .reader(|c| {
            let mut st = c.prepare(
                "SELECT workspace_id,source,purpose,harness,model,input,output,cache_read,cache_write,reasoning,cost_reported,ok,attempts,detail FROM usage_records ORDER BY id",
            )?;
            let v = st
                .query_map([], |r| {
                    let mut o = vec![];
                    for i in 0..14 {
                        o.push(match r.get_ref(i)? {
                            rusqlite::types::ValueRef::Null => Value::Null,
                            rusqlite::types::ValueRef::Integer(n) => json!(n),
                            rusqlite::types::ValueRef::Real(f) => json!(f),
                            rusqlite::types::ValueRef::Text(t) => {
                                json!(String::from_utf8_lossy(t))
                            }
                            rusqlite::types::ValueRef::Blob(_) => Value::Null,
                        });
                    }
                    Ok(Value::Array(o))
                })?
                .collect::<Result<Vec<_>, _>>()?;
            Ok::<_, StoreError>(v)
        })
        .unwrap()
}

fn clear_rows(store: &Store) {
    store
        .transaction::<_, StoreError>(|tx| {
            tx.execute("DELETE FROM usage_records", [])?;
            Ok(())
        })
        .unwrap();
}

/// A Hub with the local Node running think jobs on the stand-in claude, and the in-process
/// gateway on the very same script, side by side.
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
        until("reconciled", || rt.is_reconciled()).await;
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
        let id = steve_acp::adapters::DEFAULT_ADAPTER.0.to_string();
        steve_acp::AcpConfig {
            declared: HashMap::from([(
                id.clone(),
                serde_json::from_value(json!({"properties": {
                    "nested_sandbox": false, "self_read_only": true}}))
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

    /// Steve's gateway in process.
    fn legacy(&self, models: &[&str]) -> RecordingGateway {
        RecordingGateway::new(
            Arc::new(InProcess {
                one: LocalAcp {
                    acp: Self::acp(&self.root),
                    cwd: self.root.join("scratch"),
                },
                fast: vec![],
                smart: Self::smart(models),
                models: None,
            }),
            self.store.clone(),
        )
    }

    /// Steve's gateway over the local Node.
    fn node(&self, models: &[&str]) -> RecordingGateway {
        RecordingGateway::new(
            Arc::new(Fallback {
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

    fn reset(&self) {
        for what in ["spawns", "new", "prompt", "pid"] {
            let _ = std::fs::remove_file(format!("{}.{what}", self.log.display()));
        }
    }
}

fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks that the process exists
    unsafe { libc::kill(pid, 0) == 0 }
}

/// A background model request must not start a harness behind a persisted upgrade drain.
/// Reopening admission releases that same request exactly once, for either gateway.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn model_calls_wait_for_maintenance_before_starting() {
    let h = Local::new().await;
    for gw in [h.legacy(&["m1"]), h.node(&["m1"])] {
        let spawns = h.spawns();
        let jobs: i64 = q(&h.store, "SELECT COUNT(*) FROM runtime_jobs");
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
            q::<i64>(&h.store, "SELECT COUNT(*) FROM runtime_jobs"),
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

/// A conversation admitted before maintenance may finish its nested model call, including
/// fallback. The drain still waits for that conversation's permit before sealing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn admitted_model_calls_finish_during_maintenance() {
    let h = Local::new().await;
    let gw = h.node(&["m-busy", "m1"]);
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
    assert_eq!(out.attempts, 2, "fallback stays inside the admitted call");
    assert_eq!(h.spawns(), 2);
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

/// A busy first model, then the next: the same answer, attempts, failures and spent tokens as in
/// process, and the very same statistics rows. Each model tried is one job of one attempt.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_busy_model_falls_back_and_books_what_the_in_process_gateway_books() {
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
    let a = call(h.legacy(&models)).await;
    let legacy_rows = rows(&h.store);
    assert_eq!(h.spawns(), 2);
    clear_rows(&h.store);
    h.reset();
    let b = call(h.node(&models)).await;
    assert_eq!(h.spawns(), 2, "one process per model, none again");
    assert_eq!(a.result.as_ref().unwrap().text, "答案");
    assert_eq!(b.result.as_ref().map(|p| p.text.as_str()).unwrap(), "答案");
    assert_eq!((b.attempts, &b.model), (2, &"haiku".to_string()));
    assert_eq!(b.failures, a.failures);
    assert_eq!(b.failures, vec!["m-busy: API Error: 503 overloaded"]);
    assert_eq!(b.usage, a.usage, "failed tries' tokens are kept");
    assert_eq!(
        b.result.as_ref().unwrap().cost_usd,
        a.result.as_ref().unwrap().cost_usd
    );
    assert_eq!(rows(&h.store), legacy_rows);
    assert_eq!(
        q::<i64>(
            &h.store,
            "SELECT COUNT(*) FROM runtime_jobs WHERE state='finished' AND kind='think' AND purpose='answer'"
        ),
        2
    );
    assert_eq!(
        q::<i64>(
            &h.store,
            "SELECT COUNT(*) FROM runtime_job_attempts WHERE state='finished' AND ordinal=0"
        ),
        2
    );
}

/// The draft reaches `on_text` as in process: the current message's whole text each time it
/// grows, a new message starting over.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_draft_streams_as_in_process() {
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
    let a = stream(h.legacy(&["m1"])).await;
    let b = stream(h.node(&["m1"])).await;
    assert_eq!(a.1, vec!["先看", "先看一下", "答案"]);
    assert_eq!(b, a);
}

/// Images go to the adapter as ACP image blocks, and a chat answer's attachments are where the
/// prompt says, read the same way as in process.
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
        q::<String>(
            &h.store,
            "SELECT created_by FROM files WHERE name LIKE 'job-input-%'"
        ),
        "job"
    );

    // an attachment held by the Hub, and the in-process run's copy of it in scratch
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
    let copy = h.root.join("scratch").join(&rel);
    std::fs::create_dir_all(copy.parent().unwrap()).unwrap();
    std::fs::write(&copy, body).unwrap();
    let tools = ThinkTools::Read(vec![Readable {
        file_id: id.into(),
        rel_path: rel,
    }]);
    let legacy = h
        .legacy(&["m1"])
        .call("@read", Tier::Smart, Duration::from_secs(20), &tools)
        .await
        .unwrap();
    let legacy_new = h.read("new");
    let node = h
        .node(&["m1"])
        .call("@read", Tier::Smart, Duration::from_secs(20), &tools)
        .await
        .unwrap();
    assert_eq!((legacy.text.as_str(), node.text.as_str()), (body, body));
    // with tools the harness keeps its own system prompt and tools, on both paths
    let tools_of = |new: &str| {
        let v: Value = serde_json::from_str(new).unwrap();
        (
            v["systemPrompt"].clone(),
            v.pointer("/_meta/claudeCode/options/tools").cloned(),
        )
    };
    assert_eq!(tools_of(&legacy_new), (Value::Null, None));
    assert_eq!(tools_of(&h.read("new")), tools_of(&legacy_new));
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
    until("the harness runs", || !h.read("pid").trim().is_empty()).await;
    let pid: i32 = h.read("pid").trim().parse().unwrap();
    assert!(alive(pid));
    task.abort();
    let _ = task.await;
    until("the harness gone", || !alive(pid)).await;
    until("the attempt ended cancelled", || {
        q::<Option<String>>(
            &h.store,
            "SELECT json_extract(completion,'$.outcome') FROM runtime_job_attempts",
        )
        .as_deref()
            == Some("cancelled")
    })
    .await;
    assert_eq!(
        q::<i64>(
            &h.store,
            "SELECT COUNT(*) FROM node_commands WHERE method='cancel' AND target_job_attempt IS NOT NULL"
        ),
        1
    );
    assert_eq!(h.spawns(), 1);
}

/// A call past its timeout fails with the in-process words, and its harness is stopped.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_timeout_reads_as_in_process() {
    let h = Local::new().await;
    let t = Duration::from_millis(1500);
    let a = h
        .legacy(&["m1"])
        .call("@hang", Tier::Smart, t, &ThinkTools::None)
        .await
        .unwrap_err()
        .to_string();
    h.reset();
    let b = h
        .node(&["m1"])
        .call("@hang", Tier::Smart, t, &ThinkTools::None)
        .await
        .unwrap_err()
        .to_string();
    assert_eq!(a, "Command timed out after 1500 ms");
    assert_eq!(b, a);
    let pid: i32 = h.read("pid").trim().parse().unwrap();
    until("the harness gone", || !alive(pid)).await;
}

/// #9: a timeout ends the fallback chain in process and on the Node alike: the next model is not tried
/// (it would wait as long), and the failure says it timed out.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_timeout_ends_the_chain_on_both_paths() {
    let h = Local::new().await;
    let t = Duration::from_millis(1500);
    for gw in [h.legacy(&["m1", "m2"]), h.node(&["m1", "m2"])] {
        let a = gw
            .attempt("@hang", Tier::Smart, t, &ThinkTools::None, None)
            .await;
        assert!(a.result.is_err());
        assert_eq!(a.attempts, 1, "{:?}", a.failures);
        assert_eq!(a.failures.len(), 1);
        assert!(
            a.failures[0].starts_with("m1: [timeout] Command timed out"),
            "{:?}",
            a.failures
        );
        h.reset();
    }
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
    until("reconciled", || rt.is_reconciled()).await;
    let gw = Arc::new(RecordingGateway::new(
        Arc::new(Fallback {
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
    until("the job started", || {
        node.log().iter().any(|(m, _)| *m == "start_job")
    })
    .await;
    let exec: ExecutionRef = serde_json::from_str(&q::<String>(
        &store,
        "SELECT execution FROM runtime_job_attempts",
    ))
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
    assert_eq!(q::<i64>(&store, "SELECT COUNT(*) FROM usage_records"), 1);
    assert_eq!(
        q::<i64>(&store, "SELECT COUNT(*) FROM events"),
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
        .transaction::<_, StoreError>(|tx| {
            create_job_in(
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
    q(
        store,
        &format!(
            "SELECT json_extract(completion,'$.outcome') FROM runtime_job_attempts WHERE attempt_id='{attempt}'"
        ),
    )
}

/// The Hub restarts with job attempts open: their callers are gone. One the Node never got ends
/// here; a running one is cancelled; one the Node cannot tell the end of is abandoned. None is
/// ever started again.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hub_restart_stops_open_job_attempts_and_runs_none_again() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path());
    let node = FakeNode::new("n1");
    let gate = Arc::new(Gate::default());
    node.connect(gate.clone());
    let rt = NodeRuntime::new(store.clone(), node.clone(), Arc::new(NoDomain));
    *gate.0.lock().unwrap() = Some(Arc::downgrade(&rt));
    rt.start();
    until("reconciled", || rt.is_reconciled()).await;
    // written by this process after its own boot (as its callers would)
    let (op_a, exec_a) = orphan(&store, "job_a", "ja_a");
    let (_, exec_b) = orphan(&store, "job_b", "ja_b");
    let starts = || node.log().iter().filter(|(m, _)| *m == "start_job").count();
    until("both started", || starts() == 2).await;
    until("both running", || {
        q::<i64>(
            &store,
            "SELECT COUNT(*) FROM runtime_job_attempts WHERE state='running'",
        ) == 2
    })
    .await;
    node.lose_track(&exec_b).unwrap();
    // the Hub goes down; one more job was written but never sent
    let old = Arc::downgrade(&rt);
    drop(rt);
    until("the old coordinator gone", || old.upgrade().is_none()).await;
    orphan(&store, "job_c", "ja_c");

    let rt = NodeRuntime::new(store.clone(), node.clone(), Arc::new(NoDomain));
    *gate.0.lock().unwrap() = Some(Arc::downgrade(&rt));
    rt.start();
    until("reconciled", || rt.is_reconciled()).await;
    until("the unsent one ended here", || {
        outcome(&store, "ja_c").as_deref() == Some("cancelled")
    })
    .await;
    assert_eq!(
        q::<String>(
            &store,
            "SELECT state FROM node_commands WHERE operation_id='op_ja_c'"
        ),
        "rejected"
    );
    until("the running one asked to cancel", || {
        node.cancel_requested(&op_a)
    })
    .await;
    node.finish(&exec_a, {
        let mut c = build::done();
        c.outcome = Outcome::Cancelled;
        c.stop_reason = "cancelled".into();
        c
    })
    .await
    .unwrap();
    until("the running one ended", || {
        outcome(&store, "ja_a").as_deref() == Some("cancelled")
    })
    .await;
    until("the unknown one abandoned", || {
        outcome(&store, "ja_b").as_deref() == Some("interrupted")
    })
    .await;
    assert!(node.log().iter().any(|(m, _)| *m == "abandon"));
    assert_eq!(starts(), 2, "nothing started again");
    assert_eq!(
        q::<i64>(
            &store,
            "SELECT COUNT(*) FROM runtime_jobs WHERE state='open'"
        ),
        0
    );
    let _ = exec_b;
}
