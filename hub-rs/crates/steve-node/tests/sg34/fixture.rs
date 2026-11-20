use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use steve_acp::AcpConfig;
use steve_node::{LocalNode, NodeConfig, Root, Storage};
use steve_node_api::testkit::{
    RecordingSink,
    build::{choice, cmd, limits, policy},
};
use steve_node_api::*;
use steve_runtime::testkit::{eventually, process::Command as Process};

pub struct Env {
    pub dir: tempfile::TempDir,
    pub work: PathBuf,
    pub sink: Arc<RecordingSink>,
}
impl Env {
    pub fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let work = dir.path().join("work");
        std::fs::create_dir_all(&work).unwrap();
        Self {
            dir,
            work,
            sink: Arc::new(RecordingSink::default()),
        }
    }
    pub fn config(&self, adapter: bool) -> NodeConfig {
        let root = self.dir.path();
        let adapters = if adapter {
            let fake = Path::new(env!("CARGO_MANIFEST_DIR")).join("../steve-acp/tests/fake-acp.js");
            let wrapper = root.join("adapter.sh");
            executable(
                &wrapper,
                &format!(
                    "#!/bin/sh\nunset FAKE_ACP_LOG FAKE_ACP_GRANDCHILD FAKE_ACP_STUBBORN FAKE_ACP_FLOOD_GATE FAKE_ACP_OPEN_DELAY FAKE_ACP_OPEN_PERMISSION\nexec node '{}' \"$@\"\n",
                    fake.display()
                ),
            );
            HashMap::from([("claude-code".into(), wrapper)])
        } else {
            HashMap::new()
        };
        NodeConfig {
            node: NodeId::new("sg34").unwrap(),
            home: root.join("node"),
            roots: vec![Root {
                id: RootId::new("work").unwrap(),
                path: self.work.clone(),
                display: "Fixture".into(),
                access: RootAccess::ReadWrite,
            }],
            capacity: Capacity {
                sessions: 2,
                jobs: 0,
                execs: 2,
            },
            storage: Storage::default(),
            ceiling: steve_node::PolicyCeiling::unlimited(),
            acp: AcpConfig {
                adapters,
                declared: Default::default(),
                node: "node".into(),
                claude_executable: "/usr/bin/false".into(),
                steve_home: root.join("steve"),
                codex_home: root.join("codex"),
                user_home: root.into(),
                version: "test".into(),
            },
        }
    }
    pub fn node(&self, adapter: bool, files: Option<Arc<dyn HubFiles>>) -> LocalNode {
        let n = LocalNode::open(self.config(adapter)).unwrap();
        n.connect(self.sink.clone(), files);
        n
    }
    pub async fn workspace(&self, n: &LocalNode) -> WorkspaceRef {
        n.prepare_workspace(now_cmd(
            "sg34-prepare",
            Prepare {
                resource: "sg34-project".into(),
                kind: PrepareKind::Existing {
                    dir: RootPath {
                        root: RootId::new("work").unwrap(),
                        rel: RelPath::new("").unwrap(),
                    },
                },
            },
        ))
        .await
        .unwrap();
        match finished(n, "sg34-prepare").await {
            Observed::Finished {
                result: Some(OperationResult::Workspace(w)),
                ..
            } => w.workspace,
            other => panic!("prepare: {other:?}"),
        }
    }
    pub async fn session(&self, n: &LocalNode, w: &WorkspaceRef) -> SessionId {
        let session = SessionId::new("sg34-session").unwrap();
        n.open_session(now_cmd(
            "sg34-open",
            OpenSession {
                session: session.clone(),
                workspace: w.clone(),
                choice: choice(),
                policy: policy(),
                resume: None,
                env: Default::default(),
                state: Default::default(),
                artifact_view: None,
                mcp_servers: vec![],
                skills: vec![],
            },
        ))
        .await
        .unwrap();
        assert!(matches!(
            finished(n, "sg34-open").await,
            Observed::Finished {
                outcome: Outcome::Succeeded,
                ..
            }
        ));
        session
    }
}
pub fn wall() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}
pub fn now_cmd<T>(op: &str, body: T) -> Command<T> {
    let mut c = cmd(op, body);
    c.issued_at = Timestamp(wall());
    c.accept_before = Timestamp(wall() + 60_000);
    c
}
pub async fn finished(n: &LocalNode, id: &str) -> Observed {
    let out = std::sync::Mutex::new(None);
    let saved = &out;
    assert!(
        eventually(format!("operation {id} finished"), || async move {
            let s = n
                .operation(OperationId::new(id).unwrap())
                .await
                .unwrap()
                .state;
            if s.is_finished() {
                *saved.lock().unwrap() = Some(s);
                true
            } else {
                false
            }
        })
        .await
    );
    out.into_inner().unwrap().unwrap()
}
pub fn spec(w: &WorkspaceRef, command: &str) -> ExecSpec {
    ExecSpec {
        workspace: w.clone(),
        command: command.into(),
        cwd: None,
        policy: policy(),
        limits: Limits {
            deadline: Timestamp(wall() + 10_000),
            max_output_bytes: 64 * 1024,
            ..limits()
        },
        env: vec![],
        secrets: vec![],
    }
}
pub fn executable(path: &Path, body: &str) {
    use std::os::unix::fs::PermissionsExt;
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(path, body).unwrap();
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o755)).unwrap();
}
pub fn git(dir: &Path, args: &[&str]) -> String {
    let o = Process::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "Fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@users.noreply.github.com")
        .env("GIT_COMMITTER_NAME", "Fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@users.noreply.github.com")
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8(o.stdout).unwrap().trim().into()
}
pub async fn turn(n: &LocalNode, session: &SessionId, w: &WorkspaceRef, id: &str, text: &str) {
    n.start_turn(now_cmd(
        id,
        TurnSpec {
            session: session.clone(),
            turn: TurnId::new(id).unwrap(),
            run: RunSpec {
                attempt: AttemptId::new(id).unwrap(),
                choice: choice(),
                directory: RunDirectory::Workspace {
                    workspace: w.clone(),
                },
                prompt: vec![Part::Text { text: text.into() }],
                policy: policy(),
                limits: Limits {
                    deadline: Timestamp(wall() + 10_000),
                    offline_until: Timestamp(wall() + 10_000),
                    ..limits()
                },
                continuation: None,
            },
        },
    ))
    .await
    .unwrap();
    assert!(matches!(
        finished(n, id).await,
        Observed::Finished {
            outcome: Outcome::Succeeded,
            ..
        }
    ));
}
