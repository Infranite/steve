//! Finite Node peers pin durable admission and one-model result collection.
use std::{
    sync::{Arc, Mutex},
    time::Duration,
};
use steve_core::gateway::{OneCall, OneModel, Readable, ThinkTools, with_static};
use steve_hub::wire::node_runtime::{
    News, NodeDomain, NodePrint, NodeRuntime, ResearchAttempt, ResearchFile, Sink,
};
use steve_node_api::{
    testkit::{FakeNode, build},
    *,
};
use steve_runtime::testkit::eventually_sync;
use steve_store::{NewFile, Store, StoreError};

struct Domain;
impl NodeDomain for Domain {
    fn turn_ended(&self, _: &str, _: Option<&str>, _: &str, _: Option<String>) {}
}
struct JobHub {
    rt: Arc<NodeRuntime>,
    node: Arc<FakeNode>,
    store: Store,
    dir: tempfile::TempDir,
}
impl JobHub {
    async fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("jobs.sqlite3"), steve_store::NoPublisher).unwrap();
        store.ensure_workspace().unwrap();
        let node = FakeNode::new("sg38-node");
        let rt = NodeRuntime::new(store.clone(), node.clone(), Arc::new(Domain));
        node.connect(Arc::new(Sink(Arc::downgrade(&rt))));
        rt.start();
        eventually_sync(
            "the fixture coordinator reconciled before accepting model jobs",
            || rt.is_reconciled(),
        )
        .await;
        Self {
            rt,
            node,
            store,
            dir,
        }
    }
    fn q<T: rusqlite::types::FromSql>(&self, sql: &str) -> T {
        self.store
            .reader(|c| Ok::<_, StoreError>(c.query_row(sql, [], |r| r.get(0))?))
            .unwrap()
    }
    fn file(&self, id: &str, ws: &str, size: i64) {
        self.store
            .files()
            .create(&NewFile {
                id: id.into(),
                workspace_id: ws.into(),
                name: "evidence.txt".into(),
                mime: "text/plain".into(),
                sha256: "ab".repeat(32),
                size,
                created_by: "fixture".into(),
            })
            .unwrap();
    }
    async fn running(&self) -> (ExecutionRef, JobSpec) {
        eventually_sync("the Node accepted the durable job and its attempt", || {
            self.q::<i64>("SELECT COUNT(*) FROM runtime_job_attempts WHERE state='running'") == 1
        })
        .await;
        let exec = self
            .q::<String>("SELECT execution FROM runtime_job_attempts ORDER BY rowid DESC LIMIT 1");
        let spec =
            self.q::<String>("SELECT spec FROM runtime_job_attempts ORDER BY rowid DESC LIMIT 1");
        (
            serde_json::from_str(&exec).unwrap(),
            serde_json::from_str(&spec).unwrap(),
        )
    }
    fn print(&self) -> NodePrint {
        NodePrint {
            runtime: self.rt.clone(),
            data_dir: self.dir.path().join("data"),
        }
    }
}
impl Drop for JobHub {
    fn drop(&mut self) {
        self.rt.stop("fixture done");
    }
}
fn report(id: &str, measurement: Measurement, cost: &str) -> UsageReport {
    UsageReport {
        report_id: id.into(),
        meter_epoch: Epoch::new("meter").unwrap(),
        model: Some("reported-model".into()),
        scope: UsageScope::Main,
        measurement,
        tokens: Tokens {
            input: Some(3),
            output: Some(2),
            ..Default::default()
        },
        context_used: None,
        context_limit: None,
        cost: Some(Money {
            decimal: cost.into(),
            currency: "USD".into(),
        }),
    }
}

#[tokio::test]
async fn think_spec_stream_and_final_accounting_keep_their_original_ownership() {
    let h = JobHub::new().await;
    h.file("file-evidence", "default", 12);
    let prompt = with_static("stable instructions", "current question");
    let tools = ThinkTools::Read(vec![Readable {
        file_id: "file-evidence".into(),
        rel_path: ".steve/attachments/evidence.txt".into(),
    }]);
    let call = OneCall {
        prompt: &prompt,
        timeout: Duration::from_secs(10),
        tools: &tools,
        harness: "claude-code",
        images: &[],
    };
    let shown = Mutex::new(Vec::new());
    let callback = |s: &str| shown.lock().unwrap().push(s.to_string());
    let print = h.print();
    let run = print.once(&call, "sonnet", Some("high"), Some(&callback));
    let drive = async {
        let (exec, spec) = h.running().await;
        assert_eq!(spec.run.choice.harness, "claude-code");
        assert_eq!(spec.run.choice.model.as_deref(), Some("sonnet"));
        assert_eq!(spec.run.choice.effort.as_deref(), Some("high"));
        let JobWork::Think(work) = spec.work else {
            panic!("think work required")
        };
        assert_eq!(work.instructions.as_deref(), Some("stable instructions"));
        assert!(matches!(work.tools, ThinkToolMode::ReadAttached));
        assert_eq!(
            spec.run.prompt[0],
            Part::Text {
                text: "current question".into()
            }
        );
        let Part::File { artifact, name } = &spec.run.prompt[1] else {
            panic!("attachment required")
        };
        assert_eq!(
            (artifact.id.as_str(), artifact.size, name.as_str()),
            ("file-evidence", 12, ".steve/attachments/evidence.txt")
        );
        assert_eq!(spec.run.policy.tools.as_str(), "hub");
        assert_eq!(
            h.q::<String>("SELECT workspace_id FROM runtime_jobs"),
            "default"
        );
        assert_eq!(h.q::<i64>("SELECT COUNT(*) FROM gateway_job_accounts"), 1);
        h.node
            .draft(&exec, "draft", &["first", " answer"])
            .await
            .unwrap();
        h.node
            .emit(
                Subject::Execution(exec.clone()),
                EventBody::Usage {
                    report: report("running", Measurement::Cumulative, "0.25"),
                },
            )
            .await
            .unwrap();
        h.node
            .emit(
                Subject::Execution(exec.clone()),
                EventBody::Usage {
                    report: report("final", Measurement::Final, "0.50"),
                },
            )
            .await
            .unwrap();
        let mut completion = build::done();
        completion.result = vec![
            Part::Text {
                text: "first ".into(),
            },
            Part::Text {
                text: "answer".into(),
            },
        ];
        h.node.finish(&exec, completion).await.unwrap();
    };
    let (tried, ()) = tokio::join!(run, drive);
    assert_eq!(tried.reply.unwrap(), ("first answer".into(), Some(0.5)));
    assert_eq!(tried.reported_model.as_deref(), Some("reported-model"));
    assert_eq!(*shown.lock().unwrap(), ["first", "first answer"]);
    assert_eq!(tried.usage.len(), 1);
    assert_eq!(tried.usage[0].tokens.output, Some(2));
    assert_eq!(h.q::<String>("SELECT state FROM runtime_jobs"), "finished");
}

#[tokio::test]
async fn one_model_failure_preserves_streamed_usage_and_typed_error() {
    let h = JobHub::new().await;
    let call = OneCall {
        prompt: "question",
        timeout: Duration::from_secs(10),
        tools: &ThinkTools::None,
        harness: "claude-code",
        images: &[],
    };
    let print = h.print();
    let run = print.once(&call, "sonnet", None, None);
    let drive = async {
        let (exec, spec) = h.running().await;
        assert!(matches!(
            spec.work,
            JobWork::Think(ThinkJob {
                tools: ThinkToolMode::None,
                ..
            })
        ));
        h.node
            .emit(
                Subject::Execution(exec.clone()),
                EventBody::Usage {
                    report: report("spent", Measurement::Final, "0.1"),
                },
            )
            .await
            .unwrap();
        let mut end = build::done();
        end.outcome = Outcome::Failed;
        end.stop_reason = "error".into();
        end.error = Some(ApiError::new(ErrorCode::Invalid, steve_acp::TOOL_REFUSED));
        h.node.finish(&exec, end).await.unwrap();
    };
    let (tried, ()) = tokio::join!(run, drive);
    assert!(matches!(
        tried.reply,
        Err(steve_core::CoreError::NeedsTools)
    ));
    assert!(!tried.timed_out);
    assert_eq!(tried.usage.len(), 1);
    assert_eq!(h.q::<i64>("SELECT COUNT(*) FROM runtime_jobs"), 1);
}

fn research(files: Vec<ResearchFile>, full: bool) -> ResearchAttempt {
    ResearchAttempt {
        run: "research-owner".into(),
        workspace: "default".into(),
        harness: "claude-code".into(),
        model: "sonnet".into(),
        effort: None,
        prompt: "research question".into(),
        resume: Some("resume-handle".into()),
        files,
        cwd: None,
        readable: vec![],
        budget: ResearchBudget {
            max_turns: Some(3),
            max_tools: Some(4),
            idle_ms: 5_000,
        },
        sandbox: SandboxRequirement::None,
        full,
        steve_tools: true,
        deadline: steve_store::now() + 10_000,
    }
}
#[tokio::test]
async fn research_admission_keeps_attachment_order_earlier_paths_budget_and_permissions() {
    for full in [false, true] {
        let h = JobHub::new().await;
        h.file("file-old", "default", 7);
        h.file("file-new", "default", 9);
        let a = research(
            vec![
                ResearchFile {
                    id: "file-old".into(),
                    name: "old.txt".into(),
                    earlier: true,
                },
                ResearchFile {
                    id: "file-new".into(),
                    name: "new.txt".into(),
                    earlier: false,
                },
            ],
            full,
        );
        let deadline = a.deadline;
        let mut job = h.rt.start_research(a).await.unwrap();
        let (exec, spec) = h.running().await;
        let JobWork::Research(work) = &spec.work else {
            panic!("research work required")
        };
        assert_eq!(work.earlier, [".steve/attachments/old.txt"]);
        assert!(work.steve_tools);
        assert_eq!(work.budget.max_turns, Some(3));
        assert_eq!(work.budget.max_tools, Some(4));
        assert_eq!(work.budget.idle_ms, 5_000);
        assert_eq!(
            spec.run.continuation.as_ref().unwrap().as_str(),
            "resume-handle"
        );
        assert_eq!(spec.run.limits.deadline.0, deadline);
        assert_eq!(spec.run.limits.offline_until.0, deadline);
        assert_eq!(
            spec.run.policy.permissions,
            if full {
                PermissionMode::FullAccess
            } else {
                PermissionMode::Plan
            }
        );
        assert_eq!(spec.run.policy.tools.as_str(), "research");
        let names: Vec<_> = spec
            .run
            .prompt
            .iter()
            .filter_map(|p| match p {
                Part::File { name, .. } => Some(name.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(
            names,
            [".steve/attachments/old.txt", ".steve/attachments/new.txt"]
        );
        assert_eq!(
            h.q::<String>("SELECT thread_run_id FROM runtime_jobs"),
            "research-owner"
        );
        h.node.finish(&exec, build::done()).await.unwrap();
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(5), job.next())
                .await
                .unwrap(),
            Some(News::End(_))
        ));
        assert_eq!(
            h.q::<i64>("SELECT COUNT(*) FROM node_commands WHERE method IN ('cancel','abandon')"),
            0
        );
    }
}

#[tokio::test]
async fn missing_or_invalid_attachments_never_commit_a_partial_job() {
    let h = JobHub::new().await;
    h.file("file-negative", "default", -1);
    for id in ["file-missing", "file-negative"] {
        let error =
            h.rt.start_research(research(
                vec![ResearchFile {
                    id: id.into(),
                    name: "bad.txt".into(),
                    earlier: false,
                }],
                false,
            ))
            .await
            .err()
            .unwrap();
        assert!(
            error.contains(if id == "file-missing" {
                "is gone"
            } else {
                "attachments of another workspace"
            }),
            "{error}"
        );
        assert_eq!(h.q::<i64>("SELECT COUNT(*) FROM runtime_jobs"), 0);
        assert_eq!(h.q::<i64>("SELECT COUNT(*) FROM node_commands"), 0);
        let tools = ThinkTools::Read(vec![Readable {
            file_id: id.into(),
            rel_path: "bad.txt".into(),
        }]);
        let tried = h
            .print()
            .once(
                &OneCall {
                    prompt: "question",
                    timeout: Duration::from_secs(5),
                    tools: &tools,
                    harness: "claude-code",
                    images: &[],
                },
                "sonnet",
                None,
                None,
            )
            .await;
        assert!(tried.reply.is_err());
        assert_eq!(h.q::<i64>("SELECT COUNT(*) FROM runtime_jobs"), 0);
    }
}
