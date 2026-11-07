//! Merge lanes end to end through the core (stage 2, direct push): an item of a project finished by its
//! completion rule asks to be released; it goes into its target's lane, one card asks for the batch,
//! yes merges and pushes it, and the item's timeline tells where it is. Real git: a bare origin and the
//! item's worktree.

use std::path::{Path, PathBuf};

use steve_store::lanes;

use crate::git::git;
use crate::patch;
use crate::repo::{Release, WS};
use crate::testkit::Kit;

fn run(dir: &Path, args: &[&str]) -> String {
    git(dir, args).unwrap_or_else(|e| panic!("git {args:?}: {e}"))
}

/// An origin with main, the project's clone, and an item worktree with one commit on `branch`.
fn repos(k: &Kit, branch: &str) -> (PathBuf, PathBuf, PathBuf) {
    repos_in(&k.home().join("lane-repos"), branch)
}

fn repos_in(root: &Path, branch: &str) -> (PathBuf, PathBuf, PathBuf) {
    let root = root.to_path_buf();
    std::fs::create_dir_all(&root).unwrap();
    let (origin, clone) = (root.join("origin.git"), root.join("clone"));
    run(
        &root,
        &[
            "init",
            "--quiet",
            "--bare",
            "-b",
            "main",
            &origin.to_string_lossy(),
        ],
    );
    run(
        &root,
        &[
            "clone",
            "--quiet",
            &origin.to_string_lossy(),
            &clone.to_string_lossy(),
        ],
    );
    for (a, b) in [("user.email", "t@example.invalid"), ("user.name", "t")] {
        run(&clone, &["config", a, b]);
    }
    std::fs::write(clone.join("a.txt"), "one\n").unwrap();
    run(&clone, &["add", "-A"]);
    run(&clone, &["commit", "--quiet", "-m", "base"]);
    run(&clone, &["push", "--quiet", "origin", "HEAD:main"]);
    let wt = root.join("wt");
    run(
        &clone,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            branch,
            &wt.to_string_lossy(),
            "origin/main",
        ],
    );
    std::fs::write(wt.join("a.txt"), "ONE\n").unwrap();
    run(&wt, &["commit", "--quiet", "-am", "the change"]);
    (origin, clone, wt)
}

fn timeline(k: &Kit, i: &str) -> Vec<String> {
    k.db()
        .timeline(i)
        .unwrap()
        .into_iter()
        .map(|t| t.text)
        .collect()
}

#[test]
fn an_item_released_through_its_lane_merges_after_one_card_for_the_batch() {
    let k = Kit::new();
    let branch = "steve/st-1";
    let (origin, clone, wt) = repos(&k, branch);
    let rel = Release {
        kind: "script".into(),
        cmd: String::new(),              // the lane runs no command of its own
        done_when: Some("merge".into()), // merged is released (tag needs the repository's cut-release)
    };
    let p = k
        .db()
        .project_create(
            WS,
            "R",
            "",
            &clone.to_string_lossy(),
            "git",
            true,
            None,
            Some(&rel),
        )
        .unwrap()
        .id;
    let i = k.item("run", "收尾", Some(&p));
    k.db()
        .item_patch(
            &i,
            patch![branch: branch, cwd: wt.to_string_lossy().as_ref()],
        )
        .unwrap();
    let sid = k.session(&i, "idle");
    let s = k.db().session(&sid).unwrap().unwrap();
    k.core.tool_publish(&s, branch, "改了 a").unwrap();
    assert!(
        k.wait(|| batch_card(&k, "approve").is_some()),
        "the batch card"
    );
    // in the lane, straight through: one card for the batch, no per-item release card
    let req = k
        .db()
        .store()
        .reader(|c| lanes::request_of_item(c, &i))
        .unwrap()
        .expect("a lane request");
    assert_eq!((req.state.as_str(), req.unit), ("merging", Some(1)));
    let open = k.db().decisions_open(WS).unwrap();
    let card = open
        .iter()
        .find(|d| d.subject_type == "delivery_batch")
        .expect("the batch card");
    assert!(card.draft.contains(&i), "{}", card.draft);
    assert!(
        !open.iter().any(|d| d.subject_type == "release"),
        "no release card per item"
    );
    // yes: merged and pushed to origin's main
    k.core.resolve(&card.id, "yes", "", "me").unwrap();
    let head = run(&wt, &["rev-parse", "HEAD"]);
    assert!(
        k.wait(|| {
            let r = k
                .db()
                .store()
                .reader(|c| lanes::request_get(c, &req.id))
                .unwrap()
                .unwrap();
            r.state == "released"
        }),
        "{:?}",
        timeline(&k, &i)
    );
    let main = run(&origin, &["rev-parse", "main"]);
    assert!(
        git(&origin, &["merge-base", "--is-ancestor", &head, &main]).is_ok(),
        "the item's commit is on main"
    );
    let t = timeline(&k, &i);
    assert!(t.iter().any(|x| x.contains("进入合并通道")), "{t:?}");
    assert!(t.iter().any(|x| x.contains("MQ-1 已合并到 main")), "{t:?}");
    assert!(t.iter().any(|x| x.contains("合并即发布")), "{t:?}");
}

#[test]
fn steves_own_seeded_project_publishes_through_its_lane_and_says_so() {
    let t = tempfile::tempdir().unwrap();
    let branch = "steve/st-9";
    let (_origin, clone, wt) = repos_in(t.path(), branch);
    std::fs::create_dir_all(clone.join("ops")).unwrap();
    std::fs::write(clone.join("ops/cut-release.py"), "#!/usr/bin/env python3\n").unwrap();
    let k = Kit::with(|c| c.steve_repo = clone.clone());
    let p = k
        .db()
        .project_by_path(WS, &clone.to_string_lossy())
        .unwrap()
        .expect("the seed registered Steve's own project");
    assert!(crate::orchestrator::lane_releases(&p), "{:?}", p.release);
    let i = k.item("run", "收尾", Some(&p.id));
    k.db()
        .item_patch(
            &i,
            patch![branch: branch, cwd: wt.to_string_lossy().as_ref()],
        )
        .unwrap();
    let sid = k.session(&i, "idle");
    // only the release left: the agent is told to hand the branch to the lane, not to push or tag
    k.core.tx().await_release_for(&i).unwrap();
    let told = k.rt.prompts_of(&sid).last().cloned().unwrap_or_default();
    assert!(
        told.starts_with(
            "验收通过。确认改动都已提交到 steve/st-9 后，用 steve_publish 把它交给合并通道"
        ) && told.contains("不要自己推送"),
        "{told}"
    );
    let s = k.db().session(&sid).unwrap().unwrap();
    k.core.tool_publish(&s, branch, "改了 a").unwrap();
    assert!(
        k.db()
            .store()
            .reader(|c| lanes::request_of_item(c, &i))
            .unwrap()
            .is_some(),
        "in the lane"
    );
    assert_eq!(
        k.db()
            .decisions_of_item(&i)
            .unwrap()
            .iter()
            .filter(|d| d.subject_type == "release")
            .count(),
        0,
        "no release card running ops/release.sh"
    );
}

#[test]
fn a_batch_you_turn_down_sends_its_members_back() {
    let k = Kit::new();
    let branch = "steve/st-2";
    let (origin, clone, wt) = repos(&k, branch);
    let rel = Release {
        kind: "script".into(),
        cmd: String::new(), // the lane runs no command of its own
        done_when: Some("merge".into()),
    };
    let p = k
        .db()
        .project_create(
            WS,
            "R",
            "",
            &clone.to_string_lossy(),
            "git",
            true,
            None,
            Some(&rel),
        )
        .unwrap()
        .id;
    let i = k.item("run", "收尾", Some(&p));
    k.db()
        .item_patch(
            &i,
            patch![branch: branch, cwd: wt.to_string_lossy().as_ref()],
        )
        .unwrap();
    let sid = k.session(&i, "idle");
    let s = k.db().session(&sid).unwrap().unwrap();
    let main_before = run(&origin, &["rev-parse", "main"]);
    k.core.tool_publish(&s, branch, "改了 a").unwrap();
    assert!(
        k.wait(|| batch_card(&k, "approve").is_some()),
        "the batch card"
    );
    let card = k
        .db()
        .decisions_open(WS)
        .unwrap()
        .into_iter()
        .find(|d| d.subject_type == "delivery_batch")
        .unwrap();
    k.core.resolve(&card.id, "no", "", "me").unwrap();
    assert!(k.wait(|| {
        timeline(&k, &i)
            .iter()
            .any(|x| x.contains("你没有批准这一批"))
    }));
    // the note comes first, the request leaves the lane right after it
    assert!(
        k.wait(|| k
            .db()
            .store()
            .reader(|c| lanes::request_of_item(c, &i))
            .unwrap()
            .is_none()),
        "nothing of it left in the lane"
    );
    assert_eq!(
        run(&origin, &["rev-parse", "main"]),
        main_before,
        "nothing merged"
    );
}

#[test]
fn publishing_twice_is_one_request_in_the_lane() {
    let k = Kit::new();
    let branch = "steve/st-3";
    let (_origin, clone, wt) = repos(&k, branch);
    let rel = Release {
        kind: "script".into(),
        cmd: String::new(), // the lane runs no command of its own
        done_when: Some("merge".into()),
    };
    let p = k
        .db()
        .project_create(
            WS,
            "R",
            "",
            &clone.to_string_lossy(),
            "git",
            true,
            None,
            Some(&rel),
        )
        .unwrap()
        .id;
    let i = k.item("run", "收尾", Some(&p));
    k.db()
        .item_patch(
            &i,
            patch![branch: branch, cwd: wt.to_string_lossy().as_ref()],
        )
        .unwrap();
    let sid = k.session(&i, "idle");
    let s = k.db().session(&sid).unwrap().unwrap();
    k.core.tool_publish(&s, branch, "改了 a").unwrap();
    assert!(
        k.wait(|| batch_card(&k, "approve").is_some()),
        "the batch card"
    );
    k.core.tool_publish(&s, branch, "改了 a").unwrap();
    assert!(
        k.wait(|| batch_card(&k, "approve").is_some()),
        "the batch card"
    );
    let all = k
        .db()
        .store()
        .reader(|c| {
            let l = lanes::lanes_all(c)?;
            lanes::requests_of(c, &l[0].id, false)
        })
        .unwrap();
    assert_eq!(all.len(), 1, "one request");
    let cards = k
        .db()
        .decisions_open(WS)
        .unwrap()
        .into_iter()
        .filter(|d| d.subject_type == "delivery_batch")
        .count();
    assert_eq!(cards, 1, "one card");
}

/// A project on the lane with one item that published: (kit, item, its request id, origin).
fn published(branch: &str) -> (Kit, String, String, PathBuf) {
    let k = Kit::new();
    let (origin, clone, wt) = repos(&k, branch);
    let rel = Release {
        kind: "script".into(),
        cmd: String::new(),
        done_when: Some("merge".into()),
    };
    let p = k
        .db()
        .project_create(
            WS,
            "R",
            "",
            &clone.to_string_lossy(),
            "git",
            true,
            None,
            Some(&rel),
        )
        .unwrap()
        .id;
    let i = k.item("run", "收尾", Some(&p));
    k.db()
        .item_patch(
            &i,
            patch![branch: branch, cwd: wt.to_string_lossy().as_ref()],
        )
        .unwrap();
    let sid = k.session(&i, "idle");
    let s = k.db().session(&sid).unwrap().unwrap();
    k.core.tool_publish(&s, branch, "改了 a").unwrap();
    assert!(
        k.wait(|| batch_card(&k, "approve").is_some()),
        "the batch card"
    );
    let req = k
        .db()
        .store()
        .reader(|c| lanes::request_of_item(c, &i))
        .unwrap()
        .unwrap();
    (k, i, req.id, origin)
}

#[test]
fn a_turn_down_recorded_before_a_restart_is_acted_on_after_it() {
    let (k, i, _, _) = published("steve/st-4");
    let card = k
        .db()
        .decisions_open(WS)
        .unwrap()
        .into_iter()
        .find(|d| d.subject_type == "delivery_batch")
        .unwrap();
    // the answer is recorded, the process stops before acting on it
    k.db()
        .decision_resolve(
            &card.id,
            serde_json::json!({"approved": false}),
            "resolved",
            None,
        )
        .unwrap();
    k.core.lane_recover();
    assert!(
        k.wait(|| k
            .db()
            .store()
            .reader(|c| lanes::request_of_item(c, &i))
            .unwrap()
            .is_none()),
        "{:?}",
        timeline(&k, &i)
    );
}

#[test]
fn a_failed_release_without_its_card_gets_one_after_a_restart() {
    let (k, _, req, _) = published("steve/st-5");
    let lane = k
        .db()
        .store()
        .reader(|c| lanes::request_get(c, &req))
        .unwrap()
        .unwrap()
        .lane_id;
    // failed, recorded, and the process stopped before the card
    crate::lane::store::release_failed(k.db().store(), &lane, 1, "推送被拒").unwrap();
    k.core.lane_recover();
    let open = k.db().decisions_open(WS).unwrap();
    let card = open
        .iter()
        .find(|d| d.subject_type == "delivery_batch" && d.kind == "fail")
        .expect("the fail card");
    assert_eq!(card.draft, "推送被拒");
    k.core.lane_recover();
    let n = k
        .db()
        .decisions_open(WS)
        .unwrap()
        .iter()
        .filter(|d| d.subject_type == "delivery_batch" && d.kind == "fail")
        .count();
    assert_eq!(n, 1, "one card, however often it recovers");
}

#[test]
fn a_retry_that_failed_again_before_a_restart_asks_again_instead_of_retrying() {
    let (k, _, req, _) = published("steve/st-6");
    let lane = k
        .db()
        .store()
        .reader(|c| lanes::request_get(c, &req))
        .unwrap()
        .unwrap()
        .lane_id;
    let fails = |k: &Kit| {
        k.db()
            .decisions_open(WS)
            .unwrap()
            .into_iter()
            .filter(|d| d.subject_type == "delivery_batch" && d.kind == "fail")
            .collect::<Vec<_>>()
    };
    crate::lane::store::release_failed(k.db().store(), &lane, 1, "第一次").unwrap();
    k.core.lane_recover();
    let first = fails(&k).pop().expect("the first card");
    // you said retry, the retry ran (the answer used up) and failed again, then the process stopped
    k.db()
        .decision_resolve(
            &first.id,
            serde_json::json!({"choice": "retry"}),
            "resolved",
            None,
        )
        .unwrap();
    k.db()
        .store()
        .transaction(|tx| lanes::batch_card_acted(tx, &format!("lane:{lane}:1"), "fail"))
        .unwrap();
    crate::lane::store::release_failed(k.db().store(), &lane, 1, "第二次").unwrap();
    k.core.lane_recover();
    let open = fails(&k);
    assert_eq!(open.len(), 1, "a new card for the new failure");
    assert_eq!(open[0].draft, "第二次");
    let unit = k
        .db()
        .store()
        .reader(|c| lanes::units_of(c, &lane))
        .unwrap()
        .into_iter()
        .find(|u| u.no == 1)
        .unwrap();
    assert_eq!(unit.state, "release_failed", "not retried without you");
}

/// A project on `clone` whose items close when merged (`done_when`), and an item of it on `branch`
/// in worktree `wt` that published: the item.
fn publish_item(k: &Kit, clone: &Path, wt: &Path, branch: &str, done_when: &str) -> String {
    let rel = Release {
        kind: "script".into(),
        cmd: String::new(),
        done_when: Some(done_when.into()),
    };
    let p = k
        .db()
        .project_create(
            WS,
            branch,
            "",
            &clone.to_string_lossy(),
            "git",
            true,
            None,
            Some(&rel),
        )
        .unwrap()
        .id;
    let i = k.item("run", "收尾", Some(&p));
    k.db()
        .item_patch(
            &i,
            patch![branch: branch, cwd: wt.to_string_lossy().as_ref()],
        )
        .unwrap();
    let sid = k.session(&i, "idle");
    let s = k.db().session(&sid).unwrap().unwrap();
    k.core.tool_publish(&s, branch, "改了 a").unwrap();
    i
}

fn batch_card(k: &Kit, kind: &str) -> Option<crate::repo::Decision> {
    k.db()
        .decisions_open(WS)
        .unwrap()
        .into_iter()
        .find(|d| d.subject_type == "delivery_batch" && d.kind == kind)
}

/// The open approval card of the batch, once it is there (it is made off the lock).
fn approve_card(k: &Kit) -> crate::repo::Decision {
    assert!(
        k.wait(|| batch_card(k, "approve").is_some()),
        "the batch card"
    );
    batch_card(k, "approve").unwrap()
}

fn has_branch(dir: &Path, b: &str) -> bool {
    git(
        dir,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{b}"),
        ],
    )
    .is_ok()
}

#[test]
fn a_target_that_takes_changes_only_through_a_pr_is_never_pushed_to() {
    // a gh that says main needs a pull request
    let bin = tempfile::tempdir().unwrap();
    let gh = bin.path().join("gh");
    std::fs::write(
        &gh,
        "#!/bin/sh\ncase \"$4\" in\n  repos/acme/app/rules/branches/main) printf '[{\"type\":\"pull_request\"}]' ;;\n  repos/acme/app/branches/main) printf '{\"protected\":false}' ;;\n  graphql) printf '{\"data\":{\"repository\":{\"mergeQueue\":null}}}' ;;\n  *) echo 'Not Found (HTTP 404)' >&2; exit 1 ;;\nesac\n",
    )
    .unwrap();
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&gh, std::fs::Permissions::from_mode(0o755)).unwrap();
    let k = Kit::with(|c| c.gh = gh.clone());
    let branch = "steve/st-7";
    let (origin, clone, wt) = repos(&k, branch);
    // origin is github.com/acme/app, reached through a copy on this machine
    let url = "https://github.com/acme/app.git";
    run(&clone, &["remote", "set-url", "origin", url]);
    run(
        &clone,
        &[
            "config",
            &format!("url.{}.insteadOf", origin.to_string_lossy()),
            url,
        ],
    );
    let main_before = run(&origin, &["rev-parse", "main"]);
    let i = publish_item(&k, &clone, &wt, branch, "merge");
    // refused before any card: you are told on the item, its agent is not sent back
    assert!(
        k.wait(|| k
            .db()
            .store()
            .reader(|c| lanes::request_of_item(c, &i))
            .unwrap()
            .is_none()),
        "{:?}",
        timeline(&k, &i)
    );
    assert!(
        k.wait(|| k.open_kinds(&i).contains(&"fail".to_string())),
        "{:?}",
        k.open_kinds(&i)
    );
    let all = k.db().decisions_of_item(&i).unwrap();
    let fail = all
        .iter()
        .find(|d| d.kind == "fail" && d.status == "open")
        .unwrap();
    assert!(fail.question.contains("只能经PR合入"), "{}", fail.question);
    let batch_cards: steve_store::Result<i64> = k.db().store().reader(|c| {
        Ok(c.query_row(
            "SELECT count(*) FROM decisions WHERE subject_type = 'delivery_batch'",
            [],
            |r| r.get(0),
        )?)
    });
    assert_eq!(
        batch_cards.unwrap(),
        0,
        "no approval card for a batch that cannot merge"
    );
    // the card comes first and the item's status right after it (not in one transaction)
    assert!(
        k.wait(|| k.db().item(&i).unwrap().unwrap().status != "run"),
        "not reopened to its agent"
    );
    assert_eq!(
        run(&origin, &["rev-parse", "main"]),
        main_before,
        "nothing pushed"
    );
    let t = timeline(&k, &i);
    assert!(t.iter().any(|x| x.contains("只能经PR合入")), "{t:?}");
}

#[test]
fn a_unit_that_is_over_leaves_no_worktree_or_batch_branch_behind() {
    let k = Kit::new();
    let branch = "steve/st-8";
    let (_origin, clone, wt) = repos(&k, branch);
    let i = publish_item(&k, &clone, &wt, branch, "merge");
    let req = k
        .db()
        .store()
        .reader(|c| lanes::request_of_item(c, &i))
        .unwrap()
        .unwrap();
    let card = approve_card(&k);
    k.core.resolve(&card.id, "yes", "", "me").unwrap();
    let mq = k
        .home()
        .join("worktrees/lanes")
        .join(&req.lane_id)
        .join("MQ-1");
    assert!(
        k.wait(|| !mq.exists() && !has_branch(&clone, "steve/merge/main/MQ-1")),
        "{:?}",
        timeline(&k, &i)
    );
    assert!(has_branch(&clone, branch), "the item's own branch stays");
}

#[test]
fn a_failed_release_keeps_its_worktree_for_the_retry_and_giving_up_removes_it() {
    let k = Kit::new();
    let branch = "steve/st-9";
    let (origin, clone, _) = repos(&k, branch);
    // the repository cuts its releases itself, and that fails
    std::fs::create_dir_all(clone.join("ops")).unwrap();
    std::fs::write(
        clone.join("ops/cut-release.py"),
        "import sys\nprint('no')\nsys.exit(1)\n",
    )
    .unwrap();
    run(&clone, &["checkout", "--quiet", "main"]);
    run(&clone, &["add", "-A"]);
    run(&clone, &["commit", "--quiet", "-m", "release step"]);
    run(&clone, &["push", "--quiet", "origin", "HEAD:main"]);
    let wt = k.home().join("lane-repos/wt2");
    run(
        &clone,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "steve/st-9b",
            &wt.to_string_lossy(),
            "origin/main",
        ],
    );
    std::fs::write(wt.join("b.txt"), "b\n").unwrap();
    run(&wt, &["add", "b.txt"]);
    run(&wt, &["commit", "--quiet", "-m", "b"]);
    let i = publish_item(&k, &clone, &wt, "steve/st-9b", "tag");
    let req = k
        .db()
        .store()
        .reader(|c| lanes::request_of_item(c, &i))
        .unwrap()
        .expect("in the lane");
    let card = approve_card(&k);
    k.core.resolve(&card.id, "yes", "", "me").unwrap();
    assert!(
        k.wait(|| batch_card(&k, "fail").is_some()),
        "{:?}",
        timeline(&k, &i)
    );
    let mq = k
        .home()
        .join("worktrees/lanes")
        .join(&req.lane_id)
        .join("MQ-1");
    assert!(mq.is_dir(), "kept for the retry");
    assert!(has_branch(&clone, "steve/merge/main/MQ-1"));
    let _ = origin;
    let fail = batch_card(&k, "fail").unwrap();
    k.core.resolve(&fail.id, "drop", "", "me").unwrap();
    assert!(
        k.wait(|| !mq.exists() && !has_branch(&clone, "steve/merge/main/MQ-1")),
        "given up: nothing left of it"
    );
}

#[test]
fn an_item_arriving_while_the_card_waits_joins_that_batch_and_merges_with_it() {
    let k = Kit::new();
    let (origin, clone, wt) = repos(&k, "steve/st-10");
    let a = publish_item(&k, &clone, &wt, "steve/st-10", "merge");
    let first = approve_card(&k);
    let wt2 = k.home().join("lane-repos/wt2");
    run(
        &clone,
        &[
            "worktree",
            "add",
            "--quiet",
            "-b",
            "steve/st-11",
            &wt2.to_string_lossy(),
            "origin/main",
        ],
    );
    std::fs::write(wt2.join("b.txt"), "b\n").unwrap();
    run(&wt2, &["add", "b.txt"]);
    run(&wt2, &["commit", "--quiet", "-m", "b"]);
    let b = publish_item(&k, &clone, &wt2, "steve/st-11", "merge");
    let req_b = k
        .db()
        .store()
        .reader(|c| lanes::request_of_item(c, &b))
        .unwrap()
        .unwrap();
    assert_eq!((req_b.state.as_str(), req_b.unit), ("merging", Some(1)));
    let open: Vec<_> = k
        .db()
        .decisions_open(WS)
        .unwrap()
        .into_iter()
        .filter(|d| d.subject_type == "delivery_batch")
        .collect();
    assert_eq!(open.len(), 1, "one card for both");
    assert!(
        open[0].draft.contains(&a) && open[0].draft.contains(&b),
        "{}",
        open[0].draft
    );
    assert!(
        timeline(&k, &b)
            .iter()
            .any(|x| x.contains("并入还在等你批准的 MQ-1"))
    );
    // the card you loaded before b joined did not list it: it takes no answer, nothing merges by it
    assert!(!first.draft.contains(&b), "{}", first.draft);
    assert!(k.core.resolve(&first.id, "yes", "", "me").is_err());
    assert_ne!(open[0].id, first.id, "a new card in its place");
    let old = k.db().decision(&first.id).unwrap().unwrap();
    assert_eq!(old.status, "expired");
    k.core.resolve(&open[0].id, "yes", "", "me").unwrap();
    for i in [&a, &b] {
        assert!(
            k.wait(|| k
                .db()
                .store()
                .reader(|c| lanes::request_of_item(c, i))
                .unwrap()
                .is_none()),
            "{:?}",
            timeline(&k, i)
        );
    }
    let main = run(&origin, &["rev-parse", "main"]);
    for w in [&wt, &wt2] {
        let head = run(w, &["rev-parse", "HEAD"]);
        assert!(git(&origin, &["merge-base", "--is-ancestor", &head, &main]).is_ok());
    }
}
