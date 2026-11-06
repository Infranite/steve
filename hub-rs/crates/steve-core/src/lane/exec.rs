//! What a unit's merge does in git (stage 2, direct push): its batch branch is built in a worktree of its
//! own from the target's tip, its members merged in, in order; a member that conflicts with the ones
//! before it is backed out and leaves (the others go on); the result is pushed, fast-forward only.
//! Member branches are never rewritten.
//!
//! Plain git and the repository's own release step: no store, no lock. The caller fetches first and records the
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
    /// Where its commit is (its worktree or clone) and on which branch: fetched from there when the
    /// batch's repository doesn't have it (members of one lane may come from different clones).
    pub dir: String,
    pub branch: String,
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
        let commit = format!("{}^{{commit}}", m.head);
        if git(wt, &["cat-file", "-e", &commit]).is_err()
            && !m.dir.is_empty()
            && !m.branch.is_empty()
        {
            let _ = git(wt, &["fetch", "--quiet", &m.dir, &m.branch]);
        }
        if git(wt, &["cat-file", "-e", &commit]).is_err() {
            left.push((m.id.clone(), format!("拿不到这个提交（{}）", m.head)));
            continue;
        }
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
    // a member that "left" but whose commit a later member brought in anyway is in: say so, it is true
    let (inside, left): (Vec<_>, Vec<_>) = left.into_iter().partition(|(id, _)| {
        members
            .iter()
            .find(|m| &m.id == id)
            .is_some_and(|m| git(wt, &["merge-base", "--is-ancestor", &m.head, &tip]).is_ok())
    });
    let merged = members
        .iter()
        .filter(|m| merged.contains(&m.id) || inside.iter().any(|(id, _)| *id == m.id))
        .map(|m| m.id.clone())
        .collect();
    Ok(Integrated { tip, merged, left })
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

/// The version a release of `heads` already went out as: the oldest of the remote's `vX.Y.Z` tags
/// (`tags`: name → commit, [`crate::upstream::remote_tags`]) whose commit contains every one of them;
/// `None` when no single tag does. Asked before releasing again (after a restart): a release step never
/// runs twice for the same commits.
pub fn already_released(dir: &Path, heads: &[String], tags: &[(String, String)]) -> Option<String> {
    let mut versions: Vec<(Vec<u64>, &str, &str)> = tags
        .iter()
        .filter_map(|(name, sha)| {
            let v = name.strip_prefix('v')?;
            let parts: Option<Vec<u64>> = v.split('.').map(|p| p.parse().ok()).collect();
            parts.filter(|p| p.len() == 3).map(|p| (p, v, sha.as_str()))
        })
        .collect();
    versions.sort();
    versions
        .into_iter()
        .find(|(_, _, sha)| {
            heads
                .iter()
                .all(|h| git(dir, &["merge-base", "--is-ancestor", h, sha]).is_ok())
        })
        .map(|(_, v, _)| v.to_string())
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
            dir: String::new(),
            branch: String::new(),
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
    fn a_repository_without_its_own_release_step_is_released_by_the_merge() {
        let t = tempfile::tempdir().unwrap();
        assert_eq!(release(t.path(), "main"), Ok(None));
    }

    #[test]
    fn released_means_one_pushed_tag_holds_every_head() {
        let (_t, _origin, clone) = repos();
        let a = branch(&clone, "a", "a.txt", "ONE\ntwo\nthree\n");
        run(&clone, &["tag", "-a", "-m", "v1", "v1.0.0", &a]);
        let b = branch(&clone, "b", "a.txt", "one\ntwo\nTHREE\n");
        run(&clone, &["tag", "v1.0.1", &b]); // b only: a is not in it
        run(&clone, &["push", "--quiet", "origin", "v1.0.0", "v1.0.1"]);
        run(&clone, &["checkout", "--quiet", "a"]);
        run(&clone, &["merge", "--quiet", "--no-edit", "b"]);
        run(&clone, &["tag", "v1.0.2"]); // both, but never pushed
        let heads = [a.clone(), b.clone()];
        let tags = crate::upstream::remote_tags(&clone).unwrap();
        assert_eq!(
            already_released(&clone, &heads[..1], &tags).as_deref(),
            Some("1.0.0")
        );
        assert_eq!(
            already_released(&clone, &heads, &tags),
            None,
            "no pushed tag holds both"
        );
        run(&clone, &["push", "--quiet", "origin", "v1.0.2"]);
        let tags = crate::upstream::remote_tags(&clone).unwrap();
        assert_eq!(
            already_released(&clone, &heads, &tags).as_deref(),
            Some("1.0.2")
        );
    }
}
