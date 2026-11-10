//! The merge queue's staging on a LocalNode (E1) with real repositories: a worktree of the Node's
//! own mirror (never of the user's repository, never borrowing its objects), reset for each batch
//! with build output kept, members merged in with a conflict backed out, the result pushed with a
//! lease.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use steve_acp::AcpConfig;
use steve_node::{LocalNode, NodeConfig, Root, Storage};
use steve_node_api::testkit::RecordingSink;
use steve_node_api::testkit::build::cmd;
use steve_node_api::*;

fn sh(dir: &Path, args: &[&str]) -> String {
    let o = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.invalid")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.invalid")
        .output()
        .unwrap();
    assert!(
        o.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&o.stderr)
    );
    String::from_utf8_lossy(&o.stdout).trim().to_string()
}

fn commit(dir: &Path, file: &str, body: &str, msg: &str) -> String {
    std::fs::write(dir.join(file), body).unwrap();
    sh(dir, &["add", "-A"]);
    sh(dir, &["commit", "-qm", msg]);
    sh(dir, &["rev-parse", "HEAD"])
}

fn wall() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis() as i64
}

fn now_cmd<T>(op: &str, body: T) -> Command<T> {
    let mut c = cmd(op, body);
    c.issued_at = Timestamp(wall());
    c.accept_before = Timestamp(wall() + 60_000);
    c
}

async fn finished(n: &LocalNode, op: &str) -> Observed {
    let t0 = Instant::now();
    loop {
        let r = n.operation(OperationId::new(op).unwrap()).await.unwrap();
        if r.state.is_finished() {
            return r.state;
        }
        assert!(
            t0.elapsed() < Duration::from_secs(30),
            "{op}: {:?}",
            r.state
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

struct T {
    _dir: tempfile::TempDir,
    root: PathBuf,
    node: LocalNode,
}

impl T {
    /// `work/proj`: the user's repository on `main`, its origin `remotes/r.git` (bare); feature
    /// branches `f1` (a new file), `f2` and `f3` (the same line, two ways).
    fn new() -> T {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().to_path_buf();
        let proj = root.join("work/proj");
        std::fs::create_dir_all(&proj).unwrap();
        sh(&proj, &["init", "-q", "-b", "main"]);
        commit(&proj, "a.txt", "one\n", "a");
        let bare = root.join("remotes/r.git");
        std::fs::create_dir_all(&bare).unwrap();
        sh(&bare, &["init", "-q", "--bare", "-b", "main"]);
        sh(&proj, &["remote", "add", "origin", bare.to_str().unwrap()]);
        sh(&proj, &["push", "-q", "origin", "main"]);
        sh(&proj, &["fetch", "-q", "origin"]);
        for (b, f, body) in [
            ("f1", "b.txt", "b\n"),
            ("f2", "a.txt", "two\n"),
            ("f3", "a.txt", "three\n"),
        ] {
            sh(&proj, &["checkout", "-qb", b, "main"]);
            commit(&proj, f, body, b);
        }
        sh(&proj, &["checkout", "-q", "main"]);
        let node = LocalNode::open(NodeConfig {
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
            },
            roots: vec![Root {
                id: RootId::new("work").unwrap(),
                path: root.join("work"),
                display: "~/work".into(),
                access: RootAccess::ReadWrite,
            }],
            capacity: Capacity {
                sessions: 0,
                jobs: 0,
                execs: 2,
            },
            storage: Storage::default(),
        })
        .unwrap();
        node.connect(std::sync::Arc::new(RecordingSink::default()), None);
        T {
            _dir: dir,
            root,
            node,
        }
    }

    fn proj(&self) -> PathBuf {
        self.root.join("work/proj")
    }

    fn sha(&self, rev: &str) -> String {
        sh(&self.proj(), &["rev-parse", rev])
    }

    async fn prepare(&self, op: &str, resource: &str) -> WorkspaceRef {
        self.node
            .prepare_workspace(now_cmd(
                op,
                Prepare {
                    resource: resource.into(),
                    kind: PrepareKind::Staging {
                        source: RootPath {
                            root: RootId::new("work").unwrap(),
                            rel: RelPath::new("proj").unwrap(),
                        },
                        branch: "main".into(),
                    },
                },
            ))
            .await
            .unwrap();
        match finished(&self.node, op).await {
            Observed::Finished {
                result: Some(OperationResult::Workspace(v)),
                ..
            } => v.workspace,
            other => panic!("{other:?}"),
        }
    }

    async fn facts(&self, w: &WorkspaceRef) -> GitFacts {
        self.node
            .git_facts(GitQuery {
                workspace: w.clone(),
                refs: vec!["refs/heads/main".into()],
                ranges: vec![],
                fetch: false,
                reads: vec![],
                heads_only: false,
            })
            .await
            .unwrap()
    }

    /// Applies `action` on the facts as they are now; how it ended.
    async fn apply(&self, op: &str, w: &WorkspaceRef, action: GitAction) -> Observed {
        let f = self.facts(w).await;
        self.node
            .git_apply(now_cmd(
                op,
                GitApply {
                    workspace: w.clone(),
                    expected: f.revision,
                    expected_head: f.head,
                    action,
                },
            ))
            .await
            .unwrap();
        finished(&self.node, op).await
    }
}

fn head_of(o: &Observed) -> String {
    match o {
        Observed::Finished {
            outcome: Outcome::Succeeded,
            result: Some(OperationResult::Git(f)),
            ..
        } => f.head.clone(),
        other => panic!("{other:?}"),
    }
}

fn error_of(o: &Observed) -> ApiError {
    match o {
        Observed::Finished { error: Some(e), .. } => e.clone(),
        other => panic!("{other:?}"),
    }
}

/// The staging directory: the only one under `node/mq`.
fn staging_dir(t: &T) -> PathBuf {
    let mut d: Vec<PathBuf> = std::fs::read_dir(t.root.join("node/mq"))
        .unwrap()
        .flatten()
        .map(|e| e.path())
        .collect();
    assert_eq!(d.len(), 1);
    d.pop().unwrap()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn staging_is_the_nodes_own_mirror_reset_merged_and_pushed() {
    let t = T::new();
    let w = t.prepare("op_stage", "mq:t1").await;
    let st = staging_dir(&t);
    let f = t.facts(&w).await;
    assert_eq!(f.head, t.sha("main"), "from origin/main");
    assert_eq!(f.branch.as_deref(), Some("main"));

    // its objects are the mirror's: no alternates, nothing of the user's repository
    let common = PathBuf::from(sh(
        &st,
        &["rev-parse", "--path-format=absolute", "--git-common-dir"],
    ));
    assert!(
        common.starts_with(t.root.canonicalize().unwrap().join("node/git")),
        "{common:?}"
    );
    assert!(!common.join("objects/info/alternates").exists());
    let user_git = t.proj().join(".git").canonicalize().unwrap();
    let mirror_cfg = std::fs::read_to_string(common.join("config")).unwrap();
    assert!(!mirror_cfg.contains(user_git.to_str().unwrap()));
    // the user's repository knows nothing of it
    assert!(!sh(&t.proj(), &["worktree", "list"]).contains("node/mq"));
    // the same resource again: the same workspace
    assert_eq!(t.prepare("op_stage2", "mq:t1").await, w);

    // build output: a kept directory, a cache directory, and junk
    std::fs::create_dir_all(st.join("keep/x")).unwrap();
    std::fs::write(st.join("keep/x/f"), "k").unwrap();
    std::fs::create_dir_all(st.join("build")).unwrap();
    std::fs::write(
        st.join("build/CACHEDIR.TAG"),
        "Signature: 8a477f597d28d172789f06886806bc55",
    )
    .unwrap();
    std::fs::write(st.join("junk.txt"), "j").unwrap();
    let to = "refs/remotes/origin/main".to_string();
    let base = head_of(
        &t.apply(
            "op_reset",
            &w,
            GitAction::Reset {
                to: to.clone(),
                keep: vec![RelPath::new("keep").unwrap()],
            },
        )
        .await,
    );
    assert_eq!(base, t.sha("main"));
    assert!(st.join("keep/x/f").exists() && st.join("build/CACHEDIR.TAG").exists());
    assert!(!st.join("junk.txt").exists());

    // members: f1 and f2 merge, f3 conflicts with f2 and is backed out
    let merge = |b: &str| GitAction::Merge {
        commit: t.sha(b),
        message: Some(format!("Merge {b}")),
        back_out_on_conflict: true,
    };
    let after1 = head_of(&t.apply("op_m1", &w, merge("f1")).await);
    let after2 = head_of(&t.apply("op_m2", &w, merge("f2")).await);
    assert_ne!(after1, after2);
    let e = error_of(&t.apply("op_m3", &w, merge("f3")).await);
    assert_eq!(
        (e.code, e.safe_detail.as_str()),
        (ErrorCode::Conflict, "a.txt")
    );
    let f = t.facts(&w).await;
    assert_eq!(f.head, after2, "backed out");
    assert!(sh(&st, &["status", "--porcelain", "--untracked-files=no"]).is_empty());
    assert_eq!(
        sh(&st, &["log", "-1", "--format=%P"]).split(' ').count(),
        2,
        "a merge commit"
    );
    assert_eq!(sh(&st, &["log", "-1", "--format=%s"]), "Merge f2");

    // pushed over the base it was integrated on (the lease)
    let push = |lease: &str| GitAction::Push {
        remote: "origin".into(),
        branch: "main".into(),
        expected_remote: Some(lease.into()),
        url: t.root.join("remotes/r.git").display().to_string(),
        origin_must_be: None,
        expected_local: after2.clone(),
        auth: None,
        remote_branch: None,
    };
    let wrong = "0".repeat(40);
    let refused = t.apply("op_p0", &w, push(&wrong)).await;
    assert!(
        matches!(
            refused,
            Observed::Finished {
                outcome: Outcome::Failed,
                ..
            }
        ),
        "{refused:?}"
    );
    let pushed = t.apply("op_p1", &w, push(&base)).await;
    assert!(
        matches!(
            pushed,
            Observed::Finished {
                outcome: Outcome::Succeeded,
                ..
            }
        ),
        "{pushed:?}"
    );
    assert_eq!(
        sh(&t.root.join("remotes/r.git"), &["rev-parse", "main"]),
        after2
    );

    // the next batch: the source fetched, the staging reset on the new tip
    sh(&t.proj(), &["fetch", "-q", "origin"]);
    let next = head_of(
        &t.apply("op_reset2", &w, GitAction::Reset { to, keep: vec![] })
            .await,
    );
    assert_eq!(next, after2);

    // a reset is for a staging only
    let tidy = t
        .node
        .tidy_workspace(now_cmd(
            "op_tidy",
            Tidy {
                workspace: w.clone(),
                expected: t.node.workspace(w.clone()).await.unwrap().revision,
                keep_branch: true,
            },
        ))
        .await
        .unwrap();
    let _ = tidy;
    finished(&t.node, "op_tidy").await;
    assert!(!st.exists(), "tidied: the staging goes, the mirror stays");
    assert!(common.exists());
}
