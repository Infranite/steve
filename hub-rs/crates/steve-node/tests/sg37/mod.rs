use std::{
    collections::HashMap,
    path::{Path, PathBuf},
    sync::Arc,
};
use steve_acp::AcpConfig;
use steve_node::{LocalNode, NodeConfig, PolicyCeiling, Root, Storage};
use steve_node_api::{
    testkit::{RecordingSink, build::cmd},
    *,
};

pub struct Fixture {
    pub temp: tempfile::TempDir,
    pub root: PathBuf,
    pub home: PathBuf,
    pub sink: Arc<RecordingSink>,
}
impl Fixture {
    pub fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("work");
        std::fs::create_dir_all(root.join("proj")).unwrap();
        let home = temp.path().join("node");
        Self {
            temp,
            root,
            home,
            sink: Arc::default(),
        }
    }
    pub fn project(&self) -> PathBuf {
        self.root.join("proj")
    }
    pub fn config(&self) -> NodeConfig {
        NodeConfig {
            node: NodeId::new("hub").unwrap(),
            home: self.home.clone(),
            acp: AcpConfig {
                declared: Default::default(),
                adapters: HashMap::new(),
                node: "node".into(),
                claude_executable: "/usr/bin/false".into(),
                steve_home: self.temp.path().join("steve"),
                codex_home: self.temp.path().join("codex"),
                user_home: self.temp.path().join("owner"),
                version: "test".into(),
            },
            roots: vec![Root {
                id: RootId::new("work").unwrap(),
                path: self.root.clone(),
                display: "temporary sources".into(),
                access: RootAccess::ReadWrite,
            }],
            capacity: Capacity {
                sessions: 4,
                jobs: 0,
                execs: 0,
            },
            storage: Storage::default(),
            ceiling: PolicyCeiling::unlimited(),
        }
    }
    pub fn node(&self) -> LocalNode {
        let n = LocalNode::open(self.config()).unwrap();
        n.connect(self.sink.clone(), None);
        n
    }
    pub fn put(&self, rel: &str, bytes: &[u8]) {
        let path = self.project().join(rel);
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, bytes).unwrap()
    }
    pub fn git(&self, args: &[&str]) -> String {
        git(&self.project(), args)
    }
    pub fn repo(&self) {
        self.git(&["init", "-q", "-b", "main"]);
        self.git(&["config", "core.hooksPath", "/dev/null"]);
        self.put("a.txt", b"original content\n");
        self.git(&["add", "."]);
        self.git(&["commit", "-qm", "initial"]);
    }
    pub fn proc_count(&self) -> i64 {
        rusqlite::Connection::open_with_flags(
            self.home.join("node.sqlite3"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap()
        .query_row("SELECT count(*) FROM procs", [], |r| r.get(0))
        .unwrap()
    }
}
pub fn git(dir: &Path, args: &[&str]) -> String {
    let o = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "fixture")
        .env("GIT_AUTHOR_EMAIL", "fixture@example.invalid")
        .env("GIT_COMMITTER_NAME", "fixture")
        .env("GIT_COMMITTER_EMAIL", "fixture@example.invalid")
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "{args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).trim().to_owned()
}
pub fn at(rel: &str) -> RootPath {
    RootPath {
        root: RootId::new("work").unwrap(),
        rel: RelPath::new(rel).unwrap(),
    }
}
pub fn command<T>(id: &str, body: T) -> Command<T> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64;
    let mut c = cmd(id, body);
    c.issued_at = Timestamp(now);
    c.accept_before = Timestamp(now + 60_000);
    c
}
pub async fn finished(n: &LocalNode, id: &str) -> Observed {
    let id = OperationId::new(id).unwrap();
    steve_runtime::testkit::eventually("SG37 operation completion", || async {
        n.operation(id.clone()).await.unwrap().state.is_finished()
    })
    .await;
    n.operation(id).await.unwrap().state
}
pub fn succeeded(s: &Observed) -> bool {
    matches!(
        s,
        Observed::Finished {
            outcome: Outcome::Succeeded,
            ..
        }
    )
}
pub fn workspace() -> WorkspaceRef {
    steve_node_api::testkit::build::workspace("hub", "sg37-project")
}
pub async fn prepare(n: &LocalNode) {
    n.prepare_workspace(command(
        "sg37_prepare",
        Prepare {
            resource: "sg37-project".into(),
            kind: PrepareKind::Existing { dir: at("proj") },
        },
    ))
    .await
    .unwrap();
    let s = finished(n, "sg37_prepare").await;
    assert!(succeeded(&s), "{s:?}");
}
pub async fn scan(
    n: &LocalNode,
    id: &str,
    rel: &str,
    limits: ScanLimits,
    exclude: Vec<String>,
) -> Vec<ScanPage> {
    n.scan(command(
        id,
        ScanSpec {
            root: at(rel),
            include: vec!["**/*.md".into()],
            exclude,
            limits,
        },
    ))
    .await
    .unwrap();
    let s = finished(n, id).await;
    assert!(succeeded(&s), "{s:?}");
    let mut pages = vec![];
    let mut cursor = None;
    loop {
        let p = n
            .scan_page(
                OperationId::new(id).unwrap(),
                PageRequest { cursor, limit: 1 },
            )
            .await
            .unwrap();
        assert!(p.is_consistent());
        cursor = p.next.clone();
        pages.push(p);
        if cursor.is_none() {
            break;
        }
    }
    assert!(
        pages[..pages.len() - 1]
            .iter()
            .all(|p| p.issues.is_empty() && !p.complete)
    );
    pages
}
pub fn entries(pages: &[ScanPage]) -> Vec<FileInfo> {
    pages.iter().flat_map(|p| p.entries.clone()).collect()
}
