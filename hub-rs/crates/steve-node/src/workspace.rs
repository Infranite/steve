//! Working directories on this machine. Paths never leave the Node: the Hub names a root (a logical
//! id the Node allows) and a relative path, and gets back a workspace reference and a display text.

use std::path::{Component, Path, PathBuf};
use std::process::Command;

use sha2::Digest as _;
use steve_node_api::*;

/// A directory the Node lets the Hub use, by logical id.
#[derive(Debug, Clone)]
pub struct Root {
    pub id: RootId,
    pub path: PathBuf,
    /// How it is shown (`~/code`).
    pub display: String,
}

/// The directory name for a resource: readable when it is simple, always unique (a hash suffix).
pub(crate) fn dir_name(resource: &str) -> String {
    let readable: String = resource
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || c == '-' {
                c
            } else {
                '_'
            }
        })
        .take(40)
        .collect();
    let h = hex::encode(sha2::Sha256::digest(resource.as_bytes()));
    format!("{readable}-{}", &h[..12])
}

/// `root` joined with `rel`, resolved, and still inside `root` (symlinks included).
pub(crate) fn resolve(roots: &[Root], at: &RootPath) -> ApiResult<PathBuf> {
    let root = roots
        .iter()
        .find(|r| r.id == at.root)
        .ok_or_else(|| ApiError::new(ErrorCode::Denied, "not a root this node allows"))?;
    let rel = Path::new(at.rel.as_str());
    if rel
        .components()
        .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return Err(ApiError::new(
            ErrorCode::Invalid,
            "a relative path inside the root",
        ));
    }
    let base = root
        .path
        .canonicalize()
        .map_err(|e| ApiError::new(ErrorCode::Invalid, format!("root unavailable: {e}")))?;
    let p = base.join(rel);
    let real = p
        .canonicalize()
        .map_err(|_| ApiError::new(ErrorCode::Invalid, "no such directory"))?;
    if !real.starts_with(&base) {
        return Err(ApiError::new(ErrorCode::Denied, "outside the root"));
    }
    Ok(real)
}

fn git(dir: &Path, args: &[&str]) -> Result<String, String> {
    let o = Command::new("git")
        .args(args)
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .output()
        .map_err(|e| e.to_string())?;
    if o.status.success() {
        Ok(String::from_utf8_lossy(&o.stdout).trim().to_string())
    } else {
        Err(String::from_utf8_lossy(&o.stderr).trim().to_string())
    }
}

/// A worktree of `repo` at `path` on `branch` (created from `base`, else HEAD, when new). One that
/// is there already is adopted only if it is a worktree of that very repository on that branch.
pub(crate) fn add_worktree(
    repo: &Path,
    path: &Path,
    branch: &str,
    base: Option<&str>,
) -> Result<(), String> {
    if path.exists() {
        return check_worktree(repo, path, branch);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let p = path.to_string_lossy();
    if git(repo, &["rev-parse", "--verify", branch]).is_ok() {
        git(repo, &["worktree", "add", &p, branch]).map(|_| ())
    } else {
        git(
            repo,
            &["worktree", "add", "-b", branch, &p, base.unwrap_or("HEAD")],
        )
        .map(|_| ())
    }
}

/// `path` is a worktree of `repo` with `branch` checked out.
pub(crate) fn check_worktree(repo: &Path, path: &Path, branch: &str) -> Result<(), String> {
    let common = |d: &Path| -> Result<PathBuf, String> {
        let c = git(
            d,
            &["rev-parse", "--path-format=absolute", "--git-common-dir"],
        )?;
        PathBuf::from(c).canonicalize().map_err(|e| e.to_string())
    };
    if common(path)? != common(repo)? {
        return Err(format!(
            "{} is not a worktree of {}",
            path.display(),
            repo.display()
        ));
    }
    let top = PathBuf::from(git(path, &["rev-parse", "--show-toplevel"])?)
        .canonicalize()
        .map_err(|e| e.to_string())?;
    if top != path.canonicalize().map_err(|e| e.to_string())? {
        return Err(format!("{} is inside another worktree", path.display()));
    }
    let head = git(path, &["rev-parse", "--abbrev-ref", "HEAD"])?;
    if head != branch {
        return Err(format!(
            "{} has {head} checked out, not {branch}",
            path.display()
        ));
    }
    Ok(())
}

/// Removes a worktree or a scratch directory; a directory the workspace only adopted (an existing
/// project directory) is never removed. Returns what could not be done (a branch git would not
/// delete), never swallowing it.
pub(crate) fn remove(
    kind: &PrepareKind,
    path: &Path,
    repo: Option<&Path>,
    branch: Option<&str>,
    keep_branch: bool,
) -> Result<Option<String>, String> {
    match kind {
        PrepareKind::Existing { .. } => Ok(None),
        PrepareKind::Scratch => {
            if path.exists() {
                std::fs::remove_dir_all(path).map_err(|e| e.to_string())?;
            }
            Ok(None)
        }
        PrepareKind::Worktree { .. } => {
            let repo = repo.ok_or("no repository")?;
            if path.exists() {
                git(repo, &["worktree", "remove", &path.to_string_lossy()])?;
            }
            match (keep_branch, branch) {
                (false, Some(b)) => Ok(git(repo, &["branch", "-d", b])
                    .err()
                    .map(|e| format!("branch {b} kept: {e}"))),
                _ => Ok(None),
            }
        }
    }
}

/// A recorded workspace path is still a directory, and (for one under a root) still inside it.
pub(crate) fn still_valid(path: &Path, roots: &[Root], under_root: bool) -> ApiResult<PathBuf> {
    let real = path
        .canonicalize()
        .map_err(|_| ApiError::new(ErrorCode::Invalid, "the workspace directory is gone"))?;
    if !real.is_dir() {
        return Err(ApiError::new(
            ErrorCode::Invalid,
            "the workspace is not a directory",
        ));
    }
    if under_root
        && !roots
            .iter()
            .filter_map(|r| r.path.canonicalize().ok())
            .any(|r| real.starts_with(r))
    {
        return Err(ApiError::new(
            ErrorCode::Denied,
            "the workspace left its root",
        ));
    }
    Ok(real)
}

/// Writable: a file of a fresh random name can be created and removed in it. Nothing that was there
/// is touched (create-new, never following an existing name).
pub(crate) fn usable(path: &Path) -> bool {
    let n = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let name = format!(
        ".steve-node-probe-{}",
        &hex::encode(sha2::Sha256::digest(format!(
            "{n}{:?}",
            std::thread::current().id()
        )))[..16]
    );
    let probe = path.join(name);
    match std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&probe)
    {
        Ok(_) => {
            let _ = std::fs::remove_file(&probe);
            true
        }
        Err(_) => false,
    }
}

/// How a path is shown: with `~` for the home directory.
pub(crate) fn display(path: &Path) -> String {
    let s = path.to_string_lossy().to_string();
    match std::env::var_os("HOME") {
        Some(h) if !h.is_empty() => {
            let h = h.to_string_lossy().to_string();
            match s.strip_prefix(&h) {
                Some(rest) => format!("~{rest}"),
                None => s,
            }
        }
        _ => s,
    }
}
