//! Task workspaces: closed items give their caches and safe worktrees back, open and busy ones are
//! left alone, a worktree with a process inside stays, an interrupted pass is finished by the next, and no
//! new worktree is made below the disk floor.

use std::fs;
use std::path::{Path, PathBuf};

use crate::CreateItem;
use crate::git::tests::{branch_with, g, repo};
use crate::patch;
use crate::repo::WS;
use crate::testkit::Kit;
use crate::workspace::{self, DiskPolicy, purge, trash_dir};

/// Steve's repo in miniature: `target/` and `node_modules/` ignored, on main.
fn project(k: &Kit) -> (PathBuf, String) {
    let r = repo(k.home());
    fs::write(r.join(".gitignore"), "target/\nnode_modules/\n").unwrap();
    g(&r, &["add", "."]);
    g(&r, &["commit", "-q", "-m", "ignore caches"]);
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

/// An item of `status` on its own worktree with one commit, and a Rust and a node cache in it (1 MB each).
fn item_with_caches(k: &Kit, r: &Path, project: &str, status: &str) -> (String, String, PathBuf) {
    let stage = if status == "run" { "执行" } else { "收尾" };
    let id = k.item(status, stage, Some(project));
    let (branch, wt) = branch_with(r, &k.core.config().worktrees, &id, "x.txt", "x\n");
    k.db()
        .item_patch(
            &id,
            patch![cwd: wt.to_string_lossy().to_string(), branch: branch.as_str()],
        )
        .unwrap();
    caches(&wt);
    (id, branch, wt)
}

fn caches(wt: &Path) {
    let t = wt.join("target/debug");
    fs::create_dir_all(&t).unwrap();
    fs::write(
        wt.join("target/CACHEDIR.TAG"),
        "Signature: 8a477f597d28d172789f06886806bc55",
    )
    .unwrap();
    fs::write(t.join("big"), vec![7u8; 1 << 20]).unwrap();
    fs::create_dir_all(wt.join("node_modules/x")).unwrap();
    fs::write(wt.join("node_modules/x/index.js"), vec![1u8; 1 << 20]).unwrap();
}

fn freed_of(r: &workspace::Reclaimed, item: &str) -> u64 {
    r.freed
        .iter()
        .filter(|f| f.item == item)
        .map(|f| f.bytes)
        .sum()
}

#[test]
fn cache_dirs_are_ignored_untracked_and_marked_or_known() {
    let k = Kit::new();
    let (r, _) = project(&k);
    let wt = k.core.config().worktrees.join("T");
    g(
        &r,
        &[
            "worktree",
            "add",
            "-q",
            "-b",
            "t",
            &wt.to_string_lossy(),
            "main",
        ],
    );
    caches(&wt);
    /* a nested cache, a tagged folder git does not ignore, a tracked node_modules */
    fs::create_dir_all(wt.join("web/node_modules/y")).unwrap();
    fs::create_dir_all(wt.join("keep")).unwrap();
    fs::write(wt.join("keep/CACHEDIR.TAG"), "x").unwrap();
    fs::create_dir_all(wt.join("vendor/node_modules")).unwrap();
    fs::write(wt.join("vendor/node_modules/a.js"), "a").unwrap();
    g(&wt, &["add", "-f", "vendor/node_modules/a.js"]);
    let found: Vec<String> = workspace::cache_dirs(&wt)
        .iter()
        .map(|p| p.strip_prefix(&wt).unwrap().to_string_lossy().to_string())
        .collect();
    assert_eq!(found, vec!["node_modules", "target", "web/node_modules"]);
}

#[test]
fn a_closed_item_gives_back_its_caches_and_a_landed_worktree_keeps_its_branch() {
    let k = Kit::new();
    let (r, p) = project(&k);
    let (done, branch, wt) = item_with_caches(&k, &r, &p, "done");
    let (open, _, open_wt) = item_with_caches(&k, &r, &p, "run");
    /* landed by a rebase merge: same change, new commit id (not an ancestor of main) */
    fs::write(r.join("m.txt"), "m\n").unwrap();
    g(&r, &["add", "m.txt"]);
    g(&r, &["commit", "-q", "-m", "main moved on"]);
    g(&r, &["cherry-pick", &branch]);
    assert!(
        crate::git::git_test(&r, &["merge-base", "--is-ancestor", &branch, "main"]) == Ok(false)
    );

    let res = k.core.reclaim_workspaces().unwrap();
    assert!(freed_of(&res, &done) >= 2 << 20, "{res:?}");
    assert!(!wt.exists(), "the landed item's worktree is given back");
    assert_eq!(res.removed, vec![done.clone()]);
    g(&r, &["rev-parse", "--verify", &branch]);
    assert!(
        open_wt.join("target/debug/big").exists(),
        "an open item keeps its caches"
    );
    assert_eq!(freed_of(&res, &open), 0);
    assert!(
        fs::read_dir(trash_dir(&k.core.config().worktrees))
            .unwrap()
            .next()
            .is_none()
    );
    let tl = k.timeline_texts(&done);
    assert!(
        tl.iter().any(|t| t.starts_with("删除了工作区")
            && t.contains("未合并分支")
            && t.contains("已保留")),
        "{tl:?}"
    );
    assert!(
        tl.iter()
            .any(|t| t.contains("回收了可重建的缓存 node_modules、target")),
        "{tl:?}"
    );
    /* continuing brings the worktree back from the kept branch */
    k.core.tx().ensure_cwd(&done).unwrap();
    assert!(wt.join("x.txt").exists());
}

/// #180: a toolchain's output moved into the item's `.steve/build` (cargo's `target` through
/// `CARGO_TARGET_DIR`) has neither a known cache name nor a `CACHEDIR.TAG`. A closed item gives it
/// back even while its worktree stays (unmerged work); its sources, its commit and the build
/// directory's own `.gitignore` stay. An open item keeps it.
#[test]
fn a_closed_item_gives_back_its_toolchain_outputs_even_when_its_worktree_stays() {
    let k = Kit::new();
    let (r, p) = project(&k);
    let build = |wt: &Path| {
        let target = wt
            .join(steve_toolchains::ITEM_BUILD_DIR)
            .join("cargo/target/debug");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("big"), vec![7u8; 1 << 20]).unwrap();
        fs::write(
            wt.join(steve_toolchains::ITEM_BUILD_DIR).join(".gitignore"),
            "*\n",
        )
        .unwrap();
    };
    let (done, branch, wt) = item_with_caches(&k, &r, &p, "done");
    let (open, _, open_wt) = item_with_caches(&k, &r, &p, "run");
    // Uncommitted work keeps the worktree in place while rebuildable toolchain outputs still go.
    fs::write(wt.join("notes.txt"), "uncommitted").unwrap();
    build(&wt);
    build(&open_wt);
    let built = |wt: &Path| {
        wt.join(steve_toolchains::ITEM_BUILD_DIR)
            .join("cargo/target/debug/big")
            .exists()
    };

    let res = k.core.reclaim_workspaces().unwrap();
    assert!(freed_of(&res, &done) >= 3 << 20, "{res:?}");
    assert!(wt.join("x.txt").exists(), "dirty work keeps its worktree");
    assert!(!built(&wt), "{res:?}");
    assert!(
        wt.join(steve_toolchains::ITEM_BUILD_DIR)
            .join(".gitignore")
            .is_file()
    );
    g(&r, &["rev-parse", "--verify", &branch]);
    assert!(built(&open_wt), "an open item keeps its build");
    assert_eq!(freed_of(&res, &open), 0);
    let tl = k.timeline_texts(&done);
    assert!(
        tl.iter().any(|t| t.contains(".steve/build/cargo")),
        "{tl:?}"
    );
}

#[test]
fn clean_unmerged_closed_worktree_is_removed_keeping_branch_and_dirty_work_stays() {
    let k = Kit::new();
    let (r, p) = project(&k);
    let (done, branch, wt) = item_with_caches(&k, &r, &p, "done");
    let (dirty, _, dirty_wt) = item_with_caches(&k, &r, &p, "drop");
    fs::write(dirty_wt.join("notes.txt"), "not committed").unwrap();

    // Only inspect the environment after an assertion fails: these Git calls and the process
    // sample must not change the timing of the reclaim pass we are trying to diagnose.
    let diagnose = |res: &workspace::Reclaimed| {
        let root = &k.core.config().worktrees;
        let root = root.canonicalize().unwrap_or_else(|_| root.clone());
        let procs = workspace::process_dirs().map(|dirs| {
            dirs.into_iter()
                .filter(|path| path.starts_with(&root))
                .collect::<Vec<_>>()
        });
        let describe = |label: &str, path: &Path| {
            format!(
                "{label}: path={path:?}, exists={}, notes={:?}, x_exists={}, \
                 node_modules_exists={}, target_exists={}, sampled_in_use={:?}, \
                 git_status={:?}, git_check_ignore={:?}, git_ls_files={:?}",
                path.exists(),
                fs::read_to_string(path.join("notes.txt")),
                path.join("x.txt").exists(),
                path.join("node_modules").exists(),
                path.join("target").exists(),
                procs.as_deref().map(|p| workspace::in_use(path, p)),
                crate::git::git_read(
                    path,
                    &[
                        "status",
                        "--porcelain",
                        "--untracked-files=all",
                        "--ignored"
                    ],
                ),
                crate::git::git_read(
                    path,
                    &[
                        "check-ignore",
                        "-v",
                        "--",
                        "notes.txt",
                        "node_modules/",
                        "target/"
                    ],
                ),
                crate::git::git_read(
                    path,
                    &["ls-files", "--", "notes.txt", "node_modules", "target"],
                ),
            )
        };
        format!(
            "reclaim result={res:#?}\n{}\n{}\n\
             process_dirs under {root:?}, sampled AFTER failure \
             (not the reclaim pass's snapshot)={procs:#?}",
            describe("unmerged", &wt),
            describe("dirty", &dirty_wt),
        )
    };

    let res = k.core.reclaim_workspaces().unwrap();
    assert_eq!(res.removed, vec![done.clone()], "{}", diagnose(&res));
    assert!(!wt.exists(), "{}", diagnose(&res));
    assert_eq!(
        g(&r, &["rev-list", "--count", &format!("main..{branch}")]),
        "1",
        "the unmerged commit stays on its branch"
    );
    assert!(dirty_wt.join("notes.txt").exists(), "{}", diagnose(&res));
    assert!(
        !dirty_wt.join("node_modules").exists(),
        "{}",
        diagnose(&res)
    );
    let reason = |id: &str| {
        res.kept
            .iter()
            .find(|x| x.item == id)
            .map(|x| x.reason.clone())
    };
    assert!(
        reason(&dirty).is_some_and(|r| r.contains("未提交")),
        "{}",
        diagnose(&res)
    );
    assert!(
        k.timeline_texts(&dirty)
            .iter()
            .any(|t| t.contains("工作区清理失败")),
        "dirty cleanup failure must be shown on the item: {:?}",
        k.timeline_texts(&dirty)
    );
}

#[test]
fn busy_sessions_and_running_processes_keep_everything() {
    let k = Kit::new();
    let (r, p) = project(&k);
    let (busy, _, busy_wt) = item_with_caches(&k, &r, &p, "done");
    let s = k.session(&busy, "running");
    k.rt.st().turning.insert(s);
    let (served, _, served_wt) = item_with_caches(&k, &r, &p, "done");
    let mut child = steve_runtime::testkit::process::Command::new("sleep")
        .arg("30")
        .current_dir(&served_wt)
        .spawn()
        .unwrap();
    let res = k.core.reclaim_workspaces();
    let _ = child.kill();
    let _ = child.wait();
    let res = res.unwrap();
    assert!(busy_wt.join("target/debug/big").exists());
    assert!(served_wt.join("node_modules/x/index.js").exists());
    if workspace::process_dirs().is_some() {
        assert!(
            res.kept
                .iter()
                .any(|x| x.item == served && x.reason.contains("进程"))
        );
    }
    assert!(res.freed.is_empty() && res.removed.is_empty(), "{res:?}");
}

/// B4: a session whose harness is still being let go (a Node's release on its way) keeps the
/// worktree: the tidy says it cannot decide yet (WorkTidy asks again), and tidies once it is gone.
#[test]
fn a_release_on_its_way_keeps_the_worktree_until_it_is_done() {
    let k = Kit::new();
    let (r, p) = project(&k);
    let (done, branch, wt) = item_with_caches(&k, &r, &p, "done");
    g(&r, &["merge", "-q", "--ff-only", &branch]);
    let s = k.session(&done, "ended");
    k.rt.st()
        .release_answers
        .insert(s.clone(), crate::Released::Pending("op_release_1".into()));
    assert!(
        !k.core.tx().tidy_branch_now(&done).unwrap(),
        "asked again later"
    );
    assert!(wt.join("x.txt").exists(), "the worktree is kept");
    k.rt.st().release_answers.remove(&s);
    assert!(k.core.tx().tidy_branch_now(&done).unwrap());
    assert!(!wt.exists(), "tidied once nothing runs in it");
}

#[test]
fn an_interrupted_pass_is_finished_by_the_next() {
    let k = Kit::new();
    let (r, p) = project(&k);
    let (id, _, wt) = item_with_caches(&k, &r, &p, "done");
    let trash = trash_dir(&k.core.config().worktrees);
    /* the pass got as far as moving the caches, then the Hub stopped */
    let moved = workspace::to_trash(&trash, &id, &wt, &workspace::cache_dirs(&wt));
    assert_eq!(moved, vec!["node_modules", "target"]);
    assert!(!wt.join("target").exists() && trash.read_dir().unwrap().count() == 1);
    /* and a worktree folder deleted halfway through a removal */
    fs::remove_dir_all(&wt).unwrap();

    let res = k.core.reclaim_workspaces().unwrap();
    assert!(freed_of(&res, &id) >= 2 << 20, "{res:?}");
    assert!(trash.read_dir().unwrap().next().is_none());
    assert!(
        !g(&r, &["worktree", "list"]).contains(&wt.to_string_lossy().to_string()),
        "git forgets the half-removed worktree"
    );
    assert!(
        purge(&trash).is_empty(),
        "nothing left: purging again does nothing"
    );
}

#[test]
fn under_pressure_idle_open_items_give_back_their_caches_oldest_first() {
    let k = Kit::with(|c| {
        c.disk = DiskPolicy {
            floor: 0,
            low: u64::MAX,
            idle_ms: 3_600_000,
        }
    });
    let (r, p) = project(&k);
    let (idle, _, idle_wt) = item_with_caches(&k, &r, &p, "run");
    let (fresh, _, fresh_wt) = item_with_caches(&k, &r, &p, "run");
    steve_store::core_fixture::item(
        k.db().store(),
        &idle,
        steve_store::core_fixture::ItemState::ShiftUpdated(-7_200_000),
    );
    let res = k.core.reclaim_workspaces().unwrap();
    if workspace::process_dirs().is_some() {
        assert!(freed_of(&res, &idle) >= 2 << 20, "{res:?}");
        assert!(!idle_wt.join("target").exists() && idle_wt.join("x.txt").exists());
    }
    assert_eq!(freed_of(&res, &fresh), 0);
    assert!(fresh_wt.join("target/debug/big").exists());
    assert!(res.removed.is_empty(), "open items keep their worktree");
}

#[test]
fn below_the_floor_no_new_worktree_is_made_but_an_existing_one_continues() {
    let k = Kit::with(|c| {
        c.disk = DiskPolicy {
            floor: u64::MAX,
            low: u64::MAX,
            idle_ms: i64::MAX,
        }
    });
    let (r, p) = project(&k);
    let i = k
        .core
        .create_item(CreateItem {
            ws: WS.into(),
            goal: "g".into(),
            title: "新事项".into(),
            bot: Some("dev".into()),
            project: Some(Some(p.clone())),
            ..Default::default()
        })
        .unwrap();
    assert!(!k.core.config().worktrees.join(&i.id).exists());
    let cards = k.db().decisions_of_item(&i.id).unwrap();
    assert!(
        cards
            .iter()
            .any(|d| d.kind == "fail" && d.question.contains("磁盘空间不足")),
        "{:?}",
        cards.iter().map(|d| &d.question).collect::<Vec<_>>()
    );

    /* an item whose worktree is there already is not stopped */
    let (id, _, wt) = item_with_caches(&k, &r, &p, "run");
    let s = k.core.tx().start_session(&id, Some("继续"), None).unwrap();
    assert_eq!(s.cwd, wt.to_string_lossy());
    /* and one whose worktree was given back gets its sources again (megabytes, not a new build) */
    g(
        &r,
        &["worktree", "remove", "--force", &wt.to_string_lossy()],
    );
    k.core.tx().ensure_cwd(&id).unwrap();
    assert!(wt.join("x.txt").exists());
}

#[test]
fn the_report_counts_worktrees_caches_and_orphans() {
    let k = Kit::new();
    let (r, p) = project(&k);
    let (id, _, _) = item_with_caches(&k, &r, &p, "run");
    fs::create_dir_all(k.core.config().worktrees.join("OLD-1")).unwrap();
    fs::write(k.core.config().worktrees.join("OLD-1/f"), vec![0u8; 4096]).unwrap();
    let rep = k.core.disk_report().unwrap();
    let e = rep.entries.iter().find(|e| e.item == id).unwrap();
    assert!(
        e.exists && e.cache_bytes >= 2 << 20 && e.bytes >= e.cache_bytes,
        "{e:?}"
    );
    assert_eq!(e.caches, vec!["node_modules", "target"]);
    assert_eq!(rep.orphans.len(), 1);
    assert!(rep.orphans[0].path.ends_with("OLD-1"));
    assert!(rep.free.is_some() && rep.git.is_some());
}

/// The space really comes back: 2 GiB written into a closed item's `target/` show up as free space on the
/// volume after one pass. Run by hand (`--ignored`): other builds writing to the same disk make it noisy.
#[test]
#[ignore]
fn reclaiming_frees_the_disk_space() {
    use std::io::Write;
    let k = Kit::new();
    let (r, p) = project(&k);
    let (id, _, wt) = item_with_caches(&k, &r, &p, "done");
    let chunk: Vec<u8> = (0..1u32 << 20).map(|i| (i % 251) as u8).collect();
    for n in 0..2 {
        let mut f = fs::File::create(wt.join(format!("target/debug/blob{n}"))).unwrap();
        for _ in 0..1024 {
            f.write_all(&chunk).unwrap();
        }
        f.sync_all().unwrap();
    }
    let root = k.core.config().worktrees.clone();
    let before = workspace::space(&root).unwrap().0;
    let res = k.core.reclaim_workspaces().unwrap();
    let after = workspace::space(&root).unwrap().0;
    let gained = after.saturating_sub(before);
    println!(
        "freed (counted) {} · free before {} · after {} · gained {}",
        workspace::human(freed_of(&res, &id)),
        workspace::human(before),
        workspace::human(after),
        workspace::human(gained)
    );
    assert!(freed_of(&res, &id) >= 2 << 30);
    assert!(gained >= 3 << 29, "at least 1.5 GiB of the 2 GiB came back");
}

#[test]
fn one_pass_at_a_time() {
    use std::sync::atomic::Ordering;

    let k = Kit::new();
    // Trash is already eligible for deletion. A live worktree may legitimately be kept by the
    // process or Git guards, which say nothing about whether the reclaim gate was acquired.
    let trash = trash_dir(&k.core.config().worktrees);
    let cache = trash.join("closed-item@1/0-target");
    fs::create_dir_all(&cache).unwrap();
    let payload = cache.join("rebuildable");
    fs::write(&payload, vec![7u8; 4096]).unwrap();

    k.core.0.reclaiming.store(true, Ordering::SeqCst);
    let skipped = k.core.reclaim_workspaces().unwrap();
    assert!(skipped.skipped && skipped.freed.is_empty(), "{skipped:?}");
    assert!(skipped.removed.is_empty(), "{skipped:?}");
    assert!(
        k.core.0.reclaiming.load(Ordering::SeqCst),
        "a skipped caller must not release the current owner's gate: {skipped:?}"
    );
    assert!(
        payload.exists(),
        "a skipped caller must not purge the trash: {skipped:?}"
    );

    k.core.0.reclaiming.store(false, Ordering::SeqCst);
    let reclaimed = k.core.reclaim_workspaces().unwrap();
    assert!(!reclaimed.skipped, "{reclaimed:?}");
    assert!(
        !k.core.0.reclaiming.load(Ordering::SeqCst),
        "a completed pass must release its gate: {reclaimed:?}"
    );
    assert!(
        !cache.exists() && trash.read_dir().unwrap().next().is_none(),
        "the admitted pass must actually purge eligible trash: {reclaimed:?}"
    );
    assert!(
        reclaimed
            .freed
            .iter()
            .any(|f| { f.item == "closed-item" && f.caches == ["target"] && f.bytes > 0 }),
        "the purge must report the cache it deleted: {reclaimed:?}"
    );
}

/// An item without a project in its scratch directory, with a node and a Rust cache (1 MB each), its own
/// files, and a plain `build/` nothing marks as a cache.
fn scratch_item(k: &Kit, status: &str) -> (String, PathBuf) {
    let stage = if status == "run" { "执行" } else { "收尾" };
    let id = k.item(status, stage, None);
    let dir = k.core.config().scratch.join(&id);
    fs::create_dir_all(&dir).unwrap();
    k.db()
        .item_patch(&id, patch![cwd: dir.to_string_lossy().to_string()])
        .unwrap();
    caches(&dir);
    fs::write(dir.join("report.md"), "结论\n").unwrap();
    fs::create_dir_all(dir.join("build")).unwrap();
    fs::write(dir.join("build/out.bin"), vec![3u8; 4096]).unwrap();
    (id, dir)
}

fn pressure(low: u64) -> Kit {
    Kit::with(move |c| {
        c.disk = DiskPolicy {
            floor: 0,
            low,
            idle_ms: i64::MAX,
        }
    })
}

#[test]
fn under_pressure_an_ended_scratch_item_gives_back_only_its_marked_caches() {
    let k = pressure(u64::MAX);
    let (ended, dir) = scratch_item(&k, "done");
    let (open, open_dir) = scratch_item(&k, "run");
    let res = k.core.reclaim_workspaces().unwrap();
    if workspace::process_dirs().is_some() {
        assert!(freed_of(&res, &ended) >= 2 << 20, "{res:?}");
        assert!(!dir.join("target").exists() && !dir.join("node_modules").exists());
    }
    assert!(dir.join("report.md").exists(), "its own files stay");
    assert!(
        dir.join("build/out.bin").exists(),
        "an unmarked directory stays"
    );
    assert_eq!(
        freed_of(&res, &open),
        0,
        "an open item keeps its scratch caches"
    );
    assert!(open_dir.join("target/debug/big").exists());
}

#[test]
fn without_pressure_scratch_caches_stay() {
    let k = pressure(0);
    let (ended, dir) = scratch_item(&k, "done");
    let res = k.core.reclaim_workspaces().unwrap();
    assert_eq!(freed_of(&res, &ended), 0, "{res:?}");
    assert!(dir.join("target/debug/big").exists() && dir.join("node_modules/x/index.js").exists());
}

#[test]
fn the_report_lists_scratch_directories_with_their_caches() {
    let k = pressure(0);
    let (id, _) = scratch_item(&k, "done");
    let rep = k.core.disk_report().unwrap();
    assert!(rep.entries.iter().all(|e| e.item != id), "not a worktree");
    let e = rep.scratch.iter().find(|e| e.item == id).unwrap();
    assert!(
        e.exists && e.cache_bytes >= 2 << 20 && e.bytes > e.cache_bytes,
        "{e:?}"
    );
    assert_eq!(e.caches, vec!["node_modules", "target"]);
}

#[test]
fn shared_disposable_predicate_preserves_reclaimer_depth_and_never_follows_links() {
    use steve_toolchains::{ITEM_BUILD_DIR, Toolchains};
    let root = tempfile::tempdir().unwrap();
    let name = Toolchains::builtin().disposable_names()[0].to_owned();
    let named = root.path().join(&name);
    let marked = root.path().join("marked");
    let tool_output = root.path().join(ITEM_BUILD_DIR).join("custom-tool");
    let nested_output = root
        .path()
        .join("nested")
        .join(ITEM_BUILD_DIR)
        .join("custom-tool");
    let deep = root.path().join("a/b/c/d/e/marked");
    let user = root.path().join("user-output");
    for path in [&named, &marked, &tool_output, &nested_output, &deep, &user] {
        fs::create_dir_all(path).unwrap();
    }
    for path in [&marked, &deep] {
        fs::write(path.join("CACHEDIR.TAG"), "marker").unwrap();
    }
    let link = root.path().join("cache-link");
    std::os::unix::fs::symlink(&marked, &link).unwrap();
    for path in [&named, &marked, &tool_output, &deep] {
        assert!(
            workspace::is_disposable_dir(path, None),
            "{}",
            path.display()
        );
    }
    assert!(workspace::is_disposable_dir(&nested_output, None));
    assert!(!workspace::is_disposable_dir(
        &nested_output,
        Some(root.path())
    ));
    assert!(!workspace::is_disposable_dir(&user, None));
    assert!(!workspace::is_disposable_dir(&link, None));
    let found = workspace::scratch_cache_dirs(root.path());
    for path in [&named, &marked, &tool_output] {
        assert!(found.contains(path));
    }
    assert!(
        !found.contains(&deep),
        "reclamation retains its depth limit"
    );
    assert!(!found.contains(&link));
    assert!(
        !found.contains(&nested_output),
        "only the workspace root's build namespace is reclaimed"
    );
}
