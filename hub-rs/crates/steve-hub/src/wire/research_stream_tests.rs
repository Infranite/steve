use super::*;
use crate::wire::node_runtime::{NodeDomain, Sink};
use std::sync::atomic::AtomicBool;
use steve_core::thread::{Researcher, ResumeFrom};
use steve_node::{LocalNode, NodeConfig, Storage};
use steve_node_api::*;
use steve_store::{Store, StoreError};

struct Domain;
impl NodeDomain for Domain {
    fn turn_ended(&self, _: &str, _: Option<&str>, _: &str, _: Option<String>) {}
}

#[derive(Default)]
struct Seen(Mutex<Vec<String>>);
impl ResearchSink for Seen {
    fn text(&self, text: &str) {
        self.0.lock().unwrap().push(text.into());
    }
    fn tool(&self, _: &ToolSeen) {}
}

struct Test {
    _dir: tempfile::TempDir,
    root: PathBuf,
    research: ResearchAdapter,
    runtime: Arc<NodeRuntime>,
    node: Arc<LocalNode>,
    store: Store,
}

impl Test {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let project = root.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let store = Store::open(root.join("hub.sqlite"), steve_store::FnPublisher(|_| {})).unwrap();
        store.ensure_workspace().unwrap();
        let cfg = NodeConfig {
            ceiling: steve_node::PolicyCeiling::unlimited(),
            node: NodeId::new("hub").unwrap(),
            home: root.join("node"),
            acp: steve_acp::AcpConfig {
                declared: Default::default(),
                adapters: [(
                    "test-research".into(),
                    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                        .join("tests/fixtures/research-acp.js"),
                )]
                .into_iter()
                .collect(),
                node: "node".into(),
                claude_executable: "unused".into(),
                steve_home: root.join("state"),
                codex_home: root.join("adapter-state"),
                user_home: root.clone(),
                version: "test".into(),
                idle_release: Duration::from_secs(60),
                retry_delays: vec![],
            },
            roots: vec![steve_node::Root {
                id: RootId::new("project").unwrap(),
                display: "project".into(),
                path: project.clone(),
                access: RootAccess::ReadOnly,
            }],
            capacity: Capacity {
                sessions: 0,
                jobs: 2,
                execs: 0,
            },
            storage: Storage::default(),
        };
        let node = Arc::new(LocalNode::open(cfg).unwrap());
        let mut research = steve_research::ResearchConfig::new("unused", root.join("scratch"));
        research.home = root.clone();
        research.claude_dir = root.join("adapter-state");
        research.files_root = root.join("files");
        node.enable_research(research.clone());
        let runtime = NodeRuntime::new(store.clone(), node.clone(), Arc::new(Domain));
        node.connect(Arc::new(Sink(Arc::downgrade(&runtime))), None);
        runtime.start();
        tokio::time::timeout(Duration::from_secs(10), async {
            while !runtime.is_reconciled() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
        Self {
            _dir: dir,
            root: root.clone(),
            research: ResearchAdapter::new(
                research,
                steve_core::repo::Db::new(store.clone()),
                root.join("data"),
            )
            .on_node(NodeResearch {
                runtime: runtime.clone(),
                roots: Arc::new(move || {
                    Ok(vec![steve_node::Root {
                        id: RootId::new("project").unwrap(),
                        display: "project".into(),
                        path: project.clone(),
                        access: RootAccess::ReadOnly,
                    }])
                }),
            }),
            runtime,
            node,
            store,
        }
    }

    fn project(&self, device: &str) -> steve_core::repo::Project {
        steve_core::repo::Project {
            id: format!("project-{device}"),
            workspace_id: steve_store::WS.into(),
            name: device.into(),
            description: String::new(),
            device_id: device.into(),
            path: if device == "hub" {
                self.root.join("project").display().to_string()
            } else {
                "/remote/unavailable".into()
            },
            kind: "dir".into(),
            worktree: false,
            default_bot: None,
            release: None,
            archived: false,
            created: 1,
            updated: 1,
        }
    }

    fn request(&self, run: &str, models: &[&str]) -> ResearchRequest {
        ResearchRequest {
            run: run.into(),
            ws: steve_store::WS.into(),
            mid: "message".into(),
            user: None,
            question: "@wire-check".into(),
            history: vec![],
            main: steve_core::repo::Bot {
                id: "main".into(),
                workspace_id: steve_store::WS.into(),
                name: "Steve".into(),
                role: String::new(),
                main: true,
                harness: "test-research".into(),
                model: None,
                rules: String::new(),
                accept_mode: String::new(),
                perm_mode: "plan".into(),
                enabled: true,
                revision: 1,
                updated: 1,
                persona: String::new(),
                proactive: 1,
                avatar: Default::default(),
                default_project: None,
                prompt: None,
                device: None,
                reasoning_effort: None,
            },
            context: String::new(),
            project: Some(self.project("hub")),
            readable: vec![self.project("hub")],
            files: vec![],
            earlier_files: vec![],
            resume: None,
            stop: Arc::new(AtomicBool::new(false)),
            mcp: None,
            models: steve_core::models::Choice {
                harness: "test-research".into(),
                models: models.iter().map(|m| m.to_string()).collect(),
                effort: None,
            },
            ui: false,
        }
    }

    fn specs(&self) -> Vec<JobSpec> {
        self.store
            .reader(|c| {
                let mut st =
                    c.prepare("SELECT spec FROM runtime_job_attempts ORDER BY created, rowid")?;
                let specs = st
                    .query_map([], |r| r.get::<_, String>(0))?
                    .collect::<Result<Vec<_>, _>>()?;
                Ok::<_, StoreError>(
                    specs
                        .into_iter()
                        .map(|s| serde_json::from_str(&s).unwrap())
                        .collect(),
                )
            })
            .unwrap()
    }

    async fn run(&self, q: ResearchRequest, sink: &Seen) -> ResearchOutcome {
        // This is a safety guard for real Node processes, not a response-latency assertion.
        // A fallback opens and reaps two ACP peers; allow busy CI scheduling without
        // retrying the run or changing its completion/order/content assertions.
        let limit = Duration::from_secs(60);
        let run = q.run.clone();
        match tokio::time::timeout(limit, self.research.research(q, sink)).await {
            Ok(out) => out,
            Err(_) => {
                let state = self.store.reader(|c| {
                    let mut st = c.prepare(
                        "SELECT a.attempt_id,a.model,a.state,a.cancel_requested,
                         json_extract(a.completion,'$.outcome'),
                         c.method,c.state FROM runtime_job_attempts a
                         JOIN runtime_jobs j ON j.job_id=a.job_id
                         LEFT JOIN node_commands c ON c.target_job_attempt=a.attempt_id
                         WHERE j.thread_run_id=? ORDER BY a.created,a.rowid,c.created,c.rowid",
                    )?;
                    st.query_map([&run], |r| {
                        Ok((
                            r.get::<_, String>(0)?,
                            r.get::<_, String>(1)?,
                            r.get::<_, String>(2)?,
                            r.get::<_, bool>(3)?,
                            r.get::<_, Option<String>>(4)?,
                            r.get::<_, Option<String>>(5)?,
                            r.get::<_, Option<String>>(6)?,
                        ))
                    })?
                    .collect::<Result<Vec<_>, _>>()
                    .map_err(StoreError::from)
                });
                let text = sink.0.lock().unwrap().clone();
                // Best-effort fixture cleanup, still bounded if the Node itself is stuck.
                // Dropping the timed-out future may already have requested cancellation.
                let cleanup = tokio::time::timeout(Duration::from_secs(5), self.close()).await;
                panic!(
                    "research {run} did not complete within {limit:?}; \
                     attempts (id,model,state,cancelled,outcome,command,state): {state:?}; \
                     received text: {text:?}; cleanup: {cleanup:?}"
                );
            }
        }
    }

    async fn close(&self) {
        self.runtime.stop("test finished");
        self.node.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn research_model_failure_falls_back_before_any_text_or_tools() {
    let t = Test::new().await;
    let seen = Seen::default();
    // Repeated quick adapter errors exercise exit/reap races before the next model starts.
    for n in 0..8 {
        let out = t
            .run(
                t.request(&format!("run-fallback-{n}"), &["unavailable", "ready"]),
                &seen,
            )
            .await;
        assert_eq!(out.end, RunEnd::Done, "{out:?}");
        assert_eq!(out.model.as_deref(), Some("ready"));
        assert_eq!(out.text, "answer");
    }
    assert_eq!(*seen.0.lock().unwrap(), vec!["answer"; 8]);
    assert_eq!(
        t.specs()
            .iter()
            .map(|s| s.run.choice.model.clone().unwrap())
            .collect::<Vec<_>>(),
        ["unavailable", "ready"].repeat(8)
    );
    t.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn expired_research_continuation_starts_fresh_with_prior_answer() {
    let t = Test::new().await;
    let first = t
        .run(t.request("run-original", &["ready"]), &Seen::default())
        .await;
    assert_eq!(first.end, RunEnd::Done, "{first:?}");
    let mut q = t.request("run-followup", &["ready"]);
    q.resume = Some(ResumeFrom {
        run: "run-original".into(),
        claude_session: first.claude_session.unwrap(),
        model: "ready".into(),
        prior_answer: first.text,
    });
    let out = t.run(q, &Seen::default()).await;
    assert_eq!(out.end, RunEnd::Done, "{out:?}");
    assert_eq!(out.text, "fresh answer with prior context");
    let specs = t.specs();
    assert_eq!(specs.len(), 3, "new, failed load, fresh");
    assert!(specs[0].run.continuation.is_none());
    assert_eq!(
        specs[1].run.continuation.as_ref().map(|c| c.as_str()),
        Some("expired-session")
    );
    assert!(specs[2].run.continuation.is_none());
    t.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn remote_readable_projects_do_not_block_local_research() {
    let t = Test::new().await;
    let mut q = t.request("run-remote", &["ready"]);
    q.readable.push(t.project("remote"));
    let out = t.run(q, &Seen::default()).await;
    assert_eq!(out.end, RunEnd::Done, "{out:?}");
    let specs = t.specs();
    let JobWork::Research(job) = &specs[0].work else {
        panic!("not research")
    };
    assert!(job.readable.is_empty(), "the cwd is already readable");
    assert!(job.cwd.is_some());
    t.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_remote_project_is_not_read_as_a_local_path() {
    let t = Test::new().await;
    let mut q = t.request("run-remote-project", &["ready"]);
    // Its path deliberately exists locally: device identity must still forbid that substitution.
    let mut remote = t.project("remote");
    remote.path = t.project("hub").path;
    q.project = Some(remote);
    let out = t.run(q, &Seen::default()).await;
    assert_eq!(out.end, RunEnd::Failed, "{out:?}");
    assert!(out.error.unwrap().contains("另一节点"));
    assert!(t.specs().is_empty());
    t.close().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stopped_runtime_returns_partial_research_and_persists_cancellation() {
    let t = Test::new().await;
    let seen = Seen::default();
    let pending = t.run(t.request("run-stop", &["slow"]), &seen);
    tokio::pin!(pending);
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            tokio::select! {
                out = &mut pending => panic!("ended before runtime stopped: {out:?}"),
                _ = tokio::time::sleep(Duration::from_millis(10)) => {
                    if !seen.0.lock().unwrap().is_empty() { break; }
                }
            }
        }
    })
    .await
    .unwrap();
    t.runtime.stop("test connection lost");
    let out = tokio::time::timeout(Duration::from_secs(3), &mut pending)
        .await
        .unwrap();
    assert_eq!(out.end, RunEnd::Failed, "{out:?}");
    assert!(out.text.starts_with("answer"));
    assert!(out.text.contains("Node 连接中断"));
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let cancelled = t
                .store
                .reader(|c| {
                    Ok::<_, StoreError>(c.query_row(
                        "SELECT cancel_requested FROM runtime_job_attempts",
                        [],
                        |r| r.get::<_, bool>(0),
                    )?)
                })
                .unwrap();
            if cancelled {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    t.close().await;
}
