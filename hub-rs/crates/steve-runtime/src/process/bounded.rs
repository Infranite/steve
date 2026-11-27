//! Unified bounded subprocess execution (sync and async), reusable `PATH` resolution, and safe
//! pre-reap process-group teardown.
//!
//! Invariants:
//! - Exit is observed via `waitid(P_PID, pid, ..., WEXITED | WNOWAIT)` so the zombie leader keeps
//!   pinning its PGID until pipe draining and any `-pid` `SIGKILL` finish; `waitpid` runs last, at
//!   most once.
//! - Stdin writes and stdout/stderr reads share one non-blocking `poll(2)` loop with zero reader
//!   threads, so a `setsid()`-escaped grandchild holding a pipe cannot leak OS threads.
//! - [`DetachedCommand`] carries [`EnvPolicy`] in the type (identical `env_clear` semantics on
//!   macOS `POSIX_SPAWN_SETSID` and Linux `setsid`), and [`resolve_program`] resolves bare/relative
//!   program names strictly against the child's effective `PATH`.

use std::ffi::{OsStr, OsString};
use std::fmt;
use std::io;
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::os::unix::ffi::OsStrExt;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::process::CommandExt;
#[cfg(target_os = "macos")]
use std::os::unix::process::ExitStatusExt;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use zeroize::Zeroizing;

/// Grace period after leader exit for pipes to reach EOF before killing `-pgid` (lets normal
/// buffered stdio flush in <10 ms while bounding recovery latency).
pub const PIPE_DRAIN_GRACE: Duration = Duration::from_millis(200);
/// Maximum wait to reap the direct child after `SIGKILL` (tolerates transient kernel delays
/// without hanging callers on uninterruptible D-state children).
pub const PROCESS_KILL_REAP_TIMEOUT: Duration = Duration::from_secs(5);
/// Maximum wait for pipe EOF after `SIGKILL` to `-pgid` (bounds `setsid()`-escaped grandchildren).
pub const PIPE_POST_KILL_TIMEOUT: Duration = Duration::from_secs(2);
/// Default per-stream capture ceiling (16 MiB); narrow callers specify tighter named limits.
pub const DEFAULT_MAX_OUTPUT_BYTES: usize = 16 * 1024 * 1024;
/// Maximum `max_output_bytes` permitted by [`run_bounded_zeroizing`]; pre-allocated upfront so
/// `Zeroizing<Vec<u8>>` can never reallocate and leave unzeroized copies on the heap.
pub const ZEROIZE_MAX_OUTPUT_BYTES: usize = 64 * 1024;
/// Idle `poll(2)` wakeup cadence for `waitid(WNOHANG)` and cancellation checks.
const POLL_INTERVAL: Duration = Duration::from_millis(10);
/// Stack buffer size per non-blocking `read(2)` iteration.
const READ_CHUNK_BYTES: usize = 4096;

/// Process-group / session isolation mode for `macos_spawn::SpawnSpec`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessIsolation {
    ProcessGroup,
    DetachedSession,
}

/// Explicit child environment policy shared identically on macOS and Linux.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum EnvPolicy {
    /// Inherit the parent environment and apply ordered overrides (`Some`) / removals (`None`).
    Inherit(Vec<(OsString, Option<OsString>)>),
    /// Clear the environment on every platform and pass only these exact `KEY=VALUE` entries.
    Clear(Vec<(OsString, OsString)>),
}

impl EnvPolicy {
    /// Effective `PATH` visible to the child under this environment policy.
    pub fn effective_path(&self) -> Option<OsString> {
        match self {
            Self::Clear(e) => e
                .iter()
                .rev()
                .find(|(k, _)| k.as_bytes() == b"PATH")
                .map(|(_, v)| v.clone()),
            Self::Inherit(d) => {
                effective_path(false, d.iter().map(|(k, v)| (k.as_os_str(), v.as_deref())))
            }
        }
    }
}

/// Compute the child's effective `PATH` from `env_cleared` and ordered environment overrides.
/// Reusable by callers that build a `std::process::Command` with `.env_clear()` or custom `PATH`
/// before invoking [`resolve_program`] to avoid macOS's `fork()` fallback.
pub fn effective_path<K: AsRef<OsStr>, V: AsRef<OsStr>, I>(
    env_cleared: bool,
    overrides: I,
) -> Option<OsString>
where
    I: IntoIterator<Item = (K, Option<V>)>,
{
    let found = overrides
        .into_iter()
        .filter(|(k, _)| k.as_ref().as_bytes() == b"PATH")
        .map(|(_, v)| v.map(|val| val.as_ref().to_os_string()))
        .last();
    match found {
        Some(explicit) => explicit,
        None if env_cleared => None,
        None => std::env::var_os("PATH"),
    }
}

/// Resolve `program` into an absolute executable path using ONLY `search_path` and optional `cwd`.
///
/// - Absolute `program`: returned unchanged.
/// - Relative `program` containing `'/'`: anchored against `cwd` (or current dir) to avoid macOS's
///   `Relative + cwd` `fork()` fallback and `posix_spawn` chdir ambiguity.
/// - Bare `program` (no `'/'`): searched strictly in `search_path`; `None`/empty returns `NotFound`
///   and NEVER falls back to the parent process's `PATH`.
pub fn resolve_program(
    program: &OsStr,
    search_path: Option<&OsStr>,
    cwd: Option<&Path>,
) -> io::Result<PathBuf> {
    if program.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            "empty program name",
        ));
    }
    let path = Path::new(program);
    if path.is_absolute() {
        return Ok(path.to_path_buf());
    }
    let anchor = || -> io::Result<PathBuf> {
        match cwd {
            Some(dir) if dir.is_absolute() => Ok(dir.to_path_buf()),
            Some(dir) => Ok(std::env::current_dir()?.join(dir)),
            None => std::env::current_dir(),
        }
    };
    if program.as_bytes().contains(&b'/') {
        return Ok(anchor()?.join(path));
    }
    let Some(search) = search_path.filter(|p| !p.is_empty()) else {
        return Err(io::Error::new(
            io::ErrorKind::NotFound,
            format!(
                "program `{}` not found (child PATH is empty)",
                program.to_string_lossy()
            ),
        ));
    };
    let mut saw_denied = false;
    for dir in std::env::split_paths(search) {
        let candidate = if dir.is_absolute() {
            dir.join(path)
        } else if dir.as_os_str().is_empty() {
            anchor()?.join(path)
        } else {
            anchor()?.join(dir).join(path)
        };
        if let Ok(meta) = std::fs::metadata(&candidate)
            && meta.is_file()
        {
            if meta.permissions().mode() & 0o111 != 0 {
                return Ok(candidate);
            }
            saw_denied = true;
        }
    }
    let kind = if saw_denied {
        io::ErrorKind::PermissionDenied
    } else {
        io::ErrorKind::NotFound
    };
    Err(io::Error::new(
        kind,
        format!(
            "program `{}` not found in child PATH",
            program.to_string_lossy()
        ),
    ))
}

/// Detached-session (`setsid`) subprocess specification carrying [`EnvPolicy`] in the type so
/// `env_clear` behaves identically on macOS (`POSIX_SPAWN_SETSID`, zero `fork()`) and Linux.
#[derive(Clone, Debug)]
pub struct DetachedCommand {
    pub program: OsString,
    pub args: Vec<OsString>,
    pub env: EnvPolicy,
    pub cwd: Option<PathBuf>,
}

impl DetachedCommand {
    pub fn new(program: impl Into<OsString>) -> Self {
        Self {
            program: program.into(),
            args: Vec::new(),
            env: EnvPolicy::Inherit(Vec::new()),
            cwd: None,
        }
    }
    pub fn arg(&mut self, arg: impl AsRef<OsStr>) -> &mut Self {
        self.args.push(arg.as_ref().to_os_string());
        self
    }
    pub fn args<I: IntoIterator<Item = S>, S: AsRef<OsStr>>(&mut self, args: I) -> &mut Self {
        self.args
            .extend(args.into_iter().map(|a| a.as_ref().to_os_string()));
        self
    }
    pub fn env_clear(&mut self) -> &mut Self {
        self.env = EnvPolicy::Clear(Vec::new());
        self
    }
    pub fn env(&mut self, key: impl AsRef<OsStr>, val: impl AsRef<OsStr>) -> &mut Self {
        let (k, v) = (key.as_ref().to_os_string(), val.as_ref().to_os_string());
        match &mut self.env {
            EnvPolicy::Clear(e) => {
                e.retain(|(existing, _)| existing != &k);
                e.push((k, v));
            }
            EnvPolicy::Inherit(d) => d.push((k, Some(v))),
        }
        self
    }
    pub fn env_remove(&mut self, key: impl AsRef<OsStr>) -> &mut Self {
        let k = key.as_ref().to_os_string();
        match &mut self.env {
            EnvPolicy::Clear(e) => e.retain(|(existing, _)| existing != &k),
            EnvPolicy::Inherit(d) => d.push((k, None)),
        }
        self
    }
    pub fn current_dir(&mut self, dir: impl Into<PathBuf>) -> &mut Self {
        self.cwd = Some(dir.into());
        self
    }
}

/// Standard input configuration for bounded execution.
#[derive(Clone, Copy, Debug, Default)]
pub enum BoundedStdin<'a> {
    #[default]
    Null,
    Bytes(&'a [u8]),
    /// Preserve the stdin already configured on `std::process::Command` (unsupported on [`DetachedCommand`]).
    Preserve,
}

/// Policy when a descendant keeps stdout or stderr open past [`PIPE_DRAIN_GRACE`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HeldPipePolicy {
    #[default]
    RecoverAfterGroupKill,
    FailAfterGroupKill,
}

/// Execution contract for [`run_bounded`], [`run_bounded_zeroizing`], [`run_bounded_async`],
/// [`run_detached`], and [`run_detached_async`].
#[derive(Clone, Copy, Debug)]
pub struct BoundedOptions<'a> {
    /// Bounded low-cardinality label for `steve_process_*` metrics.
    pub operation: &'static str,
    pub timeout: Duration,
    pub stdin: BoundedStdin<'a>,
    /// When `true`, `BrokenPipe`/`WriteZero` (or early exit) lets the child's exit code surface.
    pub ignore_stdin_broken_pipe: bool,
    pub capture_output: bool,
    pub max_output_bytes: usize,
    /// Kill `-pgid` before reaping even when pipes closed cleanly (e.g. shell probes).
    pub kill_group_after_exit: bool,
    pub held_pipe_policy: HeldPipePolicy,
}

impl<'a> BoundedOptions<'a> {
    pub fn capture(operation: &'static str, timeout: Duration) -> Self {
        Self {
            operation,
            timeout,
            stdin: BoundedStdin::Null,
            ignore_stdin_broken_pipe: false,
            capture_output: true,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
            kill_group_after_exit: false,
            held_pipe_policy: HeldPipePolicy::RecoverAfterGroupKill,
        }
    }
    pub fn silent(operation: &'static str, timeout: Duration) -> Self {
        Self {
            capture_output: false,
            ..Self::capture(operation, timeout)
        }
    }
    fn with_stdin<'b>(self, stdin: BoundedStdin<'b>) -> BoundedOptions<'b> {
        BoundedOptions { stdin, ..self }
    }
}

/// Output of [`run_bounded_zeroizing`]: pre-allocated `stdout` zeroized on drop without reallocation.
pub struct ZeroizingOutput {
    pub status: ExitStatus,
    pub stdout: Zeroizing<Vec<u8>>,
    pub stderr: Vec<u8>,
}

impl fmt::Debug for ZeroizingOutput {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ZeroizingOutput")
            .field("status", &self.status)
            .field("stdout_len", &self.stdout.len())
            .field("stderr_len", &self.stderr.len())
            .finish()
    }
}

/// Strongly typed error returned at every bounded subprocess boundary.
#[derive(Debug)]
pub enum CommandError {
    Spawn {
        program: String,
        source: io::Error,
    },
    StdinConfigure {
        program: String,
        source: io::Error,
    },
    StdinWrite {
        program: String,
        source: io::Error,
    },
    TimedOut {
        program: String,
        timeout: Duration,
    },
    Cancelled {
        program: String,
    },
    Wait {
        program: String,
        source: io::Error,
    },
    PipeRead {
        program: String,
        stream: &'static str,
        source: io::Error,
    },
    PipeDrainTimedOut {
        program: String,
        stream: &'static str,
        grace: Duration,
    },
    OutputLimitExceeded {
        program: String,
        stream: &'static str,
        limit: usize,
    },
}

impl CommandError {
    fn spawn(p: &str, source: io::Error) -> Self {
        Self::Spawn {
            program: p.to_owned(),
            source,
        }
    }
    fn stdin_cfg(p: &str, source: io::Error) -> Self {
        Self::StdinConfigure {
            program: p.to_owned(),
            source,
        }
    }
    fn stdin_write(p: &str, source: io::Error) -> Self {
        Self::StdinWrite {
            program: p.to_owned(),
            source,
        }
    }
    fn wait(p: &str, source: io::Error) -> Self {
        Self::Wait {
            program: p.to_owned(),
            source,
        }
    }
    fn pipe_read(p: &str, stream: &'static str, source: io::Error) -> Self {
        Self::PipeRead {
            program: p.to_owned(),
            stream,
            source,
        }
    }
    fn metric_result(&self) -> &'static str {
        match self {
            Self::TimedOut { .. } => "timeout",
            Self::Cancelled { .. } => "cancelled",
            Self::PipeDrainTimedOut { .. } => "pipe_drain_timeout",
            Self::OutputLimitExceeded { .. } => "output_limit",
            _ => "error",
        }
    }
}

impl fmt::Display for CommandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn { program, source } => write!(f, "failed to start {program}: {source}"),
            Self::StdinConfigure { program, source } => {
                write!(f, "failed to configure stdin for {program}: {source}")
            }
            Self::StdinWrite { program, source } => {
                write!(f, "failed to write stdin to {program}: {source}")
            }
            Self::TimedOut { program, timeout } => {
                write!(f, "{program} timed out after {:.1}s", timeout.as_secs_f64())
            }
            Self::Cancelled { program } => write!(f, "{program} cancelled"),
            Self::Wait { program, source } => write!(f, "failed waiting for {program}: {source}"),
            Self::PipeRead {
                program,
                stream,
                source,
            } => write!(f, "failed reading {stream} from {program}: {source}"),
            Self::PipeDrainTimedOut {
                program,
                stream,
                grace,
            } => {
                write!(
                    f,
                    "{program} {stream} pipe did not close within {}ms after exit",
                    grace.as_millis()
                )
            }
            Self::OutputLimitExceeded {
                program,
                stream,
                limit,
            } => write!(f, "{program} {stream} exceeded {limit} bytes"),
        }
    }
}

impl std::error::Error for CommandError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Spawn { source, .. }
            | Self::StdinConfigure { source, .. }
            | Self::StdinWrite { source, .. }
            | Self::Wait { source, .. }
            | Self::PipeRead { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// Target of the initial graceful `SIGTERM` in [`stop_child_group_sync`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TermTarget {
    Leader,
    Group,
}

fn remaining_until(deadline: Instant) -> Option<Duration> {
    deadline
        .checked_duration_since(Instant::now())
        .filter(|d| !d.is_zero())
}

fn try_wait_std(child: &mut Child) -> io::Result<Option<ExitStatus>> {
    loop {
        match child.try_wait() {
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            other => return other,
        }
    }
}

fn reap_std_bounded(
    child: &mut Child,
    reaped: &mut bool,
    timeout: Duration,
) -> io::Result<Option<ExitStatus>> {
    let deadline = Instant::now() + timeout;
    loop {
        match try_wait_std(child) {
            Ok(Some(status)) => {
                *reaped = true;
                return Ok(Some(status));
            }
            Ok(None) => match remaining_until(deadline) {
                Some(rem) => std::thread::sleep(POLL_INTERVAL.min(rem)),
                None => return Ok(None),
            },
            Err(e) if e.raw_os_error() == Some(libc::ECHILD) => {
                *reaped = true;
                return Ok(None);
            }
            Err(e) => return Err(e),
        }
    }
}

/// Observe whether direct child `pid` has exited WITHOUT reaping it, so its zombie entry keeps
/// pinning its PID and process group (`pgid == pid`) against PID reuse.
pub fn exited_unreaped(pid: u32, nonblocking: bool) -> io::Result<bool> {
    let options = libc::WEXITED | libc::WNOWAIT | if nonblocking { libc::WNOHANG } else { 0 };
    loop {
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        if unsafe { libc::waitid(libc::P_PID, pid as libc::id_t, &mut info, options) } == 0 {
            #[cfg(target_os = "macos")]
            let observed = info.si_pid;
            #[cfg(not(target_os = "macos"))]
            let observed = unsafe { info.si_pid() };
            return Ok(observed != 0);
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
}

/// Gracefully stop an isolated direct child (`pgid == pid`): `SIGTERM`, wait up to `grace` via
/// `waitid(WNOWAIT)`, send `SIGKILL` to `-pid` while unreaped, then reap within `kill_reap_timeout`.
pub fn stop_child_group_sync(
    child: &mut Child,
    reaped: &mut bool,
    term_target: TermTarget,
    grace: Duration,
    kill_reap_timeout: Duration,
) -> io::Result<Option<ExitStatus>> {
    if *reaped {
        return Ok(None);
    }
    let pid = child.id();
    let pid_i32 = pid as libc::pid_t;
    let already_exited = match exited_unreaped(pid, true) {
        Ok(exited) => exited,
        Err(e) if e.raw_os_error() == Some(libc::ECHILD) => {
            *reaped = true;
            return Ok(None);
        }
        Err(e) => return Err(e),
    };
    if !already_exited && pid_i32 > 1 {
        let target = if term_target == TermTarget::Leader {
            pid_i32
        } else {
            -pid_i32
        };
        if unsafe { libc::kill(target, libc::SIGTERM) } != 0 {
            let err = io::Error::last_os_error();
            if err.raw_os_error() != Some(libc::ESRCH) {
                return Err(err);
            }
        }
        let deadline = Instant::now() + grace;
        loop {
            match exited_unreaped(pid, true) {
                Ok(true) => break,
                Ok(false) => match remaining_until(deadline) {
                    Some(rem) => std::thread::sleep(POLL_INTERVAL.min(rem)),
                    None => {
                        unsafe {
                            libc::kill(-pid_i32, libc::SIGKILL);
                            libc::kill(pid_i32, libc::SIGKILL);
                        }
                        break;
                    }
                },
                Err(e) if e.raw_os_error() == Some(libc::ECHILD) => {
                    *reaped = true;
                    return Ok(None);
                }
                Err(e) => return Err(e),
            }
        }
    }
    if pid_i32 > 1 {
        unsafe { libc::kill(-pid_i32, libc::SIGKILL) };
    }
    reap_std_bounded(child, reaped, kill_reap_timeout)
}

enum SpawnedChild {
    Std(Child),
    #[cfg(target_os = "macos")]
    RawPid(libc::pid_t),
}

struct OwnedChild {
    inner: SpawnedChild,
    pid: u32,
    reaped: bool,
    teardown_attempted: bool,
}

impl OwnedChild {
    fn from_std(child: Child) -> Self {
        let pid = child.id();
        Self {
            inner: SpawnedChild::Std(child),
            pid,
            reaped: false,
            teardown_attempted: false,
        }
    }
    #[cfg(target_os = "macos")]
    fn from_raw_pid(pid: libc::pid_t) -> Self {
        Self {
            inner: SpawnedChild::RawPid(pid),
            pid: pid as u32,
            reaped: false,
            teardown_attempted: false,
        }
    }
    fn exited_unreaped(&self) -> io::Result<bool> {
        if self.reaped {
            Ok(true)
        } else {
            exited_unreaped(self.pid, true)
        }
    }
    fn kill_group(&self, include_leader: bool) {
        if !self.reaped && self.pid > 1 {
            let pid = self.pid as libc::pid_t;
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
                if include_leader {
                    libc::kill(pid, libc::SIGKILL);
                }
            }
        }
    }
    fn reap_bounded(&mut self, timeout: Duration) -> io::Result<Option<ExitStatus>> {
        if self.reaped {
            return Ok(None);
        }
        let res = match &mut self.inner {
            SpawnedChild::Std(child) => reap_std_bounded(child, &mut self.reaped, timeout),
            #[cfg(target_os = "macos")]
            SpawnedChild::RawPid(pid) => {
                let deadline = Instant::now() + timeout;
                loop {
                    let mut status: libc::c_int = 0;
                    let rc = unsafe { libc::waitpid(*pid, &mut status, libc::WNOHANG) };
                    if rc == *pid {
                        self.reaped = true;
                        break Ok(Some(ExitStatus::from_raw(status)));
                    }
                    if rc == 0 {
                        match remaining_until(deadline) {
                            Some(rem) => std::thread::sleep(POLL_INTERVAL.min(rem)),
                            None => break Ok(None),
                        }
                    } else {
                        let err = io::Error::last_os_error();
                        if err.kind() != io::ErrorKind::Interrupted {
                            break Err(err);
                        }
                    }
                }
            }
        };
        if !matches!(res, Ok(Some(_))) {
            self.teardown_attempted = true;
        }
        res
    }
    /// Reap at most once (optionally sending `SIGKILL` first); prevents `Drop` from waiting a
    /// second `PROCESS_KILL_REAP_TIMEOUT` (5s) on uninterruptible D-state children.
    fn teardown_once(&mut self, send_kill: bool) {
        if self.reaped || self.teardown_attempted {
            return;
        }
        self.teardown_attempted = true;
        if send_kill {
            self.kill_group(true);
        }
        let _ = self.reap_bounded(PROCESS_KILL_REAP_TIMEOUT);
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        self.teardown_once(true);
    }
}

fn set_nonblocking(fd: RawFd) -> io::Result<()> {
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn record_metrics(operation: Option<&'static str>, result: &'static str, started: Instant) {
    if let Some(op) = operation {
        metrics::counter!("steve_process_runs_total", "operation" => op, "result" => result)
            .increment(1);
        metrics::histogram!("steve_process_duration_seconds", "operation" => op)
            .record(started.elapsed().as_secs_f64());
    }
}

fn record_group_kill(operation: Option<&'static str>, reason: &'static str) {
    if let Some(op) = operation {
        metrics::counter!("steve_process_group_kills_total", "operation" => op, "reason" => reason)
            .increment(1);
    }
}

/// Returns `Ok(0)` on `EINTR` so outer loops immediately re-check deadlines and cancellation.
fn poll_once(fds: &mut [libc::pollfd], wait: Duration) -> io::Result<libc::c_int> {
    let ms = wait.as_millis().clamp(1, i32::MAX as u128) as libc::c_int;
    let rc = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, ms) };
    if rc >= 0 {
        return Ok(rc);
    }
    let err = io::Error::last_os_error();
    if err.kind() == io::ErrorKind::Interrupted {
        Ok(0)
    } else {
        Err(err)
    }
}

struct StdinFeeder<'a> {
    fd: Option<OwnedFd>,
    bytes: &'a [u8],
    offset: usize,
    ignore_broken_pipe: bool,
}

struct DrainState {
    stdout_fd: Option<OwnedFd>,
    stderr_fd: Option<OwnedFd>,
    stdout_buf: Zeroizing<Vec<u8>>,
    stderr_buf: Vec<u8>,
    max_output_bytes: usize,
}

impl DrainState {
    fn new(
        stdout_fd: Option<OwnedFd>,
        stderr_fd: Option<OwnedFd>,
        max_output_bytes: usize,
        zeroize: bool,
    ) -> Self {
        let cap = if zeroize { max_output_bytes } else { 0 };
        Self {
            stdout_fd,
            stderr_fd,
            stdout_buf: Zeroizing::new(Vec::with_capacity(cap)),
            stderr_buf: Vec::new(),
            max_output_bytes,
        }
    }
    fn any_open(&self) -> bool {
        self.stdout_fd.is_some() || self.stderr_fd.is_some()
    }
    fn open_stream_name(&self) -> &'static str {
        if self.stdout_fd.is_some() {
            "stdout"
        } else {
            "stderr"
        }
    }
    fn close_all(&mut self) {
        self.stdout_fd = None;
        self.stderr_fd = None;
    }

    fn read_slot(
        slot: &mut Option<OwnedFd>,
        buf: &mut Vec<u8>,
        program: &str,
        stream: &'static str,
        revents: libc::c_short,
        max_bytes: usize,
    ) -> Result<(), CommandError> {
        if revents & libc::POLLNVAL != 0 {
            *slot = None;
            return Ok(());
        }
        let Some(fd) = slot.as_ref() else {
            return Ok(());
        };
        let mut chunk = Zeroizing::new([0u8; READ_CHUNK_BYTES]);
        loop {
            let n = unsafe { libc::read(fd.as_raw_fd(), chunk.as_mut_ptr().cast(), chunk.len()) };
            if n == 0 {
                *slot = None;
                return Ok(());
            }
            if n > 0 {
                let count = n as usize;
                if buf.len().saturating_add(count) > max_bytes {
                    return Err(CommandError::OutputLimitExceeded {
                        program: program.to_owned(),
                        stream,
                        limit: max_bytes,
                    });
                }
                buf.extend_from_slice(&chunk[..count]);
                continue;
            }
            let err = io::Error::last_os_error();
            match err.kind() {
                io::ErrorKind::Interrupted => continue,
                io::ErrorKind::WouldBlock => return Ok(()),
                _ => return Err(CommandError::pipe_read(program, stream, err)),
            }
        }
    }

    fn write_stdin(feeder: &mut StdinFeeder<'_>, program: &str) -> Result<(), CommandError> {
        let Some(fd) = feeder.fd.as_ref() else {
            return Ok(());
        };
        while feeder.offset < feeder.bytes.len() {
            let slice = &feeder.bytes[feeder.offset..];
            let n = unsafe { libc::write(fd.as_raw_fd(), slice.as_ptr().cast(), slice.len()) };
            if n > 0 {
                feeder.offset += n as usize;
                continue;
            }
            let err = if n == 0 {
                io::Error::new(io::ErrorKind::WriteZero, "child closed stdin")
            } else {
                io::Error::last_os_error()
            };
            match err.kind() {
                io::ErrorKind::Interrupted => continue,
                io::ErrorKind::WouldBlock => return Ok(()),
                io::ErrorKind::BrokenPipe | io::ErrorKind::WriteZero => {
                    feeder.fd = None;
                    return if feeder.ignore_broken_pipe {
                        Ok(())
                    } else {
                        Err(CommandError::stdin_write(program, err))
                    };
                }
                _ => return Err(CommandError::stdin_write(program, err)),
            }
        }
        feeder.fd = None;
        Ok(())
    }

    /// Unified non-blocking `poll(2)` step across optional stdin + stdout + stderr.
    fn poll_step(
        &mut self,
        program: &str,
        mut stdin: Option<&mut StdinFeeder<'_>>,
        wait: Duration,
    ) -> Result<(), CommandError> {
        let mut pfds = [libc::pollfd {
            fd: -1,
            events: 0,
            revents: 0,
        }; 3];
        let mut kinds = [0u8; 3];
        let mut count = 0usize;
        let mut push = |fd: RawFd, events: libc::c_short, kind: u8| {
            pfds[count] = libc::pollfd {
                fd,
                events,
                revents: 0,
            };
            kinds[count] = kind;
            count += 1;
        };
        if let Some(fd) = stdin.as_deref().and_then(|s| s.fd.as_ref()) {
            push(fd.as_raw_fd(), libc::POLLOUT, 0);
        }
        if let Some(fd) = &self.stdout_fd {
            push(fd.as_raw_fd(), libc::POLLIN, 1);
        }
        if let Some(fd) = &self.stderr_fd {
            push(fd.as_raw_fd(), libc::POLLIN, 2);
        }
        if count == 0 {
            std::thread::sleep(wait);
            return Ok(());
        }
        if let Err(err) = poll_once(&mut pfds[..count], wait) {
            return Err(if stdin.is_some() {
                CommandError::wait(program, err)
            } else {
                CommandError::pipe_read(program, self.open_stream_name(), err)
            });
        }
        for i in 0..count {
            let revents = pfds[i].revents;
            if revents == 0 {
                continue;
            }
            match kinds[i] {
                0 => {
                    if let Some(feeder) = stdin.as_deref_mut() {
                        Self::write_stdin(feeder, program)?;
                    }
                }
                1 => Self::read_slot(
                    &mut self.stdout_fd,
                    &mut self.stdout_buf,
                    program,
                    "stdout",
                    revents,
                    self.max_output_bytes,
                )?,
                _ => Self::read_slot(
                    &mut self.stderr_fd,
                    &mut self.stderr_buf,
                    program,
                    "stderr",
                    revents,
                    self.max_output_bytes,
                )?,
            }
        }
        Ok(())
    }

    /// Drain until EOF (`Ok(true)`), deadline (`Ok(false)`), or error/cancellation (`Err`).
    fn drain_until(
        &mut self,
        program: &str,
        deadline: Instant,
        cancelled: Option<&AtomicBool>,
    ) -> Result<bool, CommandError> {
        while self.any_open() {
            if cancelled.is_some_and(|c| c.load(Ordering::Relaxed)) {
                return Err(CommandError::Cancelled {
                    program: program.to_owned(),
                });
            }
            let Some(remaining) = remaining_until(deadline) else {
                return Ok(false);
            };
            self.poll_step(program, None, POLL_INTERVAL.min(remaining))?;
        }
        Ok(true)
    }
}

struct SpawnedFds {
    child: OwnedChild,
    stdin: Option<OwnedFd>,
    stdout: Option<OwnedFd>,
    stderr: Option<OwnedFd>,
}

fn build_resolved_std_command<'a, A, E>(
    resolved: &Path,
    args: A,
    cwd: Option<&Path>,
    env_clear: bool,
    envs: E,
) -> Command
where
    A: IntoIterator<Item = &'a OsStr>,
    E: IntoIterator<Item = (&'a OsStr, Option<&'a OsStr>)>,
{
    let mut cmd = Command::new(resolved);
    cmd.args(args);
    if let Some(dir) = cwd {
        cmd.current_dir(dir);
    }
    if env_clear {
        cmd.env_clear();
    }
    for (k, v) in envs {
        match v {
            Some(val) => {
                cmd.env(k, val);
            }
            None => {
                cmd.env_remove(k);
            }
        }
    }
    cmd
}

fn spawn_configured_std(
    command: &mut Command,
    program: &str,
    opts: BoundedOptions<'_>,
) -> Result<SpawnedFds, CommandError> {
    match opts.stdin {
        BoundedStdin::Null => {
            command.stdin(Stdio::null());
        }
        BoundedStdin::Bytes(_) => {
            command.stdin(Stdio::piped());
        }
        BoundedStdin::Preserve => {}
    }
    if opts.capture_output {
        command.stdout(Stdio::piped()).stderr(Stdio::piped());
    } else {
        command.stdout(Stdio::null()).stderr(Stdio::null());
    }
    let mut child = command
        .spawn()
        .map_err(|e| CommandError::spawn(program, e))?;
    let stdin = matches!(opts.stdin, BoundedStdin::Bytes(_))
        .then(|| child.stdin.take().map(OwnedFd::from))
        .flatten();
    let (stdout, stderr) = if opts.capture_output {
        (
            child.stdout.take().map(OwnedFd::from),
            child.stderr.take().map(OwnedFd::from),
        )
    } else {
        (None, None)
    };
    Ok(SpawnedFds {
        child: OwnedChild::from_std(child),
        stdin,
        stdout,
        stderr,
    })
}

/// Spawn `command` in its own process group (`pgid == pid`). When `program` has no `'/'` and
/// `command` overrides/removes `PATH` in `get_envs()`, resolves it against that `PATH` first so
/// macOS `std` does not fall back to `fork()`. Callers invoking `Command::env_clear()` must still
/// supply an absolute program path (e.g. via [`resolve_program`]).
fn spawn_std_process_group(
    command: &mut Command,
    program: &str,
    opts: BoundedOptions<'_>,
) -> Result<SpawnedFds, CommandError> {
    let raw = command.get_program();
    let mut rebuilt = None;
    if !raw.as_bytes().contains(&b'/')
        && let Some((_, custom_path)) = command
            .get_envs()
            .filter(|(k, _)| k.as_bytes() == b"PATH")
            .last()
    {
        let resolved = resolve_program(raw, custom_path, command.get_current_dir())
            .map_err(|e| CommandError::spawn(program, e))?;
        if matches!(opts.stdin, BoundedStdin::Preserve) {
            return Err(CommandError::stdin_cfg(
                program,
                io::Error::new(
                    io::ErrorKind::Unsupported,
                    "resolve program before combining custom PATH with BoundedStdin::Preserve",
                ),
            ));
        }
        rebuilt = Some(build_resolved_std_command(
            &resolved,
            command.get_args(),
            command.get_current_dir(),
            false,
            command.get_envs(),
        ));
    }
    let target = rebuilt.as_mut().unwrap_or(command);
    target.process_group(0);
    spawn_configured_std(target, program, opts)
}

fn spawn_detached(
    spec: &DetachedCommand,
    program: &str,
    opts: BoundedOptions<'_>,
) -> Result<SpawnedFds, CommandError> {
    if matches!(opts.stdin, BoundedStdin::Preserve) {
        return Err(CommandError::stdin_cfg(
            program,
            io::Error::new(
                io::ErrorKind::Unsupported,
                "BoundedStdin::Preserve is not supported on DetachedCommand",
            ),
        ));
    }
    #[cfg(target_os = "macos")]
    {
        use super::macos_spawn::{self, EnvSpec, SpawnSpec, StdioSpec};
        let args: Vec<&OsStr> = spec.args.iter().map(OsString::as_os_str).collect();
        let env = match &spec.env {
            EnvPolicy::Clear(e) => EnvSpec::Clear(e),
            EnvPolicy::Inherit(d) => EnvSpec::InheritWithDeltas(d),
        };
        let stdin = if matches!(opts.stdin, BoundedStdin::Bytes(_)) {
            StdioSpec::Pipe
        } else {
            StdioSpec::Null
        };
        let io = if opts.capture_output {
            StdioSpec::Pipe
        } else {
            StdioSpec::Null
        };
        let spawned = macos_spawn::spawn(SpawnSpec {
            program: &spec.program,
            args: &args,
            env,
            cwd: spec.cwd.as_deref(),
            isolation: ProcessIsolation::DetachedSession,
            stdin,
            stdout: io,
            stderr: io,
            extra_inherit_fds: &[],
        })
        .map_err(|e| CommandError::spawn(program, e))?;
        Ok(SpawnedFds {
            child: OwnedChild::from_raw_pid(spawned.pid),
            stdin: spawned.stdin,
            stdout: spawned.stdout,
            stderr: spawned.stderr,
        })
    }
    #[cfg(not(target_os = "macos"))]
    {
        let resolved = resolve_program(
            &spec.program,
            spec.env.effective_path().as_deref(),
            spec.cwd.as_deref(),
        )
        .map_err(|e| CommandError::spawn(program, e))?;
        let mut command = match &spec.env {
            EnvPolicy::Clear(e) => build_resolved_std_command(
                &resolved,
                spec.args.iter().map(OsString::as_os_str),
                spec.cwd.as_deref(),
                true,
                e.iter().map(|(k, v)| (k.as_os_str(), Some(v.as_os_str()))),
            ),
            EnvPolicy::Inherit(d) => build_resolved_std_command(
                &resolved,
                spec.args.iter().map(OsString::as_os_str),
                spec.cwd.as_deref(),
                false,
                d.iter().map(|(k, v)| (k.as_os_str(), v.as_deref())),
            ),
        };
        // SAFETY: `libc::setsid` is async-signal-safe per POSIX.
        unsafe {
            command.pre_exec(|| {
                if libc::setsid() == -1 {
                    Err(io::Error::last_os_error())
                } else {
                    Ok(())
                }
            });
        }
        spawn_configured_std(&mut command, program, opts)
    }
}

/// Single shared `poll(2)` + `waitid(WNOWAIT)` state machine serving production and testkit.
#[allow(clippy::too_many_arguments)]
fn drive_spawned(
    fds: SpawnedFds,
    program: &str,
    started: Instant,
    opts: BoundedOptions<'_>,
    emit_metrics: bool,
    zeroize_stdout: bool,
    drain_on_timeout: bool,
    cancelled: Option<&AtomicBool>,
    on_before_reap: &mut dyn FnMut(),
) -> Result<ZeroizingOutput, (CommandError, Vec<u8>, Vec<u8>)> {
    let op = emit_metrics.then_some(opts.operation);
    let deadline = started + opts.timeout;
    let stdin_bytes = match opts.stdin {
        BoundedStdin::Bytes(b) => b,
        _ => &[],
    };
    let mut child = fds.child;
    let mut feeder = StdinFeeder {
        fd: (!stdin_bytes.is_empty()).then_some(fds.stdin).flatten(),
        bytes: stdin_bytes,
        offset: 0,
        ignore_broken_pipe: opts.ignore_stdin_broken_pipe,
    };
    let mut drain = DrainState::new(
        fds.stdout,
        fds.stderr,
        opts.max_output_bytes,
        zeroize_stdout,
    );
    let mut group_already_killed = false;

    let run = (|| -> Result<ExitStatus, CommandError> {
        if let Some(fd) = &feeder.fd {
            set_nonblocking(fd.as_raw_fd()).map_err(|e| CommandError::stdin_cfg(program, e))?;
        }
        for (stream, fd) in [("stdout", &drain.stdout_fd), ("stderr", &drain.stderr_fd)] {
            if let Some(fd) = fd {
                set_nonblocking(fd.as_raw_fd())
                    .map_err(|e| CommandError::pipe_read(program, stream, e))?;
            }
        }
        loop {
            if cancelled.is_some_and(|c| c.load(Ordering::Relaxed)) {
                return Err(CommandError::Cancelled {
                    program: program.to_owned(),
                });
            }
            if child
                .exited_unreaped()
                .map_err(|e| CommandError::wait(program, e))?
            {
                break;
            }
            let Some(remaining) = remaining_until(deadline) else {
                return Err(CommandError::TimedOut {
                    program: program.to_owned(),
                    timeout: opts.timeout,
                });
            };
            drain.poll_step(program, Some(&mut feeder), POLL_INTERVAL.min(remaining))?;
        }
        if feeder.fd.take().is_some() && !opts.ignore_stdin_broken_pipe {
            return Err(CommandError::stdin_write(
                program,
                io::Error::new(
                    io::ErrorKind::BrokenPipe,
                    "child exited before consuming stdin",
                ),
            ));
        }

        drain.drain_until(program, Instant::now() + PIPE_DRAIN_GRACE, cancelled)?;
        on_before_reap();
        let pipes_held = drain.any_open();
        if pipes_held || opts.kill_group_after_exit {
            child.kill_group(false);
            group_already_killed = true;
            record_group_kill(
                op,
                if pipes_held {
                    "pipe_drain"
                } else {
                    "post_exit"
                },
            );
        }
        if pipes_held {
            let held_stream = drain.open_stream_name();
            let closed =
                drain.drain_until(program, Instant::now() + PIPE_POST_KILL_TIMEOUT, cancelled)?;
            if !closed || opts.held_pipe_policy == HeldPipePolicy::FailAfterGroupKill {
                let stream = if closed {
                    held_stream
                } else {
                    drain.open_stream_name()
                };
                return Err(CommandError::PipeDrainTimedOut {
                    program: program.to_owned(),
                    stream,
                    grace: PIPE_DRAIN_GRACE,
                });
            }
        }

        child
            .reap_bounded(PROCESS_KILL_REAP_TIMEOUT)
            .map_err(|e| CommandError::wait(program, e))?
            .ok_or_else(|| {
                CommandError::wait(
                    program,
                    io::Error::new(
                        io::ErrorKind::TimedOut,
                        "exited child could not be reaped within PROCESS_KILL_REAP_TIMEOUT",
                    ),
                )
            })
    })();

    match run {
        Ok(status) => {
            record_metrics(op, if status.success() { "ok" } else { "nonzero" }, started);
            Ok(ZeroizingOutput {
                status,
                stdout: drain.stdout_buf,
                stderr: drain.stderr_buf,
            })
        }
        Err(err) => {
            let result_label = err.metric_result();
            feeder.fd = None;
            on_before_reap();
            if !group_already_killed {
                child.kill_group(true);
                record_group_kill(op, result_label);
            }
            if drain_on_timeout && matches!(err, CommandError::TimedOut { .. }) {
                let _ = drain.drain_until(program, Instant::now() + PIPE_DRAIN_GRACE, None);
            }
            drain.close_all();
            child.teardown_once(false);
            record_metrics(op, result_label, started);
            Err((
                err,
                std::mem::take(&mut *drain.stdout_buf),
                drain.stderr_buf,
            ))
        }
    }
}

fn execute_bounded(
    program: &str,
    opts: BoundedOptions<'_>,
    zeroize_stdout: bool,
    cancelled: Option<&AtomicBool>,
    spawn: impl FnOnce() -> Result<SpawnedFds, CommandError>,
) -> Result<ZeroizingOutput, CommandError> {
    let started = Instant::now();
    if cancelled.is_some_and(|c| c.load(Ordering::Relaxed)) {
        record_metrics(Some(opts.operation), "cancelled", started);
        return Err(CommandError::Cancelled {
            program: program.to_owned(),
        });
    }
    if zeroize_stdout && opts.max_output_bytes > ZEROIZE_MAX_OUTPUT_BYTES {
        record_metrics(Some(opts.operation), "error", started);
        return Err(CommandError::pipe_read(
            program,
            "stdout",
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "run_bounded_zeroizing requires max_output_bytes <= {ZEROIZE_MAX_OUTPUT_BYTES}"
                ),
            ),
        ));
    }
    let fds = spawn().inspect_err(|_| record_metrics(Some(opts.operation), "error", started))?;
    let mut noop = || {};
    drive_spawned(
        fds,
        program,
        started,
        opts,
        true,
        zeroize_stdout,
        false,
        cancelled,
        &mut noop,
    )
    .map_err(|(err, _, _)| err)
}

fn into_plain_output(mut out: ZeroizingOutput) -> Output {
    Output {
        status: out.status,
        stdout: std::mem::take(&mut *out.stdout),
        stderr: out.stderr,
    }
}

/// Run `command` synchronously in an isolated process group (`pgid == pid`).
pub fn run_bounded(
    command: &mut Command,
    program: &str,
    opts: BoundedOptions<'_>,
) -> Result<Output, CommandError> {
    execute_bounded(program, opts, false, None, || {
        spawn_std_process_group(command, program, opts)
    })
    .map(into_plain_output)
}

/// Like [`run_bounded`], returning pre-allocated `Zeroizing<Vec<u8>>` stdout (`<= ZEROIZE_MAX_OUTPUT_BYTES`).
pub fn run_bounded_zeroizing(
    command: &mut Command,
    program: &str,
    opts: BoundedOptions<'_>,
) -> Result<ZeroizingOutput, CommandError> {
    execute_bounded(program, opts, true, None, || {
        spawn_std_process_group(command, program, opts)
    })
}

/// Run `command` synchronously in a detached session (`POSIX_SPAWN_SETSID` on macOS, `setsid` on Linux).
pub fn run_detached(
    command: &DetachedCommand,
    program: &str,
    opts: BoundedOptions<'_>,
) -> Result<Output, CommandError> {
    execute_bounded(program, opts, false, None, || {
        spawn_detached(command, program, opts)
    })
    .map(into_plain_output)
}

struct CancelOnDrop(Arc<AtomicBool>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

async fn run_blocking_cancellable<F>(
    program: String,
    opts: BoundedOptions<'_>,
    f: F,
) -> Result<Output, CommandError>
where
    F: FnOnce(&str, BoundedOptions<'_>, &AtomicBool) -> Result<ZeroizingOutput, CommandError>
        + Send
        + 'static,
{
    let owned_stdin = match opts.stdin {
        BoundedStdin::Bytes(bytes) => Some(Zeroizing::new(bytes.to_vec())),
        BoundedStdin::Null | BoundedStdin::Preserve => None,
    };
    let preserve = matches!(opts.stdin, BoundedStdin::Preserve);
    let base_opts = opts.with_stdin(BoundedStdin::Null);
    let cancelled = Arc::new(AtomicBool::new(false));
    let _guard = CancelOnDrop(cancelled.clone());
    let task_cancel = cancelled.clone();
    let program_for_join = program.clone();

    tokio::task::spawn_blocking(move || {
        let stdin = match owned_stdin.as_deref() {
            Some(bytes) => BoundedStdin::Bytes(bytes.as_slice()),
            None if preserve => BoundedStdin::Preserve,
            None => BoundedStdin::Null,
        };
        f(&program, base_opts.with_stdin(stdin), &task_cancel).map(into_plain_output)
    })
    .await
    .map_err(|err| {
        CommandError::wait(
            &program_for_join,
            io::Error::other(format!("bounded process task failed: {err}")),
        )
    })?
}

/// Async counterpart of [`run_bounded`]; cancels and kills the child group when dropped.
pub async fn run_bounded_async(
    mut command: Command,
    program: impl Into<String>,
    opts: BoundedOptions<'_>,
) -> Result<Output, CommandError> {
    run_blocking_cancellable(program.into(), opts, move |p, o, c| {
        execute_bounded(p, o, false, Some(c), || {
            spawn_std_process_group(&mut command, p, o)
        })
    })
    .await
}

/// Async counterpart of [`run_detached`] (`POSIX_SPAWN_SETSID` on macOS, `setsid` on Linux).
pub async fn run_detached_async(
    command: DetachedCommand,
    program: impl Into<String>,
    opts: BoundedOptions<'_>,
) -> Result<Output, CommandError> {
    run_blocking_cancellable(program.into(), opts, move |p, o, c| {
        execute_bounded(p, o, false, Some(c), || spawn_detached(&command, p, o))
    })
    .await
}

/// Shared driver for `testkit::process::Command::output_within`; returns partial stdout/stderr on failure.
#[cfg(feature = "testkit")]
pub(crate) fn collect_spawned_within(
    child: Child,
    stdout_fd: Option<OwnedFd>,
    stderr_fd: Option<OwnedFd>,
    timeout: Duration,
    max_output_bytes: usize,
    mut on_before_reap: impl FnMut(),
) -> Result<Output, (CommandError, Vec<u8>, Vec<u8>)> {
    let opts = BoundedOptions {
        ignore_stdin_broken_pipe: true,
        max_output_bytes,
        ..BoundedOptions::capture("testkit", timeout)
    };
    drive_spawned(
        SpawnedFds {
            child: OwnedChild::from_std(child),
            stdin: None,
            stdout: stdout_fd,
            stderr: stderr_fd,
        },
        "test-command",
        Instant::now(),
        opts,
        false,
        false,
        true,
        None,
        &mut on_before_reap,
    )
    .map(into_plain_output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn unique_tmp_dir(prefix: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "{prefix}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&dir).unwrap();
        dir
    }

    #[test]
    fn resolve_program_uses_only_child_path_and_never_leaks_parent_path() {
        let dir = unique_tmp_dir("steve-rt-resolve");
        let tool = dir.join("custom-tool");
        fs::write(&tool, "#!/bin/sh\necho ok\n").unwrap();
        fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).unwrap();
        let nonexec = dir.join("nonexec-tool");
        fs::write(&nonexec, "#!/bin/sh\necho no\n").unwrap();
        fs::set_permissions(&nonexec, fs::Permissions::from_mode(0o644)).unwrap();

        assert_eq!(
            resolve_program(OsStr::new("custom-tool"), Some(dir.as_os_str()), None).unwrap(),
            tool
        );
        assert_eq!(
            resolve_program(OsStr::new("sh"), Some(dir.as_os_str()), None)
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
        assert_eq!(
            resolve_program(OsStr::new("sh"), None, None)
                .unwrap_err()
                .kind(),
            io::ErrorKind::NotFound
        );
        assert_eq!(
            resolve_program(OsStr::new("nonexec-tool"), Some(dir.as_os_str()), None)
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        let anchored = resolve_program(OsStr::new("./custom-tool"), None, Some(&dir)).unwrap();
        assert!(anchored.is_absolute() && anchored == dir.join("./custom-tool"));
        assert_eq!(
            effective_path::<&str, &str, _>(true, std::iter::empty()),
            None
        );
        assert_eq!(
            effective_path(true, [("PATH", Some("/custom"))]),
            Some(OsString::from("/custom"))
        );
        assert_eq!(
            effective_path::<&str, &str, _>(false, [("PATH", None)]),
            None
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn detached_and_process_group_custom_path_never_leak_parent_path_or_env() {
        let dir = unique_tmp_dir("steve-rt-detached-env");
        let script = dir.join("print-env");
        fs::write(&script, "#!/bin/sh\n/usr/bin/env\n").unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

        let mut cleared_no_path = DetachedCommand::new("sh");
        cleared_no_path.env_clear().args(["-c", "echo leaked"]);
        let err = run_detached(
            &cleared_no_path,
            "sh",
            BoundedOptions::capture("t1", Duration::from_secs(5)),
        )
        .unwrap_err();
        assert!(
            matches!(err, CommandError::Spawn { ref source, .. } if source.kind() == io::ErrorKind::NotFound),
            "{err:?}"
        );

        let mut custom_missing = DetachedCommand::new("sh");
        custom_missing
            .env_clear()
            .env("PATH", &dir)
            .args(["-c", "echo leaked"]);
        let err = run_detached(
            &custom_missing,
            "sh",
            BoundedOptions::capture("t2", Duration::from_secs(5)),
        )
        .unwrap_err();
        assert!(
            matches!(err, CommandError::Spawn { ref source, .. } if source.kind() == io::ErrorKind::NotFound),
            "{err:?}"
        );

        let mut cleared_ok = DetachedCommand::new("print-env");
        cleared_ok
            .env_clear()
            .env("PATH", &dir)
            .env("ONLY_THIS", "present");
        let out = run_detached(
            &cleared_ok,
            "print-env",
            BoundedOptions::capture("t3", Duration::from_secs(5)),
        )
        .unwrap();
        assert!(out.status.success());
        let env_text = String::from_utf8_lossy(&out.stdout);
        assert!(
            env_text.contains("ONLY_THIS=present") && !env_text.contains("HOME="),
            "{env_text}"
        );

        let mut pg_cmd = Command::new("print-env");
        pg_cmd.env("PATH", &dir);
        assert!(
            run_bounded(
                &mut pg_cmd,
                "print-env",
                BoundedOptions::capture("t4", Duration::from_secs(5))
            )
            .unwrap()
            .status
            .success()
        );

        let mut pg_missing = Command::new("sh");
        pg_missing.env("PATH", &dir);
        let err = run_bounded(
            &mut pg_missing,
            "sh",
            BoundedOptions::capture("t5", Duration::from_secs(5)),
        )
        .unwrap_err();
        assert!(
            matches!(err, CommandError::Spawn { ref source, .. } if source.kind() == io::ErrorKind::NotFound),
            "{err:?}"
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn owned_child_drop_does_not_repeat_reap_after_failed_reap() {
        let mut cmd = Command::new("/bin/sleep");
        cmd.arg("30")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .process_group(0);
        let mut owned = OwnedChild::from_std(cmd.spawn().unwrap());
        let pid = owned.pid as libc::pid_t;
        assert!(
            owned
                .reap_bounded(Duration::from_millis(20))
                .unwrap()
                .is_none()
        );
        assert!(owned.teardown_attempted);
        let drop_started = Instant::now();
        drop(owned);
        assert!(
            drop_started.elapsed() < Duration::from_secs(1),
            "Drop must not repeat 5s reap"
        );
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
            libc::waitpid(pid, std::ptr::null_mut(), 0);
        }
    }

    #[test]
    fn run_bounded_kills_process_group_on_timeout_without_reap_race() {
        let dir = unique_tmp_dir("steve-rt-proc-timeout");
        let pid_file = dir.join("child.pid");
        let mut cmd = Command::new("/bin/sh");
        cmd.args([
            "-c",
            "/bin/sleep 60 & printf '%s' \"$!\" > \"$1\"; wait",
            "t",
        ])
        .arg(&pid_file);
        let started = Instant::now();
        let err = run_bounded(
            &mut cmd,
            "/bin/sh",
            BoundedOptions::capture("test_timeout", Duration::from_millis(200)),
        )
        .unwrap_err();
        assert!(matches!(err, CommandError::TimedOut { .. }), "{err:?}");
        assert!(started.elapsed() < Duration::from_secs(3));
        if let Ok(raw) = fs::read_to_string(&pid_file)
            && let Ok(grandchild) = raw.trim().parse::<libc::pid_t>()
        {
            let deadline = Instant::now() + Duration::from_secs(2);
            while unsafe { libc::kill(grandchild, 0) } == 0 && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(10));
            }
            assert_eq!(unsafe { libc::kill(grandchild, 0) }, -1);
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn run_bounded_recovers_from_grandchild_holding_pipes_and_enforces_limit() {
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "/bin/sleep 60 & printf 'payload-ok'"]);
        let started = Instant::now();
        let out = run_bounded(
            &mut cmd,
            "/bin/sh",
            BoundedOptions::capture("test_pipe_drain", Duration::from_secs(5)),
        )
        .unwrap();
        assert!(out.status.success() && String::from_utf8_lossy(&out.stdout) == "payload-ok");
        assert!(started.elapsed() < Duration::from_secs(3));

        let mut noisy = Command::new("/bin/sh");
        noisy.args(["-c", "printf '0123456789abcdef'"]);
        let err = run_bounded(
            &mut noisy,
            "/bin/sh",
            BoundedOptions {
                max_output_bytes: 8,
                ..BoundedOptions::capture("test_limit", Duration::from_secs(5))
            },
        )
        .unwrap_err();
        assert!(
            matches!(
                err,
                CommandError::OutputLimitExceeded {
                    stream: "stdout",
                    limit: 8,
                    ..
                }
            ),
            "{err:?}"
        );
    }

    #[test]
    fn zeroizing_cap_and_stdin_broken_pipe_policies() {
        let mut oversized = Command::new("/bin/sh");
        oversized.args(["-c", "echo hi"]);
        let err = run_bounded_zeroizing(
            &mut oversized,
            "/bin/sh",
            BoundedOptions {
                max_output_bytes: ZEROIZE_MAX_OUTPUT_BYTES + 1,
                ..BoundedOptions::capture("zcap", Duration::from_secs(5))
            },
        )
        .unwrap_err();
        assert!(matches!(err, CommandError::PipeRead { .. }), "{err:?}");

        let payload = vec![b'A'; 128 * 1024];
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "exit 11"]);
        let out = run_bounded_zeroizing(
            &mut cmd,
            "/bin/sh",
            BoundedOptions {
                stdin: BoundedStdin::Bytes(&payload),
                ignore_stdin_broken_pipe: true,
                max_output_bytes: 4096,
                ..BoundedOptions::capture("epipe", Duration::from_secs(5))
            },
        )
        .unwrap();
        assert_eq!(out.status.code(), Some(11));

        let mut strict = Command::new("/bin/sh");
        strict.args(["-c", "exit 0"]);
        let err = run_bounded(
            &mut strict,
            "/bin/sh",
            BoundedOptions {
                stdin: BoundedStdin::Bytes(&payload),
                ignore_stdin_broken_pipe: false,
                ..BoundedOptions::capture("strict", Duration::from_secs(5))
            },
        )
        .unwrap_err();
        assert!(matches!(err, CommandError::StdinWrite { .. }), "{err:?}");
    }

    #[test]
    fn detached_session_isolates_session_and_resets_signals() {
        use std::os::unix::process::ExitStatusExt;

        let mut cmd = DetachedCommand::new("/usr/bin/python3");
        cmd.args([
            "-c",
            "import os; print(os.getpid(), os.getpgrp(), os.getsid(0))",
        ]);
        let parent_sid = unsafe { libc::getsid(0) };
        let out = run_detached(
            &cmd,
            "/usr/bin/python3",
            BoundedOptions {
                kill_group_after_exit: true,
                ..BoundedOptions::capture("test_detached", Duration::from_secs(10))
            },
        )
        .unwrap();
        assert!(
            out.status.success(),
            "{:?}",
            String::from_utf8_lossy(&out.stderr)
        );
        let parts: Vec<i32> = String::from_utf8_lossy(&out.stdout)
            .split_whitespace()
            .map(|s| s.parse().unwrap())
            .collect();
        assert_eq!(parts.len(), 3);
        assert_eq!(parts[0], parts[1]);
        assert_eq!(parts[1], parts[2]);
        assert_ne!(parts[2], parent_sid);

        let mut sigpipe_cmd = DetachedCommand::new("/bin/sh");
        sigpipe_cmd.args(["-c", "kill -PIPE $$"]);
        assert_eq!(
            run_detached(
                &sigpipe_cmd,
                "/bin/sh",
                BoundedOptions::capture("sigpipe", Duration::from_secs(5))
            )
            .unwrap()
            .status
            .signal(),
            Some(libc::SIGPIPE)
        );

        let mut block: libc::sigset_t = unsafe { std::mem::zeroed() };
        let mut prev: libc::sigset_t = unsafe { std::mem::zeroed() };
        unsafe {
            libc::sigemptyset(&mut block);
            libc::sigaddset(&mut block, libc::SIGUSR1);
            assert_eq!(libc::pthread_sigmask(libc::SIG_BLOCK, &block, &mut prev), 0);
        }
        let mut mask_cmd = DetachedCommand::new("/bin/sh");
        mask_cmd.args(["-c", "kill -USR1 $$"]);
        let mask_out = run_detached(
            &mask_cmd,
            "/bin/sh",
            BoundedOptions::capture("sigmask", Duration::from_secs(5)),
        );
        unsafe {
            libc::pthread_sigmask(libc::SIG_SETMASK, &prev, std::ptr::null_mut());
        }
        assert_eq!(mask_out.unwrap().status.signal(), Some(libc::SIGUSR1));
    }

    #[tokio::test]
    async fn run_bounded_async_cancels_and_kills_group_on_drop() {
        let dir = unique_tmp_dir("steve-rt-proc-cancel");
        let pid_file = dir.join("child.pid");
        let mut cmd = Command::new("/bin/sh");
        cmd.args([
            "-c",
            "/bin/sleep 60 & printf '%s' \"$!\" > \"$1\"; wait",
            "t",
        ])
        .arg(&pid_file);
        let fut = run_bounded_async(
            cmd,
            "/bin/sh",
            BoundedOptions::capture("test_async_cancel", Duration::from_secs(30)),
        );
        assert!(
            tokio::time::timeout(Duration::from_millis(250), fut)
                .await
                .is_err()
        );
        if let Ok(raw) = fs::read_to_string(&pid_file)
            && let Ok(grandchild) = raw.trim().parse::<libc::pid_t>()
        {
            let deadline = Instant::now() + Duration::from_secs(2);
            while unsafe { libc::kill(grandchild, 0) } == 0 && Instant::now() < deadline {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            assert_eq!(unsafe { libc::kill(grandchild, 0) }, -1);
        }
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn pre_cancelled_task_does_not_spawn() {
        let dir = unique_tmp_dir("steve-rt-precancel");
        let marker = dir.join("spawned");
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "touch \"$1\"", "t"]).arg(&marker);
        let cancel = AtomicBool::new(true);
        let err = execute_bounded(
            "/bin/sh",
            BoundedOptions::capture("test_precancel", Duration::from_secs(5)),
            false,
            Some(&cancel),
            || {
                spawn_std_process_group(
                    &mut cmd,
                    "/bin/sh",
                    BoundedOptions::capture("x", Duration::from_secs(5)),
                )
            },
        )
        .unwrap_err();
        assert!(matches!(err, CommandError::Cancelled { .. }));
        assert!(!marker.exists());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn stop_child_group_sync_kills_descendants_before_reaping_and_is_idempotent() {
        let dir = unique_tmp_dir("steve-rt-stop-sync");
        let pid_file = dir.join("desc.pid");
        let mut cmd = Command::new("/bin/sh");
        cmd.args([
            "-c",
            "/bin/sleep 60 & printf '%s' \"$!\" > \"$1\"; exit 0",
            "t",
        ])
        .arg(&pid_file)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .process_group(0);
        let mut child = cmd.spawn().unwrap();
        let pid = child.id();
        let deadline = Instant::now() + Duration::from_secs(2);
        while (!pid_file.exists() || !exited_unreaped(pid, true).unwrap())
            && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(10));
        }
        let desc: libc::pid_t = fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        let mut reaped = false;
        assert!(
            stop_child_group_sync(
                &mut child,
                &mut reaped,
                TermTarget::Group,
                Duration::from_millis(50),
                PROCESS_KILL_REAP_TIMEOUT
            )
            .unwrap()
            .is_some()
        );
        assert!(reaped);
        let deadline = Instant::now() + Duration::from_secs(2);
        while unsafe { libc::kill(desc, 0) } == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(unsafe { libc::kill(desc, 0) }, -1);
        assert!(
            stop_child_group_sync(
                &mut child,
                &mut reaped,
                TermTarget::Group,
                Duration::from_millis(50),
                PROCESS_KILL_REAP_TIMEOUT
            )
            .unwrap()
            .is_none()
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
