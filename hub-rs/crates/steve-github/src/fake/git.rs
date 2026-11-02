//! Bare git remotes standing in for `https://github.com/<owner>/<name>`.
//!
//! Tests point git at them with `url.<root>/.insteadOf=https://github.com/`
//! (see [`super::FakeGitHub::git_url_rewrite`]) so a real `git push` lands
//! here, and the fake's PR creation can check the branch was really pushed.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::FakeError;

/// Path of the bare repo for `full_name` under `root` (`<root>/<o>/<r>.git`).
pub fn bare_path(root: &Path, full_name: &str) -> PathBuf {
    root.join(format!("{full_name}.git"))
}

fn git(dir: &Path, args: &[&str]) -> Result<String, FakeError> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        // Never read the developer's global/system config (hooks, signing…).
        .env("GIT_CONFIG_GLOBAL", "/dev/null")
        .env("GIT_CONFIG_NOSYSTEM", "1")
        .output()
        .map_err(|e| FakeError::Git(e.to_string()))?;
    if !out.status.success() {
        return Err(FakeError::Git(
            String::from_utf8_lossy(&out.stderr).trim().to_string(),
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).to_string())
}

/// Creates the bare repo (idempotent) with `default_branch` as HEAD.
pub fn init_bare(root: &Path, full_name: &str, default_branch: &str) -> Result<PathBuf, FakeError> {
    let path = bare_path(root, full_name);
    if !path.join("HEAD").exists() {
        std::fs::create_dir_all(&path).map_err(|e| FakeError::Git(e.to_string()))?;
        git(
            &path,
            &[
                "init",
                "--bare",
                "--quiet",
                &format!("--initial-branch={default_branch}"),
            ],
        )?;
    }
    Ok(path)
}

/// Branch → sha as currently in the bare repo.
pub fn branches(bare: &Path) -> Result<BTreeMap<String, String>, FakeError> {
    let out = git(
        bare,
        &[
            "for-each-ref",
            "--format=%(refname:strip=2) %(objectname)",
            "refs/heads",
        ],
    )?;
    Ok(out
        .lines()
        .filter_map(|l| l.split_once(' '))
        .map(|(b, s)| (b.to_string(), s.to_string()))
        .collect())
}

pub fn delete_branch(bare: &Path, branch: &str) -> Result<(), FakeError> {
    git(bare, &["update-ref", "-d", &format!("refs/heads/{branch}")]).map(|_| ())
}
