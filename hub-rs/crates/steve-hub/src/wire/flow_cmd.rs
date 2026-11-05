//! `cmd` flow steps on the Node that holds the project (N0e-E6), for a Hub whose local Node is on
//! (`STEVE_NODE_MODE=local`): the project is found through [`Workspaces`], its directory prepared
//! as the workspace `project:<id>`, and the command written to the Node's ledger for the run
//! (`flow_run:<run>`) before it is sent, so it runs at most once. A Hub with its local Node off has
//! none of this: [`CmdRan::Legacy`], and the step runs on the Hub as before.

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

/// The ids of one step's operations: a run and a step are one command, asked again never re-run.
fn op_of(prefix: &str, req: &CmdRequest) -> String {
    derived_id(prefix, &format!("{}/{}", req.run_id, req.step))
}

fn command<B>(op: &str, body: B, within: Duration) -> Result<Command<B>, String> {
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
    /// The project directory as a workspace on `node`.
    async fn prepare(
        &self,
        node: &Arc<dyn Node>,
        req: &CmdRequest,
        dir: RootPath,
    ) -> Result<WorkspaceRef, String> {
        // a prepare of the same resource and directory is the workspace there is: a fresh id each
        // time is safe (it changes nothing)
        let c = command(
            &steve_store::uid("op_fprep"),
            Prepare {
                resource: format!("project:{}", req.project_id),
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
            Err(PlacementError::NotFound(_) | PlacementError::NoWorkspace) => {
                return CmdRan::NoProject;
            }
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
        let op = op_of("op_fcmd", &req);
        let subject = format!("flow_run:{}", req.run_id);
        let runtime = &placed.runtime;
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
        dir: tempfile::TempDir,
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
        let dir = tempfile::tempdir().unwrap();
        let store = Store::open(dir.path().join("hub.sqlite3"), steve_store::NoPublisher).unwrap();
        store.ensure_workspace().unwrap();
        let node = Arc::new(
            steve_node::LocalNode::open(steve_node::NodeConfig {
                node: NodeId::new("local").unwrap(),
                home: dir.path().join("node"),
                acp: steve_acp::AcpConfig {
                    adapters: Default::default(),
                    node: "node".into(),
                    claude_executable: "/usr/bin/false".into(),
                    steve_home: dir.path().join("steve"),
                    codex_home: dir.path().join("codex"),
                    user_home: dir.path().to_path_buf(),
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
                node,
                runtime: runtime.clone(),
            }),
            None,
            vec![],
        );
        T {
            cmd: FlowCmd::new(store.clone(), ws),
            dir,
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
            command: command.into(),
            timeout: Duration::from_secs(10),
        }
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_step_runs_once_on_the_projects_node_through_the_ledger() {
        let t = hub();
        let p = t.dir.path().join("proj");
        std::fs::create_dir_all(&p).unwrap();
        // made after boot: no restart needed for its root
        project(&t, "p1", "hub", p.to_str().unwrap());
        let count = t.dir.path().join("count");
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
        let p = t.dir.path().join("proj");
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
}
