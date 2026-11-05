//! [`steve_node_api::WorkspaceExecutor::git_facts`] on this machine: what git says in a workspace,
//! read through the workspace's [`Commands`] pipe (registered, bounded by its deadline, never a
//! lazy fetch). Only fixed read commands; a ref or revision never becomes an option.

use std::path::Path;

use sha2::Digest as _;
use steve_node_api::*;

use crate::workspace::Commands;

/// What one fixed command may print before it is cut (a patch or a file is cut at the read's own
/// `max`, well below this).
const OUT_CAP: usize = 8 << 20;
/// Bytes of a patch read past `max`, so the cut is made after git's leading blank line is trimmed.
const SLACK: usize = 4096;

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
    fn out(&self, args: &[&str], cap: usize) -> ApiResult<(Vec<u8>, bool)> {
        self.c.read(self.dir, args, cap).map_err(invalid)
    }
    fn line(&self, args: &[&str]) -> ApiResult<String> {
        Ok(text(&self.out(args, 64 * 1024)?.0))
    }
    /// The commit `r` names, or `None` when it names none.
    fn commit(&self, r: &str) -> ApiResult<Option<String>> {
        let spec = format!("{}^{{commit}}", rev(r)?);
        Ok(self
            .line(&["rev-parse", "--verify", "--quiet", &spec])
            .ok()
            .filter(|s| !s.is_empty()))
    }
    fn must(&self, r: &str) -> ApiResult<String> {
        self.commit(r)?
            .ok_or_else(|| invalid(format!("no such revision: {r}")))
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
                    Err(_) => (sha, not_found()),
                }
            }
            GitRead::Show {
                rev: at,
                path: None,
                max,
            } => {
                let sha = self.must(at)?;
                let meta =
                    self.line(&["show", "-s", "--format=%H%x1f%s%x1f%an%x1f%ct%x1f%B", &sha])?;
                let f: Vec<&str> = meta.splitn(5, '\u{1f}').collect();
                let (stat, patch, truncated) =
                    self.stat_patch(&["show", "--format="], &sha, *max)?;
                let commit = CommitInfo {
                    sha: f.first().copied().unwrap_or(&sha).to_string(),
                    subject: f.get(1).copied().unwrap_or_default().to_string(),
                    author: f.get(2).copied().unwrap_or_default().to_string(),
                    time: Timestamp(
                        f.get(3).and_then(|t| t.parse::<i64>().ok()).unwrap_or(0) * 1000,
                    ),
                    body: Some(f.get(4).copied().unwrap_or_default().to_string()),
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
                    "--format=%H%x1f%s%x1f%an%x1f%ct",
                    &span,
                ])?;
                let mut commits: Vec<CommitInfo> = log
                    .lines()
                    .filter(|l| !l.is_empty())
                    .map(|l| {
                        let f: Vec<&str> = l.splitn(4, '\u{1f}').collect();
                        CommitInfo {
                            sha: f.first().copied().unwrap_or_default().to_string(),
                            subject: f.get(1).copied().unwrap_or_default().to_string(),
                            author: f.get(2).copied().unwrap_or_default().to_string(),
                            time: Timestamp(
                                f.get(3).and_then(|t| t.parse::<i64>().ok()).unwrap_or(0) * 1000,
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
            _ => {
                return Err(ApiError::new(
                    ErrorCode::Unsupported,
                    "this git read is not served here yet",
                ));
            }
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

/// The facts `q` asks for in the checkout at `dir`. A fetch and ranges are not served yet
/// (`unsupported`), nor are the working-tree reads.
pub(crate) fn facts(c: &Commands, dir: &Path, q: &GitQuery) -> ApiResult<GitFacts> {
    if q.fetch || !q.ranges.is_empty() {
        return Err(ApiError::new(
            ErrorCode::Unsupported,
            "fetch and ranges are not served here yet",
        ));
    }
    let g = Git { c, dir };
    let head = g
        .commit("HEAD")?
        .ok_or_else(|| invalid("not a git checkout with a commit"))?;
    let branch = g
        .line(&["symbolic-ref", "--short", "-q", "HEAD"])
        .ok()
        .filter(|b| !b.is_empty());
    let status = g.out(&["status", "--porcelain", "-z"], OUT_CAP)?.0;
    let mut refs = vec![];
    for r in &q.refs {
        refs.push((r.clone(), g.commit(r)?));
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
    // what a decision made on these facts checks again: HEAD, the branch and the working tree
    let mut h = sha2::Sha256::new();
    h.update(head.as_bytes());
    h.update([0]);
    h.update(branch.as_deref().unwrap_or_default().as_bytes());
    h.update([0]);
    h.update(&status);
    let revision = Revision::new(format!("g{}", &hex::encode(h.finalize())[..32]))
        .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?;
    Ok(GitFacts {
        revision,
        head,
        branch,
        dirty: !status.is_empty(),
        refs,
        ranges: vec![],
        reads,
    })
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
    fn a_patch_is_cut_at_a_character() {
        let (s, more) = cut("ab中".into(), 3, false);
        assert_eq!((s.as_str(), more), ("ab", true));
        assert_eq!(cut("abc".into(), 3, false), ("abc".into(), false));
        assert_eq!(cut("abc".into(), 3, true), ("abc".into(), true));
    }
}
