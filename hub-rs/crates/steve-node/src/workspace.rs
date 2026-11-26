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
        .ok_or_else(|| Error::new(Code::Forbidden, "not a root this node allows"))?;
    if write && root.access == RootAccess::ReadOnly {
        return Err(Error::new(
            Code::Forbidden,
            "a read-only root: no workspace there",
        ));
    }
    let rel = Path::new(at.rel.as_str());
    if rel
        .components()
        .any(|c| !matches!(c, Component::Normal(_) | Component::CurDir))
    {
        return Err(Error::new(Code::Invalid, "a relative path inside the root"));
    }
    let base = root
        .path
        .canonicalize()
        .map_err(|e| Error::new(Code::Invalid, format!("root unavailable: {e}")))?;
    let p = base.join(rel);
    let real = p
        .canonicalize()
        .map_err(|_| Error::new(Code::Invalid, "no such directory"))?;
    if !real.starts_with(&base) {
        return Err(Error::new(Code::Forbidden, "outside the root"));
    }
    Ok(real)
}

/// Where a directory that may not exist yet goes: `at` names a read-write root and a relative
/// path of plain names (at least one) inside it. Its parent must exist and resolve inside the root
/// (symlinks included); the directory itself, when it is there, must resolve to the same place.
pub(crate) fn target(roots: &[Root], at: &RootPath) -> ApiResult<PathBuf> {
    let rel = Path::new(at.rel.as_str());
    let name = rel
        .file_name()
        .ok_or_else(|| Error::new(Code::Invalid, "a directory inside the root, not the root"))?;
    let parent = RootPath {
        root: at.root.clone(),
        rel: RelPath::new(
            rel.parent()
                .and_then(Path::to_str)
                .unwrap_or_default()
                .to_string(),
        )
        .map_err(|e| Error::new(Code::Invalid, e.to_string()))?,
    };
    let p = resolve(roots, &parent, true)?.join(name);
    if p.exists() {
        let real = p
            .canonicalize()
            .map_err(|e| Error::new(Code::Invalid, e.to_string()))?;
        if real != p {
            return Err(Error::new(Code::Forbidden, "outside the root"));
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
        return Err(Error::new(
            Code::Invalid,
            "a relative path inside the workspace",
        ));
    }
    let real = base
        .join(r)
        .canonicalize()
        .map_err(|_| Error::new(Code::Invalid, "no such directory"))?;
    if !real.starts_with(base) {
        return Err(Error::new(Code::Forbidden, "outside the workspace"));
    }
    if !real.is_dir() {
        return Err(Error::new(Code::Invalid, "not a directory"));
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

/// Whether `git args…` mutates `.git/worktrees` metadata (`git worktree` subcommands other than
/// `list`: `add`, `remove`, `prune`, `move`, `repair`, `lock`, `unlock`) or is a `git branch`, which
/// walks that metadata and dies when an entry goes away under it. Kept in lockstep with
/// `steve-core/src/git.rs` so only these take the common `.git` `flock`, while `fetch` / `push` /
/// `rebase` / `commit` rely on Git's own locks without blocking worktrees.
fn git_touches_worktrees(args: &[&str]) -> bool {
    let mut i = 0;
    while i < args.len() {
        let a = args[i];
        if matches!(
            a,
            "-c" | "-C"
                | "--git-dir"
                | "--work-tree"
                | "--namespace"
                | "--super-prefix"
                | "--config-env"
        ) {
            i += 2;
            continue;
        }
        if a == "--" {
            i += 1;
            break;
        }
        if a.starts_with('-') {
            i += 1;
            continue;
        }
        break;
    }
    match args.get(i).copied() {
        Some("worktree") => matches!(
            args[i + 1..].iter().copied().find(|a| !a.starts_with('-')),
            Some(sub) if sub != "list"
        ),
        Some("branch") => true,
        _ => false,
    }
}

impl Commands {
    pub(crate) fn git(&self, dir: &Path, args: &[&str]) -> Result<String, String> {
        match self.run(dir, args, &[], 1 << 20, git_touches_worktrees(args))? {
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
        self.run_full(
            dir,
            args,
            &all,
            &[],
            1 << 20,
            deadline,
            git_touches_worktrees(args),
        )
    }

    /// `git args` in `dir` under the pipe: stdout (the first `cap` bytes, the rest drained and
    /// dropped) and whether it was cut; a failure is its stderr.
    fn run(
        &self,
        dir: &Path,
        args: &[&str],
        env: &[(&str, &str)],
        cap: usize,
        lock_worktrees: bool,
    ) -> Result<(Vec<u8>, bool), Failed> {
        let r = self.run_full(dir, args, env, &[], cap, DEADLINE, lock_worktrees)?;
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
        lock_worktrees: bool,
    ) -> Result<Ran, Failed> {
        use std::os::fd::AsRawFd;
        use std::sync::atomic::Ordering;
        let session = control_session(&self.op);
        if self.inner.closed.load(Ordering::SeqCst) {
            return Err(Failed::Pipe("node shutting down".into()));
        }
        let started_at = std::time::Instant::now();
        let deadline = started_at + limit;
        // Advisory `flock` on the repository's common `.git` directory (`O_RDONLY | O_CLOEXEC`)
        // serializes `git worktree` changes (`.git/worktrees` directory creation/removal) with
        // `steve-core` (`prune_worktrees`, `lane`, `tidy`) and sibling processes while honoring
        // `limit` and Node shutdown; non-worktree commands (`fetch`, `rebase`, `push`) do not lock.
        let _worktrees_lock = if lock_worktrees
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
        let GitPipes {
            mut child,
            pid,
            started,
            sent,
            feeding,
            mut out,
            mut err,
        } = self.open_git_pipes(dir, args, env, input, &session)?;
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut truncated = false;
        const ERR_CAP: usize = 64 * 1024;
        let status = loop {
            truncated |= drain_git_pipe(&mut out, &mut stdout, cap);
            drain_git_pipe(&mut err, &mut stderr, ERR_CAP);
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
        truncated |= drain_git_pipe(&mut out, &mut stdout, cap);
        drain_git_pipe(&mut err, &mut stderr, ERR_CAP);
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

    /// Register the child before releasing its go pipe, then make both output pipes bounded.
    fn open_git_pipes(
        &self,
        dir: &Path,
        args: &[&str],
        env: &[(&str, &str)],
        input: &[u8],
        session: &SessionId,
    ) -> Result<GitPipes, Failed> {
        use std::io::Write;
        use std::os::{fd::AsRawFd, unix::process::CommandExt};
        use std::process::Stdio;
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
                session,
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
        let out = child.stdout.take().expect("pipe");
        let err = child.stderr.take().expect("pipe");
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
        Ok(GitPipes {
            child,
            pid,
            started,
            sent,
            feeding,
            out,
            err,
        })
    }
}

/// The running Git process and its pipes, with the journal's original process identity.
struct GitPipes {
    child: std::process::Child,
    pid: i64,
    started: Option<u64>,
    sent: std::io::Result<()>,
    feeding: Option<std::thread::JoinHandle<()>>,
    out: std::process::ChildStdout,
    err: std::process::ChildStderr,
}

/// Drain a bounded per-tick batch; excess bytes are discarded so the child can finish.
fn drain_git_pipe(reader: &mut dyn std::io::Read, dest: &mut Vec<u8>, cap: usize) -> bool {
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

/// How long fetching the remote a new branch starts from may take.
const FETCH_DEADLINE: std::time::Duration = std::time::Duration::from_secs(120);

/// Where a new branch starts when the caller names no base: the remote's default branch as it is
/// now, fetched right before (a local checkout may be far behind it, and work branched from there
/// collides with what the remote already has). The remote is `origin`, else the only one there is.
/// `HEAD` when the repository has no remote, it cannot be reached, or its default branch is not
/// known: the work starts where it always did. An explicit authentication failure is surfaced
/// instead of silently creating a branch at a potentially stale local HEAD.
pub(crate) fn fresh_base(commands: &Commands, repo: &Path) -> ApiResult<String> {
    let remotes = commands.git(repo, &["remote"]).unwrap_or_default();
    let names: Vec<&str> = remotes
        .lines()
        .map(str::trim)
        .filter(|r| !r.is_empty())
        .collect();
    let remote = match names.as_slice() {
        [] => return Ok("HEAD".into()),
        [one] => *one,
        many if many.contains(&"origin") => "origin",
        _ => return Ok("HEAD".into()),
    };
    let fetch = |args: &[&str]| {
        let Ok(r) = commands.write_with(repo, args, &[], FETCH_DEADLINE) else {
            return Ok(false);
        };
        if r.code == Some(0) {
            return Ok(true);
        }
        if let Some(error) = crate::git::authentication_error(&r.stderr) {
            return Err(error);
        }
        Ok(false)
    };
    if !fetch(&["fetch", "--quiet", remote])? {
        return Ok("HEAD".into());
    }
    let has = |r: &str| {
        commands
            .git(
                repo,
                &[
                    "rev-parse",
                    "--verify",
                    "--quiet",
                    &format!("{r}^{{commit}}"),
                ],
            )
            .is_ok()
    };
    let head = format!("refs/remotes/{remote}/HEAD");
    /* a clone knows its remote's default branch; a remote added by hand learns it once */
    if !has(&head) {
        fetch(&["remote", "set-head", remote, "--auto"])?;
    }
    Ok([
        head,
        format!("refs/remotes/{remote}/main"),
        format!("refs/remotes/{remote}/master"),
    ]
    .into_iter()
    .find(|r| has(r))
    .unwrap_or_else(|| "HEAD".into()))
}

/// A worktree of `repo` at `path` on `branch` (created from `base`, else from [`fresh_base`], when
/// new). One that is there already is adopted only if it is a worktree of that very repository on
/// that branch.
pub(crate) fn add_worktree(
    commands: &Commands,
    repo: &Path,
    path: &Path,
    branch: &str,
    base: Option<&str>,
) -> ApiResult<()> {
    let invalid = |e| Error::new(Code::Invalid, format!("git worktree: {e}"));
    if path.exists() {
        return check_worktree(commands, repo, path, branch).map_err(invalid);
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| invalid(e.to_string()))?;
    }
    let p = path.to_string_lossy();
    if commands
        .git(repo, &["rev-parse", "--verify", branch])
        .is_ok()
    {
        commands
            .git(repo, &["worktree", "add", &p, branch])
            .map(|_| ())
            .map_err(invalid)
    } else {
        let base = match base {
            Some(b) => b.to_string(),
            None => fresh_base(commands, repo)?,
        };
        /* `--no-track`: the item's branch is its own, not a copy of the remote's default one */
        commands
            .git(
                repo,
                &["worktree", "add", "--no-track", "-b", branch, &p, &base],
            )
            .map(|_| ())
            .map_err(invalid)
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
        return Err(Error::new(
            Code::Exhausted,
            format!("disk low: {free} bytes free, the floor is {floor}"),
        ));
    }
    add_worktree(commands, repo, path, branch, base)
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
pub fn still_valid(path: &Path, roots: &[Root], under_root: bool) -> ApiResult<PathBuf> {
    let real = path
        .canonicalize()
        .map_err(|_| Error::new(Code::Invalid, "the workspace directory is gone"))?;
    if !real.is_dir() {
        return Err(Error::new(
            Code::Invalid,
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
        return Err(Error::new(Code::Forbidden, "the workspace left its root"));
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

/// Bounded timeout for one lightweight workspace fingerprint probe.
const WORKSPACE_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(3);
/// Output cap for `git status --porcelain=v2 --branch -z` during progress observation.
const WORKSPACE_PROBE_OUTPUT_CAP: usize = 256 * 1024;
/// Maximum dirty/untracked paths stat'd to detect repeated edits on the same dirty file.
const WORKSPACE_STAT_PATHS_MAX: usize = 256;
/// Maximum directory entries visited when fingerprinting a non-git workspace.
const WORKSPACE_WALK_ENTRIES_MAX: usize = 1024;

/// Directories the progress fingerprint never walks: every toolchain's in-tree build and cache
/// directories, named in the toolchain data rather than here.
static WALK_SKIPPED_DIRS: std::sync::LazyLock<Vec<String>> = std::sync::LazyLock::new(|| {
    steve_toolchains::Toolchains::builtin()
        .in_tree_names()
        .into_iter()
        .map(str::to_owned)
        .collect()
});
/// Maximum recursion depth when fingerprinting a non-git workspace.
const WORKSPACE_WALK_DEPTH_MAX: usize = 4;

/// Lightweight, deterministic fingerprint of the workspace's current state (HEAD, tracked/untracked
/// changes, and dirty file metadata in git repos; bounded directory stat walk otherwise).
/// Returns `None` if the workspace cannot be probed within its timeout or output cap.
pub(crate) fn fingerprint(dir: &Path) -> Option<Revision> {
    if dir.join(".git").exists() {
        git_fingerprint(dir)
    } else {
        dir_fingerprint(dir)
    }
}

fn git_fingerprint(dir: &Path) -> Option<Revision> {
    use std::io::Read;
    use std::os::fd::AsRawFd;
    use std::os::unix::process::CommandExt;

    let mut child = Command::new("git")
        .args([
            "status",
            "--porcelain=v2",
            "--branch",
            "-z",
            "--untracked-files=normal",
        ])
        .current_dir(dir)
        .env("GIT_OPTIONAL_LOCKS", "0")
        .env("GIT_NO_LAZY_FETCH", "1")
        .env("GIT_TERMINAL_PROMPT", "0")
        .process_group(0)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .ok()?;
    let mut pipe = child.stdout.take()?;
    let flags = unsafe { libc::fcntl(pipe.as_raw_fd(), libc::F_GETFL) };
    if flags < 0
        || unsafe { libc::fcntl(pipe.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
    {
        let _ = child.kill();
        let _ = child.wait();
        return None;
    }
    let until = std::time::Instant::now() + WORKSPACE_PROBE_TIMEOUT;
    let mut output = Vec::new();
    let mut bytes = [0u8; 4096];
    let ok = loop {
        for _ in 0..16 {
            match pipe.read(&mut bytes) {
                Ok(0) | Err(_) => break,
                Ok(n) => output.extend_from_slice(&bytes[..n]),
            }
        }
        if std::time::Instant::now() >= until || output.len() > WORKSPACE_PROBE_OUTPUT_CAP {
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            let _ = child.kill();
            let _ = child.wait();
            return None;
        }
        match child.try_wait().ok()? {
            Some(status) => {
                while let Ok(n) = pipe.read(&mut bytes) {
                    if n == 0 || output.len() + n > WORKSPACE_PROBE_OUTPUT_CAP {
                        break;
                    }
                    output.extend_from_slice(&bytes[..n]);
                }
                break status.success();
            }
            None => std::thread::sleep(std::time::Duration::from_millis(10)),
        }
    };
    if !ok {
        return None;
    }

    let mut hasher = sha2::Sha256::new();
    hasher.update(b"git-v2\0");
    hasher.update(&output);
    let mut stat_count = 0usize;
    let mut records = output.split(|b| *b == 0);
    while let Some(raw) = records.next() {
        if raw.is_empty() {
            continue;
        }
        let Ok(entry) = std::str::from_utf8(raw) else {
            continue;
        };
        let path = if entry.starts_with("1 ") {
            entry.splitn(9, ' ').nth(8)
        } else if entry.starts_with("2 ") {
            let _orig = records.next();
            entry.splitn(10, ' ').nth(9)
        } else if entry.starts_with("u ") {
            entry.splitn(11, ' ').nth(10)
        } else if let Some(p) = entry.strip_prefix("? ") {
            Some(p)
        } else {
            None
        };
        if let Some(rel) = path {
            stat_count += 1;
            if stat_count > WORKSPACE_STAT_PATHS_MAX {
                break;
            }
            hasher.update(b"\0stat\0");
            hasher.update(rel.as_bytes());
            update_meta_hash(&mut hasher, &dir.join(rel));
        }
    }
    let hex = hex::encode(hasher.finalize());
    Revision::new(format!("ws_{}", &hex[..32])).ok()
}

fn dir_fingerprint(dir: &Path) -> Option<Revision> {
    let mut entries = Vec::new();
    collect_dir_entries(dir, Path::new(""), 0, &mut entries);
    entries.sort_unstable_by(|a, b| a.0.cmp(&b.0));
    let mut hasher = sha2::Sha256::new();
    hasher.update(b"dir-v1\0");
    for (rel, tag, len, mtime) in entries {
        hasher.update(rel.as_bytes());
        hasher.update([0, tag]);
        hasher.update(len.to_le_bytes());
        hasher.update(mtime.to_le_bytes());
    }
    let hex = hex::encode(hasher.finalize());
    Revision::new(format!("ws_{}", &hex[..32])).ok()
}

fn collect_dir_entries(
    base: &Path,
    rel: &Path,
    depth: usize,
    out: &mut Vec<(String, u8, u64, u128)>,
) {
    if depth > WORKSPACE_WALK_DEPTH_MAX || out.len() >= WORKSPACE_WALK_ENTRIES_MAX {
        return;
    }
    let Ok(rd) = std::fs::read_dir(base.join(rel)) else {
        return;
    };
    let mut names: Vec<std::ffi::OsString> = rd.flatten().map(|e| e.file_name()).collect();
    names.sort_unstable();
    for name in names {
        if out.len() >= WORKSPACE_WALK_ENTRIES_MAX {
            break;
        }
        let s = name.to_string_lossy();
        if s == ".git" || WALK_SKIPPED_DIRS.iter().any(|d| *d == s.as_ref()) {
            continue;
        }
        let child_rel = if rel.as_os_str().is_empty() {
            PathBuf::from(&name)
        } else {
            rel.join(&name)
        };
        let full = base.join(&child_rel);
        let Ok(meta) = std::fs::symlink_metadata(&full) else {
            continue;
        };
        let tag = if meta.is_dir() {
            1
        } else if meta.file_type().is_symlink() {
            2
        } else {
            0
        };
        let mtime = meta
            .modified()
            .ok()
            .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        out.push((
            child_rel.to_string_lossy().into_owned(),
            tag,
            meta.len(),
            mtime,
        ));
        if meta.is_dir() && !meta.file_type().is_symlink() {
            collect_dir_entries(base, &child_rel, depth + 1, out);
        }
    }
}

fn update_meta_hash(hasher: &mut sha2::Sha256, path: &Path) {
    match std::fs::symlink_metadata(path) {
        Ok(meta) => {
            hasher.update([1u8]);
            hasher.update(meta.len().to_le_bytes());
            let mtime = meta
                .modified()
                .ok()
                .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                .map(|d| d.as_nanos())
                .unwrap_or(0);
            hasher.update(mtime.to_le_bytes());
        }
        Err(_) => hasher.update([0u8]),
    }
}

#[cfg(test)]
mod tests {
    use super::{fingerprint, git_touches_worktrees};

    #[test]
    fn workspace_fingerprint_detects_git_and_non_git_mutations() {
        let dir = tempfile::tempdir().unwrap();
        let plain = dir.path().join("scratch");
        std::fs::create_dir_all(&plain).unwrap();
        std::fs::write(plain.join("a.txt"), "hello").unwrap();
        let f0 = fingerprint(&plain).expect("non-git fingerprint");
        assert_eq!(fingerprint(&plain), Some(f0.clone()));
        std::fs::write(plain.join("a.txt"), "hello world").unwrap();
        let f1 = fingerprint(&plain).expect("non-git fingerprint after edit");
        assert_ne!(f0, f1);

        let repo = dir.path().join("repo");
        std::fs::create_dir_all(&repo).unwrap();
        let run_git = |args: &[&str]| {
            let status = std::process::Command::new("git")
                .args(args)
                .current_dir(&repo)
                .env("GIT_AUTHOR_NAME", "test")
                .env("GIT_AUTHOR_EMAIL", "test@example.com")
                .env("GIT_COMMITTER_NAME", "test")
                .env("GIT_COMMITTER_EMAIL", "test@example.com")
                .status()
                .unwrap();
            assert!(status.success(), "git {args:?} failed");
        };
        run_git(&["init", "-q"]);
        std::fs::write(repo.join("src.rs"), "fn main() {}\n").unwrap();
        run_git(&["add", "src.rs"]);
        run_git(&["commit", "-q", "-m", "init"]);
        let g0 = fingerprint(&repo).expect("clean git fingerprint");
        assert_eq!(fingerprint(&repo), Some(g0.clone()));

        // Untracked file changes fingerprint
        std::fs::write(repo.join("note.md"), "draft 1\n").unwrap();
        let g1 = fingerprint(&repo).expect("untracked file fingerprint");
        assert_ne!(g0, g1);

        // Editing an already-dirty tracked file (same porcelain status) changes fingerprint
        std::fs::write(repo.join("src.rs"), "fn main() { println!(\"1\"); }\n").unwrap();
        let g2 = fingerprint(&repo).expect("dirty tracked fingerprint");
        assert_ne!(g1, g2);
        std::fs::write(repo.join("src.rs"), "fn main() { println!(\"12345\"); }\n").unwrap();
        let g3 = fingerprint(&repo).expect("re-edited tracked fingerprint");
        assert_ne!(g2, g3);

        // Committing changes HEAD -> new fingerprint
        run_git(&["add", "src.rs"]);
        run_git(&["commit", "-q", "-m", "update"]);
        let g4 = fingerprint(&repo).expect("post-commit fingerprint");
        assert_ne!(g3, g4);
    }

    #[test]
    fn git_touches_worktrees_matches_branch_and_non_list_worktree_subcommands() {
        for locked in [
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "steve/st-1",
                "/tmp/wt",
                "main",
            ][..],
            &["worktree", "remove", "--force", "/tmp/wt"],
            &["worktree", "prune"],
            &["worktree", "move", "/tmp/wt", "/tmp/wt2"],
            &["worktree", "repair"],
            &["worktree", "lock", "/tmp/wt"],
            &["worktree", "unlock", "/tmp/wt"],
            &[
                "-c",
                "protocol.version=2",
                "worktree",
                "add",
                "/tmp/wt",
                "main",
            ],
            &[
                "-C",
                "/tmp/repo",
                "-c",
                "gc.auto=0",
                "worktree",
                "add",
                "/tmp/wt",
            ],
            &["branch", "-d", "steve/st-1"],
            &["-C", "/tmp/repo", "branch", "-D", "steve/st-1"],
        ] {
            assert!(
                git_touches_worktrees(locked),
                "expected locked for {locked:?}"
            );
        }
        for unlocked in [
            &["worktree", "list"][..],
            &["worktree", "list", "--porcelain"],
            &["-C", "/tmp/repo", "worktree", "list"],
            &["fetch", "origin"],
            &["push", "origin", "HEAD"],
            &["rebase", "main"],
            &["commit", "-m", "worktree"],
            &["clone", "https://example.com/r.git", "/tmp/r"],
            &["-c"],
            &[],
        ] {
            assert!(
                !git_touches_worktrees(unlocked),
                "expected unlocked for {unlocked:?}"
            );
        }
    }
}
