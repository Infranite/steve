//! The target a working copy merges into: the upstream repository, named the same however its remote is
//! spelled, and a branch of it. Two projects, clones or worktrees of one repository share one target.

use std::path::Path;

use crate::git::git;

#[derive(Debug, Clone, PartialEq, Eq, Hash, serde::Serialize)]
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

/// Hosts whose repository paths are the same in any case.
const CASELESS_HOSTS: &[&str] = &["github.com", "gitlab.com", "bitbucket.org"];

/// `host[:port]/path` of a remote URL, the same for scp-like (`git@host:o/r.git`), `ssh://`,
/// `https://` and `git://` spellings, with or without user, the protocol's default port, `.git` and a
/// trailing `/`. The host is in lower case; the path too only on [`CASELESS_HOSTS`] (elsewhere `App`
/// and `app` may be two repositories, as may two ports). A local path (or `file://`) is `file:` and
/// the path as written; [`target_of`] resolves it. `None` when there is nothing to name.
pub fn normalize_url(url: &str) -> Option<String> {
    let url = url.trim();
    if url.is_empty() {
        return None;
    }
    if let Some(p) = url.strip_prefix("file://") {
        return Some(format!("file:{}", p.trim_end_matches('/')));
    }
    if url.starts_with('/') || url.starts_with('.') || url.starts_with('~') {
        return Some(format!("file:{}", url.trim_end_matches('/')));
    }
    let (host, port, path) = if let Some((scheme, rest)) = url.split_once("://") {
        let (auth, path) = rest.split_once('/').unwrap_or((rest, ""));
        let host = auth.rsplit_once('@').map_or(auth, |(_, h)| h);
        let (host, port) = match host.rsplit_once(':') {
            Some((h, p)) if !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()) => (h, Some(p)),
            _ => (host, None),
        };
        let default = match scheme.to_lowercase().as_str() {
            "ssh" | "git+ssh" | "ssh+git" => "22",
            "https" => "443",
            "http" => "80",
            "git" => "9418",
            _ => "",
        };
        (host, port.filter(|p| *p != default), path)
    } else {
        /* scp-like: [user@]host:path (no port) */
        let (auth, path) = url.split_once(':')?;
        (auth.rsplit_once('@').map_or(auth, |(_, h)| h), None, path)
    };
    let host = host.to_lowercase();
    let path = path
        .trim_end_matches('/')
        .trim_end_matches(".git")
        .trim_matches('/');
    let path = if CASELESS_HOSTS.contains(&host.as_str()) {
        path.to_lowercase()
    } else {
        path.to_string()
    };
    if host.is_empty() || path.is_empty() {
        return None;
    }
    Some(match port {
        Some(p) => format!("{host}:{p}/{path}"),
        None => format!("{host}/{path}"),
    })
}

/// The target of the working copy `dir`: its `origin` (or the repository itself when there is no
/// remote) and `branch`, or the remote's default branch (`origin/HEAD`, else `main`, else `master`).
pub fn target_of(dir: &Path, branch: Option<&str>) -> Result<Target, String> {
    let remotes = git(dir, &["remote"])?;
    let has_origin = remotes.lines().any(|r| r == "origin");
    let upstream = if has_origin {
        /* the URL git really uses (`insteadOf` shorthands such as `gh:` expanded), unless that only
        points a network remote at a copy on this machine (a mirror): then the remote it stands for */
        let configured = git(dir, &["config", "--get", "remote.origin.url"])?;
        let used = git(dir, &["remote", "get-url", "origin"])?;
        let (c, u) = (normalize_url(&configured), normalize_url(&used));
        let pick = match (&c, &u) {
            (Some(c), Some(u)) if u.starts_with("file:") && !c.starts_with("file:") => Some(c),
            (_, Some(u)) => Some(u),
            (Some(c), None) => Some(c),
            (None, None) => None,
        };
        match pick {
            Some(x) if x.starts_with("file:") => local(dir, &x["file:".len()..])?,
            Some(x) => x.clone(),
            None => return Err(steve_i18n::t!("core.lane.origin_unread", url = used)),
        }
    } else {
        local(dir, ".")?
    };
    let branch = match branch {
        Some(b) => b.to_string(),
        None => default_branch(dir, has_origin)?,
    };
    Ok(Target { upstream, branch })
}

/// `file:` and the real path of the git directory of the repository at `path` (relative to `dir`):
/// one name for a bare repository, a checkout, its `.git` and its worktrees.
fn local(dir: &Path, path: &str) -> Result<String, String> {
    let p = Path::new(path);
    let p = if p.is_absolute() {
        p.to_path_buf()
    } else {
        dir.join(p)
    };
    let common = git(&p, &["rev-parse", "--git-common-dir"])?;
    let common = Path::new(&common);
    let common = if common.is_absolute() {
        common.to_path_buf()
    } else {
        p.join(common)
    };
    let real = common
        .canonicalize()
        .map_err(|e| format!("{}：{e}", common.display()))?;
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
        return Err(steve_i18n::t!("core.lane.no_default_branch"));
    }
    for b in ["main", "master"] {
        if exists(&format!("refs/heads/{b}"))? {
            return Ok(b.into());
        }
    }
    Err(steve_i18n::t!("core.lane.no_main"))
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
            "https://github.com:443/Infranite/steve",
            "git://github.com/Infranite/steve.git",
        ] {
            assert_eq!(normalize_url(u), want, "{u}");
        }
        assert_eq!(
            normalize_url("git@gitlab.example.com:group/sub/proj.git").as_deref(),
            Some("gitlab.example.com/group/sub/proj")
        );
        /* elsewhere case and a port can tell two repositories apart */
        assert_ne!(
            normalize_url("ssh://git@git.example.com/srv/App.git"),
            normalize_url("ssh://git@git.example.com/srv/app.git")
        );
        assert_eq!(
            normalize_url("ssh://git@git.example.com:2222/team/app.git").as_deref(),
            Some("git.example.com:2222/team/app")
        );
        assert_ne!(
            normalize_url("ssh://git@git.example.com:2222/team/app.git"),
            normalize_url("ssh://git@git.example.com:3333/team/app.git")
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
