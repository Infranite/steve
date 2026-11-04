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
    let root = k.home().join("lane-repos");
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
        cmd: "/bin/false".into(),
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
fn a_batch_you_turn_down_sends_its_members_back() {
    let k = Kit::new();
    let branch = "steve/st-2";
    let (origin, clone, wt) = repos(&k, branch);
    let rel = Release {
        kind: "script".into(),
        cmd: "/bin/false".into(),
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
    let r = k
        .db()
        .store()
        .reader(|c| lanes::request_of_item(c, &i))
        .unwrap();
    assert!(r.is_none(), "nothing of it left in the lane");
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
        cmd: "/bin/false".into(),
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
    k.core.tool_publish(&s, branch, "改了 a").unwrap();
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
