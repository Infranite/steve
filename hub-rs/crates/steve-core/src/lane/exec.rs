//! What a unit's merge does in git (stage 2, direct push): its batch branch is built in a worktree of its
//! own from the target's tip, its members merged in, in order; a member that conflicts with the ones
//! before it is backed out and leaves (the others go on); the lane's checks run on the result; the
//! result is pushed, fast-forward only. Member branches are never rewritten.
//!
//! Plain git and the lane's own commands: no store, no lock. The caller fetches first and records the
//! outcome ([`super::store`]).

use std::path::Path;
use std::process::{Command, Stdio};

use crate::git::git;
use crate::util::js_head;

/// One request to merge: its id in the lane, the commit it merges, how the merge commit names it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Member {
    pub id: String,
    pub head: String,
    pub label: String,
}

/// What integrating a unit came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Integrated {
    /// The batch branch's commit.
    pub tip: String,
    /// The members merged in, in order.
    pub merged: Vec<String>,
    /// The members that left, with why.
    pub left: Vec<(String, String)>,
}

/// Build `branch` in worktree `wt` (made from `repo`, replaced if it is there) from commit `base`, and
/// merge `members` into it in order. A conflict git's recorded resolutions (rerere) settle completely
/// is taken as settled; any other conflict backs that member out.
pub fn integrate(
    repo: &Path,
    wt: &Path,
    branch: &str,
    base: &str,
    members: &[Member],
) -> Result<Integrated, String> {
    if wt.exists() {
        let _ = git(
            repo,
            &["worktree", "remove", "--force", &wt.to_string_lossy()],
        );
        let _ = std::fs::remove_dir_all(wt);
    }
    git(
        repo,
        &[
            "worktree",
            "add",
            "--quiet",
            "-B",
            branch,
            &wt.to_string_lossy(),
            base,
        ],
    )?;
    let mut merged = vec![];
    let mut left = vec![];
    for m in members {
        let msg = format!("Merge {} into {branch}", m.label);
        let r = git(
            wt,
            &[
                "-c",
                "rerere.enabled=true",
                "-c",
                "rerere.autoupdate=true",
                "merge",
                "--no-ff",
                "--no-edit",
                "-m",
                &msg,
                &m.head,
            ],
        );
        if r.is_ok() {
            merged.push(m.id.clone());
            continue;
        }
        let unmerged = git(wt, &["diff", "--name-only", "--diff-filter=U"]).unwrap_or_default();
        if unmerged.is_empty() && git(wt, &["rev-parse", "-q", "--verify", "MERGE_HEAD"]).is_ok() {
            // every conflict had a recorded resolution
            git(wt, &["commit", "--no-edit", "--quiet"])?;
            merged.push(m.id.clone());
            continue;
        }
        let _ = git(wt, &["merge", "--abort"]);
        let files: Vec<&str> = unmerged.lines().filter(|l| !l.is_empty()).collect();
        left.push((
            m.id.clone(),
            if files.is_empty() {
                format!("合并失败：{}", r.err().unwrap_or_default())
            } else {
                format!("和同一批里前面的改动冲突：{}", files.join("、"))
            },
        ));
    }
    let tip = git(wt, &["rev-parse", "HEAD"])?;
    Ok(Integrated { tip, merged, left })
}

/// Run the lane's checks in `wt`, in order; the first that fails, with the end of its output.
pub fn checks(wt: &Path, commands: &[String]) -> Result<(), String> {
    for c in commands {
        let out = Command::new("/bin/sh")
            .arg("-c")
            .arg(c)
            .current_dir(wt)
            .stdin(Stdio::null())
            .output()
            .map_err(|e| format!("{c}：{e}"))?;
        if !out.status.success() {
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            let tail: String = text
                .chars()
                .rev()
                .take(600)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
            return Err(format!("检查没有通过：{c}\n{}", js_head(tail.trim(), 600)));
        }
    }
    Ok(())
}

/// Release the target the repository's own way when it has one: `ops/cut-release.py cut` (the version
/// is chosen there, main and its tag pushed together). `Ok(None)`: no release step of its own (merged
/// is released). The version cut, when one was.
pub fn release(wt: &Path, branch: &str) -> Result<Option<String>, String> {
    let script = wt.join("ops/cut-release.py");
    if !script.is_file() {
        return Ok(None);
    }
    let out = Command::new("python3")
        .arg(&script)
        .args(["cut", "--branch", branch])
        .current_dir(wt)
        .stdin(Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    let text = String::from_utf8_lossy(&out.stdout).to_string();
    if !out.status.success() {
        return Err(js_head(
            &format!("{text}{}", String::from_utf8_lossy(&out.stderr)),
            600,
        )
        .to_string());
    }
    Ok(text
        .lines()
        .rev()
        .find_map(|l| l.trim().strip_prefix("released v").map(str::to_string)))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn run(dir: &Path, args: &[&str]) -> String {
        git(dir, args).unwrap_or_else(|e| panic!("git {args:?}: {e}"))
    }

    /// A bare origin with `main`, and a clone to work in.
    fn repos() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let t = tempfile::tempdir().unwrap();
        let (origin, clone) = (t.path().join("origin.git"), t.path().join("clone"));
        run(
            t.path(),
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
            t.path(),
            &[
                "clone",
                "--quiet",
                &origin.to_string_lossy(),
                &clone.to_string_lossy(),
            ],
        );
        for (k, v) in [("user.email", "t@example.invalid"), ("user.name", "t")] {
            run(&clone, &["config", k, v]);
        }
        std::fs::write(clone.join("a.txt"), "one\ntwo\nthree\n").unwrap();
        run(&clone, &["add", "-A"]);
        run(&clone, &["commit", "--quiet", "-m", "base"]);
        run(&clone, &["push", "--quiet", "origin", "HEAD:main"]);
        (t, origin, clone)
    }

    /// A branch off main changing `file` to `text`; its head.
    fn branch(clone: &Path, name: &str, file: &str, text: &str) -> String {
        run(clone, &["checkout", "--quiet", "-B", name, "origin/main"]);
        std::fs::write(clone.join(file), text).unwrap();
        run(clone, &["commit", "--quiet", "-am", name]);
        run(clone, &["rev-parse", "HEAD"])
    }

    fn member(id: &str, head: &str) -> Member {
        Member {
            id: id.into(),
            head: head.into(),
            label: id.into(),
        }
    }

    #[test]
    fn members_merge_in_order_and_a_conflicting_one_leaves_while_the_others_go_on() {
        let (t, _origin, clone) = repos();
        std::fs::write(clone.join("b.txt"), "b\n").unwrap();
        run(&clone, &["add", "b.txt"]);
        run(&clone, &["commit", "--quiet", "-m", "b"]);
        run(&clone, &["push", "--quiet", "origin", "HEAD:main"]);
        let a = branch(&clone, "a", "a.txt", "ONE\ntwo\nthree\n");
        let b = branch(&clone, "b", "b.txt", "B\n");
        let c = branch(&clone, "c", "a.txt", "uno\ntwo\nthree\n"); // the same line as a
        let base = run(&clone, &["rev-parse", "origin/main"]);
        let wt = t.path().join("MQ-1");
        let r = integrate(
            &clone,
            &wt,
            "steve/merge/MQ-1",
            &base,
            &[member("A", &a), member("B", &b), member("C", &c)],
        )
        .unwrap();
        assert_eq!(r.merged, ["A", "B"]);
        assert_eq!(r.left.len(), 1);
        assert_eq!(r.left[0].0, "C");
        assert!(r.left[0].1.contains("a.txt"), "{}", r.left[0].1);
        // the batch has A's and B's changes; the members' branches are untouched
        assert_eq!(
            std::fs::read_to_string(wt.join("a.txt")).unwrap(),
            "ONE\ntwo\nthree\n"
        );
        assert_eq!(std::fs::read_to_string(wt.join("b.txt")).unwrap(), "B\n");
        assert_eq!(run(&clone, &["rev-parse", "c"]), c);
        // pushed fast-forward
        crate::upstream::push_head(&wt, "main").unwrap();
        run(&clone, &["fetch", "--quiet", "origin"]);
        assert_eq!(run(&clone, &["rev-parse", "origin/main"]), r.tip);
    }

    #[test]
    fn a_target_that_moved_rejects_the_push_it_is_never_overwritten() {
        let (t, _origin, clone) = repos();
        let a = branch(&clone, "a", "a.txt", "ONE\ntwo\nthree\n");
        let base = run(&clone, &["rev-parse", "origin/main"]);
        let wt = t.path().join("MQ-1");
        integrate(&clone, &wt, "steve/merge/MQ-1", &base, &[member("A", &a)]).unwrap();
        // someone pushed to main meanwhile
        run(
            &clone,
            &["checkout", "--quiet", "-B", "other", "origin/main"],
        );
        std::fs::write(clone.join("z.txt"), "z\n").unwrap();
        run(&clone, &["add", "z.txt"]);
        run(&clone, &["commit", "--quiet", "-m", "other"]);
        run(&clone, &["push", "--quiet", "origin", "HEAD:main"]);
        assert!(crate::upstream::push_head(&wt, "main").is_err());
    }

    #[test]
    fn checks_run_in_order_and_the_failing_one_is_named() {
        let t = tempfile::tempdir().unwrap();
        assert!(checks(t.path(), &[]).is_ok(), "no checks: nothing to fail");
        assert!(checks(t.path(), &["true".into()]).is_ok());
        let e = checks(
            t.path(),
            &[
                "true".into(),
                "echo nope; exit 3".into(),
                "touch never".into(),
            ],
        )
        .unwrap_err();
        assert!(e.contains("echo nope; exit 3") && e.contains("nope"), "{e}");
        assert!(
            !t.path().join("never").exists(),
            "stops at the first failure"
        );
    }

    #[test]
    fn a_repository_without_its_own_release_step_is_released_by_the_merge() {
        let t = tempfile::tempdir().unwrap();
        assert_eq!(release(t.path(), "main"), Ok(None));
    }
}
