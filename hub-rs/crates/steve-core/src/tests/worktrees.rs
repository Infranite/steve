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
fn a_finished_items_worktree_goes_back_through_its_backend_preserving_unmerged_branches_and_reporting_failures()
 {
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
    k.core.workspace_tidied(&done, true, None).unwrap();
    let tl = k.timeline_texts(&done);
    assert!(
        tl.iter()
            .any(|t| t == &format!("删除了工作区 {}；删除了已合并的分支 {branch}", wt.display())),
        "{tl:?}"
    );

    // done, not merged: worktree is still cleaned right away while its unmerged branch stays
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
    assert!(k.rt.st().tidies.contains(&(s2.clone(), true)));
    g(&r, &["worktree", "remove", &wt2.to_string_lossy()]);
    k.core.workspace_tidied(&open, true, None).unwrap();
    let tl = k.timeline_texts(&open);
    assert!(
        tl.iter()
            .any(|t| t.contains(&format!("删除了工作区 {}", wt2.display()))
                && t.contains(&format!("未合并分支 {b2} 已保留"))),
        "{tl:?}"
    );
    // a refused or failed tidy is displayed on the item (never swallowed) and deduplicated
    k.core
        .workspace_tidied(
            &done,
            false,
            Some("worktree contains modified or untracked files"),
        )
        .unwrap();
    k.core
        .workspace_tidied(
            &done,
            false,
            Some("worktree contains modified or untracked files"),
        )
        .unwrap();
    let failed_notes: Vec<_> = k
        .timeline_texts(&done)
        .into_iter()
        .filter(|t| t.contains("工作区清理失败") && t.contains("modified or untracked"))
        .collect();
    assert_eq!(failed_notes.len(), 1, "{failed_notes:?}");
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

#[test]
fn a_session_bound_to_a_directory_it_only_opens_gets_its_worktree_before_it_is_bound() {
    // an item first prepared by its backend as an existing directory keeps that plan: the
    // backend opens it right away, so the core makes a worktree tidied since before it binds
    let k = kit(true);
    k.rt.st().binds_existing = true;
    let (r, p) = project(&k);
    let id = k.item("run", "执行", Some(&p));
    let (branch, wt) = branch_with(&r, &k.core.config().worktrees, &id, "x.txt", "x\n");
    k.db()
        .item_patch(
            &id,
            patch![cwd: wt.to_string_lossy().to_string(), branch: branch.as_str()],
        )
        .unwrap();
    g(&r, &["worktree", "remove", &wt.to_string_lossy()]);
    k.core.tx().start_session(&id, Some("继续"), None).unwrap();
    assert_eq!(k.rt.st().bound_with_dir, vec![true], "made before the bind");
    assert!(wt.join("x.txt").exists());
}

#[test]
fn a_worktree_the_core_could_not_make_is_not_left_named_and_a_retry_still_waits_for_disk() {
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
        st.binds_worktrees = false;
    }
    let (_, p) = project(&k);
    let id = k.item("run", "定义", Some(&p));
    let wt = k.core.config().worktrees.join(&id);
    assert!(k.core.tx().start_session(&id, None, None).is_err());
    let i = k.db().item(&id).unwrap().unwrap();
    assert!(i.cwd.as_deref().is_none_or(str::is_empty), "{:?}", i.cwd);
    assert!(i.branch.as_deref().is_none_or(str::is_empty));
    assert!(
        k.db()
            .sessions_of_item(&id)
            .unwrap()
            .iter()
            .all(|s| s.status == "ended")
    );
    // continuing it later is still a new worktree below the floor: refused, not made
    k.core.tx().ensure_cwd(&id).unwrap();
    assert!(k.core.tx().start_session(&id, None, None).is_err());
    assert!(!wt.exists());
}

#[test]
fn a_workspace_binding_conflict_retires_the_session_before_continue() {
    let k = kit(true);
    let (_, p) = project(&k);
    let i = create(&k, &p);
    let old = k.db().sessions_of_item(&i.id).unwrap().pop().unwrap();
    k.core
        .workspace_binding_failed(&old.id, "workspace changed; continue in a new session")
        .unwrap();
    assert_eq!(k.db().session(&old.id).unwrap().unwrap().status, "ended");
    let d = k
        .db()
        .decisions_of_item(&i.id)
        .unwrap()
        .into_iter()
        .find(|d| d.kind == "fail" && d.status == steve_types::DecisionStatus::Open)
        .unwrap();
    k.core.resolve(&d.id, "continue", "", "me").unwrap();
    let sessions = k.db().sessions_of_item(&i.id).unwrap();
    assert_eq!(sessions.len(), 2);
    assert_ne!(sessions.last().unwrap().id, old.id);
    k.core
        .workspace_binding_failed(&old.id, "late conflict")
        .unwrap();
    assert!(!k.open_kinds(&i.id).contains(&"fail".into()));
}

#[test]
fn separate_installations_never_take_another_worktrees_branch() {
    let k = kit(true);
    let (repo, p) = project(&k);
    let i = create(&k, &p);
    let other = k.home().join("another-installation-worktree");
    let old_branch = format!("steve/{}", i.id.to_lowercase());
    node_makes(&repo, &other, &old_branch);
    std::fs::write(other.join("uncommitted.txt"), "keep this work").unwrap();
    let plan = k.core.tx().planned_worktree(&i).unwrap().unwrap();
    assert_ne!(plan.1, old_branch);
    assert_eq!(plan.1, k.core.tx().planned_worktree(&i).unwrap().unwrap().1);
    assert_eq!(
        std::fs::read_to_string(other.join("uncommitted.txt")).unwrap(),
        "keep this work"
    );
    assert_eq!(g(&other, &["branch", "--show-current"]), old_branch);
}

#[test]
fn a_worktree_in_a_unicode_directory_keeps_its_own_branch() {
    let k = kit(true);
    let (r, p) = project(&k);
    let i = create(&k, &p);
    let target = k.home().join("workspaces with spaces/任务").join(&i.id);
    let branch = i.branch.clone().unwrap();
    node_makes(&r, &target, &branch);
    assert_eq!(crate::worktrees::worktree_branch(&i, &r, &target), branch);
}

fn attach_merged_worktree(
    k: &Kit,
    r: &Path,
    project_id: &str,
    item_id: &str,
    file: &str,
) -> (String, PathBuf) {
    let (branch, wt) = branch_with(r, &k.core.config().worktrees, item_id, file, "ok\n");
    g(r, &["merge", "-q", "--ff-only", &branch]);
    k.db()
        .item_patch(
            item_id,
            patch![
                project_id: project_id,
                cwd: wt.to_string_lossy().to_string(),
                branch: branch.as_str()
            ],
        )
        .unwrap();
    (branch, wt)
}

#[test]
fn every_close_path_stops_sessions_and_cleans_worktree_and_merged_branch() {
    let k = Kit::new();
    let (r, p) = project(&k);

    // 1. Owner finish (with an active idle session whose release starts pending)
    let owner_id = k.item("run", "执行", Some(&p));
    let (owner_br, owner_wt) = attach_merged_worktree(&k, &r, &p, &owner_id, "owner.txt");
    let owner_sess = k.session(&owner_id, "idle");
    k.rt.st().release_answers.insert(
        owner_sess.clone(),
        crate::Released::Pending("op_rel_owner".into()),
    );
    k.finish(&owner_id).unwrap();
    assert_eq!(k.phase(&owner_id), "closed");
    assert_eq!(
        k.pump_outbox().retried,
        1,
        "waits until session is released"
    );
    assert!(owner_wt.exists());
    assert!(k.rt.st().released.contains(&owner_sess));
    k.rt.st().release_answers.remove(&owner_sess);
    assert!(k.pump_outbox_at(steve_store::now() + 60_000).done >= 1);
    assert!(!owner_wt.exists(), "owner finish cleaned worktree");
    assert!(g(&r, &["branch", "--list", &owner_br]).is_empty());

    // 2. Cancel / Drop
    let drop_id = k.item("run", "执行", Some(&p));
    let (drop_br, drop_wt) = attach_merged_worktree(&k, &r, &p, &drop_id, "drop.txt");
    let drop_sess = k.session(&drop_id, "idle");
    k.core
        .item_action(&drop_id, crate::ItemAction::Drop, "me")
        .unwrap();
    assert_eq!(k.phase(&drop_id), "closed");
    assert!(k.pump_outbox().done >= 1);
    assert!(!drop_wt.exists(), "drop cleaned worktree");
    assert!(g(&r, &["branch", "--list", &drop_br]).is_empty());
    assert!(k.rt.st().released.contains(&drop_sess));

    // 3. Fail
    let fail_id = k.item("run", "执行", Some(&p));
    let (fail_br, fail_wt) = attach_merged_worktree(&k, &r, &p, &fail_id, "fail.txt");
    let _fail_sess = k.session(&fail_id, "idle");
    let expected = k.db().item(&fail_id).unwrap().unwrap().cas_revision;
    k.core
        .lifecycle_command(
            &fail_id,
            "fail",
            1,
            expected,
            Some("out of scope"),
            None,
            None,
            "me",
        )
        .unwrap();
    assert_eq!(k.phase(&fail_id), "closed");
    assert!(k.pump_outbox().done >= 1);
    assert!(!fail_wt.exists(), "fail cleaned worktree");
    assert!(g(&r, &["branch", "--list", &fail_br]).is_empty());

    // 4. Automatic completion
    let auto_id = k.item("run", "执行", Some(&p));
    let (auto_br, auto_wt) = attach_merged_worktree(&k, &r, &p, &auto_id, "auto.txt");
    let auto_sess = k.session(&auto_id, "idle");
    k.core
        .tx()
        .request_delivery_close(
            &auto_id,
            "steve",
            crate::cycle::ClosePolicy::Automatic,
            true,
            "automatic",
            None,
            vec![],
        )
        .unwrap();
    assert_eq!(k.phase(&auto_id), "closed");
    assert!(k.pump_outbox().done >= 1);
    assert!(!auto_wt.exists(), "automatic completion cleaned worktree");
    assert!(g(&r, &["branch", "--list", &auto_br]).is_empty());
    assert!(k.rt.st().released.contains(&auto_sess));
}

#[test]
fn closed_item_removes_clean_worktree_and_preserves_and_marks_unmerged_branch() {
    let k = Kit::new();
    let (r, p) = project(&k);
    let id = k.item("run", "执行", Some(&p));
    let (branch, wt) = branch_with(
        &r,
        &k.core.config().worktrees,
        &id,
        "unmerged.txt",
        "work\n",
    );
    let sha = g(
        &r,
        &["rev-parse", "--short", &format!("refs/heads/{branch}")],
    );
    k.db()
        .item_patch(
            &id,
            patch![cwd: wt.to_string_lossy().to_string(), branch: branch.as_str()],
        )
        .unwrap();
    k.finish(&id).unwrap();
    assert!(k.pump_outbox().done >= 1);
    assert!(!wt.exists(), "clean worktree is removed even when unmerged");
    assert_eq!(
        g(
            &r,
            &["rev-parse", "--short", &format!("refs/heads/{branch}")]
        ),
        sha,
        "unmerged branch stays intact"
    );
    let tl = k.timeline_texts(&id);
    assert!(
        tl.iter()
            .any(|t| t.contains("未合并分支") && t.contains(&branch) && t.contains(&sha)),
        "preserved unmerged branch must be noted on item: {tl:?}"
    );
    // Reopening via a new cycle recreates the worktree from the preserved branch.
    let rev = k.db().item(&id).unwrap().unwrap().cas_revision;
    k.core
        .lifecycle_command(&id, "new_cycle", 1, rev, None, Some("继续"), None, "me")
        .unwrap();
    assert!(wt.join("unmerged.txt").exists());
}

#[test]
fn worktree_cleanup_failure_is_displayed_on_item_and_not_swallowed() {
    let k = Kit::new();
    let (r, p) = project(&k);
    let id = k.item("run", "执行", Some(&p));
    let (branch, wt) = branch_with(&r, &k.core.config().worktrees, &id, "dirty.txt", "a\n");
    fs::write(wt.join("uncommitted.txt"), "keep me").unwrap();
    k.db()
        .item_patch(
            &id,
            patch![cwd: wt.to_string_lossy().to_string(), branch: branch.as_str()],
        )
        .unwrap();
    k.finish(&id).unwrap();
    assert!(k.pump_outbox().done >= 1);
    assert!(wt.join("uncommitted.txt").exists(), "dirty worktree stays");
    let tl = k.timeline_texts(&id);
    assert!(
        tl.iter().any(|t| t.contains("工作区清理失败")),
        "cleanup failure must be shown on item: {tl:?}"
    );
    // Running tidy again does not spam duplicate identical failure lines.
    assert!(k.core.tx().tidy_branch_now(&id).unwrap());
    let fail_count = k
        .timeline_texts(&id)
        .iter()
        .filter(|t| t.contains("工作区清理失败"))
        .count();
    assert_eq!(fail_count, 1);
}

#[test]
fn backfill_closed_worktrees_cleans_leftovers_enqueues_busy_and_is_idempotent() {
    let k = Kit::new();
    let (r, p) = project(&k);

    // 1. Closed + merged leftover
    let merged = k.item("done", "收尾", Some(&p));
    let (m_br, m_wt) = attach_merged_worktree(&k, &r, &p, &merged, "m.txt");

    // 2. Closed (dropped) + unmerged leftover
    let unmerged = k.item("drop", "收尾", Some(&p));
    let (u_br, u_wt) = branch_with(&r, &k.core.config().worktrees, &unmerged, "u.txt", "u\n");
    k.db()
        .item_patch(
            &unmerged,
            patch![cwd: u_wt.to_string_lossy().to_string(), branch: u_br.as_str()],
        )
        .unwrap();

    // 3. Closed + dirty leftover
    let dirty = k.item("done", "收尾", Some(&p));
    let (d_br, d_wt) = branch_with(&r, &k.core.config().worktrees, &dirty, "d.txt", "d\n");
    fs::write(d_wt.join("scratch.tmp"), "uncommitted").unwrap();
    k.db()
        .item_patch(
            &dirty,
            patch![cwd: d_wt.to_string_lossy().to_string(), branch: d_br.as_str()],
        )
        .unwrap();

    // 4. Closed + pending-release session (must be enqueued to WorkTidy outbox)
    let pending = k.item("done", "收尾", Some(&p));
    let (p_br, p_wt) = attach_merged_worktree(&k, &r, &p, &pending, "p.txt");
    let p_sess = k.session(&pending, "idle");
    k.rt.st()
        .release_answers
        .insert(p_sess.clone(), crate::Released::Pending("op_rel_bf".into()));

    // 5. Open item must NOT be touched by backfill
    let open = k.item("run", "执行", Some(&p));
    let (_, open_wt) = branch_with(&r, &k.core.config().worktrees, &open, "open.txt", "open\n");
    k.db()
        .item_patch(&open, patch![cwd: open_wt.to_string_lossy().to_string()])
        .unwrap();

    let first = k.core.backfill_closed_worktrees().unwrap();
    assert!(!first.skipped);
    assert_eq!(first.scanned, 4, "{first:?}");
    assert_eq!(first.cleaned, 2, "{first:?}");
    assert_eq!(first.enqueued, 1, "{first:?}");
    assert_eq!(first.failed, 1, "{first:?}");

    assert!(!m_wt.exists());
    assert!(g(&r, &["branch", "--list", &m_br]).is_empty());
    assert!(!u_wt.exists());
    g(&r, &["rev-parse", "--verify", &u_br]);
    assert!(
        k.timeline_texts(&unmerged)
            .iter()
            .any(|t| t.contains("未合并分支") && t.contains(&u_br))
    );
    assert!(d_wt.exists());
    assert!(
        k.timeline_texts(&dirty)
            .iter()
            .any(|t| t.contains("工作区清理失败"))
    );
    assert!(open_wt.exists());
    assert!(p_wt.exists());

    // Once the pending session releases, WorkTidy completes the enqueued backfill item.
    k.rt.st().release_answers.remove(&p_sess);
    assert!(k.pump_outbox().done >= 1);
    assert!(!p_wt.exists());
    assert!(g(&r, &["branch", "--list", &p_br]).is_empty());

    // Second invocation is a strict no-op (does not rescan even if a new closed leftover is seeded).
    let later = k.item("done", "收尾", Some(&p));
    let (_, later_wt) = attach_merged_worktree(&k, &r, &p, &later, "later.txt");
    let second = k.core.backfill_closed_worktrees().unwrap();
    assert!(second.skipped, "marker makes backfill strictly one-time");
    assert_eq!(second.scanned, 0);
    assert!(later_wt.exists(), "no rescan occurred on second boot");
}
