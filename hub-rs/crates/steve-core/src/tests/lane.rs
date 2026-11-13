//! Merge lanes on real repositories: which target a working copy merges into, and whether a change
//! is on the target already.

use std::path::{Path, PathBuf};
use std::process::Command;

use crate::lane::target::target_of;
use crate::upstream::{Presence, presence};

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

/// Whether `head`'s change is on `target` (a ref) as last fetched; a target that is not there is
/// unknown.
fn on(r: &Path, target: &str, head: &str) -> Presence {
    match Command::new("git")
        .arg("-C")
        .arg(r)
        .args(["rev-parse", "--verify", "--quiet", target])
        .output()
    {
        Ok(o) if o.status.success() => presence(r, head, String::from_utf8_lossy(&o.stdout).trim()),
        _ => Presence::Unknown(format!("没有 {target}")),
    }
}

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

    /* a clone of that checkout merges into the same repository */
    let of_solo = t.path().join("of-solo");
    g(
        t.path(),
        &[
            "clone",
            "-q",
            &solo.to_string_lossy(),
            &of_solo.to_string_lossy(),
        ],
    );
    assert_eq!(target_of(&of_solo, None).unwrap().upstream, s.upstream);

    /* an `insteadOf` shorthand is the repository it expands to … */
    g(
        &other,
        &["config", "url.https://github.com/.insteadOf", "gh:"],
    );
    g(&other, &["remote", "set-url", "origin", "gh:Acme/App.git"]);
    assert_eq!(target_of(&other, Some("release")).unwrap(), a);
    /* … but a network remote served from a copy on this machine is still that remote */
    g(
        &r,
        &[
            "config",
            &format!(
                "url.{}.insteadOf",
                t.path().join("origin.git").to_string_lossy()
            ),
            "git@github.com:Acme/App.git",
        ],
    );
    assert_eq!(target_of(&r, Some("release")).unwrap(), a);
}

#[test]
fn a_change_already_on_the_target_is_found_however_it_was_merged() {
    let t = tempfile::tempdir().unwrap();
    let r = repo(t.path());
    let base = g(&r, &["rev-parse", "HEAD"]);

    /* not merged */
    g(&r, &["checkout", "-q", "-b", "feat"]);
    let feat = commit(&r, "b.txt", "feature\n", "feature");
    assert_eq!(on(&r, UP, &feat), Presence::Absent);

    /* merged as is (fast-forward) */
    g(&r, &["checkout", "-q", "main"]);
    g(&r, &["merge", "-q", "--ff-only", "feat"]);
    land(&r);
    assert_eq!(
        on(&r, UP, &feat),
        Presence::Landed {
            commit: feat.clone()
        }
    );

    /* rebased: main moved on, the branch's copy differs in id only */
    g(&r, &["checkout", "-q", "-b", "two", &base]);
    let two = commit(&r, "c.txt", "second\n", "second");
    g(&r, &["checkout", "-q", "main"]);
    g(&r, &["cherry-pick", &two]);
    land(&r);
    let copy = g(&r, &["rev-parse", "HEAD"]);
    assert_ne!(copy, two);
    assert_eq!(on(&r, UP, &two), Presence::Landed { commit: copy });

    /* squashed: two commits arrive as one */
    g(&r, &["checkout", "-q", "-b", "three", &base]);
    commit(&r, "d.txt", "x\n", "d1");
    let three = commit(&r, "d.txt", "x\ny\n", "d2");
    g(&r, &["checkout", "-q", "main"]);
    g(&r, &["merge", "-q", "--squash", "three"]);
    g(&r, &["commit", "-q", "-m", "squash"]);
    land(&r);
    let squash = g(&r, &["rev-parse", "HEAD"]);
    assert_eq!(on(&r, UP, &three), Presence::Landed { commit: squash });

    /* a branch with no history in common with the target (an old line): definitely not on it */
    g(&r, &["checkout", "-q", "--orphan", "old"]);
    let old = commit(&r, "old.txt", "old\n", "old line");
    assert_eq!(on(&r, UP, &old), Presence::Absent);
    g(&r, &["checkout", "-q", "main"]);

    /* git can't tell: not "absent" */
    assert!(matches!(
        on(&r, UP, "0123456789abcdef0123456789abcdef01234567"),
        Presence::Unknown(_)
    ));
    assert!(matches!(
        on(&r, "refs/remotes/origin/nope", &three),
        Presence::Unknown(_)
    ));
}

/// A `gh` that answers `gh api <path>` from `answers` (path → JSON), 404 for anything else.
fn fake_gh(home: &Path, answers: &[(&str, &str)]) -> PathBuf {
    let mut s = String::from("#!/bin/sh\ncase \"$4\" in\n");
    for (path, json) in answers {
        s.push_str(&format!("  '{path}') printf '%s' '{json}' ;;\n"));
    }
    s.push_str("  *) echo 'gh: Not Found (HTTP 404)' >&2; exit 1 ;;\nesac\n");
    let p = home.join("gh");
    std::fs::write(&p, s).unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
    p
}

#[test]
fn a_target_is_recognized_from_its_repository_and_host_with_evidence() {
    use crate::lane::profile::{How, Merge, Ship, profile};
    let t = tempfile::tempdir().unwrap();
    let r = repo(t.path());
    std::fs::create_dir_all(r.join(".github/workflows")).unwrap();
    std::fs::create_dir_all(r.join("docs/releases/unreleased")).unwrap();
    std::fs::create_dir_all(r.join("db/migrations")).unwrap();
    std::fs::create_dir_all(r.join("web")).unwrap();
    write(
        &r,
        ".github/workflows/ci.yml",
        "name: CI\non:\n  pull_request:\n  merge_group:\njobs: {}\n",
    );
    write(
        &r,
        ".github/workflows/release.yml",
        "on:\n  push:\n    tags: ['v*']\njobs: {}\n",
    );
    write(&r, "docs/releases/0.1.0.md", "# 0.1.0\n");
    write(&r, "docs/releases/unreleased/ST-1.md", "- one\n");
    write(&r, "db/migrations/001.sql", "");
    write(&r, "db/migrations/002.sql", "");
    write(&r, "db/migrations/LOCK", "");
    write(&r, "Cargo.toml", "[workspace]\nmembers = []\n");
    write(&r, "Cargo.lock", "");
    write(
        &r,
        "web/package.json",
        r#"{"scripts":{"check":"tsc","test":"node --test"}}"#,
    );
    commit(&r, "ops.txt", "", "layout");
    g(&r, &["tag", "-a", "v0.1.0", "-m", "v0.1.0"]);
    land(&r);
    g(
        &r,
        &["remote", "set-url", "origin", "git@github.com:Acme/App.git"],
    );
    g(
        &r,
        &[
            "config",
            &format!(
                "url.{}.insteadOf",
                t.path().join("origin.git").to_string_lossy()
            ),
            "git@github.com:Acme/App.git",
        ],
    );

    /* GitHub: a ruleset with a merge queue and a required check */
    let gh = fake_gh(
        t.path(),
        &[
            (
                "repos/acme/app/rules/branches/main",
                r#"[{"type":"merge_queue"},{"type":"required_status_checks","parameters":{"required_status_checks":[{"context":"test"}]}}]"#,
            ),
            ("repos/acme/app/branches/main", r#"{"protected":false}"#),
            ("graphql", r#"{"data":{"repository":{"mergeQueue":null}}}"#),
        ],
    );
    let p = profile(&r, None, None, &gh).unwrap();
    assert_eq!(p.target.key(), "github.com/acme/app#main");
    assert!(p.reads.starts_with("origin/main "), "{}", p.reads);
    assert_eq!(
        (p.merge.value, &p.merge.how),
        (Merge::HostQueue, &How::Found)
    );
    let checks: Vec<(&str, &str)> = p
        .checks
        .value
        .iter()
        .map(|c| (c.name.as_str(), c.from))
        .collect();
    assert_eq!(
        checks,
        vec![
            ("test", "required"),
            (".github/workflows/ci.yml", "workflow"),
            ("cargo test --workspace", "suggested"),
            ("npm --prefix web run check", "suggested"),
            ("npm --prefix web test", "suggested"),
        ]
    );
    assert_eq!((p.ship.value, &p.ship.how), (Ship::Tag, &How::Found));
    let kinds: Vec<(&str, &str)> = p
        .numbered
        .iter()
        .map(|n| (n.kind, n.path.as_str()))
        .collect();
    assert_eq!(
        kinds,
        vec![
            ("version", "tag vX.Y.Z"),
            ("notes", "docs/releases/"),
            ("migrations", "db/migrations/"),
            ("lockfile", "Cargo.lock"),
        ]
    );
    assert!(
        p.numbered[1].handling.contains("unreleased"),
        "{:?}",
        p.numbered[1]
    );
    assert!(
        p.numbered[2].evidence.contains("最新 002.sql，有 LOCK"),
        "{:?}",
        p.numbered[2]
    );
    assert!(
        p.pace.landed >= 2 && p.pace.releases == 1 && p.pace.authors == 1,
        "{:?}",
        p.pace
    );

    /* the project's own rule outranks what the repository suggests */
    let rel = crate::repo::Release {
        kind: "script".into(),
        cmd: "ops/release.sh".into(),
        done_when: None,
    };
    assert_eq!(
        profile(&r, None, Some(&rel), &gh).unwrap().ship.value,
        Ship::Script
    );

    /* GitHub can't be read: the safe default, and why, never "nothing there" */
    let down = t.path().join("no-gh");
    let p = profile(&r, None, None, &down).unwrap();
    assert_eq!(p.merge.value, Merge::Push);
    assert!(matches!(p.merge.how, How::Unknown { .. }), "{:?}", p.merge);
    assert!(
        matches!(p.checks.how, How::Unknown { .. }),
        "{:?}",
        p.checks
    );
    assert_eq!(p.checks.value[0].name, ".github/workflows/ci.yml");

    /* classic protection that requires a PR */
    let gh = fake_gh(
        t.path(),
        &[
            ("repos/acme/app/rules/branches/main", "[]"),
            ("repos/acme/app/branches/main", r#"{"protected":true}"#),
            ("graphql", r#"{"data":{"repository":{"mergeQueue":null}}}"#),
            (
                "repos/acme/app/branches/main/protection",
                r#"{"required_pull_request_reviews":{},"required_status_checks":{"contexts":["build"]}}"#,
            ),
        ],
    );
    let p = profile(&r, None, None, &gh).unwrap();
    assert_eq!(p.merge.value, Merge::HostPr);
    assert_eq!(p.checks.value[0].name, "build");

    /* only on this machine: pushed, and nothing asked of a host */
    g(
        &r,
        &[
            "config",
            "--unset",
            &format!(
                "url.{}.insteadOf",
                t.path().join("origin.git").to_string_lossy()
            ),
        ],
    );
    g(
        &r,
        &[
            "remote",
            "set-url",
            "origin",
            &t.path().join("origin.git").to_string_lossy(),
        ],
    );
    let p = profile(&r, None, None, &down).unwrap();
    assert_eq!((p.merge.value, &p.merge.how), (Merge::Push, &How::Found));
    assert_eq!(p.checks.how, How::Found);
}

#[test]
fn what_can_not_be_read_is_unknown_never_none() {
    use crate::lane::profile::{How, Merge, Ship, profile};
    let t = tempfile::tempdir().unwrap();
    let r = repo(t.path());
    std::fs::create_dir_all(r.join(".github/workflows")).unwrap();
    std::fs::create_dir_all(r.join("db/migrations")).unwrap();
    /* a workflow under a non-ASCII name, one this reader can't take, numbered migrations past 9 */
    write(&r, ".github/workflows/检查.yml", "on: [pull_request]\n");
    write(&r, ".github/workflows/odd.yml", "on: &events\n  push:\n");
    for m in ["9_init.sql", "10_add.sql", "2_more.sql"] {
        write(&r, &format!("db/migrations/{m}"), "");
    }
    commit(&r, "x.txt", "x\n", "layout");
    /* a version tag on another branch only */
    g(&r, &["checkout", "-q", "-b", "release/1.x"]);
    commit(&r, "y.txt", "y\n", "backport");
    g(&r, &["tag", "-a", "v1.0.1", "-m", "v1.0.1"]);
    g(&r, &["checkout", "-q", "main"]);
    land(&r);
    let bare = t.path().join("origin.git");
    g(
        &r,
        &["remote", "set-url", "origin", "git@github.com:Acme/App.git"],
    );
    g(
        &r,
        &[
            "config",
            &format!("url.{}.insteadOf", bare.to_string_lossy()),
            "git@github.com:Acme/App.git",
        ],
    );

    /* rulesets only: `protected`, and no classic protection to read */
    let gh = fake_gh(
        t.path(),
        &[
            (
                "repos/acme/app/rules/branches/main",
                r#"[{"type":"pull_request"}]"#,
            ),
            ("repos/acme/app/branches/main", r#"{"protected":true}"#),
            ("graphql", r#"{"data":{"repository":{"mergeQueue":null}}}"#),
        ],
    );
    let p = profile(&r, None, None, &gh).unwrap();
    assert_eq!(
        (p.merge.value, &p.merge.how),
        (Merge::HostPr, &How::Found),
        "{:?}",
        p.merge
    );
    let flow = p
        .checks
        .value
        .iter()
        .find(|c| c.from == "workflow")
        .unwrap();
    assert_eq!(
        (flow.name.as_str(), flow.on.clone()),
        (
            ".github/workflows/检查.yml",
            vec!["pull_request".to_string()]
        )
    );
    assert!(
        matches!(&p.checks.how, How::Unknown { why } if why.contains("odd.yml")),
        "{:?}",
        p.checks
    );
    assert_eq!(p.ship.value, Ship::Merge);
    assert!(
        matches!(&p.ship.how, How::Unknown { why } if why.contains("odd.yml")),
        "{:?}",
        p.ship
    );
    let mig = p.numbered.iter().find(|n| n.kind == "migrations").unwrap();
    assert!(mig.evidence.contains("最新 10_add.sql"), "{mig:?}");
    assert_eq!(
        p.pace.releases, 0,
        "a tag on another branch is not this branch's release"
    );

    /* classic protection we may not read: what was read stands, the rest is unknown */
    let gh = fake_gh(
        t.path(),
        &[
            ("repos/acme/app/rules/branches/main", "[]"),
            ("repos/acme/app/branches/main", r#"{"protected":true}"#),
            ("graphql", r#"{"data":{"repository":{"mergeQueue":null}}}"#),
        ],
    );
    std::fs::write(
        t.path().join("gh"),
        std::fs::read_to_string(t.path().join("gh"))
            .unwrap()
            .replace("  *) echo", "  repos/acme/app/branches/main/protection) echo 'gh: Must have admin rights (HTTP 403)' >&2; exit 1 ;;\n  *) echo"),
    )
    .unwrap();
    let p = profile(&r, None, None, &gh).unwrap();
    assert!(
        matches!(&p.merge.how, How::Unknown { why } if why.contains("管理员")),
        "{:?}",
        p.merge
    );
    assert!(matches!(p.checks.how, How::Unknown { .. }));

    /* a merge queue set up in classic protection: only GraphQL tells */
    let gh = fake_gh(
        t.path(),
        &[
            ("repos/acme/app/rules/branches/main", "[]"),
            ("repos/acme/app/branches/main", r#"{"protected":false}"#),
            (
                "graphql",
                r#"{"data":{"repository":{"mergeQueue":{"id":"MQ_1"}}}}"#,
            ),
        ],
    );
    assert_eq!(
        profile(&r, None, None, &gh).unwrap().merge.value,
        Merge::HostQueue
    );

    /* a branch name with `#` and `/` reaches GitHub as one segment */
    g(&r, &["checkout", "-q", "-b", "rel/1#2"]);
    g(&r, &["push", "-q", "origin", "rel/1#2"]);
    g(&r, &["fetch", "-q", "origin"]);
    let gh = fake_gh(
        t.path(),
        &[
            (
                "repos/acme/app/rules/branches/rel%2F1%232",
                r#"[{"type":"merge_queue"}]"#,
            ),
            (
                "repos/acme/app/branches/rel%2F1%232",
                r#"{"protected":false}"#,
            ),
        ],
    );
    assert_eq!(
        profile(&r, Some("rel/1#2"), None, &gh).unwrap().merge.value,
        Merge::HostQueue
    );

    /* a host this reader doesn't know: checks unknown, not "none" */
    g(
        &r,
        &[
            "remote",
            "set-url",
            "origin",
            "git@gitlab.example.com:acme/app.git",
        ],
    );
    g(
        &r,
        &[
            "config",
            &format!("url.{}.insteadOf", bare.to_string_lossy()),
            "git@gitlab.example.com:acme/app.git",
        ],
    );
    let p = profile(&r, Some("main"), None, &gh).unwrap();
    assert!(matches!(&p.merge.how, How::Unknown { .. }));
    assert!(
        matches!(&p.checks.how, How::Unknown { why } if why.contains("gitlab.example.com")),
        "{:?}",
        p.checks
    );
}

#[test]
fn a_fetched_profile_reads_github_during_the_fetch_and_the_repository_after_it() {
    use crate::lane::profile::{Merge, profile_fetched};
    let t = tempfile::tempdir().unwrap();
    let r = repo(t.path());
    g(
        &r,
        &["remote", "set-url", "origin", "git@github.com:Acme/App.git"],
    );
    g(
        &r,
        &[
            "config",
            &format!(
                "url.{}.insteadOf",
                t.path().join("origin.git").to_string_lossy()
            ),
            "git@github.com:Acme/App.git",
        ],
    );
    /* each GitHub read takes a while */
    let gh = fake_gh(
        t.path(),
        &[
            (
                "repos/acme/app/rules/branches/main",
                r#"[{"type":"pull_request"}]"#,
            ),
            ("repos/acme/app/branches/main", r#"{"protected":false}"#),
            ("graphql", r#"{"data":{"repository":{"mergeQueue":null}}}"#),
        ],
    );
    let slow = std::fs::read_to_string(&gh)
        .unwrap()
        .replacen("case", "sleep 0.6\ncase", 1);
    std::fs::write(&gh, slow).unwrap();

    /* someone else lands a commit; the fetch brings it */
    let other = t.path().join("other");
    g(
        t.path(),
        &[
            "clone",
            "-q",
            &t.path().join("origin.git").to_string_lossy(),
            &other.to_string_lossy(),
        ],
    );
    let theirs = commit(&other, "b.txt", "b\n", "theirs");
    g(&other, &["push", "-q", "origin", "main"]);

    let started = std::time::Instant::now();
    let (fetch, p) = profile_fetched(&r, None, None, &gh, || {
        std::thread::sleep(std::time::Duration::from_millis(600));
        crate::upstream::fetch(&r)?;
        Err("half done".into())
    });
    let took = started.elapsed();
    let p = p.unwrap();
    assert_eq!(fetch.as_deref(), Some("half done"));
    assert!(p.reads.ends_with(&theirs[..7]), "{}", p.reads);
    assert_eq!(p.merge.value, Merge::HostPr);
    // the four GitHub reads and the fetch overlap: one after another they take 3s
    assert!(took < std::time::Duration::from_millis(2000), "{took:?}");
}
