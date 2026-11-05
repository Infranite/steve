//! lifecycle.ts, second half, and the git parts of the orchestrator: item worktrees, the rebase before a
//! release, and giving worktrees back. Nothing here forces anything: a conflicting rebase is aborted, a dirty
//! worktree stays, an unmerged branch stays.
//!
//! These run synchronously like `execFileSync` in TS, inside the critical section whose result they decide.

use crate::util::js_head;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;

/// The git Steve runs: `STEVE_GIT` when set (a pinned build Steve manages), else `git` on `PATH`. Every git
/// call of the domain layer goes through it, so one setting moves them all.
pub fn program() -> &'static Path {
    static GIT: OnceLock<PathBuf> = OnceLock::new();
    GIT.get_or_init(|| {
        std::env::var_os("STEVE_GIT")
            .filter(|g| !g.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| "git".into())
    })
}

/// `git` as a command, without Steve's upgrade secrets in its environment.
pub fn command() -> Command {
    let mut c = Command::new(program());
    c.env_remove("STEVE_UPGRADE_CONTROL_TOKEN")
        .env_remove("STEVE_SUPERVISOR_PID")
        .env_remove("STEVE_UPGRADE_ID");
    c
}

/// `git --version`'s `(major, minor)`; `None` when there is no git to run.
pub fn version() -> Option<(u32, u32)> {
    let out = command()
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .ok()?;
    let text = String::from_utf8_lossy(&out.stdout);
    let v = text.split_whitespace().nth(2)?;
    let mut n = v.split('.').map(|x| x.parse::<u32>().ok());
    Some((n.next()??, n.next()??))
}

/// `git -C dir args…`: trimmed stdout, or the trimmed stderr (≤300 chars) as the error.
pub fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let out = command()
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_EDITOR", "true")
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    outcome(
        args,
        out.status.success(),
        &String::from_utf8_lossy(&out.stdout),
        &String::from_utf8_lossy(&out.stderr),
    )
}

/// How long a read of the repository through [`git_read`] may take. Generous: it only has to end a
/// git that hangs (a helper waiting on something that never comes), never a slow one.
const READ_DEADLINE: Duration = Duration::from_secs(120);

/// [`git`] for a command that only reads (`status`, `rev-parse`, `rev-list`, `merge-base`): it takes no
/// optional lock (so it never waits on a stale `index.lock`), never fetches a missing object, and is
/// killed at [`READ_DEADLINE`]. A command that changes the repository goes through [`git`], whose
/// half-done work a kill would leave behind.
pub fn git_read(dir: &Path, args: &[&str]) -> Result<String, String> {
    let mut cmd = command();
    cmd.arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_TERMINAL_PROMPT", "0");
    match crate::util::run_deadline(cmd, READ_DEADLINE) {
        Ok((ok, out, err)) => outcome(args, ok, &out, &err),
        Err(crate::util::RunError::Spawn(e)) => Err(e.to_string()),
        Err(e) => Err(format!("git {}: {e}", args.join(" "))),
    }
}

/// Trimmed stdout, or the trimmed stderr (≤300 chars) as the error.
fn outcome(args: &[&str], ok: bool, out: &str, err: &str) -> Result<String, String> {
    if ok {
        Ok(out.trim().to_string())
    } else {
        let err = err.trim();
        Err(if err.is_empty() {
            format!("git {} failed", args.join(" "))
        } else {
            js_head(err, 300).to_string()
        })
    }
}

/// A yes/no question git answers with its exit status (`merge-base --is-ancestor`, `rev-parse --verify
/// --quiet`): 0 is yes, 1 is no; anything else, or no git at all, is an error, never a no.
pub fn git_test(dir: &Path, args: &[&str]) -> Result<bool, String> {
    let out = command()
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
    let out = command()
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

/// Whether a read-only git command succeeds (through [`git_read`], so it is killed at its deadline).
fn succeeds(dir: &Path, args: &[&str]) -> bool {
    git_read(dir, args).is_ok()
}

/// Whether a git command that changes the repository succeeds (through [`git`]: never killed, so it
/// cannot leave half-done work behind).
fn succeeds_write(dir: &Path, args: &[&str]) -> bool {
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
        let dirty = git_read(worktree, &["status", "--porcelain", "--untracked-files=no"])?;
        if !dirty.is_empty() {
            return Ok(Rebase::Dirty(
                dirty.lines().take(10).collect::<Vec<_>>().join("\n"),
            ));
        }
        if succeeds(worktree, &["merge-base", "--is-ancestor", base, "HEAD"]) {
            return Ok(Rebase::Ok { moved: false });
        }
        let before = git_read(worktree, &["rev-parse", "HEAD"])?;
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
            moved: git_read(worktree, &["rev-parse", "HEAD"])? != before,
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
        let ahead = git_read(repo, &["rev-list", "--count", &format!("{base}..{head}")]).ok()?;
        if ahead.parse::<u64>().ok()? > 0 {
            return Some(true);
        }
    } else {
        /* the base must exist, or "no branch" may just be a repo we don't understand */
        git_read(repo, &["rev-parse", "--verify", "--quiet", base]).ok()?;
    }
    match worktree.filter(|w| w.exists()) {
        Some(w) if is_git(w) => {
            /* the worktree may have moved off the branch; its own commits count too */
            let ahead = git_read(w, &["rev-list", "--count", &format!("{base}..HEAD")]).ok()?;
            Some(
                ahead.parse::<u64>().ok()? > 0
                    || !git_read(w, &["status", "--porcelain"]).ok()?.is_empty(),
            )
        }
        Some(_) => None,
        None => Some(false),
    }
}

/// Whether the work on `branch` is in `base`: merged, or landed rebased or squashed
/// ([`crate::upstream::presence`]). Remembered per pair of tips, since comparing the patches of an old branch
/// can take seconds and the hub asks again on every pass; a git error is a no and is not remembered.
pub fn landed(repo: &Path, branch: &str, base: &str) -> bool {
    type Memo = HashMap<(PathBuf, String, String), bool>;
    static MEMO: OnceLock<Mutex<Memo>> = OnceLock::new();
    let head = format!("refs/heads/{branch}");
    let (Ok(tip), Ok(at)) = (
        git_read(repo, &["rev-parse", "--verify", "--quiet", &head]),
        git_read(repo, &["rev-parse", "--verify", "--quiet", base]),
    ) else {
        return false;
    };
    let key = (repo.to_path_buf(), tip, at);
    let memo = MEMO.get_or_init(Default::default);
    if let Some(v) = memo.lock().unwrap_or_else(|e| e.into_inner()).get(&key) {
        return *v;
    }
    let v = match crate::upstream::presence(repo, &head, base) {
        crate::upstream::Presence::Landed { .. } => true,
        crate::upstream::Presence::Absent => false,
        crate::upstream::Presence::Unknown(_) => return false,
    };
    let mut m = memo.lock().unwrap_or_else(|e| e.into_inner());
    if m.len() > 4096 {
        m.clear();
    }
    m.insert(key, v);
    v
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

/// What a tidy decides on about an item's branch (git reads only).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BranchFacts {
    pub exists: bool,
    /// An ancestor of the base (`branch -d` deletes it).
    pub merged: bool,
    /// Its work is in the base: merged, or landed rebased or squashed ([`landed`]).
    pub landed: bool,
}

pub fn branch_facts(repo: &Path, branch: &str, base: &str) -> BranchFacts {
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
    BranchFacts {
        exists,
        merged,
        landed: merged || (exists && landed(repo, branch, base)),
    }
}

/// After an item is done or dropped: remove its worktree when it is clean (and, if `only_merged`, when the
/// branch's work is in `base`), and delete the branch when it is fully merged into `base`. Unmerged work is
/// never deleted.
///
/// "In `base`" also counts a branch that landed rebased or squashed (a PR merged by rebase gives its commits
/// new ids, so `merge-base --is-ancestor` alone kept such worktrees forever); its branch stays, since
/// `branch -d` only deletes what is an ancestor of `base`, and the worktree can be made again from it.
pub fn tidy_worktree(
    repo: &Path,
    worktree: &Path,
    branch: &str,
    only_merged: bool,
    base: &str,
) -> (WorktreeTidy, BranchTidy) {
    let BranchFacts {
        exists,
        merged,
        landed,
    } = branch_facts(repo, branch, base);
    let mut wt = WorktreeTidy::Absent;
    if worktree.exists() {
        /* `git worktree remove` without --force refuses modified or untracked files */
        wt = if only_merged && !landed {
            WorktreeTidy::Unmerged
        } else if succeeds_write(repo, &["worktree", "remove", &worktree.to_string_lossy()]) {
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
        && succeeds_write(repo, &["branch", "-d", branch])
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
