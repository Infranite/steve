//! The merge queue's staging (E1): where a target's batches are integrated, checked and pushed
//! from. Each is a worktree of the Node's own bare mirror of the project's repository
//! (`<home>/git/<digest>.git`), filled by fetching from the repository: the mirror has objects of
//! its own (no alternates into the user's repository), and the user's config and hooks play no
//! part. One staging per target, reused batch after batch ([`reset`]), so build output stays warm.
//!
//! Every git here runs with hooks off, no filesystem monitor and no `ext::` transport.

use std::path::{Path, PathBuf};
use std::time::Duration;

use sha2::Digest as _;
use steve_node_api::*;

use crate::workspace::Commands;

/// How long refreshing the mirror from its source may take (a local fetch).
const REFRESH_DEADLINE: Duration = Duration::from_secs(300);
/// How long a merge, a checkout or a clean may take.
const STEP_DEADLINE: Duration = Duration::from_secs(300);
/// What every git of the staging gets on its command line.
const SAFE: [&str; 6] = [
    "-c",
    "core.hooksPath=/dev/null",
    "-c",
    "core.fsmonitor=false",
    "-c",
    "protocol.ext.allow=never",
];

fn internal(e: impl ToString) -> ApiError {
    ApiError::new(ErrorCode::Internal, e.to_string())
}

fn invalid(e: impl ToString) -> ApiError {
    ApiError::new(ErrorCode::Invalid, e.to_string())
}

fn with_safe<'a>(args: &[&'a str]) -> Vec<&'a str> {
    let mut v: Vec<&str> = SAFE.to_vec();
    v.extend_from_slice(args);
    v
}

/// `git args` (made safe) in `dir`, under the pipe; its stdout, or why it failed.
fn run(c: &Commands, dir: &Path, args: &[&str], deadline: Duration) -> ApiResult<String> {
    let r = c
        .write_with(dir, &with_safe(args), &[], deadline)
        .map_err(|f| internal(String::from(f)))?;
    match r.code {
        Some(0) => Ok(String::from_utf8_lossy(&r.stdout).trim().to_string()),
        Some(_) => Err(invalid(format!(
            "git {}: {}",
            args.first().copied().unwrap_or(""),
            r.stderr
        ))),
        None => Err(internal("git timed out")),
    }
}

/// Where the Node keeps its mirror of the repository at `source`.
pub(crate) fn mirror_of(home: &Path, source: &Path) -> PathBuf {
    let real = source
        .canonicalize()
        .unwrap_or_else(|_| source.to_path_buf());
    let h = sha2::Sha256::digest(real.to_string_lossy().as_bytes());
    home.join("git")
        .join(format!("{}.git", &hex::encode(h)[..16]))
}

/// Where the staging of `name` goes.
pub(crate) fn place_of(home: &Path, name: &str) -> PathBuf {
    home.join("mq").join(name)
}

/// Make the mirror when it is not there, and fetch into it from `source`: its branches as
/// `refs/source/heads/*`, its remote-tracking refs as they are, its tags. Its `origin` is set to
/// what `source`'s is (the push checks it), and a committer identity is kept for the merges (the
/// source's, else Steve's).
pub(crate) fn refresh(c: &Commands, mirror: &Path, source: &Path) -> ApiResult<()> {
    if !mirror.join("HEAD").exists() {
        let parent = mirror.parent().ok_or_else(|| internal("no parent"))?;
        std::fs::create_dir_all(parent).map_err(internal)?;
        let m = mirror.to_string_lossy();
        run(c, parent, &["init", "--quiet", "--bare", &m], STEP_DEADLINE)?;
    }
    let src = source.to_string_lossy();
    run(
        c,
        mirror,
        &[
            "fetch",
            "--quiet",
            "--prune",
            "--no-write-fetch-head",
            &src,
            "+refs/heads/*:refs/source/heads/*",
            "+refs/remotes/origin/*:refs/remotes/origin/*",
            "+refs/tags/*:refs/tags/*",
        ],
        REFRESH_DEADLINE,
    )?;
    let get = |dir: &Path, key: &str| {
        c.git(dir, &["config", "--get", key])
            .ok()
            .filter(|v| !v.is_empty())
    };
    if let Some(url) = get(source, "remote.origin.url")
        && get(mirror, "remote.origin.url").as_deref() != Some(url.as_str())
    {
        let _ = c.git(mirror, &["config", "remote.origin.url", &url]);
    }
    for (key, fallback) in [("user.name", "Steve"), ("user.email", "steve@localhost")] {
        if get(mirror, key).is_none() {
            let v = get(source, key).unwrap_or_else(|| fallback.to_string());
            c.git(mirror, &["config", key, &v]).map_err(internal)?;
        }
    }
    Ok(())
}

/// The staging worktree of `mirror` at `path` on `branch`, from `refs/remotes/origin/<branch>`. One
/// that is there already is adopted only if it is that worktree.
pub(crate) fn add(c: &Commands, mirror: &Path, path: &Path, branch: &str) -> ApiResult<()> {
    if path.exists() {
        return crate::workspace::check_worktree(c, mirror, path, branch).map_err(invalid);
    }
    let start = format!("refs/remotes/origin/{branch}");
    if c.git(mirror, &["rev-parse", "--verify", "--quiet", &start])
        .is_err()
    {
        return Err(invalid(format!(
            "the repository has no origin/{branch} to stage from"
        )));
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(internal)?;
    }
    let p = path.to_string_lossy();
    run(
        c,
        mirror,
        &[
            "worktree", "add", "--quiet", "--force", "-B", branch, &p, &start,
        ],
        STEP_DEADLINE,
    )?;
    Ok(())
}

/// Removes the staging worktree (the mirror stays for the next one).
pub(crate) fn remove(c: &Commands, mirror: &Path, path: &Path) -> Result<(), String> {
    if path.exists() {
        let p = path.to_string_lossy();
        c.git(mirror, &["worktree", "remove", "--force", &p])?;
    }
    let _ = c.git(mirror, &["worktree", "prune"]);
    Ok(())
}

fn check(
    c: &Commands,
    dir: &Path,
    expected: &Revision,
    expected_head: &str,
) -> ApiResult<GitFacts> {
    let before = crate::git::now(c, dir)?;
    if before.revision != *expected || before.head != expected_head {
        return Err(ApiError::new(ErrorCode::Stale, "changed since"));
    }
    Ok(before)
}

/// The mirror refreshed from `source`, then the staging at `dir` on `branch` at `to`, forcibly, with
/// what git does not track removed but the directories named in `keep` (wherever they are) and the
/// ones holding a `CACHEDIR.TAG`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn reset(
    c: &Commands,
    dir: &Path,
    mirror: &Path,
    source: &Path,
    branch: &str,
    to: &str,
    keep: &[RelPath],
    expected: &Revision,
    expected_head: &str,
) -> ApiResult<GitFacts> {
    check(c, dir, expected, expected_head)?;
    refresh(c, mirror, source)?;
    let commit = format!("{to}^{{commit}}");
    let sha = run(
        c,
        dir,
        &["rev-parse", "--verify", "--quiet", &commit],
        STEP_DEADLINE,
    )
    .map_err(|_| invalid(format!("no such commit: {to}")))?;
    // a merge left half done (a crash) goes first
    let _ = c.write_with(dir, &with_safe(&["merge", "--abort"]), &[], STEP_DEADLINE);
    run(
        c,
        dir,
        &["checkout", "--quiet", "--force", "-B", branch, &sha],
        STEP_DEADLINE,
    )?;
    let mut args: Vec<String> = vec!["clean".into(), "-ffdxq".into()];
    // a name, kept wherever it is (`target`, `node_modules`: the toolchains' build output)
    for k in keep {
        args.push("-e".into());
        args.push(k.as_str().trim_matches('/').to_string());
    }
    for d in cache_dirs(c, dir)? {
        args.push("-e".into());
        args.push(format!("/{d}"));
    }
    let a: Vec<&str> = args.iter().map(String::as_str).collect();
    run(c, dir, &a, STEP_DEADLINE)?;
    crate::git::now(c, dir)
}

/// The untracked directories `git clean` would remove that hold a `CACHEDIR.TAG` (relative, no
/// trailing slash).
fn cache_dirs(c: &Commands, dir: &Path) -> ApiResult<Vec<String>> {
    let out = run(c, dir, &["clean", "-ndx"], STEP_DEADLINE)?;
    Ok(out
        .lines()
        .filter_map(|l| l.strip_prefix("Would remove "))
        .filter(|p| p.ends_with('/'))
        .map(|p| p.trim_end_matches('/').to_string())
        .filter(|p| dir.join(p).join("CACHEDIR.TAG").is_file())
        .collect())
}

/// Merges `commit` into the staging's HEAD (a merge commit, `message` its message). A conflict is
/// backed out (`back_out`) and is `conflict` with the paths, one per line; uncommitted changes to
/// tracked files are `busy`. The facts after it.
pub(crate) fn merge(
    c: &Commands,
    dir: &Path,
    commit: &str,
    message: Option<&str>,
    back_out: bool,
    expected: &Revision,
    expected_head: &str,
) -> ApiResult<GitFacts> {
    check(c, dir, expected, expected_head)?;
    let tracked = run(
        c,
        dir,
        &["status", "--porcelain", "--untracked-files=no"],
        STEP_DEADLINE,
    )?;
    if !tracked.is_empty() {
        let lines: Vec<&str> = tracked.lines().take(10).collect();
        return Err(ApiError::new(ErrorCode::Busy, lines.join("\n")));
    }
    let target = format!("{commit}^{{commit}}");
    let sha = run(
        c,
        dir,
        &["rev-parse", "--verify", "--quiet", &target],
        STEP_DEADLINE,
    )
    .map_err(|_| invalid(format!("no such commit: {commit}")))?;
    let mut args = vec!["merge", "--no-ff", "--no-edit", "--quiet"];
    if let Some(m) = message {
        args.extend(["-m", m]);
    }
    args.push(&sha);
    let r = c
        .write_with(dir, &with_safe(&args), &[], STEP_DEADLINE)
        .map_err(|f| internal(String::from(f)))?;
    if r.code == Some(0) {
        return crate::git::now(c, dir);
    }
    let files = run(
        c,
        dir,
        &["diff", "--name-only", "--diff-filter=U"],
        STEP_DEADLINE,
    )
    .unwrap_or_default();
    if back_out {
        let _ = c.write_with(dir, &with_safe(&["merge", "--abort"]), &[], STEP_DEADLINE);
        // a merge that never started (a bad commit) has nothing to abort; the tree is as before
        let _ = run(
            c,
            dir,
            &["reset", "--quiet", "--hard", expected_head],
            STEP_DEADLINE,
        );
    }
    Err(match (r.code, files.is_empty()) {
        (None, _) => internal("the merge timed out"),
        (_, false) => ApiError::new(ErrorCode::Conflict, files),
        (_, true) => invalid(r.stderr),
    })
}
