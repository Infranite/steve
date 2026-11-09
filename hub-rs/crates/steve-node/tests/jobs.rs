//! LocalNode running think jobs against a stand-in ACP adapter: one model per attempt, the draft
//! as it is written, files and images as ACP sends them, and a process group that is proven gone
//! however the attempt ends. Every harness runs the same way; the adapter is one of nobody's.
#![cfg(unix)]

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::json;
use steve_acp::AcpConfig;
use steve_node::{LocalNode, NodeConfig, Storage};
use steve_node_api::testkit::RecordingSink;
use steve_node_api::testkit::build::{cmd, job};
use steve_node_api::*;

/// The stand-in ACP adapter. Its argv goes to `<log>.args`, its working directory to `<log>.cwd`,
/// one line per process to `<log>.spawns`, `session/new` to `<log>.new`, the config options it was
/// set to to `<log>.config`, the prompt to `<log>.prompt`. What it does is picked by the message:
/// `@fail` (a 503, its usage reported), `@hang` (its pid to `<log>.pid`), `@read` (reads the
/// attachment, says so as an ACP read, and answers with it), `@stop` (ends with `max_tokens`);
/// otherwise two messages, the second the answer.
const ADAPTER: &str = r#"#!/usr/bin/env python3
import json, os, sys, time
log = "FAKE_LOG"
def put(what, text, mode="w"):
    try:
        with open(log + "." + what, mode) as f: f.write(text)
    except OSError:
        pass
put("spawns", "%d\n" % os.getpid(), "a")
put("args", "\n".join(sys.argv[1:]))
put("cwd", os.getcwd())
def send(v):
    print(json.dumps(v, ensure_ascii=False), flush=True)
def update(sid, u):
    send({"jsonrpc": "2.0", "method": "session/update", "params": {"sessionId": sid, "update": u}})
def chunk(sid, text, mid):
    update(sid, {"sessionUpdate": "agent_message_chunk", "messageId": mid, "content": {"type": "text", "text": text}})
options = lambda model, effort: [
    {"id": "model", "category": "model", "type": "select", "currentValue": model,
     "options": [{"value": "m1"}, {"value": "m2"}]},
    {"id": "effort", "category": "thought_level", "type": "select", "currentValue": effort,
     "options": [{"value": "low"}, {"value": "high"}]}]
state = {"model": "m1", "effort": "low"}
for line in sys.stdin:
    msg = json.loads(line)
    m, p = msg.get("method"), msg.get("params") or {}
    result = {}
    if m == "initialize":
        result = {"protocolVersion": 1, "agentCapabilities": {
            "promptCapabilities": {"image": True},
            "sessionCapabilities": {"systemPrompt": {"mode": {"override": {}, "append": {}}}}}}
    elif m == "session/new":
        put("new", json.dumps(p, ensure_ascii=False))
        result = {"sessionId": "s1", "modes": {"currentModeId": "default", "availableModes": [
            {"id": "default"}, {"id": "plan"}]}, "configOptions": options(state["model"], state["effort"])}
    elif m == "session/set_config_option":
        state[p["configId"]] = p["value"]
        put("config", "%s=%s\n" % (p["configId"], p["value"]), "a")
        result = {"configOptions": options(state["model"], state["effort"])}
    elif m == "session/prompt":
        put("prompt", json.dumps(p["prompt"], ensure_ascii=False))
        sid, text = p["sessionId"], p["prompt"][0].get("text", "")
        if "@fail" in text:
            update(sid, {"sessionUpdate": "usage_update", "used": 4, "size": 100,
                         "cost": {"amount": 0.01, "currency": "USD"}})
            send({"jsonrpc": "2.0", "id": msg["id"], "error": {"code": -32603, "message": "API Error: 503 overloaded"}})
            continue
        if "@hang" in text:
            put("pid", str(os.getpid()))
            chunk(sid, "写到一半", "a")
            time.sleep(60)
        if "@read" in text:
            path = os.path.join(os.getcwd(), ".steve/attachments/att_1-notes.md")
            update(sid, {"sessionUpdate": "tool_call", "toolCallId": "t1", "kind": "read",
                         "title": "Read", "status": "pending", "locations": [{"path": path}],
                         "rawInput": {"file_path": path}})
            body = open(path).read()
            update(sid, {"sessionUpdate": "tool_call_update", "toolCallId": "t1", "status": "completed"})
            chunk(sid, body, "a")
            result = {"stopReason": "end_turn"}
        elif "@stop" in text:
            chunk(sid, "写了一半", "a")
            result = {"stopReason": "max_tokens"}
        else:
            chunk(sid, "先看", "a"); chunk(sid, "一下", "a"); chunk(sid, "答案", "b")
            result = {"stopReason": "end_turn", "usage": {"inputTokens": 3, "outputTokens": 2}}
    if "id" in msg:
        send({"jsonrpc": "2.0", "id": msg["id"], "result": result})
"#;

/// The adapter id the jobs run on: one of nobody's, declared to keep its own sandbox (so its logs
/// reach the test). [`a_custom_adapter_with_nothing_declared_runs_in_the_nodes_sandbox`] runs it
/// with nothing declared.
const FAKE: &str = "fake";

struct T {
    _dir: Option<tempfile::TempDir>,
    root: PathBuf,
    log: PathBuf,
    sink: Arc<RecordingSink>,
    files: Arc<Files>,
}

impl T {
    fn new() -> T {
        let dir = tempfile::tempdir().unwrap();
        let mut t = T::at(&dir.path().canonicalize().unwrap());
        t._dir = Some(dir);
        t
    }

    /// On a directory someone else keeps (the parent of a child-process Node).
    fn at(root: &Path) -> T {
        let root = root.to_path_buf();
        let log = root.join("fake");
        let body = ADAPTER.replace("FAKE_LOG", &log.display().to_string());
        let p = root.join("adapter.py");
        std::fs::write(&p, body).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        T {
            _dir: None,
            root,
            log,
            sink: Arc::new(RecordingSink::default()),
            files: Arc::new(Files::default()),
        }
    }

    fn config(&self, jobs: u32) -> NodeConfig {
        NodeConfig {
            ceiling: steve_node::PolicyCeiling::unlimited(),
            node: NodeId::new("hub").unwrap(),
            home: self.root.join("node"),
            acp: AcpConfig {
                declared: HashMap::from([(
                    FAKE.to_string(),
                    serde_json::from_value(json!({"properties": {
                        "nested_sandbox": false, "self_read_only": true}}))
                    .unwrap(),
                )]),
                adapters: HashMap::from([
                    (FAKE.to_string(), self.root.join("adapter.py")),
                    ("bare".to_string(), self.root.join("adapter.py")),
                ]),
                node: "node".into(),
                claude_executable: "claude".into(),
                steve_home: self.root.join("steve"),
                codex_home: self.root.join("codex-home"),
                user_home: self.root.clone(),
                version: "test".into(),
                idle_release: Duration::from_secs(600),
                retry_delays: vec![],
            },
            roots: vec![],
            capacity: Capacity {
                sessions: 0,
                jobs,
                execs: 0,
            },
            storage: Storage::default(),
        }
    }

    /// Opens the Node on the home. A process another test forks meanwhile shares the home's lock
    /// until it execs: a reopen right after a drop may have to wait for that.
    fn open(&self, jobs: u32) -> LocalNode {
        let t0 = Instant::now();
        loop {
            match LocalNode::open(self.config(jobs)) {
                Ok(n) => return n,
                Err(e)
                    if e.code == ErrorCode::Conflict && t0.elapsed() < Duration::from_secs(5) =>
                {
                    std::thread::sleep(Duration::from_millis(20))
                }
                Err(e) => panic!("{e:?}"),
            }
        }
    }

    fn node(&self) -> LocalNode {
        let n = self.open(4);
        n.connect(self.sink.clone(), Some(self.files.clone()));
        n
    }

    fn read(&self, what: &str) -> String {
        std::fs::read_to_string(format!("{}.{what}", self.log.display())).unwrap_or_default()
    }

    fn spawns(&self) -> usize {
        self.read("spawns").lines().count()
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
}

/// Attachments the Hub holds, by id; who asked is recorded.
#[derive(Default)]
struct Files {
    blobs: Mutex<HashMap<String, Vec<u8>>>,
    asked: Mutex<Vec<FileAccess>>,
}

impl Files {
    fn add(&self, id: &str, bytes: &[u8], mime: &str) -> ArtifactRef {
        self.blobs.lock().unwrap().insert(id.into(), bytes.to_vec());
        ArtifactRef {
            id: ArtifactId::new(id).unwrap(),
            sha256: Digest::sha256(bytes),
            size: bytes.len() as u64,
            mime: mime.into(),
        }
    }
}

#[async_trait::async_trait]
impl HubFiles for Files {
    async fn fetch(&self, a: FileAccess, id: ArtifactId, o: u64, m: u32) -> ApiResult<ReadChunk> {
        self.asked.lock().unwrap().push(a);
        let b = self
            .blobs
            .lock()
            .unwrap()
            .get(id.as_str())
            .cloned()
            .ok_or_else(|| ApiError::new(ErrorCode::Denied, "not granted"))?;
        let from = (o as usize).min(b.len());
        let to = (from + m as usize).min(b.len());
        Ok(ReadChunk {
            offset: o,
            eof: to == b.len(),
            bytes: b[from..to].to_vec(),
        })
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

fn wall() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

/// A command accepted now (the real clock).
fn now_cmd<B>(op: &str, body: B) -> Command<B> {
    let mut c = cmd(op, body);
    c.issued_at = Timestamp(wall());
    c.accept_before = Timestamp(wall() + 60_000);
    c
}

/// A think job on `harness` with a deadline a minute away.
fn think(attempt: &str, harness: &str, text: &str) -> JobSpec {
    let mut j = job("j_1", attempt, text);
    j.run.choice.harness = harness.into();
    j.run.choice.model = Some("m1".into());
    j.run.limits.deadline = Timestamp(wall() + 60_000);
    j.run.limits.offline_until = Timestamp(wall() + 60_000);
    j.work = JobWork::Think(ThinkJob {
        instructions: Some("你是 Steve。".into()),
        tools: ThinkToolMode::None,
    });
    j
}

fn exec(attempt: &str) -> ExecutionRef {
    ExecutionRef::Job {
        job: JobId::new("j_1").unwrap(),
        attempt: AttemptId::new(attempt).unwrap(),
    }
}

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

fn completion(s: Observed) -> Completion {
    match s {
        Observed::Finished {
            outcome,
            result: Some(OperationResult::Execution(c)),
            ..
        } => {
            assert_eq!(outcome, c.outcome);
            c
        }
        s => panic!("{s:?}"),
    }
}

fn gone(pid: i64) -> bool {
    let r = unsafe { libc::kill(pid as libc::pid_t, 0) };
    r != 0 && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
}

/// Nothing of the attempt is left under the Node's home.
fn no_scratch(t: &T) -> bool {
    std::fs::read_dir(t.root.join("node/jobs"))
        .map(|d| d.count() == 0)
        .unwrap_or(true)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_think_job_streams_its_draft_and_ends_with_its_answer() {
    let t = T::new();
    let n = t.node();
    let r = n
        .start_job(now_cmd("op_j", think("a_1", FAKE, "数数")))
        .await
        .unwrap();
    assert_eq!(r.state, Observed::Running);
    let c = completion(finished(&n, "op_j").await);
    assert_eq!(
        (c.outcome, c.stop_reason.as_str(), c.result.clone()),
        (
            Outcome::Succeeded,
            "end_turn",
            vec![Part::Text {
                text: "答案".into()
            }]
        )
    );
    t.until("the end", |t| !t.ended().is_empty()).await;
    assert_eq!(
        t.kinds(),
        [
            "attempt_started",
            "message_started",
            "text_delta",
            "text_delta",
            "message_started",
            "text_delta",
            "usage",
            "attempt_ended"
        ]
    );
    let evs = t.sink.events();
    let deltas: Vec<(String, u64, String)> = evs
        .iter()
        .filter_map(|e| match &e.body {
            EventBody::TextDelta {
                message,
                utf8_offset,
                text,
                ..
            } => Some((message.clone(), *utf8_offset, text.clone())),
            _ => None,
        })
        .collect();
    assert_eq!(
        deltas
            .iter()
            .map(|d| (d.1, d.2.as_str()))
            .collect::<Vec<_>>(),
        [(0, "先看"), ("先看".len() as u64, "一下"), (0, "答案")]
    );
    assert_eq!(deltas[0].0, deltas[1].0);
    assert_ne!(deltas[1].0, deltas[2].0, "a new message is a new message");
    assert!(
        evs.iter()
            .all(|e| e.subject == Subject::Execution(exec("a_1")))
    );
    let usage = evs
        .iter()
        .find_map(|e| match &e.body {
            EventBody::Usage { report } => Some(report.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        (usage.measurement, usage.tokens.input, usage.tokens.output),
        (Measurement::Final, Some(3), Some(2))
    );
    // the head as the session's system prompt in place of the harness's own (no tools); the
    // message alone is the prompt; one model
    let new: serde_json::Value = serde_json::from_str(&t.read("new")).unwrap();
    assert_eq!(
        new["systemPrompt"],
        json!({"mode": "override", "content": [{"type": "text", "text": "你是 Steve。"}]})
    );
    let prompt: serde_json::Value = serde_json::from_str(&t.read("prompt")).unwrap();
    assert_eq!(prompt, json!([{"type": "text", "text": "数数"}]));
    assert_eq!(t.read("config"), "model=m1\n");
    // in a scratch directory of its own, cleaned up after
    let cwd = t.read("cwd");
    assert!(
        cwd.trim()
            .starts_with(&*t.root.join("node/jobs").to_string_lossy()),
        "{cwd}"
    );
    assert!(no_scratch(&t));
    assert_eq!(t.spawns(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_read_attached_job_reads_its_file_and_nothing_else() {
    let t = T::new();
    let n = t.node();
    let a = t.files.add("att_1", b"notes", "text/markdown");
    let mut j = think("a_1", FAKE, "@read 总结 .steve/attachments/att_1-notes.md");
    j.run.prompt.push(Part::File {
        artifact: a,
        name: ".steve/attachments/att_1-notes.md".into(),
    });
    j.work = JobWork::Think(ThinkJob {
        instructions: Some("你是 Steve。".into()),
        tools: ThinkToolMode::ReadAttached,
    });
    n.start_job(now_cmd("op_j", j)).await.unwrap();
    let c = completion(finished(&n, "op_j").await);
    assert_eq!(
        c.result,
        vec![Part::Text {
            text: "notes".into()
        }]
    );
    // with tools the head comes after the harness's own system prompt
    let new: serde_json::Value = serde_json::from_str(&t.read("new")).unwrap();
    assert_eq!(new["systemPrompt"]["mode"], "append");
    assert_eq!(
        t.files.asked.lock().unwrap().clone(),
        vec![FileAccess::Execution(exec("a_1"))],
        "fetched for this attempt"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn images_go_as_acp_image_blocks() {
    let t = T::new();
    let n = t.node();
    let png = t.files.add("img_1", b"\x89PNG-bytes", "image/png");
    let mut j = think("a_1", FAKE, "看图");
    j.run.prompt.push(Part::Image { artifact: png });
    n.start_job(now_cmd("op_c", j)).await.unwrap();
    completion(finished(&n, "op_c").await);
    let prompt: serde_json::Value = serde_json::from_str(&t.read("prompt")).unwrap();
    use base64::Engine as _;
    assert_eq!(
        prompt,
        json!([{"type": "text", "text": "看图"}, {"type": "image", "mimeType": "image/png",
            "data": base64::engine::general_purpose::STANDARD.encode(b"\x89PNG-bytes")}])
    );
}

/// An adapter that stops the turn otherwise (`max_tokens`) fails with that reason.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_turn_the_adapter_cut_short_fails_with_its_stop_reason() {
    let t = T::new();
    let n = t.node();
    n.start_job(now_cmd("op_j", think("a_1", FAKE, "@stop")))
        .await
        .unwrap();
    let c = completion(finished(&n, "op_j").await);
    assert_eq!(
        (c.outcome, c.stop_reason.as_str(), c.result),
        (Outcome::Failed, "max_tokens", vec![])
    );
}

/// An adapter of one's own with nothing declared: plain ACP, in the Node's read-only sandbox where
/// the platform has one (what it writes outside its scratch directory does not land).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_custom_adapter_with_nothing_declared_runs_in_the_nodes_sandbox() {
    let t = T::new();
    let n = t.node();
    n.start_job(now_cmd("op_j", think("a_1", "bare", "数数")))
        .await
        .unwrap();
    let c = completion(finished(&n, "op_j").await);
    assert_eq!(
        (c.outcome, c.result),
        (
            Outcome::Succeeded,
            vec![Part::Text {
                text: "答案".into()
            }]
        )
    );
    if steve_research::sandbox::available() {
        assert_eq!(t.spawns(), 0, "its log is outside what it may write");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_failing_model_fails_once_and_nothing_tries_another() {
    let t = T::new();
    let n = t.node();
    n.start_job(now_cmd("op_j", think("a_1", FAKE, "@fail")))
        .await
        .unwrap();
    let c = completion(finished(&n, "op_j").await);
    assert_eq!(
        (c.outcome, c.stop_reason.as_str()),
        (Outcome::Failed, "error")
    );
    assert!(c.transient, "a 503 may pass by itself");
    assert_eq!(
        c.error.map(|e| e.safe_detail),
        Some("API Error: 503 overloaded".into())
    );
    t.until("the end", |t| !t.ended().is_empty()).await;
    // the failed try's tokens are reported too
    assert!(t.kinds().contains(&"usage".to_string()));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(t.spawns(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn cancel_stops_the_whole_group() {
    let t = T::new();
    let n = t.node();
    n.start_job(now_cmd("op_j", think("a_1", FAKE, "@hang")))
        .await
        .unwrap();
    t.until("the harness", |t| !t.read("pid").is_empty()).await;
    t.until("the draft", |t| t.kinds().contains(&"text_delta".into()))
        .await;
    let pid: i64 = t.read("pid").trim().parse().unwrap();
    n.cancel(now_cmd(
        "op_c",
        Cancel {
            target: OperationId::new("op_j").unwrap(),
            reason: "stop".into(),
        },
    ))
    .await
    .unwrap();
    let c = completion(finished(&n, "op_j").await);
    assert_eq!(
        (c.outcome, c.stop_reason.as_str(), c.error, c.result),
        (Outcome::Cancelled, "cancelled", None, vec![])
    );
    assert!(gone(pid), "the harness is gone");
    // its `sleep` child too: nothing is left in its group
    assert!(unsafe { libc::kill(-(pid as libc::pid_t), 0) } != 0);
    assert!(no_scratch(&t));
    // its capacity is free again
    n.start_job(now_cmd("op_2", think("a_2", FAKE, "x")))
        .await
        .unwrap();
    completion(finished(&n, "op_2").await);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_job_past_its_deadline_times_out() {
    let t = T::new();
    let n = t.node();
    let mut j = think("a_1", FAKE, "@hang");
    j.run.limits.deadline = Timestamp(wall() + 1_000);
    n.start_job(now_cmd("op_j", j)).await.unwrap();
    t.until("the harness", |t| !t.read("pid").is_empty()).await;
    let pid: i64 = t.read("pid").trim().parse().unwrap();
    let c = completion(finished(&n, "op_j").await);
    assert_eq!(
        (c.outcome, c.stop_reason.as_str()),
        (Outcome::TimedOut, "timeout")
    );
    assert_eq!(c.error.map(|e| e.code), Some(ErrorCode::Expired));
    assert!(gone(pid));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_restart_leaves_the_job_unknown_until_it_is_abandoned() {
    let t = T::new();
    let n = t.node();
    n.start_job(now_cmd("op_j", think("a_1", FAKE, "@hang")))
        .await
        .unwrap();
    t.until("the harness", |t| !t.read("pid").is_empty()).await;
    let pid: i64 = t.read("pid").trim().parse().unwrap();
    n.shutdown().await;
    drop(n);
    assert!(gone(pid), "a shutdown stops the harness");
    let n = t.node();
    let r = n
        .operation(OperationId::new("op_j").unwrap())
        .await
        .unwrap();
    assert!(matches!(r.state, Observed::Unknown { .. }), "{r:?}");
    // never run again: the same command gives the same receipt
    let again = n
        .start_job(now_cmd("op_j", think("a_1", FAKE, "@hang")))
        .await;
    assert_eq!(t.spawns(), 1, "{again:?}");
    n.abandon(now_cmd(
        "op_ab",
        Abandon {
            target: OperationId::new("op_j").unwrap(),
            reason: "hub restarted".into(),
        },
    ))
    .await
    .unwrap();
    let c = completion(finished(&n, "op_j").await);
    assert_eq!(c.outcome, Outcome::Interrupted);
    t.until("the end", |t| !t.ended().is_empty()).await;
    assert_eq!(t.ended()[0].outcome, Outcome::Interrupted);
}

/// Runs in a child process (see `a_killed_node_leaves_its_job_group_until_it_is_abandoned`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn child_job_node_process() {
    let Ok(root) = std::env::var("STEVE_NODE_JOB_CHILD_ROOT") else {
        return;
    };
    let t = T::at(Path::new(&root));
    let n = t.node();
    n.start_job(now_cmd("op_j", think("a_1", FAKE, "@hang")))
        .await
        .unwrap();
    t.until("the harness", |t| !t.read("pid").is_empty()).await;
    std::fs::write(Path::new(&root).join("ready"), "").unwrap();
    tokio::time::sleep(Duration::from_secs(120)).await;
}

/// A Node killed mid-job leaves the harness's group running, recorded under the old incarnation:
/// the job is unknown after the restart, and the Hub's abandon stops that group before it ends.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_killed_node_leaves_its_job_group_until_it_is_abandoned() {
    let t = T::new();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "child_job_node_process", "--nocapture"])
        .env("STEVE_NODE_JOB_CHILD_ROOT", &t.root)
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
    let pid: i64 = t.read("pid").trim().parse().unwrap();
    assert!(!gone(pid), "the harness outlived the Node");
    let n = t.node();
    assert!(n.incarnation() >= 2);
    let r = n
        .operation(OperationId::new("op_j").unwrap())
        .await
        .unwrap();
    assert!(matches!(r.state, Observed::Unknown { .. }), "{r:?}");
    assert!(!gone(pid), "a restart does not stop it by itself");
    n.abandon(now_cmd(
        "op_ab",
        Abandon {
            target: OperationId::new("op_j").unwrap(),
            reason: "node crashed".into(),
        },
    ))
    .await
    .unwrap();
    let ab = finished(&n, "op_ab").await;
    assert!(
        matches!(
            ab,
            Observed::Finished {
                outcome: Outcome::Succeeded,
                ..
            }
        ),
        "{ab:?}"
    );
    let c = completion(finished(&n, "op_j").await);
    assert_eq!(c.outcome, Outcome::Interrupted);
    assert!(gone(pid), "the abandon stopped the harness");
    // its `sleep` child too: nothing is left in its group
    assert!(unsafe { libc::kill(-(pid as libc::pid_t), 0) } != 0);
    assert_eq!(t.spawns(), 1, "nothing ran it again");
    // and forgotten once gone
    let db = rusqlite::Connection::open(t.root.join("node/node.sqlite3")).unwrap();
    let procs: i64 = db
        .query_row("SELECT count(*) FROM procs", [], |r| r.get(0))
        .unwrap();
    assert_eq!(procs, 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_job_command_runs_once_and_an_attempt_never_twice() {
    let t = T::new();
    let n = t.node();
    let c = now_cmd("op_j", think("a_1", FAKE, "x"));
    let first = n.start_job(c.clone()).await.unwrap();
    assert_eq!(n.start_job(c.clone()).await.unwrap(), first);
    completion(finished(&n, "op_j").await);
    assert_eq!(n.start_job(c).await.unwrap().operation.as_str(), "op_j");
    let e = n
        .start_job(now_cmd("op_j2", think("a_1", FAKE, "x")))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Conflict);
    assert_eq!(t.spawns(), 1);
}

#[tokio::test]
async fn what_this_node_cannot_run_is_refused() {
    let t = T::new();
    // no job capacity: no jobs at all
    let n = t.open(0);
    let e = n
        .start_job(now_cmd("op_1", think("a_1", FAKE, "x")))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Unsupported);
    let caps = n.probe(false).await.unwrap();
    assert!(caps.harnesses.iter().all(|h| !h.jobs));
    n.shutdown().await;
    drop(n);

    let n = t.open(1);
    let caps = n.probe(false).await.unwrap();
    let jobs: Vec<&str> = caps
        .harnesses
        .iter()
        .filter(|h| h.jobs)
        .map(|h| h.name.as_str())
        .collect();
    assert_eq!(jobs, ["bare", FAKE], "every adapter runs jobs");
    for (op, spec) in [
        ("op_r", {
            let mut r = steve_node_api::testkit::build::research("j_1", "a_r", "x");
            r.run.limits = think("a_r", FAKE, "x").run.limits;
            r
        }),
        ("op_h", think("a_h", "gemini", "x")),
    ] {
        let e = n.start_job(now_cmd(op, spec)).await.unwrap_err();
        assert_eq!(e.code, ErrorCode::Unsupported, "{op}");
    }
    let mut past = think("a_p", FAKE, "x");
    past.run.limits.deadline = Timestamp(wall() - 1);
    assert_eq!(
        n.start_job(now_cmd("op_p", past)).await.unwrap_err().code,
        ErrorCode::Expired
    );
    // one at a time with a capacity of one
    n.start_job(now_cmd("op_1", think("a_1", FAKE, "@hang")))
        .await
        .unwrap();
    assert_eq!(
        n.start_job(now_cmd("op_2", think("a_2", FAKE, "x")))
            .await
            .unwrap_err()
            .code,
        ErrorCode::Exhausted
    );
    n.shutdown().await;
}
