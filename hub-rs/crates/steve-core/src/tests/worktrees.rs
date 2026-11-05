//! N0e-E11a: an item's worktree made and given back by its session's backend (the local Node). The
//! core only names where it goes and decides whether it may go; it never runs `git worktree add` or
//! `remove` for such a session, and applies what the backend tells it only to the item's current
//! session.

use std::fs;
use std::path::{Path, PathBuf};

use crate::CreateItem;
use crate::git::tests::{branch_with, g, repo};
use crate::patch;
use crate::repo::WS;
use crate::testkit::Kit;
use crate::workspace::DiskPolicy;

fn project(k: &Kit) -> (PathBuf, String) {
    let r = repo(k.home());
    let p = k
        .db()
        .project_create(
            WS,
            "G",
            "",
            &r.to_string_lossy(),
            "git",
            true,
            Some("dev"),
            None,
        )
        .unwrap();
    (r, p.id)
}

/// A kit whose backend makes worktrees, binding every new session there when `binds`.
fn kit(binds: bool) -> Kit {
    let k = Kit::new();
    {
        let mut st = k.rt.st();
        st.makes_worktrees = true;
        st.binds_worktrees = binds;
    }
    k
}

fn create(k: &Kit, p: &str) -> crate::repo::Item {
    k.core
        .create_item(CreateItem {
            ws: WS.into(),
            goal: "g".into(),
            title: "新事项".into(),
            bot: Some("dev".into()),
            project: Some(Some(p.into())),
            ..Default::default()
        })
        .unwrap()
}

/// The Node making the worktree (as `git worktree add` does where the core named it).
fn node_makes(r: &Path, wt: &Path, branch: &str) {
    g(
        r,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            branch,
            &wt.to_string_lossy(),
            "main",
        ],
    );
}

#[test]
fn a_worktree_its_backend_makes_is_only_named_under_the_lock() {
    let k = kit(true);
    let (r, p) = project(&k);
    let i = create(&k, &p);
    let wt = k.core.config().worktrees.join(&i.id);
    let branch = format!("steve/{}", i.id.to_lowercase());
    // named, not made: the session starts there, the agent is told where it works
    assert!(!wt.exists(), "the core made no worktree");
    let i = k.db().item(&i.id).unwrap().unwrap();
    assert_eq!(i.cwd.as_deref(), Some(wt.to_string_lossy().as_ref()));
    assert_eq!(i.branch.as_deref(), Some(branch.as_str()));
    let s = k.db().sessions_of_item(&i.id).unwrap().pop().unwrap();
    assert_eq!(s.cwd, wt.to_string_lossy());
    assert!(k.rt.prompts_of(&s.id)[0].contains(&format!("Working directory: {}", wt.display())));
    assert!(
        crate::git::git_test(&r, &["rev-parse", "--verify", "--quiet", &branch]) != Ok(true),
        "no branch made either"
    );
    // the backend made it: where it is is applied, the same place (no change)
    node_makes(&r, &wt, &branch);
    k.core
        .workspace_ready(&s.id, &wt.to_string_lossy(), Some(&branch))
        .unwrap();
    assert_eq!(
        k.db().item(&i.id).unwrap().unwrap().cwd.as_deref(),
        Some(wt.to_string_lossy().as_ref())
    );
}

#[test]
fn a_late_answer_for_an_older_session_changes_nothing() {
    let k = kit(true);
    let (_, p) = project(&k);
    let i = create(&k, &p);
    let old = k.db().sessions_of_item(&i.id).unwrap().pop().unwrap();
    k.db()
        .session_patch(&old.id, patch![status: "ended"])
        .unwrap();
    let new = k
        .core
        .tx()
        .start_session(&i.id, Some("继续"), None)
        .unwrap();
    k.core
        .workspace_ready(&old.id, "/elsewhere/ST-x", Some("steve/x"))
        .unwrap();
    let after = k.db().item(&i.id).unwrap().unwrap();
    assert_eq!(after.cwd.as_deref(), Some(new.cwd.as_str()));
    assert_ne!(after.branch.as_deref(), Some("steve/x"));
    // nor does a prepare failure of it raise a card
    k.core.workspace_failed(&old.id, false, "boom").unwrap();
    assert!(!k.open_kinds(&i.id).contains(&"fail".to_string()));
    // the current one's does, in the start's words
    k.core
        .workspace_failed(&new.id, false, "git worktree: not a worktree")
        .unwrap();
    let cards = k.db().decisions_of_item(&i.id).unwrap();
    assert!(
        cards
            .iter()
            .any(|d| d.kind == "fail" && d.question == "开不了会话：git worktree: not a worktree"),
        "{:?}",
        cards.iter().map(|d| &d.question).collect::<Vec<_>>()
    );
}

#[test]
fn below_the_floor_the_backends_refusal_reads_as_steves() {
    let k = Kit::with(|c| {
        c.disk = DiskPolicy {
            floor: u64::MAX,
            low: u64::MAX,
            idle_ms: i64::MAX,
        }
    });
    {
        let mut st = k.rt.st();
        st.makes_worktrees = true;
        st.binds_worktrees = true;
    }
    let (_, p) = project(&k);
    // the floor is the Node's to check now: the item starts, its session waits for the worktree
    let i = create(&k, &p);
    let s = k.db().sessions_of_item(&i.id).unwrap().pop().unwrap();
    assert!(k.open_kinds(&i.id).is_empty());
    k.core.workspace_failed(&s.id, true, "disk low").unwrap();
    let cards = k.db().decisions_of_item(&i.id).unwrap();
    assert!(
        cards.iter().any(|d| d.kind == "fail"
            && d.question.starts_with("开不了会话：")
            && d.question.contains("磁盘空间不足")),
        "{:?}",
        cards.iter().map(|d| &d.question).collect::<Vec<_>>()
    );
}

#[test]
fn a_session_left_with_the_core_gets_its_worktree_from_the_core() {
    let k = kit(false);
    let (r, p) = project(&k);
    let i = create(&k, &p);
    let wt = k.core.config().worktrees.join(&i.id);
    assert!(wt.join("a.txt").exists(), "made here, as without the Node");
    assert_eq!(
        g(&wt, &["rev-parse", "--abbrev-ref", "HEAD"]),
        format!("steve/{}", i.id.to_lowercase())
    );
    let _ = r;
}

#[test]
fn a_finished_items_worktree_goes_back_through_its_backend_and_unmerged_work_stays() {
    let k = kit(true);
    let (r, p) = project(&k);
    // done, its work merged: given back by the backend, the merged branch with it
    let done = k.item("done", "收尾", Some(&p));
    let (branch, wt) = branch_with(&r, &k.core.config().worktrees, &done, "x.txt", "x\n");
    k.db()
        .item_patch(
            &done,
            patch![cwd: wt.to_string_lossy().to_string(), branch: branch.as_str()],
        )
        .unwrap();
    g(&r, &["merge", "-q", "--ff-only", &branch]);
    let s = k.session(&done, "ended");
    k.rt.st().node_made.insert(s.clone());
    assert!(k.core.tx().tidy_branch_now(&done).unwrap());
    assert_eq!(k.rt.st().tidies, vec![(s.clone(), false)]);
    assert!(wt.exists(), "the core removed nothing itself");
    // what the backend did is written when it says so
    g(&r, &["worktree", "remove", &wt.to_string_lossy()]);
    g(&r, &["branch", "-d", &branch]);
    k.core.workspace_tidied(&done, true).unwrap();
    let tl = k.timeline_texts(&done);
    assert!(
        tl.iter()
            .any(|t| t == &format!("删除了工作区 {}；删除了已合并的分支 {branch}", wt.display())),
        "{tl:?}"
    );

    // done, not merged: kept, nothing asked
    let open = k.item("done", "收尾", Some(&p));
    let (b2, wt2) = branch_with(&r, &k.core.config().worktrees, &open, "y.txt", "y\n");
    k.db()
        .item_patch(
            &open,
            patch![cwd: wt2.to_string_lossy().to_string(), branch: b2.as_str()],
        )
        .unwrap();
    let s2 = k.session(&open, "ended");
    k.rt.st().node_made.insert(s2.clone());
    assert!(k.core.tx().tidy_branch_now(&open).unwrap());
    assert!(k.rt.st().tidies.iter().all(|(s, _)| s != &s2));
    assert!(wt2.exists());
    // archived: a clean worktree goes back, its unmerged branch stays
    k.db().item_patch(&open, patch![archived: true]).unwrap();
    assert!(k.core.tx().tidy_branch_now(&open).unwrap());
    assert!(k.rt.st().tidies.contains(&(s2.clone(), true)));
    g(&r, &["worktree", "remove", &wt2.to_string_lossy()]);
    k.core.workspace_tidied(&open, true).unwrap();
    let tl = k.timeline_texts(&open);
    assert!(
        tl.iter()
            .any(|t| t.starts_with(&format!("删除了工作区 {}（分支 {b2} 保留在", wt2.display()))),
        "{tl:?}"
    );
    // a refused or failed tidy says nothing
    k.core.workspace_tidied(&done, false).unwrap();
    assert_eq!(
        k.timeline_texts(&done)
            .iter()
            .filter(|t| t.starts_with("删除了工作区"))
            .count(),
        1
    );
}

#[test]
fn a_busy_session_keeps_the_worktree_and_nothing_is_asked() {
    let k = kit(true);
    let (r, p) = project(&k);
    let done = k.item("done", "收尾", Some(&p));
    let (branch, wt) = branch_with(&r, &k.core.config().worktrees, &done, "x.txt", "x\n");
    k.db()
        .item_patch(
            &done,
            patch![cwd: wt.to_string_lossy().to_string(), branch: branch.as_str()],
        )
        .unwrap();
    g(&r, &["merge", "-q", "--ff-only", &branch]);
    let s = k.session(&done, "running");
    k.rt.st().node_made.insert(s.clone());
    k.rt.st().turning.insert(s);
    assert!(!k.core.tx().tidy_branch_now(&done).unwrap());
    assert!(k.rt.st().tidies.is_empty());
    assert!(wt.exists());
}

#[test]
fn a_removed_worktree_its_backend_makes_again_is_not_made_by_the_core() {
    let k = kit(true);
    let (r, p) = project(&k);
    let id = k.item("run", "执行", Some(&p));
    let (branch, wt) = branch_with(&r, &k.core.config().worktrees, &id, "x.txt", "x\n");
    k.db()
        .item_patch(
            &id,
            patch![cwd: wt.to_string_lossy().to_string(), branch: branch.as_str()],
        )
        .unwrap();
    let s = k.session(&id, "idle");
    g(&r, &["worktree", "remove", &wt.to_string_lossy()]);
    // its session's backend makes it again when it opens: not here
    k.rt.st().node_made.insert(s.clone());
    k.core.tx().ensure_cwd(&id).unwrap();
    assert!(!wt.exists());
    // a session the core runs gets it back from the core, as before
    k.rt.st().node_made.remove(&s);
    k.core.tx().ensure_cwd(&id).unwrap();
    assert!(wt.join("x.txt").exists());
    let _ = fs::metadata(&wt);
}
