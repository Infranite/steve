//! Merge lanes on real repositories: which target a working copy merges into, and what a request
//! brings that the target or another request has already.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::lane::precheck::{Overlap, Presence, Request, overlap, precheck};
use crate::lane::target::target_of;

fn g(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_AUTHOR_NAME", "t")
        .env("GIT_AUTHOR_EMAIL", "t@example.com")
        .env("GIT_COMMITTER_NAME", "t")
        .env("GIT_COMMITTER_EMAIL", "t@example.com")
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "git {args:?}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

fn write(dir: &Path, f: &str, body: &str) {
    std::fs::write(dir.join(f), body).unwrap();
}

/// `f` gets `body`, committed with `msg`; the new head.
fn commit(dir: &Path, f: &str, body: &str, msg: &str) -> String {
    write(dir, f, body);
    g(dir, &["add", "-A"]);
    g(dir, &["commit", "-q", "-m", msg]);
    g(dir, &["rev-parse", "HEAD"])
}

/// A repository on `main` with one commit, and a bare `origin` it was pushed to.
fn repo(home: &Path) -> PathBuf {
    let r = home.join("r");
    std::fs::create_dir_all(&r).unwrap();
    g(&r, &["init", "-q", "-b", "main"]);
    commit(&r, "a.txt", "one\ntwo\nthree\n", "base");
    let bare = home.join("origin.git");
    g(
        home,
        &[
            "clone",
            "-q",
            "--bare",
            &r.to_string_lossy(),
            &bare.to_string_lossy(),
        ],
    );
    g(&r, &["remote", "add", "origin", &bare.to_string_lossy()]);
    g(&r, &["fetch", "-q", "origin"]);
    r
}

const UP: &str = "refs/remotes/origin/main";

/// Pushes `main` and fetches, as another merge into the target would.
fn land(r: &Path) {
    g(r, &["push", "-q", "origin", "main"]);
    g(r, &["fetch", "-q", "origin"]);
}

#[test]
fn every_spelling_clone_and_worktree_of_a_repository_is_one_target() {
    let t = tempfile::tempdir().unwrap();
    let r = repo(t.path());
    let mine = target_of(&r, None).unwrap();
    assert_eq!(mine.branch, "main");
    assert!(mine.upstream.starts_with("file:") && mine.upstream.ends_with("origin.git"));

    /* a worktree and a second clone of the same origin */
    let wt = t.path().join("wt");
    g(
        &r,
        &["worktree", "add", "-q", "-b", "x", &wt.to_string_lossy()],
    );
    assert_eq!(target_of(&wt, None).unwrap(), mine);
    let other = t.path().join("clone");
    g(
        t.path(),
        &[
            "clone",
            "-q",
            &t.path().join("origin.git").to_string_lossy(),
            &other.to_string_lossy(),
        ],
    );
    assert_eq!(target_of(&other, None).unwrap(), mine);

    /* the URL as configured names it, whatever insteadOf turns it into */
    g(
        &r,
        &["remote", "set-url", "origin", "git@github.com:Acme/App.git"],
    );
    g(
        &other,
        &["remote", "set-url", "origin", "https://github.com/acme/app"],
    );
    let a = target_of(&r, Some("release")).unwrap();
    assert_eq!(a.key(), "github.com/acme/app#release");
    assert_eq!(target_of(&other, Some("release")).unwrap(), a);

    /* no remote: the repository itself, on its main */
    let solo = t.path().join("solo");
    std::fs::create_dir_all(&solo).unwrap();
    g(&solo, &["init", "-q", "-b", "main"]);
    commit(&solo, "a.txt", "x\n", "base");
    let s = target_of(&solo, None).unwrap();
    assert!(
        s.upstream.starts_with("file:") && s.upstream.ends_with("/solo/.git"),
        "{s:?}"
    );
    assert_eq!(s.branch, "main");
}

#[test]
fn a_change_already_on_the_target_is_found_however_it_was_merged() {
    let t = tempfile::tempdir().unwrap();
    let r = repo(t.path());
    let base = g(&r, &["rev-parse", "HEAD"]);

    /* not merged */
    g(&r, &["checkout", "-q", "-b", "feat"]);
    let feat = commit(&r, "b.txt", "feature\n", "feature");
    assert_eq!(precheck(&r, UP, &feat, &[]).presence, Presence::Absent);

    /* merged as is (fast-forward) */
    g(&r, &["checkout", "-q", "main"]);
    g(&r, &["merge", "-q", "--ff-only", "feat"]);
    land(&r);
    let p = precheck(&r, UP, &feat, &[]);
    assert_eq!(
        p.presence,
        Presence::Landed {
            commit: feat.clone()
        }
    );
    assert_eq!(p.target_commit.as_deref(), Some(feat.as_str()));
    assert!(p.checked_at > 0);

    /* rebased: main moved on, the branch's copy differs in id only */
    g(&r, &["checkout", "-q", "-b", "two", &base]);
    let two = commit(&r, "c.txt", "second\n", "second");
    g(&r, &["checkout", "-q", "main"]);
    g(&r, &["cherry-pick", &two]);
    land(&r);
    let copy = g(&r, &["rev-parse", "HEAD"]);
    assert_ne!(copy, two);
    assert_eq!(
        precheck(&r, UP, &two, &[]).presence,
        Presence::Landed { commit: copy }
    );

    /* squashed: two commits arrive as one */
    g(&r, &["checkout", "-q", "-b", "three", &base]);
    commit(&r, "d.txt", "x\n", "d1");
    let three = commit(&r, "d.txt", "x\ny\n", "d2");
    g(&r, &["checkout", "-q", "main"]);
    g(&r, &["merge", "-q", "--squash", "three"]);
    g(&r, &["commit", "-q", "-m", "squash"]);
    land(&r);
    let squash = g(&r, &["rev-parse", "HEAD"]);
    assert_eq!(
        precheck(&r, UP, &three, &[]).presence,
        Presence::Landed { commit: squash }
    );

    /* git can't tell: not "absent" */
    assert!(matches!(
        precheck(&r, UP, "0123456789abcdef0123456789abcdef01234567", &[]).presence,
        Presence::Unknown(_)
    ));
    assert!(matches!(
        precheck(&r, "refs/remotes/origin/nope", &three, &[]).presence,
        Presence::Unknown(_)
    ));
}

#[test]
fn overlapping_requests_are_told_apart_only_on_exact_evidence() {
    let t = tempfile::tempdir().unwrap();
    let r = repo(t.path());
    let base = g(&r, &["rev-parse", "HEAD"]);

    /* the same change on two bases: equal patch ids */
    g(&r, &["checkout", "-q", "-b", "a", &base]);
    let a = commit(&r, "b.txt", "feature\n", "feature");
    g(&r, &["checkout", "-q", "main"]);
    commit(&r, "z.txt", "elsewhere\n", "main moves on");
    land(&r);
    g(&r, &["checkout", "-q", "-b", "a2"]);
    g(&r, &["cherry-pick", &a]);
    let a2 = g(&r, &["rev-parse", "HEAD"]);
    let up = g(&r, &["rev-parse", UP]);
    assert_eq!(overlap(&r, &up, &a2, &a), Some(Overlap::Same));

    /* one builds on the other: the earlier adds nothing on top of the later */
    g(&r, &["checkout", "-q", "-b", "more", &a]);
    let more = commit(&r, "b.txt", "feature\nand more\n", "more");
    assert_eq!(overlap(&r, &up, &a, &more), Some(Overlap::InOther));
    assert_eq!(overlap(&r, &up, &more, &a), Some(Overlap::HasOther));

    /* nothing shared */
    g(&r, &["checkout", "-q", "-b", "c", &base]);
    let c = commit(&r, "c.txt", "other\n", "other");
    assert_eq!(overlap(&r, &up, &c, &a), None);

    /* the ST-36/ST-34 case: the same work redone on an older base, one line differing */
    g(&r, &["checkout", "-q", "-b", "x", &base]);
    let x = commit(
        &r,
        "a.txt",
        "one\ntwo\nthree\nkinds: bug feat\nlabel row\nfilter\n",
        "kinds",
    );
    g(&r, &["checkout", "-q", "-b", "y", &base]);
    let y = commit(
        &r,
        "a.txt",
        "one\ntwo\nthree\nkinds: bug feat doc\nlabel row\nfilter\n",
        "kinds again",
    );
    assert_eq!(
        overlap(&r, &up, &y, &x),
        Some(Overlap::Partial {
            files: vec!["a.txt".into()],
            present: 2,
            added: 3
        })
    );

    /* precheck lists only the requests that overlap, and keeps the order */
    let p = precheck(
        &r,
        UP,
        &a2,
        &[
            Request {
                id: "ST-1",
                head: &c,
            },
            Request {
                id: "ST-2",
                head: &a,
            },
            Request {
                id: "ST-3",
                head: &a2,
            },
        ],
    );
    assert_eq!(p.overlaps, vec![("ST-2".to_string(), Overlap::Same)]);
    assert!(matches!(
        overlap(&r, &up, &a, "0123456789abcdef0123456789abcdef01234567"),
        Some(Overlap::Unknown(_))
    ));
}
