//! Running a merge-queue batch (S3) against a Node that does its git for real, with the faults a
//! Node and a host can give: a conflict backed out, the target moving under the push, a failing
//! check with its tail, a push whose end is unknown (reconciled, never pushed again blind), a
//! numbered file clashing.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

use async_trait::async_trait;

use crate::mq::exec::{Checked, Member, Merged, MqNode, Pushed, Record, Staging, Step, run_batch};
use crate::mq::policy::Policy;
use crate::mq::queue::Outcome;

fn g(dir: &Path, args: &[&str]) -> Result<String, String> {
    let o = Command::new("git")
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
    if o.status.success() {
        Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&o.stderr).trim().to_string())
    }
}

fn ok(dir: &Path, args: &[&str]) -> String {
    g(dir, args).unwrap_or_else(|e| panic!("git {args:?}: {e}"))
}

fn commit(dir: &Path, file: &str, body: &str) -> String {
    let p = dir.join(file);
    std::fs::create_dir_all(p.parent().unwrap()).unwrap();
    std::fs::write(p, body).unwrap();
    ok(dir, &["add", "-A"]);
    ok(dir, &["commit", "-qm", file]);
    ok(dir, &["rev-parse", "HEAD"])
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Fault {
    None,
    /// The push lands, its answer is lost.
    LostAfter,
    /// The push never left, its answer is lost.
    LostBefore,
    /// The push's answer is lost and the target cannot be read.
    Blind,
    /// Checks are killed.
    Killed,
}

/// A Node doing the staging's git for real: `upstream` the host, `source` the user's repository,
/// `staging` the Node's own copy. Every operation answers the same when asked again by its id.
pub(super) struct GitNode {
    _t: Option<tempfile::TempDir>,
    upstream: PathBuf,
    source: PathBuf,
    staging: PathBuf,
    other: PathBuf,
    pub(super) fault: Mutex<Fault>,
    /// Someone else pushes to the target before each of our next `n` pushes.
    moves: AtomicUsize,
    pub(super) pushes: AtomicUsize,
    pub(super) checks: AtomicUsize,
    done: Mutex<HashMap<String, String>>,
}

impl GitNode {
    fn new() -> GitNode {
        let t = tempfile::tempdir().unwrap();
        let root = t.path().to_path_buf();
        let upstream = root.join("up.git");
        std::fs::create_dir_all(&upstream).unwrap();
        ok(&upstream, &["init", "-q", "--bare", "-b", "main"]);
        let source = root.join("src");
        std::fs::create_dir_all(&source).unwrap();
        ok(&source, &["init", "-q", "-b", "main"]);
        commit(&source, "a.txt", "one\n");
        commit(&source, "migrations/057_x.sql", "x\n");
        ok(
            &source,
            &["remote", "add", "origin", upstream.to_str().unwrap()],
        );
        ok(&source, &["push", "-q", "origin", "main"]);
        let mut n = GitNode::over(&root, &source, &upstream);
        n._t = Some(t);
        n
    }

    /// A Node staging the repository `source` (whose `origin` is `upstream`), its files under `root`.
    pub(super) fn over(root: &Path, source: &Path, upstream: &Path) -> GitNode {
        let staging = root.join("staging");
        ok(root, &["clone", "-q", source.to_str().unwrap(), staging.to_str().unwrap()]);
        for (k, v) in [("user.name", "steve"), ("user.email", "steve@example.invalid")] {
            ok(&staging, &["config", k, v]);
        }
        let other = root.join("other");
        ok(root, &["clone", "-q", upstream.to_str().unwrap(), other.to_str().unwrap()]);
        for (k, v) in [("user.name", "o"), ("user.email", "o@example.invalid")] {
            ok(&other, &["config", k, v]);
        }
        GitNode {
            _t: None,
            upstream: upstream.to_path_buf(),
            source: source.to_path_buf(),
            staging,
            other,
            fault: Mutex::new(Fault::None),
            moves: AtomicUsize::new(0),
            pushes: AtomicUsize::new(0),
            checks: AtomicUsize::new(0),
            done: Mutex::new(HashMap::new()),
        }
    }

    /// A branch of the user's repository from `main` with `file` = `body`: its head.
    fn member(&self, branch: &str, file: &str, body: &str) -> String {
        ok(&self.source, &["checkout", "-qB", branch, "origin/main"]);
        let h = commit(&self.source, file, body);
        ok(&self.source, &["checkout", "-q", "main"]);
        h
    }

    pub(super) fn remote_main(&self) -> String {
        ok(&self.upstream, &["rev-parse", "main"])
    }

    /// Someone else lands a change on the target.
    pub(super) fn someone_pushes(&self, n: usize) {
        ok(&self.other, &["pull", "-q", "--ff-only"]);
        commit(&self.other, &format!("other{n}.txt"), "o\n");
        ok(&self.other, &["push", "-q", "origin", "main"]);
    }

    fn memo(&self, op: &str) -> Option<String> {
        self.done.lock().unwrap().get(op).cloned()
    }

    fn keep(&self, op: &str, v: &str) {
        self.done.lock().unwrap().insert(op.into(), v.into());
    }
}

#[async_trait]
impl MqNode for GitNode {
    async fn stage(&self, _s: &Staging, op: &str) -> Step<String> {
        if let Some(v) = self.memo(op) {
            return Step::Done(v);
        }
        ok(&self.source, &["fetch", "-q", "origin"]);
        let src = self.source.to_str().unwrap();
        ok(
            &self.staging,
            &[
                "fetch",
                "-q",
                src,
                "+refs/remotes/origin/*:refs/remotes/origin/*",
                "+refs/heads/*:refs/source/*",
            ],
        );
        ok(
            &self.staging,
            &["checkout", "-qfB", "main", "refs/remotes/origin/main"],
        );
        ok(&self.staging, &["clean", "-ffdxq"]);
        let base = ok(&self.staging, &["rev-parse", "HEAD"]);
        self.keep(op, &base);
        Step::Done(base)
    }

    async fn merge(&self, _s: &Staging, op: &str, commit: &str, message: &str) -> Merged {
        if let Some(v) = self.memo(op) {
            return Merged::Done(v);
        }
        match g(
            &self.staging,
            &["merge", "--no-ff", "--no-edit", "-m", message, commit],
        ) {
            Ok(_) => {
                let tip = ok(&self.staging, &["rev-parse", "HEAD"]);
                self.keep(op, &tip);
                Merged::Done(tip)
            }
            Err(e) => {
                let files = ok(&self.staging, &["diff", "--name-only", "--diff-filter=U"]);
                let _ = g(&self.staging, &["merge", "--abort"]);
                if files.is_empty() {
                    Merged::Failed(e)
                } else {
                    Merged::Conflict(files.lines().map(String::from).collect())
                }
            }
        }
    }

    async fn changed(&self, _s: &Staging, from: &str, to: &str) -> Step<Vec<String>> {
        match g(&self.staging, &["diff", "--name-only", from, to]) {
            Ok(o) => Step::Done(o.lines().map(String::from).collect()),
            Err(e) => Step::Failed(e),
        }
    }

    async fn present(&self, _s: &Staging, rev: &str, paths: &[String]) -> Step<Vec<bool>> {
        Step::Done(
            paths
                .iter()
                .map(|p| g(&self.staging, &["cat-file", "-e", &format!("{rev}:{p}")]).is_ok())
                .collect(),
        )
    }

    async fn check(&self, _s: &Staging, op: &str, command: &str, _d: Duration) -> Checked {
        if let Some(v) = self.memo(op) {
            return if v.is_empty() {
                Checked::Passed
            } else {
                Checked::Failed(v)
            };
        }
        self.checks.fetch_add(1, Ordering::SeqCst);
        if *self.fault.lock().unwrap() == Fault::Killed {
            return Checked::Unknown("killed".into());
        }
        let o = Command::new("/bin/sh")
            .args(["-c", command])
            .current_dir(&self.staging)
            .output()
            .unwrap();
        let out = format!(
            "{}{}",
            String::from_utf8_lossy(&o.stdout),
            String::from_utf8_lossy(&o.stderr)
        );
        let v = if o.status.success() {
            String::new()
        } else {
            out
        };
        self.keep(op, &v);
        if v.is_empty() {
            Checked::Passed
        } else {
            Checked::Failed(v)
        }
    }

    async fn push(&self, _s: &Staging, op: &str, base: &str, tip: &str) -> Pushed {
        // asked again: what it did; never a second push under one operation
        if let Some(v) = self.memo(op) {
            return match v.as_str() {
                "done" => Pushed::Done,
                "moved" => Pushed::Moved,
                w => Pushed::Unknown(w.into()),
            };
        }
        if self
            .moves
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |n| n.checked_sub(1))
            .is_ok()
        {
            self.someone_pushes(
                self.pushes.load(Ordering::SeqCst) + 100 * self.moves.load(Ordering::SeqCst),
            );
        }
        let fault = *self.fault.lock().unwrap();
        if fault == Fault::LostBefore {
            *self.fault.lock().unwrap() = Fault::None;
            self.keep(op, "lost");
            return Pushed::Unknown("lost".into());
        }
        self.pushes.fetch_add(1, Ordering::SeqCst);
        let lease = format!("--force-with-lease=refs/heads/main:{base}");
        let up = self.upstream.to_str().unwrap();
        let r = g(
            &self.staging,
            &["push", "-q", &lease, up, &format!("{tip}:refs/heads/main")],
        );
        let v = match (r, fault) {
            (Ok(_), Fault::LostAfter | Fault::Blind) => "lost",
            (Ok(_), _) => "done",
            (Err(_), _) => "moved",
        };
        self.keep(op, v);
        match v {
            "done" => Pushed::Done,
            "moved" => Pushed::Moved,
            w => Pushed::Unknown(w.into()),
        }
    }

    async fn remote(&self, _s: &Staging) -> Option<String> {
        if *self.fault.lock().unwrap() == Fault::Blind {
            return None;
        }
        let up = self.upstream.to_str().unwrap();
        g(&self.staging, &["ls-remote", up, "refs/heads/main"])
            .ok()
            .and_then(|o| o.split_whitespace().next().map(String::from))
    }

    async fn contains(&self, _s: &Staging, ancestor: &str, of: &str) -> Option<bool> {
        let up = self.upstream.to_str().unwrap();
        g(
            &self.staging,
            &["fetch", "-q", up, "+refs/heads/*:refs/up/*"],
        )
        .ok()?;
        Some(
            g(
                &self.staging,
                &["merge-base", "--is-ancestor", ancestor, of],
            )
            .is_ok(),
        )
    }
}

fn staging() -> Staging {
    Staging {
        target_id: "mq_t".into(),
        project_id: "p1".into(),
        upstream: "local/up".into(),
        branch: "main".into(),
        keep: vec![],
    }
}

fn m(id: &str, head: &str) -> Member {
    Member {
        id: id.into(),
        head: head.into(),
        label: id.into(),
        verified_base: None,
    }
}

async fn go(
    n: &GitNode,
    batch: u64,
    members: &[Member],
    p: &Policy,
) -> (crate::mq::exec::Ran, Vec<Record>) {
    let seen = Mutex::new(vec![]);
    let r = run_batch(n, &staging(), batch, 0, members, p, true, &|r| {
        seen.lock().unwrap().push(r)
    })
    .await;
    (r, seen.into_inner().unwrap())
}

fn landed(o: &Option<Outcome>) -> String {
    match o {
        Some(Outcome::Landed { commit }) => commit.clone(),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn two_members_land_in_one_merge_each_and_a_conflict_is_backed_out() {
    let n = GitNode::new();
    let a = n.member("a", "b.txt", "b\n");
    let b = n.member("b", "a.txt", "two\n");
    let c = n.member("c", "a.txt", "three\n");
    let (r, rec) = go(
        &n,
        1,
        &[m("A", &a), m("B", &b), m("C", &c)],
        &Policy::default(),
    )
    .await;
    let tip = landed(&r.outcome);
    assert_eq!(n.remote_main(), tip);
    assert_eq!(r.left.len(), 1);
    assert_eq!(r.left[0].0, "C");
    assert!(r.left[0].1.contains("a.txt"), "{}", r.left[0].1);
    // A and B are in, as merge commits on the target: what was accepted, unrewritten
    for h in [&a, &b] {
        assert!(g(&n.upstream, &["merge-base", "--is-ancestor", h, "main"]).is_ok());
    }
    assert!(g(&n.upstream, &["merge-base", "--is-ancestor", &c, "main"]).is_err());
    assert_eq!(n.pushes.load(Ordering::SeqCst), 1);
    assert!(
        rec.iter()
            .any(|r| r.kind == "merge" && r.state == "conflict")
    );
}

#[tokio::test]
async fn a_target_that_moved_is_integrated_again_at_most_twice() {
    let n = GitNode::new();
    let a = n.member("a", "b.txt", "b\n");
    n.moves.store(1, Ordering::SeqCst);
    let (r, _) = go(&n, 1, &[m("A", &a)], &Policy::default()).await;
    landed(&r.outcome);
    assert_eq!(r.moved, 1);
    assert!(g(&n.upstream, &["merge-base", "--is-ancestor", &a, "main"]).is_ok());
    // it keeps moving: after two more integrations it stops, unknown, nothing forced over it
    let n = GitNode::new();
    let a = n.member("a", "b.txt", "b\n");
    n.moves.store(9, Ordering::SeqCst);
    let (r, _) = go(&n, 1, &[m("A", &a)], &Policy::default()).await;
    assert!(
        matches!(&r.outcome, Some(Outcome::Unknown { why }) if why.contains("2")),
        "{:?}",
        r.outcome
    );
    assert_eq!(r.moved, 2);
    assert!(g(&n.upstream, &["merge-base", "--is-ancestor", &a, "main"]).is_err());
}

#[tokio::test]
async fn a_failing_check_fails_with_its_tail_and_a_killed_one_is_unknown() {
    let n = GitNode::new();
    let a = n.member("a", "b.txt", "b\n");
    let p = Policy {
        checks: vec!["echo building; echo broken >&2; exit 3".into()],
        ..Policy::default()
    };
    let (r, _) = go(&n, 1, &[m("A", &a)], &p).await;
    match &r.outcome {
        Some(Outcome::Failed { tail }) => assert!(tail.contains("broken"), "{tail}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(n.pushes.load(Ordering::SeqCst), 0, "nothing pushed");
    *n.fault.lock().unwrap() = Fault::Killed;
    let (r, _) = go(&n, 2, &[m("A", &a)], &p).await;
    assert!(matches!(r.outcome, Some(Outcome::Unknown { .. })));
}

#[tokio::test]
async fn a_verified_head_alone_on_its_base_runs_no_checks_again() {
    let n = GitNode::new();
    let base = n.remote_main();
    let a = n.member("a", "b.txt", "b\n");
    let p = Policy {
        checks: vec!["exit 1".into()],
        ..Policy::default()
    };
    let mut one = m("A", &a);
    one.verified_base = Some(base);
    let (r, _) = go(&n, 1, &[one], &p).await;
    landed(&r.outcome);
    assert_eq!(n.checks.load(Ordering::SeqCst), 0);
    // you said land without checks
    let n2 = GitNode::new();
    let a = n2.member("a", "b.txt", "b\n");
    let r = run_batch(&n2, &staging(), 1, 0, &[m("A", &a)], &p, false, &|_| {}).await;
    landed(&r.outcome);
    assert_eq!(n2.checks.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn a_push_lost_after_it_landed_is_reconciled_and_never_sent_again() {
    let n = GitNode::new();
    let a = n.member("a", "b.txt", "b\n");
    *n.fault.lock().unwrap() = Fault::LostAfter;
    let (r, rec) = go(&n, 1, &[m("A", &a)], &Policy::default()).await;
    let tip = landed(&r.outcome);
    assert_eq!(n.remote_main(), tip);
    assert_eq!(n.pushes.load(Ordering::SeqCst), 1);
    assert!(
        rec.iter()
            .any(|r| r.kind == "reconcile" && r.state == "landed")
    );
    // the same run asked again (a restart): every step answers from before, nothing is pushed
    let (again, _) = go(&n, 1, &[m("A", &a)], &Policy::default()).await;
    assert_eq!(landed(&again.outcome), tip);
    assert_eq!(n.pushes.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_push_lost_before_it_left_is_pushed_once_more_under_the_lease() {
    let n = GitNode::new();
    let base = n.remote_main();
    let a = n.member("a", "b.txt", "b\n");
    *n.fault.lock().unwrap() = Fault::LostBefore;
    let (r, rec) = go(&n, 1, &[m("A", &a)], &Policy::default()).await;
    let tip = landed(&r.outcome);
    assert_ne!(tip, base);
    assert_eq!(n.remote_main(), tip);
    assert_eq!(
        n.pushes.load(Ordering::SeqCst),
        1,
        "the lost one never left"
    );
    assert!(
        rec.iter()
            .any(|r| r.kind == "reconcile" && r.state == "not_pushed")
    );
}

#[tokio::test]
async fn a_push_whose_end_cannot_be_read_is_unknown() {
    let n = GitNode::new();
    let a = n.member("a", "b.txt", "b\n");
    *n.fault.lock().unwrap() = Fault::Blind;
    let (r, _) = go(&n, 1, &[m("A", &a)], &Policy::default()).await;
    assert!(
        matches!(r.outcome, Some(Outcome::Unknown { .. })),
        "{:?}",
        r.outcome
    );
    assert_eq!(n.pushes.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn a_migration_number_taken_on_the_target_is_sent_back_with_the_next_free_one() {
    let n = GitNode::new();
    // the target took 058 meanwhile (another change)
    n.someone_pushes(0);
    ok(&n.other, &["pull", "-q", "--ff-only"]);
    commit(&n.other, "migrations/058_theirs.sql", "t\n");
    ok(&n.other, &["push", "-q", "origin", "main"]);
    // two members: one adds 058 too, the other something else
    let a = n.member("a", "migrations/058_mine.sql", "m\n");
    let b = n.member("b", "c.txt", "c\n");
    let (r, _) = go(&n, 1, &[m("A", &a), m("B", &b)], &Policy::default()).await;
    landed(&r.outcome);
    assert_eq!(r.left.len(), 1);
    assert_eq!(r.left[0].0, "A");
    assert!(r.left[0].1.contains("059"), "{}", r.left[0].1);
}
