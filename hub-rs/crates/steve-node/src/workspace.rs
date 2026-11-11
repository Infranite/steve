//! Working directories on this machine. Paths never leave the Node: the Hub names a root (a logical
//! id the Node allows) and a relative path, and gets back a workspace reference and a display text.

use std::path::{Component, Path, PathBuf};
use std::process::Command;

use sha2::Digest as _;
use steve_node_api::*;

/// A directory the Node lets the Hub use, by logical id.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
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
pub fn resolve(roots: &[Root], at: &RootPath, write: bool) -> ApiResult<PathBuf> {
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
/// Why a git under the pipe gave no output.
#[derive(Debug)]
pub(crate) enum Failed {
    /// Git ran and refused (its stderr): no such revision, not a repository.
    Git(String),
    /// Git did not finish under the pipe: cancelled, past its deadline, not started or not reaped.
    Pipe(String),
}

impl From<Failed> for String {
    fn from(f: Failed) -> String {
        match f {
            Failed::Git(s) | Failed::Pipe(s) => s,
        }
    }
}

pub(crate) struct Commands {
    pub inner: std::sync::Arc<crate::Inner>,
    pub op: OperationId,
}
fn common_git_dir(dir: &Path) -> Option<PathBuf> {
    let resolve_common = |gitdir: &Path| -> PathBuf {
        let common = match std::fs::read_to_string(gitdir.join("commondir")) {
            Ok(rel) => {
                let rel = Path::new(rel.trim());
                if rel.is_absolute() {
                    rel.to_path_buf()
                } else {
                    gitdir.join(rel)
                }
            }
            Err(_) => gitdir.to_path_buf(),
        };
        std::fs::canonicalize(&common).unwrap_or(common)
    };
    for cur in dir.ancestors() {
        let dot = cur.join(".git");
        if let Ok(meta) = std::fs::metadata(&dot) {
            if meta.is_dir() {
                return Some(resolve_common(&dot));
            }
            if meta.is_file() {
                let text = std::fs::read_to_string(&dot).ok()?;
                let raw = text.lines().next()?.strip_prefix("gitdir:")?.trim();
                let gitdir = if Path::new(raw).is_absolute() {
                    PathBuf::from(raw)
                } else {
                    cur.join(raw)
                };
                return Some(resolve_common(&gitdir));
            }
        }
        if cur.join("HEAD").is_file()
            && (cur.join("commondir").is_file()
                || cur.join("objects").is_dir()
                || cur.join("refs").is_dir())
        {
            return Some(resolve_common(cur));
        }
    }
    None
}

fn git_mutates_repo(args: &[&str]) -> bool {
    let mut i = 0;
    while i < args.len() {
        let a = args[i];
        if matches!(
            a,
            "-c" | "-C" | "--git-dir" | "--work-tree" | "--namespace" | "--config-env"
        ) {
            i += 2;
            continue;
        }
        if a.starts_with('-') {
            i += 1;
            continue;
        }
        let rest = &args[i + 1..];
        return match a {
            "status" | "diff" | "log" | "show" | "ls-files" | "ls-tree" | "cat-file"
            | "rev-parse" | "rev-list" | "merge-base" | "for-each-ref" | "cherry" | "patch-id"
            | "check-ignore" | "name-rev" | "describe" | "shortlog" | "version" | "help"
            | "init" => false,
            "symbolic-ref" => rest.iter().filter(|x| !x.starts_with('-')).count() > 1,
            "config" => !rest
                .iter()
                .any(|x| matches!(*x, "--get" | "--get-all" | "--get-regexp" | "--list" | "-l")),
            _ => true,
        };
    }
    false
}

impl Commands {
    pub(crate) fn git(&self, dir: &Path, args: &[&str]) -> Result<String, String> {
        match self.run(dir, args, &[], 1 << 20, git_mutates_repo(args))? {
            (_, true) => Err("git output limit".into()),
            (out, false) => Ok(String::from_utf8_lossy(&out).trim().to_string()),
        }
    }

    /// A git that only reads: no optional lock taken, no missing object fetched, no prompt. Its
    /// stdout as it is, the first `cap` bytes of it with whether there was more.
    pub(crate) fn read(
        &self,
        dir: &Path,
        args: &[&str],
        cap: usize,
    ) -> Result<(Vec<u8>, bool), Failed> {
        const READ_ENV: [(&str, &str); 3] = [
            ("GIT_OPTIONAL_LOCKS", "0"),
            ("GIT_NO_LAZY_FETCH", "1"),
            ("GIT_TERMINAL_PROMPT", "0"),
        ];
        self.run(dir, args, &READ_ENV, cap, false)
    }

    /// A git that only reads, as [`Commands::read`], with more environment (`GIT_INDEX_FILE` for a
    /// throwaway index), `input` on its stdin and its own deadline: its exit code (`None`: it did
    /// not finish), stdout and whether it was cut, and stderr. Only a failure of the pipe is an
    /// error.
    pub(crate) fn read_with(
        &self,
        dir: &Path,
        args: &[&str],
        env: &[(&str, &str)],
        input: &[u8],
        cap: usize,
        deadline: std::time::Duration,
    ) -> Result<Ran, Failed> {
        let mut all: Vec<(&str, &str)> = vec![
            ("GIT_OPTIONAL_LOCKS", "0"),
            ("GIT_NO_LAZY_FETCH", "1"),
            ("GIT_TERMINAL_PROMPT", "0"),
        ];
        all.extend_from_slice(env);
        self.run_full(dir, args, &all, input, cap, deadline, false)
    }

    /// A git that changes the repository (a rebase, its abort, a fetch), under the pipe with the
    /// deadline given: as [`Commands::read_with`] without the read-only environment.
    pub(crate) fn write_with(
        &self,
        dir: &Path,
        args: &[&str],
        env: &[(&str, &str)],
        deadline: std::time::Duration,
    ) -> Result<Ran, Failed> {
        let mut all: Vec<(&str, &str)> = vec![("GIT_TERMINAL_PROMPT", "0"), ("GIT_EDITOR", "true")];
        all.extend_from_slice(env);
        self.run_full(dir, args, &all, &[], 1 << 20, deadline, true)
    }

    /// `git args` in `dir` under the pipe: stdout (the first `cap` bytes, the rest drained and
    /// dropped) and whether it was cut; a failure is its stderr.
    fn run(
        &self,
        dir: &Path,
        args: &[&str],
        env: &[(&str, &str)],
        cap: usize,
        mutating: bool,
    ) -> Result<(Vec<u8>, bool), Failed> {
        let r = self.run_full(dir, args, env, &[], cap, DEADLINE, mutating)?;
        match r.code {
            Some(0) => Ok((r.stdout, r.truncated)),
            Some(_) => Err(Failed::Git(r.stderr)),
            None => Err(Failed::Pipe("git cancelled or timed out".into())),
        }
    }

    fn run_full(
        &self,
        dir: &Path,
        args: &[&str],
        env: &[(&str, &str)],
        input: &[u8],
        cap: usize,
        limit: std::time::Duration,
        mutating: bool,
    ) -> Result<Ran, Failed> {
        use std::io::{Read, Write};
        use std::os::{fd::AsRawFd, unix::process::CommandExt};
        use std::process::Stdio;
        use std::sync::atomic::Ordering;
        let session = control_session(&self.op);
        if self.inner.closed.load(Ordering::SeqCst) {
            return Err(Failed::Pipe("node shutting down".into()));
        }
        let started_at = std::time::Instant::now();
        let deadline = started_at + limit;
        // Advisory `flock` on the repository's common `.git` directory (`O_RDONLY | O_CLOEXEC`)
        // serializes repository mutations with `steve-core` (`prune_worktrees`, `lane`, `tidy`)
        // and sibling processes while still honoring `limit` and Node shutdown.
        let _repo_lock = if mutating
            && let Some(common) = common_git_dir(dir)
            && let Ok(f_dir) = std::fs::File::open(common)
        {
            loop {
                if unsafe { libc::flock(f_dir.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
                    break Some(f_dir);
                }
                let err = std::io::Error::last_os_error();
                if !matches!(
                    err.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) {
                    break None;
                }
                if self.inner.closed.load(Ordering::SeqCst) {
                    return Err(Failed::Pipe("node shutting down".into()));
                }
                if std::time::Instant::now() >= deadline {
                    return Ok(Ran {
                        code: None,
                        stdout: Vec::new(),
                        truncated: false,
                        stderr: String::new(),
                    });
                }
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
        } else {
            None
        };
        let mut child = Command::new("/bin/sh")
            .args([
                "-c",
                "IFS= read -r go || exit 125; exec git \"$@\"",
                "steve-git",
            ])
            .args(args)
            .envs(env.iter().copied())
            .current_dir(dir)
            .process_group(0)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| Failed::Pipe(e.to_string()))?;
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
            return Err(Failed::Pipe(e.to_string()));
        }
        let mut pipe = child.stdin.take().expect("pipe");
        let sent = pipe.write_all(b"go\n");
        // what git reads comes after the go line, written on a thread of its own so a git that
        // writes as it reads never waits on this one
        let feeding = (sent.is_ok() && !input.is_empty()).then(|| {
            let data = input.to_vec();
            std::thread::spawn(move || {
                let _ = pipe.write_all(&data);
            })
        });
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
                return Err(Failed::Pipe(
                    "cannot bound git pipes; process evidence retained".into(),
                ));
            }
        }
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut truncated = false;
        // whether `dest` had to drop some of what came, past `cap`
        let drain = |reader: &mut dyn Read, dest: &mut Vec<u8>, cap: usize| -> bool {
            let mut cut = false;
            let mut buf = [0; 4096];
            // Per tick budget; a continuously writable pipe must not starve timeout checks.
            for _ in 0..64 {
                match reader.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        // past the cap the rest is read and dropped, so git can finish
                        let keep = n.min(cap.saturating_sub(dest.len()));
                        dest.extend_from_slice(&buf[..keep]);
                        cut |= keep < n;
                    }
                }
            }
            cut
        };
        const ERR_CAP: usize = 64 * 1024;
        let status = loop {
            truncated |= drain(&mut out, &mut stdout, cap);
            drain(&mut err, &mut stderr, ERR_CAP);
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
        truncated |= drain(&mut out, &mut stdout, cap);
        drain(&mut err, &mut stderr, ERR_CAP);
        if !crate::procs::wait_gone(pid, started).gone() {
            return Err(Failed::Pipe("git group stop uncertain".into()));
        }
        self.inner
            .j
            .tx(|t| t.drop_proc(&session, pid))
            .map_err(|e| Failed::Pipe(e.to_string()))?;
        if let Some(f) = feeding {
            let _ = f.join();
        }
        Ok(Ran {
            code: status.map(|s| s.code().unwrap_or(-1)),
            stdout,
            truncated,
            stderr: String::from_utf8_lossy(&stderr).trim().to_string(),
        })
    }
}

/// How long a git under the pipe may take, unless its caller says otherwise.
const DEADLINE: std::time::Duration = std::time::Duration::from_secs(30);

/// What one git under the pipe did.
pub(crate) struct Ran {
    /// `None`: it did not finish (cancelled, past its deadline).
    pub code: Option<i32>,
    pub stdout: Vec<u8>,
    pub truncated: bool,
    pub stderr: String,
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
        PrepareKind::Staging { .. } => {
            let mirror = repo.ok_or("no mirror")?;
            crate::staging::remove(commands, mirror, path).map(|_| None)
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
pub fn still_valid(path: &Path, roots: &[Root], under_root: bool) -> ApiResult<PathBuf> {
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
