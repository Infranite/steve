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

    /* indentation is meaning: equal `--stable` patch ids are not the same change */
    g(&r, &["checkout", "-q", "-b", "i1", &base]);
    let i1 = commit(&r, "f.py", "if x:\n    return 1\n", "indent 4");
    g(&r, &["checkout", "-q", "-b", "i2", &base]);
    let i2 = commit(&r, "f.py", "if x:\n        return 1\n", "indent 8");
    assert!(matches!(
        overlap(&r, &up, &i2, &i1),
        Some(Overlap::Partial { .. })
    ));

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
