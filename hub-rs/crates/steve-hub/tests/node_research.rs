//! Inline research as jobs on the local Node (N0d-9, `STEVE_NODE_JOBS=research`): the same run
//! through both executors with one stand-in harness gives the same outcome, drafts, tool rows,
//! statistics row and audit; a follow-up continues the session; admission, the stop button and
//! shutdown behave as in process; Steve's research tools answer a research job only, and only
//! while its run is running.
#![cfg(unix)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use steve_core::thread::{
    ResearchOutcome, ResearchRequest, ResearchSink, Researcher, ResumeFrom, RunEnd, ToolSeen,
};
use steve_hub::wire::node_files::NodeFiles;
use steve_hub::wire::node_runtime::{NodeDomain, NodeRuntime, Sink};
use steve_hub::wire::node_tools::{NodeTools, Service};
use steve_hub::wire::research::{NodeResearch, ResearchAdapter};
use steve_node_api::*;
use steve_store::{FnPublisher, Store, StoreError};

/// The stand-in `claude -p` for research. argv to `<log>.args`, one line per process to
/// `<log>.spawns`. `--model m-busy` fails at once; `@hang` writes a draft, its pid to `<log>.pid`
/// and waits; `@bash` asks for a shell (a violation); a `--resume` answers the follow-up;
/// otherwise it reads a file and answers with an offer.
const CLAUDE: &str = r#"#!/bin/sh
log="@@LOG@@"
echo "$$" >> "$log.spawns"
printf '%s\n' "$@" > "$log.args"
model=""; prev=""
for a in "$@"; do if [ "$prev" = "--model" ]; then model="$a"; fi; prev="$a"; done
emit() { sed "s/@@MODEL@@/$model/g"; }
echo '{"type":"system","subtype":"init","session_id":"sess-1","model":"'"$model"'"}'
if [ "$model" = "m-busy" ]; then
  echo '{"type":"result","subtype":"error_during_execution","is_error":true,"result":"API Error: 503 overloaded","session_id":"sess-1"}'
  exit 1
fi
case "$*" in
*@hang*)
  echo '{"type":"stream_event","event":{"type":"message_start","message":{"id":"m1"}}}'
  echo '{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"写到一半"}}}'
  echo "$$" > "$log.pid"
  sleep 60 & wait
  exit 0;;
*@bash*)
  echo '{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","id":"t1","name":"Bash","input":{"command":"touch x"}}]}}'
  sleep 5
  exit 0;;
*--resume*)
  emit <<'EOF'
{"type":"stream_event","event":{"type":"message_start","message":{"id":"m3"}}}
{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"接着说。"}}}
{"type":"result","subtype":"success","is_error":false,"result":"接着说。","session_id":"sess-1","total_cost_usd":0.5,"modelUsage":{"@@MODEL@@":{"inputTokens":7,"outputTokens":1,"costUSD":0.5}}}
EOF
  exit 0;;
esac
emit <<'EOF'
{"type":"stream_event","event":{"type":"message_start","message":{"id":"m1"}}}
{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"@@LIB@@"}}]}}
{"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":"t1","content":"1\tfn add() {}"}]}}
{"type":"stream_event","event":{"type":"message_start","message":{"id":"m2"}}}
{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"add 什么都不做。"}}}
{"type":"result","subtype":"success","is_error":false,"result":"add 什么都不做。\nOFFER: {\"title\":\"实现 add\",\"goal\":\"让 add 返回和\"}","session_id":"sess-1","total_cost_usd":0.25,"modelUsage":{"@@MODEL@@":{"inputTokens":3,"outputTokens":2,"cacheReadInputTokens":5,"costUSD":0.25}}}
EOF
"#;

struct NoDomain;
impl NodeDomain for NoDomain {
    fn turn_ended(&self, _: &str, _: Option<&str>, _: &str, _: Option<String>) {}
}

/// Steve's tools as a stand-in: a research tool answers, a session's tool exists too.
struct Tools;
#[async_trait::async_trait]
impl Service for Tools {
    fn catalog(&self) -> Vec<ToolDescriptor> {
        vec![ToolDescriptor {
            name: "steve_update".into(),
            description: "write".into(),
            input_schema: json!({"type": "object"}),
            read_only: false,
        }]
    }
    async fn call(&self, _: steve_store::Session, _: &str, _: Value) -> Result<String, String> {
        Ok("session tool".into())
    }
    fn research_catalog(&self) -> Vec<ToolDescriptor> {
        steve_hub::wire::research_mcp::descriptors()
    }
    async fn research_call(
        &self,
        run: steve_store::ThreadRun,
        name: &str,
        _: Value,
    ) -> Result<String, String> {
        Ok(format!("{name} for {}", run.id))
    }
}

#[derive(Default)]
struct Seen {
    text: Mutex<Vec<String>>,
    tools: Mutex<Vec<ToolSeen>>,
}
impl ResearchSink for Seen {
    fn text(&self, t: &str) {
        self.text.lock().unwrap().push(t.into());
    }
    fn tool(&self, t: &ToolSeen) {
        self.tools.lock().unwrap().push(t.clone());
    }
}

async fn until(what: &str, f: impl Fn() -> bool) {
    let t0 = Instant::now();
    while !f() {
        assert!(
            t0.elapsed() < Duration::from_secs(20),
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn alive(pid: i32) -> bool {
    // SAFETY: signal 0 only checks that the process exists
    unsafe { libc::kill(pid, 0) == 0 }
}

fn q<T: rusqlite::types::FromSql>(store: &Store, sql: &str) -> T {
    store
        .reader(|c| Ok::<_, StoreError>(c.query_row(sql, [], |r| r.get(0))?))
        .unwrap()
}

/// The research statistics rows, without ids, times and durations.
fn rows(store: &Store) -> Vec<Value> {
    store
        .reader(|c| {
            let mut st = c.prepare(
                "SELECT workspace_id,purpose,harness,model,input,output,cache_read,cache_write,reasoning,cost_reported,ok,attempts,detail FROM usage_records WHERE source='gateway' ORDER BY id",
            )?;
            let v = st
                .query_map([], |r| {
                    let mut o = vec![];
                    for i in 0..13 {
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

/// The shared files: the project and the stand-in harness.
struct World {
    _dir: tempfile::TempDir,
    root: PathBuf,
    proj: PathBuf,
    log: PathBuf,
    claude: PathBuf,
}

impl World {
    fn new() -> World {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let proj = root.join("proj");
        std::fs::create_dir_all(proj.join("src")).unwrap();
        std::fs::write(proj.join("src/lib.rs"), "fn add() {}\n").unwrap();
        std::fs::create_dir_all(root.join("home")).unwrap();
        let log = root.join("fake");
        let claude = root.join("claude");
        std::fs::write(
            &claude,
            CLAUDE
                .replace("@@LOG@@", &log.display().to_string())
                .replace("@@LIB@@", &proj.join("src/lib.rs").display().to_string()),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&claude, std::fs::Permissions::from_mode(0o755)).unwrap();
        World {
            _dir: dir,
            root,
            proj,
            log,
            claude,
        }
    }

    fn read(&self, what: &str) -> String {
        std::fs::read_to_string(format!("{}.{what}", self.log.display())).unwrap_or_default()
    }

    fn spawns(&self) -> usize {
        self.read("spawns").lines().count()
    }

    fn reset(&self) {
        for what in ["spawns", "args", "pid"] {
            let _ = std::fs::remove_file(format!("{}.{what}", self.log.display()));
        }
    }

    /// The value after `flag` in the last run's argv.
    fn flag(&self, f: &str) -> Option<String> {
        let args = self.read("args");
        let l: Vec<&str> = args.lines().collect();
        l.iter().position(|x| *x == f).map(|i| l[i + 1].to_string())
    }

    async fn pid(&self) -> i32 {
        until("the harness's pid", || !self.read("pid").trim().is_empty()).await;
        self.read("pid").trim().parse().unwrap()
    }
}

/// One Hub (its own store) with research in process or on its local Node.
struct Hub {
    store: Store,
    adapter: Arc<ResearchAdapter>,
    // the coordinator the Node's sink reaches (held weakly there)
    rt: Option<Arc<NodeRuntime>>,
    tools: Option<Arc<NodeTools>>,
    data: PathBuf,
    /// What the adapter is told the local Node's roots are (a test may break them).
    roots: Arc<Mutex<Result<Vec<steve_node::Root>, String>>>,
}

impl Hub {
    async fn new(w: &World, name: &str, on_node: bool, max_global: usize) -> Hub {
        let base = w.root.join(name);
        let data = base.join("data");
        std::fs::create_dir_all(&data).unwrap();
        let store = Store::open(base.join("steve.sqlite3"), FnPublisher(|_| {})).unwrap();
        store.ensure_workspace().unwrap();
        let mut cfg = steve_research::ResearchConfig::new(&w.claude, base.join("scratch"));
        cfg.models = vec!["m1".into()];
        cfg.home = w.root.join("home");
        cfg.claude_dir = w.root.join("claude");
        cfg.files_root = base.join("files");
        cfg.sandbox = steve_research::SandboxMode::Off;
        cfg.max_global = max_global;
        let adapter = ResearchAdapter::new(
            cfg.clone(),
            steve_core::repo::Db::new(store.clone()),
            data.clone(),
        );
        if !on_node {
            return Hub {
                store,
                adapter: Arc::new(adapter),
                rt: None,
                tools: None,
                data,
                roots: Arc::new(Mutex::new(Ok(vec![]))),
            };
        }
        let roots = vec![steve_node::Root {
            id: RootId::new("proj").unwrap(),
            path: w.proj.clone(),
            display: "proj".into(),
            access: RootAccess::ReadWrite,
        }];
        let node = Arc::new(
            steve_node::LocalNode::open(steve_node::NodeConfig {
                node: NodeId::new("local").unwrap(),
                home: base.join("node"),
                acp: steve_acp::AcpConfig {
                    adapters: HashMap::new(),
                    node: "node".into(),
                    claude_executable: "claude".into(),
                    steve_home: base.join("steve"),
                    codex_home: base.join("codex-home"),
                    user_home: w.root.clone(),
                    version: "test".into(),
                    idle_release: Duration::from_secs(600),
                    retry_delays: vec![],
                },
                roots: roots.clone(),
                capacity: Capacity {
                    sessions: 0,
                    jobs: steve_hub::wire::LOCAL_JOB_CAPACITY,
                    execs: 0,
                },
                storage: steve_node::Storage::default(),
                jobs: Default::default(),
            })
            .unwrap(),
        );
        node.enable_research(cfg);
        let rt = NodeRuntime::new(store.clone(), node.clone(), Arc::new(NoDomain));
        node.connect(
            Arc::new(Sink(Arc::downgrade(&rt))),
            Some(Arc::new(NodeFiles::new(
                store.clone(),
                NodeId::new("local").unwrap(),
                data.clone(),
            ))),
        );
        let tools = NodeTools::open(
            store.clone(),
            NodeId::new("local").unwrap(),
            &base.join("host"),
        )
        .unwrap();
        tools.install(Arc::new(Tools)).unwrap();
        node.connect_tools(tools.clone()).await.unwrap();
        rt.start();
        until("reconciled", || rt.is_reconciled()).await;
        let told = Arc::new(Mutex::new(Ok(roots)));
        let now = told.clone();
        let adapter = adapter.on_node(NodeResearch {
            runtime: rt.clone(),
            roots: Arc::new(move || now.lock().unwrap().clone()),
        });
        Hub {
            store,
            adapter: Arc::new(adapter),
            rt: Some(rt),
            tools: Some(tools),
            data,
            roots: told,
        }
    }

    /// A file of the workspace, its bytes where the Hub keeps them.
    fn file(&self, id: &str, name: &str, body: &str) -> steve_store::FileRef {
        use sha2::Digest as _;
        let sha = hex::encode(sha2::Sha256::digest(body.as_bytes()));
        let row = self
            .store
            .files()
            .create(&steve_store::NewFile {
                id: id.into(),
                workspace_id: "default".into(),
                sha256: sha,
                name: name.into(),
                mime: "text/markdown".into(),
                size: body.len() as i64,
                created_by: "u".into(),
            })
            .unwrap();
        let p = steve_core::attachments::blob_path(&self.data, &row);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, body).unwrap();
        steve_store::FileRef {
            id: id.into(),
            name: name.into(),
            mime: row.mime,
            size: row.size,
        }
    }

    fn thread_run(&self, run: &str, ws: &str) {
        self.store
            .transaction::<_, StoreError>(|tx| {
                tx.execute(
                    "INSERT INTO thread_runs (id, workspace_id, message_id, status, created) VALUES (?, ?, 'msg_1', 'running', 1)",
                    [run, ws],
                )?;
                Ok(())
            })
            .unwrap();
    }
}

fn bot() -> steve_core::repo::Bot {
    steve_core::repo::Bot {
        id: "steve".into(),
        workspace_id: "default".into(),
        name: "Steve".into(),
        role: "r".into(),
        main: true,
        harness: "claude-code".into(),
        model: None,
        reasoning_effort: None,
        rules: "说重点。".into(),
        accept_mode: "inherit".into(),
        perm_mode: "default".into(),
        enabled: true,
        revision: 1,
        updated: 0,
        persona: String::new(),
        proactive: 1,
        avatar: Default::default(),
        default_project: None,
        prompt: None,
        device: None,
    }
}

fn request(w: &World, run: &str, ws: &str, question: &str) -> ResearchRequest {
    let project = steve_core::repo::Project {
        id: "prj".into(),
        workspace_id: "default".into(),
        name: "add".into(),
        description: "加法".into(),
        device_id: "hub".into(),
        path: w.proj.display().to_string(),
        kind: "dir".into(),
        worktree: false,
        default_bot: None,
        release: None,
        archived: false,
        created: 0,
        updated: 0,
    };
    ResearchRequest {
        run: run.into(),
        ws: ws.into(),
        mid: "msg_1".into(),
        user: None,
        question: question.into(),
        history: vec![],
        main: bot(),
        context: String::new(),
        project: Some(project.clone()),
        readable: vec![project],
        files: vec![],
        earlier_files: vec![],
        resume: None,
        stop: Arc::new(AtomicBool::new(false)),
        mcp: None,
        models: Default::default(),
        ui: false,
    }
}

/// What the thread keeps of an outcome (the time to the first text is the run's own).
fn kept(o: &ResearchOutcome) -> ResearchOutcome {
    ResearchOutcome {
        first_text_ms: None,
        ..o.clone()
    }
}

fn tool_rows(s: &Seen) -> Vec<(String, String, bool, Option<String>)> {
    s.tools
        .lock()
        .unwrap()
        .iter()
        .map(|t| (t.name.clone(), t.brief.clone(), t.ok, t.error.clone()))
        .collect()
}

/// The same question through both executors: the same outcome (answer without its offer line,
/// the offer, end, model, session, turns), drafts, tool rows and statistics row. A busy model falls
/// back to the next one alike, and a follow-up continues the session with its model on both. On
/// the Node each attempt is one research job of its thread run.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_run_on_the_node_matches_the_run_in_process() {
    let w = World::new();
    let legacy = Hub::new(&w, "legacy", false, 3).await;
    let node = Hub::new(&w, "node", true, 3).await;
    let mut outs = vec![];
    for h in [&legacy, &node] {
        w.reset();
        let seen = Seen::default();
        let out = h
            .adapter
            .research(request(&w, "tr_1", "default", "add 怎么工作？"), &seen)
            .await;
        assert_eq!(out.end, RunEnd::Done, "{out:?}");
        assert_eq!(w.spawns(), 1);
        assert_eq!(w.flag("--model").as_deref(), Some("m1"));
        assert!(w.read("args").contains("add 怎么工作？"));
        assert!(w.read("args").contains("说重点。"));
        assert!(out.first_text_ms.is_some());
        outs.push((
            kept(&out),
            seen.text.lock().unwrap().last().cloned(),
            tool_rows(&seen),
        ));
    }
    assert_eq!(outs[0], outs[1]);
    let (out, draft, tools) = &outs[0];
    assert_eq!(out.text, "add 什么都不做。");
    assert_eq!(out.offer.as_ref().unwrap().title, "实现 add");
    assert_eq!(out.claude_session.as_deref(), Some("sess-1"));
    assert_eq!(out.model.as_deref(), Some("m1"));
    assert_eq!(out.turns, 2);
    assert_eq!(draft.as_deref(), Some("add 什么都不做。"));
    assert_eq!(tools.len(), 1);
    assert_eq!(tools[0].0, "Read");
    assert!(tools[0].2);
    let booked = rows(&legacy.store);
    assert_eq!(booked.len(), 1);
    assert_eq!(booked, rows(&node.store));
    assert_eq!(booked[0][1], "research");
    assert_eq!(booked[0][3], "m1");
    assert_eq!(booked[0][4], 3);
    assert_eq!(booked[0][9], 0.25);
    // the Node ran it as one research job of the thread run, finished
    assert_eq!(
        q::<String>(
            &node.store,
            "SELECT kind||' '||purpose||' '||thread_run_id||' '||state FROM runtime_jobs"
        ),
        "research research tr_1 finished"
    );
    assert_eq!(
        q::<i64>(&legacy.store, "SELECT COUNT(*) FROM runtime_jobs"),
        0
    );

    // a busy first model: the next one answers, on both
    let mut outs = vec![];
    for h in [&legacy, &node] {
        w.reset();
        let mut r = request(&w, "tr_2", "default", "add 怎么工作？");
        r.models = steve_core::models::Choice {
            harness: "claude-code".into(),
            models: vec!["m-busy".into(), "m2".into()],
            effort: Some("max".into()),
        };
        let out = h.adapter.research(r, &Seen::default()).await;
        assert_eq!(out.end, RunEnd::Done, "{out:?}");
        assert_eq!(w.spawns(), 2);
        assert_eq!(w.flag("--effort").as_deref(), Some("max"));
        outs.push(kept(&out));
    }
    assert_eq!(outs[0], outs[1]);
    assert_eq!(outs[0].model.as_deref(), Some("m2"));
    assert_eq!(rows(&legacy.store), rows(&node.store));
    assert_eq!(
        q::<i64>(
            &node.store,
            "SELECT COUNT(*) FROM runtime_jobs WHERE thread_run_id='tr_2'"
        ),
        2
    );

    // a follow-up continues the session with the model that answered
    let mut outs = vec![];
    for h in [&legacy, &node] {
        w.reset();
        let mut r = request(&w, "tr_3", "default", "然后呢？");
        r.resume = Some(ResumeFrom {
            run: "tr_2".into(),
            claude_session: "sess-1".into(),
            model: "m2".into(),
            prior_answer: "add 什么都不做。".into(),
        });
        let out = h.adapter.research(r, &Seen::default()).await;
        assert_eq!(out.end, RunEnd::Done, "{out:?}");
        assert_eq!(w.spawns(), 1);
        assert_eq!(w.flag("--resume").as_deref(), Some("sess-1"));
        assert_eq!(w.flag("--model").as_deref(), Some("m2"));
        outs.push(kept(&out));
    }
    assert_eq!(outs[0], outs[1]);
    assert_eq!(outs[0].text, "接着说。");
    assert_eq!(rows(&legacy.store), rows(&node.store));
}

/// A shell call ends the run as a violation on both, with the same end, answer and audit row.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_violation_ends_and_is_audited_alike() {
    let w = World::new();
    let legacy = Hub::new(&w, "legacy", false, 3).await;
    let node = Hub::new(&w, "node", true, 3).await;
    let mut outs = vec![];
    for h in [&legacy, &node] {
        let out = h
            .adapter
            .research(
                request(&w, "tr_v", "default", "@bash 跑一下"),
                &Seen::default(),
            )
            .await;
        assert_eq!(out.end, RunEnd::Violated, "{out:?}");
        outs.push(kept(&out));
    }
    assert_eq!(outs[0], outs[1]);
    let audit = |s: &Store| {
        q::<String>(
            s,
            "SELECT action||' '||target||' '||detail FROM audit WHERE action='thread.research_violation'",
        )
    };
    assert_eq!(audit(&legacy.store), audit(&node.store));
    assert!(
        audit(&node.store).contains("Bash"),
        "{}",
        audit(&node.store)
    );
}

/// The stop button ends the job on the Node with what it wrote, its harness gone, as in process;
/// shutdown says the processes are gone only once they are, and admits nothing after.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stop_and_shutdown_end_the_harness() {
    let w = World::new();
    let legacy = Hub::new(&w, "legacy", false, 3).await;
    let node = Hub::new(&w, "node", true, 3).await;
    let mut outs = vec![];
    for h in [&legacy, &node] {
        w.reset();
        let r = request(&w, "tr_s", "default", "@hang 慢慢读");
        let stop = r.stop.clone();
        let a = h.adapter.clone();
        let run = tokio::spawn(async move { a.research(r, &Seen::default()).await });
        let pid = w.pid().await;
        tokio::time::sleep(Duration::from_millis(300)).await;
        stop.store(true, Ordering::SeqCst);
        let out = tokio::time::timeout(Duration::from_secs(20), run)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(out.end, RunEnd::Stopped, "{out:?}");
        assert!(!alive(pid), "the harness is gone");
        outs.push(kept(&out));
    }
    assert_eq!(outs[0], outs[1]);
    assert!(outs[0].text.starts_with("写到一半"), "{:?}", outs[0].text);

    // shutdown: the running harness is stopped and gone before it says true
    w.reset();
    let a = node.adapter.clone();
    let r = request(&w, "tr_d", "default", "@hang 再读");
    let run = tokio::spawn(async move { a.research(r, &Seen::default()).await });
    let pid = w.pid().await;
    assert!(node.adapter.shutdown(Duration::from_secs(20)).await);
    assert!(!alive(pid));
    let out = run.await.unwrap();
    assert_eq!(out.end, RunEnd::Stopped, "{out:?}");
    let out = node
        .adapter
        .research(request(&w, "tr_e", "default", "还能问吗"), &Seen::default())
        .await;
    assert_eq!(out.end, RunEnd::Failed);
    assert_eq!(out.error.as_deref(), Some("服务正在关闭，研究已停止"));
    assert_eq!(
        q::<i64>(
            &node.store,
            "SELECT COUNT(*) FROM runtime_jobs WHERE thread_run_id='tr_e'"
        ),
        0
    );
}

/// One run per workspace and `max_global` at once on the Node path too, with today's words.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn busy_and_full_are_refused_with_the_same_words() {
    let w = World::new();
    let node = Hub::new(&w, "node", true, 1).await;
    let r = request(&w, "tr_a", "default", "@hang 读");
    let stop = r.stop.clone();
    let a = node.adapter.clone();
    let run = tokio::spawn(async move { a.research(r, &Seen::default()).await });
    w.pid().await;
    let busy = node
        .adapter
        .research(request(&w, "tr_b", "default", "又问"), &Seen::default())
        .await;
    assert_eq!(
        busy.error.as_deref(),
        Some("这个工作空间已经有一个研究在跑")
    );
    let full = node
        .adapter
        .research(request(&w, "tr_c", "w2", "另一个空间"), &Seen::default())
        .await;
    assert_eq!(full.error.as_deref(), Some("同时在跑的研究太多，稍后再问"));
    stop.store(true, Ordering::SeqCst);
    assert_eq!(run.await.unwrap().end, RunEnd::Stopped);
    // the slot is free again
    let out = node
        .adapter
        .research(request(&w, "tr_f", "default", "现在呢"), &Seen::default())
        .await;
    assert_eq!(out.end, RunEnd::Done, "{out:?}");
}

/// The question's attachments and those of the message it follows up on reach the run on the
/// Node as they do in process: copied where it may read them and named in the prompt, the earlier
/// ones marked so.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn attachments_reach_the_run_with_their_marks() {
    let w = World::new();
    let legacy = Hub::new(&w, "legacy", false, 3).await;
    let node = Hub::new(&w, "node", true, 3).await;
    let mut notes = vec![];
    for h in [&legacy, &node] {
        w.reset();
        let mut r = request(&w, "tr_f", "default", "看看附件");
        r.files = vec![h.file("fil_a", "notes.md", "# 笔记")];
        r.earlier_files = vec![h.file("fil_b", "old.md", "# 旧的")];
        let out = h.adapter.research(r, &Seen::default()).await;
        assert_eq!(out.end, RunEnd::Done, "{out:?}");
        let args = w.read("args");
        let note: Vec<String> = args
            .lines()
            .filter(|l| l.starts_with("- ") && l.contains("text/markdown"))
            .map(|l| {
                // the copy's directory is the run's own
                let path = l.split("（text").next().unwrap_or(l);
                let file = path.rsplit('/').next().unwrap_or(path);
                let mark = if l.contains("（前面消息里的）") {
                    "earlier "
                } else {
                    ""
                };
                format!("{mark}{file}")
            })
            .collect();
        assert_eq!(note.len(), 2, "{args}");
        notes.push(note);
    }
    assert_eq!(notes[0], notes[1]);
    assert!(notes[1][1].starts_with("earlier old.md"), "{:?}", notes[1]);
}

/// Steve's tools through the Node answer a research job with the research catalog only, every
/// call checked again: a tool outside it is refused, and nothing answers once the run is no
/// longer running.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn research_tools_answer_only_a_running_research_job() {
    let w = World::new();
    let node = Hub::new(&w, "node", true, 3).await;
    node.thread_run("tr_t", "default");
    let mut r = request(&w, "tr_t", "default", "@hang 用工具");
    r.mcp = Some(steve_core::McpEndpoint {
        url: "http://127.0.0.1:9/mcp".into(),
        token: "unused".into(),
    });
    let stop = r.stop.clone();
    let a = node.adapter.clone();
    let run = tokio::spawn(async move { a.research(r, &Seen::default()).await });
    w.pid().await;
    // the harness was given the Node's own endpoint, never the Hub's /mcp
    let mcp = q::<String>(&node.store, "SELECT spec FROM runtime_job_attempts");
    assert!(mcp.contains("\"steve_tools\":true"), "{mcp}");
    let exec: ExecutionRef = serde_json::from_str(&q::<String>(
        &node.store,
        "SELECT execution FROM runtime_job_attempts",
    ))
    .unwrap();
    let tools = node.tools.clone().unwrap();
    let catalog = tools.catalog(exec.clone()).await.unwrap();
    let names: Vec<&str> = catalog.tools.iter().map(|t| t.name.as_str()).collect();
    assert!(names.contains(&"steve_items_search"), "{names:?}");
    assert!(!names.contains(&"steve_update"));
    assert!(catalog.tools.iter().all(|t| t.read_only));
    let call = |name: &str, n: u32| ToolCall {
        id: CallId::new(format!("call-{n}")).unwrap(),
        execution: exec.clone(),
        catalog: catalog.revision.clone(),
        name: name.into(),
        args: json!({"query": "add"}),
        deadline: Timestamp(steve_store::now() + 60_000),
    };
    match tools.call(call("steve_items_search", 1)).await.unwrap() {
        ToolResult::Completed { parts, .. } => {
            assert_eq!(
                parts,
                vec![Part::Text {
                    text: "steve_items_search for tr_t".into()
                }]
            )
        }
        other => panic!("{other:?}"),
    }
    let refused = tools.call(call("steve_update", 2)).await.unwrap_err();
    assert_eq!(refused.code, ErrorCode::Denied);
    // the run is over (the thread wrote its end): nothing more for this job
    node.store
        .transaction::<_, StoreError>(|tx| {
            tx.execute("UPDATE thread_runs SET status='done' WHERE id='tr_t'", [])?;
            Ok(())
        })
        .unwrap();
    assert_eq!(
        tools.catalog(exec.clone()).await.unwrap_err().code,
        ErrorCode::Denied
    );
    assert_eq!(
        tools
            .call(call("steve_items_search", 3))
            .await
            .unwrap_err()
            .code,
        ErrorCode::Denied
    );
    stop.store(true, Ordering::SeqCst);
    assert_eq!(run.await.unwrap().end, RunEnd::Stopped);
}

/// A setting without a wall time gives the job a deadline that never ends a live run (in process
/// such a run has no wall limit): not the old 6 h backstop the Node reported as the time budget.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_wall_time_is_no_deadline_on_the_node() {
    let w = World::new();
    let node = Hub::new(&w, "node", true, 3).await;
    let t0 = steve_store::now();
    let out = node
        .adapter
        .research(
            request(&w, "tr_w", "default", "add 怎么工作？"),
            &Seen::default(),
        )
        .await;
    assert_eq!(out.end, RunEnd::Done, "{out:?}");
    let deadline = q::<i64>(
        &node.store,
        "SELECT json_extract(spec, '$.run.limits.deadline') FROM runtime_job_attempts",
    );
    let far = i64::try_from(steve_hub::wire::research::NO_WALL_LIMIT.as_millis()).unwrap();
    assert!(
        deadline >= t0 + far,
        "deadline {deadline} is {} ms away",
        deadline - t0
    );
}

/// A run whose project is under none of the Node's roots, or whose roots cannot be read, goes in
/// process (it still answers), counted where `/healthz` shows it; a run under the roots is not.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_fallback_in_process_is_counted() {
    let w = World::new();
    let node = Hub::new(&w, "node", true, 3).await;
    let rt = node.rt.clone().unwrap();
    let health = || steve_hub::wire::node_runtime::health(&node.store, Some(&rt), false).unwrap();
    assert_eq!(health()["research_fallbacks"], 0);

    // under the roots: on the Node, nothing counted
    let out = node
        .adapter
        .research(
            request(&w, "tr_1", "default", "add 怎么工作？"),
            &Seen::default(),
        )
        .await;
    assert_eq!(out.end, RunEnd::Done, "{out:?}");
    assert_eq!(rt.research_fallbacks(), 0);

    // a project outside every root: in process, counted (a follow-up: the stand-in reads nothing)
    let outside = w.root.join("elsewhere");
    std::fs::create_dir_all(&outside).unwrap();
    let mut r = request(&w, "tr_2", "default", "然后呢？");
    for p in r.project.iter_mut().chain(r.readable.iter_mut()) {
        p.path = outside.display().to_string();
    }
    r.resume = Some(ResumeFrom {
        run: "tr_1".into(),
        claude_session: "sess-1".into(),
        model: "m1".into(),
        prior_answer: "add 什么都不做。".into(),
    });
    let out = node.adapter.research(r, &Seen::default()).await;
    assert_eq!(out.end, RunEnd::Done, "{out:?}");
    assert_eq!(rt.research_fallbacks(), 1);
    assert_eq!(
        q::<i64>(
            &node.store,
            "SELECT COUNT(*) FROM runtime_jobs WHERE thread_run_id='tr_2'"
        ),
        0
    );

    // the roots cannot be read: in process too, counted, not hidden as "no roots"
    *node.roots.lock().unwrap() = Err("store: locked".into());
    let out = node
        .adapter
        .research(
            request(&w, "tr_3", "default", "add 怎么工作？"),
            &Seen::default(),
        )
        .await;
    assert_eq!(out.end, RunEnd::Done, "{out:?}");
    assert_eq!(rt.research_fallbacks(), 2);
    assert_eq!(health()["research_fallbacks"], 2);
}
