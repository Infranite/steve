//! LocalNode research jobs against a scripted fake harness (no model is called): one attempt per
//! job with the read-only layers of steve-research, its facts in Steve's events.

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
            jobs: Default::default(),
        }
    }

    /// The research setup a host gives: these harnesses, this home's secrets off limits.
    fn research(&self, claude: &Path, codex: Option<&Path>) -> steve_research::ResearchConfig {
        let mut c = steve_research::ResearchConfig::new(claude, self.root.join("unused"));
        c.home = self.home.clone();
        c.claude_dir = self.root.join("claude");
        c.files_root = self.root.join("files");
        c.codex_executable = codex.map(Path::to_path_buf);
        c
    }

    fn node(&self, claude: &Path) -> LocalNode {
        self.node_with(claude, 2, Arc::new(Files::default()))
    }

    fn node_with(&self, claude: &Path, jobs: u32, files: Arc<dyn HubFiles>) -> LocalNode {
        let n = LocalNode::open(self.config(jobs)).unwrap();
        n.connect(self.sink.clone(), Some(files));
        n.enable_research(self.research(claude, None));
        n
    }

    /// A fake harness: `body` is shell run after its arguments went to `args.txt` (one run per
    /// block) and a line to `spawns`.
    fn harness(&self, name: &str, body: &str) -> PathBuf {
        let p = self.root.join(name);
        let script = format!(
            "#!/bin/sh\necho x >> '{spawns}'\nprintf '%s\\n' \"$@\" >> '{args}'\necho @@END@@ >> '{args}'\n{body}\n",
            spawns = self.root.join("spawns").display(),
            args = self.root.join("args.txt").display()
        );
        std::fs::write(&p, script).unwrap();
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        p
    }

    fn spawns(&self) -> usize {
        std::fs::read_to_string(self.root.join("spawns"))
            .unwrap_or_default()
            .lines()
            .count()
    }

    /// The arguments of each run, in order.
    fn runs(&self) -> Vec<Vec<String>> {
        std::fs::read_to_string(self.root.join("args.txt"))
            .unwrap_or_default()
            .split("@@END@@\n")
            .filter(|r| !r.trim().is_empty())
            .map(|r| r.lines().map(String::from).collect())
            .collect()
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
                harness: "claude-code".into(),
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

/// Shell that prints these stream-json lines.
fn emit(lines: &[String]) -> String {
    lines
        .iter()
        .map(|l| format!("printf '%s\\n' '{}'", l.replace('\'', "'\\''")))
        .collect::<Vec<_>>()
        .join("\n")
}
fn init() -> String {
    r#"{"type":"system","subtype":"init","session_id":"sess-1","model":"claude-sonnet-5-5"}"#.into()
}
fn start(id: &str) -> String {
    format!(
        r#"{{"type":"stream_event","event":{{"type":"message_start","message":{{"id":"{id}"}}}}}}"#
    )
}
fn text(t: &str) -> String {
    json!({"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":t}}}).to_string()
}
fn tool(msg: &str, id: &str, name: &str, input: serde_json::Value) -> String {
    json!({"type":"assistant","message":{"id":msg,"content":[{"type":"tool_use","id":id,"name":name,"input":input}]}}).to_string()
}
fn tool_result(id: &str) -> String {
    json!({"type":"user","message":{"role":"user","content":[{"type":"tool_result","tool_use_id":id,"content":"1\tfn add() {}"}]}}).to_string()
}
fn result(t: &str) -> String {
    json!({"type":"result","subtype":"success","is_error":false,"result":t,"session_id":"sess-1",
        "total_cost_usd":0.25,
        "modelUsage":{"claude-sonnet-5-5":{"inputTokens":10,"outputTokens":5,"cacheReadInputTokens":100,"costUSD":0.25}}})
    .to_string()
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
    let exe = t.harness(
        "claude.sh",
        &emit(&[
            init(),
            start("m1"),
            text("我先看看"),
            tool("m1", "t1", "Read", json!({"file_path": lib})),
            tool_result("t1"),
            start("m2"),
            text("add 什么都不做，"),
            text("见 src/lib.rs:1。"),
            result(answer),
        ]),
    );
    let n = t.node(&exe);
    let first = now_cmd("op_j", job("a_1", "claude-sonnet-5-5", "add 怎么工作？"));
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
    // one process, the model given, the readable root as an extra directory, no resume
    assert_eq!(t.spawns(), 1);
    let args = &t.runs()[0];
    let i = args.iter().position(|a| a == "--model").unwrap();
    assert_eq!(args[i + 1], "claude-sonnet-5-5");
    assert!(args.contains(&format!("--add-dir={}", t.root.join("work/docs").display())));
    assert!(!args.contains(&"--resume".to_string()));
    assert!(args.contains(&"--tools=Read,Grep,Glob".to_string()));
    let bodies = t.bodies();
    // the draft: a new message replaces the first one
    let started: Vec<&String> = bodies
        .iter()
        .filter_map(|b| match b {
            EventBody::MessageStarted { message, .. } => Some(message),
            _ => None,
        })
        .collect();
    assert_eq!(started, ["a_1.m1", "a_1.m2"]);
    let second: String = bodies
        .iter()
        .filter_map(|b| match b {
            EventBody::TextDelta { message, text, .. } if message == "a_1.m2" => Some(text.clone()),
            _ => None,
        })
        .collect();
    assert_eq!(second, "add 什么都不做，见 src/lib.rs:1。");
    // the tool row, running then done
    let phases: Vec<(ToolPhase, ToolCategory, Option<String>)> = bodies
        .iter()
        .filter_map(|b| match b {
            EventBody::ToolChanged { tool } => {
                Some((tool.phase, tool.category, tool.target.clone()))
            }
            _ => None,
        })
        .collect();
    assert_eq!(
        phases,
        [
            (
                ToolPhase::Running,
                ToolCategory::Read,
                Some("src/lib.rs".into())
            ),
            (
                ToolPhase::Succeeded,
                ToolCategory::Read,
                Some("src/lib.rs".into())
            ),
        ]
    );
    // the tokens and dollars of the attempt, on its model
    let usage: Vec<&UsageReport> = bodies
        .iter()
        .filter_map(|b| match b {
            EventBody::Usage { report } => Some(report),
            _ => None,
        })
        .collect();
    assert_eq!(usage.len(), 1);
    assert_eq!(usage[0].model.as_deref(), Some("claude-sonnet-5-5"));
    assert_eq!(usage[0].measurement, Measurement::Final);
    assert_eq!(
        (
            usage[0].tokens.input,
            usage[0].tokens.output,
            usage[0].tokens.cache_read
        ),
        (Some(10), Some(5), Some(100))
    );
    assert_eq!(usage[0].cost.as_ref().unwrap().decimal, "0.25");
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
    let exe = t.harness(
        "claude.sh",
        &format!(
            "{}\nsleep 3\ntouch '{}'",
            emit(&[
                init(),
                start("m1"),
                text("我先看看"),
                tool(
                    "m1",
                    "t1",
                    "Write",
                    json!({"file_path": "keep.txt", "content": "x"})
                ),
            ]),
            survived.display()
        ),
    );
    let n = t.node(&exe);
    let t0 = Instant::now();
    n.start_job(now_cmd("op_j", job("a_1", "claude-sonnet-5-5", "q")))
        .await
        .unwrap();
    let c = t.end_of(&n, "op_j").await;
    assert!(t0.elapsed() < Duration::from_secs(3), "killed promptly");
    assert_eq!(c.outcome, Outcome::Failed);
    assert_eq!(c.error.as_ref().unwrap().code, ErrorCode::Denied);
    assert_eq!(
        c.job.unwrap().end,
        JobEnd::Violated {
            rule: ViolationRule::Tool,
            tool: "Write".into(),
            target: None
        }
    );
    // what it wrote before stays
    assert_eq!(
        c.result,
        vec![Part::Text {
            text: "我先看看".into()
        }]
    );
    tokio::time::sleep(Duration::from_secs(4)).await;
    assert!(!survived.exists(), "the harness's process group was killed");
    assert_eq!(
        std::fs::read_to_string(t.root.join("work/proj/keep.txt")).unwrap(),
        "orig"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn budgets_end_a_job_with_what_it_wrote() {
    let t = T::new();
    let lib = t.root.join("work/proj/src/lib.rs").display().to_string();
    let read = |id: &str| tool("m1", id, "Read", json!({"file_path": lib}));
    let exe = t.harness(
        "claude.sh",
        &format!(
            "{}\nsleep 30",
            emit(&[
                init(),
                start("m1"),
                text("读了一半"),
                read("t1"),
                tool_result("t1"),
                read("t2")
            ])
        ),
    );
    let n = t.node(&exe);
    let mut tools = job("a_tools", "claude-sonnet-5-5", "q");
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

    let exe = t.harness(
        "turns.sh",
        &format!(
            "{}\nsleep 30",
            emit(&[init(), start("m1"), text("一"), start("m2")])
        ),
    );
    n.enable_research(t.research(&exe, None));
    let n2 = n;
    let mut turns = job("a_turns", "claude-sonnet-5-5", "q");
    research(&mut turns).budget.max_turns = Some(1);
    n2.start_job(now_cmd("op_turns", turns)).await.unwrap();
    let c = t.end_of(&n2, "op_turns").await;
    assert_eq!(
        c.job.unwrap().end,
        JobEnd::Budget {
            hit: BudgetHit::Turns
        }
    );

    // silent for idle_ms: stuck
    let mut idle = job("a_idle", "claude-sonnet-5-5", "q");
    research(&mut idle).budget.idle_ms = 300;
    research(&mut idle).budget.max_turns = Some(5);
    n2.start_job(now_cmd("op_idle", idle)).await.unwrap();
    let c = t.end_of(&n2, "op_idle").await;
    assert_eq!(c.outcome, Outcome::TimedOut);
    assert_eq!(
        c.job.unwrap().end,
        JobEnd::Budget {
            hit: BudgetHit::Idle
        }
    );

    // the job's deadline
    let mut late = job("a_late", "claude-sonnet-5-5", "q");
    research(&mut late).budget.max_turns = Some(5);
    late.run.limits.deadline = Timestamp(wall() + 700);
    n2.start_job(now_cmd("op_late", late)).await.unwrap();
    let c = t.end_of(&n2, "op_late").await;
    assert_eq!(c.outcome, Outcome::TimedOut);
    assert_eq!(
        c.job.unwrap().end,
        JobEnd::Budget {
            hit: BudgetHit::Time
        }
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_follow_up_resumes_the_session_on_the_same_model() {
    let t = T::new();
    let exe = t.harness(
        "claude.sh",
        &emit(&[init(), start("m1"), text("接着说"), result("接着说")]),
    );
    let n = t.node(&exe);
    let mut spec = job("a_2", "claude-opus-5-5", "再细一点");
    spec.run.continuation = Some(ResumeHandle::new("sess-0").unwrap());
    n.start_job(now_cmd("op_j", spec)).await.unwrap();
    let c = t.end_of(&n, "op_j").await;
    assert_eq!(c.outcome, Outcome::Succeeded);
    let args = &t.runs()[0];
    let i = args.iter().position(|a| a == "--resume").unwrap();
    assert_eq!(args[i + 1], "sess-0");
    let m = args.iter().position(|a| a == "--model").unwrap();
    assert_eq!(args[m + 1], "claude-opus-5-5");
    assert_eq!(args[1], "再细一点");
    // the session to continue next time
    assert_eq!(c.continuation, Some(ResumeHandle::new("sess-1").unwrap()));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn one_model_only_a_failure_is_the_attempts_end() {
    let t = T::new();
    let exe = t.harness("claude.sh", "echo 'overloaded' >&2\nexit 1");
    let n = t.node(&exe);
    n.start_job(now_cmd("op_j", job("a_1", "claude-sonnet-5-5", "q")))
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

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn codex_is_never_wrapped_in_sandbox_exec() {
    let t = T::new();
    let lines = [
        json!({"type": "item.completed", "item": {"id": "i0", "type": "agent_message", "text": "看过了"}}),
        json!({"type": "turn.completed", "usage": {"input_tokens": 1000, "cached_input_tokens": 400, "output_tokens": 50}}),
    ]
    .map(|v| v.to_string());
    let codex = t.harness("codex.sh", &emit(&lines));
    let claude = t.harness("claude.sh", "exit 1");
    let n = LocalNode::open(t.config(2)).unwrap();
    n.connect(t.sink.clone(), None);
    n.enable_research(t.research(&claude, Some(&codex)));
    let mut spec = job("a_1", "gpt-6-luna", "q");
    spec.run.choice.harness = "codex".into();
    spec.run.policy.sandbox = SandboxRequirement::AllowFallback {
        preferred: vec!["fs-readonly".into()],
        acceptable: vec![],
    };
    n.start_job(now_cmd("op_j", spec.clone())).await.unwrap();
    let c = t.end_of(&n, "op_j").await;
    assert_eq!(c.outcome, Outcome::Succeeded, "{c:?}");
    assert!(!c.job.unwrap().sandboxed);
    assert_eq!(c.continuation, None);
    // codex itself ran, not sandbox-exec around it
    assert_eq!(t.runs()[0][0], "exec");
    let usage: Vec<UsageReport> = t
        .bodies()
        .into_iter()
        .filter_map(|b| match b {
            EventBody::Usage { report } => Some(report),
            _ => None,
        })
        .collect();
    assert_eq!(
        (usage[0].tokens.input, usage[0].tokens.cache_read),
        (Some(600), Some(400))
    );
    // required, it is refused before anything runs; nor does codex continue a session
    spec.run.attempt = AttemptId::new("a_2").unwrap();
    spec.run.policy.sandbox = SandboxRequirement::Required {
        guarantees: vec!["fs-readonly".into()],
    };
    let e = n
        .start_job(now_cmd("op_j2", spec.clone()))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Unsupported);
    spec.run.policy.sandbox = SandboxRequirement::None;
    spec.run.continuation = Some(ResumeHandle::new("th-1").unwrap());
    let e = n.start_job(now_cmd("op_j3", spec)).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::Invalid);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_preferred_sandbox_is_used_where_this_machine_has_one() {
    let t = T::new();
    let exe = t.harness(
        "claude.sh",
        &emit(&[init(), start("m1"), text("好"), result("好")]),
    );
    let n = t.node(&exe);
    let mut spec = job("a_1", "claude-sonnet-5-5", "q");
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
    // the harness reads the copy it was told about (the run's files directory)
    let exe = t.harness(
        "claude.sh",
        &format!(
            "cat '{}'/*/notes.txt > '{}'\n{}",
            t.root.join("files").display(),
            seen.display(),
            emit(&[init(), start("m1"), text("看了"), result("看了")])
        ),
    );
    let n = LocalNode::open(t.config(2)).unwrap();
    n.connect(t.sink.clone(), Some(files.clone()));
    n.enable_research(t.research(&exe, None));
    let mut spec = job("a_1", "claude-sonnet-5-5", "附件说了什么？");
    spec.run.prompt.push(Part::File {
        artifact: notes,
        name: ".steve/attachments/notes.txt".into(),
    });
    spec.run.prompt.push(Part::Image { artifact: png });
    n.start_job(now_cmd("op_j", spec)).await.unwrap();
    let c = t.end_of(&n, "op_j").await;
    assert_eq!(c.outcome, Outcome::Succeeded, "{c:?}");
    assert_eq!(std::fs::read_to_string(&seen).unwrap(), "hello notes");
    // the image goes inline: the prompt is a stream-json message on stdin
    let args = &t.runs()[0];
    assert!(
        args.windows(2)
            .any(|w| w == ["--input-format", "stream-json"])
    );
    // the copies are gone with the job
    until("files removed", || {
        std::fs::read_dir(t.root.join("files")).map_or(true, |mut d| d.next().is_none())
    })
    .await;

    let exe = t.harness(
        "secret.sh",
        &format!(
            "{}\nsleep 5",
            emit(&[
                init(),
                start("m1"),
                tool("m1", "t1", "Read", json!({"file_path": "~/.ssh/id_rsa"}))
            ])
        ),
    );
    n.enable_research(t.research(&exe, None));
    n.start_job(now_cmd("op_s", job("a_2", "claude-sonnet-5-5", "q")))
        .await
        .unwrap();
    let c = t.end_of(&n, "op_s").await;
    assert_eq!(
        c.job.unwrap().end,
        JobEnd::Violated {
            rule: ViolationRule::Sensitive,
            tool: "Read".into(),
            target: Some("~/.ssh/id_rsa".into())
        }
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
    let copy = t.root.join("mcp-copy.json");
    let pid = t.root.join("pid");
    // keeps its MCP config, then writes a little and waits (with a child) until it is stopped
    let exe = t.harness(
        "claude.sh",
        &format!(
            "for a in \"$@\"; do case \"$a\" in --mcp-config=*) cp \"${{a#--mcp-config=}}\" '{copy}';; esac; done\n{}\nsleep 30 &\necho $! > '{pid}'\nwait",
            emit(&[init(), start("m1"), text("我先看看")]),
            copy = copy.display(),
            pid = pid.display(),
        ),
    );
    let n = t.node(&exe);
    let tools = Arc::new(Tools::default());
    n.connect_tools(tools.clone()).await.unwrap();
    let mut spec = job("a_1", "claude-sonnet-5-5", "q");
    research(&mut spec).steve_tools = true;
    n.start_job(now_cmd("op_j", spec)).await.unwrap();
    until("the harness waits", || pid.exists() && copy.exists()).await;
    let cfg: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&copy).unwrap()).unwrap();
    let server = &cfg["mcpServers"]["steve"];
    let url = server["url"].as_str().unwrap().to_string();
    let token = server["headers"]["Authorization"]
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
    let child: i32 = std::fs::read_to_string(&pid)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
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
    until("the harness's child is gone", || !alive(child)).await;
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
    let exe = t.harness("claude.sh", "exit 0");
    let n = LocalNode::open(t.config(0)).unwrap();
    n.enable_research(t.research(&exe, None));
    let e = n
        .start_job(now_cmd("op_0", job("a_0", "m", "q")))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Unsupported);
    let t = T::new();
    let exe = t.harness("claude.sh", "exit 0");
    let n = t.node(&exe);
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
        (refused(|s| s.run.choice.model = None), ErrorCode::Invalid),
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
    let pid = t.root.join("pid");
    let exe = t.harness(
        "claude.sh",
        &format!(
            "{}\nsleep 30 &\necho $! > '{pid}'\nwait",
            emit(&[init(), start("m1"), text("写到一半")]),
            pid = pid.display(),
        ),
    );
    let n = t.node(&exe);
    n.start_job(now_cmd("op_j", job("a_1", "claude-sonnet-5-5", "q")))
        .await
        .unwrap();
    until("the harness waits", || pid.exists()).await;
    let child: i32 = std::fs::read_to_string(&pid)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
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

/// A harness that writes a little, then (while `hold` exists) waits with a child whose pid goes
/// to `pid`, then answers.
fn holding(t: &T) -> (PathBuf, PathBuf, PathBuf) {
    let (hold, pid) = (t.root.join("hold"), t.root.join("pid"));
    let exe = t.harness(
        "claude.sh",
        &format!(
            "{}\nif [ -e '{hold}' ]; then sleep 30 & echo $! > '{pid}'; wait; fi\n{}",
            emit(&[init(), start("m1"), text("写到一半")]),
            emit(&[result("好")]),
            hold = hold.display(),
            pid = pid.display(),
        ),
    );
    std::fs::write(&hold, "").unwrap();
    (exe, hold, pid)
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
    let n = t.node_with(&t.root.join("claude.sh"), 1, Arc::new(Files::default()));
    n.start_job(now_cmd("op_j", job("a_1", "claude-sonnet-5-5", "q")))
        .await
        .unwrap();
    until("the harness waits", || t.root.join("pid").exists()).await;
    std::fs::write(t.root.join("ready"), "").unwrap();
    tokio::time::sleep(Duration::from_secs(120)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_job_left_unknown_by_a_crash_is_abandoned_and_frees_its_slot() {
    let t = T::new();
    let (exe, hold, pid) = holding(&t);
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
    // the harness's group outlived the Node that ran it
    let harness_child = pid_of(&pid);
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
    std::fs::remove_file(&hold).unwrap();
    let n = t.node_with(&exe, 1, Arc::new(Files::default()));
    let r = n
        .operation(OperationId::new("op_j").unwrap())
        .await
        .unwrap();
    assert_eq!(r.state.tag(), StateTag::Unknown);
    // the unknown job holds the one slot
    let e = n
        .start_job(now_cmd("op_k", job("a_2", "claude-sonnet-5-5", "q")))
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
    n.start_job(now_cmd("op_k2", job("a_3", "claude-sonnet-5-5", "q")))
        .await
        .unwrap();
    let c = t.end_of(&n, "op_k2").await;
    assert_eq!(c.outcome, Outcome::Succeeded, "{c:?}");
    n.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_job_stops_when_the_hub_is_unreachable_past_its_lease() {
    let t = T::new();
    let (exe, _hold, pid) = holding(&t);
    // never connected: the Hub is unreachable from the start
    let n = LocalNode::open(t.config(1)).unwrap();
    n.enable_research(t.research(&exe, None));
    let mut spec = job("a_1", "claude-sonnet-5-5", "q");
    spec.run.limits.offline_until = Timestamp(wall() + 1_500);
    n.start_job(now_cmd("op_j", spec)).await.unwrap();
    let c = finished(&n, "op_j").await;
    assert_eq!(c.outcome, Outcome::TimedOut, "{c:?}");
    assert_eq!(c.stop_reason, "offline");
    assert_eq!(c.error.unwrap().code, ErrorCode::Offline);
    assert!(c.job.unwrap().end.fits(Outcome::TimedOut));
    assert!(!alive(pid_of(&pid)));
    n.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_job_stops_at_its_output_cap() {
    let t = T::new();
    let (exe, _hold, pid) = holding(&t);
    let n = t.node(&exe);
    let mut spec = job("a_1", "claude-sonnet-5-5", "q");
    // "写到一半" is 12 bytes
    spec.run.limits.max_output_bytes = 8;
    n.start_job(now_cmd("op_j", spec)).await.unwrap();
    let c = t.end_of(&n, "op_j").await;
    assert_eq!(c.outcome, Outcome::Cancelled, "{c:?}");
    assert_eq!(c.stop_reason, "max_output");
    assert_eq!(c.error.unwrap().code, ErrorCode::Exhausted);
    assert_eq!(c.job.unwrap().end, JobEnd::Stopped);
    // stopped as it wrote, perhaps before it started its child
    until("the harness's child is gone", || {
        !pid.exists() || !alive(pid_of(&pid))
    })
    .await;
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

/// A job whose prompt has one file the Hub serves after `delay`.
fn with_file(delay: Duration) -> (Arc<Slow>, JobSpec) {
    let files = Slow(delay, Files::default());
    let a = files.1.artifact("art_1", "text/plain", b"notes");
    let mut spec = job("a_1", "claude-sonnet-5-5", "q");
    spec.run.prompt.push(Part::File {
        artifact: a,
        name: ".steve/attachments/notes.txt".into(),
    });
    (Arc::new(files), spec)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_stalled_attachment_fetch_yields_to_cancel_shutdown_and_deadline() {
    let hang = Duration::from_secs(3600);
    // a cancel during the fetch ends it at once, before any harness
    let t = T::new();
    let (exe, _hold, _pid) = holding(&t);
    let (files, spec) = with_file(hang);
    let n = t.node_with(&exe, 1, files);
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
    let (exe, _hold, _pid) = holding(&t);
    let (files, spec) = with_file(hang);
    let n = t.node_with(&exe, 1, files);
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
    let (exe, _hold, _pid) = holding(&t);
    let (files, mut spec) = with_file(hang);
    spec.run.limits.deadline = Timestamp(wall() + 800);
    let n = t.node_with(&exe, 1, files);
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
    let (exe, _hold, pid) = holding(&t);
    let (files, mut spec) = with_file(Duration::from_millis(1_500));
    let deadline = wall() + 3_000;
    spec.run.limits.deadline = Timestamp(deadline);
    let n = t.node_with(&exe, 1, files);
    n.start_job(now_cmd("op_j", spec)).await.unwrap();
    let c = t.end_of(&n, "op_j").await;
    let late = wall() - deadline;
    assert_eq!(c.outcome, Outcome::TimedOut, "{c:?}");
    assert_eq!(t.spawns(), 1);
    // ended at the job's deadline, not the fetch's time past it
    assert!(late < 1_000, "{late} ms past the deadline");
    assert!(!alive(pid_of(&pid)));
    n.shutdown().await;
}
