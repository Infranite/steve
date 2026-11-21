use serde_json::json;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
use steve_node::{LocalNode, NodeConfig, PolicyCeiling, Root, Storage};
use steve_node_api::testkit::{RecordingSink, build};
use steve_node_api::*;
use steve_runtime::testkit::{eventually, eventually_sync};

struct Env {
    dir: tempfile::TempDir,
    sink: Arc<RecordingSink>,
}
impl Env {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(dir.path().join("work/src")).unwrap();
        std::fs::write(dir.path().join("work/src/data"), "evidence").unwrap();
        Self {
            dir,
            sink: Arc::new(RecordingSink::default()),
        }
    }
    fn node(&self) -> LocalNode {
        use std::os::unix::fs::PermissionsExt;
        let root = self.dir.path().canonicalize().unwrap();
        let wrapper = root.join("peer.sh");
        let peer = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/sg39/peer.mjs");
        std::fs::write(
            &wrapper,
            format!(
                "#!/bin/sh\nexec node '{}' '{}'\n",
                peer.display(),
                root.join("peer").display()
            ),
        )
        .unwrap();
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
        let n = LocalNode::open(NodeConfig {
            node: NodeId::new("sg39").unwrap(),
            home: root.join("node"),
            acp: steve_acp::AcpConfig {
                declared: HashMap::from([(
                    "sg39".into(),
                    serde_json::from_value(
                        json!({"properties":{"self_read_only":true,"nested_sandbox":false}}),
                    )
                    .unwrap(),
                )]),
                adapters: HashMap::from([("sg39".into(), wrapper)]),
                node: "node".into(),
                claude_executable: "/usr/bin/false".into(),
                steve_home: root.join("steve"),
                codex_home: root.join("codex"),
                user_home: root.clone(),
                version: "fixture".into(),
            },
            roots: vec![Root {
                id: RootId::new("work").unwrap(),
                path: root.join("work"),
                display: "work".into(),
                access: RootAccess::ReadOnly,
            }],
            capacity: Capacity {
                sessions: 0,
                jobs: 1,
                execs: 0,
            },
            storage: Storage::default(),
            ceiling: PolicyCeiling::unlimited(),
        })
        .unwrap();
        n.connect(self.sink.clone(), None);
        let mut cfg = steve_research::ResearchConfig::new(root.join("unused"));
        cfg.home = root.join("user");
        cfg.files_root = root.join("files");
        n.enable_research(cfg);
        n
    }
    fn spec(&self, text: &str, research: bool) -> JobSpec {
        let mut s = if research {
            build::research("sg39-job", "sg39-attempt", text)
        } else {
            build::job("sg39-job", "sg39-attempt", text)
        };
        s.run.choice.harness = "sg39".into();
        s.run.choice.model = None;
        s.run.policy.permissions = PermissionMode::Plan;
        s.run.policy.sandbox = SandboxRequirement::None;
        s.run.limits.deadline = Timestamp(wall() + 30_000);
        s.run.limits.offline_until = Timestamp(wall() + 30_000);
        if let JobWork::Research(ref mut r) = s.work {
            r.steve_tools = false;
            r.cwd = Some(RootPath {
                root: RootId::new("work").unwrap(),
                rel: RelPath::new("").unwrap(),
            });
        }
        s
    }
    fn read(&self, ext: &str) -> String {
        std::fs::read_to_string(self.dir.path().join(format!("peer.{ext}"))).unwrap()
    }
    async fn terminal(&self, n: &LocalNode) -> Completion {
        assert!(
            eventually("job terminal receipt", || async {
                n.operation(OperationId::new("sg39-op").unwrap())
                    .await
                    .unwrap()
                    .state
                    .is_finished()
            })
            .await
        );
        let r = n
            .operation(OperationId::new("sg39-op").unwrap())
            .await
            .unwrap();
        let Observed::Finished {
            result: Some(OperationResult::Execution(c)),
            ..
        } = r.state
        else {
            panic!("{r:?}")
        };
        assert!(
            eventually_sync("matching AttemptEnded", || self.sink.events().iter().any(
                |e| matches!(&e.body,EventBody::AttemptEnded{completion} if completion==&c)
            ))
            .await
        );
        c
    }
}
fn wall() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}
fn cmd<B>(id: &str, body: B) -> Command<B> {
    let mut c = build::cmd(id, body);
    c.issued_at = Timestamp(wall());
    c.accept_before = Timestamp(wall() + 30_000);
    c
}
fn result(c: &Completion) -> String {
    c.result
        .iter()
        .filter_map(|p| {
            if let Part::Text { text } = p {
                Some(text.as_str())
            } else {
                None
            }
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sg39_think_messages_usage_errors_and_stop_reasons() {
    for (prompt, outcome, reason) in [
        ("reset", Outcome::Succeeded, "end_turn"),
        ("error", Outcome::Failed, "error"),
        ("budget", Outcome::Failed, "max_tokens"),
    ] {
        let e = Env::new();
        let n = e.node();
        let s = e.spec(prompt, false);
        let command = cmd("sg39-op", s);
        n.start_job(command.clone()).await.unwrap();
        let c = e.terminal(&n).await;
        assert_eq!(c.outcome, outcome);
        assert_eq!(c.stop_reason, reason);
        if prompt == "reset" {
            assert_eq!(result(&c), "answer");
            assert!(c.error.is_none());
        } else {
            assert!(c.error.is_some());
        }
        if prompt == "error" {
            assert!(c.transient);
        }
        let events = e.sink.events();
        let messages: Vec<_> = events
            .iter()
            .filter_map(|e| {
                if let EventBody::MessageStarted { message, .. } = &e.body {
                    Some(message.clone())
                } else {
                    None
                }
            })
            .collect();
        assert_eq!(messages.len(), if prompt == "error" { 1 } else { 2 });
        let usage = events
            .iter()
            .position(|e| matches!(e.body, EventBody::Usage { .. }))
            .unwrap();
        let ended = events
            .iter()
            .position(|e| matches!(e.body, EventBody::AttemptEnded { .. }))
            .unwrap();
        assert!(usage < ended);
        assert_eq!(
            events
                .iter()
                .filter(|e| matches!(e.body, EventBody::AttemptEnded { .. }))
                .count(),
            1
        );
        assert_eq!(
            n.start_job(command).await.unwrap().state,
            Observed::Finished {
                outcome: c.outcome,
                result: Some(OperationResult::Execution(c.clone())),
                error: c.error.clone()
            }
        );
        n.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sg39_research_keeps_tool_budget_result_continuation_and_scoped_temp() {
    for (prompt, tools, end) in [
        ("read", None, JobEnd::Done),
        (
            "read",
            Some(1),
            JobEnd::Budget {
                hit: BudgetHit::Tools,
            },
        ),
    ] {
        let e = Env::new();
        let n = e.node();
        let mut s = e.spec(prompt, true);
        if let JobWork::Research(ref mut r) = s.work {
            r.budget.max_tools = tools;
        }
        n.start_job(cmd("sg39-op", s)).await.unwrap();
        let c = e.terminal(&n).await;
        assert_eq!(c.job.as_ref().unwrap().end, end);
        assert_eq!(result(&c), "answer");
        assert_eq!(c.continuation.as_ref().unwrap().as_str(), "sg39-session");
        assert_eq!(c.job.as_ref().unwrap().turns, 2);
        if tools.is_none() {
            assert_eq!(c.job.as_ref().unwrap().files_read, ["src/data"]);
            assert_eq!(c.outcome, Outcome::Succeeded);
        } else {
            assert_eq!(c.outcome, Outcome::Failed);
            assert_eq!(c.stop_reason, "max_turns");
        }
        let cwd = PathBuf::from(e.read("cwd"));
        assert_eq!(cwd, e.dir.path().join("work").canonicalize().unwrap());
        let temp = PathBuf::from(e.read("tmp"));
        assert!(temp.starts_with(e.dir.path().canonicalize().unwrap().join("node/jobs")));
        assert!(!temp.exists());
        n.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn sg39_cancellation_preserves_research_draft_and_frees_shared_capacity() {
    for research in [false, true] {
        let e = Env::new();
        let n = e.node();
        n.start_job(cmd("sg39-op", e.spec("hold", research)))
            .await
            .unwrap();
        assert!(
            eventually_sync("draft before cancellation", || e.sink.events().iter().any(
                |e| matches!(&e.body,EventBody::TextDelta{text,..} if text.contains("draft"))
            ))
            .await
        );
        n.cancel(cmd(
            "sg39-cancel",
            Cancel {
                target: OperationId::new("sg39-op").unwrap(),
                reason: "fixture".into(),
            },
        ))
        .await
        .unwrap();
        let c = e.terminal(&n).await;
        assert_eq!(c.outcome, Outcome::Cancelled);
        if research {
            assert_eq!(result(&c), "first draft");
        }
        let mut next = e.spec("reset", false);
        next.run.attempt = AttemptId::new("next-attempt").unwrap();
        n.start_job(cmd("next-op", next)).await.unwrap();
        assert!(
            eventually("next job capacity", || async {
                n.operation(OperationId::new("next-op").unwrap())
                    .await
                    .unwrap()
                    .state
                    .is_finished()
            })
            .await
        );
        n.shutdown().await;
    }
}
