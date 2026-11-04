//! The target a working copy merges into: the upstream repository, named the same however its remote is
//! spelled, and a branch of it. Two projects, clones or worktrees of one repository share one target.

use std::path::Path;

use crate::git::git;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Target {
    /// `host/path` of the remote (`github.com/infranite/steve`), or `file:` and the real path of the
    /// repository when it is only on this machine.
    pub upstream: String,
    pub branch: String,
}

impl Target {
    /// One string for keys and logs: `github.com/infranite/steve#main`.
    pub fn key(&self) -> String {
        format!("{}#{}", self.upstream, self.branch)
    }
}

/// `host/path` of a remote URL, the same for scp-like (`git@host:o/r.git`), `ssh://`, `https://` and
/// `git://` spellings, with or without user, port, `.git` and a trailing `/`, in any case. A local
/// path (or `file://`) is `file:` and the path as written; [`target_of`] resolves it. `None` when there
/// is nothing to name.
pub fn normalize_url(url: &str) -> Option<String> {
    let url = url.trim();
    if url.is_empty() {
        return None;
    }
    let trim = |p: &str| {
        p.trim_end_matches('/')
            .trim_end_matches(".git")
            .trim_matches('/')
            .to_lowercase()
    };
    if let Some(p) = url.strip_prefix("file://") {
        return Some(format!("file:{}", p.trim_end_matches('/')));
    }
    if url.starts_with('/') || url.starts_with('.') || url.starts_with('~') {
        return Some(format!("file:{}", url.trim_end_matches('/')));
    }
    let (host, path) = if let Some((_, rest)) = url.split_once("://") {
        let (auth, path) = rest.split_once('/').unwrap_or((rest, ""));
        let host = auth.rsplit_once('@').map_or(auth, |(_, h)| h);
        (host.split(':').next().unwrap_or(host), path)
    } else {
        /* scp-like: [user@]host:path */
        let (auth, path) = url.split_once(':')?;
        (auth.rsplit_once('@').map_or(auth, |(_, h)| h), path)
    };
    let (host, path) = (host.to_lowercase(), trim(path));
    (!host.is_empty() && !path.is_empty()).then(|| format!("{host}/{path}"))
}

/// The target of the working copy `dir`: its `origin` (or the repository itself when there is no
/// remote) and `branch`, or the remote's default branch (`origin/HEAD`, else `main`, else `master`).
pub fn target_of(dir: &Path, branch: Option<&str>) -> Result<Target, String> {
    let remotes = git(dir, &["remote"])?;
    let has_origin = remotes.lines().any(|r| r == "origin");
    let upstream = if has_origin {
        /* as configured (`get-url` would apply `insteadOf`): it names the repository */
        let url = git(dir, &["config", "--get", "remote.origin.url"])?;
        match normalize_url(&url) {
            Some(u) if u.starts_with("file:") => local(dir, &u["file:".len()..])?,
            Some(u) => u,
            None => return Err(format!("origin 的地址认不出来：{url}")),
        }
    } else {
        let common = git(dir, &["rev-parse", "--git-common-dir"])?;
        local(dir, &common)?
    };
    let branch = match branch {
        Some(b) => b.to_string(),
        None => default_branch(dir, has_origin)?,
    };
    Ok(Target { upstream, branch })
}

/// `file:` and the real path of a repository on this machine (relative paths are from `dir`).
fn local(dir: &Path, path: &str) -> Result<String, String> {
    let p = Path::new(path);
    let p = if p.is_absolute() {
        p.to_path_buf()
    } else {
        dir.join(p)
    };
    let real = p
        .canonicalize()
        .map_err(|e| format!("{}：{e}", p.display()))?;
    Ok(format!(
        "file:{}",
        real.to_string_lossy().trim_end_matches('/')
    ))
}

fn default_branch(dir: &Path, has_origin: bool) -> Result<String, String> {
    let exists = |r: &str| crate::git::git_test(dir, &["rev-parse", "--verify", "--quiet", r]);
    if has_origin {
        if let Ok(h) = git(
            dir,
            &[
                "symbolic-ref",
                "--quiet",
                "--short",
                "refs/remotes/origin/HEAD",
            ],
        ) && let Some(b) = h.strip_prefix("origin/")
        {
            return Ok(b.to_string());
        }
        for b in ["main", "master"] {
            if exists(&format!("refs/remotes/origin/{b}"))? {
                return Ok(b.into());
            }
        }
        return Err("远端没有默认分支（origin/HEAD、main、master 都没有）".into());
    }
    for b in ["main", "master"] {
        if exists(&format!("refs/heads/{b}"))? {
            return Ok(b.into());
        }
    }
    Err("仓库没有 main 或 master 分支".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_repository_has_one_name_however_its_remote_is_spelled() {
        let want = Some("github.com/infranite/steve".to_string());
        for u in [
            "git@github.com:Infranite/steve.git",
            "git@github.com:Infranite/steve",
            "ssh://git@github.com/Infranite/steve.git",
            "ssh://git@github.com:22/Infranite/steve.git",
            "https://github.com/Infranite/steve",
            "https://github.com/Infranite/steve.git/",
            "https://user:token@GitHub.com/infranite/Steve.git",
            "git://github.com/Infranite/steve.git",
        ] {
            assert_eq!(normalize_url(u), want, "{u}");
        }
        assert_eq!(
            normalize_url("git@gitlab.example.com:group/sub/proj.git").as_deref(),
            Some("gitlab.example.com/group/sub/proj")
        );
        assert_eq!(
            normalize_url("/srv/git/steve.git/").as_deref(),
            Some("file:/srv/git/steve.git")
        );
        assert_eq!(
            normalize_url("file:///srv/git/steve.git").as_deref(),
            Some("file:/srv/git/steve.git")
        );
        assert_eq!(normalize_url(""), None);
        assert_eq!(normalize_url("github.com"), None);
    }
}
