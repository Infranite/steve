//! Pushing an item's branch (github.md §4.1, §12). The Hub pushes, never the agent, and the token
//! never meets anything the agent controls:
//!
//! - the worktree's `origin` must be `github.com/<owner>/<name>` of the linked repo, else refused;
//! - the push runs from a throwaway bare repo that borrows the worktree's objects
//!   (`objects/info/alternates`), with system and global config off, so the worktree's own config
//!   (hooks, `http.proxy`, `insteadOf`, credential helpers) plays no part;
//! - the token goes in as `http.<git_base>/.extraheader` through `GIT_CONFIG_COUNT` environment
//!   variables: not on the command line, not in a file, not in logs;
//! - the target is the canonical `<git_base>/<owner>/<name>.git`, not whatever `origin` says.
//!
//! Synchronous (`std::process`); call it from `spawn_blocking`.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

use base64::Engine;
use zeroize::Zeroizing;

use crate::repos::parse_remote;

/// Why a push did not happen. Chinese, never containing the token.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PushError {
    #[error("分支名不对：{0}")]
    BadBranch(String),
    #[error("工作目录的 origin 不是 {expected}")]
    WrongRemote { expected: String },
    #[error("分支 {0} 不存在")]
    NoBranch(String),
    #[error("git 失败：{0}")]
    Git(String),
    /// Refused by the test guard (`STEVE_FORBID_REAL_HOSTS=1`), nothing pushed.
    #[error("{0}")]
    HostForbidden(String),
}

/// One push.
pub struct PushSpec<'a> {
    /// The item's worktree (`items.cwd`).
    pub cwd: &'a Path,
    pub branch: &'a str,
    /// `owner/name` of the linked repo.
    pub full_name: &'a str,
    /// `https://github.com` (a test's bare remotes otherwise).
    pub git_base: &'a str,
    /// Installation token or PAT.
    pub token: &'a str,
}

/// Branch names Steve pushes: `steve/st-12` and the like. No `..`, no leading `-`, no spaces or
/// refspec characters.
pub fn branch_ok(b: &str) -> bool {
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

/// `git` with the user's system and global config switched off and no prompts.
fn git(dir: &Path) -> Command {
    let mut c = Command::new("git");
    for key in [
        "STEVE_UPGRADE_CONTROL_TOKEN",
        "STEVE_SUPERVISOR_PID",
        "STEVE_UPGRADE_ID",
    ] {
        c.env_remove(key);
    }
    c.arg("-C")
        .arg(dir)
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_TERMINAL_PROMPT", "0")
        .env_remove("GIT_CONFIG_COUNT")
        .env_remove("GIT_CONFIG_PARAMETERS")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .stdin(Stdio::null());
    c
}

fn run(mut c: Command) -> Result<String, PushError> {
    let out = c.output().map_err(|e| PushError::Git(e.to_string()))?;
    if !out.status.success() {
        let err = String::from_utf8_lossy(&out.stderr);
        return Err(PushError::Git(err.trim().chars().take(300).collect()));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// The configured `origin` URL (not rewritten by `insteadOf`) must name `full_name`.
pub fn check_remote(cwd: &Path, full_name: &str) -> Result<(), PushError> {
    let mut c = git(cwd);
    c.args(["config", "--get", "remote.origin.url"]);
    let url = run(c).unwrap_or_default();
    let wrong = || PushError::WrongRemote {
        expected: format!("github.com/{full_name}"),
    };
    match parse_remote(&url) {
        Some(r) if r == full_name.to_lowercase() => Ok(()),
        _ => Err(wrong()),
    }
}

/// Commits on `branch` that `base` does not have: `origin/<base>..<branch>`, or `<base>..<branch>`
/// when the worktree has no remote-tracking copy of the base.
pub fn ahead(cwd: &Path, base: &str, branch: &str) -> Result<u64, PushError> {
    if !branch_ok(branch) {
        return Err(PushError::BadBranch(branch.into()));
    }
    if !branch_ok(base) {
        return Err(PushError::BadBranch(base.into()));
    }
    let has = |r: &str| {
        let mut c = git(cwd);
        c.args(["rev-parse", "--verify", "--quiet", r]);
        run(c).is_ok()
    };
    let from = if has(&format!("refs/remotes/origin/{base}")) {
        format!("refs/remotes/origin/{base}")
    } else {
        format!("refs/heads/{base}")
    };
    let mut c = git(cwd);
    c.args([
        "rev-list",
        "--count",
        &format!("{from}..refs/heads/{branch}"),
    ]);
    run(c)?
        .parse()
        .map_err(|_| PushError::Git("rev-list".into()))
}

/// The object directory of the worktree's repository (shared by all its worktrees).
fn objects_dir(cwd: &Path) -> Result<PathBuf, PushError> {
    let mut c = git(cwd);
    c.args(["rev-parse", "--git-common-dir"]);
    let d = PathBuf::from(run(c)?);
    let d = if d.is_absolute() { d } else { cwd.join(d) };
    Ok(d.join("objects"))
}

/// The test guard on the push target: a local path (a test's bare remotes) is fine, a URL must be
/// on loopback when `STEVE_FORBID_REAL_HOSTS=1`.
fn check_git_base(base: &str) -> Result<(), PushError> {
    if Path::new(base).is_absolute() {
        return Ok(());
    }
    steve_modules::net::check_url(base).map_err(|e| PushError::HostForbidden(e.to_string()))
}

/// Pushes `refs/heads/<branch>` to the same branch of the repo; returns the pushed sha.
pub fn push(s: &PushSpec) -> Result<String, PushError> {
    if !branch_ok(s.branch) {
        return Err(PushError::BadBranch(s.branch.into()));
    }
    check_remote(s.cwd, s.full_name)?;
    check_git_base(s.git_base)?;
    let mut c = git(s.cwd);
    c.args([
        "rev-parse",
        "--verify",
        "--quiet",
        &format!("refs/heads/{}^{{commit}}", s.branch),
    ]);
    let sha = run(c).map_err(|_| PushError::NoBranch(s.branch.into()))?;
    let objects = objects_dir(s.cwd)?;

    let tmp = tempfile::Builder::new()
        .prefix("steve-push-")
        .tempdir()
        .map_err(|e| PushError::Git(e.to_string()))?;
    let mut c = git(tmp.path());
    c.args(["init", "--bare", "--quiet"]);
    run(c)?;
    std::fs::write(
        tmp.path().join("objects/info/alternates"),
        format!("{}\n", objects.display()),
    )
    .map_err(|e| PushError::Git(e.to_string()))?;
    let mut c = git(tmp.path());
    c.args(["update-ref", &format!("refs/heads/{}", s.branch), &sha]);
    run(c)?;

    let base = s.git_base.trim_end_matches('/');
    let url = format!("{base}/{}.git", s.full_name);
    let auth = Zeroizing::new(
        base64::engine::general_purpose::STANDARD
            .encode(Zeroizing::new(format!("x-access-token:{}", s.token)).as_bytes()),
    );
    let header = Zeroizing::new(format!("AUTHORIZATION: basic {}", auth.as_str()));
    let mut c = git(tmp.path());
    c.env("GIT_CONFIG_COUNT", "1")
        .env("GIT_CONFIG_KEY_0", format!("http.{base}/.extraheader"))
        .env("GIT_CONFIG_VALUE_0", header.as_str())
        .args([
            "push",
            "--quiet",
            "--no-verify",
            &url,
            &format!("refs/heads/{0}:refs/heads/{0}", s.branch),
        ]);
    run(c)?;
    Ok(sha)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn branch_names() {
        for b in ["steve/st-12", "main", "fix_1.2"] {
            assert!(branch_ok(b), "{b}");
        }
        for b in [
            "", "-x", "a..b", "a b", "a:b", "+main", "a/", "/a", "x.lock", "a~1",
        ] {
            assert!(!branch_ok(b), "{b}");
        }
    }

    #[test]
    fn real_git_base_refused_under_the_flag() {
        if !steve_modules::net::forbidding_child(
            "push::tests::real_git_base_refused_under_the_flag",
        ) {
            return;
        }
        assert_eq!(
            check_git_base("https://github.com"),
            Err(PushError::HostForbidden(
                "测试环境禁止连接外部主机：github.com".into()
            ))
        );
        assert_eq!(check_git_base("/tmp/steve-fake-remotes"), Ok(()));
        assert_eq!(check_git_base("http://127.0.0.1:9/git"), Ok(()));
    }
}
