//! LocalNode's git ([`WorkspaceExecutor::git_facts`], [`WorkspaceExecutor::git_apply`]) with real
//! repositories: facts, and the push as the Hub's own did it (the origin checked, a throwaway
//! repository, the credential as a header that is in no file, journal or argv, the deadline, the
//! test guard), and a Node that dies while a push runs (unknown, never pushed again).

use std::collections::HashMap;
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use steve_acp::AcpConfig;
use steve_node::{LocalNode, NodeConfig, Root, Storage};
use steve_node_api::testkit::build::cmd;
use steve_node_api::testkit::{FakeSecrets, RecordingSink};
use steve_node_api::*;

const BRANCH: &str = "steve/st-1";
const TOKEN: &str = "ghs-T0ken-value-7731";
/// What the Hub sends as the header's value for [`TOKEN`].
const HEADER_VALUE: &str = "basic eC1hY2Nlc3MtdG9rZW46Z2hzLVQwa2VuLXZhbHVlLTc3MzE=";

struct T {
    _dir: Option<tempfile::TempDir>,
    root: PathBuf,
}

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

impl T {
    fn new() -> T {
        let dir = tempfile::tempdir().unwrap();
        let mut t = T::at(dir.path());
        t._dir = Some(dir);
        t
    }

    /// `work/proj`: a repository whose origin is github.com/o/r, on `BRANCH` one commit past
    /// `main`; `remotes/o/r.git`: the bare repository pushes go to, with `main`.
    fn at(root: &Path) -> T {
        let proj = root.join("work/proj");
        if !proj.exists() {
            std::fs::create_dir_all(&proj).unwrap();
            sh(&proj, &["init", "-q", "-b", "main"]);
            std::fs::write(proj.join("a.txt"), "a\n").unwrap();
            sh(&proj, &["add", "."]);
            sh(&proj, &["commit", "-qm", "a"]);
            sh(
                &proj,
                &["remote", "add", "origin", "https://github.com/o/r.git"],
            );
            let bare = root.join("remotes/o/r.git");
            std::fs::create_dir_all(&bare).unwrap();
            sh(&bare, &["init", "-q", "--bare", "-b", "main"]);
            sh(&proj, &["push", "-q", bare.to_str().unwrap(), "main:main"]);
            sh(&proj, &["checkout", "-qb", BRANCH]);
            std::fs::write(proj.join("b.txt"), "b\n").unwrap();
            sh(&proj, &["add", "."]);
            sh(&proj, &["commit", "-qm", "b"]);
        }
        T {
            root: root.to_path_buf(),
            _dir: None,
        }
    }

    fn proj(&self) -> PathBuf {
        self.root.join("work/proj")
    }

    fn bare(&self) -> PathBuf {
        self.root.join("remotes/o/r.git")
    }

    fn node(&self) -> LocalNode {
        let root = self.root.clone();
        let n = LocalNode::open(NodeConfig {
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
                access: RootAccess::ReadWrite,
            }],
            capacity: Capacity {
                sessions: 0,
                jobs: 0,
                execs: 4,
            },
            storage: Storage::default(),
            jobs: Default::default(),
        })
        .unwrap();
        n.connect(Arc::new(RecordingSink::default()), None);
        n
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

const RESOURCE: &str = "item:i1";

async fn workspace(n: &LocalNode) -> WorkspaceRef {
    n.prepare_workspace(now_cmd(
        "op_prep",
        Prepare {
            resource: RESOURCE.into(),
            kind: PrepareKind::Existing {
                dir: RootPath {
                    root: RootId::new("work").unwrap(),
                    rel: RelPath::new("proj").unwrap(),
                },
            },
        },
    ))
    .await
    .unwrap();
    match finished(n, "op_prep").await {
        Observed::Finished {
            result: Some(OperationResult::Workspace(v)),
            ..
        } => v.workspace,
        other => panic!("prepare: {other:?}"),
    }
}

fn query(w: &WorkspaceRef) -> GitQuery {
    GitQuery {
        workspace: w.clone(),
        refs: vec![],
        ranges: vec![],
        fetch: false,
        reads: vec![],
        heads_only: false,
    }
}

/// A push of `BRANCH` at what it is now to `url`, against the facts as they are now.
async fn push(n: &LocalNode, w: &WorkspaceRef, url: &str, auth: bool) -> GitApply {
    let f = n
        .git_facts(GitQuery {
            refs: vec![format!("refs/heads/{BRANCH}")],
            ..query(w)
        })
        .await
        .unwrap();
    GitApply {
        workspace: w.clone(),
        expected: f.revision,
        expected_head: f.head,
        action: GitAction::Push {
            remote: "origin".into(),
            branch: BRANCH.into(),
            expected_remote: None,
            url: url.into(),
            origin_must_be: Some("o/r".into()),
            expected_local: f.refs[0].1.clone().unwrap(),
            auth: auth.then(|| SecretRef {
                id: "github".into(),
                how: SecretDelivery::Header {
                    name: "AUTHORIZATION".into(),
                },
            }),
        },
    }
}

fn remote_branch(t: &T) -> Option<String> {
    let o = std::process::Command::new("git")
        .arg("-C")
        .arg(t.bare())
        .args([
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{BRANCH}"),
        ])
        .output()
        .unwrap();
    o.status
        .success()
        .then(|| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// Every byte under `dir`.
fn all_bytes(dir: &Path) -> Vec<u8> {
    let mut out = vec![];
    if let Ok(rd) = std::fs::read_dir(dir) {
        for e in rd.flatten() {
            let p = e.path();
            if p.is_dir() {
                out.extend(all_bytes(&p));
            } else if let Ok(b) = std::fs::read(&p) {
                out.extend(b);
            }
        }
    }
    out
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn facts_of_a_workspace() {
    let t = T::new();
    let n = t.node();
    let w = workspace(&n).await;
    let main = sh(&t.proj(), &["rev-parse", "main"]);
    let tip = sh(&t.proj(), &["rev-parse", "HEAD"]);
    let f = n
        .git_facts(GitQuery {
            refs: vec!["main".into(), "refs/heads/nope".into()],
            ranges: vec![("main".into(), BRANCH.into())],
            reads: vec![
                GitRead::IsAncestor {
                    ancestor: BRANCH.into(),
                    of: "main".into(),
                },
                GitRead::IsAncestor {
                    ancestor: "main".into(),
                    of: BRANCH.into(),
                },
                GitRead::RemoteUrl {
                    remote: "origin".into(),
                },
                GitRead::RemoteUrl {
                    remote: "upstream".into(),
                },
                GitRead::Status,
            ],
            ..query(&w)
        })
        .await
        .unwrap();
    assert_eq!(f.head, tip);
    assert_eq!(f.branch.as_deref(), Some(BRANCH));
    assert!(!f.dirty);
    assert_eq!(
        f.refs,
        vec![
            ("main".into(), Some(main.clone())),
            ("refs/heads/nope".into(), None)
        ]
    );
    let r = &f.ranges[0];
    assert_eq!((r.ahead, r.behind), (1, 0));
    assert_eq!(r.merge_base.as_deref(), Some(main.as_str()));
    assert!(r.overlap.is_empty() && r.conflicts.is_empty());
    let answers: Vec<_> = f.reads.iter().map(|r| r.answer.clone()).collect();
    assert_eq!(
        answers,
        vec![
            GitAnswer::IsAncestor { yes: false },
            GitAnswer::IsAncestor { yes: true },
            GitAnswer::RemoteUrl {
                url: Some("https://github.com/o/r.git".into())
            },
            GitAnswer::RemoteUrl { url: None },
            GitAnswer::Status { entries: vec![] },
        ]
    );

    // a conflicting change on main: overlap and conflicts; a dirty tree; a new revision
    sh(&t.proj(), &["checkout", "-q", "main"]);
    std::fs::write(t.proj().join("b.txt"), "other\n").unwrap();
    sh(&t.proj(), &["add", "."]);
    sh(&t.proj(), &["commit", "-qm", "c"]);
    sh(&t.proj(), &["checkout", "-q", BRANCH]);
    std::fs::write(t.proj().join("new.txt"), "x").unwrap();
    let g = n
        .git_facts(GitQuery {
            ranges: vec![("main".into(), BRANCH.into())],
            ..query(&w)
        })
        .await
        .unwrap();
    assert_ne!(g.revision, f.revision, "a branch moved");
    assert!(g.dirty);
    // the status left out: not read, the rest the same
    let quick = n
        .git_facts(GitQuery {
            heads_only: true,
            ..query(&w)
        })
        .await
        .unwrap();
    assert!(!quick.dirty);
    assert_eq!(quick.head, g.head);
    assert_ne!(
        quick.revision, g.revision,
        "never equal to one with the tree in it"
    );
    assert!(quick.revision.as_str().starts_with('h'));
    let r = &g.ranges[0];
    assert_eq!((r.ahead, r.behind), (1, 1));
    assert_eq!(r.overlap, vec![RelPath::new("b.txt").unwrap()]);
    assert_eq!(r.conflicts, vec![RelPath::new("b.txt").unwrap()]);
    // nothing but a file changed: the same revision
    std::fs::write(t.proj().join("new.txt"), "y").unwrap();
    assert_eq!(n.git_facts(query(&w)).await.unwrap().revision, g.revision);

    // what it will not read
    let e = n
        .git_facts(GitQuery {
            refs: vec!["--output=/tmp/x".into()],
            ..query(&w)
        })
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Invalid);
    n.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_push_lands_from_a_throwaway_repository() {
    let t = T::new();
    let n = t.node();
    let hub = Arc::new(FakeSecrets::default());
    n.connect_secrets(hub.clone());
    let w = workspace(&n).await;
    // what the workspace's own config would do is not done: a hook, a redirect of the target
    let hooked = t.root.join("hook-ran");
    let hook = t.proj().join(".git/hooks/pre-push");
    std::fs::write(&hook, format!("#!/bin/sh\ntouch '{}'\n", hooked.display())).unwrap();
    std::process::Command::new("chmod")
        .args(["+x", hook.to_str().unwrap()])
        .status()
        .unwrap();
    let elsewhere = t.root.join("elsewhere.git");
    sh(
        &t.proj(),
        &["init", "-q", "--bare", elsewhere.to_str().unwrap()],
    );
    sh(
        &t.proj(),
        &[
            "config",
            &format!("url.{}.insteadOf", elsewhere.display()),
            t.bare().to_str().unwrap(),
        ],
    );
    let config_before = std::fs::read(t.proj().join(".git/config")).unwrap();
    let a = push(&n, &w, t.bare().to_str().unwrap(), true).await;
    hub.give("op_push1", "github", HEADER_VALUE.as_bytes());
    let c = now_cmd("op_push1", a);
    n.git_apply(c.clone()).await.unwrap();
    let s = finished(&n, "op_push1").await;
    assert!(
        matches!(
            s,
            Observed::Finished {
                outcome: Outcome::Succeeded,
                result: Some(OperationResult::Git(_)),
                ..
            }
        ),
        "{s:?}"
    );
    let tip = sh(&t.proj(), &["rev-parse", BRANCH]);
    assert_eq!(remote_branch(&t).as_deref(), Some(tip.as_str()));
    assert!(!hooked.exists(), "the workspace's hook did not run");
    assert_eq!(
        sh(&elsewhere, &["for-each-ref"]),
        "",
        "insteadOf in the workspace did not redirect it"
    );
    assert_eq!(hub.taken().len(), 1, "the credential was taken once");
    assert_eq!(
        std::fs::read(t.proj().join(".git/config")).unwrap(),
        config_before,
        "the workspace's config is as it was"
    );
    assert!(
        !t.root.join("node/push").exists()
            || std::fs::read_dir(t.root.join("node/push")).unwrap().count() == 0,
        "the throwaway repository is gone"
    );
    // the same command again: its receipt, nothing taken or pushed again
    assert!(n.git_apply(c).await.unwrap().state.is_finished());
    assert_eq!(hub.taken().len(), 1);

    // a lease that does not hold: git refuses, nothing changes there
    std::fs::write(t.proj().join("c.txt"), "c\n").unwrap();
    sh(&t.proj(), &["add", "."]);
    sh(&t.proj(), &["commit", "-qm", "c"]);
    let mut b = push(&n, &w, t.bare().to_str().unwrap(), false).await;
    if let GitAction::Push {
        expected_remote, ..
    } = &mut b.action
    {
        *expected_remote = Some("0".repeat(40));
    }
    n.git_apply(now_cmd("op_push2", b)).await.unwrap();
    let s = finished(&n, "op_push2").await;
    let Observed::Finished {
        outcome: Outcome::Failed,
        error: Some(e),
        ..
    } = s
    else {
        panic!("{s:?}")
    };
    assert!(e.safe_detail.starts_with("git push:"), "{e}");
    assert_eq!(remote_branch(&t).as_deref(), Some(tip.as_str()));
    n.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_push_whose_preconditions_do_not_hold_is_refused_before_it_is_taken() {
    let t = T::new();
    let n = t.node();
    let w = workspace(&n).await;
    let url = t.bare().to_str().unwrap().to_string();

    // the workspace's origin is another repository
    let mut a = push(&n, &w, &url, false).await;
    if let GitAction::Push { origin_must_be, .. } = &mut a.action {
        *origin_must_be = Some("o/other".into());
    }
    let e = n.git_apply(now_cmd("op_p1", a)).await.unwrap_err();
    assert_eq!(
        (e.code, e.safe_detail.as_str()),
        (ErrorCode::Denied, steve_node::git::WRONG_REMOTE)
    );

    // the branch moved since the Hub looked
    let a = push(&n, &w, &url, false).await;
    std::fs::write(t.proj().join("c.txt"), "c\n").unwrap();
    sh(&t.proj(), &["add", "."]);
    sh(&t.proj(), &["commit", "-qm", "c"]);
    let e = n.git_apply(now_cmd("op_p2", a.clone())).await.unwrap_err();
    assert_eq!(
        (e.code, e.safe_detail.as_str()),
        (ErrorCode::Stale, steve_node::git::BRANCH_MOVED)
    );
    // HEAD moved off it: stale as well
    let mut a = push(&n, &w, &url, false).await;
    sh(&t.proj(), &["checkout", "-q", "main"]);
    let e = n.git_apply(now_cmd("op_p3", a.clone())).await.unwrap_err();
    assert_eq!(e.code, ErrorCode::Stale);
    sh(&t.proj(), &["checkout", "-q", BRANCH]);
    // the branch is gone
    a = push(&n, &w, &url, false).await;
    sh(&t.proj(), &["checkout", "-q", "main"]);
    sh(&t.proj(), &["branch", "-qD", BRANCH]);
    let e = n.git_apply(now_cmd("op_p4", a)).await.unwrap_err();
    assert_eq!(
        (e.code, e.safe_detail.as_str()),
        (ErrorCode::Stale, steve_node::git::NO_BRANCH)
    );
    assert_eq!(remote_branch(&t), None, "nothing pushed");

    // fields that are not a push's, and what this Node does not apply
    let f = n.git_facts(query(&w)).await.unwrap();
    let base = |action| GitApply {
        workspace: w.clone(),
        expected: f.revision.clone(),
        expected_head: f.head.clone(),
        action,
    };
    let shaped = |branch: &str, url: &str, how: SecretDelivery| GitAction::Push {
        remote: "origin".into(),
        branch: branch.into(),
        expected_remote: None,
        url: url.into(),
        origin_must_be: Some("o/r".into()),
        expected_local: f.head.clone(),
        auth: Some(SecretRef {
            id: "github".into(),
            how,
        }),
    };
    let header = || SecretDelivery::Header {
        name: "AUTHORIZATION".into(),
    };
    for (i, action) in [
        shaped("a b", &url, header()),
        shaped("main", "--upload-pack=x", header()),
        shaped("main", &url, SecretDelivery::Env { name: "T".into() }),
        shaped(
            "main",
            &url,
            SecretDelivery::Header {
                name: "X: y\nZ".into(),
            },
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let e = n
            .git_apply(now_cmd(&format!("op_bad{i}"), base(action)))
            .await
            .unwrap_err();
        assert_eq!(e.code, ErrorCode::Invalid, "{i}: {e}");
    }
    let e = n
        .git_apply(now_cmd(
            "op_merge",
            base(GitAction::Merge {
                commit: "main".into(),
                message: None,
                back_out_on_conflict: true,
            }),
        ))
        .await
        .unwrap_err();
    assert_eq!(e.code, ErrorCode::Unsupported);
    n.shutdown().await;
}

/// A server on loopback that takes git's first request, keeps its headers, and never answers.
struct Hanging {
    port: u16,
    requests: Arc<Mutex<Vec<String>>>,
}

fn hanging() -> Hanging {
    let l = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = l.local_addr().unwrap().port();
    let requests = Arc::new(Mutex::new(vec![]));
    let seen = requests.clone();
    std::thread::spawn(move || {
        for s in l.incoming().flatten() {
            let seen = seen.clone();
            std::thread::spawn(move || {
                let mut s = s;
                let mut buf = vec![];
                let mut b = [0u8; 1024];
                while !contains(&buf, b"\r\n\r\n") {
                    match s.read(&mut b) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => buf.extend_from_slice(&b[..n]),
                    }
                }
                seen.lock()
                    .unwrap()
                    .push(String::from_utf8_lossy(&buf).into_owned());
                // held open, never answered, until the client goes
                let _ = s.read(&mut b);
                let _ = s.write_all(b"");
            });
        }
    });
    Hanging { port, requests }
}

/// Every running process's command line.
fn all_argv() -> String {
    let o = std::process::Command::new("ps")
        .args(["-axww", "-o", "command="])
        .output()
        .unwrap();
    String::from_utf8_lossy(&o.stdout).into_owned()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_running_push_keeps_its_credential_private_and_stops_with_the_node() {
    let t = T::new();
    let n = t.node();
    let hub = Arc::new(FakeSecrets::default());
    n.connect_secrets(hub.clone());
    let w = workspace(&n).await;
    let server = hanging();
    let url = format!("http://127.0.0.1:{}/o/r.git", server.port);
    let a = push(&n, &w, &url, true).await;
    hub.give("op_hang", "github", HEADER_VALUE.as_bytes());
    n.git_apply(now_cmd("op_hang", a)).await.unwrap();
    // while git waits on the server: the header went to it, and no command line has the token
    // Use the ordinary push budget here: the short deadline test below may legitimately spend
    // its budget checking the workspace and preparing the throwaway repository.
    let until = Instant::now() + Duration::from_secs(30);
    while server.requests.lock().unwrap().is_empty() {
        let state = n
            .operation(OperationId::new("op_hang").unwrap())
            .await
            .unwrap()
            .state;
        assert!(
            !state.is_finished() && Instant::now() < until,
            "git never reached the server: {state:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let req = server.requests.lock().unwrap()[0].clone();
    assert!(
        req.to_ascii_lowercase()
            .contains(&format!("authorization: {HEADER_VALUE}").to_ascii_lowercase()),
        "{req}"
    );
    let argv = all_argv();
    assert!(argv.contains(&url), "the push is running: {argv}");
    for leak in [TOKEN, HEADER_VALUE, "eC1hY2Nlc3MtdG9rZW46"] {
        assert!(!argv.contains(leak), "on a command line: {leak}");
    }
    n.shutdown().await;
    let s = finished(&n, "op_hang").await;
    assert!(
        matches!(
            s,
            Observed::Finished {
                outcome: Outcome::Interrupted,
                ..
            }
        ),
        "{s:?}"
    );
    assert!(!all_argv().contains(&url), "the push must be gone");
    assert_eq!(
        std::fs::read_dir(t.root.join("node/push")).unwrap().count(),
        0
    );
    // not in the workspace's config, the journal or anything else the Node keeps
    let mut kept = all_bytes(&t.root.join("node"));
    kept.extend(all_bytes(&t.proj().join(".git")));
    for leak in [TOKEN, HEADER_VALUE, "eC1hY2Nlc3MtdG9rZW46"] {
        assert!(!contains(&kept, leak.as_bytes()), "kept: {leak}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_hanging_push_exhausts_its_budget_and_leaves_no_process_or_credential() {
    let t = T::new();
    let n = t.node();
    n.set_push_timeout(Duration::from_secs(2));
    let hub = Arc::new(FakeSecrets::default());
    n.connect_secrets(hub.clone());
    let w = workspace(&n).await;
    let server = hanging();
    let url = format!("http://127.0.0.1:{}/o/r.git", server.port);
    let a = push(&n, &w, &url, true).await;
    hub.give("op_deadline", "github", HEADER_VALUE.as_bytes());
    let t0 = Instant::now();
    n.git_apply(now_cmd("op_deadline", a)).await.unwrap();
    // The deadline includes preparation. Whether git reaches the server before it expires
    // depends on machine load; either way it must time out and leave nothing running.
    let s = finished(&n, "op_deadline").await;
    assert!(
        matches!(
            s,
            Observed::Finished {
                outcome: Outcome::TimedOut,
                error: Some(ApiError {
                    code: ErrorCode::OutcomeUnknown,
                    ..
                }),
                ..
            }
        ),
        "{s:?}"
    );
    assert!(t0.elapsed() < Duration::from_secs(15), "{:?}", t0.elapsed());
    assert!(!all_argv().contains(&url), "the push must be gone");
    assert_eq!(
        std::fs::read_dir(t.root.join("node/push")).unwrap().count(),
        0
    );
    n.shutdown().await;
    let mut kept = all_bytes(&t.root.join("node"));
    kept.extend(all_bytes(&t.proj().join(".git")));
    for leak in [TOKEN, HEADER_VALUE, "eC1hY2Nlc3MtdG9rZW46"] {
        assert!(!contains(&kept, leak.as_bytes()), "kept: {leak}");
    }
}

/// Runs in a child with `STEVE_FORBID_REAL_HOSTS=1` (see the test below).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn child_forbidden_push() {
    if std::env::var("STEVE_FORBID_REAL_HOSTS").as_deref() != Ok("1") {
        return;
    }
    let t = T::new();
    let n = t.node();
    let w = workspace(&n).await;
    let a = push(&n, &w, "https://github.com/o/r.git", false).await;
    let e = n.git_apply(now_cmd("op_far", a)).await.unwrap_err();
    assert_eq!(
        (e.code, e.safe_detail.as_str()),
        (ErrorCode::Denied, steve_node::git::HOST_FORBIDDEN)
    );
    // a local path and loopback stay allowed
    let a = push(&n, &w, t.bare().to_str().unwrap(), false).await;
    n.git_apply(now_cmd("op_near", a)).await.unwrap();
    finished(&n, "op_near").await;
    assert!(remote_branch(&t).is_some());
    n.shutdown().await;
}

#[test]
fn the_test_guard_refuses_a_real_host() {
    let out = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "child_forbidden_push", "--nocapture"])
        .env("STEVE_FORBID_REAL_HOSTS", "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        out.status.success() && stdout.contains("1 passed"),
        "{stdout}\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

// ── a real crash: the Node is a child process killed with SIGKILL while git pushes ──

/// Runs in a child process (see the test below).
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn child_push_process() {
    let (Ok(root), Ok(port)) = (
        std::env::var("STEVE_PUSH_CHILD_ROOT"),
        std::env::var("STEVE_PUSH_CHILD_PORT"),
    ) else {
        return;
    };
    let t = T::at(Path::new(&root));
    let n = t.node();
    let w = workspace(&n).await;
    let a = push(&n, &w, &format!("http://127.0.0.1:{port}/o/r.git"), false).await;
    let c = now_cmd("op_crash", a);
    std::fs::write(
        Path::new(&root).join("apply.json"),
        serde_json::to_vec(&c).unwrap(),
    )
    .unwrap();
    n.git_apply(c).await.unwrap();
    std::fs::write(Path::new(&root).join("sent"), "").unwrap();
    tokio::time::sleep(Duration::from_secs(120)).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_node_that_dies_while_it_pushes_never_pushes_again() {
    let t = T::new();
    let server = hanging();
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "child_push_process", "--nocapture"])
        .env("STEVE_PUSH_CHILD_ROOT", &t.root)
        .env("STEVE_PUSH_CHILD_PORT", server.port.to_string())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .unwrap();
    let t0 = Instant::now();
    while !(t.root.join("sent").exists() && !server.requests.lock().unwrap().is_empty()) {
        assert!(
            t0.elapsed() < Duration::from_secs(30),
            "the child never pushed"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    child.kill().unwrap();
    child.wait().unwrap();

    let n = t.node();
    let op = OperationId::new("op_crash").unwrap();
    assert_eq!(
        n.operation(op.clone()).await.unwrap().state.tag(),
        StateTag::Unknown
    );
    // the same command again: what the Node knows, no second push
    let c: Command<GitApply> =
        serde_json::from_slice(&std::fs::read(t.root.join("apply.json")).unwrap()).unwrap();
    let r = n.git_apply(c).await.unwrap();
    assert_eq!(r.state.tag(), StateTag::Unknown);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert_eq!(server.requests.lock().unwrap().len(), 1, "pushed once");

    // recover stops what is left of it: interrupted, never replayed
    let stored = rusqlite::Connection::open(t.root.join("node/node.sqlite3"))
        .unwrap()
        .query_row("SELECT digest FROM ops WHERE id='op_crash'", [], |r| {
            r.get::<_, String>(0)
        })
        .unwrap();
    n.recover(now_cmd(
        "op_recover",
        Recover {
            target: op.clone(),
            expected: Digest::new(stored).unwrap(),
            reason: "the node died".into(),
        },
    ))
    .await
    .unwrap();
    finished(&n, "op_recover").await;
    assert!(matches!(
        n.operation(op).await.unwrap().state,
        Observed::Finished {
            outcome: Outcome::Interrupted,
            ..
        }
    ));
    assert_eq!(server.requests.lock().unwrap().len(), 1);
    n.shutdown().await;
}
