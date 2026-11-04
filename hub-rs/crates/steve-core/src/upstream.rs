//! The item's result as git sees it, beyond the worktree: its own change (what the frozen-result check
//! compares, so a rebase or a reset onto the merged commit is not a new result), and where it landed
//! upstream (what a project's completion rule `Release::done_when` asks for: merged into the remote's
//! default branch, and a pushed tag that contains it).
//!
//! [`own_change`] and [`describe_change`] are local and run inside the critical section like the rest of
//! the git work. [`probe`] talks to the remote (fetch, `ls-remote`) with a deadline and must never run
//! under the core's lock: callers run it on a thread of its own and come back with the result.

use std::io::Read;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::git::git;
use crate::util::js_head;

pub use steve_store::{DONE_MERGE, DONE_TAG};

/// How long a fetch or `ls-remote` may take.
const NET_DEADLINE: Duration = Duration::from_secs(60);

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
    let mut src = Command::new("git")
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
    let ids = Command::new("git")
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
    let (a, b) = (parse(frozen), parse(now));
    let names = |args: &[&str]| -> Vec<String> {
        git(dir, args)
            .map(|s| s.lines().map(String::from).collect())
            .unwrap_or_default()
    };
    let mut parts = vec![];
    if let (Some((ba, _)), Some((bb, _))) = (a.own, b.own)
        && ba != bb
    {
        parts.push("分支换到了新的基线".to_string());
    }
    let changed = names(&["diff", "--name-only", a.tree, b.tree]);
    /* only what the item itself touches, then or now: a new base brings other people's files along */
    let own: Vec<String> = match (a.own, b.own) {
        (Some((ba, _)), Some((bb, _))) => {
            let mut v = names(&["diff", "--name-only", ba, a.tree]);
            v.extend(names(&["diff", "--name-only", bb, b.tree]));
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

/// Fetches `origin` with its tags, with the network deadline. Never under the core's lock.
pub fn fetch(dir: &Path) -> Result<(), String> {
    git_net(dir, &["fetch", "--quiet", "--tags", "origin"]).map(|_| ())
}

/// `git args…` with a deadline (network calls). The output is drained while the command runs.
fn git_net(dir: &Path, args: &[&str]) -> Result<String, String> {
    let mut child = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .env("GIT_TERMINAL_PROMPT", "0")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| e.to_string())?;
    let drain = |r: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut s = String::new();
            if let Some(mut r) = r {
                let _ = r.read_to_string(&mut s);
            }
            s
        })
    };
    let out = drain(
        child
            .stdout
            .take()
            .map(|o| Box::new(o) as Box<dyn Read + Send>),
    );
    let err = drain(
        child
            .stderr
            .take()
            .map(|o| Box::new(o) as Box<dyn Read + Send>),
    );
    let end = Instant::now() + NET_DEADLINE;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if Instant::now() < end => std::thread::sleep(Duration::from_millis(50)),
            _ => {
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
        }
    };
    let (out, err) = (
        out.join().unwrap_or_default(),
        err.join().unwrap_or_default(),
    );
    match status {
        Some(s) if s.success() => Ok(out.trim().to_string()),
        Some(_) => Err(js_head(err.trim(), 300).to_string()),
        None => Err(format!("git {} 超时", args.first().unwrap_or(&""))),
    }
}

/// `owner/repo` of a GitHub remote URL.
pub fn github_slug(url: &str) -> Option<String> {
    let rest = url
        .strip_prefix("git@github.com:")
        .or_else(|| url.strip_prefix("ssh://git@github.com/"))
        .or_else(|| url.split_once("github.com/").map(|(_, r)| r))?;
    let rest = rest.trim_end_matches('/').trim_end_matches(".git");
    let mut p = rest.split('/');
    let (o, r) = (p.next()?, p.next()?);
    (!o.is_empty() && !r.is_empty()).then(|| format!("{o}/{r}"))
}

/// What [`probe`] found out about the item's result upstream.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Probe {
    /// The rule asked for holds.
    pub met: bool,
    /// The remote default branch looked at (`origin/main`).
    pub branch: String,
    /// The upstream commit that brought the item's change in.
    pub landed: Option<String>,
    /// The pushed tag that contains it, and the tag's commit.
    pub tag: Option<(String, String)>,
    /// `owner/repo` when the remote is on GitHub.
    pub slug: Option<String>,
    /// What is still missing, in words (empty when `met`).
    pub missing: Vec<String>,
}

impl Probe {
    /// One line for the timeline: what holds, or what is missing.
    pub fn summary(&self) -> String {
        let short = |s: &str| s.chars().take(7).collect::<String>();
        if !self.missing.is_empty() {
            return self.missing.join("；");
        }
        let mut s = format!(
            "提交已合并到 {}（{}）",
            self.branch,
            self.landed.as_deref().map(short).unwrap_or_default()
        );
        if let Some((t, c)) = &self.tag {
            s.push_str(&format!("，tag {t} 已推送（{}）", short(c)));
        }
        s
    }
}

fn missing(m: impl Into<String>) -> Probe {
    Probe {
        missing: vec![m.into()],
        ..Default::default()
    }
}

/// Where the commit `head` of the worktree `dir` stands against `rule` (`merge` or `tag`), after
/// fetching the remote `origin`. Talks to the network: never under the core's lock.
pub fn probe(dir: &Path, rule: &str) -> Probe {
    if git(dir, &["status", "--porcelain"]).is_ok_and(|s| !s.is_empty()) {
        return missing("工作区还有没提交的改动");
    }
    /* the URL as configured (`get-url` would apply `insteadOf`): it names the repository */
    let Ok(url) = git(dir, &["config", "--get", "remote.origin.url"]) else {
        return missing("没有 origin 远端，核对不了是否已合并");
    };
    if let Err(e) = fetch(dir) {
        return missing(format!("取不到远端的最新状态：{e}"));
    }
    let Some(up) = [
        "refs/remotes/origin/HEAD",
        "refs/remotes/origin/main",
        "refs/remotes/origin/master",
    ]
    .into_iter()
    .find(|r| git(dir, &["rev-parse", "--verify", "--quiet", r]).is_ok()) else {
        return missing("远端没有默认分支");
    };
    let branch = git(dir, &["rev-parse", "--abbrev-ref", up])
        .ok()
        .filter(|b| !b.is_empty() && b != "origin/HEAD")
        .unwrap_or_else(|| up.trim_start_matches("refs/remotes/").to_string());
    let mut p = Probe {
        branch: branch.clone(),
        slug: github_slug(&url),
        ..Default::default()
    };
    let Ok(head) = git(dir, &["rev-parse", "HEAD"]) else {
        return missing("读不到 HEAD");
    };
    match presence(dir, &head, up) {
        Presence::Landed { commit } => p.landed = Some(commit),
        Presence::Absent => {
            p.missing
                .push(format!("提交还不在 {branch} 上（PR 还没合并）"));
            return p;
        }
        Presence::Unknown(e) => {
            p.missing
                .push(format!("查不到提交是否已在 {branch} 上：{e}"));
            return p;
        }
    }
    if rule == DONE_TAG {
        p.tag = pushed_tag(dir, p.landed.as_deref().unwrap_or(&head));
        if p.tag.is_none() {
            p.missing.push("还没有推送包含这些提交的 tag".into());
            return p;
        }
    }
    p.met = true;
    p
}

/// Whether a change is on a target branch. Not finding it and not being able to look are kept apart:
/// a failed git is [`Presence::Unknown`], never [`Presence::Absent`].
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
/// Local git only: fetch first ([`fetch`]).
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
    let mb = git(dir, &["merge-base", head, up])?;
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

/// The pushed tag closest after `commit` that contains it (`(name, tag commit)`).
fn pushed_tag(dir: &Path, commit: &str) -> Option<(String, String)> {
    let out = git_net(dir, &["ls-remote", "--tags", "origin"]).ok()?;
    let mut tags: Vec<(String, String)> = vec![];
    for l in out.lines() {
        let Some((sha, r)) = l.split_once('\t') else {
            continue;
        };
        let Some(name) = r.strip_prefix("refs/tags/") else {
            continue;
        };
        /* the peeled line (`^{}`) of an annotated tag names its commit */
        let (name, peeled) = match name.strip_suffix("^{}") {
            Some(n) => (n, true),
            None => (name, false),
        };
        match tags.iter_mut().find(|(n, _)| n == name) {
            Some(t) if peeled => t.1 = sha.into(),
            Some(_) => {}
            None => tags.push((name.into(), sha.into())),
        }
    }
    tags.into_iter()
        .filter(|(_, c)| {
            git(dir, &["cat-file", "-e", &format!("{c}^{{commit}}")]).is_ok()
                && git(dir, &["merge-base", "--is-ancestor", commit, c]).is_ok()
        })
        .filter_map(|(n, c)| {
            let d = git(dir, &["rev-list", "--count", &format!("{commit}..{c}")])
                .ok()?
                .parse::<u64>()
                .ok()?;
            Some((d, n, c))
        })
        .min_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)))
        .map(|(_, n, c)| (n, c))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn github_slugs() {
        assert_eq!(
            github_slug("git@github.com:Infranite/steve.git").as_deref(),
            Some("Infranite/steve")
        );
        assert_eq!(
            github_slug("https://github.com/Infranite/steve").as_deref(),
            Some("Infranite/steve")
        );
        assert_eq!(github_slug("/tmp/origin.git"), None);
    }

    #[test]
    fn fingerprints_parse_in_both_forms() {
        let p = parse("h:t");
        assert_eq!((p.head, p.tree, p.own), ("h", "t", None));
        let p = parse("h:t:b:x");
        assert_eq!(p.own, Some(("b", "x")));
    }
}
