//! What a unit's merge does in git (stage 2, direct push): its batch branch is built in a worktree of its
//! own from the target's tip, its members merged in, in order; a member that conflicts with the ones
//! before it is backed out and leaves (the others go on); the lane's checks run on the result; the
//! result is pushed, fast-forward only. Member branches are never rewritten.
//!
//! Plain git and the lane's own commands: no store, no lock. The caller fetches first and records the
//! outcome ([`super::store`]).

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

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

/// How long the lane lets `ops/cut-release.py cut` wait for CI on the merged head (`--ci-wait`), shorter
/// than the script's own 45 minutes: the unit's release holds the lane until it returns. A run still
/// going after this is refused ("cut again once it has passed"), and the retry card picks it up.
pub const LANE_CI_WAIT: Duration = Duration::from_secs(30 * 60);
/// On top of the CI wait: fetching, pushing, and another attempt when origin moved meanwhile. The release
/// step is stopped after this, whatever it is doing (a push that did land is found by
/// [`already_released`] before the next try).
const RELEASE_SLACK: Duration = Duration::from_secs(15 * 60);

/// Release the target the repository's own way when it has one: `ops/cut-release.py cut` (the version
/// is chosen there, main and its tag pushed together). `Ok(None)`: no release step of its own (merged
/// is released). The version cut, when one was.
///
/// The script tags only a head whose CI passed and waits while CI runs, so this returns about one CI run
/// after the merge: at most [`LANE_CI_WAIT`] for CI (passed as `--ci-wait` when the script takes it),
/// the whole step at most [`RELEASE_SLACK`] more. A refusal leads the error (the progress lines follow):
/// a failed run names `gh run rerun <id> --failed`, and retrying the release before that run passes is
/// refused the same way.
pub fn release(wt: &Path, branch: &str) -> Result<Option<String>, String> {
    release_within(wt, branch, LANE_CI_WAIT, LANE_CI_WAIT + RELEASE_SLACK)
}

fn release_within(
    wt: &Path,
    branch: &str,
    ci_wait: Duration,
    deadline: Duration,
) -> Result<Option<String>, String> {
    let script = wt.join("ops/cut-release.py");
    if !script.is_file() {
        return Ok(None);
    }
    let mut cmd = Command::new("python3");
    cmd.arg(&script).args(["cut", "--branch", branch]);
    // an older script without the CI gate takes no --ci-wait
    if std::fs::read_to_string(&script).is_ok_and(|t| t.contains("--ci-wait")) {
        cmd.args(["--ci-wait", &ci_wait.as_secs().to_string()]);
    }
    let mut child = cmd
        .current_dir(wt)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let drain = |r: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut b = Vec::new();
            if let Some(mut r) = r {
                let _ = r.read_to_end(&mut b);
            }
            String::from_utf8_lossy(&b).into_owned()
        })
    };
    let out = drain(child.stdout.take().map(|r| Box::new(r) as _));
    let err = drain(child.stderr.take().map(|r| Box::new(r) as _));
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if start.elapsed() < deadline => {
                std::thread::sleep(Duration::from_millis(200))
            }
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let Some(status) = status else {
        // not waited for: what the script started may still hold its output open
        return Err(format!(
            "发布脚本 {} 分钟还没结束，已停止；确认 CI 和 origin 的状态后再重试。",
            deadline.as_secs().div_ceil(60)
        ));
    };
    let (text, err) = (
        out.join().unwrap_or_default(),
        err.join().unwrap_or_default(),
    );
    if !status.success() {
        return Err(refusal(&format!("{text}{err}")));
    }
    Ok(text
        .lines()
        .rev()
        .find_map(|l| l.trim().strip_prefix("released v").map(str::to_string)))
}

/// The release script's output for the retry card: what refused first (`✗ …`, with a line in Chinese
/// on what to do before retrying when it is about CI), then the rest, cut to 600 characters.
fn refusal(output: &str) -> String {
    let (why, rest): (Vec<&str>, Vec<&str>) = output
        .lines()
        .filter(|l| !l.trim().is_empty())
        .partition(|l| l.trim_start().starts_with('✗'));
    let what = why.join("\n");
    let lead = if what.contains("gh run rerun") {
        "CI 没有通过：偶发失败就先用下面的 gh run rerun 重跑失败的任务，等它通过后再重试；否则先在主分支上修好。\n"
    } else if what.contains("CI") && what.contains("is still") {
        "CI 还在跑：等它通过后再重试。\n"
    } else {
        ""
    };
    let all = format!("{lead}{}", [what, rest.join("\n")].join("\n"));
    js_head(all.trim(), 600).to_string()
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

    /// `ops/cut-release.py` in a fresh directory: `body` is the script (Python), its argv lands in `argv`.
    fn script(body: &str) -> tempfile::TempDir {
        let t = tempfile::tempdir().unwrap();
        std::fs::create_dir_all(t.path().join("ops")).unwrap();
        let head = "import sys, time, pathlib\npathlib.Path(__file__).with_name('argv').write_text(' '.join(sys.argv[1:]))\n";
        std::fs::write(t.path().join("ops/cut-release.py"), format!("{head}{body}")).unwrap();
        t
    }

    fn argv(t: &tempfile::TempDir) -> String {
        std::fs::read_to_string(t.path().join("ops/argv")).unwrap()
    }

    #[test]
    fn the_lane_bounds_the_ci_wait_of_a_script_that_takes_one() {
        let t = script("# --ci-wait S\nprint('released v1.2.3')\n");
        assert_eq!(release(t.path(), "main"), Ok(Some("1.2.3".into())));
        assert_eq!(
            argv(&t),
            format!("cut --branch main --ci-wait {}", LANE_CI_WAIT.as_secs())
        );
        assert!(LANE_CI_WAIT < Duration::from_secs(45 * 60));
        // a script without the CI gate is run as before
        let t = script("print('released v1.2.4')\n");
        assert_eq!(release(t.path(), "main"), Ok(Some("1.2.4".into())));
        assert_eq!(argv(&t), "cut --branch main");
    }

    #[test]
    fn a_failed_ci_leads_the_retry_card_with_the_rerun_hint() {
        // many progress lines first, as a long wait prints them; the refusal comes last
        let t = script(concat!(
            "# --ci-wait\n",
            "for i in range(40): print(f'waiting for CI on abc1234 (in_progress {i})', file=sys.stderr)\n",
            "print('✗ CI (ci.yml) failure on abc1234: https://example.invalid/runs/42. ',\n",
            "      'If it was a flake, run its failed jobs again: gh run rerun 42 --failed', file=sys.stderr)\n",
            "sys.exit(1)\n",
        ));
        let e = release(t.path(), "main").unwrap_err();
        assert!(e.starts_with("CI 没有通过"), "{e}");
        assert!(e.contains("gh run rerun 42 --failed"), "{e}");
        assert!(
            e.find("✗").unwrap() < e.find("waiting for CI").unwrap(),
            "{e}"
        );
        assert!(crate::util::js_len(&e) <= 600);
    }

    #[test]
    fn a_release_step_that_does_not_end_is_stopped() {
        let t = script("time.sleep(60)\n");
        let start = Instant::now();
        let e = release_within(
            t.path(),
            "main",
            Duration::from_secs(1),
            Duration::from_secs(1),
        )
        .unwrap_err();
        assert!(start.elapsed() < Duration::from_secs(20), "stopped in time");
        assert!(e.contains("已停止"), "{e}");
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
