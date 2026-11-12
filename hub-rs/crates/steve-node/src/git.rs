//! Git in a workspace on this machine.
//!
//! [`steve_node_api::WorkspaceExecutor::git_facts`]: what git says in a workspace, read through the
//! workspace's [`Commands`] pipe (registered, bounded by its deadline, never a lazy fetch). Only
//! fixed read commands; a ref or revision never becomes an option. A fetch (and `RemoteTags`)
//! talks to the remote under [`NET_DEADLINE`]. A rebase ([`rebase`]) runs through the same pipe.
//!
//! The push (N0e-E8, [`GitAction::Push`]), as the Hub's own did it:
//! - the workspace's remote must point at `origin_must_be` (`github.com/<owner>/<name>`, the URL
//!   as configured, not rewritten by `insteadOf`), else it is refused;
//! - the branch must be at `expected_local`, and HEAD and the revision as the Hub saw them;
//! - it runs from a throwaway bare repository under the Node's home (`push/<op>`) that borrows the
//!   workspace's objects (`objects/info/alternates`), so the workspace's own config (hooks,
//!   `http.proxy`, `insteadOf`, credential helpers) plays no part;
//! - the credential, taken from the Hub ([`crate::secrets`]) once, right before, goes in as
//!   `http.<url>.extraheader` through `GIT_CONFIG_COUNT` environment variables: not on the command
//!   line, not in a file, not in the journal;
//! - the target is the canonical `url`, as a fast-forward or under the `expected_remote` lease;
//! - it is a command like [`crate::exec`]: its process group is journaled before git starts, killed
//!   at the deadline, and a Node that died while it ran leaves it `unknown` (never pushed again).
//!
//! The git that sets up the bare repository runs with the system and global config off, no
//! prompts, and nothing in the environment that points it elsewhere (`GIT_DIR`,
//! `GIT_CONFIG_PARAMETERS`…). With `STEVE_FORBID_REAL_HOSTS=1` (the test guard) a push to anything
//! but a local path or a loopback address is refused here too, whatever the Hub let through.

use std::io::Read;
use std::os::unix::process::CommandExt;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::{Duration, Instant};

use sha2::Digest as _;
use steve_node_api::*;
use zeroize::Zeroizing;

use crate::workspace::{Commands, Failed};

/// What one fixed command may print before it is cut (a patch or a file is cut at the read's own
/// `max`, well below this).
const OUT_CAP: usize = 8 << 20;
/// Bytes of a patch read past `max`, so the cut is made after git's leading blank line is trimmed.
const SLACK: usize = 4096;
/// What a patch fed to `patch-id` may be (a branch's whole upstream log, for `Landed`).
const PATCH_CAP: usize = 64 << 20;
/// How long a fetch or `ls-remote` may take.
pub(crate) const NET_DEADLINE: std::time::Duration = std::time::Duration::from_secs(60);
/// How long one local read may take.
const READ_DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

/// The refs a branch may have started from, most authoritative first (as the Hub's own git has
/// always looked for them).
const BASES: &[&str] = &[
    "refs/remotes/origin/HEAD",
    "refs/remotes/origin/main",
    "refs/remotes/origin/master",
    "refs/heads/main",
    "refs/heads/master",
];

fn invalid(what: impl Into<String>) -> ApiError {
    ApiError::new(ErrorCode::Invalid, what)
}

/// A ref or revision the Hub names: never empty, never an option, no NUL, no space.
fn rev(s: &str) -> ApiResult<&str> {
    if s.is_empty() || s.starts_with('-') || s.chars().any(|c| c.is_whitespace() || c == '\0') {
        return Err(invalid("not a revision"));
    }
    Ok(s)
}

fn text(b: &[u8]) -> String {
    String::from_utf8_lossy(b).trim().to_string()
}

/// `s` cut to at most `max` bytes at a character, and whether it was.
fn cut(mut s: String, max: usize, more: bool) -> (String, bool) {
    if s.len() <= max {
        return (s, more);
    }
    let mut end = max;
    while !s.is_char_boundary(end) {
        end -= 1;
    }
    s.truncate(end);
    (s, true)
}

struct Git<'a> {
    c: &'a Commands,
    dir: &'a Path,
}

impl Git<'_> {
    /// What git printed. Git refusing is `invalid`; git not finishing under the pipe (its deadline,
    /// a cancel) is `internal`, never mistaken for an answer.
    fn out(&self, args: &[&str], cap: usize) -> ApiResult<(Vec<u8>, bool)> {
        self.c.read(self.dir, args, cap).map_err(|f| match f {
            Failed::Git(e) => invalid(e),
            Failed::Pipe(e) => ApiError::new(ErrorCode::Internal, e),
        })
    }
    /// What git printed, or `None` when git refused.
    fn may(&self, args: &[&str]) -> ApiResult<Option<String>> {
        match self.line(args) {
            Ok(s) => Ok(Some(s)),
            Err(e) if e.code == ErrorCode::Invalid => Ok(None),
            Err(e) => Err(e),
        }
    }
    fn line(&self, args: &[&str]) -> ApiResult<String> {
        Ok(text(&self.out(args, 64 * 1024)?.0))
    }
    /// The commit `r` names, or `None` when it names none.
    fn commit(&self, r: &str) -> ApiResult<Option<String>> {
        let spec = format!("{}^{{commit}}", rev(r)?);
        Ok(self
            .may(&["rev-parse", "--verify", "--quiet", &spec])?
            .filter(|s| !s.is_empty()))
    }
    fn must(&self, r: &str) -> ApiResult<String> {
        self.commit(r)?
            .ok_or_else(|| invalid(format!("no such revision: {r}")))
    }
    /// A git that answers with its exit status (`merge-base --is-ancestor`): 0 yes, 1 no, anything
    /// else an error, never a no.
    fn test(&self, args: &[&str]) -> ApiResult<bool> {
        let r = self.ran(args, &[], &[], 64 * 1024, READ_DEADLINE)?;
        match r.code {
            Some(0) => Ok(true),
            Some(1) => Ok(false),
            _ => Err(invalid(r.stderr)),
        }
    }
    /// One git under the pipe as it ran; only a git that did not finish is an error (`internal`).
    fn ran(
        &self,
        args: &[&str],
        env: &[(&str, &str)],
        input: &[u8],
        cap: usize,
        deadline: std::time::Duration,
    ) -> ApiResult<crate::workspace::Ran> {
        let r = self
            .c
            .read_with(self.dir, args, env, input, cap, deadline)
            .map_err(|f| ApiError::new(ErrorCode::Internal, String::from(f)))?;
        if r.code.is_none() {
            return Err(ApiError::new(
                ErrorCode::Internal,
                "git cancelled or timed out",
            ));
        }
        Ok(r)
    }
    /// The merge base of two commits: `None` when they share no history.
    fn merge_base(&self, a: &str, b: &str) -> ApiResult<Option<String>> {
        let r = self.ran(&["merge-base", a, b], &[], &[], 64 * 1024, READ_DEADLINE)?;
        match r.code {
            Some(0) => Ok(Some(text(&r.stdout))),
            Some(1) if r.stderr.is_empty() => Ok(None),
            _ => Err(invalid(r.stderr)),
        }
    }
    fn count(&self, span: &str) -> ApiResult<u32> {
        self.line(&["rev-list", "--count", span])?
            .parse()
            .map_err(|_| invalid("rev-list printed no count"))
    }
    /// `(patch id, commit)` of each commit (or the one diff) `args` prints, through
    /// `patch-id --stable`; empty when there is no change. A patch past [`PATCH_CAP`] is an error:
    /// part of it would give another id.
    fn patch_ids(&self, args: &[&str]) -> ApiResult<Vec<(String, String)>> {
        let src = self.ran(args, &[], &[], PATCH_CAP, READ_DEADLINE)?;
        if src.code != Some(0) {
            return Err(invalid(src.stderr));
        }
        if src.truncated {
            return Err(invalid("the patch is too big to compare"));
        }
        let ids = self.ran(
            &["patch-id", "--stable"],
            &[],
            &src.stdout,
            1 << 20,
            READ_DEADLINE,
        )?;
        if ids.code != Some(0) {
            return Err(invalid(ids.stderr));
        }
        Ok(String::from_utf8_lossy(&ids.stdout)
            .lines()
            .filter_map(|l| {
                let mut p = l.split_whitespace();
                Some((p.next()?.to_string(), p.next()?.to_string()))
            })
            .collect())
    }
    /// Where `head`'s branch left the line it came from: its merge base with the nearest of the
    /// default branches there are.
    fn base_of(&self, head: &str) -> ApiResult<Option<String>> {
        let mut refs = vec![];
        for r in BASES {
            if self.commit(r)?.is_some() {
                refs.push(*r);
            }
        }
        if refs.is_empty() {
            return Ok(None);
        }
        let mut args = vec!["merge-base", head];
        args.extend(refs);
        Ok(self.may(&args)?.filter(|b| !b.is_empty()))
    }
    /// The tree the working directory would commit as, untracked files included, through a
    /// throwaway index (the workspace's own index is never touched), and the own change in it.
    fn fingerprint(&self, head: &str) -> ApiResult<GitAnswer> {
        let index = std::env::temp_dir().join(format!(
            "steve-node-fingerprint-{}-{}",
            std::process::id(),
            FINGERPRINTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed)
        ));
        let out = self.tree_with(&index);
        let _ = std::fs::remove_file(&index);
        let tree = out?;
        let own = match self.base_of(head)? {
            Some(base) => {
                let patch = self
                    .patch_ids(&["diff-tree", "-p", "--no-color", &base, &tree])?
                    .into_iter()
                    .next()
                    .map(|(p, _)| p)
                    .unwrap_or_else(|| "-".into());
                Some(OwnChange { base, patch })
            }
            None => None,
        };
        Ok(GitAnswer::Fingerprint { tree, own })
    }
    fn tree_with(&self, index: &Path) -> ApiResult<String> {
        let ix = index.to_string_lossy().to_string();
        let env = [("GIT_INDEX_FILE", ix.as_str())];
        let step = |args: &[&str]| -> ApiResult<String> {
            let r = self.ran(args, &env, &[], 64 * 1024, READ_DEADLINE)?;
            match r.code {
                Some(0) => Ok(text(&r.stdout)),
                _ => Err(invalid(r.stderr)),
            }
        };
        // start from the workspace's index (its stat cache keeps this fast), then take in the tree
        let real = self.may(&["rev-parse", "--path-format=absolute", "--git-path", "index"])?;
        match real.filter(|r| Path::new(r).exists()) {
            Some(r) => {
                std::fs::copy(&r, index)
                    .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?;
            }
            None => {
                step(&["read-tree", "HEAD"])?;
            }
        }
        step(&["add", "--all"])?;
        step(&["write-tree"])
    }
    /// The commit on `up` that brought `head`'s change in (the Hub's `upstream::presence`).
    fn landed(&self, head: &str, up: &str) -> ApiResult<Option<String>> {
        if self.test(&["merge-base", "--is-ancestor", head, up])? {
            return Ok(Some(head.into()));
        }
        // no shared history: it can't be on `up`
        let Some(mb) = self.merge_base(head, up)? else {
            return Ok(None);
        };
        let range = format!("{mb}..{up}");
        let upstream = self.patch_ids(&["log", "--no-merges", "-p", "--no-color", &range])?;
        let find = |pid: &str| {
            upstream
                .iter()
                .find(|(p, _)| p == pid)
                .map(|(_, c)| c.clone())
        };
        let cherry = self.line(&["cherry", up, head])?;
        if !cherry.is_empty() && cherry.lines().all(|l| l.starts_with('-')) {
            let tip = self.patch_ids(&["show", "-p", "--no-color", "--format=commit %H", head])?;
            if let Some(c) = tip.first().and_then(|(p, _)| find(p)) {
                return Ok(Some(c));
            }
        }
        // squash merge: one upstream commit with the whole change
        let all = self.patch_ids(&["diff-tree", "-p", "--no-color", &mb, head])?;
        Ok(all.first().and_then(|(p, _)| find(p)))
    }
    /// The working tree's changes, as `status --porcelain -z` lists them.
    fn status(&self) -> ApiResult<Vec<StatusEntry>> {
        let (out, _) = self.out(&["status", "--porcelain", "-z"], OUT_CAP)?;
        Ok(porcelain(&out))
    }
    /// Stat and patch of `cmd … spec` (`show --format= <sha>`, `diff <a>...<b>`) with paths as they
    /// are (`docs/对话.md`), the patch cut at `max`.
    fn stat_patch(&self, cmd: &[&str], spec: &str, max: u32) -> ApiResult<(String, String, bool)> {
        fn with<'a>(cmd: &[&'a str], extra: &[&'a str], spec: &'a str) -> Vec<&'a str> {
            let mut all = vec!["-c", "core.quotePath=false"];
            all.extend_from_slice(cmd);
            all.extend_from_slice(extra);
            all.push(spec);
            all
        }
        let stat = text(&self.out(&with(cmd, &["--stat=120"], spec), OUT_CAP)?.0);
        let (patch, more) = self.out(
            &with(cmd, &["--patch", "--no-color"], spec),
            (max as usize + SLACK).min(OUT_CAP),
        )?;
        let (patch, truncated) = cut(text(&patch), max as usize, more);
        Ok((stat, patch, truncated))
    }

    fn answer(&self, r: &GitRead) -> ApiResult<(String, GitAnswer)> {
        Ok(match r {
            GitRead::Show {
                rev: at,
                path: Some(path),
                max,
            } => {
                let Some(sha) = self.commit(at)? else {
                    return Ok((String::new(), not_found()));
                };
                let spec = format!("{sha}:{}", path.as_str());
                match self.out(&["show", &spec], *max as usize) {
                    Ok((bytes, truncated)) => (
                        sha,
                        GitAnswer::Blob {
                            found: true,
                            bytes,
                            truncated,
                        },
                    ),
                    Err(e) if e.code == ErrorCode::Invalid => (sha, not_found()),
                    Err(e) => return Err(e),
                }
            }
            GitRead::Show {
                rev: at,
                path: None,
                max,
            } => {
                let Some(sha) = self.commit(at)? else {
                    return Ok((String::new(), not_found()));
                };
                let meta = self.line(&[
                    "show",
                    "-s",
                    "--format=%H%x1f%h%x1f%s%x1f%an%x1f%ct%x1f%B",
                    &sha,
                ])?;
                let f: Vec<&str> = meta.splitn(6, '\u{1f}').collect();
                let (stat, patch, truncated) =
                    self.stat_patch(&["show", "--format="], &sha, *max)?;
                let commit = CommitInfo {
                    sha: f.first().copied().unwrap_or(&sha).to_string(),
                    abbrev: f.get(1).map(|h| h.to_string()),
                    subject: f.get(2).copied().unwrap_or_default().to_string(),
                    author: f.get(3).copied().unwrap_or_default().to_string(),
                    time: Timestamp(
                        f.get(4).and_then(|t| t.parse::<i64>().ok()).unwrap_or(0) * 1000,
                    ),
                    body: Some(f.get(5).copied().unwrap_or_default().to_string()),
                };
                (
                    sha,
                    GitAnswer::Commit {
                        commit,
                        stat,
                        patch,
                        truncated,
                    },
                )
            }
            GitRead::Log { range, max } => {
                let to = self.must(&range.to)?;
                let span = match &range.from {
                    Some(from) => format!("{}..{to}", self.must(from)?),
                    None => to.clone(),
                };
                // one more than asked: whether there are more
                let n = (u64::from(*max) + 1).to_string();
                let log = self.line(&[
                    "log",
                    "--no-color",
                    "-n",
                    &n,
                    "--format=%H%x1f%h%x1f%s%x1f%an%x1f%ct",
                    &span,
                ])?;
                let mut commits: Vec<CommitInfo> = log
                    .lines()
                    .filter(|l| !l.is_empty())
                    .map(|l| {
                        let f: Vec<&str> = l.splitn(5, '\u{1f}').collect();
                        CommitInfo {
                            sha: f.first().copied().unwrap_or_default().to_string(),
                            abbrev: f.get(1).map(|h| h.to_string()),
                            subject: f.get(2).copied().unwrap_or_default().to_string(),
                            author: f.get(3).copied().unwrap_or_default().to_string(),
                            time: Timestamp(
                                f.get(4).and_then(|t| t.parse::<i64>().ok()).unwrap_or(0) * 1000,
                            ),
                            body: None,
                        }
                    })
                    .collect();
                let truncated = commits.len() > *max as usize;
                commits.truncate(*max as usize);
                (to, GitAnswer::Log { commits, truncated })
            }
            GitRead::Diff { range, max } => {
                let Some(from) = &range.from else {
                    return Err(ApiError::new(
                        ErrorCode::Unsupported,
                        "a diff needs the side it is from",
                    ));
                };
                let to = self.must(&range.to)?;
                // what `to` changed since it left `from` (its commits of the log above)
                let span = format!("{}...{to}", self.must(from)?);
                let (stat, patch, truncated) = self.stat_patch(&["diff"], &span, *max)?;
                (
                    to,
                    GitAnswer::Diff {
                        stat,
                        patch,
                        truncated,
                    },
                )
            }
            GitRead::IsAncestor { ancestor, of } => {
                let a = self.must(ancestor)?;
                let of = self.must(of)?;
                let yes = self.test(&["merge-base", "--is-ancestor", &a, &of])?;
                (of, GitAnswer::IsAncestor { yes })
            }
            GitRead::Status => (
                self.must("HEAD")?,
                GitAnswer::Status {
                    entries: self.status()?,
                },
            ),
            GitRead::Fingerprint => {
                let head = self.must("HEAD")?;
                let a = self.fingerprint(&head)?;
                (head, a)
            }
            GitRead::ChangedPaths { from, to, max } => {
                let (from, to) = (rev(from)?, rev(to)?);
                let (out, more) = self.out(&["diff", "--name-only", "-z", from, to], OUT_CAP)?;
                let mut paths: Vec<String> = out
                    .split(|b| *b == 0)
                    .filter(|p| !p.is_empty())
                    .map(|p| String::from_utf8_lossy(p).to_string())
                    .collect();
                let truncated = more || paths.len() > *max as usize;
                paths.truncate(*max as usize);
                (to.to_string(), GitAnswer::ChangedPaths { paths, truncated })
            }
            GitRead::Landed { commit, onto } => {
                let commit = self.must(commit)?;
                let up = self.must(onto)?;
                let landed = self.landed(&commit, &up)?;
                (up, GitAnswer::Landed { commit: landed })
            }
            GitRead::RemoteTags { remote } => {
                let remote = rev(remote)?;
                let r = self.ran(
                    &["ls-remote", "--tags", remote],
                    &[],
                    &[],
                    OUT_CAP,
                    NET_DEADLINE,
                )?;
                if r.code != Some(0) {
                    return Err(invalid(r.stderr));
                }
                (
                    String::new(),
                    GitAnswer::RemoteTags {
                        tags: remote_tags(&String::from_utf8_lossy(&r.stdout)),
                    },
                )
            }
            GitRead::RemoteUrl { remote } => {
                let key = format!("remote.{}.url", rev(remote)?);
                let url = self
                    .may(&["config", "--get", &key])?
                    .map(|u| no_credentials(&u));
                (String::new(), GitAnswer::RemoteUrl { url })
            }
            GitRead::TrackedIgnored { .. } => {
                return Err(ApiError::new(
                    ErrorCode::Unsupported,
                    "this git read is not served here yet",
                ));
            }
        })
    }

    /// What `from..to` holds each way, and where they met.
    fn range(&self, from: &str, to: &str) -> ApiResult<RangeFacts> {
        let a = self.must(from)?;
        let b = self.must(to)?;
        let merge_base = self.merge_base(&a, &b)?;
        // the paths both sides changed since they met
        let mut overlap = vec![];
        if let Some(mb) = &merge_base {
            let changed = |side: &str| -> ApiResult<Vec<RelPath>> {
                let o = self.out(
                    &["diff", "--name-only", "-z", "--no-renames", mb, side],
                    OUT_CAP,
                )?;
                Ok(names(&o.0))
            };
            let theirs = changed(&b)?;
            overlap = changed(&a)?
                .into_iter()
                .filter(|p| theirs.contains(p))
                .collect();
        }
        // exit 0: it merges cleanly; 1: the conflicted paths follow the tree
        let m = self.ran(
            &[
                "merge-tree",
                "--write-tree",
                "--name-only",
                "--no-messages",
                "-z",
                &a,
                &b,
            ],
            &[],
            &[],
            OUT_CAP,
            READ_DEADLINE,
        )?;
        let conflicts = match m.code {
            Some(0) => vec![],
            Some(1) => {
                let mut parts = m.stdout.splitn(2, |b| *b == 0);
                parts.next();
                names(parts.next().unwrap_or_default())
            }
            _ => return Err(invalid(format!("merge-tree: {}", m.stderr))),
        };
        Ok(RangeFacts {
            merge_base,
            ahead: self.count(&format!("{a}..{b}"))?,
            behind: self.count(&format!("{b}..{a}"))?,
            from: a,
            to: b,
            overlap,
            conflicts,
        })
    }
}

fn not_found() -> GitAnswer {
    GitAnswer::Blob {
        found: false,
        bytes: vec![],
        truncated: false,
    }
}

static FINGERPRINTS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// `-z` names, as relative paths (one that is no valid relative path is left out).
fn names(bytes: &[u8]) -> Vec<RelPath> {
    bytes
        .split(|b| *b == 0)
        .filter(|n| !n.is_empty())
        .filter_map(|n| RelPath::new(String::from_utf8_lossy(n).into_owned()).ok())
        .collect()
}

/// `status --porcelain -z` as entries: a rename's or copy's old path (the field after it) is
/// skipped, a directory's trailing `/` dropped; a path that is no relative path is left out.
fn porcelain(out: &[u8]) -> Vec<StatusEntry> {
    let mut entries = vec![];
    let mut fields = out.split(|b| *b == 0).filter(|f| !f.is_empty());
    while let Some(f) = fields.next() {
        if f.len() < 4 {
            continue;
        }
        let code = String::from_utf8_lossy(&f[..2]).to_string();
        let path = String::from_utf8_lossy(&f[3..]);
        if code.contains('R') || code.contains('C') {
            fields.next();
        }
        if let Ok(path) = RelPath::new(path.trim_end_matches('/')) {
            entries.push(StatusEntry { path, code });
        }
    }
    entries
}

/// `ls-remote --tags` as `(name, commit)`: the peeled line (`^{}`) of an annotated tag names its
/// commit.
fn remote_tags(out: &str) -> Vec<(String, String)> {
    let mut tags: Vec<(String, String)> = vec![];
    for l in out.lines() {
        let Some((sha, r)) = l.split_once('\t') else {
            continue;
        };
        let Some(name) = r.strip_prefix("refs/tags/") else {
            continue;
        };
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
    tags
}

/// A remote URL without what may be a credential in it (`https://user:token@host/…`).
fn no_credentials(url: &str) -> String {
    let Some((scheme, rest)) = url.split_once("://") else {
        return url.to_string();
    };
    let (auth, path) = rest.split_at(rest.find('/').unwrap_or(rest.len()));
    match auth.rsplit_once('@') {
        Some((_, host)) => format!("{scheme}://{host}{path}"),
        None => url.to_string(),
    }
}

/// The facts `q` asks for in the checkout at `dir`, after fetching `origin` (with its tags) when it
/// asks for that. A `dir` that is no checkout with a commit, or a read git refuses, is `invalid`; a
/// fetch the remote refuses is `invalid` with git's words; git not finishing (its deadline) is
/// `internal`.
pub(crate) fn facts(c: &Commands, dir: &Path, q: &GitQuery) -> ApiResult<GitFacts> {
    let g = Git { c, dir };
    if q.fetch {
        let r = c
            .write_with(
                dir,
                &["fetch", "--quiet", "--tags", "origin"],
                &[],
                NET_DEADLINE,
            )
            .map_err(|f| ApiError::new(ErrorCode::Internal, String::from(f)))?;
        match r.code {
            Some(0) => {}
            Some(_) => return Err(invalid(format!("fetch: {}", r.stderr))),
            None => return Err(ApiError::new(ErrorCode::Internal, "fetch timed out")),
        }
    }
    let (head, branch, status, revision) = observe(&g, q.heads_only)?;
    let mut refs = vec![];
    for r in &q.refs {
        refs.push((r.clone(), g.commit(r)?));
    }
    let mut ranges = vec![];
    for (from, to) in &q.ranges {
        ranges.push(g.range(from, to)?);
    }
    let mut reads = vec![];
    for r in &q.reads {
        let (observed, answer) = g.answer(r)?;
        reads.push(GitReadFact {
            read: r.clone(),
            observed,
            answer,
        });
    }
    Ok(GitFacts {
        revision,
        head,
        branch,
        dirty: !status.is_empty(),
        refs,
        ranges,
        reads,
    })
}

/// HEAD, the branch, the working tree's status and the revision they make (what a decision made
/// on them checks again).
fn observe(g: &Git, heads_only: bool) -> ApiResult<(String, Option<String>, Vec<u8>, Revision)> {
    let head = g
        .commit("HEAD")?
        .ok_or_else(|| invalid("not a git checkout with a commit"))?;
    let branch = g
        .may(&["symbolic-ref", "--short", "-q", "HEAD"])?
        .filter(|b| !b.is_empty());
    // a preview reads no working tree: no untracked-file scan for it
    let status = match heads_only {
        true => vec![],
        false => g.out(&["status", "--porcelain", "-z"], OUT_CAP)?.0,
    };
    let mut h = sha2::Sha256::new();
    h.update(head.as_bytes());
    h.update([0]);
    h.update(branch.as_deref().unwrap_or_default().as_bytes());
    h.update([0]);
    h.update(&status);
    let kind = if heads_only { "h" } else { "g" };
    let revision = Revision::new(format!("{kind}{}", &hex::encode(h.finalize())[..32]))
        .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?;
    Ok((head, branch, status, revision))
}

/// The facts of the checkout at `dir` with nothing asked beyond them (what an apply checks).
pub(crate) fn now(c: &Commands, dir: &Path) -> ApiResult<GitFacts> {
    let (head, branch, status, revision) = observe(&Git { c, dir }, false)?;
    Ok(GitFacts {
        revision,
        head,
        branch,
        dirty: !status.is_empty(),
        refs: vec![],
        ranges: vec![],
        reads: vec![],
    })
}

/// How long a rebase may take (its hooks included).
const REBASE_DEADLINE: std::time::Duration = std::time::Duration::from_secs(300);

/// Rebases the branch checked out at `dir` onto `onto`, if the checkout is still at `expected`
/// and `expected_head` (`stale` otherwise). Uncommitted changes to tracked files are `busy`; a
/// conflict is backed out and is `conflict` with the paths, one per line; `onto` already under
/// HEAD changes nothing. The facts after it.
pub(crate) fn rebase(
    c: &Commands,
    dir: &Path,
    onto: &str,
    expected: &Revision,
    expected_head: &str,
) -> ApiResult<GitFacts> {
    let g = Git { c, dir };
    let before = now(c, dir)?;
    if before.revision != *expected || before.head != expected_head {
        return Err(ApiError::new(ErrorCode::Stale, "changed since"));
    }
    let onto = g.must(onto)?;
    let tracked = text(
        &g.out(&["status", "--porcelain", "--untracked-files=no"], OUT_CAP)?
            .0,
    );
    if !tracked.is_empty() {
        let lines: Vec<&str> = tracked.lines().take(10).collect();
        return Err(ApiError::new(ErrorCode::Busy, lines.join("\n")));
    }
    if g.test(&["merge-base", "--is-ancestor", &onto, "HEAD"])? {
        return Ok(before);
    }
    let pipe = |f| ApiError::new(ErrorCode::Internal, String::from(f));
    let r = c
        .write_with(dir, &["rebase", &onto], &[], REBASE_DEADLINE)
        .map_err(pipe)?;
    if r.code == Some(0) {
        return now(c, dir);
    }
    let files = c
        .write_with(
            dir,
            &["diff", "--name-only", "--diff-filter=U"],
            &[],
            READ_DEADLINE,
        )
        .map(|r| text(&r.stdout))
        .unwrap_or_default();
    let abort = c
        .write_with(dir, &["rebase", "--abort"], &[], READ_DEADLINE)
        .map_err(pipe)?;
    if abort.code != Some(0)
        && g.may(&[
            "rev-parse",
            "--path-format=absolute",
            "--git-path",
            "rebase-merge",
        ])?
        .is_some_and(|p| Path::new(&p).exists())
    {
        return Err(ApiError::new(
            ErrorCode::Internal,
            format!("the rebase could not be backed out: {}", abort.stderr),
        ));
    }
    Err(match (r.code, files.is_empty()) {
        (None, _) => ApiError::new(
            ErrorCode::Internal,
            "the rebase timed out and was backed out",
        ),
        (_, false) => ApiError::new(ErrorCode::Conflict, files),
        (_, true) => invalid(r.stderr),
    })
}

// ── the push ──

/// How long one read may take.
const READ_TIMEOUT: Duration = Duration::from_secs(30);
/// What one read may print.
const READ_CAP: usize = 4 << 20;
/// How long a push may take by default ([`crate::LocalNode::set_push_timeout`]). git also gives
/// up by itself on a stalled transfer (`http.lowSpeed*`).
pub(crate) const PUSH_TIMEOUT: Duration = Duration::from_secs(5 * 60);
/// What a push's output keeps.
const PUSH_OUTPUT: usize = 16 * 1024;
/// What of git's complaint goes into an error.
const ERROR_CHARS: usize = 300;

/// The detail of a push refused by the test guard (the Hub shows its own text for it).
pub const HOST_FORBIDDEN: &str = "push target forbidden by STEVE_FORBID_REAL_HOSTS";
/// The details of a push whose branch is not where the Hub saw it, or is gone.
pub const BRANCH_MOVED: &str = "the branch moved";
pub const NO_BRANCH: &str = "no such branch";
/// The detail of a push whose workspace's remote points elsewhere.
pub const WRONG_REMOTE: &str = "the workspace's remote points elsewhere";

/// The process environment git must not take from the Node (or the Hub it runs in).
const UNSET: [&str; 14] = [
    "GIT_DIR",
    "GIT_WORK_TREE",
    "GIT_INDEX_FILE",
    "GIT_OBJECT_DIRECTORY",
    "GIT_ALTERNATE_OBJECT_DIRECTORIES",
    "GIT_CONFIG",
    "GIT_CONFIG_PARAMETERS",
    "GIT_CONFIG_COUNT",
    "GIT_NAMESPACE",
    "GIT_SSH",
    "GIT_SSH_COMMAND",
    "GIT_ASKPASS",
    "SSH_ASKPASS",
    "GIT_PROXY_COMMAND",
];

/// The push command: its URL, refspec and lease come in as variables, never as text of it.
const PUSH_SH: &str = "exec git -c core.hooksPath=/dev/null push --quiet --no-verify \
                       ${STEVE_PUSH_LEASE:+\"--force-with-lease=$STEVE_PUSH_LEASE\"} \
                       -- \"$STEVE_PUSH_URL\" \"$STEVE_PUSH_REFSPEC\"";

/// `git -C dir` with the system and global config off and no prompts.
fn git(dir: &Path) -> std::process::Command {
    let mut c = std::process::Command::new("git");
    for k in UNSET {
        c.env_remove(k);
    }
    for k in [
        "STEVE_UPGRADE_CONTROL_TOKEN",
        "STEVE_SUPERVISOR_PID",
        "STEVE_UPGRADE_ID",
    ] {
        c.env_remove(k);
    }
    c.arg("-C")
        .arg(dir)
        // reads take no lock and run no repository-configured monitor
        .args(["--no-optional-locks", "-c", "core.fsmonitor=false"])
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env("GIT_OPTIONAL_LOCKS", "0")
        .process_group(0)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    c
}

struct Out {
    code: Option<i32>,
    stderr: Vec<u8>,
}

impl Out {
    fn complaint(&self) -> String {
        String::from_utf8_lossy(&self.stderr)
            .trim()
            .chars()
            .take(ERROR_CHARS)
            .collect()
    }
}

/// Runs a git read in `dir`, bounded; its group is killed when it ends.
fn read(dir: &Path, args: &[&str]) -> ApiResult<Out> {
    let mut c = git(dir);
    c.args(args);
    let mut child = c
        .spawn()
        .map_err(|e| ApiError::new(ErrorCode::Internal, format!("git: {e}")))?;
    let pgid = child.id() as libc::pid_t;
    let drain = |p: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut v = Vec::new();
            if let Some(p) = p {
                let _ = p.take(READ_CAP as u64 + 1).read_to_end(&mut v);
            }
            v
        })
    };
    let out = drain(child.stdout.take().map(|p| Box::new(p) as _));
    let err = drain(child.stderr.take().map(|p| Box::new(p) as _));
    let until = Instant::now() + READ_TIMEOUT;
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Some(s),
            Ok(None) if Instant::now() < until => std::thread::sleep(Duration::from_millis(5)),
            _ => break None,
        }
    };
    // what it started goes with it (the group's leader may be reaped: only its group id is used)
    unsafe {
        libc::kill(-pgid, libc::SIGKILL);
    }
    let Some(status) = status else {
        let _ = child.kill();
        let _ = child.wait();
        return Err(ApiError::new(ErrorCode::Expired, "git took too long"));
    };
    let stdout = out.join().unwrap_or_default();
    let stderr = err.join().unwrap_or_default();
    if stdout.len() > READ_CAP {
        return Err(ApiError::new(ErrorCode::Exhausted, "git printed too much"));
    }
    Ok(Out {
        code: status.code(),
        stderr,
    })
}

/// Branch names a push takes: `steve/st-12` and the like (as the Hub's own push did).
pub(crate) fn branch_ok(b: &str) -> bool {
    !b.is_empty()
        && b.len() <= 200
        && !b.starts_with('-')
        && !b.starts_with('/')
        && !b.ends_with('/')
        && !b.ends_with(".lock")
        && !b.contains("..")
        && !b.contains("//")
        && b.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.' | '/'))
}

fn remote_ok(r: &str) -> bool {
    !r.is_empty()
        && r.len() <= 64
        && !r.starts_with('-')
        && r.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_' | '.'))
}

fn sha_ok(s: &str) -> bool {
    (s.len() == 40 || s.len() == 64) && s.bytes().all(|b| b.is_ascii_hexdigit())
}

/// The test guard's flag, as the Hub reads it: on only for `1`.
fn forbid_real_hosts() -> bool {
    static FLAG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FLAG.get_or_init(|| {
        std::env::var("STEVE_FORBID_REAL_HOSTS")
            .ok()
            .is_some_and(|v| v.trim() == "1")
    })
}

/// Whether a push may go to `url`: a local path or `file://` always; any other URL only on a
/// loopback host while `forbid` (the test guard) is on.
pub(crate) fn target_allowed(forbid: bool, url: &str) -> bool {
    if !forbid || url.starts_with('/') || url.starts_with("file://") {
        return true;
    }
    let Some((_, rest)) = url.split_once("://") else {
        return false;
    };
    let authority = rest.split(['/', '?', '#']).next().unwrap_or("");
    let host = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = if let Some(v6) = host.strip_prefix('[') {
        v6.split(']').next().unwrap_or("")
    } else {
        host.rsplit_once(':').map_or(host, |(h, _)| h)
    };
    let host = host.trim_end_matches('.');
    host.eq_ignore_ascii_case("localhost")
        || host
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.to_canonical().is_loopback())
}

/// A push, checked: where it goes, what it pushes, and from which objects.
pub(crate) struct PushPlan {
    pub url: String,
    pub branch: String,
    /// The branch at `url` it goes to.
    pub to: String,
    pub sha: String,
    pub lease: Option<String>,
    pub objects: PathBuf,
    /// The header's name, when it carries a credential.
    pub header: Option<String>,
}

/// What a push's fields must be, before anything is read.
pub(crate) fn check_push_shape(a: &GitAction) -> ApiResult<()> {
    let GitAction::Push {
        remote,
        branch,
        expected_remote,
        url,
        origin_must_be,
        expected_local,
        auth,
        remote_branch,
    } = a
    else {
        return Err(ApiError::new(
            ErrorCode::Unsupported,
            "this node applies no merge yet",
        ));
    };
    if !branch_ok(branch) || remote_branch.as_deref().is_some_and(|b| !branch_ok(b)) {
        return Err(invalid("not a branch Steve pushes"));
    }
    if !remote_ok(remote) {
        return Err(invalid("not a remote name"));
    }
    if !sha_ok(expected_local) || expected_remote.as_deref().is_some_and(|s| !sha_ok(s)) {
        return Err(invalid("not a commit id"));
    }
    if url.is_empty()
        || url.len() > 2048
        || url.starts_with('-')
        || url.chars().any(|c| c.is_whitespace() || c.is_control())
        || !(url.starts_with('/') || url.contains("://"))
    {
        return Err(invalid("not a push URL"));
    }
    if origin_must_be
        .as_deref()
        .is_some_and(|o| github_repo(&format!("https://github.com/{o}")).is_none())
    {
        return Err(invalid("not owner/name"));
    }
    if let Some(a) = auth {
        let SecretDelivery::Header { name } = &a.how else {
            return Err(invalid("a push credential goes as a header"));
        };
        if a.id.is_empty()
            || a.id.len() > 256
            || name.is_empty()
            || name.len() > 64
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return Err(invalid("a bad push header"));
        }
    }
    if !target_allowed(forbid_real_hosts(), url) {
        return Err(ApiError::new(ErrorCode::Denied, HOST_FORBIDDEN));
    }
    Ok(())
}

/// The push's preconditions in `dir`, now: the remote, the branch, HEAD and the revision, read
/// through the workspace's pipe as [`facts`] reads them. The revision is compared the way the Hub
/// read it: heads only (`h…`, [`GitQuery::heads_only`]) or with the working tree.
pub(crate) fn check_push(c: &Commands, dir: &Path, apply: &GitApply) -> ApiResult<PushPlan> {
    check_push_shape(&apply.action)?;
    let GitAction::Push {
        remote,
        branch,
        expected_remote,
        url,
        origin_must_be,
        expected_local,
        auth,
        remote_branch,
    } = &apply.action
    else {
        unreachable!("checked above");
    };
    let g = Git { c, dir };
    if let Some(want) = origin_must_be {
        let at = g.may(&["config", "--get", &format!("remote.{remote}.url")])?;
        if at.as_deref().and_then(github_repo) != Some(want.to_lowercase()) {
            return Err(ApiError::new(ErrorCode::Denied, WRONG_REMOTE));
        }
    }
    match g.commit(&format!("refs/heads/{branch}"))? {
        None => return Err(ApiError::new(ErrorCode::Stale, NO_BRANCH)),
        Some(sha) if sha != *expected_local => {
            return Err(ApiError::new(ErrorCode::Stale, BRANCH_MOVED));
        }
        Some(_) => {}
    }
    let heads_only = apply.expected.as_str().starts_with('h');
    let (head, _, _, revision) = observe(&g, heads_only)?;
    if revision != apply.expected || head != apply.expected_head {
        return Err(ApiError::new(ErrorCode::Stale, "changed since"));
    }
    let common = PathBuf::from(g.line(&["rev-parse", "--git-common-dir"])?);
    let common = if common.is_absolute() {
        common
    } else {
        dir.join(common)
    };
    let to = remote_branch.clone().unwrap_or_else(|| branch.clone());
    Ok(PushPlan {
        url: url.clone(),
        branch: branch.clone(),
        to: to.clone(),
        sha: expected_local.clone(),
        lease: expected_remote
            .as_ref()
            .map(|s| format!("refs/heads/{to}:{s}")),
        objects: common.join("objects"),
        header: auth.as_ref().map(|a| match &a.how {
            SecretDelivery::Header { name } => name.clone(),
            _ => unreachable!("checked above"),
        }),
    })
}

/// `<home>/push`.
fn root(home: &Path) -> PathBuf {
    home.join("push")
}

/// Removes the bare repositories a crash left (at open: nothing runs yet).
pub(crate) fn cleanup(home: &Path) {
    let r = root(home);
    if r.exists()
        && let Err(e) = std::fs::remove_dir_all(&r)
    {
        tracing::warn!("node: could not remove leftover push repositories: {e}");
    }
}

/// The throwaway repository of one push; removed on drop.
pub(crate) struct Bare(PathBuf);

impl Drop for Bare {
    fn drop(&mut self) {
        if let Err(e) = std::fs::remove_dir_all(&self.0)
            && e.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!("node: could not remove a push repository: {e}");
        }
    }
}

/// A bare repository for `op` that has the plan's branch at its commit, from the workspace's
/// objects.
pub(crate) fn bare(home: &Path, op: &OperationId, plan: &PushPlan) -> Result<Bare, String> {
    let name = hex::encode(sha2::Sha256::digest(op.as_str().as_bytes()));
    let dir = root(home).join(&name[..24]);
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).map_err(|e| e.to_string())?;
    let b = Bare(dir);
    let run = |args: &[&str]| -> Result<(), String> {
        let o = read(&b.0, args).map_err(|e| e.safe_detail)?;
        if o.code != Some(0) {
            return Err(format!("git {}: {}", args[0], o.complaint()));
        }
        Ok(())
    };
    run(&["init", "--bare", "--quiet"])?;
    std::fs::write(
        b.0.join("objects/info/alternates"),
        format!("{}\n", plan.objects.display()),
    )
    .map_err(|e| e.to_string())?;
    run(&[
        "update-ref",
        &format!("refs/heads/{}", plan.branch),
        &plan.sha,
    ])?;
    Ok(b)
}

/// The command that pushes `plan` from `bare`: environment only, the header's value a secret.
pub(crate) fn push_run(
    bare: &Bare,
    plan: &PushPlan,
    header: Option<Zeroizing<Vec<u8>>>,
    deadline: i64,
) -> crate::exec::Run {
    let mut env: Vec<(String, String)> = vec![
        ("GIT_CONFIG_NOSYSTEM".into(), "1".into()),
        ("GIT_CONFIG_GLOBAL".into(), "/dev/null".into()),
        ("GIT_TERMINAL_PROMPT".into(), "0".into()),
        ("STEVE_PUSH_URL".into(), plan.url.clone()),
        (
            "STEVE_PUSH_REFSPEC".into(),
            format!("refs/heads/{}:refs/heads/{}", plan.branch, plan.to),
        ),
    ];
    if let Some(l) = &plan.lease {
        env.push(("STEVE_PUSH_LEASE".into(), l.clone()));
    }
    // a transfer slower than 1 byte/s for 60 s is given up by git itself
    let mut config: Vec<(String, String)> = vec![
        ("http.lowSpeedLimit".into(), "1".into()),
        ("http.lowSpeedTime".into(), "60".into()),
    ];
    let mut secret_env = vec![];
    if let Some(h) = header {
        let at = config.len();
        config.push((format!("http.{}.extraheader", plan.url), String::new()));
        secret_env.push((format!("GIT_CONFIG_VALUE_{at}"), h));
    }
    env.push(("GIT_CONFIG_COUNT".into(), config.len().to_string()));
    for (i, (k, v)) in config.into_iter().enumerate() {
        env.push((format!("GIT_CONFIG_KEY_{i}"), k));
        if !secret_env
            .iter()
            .any(|(n, _)| *n == format!("GIT_CONFIG_VALUE_{i}"))
        {
            env.push((format!("GIT_CONFIG_VALUE_{i}"), v));
        }
    }
    let unset = UNSET
        .iter()
        .filter(|k| **k != "GIT_CONFIG_COUNT")
        .copied()
        .collect::<Vec<_>>()
        .join(" ");
    crate::exec::Run {
        dir: bare.0.clone(),
        command: format!("unset {unset}; {PUSH_SH}"),
        env,
        deadline,
        cap: PUSH_OUTPUT,
        secret_env,
        stdin: None,
        confined: false,
        readable: vec![],
    }
}

/// git's complaint about a push, cut, as an error's detail.
pub(crate) fn complaint(stderr: &str) -> String {
    stderr.trim().chars().take(ERROR_CHARS).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_revision_is_never_an_option() {
        assert!(rev("main").is_ok());
        assert!(rev("steve/st-1").is_ok());
        for bad in ["", "-n", "--output=/tmp/x", "a b", "a\0b"] {
            assert_eq!(rev(bad).unwrap_err().code, ErrorCode::Invalid, "{bad:?}");
        }
    }

    #[test]
    fn a_remote_url_never_carries_a_credential() {
        assert_eq!(
            no_credentials("https://x:tok@github.com/o/r.git"),
            "https://github.com/o/r.git"
        );
        assert_eq!(no_credentials("ssh://git@host:22/o/r"), "ssh://host:22/o/r");
        assert_eq!(
            no_credentials("git@github.com:o/r.git"),
            "git@github.com:o/r.git"
        );
        assert_eq!(no_credentials("/tmp/origin.git"), "/tmp/origin.git");
    }

    #[test]
    fn porcelain_entries_skip_a_rename_source_and_a_trailing_slash() {
        let e = porcelain(b" M a.rs\0R  new.rs\0old.rs\0?? dir/\0");
        let got: Vec<(&str, &str)> = e
            .iter()
            .map(|e| (e.code.as_str(), e.path.as_str()))
            .collect();
        assert_eq!(got, [(" M", "a.rs"), ("R ", "new.rs"), ("??", "dir")]);
    }

    #[test]
    fn remote_tags_take_the_peeled_commit() {
        let t = remote_tags("a1\trefs/tags/v1\nb2\trefs/tags/v2\nc3\trefs/tags/v2^{}\n");
        assert_eq!(t, [("v1".into(), "a1".into()), ("v2".into(), "c3".into())]);
    }

    #[test]
    fn a_patch_is_cut_at_a_character() {
        let (s, more) = cut("ab中".into(), 3, false);
        assert_eq!((s.as_str(), more), ("ab", true));
        assert_eq!(cut("abc".into(), 3, false), ("abc".into(), false));
        assert_eq!(cut("abc".into(), 3, true), ("abc".into(), true));
    }

    #[test]
    fn the_guard_allows_local_and_loopback_targets_only() {
        for u in [
            "/tmp/remotes/o/r.git",
            "file:///tmp/r.git",
            "http://127.0.0.1:9/o/r.git",
            "http://localhost/o/r.git",
            "https://x@[::1]:8/o/r.git",
        ] {
            assert!(target_allowed(true, u), "{u}");
        }
        for u in [
            "https://github.com/o/r.git",
            "https://127.0.0.1.evil.com/o/r.git",
            "https://localhost@github.com/o/r.git",
            "github.com:o/r",
        ] {
            assert!(!target_allowed(true, u), "{u}");
            assert!(target_allowed(false, u), "{u}");
        }
    }

    #[test]
    fn names_a_push_takes() {
        for b in ["steve/st-12", "main", "fix_1.2"] {
            assert!(branch_ok(b), "{b}");
        }
        for b in [
            "", "-x", "a..b", "a b", "a:b", "+main", "a/", "/a", "x.lock", "a~1",
        ] {
            assert!(!branch_ok(b), "{b}");
        }
    }
}
