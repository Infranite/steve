//! lifecycle.ts, second half, and the git parts of the orchestrator: item worktrees, the rebase before a
//! release, and giving worktrees back. Nothing here forces anything: a conflicting rebase is aborted, a dirty
//! worktree stays, an unmerged branch stays.
//!
//! These run synchronously like `execFileSync` in TS, inside the critical section whose result they decide.

use crate::util::js_head;
use std::path::Path;
use std::process::{Command, Stdio};

/// `git -C dir args…`: trimmed stdout, or the trimmed stderr (≤300 chars) as the error.
pub fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = Command::new("git")
        .env_remove("STEVE_UPGRADE_CONTROL_TOKEN")
        .env_remove("STEVE_SUPERVISOR_PID")
        .env_remove("STEVE_UPGRADE_ID")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_EDITOR", "true")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    if out.status.success() {
        Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
    } else {
        let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
        let err = if err.is_empty() {
            format!("git {} failed", args.join(" "))
        } else {
            err
        };
        Err(js_head(&err, 300).to_string())
    }
}

/// A yes/no question git answers with its exit status (`merge-base --is-ancestor`, `rev-parse --verify
/// --quiet`): 0 is yes, 1 is no; anything else, or no git at all, is an error, never a no.
pub fn git_test(dir: &Path, args: &[&str]) -> Result<bool, String> {
    let out = Command::new("git")
        .env_remove("STEVE_UPGRADE_CONTROL_TOKEN")
        .env_remove("STEVE_SUPERVISOR_PID")
        .env_remove("STEVE_UPGRADE_ID")
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    match out.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => {
            let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
            Err(if err.is_empty() {
                format!("git {} failed", args.join(" "))
            } else {
                js_head(&err, 300).to_string()
            })
        }
    }
}

/// The merge base of `a` and `b`: `None` when they share no history (git says so with exit 1),
/// an error when git fails.
pub fn merge_base(dir: &Path, a: &str, b: &str) -> Result<Option<String>, String> {
    let out = Command::new("git")
        .env_remove("STEVE_UPGRADE_CONTROL_TOKEN")
        .env_remove("STEVE_SUPERVISOR_PID")
        .env_remove("STEVE_UPGRADE_ID")
        .arg("-C")
        .arg(dir)
        .args(["merge-base", a, b])
        .stdin(Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    match out.status.code() {
        Some(0) => Ok(Some(
            String::from_utf8_lossy(&out.stdout).trim().to_string(),
        )),
        Some(1) if out.stderr.is_empty() => Ok(None),
        _ => {
            let err = String::from_utf8_lossy(&out.stderr).trim().to_string();
            Err(if err.is_empty() {
                format!("git merge-base {a} {b} failed")
            } else {
                js_head(&err, 300).to_string()
            })
        }
    }
}

fn succeeds(dir: &Path, args: &[&str]) -> bool {
    git(dir, args).is_ok()
}

pub fn is_git(dir: &Path) -> bool {
    succeeds(dir, &["rev-parse", "--is-inside-work-tree"])
}

/// The item's worktree at `path` on `branch`: reuses the branch if it exists, else branches from HEAD.
pub fn add_worktree(repo: &Path, path: &Path, branch: &str) -> Result<(), String> {
    if path.exists() {
        return Ok(());
    }
    let p = path.to_string_lossy();
    if succeeds(repo, &["rev-parse", "--verify", branch]) {
        git(repo, &["worktree", "add", &p, branch]).map(|_| ())
    } else {
        git(repo, &["worktree", "add", "-b", branch, &p, "HEAD"]).map(|_| ())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Rebase {
    Ok { moved: bool },
    Dirty(String),
    Conflict(String),
    Error(String),
}

/// Rebase the branch checked out in `worktree` onto `base`; on a conflict the rebase is aborted and the
/// worktree is as before.
pub fn rebase_onto(worktree: &Path, base: &str) -> Rebase {
    let inner = || -> Result<Rebase, String> {
        let dirty = git(worktree, &["status", "--porcelain", "--untracked-files=no"])?;
        if !dirty.is_empty() {
            return Ok(Rebase::Dirty(
                dirty.lines().take(10).collect::<Vec<_>>().join("\n"),
            ));
        }
        if succeeds(worktree, &["merge-base", "--is-ancestor", base, "HEAD"]) {
            return Ok(Rebase::Ok { moved: false });
        }
        let before = git(worktree, &["rev-parse", "HEAD"])?;
        if let Err(e) = git(worktree, &["rebase", base]) {
            let files =
                git(worktree, &["diff", "--name-only", "--diff-filter=U"]).unwrap_or_default();
            let _ = git(worktree, &["rebase", "--abort"]);
            return Ok(if files.is_empty() {
                Rebase::Error(e)
            } else {
                Rebase::Conflict(files.lines().collect::<Vec<_>>().join("、"))
            });
        }
        Ok(Rebase::Ok {
            moved: git(worktree, &["rev-parse", "HEAD"])? != before,
        })
    };
    inner().unwrap_or_else(Rebase::Error)
}

/// Whether an item's work has anything a release could ship: commits on `branch` beyond `base` in `repo`, or
/// uncommitted (including untracked) files in its `worktree`. A missing branch and a missing worktree mean
/// nothing to ship. `None` when git can't tell (no repo, no `base`), so callers keep releasing as before.
pub fn has_changes(repo: &Path, worktree: Option<&Path>, branch: &str, base: &str) -> Option<bool> {
    if !is_git(repo) {
        return None;
    }
    let head = format!("refs/heads/{branch}");
    if succeeds(repo, &["rev-parse", "--verify", "--quiet", &head]) {
        let ahead = git(repo, &["rev-list", "--count", &format!("{base}..{head}")]).ok()?;
        if ahead.parse::<u64>().ok()? > 0 {
            return Some(true);
        }
    } else {
        /* the base must exist, or "no branch" may just be a repo we don't understand */
        git(repo, &["rev-parse", "--verify", "--quiet", base]).ok()?;
    }
    match worktree.filter(|w| w.exists()) {
        Some(w) if is_git(w) => {
            /* the worktree may have moved off the branch; its own commits count too */
            let ahead = git(w, &["rev-list", "--count", &format!("{base}..HEAD")]).ok()?;
            Some(
                ahead.parse::<u64>().ok()? > 0
                    || !git(w, &["status", "--porcelain"]).ok()?.is_empty(),
            )
        }
        Some(_) => None,
        None => Some(false),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorktreeTidy {
    Removed,
    Dirty,
    Unmerged,
    Absent,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BranchTidy {
    Deleted,
    Kept,
    Absent,
}

/// After an item is done or dropped: remove its worktree when it is clean (and, if `only_merged`, when the
/// branch is merged), and delete the branch when it is fully merged into `base`. Unmerged work is never deleted.
pub fn tidy_worktree(
    repo: &Path,
    worktree: &Path,
    branch: &str,
    only_merged: bool,
    base: &str,
) -> (WorktreeTidy, BranchTidy) {
    let exists = succeeds(
        repo,
        &[
            "rev-parse",
            "--verify",
            "--quiet",
            &format!("refs/heads/{branch}"),
        ],
    );
    let merged = exists
        && succeeds(
            repo,
            &[
                "merge-base",
                "--is-ancestor",
                &format!("refs/heads/{branch}"),
                base,
            ],
        );
    let mut wt = WorktreeTidy::Absent;
    if worktree.exists() {
        /* `git worktree remove` without --force refuses modified or untracked files */
        wt = if only_merged && !merged {
            WorktreeTidy::Unmerged
        } else if succeeds(repo, &["worktree", "remove", &worktree.to_string_lossy()]) {
            WorktreeTidy::Removed
        } else {
            WorktreeTidy::Dirty
        };
    }
    let mut br = if exists {
        BranchTidy::Kept
    } else {
        BranchTidy::Absent
    };
    /* -d checks again that the branch is merged */
    if exists
        && merged
        && !matches!(wt, WorktreeTidy::Dirty | WorktreeTidy::Unmerged)
        && succeeds(repo, &["branch", "-d", branch])
    {
        br = BranchTidy::Deleted;
    }
    (wt, br)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use std::fs;
    use std::path::PathBuf;

    pub fn g(dir: &Path, args: &[&str]) -> String {
        git(dir, args).unwrap_or_else(|e| panic!("git {args:?}: {e}"))
    }
    /// A repo on main with one commit.
    pub fn repo(root: &Path) -> PathBuf {
        let r = root.join("repo");
        fs::create_dir_all(&r).unwrap();
        g(&r, &["init", "-q", "-b", "main"]);
        g(&r, &["config", "user.email", "t@t"]);
        g(&r, &["config", "user.name", "t"]);
        g(&r, &["config", "commit.gpgsign", "false"]);
        fs::write(r.join("a.txt"), "a\n").unwrap();
        g(&r, &["add", "."]);
        g(&r, &["commit", "-q", "-m", "a"]);
        r
    }
    pub fn branch_with(
        repo: &Path,
        wts: &Path,
        dir: &str,
        file: &str,
        text: &str,
    ) -> (String, PathBuf) {
        let name = format!("steve/{}", dir.to_lowercase());
        let wt = wts.join(dir);
        g(
            repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                &name,
                &wt.to_string_lossy(),
                "main",
            ],
        );
        fs::write(wt.join(file), text).unwrap();
        g(&wt, &["add", "."]);
        g(&wt, &["commit", "-q", "-m", file]);
        (name, wt)
    }
    fn on_main(repo: &Path, file: &str, text: &str) {
        fs::write(repo.join(file), text).unwrap();
        g(repo, &["add", "."]);
        g(repo, &["commit", "-q", "-m", &format!("main {file}")]);
    }

    #[test]
    fn rebase_moves_a_branch_behind_main_and_aborts_a_conflict() {
        let t = tempfile::tempdir().unwrap();
        let r = repo(t.path());
        let wts = t.path().join("wt");
        let (_, ok) = branch_with(&r, &wts, "T-1", "b.txt", "b\n");
        on_main(&r, "c.txt", "c\n");
        assert_eq!(rebase_onto(&ok, "main"), Rebase::Ok { moved: true });
        g(&ok, &["merge-base", "--is-ancestor", "main", "HEAD"]);
        assert_eq!(rebase_onto(&ok, "main"), Rebase::Ok { moved: false });

        let (_, bad) = branch_with(&r, &wts, "T-2", "a.txt", "mine\n");
        on_main(&r, "a.txt", "theirs\n");
        let head = g(&bad, &["rev-parse", "HEAD"]);
        match rebase_onto(&bad, "main") {
            Rebase::Conflict(f) => assert!(f.contains("a.txt")),
            other => panic!("{other:?}"),
        }
        assert_eq!(g(&bad, &["rev-parse", "HEAD"]), head);
        assert_eq!(g(&bad, &["status", "--porcelain"]), "");

        fs::write(ok.join("b.txt"), "changed\n").unwrap();
        assert!(matches!(rebase_onto(&ok, "main"), Rebase::Dirty(_)));
    }

    #[test]
    fn has_changes_sees_commits_and_dirty_files_only() {
        let t = tempfile::tempdir().unwrap();
        let r = repo(t.path());
        let wts = t.path().join("wt");
        let none = wts.join("none");
        assert_eq!(
            has_changes(&r, Some(&none), "steve/none", "main"),
            Some(false)
        );
        assert_eq!(has_changes(t.path(), None, "steve/x", "main"), None);
        assert_eq!(has_changes(&r, None, "steve/x", "nope"), None);

        let (c, cwt) = branch_with(&r, &wts, "T-1", "c.txt", "c\n");
        assert_eq!(has_changes(&r, Some(&cwt), &c, "main"), Some(true));

        let e = "steve/t-2";
        let ewt = wts.join("T-2");
        g(
            &r,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                e,
                &ewt.to_string_lossy(),
                "main",
            ],
        );
        assert_eq!(has_changes(&r, Some(&ewt), e, "main"), Some(false));
        assert_eq!(has_changes(&r, None, e, "main"), Some(false));
        fs::write(ewt.join("new.txt"), "x").unwrap();
        assert_eq!(has_changes(&r, Some(&ewt), e, "main"), Some(true));
        fs::remove_file(ewt.join("new.txt")).unwrap();
        fs::write(ewt.join("a.txt"), "changed\n").unwrap();
        assert_eq!(has_changes(&r, Some(&ewt), e, "main"), Some(true));
    }

    #[test]
    fn tidy_removes_merged_work_and_keeps_unmerged_or_dirty() {
        let t = tempfile::tempdir().unwrap();
        let r = repo(t.path());
        let wts = t.path().join("wt");
        let (m, mwt) = branch_with(&r, &wts, "T-1", "m.txt", "m\n");
        g(&r, &["merge", "-q", "--ff-only", &m]);
        assert_eq!(
            tidy_worktree(&r, &mwt, &m, true, "main"),
            (WorktreeTidy::Removed, BranchTidy::Deleted)
        );
        assert!(!mwt.exists());

        let (o, owt) = branch_with(&r, &wts, "T-2", "o.txt", "o\n");
        assert_eq!(
            tidy_worktree(&r, &owt, &o, true, "main"),
            (WorktreeTidy::Unmerged, BranchTidy::Kept)
        );
        assert!(owt.exists());
        assert_eq!(
            tidy_worktree(&r, &owt, &o, false, "main"),
            (WorktreeTidy::Removed, BranchTidy::Kept)
        );
        g(&r, &["rev-parse", "--verify", &o]);

        let (d, dwt) = branch_with(&r, &wts, "T-3", "d.txt", "d\n");
        fs::write(dwt.join("untracked.txt"), "x").unwrap();
        assert_eq!(
            tidy_worktree(&r, &dwt, &d, false, "main"),
            (WorktreeTidy::Dirty, BranchTidy::Kept)
        );
        assert!(dwt.join("untracked.txt").exists());
    }

    #[test]
    fn worktree_add_reuses_an_existing_branch() {
        let t = tempfile::tempdir().unwrap();
        let r = repo(t.path());
        let wt = t.path().join("wt/ST-1");
        add_worktree(&r, &wt, "steve/st-1").unwrap();
        assert!(wt.join("a.txt").exists());
        g(&r, &["worktree", "remove", &wt.to_string_lossy()]);
        add_worktree(&r, &wt, "steve/st-1").unwrap();
        assert_eq!(g(&wt, &["rev-parse", "--abbrev-ref", "HEAD"]), "steve/st-1");
        assert!(is_git(&r) && !is_git(t.path()));
    }
}
