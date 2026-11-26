#![allow(dead_code)]

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;

use steve_github::push::{GitNodes, NodePush, PushError, branch_ok};
use steve_store::Store;

/// Test [`GitNodes`] adapter that fulfills Node git operations locally against the test's
/// temporary git repositories using the paths stored in SQLite.
pub struct TestGitNodes {
    store: Store,
}

impl TestGitNodes {
    pub fn new(store: Store) -> Arc<Self> {
        Arc::new(Self { store })
    }

    fn item_dir(&self, item: &str) -> Result<PathBuf, PushError> {
        let row = self
            .store
            .items()
            .get(item)
            .map_err(|e| PushError::Git(e.to_string()))?
            .ok_or_else(|| PushError::Node(format!("item {item} not found")))?;
        if let Some(cwd) = row.cwd.filter(|s| !s.is_empty()) {
            return Ok(PathBuf::from(cwd));
        }
        if let Some(pid) = row.project_id
            && let Some(p) = self
                .store
                .projects()
                .get(&pid)
                .map_err(|e| PushError::Git(e.to_string()))?
            && !p.path.is_empty()
        {
            return Ok(PathBuf::from(p.path));
        }
        Err(PushError::Node(format!("item {item} has no directory")))
    }

    fn git_out(dir: &Path, args: &[&str]) -> Result<String, String> {
        let out = Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .env("GIT_CONFIG_GLOBAL", "/dev/null")
            .env("GIT_CONFIG_NOSYSTEM", "1")
            .output()
            .map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
    }
}

fn github_repo(url: &str) -> Option<String> {
    let u = url.trim();
    let rest = u
        .strip_prefix("https://github.com/")
        .or_else(|| u.strip_prefix("http://github.com/"))
        .or_else(|| u.strip_prefix("git@github.com:"))
        .or_else(|| u.strip_prefix("ssh://git@github.com/"))
        .or_else(|| u.strip_prefix("git://github.com/"))
        .or_else(|| {
            u.strip_prefix("https://")
                .and_then(|r| r.split_once('@'))
                .and_then(|(_, r)| r.strip_prefix("github.com/"))
        })?;
    let rest = rest.trim_end_matches('/');
    let rest = rest.strip_suffix(".git").unwrap_or(rest);
    let (o, n) = rest.split_once('/')?;
    (!o.is_empty() && !n.is_empty() && !n.contains('/')).then(|| format!("{o}/{n}").to_lowercase())
}

#[async_trait::async_trait]
impl GitNodes for TestGitNodes {
    async fn ahead(&self, item: &str, base: &str, branch: &str) -> Option<Result<bool, PushError>> {
        if !branch_ok(branch) {
            return Some(Err(PushError::BadBranch(branch.into())));
        }
        let dir = match self.item_dir(item) {
            Ok(d) => d,
            Err(e) => return Some(Err(e)),
        };
        let remote_base = format!("origin/{base}");
        let cmp = if Self::git_out(&dir, &["rev-parse", "--verify", &remote_base]).is_ok() {
            remote_base
        } else {
            base.to_string()
        };
        let range = format!("{cmp}..{branch}");
        let res = Self::git_out(&dir, &["rev-list", "--count", &range])
            .map(|s| s.parse::<u64>().unwrap_or(0) > 0)
            .map_err(PushError::Git);
        Some(res)
    }

    async fn head_of(&self, item: &str, branch: &str) -> Option<Result<String, PushError>> {
        if !branch_ok(branch) {
            return Some(Err(PushError::BadBranch(branch.into())));
        }
        let dir = match self.item_dir(item) {
            Ok(d) => d,
            Err(e) => return Some(Err(e)),
        };
        let full = format!("refs/heads/{branch}");
        Some(
            Self::git_out(&dir, &["rev-parse", "--verify", &full])
                .map_err(|_| PushError::NoBranch(branch.into())),
        )
    }

    async fn push(&self, p: &NodePush<'_>) -> Option<Result<String, PushError>> {
        if !branch_ok(p.branch) {
            return Some(Err(PushError::BadBranch(p.branch.into())));
        }
        let dir = match self.item_dir(p.item) {
            Ok(d) => d,
            Err(e) => return Some(Err(e)),
        };
        let origin = match Self::git_out(&dir, &["remote", "get-url", "origin"]) {
            Ok(u) => u,
            Err(_) => {
                return Some(Err(PushError::WrongRemote {
                    expected: p.full_name.into(),
                }));
            }
        };
        if github_repo(&origin).as_deref() != Some(&p.full_name.to_lowercase()) {
            return Some(Err(PushError::WrongRemote {
                expected: p.full_name.into(),
            }));
        }
        let full = format!("refs/heads/{}", p.branch);
        let now = match Self::git_out(&dir, &["rev-parse", "--verify", &full]) {
            Ok(s) => s,
            Err(_) => return Some(Err(PushError::NoBranch(p.branch.into()))),
        };
        if now != p.expect {
            return Some(Err(PushError::Moved {
                branch: p.branch.into(),
                now,
            }));
        }
        let refspec = format!("{full}:{full}");
        Some(
            Self::git_out(&dir, &["push", "-q", p.url, &refspec])
                .map(|_| now)
                .map_err(PushError::Git),
        )
    }

    async fn origins(&self, projects: &[String]) -> Option<Vec<Option<String>>> {
        let out = projects
            .iter()
            .map(|pid| {
                let p = self.store.projects().get(pid).ok()??;
                if p.path.is_empty() {
                    return None;
                }
                let url =
                    Self::git_out(Path::new(&p.path), &["remote", "get-url", "origin"]).ok()?;
                github_repo(&url)
            })
            .collect();
        Some(out)
    }
}
