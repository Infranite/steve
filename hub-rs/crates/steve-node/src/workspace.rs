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
    /// A read-only root is listed, read and scanned; no workspace is prepared in it.
    pub access: RootAccess,
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

/// `root` joined with `rel`, resolved, and still inside `root` (symlinks included). `write`: the
/// root must allow workspaces (a read-only root is `denied`).
pub(crate) fn resolve(roots: &[Root], at: &RootPath, write: bool) -> ApiResult<PathBuf> {
    let root = roots
        .iter()
        .find(|r| r.id == at.root)
        .ok_or_else(|| ApiError::new(ErrorCode::Denied, "not a root this node allows"))?;
    if write && root.access == RootAccess::ReadOnly {
        return Err(ApiError::new(
            ErrorCode::Denied,
            "a read-only root: no workspace there",
        ));
    }
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

/// Where a directory that may not exist yet goes: `at` names a read-write root and a relative
/// path of plain names (at least one) inside it. Its parent must exist and resolve inside the root
/// (symlinks included); the directory itself, when it is there, must resolve to the same place.
pub(crate) fn target(roots: &[Root], at: &RootPath) -> ApiResult<PathBuf> {
    let rel = Path::new(at.rel.as_str());
    let name = rel.file_name().ok_or_else(|| {
        ApiError::new(
            ErrorCode::Invalid,
            "a directory inside the root, not the root",
        )
    })?;
    let parent = RootPath {
        root: at.root.clone(),
        rel: RelPath::new(
            rel.parent()
                .and_then(Path::to_str)
                .unwrap_or_default()
                .to_string(),
        )
        .map_err(|e| ApiError::new(ErrorCode::Invalid, e.to_string()))?,
    };
    let p = resolve(roots, &parent, true)?.join(name);
    if p.exists() {
        let real = p
            .canonicalize()
            .map_err(|e| ApiError::new(ErrorCode::Invalid, e.to_string()))?;
        if real != p {
            return Err(ApiError::new(ErrorCode::Denied, "outside the root"));
        }
    }
    Ok(p)
}

pub(crate) fn control_session(op: &OperationId) -> SessionId {
    SessionId::new(derived_id("workspace-control", op.as_str())).expect("derived id")
}

/// The directory `rel` below `base` (a workspace's own directory), resolved and still inside it.
pub(crate) fn below(base: &Path, rel: &RelPath) -> ApiResult<PathBuf> {
    let r = Path::new(rel.as_str());
    if r.components()
        .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return Err(ApiError::new(
            ErrorCode::Invalid,
            "a relative path inside the workspace",
        ));
    }
    let real = base
        .join(r)
        .canonicalize()
        .map_err(|_| ApiError::new(ErrorCode::Invalid, "no such directory"))?;
    if !real.starts_with(base) {
        return Err(ApiError::new(ErrorCode::Denied, "outside the workspace"));
    }
    if !real.is_dir() {
        return Err(ApiError::new(ErrorCode::Invalid, "not a directory"));
    }
    Ok(real)
}

/// Every git process waits on a pipe until its identity is durable. A Node crash before recording
/// closes the pipe: the shell exits without executing git. After recording, recovery can stop it.
pub(crate) struct Commands {
    pub inner: std::sync::Arc<crate::Inner>,
    pub op: OperationId,
}
impl Commands {
    fn git(&self, dir: &Path, args: &[&str]) -> Result<String, String> {
        use std::io::{Read, Write};
        use std::os::{fd::AsRawFd, unix::process::CommandExt};
        use std::process::Stdio;
        use std::sync::atomic::Ordering;
        let session = control_session(&self.op);
        if self.inner.closed.load(Ordering::SeqCst) {
            return Err("node shutting down".into());
        }
        let mut child = Command::new("/bin/sh")
            .args([
                "-c",
                "IFS= read -r go || exit 125; exec git \"$@\"",
                "steve-git",
            ])
            .args(args)
            .current_dir(dir)
            .process_group(0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| e.to_string())?;
        let pid = child.id() as i64;
        let started = crate::procs::start_time(pid);
        if let Err(e) = self.inner.j.tx(|t| {
            t.put_proc(
                &session,
                pid,
                0,
                self.inner.incarnation,
                "workspace",
                started,
            )
        }) {
            drop(child.stdin.take());
            let _ = child.kill();
            let _ = child.wait();
            return Err(e.to_string());
        }
        let mut input = child.stdin.take().expect("pipe");
        let sent = input.write_all(b"go\n");
        drop(input);
        let mut out = child.stdout.take().expect("pipe");
        let mut err = child.stderr.take().expect("pipe");
        for fd in [out.as_raw_fd(), err.as_raw_fd()] {
            let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
            if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
            {
                if crate::procs::state(pid, started) == crate::procs::Stopped::Alive {
                    unsafe {
                        libc::kill(-(pid as i32), libc::SIGKILL);
                    }
                }
                let _ = child.kill();
                let _ = child.wait();
                return Err("cannot bound git pipes; process evidence retained".into());
            }
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut truncated = false;
        let mut drain = |reader: &mut dyn Read, dest: &mut Vec<u8>| {
            let mut buf = [0; 4096];
            // Per tick budget; a continuously writable pipe must not starve timeout checks.
            for _ in 0..64 {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if dest.len() + n <= 1 << 20 {
                            dest.extend_from_slice(&buf[..n]);
                        } else {
                            truncated = true;
                            break;
                        }
                    }
                }
            }
        };
        let status = loop {
            drain(&mut out, &mut stdout);
            drain(&mut err, &mut stderr);
            if let Ok(Some(status)) = child.try_wait() {
                break Some(status);
            }
            if sent.is_err()
                || self.inner.closed.load(Ordering::SeqCst)
                || std::time::Instant::now() >= deadline
            {
                // Child is unreaped, so its pid cannot have been recycled. Descendants are killed
                // only while the group identity still matches; uncertainty retains the procs row.
                if crate::procs::state(pid, started) == crate::procs::Stopped::Alive {
                    unsafe {
                        libc::kill(-(pid as i32), libc::SIGKILL);
                    }
                }
                let _ = child.kill();
                let _ = child.wait();
                break None;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        drain(&mut out, &mut stdout);
        drain(&mut err, &mut stderr);
        if !crate::procs::state(pid, started).gone() {
            return Err("git group stop uncertain".into());
        }
        self.inner
            .j
            .tx(|t| t.drop_proc(&session, pid))
            .map_err(|e| e.to_string())?;
        if truncated {
            return Err("git output limit".into());
        }
        match status {
            Some(status) if status.success() => {
                Ok(String::from_utf8_lossy(&stdout).trim().to_string())
            }
            Some(_) => Err(String::from_utf8_lossy(&stderr).trim().to_string()),
            None => Err("git cancelled or timed out".into()),
        }
    }
}

/// A worktree of `repo` at `path` on `branch` (created from `base`, else HEAD, when new). One that
/// is there already is adopted only if it is a worktree of that very repository on that branch.
pub(crate) fn add_worktree(
    commands: &Commands,
    repo: &Path,
    path: &Path,
    branch: &str,
    base: Option<&str>,
) -> Result<(), String> {
    if path.exists() {
        return check_worktree(commands, repo, path, branch);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
    }
    let p = path.to_string_lossy();
    if commands
        .git(repo, &["rev-parse", "--verify", branch])
        .is_ok()
    {
        commands
            .git(repo, &["worktree", "add", &p, branch])
            .map(|_| ())
    } else {
        commands
            .git(
                repo,
                &["worktree", "add", "-b", branch, &p, base.unwrap_or("HEAD")],
            )
            .map(|_| ())
    }
}

/// How many bytes are free where a worktree goes (`None`: it cannot be told).
pub(crate) type FreeBytes = Box<dyn Fn() -> Option<u64> + Send>;

/// [`add_worktree`], refused (`exhausted`, "disk low") when the worktree would be new on a new
/// branch while fewer than the floor's bytes are free there: a new item waits for disk space, one
/// whose branch is there gets its sources back (megabytes).
pub(crate) fn add_worktree_above(
    commands: &Commands,
    repo: &Path,
    path: &Path,
    branch: &str,
    base: Option<&str>,
    floor: Option<(u64, FreeBytes)>,
) -> ApiResult<()> {
    if let Some((floor, free)) = floor
        && !path.exists()
        && commands
            .git(
                repo,
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("refs/heads/{branch}"),
                ],
            )
            .is_err()
        && let Some(free) = free()
        && free < floor
    {
        return Err(ApiError::new(
            ErrorCode::Exhausted,
            format!("disk low: {free} bytes free, the floor is {floor}"),
        ));
    }
    add_worktree(commands, repo, path, branch, base)
        .map_err(|e| ApiError::new(ErrorCode::Invalid, format!("git worktree: {e}")))
}

/// `path` is a worktree of `repo` with `branch` checked out.
pub(crate) fn check_worktree(
    commands: &Commands,
    repo: &Path,
    path: &Path,
    branch: &str,
) -> Result<(), String> {
    let common = |d: &Path| -> Result<PathBuf, String> {
        let c = commands.git(
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
    let top = PathBuf::from(commands.git(path, &["rev-parse", "--show-toplevel"])?)
        .canonicalize()
        .map_err(|e| e.to_string())?;
    if top != path.canonicalize().map_err(|e| e.to_string())? {
        return Err(format!("{} is inside another worktree", path.display()));
    }
    let head = commands.git(path, &["rev-parse", "--abbrev-ref", "HEAD"])?;
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
    commands: &Commands,
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
                commands.git(repo, &["worktree", "remove", &path.to_string_lossy()])?;
            }
            match (keep_branch, branch) {
                (false, Some(b)) => Ok(commands
                    .git(repo, &["branch", "-d", b])
                    .err()
                    .map(|e| format!("branch {b} kept: {e}"))),
                _ => Ok(None),
            }
        }
    }
}

/// A recorded workspace path is still a directory, and (for one under a root) still inside a root
/// that allows workspaces.
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
            .filter(|r| r.access == RootAccess::ReadWrite)
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
