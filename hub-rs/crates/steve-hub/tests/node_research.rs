//! Inline research as jobs on the local Node (N0d-9): a run's attempts as research jobs on a
//! scripted ACP adapter (`steve-node`'s research-script.mjs, no model): the outcome, drafts, tool
//! rows, statistics row and audit; a follow-up continues the session; admission, the stop button
//! and shutdown; Steve's research tools answer a research job only, and only while its run is
//! running; and nothing runs without the Node or outside its roots.
#![cfg(unix)]

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use steve_core::thread::{ResearchRequest, ResearchSink, Researcher, ResumeFrom, RunEnd, ToolSeen};
use steve_hub::wire::node_files::NodeFiles;
use steve_hub::wire::node_runtime::{NodeDomain, NodeRuntime, Sink};
use steve_hub::wire::node_tools::{NodeTools, Service};
use steve_hub::wire::research::{NodeResearch, ResearchAdapter};
use steve_node_api::*;
use steve_store::{FnPublisher, Store, StoreError};

/// The offer line of the scripted answers.
const OFFER: &str = "OFFER: {\"title\":\"实现 add\",\"goal\":\"让 add 返回和\"}";

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
    async fn research_catalog(&self, _: &steve_store::ThreadRun) -> Vec<ToolDescriptor> {
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
    /// calls that began: (name, kind, brief)
    running: Mutex<Vec<(String, String, String)>>,
}
impl ResearchSink for Seen {
    fn text(&self, t: &str) {
        self.text.lock().unwrap().push(t.into());
    }
    fn tool(&self, t: &ToolSeen) {
        self.tools.lock().unwrap().push(t.clone());
    }
    fn tool_running(&self, name: &str, kind: &str, brief: &str) {
        self.running
            .lock()
            .unwrap()
            .push((name.into(), kind.into(), brief.into()));
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

/// The shared files: the project and the scripted adapter.
struct World {
    _dir: tempfile::TempDir,
    root: PathBuf,
    proj: PathBuf,
    log: PathBuf,
    adapter: PathBuf,
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
        let adapter = root.join("adapter.sh");
        let script = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../steve-node/tests/research-script.mjs");
        std::fs::write(
            &adapter,
            format!(
                "#!/bin/sh\nexport FAKE_RESEARCH_LOG='{}'\nexec node '{}'\n",
                log.display(),
                script.display()
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&adapter, std::fs::Permissions::from_mode(0o755)).unwrap();
        World {
            _dir: dir,
            root,
            proj,
            log,
            adapter,
        }
    }

    fn read(&self, what: &str) -> String {
        std::fs::read_to_string(format!("{}.{what}", self.log.display())).unwrap_or_default()
    }

    fn spawns(&self) -> usize {
        self.read("spawns").lines().count()
    }

    fn reset(&self) {
        for what in ["spawns", "config", "prompt", "pid", "load", "new"] {
            let _ = std::fs::remove_file(format!("{}.{what}", self.log.display()));
        }
    }

    /// The text of the last prompt.
    fn prompt(&self) -> String {
        let v: Value = serde_json::from_str(&self.read("prompt")).unwrap_or(Value::Null);
        v[0]["text"].as_str().unwrap_or_default().to_string()
    }

    async fn pid(&self) -> i32 {
        until("the adapter's child", || {
            !self.read("pid").trim().is_empty()
        })
        .await;
        self.read("pid").trim().parse().unwrap()
    }

    /// The question that answers with a read of the project and an offer, its usage reported.
    fn answering(&self, question: &str) -> String {
        format!(
            "{question}\n@msg m1\n@say 我先看看\n@read {}\n@msg m2\n@say add 什么都不做。\\n{OFFER}\n@usage 3 2 5 0.25",
            self.proj.join("src/lib.rs").display()
        )
    }
}

/// A question whose run writes a little and then waits (with a child) until it is stopped.
fn hanging(question: &str) -> String {
    format!("{question}\n@msg m1\n@say 写到一半\n@child\n@wait")
}

/// One Hub (its own store) with research on its local Node, or with no Node at all.
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
        Hub::with(
            w,
            name,
            on_node,
            max_global,
            steve_research::SandboxMode::Off,
        )
        .await
    }

    /// [`Hub::new`] whose research asks the Node for `sandbox`.
    async fn with(
        w: &World,
        name: &str,
        on_node: bool,
        max_global: usize,
        sandbox: steve_research::SandboxMode,
    ) -> Hub {
        let base = w.root.join(name);
        let data = base.join("data");
        std::fs::create_dir_all(&data).unwrap();
        let store = Store::open(base.join("steve.sqlite3"), FnPublisher(|_| {})).unwrap();
        store.ensure_workspace().unwrap();
        let mut cfg = steve_research::ResearchConfig::new(base.join("scratch"));
        cfg.models = vec!["m1".into()];
        cfg.home = w.root.join("home");
        cfg.files_root = base.join("files");
        cfg.sandbox = sandbox;
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
        // the scripted adapter as Steve's default one (research follows Steve's harness)
        let id = steve_acp::adapters::DEFAULT_ADAPTER.0.to_string();
        let node = Arc::new(
            steve_node::LocalNode::open(steve_node::NodeConfig {
                ceiling: steve_node::PolicyCeiling::unlimited(),
                node: NodeId::new("local").unwrap(),
                home: base.join("node"),
                acp: steve_acp::AcpConfig {
                    declared: HashMap::from([(
                        id.clone(),
                        serde_json::from_value(json!({"properties": {"nested_sandbox": false}}))
                            .unwrap(),
                    )]),
                    adapters: HashMap::from([(id, w.adapter.clone())]),
                    node: "node".into(),
                    claude_executable: "claude".into(),
                    steve_home: base.join("steve"),
                    codex_home: base.join("codex-home"),
                    user_home: w.root.clone(),
                    version: "test".into(),
                },
                roots: roots.clone(),
                capacity: Capacity {
                    sessions: 0,
                    jobs: steve_hub::wire::LOCAL_JOB_CAPACITY,
                    execs: 0,
                },
                storage: steve_node::Storage::default(),
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
        full: false,
        brief: String::new(),
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

fn tool_rows(s: &Seen) -> Vec<(String, String, bool, Option<String>)> {
    s.tools
        .lock()
        .unwrap()
        .iter()
        .map(|t| (t.name.clone(), t.brief.clone(), t.ok, t.error.clone()))
        .collect()
}

/// A question on the Node: the outcome (answer without its offer line, the offer, end, model,
/// session, turns), drafts, tool rows and statistics row. A busy model falls back to the next one,
/// and a follow-up continues the session with its model. Each attempt is one research job of its
/// thread run.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_run_on_the_node_answers_falls_back_and_continues() {
    let w = World::new();
    let node = Hub::new(&w, "node", true, 3).await;
    let seen = Seen::default();
    let out = node
        .adapter
        .research(
            request(&w, "tr_1", "default", &w.answering("add 怎么工作？")),
            &seen,
        )
        .await;
    assert_eq!(out.end, RunEnd::Done, "{out:?}");
    assert_eq!(w.spawns(), 1);
    assert_eq!(w.read("config"), "model=m1\n");
    assert!(w.prompt().contains("add 怎么工作？"));
    assert!(w.prompt().contains("说重点。"));
    assert!(out.first_text_ms.is_some());
    assert_eq!(out.text, "add 什么都不做。");
    assert_eq!(out.offer.as_ref().unwrap().title, "实现 add");
    assert_eq!(out.session.as_deref(), Some("sess-1"));
    assert_eq!(out.model.as_deref(), Some("m1"));
    assert_eq!(out.turns, 2);
    assert_eq!(
        seen.text.lock().unwrap().last().map(String::as_str),
        Some(format!("add 什么都不做。\n{OFFER}").as_str())
    );
    let tools = tool_rows(&seen);
    assert_eq!(tools.len(), 1, "{tools:?}");
    assert!(tools[0].2);
    let booked = rows(&node.store);
    assert_eq!(booked.len(), 1);
    assert_eq!(booked[0][1], "research");
    assert_eq!(booked[0][2], "claude-code");
    assert_eq!(booked[0][3], "m1");
    assert_eq!(booked[0][4], 3);
    assert_eq!(booked[0][9], 0.25);
    // one research job of the thread run, finished
    assert_eq!(
        q::<String>(
            &node.store,
            "SELECT kind||' '||purpose||' '||thread_run_id||' '||state FROM runtime_jobs"
        ),
        "research research tr_1 finished"
    );

    // a busy first model: the next one answers
    w.reset();
    let mut r = request(
        &w,
        "tr_2",
        "default",
        &w.answering("add 怎么工作？\n@failon m-busy API Error: 503 overloaded"),
    );
    r.models = steve_core::models::Choice {
        harness: "claude-code".into(),
        models: vec!["m-busy".into(), "m2".into()],
        effort: Some("max".into()),
    };
    let out = node.adapter.research(r, &Seen::default()).await;
    assert_eq!(out.end, RunEnd::Done, "{out:?}");
    assert_eq!(w.spawns(), 2);
    assert!(w.read("config").contains("effort=max"));
    assert_eq!(out.model.as_deref(), Some("m2"));
    assert_eq!(
        q::<i64>(
            &node.store,
            "SELECT COUNT(*) FROM runtime_jobs WHERE thread_run_id='tr_2'"
        ),
        2
    );

    // a follow-up continues the session with the model that answered, which is still one to try
    w.reset();
    let mut r = request(&w, "tr_3", "default", "然后呢？\n@msg m3\n@say 接着说。");
    r.models = steve_core::models::Choice {
        harness: "claude-code".into(),
        models: vec!["m-busy".into(), "m2".into()],
        effort: Some("max".into()),
    };
    r.resume = Some(ResumeFrom {
        run: "tr_2".into(),
        session: "sess-1".into(),
        harness: "claude-code".into(),
        model: "m2".into(),
    });
    let out = node.adapter.research(r, &Seen::default()).await;
    assert_eq!(out.end, RunEnd::Done, "{out:?}");
    assert_eq!(w.spawns(), 1);
    assert_eq!(w.read("load"), "sess-1");
    assert!(w.read("config").contains("model=m2"));
    assert_eq!(out.text, "接着说。");
}

/// A call that is no read ends the run as a violation, audited.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_violation_ends_and_is_audited() {
    let w = World::new();
    let node = Hub::new(&w, "node", true, 3).await;
    let out = node
        .adapter
        .research(
            request(
                &w,
                "tr_v",
                "default",
                "跑一下\n@asking execute touch x\n@sleep 5000",
            ),
            &Seen::default(),
        )
        .await;
    assert_eq!(out.end, RunEnd::Violated, "{out:?}");
    let audit = q::<String>(
        &node.store,
        "SELECT action||' '||target||' '||detail FROM audit WHERE action='thread.research_violation'",
    );
    assert!(audit.contains("execute"), "{audit}");
}

/// What ends a read-only run is what the owner's own run is for: with `full` the job is sent with
/// full access and no read-only sandbox (a Node refuses the two together), and without a project
/// it starts in the user's home.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_owners_own_run_acts_on_the_machine() {
    let w = World::new();
    let node = Hub::with(&w, "node", true, 3, steve_research::SandboxMode::Auto).await;
    let made = w.root.join("made-by-the-run");
    let question = format!(
        "整理一下\n@msg m1\n@asking execute touch x\n@touch {}\n@msg m2\n@say 整理好了",
        made.display()
    );
    let mut req = request(&w, "tr_f", "default", &question);
    req.full = true;
    let seen = Seen::default();
    let out = node.adapter.research(req, &seen).await;
    assert_eq!(out.end, RunEnd::Done, "{out:?}");
    assert_eq!(out.text, "整理好了");
    // the command is said while it runs (here it never ends): once, with its category
    let running = seen.running.lock().unwrap().clone();
    assert_eq!(running.len(), 1, "{running:?}");
    assert_eq!(
        (running[0].1.as_str(), running[0].2.as_str()),
        ("exec", "touch x"),
        "{running:?}"
    );
    assert!(tool_rows(&seen).is_empty());
    assert!(made.exists(), "outside the project, and written");
    let new: Value = serde_json::from_str(&w.read("new")).unwrap();
    assert_eq!(
        new["cwd"],
        w.proj.to_str().unwrap(),
        "the project it is about"
    );
    assert_eq!(
        q::<i64>(
            &node.store,
            "SELECT count(*) FROM audit WHERE action='thread.research_violation'"
        ),
        0
    );

    w.reset();
    let mut req = request(
        &w,
        "tr_h",
        "default",
        "磁盘还剩多少\n@msg m1\n@say 还有一半",
    );
    req.full = true;
    req.project = None;
    req.readable = vec![];
    let out = node.adapter.research(req, &Seen::default()).await;
    assert_eq!(out.end, RunEnd::Done, "{out:?}");
    let new: Value = serde_json::from_str(&w.read("new")).unwrap();
    assert_eq!(new["cwd"], w.root.to_str().unwrap(), "the user's home");
}

/// The stop button ends the job on the Node with what it wrote, its processes gone; shutdown says
/// the processes are gone only once they are, and admits nothing after.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn stop_and_shutdown_end_the_harness() {
    let w = World::new();
    let node = Hub::new(&w, "node", true, 3).await;
    let r = request(&w, "tr_s", "default", &hanging("慢慢读"));
    let stop = r.stop.clone();
    let a = node.adapter.clone();
    let run = tokio::spawn(async move { a.research(r, &Seen::default()).await });
    let pid = w.pid().await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    stop.store(true, Ordering::SeqCst);
    let out = tokio::time::timeout(Duration::from_secs(20), run)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(out.end, RunEnd::Stopped, "{out:?}");
    assert!(!alive(pid), "the adapter's child is gone");
    assert!(out.text.starts_with("写到一半"), "{:?}", out.text);

    // shutdown: the running adapter is stopped and gone before it says true
    w.reset();
    let a = node.adapter.clone();
    let r = request(&w, "tr_d", "default", &hanging("再读"));
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

/// One run per workspace and `max_global` at once.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn busy_and_full_are_refused() {
    let w = World::new();
    let node = Hub::new(&w, "node", true, 1).await;
    let r = request(&w, "tr_a", "default", &hanging("读"));
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
        .research(
            request(&w, "tr_f", "default", &w.answering("现在呢")),
            &Seen::default(),
        )
        .await;
    assert_eq!(out.end, RunEnd::Done, "{out:?}");
}

/// The question's attachments and those of the message it follows up on reach the run: copied
/// where it may read them and named in the prompt, the earlier ones marked so.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn attachments_reach_the_run_with_their_marks() {
    let w = World::new();
    let node = Hub::new(&w, "node", true, 3).await;
    let mut r = request(&w, "tr_f", "default", &w.answering("看看附件"));
    r.files = vec![node.file("fil_a", "notes.md", "# 笔记")];
    r.earlier_files = vec![node.file("fil_b", "old.md", "# 旧的")];
    let out = node.adapter.research(r, &Seen::default()).await;
    assert_eq!(out.end, RunEnd::Done, "{out:?}");
    let prompt = w.prompt();
    let notes: Vec<String> = prompt
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
    assert_eq!(notes.len(), 2, "{prompt}");
    assert!(notes[0].starts_with("notes.md"), "{notes:?}");
    assert!(notes[1].starts_with("earlier old.md"), "{notes:?}");
}

/// Steve's tools through the Node answer a research job with the research catalog only, every
/// call checked again: a tool outside it is refused, and nothing answers once the run is no
/// longer running.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn research_tools_answer_only_a_running_research_job() {
    let w = World::new();
    let node = Hub::new(&w, "node", true, 3).await;
    node.thread_run("tr_t", "default");
    let mut r = request(&w, "tr_t", "default", &hanging("用工具"));
    r.mcp = Some(steve_core::McpEndpoint {
        url: "http://127.0.0.1:9/mcp".into(),
        token: "unused".into(),
    });
    let stop = r.stop.clone();
    let a = node.adapter.clone();
    let run = tokio::spawn(async move { a.research(r, &Seen::default()).await });
    w.pid().await;
    // the adapter was given the Node's own endpoint, never the Hub's /mcp
    let mcp = q::<String>(&node.store, "SELECT spec FROM runtime_job_attempts");
    assert!(mcp.contains("\"steve_tools\":true"), "{mcp}");
    let new: Value = serde_json::from_str(&w.read("new")).unwrap();
    let url = new["mcpServers"][0]["url"].as_str().unwrap_or_default();
    assert!(
        url.starts_with("http://127.0.0.1:") && !url.contains(":9/"),
        "{new}"
    );
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

/// A setting without a wall time gives the job a deadline that never ends a live run: not the
/// old 6 h backstop the Node reported as the time budget.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_wall_time_is_no_deadline_on_the_node() {
    let w = World::new();
    let node = Hub::new(&w, "node", true, 3).await;
    let t0 = steve_store::now();
    let out = node
        .adapter
        .research(
            request(&w, "tr_w", "default", &w.answering("add 怎么工作？")),
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

/// A run whose project is under none of the Node's roots, or whose roots cannot be read, fails
/// with a reason, counted where `/healthz` shows it; a Hub without its Node has no research.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn what_the_node_cannot_read_fails_and_is_counted() {
    let w = World::new();
    let node = Hub::new(&w, "node", true, 3).await;
    let rt = node.rt.clone().unwrap();
    let health = || steve_hub::wire::node_runtime::health(&node.store, Some(&rt), false).unwrap();
    assert_eq!(health()["research_fallbacks"], 0);

    // under the roots: on the Node, nothing counted
    let out = node
        .adapter
        .research(
            request(&w, "tr_1", "default", &w.answering("add 怎么工作？")),
            &Seen::default(),
        )
        .await;
    assert_eq!(out.end, RunEnd::Done, "{out:?}");
    assert_eq!(rt.research_fallbacks(), 0);

    // a project outside every root: refused, counted, no job
    let outside = w.root.join("elsewhere");
    std::fs::create_dir_all(&outside).unwrap();
    let mut r = request(&w, "tr_2", "default", "然后呢？");
    for p in r.project.iter_mut().chain(r.readable.iter_mut()) {
        p.path = outside.display().to_string();
    }
    let out = node.adapter.research(r, &Seen::default()).await;
    assert_eq!(out.end, RunEnd::Failed, "{out:?}");
    assert!(out.error.is_some());
    assert_eq!(rt.research_fallbacks(), 1);
    assert_eq!(
        q::<i64>(
            &node.store,
            "SELECT COUNT(*) FROM runtime_jobs WHERE thread_run_id='tr_2'"
        ),
        0
    );

    // the roots cannot be read: refused too, counted, not hidden as "no roots"
    *node.roots.lock().unwrap() = Err("store: locked".into());
    let out = node
        .adapter
        .research(
            request(&w, "tr_3", "default", "add 怎么工作？"),
            &Seen::default(),
        )
        .await;
    assert_eq!(out.end, RunEnd::Failed, "{out:?}");
    assert_eq!(rt.research_fallbacks(), 2);
    assert_eq!(health()["research_fallbacks"], 2);

    // no local Node: no research, said so
    let off = Hub::new(&w, "off", false, 3).await;
    let out = off
        .adapter
        .research(request(&w, "tr_4", "default", "问"), &Seen::default())
        .await;
    assert_eq!(out.end, RunEnd::Failed);
    assert!(out.error.is_some());
}
