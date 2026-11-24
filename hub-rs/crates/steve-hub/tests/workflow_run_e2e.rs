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
    workflow_run::{SETTING_NO_EXECUTOR, WF_RUN_SUBJECT, budget_settings},
};
use steve_hub::wire::core::CoreMcp;
use steve_hub::wire::workspaces::Workspaces;
use steve_hub::workflow_step::StepRunner;
use steve_mcp::Backend as _;
use steve_modules::Caller;
use steve_store::workflow_runs::{
    ReviewGateFailure, ReviewGateVerdict, WfAttemptStatus, WfCallStatus, WfRunStatus, WorkflowRuns,
    pause_reason,
};
use steve_store::{
    NewBot, NewItem, NewProject, NewSession, NoPublisher, Session, Store, StoreError, UserRows, WS,
    WorkspaceSettings,
};
use steve_types::{DecisionPrompt, DecisionStatus, ItemPhase, PlacementRejection};
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
        Self::attach(dir, home, store, use_subprocess)
    }

    fn attach(dir: tempfile::TempDir, home: PathBuf, store: Store, use_subprocess: bool) -> Self {
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
        if use_subprocess {
            core.set_workflow_executor(Arc::new(real_step_runner()));
        } else {
            core.set_workflow_executor(Arc::new(InProcessJsExecutor));
        }
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
        .core
        .tool_workflow_run(&coord, json!({ "action": "submit", "script": script }))
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
        .core
        .tool_workflow_run(&coord, json!({ "action": "submit", "script": script }))
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
        .core
        .tool_workflow_run(&coord, json!({ "action": "submit", "script": script }))
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
    let slow_item = env.store.items().get(&slow_child).unwrap().unwrap();
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
        .core
        .tool_workflow_run(&coord, json!({ "action": "submit", "script": script }))
        .await
        .unwrap();
    let run_id = snap["run"]["id"].as_str().unwrap().to_owned();

    // Pause and resume via steve_workflow_run.
    let paused = env
        .core
        .tool_workflow_run(&coord, json!({ "action": "pause" }))
        .await
        .unwrap();
    assert_eq!(paused["run"]["status"], "paused");
    let resumed = env
        .core
        .tool_workflow_run(&coord, json!({ "action": "resume" }))
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
    env.core
        .tool_workflow_run(
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
        .core
        .tool_workflow_run(&coord, json!({ "action": "submit", "script": script_v1 }))
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
        .core
        .tool_workflow_run(&coord, json!({ "action": "submit", "script": script_v2 }))
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
    assert_eq!(
        env.store
            .items()
            .get(&child_b)
            .unwrap()
            .unwrap()
            .outcome
            .as_deref(),
        Some("cancelled")
    );

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
    env.core
        .tool_workflow_run(&coord, json!({ "action": "submit", "script": script_v3 }))
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
    env.core
        .tool_workflow_run(&coord, json!({ "action": "submit", "script": script_v4 }))
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
            WorkflowRuns(w).finalize_attempt(&child_e_id, 1, "completed", None, None)?;
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
    env.core
        .tool_workflow_run(&coord, json!({ "action": "submit", "script": script_v5 }))
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
        .core
        .tool_workflow_run(&coord, json!({ "action": "submit", "script": script }))
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
        run.pause_reason_code.as_deref(),
        Some(pause_reason::NON_DETERMINISTIC_REPLAY)
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
        .core
        .tool_workflow_run(&coord, json!({ "action": "submit", "script": script }))
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
        .core
        .tool_workflow_run(&coord, json!({ "action": "submit", "script": script }))
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
        run.pause_reason_code.as_deref(),
        Some(pause_reason::REVIEW_GATE_BLOCKED)
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
        .core
        .tool_workflow_run(&coord, json!({ "action": "submit", "script": script_v1 }))
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
    env.core
        .tool_workflow_run(&coord, json!({ "action": "submit", "script": script_v2 }))
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
        .core
        .tool_workflow_run(&coord, json!({ "action": "submit", "script": script_v1 }))
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
        .core
        .tool_workflow_run(&coord, json!({ "action": "submit", "script": script_v2 }))
        .await
        .unwrap();
    assert_eq!(snap2["run"]["status"], "paused");
    assert_eq!(
        snap2["run"]["pause_reason_code"],
        pause_reason::BUDGET_EXHAUSTED
    );
    let dec_id = snap2["decision_id"].as_str().unwrap().to_owned();
    let dec = env.store.decisions().get(&dec_id).unwrap().unwrap();
    assert_eq!(
        dec.question_template,
        Some(DecisionPrompt::WorkflowBudgetExhausted {
            scope: "cycle_total".into(),
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
        .core
        .tool_workflow_run(&coord, json!({ "action": "submit", "script": script }))
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

    // Scenario 12b: Structured owner takeover validates schema and settles child1 atomically!
    assert!(
        env.core
            .wf_child_owner_deliver(&child1_id, &json!({ "wrong": 1 }), "bad", "owner")
            .is_err()
    );
    let c1_sess = env.child_session(&child1_id);
    env.core
        .wf_child_owner_deliver(
            &child1_id,
            &json!({ "ok": true }),
            "owner takeover",
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

    // Give cancel task a brief moment to settle child2's running turn.
    for _ in 0..50 {
        let c2 = env.store.items().get(&child2_id).unwrap().unwrap();
        if c2.phase == ItemPhase::Closed {
            break;
        }
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
    let c2 = env.store.items().get(&child2_id).unwrap().unwrap();
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
            .core
            .tool_workflow_run(&coord, json!({ "action": "submit", "script": script }))
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

    // Restart #1 (Hub crashed while step 1 was still Dispatched): resume MUST NOT duplicate step 1!
    {
        let store = Store::open(&db_path, NoPublisher).unwrap();
        let env = Env::attach(tempfile::tempdir().unwrap(), home.clone(), store, false);
        let resumed = env.core.resume_running_workflows().await.unwrap();
        assert_eq!(resumed, 1);
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

    // Restart #2 (Hub boots after crash right after step 1 commit): resumes & dispatches step 2 ONCE!
    {
        let store = Store::open(&db_path, NoPublisher).unwrap();
        let env = Env::attach(tempfile::tempdir().unwrap(), home.clone(), store, false);
        assert_eq!(env.core.resume_running_workflows().await.unwrap(), 1);
        assert_eq!(
            env.store
                .read(|r| WorkflowRuns(r).list_attempts_for_run(&run_id))
                .unwrap()
                .len(),
            2,
            "restart after post-commit crash must advance cleanly to step 2"
        );
        // Running recovery again is idempotent (still 2 attempts, 0 duplicates).
        assert_eq!(env.core.resume_running_workflows().await.unwrap(), 1);
        assert_eq!(
            env.store
                .read(|r| WorkflowRuns(r).list_attempts_for_run(&run_id))
                .unwrap()
                .len(),
            2
        );
    }
}

/// Scenario 15 & 16: PR #544 coordinator PlacementBlocked rules (`noExecutor = ask | steve`) and
/// parent goal revision superseding active workflow runs.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn placement_blocked_coordinator_rules_and_goal_revision_superseding() {
    let env = Env::new(false);
    let (parent_id, coord) = env.create_parent(None);

    // 1. Default `noExecutor = ask`: required specialty "quantum" has no worker -> MUST NOT
    //    silently fall back to Steve; marks attempt `blocked` and opens WorkflowPlacementBlocked card.
    let script = r#"
        export default async function () {
            return await agent("quantum work", { specialties: ["quantum"], title: "Quantum Step" });
        }
    "#;
    let snap = env
        .core
        .tool_workflow_run(&coord, json!({ "action": "submit", "script": script }))
        .await
        .unwrap();
    let run_id = snap["run"]["id"].as_str().unwrap().to_owned();
    assert_eq!(snap["run"]["status"], "paused");
    assert_eq!(
        snap["run"]["pause_reason_code"],
        pause_reason::PLACEMENT_BLOCKED
    );
    let dec_id = snap["decision_id"].as_str().unwrap().to_owned();
    let dec = env.store.decisions().get(&dec_id).unwrap().unwrap();
    assert!(matches!(
        dec.question_template,
        Some(DecisionPrompt::WorkflowPlacementBlocked { .. })
    ));

    // Owner chooses `assign_bot` with `coder` -> promotes blocked attempt & dispatches!
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

    // 2. Goal revision supersedes active run (goal_version 1 -> 2) and cancels active child!
    env.core
        .revise_goal(&parent_id, "Completely new v2 goal")
        .unwrap();
    let old_run = env
        .store
        .read(|r| WorkflowRuns(r).get_run(&run_id))
        .unwrap()
        .unwrap();
    assert_eq!(old_run.status, WfRunStatus::Superseded);
    for _ in 0..50 {
        if env.store.items().get(&active_child).unwrap().unwrap().phase == ItemPhase::Closed {
            break;
        }
        tokio::time::sleep(Duration::from_millis(15)).await;
    }
    assert_eq!(
        env.store
            .items()
            .get(&active_child)
            .unwrap()
            .unwrap()
            .outcome
            .as_deref(),
        Some("cancelled")
    );

    // 3. Workspace setting `noExecutor = steve`: when no worker matches, Steve (main bot) executes directly!
    env.store
        .write(|w| WorkspaceSettings(w).set(WS, SETTING_NO_EXECUTOR, "steve", steve_store::now()))
        .unwrap();
    let snap_v2 = env
        .core
        .tool_workflow_run(&coord, json!({ "action": "submit", "script": script }))
        .await
        .unwrap();
    assert_eq!(snap_v2["run"]["goal_version"], 2);
    assert_eq!(snap_v2["run"]["status"], "running");
    assert_eq!(snap_v2["attempts"][0]["bot_id"], "steve");
    assert_eq!(snap_v2["attempts"][0]["status"], "dispatched");
}
