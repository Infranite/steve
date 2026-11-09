//! LocalNode research jobs against scripted ACP adapters (no model is called): one attempt per
//! job with the read-only layers, its facts in Steve's events.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::json;
use steve_acp::AcpConfig;
use steve_node::{LocalNode, NodeConfig, Root, Storage};
use steve_node_api::testkit::RecordingSink;
use steve_node_api::testkit::build::cmd;
use steve_node_api::*;

struct T {
    _dir: Option<tempfile::TempDir>,
    root: PathBuf,
    home: PathBuf,
    sink: Arc<RecordingSink>,
}

impl T {
    fn acp_node(&self) -> LocalNode {
        self.acp_node_for("sample-adapter")
    }

    fn acp_node_for(&self, harness: &str) -> LocalNode {
        let mut cfg = self.config(2);
        // A wrapper is executable regardless of whether an adapter entry point ends in .js.
        let wrapper = self.root.join("research-acp.sh");
        let fake = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/research-acp.mjs");
        std::fs::write(
            &wrapper,
            format!("#!/bin/sh\nexec node '{}'\n", fake.display()),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&wrapper, std::fs::Permissions::from_mode(0o755)).unwrap();
        cfg.acp.adapters.insert(harness.into(), wrapper);
        let n = LocalNode::open(cfg).unwrap();
        n.connect(self.sink.clone(), Some(Arc::new(Files::default())));
        n.enable_research(self.research());
        n
    }

    fn new() -> T {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        let home = root.join("home");
        std::fs::create_dir_all(home.join(".ssh")).unwrap();
        std::fs::write(home.join(".ssh/id_rsa"), "PRIVATE").unwrap();
        std::fs::create_dir_all(root.join("work/proj/src")).unwrap();
        std::fs::create_dir_all(root.join("work/docs")).unwrap();
        std::fs::write(root.join("work/proj/src/lib.rs"), "fn add() {}\n").unwrap();
        std::fs::write(root.join("work/proj/keep.txt"), "orig").unwrap();
        T {
            _dir: Some(dir),
            root,
            home,
            sink: Arc::new(RecordingSink::default()),
        }
    }

    /// The same setup over a root made by another process's `T::new`.
    fn at(root: &Path) -> T {
        T {
            _dir: None,
            root: root.to_path_buf(),
            home: root.join("home"),
            sink: Arc::new(RecordingSink::default()),
        }
    }

    fn config(&self, jobs: u32) -> NodeConfig {
        let root = self.root.clone();
        NodeConfig {
            ceiling: steve_node::PolicyCeiling::unlimited(),
            node: NodeId::new("hub").unwrap(),
            home: root.join("node"),
            acp: AcpConfig {
                declared: Default::default(),
                adapters: HashMap::new(),
                node: "node".into(),
                claude_executable: "claude".into(),
                steve_home: root.join("steve"),
                codex_home: root.join("codex"),
                user_home: root.clone(),
                version: "test".into(),
                idle_release: Duration::from_secs(600),
                retry_delays: vec![],
            },
            roots: vec![Root {
                id: RootId::new("work").unwrap(),
                path: root.join("work"),
                display: "~/work".into(),
                access: RootAccess::ReadOnly,
            }],
            capacity: Capacity {
                sessions: 0,
                jobs,
                execs: 0,
            },
            storage: Storage::default(),
        }
    }

    /// The research setup a host gives: this home's secrets off limits.
    fn research(&self) -> steve_research::ResearchConfig {
        let mut c = steve_research::ResearchConfig::new(self.root.join("unused"));
        c.home = self.home.clone();
        c.files_root = self.root.join("files");
        c
    }

    /// Where the scripted adapter logs `what` (research-script.mjs).
    fn log(&self, what: &str) -> PathBuf {
        self.root.join(format!("fake.{what}"))
    }

    fn read(&self, what: &str) -> String {
        std::fs::read_to_string(self.log(what)).unwrap_or_default()
    }

    /// The scripted adapter (`research-script.mjs`) behind a wrapper that names its log.
    fn write_adapter(&self) -> PathBuf {
        let p = self.root.join("scripted.sh");
        let script = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/research-script.mjs");
        std::fs::write(
            &p,
            format!(
                "#!/bin/sh\nexport FAKE_RESEARCH_LOG='{}'\nexec node '{}'\n",
                self.root.join("fake").display(),
                script.display()
            ),
        )
        .unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    /// A Node with research on and the scripted adapter as `scripted` (nothing declared: plain
    /// ACP), `own-sandbox` (it keeps its own read-only sandbox) and `no-nesting` (it has none and
    /// cannot run in the Node's).
    fn scripted_node(&self, jobs: u32, files: Arc<dyn HubFiles>) -> LocalNode {
        let n = LocalNode::open(self.scripted_config(jobs)).unwrap();
        n.connect(self.sink.clone(), Some(files));
        n.enable_research(self.research());
        n
    }

    fn scripted_config(&self, jobs: u32) -> NodeConfig {
        let adapter = self.write_adapter();
        let mut cfg = self.config(jobs);
        for id in ["scripted", "own-sandbox", "no-nesting"] {
            cfg.acp.adapters.insert(id.into(), adapter.clone());
        }
        let declare = |props: serde_json::Value| -> steve_acp::Declaration {
            serde_json::from_value(json!({ "properties": props })).unwrap()
        };
        cfg.acp.declared.insert(
            "own-sandbox".into(),
            declare(json!({"self_read_only": true, "nested_sandbox": false})),
        );
        cfg.acp.declared.insert(
            "no-nesting".into(),
            declare(json!({"self_read_only": false, "nested_sandbox": false})),
        );
        cfg
    }

    fn node(&self) -> LocalNode {
        self.scripted_node(2, Arc::new(Files::default()))
    }

    fn spawns(&self) -> usize {
        self.read("spawns").lines().count()
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

    fn bodies(&self) -> Vec<EventBody> {
        self.sink.events().into_iter().map(|e| e.body).collect()
    }

    async fn end_of(&self, n: &LocalNode, op: &str) -> Completion {
        let t0 = Instant::now();
        loop {
            let r = n.operation(OperationId::new(op).unwrap()).await.unwrap();
            if let Observed::Finished {
                result: Some(OperationResult::Execution(c)),
                ..
            } = r.state
            {
                // its events reach the Hub too
                while !self.ended().contains(&c) {
                    assert!(t0.elapsed() < Duration::from_secs(20), "no AttemptEnded");
                    tokio::time::sleep(Duration::from_millis(20)).await;
                }
                return c;
            }
            assert!(
                t0.elapsed() < Duration::from_secs(20),
                "{op}: {:?}",
                r.state
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

/// Serves the artifacts it was given, to any execution.
#[derive(Default)]
struct Files(Mutex<HashMap<String, Vec<u8>>>);

impl Files {
    fn artifact(&self, id: &str, mime: &str, bytes: &[u8]) -> ArtifactRef {
        self.0.lock().unwrap().insert(id.into(), bytes.to_vec());
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
    async fn fetch(
        &self,
        _a: FileAccess,
        id: ArtifactId,
        offset: u64,
        _max: u32,
    ) -> ApiResult<ReadChunk> {
        let b = self.0.lock().unwrap().get(id.as_str()).cloned();
        let b = b.ok_or_else(|| ApiError::new(ErrorCode::Denied, "not yours"))?;
        Ok(ReadChunk {
            offset,
            bytes: b[offset as usize..].to_vec(),
            eof: true,
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

fn now_cmd<B>(op: &str, body: B) -> Command<B> {
    let mut c = cmd(op, body);
    c.issued_at = Timestamp(wall());
    c.accept_before = Timestamp(wall() + 60_000);
    c
}

fn at(rel: &str) -> RootPath {
    RootPath {
        root: RootId::new("work").unwrap(),
        rel: RelPath::new(rel).unwrap(),
    }
}

/// A research job over `work/proj`, with no tools, no sandbox and no limit but a far deadline.
fn job(attempt: &str, model: &str, text: &str) -> JobSpec {
    JobSpec {
        job: JobId::new("j_1").unwrap(),
        run: RunSpec {
            attempt: AttemptId::new(attempt).unwrap(),
            choice: HarnessChoice {
                harness: "scripted".into(),
                model: Some(model.into()),
                effort: None,
            },
            directory: RunDirectory::Scratch,
            prompt: vec![Part::Text { text: text.into() }],
            policy: ExecutionPolicy {
                revision: Revision::new("p1").unwrap(),
                permissions: PermissionMode::Plan,
                sandbox: SandboxRequirement::None,
                readable: vec![],
                network: NetworkScope::Unrestricted,
                tools: GrantId::new("g_research").unwrap(),
            },
            limits: Limits {
                deadline: Timestamp(wall() + 600_000),
                offline_until: Timestamp(wall() + 600_000),
                max_output_bytes: 1 << 20,
                max_tokens: None,
                max_cost: None,
            },
            continuation: None,
        },
        work: JobWork::Research(ResearchJob {
            cwd: Some(at("proj")),
            readable: vec![at("docs")],
            budget: ResearchBudget {
                max_turns: None,
                max_tools: None,
                idle_ms: 600_000,
            },
            steve_tools: false,
            earlier: vec![],
        }),
    }
}

fn research(spec: &mut JobSpec) -> &mut ResearchJob {
    match &mut spec.work {
        JobWork::Research(r) => r,
        _ => unreachable!(),
    }
}

fn acp_job(attempt: &str, text: &str) -> JobSpec {
    let mut spec = job(attempt, "", text);
    spec.run.choice.harness = "sample-adapter".into();
    spec
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acp_research_streams_before_completion_and_keeps_reads_usage_and_continuation() {
    let t = T::new();
    let n = t.acp_node();
    n.start_job(now_cmd("acp_stream", acp_job("a_acp", "stream")))
        .await
        .unwrap();
    until("first ACP chunk", || {
        t.bodies()
            .iter()
            .any(|e| matches!(e, EventBody::TextDelta { text, .. } if text == "答案第一段"))
    })
    .await;
    assert!(
        t.ended().is_empty(),
        "the first chunk must precede the final result"
    );
    assert_eq!(
        n.operation(OperationId::new("acp_stream").unwrap())
            .await
            .unwrap()
            .state,
        Observed::Running
    );
    let c = t.end_of(&n, "acp_stream").await;
    assert_eq!(c.outcome, Outcome::Succeeded, "{c:?}");
    assert_eq!(
        c.result,
        vec![Part::Text {
            text: "答案第一段，答案第二段。".into()
        }]
    );
    assert_eq!(
        c.continuation,
        Some(ResumeHandle::new("research-session").unwrap())
    );
    let report = c.job.unwrap();
    assert_eq!(report.files_read, ["src/lib.rs"]);
    assert_eq!(report.turns, 2);
    assert!(
        t.bodies()
            .iter()
            .any(|e| matches!(e, EventBody::Usage { .. }))
    );
    n.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acp_research_keeps_read_only_tool_budget_and_idle_limits() {
    for (name, expected) in [
        (
            "violation",
            JobEnd::Violated {
                rule: ViolationRule::Tool,
                tool: "arbitrary_reader".into(),
                target: None,
            },
        ),
        (
            "tools",
            JobEnd::Budget {
                hit: BudgetHit::Tools,
            },
        ),
        (
            "idle",
            JobEnd::Budget {
                hit: BudgetHit::Idle,
            },
        ),
        ("thought", JobEnd::Done),
    ] {
        let t = T::new();
        let n = t.acp_node();
        let mut spec = acp_job("a_acp", name);
        research(&mut spec).budget.max_tools = Some(1);
        research(&mut spec).budget.idle_ms = if matches!(name, "idle" | "thought") {
            800
        } else {
            600_000
        };
        n.start_job(now_cmd("acp_limit", spec)).await.unwrap();
        let c = t.end_of(&n, "acp_limit").await;
        assert_eq!(c.job.unwrap().end, expected, "{name}");
        if name == "thought" {
            assert_eq!(
                c.result,
                vec![Part::Text {
                    text: "完成".into()
                }]
            );
            assert!(!t.bodies().iter().any(|e| matches!(e, EventBody::TextDelta { text, .. } if text.contains("private-thought"))));
        }
        n.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acp_research_checks_mcp_identity_without_trusting_display_titles() {
    for (prompt, allowed) in [
        ("mcp_read", true),
        ("mcp_write", false),
        ("mcp_external", false),
        ("mcp_nested_name", false),
        ("mcp_title_only", false),
    ] {
        let t = T::new();
        let n = t.acp_node();
        n.connect_tools(Arc::new(Tools::default())).await.unwrap();
        let mut spec = acp_job("a_acp_mcp", prompt);
        research(&mut spec).steve_tools = true;
        n.start_job(now_cmd("acp_mcp", spec)).await.unwrap();
        let completion = t.end_of(&n, "acp_mcp").await;
        let report = completion.job.unwrap();
        if allowed {
            assert_eq!(report.end, JobEnd::Done, "{prompt}");
            assert_eq!(
                completion.result,
                vec![Part::Text {
                    text: "已查询事项进度".into()
                }]
            );
            assert!(t.bodies().iter().any(|e| matches!(e,
                EventBody::ToolChanged { tool } if tool.origin == ToolOrigin::Steve && tool.name == "mcp__steve__steve_item_read")));
        } else {
            assert!(
                matches!(
                    report.end,
                    JobEnd::Violated {
                        rule: ViolationRule::Tool,
                        ..
                    }
                ),
                "{prompt}: {:?}",
                report.end
            );
        }
        assert!(report.files_read.is_empty());
        n.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acp_research_checks_command_backed_reads_and_searches_in_the_same_sandbox() {
    for (harness, prompt, denied) in [
        ("codex", "command_list", None),
        ("codex", "command_read", None),
        ("codex", "command_search", None),
        ("codex", "command_outside", Some(ViolationRule::OutOfScope)),
        ("codex", "command_secret", Some(ViolationRule::Sensitive)),
        ("codex", "command_edit", Some(ViolationRule::Tool)),
        ("codex", "command_external", Some(ViolationRule::Tool)),
        ("sample-adapter", "command_list", Some(ViolationRule::Tool)),
    ] {
        let t = T::new();
        let n = t.acp_node_for(harness);
        let mut spec = acp_job("a_acp_command", prompt);
        spec.run.choice.harness = harness.into();
        n.start_job(now_cmd("acp_command", spec)).await.unwrap();
        let completion = t.end_of(&n, "acp_command").await;
        let report = completion.job.unwrap();
        if let Some(expected) = denied {
            assert!(
                matches!(report.end, JobEnd::Violated { rule, .. } if rule == expected),
                "{harness}/{prompt}: {:?}",
                report.end
            );
        } else {
            assert_eq!(report.end, JobEnd::Done, "{harness}/{prompt}");
            assert_eq!(
                completion.result,
                vec![Part::Text {
                    text: "已读取项目证据".into()
                }]
            );
        }
        n.shutdown().await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn acp_research_cancellation_keeps_the_partial_reply() {
    let t = T::new();
    let n = t.acp_node();
    n.start_job(now_cmd("acp_cancel", acp_job("a_acp", "cancel")))
        .await
        .unwrap();
    until("partial ACP reply", || {
        t.bodies()
            .iter()
            .any(|e| matches!(e, EventBody::TextDelta { .. }))
    })
    .await;
    n.cancel(now_cmd(
        "stop_acp",
        Cancel {
            target: OperationId::new("acp_cancel").unwrap(),
            reason: "stop".into(),
        },
    ))
    .await
    .unwrap();
    let c = t.end_of(&n, "acp_cancel").await;
    assert_eq!(c.outcome, Outcome::Cancelled);
    assert_eq!(
        c.result,
        vec![Part::Text {
            text: "我先".into()
        }]
    );
    n.shutdown().await;
}

/// The scripted adapter's job: `script` is the prompt (its `@` lines run, see research-script.mjs).
fn scripted(attempt: &str, model: &str, script: &str) -> JobSpec {
    job(attempt, model, script)
}

fn alive(pid: i32) -> bool {
    unsafe { libc::kill(pid, 0) == 0 }
}

async fn until(what: &str, f: impl Fn() -> bool) {
    let t0 = Instant::now();
    while !f() {
        assert!(t0.elapsed() < Duration::from_secs(15), "timed out: {what}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_research_job_answers_with_its_draft_tools_usage_and_report() {
    let t = T::new();
    let lib = t.root.join("work/proj/src/lib.rs").display().to_string();
    let answer = "add 什么都不做，见 src/lib.rs:1。\nOFFER: {\"title\":\"实现 add\",\"goal\":\"让 add 返回和\"}";
    let script = format!(
        "add 怎么工作？\n@msg m1\n@say 我先看看\n@read {lib}\n@msg m2\n@say add 什么都不做，\n@say 见 src/lib.rs:1。\\nOFFER: {{\"title\":\"实现 add\",\"goal\":\"让 add 返回和\"}}\n@usage 10 5 100 0.25"
    );
    let n = t.node();
    let first = now_cmd("op_j", scripted("a_1", "claude-sonnet-5-5", &script));
    let r = n.start_job(first.clone()).await.unwrap();
    assert_eq!(r.state, Observed::Running);
    let c = t.end_of(&n, "op_j").await;
    assert_eq!(c.outcome, Outcome::Succeeded, "{c:?}");
    // the answer as written: the Hub takes the offer out and adds marks
    assert_eq!(
        c.result,
        vec![Part::Text {
            text: answer.into()
        }]
    );
    assert_eq!(c.continuation, Some(ResumeHandle::new("sess-1").unwrap()));
    assert!(!c.acted);
    let report = c.job.clone().unwrap();
    assert_eq!(report.end, JobEnd::Done);
    assert_eq!(report.files_read, vec!["src/lib.rs".to_string()]);
    assert_eq!(report.turns, 2);
    // one adapter, the model given, a new session (nothing loaded) in Steve's plan mode
    assert_eq!(t.spawns(), 1);
    assert_eq!(t.read("config"), "model=claude-sonnet-5-5\n");
    assert!(t.read("load").is_empty());
    let bodies = t.bodies();
    // the draft: a new message replaces the first one
    let started: Vec<&String> = bodies
        .iter()
        .filter_map(|b| match b {
            EventBody::MessageStarted { message, .. } => Some(message),
            _ => None,
        })
        .collect();
    assert_eq!(started.len(), 2);
    let second: String = bodies
        .iter()
        .filter_map(|b| match b {
            EventBody::TextDelta { message, text, .. } if message == started[1] => {
                Some(text.clone())
            }
            _ => None,
        })
        .collect();
    assert_eq!(second, answer);
    // the tool row, its last view done
    let tools: Vec<(ToolPhase, ToolCategory, Option<String>)> = bodies
        .iter()
        .filter_map(|b| match b {
            EventBody::ToolChanged { tool } => {
                Some((tool.phase, tool.category, tool.target.clone()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        tools.last().unwrap(),
        &(
            ToolPhase::Succeeded,
            ToolCategory::Read,
            Some("src/lib.rs".into())
        )
    );
    // the tokens the adapter reported for the turn, and its dollars
    let usage: Vec<&UsageReport> = bodies
        .iter()
        .filter_map(|b| match b {
            EventBody::Usage { report } => Some(report),
            _ => None,
        })
        .collect();
    let fin = usage
        .iter()
        .find(|u| u.measurement == Measurement::Final)
        .unwrap();
    assert_eq!(
        (fin.tokens.input, fin.tokens.output, fin.tokens.cache_read),
        (Some(10), Some(5), Some(100))
    );
    assert!(
        usage
            .iter()
            .any(|u| u.cost.as_ref().is_some_and(|c| c.decimal == "0.25"))
    );
    // the job's scratch directory is gone
    let jobs = t.root.join("node/jobs");
    assert!(std::fs::read_dir(&jobs).map_or(true, |mut d| d.next().is_none()));
    // a resend is the same receipt; the same attempt under another operation is a conflict
    let again = n.start_job(first.clone()).await.unwrap();
    assert!(again.state.is_finished());
    let mut other = first;
    other.id = OperationId::new("op_j2").unwrap();
    let e = n.start_job(other).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::Conflict);
    assert_eq!(t.spawns(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_write_is_a_violation_and_the_harness_dies_with_it() {
    let t = T::new();
    let survived = t.root.join("survived");
    let n = t.node();
    let t0 = Instant::now();
    let script = format!(
        "q\n@msg m1\n@say 我先看看\n@asking edit keep.txt\n@sleep 3000\n@touch {}",
        survived.display()
    );
    n.start_job(now_cmd("op_j", scripted("a_1", "m", &script)))
        .await
        .unwrap();
    let c = t.end_of(&n, "op_j").await;
    assert!(t0.elapsed() < Duration::from_secs(3), "killed promptly");
    assert_eq!(c.outcome, Outcome::Failed);
    assert_eq!(c.error.as_ref().unwrap().code, ErrorCode::Denied);
    assert!(
        matches!(
            c.job.unwrap().end,
            JobEnd::Violated {
                rule: ViolationRule::Tool,
                ..
            }
        ),
        "an edit is no read, whatever it is called"
    );
    // what it wrote before stays
    assert_eq!(
        c.result,
        vec![Part::Text {
            text: "我先看看".into()
        }]
    );
    tokio::time::sleep(Duration::from_secs(4)).await;
    assert!(!survived.exists(), "the adapter's process group was killed");
    assert_eq!(
        std::fs::read_to_string(t.root.join("work/proj/keep.txt")).unwrap(),
        "orig"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn budgets_end_a_job_with_what_it_wrote() {
    let t = T::new();
    let n = t.node();
    let mut tools = scripted(
        "a_tools",
        "m",
        "q\n@msg m1\n@say 读了一半\n@read src/lib.rs\n@read src/lib.rs\n@sleep 30000",
    );
    research(&mut tools).budget.max_tools = Some(1);
    n.start_job(now_cmd("op_tools", tools)).await.unwrap();
    let c = t.end_of(&n, "op_tools").await;
    assert_eq!(
        c.job.unwrap().end,
        JobEnd::Budget {
            hit: BudgetHit::Tools
        }
    );
    assert_eq!(
        c.result,
        vec![Part::Text {
            text: "读了一半".into()
        }]
    );

    let mut turns = scripted(
        "a_turns",
        "m",
        "q\n@msg m1\n@say 一\n@msg m2\n@say 二\n@sleep 30000",
    );
    research(&mut turns).budget.max_turns = Some(1);
    n.start_job(now_cmd("op_turns", turns)).await.unwrap();
    let c = t.end_of(&n, "op_turns").await;
    assert_eq!(
        c.job.unwrap().end,
        JobEnd::Budget {
            hit: BudgetHit::Turns
        }
    );

    // silent for idle_ms: stuck
    let mut idle = scripted("a_idle", "m", "q\n@msg m1\n@say 一\n@sleep 30000");
    research(&mut idle).budget.idle_ms = 300;
    n.start_job(now_cmd("op_idle", idle)).await.unwrap();
    let c = t.end_of(&n, "op_idle").await;
    assert_eq!(c.outcome, Outcome::TimedOut);
    assert_eq!(
        c.job.unwrap().end,
        JobEnd::Budget {
            hit: BudgetHit::Idle
        }
    );

    // the job's deadline
    let mut late = scripted("a_late", "m", "q\n@msg m1\n@say 一\n@sleep 30000");
    late.run.limits.deadline = Timestamp(wall() + 700);
    n.start_job(now_cmd("op_late", late)).await.unwrap();
    let c = t.end_of(&n, "op_late").await;
    assert_eq!(c.outcome, Outcome::TimedOut);
    assert_eq!(
        c.job.unwrap().end,
        JobEnd::Budget {
            hit: BudgetHit::Time
        }
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_follow_up_continues_the_session_on_the_same_model() {
    let t = T::new();
    let n = t.node();
    let mut spec = scripted("a_2", "claude-opus-5-5", "再细一点\n@msg m1\n@say 接着说");
    spec.run.continuation = Some(ResumeHandle::new("sess-0").unwrap());
    n.start_job(now_cmd("op_j", spec)).await.unwrap();
    let c = t.end_of(&n, "op_j").await;
    assert_eq!(c.outcome, Outcome::Succeeded);
    // the session loaded, the model set, the follow-up alone as the prompt
    assert_eq!(t.read("load"), "sess-0");
    assert_eq!(t.read("config"), "model=claude-opus-5-5\n");
    let prompt: serde_json::Value = serde_json::from_str(&t.read("prompt")).unwrap();
    assert!(prompt[0]["text"].as_str().unwrap().starts_with("再细一点"));
    // the session to continue next time
    assert_eq!(c.continuation, Some(ResumeHandle::new("sess-0").unwrap()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_model_only_a_failure_is_the_attempts_end() {
    let t = T::new();
    let n = t.node();
    n.start_job(now_cmd(
        "op_j",
        scripted("a_1", "m", "q\n@fail API Error: 503 overloaded"),
    ))
    .await
    .unwrap();
    let c = t.end_of(&n, "op_j").await;
    assert_eq!(c.outcome, Outcome::Failed);
    assert_eq!(c.job.unwrap().end, JobEnd::Failed);
    assert!(c.error.unwrap().safe_detail.contains("overloaded"));
    // no second model: that is the Hub's next attempt
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(t.spawns(), 1);
}

/// An adapter that declares it keeps its own read-only sandbox (and cannot be nested in another)
/// runs as it is; one that can be nested in neither is refused a required sandbox.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn an_adapter_with_a_sandbox_of_its_own_is_not_wrapped() {
    let t = T::new();
    let n = t.node();
    let mut spec = scripted("a_1", "m", "q\n@msg m1\n@say 看过了");
    spec.run.choice.harness = "own-sandbox".into();
    spec.run.policy.sandbox = SandboxRequirement::AllowFallback {
        preferred: vec!["fs-readonly".into()],
        acceptable: vec![],
    };
    n.start_job(now_cmd("op_j", spec.clone())).await.unwrap();
    let c = t.end_of(&n, "op_j").await;
    assert_eq!(c.outcome, Outcome::Succeeded, "{c:?}");
    assert!(c.job.unwrap().sandboxed, "its own sandbox");
    assert_eq!(t.spawns(), 1, "started as it is: it could log");
    // required: its own read-only sandbox is the guarantee
    spec.run.attempt = AttemptId::new("a_2").unwrap();
    spec.run.policy.sandbox = SandboxRequirement::Required {
        guarantees: vec!["fs-readonly".into()],
    };
    n.start_job(now_cmd("op_j2", spec.clone())).await.unwrap();
    assert_eq!(t.end_of(&n, "op_j2").await.outcome, Outcome::Succeeded);
    // nothing of its own and no nesting: refused before anything runs
    spec.run.attempt = AttemptId::new("a_3").unwrap();
    spec.run.choice.harness = "no-nesting".into();
    let e = n.start_job(now_cmd("op_j3", spec)).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::Unsupported);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_preferred_sandbox_is_used_where_this_machine_has_one() {
    let t = T::new();
    let n = t.node();
    let mut spec = scripted("a_1", "m", "q\n@msg m1\n@say 好");
    spec.run.policy.sandbox = SandboxRequirement::AllowFallback {
        preferred: vec!["fs-readonly".into()],
        acceptable: vec![],
    };
    n.start_job(now_cmd("op_j", spec)).await.unwrap();
    let c = t.end_of(&n, "op_j").await;
    assert_eq!(c.outcome, Outcome::Succeeded, "{c:?}");
    assert_eq!(
        c.job.unwrap().sandboxed,
        steve_research::sandbox::available()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn attachments_are_readable_and_secrets_are_not() {
    let t = T::new();
    let files = Arc::new(Files::default());
    let notes = files.artifact("f_1", "text/plain", b"hello notes");
    let png = files.artifact("f_2", "image/png", b"\x89PNG");
    let seen = t.root.join("seen.txt");
    let n = t.scripted_node(2, files.clone());
    // the adapter reads the copy it was told about (the run's files directory)
    let script = format!(
        "附件说了什么？\n@cat {} {}\n@msg m1\n@say 看了",
        t.root.join("files").display(),
        seen.display()
    );
    let mut spec = scripted("a_1", "m", &script);
    spec.run.prompt.push(Part::File {
        artifact: notes,
        name: ".steve/attachments/notes.txt".into(),
    });
    spec.run.prompt.push(Part::Image { artifact: png });
    n.start_job(now_cmd("op_j", spec)).await.unwrap();
    let c = t.end_of(&n, "op_j").await;
    assert_eq!(c.outcome, Outcome::Succeeded, "{c:?}");
    assert_eq!(std::fs::read_to_string(&seen).unwrap(), "hello notes");
    // the image goes inline, as an ACP image block
    let prompt: serde_json::Value = serde_json::from_str(&t.read("prompt")).unwrap();
    assert_eq!(prompt[1]["type"], "image");
    assert_eq!(prompt[1]["mimeType"], "image/png");
    // the copies are gone with the job
    until("files removed", || {
        std::fs::read_dir(t.root.join("files")).map_or(true, |mut d| d.next().is_none())
    })
    .await;

    n.start_job(now_cmd(
        "op_s",
        scripted("a_2", "m", "q\n@asking read ~/.ssh/id_rsa\n@sleep 5000"),
    ))
    .await
    .unwrap();
    let c = t.end_of(&n, "op_s").await;
    assert!(
        matches!(
            c.job.unwrap().end,
            JobEnd::Violated {
                rule: ViolationRule::Sensitive,
                target: Some(ref p),
                ..
            } if p == "~/.ssh/id_rsa"
        ),
        "a secret is not read"
    );
}

#[derive(Default)]
struct Tools {
    calls: Mutex<Vec<ToolCall>>,
}

#[async_trait::async_trait]
impl HubTools for Tools {
    async fn catalog(&self, _e: ExecutionRef) -> ApiResult<Catalog> {
        Ok(Catalog {
            revision: Revision::new("c1").unwrap(),
            tools: vec![ToolDescriptor {
                name: "steve_knowledge_search".into(),
                description: "Search the knowledge base.".into(),
                input_schema: json!({"type": "object", "properties": {"query": {"type": "string"}}}),
                read_only: true,
            }],
        })
    }
    async fn call(&self, q: ToolCall) -> ApiResult<ToolResult> {
        self.calls.lock().unwrap().push(q);
        Ok(ToolResult::Completed {
            parts: vec![Part::Text {
                text: "found it".into(),
            }],
            structured: None,
            end_turn: false,
        })
    }
    async fn call_status(&self, _k: ToolKey) -> ApiResult<ToolResult> {
        Err(ApiError::new(ErrorCode::Unsupported, "x"))
    }
    async fn cancel_call(&self, _k: ToolKey) -> ApiResult<ToolResult> {
        Err(ApiError::new(ErrorCode::Unsupported, "x"))
    }
}

/// A JSON-RPC POST to an MCP endpoint over plain HTTP/1.1.
async fn mcp(url: &str, token: &str, body: serde_json::Value) -> (u16, serde_json::Value) {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let rest = url.strip_prefix("http://").unwrap();
    let (host, path) = rest.split_once('/').unwrap();
    let mut s = tokio::net::TcpStream::connect(host).await.unwrap();
    let b = body.to_string();
    let req = format!(
        "POST /{path} HTTP/1.1\r\nHost: {host}\r\nContent-Type: application/json\r\nAccept: application/json, text/event-stream\r\nMcp-Protocol-Version: 2025-06-18\r\nAuthorization: Bearer {token}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{b}",
        b.len()
    );
    s.write_all(req.as_bytes()).await.unwrap();
    let mut out = String::new();
    s.read_to_string(&mut out).await.unwrap();
    let status: u16 = out.split_whitespace().nth(1).unwrap().parse().unwrap();
    let body = out.split("\r\n\r\n").nth(1).unwrap_or("");
    let v = body
        .find('{')
        .and_then(|i| body.rfind('}').map(|j| &body[i..=j]))
        .and_then(|j| serde_json::from_str(j).ok())
        .unwrap_or(serde_json::Value::Null);
    (status, v)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn steves_tools_count_for_the_job_until_a_cancel_stops_it() {
    let t = T::new();
    let n = t.node();
    let tools = Arc::new(Tools::default());
    n.connect_tools(tools.clone()).await.unwrap();
    // keeps the MCP servers it was given, writes a little and waits (with a child) until stopped
    let mut spec = scripted("a_1", "m", "q\n@mcp\n@msg m1\n@say 我先看看\n@child\n@wait");
    research(&mut spec).steve_tools = true;
    n.start_job(now_cmd("op_j", spec)).await.unwrap();
    until("the adapter waits", || {
        !t.read("pid").is_empty() && !t.read("mcp").is_empty()
    })
    .await;
    // Steve's /mcp over http, with the job's own token
    let servers: serde_json::Value = serde_json::from_str(&t.read("mcp")).unwrap();
    let server = &servers[0];
    assert_eq!(
        (&server["type"], &server["name"]),
        (&json!("http"), &json!("steve"))
    );
    let url = server["url"].as_str().unwrap().to_string();
    let token = server["headers"][0]["value"]
        .as_str()
        .unwrap()
        .strip_prefix("Bearer ")
        .unwrap()
        .to_string();
    assert!(
        url.starts_with("http://127.0.0.1:"),
        "the Node's endpoint: {url}"
    );
    let (s, list) = mcp(
        &url,
        &token,
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"}),
    )
    .await;
    assert_eq!(s, 200, "{list}");
    assert_eq!(list["result"]["tools"][0]["name"], "steve_knowledge_search");
    assert_eq!(
        list["result"]["tools"][0]["annotations"]["readOnlyHint"],
        true
    );
    let (_, called) = mcp(
        &url,
        &token,
        json!({"jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": {"name": "steve_knowledge_search", "arguments": {"query": "add"}}}),
    )
    .await;
    assert_eq!(called["result"]["content"][0]["text"], "found it");
    assert_eq!(
        tools.calls.lock().unwrap()[0].execution,
        ExecutionRef::Job {
            job: JobId::new("j_1").unwrap(),
            attempt: AttemptId::new("a_1").unwrap(),
        }
    );
    until("the draft", || {
        t.bodies()
            .iter()
            .any(|b| matches!(b, EventBody::TextDelta { .. }))
    })
    .await;
    let child = pid_of(&t.log("pid"));
    assert!(alive(child));
    n.cancel(now_cmd(
        "op_c",
        Cancel {
            target: OperationId::new("op_j").unwrap(),
            reason: "stop".into(),
        },
    ))
    .await
    .unwrap();
    let c = t.end_of(&n, "op_j").await;
    assert_eq!(c.outcome, Outcome::Cancelled);
    assert_eq!(c.job.unwrap().end, JobEnd::Stopped);
    // what it wrote until then is kept
    assert_eq!(
        c.result,
        vec![Part::Text {
            text: "我先看看".into()
        }]
    );
    until("the adapter's child is gone", || !alive(child)).await;
    // the job's token went with it
    let (s, _) = mcp(
        &url,
        &token,
        json!({"jsonrpc": "2.0", "id": 3, "method": "tools/list"}),
    )
    .await;
    assert_eq!(s, 401);
    assert_eq!(tools.calls.lock().unwrap().len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn what_a_research_job_cannot_have_is_refused_before_it_is_accepted() {
    // not enabled, or no job capacity
    let t = T::new();
    let n = LocalNode::open(t.config(2)).unwrap();
    let e = n
        .start_job(now_cmd("op_0", job("a_0", "m", "q")))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Unsupported);
    let t = T::new();
    let n = t.scripted_node(0, Arc::new(Files::default()));
    let e = n
        .start_job(now_cmd("op_0", job("a_0", "m", "q")))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Unsupported);
    let t = T::new();
    let n = t.node();
    let refused = |f: fn(&mut JobSpec)| {
        let mut s = job("a_x", "m", "q");
        f(&mut s);
        s
    };
    let cases: Vec<(JobSpec, ErrorCode)> = vec![
        (
            refused(|s| research(s).cwd = Some(at("nowhere"))),
            ErrorCode::Invalid,
        ),
        (
            refused(|s| {
                research(s).readable = vec![RootPath {
                    root: RootId::new("other").unwrap(),
                    rel: RelPath::new("x").unwrap(),
                }]
            }),
            ErrorCode::Denied,
        ),
        (
            refused(|s| s.run.policy.permissions = PermissionMode::Ask),
            ErrorCode::Invalid,
        ),
        (
            refused(|s| s.run.policy.network = NetworkScope::Denied),
            ErrorCode::Unsupported,
        ),
        (
            refused(|s| {
                s.run.policy.sandbox = SandboxRequirement::Required {
                    guarantees: vec!["no-network".into()],
                }
            }),
            ErrorCode::Unsupported,
        ),
        (
            refused(|s| s.run.choice.harness = "gemini".into()),
            ErrorCode::Unsupported,
        ),
        (
            refused(|s| s.run.limits.max_tokens = Some(10)),
            ErrorCode::Unsupported,
        ),
        (
            refused(|s| research(s).steve_tools = true),
            ErrorCode::Unsupported,
        ),
    ];
    for (i, (spec, code)) in cases.into_iter().enumerate() {
        let op = format!("op_{i}");
        let e = n.start_job(now_cmd(&op, spec)).await.unwrap_err();
        assert_eq!(e.code, code, "case {i}: {e:?}");
        // nothing was accepted
        assert!(n.operation(OperationId::new(op).unwrap()).await.is_err());
    }
    assert_eq!(t.spawns(), 0);
    // a think job is not this step's
    let think = steve_node_api::testkit::build::job("j_t", "a_t", "q");
    let e = n.start_job(now_cmd("op_t", think)).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::Unsupported);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn shutdown_stops_a_running_job_and_says_so() {
    let t = T::new();
    let n = t.node();
    n.start_job(now_cmd(
        "op_j",
        scripted("a_1", "m", "q\n@msg m1\n@say 写到一半\n@child\n@wait"),
    ))
    .await
    .unwrap();
    until("the adapter waits", || !t.read("pid").is_empty()).await;
    let child = pid_of(&t.log("pid"));
    n.shutdown().await;
    // its processes are gone, so it is over (not unknown): interrupted by this Node
    assert!(!alive(child));
    let r = n
        .operation(OperationId::new("op_j").unwrap())
        .await
        .unwrap();
    let Observed::Finished {
        outcome,
        result: Some(OperationResult::Execution(c)),
        ..
    } = r.state
    else {
        panic!("{:?}", r.state)
    };
    assert_eq!(outcome, Outcome::Interrupted);
    assert_eq!(c.stop_reason, "node_shutdown");
    assert_eq!(
        c.result,
        vec![Part::Text {
            text: "写到一半".into()
        }]
    );
}

/// The completion an operation finished with (from its receipt only: no Hub needed).
async fn finished(n: &LocalNode, op: &str) -> Completion {
    let t0 = Instant::now();
    loop {
        let r = n.operation(OperationId::new(op).unwrap()).await.unwrap();
        if let Observed::Finished {
            result: Some(OperationResult::Execution(c)),
            ..
        } = r.state
        {
            return c;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(20),
            "{op}: {:?}",
            r.state
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// A job that writes a little, then (while `<log>.hold` exists) waits with a child whose pid goes
/// to `<log>.pid`, then answers.
const HOLDING: &str = "q\n@msg m1\n@say 写到一半\n@hold\n@msg m2\n@say 好";

fn holding(t: &T, attempt: &str) -> JobSpec {
    std::fs::write(t.log("hold"), "").unwrap();
    scripted(attempt, "m", HOLDING)
}

fn pid_of(path: &Path) -> i32 {
    std::fs::read_to_string(path)
        .unwrap()
        .trim()
        .parse()
        .unwrap()
}

/// Runs in a child process (see `a_job_left_unknown_by_a_crash_is_abandoned_and_frees_its_slot`).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn child_research_node() {
    let Ok(root) = std::env::var("STEVE_RESEARCH_CHILD_ROOT") else {
        return;
    };
    let t = T::at(Path::new(&root));
    let n = t.scripted_node(1, Arc::new(Files::default()));
    n.start_job(now_cmd("op_j", holding(&t, "a_1")))
        .await
        .unwrap();
    until("the adapter waits", || !t.read("pid").is_empty()).await;
    std::fs::write(t.root.join("ready"), "").unwrap();
    tokio::time::sleep(Duration::from_secs(120)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_job_left_unknown_by_a_crash_is_abandoned_and_frees_its_slot() {
    let t = T::new();
    t.write_adapter();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "child_research_node", "--nocapture"])
        .env("STEVE_RESEARCH_CHILD_ROOT", &t.root)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    until("the child node runs its job", || {
        t.root.join("ready").exists()
    })
    .await;
    child.kill().unwrap();
    child.wait().unwrap();
    // the adapter's group outlived the Node that ran it
    let harness_child = pid_of(&t.log("pid"));
    struct Cleanup(i32);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            unsafe {
                libc::kill(self.0, libc::SIGKILL);
            }
        }
    }
    let _cleanup = Cleanup(harness_child);
    assert!(alive(harness_child));
    std::fs::remove_file(t.log("hold")).unwrap();
    let n = t.scripted_node(1, Arc::new(Files::default()));
    let r = n
        .operation(OperationId::new("op_j").unwrap())
        .await
        .unwrap();
    assert_eq!(r.state.tag(), StateTag::Unknown);
    // the unknown job holds the one slot
    let e = n
        .start_job(now_cmd("op_k", scripted("a_2", "m", HOLDING)))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Exhausted);
    // the Hub gives it up: its group is stopped first, then it ends as interrupted
    let ab = n
        .abandon(now_cmd(
            "op_ab",
            Abandon {
                target: OperationId::new("op_j").unwrap(),
                reason: "node crashed".into(),
            },
        ))
        .await
        .unwrap();
    assert_eq!(ab.state, Observed::Accepted);
    let t0 = Instant::now();
    loop {
        let r = n
            .operation(OperationId::new("op_ab").unwrap())
            .await
            .unwrap();
        if r.state.is_finished() {
            assert!(
                matches!(
                    r.state,
                    Observed::Finished {
                        outcome: Outcome::Succeeded,
                        ..
                    }
                ),
                "{:?}",
                r.state
            );
            break;
        }
        assert!(t0.elapsed() < Duration::from_secs(20), "{:?}", r.state);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(!alive(harness_child));
    let c = t.end_of(&n, "op_j").await;
    assert_eq!(c.outcome, Outcome::Interrupted);
    assert_eq!(c.stop_reason, "abandoned");
    assert_eq!(c.job.unwrap().end, JobEnd::Stopped);
    // the slot is free: the next job runs
    n.start_job(now_cmd("op_k2", scripted("a_3", "m", HOLDING)))
        .await
        .unwrap();
    let c = t.end_of(&n, "op_k2").await;
    assert_eq!(c.outcome, Outcome::Succeeded, "{c:?}");
    n.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_job_stops_when_the_hub_is_unreachable_past_its_lease() {
    let t = T::new();
    // never connected: the Hub is unreachable from the start
    let n = LocalNode::open(t.scripted_config(1)).unwrap();
    n.enable_research(t.research());
    let mut spec = holding(&t, "a_1");
    spec.run.limits.offline_until = Timestamp(wall() + 1_500);
    n.start_job(now_cmd("op_j", spec)).await.unwrap();
    let c = finished(&n, "op_j").await;
    assert_eq!(c.outcome, Outcome::TimedOut, "{c:?}");
    assert_eq!(c.stop_reason, "offline");
    assert_eq!(c.error.unwrap().code, ErrorCode::Offline);
    assert!(c.job.unwrap().end.fits(Outcome::TimedOut));
    assert!(!alive(pid_of(&t.log("pid"))));
    n.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_job_stops_at_its_output_cap() {
    let t = T::new();
    let n = t.node();
    // the child is known before the output that ends the job, so its cleanup is what is checked
    let long = "写到一半".repeat(300);
    let mut spec = scripted(
        "a_1",
        "m",
        &format!("q\n@child\n@msg m1\n@say {long}\n@wait"),
    );
    // the handshake fits, the draft (3600 bytes) does not
    spec.run.limits.max_output_bytes = 2_000;
    n.start_job(now_cmd("op_j", spec)).await.unwrap();
    let c = t.end_of(&n, "op_j").await;
    assert_eq!(c.outcome, Outcome::Cancelled, "{c:?}");
    assert_eq!(c.stop_reason, "max_output");
    assert_eq!(c.error.unwrap().code, ErrorCode::Exhausted);
    assert_eq!(c.job.unwrap().end, JobEnd::Stopped);
    let child = pid_of(&t.log("pid"));
    until("the adapter's child is gone", || !alive(child)).await;
    n.shutdown().await;
}

/// Attachments that come after `delay` (or never).
struct Slow(Duration, Files);

#[async_trait::async_trait]
impl HubFiles for Slow {
    async fn fetch(
        &self,
        a: FileAccess,
        id: ArtifactId,
        offset: u64,
        max: u32,
    ) -> ApiResult<ReadChunk> {
        tokio::time::sleep(self.0).await;
        self.1.fetch(a, id, offset, max).await
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

/// A holding job whose prompt has one file the Hub serves after `delay`.
fn with_file(t: &T, delay: Duration) -> (Arc<Slow>, JobSpec) {
    let files = Slow(delay, Files::default());
    let a = files.1.artifact("art_1", "text/plain", b"notes");
    let mut spec = holding(t, "a_1");
    spec.run.prompt.push(Part::File {
        artifact: a,
        name: ".steve/attachments/notes.txt".into(),
    });
    (Arc::new(files), spec)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stalled_attachment_fetch_yields_to_cancel_shutdown_and_deadline() {
    let hang = Duration::from_secs(3600);
    // a cancel during the fetch ends it at once, before any adapter
    let t = T::new();
    let (files, spec) = with_file(&t, hang);
    let n = t.scripted_node(1, files);
    n.start_job(now_cmd("op_j", spec)).await.unwrap();
    n.cancel(now_cmd(
        "op_c",
        Cancel {
            target: OperationId::new("op_j").unwrap(),
            reason: "stop".into(),
        },
    ))
    .await
    .unwrap();
    let c = t.end_of(&n, "op_j").await;
    assert_eq!(c.outcome, Outcome::Cancelled, "{c:?}");
    assert_eq!(c.job.unwrap().end, JobEnd::Stopped);
    assert_eq!(t.spawns(), 0);
    n.shutdown().await;

    // a shutdown does not wait for the Hub
    let t = T::new();
    let (files, spec) = with_file(&t, hang);
    let n = t.scripted_node(1, files);
    n.start_job(now_cmd("op_j", spec)).await.unwrap();
    tokio::time::sleep(Duration::from_millis(200)).await;
    tokio::time::timeout(Duration::from_secs(5), n.shutdown())
        .await
        .expect("shutdown waits for a stalled fetch");
    let c = finished(&n, "op_j").await;
    assert_eq!(c.outcome, Outcome::Interrupted, "{c:?}");
    assert_eq!(c.stop_reason, "node_shutdown");
    assert_eq!(t.spawns(), 0);

    // the deadline holds during the fetch
    let t = T::new();
    let (files, mut spec) = with_file(&t, hang);
    spec.run.limits.deadline = Timestamp(wall() + 800);
    let n = t.scripted_node(1, files);
    n.start_job(now_cmd("op_j", spec)).await.unwrap();
    let c = t.end_of(&n, "op_j").await;
    assert_eq!(c.outcome, Outcome::TimedOut, "{c:?}");
    assert_eq!(
        c.job.unwrap().end,
        JobEnd::Budget {
            hit: BudgetHit::Time
        }
    );
    assert_eq!(t.spawns(), 0);
    n.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_slow_fetch_does_not_move_the_jobs_deadline() {
    let t = T::new();
    let (files, mut spec) = with_file(&t, Duration::from_millis(1_500));
    let deadline = wall() + 3_000;
    spec.run.limits.deadline = Timestamp(deadline);
    let n = t.scripted_node(1, files);
    n.start_job(now_cmd("op_j", spec)).await.unwrap();
    let c = t.end_of(&n, "op_j").await;
    let late = wall() - deadline;
    assert_eq!(c.outcome, Outcome::TimedOut, "{c:?}");
    assert_eq!(t.spawns(), 1);
    // ended at the job's deadline, not the fetch's time past it
    assert!(late < 1_000, "{late} ms past the deadline");
    assert!(!alive(pid_of(&t.log("pid"))));
    n.shutdown().await;
}
