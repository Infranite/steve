//! `cmd` flow steps on the Node that holds the project (N0e-E6), for a Hub whose local Node is on
//! (`STEVE_NODE_MODE=local`): the project is found through [`Workspaces`], its directory prepared
//! as a workspace of the project and that directory (`project:<id>:<dir>`, so a project moved to
//! another path is a new one), and the command written to the Node's ledger for the run
//! (`flow_run:<run>`) before it is sent, so it runs at most once. A retry you ask for after a
//! restart cut the step short ([`CmdRequest::rerun`]) is a new command, sent once the lost one is
//! stopped. A Hub with its local Node off has none of this: [`CmdRan::Legacy`], and the step runs
//! on the Hub as before.

use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use rusqlite::OptionalExtension;
use steve_flow::nodes::cmd::{CmdNode, CmdRan, CmdRequest};
use steve_node_api::*;
use steve_store::{Store, StoreError};

use super::workspaces::{At, PlacementError, Subject, Workspaces};

/// What each stream of a command keeps (as the Hub's own `cmd`: 64 KiB).
const OUTPUT_CAP: u64 = 64 * 1024;
/// How long a prepare of an existing directory may take.
const PREPARE_WAIT: Duration = Duration::from_secs(60);
/// Past the command's own deadline, how long its answer may take to come back.
const ANSWER_GRACE: Duration = Duration::from_secs(60);
/// How long a step run again waits for the lost command before it to be stopped.
const LOST_WAIT: Duration = Duration::from_secs(60);
/// The step's command before it was lost and could not be stopped: the new one does not run.
const LOST_RUNS: &str = "上一次的命令结果未知，且没能确认它已停止，这次没有运行";

pub struct FlowCmd {
    store: Store,
    workspaces: Arc<Workspaces>,
}

impl FlowCmd {
    pub fn new(store: Store, workspaces: Arc<Workspaces>) -> Arc<FlowCmd> {
        Arc::new(FlowCmd { store, workspaces })
    }
}

fn wall() -> i64 {
    steve_store::now()
}

/// The id of one step's command: a run and a step (and each rerun you asked for) are one command,
/// asked again never run again.
fn op_of(prefix: &str, req: &CmdRequest, rerun: u32) -> String {
    let key = match rerun {
        0 => format!("{}/{}", req.run_id, req.step),
        n => format!("{}/{}/{n}", req.run_id, req.step),
    };
    derived_id(prefix, &key)
}

/// The workspace resource of `project` at `dir`: the directory is part of it, so a project whose
/// path changed gets a workspace of its new directory, never a conflict with the old one.
pub(super) fn resource_of(project: &str, dir: &RootPath) -> String {
    format!("project:{project}:{}", dir_digest(dir))
}

/// A short digest of a directory, for a workspace resource that changes with it.
pub(super) fn dir_digest(dir: &RootPath) -> String {
    use sha2::Digest as _;
    let h = sha2::Sha256::digest(format!("{}\0{}", dir.root.as_str(), dir.rel.as_str()));
    hex::encode(h)[..16].to_string()
}

pub(super) fn command<B>(op: &str, body: B, within: Duration) -> Result<Command<B>, String> {
    Ok(Command {
        id: OperationId::new(op).map_err(|e| e.to_string())?,
        issued_at: Timestamp(wall()),
        accept_before: Timestamp(wall() + within.as_millis() as i64),
        grant: GrantId::new("hub").map_err(|e| e.to_string())?,
        body,
    })
}

/// An unarchived project of the flow's workspace (on any device).
fn project_in(store: &Store, ws: &str, id: &str) -> Result<bool, StoreError> {
    store.reader(|c| {
        Ok(c.query_row(
            "SELECT 1 FROM projects WHERE id=? AND workspace_id=? AND archived=0",
            [id, ws],
            |_| Ok(()),
        )
        .optional()?
        .is_some())
    })
}

impl FlowCmd {
    /// `op`, an earlier command of the step, is not running: never sent, ended, or (lost) stopped
    /// by a recover now.
    async fn stopped(
        &self,
        runtime: &super::node_runtime::NodeRuntime,
        op: &str,
    ) -> Result<(), String> {
        let until = Instant::now() + LOST_WAIT;
        let mut asked = false;
        loop {
            match runtime.ledger(op) {
                Ok(Some(l)) if l.state == "unknown" => {
                    // asked once: a recover the Node could not finish leaves it as it is
                    if !asked {
                        asked = true;
                        if !runtime.stop_lost(op).map_err(|e| e.to_string())? {
                            return Err(LOST_RUNS.into());
                        }
                    }
                }
                Ok(_) => return Ok(()),
                Err(e) => tracing::warn!("flow cmd {op}: {e}"),
            }
            if Instant::now() >= until {
                return Err(LOST_RUNS.into());
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }

    /// The project directory as a workspace on `node`.
    async fn prepare(
        &self,
        node: &Arc<dyn Node>,
        req: &CmdRequest,
        dir: RootPath,
    ) -> Result<WorkspaceRef, String> {
        prepare_dir(node, resource_of(&req.project_id, &dir), dir).await
    }
}

/// `dir` as the workspace `resource` on `node` (an existing directory, worked in as it is).
pub(super) async fn prepare_dir(
    node: &Arc<dyn Node>,
    resource: String,
    dir: RootPath,
) -> Result<WorkspaceRef, String> {
    {
        // a prepare of the same resource and directory is the workspace there is: a fresh id each
        // time is safe (it changes nothing)
        let c = command(
            &steve_store::uid("op_fprep"),
            Prepare {
                resource,
                kind: PrepareKind::Existing { dir },
            },
            PREPARE_WAIT,
        )?;
        let until = Instant::now() + PREPARE_WAIT;
        let mut r = loop {
            match node.prepare_workspace(c.clone()).await {
                // another run's step is making it right now
                Err(e) if e.code == ErrorCode::Busy && Instant::now() < until => {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                other => break other.map_err(|e| e.to_string())?,
            }
        };
        while !r.state.is_finished() {
            if Instant::now() >= until {
                return Err("the workspace was not ready in time".into());
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
            r = node
                .operation(c.id.clone())
                .await
                .map_err(|e| e.to_string())?;
        }
        match r.state {
            Observed::Finished {
                result: Some(OperationResult::Workspace(v)),
                ..
            } => Ok(v.workspace),
            Observed::Finished { error: Some(e), .. } => Err(e.to_string()),
            other => Err(format!("prepare: {:?}", other.tag())),
        }
    }
}

#[async_trait]
impl CmdNode for FlowCmd {
    async fn run(&self, req: CmdRequest) -> CmdRan {
        match project_in(&self.store, &req.workspace_id, &req.project_id) {
            Ok(true) => {}
            Ok(false) => return CmdRan::NoProject,
            Err(e) => return CmdRan::NotRun(e.to_string()),
        }
        // the local roots follow projects; a project made a moment ago may not be in them yet
        if let Err(e) = self.workspaces.refresh_roots() {
            return CmdRan::NotRun(e.to_string());
        }
        let placed = match self
            .workspaces
            .placement_of(Subject::Project(&req.project_id))
        {
            Ok(p) => p,
            Err(PlacementError::LocalOff) => return CmdRan::Legacy,
            Err(PlacementError::Offline { .. }) => return CmdRan::Offline,
            Err(
                PlacementError::NotFound(_) | PlacementError::NoWorkspace | PlacementError::Outside,
            ) => {
                return CmdRan::NoProject;
            }
            Err(e @ PlacementError::NotOwners { .. }) => return CmdRan::NotRun(e.to_string()),
            Err(PlacementError::Store(e)) => return CmdRan::NotRun(e.to_string()),
        };
        let At::Root(dir) = placed.at.clone() else {
            return CmdRan::NoProject;
        };
        let workspace = match self.prepare(&placed.node, &req, dir).await {
            Ok(w) => w,
            Err(e) => return CmdRan::NotRun(e),
        };
        let deadline = wall() + req.timeout.as_millis() as i64;
        let spec = ExecSpec {
            workspace,
            command: req.command.clone(),
            cwd: None,
            policy: ExecutionPolicy {
                revision: Revision::new("flow-cmd").expect("constant id"),
                permissions: PermissionMode::Ask,
                sandbox: SandboxRequirement::None,
                readable: vec![],
                network: NetworkScope::Unrestricted,
                tools: GrantId::new("hub").expect("constant id"),
            },
            limits: Limits {
                deadline: Timestamp(deadline),
                offline_until: Timestamp(deadline),
                max_output_bytes: OUTPUT_CAP,
                max_tokens: None,
                max_cost: None,
            },
            env: vec![],
            secrets: vec![],
        };
        let op = op_of("op_fcmd", &req, req.rerun);
        let subject = format!("flow_run:{}", req.run_id);
        let runtime = &placed.runtime;
        // run again after a restart: what is left of the lost command is stopped first
        for before in 0..req.rerun {
            if let Err(e) = self.stopped(runtime, &op_of("op_fcmd", &req, before)).await {
                return CmdRan::NotRun(e);
            }
        }
        if let Err(e) = runtime.submit(&op, &subject, "exec", spec) {
            // the same step asked again: wait for the one there is, never a second one
            match runtime.ledger(&op) {
                Ok(Some(l)) if l.subject == subject => {}
                _ => return CmdRan::NotRun(e.to_string()),
            }
        }
        let until = Instant::now() + req.timeout + ANSWER_GRACE;
        loop {
            match runtime.ledger(&op) {
                Ok(Some(l)) if l.is_settled() => return ran(&l),
                Ok(Some(_)) => {}
                Ok(None) => return CmdRan::Unknown,
                // the store is there again later; the command is in the ledger either way
                Err(e) => tracing::warn!("flow cmd {op}: {e}"),
            }
            if Instant::now() >= until {
                return CmdRan::Unknown;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    }
}

/// A settled ledger entry as a step's result.
fn ran(l: &super::node_runtime::LedgerEntry) -> CmdRan {
    if l.state == "rejected" {
        return CmdRan::NotRun(
            l.refusal
                .as_ref()
                .map(|e| e.to_string())
                .unwrap_or_else(|| "refused".into()),
        );
    }
    match l.receipt.as_ref().map(|r| &r.state) {
        Some(Observed::Finished {
            outcome: Outcome::TimedOut,
            ..
        }) => CmdRan::TimedOut,
        Some(Observed::Finished {
            outcome: Outcome::Succeeded,
            result: Some(OperationResult::Exec(r)),
            ..
        }) => CmdRan::Ended {
            exit_code: r.exit_code,
            stdout: r.stdout.clone(),
            stderr: r.stderr.clone(),
        },
        Some(Observed::Finished {
            outcome: Outcome::Failed,
            error: Some(e),
            ..
        }) => CmdRan::NotRun(e.to_string()),
        // unknown, interrupted, cancelled: whether it ran to its end is not known
        _ => CmdRan::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::super::node_runtime::{NodeRuntime, Sink};
    use super::super::node_tools::tests::Ends;
    use super::super::workspaces::LocalFace;
    use super::*;

    struct T {
        root: std::path::PathBuf,
        _tmp: Option<tempfile::TempDir>,
        node: Arc<steve_node::LocalNode>,
        store: Store,
        runtime: Arc<NodeRuntime>,
        cmd: Arc<FlowCmd>,
    }

    fn exec(store: &Store, sql: &str) {
        store
            .transaction::<_, StoreError>(|tx| {
                tx.execute_batch(sql)?;
                Ok(())
            })
            .unwrap();
    }

    /// A Hub with its local Node on (commands allowed) and its coordinator running.
    fn hub() -> T {
        let tmp = tempfile::tempdir().unwrap();
        let mut t = hub_at(tmp.path());
        t._tmp = Some(tmp);
        t
    }

    /// [`hub`] in `root`, as it is there (a Hub started again after a crash).
    fn hub_at(root: &std::path::Path) -> T {
        let dir = root;
        let store = Store::open(dir.join("hub.sqlite3"), steve_store::NoPublisher).unwrap();
        store.ensure_workspace().unwrap();
        let node = Arc::new(
            steve_node::LocalNode::open(steve_node::NodeConfig {
                ceiling: steve_node::PolicyCeiling::unlimited(),
                node: NodeId::new("local").unwrap(),
                home: dir.join("node"),
                acp: steve_acp::AcpConfig {
                    declared: Default::default(),
                    adapters: Default::default(),
                    node: "node".into(),
                    claude_executable: "/usr/bin/false".into(),
                    steve_home: dir.join("steve"),
                    codex_home: dir.join("codex"),
                    user_home: dir.to_path_buf(),
                    version: "test".into(),
                    idle_release: Duration::from_secs(600),
                    retry_delays: vec![],
                },
                roots: vec![],
                capacity: Capacity {
                    sessions: 0,
                    jobs: 0,
                    execs: 4,
                },
                storage: Default::default(),
            })
            .unwrap(),
        );
        let runtime = NodeRuntime::new(store.clone(), node.clone(), Arc::new(Ends));
        node.connect(Arc::new(Sink(Arc::downgrade(&runtime))), None);
        runtime.start();
        let ws = Workspaces::new(
            store.clone(),
            Some(LocalFace {
                node: node.clone(),
                runtime: runtime.clone(),
            }),
            None,
            vec![],
        );
        T {
            cmd: FlowCmd::new(store.clone(), ws),
            root: root.to_path_buf(),
            _tmp: None,
            node,
            store,
            runtime,
        }
    }

    fn project(t: &T, id: &str, device: &str, path: &str) {
        exec(
            &t.store,
            &format!(
                "INSERT INTO projects(id,workspace_id,name,path,kind,device_id,created,updated) VALUES ('{id}','default','{id}','{path}','dir','{device}',1,1)"
            ),
        );
    }

    fn req(project: &str, run: &str, command: &str) -> CmdRequest {
        CmdRequest {
            workspace_id: "default".into(),
            project_id: project.into(),
            run_id: run.into(),
            step: "n".into(),
            rerun: 0,
            command: command.into(),
            timeout: Duration::from_secs(10),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_step_runs_once_on_the_projects_node_through_the_ledger() {
        let t = hub();
        let p = t.root.as_path().join("proj");
        std::fs::create_dir_all(&p).unwrap();
        // made after boot: no restart needed for its root
        project(&t, "p1", "hub", p.to_str().unwrap());
        let count = t.root.as_path().join("count");
        let c = format!(
            "echo ran >> '{}'; pwd; echo bad >&2; exit 2",
            count.display()
        );
        let got = t.cmd.run(req("p1", "r1", &c)).await;
        let real = p.canonicalize().unwrap();
        assert_eq!(
            got,
            CmdRan::Ended {
                exit_code: Some(2),
                stdout: format!("{}\n", real.display()),
                stderr: "bad\n".into(),
            }
        );
        let op = derived_id("op_fcmd", "r1/n");
        let l = t.runtime.ledger(&op).unwrap().unwrap();
        assert_eq!(
            (l.subject.as_str(), l.state.as_str()),
            ("flow_run:r1", "finished")
        );
        // the same step asked again: the answer it had, not a second run
        assert_eq!(t.cmd.run(req("p1", "r1", &c)).await, got);
        tokio::time::sleep(Duration::from_millis(200)).await;
        assert_eq!(std::fs::read_to_string(&count).unwrap(), "ran\n");
        // the deadline
        let mut slow = req("p1", "r2", "sleep 5");
        slow.timeout = Duration::from_millis(300);
        assert_eq!(t.cmd.run(slow).await, CmdRan::TimedOut);
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn elsewhere_nowhere_or_with_the_local_node_off() {
        let t = hub();
        let p = t.root.as_path().join("proj");
        std::fs::create_dir_all(&p).unwrap();
        project(&t, "p1", "hub", p.to_str().unwrap());
        project(&t, "p2", "n_far", "work/repo");
        project(&t, "p3", "hub", p.to_str().unwrap());
        exec(&t.store, "UPDATE projects SET archived=1 WHERE id='p3'");
        assert_eq!(t.cmd.run(req("p2", "r1", "true")).await, CmdRan::Offline);
        assert_eq!(t.cmd.run(req("p3", "r1", "true")).await, CmdRan::NoProject);
        assert_eq!(
            t.cmd.run(req("nope", "r1", "true")).await,
            CmdRan::NoProject
        );
        let mut other = req("p1", "r1", "true");
        other.workspace_id = "elsewhere".into();
        assert_eq!(t.cmd.run(other).await, CmdRan::NoProject);
        // STEVE_NODE_MODE off: the Hub runs it itself, as before
        let off = FlowCmd::new(
            t.store.clone(),
            Workspaces::new(t.store.clone(), None, None, vec![]),
        );
        assert_eq!(off.run(req("p1", "r1", "true")).await, CmdRan::Legacy);
        assert!(
            t.runtime
                .ledger(&derived_id("op_fcmd", "r1/n"))
                .unwrap()
                .is_none()
        );
    }

    /// A project whose path changed (catalog `patch`): its steps run in the new directory, never
    /// "prepared otherwise" against the old one's workspace.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_project_moved_to_another_path_runs_there() {
        let t = hub();
        let (a, b) = (t.root.join("a"), t.root.join("b"));
        std::fs::create_dir_all(&a).unwrap();
        std::fs::create_dir_all(&b).unwrap();
        project(&t, "p1", "hub", a.to_str().unwrap());
        let pwd = |p: &std::path::Path| CmdRan::Ended {
            exit_code: Some(0),
            stdout: format!("{}\n", p.canonicalize().unwrap().display()),
            stderr: String::new(),
        };
        assert_eq!(t.cmd.run(req("p1", "r1", "pwd")).await, pwd(&a));
        exec(
            &t.store,
            &format!("UPDATE projects SET path='{}' WHERE id='p1'", b.display()),
        );
        assert_eq!(t.cmd.run(req("p1", "r2", "pwd")).await, pwd(&b));
        // and back again: the first directory's workspace is the one there is
        exec(
            &t.store,
            &format!("UPDATE projects SET path='{}' WHERE id='p1'", a.display()),
        );
        assert_eq!(t.cmd.run(req("p1", "r3", "pwd")).await, pwd(&a));
    }

    fn alive(pid_file: &std::path::Path) -> bool {
        let pid: i32 = std::fs::read_to_string(pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        unsafe { libc::kill(-pid, 0) == 0 }
    }

    async fn eventually(what: &str, within: Duration, mut f: impl FnMut() -> bool) {
        let t0 = Instant::now();
        while !f() {
            assert!(t0.elapsed() < within, "{what}");
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }

    /// The command of step `step` of run `r1`: its pid in `<step>.pid`, then a long sleep, within
    /// `timeout`.
    fn sleeper(t: &T, step: &str, timeout: Duration) -> CmdRequest {
        let mut r = req(
            "p1",
            "r1",
            &format!(
                "echo ran >> '{root}/{step}.runs'; echo $$ > '{root}/{step}.pid'; exec sleep 120",
                root = t.root.display()
            ),
        );
        r.step = step.into();
        r.timeout = timeout;
        r
    }

    /// Runs in a child process (see `a_command_lost_in_a_crash_is_stopped_and_never_run_again`):
    /// a Hub with two steps' commands running when it is killed.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn child_flow_cmd_crash() {
        let Ok(root) = std::env::var("STEVE_FLOW_CMD_CHILD_ROOT") else {
            return;
        };
        let t = hub_at(std::path::Path::new(&root));
        let p = t.root.join("proj");
        std::fs::create_dir_all(&p).unwrap();
        project(&t, "p1", "hub", p.to_str().unwrap());
        for (step, timeout) in [("a", 3), ("b", 300)] {
            let (cmd, r) = (
                t.cmd.clone(),
                sleeper(&t, step, Duration::from_secs(timeout)),
            );
            tokio::spawn(async move { cmd.run(r).await });
        }
        for step in ["a", "b"] {
            let op = derived_id("op_fcmd", &format!("r1/{step}"));
            eventually("the command started", Duration::from_secs(20), || {
                t.root.join(format!("{step}.pid")).exists()
                    && t.runtime
                        .ledger(&op)
                        .unwrap()
                        .is_some_and(|l| l.state != "pending")
            })
            .await;
        }
        std::fs::write(t.root.join("ready"), "").unwrap();
        tokio::time::sleep(Duration::from_secs(300)).await;
    }

    /// The Hub (and its local Node with it) is killed while two steps' commands run. After the
    /// restart both are unknown and never sent again; the one whose deadline passed is stopped by a
    /// recover on its own, the other when you ask for its step to run again (a new command then);
    /// with no command left open, the workspace can be tidied.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_command_lost_in_a_crash_is_stopped_and_never_run_again() {
        let tmp = tempfile::tempdir().unwrap();
        let mut child = std::process::Command::new(std::env::current_exe().unwrap())
            .args([
                "--exact",
                "wire::flow_cmd::tests::child_flow_cmd_crash",
                "--nocapture",
            ])
            .env("STEVE_FLOW_CMD_CHILD_ROOT", tmp.path())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()
            .unwrap();
        let ready = tmp.path().join("ready");
        eventually("the child never got ready", Duration::from_secs(60), || {
            ready.exists()
        })
        .await;
        child.kill().unwrap();
        child.wait().unwrap();
        let (a_pid, b_pid) = (tmp.path().join("a.pid"), tmp.path().join("b.pid"));
        assert!(
            alive(&a_pid) && alive(&b_pid),
            "the groups outlived the Hub"
        );

        let t = hub_at(tmp.path());
        let (a, b) = (derived_id("op_fcmd", "r1/a"), derived_id("op_fcmd", "r1/b"));
        // `a`: past its deadline (3 s) its group is stopped, and it ends interrupted (unknown to
        // the step), not run again
        eventually(
            "a is stopped at its deadline",
            Duration::from_secs(30),
            || !alive(&a_pid),
        )
        .await;
        eventually("a ends interrupted", Duration::from_secs(10), || {
            t.runtime
                .ledger(&a)
                .unwrap()
                .is_some_and(|l| l.state == "finished" && super::ran(&l) == CmdRan::Unknown)
        })
        .await;
        // `b`: its deadline is far off; the lost command keeps running and stays unknown
        let l = t.runtime.ledger(&b).unwrap().unwrap();
        assert_eq!(l.state, "unknown");
        assert!(alive(&b_pid));
        assert_eq!(
            t.cmd.run(sleeper(&t, "b", Duration::from_secs(300))).await,
            CmdRan::Unknown,
            "asked again, the lost one's answer"
        );
        // the workspace is refused while `b` is open
        let At::Root(dir) = t
            .cmd
            .workspaces
            .placement_of(Subject::Project("p1"))
            .unwrap()
            .at
        else {
            panic!("the project has a root");
        };
        let w = WorkspaceRef {
            node: NodeId::new("local").unwrap(),
            id: WorkspaceId::new(derived_id("w", &resource_of("p1", &dir))).unwrap(),
        };
        let tidy = |rev: Revision| {
            command(
                &steve_store::uid("op_tidy"),
                Tidy {
                    workspace: w.clone(),
                    expected: rev,
                    keep_branch: true,
                },
                Duration::from_secs(60),
            )
            .unwrap()
        };
        let view = t.node.workspace(w.clone()).await.unwrap();
        let e = t
            .node
            .tidy_workspace(tidy(view.revision.clone()))
            .await
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::Busy, "{e}");
        // you ask for `b` to run again: the lost one is stopped first, then it runs anew, once
        let mut again = sleeper(&t, "b", Duration::from_secs(10));
        again.rerun = 1;
        again.command = format!("echo again >> '{}/b.runs'; echo done", tmp.path().display());
        assert_eq!(
            t.cmd.run(again.clone()).await,
            CmdRan::Ended {
                exit_code: Some(0),
                stdout: "done\n".into(),
                stderr: String::new(),
            }
        );
        assert!(!alive(&b_pid));
        assert_eq!(t.runtime.ledger(&b).unwrap().unwrap().state, "finished");
        // asked again: the answer it had
        assert!(matches!(t.cmd.run(again).await, CmdRan::Ended { .. }));
        tokio::time::sleep(Duration::from_millis(200)).await;
        for (step, runs) in [("a", "ran\n"), ("b", "ran\nagain\n")] {
            assert_eq!(
                std::fs::read_to_string(tmp.path().join(format!("{step}.runs"))).unwrap(),
                runs,
                "{step}: never run a second time"
            );
        }
        // nothing open in the workspace any more: tidy is allowed
        let view = t.node.workspace(w.clone()).await.unwrap();
        t.node.tidy_workspace(tidy(view.revision)).await.unwrap();
    }
}
