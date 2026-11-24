//! Local Git comparisons for frozen worktree results and safe cleanup.

use std::path::Path;
use std::process::Stdio;

use crate::git::git;

/// The refs a branch of the item may have started from, most authoritative first.
const BASES: &[&str] = &[
    "refs/remotes/origin/HEAD",
    "refs/remotes/origin/main",
    "refs/remotes/origin/master",
    "refs/heads/main",
    "refs/heads/master",
];

/// The patch id of what `git log -p` / `git diff-tree -p` with `args` prints, per commit (`(patch id,
/// commit)`; the commit is all zeros for a plain diff). Empty when there is no change; an error when
/// either git fails, which is not the same as no change.
pub(crate) fn patch_ids(dir: &Path, args: &[&str]) -> Result<Vec<(String, String)>, String> {
    let mut src = crate::git::command()
        .arg("-C")
        .arg(dir)
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| e.to_string())?;
    let Some(out) = src.stdout.take() else {
        let _ = src.wait();
        return Err("git 没有输出".into());
    };
    let ids = crate::git::command()
        .arg("-C")
        .arg(dir)
        .args(["patch-id", "--stable"])
        .stdin(Stdio::from(out))
        .stderr(Stdio::null())
        .output();
    let src_ok = src.wait().is_ok_and(|s| s.success());
    let ids = ids.map_err(|e| e.to_string())?;
    if !src_ok || !ids.status.success() {
        return Err(format!("git {} failed", args.first().unwrap_or(&"")));
    }
    Ok(String::from_utf8_lossy(&ids.stdout)
        .lines()
        .filter_map(|l| {
            let mut p = l.split_whitespace();
            Some((p.next()?.to_string(), p.next()?.to_string()))
        })
        .collect())
}

/// Where the item's branch left the line it came from: the merge base of `head` with the default
/// branches there are (local and remote, the nearest of them). `None` when there is none.
fn base_of(dir: &Path, head: &str) -> Option<String> {
    let refs: Vec<&str> = BASES
        .iter()
        .copied()
        .filter(|r| git(dir, &["rev-parse", "--verify", "--quiet", r]).is_ok())
        .collect();
    if refs.is_empty() {
        return None;
    }
    let mut args = vec!["merge-base", head];
    args.extend(refs);
    git(dir, &args).ok().filter(|b| !b.is_empty())
}

/// The item's own change in `tree` (a tree id of the worktree, `head` its commit): `(base, patch id)`
/// of the diff from where the branch left its default branch. The patch id is `-` for no change. The
/// same change rebased onto a newer base keeps its patch id; `None` when git can't tell.
pub fn own_change(dir: &Path, head: &str, tree: &str) -> Option<(String, String)> {
    let base = base_of(dir, head)?;
    let patch = patch_ids(dir, &["diff-tree", "-p", "--no-color", &base, tree])
        .ok()?
        .into_iter()
        .next()
        .map(|(p, _)| p)
        .unwrap_or_else(|| "-".into());
    Some((base, patch))
}

/// A fingerprint's parts: `head:tree[:base:patch]` (the short form is the one written before own
/// changes were recorded).
pub struct Print<'a> {
    pub head: &'a str,
    pub tree: &'a str,
    pub own: Option<(&'a str, &'a str)>,
}

pub fn parse(f: &str) -> Print<'_> {
    let mut p = f.split(':');
    let head = p.next().unwrap_or("");
    let tree = p.next().unwrap_or("");
    let own = match (p.next(), p.next()) {
        (Some(b), Some(x)) => Some((b, x)),
        _ => None,
    };
    Print { head, tree, own }
}

/// What changed between two fingerprints of the worktree, in words for the timeline and the agent:
/// the files of the item's own change that differ, and whether the branch moved onto a new base.
pub fn describe_change(dir: &Path, frozen: &str, now: &str) -> String {
    describe_with(frozen, now, |from, to| {
        git(dir, &["diff", "--name-only", from, to])
            .map(|s| s.lines().map(String::from).collect())
            .unwrap_or_default()
    })
}

/// The pairs of trees (or commits) [`describe_with`] compares, for `frozen` and `now`.
pub(crate) fn describe_pairs(frozen: &str, now: &str) -> Vec<(String, String)> {
    let (a, b) = (parse(frozen), parse(now));
    let mut v = vec![(a.tree.to_string(), b.tree.to_string())];
    if let (Some((ba, _)), Some((bb, _))) = (a.own, b.own) {
        v.push((ba.to_string(), a.tree.to_string()));
        v.push((bb.to_string(), b.tree.to_string()));
    }
    v
}

/// [`describe_change`] with the files that differ between two trees given by `names` (empty when
/// it can't tell).
pub(crate) fn describe_with(
    frozen: &str,
    now: &str,
    names: impl Fn(&str, &str) -> Vec<String>,
) -> String {
    let (a, b) = (parse(frozen), parse(now));
    let mut parts = vec![];
    if let (Some((ba, _)), Some((bb, _))) = (a.own, b.own)
        && ba != bb
    {
        parts.push("分支换到了新的基线".to_string());
    }
    let changed = names(a.tree, b.tree);
    /* only what the item itself touches, then or now: a new base brings other people's files along */
    let own: Vec<String> = match (a.own, b.own) {
        (Some((ba, _)), Some((bb, _))) => {
            let mut v = names(ba, a.tree);
            v.extend(names(bb, b.tree));
            v
        }
        _ => changed.clone(),
    };
    let files: Vec<&String> = changed.iter().filter(|f| own.contains(f)).collect();
    if !files.is_empty() {
        let shown: Vec<&str> = files.iter().take(3).map(|f| f.as_str()).collect();
        parts.push(if files.len() > 3 {
            format!("改了 {} 等 {} 个文件", shown.join("、"), files.len())
        } else {
            format!("改了 {}", shown.join("、"))
        });
    }
    if parts.is_empty() {
        "工作区的内容变了".into()
    } else {
        parts.join("，")
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Presence {
    Absent,
    /// The commit on the target that brought the change in.
    Landed {
        commit: String,
    },
    Unknown(String),
}

/// The commit on `up` that brought `head`'s change in: `head` itself when it is on `up`, the rebased
/// copy of its last commit (every own commit has an equivalent upstream), or a squash of all of them.
/// Compare local objects and refs.
pub fn presence(dir: &Path, head: &str, up: &str) -> Presence {
    match landed(dir, head, up) {
        Ok(Some(commit)) => Presence::Landed { commit },
        Ok(None) => Presence::Absent,
        Err(e) => Presence::Unknown(e),
    }
}

fn landed(dir: &Path, head: &str, up: &str) -> Result<Option<String>, String> {
    if crate::git::git_test(dir, &["merge-base", "--is-ancestor", head, up])? {
        return Ok(Some(head.into()));
    }
    /* no shared history: it can't be on `up` */
    let Some(mb) = crate::git::merge_base(dir, head, up)? else {
        return Ok(None);
    };
    let range = format!("{mb}..{up}");
    let upstream = patch_ids(dir, &["log", "--no-merges", "-p", "--no-color", &range])?;
    let find = |pid: &str| {
        upstream
            .iter()
            .find(|(p, _)| p == pid)
            .map(|(_, c)| c.clone())
    };
    let cherry = git(dir, &["cherry", up, head])?;
    if !cherry.is_empty() && cherry.lines().all(|l| l.starts_with('-')) {
        let tip = patch_ids(
            dir,
            &["show", "-p", "--no-color", "--format=commit %H", head],
        )?;
        if let Some(c) = tip.first().and_then(|(p, _)| find(p)) {
            return Ok(Some(c));
        }
    }
    /* squash merge: one upstream commit with the whole change */
    let all = patch_ids(dir, &["diff-tree", "-p", "--no-color", &mb, head])?;
    Ok(all.first().and_then(|(p, _)| find(p)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fingerprints_parse_in_both_forms() {
        let p = parse("h:t");
        assert_eq!((p.head, p.tree, p.own), ("h", "t", None));
        let p = parse("h:t:b:x");
        assert_eq!(p.own, Some(("b", "x")));
    }
}
