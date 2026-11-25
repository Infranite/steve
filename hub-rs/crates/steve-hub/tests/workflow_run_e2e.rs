//! WF-3 end-to-end acceptance suite covering all 13 ledger/lifecycle/gate scenarios (§4.1 WF-3),
//! Git tree/commit output fingerprint verification for cross-version lazy hydration (#503),
//! PR #544 coordinator placement rules (`noExecutor = ask | steve`), goal superseding, and
//! real `steve-hub --workflow-step` subprocess execution.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::{Arc, Mutex, Once};
use std::time::Duration;

use serde_json::{Value, json};
use steve_core::{
    AgentRuntime, Answered, Core, CoreConfig, DecisionCommand, Flushed, ModelGateway,
    PermissionAnswer, Prompted, Released, Running, Stop, Tier, TurnEnd, WorkflowStepExecutor,
    workflow_run::{WF_RUN_SUBJECT, budget_settings},
};
use steve_hub::wire::core::CoreMcp;
use steve_hub::wire::workspaces::Workspaces;
use steve_hub::workflow_step::StepRunner;
use steve_mcp::Backend as _;
use steve_modules::Caller;
use steve_store::testkit::SqliteFixtures;
use steve_store::workflow_runs::{
    ReviewGateFailure, ReviewGateVerdict, WfAttemptStatus, WfCallStatus, WfChildOutcome,
    WfPauseReason, WfRunStatus, WorkflowRuns,
};
use steve_store::{
    NewBot, NewItem, NewProject, NewSession, NoPublisher, Session, Store, StoreError, UserRows, WS,
    WorkspaceSettings, bot_admin, bot_dispatch,
};
use steve_types::{
    CoordinatorDeferSource, DecisionPrompt, DecisionStatus, ItemPhase, NO_EXECUTOR,
    NoExecutorPolicy, PlacementReason, PlacementRejection, WfBudgetScope,
};
use steve_workflow_api::{StepFrame, StepInput};

/// Fast in-process JS executor wrapping `steve_workflow_js::run_step` for deterministic tests.
struct InProcessJsExecutor;

#[async_trait::async_trait]
impl WorkflowStepExecutor for InProcessJsExecutor {
    async fn execute(&self, input: &StepInput) -> StepFrame {
        steve_workflow_js::run_step(input)
    }
}

struct NoModels;

#[async_trait::async_trait]
impl ModelGateway for NoModels {
    async fn call(
        &self,
        _: &str,
        _: Tier,
        _: Duration,
        _: &steve_core::gateway::ThinkTools,
    ) -> steve_core::Result<steve_core::gateway::Printed> {
        panic!("WF-3 acceptance test must not call an LLM")
    }
}

#[derive(Default)]
struct RtState {
    turning: HashSet<String>,
    attempts: HashMap<String, String>,
    next_attempt: u64,
    cancelled: Vec<String>,
    prompts: Vec<(String, String)>,
    placement_rejections: HashMap<String, PlacementRejection>,
}

struct TestRuntime {
    store: Store,
    st: Mutex<RtState>,
}

impl TestRuntime {
    fn new(store: Store) -> Self {
        Self {
            store,
            st: Mutex::new(RtState::default()),
        }
    }

    fn end_turn(&self, core: &Core, sid: &str, stop: &str) {
        let attempt = {
            let mut st = self.st.lock().unwrap();
            st.turning.remove(sid);
            let a = st.attempts.remove(sid);
            let _ = self.store.sessions().patch(
                sid,
                &steve_store::SessionPatch {
                    status: Some(if stop == "error" { "error" } else { "idle" }.into()),
                    ..Default::default()
                },
            );
            a
        };
        core.on_turn_end(TurnEnd {
            session_id: sid.into(),
            attempt,
            stop_reason: stop.into(),
            error: None,
        })
        .unwrap();
    }
}

impl AgentRuntime for TestRuntime {
    fn assess_placement_node(
        &self,
        _: &str,
        _: Option<&steve_store::Project>,
        node: &str,
        _: &steve_node_api::HarnessChoice,
        _: &str,
    ) -> steve_core::Result<Option<PlacementRejection>> {
        Ok(self
            .st
            .lock()
            .unwrap()
            .placement_rejections
            .get(node)
            .copied())
    }

    fn prompt(
        &self,
        session_id: &str,
        text: &str,
        _: &str,
        _: &[steve_core::repo::FileRef],
    ) -> steve_core::Result<Prompted> {
        let mut st = self.st.lock().unwrap();
        st.turning.insert(session_id.into());
        st.next_attempt += 1;
        let a = format!("att-{}", st.next_attempt);
        st.attempts.insert(session_id.into(), a);
        st.prompts.push((session_id.into(), text.into()));
        let _ = self.store.sessions().patch(
            session_id,
            &steve_store::SessionPatch {
                status: Some("running".into()),
                ..Default::default()
            },
        );
        Ok(Prompted::Sent)
    }

    fn running(&self, session_id: &str) -> Option<Running> {
        let st = self.st.lock().unwrap();
        st.attempts.get(session_id).map(|a| Running {
            session: session_id.into(),
            attempt: a.clone(),
        })
    }

    fn stop(&self, turn: &Running) -> steve_core::Result<Stop> {
        let mut st = self.st.lock().unwrap();
        if st.attempts.get(&turn.session).map(String::as_str) == Some(turn.attempt.as_str()) {
            st.turning.remove(&turn.session);
            st.attempts.remove(&turn.session);
            st.cancelled.push(turn.session.clone());
            let _ = self.store.sessions().patch(
                &turn.session,
                &steve_store::SessionPatch {
                    status: Some("idle".into()),
                    ..Default::default()
                },
            );
            Ok(Stop::Requested)
        } else {
            Ok(Stop::NotRunning)
        }
    }

    fn is_busy(&self, session_id: &str) -> bool {
        self.st.lock().unwrap().turning.contains(session_id)
    }

    fn release(&self, _: &str) -> steve_core::Result<Released> {
        Ok(Released::Gone)
    }

    fn answer_permission(&self, _: PermissionAnswer<'_>) -> steve_core::Result<Answered> {
        Ok(Answered::Applied)
    }

    fn hold(&self, _: bool) {}
    fn is_holding(&self) -> bool {
        false
    }
    fn running_turns(&self) -> usize {
        self.st.lock().unwrap().turning.len()
    }
    fn flush(&self, _: &str, _: Option<(&str, &str)>) -> Flushed {
        Flushed::Empty
    }
}

struct Env {
    dir: tempfile::TempDir,
    store: Store,
    rt: Arc<TestRuntime>,
    core: Core,
    mcp: CoreMcp,
}

impl Env {
    fn new(use_subprocess: bool) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().join("steve");
        std::fs::create_dir_all(&home).unwrap();
        let store = Store::open(home.join("steve.sqlite3"), NoPublisher).unwrap();
        store.ensure_workspace().unwrap();
        store
            .write(|tx| {
                UserRows(tx).insert(
                    "owner",
                    WS,
                    "sub_owner",
                    "Owner",
                    "owner",
                    steve_store::now(),
                )?;
                UserRows(tx).insert(
                    "member",
                    WS,
                    "sub_member",
                    "Member",
                    "member",
                    steve_store::now(),
                )?;
                tx.members().add(WS, "owner", "owner", 1)?;
                tx.members().add(WS, "member", "member", 2)?;
                Ok::<_, StoreError>(())
            })
            .unwrap();
        for (id, main, specialties) in [
            ("steve", true, vec![]),
            ("coder", false, vec!["coding".into(), "rust".into()]),
            ("reviewer", false, vec!["review".into(), "audit".into()]),
        ] {
            store
                .bots()
                .seed(
                    id,
                    main,
                    &NewBot {
                        name: id.into(),
                        harness: "fixture".into(),
                        specialties: Some(specialties),
                        ..Default::default()
                    },
                    WS,
                )
                .unwrap();
        }
        Self::attach_with(
            dir,
            home,
            store,
            if use_subprocess {
                Arc::new(real_step_runner())
            } else {
                Arc::new(InProcessJsExecutor)
            },
        )
    }

    fn with_executor(executor: Arc<dyn WorkflowStepExecutor>) -> Self {
        let env = Self::new(false);
        let cfg = CoreConfig::for_home(&env.mcp.home);
        let core = Core::new(
            steve_core::repo::Db::new(env.store.clone()),
            env.rt.clone(),
            Arc::new(NoModels),
            cfg,
        );
        core.set_workflow_executor(executor);
        let workspaces = Workspaces::new(env.store.clone(), None, None, vec![]);
        let mcp = CoreMcp {
            store: env.store.clone(),
            core: core.clone(),
            home: env.mcp.home,
            public_url: "http://127.0.0.1:18701".into(),
            workspaces,
        };
        Self {
            dir: env.dir,
            store: env.store,
            rt: env.rt,
            core,
            mcp,
        }
    }

    fn attach(dir: tempfile::TempDir, home: PathBuf, store: Store, use_subprocess: bool) -> Self {
        Self::attach_with(
            dir,
            home,
            store,
            if use_subprocess {
                Arc::new(real_step_runner())
            } else {
                Arc::new(InProcessJsExecutor)
            },
        )
    }

    fn attach_with(
        dir: tempfile::TempDir,
        home: PathBuf,
        store: Store,
        executor: Arc<dyn WorkflowStepExecutor>,
    ) -> Self {
        let rt = Arc::new(TestRuntime::new(store.clone()));
        let cfg = CoreConfig::for_home(&home);
        std::fs::create_dir_all(&cfg.scratch).unwrap();
        std::fs::create_dir_all(&cfg.worktrees).unwrap();
        let core = Core::new(
            steve_core::repo::Db::new(store.clone()),
            rt.clone(),
            Arc::new(NoModels),
            cfg,
        );
        core.set_workflow_executor(executor);
        let workspaces = Workspaces::new(store.clone(), None, None, vec![]);
        let mcp = CoreMcp {
            store: store.clone(),
            core: core.clone(),
            home,
            public_url: "http://127.0.0.1:18701".into(),
            workspaces,
        };
        Self {
            dir,
            store,
            rt,
            core,
            mcp,
        }
    }

    fn owner(&self) -> Caller {
        Caller {
            ws: WS.into(),
            me: "owner".into(),
        }
    }

    fn member(&self) -> Caller {
        Caller {
            ws: WS.into(),
            me: "member".into(),
        }
    }

    fn create_parent(&self, project_id: Option<String>) -> (String, Session) {
        let tag = self.store.tags().kinds(WS).unwrap().remove(0).id;
        let item = self
            .store
            .items()
            .create(
                WS,
                &NewItem {
                    title: "Parent workflow task".into(),
                    goal: "Complete workflow".into(),
                    bot_id: "steve".into(),
                    project_id,
                    tags: vec![tag],
                    requested_by: Some("owner".into()),
                    ..Default::default()
                },
            )
            .unwrap()
            .unwrap();
        let sess = self
            .store
            .sessions()
            .create(&NewSession {
                workspace_id: WS.into(),
                item_id: Some(item.id.clone()),
                bot_id: Some("steve".into()),
                harness: "fixture".into(),
                model: None,
                reasoning_effort: None,
                cwd: self.dir.path().to_string_lossy().into(),
                title: "Coordinator".into(),
                token: format!("tok-{}", item.id),
            })
            .unwrap();
        self.store
            .sessions()
            .patch(
                &sess.id,
                &steve_store::SessionPatch {
                    status: Some("idle".into()),
                    ..Default::default()
                },
            )
            .unwrap();
        let sess = self.store.sessions().get(&sess.id).unwrap().unwrap();
        (item.id, sess)
    }

    fn child_session(&self, child_item_id: &str) -> Session {
        self.store
            .sessions()
            .of_item(child_item_id)
            .unwrap()
            .into_iter()
            .last()
            .expect("child item must have a session")
    }

    async fn deliver_child(&self, child_item_id: &str, result: Value) {
        let sess = self.child_session(child_item_id);
        self.mcp
            .deliver(&sess, "done".into(), vec![], vec![], None, Some(result))
            .await
            .expect("child deliver should succeed");
        self.rt.end_turn(&self.core, &sess.id, "end_turn");
    }

    async fn wf_tool(&self, sess: &Session, args: Value) -> steve_core::Result<Value> {
        let out = self.core.tool_workflow_run(sess, args).await?;
        Ok(serde_json::to_value(out).unwrap())
    }

    async fn fail_child(&self, child_item_id: &str, reason: &str) {
        let sess = self.child_session(child_item_id);
        let out = self
            .core
            .tool_workflow_run(&sess, json!({ "action": "fail", "reason": reason }))
            .await
            .expect("child fail should succeed");
        assert!(out.ends_turn(), "child fail output must mark ends_turn");
        self.rt.end_turn(&self.core, &sess.id, "end_turn");
    }

    async fn wait_for_run_status(&self, run_id: &str, target: WfRunStatus) {
        for _ in 0..80 {
            let cur = self
                .store
                .read(|r| WorkflowRuns(r).get_run(run_id))
                .unwrap()
                .unwrap();
            if cur.status == target {
                return;
            }
            tokio::time::sleep(Duration::from_millis(15)).await;
        }
        let cur = self
            .store
            .read(|r| WorkflowRuns(r).get_run(run_id))
            .unwrap()
            .unwrap();
        panic!(
            "timed out waiting for run {run_id} status={target:?}, stuck in {:?} (pause={:?})",
            cur.status, cur.pause_reason_code
        );
    }

    async fn wait_for_dispatched_count(&self, run_id: &str, expected_total_attempts: usize) {
        for _ in 0..100 {
            let attempts = self
                .store
                .read(|r| WorkflowRuns(r).list_attempts_for_run(run_id))
                .unwrap();
            let ready = attempts.len() >= expected_total_attempts
                && attempts.iter().all(|a| {
                    a.status != WfAttemptStatus::Dispatched
                        || a.child_item_id.as_deref().is_some_and(|cid| {
                            !self.store.sessions().of_item(cid).unwrap().is_empty()
                        })
                });
            if ready {
                return;
            }
            tokio::time::sleep(Duration::from_millis(15)).await;
        }
        let n = self
            .store
            .read(|r| WorkflowRuns(r).list_attempts_for_run(run_id))
            .unwrap()
            .len();
        panic!(
            "timed out waiting for {expected_total_attempts} dispatched sessions on {run_id}, got {n}"
        );
    }

    async fn wait_for_item_closed(&self, item_id: &str) -> steve_store::Item {
        for _ in 0..100 {
            let item = self.store.items().get(item_id).unwrap().unwrap();
            if item.phase == ItemPhase::Closed {
                return item;
            }
            tokio::time::sleep(Duration::from_millis(15)).await;
        }
        let item = self.store.items().get(item_id).unwrap().unwrap();
        panic!(
            "timed out waiting for item {item_id} to close, stuck in phase={:?}",
            item.phase
        );
    }
}

fn real_step_runner() -> StepRunner {
    let bin = PathBuf::from(env!("CARGO_BIN_EXE_steve-hub"));
    static WARM: Once = Once::new();
    WARM.call_once(|| {
        let out = Command::new(&bin)
            .arg("--build-info")
            .output()
            .expect("warm steve-hub binary");
        assert!(out.status.success());
    });
    StepRunner::new(bin)
}

fn git_run(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "Steve Test")
        .env("GIT_AUTHOR_EMAIL", "test@steve.local")
        .env("GIT_COMMITTER_NAME", "Steve Test")
        .env("GIT_COMMITTER_EMAIL", "test@steve.local")
        .output()
        .expect("git command");
    assert!(
        out.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_owned()
}

fn init_git_project(env: &Env, name: &str) -> (String, PathBuf) {
    let repo = env.dir.path().join(name);
    std::fs::create_dir_all(&repo).unwrap();
    git_run(&repo, &["init", "-b", "main"]);
    std::fs::write(repo.join("README.md"), "initial\n").unwrap();
    git_run(&repo, &["add", "."]);
    git_run(&repo, &["commit", "-m", "initial commit"]);
    let proj = env
        .store
        .projects()
        .create(
            WS,
            &NewProject {
                name: name.into(),
                path: repo.to_string_lossy().into(),
                kind: "git".into(),
                worktree: true,
                ..Default::default()
            },
        )
        .unwrap();
    (proj.id, repo)
}

/// Scenarios 1 & 17: Sequential multi-step workflow + real `--workflow-step` subprocess +
/// independent review gate unblocking parent delivery.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scenario_01_sequential_with_real_subprocess_and_independent_review() {
    let env = Env::new(true);
    let (parent_id, coord) = env.create_parent(None);

    let script = r#"
        export default async function () {
            const a = await phase("build", () => agent("implement part A", {
                bot: "coder",
                title: "Step A",
                schema: {
                    type: "object",
                    required: ["artifact"],
                    properties: { artifact: { type: "string" } },
                    additionalProperties: false
                }
            }));
            const b = await phase("verify", () => agent("verify " + a.artifact, {
                bot: "coder",
                title: "Step B"
            }));
            const r = await phase("review", () => agent("independent final review", {
                bot: "reviewer",
                title: "Final Review",
                review: true
            }));
            return { a, b, approved: r.approved };
        }
    "#;

    let res = env
        .wf_tool(&coord, json!({ "action": "submit", "script": script }))
        .await
        .unwrap();
    let run_id = res["run"]["id"].as_str().unwrap().to_owned();
    assert_eq!(res["run"]["status"], "running");
    assert_eq!(res["attempts"].as_array().unwrap().len(), 1);

    // Parent delivery MUST be blocked while workflow is running.
    let blocked_deliver = env
        .mcp
        .deliver(&coord, "premature".into(), vec![], vec![], None, None)
        .await;
    assert!(
        blocked_deliver.is_err(),
        "parent cannot deliver while wf_run is active"
    );

    // Step A delivers structured result -> atomically finalizes -> wakes Step B.
    let child_a = res["attempts"][0]["child_item_id"]
        .as_str()
        .unwrap()
        .to_owned();
    env.deliver_child(&child_a, json!({ "artifact": "libfoo.a" }))
        .await;
    env.wait_for_dispatched_count(&run_id, 2).await;

    // Step B delivers -> wakes independent Review step on `reviewer`.
    let attempts = env
        .store
        .read(|r| WorkflowRuns(r).list_attempts_for_run(&run_id))
        .unwrap();
    let child_b = attempts[1].child_item_id.clone().unwrap();
    env.deliver_child(&child_b, json!({ "tests": "pass" }))
        .await;
    env.wait_for_dispatched_count(&run_id, 3).await;

    // Review delivers `{ approved: true }` on independent bot -> run completes & verifies review.
    let attempts = env
        .store
        .read(|r| WorkflowRuns(r).list_attempts_for_run(&run_id))
        .unwrap();
    let child_r = attempts[2].child_item_id.clone().unwrap();
    assert_eq!(attempts[2].bot_id.as_deref(), Some("reviewer"));
    env.deliver_child(&child_r, json!({ "approved": true, "notes": "lgtm" }))
        .await;
    env.wait_for_run_status(&run_id, WfRunStatus::Completed)
        .await;

    let run = env
        .store
        .read(|r| WorkflowRuns(r).get_run(&run_id))
        .unwrap()
        .unwrap();
    assert!(run.review_verified_call_id.is_some());

    // Now parent delivery succeeds cleanly!
    env.mcp
        .deliver(&coord, "all done".into(), vec![], vec![], None, None)
        .await
        .expect("parent delivery should succeed after verified independent review");
    let parent = env.store.items().get(&parent_id).unwrap().unwrap();
    assert_eq!(parent.phase, ItemPhase::Closed);
}

/// Scenario 2: Parallel fan-out via `all()`, plus `mutex_key` serialization and parallel cap queueing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scenario_02_parallel_all_with_mutex_and_parallel_cap() {
    let env = Env::new(false);
    let (_parent_id, coord) = env.create_parent(None);

    let script = r#"
        export default async function () {
            const [x, y, z] = await all([
                () => agent("left", { bot: "coder", mutex_key: "staging" }),
                () => agent("right", { bot: "coder" }),
                () => agent("mutex-second", { bot: "coder", mutex_key: "staging" }),
            ]);
            const r = await agent("review", { bot: "reviewer", review: true });
            return { x, y, z, ok: r.approved };
        }
    "#;

    let snap = env
        .wf_tool(&coord, json!({ "action": "submit", "script": script }))
        .await
        .unwrap();
    let run_id = snap["run"]["id"].as_str().unwrap().to_owned();

    // `left` (mutex=staging) and `right` dispatch concurrently; `mutex-second` queues until `left` settles!
    let attempts = env
        .store
        .read(|r| WorkflowRuns(r).list_attempts_for_run(&run_id))
        .unwrap();
    assert_eq!(
        attempts.len(),
        2,
        "mutex_key must serialize third call while first holds lock"
    );
    let child_left = attempts[0].child_item_id.clone().unwrap();
    let child_right = attempts[1].child_item_id.clone().unwrap();

    // Finish `left` -> frees mutex -> `mutex-second` dispatches!
    env.deliver_child(&child_left, json!({ "n": 1 })).await;
    env.wait_for_dispatched_count(&run_id, 3).await;

    let attempts = env
        .store
        .read(|r| WorkflowRuns(r).list_attempts_for_run(&run_id))
        .unwrap();
    let child_third = attempts[2].child_item_id.clone().unwrap();
    env.deliver_child(&child_right, json!({ "n": 2 })).await;
    env.deliver_child(&child_third, json!({ "n": 3 })).await;
    env.wait_for_dispatched_count(&run_id, 4).await;

    let attempts = env
        .store
        .read(|r| WorkflowRuns(r).list_attempts_for_run(&run_id))
        .unwrap();
    env.deliver_child(
        attempts[3].child_item_id.as_deref().unwrap(),
        json!({ "approved": true }),
    )
    .await;
    env.wait_for_run_status(&run_id, WfRunStatus::Completed)
        .await;

    let run = env
        .store
        .read(|r| WorkflowRuns(r).get_run(&run_id))
        .unwrap()
        .unwrap();
    let result: Value = serde_json::from_str(run.result_json.as_deref().unwrap()).unwrap();
    assert_eq!(result["x"]["n"], 1);
    assert_eq!(result["y"]["n"], 2);
    assert_eq!(result["z"]["n"], 3);
}

/// Scenario 3: `race()` cancels losing branch calls and their active child items.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scenario_03_race_cancels_losing_branch() {
    let env = Env::new(false);
    let (_parent_id, coord) = env.create_parent(None);

    let script = r#"
        export default async function () {
            const win = await race([
                () => agent("fast branch", { bot: "coder", title: "Fast" }),
                () => agent("slow branch", { bot: "coder", title: "Slow" }),
            ]);
            const r = await agent("review", { bot: "reviewer", review: true });
            return { win, ok: r.approved };
        }
    "#;
    let snap = env
        .wf_tool(&coord, json!({ "action": "submit", "script": script }))
        .await
        .unwrap();
    let run_id = snap["run"]["id"].as_str().unwrap().to_owned();
    let attempts = env
        .store
        .read(|r| WorkflowRuns(r).list_attempts_for_run(&run_id))
        .unwrap();
    assert_eq!(attempts.len(), 2);
    let fast_child = attempts[0].child_item_id.clone().unwrap();
    let slow_child = attempts[1].child_item_id.clone().unwrap();
    let slow_call_id = attempts[1].call_id.clone();

    // Fast finishes first -> race resolves -> slow call & slow child cancelled!
    env.deliver_child(&fast_child, json!({ "winner": "fast" }))
        .await;
    env.wait_for_dispatched_count(&run_id, 3).await;

    let slow_call = env
        .store
        .read(|r| WorkflowRuns(r).get_call(&slow_call_id))
        .unwrap()
        .unwrap();
    assert_eq!(slow_call.status, WfCallStatus::Cancelled);
    let slow_item = env.wait_for_item_closed(&slow_child).await;
    assert_eq!(slow_item.phase, ItemPhase::Closed);
    assert_eq!(slow_item.outcome.as_deref(), Some("cancelled"));

    let attempts = env
        .store
        .read(|r| WorkflowRuns(r).list_attempts_for_run(&run_id))
        .unwrap();
    env.deliver_child(
        attempts[2].child_item_id.as_deref().unwrap(),
        json!({ "approved": true }),
    )
    .await;
    env.wait_for_run_status(&run_id, WfRunStatus::Completed)
        .await;
}

/// Scenario 4: Pause/resume, child explicit `action=fail` caught in JS `try/catch`, and waiting on
/// human (`steve_ask`) preserving retry budget.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scenario_04_resume_fail_catch_and_waiting_human_preserves_budget() {
    let env = Env::new(false);
    let (_parent_id, coord) = env.create_parent(None);

    let script = r#"
        export default async function () {
            let outcome;
            try {
                outcome = await agent("primary attempt", { bot: "coder" });
            } catch (e) {
                outcome = await agent("fallback after " + e.code, { bot: "coder" });
            }
            const r = await agent("review", { bot: "reviewer", review: true });
            return { outcome, ok: r.approved };
        }
    "#;
    let snap = env
        .wf_tool(&coord, json!({ "action": "submit", "script": script }))
        .await
        .unwrap();
    let run_id = snap["run"]["id"].as_str().unwrap().to_owned();

    // Pause and resume via steve_workflow_run.
    let paused = env
        .wf_tool(&coord, json!({ "action": "pause" }))
        .await
        .unwrap();
    assert_eq!(paused["run"]["status"], "paused");
    let resumed = env
        .wf_tool(&coord, json!({ "action": "resume" }))
        .await
        .unwrap();
    assert_eq!(resumed["run"]["status"], "running");

    // Child 1 asks the owner (`steve_ask`) and ends its turn: MUST remain Dispatched (no failure, no retry consumed!).
    let attempts = env
        .store
        .read(|r| WorkflowRuns(r).list_attempts_for_run(&run_id))
        .unwrap();
    let child1 = attempts[0].child_item_id.clone().unwrap();
    let child1_sess = env.child_session(&child1);
    env.core
        .tool_ask(
            &child1_sess,
            "Which API version?",
            &["v1".into(), "v2".into()],
        )
        .unwrap();
    env.rt.end_turn(&env.core, &child1_sess.id, "end_turn");
    let a1 = env
        .store
        .read(|r| WorkflowRuns(r).attempt_by_child_item(&child1))
        .unwrap()
        .unwrap();
    assert_eq!(a1.status, WfAttemptStatus::Dispatched);
    assert_eq!(
        env.store
            .read(|r| WorkflowRuns(r).count_call_attempts(&a1.call_id))
            .unwrap(),
        1
    );

    // Child 1 explicitly reports failure via `steve_workflow_run { action: "fail" }` -> caught by JS catch -> fallback runs!
    env.wf_tool(
        &child1_sess,
        json!({ "action": "fail", "reason": "upstream unavailable" }),
    )
    .await
    .unwrap();
    env.wait_for_dispatched_count(&run_id, 2).await;

    let attempts = env
        .store
        .read(|r| WorkflowRuns(r).list_attempts_for_run(&run_id))
        .unwrap();
    let child2 = attempts[1].child_item_id.clone().unwrap();
    env.deliver_child(&child2, json!({ "recovered": true }))
        .await;
    env.wait_for_dispatched_count(&run_id, 3).await;

    let attempts = env
        .store
        .read(|r| WorkflowRuns(r).list_attempts_for_run(&run_id))
        .unwrap();
    env.deliver_child(
        attempts[2].child_item_id.as_deref().unwrap(),
        json!({ "approved": true }),
    )
    .await;
    env.wait_for_run_status(&run_id, WfRunStatus::Completed)
        .await;
}

/// Scenario 5 + #503 Git tree/commit fingerprint verification:
/// - Cross-version lazy hydration reuses completed call A without re-running AND cancels orphaned running call B.
/// - For Git-backed projects:
///   (a) Matching `(HEAD, tree)` verifies & lazily hydrates (0 re-runs);
///   (b) Mutated worktree (`tree` or `HEAD` changed) rejects hydration, invalidates & re-runs while keeping cycle budget count;
///   (c) Tidied worktree with matching `HEAD^{tree}` in repository still verifies & lazily hydrates;
///   (d) Git-backed call without recorded `(HEAD, tree)` refuses unverified hydration.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scenario_05_cross_version_lazy_hydration_and_git_fingerprint_verification() {
    let env = Env::new(false);
    let (proj_id, repo_path) = init_git_project(&env, "wf3-git-repo");
    let (_parent_id, coord) = env.create_parent(Some(proj_id.clone()));

    let script_v1 = r#"
        export default async function () {
            const a = await agent("step A", { bot: "coder", title: "A" });
            const b = await agent("step B", { bot: "coder", title: "B" });
            return { a, b };
        }
    "#;
    let snap1 = env
        .wf_tool(&coord, json!({ "action": "submit", "script": script_v1 }))
        .await
        .unwrap();
    let run_id = snap1["run"]["id"].as_str().unwrap().to_owned();
    let child_a = snap1["attempts"][0]["child_item_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let call_a_id = snap1["attempts"][0]["call_id"].as_str().unwrap().to_owned();

    // Child A makes a real Git commit in its worktree and delivers.
    let child_a_item = env.store.items().get(&child_a).unwrap().unwrap();
    let child_a_cwd = PathBuf::from(child_a_item.cwd.as_deref().expect("git child cwd"));
    std::fs::write(child_a_cwd.join("feature.rs"), "pub fn a() -> i32 { 42 }\n").unwrap();
    git_run(&child_a_cwd, &["add", "feature.rs"]);
    git_run(&child_a_cwd, &["commit", "-m", "implement A"]);
    let expected_head = git_run(&child_a_cwd, &["rev-parse", "HEAD"]);
    let expected_tree = git_run(&child_a_cwd, &["rev-parse", "HEAD^{tree}"]);

    env.deliver_child(&child_a, json!({ "value": 42 })).await;
    env.wait_for_dispatched_count(&run_id, 2).await;

    // Verify (HEAD, tree, cwd) were captured atomically onto wf_calls!
    let call_a = env
        .store
        .read(|r| WorkflowRuns(r).get_call(&call_a_id))
        .unwrap()
        .unwrap();
    assert_eq!(call_a.git_head.as_deref(), Some(expected_head.as_str()));
    assert_eq!(call_a.git_tree.as_deref(), Some(expected_tree.as_str()));
    assert!(call_a.git_cwd.is_some());

    // Step B is currently running (Dispatched). Revise to v2: keep A (unchanged git tree), replace B with C!
    let attempts_before_v2 = env
        .store
        .read(|r| WorkflowRuns(r).list_attempts_for_run(&run_id))
        .unwrap();
    let child_b = attempts_before_v2[1].child_item_id.clone().unwrap();
    let call_b_id = attempts_before_v2[1].call_id.clone();

    let script_v2 = r#"
        export default async function () {
            const a = await agent("step A", { bot: "coder", title: "A" });
            const c = await agent("step C using " + a.value, { bot: "coder", title: "C" });
            return { a, c };
        }
    "#;
    let snap2 = env
        .wf_tool(&coord, json!({ "action": "submit", "script": script_v2 }))
        .await
        .unwrap();
    assert_eq!(snap2["run"]["active_script_version"], 2);

    // (a) Verified Git fingerprint -> A was lazily hydrated from v1 (still 1 attempt!),
    // orphan B was cancelled, and C was dispatched!
    assert_eq!(
        env.store
            .read(|r| WorkflowRuns(r).count_call_attempts(&call_a_id))
            .unwrap(),
        1,
        "verified Git output must hydrate without re-running step A"
    );
    let vc_v2 = env
        .store
        .read(|r| WorkflowRuns(r).list_version_calls(&run_id, 2))
        .unwrap();
    assert_eq!(vc_v2[0].call_id, call_a_id);
    assert_eq!(vc_v2[0].hydrated_from_version, Some(1));
    assert_eq!(
        env.store
            .read(|r| WorkflowRuns(r).get_call(&call_b_id))
            .unwrap()
            .unwrap()
            .status,
        WfCallStatus::Cancelled
    );
    let cancelled_b = env.wait_for_item_closed(&child_b).await;
    assert_eq!(cancelled_b.outcome.as_deref(), Some("cancelled"));

    // (b) Now simulate worktree tampering (dirty edit changing git tree!) and revise to v3:
    // pre-hydration verification MUST reject A, invalidate it, and re-run A (attempt 2)!
    std::fs::write(
        child_a_cwd.join("feature.rs"),
        "pub fn a() -> i32 { 999 }\n",
    )
    .unwrap();
    let script_v3 = r#"
        export default async function () {
            const a = await agent("step A", { bot: "coder", title: "A" });
            const d = await agent("step D", { bot: "coder", title: "D" });
            return { a, d };
        }
    "#;
    env.wf_tool(&coord, json!({ "action": "submit", "script": script_v3 }))
        .await
        .unwrap();
    assert_eq!(
        env.store
            .read(|r| WorkflowRuns(r).count_call_attempts(&call_a_id))
            .unwrap(),
        2,
        "mutated Git worktree MUST reject hydration and re-run step A as attempt 2"
    );
    let a_attempts = env
        .store
        .read(|r| WorkflowRuns(r).list_attempts_for_call(&call_a_id))
        .unwrap();
    assert_eq!(a_attempts[0].status, WfAttemptStatus::Cancelled);
    assert_eq!(
        a_attempts[0].error_code.as_deref(),
        Some("git_fingerprint_mismatch")
    );
    assert_eq!(a_attempts[1].status, WfAttemptStatus::Dispatched);

    // (c) Attempt 2 commits cleanly, delivers, THEN its worktree is tidied (removed from disk),
    // while the commit object still lives in repo_path: v4 MUST verify HEAD^{tree} and hydrate!
    let child_a2 = a_attempts[1].child_item_id.clone().unwrap();
    let child_a2_cwd = PathBuf::from(
        env.store
            .items()
            .get(&child_a2)
            .unwrap()
            .unwrap()
            .cwd
            .unwrap(),
    );
    std::fs::write(
        child_a2_cwd.join("feature.rs"),
        "pub fn a() -> i32 { 77 }\n",
    )
    .unwrap();
    git_run(&child_a2_cwd, &["add", "feature.rs"]);
    git_run(&child_a2_cwd, &["commit", "-m", "implement A attempt 2"]);
    env.deliver_child(&child_a2, json!({ "value": 77 })).await;
    // Wait for step D to dispatch, then remove child_a2_cwd worktree directory!
    env.wait_for_dispatched_count(&run_id, 5).await;
    let _ = git_run(
        &repo_path,
        &[
            "worktree",
            "remove",
            "--force",
            &child_a2_cwd.to_string_lossy(),
        ],
    );
    assert!(!child_a2_cwd.exists());

    let script_v4 = r#"
        export default async function () {
            const a = await agent("step A", { bot: "coder", title: "A" });
            const e = await agent("step E", { bot: "coder", title: "E" });
            return { a, e };
        }
    "#;
    env.wf_tool(&coord, json!({ "action": "submit", "script": script_v4 }))
        .await
        .unwrap();
    assert_eq!(
        env.store
            .read(|r| WorkflowRuns(r).count_call_attempts(&call_a_id))
            .unwrap(),
        2,
        "tidied worktree with intact commit HEAD^{{tree}} in repo MUST still verify & hydrate"
    );
    let vc_v4 = env
        .store
        .read(|r| WorkflowRuns(r).list_version_calls(&run_id, 4))
        .unwrap();
    assert_eq!(vc_v4[0].call_id, call_a_id);
    assert_eq!(vc_v4[0].hydrated_from_version, Some(1));

    // (d) Git-backed call whose recorded fingerprint is missing MUST NOT hydrate unverified output!
    let child_e = env
        .store
        .read(|r| WorkflowRuns(r).list_attempts_for_run(&run_id))
        .unwrap()
        .into_iter()
        .last()
        .unwrap();
    let call_e_id = child_e.call_id.clone();
    let child_e_id = child_e.child_item_id.unwrap();
    // Simulate a legacy/unverified delivery on a Git project without any recorded (HEAD, tree).
    env.store
        .write(|w| {
            WorkflowRuns(w).stage_attempt_closing(
                &child_e_id,
                1,
                r#"{"unverified":true}"#,
                "[]",
            )?;
            WorkflowRuns(w).finalize_attempt(
                &child_e_id,
                1,
                WfChildOutcome::Completed,
                None,
                None,
            )?;
            Ok::<_, StoreError>(())
        })
        .unwrap();
    let script_v5 = r#"
        export default async function () {
            const a = await agent("step A", { bot: "coder", title: "A" });
            const e = await agent("step E", { bot: "coder", title: "E" });
            return { a, e };
        }
    "#;
    env.wf_tool(&coord, json!({ "action": "submit", "script": script_v5 }))
        .await
        .unwrap();
    let e_attempts = env
        .store
        .read(|r| WorkflowRuns(r).list_attempts_for_call(&call_e_id))
        .unwrap();
    assert_eq!(
        e_attempts.len(),
        2,
        "Git-backed call without recorded (HEAD, tree) MUST refuse unverified hydration and re-run"
    );
    assert_eq!(
        e_attempts[0].error_code.as_deref(),
        Some("missing_git_fingerprint")
    );
}

/// Scenario 6: Same-version non-deterministic replay pauses BEFORE dispatching any call and opens
/// a W3.3 `WorkflowNonDeterministicReplay` decision card.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scenario_06_non_deterministic_replay_pauses_and_opens_decision() {
    let env = Env::new(false);
    let (parent_id, coord) = env.create_parent(None);

    // Script illegally depends on `state.runRevision` in its first call prompt!
    let script = r#"
        export default async function () {
            const a = await agent("step rev=" + state.runRevision, { bot: "coder" });
            return a;
        }
    "#;
    let snap = env
        .wf_tool(&coord, json!({ "action": "submit", "script": script }))
        .await
        .unwrap();
    let run_id = snap["run"]["id"].as_str().unwrap().to_owned();
    let child1 = snap["attempts"][0]["child_item_id"]
        .as_str()
        .unwrap()
        .to_owned();

    // Deliver child1 -> bumps run_revision -> next replay emits a DIFFERENT seq 0 identity!
    env.deliver_child(&child1, json!({ "ok": true })).await;
    env.wait_for_run_status(&run_id, WfRunStatus::Paused).await;

    let run = env
        .store
        .read(|r| WorkflowRuns(r).get_run(&run_id))
        .unwrap()
        .unwrap();
    assert_eq!(
        run.pause_reason_code,
        Some(WfPauseReason::NonDeterministicReplay)
    );
    // Zero second attempt was dispatched!
    assert_eq!(
        env.store
            .read(|r| WorkflowRuns(r).list_attempts_for_run(&run_id))
            .unwrap()
            .len(),
        1
    );
    let dec = env
        .store
        .decisions()
        .of_item(&parent_id)
        .unwrap()
        .into_iter()
        .find(|d| d.subject_type == WF_RUN_SUBJECT && d.status == DecisionStatus::Open)
        .expect("non-deterministic replay decision card");
    assert_eq!(
        dec.question_template,
        Some(DecisionPrompt::WorkflowNonDeterministicReplay)
    );
}

/// Scenario 7: Pre-file-receive Schema validation rejects missing, schema-violating, and >32 KiB
/// `steve_deliver.result` BEFORE receiving any files (zero orphan files, zero artifacts, zero state change).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scenario_07_deliver_schema_rejection_leaves_zero_orphan_files() {
    let env = Env::new(false);
    let (_parent_id, coord) = env.create_parent(None);

    let script = r#"
        export default async function () {
            const a = await agent("produce report", {
                bot: "coder",
                schema: {
                    type: "object",
                    required: ["score"],
                    properties: { score: { type: "integer", minimum: 0, maximum: 100 } },
                    additionalProperties: false
                }
            });
            return a;
        }
    "#;
    let snap = env
        .wf_tool(&coord, json!({ "action": "submit", "script": script }))
        .await
        .unwrap();
    let child_id = snap["attempts"][0]["child_item_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let child_item = env.store.items().get(&child_id).unwrap().unwrap();
    let child_dir = PathBuf::from(child_item.cwd.as_deref().unwrap());
    let file_path = child_dir.join("orphan-check.txt");
    std::fs::write(&file_path, "must not become an orphan delivery").unwrap();
    let child_sess = env.child_session(&child_id);

    let file_output = || {
        vec![steve_mcp::Output {
            kind: "file".into(),
            title: "Report".into(),
            location: Some(file_path.to_string_lossy().into()),
            role: Some("result".into()),
        }]
    };

    // 1. Missing result -> rejected BEFORE file receive (`missing_result`, never reaches svc.receive).
    let err1 = env
        .mcp
        .deliver(
            &child_sess,
            "missing".into(),
            file_output(),
            vec![],
            None,
            None,
        )
        .await
        .unwrap_err();
    assert!(err1.to_string().contains("missing_result"), "got {err1}");

    // 2. Schema violation (score out of range + extra key) -> rejected BEFORE file receive.
    let err2 = env
        .mcp
        .deliver(
            &child_sess,
            "bad".into(),
            file_output(),
            vec![],
            None,
            Some(json!({ "score": 150, "extra": true })),
        )
        .await
        .unwrap_err();
    assert!(err2.to_string().contains("schema_violation"), "got {err2}");

    // 3. Oversized > 32 KiB -> rejected BEFORE file receive.
    let err3 = env
        .mcp
        .deliver(
            &child_sess,
            "huge".into(),
            file_output(),
            vec![],
            None,
            Some(json!({ "score": 10, "blob": "x".repeat(33 * 1024) })),
        )
        .await
        .unwrap_err();
    assert!(err3.to_string().contains("too_large"), "got {err3}");

    // Zero orphan files in Hub delivery store or on disk, zero artifacts, child still active!
    let deliveries = env
        .store
        .read(|r| steve_store::delivery_repo::owned(r, WS, "owner"))
        .unwrap();
    assert!(
        deliveries.is_empty(),
        "rejected schema deliveries must leave 0 orphan DB rows, found {}",
        deliveries.len()
    );
    let deliveries_dir = env.mcp.home.join("deliveries");
    assert!(
        !deliveries_dir.exists() || std::fs::read_dir(&deliveries_dir).unwrap().next().is_none(),
        "rejected schema deliveries must leave 0 orphan files on disk"
    );
    assert!(env.store.artifacts().list(&child_id).unwrap().is_empty());
    assert_eq!(
        env.store.items().get(&child_id).unwrap().unwrap().phase,
        ItemPhase::Active
    );

    // Valid result now passes pre-validation and delivers cleanly!
    env.mcp
        .deliver(
            &child_sess,
            "ok".into(),
            vec![],
            vec![],
            None,
            Some(json!({ "score": 98 })),
        )
        .await
        .expect("valid result should succeed");
    env.rt.end_turn(&env.core, &child_sess.id, "end_turn");
    assert_eq!(
        env.store.items().get(&child_id).unwrap().unwrap().phase,
        ItemPhase::Closed
    );
}

/// Scenario 8: Hub independent review gate rejects same-Bot review (`SameBotAsWorker`) even when
/// review returns `{ approved: true }` and blocks parent delivery.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scenario_08_same_bot_review_rejected_by_hub() {
    let env = Env::new(false);
    let (parent_id, coord) = env.create_parent(None);

    let script = r#"
        export default async function () {
            await agent("implement", { bot: "coder" });
            const r = await agent("self review", { bot: "coder", review: true });
            return r;
        }
    "#;
    let snap = env
        .wf_tool(&coord, json!({ "action": "submit", "script": script }))
        .await
        .unwrap();
    let run_id = snap["run"]["id"].as_str().unwrap().to_owned();
    env.deliver_child(
        snap["attempts"][0]["child_item_id"].as_str().unwrap(),
        json!({ "done": true }),
    )
    .await;
    env.wait_for_dispatched_count(&run_id, 2).await;

    let attempts = env
        .store
        .read(|r| WorkflowRuns(r).list_attempts_for_run(&run_id))
        .unwrap();
    env.deliver_child(
        attempts[1].child_item_id.as_deref().unwrap(),
        json!({ "approved": true }),
    )
    .await;
    env.wait_for_run_status(&run_id, WfRunStatus::Paused).await;

    let run = env
        .store
        .read(|r| WorkflowRuns(r).get_run(&run_id))
        .unwrap()
        .unwrap();
    assert_eq!(
        run.pause_reason_code,
        Some(WfPauseReason::ReviewGateBlocked)
    );
    assert_eq!(
        env.store
            .read(|r| WorkflowRuns(r).verify_independent_review(&run_id))
            .unwrap(),
        ReviewGateVerdict::Blocked {
            reason: ReviewGateFailure::SameBotAsWorker {
                bot_id: "coder".into()
            }
        }
    );
    // Parent delivery stays blocked!
    assert!(
        env.mcp
            .deliver(&coord, "try close".into(), vec![], vec![], None, None)
            .await
            .is_err()
    );
    let dec = env
        .store
        .decisions()
        .of_item(&parent_id)
        .unwrap()
        .into_iter()
        .find(|d| d.subject_type == WF_RUN_SUBJECT && d.status == DecisionStatus::Open)
        .unwrap();
    assert_eq!(
        dec.question_template,
        Some(DecisionPrompt::WorkflowReviewBlocked {
            reason: "same_bot_as_worker".into()
        })
    );
}

/// Scenario 9: Hub independent review gate rejects stale review (work settled AFTER review was
/// dispatched), then passes once a fresh independent review runs after the latest work.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scenario_09_stale_review_rejected_then_fresh_review_passes() {
    let env = Env::new(false);
    let (_parent_id, coord) = env.create_parent(None);

    let script_v1 = r#"
        export default async function () {
            await agent("work 1", { bot: "coder" });
            const r = await agent("early review", { bot: "reviewer", review: true });
            await agent("work 2 after review", { bot: "coder" });
            return r;
        }
    "#;
    let snap = env
        .wf_tool(&coord, json!({ "action": "submit", "script": script_v1 }))
        .await
        .unwrap();
    let run_id = snap["run"]["id"].as_str().unwrap().to_owned();

    // Work 1 settles
    env.deliver_child(
        snap["attempts"][0]["child_item_id"].as_str().unwrap(),
        json!({ "w": 1 }),
    )
    .await;
    env.wait_for_dispatched_count(&run_id, 2).await;

    // Early independent review settles (approved: true)
    let attempts = env
        .store
        .read(|r| WorkflowRuns(r).list_attempts_for_run(&run_id))
        .unwrap();
    env.deliver_child(
        attempts[1].child_item_id.as_deref().unwrap(),
        json!({ "approved": true }),
    )
    .await;
    env.wait_for_dispatched_count(&run_id, 3).await;

    // Work 2 settles AFTER review -> review is now stale -> Hub blocks completion!
    let attempts = env
        .store
        .read(|r| WorkflowRuns(r).list_attempts_for_run(&run_id))
        .unwrap();
    env.deliver_child(
        attempts[2].child_item_id.as_deref().unwrap(),
        json!({ "w": 2 }),
    )
    .await;
    env.wait_for_run_status(&run_id, WfRunStatus::Paused).await;

    assert!(matches!(
        env.store
            .read(|r| WorkflowRuns(r).verify_independent_review(&run_id))
            .unwrap(),
        ReviewGateVerdict::Blocked {
            reason: ReviewGateFailure::StaleReview { .. }
        }
    ));

    // Revise to v2 adding a fresh review after work 2 -> hydrates work 1, early review, work 2, dispatches fresh review!
    let script_v2 = r#"
        export default async function () {
            await agent("work 1", { bot: "coder" });
            await agent("early review", { bot: "reviewer", review: true });
            await agent("work 2 after review", { bot: "coder" });
            const r2 = await agent("fresh review", { bot: "reviewer", review: true });
            return r2;
        }
    "#;
    env.wf_tool(&coord, json!({ "action": "submit", "script": script_v2 }))
        .await
        .unwrap();
    env.wait_for_dispatched_count(&run_id, 4).await;

    let attempts = env
        .store
        .read(|r| WorkflowRuns(r).list_attempts_for_run(&run_id))
        .unwrap();
    env.deliver_child(
        attempts[3].child_item_id.as_deref().unwrap(),
        json!({ "approved": true }),
    )
    .await;
    env.wait_for_run_status(&run_id, WfRunStatus::Completed)
        .await;
}

/// Scenario 10: Cycle budget persists across script revisions, pauses & opens W3.3 owner-only
/// decision card on exhaustion, and resumes when owner approves `add_budget`.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scenario_10_cycle_budget_exhaustion_and_owner_top_up() {
    let env = Env::new(false);
    // Tight workspace cycle budget: max_total_attempts = 2
    env.store
        .write(|w| {
            WorkspaceSettings(w).set(
                WS,
                budget_settings::MAX_TOTAL_ATTEMPTS,
                "2",
                steve_store::now(),
            )
        })
        .unwrap();
    let (parent_id, coord) = env.create_parent(None);

    let script_v1 = r#"
        export default async function () {
            await agent("step 1", { bot: "coder" });
            await agent("step 2", { bot: "coder" });
            return { ok: true };
        }
    "#;
    let snap = env
        .wf_tool(&coord, json!({ "action": "submit", "script": script_v1 }))
        .await
        .unwrap();
    let run_id = snap["run"]["id"].as_str().unwrap().to_owned();
    env.deliver_child(
        snap["attempts"][0]["child_item_id"].as_str().unwrap(),
        json!({ "n": 1 }),
    )
    .await;
    env.wait_for_dispatched_count(&run_id, 2).await;
    let attempts = env
        .store
        .read(|r| WorkflowRuns(r).list_attempts_for_run(&run_id))
        .unwrap();
    env.deliver_child(
        attempts[1].child_item_id.as_deref().unwrap(),
        json!({ "n": 2 }),
    )
    .await;
    env.wait_for_run_status(&run_id, WfRunStatus::Paused).await; // paused for review_gate_blocked

    // Revise to v2 to add review (attempt #3 -> hits cycle budget 2/2; script revision MUST NOT reset budget!).
    let script_v2 = r#"
        export default async function () {
            await agent("step 1", { bot: "coder" });
            await agent("step 2", { bot: "coder" });
            return await agent("review", { bot: "reviewer", review: true });
        }
    "#;
    let snap2 = env
        .wf_tool(&coord, json!({ "action": "submit", "script": script_v2 }))
        .await
        .unwrap();
    assert_eq!(snap2["run"]["status"], "paused");
    assert_eq!(
        snap2["run"]["pause_reason_code"],
        WfPauseReason::BudgetExhausted.as_str()
    );
    let dec_id = snap2["decision_id"].as_str().unwrap().to_owned();
    let dec = env.store.decisions().get(&dec_id).unwrap().unwrap();
    assert_eq!(
        dec.question_template,
        Some(DecisionPrompt::WorkflowBudgetExhausted {
            scope: WfBudgetScope::CycleTotal,
            used: 2,
            limit: 2,
        })
    );

    // Non-owner cannot approve budget top-up (403).
    assert!(
        env.core
            .decision_command(
                &env.member(),
                &dec_id,
                Some(dec.revision),
                DecisionCommand::Resolve {
                    choice: "add_budget".into(),
                    text: "2".into(),
                },
            )
            .is_err()
    );

    // Owner approves add_budget +2 -> run resumes & dispatches review attempt #3!
    env.core
        .decision_command(
            &env.owner(),
            &dec_id,
            Some(dec.revision),
            DecisionCommand::Resolve {
                choice: "add_budget".into(),
                text: "2".into(),
            },
        )
        .unwrap();
    env.wait_for_dispatched_count(&run_id, 3).await;

    let attempts = env
        .store
        .read(|r| WorkflowRuns(r).list_attempts_for_run(&run_id))
        .unwrap();
    env.deliver_child(
        attempts[2].child_item_id.as_deref().unwrap(),
        json!({ "approved": true }),
    )
    .await;
    env.wait_for_run_status(&run_id, WfRunStatus::Completed)
        .await;
    assert_eq!(
        env.store
            .read(|r| WorkflowRuns(r).count_cycle_attempts(&parent_id, 1))
            .unwrap(),
        3
    );
}

/// Scenario 11 & 12:
/// - Child item rejects unstructured `finish_by_owner`, accepts structured owner takeover `wf_child_owner_deliver`.
/// - Parent `finish_by_owner` cancels active `wf_run` and automatically retires all active children.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scenarios_11_and_12_bidirectional_owner_finish_contract() {
    let env = Env::new(false);
    let (parent_id, coord) = env.create_parent(None);

    let script = r#"
        export default async function () {
            const [a, b] = await all([
                () => agent("child 1", {
                    bot: "coder",
                    schema: { type: "object", required: ["ok"], properties: { ok: { type: "boolean" } } }
                }),
                () => agent("child 2", { bot: "coder" }),
            ]);
            return { a, b };
        }
    "#;
    let snap = env
        .wf_tool(&coord, json!({ "action": "submit", "script": script }))
        .await
        .unwrap();
    let run_id = snap["run"]["id"].as_str().unwrap().to_owned();
    let child1_id = snap["attempts"][0]["child_item_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let child2_id = snap["attempts"][1]["child_item_id"]
        .as_str()
        .unwrap()
        .to_owned();

    // Scenario 12a: Unstructured `finish_by_owner` on a workflow child item MUST be rejected!
    let c1 = env.store.items().get(&child1_id).unwrap().unwrap();
    let err = env
        .core
        .lifecycle_command(
            &child1_id,
            "finish",
            c1.cycle,
            c1.cas_revision,
            None,
            None,
            None,
            "owner",
        )
        .unwrap_err();
    assert!(
        err.to_string()
            .contains("wf_child_structured_delivery_required"),
        "expected structured delivery refusal, got {err}"
    );
    assert_eq!(
        env.store.items().get(&child1_id).unwrap().unwrap().phase,
        ItemPhase::Active
    );

    // Scenario 12b: Structured owner takeover via `lifecycle_command("finish", result)` validates
    // schema and settles child1 atomically!
    assert!(
        env.core
            .lifecycle_command(
                &child1_id,
                "finish",
                c1.cycle,
                c1.cas_revision,
                Some("bad"),
                None,
                Some(&json!({ "wrong": 1 })),
                "owner",
            )
            .is_err()
    );
    let c1_sess = env.child_session(&child1_id);
    env.core
        .lifecycle_command(
            &child1_id,
            "finish",
            c1.cycle,
            c1.cas_revision,
            Some("owner takeover"),
            None,
            Some(&json!({ "ok": true })),
            "owner",
        )
        .unwrap();
    env.rt.end_turn(&env.core, &c1_sess.id, "end_turn");
    let closed_c1 = env.store.items().get(&child1_id).unwrap().unwrap();
    assert_eq!(closed_c1.phase, ItemPhase::Closed);
    let a1 = env
        .store
        .read(|r| WorkflowRuns(r).attempt_by_child_item(&child1_id))
        .unwrap()
        .unwrap();
    assert_eq!(a1.status, WfAttemptStatus::Succeeded);

    // Scenario 11: Parent `finish_by_owner` cancels active wf_run AND retires still-running child2!
    let parent = env.store.items().get(&parent_id).unwrap().unwrap();
    let closed_parent = env
        .core
        .lifecycle_command(
            &parent_id,
            "finish",
            parent.cycle,
            parent.cas_revision,
            None,
            None,
            None,
            "owner",
        )
        .unwrap();
    assert_eq!(closed_parent.phase, ItemPhase::Closed);

    let run = env
        .store
        .read(|r| WorkflowRuns(r).get_run(&run_id))
        .unwrap()
        .unwrap();
    assert_eq!(run.status, WfRunStatus::Cancelled);

    let c2 = env.wait_for_item_closed(&child2_id).await;
    assert_eq!(c2.phase, ItemPhase::Closed);
    assert_eq!(c2.outcome.as_deref(), Some("cancelled"));
}

/// Scenario 13: Crash & restart continuation with ZERO duplicate dispatch (both mid-call and
/// immediately post-commit before async step wakeup).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn scenario_13_crash_restart_zero_duplicate_dispatch() {
    let dir = tempfile::tempdir().unwrap();
    let home = dir.path().join("steve");
    std::fs::create_dir_all(&home).unwrap();
    let db_path = home.join("steve.sqlite3");

    let (run_id, child1_id, child1_sess_id) = {
        let store = Store::open(&db_path, NoPublisher).unwrap();
        store.ensure_workspace().unwrap();
        for (id, main) in [("steve", true), ("coder", false), ("reviewer", false)] {
            store
                .bots()
                .seed(
                    id,
                    main,
                    &NewBot {
                        name: id.into(),
                        harness: "fixture".into(),
                        ..Default::default()
                    },
                    WS,
                )
                .unwrap();
        }
        let env = Env::attach(tempfile::tempdir().unwrap(), home.clone(), store, false);
        let (_parent_id, coord) = env.create_parent(None);
        let script = r#"
            export default async function () {
                const a = await agent("step 1", { bot: "coder" });
                const b = await agent("step 2 after " + a.v, { bot: "coder" });
                return { a, b };
            }
        "#;
        let snap = env
            .wf_tool(&coord, json!({ "action": "submit", "script": script }))
            .await
            .unwrap();
        let run_id = snap["run"]["id"].as_str().unwrap().to_owned();
        let child1 = snap["attempts"][0]["child_item_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let sess1 = env.child_session(&child1).id;
        (run_id, child1, sess1)
    };

    // Restart #1 (Hub crashed while step 1 was still Dispatched): production `core.ready()` MUST
    // NOT duplicate step 1!
    {
        let store = Store::open(&db_path, NoPublisher).unwrap();
        let env = Env::attach(tempfile::tempdir().unwrap(), home.clone(), store, false);
        env.core.ready();
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert_eq!(
            env.store
                .read(|r| WorkflowRuns(r).list_attempts_for_run(&run_id))
                .unwrap()
                .len(),
            1,
            "restart while call 1 is active must produce 0 duplicate dispatches"
        );

        // Now deliver & commit child 1 on a Core WITHOUT workflow_executor wired, simulating a
        // crash immediately after commit_cycle committed attempt=succeeded before step 2 woke up!
        let rt_no_exec = Arc::new(TestRuntime::new(env.store.clone()));
        let core_no_exec = Core::new(
            steve_core::repo::Db::new(env.store.clone()),
            rt_no_exec.clone(),
            Arc::new(NoModels),
            CoreConfig::for_home(&home),
        );
        let sess1 = env.store.sessions().get(&child1_sess_id).unwrap().unwrap();
        core_no_exec
            .tool_deliver_with_result(
                &sess1,
                "step 1 done",
                &[],
                &[],
                &[],
                Some(&json!({ "v": "ok" })),
                false,
            )
            .unwrap();
        rt_no_exec.end_turn(&core_no_exec, &child1_sess_id, "end_turn");
        // Child 1 committed atomically on turn end, and step 2 was NOT launched because
        // core_no_exec had no executor (simulating crash right after commit_cycle).
        assert_eq!(
            env.store
                .read(|r| WorkflowRuns(r).attempt_by_child_item(&child1_id))
                .unwrap()
                .unwrap()
                .status,
            WfAttemptStatus::Succeeded
        );
        assert_eq!(
            env.store
                .read(|r| WorkflowRuns(r).list_attempts_for_run(&run_id))
                .unwrap()
                .len(),
            1
        );
    }

    // Restart #2 (Hub boots after crash right after step 1 commit): production `core.ready()`
    // resumes & dispatches step 2 ONCE!
    {
        let store = Store::open(&db_path, NoPublisher).unwrap();
        let env = Env::attach(tempfile::tempdir().unwrap(), home.clone(), store, false);
        env.core.ready();
        env.wait_for_dispatched_count(&run_id, 2).await;
        assert_eq!(
            env.store
                .read(|r| WorkflowRuns(r).list_attempts_for_run(&run_id))
                .unwrap()
                .len(),
            2,
            "restart after post-commit crash must advance cleanly to step 2"
        );
        // Running boot recovery / lifecycle_tick again is idempotent (still 2 attempts, 0 duplicates).
        env.core.ready();
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert_eq!(
            env.store
                .read(|r| WorkflowRuns(r).list_attempts_for_run(&run_id))
                .unwrap()
                .len(),
            2
        );
    }
}

/// Scenario 15 & 16: PR #544 + workflow-script-v3 §6 #4 coordinator placement rules
/// (Steve is coordinator, never a Team member; no eligible Bot opens the single workspace
/// `NO_EXECUTOR` card; `assign_bot` dispatches to chosen Bot; `later` runs waiting step on Steve
/// and flips `noExecutor` from `ask` to `steve`; qualified ordinary Bot still wins over Steve)
/// and parent goal revision superseding active workflow runs.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn placement_blocked_coordinator_rules_and_goal_revision_superseding() {
    let env = Env::new(false);
    let (parent_id, coord) = env.create_parent(None);

    // 0. Coordinator rule 1: Steve (main bot) cannot be saved as a Team member.
    let team_err = env
        .store
        .write(|w| {
            bot_admin::save(
                w,
                WS,
                None,
                "Invalid Team",
                &[bot_dispatch::Member {
                    bot: "steve".into(),
                    nodes: None,
                }],
            )
        })
        .unwrap_err();
    assert!(
        matches!(team_err, StoreError::Rejected(_)),
        "expected StoreError::Rejected when adding main bot to Team, got: {team_err:?}"
    );

    // 1. Default `noExecutor = ask`: required specialty "quantum" has no worker -> MUST NOT
    //    silently fall back to Steve; creates unassigned child item, marks attempt `blocked`,
    //    pauses run, and opens the single workspace `NO_EXECUTOR` AskOwner card.
    let script = r#"
        export default async function () {
            return await agent("quantum work", { specialties: ["quantum"], title: "Quantum Step" });
        }
    "#;
    let snap = env
        .wf_tool(&coord, json!({ "action": "submit", "script": script }))
        .await
        .unwrap();
    let run_id = snap["run"]["id"].as_str().unwrap().to_owned();
    assert_eq!(snap["run"]["status"], "paused");
    assert_eq!(
        snap["run"]["pause_reason_code"],
        WfPauseReason::PlacementBlocked.as_str()
    );
    let dec_id = snap["decision_id"].as_str().unwrap().to_owned();
    let dec = env.store.decisions().get(&dec_id).unwrap().unwrap();
    assert_eq!(dec.subject_type, NO_EXECUTOR);
    assert!(matches!(
        dec.question_template,
        Some(DecisionPrompt::NoExecutor { .. })
    ));
    assert!(dec.options.iter().any(|o| o == "assign_bot"));
    assert!(dec.options.iter().any(|o| o == "later"));
    let blocked_attempts = env
        .store
        .read(|r| WorkflowRuns(r).list_attempts_for_run(&run_id))
        .unwrap();
    assert_eq!(blocked_attempts.len(), 1);
    assert_eq!(blocked_attempts[0].status, WfAttemptStatus::Blocked);
    let waiting_child_id = blocked_attempts[0].child_item_id.clone().unwrap();
    assert_eq!(
        env.store
            .items()
            .get(&waiting_child_id)
            .unwrap()
            .unwrap()
            .bot_id,
        None,
        "blocked workflow child must start unassigned"
    );

    // Owner chooses `assign_bot` with `coder` on the NO_EXECUTOR card -> promotes blocked attempt,
    // starts the waiting child on `coder`, and resumes the workflow run!
    env.core
        .decision_command(
            &env.owner(),
            &dec_id,
            Some(dec.revision),
            DecisionCommand::Resolve {
                choice: "assign_bot".into(),
                text: "coder".into(),
            },
        )
        .unwrap();
    let attempts = env
        .store
        .read(|r| WorkflowRuns(r).list_attempts_for_run(&run_id))
        .unwrap();
    assert_eq!(attempts.len(), 1);
    assert_eq!(attempts[0].status, WfAttemptStatus::Dispatched);
    assert_eq!(attempts[0].bot_id.as_deref(), Some("coder"));
    let active_child = attempts[0].child_item_id.clone().unwrap();
    assert_eq!(active_child, waiting_child_id);
    assert_eq!(
        env.store.sessions().of_item(&active_child).unwrap().len(),
        1,
        "assign_bot must start the waiting workflow child session"
    );

    // 2. Goal revision supersedes active run (goal_version 1 -> 2) and cancels active child!
    env.core
        .revise_goal(&parent_id, "Completely new v2 goal", "owner")
        .unwrap();
    let old_run = env
        .store
        .read(|r| WorkflowRuns(r).get_run(&run_id))
        .unwrap()
        .unwrap();
    assert_eq!(old_run.status, WfRunStatus::Superseded);
    let cancelled_child = env.wait_for_item_closed(&active_child).await;
    assert_eq!(cancelled_child.outcome.as_deref(), Some("cancelled"));

    // 3. Coordinator rule 2: submit v2 while `noExecutor = ask` -> opens NO_EXECUTOR card;
    //    owner chooses `later` -> Steve executes the waiting workflow step itself AND flips
    //    workspace `noExecutor` policy from `ask` to `steve`!
    let snap_v2 = env
        .wf_tool(&coord, json!({ "action": "submit", "script": script }))
        .await
        .unwrap();
    let run_v2_id = snap_v2["run"]["id"].as_str().unwrap().to_owned();
    assert_eq!(snap_v2["run"]["goal_version"], 2);
    assert_eq!(snap_v2["run"]["status"], "paused");
    let dec2_id = snap_v2["decision_id"].as_str().unwrap().to_owned();
    let dec2 = env.store.decisions().get(&dec2_id).unwrap().unwrap();
    assert_eq!(dec2.subject_type, NO_EXECUTOR);

    env.core
        .decision_command(
            &env.owner(),
            &dec2_id,
            Some(dec2.revision),
            DecisionCommand::Resolve {
                choice: "later".into(),
                text: String::new(),
            },
        )
        .unwrap();
    assert_eq!(
        env.store
            .read(|r| bot_dispatch::no_executor_policy(r, WS))
            .unwrap(),
        NoExecutorPolicy::Steve,
        "choosing `later` must flip workspace noExecutor policy to `steve`"
    );
    let v2_attempts = env
        .store
        .read(|r| WorkflowRuns(r).list_attempts_for_run(&run_v2_id))
        .unwrap();
    assert_eq!(v2_attempts.len(), 1);
    assert_eq!(v2_attempts[0].status, WfAttemptStatus::Dispatched);
    assert_eq!(v2_attempts[0].bot_id.as_deref(), Some("steve"));
    let v2_child = v2_attempts[0].child_item_id.clone().unwrap();
    let v2_fact = env
        .store
        .read(|r| bot_dispatch::initial(r, &v2_child))
        .unwrap()
        .unwrap();
    assert_eq!(
        v2_fact.explanation.reason,
        PlacementReason::CoordinatorDeferred
    );
    assert_eq!(
        v2_fact.explanation.deferred_by,
        Some(CoordinatorDeferSource::Card)
    );
    assert_eq!(env.store.sessions().of_item(&v2_child).unwrap().len(), 1);

    // 4. Subsequent blocked step under `noExecutor = steve` executes directly on Steve
    //    (`CoordinatorDeferSource::Setting`), while a qualified ordinary Bot still wins!
    env.core
        .revise_goal(&parent_id, "Completely new v3 goal", "owner")
        .unwrap();
    let script_v3 = r#"
        export default async function () {
            return await all([
                () => agent("quantum work", { specialties: ["quantum"], title: "Quantum Direct" }),
                () => agent("rust implementation", { specialties: ["rust"], title: "Rust Direct" }),
            ]);
        }
    "#;
    let snap_v3 = env
        .wf_tool(&coord, json!({ "action": "submit", "script": script_v3 }))
        .await
        .unwrap();
    assert_eq!(snap_v3["run"]["goal_version"], 3);
    assert_eq!(snap_v3["run"]["status"], "running");
    let v3_attempts = env
        .store
        .read(|r| WorkflowRuns(r).list_attempts_for_run(snap_v3["run"]["id"].as_str().unwrap()))
        .unwrap();
    assert_eq!(v3_attempts.len(), 2);
    let bots_by_call: HashSet<&str> = v3_attempts
        .iter()
        .filter_map(|a| a.bot_id.as_deref())
        .collect();
    assert!(
        bots_by_call.contains("steve") && bots_by_call.contains("coder"),
        "expected Steve for unmatched step and qualified coder for rust step, got: {bots_by_call:?}"
    );
    let steve_child = v3_attempts
        .iter()
        .find(|a| a.bot_id.as_deref() == Some("steve"))
        .and_then(|a| a.child_item_id.as_deref())
        .unwrap();
    let v3_fact = env
        .store
        .read(|r| bot_dispatch::initial(r, steve_child))
        .unwrap()
        .unwrap();
    assert_eq!(
        v3_fact.explanation.reason,
        PlacementReason::CoordinatorDeferred
    );
    assert_eq!(
        v3_fact.explanation.deferred_by,
        Some(CoordinatorDeferSource::Setting)
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_05_e2e_completed_script_cancels_unfinished_sibling_child() {
    let env = Env::new(false);
    let (_parent_id, coord) = env.create_parent(None);
    let script = r#"
        export default async function () {
            try {
                await all([
                    () => agent("branch A", { bot: "coder", title: "A" }),
                    () => agent("branch B", { bot: "coder", title: "B" }),
                ]);
                return { ok: true };
            } catch (e) {
                return { recovered: e.code };
            }
        }
    "#;
    let snap = env
        .wf_tool(&coord, json!({ "action": "submit", "script": script }))
        .await
        .unwrap();
    let run_id = snap["run"]["id"].as_str().unwrap().to_owned();
    let child_a = snap["attempts"][0]["child_item_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let child_b = snap["attempts"][1]["child_item_id"]
        .as_str()
        .unwrap()
        .to_owned();
    let call_b = snap["attempts"][1]["call_id"].as_str().unwrap().to_owned();

    // Fail A while B is still running; catch completes the script so B MUST be cancelled.
    env.fail_child(&child_a, "boom").await;
    let closed_b = env.wait_for_item_closed(&child_b).await;
    assert_eq!(closed_b.outcome.as_deref(), Some("cancelled"));
    let call_b_row = env
        .store
        .read(|r| WorkflowRuns(r).get_call(&call_b))
        .unwrap()
        .unwrap();
    assert_eq!(call_b_row.status, WfCallStatus::Cancelled);
    let att_b = env
        .store
        .read(|r| WorkflowRuns(r).attempt_by_child_item(&child_b))
        .unwrap()
        .unwrap();
    assert_eq!(att_b.status, WfAttemptStatus::Cancelled);
    let _ = run_id;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_06_failed_or_cancelled_calls_not_hydrated_across_versions_and_retry_after_catch() {
    // Part A: A Failed call in v1 must NOT poison v2 (only Completed calls hydrate across versions).
    let env = Env::new(false);
    let (_parent_id, coord) = env.create_parent(None);
    let script_v1 = r#"
        export default async function () {
            try {
                const a = await agent("flaky step", { bot: "coder", title: "Flaky" });
                return { ok: a };
            } catch (e) {
                return await agent("after fail", { bot: "coder", title: "Fallback" });
            }
        }
    "#;
    let snap1 = env
        .wf_tool(&coord, json!({ "action": "submit", "script": script_v1 }))
        .await
        .unwrap();
    let run_id = snap1["run"]["id"].as_str().unwrap().to_owned();
    let call_flaky = snap1["attempts"][0]["call_id"].as_str().unwrap().to_owned();
    let child_flaky_1 = snap1["attempts"][0]["child_item_id"]
        .as_str()
        .unwrap()
        .to_owned();

    // Fail flaky step -> catch branch dispatches "after fail" (recorded at seq 1 of v1).
    env.fail_child(&child_flaky_1, "transient").await;
    env.wait_for_dispatched_count(&run_id, 2).await;
    let child_fallback = env
        .store
        .read(|r| WorkflowRuns(r).list_attempts_for_run(&run_id))
        .unwrap()[1]
        .child_item_id
        .clone()
        .unwrap();

    // Part B: Retry the caught failed call inside v1! Must NOT trigger NonDeterministicReplay!
    let snap_retry = env
        .wf_tool(&coord, json!({ "action": "retry", "call_id": call_flaky }))
        .await
        .unwrap();
    assert_eq!(
        snap_retry["run"]["status"], "running",
        "retrying a caught failed call must stay running, not pause with non_deterministic_replay: {snap_retry}"
    );
    let closed_fallback = env.wait_for_item_closed(&child_fallback).await;
    assert_eq!(closed_fallback.outcome.as_deref(), Some("cancelled"));
    let flaky_attempts_after_retry = env
        .store
        .read(|r| WorkflowRuns(r).list_attempts_for_call(&call_flaky))
        .unwrap();
    assert_eq!(flaky_attempts_after_retry.len(), 2);
    assert_eq!(
        flaky_attempts_after_retry[1].status,
        WfAttemptStatus::Dispatched
    );

    // Fail attempt 2 as well so flaky call is Failed again, then submit v2 calling the EXACT same
    // "flaky step" WITHOUT catch: v2 MUST NOT hydrate the v1 failure; it must dispatch attempt 3!
    let child_flaky_2 = flaky_attempts_after_retry[1].child_item_id.clone().unwrap();
    env.fail_child(&child_flaky_2, "still failing").await;
    env.wait_for_dispatched_count(&run_id, 4).await;

    let script_v2 = r#"
        export default async function () {
            const a = await agent("flaky step", { bot: "coder", title: "Flaky" });
            const r = await agent("review", { bot: "reviewer", review: true });
            return { a, r };
        }
    "#;
    let snap_v2 = env
        .wf_tool(&coord, json!({ "action": "submit", "script": script_v2 }))
        .await
        .unwrap();
    assert_eq!(
        snap_v2["run"]["status"], "running",
        "v2 must NOT inherit v1's Failed status as a poisoned recorded call: {snap_v2}"
    );
    let flaky_attempts_v2 = env
        .store
        .read(|r| WorkflowRuns(r).list_attempts_for_call(&call_flaky))
        .unwrap();
    assert_eq!(
        flaky_attempts_v2.len(),
        3,
        "v2 must dispatch a fresh attempt 3 for previously failed call"
    );
    assert_eq!(flaky_attempts_v2[2].status, WfAttemptStatus::Dispatched);
    let vc_v2 = env
        .store
        .read(|r| WorkflowRuns(r).list_version_calls(&run_id, 2))
        .unwrap();
    assert_eq!(
        vc_v2[0].hydrated_from_version, None,
        "re-queued non-completed call must NOT be marked as hydrated"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_07_cancelled_run_can_resubmit_script_and_clear_delivery_blocker() {
    let env = Env::new(false);
    let (parent_id, coord) = env.create_parent(None);

    let script_v1 = r#"
        export default async function () {
            return await agent("old plan", { bot: "coder" });
        }
    "#;
    let snap1 = env
        .wf_tool(&coord, json!({ "action": "submit", "script": script_v1 }))
        .await
        .unwrap();
    let run_id = snap1["run"]["id"].as_str().unwrap().to_owned();

    // Cancel the run.
    let cancelled = env
        .wf_tool(
            &coord,
            json!({ "action": "cancel", "reason": "wrong approach" }),
        )
        .await
        .unwrap();
    assert_eq!(cancelled["run"]["status"], "cancelled");

    // Resubmit a brand-new script version on the cancelled run -> MUST reopen cleanly (v2)!
    let script_v2 = r#"
        export default async function () {
            const w = await agent("better plan", { bot: "coder" });
            const r = await agent("audit better plan", { bot: "reviewer", review: true });
            return { w, r };
        }
    "#;
    let snap2 = env
        .wf_tool(&coord, json!({ "action": "submit", "script": script_v2 }))
        .await
        .expect("cancelled run must allow submitting a new script version");
    assert_eq!(snap2["run"]["id"], run_id);
    assert_eq!(snap2["run"]["active_script_version"], 2);
    assert_eq!(snap2["run"]["status"], "running");

    let work_child = env
        .store
        .read(|r| WorkflowRuns(r).list_active_attempts_for_run(&run_id))
        .unwrap()[0]
        .child_item_id
        .clone()
        .unwrap();
    env.deliver_child(&work_child, json!({ "ok": true })).await;
    env.wait_for_dispatched_count(&run_id, 3).await;
    let rev_child = env
        .store
        .read(|r| WorkflowRuns(r).list_active_attempts_for_run(&run_id))
        .unwrap()[0]
        .child_item_id
        .clone()
        .unwrap();
    env.deliver_child(&rev_child, json!({ "approved": true }))
        .await;
    env.wait_for_run_status(&run_id, WfRunStatus::Completed)
        .await;

    // Parent delivery gate must now be completely unblocked!
    env.core
        .tool_deliver(
            &coord,
            "Parent complete after reopening cancelled run",
            &[],
            &[],
            &[],
        )
        .expect("parent delivery must succeed once reopened run completes");
    let _ = parent_id;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_10_infra_faults_retry_bounded_and_distinguish_from_script_faults() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use steve_workflow_api::{WorkflowFault, WorkflowFaultCode};

    struct FlakyInfraExecutor {
        fail_times: usize,
        code: WorkflowFaultCode,
        calls: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl WorkflowStepExecutor for FlakyInfraExecutor {
        async fn execute(&self, input: &StepInput) -> StepFrame {
            let n = self.calls.fetch_add(1, Ordering::SeqCst);
            if n < self.fail_times {
                StepFrame::Failed {
                    fault: WorkflowFault::new(self.code, format!("injected fault #{n}")),
                    emitted_calls: vec![],
                    logs: vec![],
                }
            } else {
                steve_workflow_js::run_step(input)
            }
        }
    }

    // 1. Two transient SubprocessFailed errors followed by success -> auto-retries and completes!
    {
        let calls = Arc::new(AtomicUsize::new(0));
        let env = Env::with_executor(Arc::new(FlakyInfraExecutor {
            fail_times: 2,
            code: WorkflowFaultCode::SubprocessFailed,
            calls: calls.clone(),
        }));
        let (_p, coord) = env.create_parent(None);
        let snap = env
            .core
            .tool_workflow_run(
                &coord,
                json!({ "action": "submit", "script": "export default async function () { return { ok: 1 }; }" }),
            )
            .await
            .unwrap();
        let snap = serde_json::to_value(snap).unwrap();
        assert_eq!(
            snap["run"]["status"], "completed",
            "transient infra faults must auto-retry: {snap}"
        );
        assert_eq!(calls.load(Ordering::SeqCst), 3);
    }

    // 2. Persistent Timeout exhausts bounded retries and pauses as InfrastructureFault (NOT ScriptFault).
    {
        let calls = Arc::new(AtomicUsize::new(0));
        let env = Env::with_executor(Arc::new(FlakyInfraExecutor {
            fail_times: 10,
            code: WorkflowFaultCode::Timeout,
            calls: calls.clone(),
        }));
        let (_p, coord) = env.create_parent(None);
        let snap = env
            .core
            .tool_workflow_run(
                &coord,
                json!({ "action": "submit", "script": "export default async function () { return 1; }" }),
            )
            .await
            .unwrap();
        let snap = serde_json::to_value(snap).unwrap();
        assert_eq!(snap["run"]["status"], "paused");
        assert_eq!(
            snap["run"]["pause_reason_code"],
            WfPauseReason::InfrastructureFault.as_str(),
            "exhausted infra fault must use InfrastructureFault pause reason: {snap}"
        );
        assert!(
            calls.load(Ordering::SeqCst) > 1,
            "infra fault must retry before pausing, got {} calls",
            calls.load(Ordering::SeqCst)
        );
    }

    // 3. Deterministic SyntaxError must NOT retry and pauses immediately as ScriptFault.
    {
        let calls = Arc::new(AtomicUsize::new(0));
        let env = Env::with_executor(Arc::new(FlakyInfraExecutor {
            fail_times: 10,
            code: WorkflowFaultCode::SyntaxError,
            calls: calls.clone(),
        }));
        let (_p, coord) = env.create_parent(None);
        let snap = env
            .core
            .tool_workflow_run(
                &coord,
                json!({ "action": "submit", "script": "export default async function () { return 1; }" }),
            )
            .await
            .unwrap();
        let snap = serde_json::to_value(snap).unwrap();
        assert_eq!(snap["run"]["status"], "paused");
        assert_eq!(
            snap["run"]["pause_reason_code"],
            WfPauseReason::ScriptFault.as_str()
        );
        assert_eq!(
            calls.load(Ordering::SeqCst),
            1,
            "script faults must not auto-retry"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_12_corrupt_options_json_errors_instead_of_silently_defaulting() {
    let env = Env::new(false);
    let (_parent_id, coord) = env.create_parent(None);

    // Typed WfToolOutput::Empty when no run exists yet.
    let empty_out = env
        .core
        .tool_workflow_run(&coord, json!({ "action": "get" }))
        .await
        .unwrap();
    assert!(
        matches!(empty_out, steve_core::WfToolOutput::Empty { run: None }),
        "expected typed WfToolOutput::Empty, got {empty_out:?}"
    );

    let script = r#"
        export default async function () {
            const a = await agent("first", { bot: "coder" });
            const b = await agent("second", { bot: "coder", mutex_key: "m1" });
            return { a, b };
        }
    "#;
    // Submit returns strongly-typed WfToolOutput::Snapshot.
    let typed_out = env
        .core
        .tool_workflow_run(&coord, json!({ "action": "submit", "script": script }))
        .await
        .unwrap();
    let snap = typed_out
        .into_snapshot()
        .expect("submit must return WfToolOutput::Snapshot");
    assert_eq!(
        env.core.workflow_run_lock_count(),
        0,
        "workflow_run_locks must be cleaned up once step completes"
    );
    let run_id = snap.run.id.clone();

    // Corrupt call 0's options_canonical_json and requeue it so dispatch must parse it.
    let call_id = snap.calls[0].id.clone();
    let child_id = snap.attempts[0].child_item_id.clone().unwrap();
    env.fail_child(&child_id, "force requeue").await;
    env.store
        .write(|w| WorkflowRuns(w).requeue_call_for_retry(&call_id))
        .unwrap();
    env.store
        .fixture_write(|c| {
            c.execute(
                "UPDATE wf_calls SET options_canonical_json = '{corrupt' WHERE id = ?1",
                [&call_id],
            )?;
            Ok(())
        })
        .unwrap();

    let res = env.core.step_workflow_run(&run_id).await;
    assert!(
        res.is_err(),
        "corrupt options_canonical_json must return an error instead of unwrap_or_default(), got: {res:?}"
    );
    assert_eq!(
        env.core.workflow_run_lock_count(),
        0,
        "workflow_run_locks must also clean up after a step error"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn review_01_workflow_steps_offload_blocking_and_cap_concurrency() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct SlowCountingExecutor {
        in_flight: Arc<AtomicUsize>,
        max_in_flight: Arc<AtomicUsize>,
    }

    #[async_trait::async_trait]
    impl WorkflowStepExecutor for SlowCountingExecutor {
        async fn execute(&self, input: &StepInput) -> StepFrame {
            let cur = self.in_flight.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_in_flight.fetch_max(cur, Ordering::SeqCst);
            tokio::time::sleep(Duration::from_millis(60)).await;
            let frame = steve_workflow_js::run_step(input);
            self.in_flight.fetch_sub(1, Ordering::SeqCst);
            frame
        }
    }

    // 1. Concurrency cap: 8 concurrent workflow runs must never exceed the global step cap (4).
    let in_flight = Arc::new(AtomicUsize::new(0));
    let max_in_flight = Arc::new(AtomicUsize::new(0));
    let env = Env::with_executor(Arc::new(SlowCountingExecutor {
        in_flight: in_flight.clone(),
        max_in_flight: max_in_flight.clone(),
    }));
    let mut set = tokio::task::JoinSet::new();
    for i in 0..8 {
        let (_pid, coord) = env.create_parent(None);
        let core = env.core.clone();
        set.spawn(async move {
            let script = format!("export default async function () {{ return {{ n: {i} }}; }}");
            core.tool_workflow_run(&coord, json!({ "action": "submit", "script": script }))
                .await
                .unwrap()
        });
    }
    while let Some(res) = set.join_next().await {
        let out = res.unwrap().into_snapshot().unwrap();
        assert_eq!(out.run.status, WfRunStatus::Completed);
    }
    let observed_max = max_in_flight.load(Ordering::SeqCst);
    assert!(
        observed_max <= 4,
        "concurrent workflow steps must be capped (<= 4), observed max_in_flight = {observed_max}"
    );
    assert!(observed_max >= 2, "expected parallel execution up to cap");

    // 2. Blocking offload: on a single-threaded tokio runtime, contended SQLite / Core work inside
    //    `tool_workflow_run` must not stall the tokio event loop timer.
    let ticks = std::thread::spawn(|| {
        let single_rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        single_rt.block_on(async {
            let env2 = Env::new(false);
            let (_pid, coord) = env2.create_parent(None);
            let store_blocker = env2.store.clone();
            let (locked_tx, locked_rx) = std::sync::mpsc::channel::<()>();
            let blocker = std::thread::spawn(move || {
                store_blocker
                    .fixture_write(|_| {
                        let _ = locked_tx.send(());
                        std::thread::sleep(Duration::from_millis(160));
                        Ok(())
                    })
                    .unwrap();
            });
            locked_rx.recv().unwrap();

            let tick_count = Arc::new(AtomicUsize::new(0));
            let tc = tick_count.clone();
            let ticker = tokio::spawn(async move {
                let mut iv = tokio::time::interval(Duration::from_millis(15));
                loop {
                    iv.tick().await;
                    tc.fetch_add(1, Ordering::SeqCst);
                }
            });

            // Submit requires a SQLite write, so it blocks behind `blocker` for ~160ms.
            let out = env2
                .core
                .tool_workflow_run(
                    &coord,
                    json!({ "action": "submit", "script": "export default async function () { return 1; }" }),
                )
                .await
                .unwrap();
            assert_eq!(
                out.into_snapshot().unwrap().run.status,
                WfRunStatus::Completed
            );
            ticker.abort();
            blocker.join().unwrap();
            tick_count.load(Ordering::SeqCst)
        })
    })
    .join()
    .unwrap();
    assert!(
        ticks >= 4,
        "tool_workflow_run blocked the single-threaded tokio runtime (only {ticks} 15ms timer ticks during 160ms SQLite lock)"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_03_shared_repo_and_remote_node_git_hydration_preserve_valid_commits() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    use steve_core::node_git::{Applied, Ask, NodeGit};
    use steve_node_api as na;

    // Part A: `worktree = false` shared repository — subsequent commits advance HEAD in p.path,
    // which MUST NOT invalidate step A's already-committed output when revising to v2.
    {
        let env = Env::new(false);
        let (proj_id, repo_path) = init_git_project(&env, "wf3-shared-repo");
        env.store
            .fixture_write(|c| {
                c.execute("UPDATE projects SET worktree = 0 WHERE id = ?1", [&proj_id])?;
                Ok(())
            })
            .unwrap();
        let (_parent_id, coord) = env.create_parent(Some(proj_id.clone()));

        let script_v1 = r#"
            export default async function () {
                const a = await agent("step A", { bot: "coder", title: "A" });
                const b = await agent("step B", { bot: "coder", title: "B" });
                return { a, b };
            }
        "#;
        let snap1 = env
            .wf_tool(&coord, json!({ "action": "submit", "script": script_v1 }))
            .await
            .unwrap();
        let run_id = snap1["run"]["id"].as_str().unwrap().to_owned();
        let child_a = snap1["attempts"][0]["child_item_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let call_a_id = snap1["attempts"][0]["call_id"].as_str().unwrap().to_owned();

        // Child A commits in shared repo_path and delivers.
        std::fs::write(repo_path.join("a.txt"), "from step A\n").unwrap();
        git_run(&repo_path, &["add", "a.txt"]);
        git_run(&repo_path, &["commit", "-m", "commit A"]);
        env.deliver_child(&child_a, json!({ "done": "A" })).await;
        env.wait_for_dispatched_count(&run_id, 2).await;

        // Another commit lands in the shared repo_path, advancing HEAD!
        std::fs::write(repo_path.join("b.txt"), "from step B\n").unwrap();
        git_run(&repo_path, &["add", "b.txt"]);
        git_run(&repo_path, &["commit", "-m", "commit B"]);

        // Submit v2 reusing step A -> step A's commit & tree still exist in repo_path -> MUST hydrate!
        let script_v2 = r#"
            export default async function () {
                const a = await agent("step A", { bot: "coder", title: "A" });
                const c = await agent("step C", { bot: "coder", title: "C" });
                return { a, c };
            }
        "#;
        env.wf_tool(&coord, json!({ "action": "submit", "script": script_v2 }))
            .await
            .unwrap();
        assert_eq!(
            env.store
                .read(|r| WorkflowRuns(r).count_call_attempts(&call_a_id))
                .unwrap(),
            1,
            "worktree=false shared repo: subsequent commit must NOT invalidate prior call A"
        );
    }

    // Part B: Remote Node child item — cwd does not exist on Hub; NodeGit verifies commit/tree
    // off-lock and caches the verified result per script version.
    {
        struct MockNodeGit {
            remote_item: std::sync::Mutex<Option<String>>,
            valid_head: String,
            valid_tree: String,
            present: std::sync::atomic::AtomicBool,
            facts_calls: Arc<AtomicUsize>,
        }

        #[async_trait::async_trait]
        impl NodeGit for MockNodeGit {
            fn on_node(&self, item: &str) -> bool {
                self.remote_item.lock().unwrap().as_deref() == Some(item)
            }

            async fn facts(&self, _item: &str, q: Ask) -> na::ApiResult<na::GitFacts> {
                self.facts_calls.fetch_add(1, Ordering::SeqCst);
                if !self.present.load(Ordering::SeqCst) {
                    return Err(na::Error::new(na::Code::Invalid, "commit missing on node"));
                }
                let mut reads = Vec::new();
                for r in q.reads {
                    let answer = match &r {
                        na::GitRead::Show { rev, .. } if rev == &self.valid_head => {
                            na::GitAnswer::Commit {
                                commit: na::CommitInfo {
                                    sha: self.valid_head.clone(),
                                    subject: "remote commit".into(),
                                    author: "bot".into(),
                                    time: na::Timestamp(0),
                                    abbrev: None,
                                    body: None,
                                },
                                stat: String::new(),
                                patch: String::new(),
                                truncated: false,
                            }
                        }
                        na::GitRead::ChangedPaths { from, to, .. }
                            if from == &self.valid_head && to == &self.valid_tree =>
                        {
                            na::GitAnswer::ChangedPaths {
                                paths: vec![],
                                truncated: false,
                            }
                        }
                        _ => {
                            return Err(na::Error::new(na::Code::Invalid, "unexpected rev/tree"));
                        }
                    };
                    reads.push(na::GitReadFact {
                        read: r,
                        observed: self.valid_head.clone(),
                        answer,
                    });
                }
                Ok(na::GitFacts {
                    revision: na::Revision::new("rev1").unwrap(),
                    head: "0000000000000000000000000000000000000000".into(),
                    branch: Some("main".into()),
                    dirty: false,
                    refs: vec![],
                    ranges: vec![],
                    reads,
                })
            }

            async fn apply(
                &self,
                _item: &str,
                _operation: &str,
                _subject: &str,
                _on: &na::GitFacts,
                _action: na::GitAction,
            ) -> Applied {
                Applied::Unknown("unused".into())
            }
        }

        let env = Env::new(false);
        let facts_calls = Arc::new(AtomicUsize::new(0));
        let remote_head = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa".to_string();
        let remote_tree = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb".to_string();
        let node_git = Arc::new(MockNodeGit {
            remote_item: std::sync::Mutex::new(None),
            valid_head: remote_head.clone(),
            valid_tree: remote_tree.clone(),
            present: std::sync::atomic::AtomicBool::new(true),
            facts_calls: facts_calls.clone(),
        });
        env.core.set_node_git(node_git.clone());

        let (_parent_id, coord) = env.create_parent(None);
        let script_v1 = r#"
            export default async function () {
                const a = await agent("remote step A", { bot: "coder" });
                const b = await agent("step B", { bot: "coder" });
                return { a, b };
            }
        "#;
        let snap1 = env
            .wf_tool(&coord, json!({ "action": "submit", "script": script_v1 }))
            .await
            .unwrap();
        let run_id = snap1["run"]["id"].as_str().unwrap().to_owned();
        let child_a = snap1["attempts"][0]["child_item_id"]
            .as_str()
            .unwrap()
            .to_owned();
        let call_a_id = snap1["attempts"][0]["call_id"].as_str().unwrap().to_owned();
        *node_git.remote_item.lock().unwrap() = Some(child_a.clone());

        // Deliver step A, then record a remote-only git fingerprint whose cwd does NOT exist on Hub.
        env.deliver_child(&child_a, json!({ "remote": true })).await;
        env.wait_for_dispatched_count(&run_id, 2).await;
        env.store
            .fixture_write(|c| {
                c.execute(
                    "UPDATE wf_calls SET git_head = ?1, git_tree = ?2, git_cwd = '/nonexistent/remote-node/wt' WHERE id = ?3",
                    [&remote_head, &remote_tree, &call_a_id],
                )?;
                Ok(())
            })
            .unwrap();

        // Revise to v2 keeping remote step A -> NodeGit verifies commit+tree -> hydrates (1 attempt)!
        facts_calls.store(0, Ordering::SeqCst);
        let script_v2 = r#"
            export default async function () {
                const a = await agent("remote step A", { bot: "coder" });
                const c = await agent("step C", { bot: "coder" });
                const d = await agent("step D", { bot: "coder" });
                return { a, c, d };
            }
        "#;
        env.wf_tool(&coord, json!({ "action": "submit", "script": script_v2 }))
            .await
            .unwrap();
        assert_eq!(
            env.store
                .read(|r| WorkflowRuns(r).count_call_attempts(&call_a_id))
                .unwrap(),
            1,
            "remote Node git output verified via NodeGit must hydrate without re-running"
        );
        assert_eq!(
            facts_calls.load(Ordering::SeqCst),
            1,
            "remote Node verification must be queried once for v2"
        );

        // Advancing another step within v2 (step C -> step D) must NOT re-query NodeGit for A!
        let child_c = env
            .store
            .read(|r| WorkflowRuns(r).list_active_attempts_for_run(&run_id))
            .unwrap()[0]
            .child_item_id
            .clone()
            .unwrap();
        env.deliver_child(&child_c, json!({ "c": 1 })).await;
        env.wait_for_dispatched_count(&run_id, 4).await;
        assert_eq!(
            facts_calls.load(Ordering::SeqCst),
            1,
            "cached hydration verification must not re-query NodeGit on subsequent steps of v2"
        );

        // When Node reports the commit missing on v3, hydration MUST be rejected and step A re-runs!
        node_git.present.store(false, Ordering::SeqCst);
        let script_v3 = r#"
            export default async function () {
                const a = await agent("remote step A", { bot: "coder" });
                const e = await agent("step E", { bot: "coder" });
                return { a, e };
            }
        "#;
        env.wf_tool(&coord, json!({ "action": "submit", "script": script_v3 }))
            .await
            .unwrap();
        assert_eq!(
            env.store
                .read(|r| WorkflowRuns(r).count_call_attempts(&call_a_id))
                .unwrap(),
            2,
            "missing commit on remote Node must invalidate and re-run step A"
        );
        assert_eq!(
            facts_calls.load(Ordering::SeqCst),
            2,
            "new script version v3 must re-verify remote Node git output"
        );
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_04_no_resume_maintenance_hold_and_mutex_wait_do_not_spin_steps() {
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct CountingJsExecutor(Arc<AtomicUsize>);

    #[async_trait::async_trait]
    impl WorkflowStepExecutor for CountingJsExecutor {
        async fn execute(&self, input: &StepInput) -> StepFrame {
            self.0.fetch_add(1, Ordering::SeqCst);
            steve_workflow_js::run_step(input)
        }
    }

    // 1. STEVE_NO_RESUME: ready() + lifecycle_tick() must NOT step pre-existing running runs!
    {
        let env = Env::new(false);
        let (parent_id, _coord) = env.create_parent(None);
        env.store
            .write(|w| {
                WorkflowRuns(w).create_run(
                    WS,
                    &parent_id,
                    1,
                    1,
                    "export default async function () { return await agent('should not run', { bot: 'coder' }); }",
                    "sha-no-resume",
                    None,
                )
            })
            .unwrap();

        let count = Arc::new(AtomicUsize::new(0));
        let mut cfg = CoreConfig::for_home(&env.mcp.home);
        cfg.no_resume = true;
        let core_nr = Core::new(
            steve_core::repo::Db::new(env.store.clone()),
            env.rt.clone(),
            Arc::new(NoModels),
            cfg,
        );
        core_nr.set_workflow_executor(Arc::new(CountingJsExecutor(count.clone())));
        core_nr.ready();
        core_nr.lifecycle_tick().unwrap();
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert_eq!(
            count.load(Ordering::SeqCst),
            0,
            "STEVE_NO_RESUME=1 must prevent ready() and lifecycle_tick() from stepping running runs"
        );
    }

    // 2. Maintenance hold: lifecycle_tick() must NOT step running runs while held.
    {
        let count = Arc::new(AtomicUsize::new(0));
        let env = Env::with_executor(Arc::new(CountingJsExecutor(count.clone())));
        let (parent_id, _coord) = env.create_parent(None);
        let run = env
            .store
            .write(|w| {
                WorkflowRuns(w).create_run(
                    WS,
                    &parent_id,
                    1,
                    1,
                    "export default async function () { return { ok: true }; }",
                    "sha-hold",
                    None,
                )
            })
            .unwrap();
        env.core.maintenance_hold(true).unwrap();
        env.core.lifecycle_tick().unwrap();
        tokio::time::sleep(Duration::from_millis(80)).await;
        assert_eq!(
            count.load(Ordering::SeqCst),
            0,
            "maintenance_hold(true) must prevent lifecycle_tick() from stepping running runs"
        );
        env.core.maintenance_hold(false).unwrap();
        env.wait_for_run_status(&run.id, WfRunStatus::Completed)
            .await;
    }

    // 3. Waiting on another run's mutex_key must NOT spin `--workflow-step` on every tick!
    {
        let count = Arc::new(AtomicUsize::new(0));
        let env = Env::with_executor(Arc::new(CountingJsExecutor(count.clone())));
        let (_p1, coord1) = env.create_parent(None);
        let (_p2, coord2) = env.create_parent(None);

        let script1 = r#"export default async function () { return await agent("hold lock", { bot: "coder", mutex_key: "deploy" }); }"#;
        let snap1 = env
            .wf_tool(&coord1, json!({ "action": "submit", "script": script1 }))
            .await
            .unwrap();
        let child1 = snap1["attempts"][0]["child_item_id"]
            .as_str()
            .unwrap()
            .to_owned();

        let script2 = r#"export default async function () { return await agent("wait lock", { bot: "coder", mutex_key: "deploy" }); }"#;
        let snap2 = env
            .wf_tool(&coord2, json!({ "action": "submit", "script": script2 }))
            .await
            .unwrap();
        let run2_id = snap2["run"]["id"].as_str().unwrap().to_owned();
        assert!(
            snap2["attempts"].as_array().unwrap().is_empty(),
            "run2 must be queued behind run1's mutex_key"
        );

        // Reset step counter and trigger 3 periodic ticks: run2 must NOT spin the JS step executor!
        count.store(0, Ordering::SeqCst);
        for _ in 0..3 {
            env.core.lifecycle_tick().unwrap();
            tokio::time::sleep(Duration::from_millis(40)).await;
        }
        assert_eq!(
            count.load(Ordering::SeqCst),
            0,
            "run waiting on another run's mutex_key must NOT spin step subprocess on periodic ticks"
        );

        // Once run1 releases the mutex, next tick dispatches run2!
        env.deliver_child(&child1, json!({ "ok": 1 })).await;
        env.core.lifecycle_tick().unwrap();
        env.wait_for_dispatched_count(&run2_id, 1).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn review_11_workflow_child_uses_create_item_from_and_owner_finish_api() {
    let env = Env::new(false);
    let backend_tag = env
        .store
        .tags()
        .create(
            WS,
            &steve_store::NewTag {
                name: "backend".into(),
                cat: "domain".into(),
                description: "Backend work".into(),
            },
            "owner",
        )
        .unwrap();
    let (_parent_id, coord) = env.create_parent(None);
    let script = r#"
        export default async function () {
            const w = await agent("implement service", {
                bot: "coder",
                tags: ["backend"],
                schema: { type: "object", required: ["ok"], properties: { ok: { type: "boolean" } } }
            });
            return w;
        }
    "#;
    let snap = env
        .wf_tool(&coord, json!({ "action": "submit", "script": script }))
        .await
        .unwrap();
    let child_id = snap["attempts"][0]["child_item_id"]
        .as_str()
        .unwrap()
        .to_owned();

    // 1. Unified create_item_from invariants: item.create audit entry, workspace tag_ids, requested_by.
    let audit_count: i64 = env
        .store
        .fixture_read(|c| {
            let n: i64 = c.query_row(
                "SELECT COUNT(*) FROM audit WHERE workspace_id = ?1 AND action = 'item.create' AND target = ?2",
                [WS, &child_id],
                |row| row.get(0),
            )?;
            Ok::<i64, StoreError>(n)
        })
        .unwrap();
    assert_eq!(
        audit_count, 1,
        "workflow child must be created via create_item_from and emit item.create audit"
    );
    let child_tags = env.store.tags().of_item(&child_id).unwrap();
    assert!(
        child_tags.iter().any(|t| t == &backend_tag.id),
        "workflow child must resolve CallOptions.tags into workspace tag_ids, got {:?}",
        child_tags
    );
    assert_eq!(
        env.store
            .read(|r| steve_store::item_repo::requested_by(r, &child_id))
            .unwrap()
            .as_deref(),
        Some("owner"),
        "workflow child must inherit verified requester membership"
    );

    // 2. Owner structured takeover wired into real `lifecycle_command("finish", result)` API:
    //    - Omitting `result` is refused with `wf_child_structured_delivery_required`;
    //    - Schema-violating `result` is refused with 409;
    //    - Valid `result` commits child completion atomically with CAS check.
    let child_item = env.store.items().get(&child_id).unwrap().unwrap();
    let err_missing = env
        .core
        .lifecycle_command(
            &child_id,
            "finish",
            child_item.cycle,
            child_item.cas_revision,
            None,
            None,
            None,
            "owner",
        )
        .unwrap_err();
    assert!(
        err_missing
            .to_string()
            .contains("wf_child_structured_delivery_required")
    );
    let err_bad = env
        .core
        .lifecycle_command(
            &child_id,
            "finish",
            child_item.cycle,
            child_item.cas_revision,
            Some("bad"),
            None,
            Some(&json!({ "ok": "not-a-bool" })),
            "owner",
        )
        .unwrap_err();
    assert!(err_bad.to_string().contains("schema_violation"));

    let child_sess = env.child_session(&child_id);
    env.core
        .lifecycle_command(
            &child_id,
            "finish",
            child_item.cycle,
            child_item.cas_revision,
            Some("owner completed step"),
            None,
            Some(&json!({ "ok": true })),
            "owner",
        )
        .unwrap();
    env.rt.end_turn(&env.core, &child_sess.id, "end_turn");
    let closed = env.store.items().get(&child_id).unwrap().unwrap();
    assert_eq!(closed.phase, ItemPhase::Closed);
    let attempt = env
        .store
        .read(|r| WorkflowRuns(r).attempt_by_child_item(&child_id))
        .unwrap()
        .unwrap();
    assert_eq!(attempt.status, WfAttemptStatus::Succeeded);
}
