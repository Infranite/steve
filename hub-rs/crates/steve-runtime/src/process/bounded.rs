//! Unified bounded subprocess execution (sync and async) and safe process-group teardown.
//!
//! Design invariants:
//! - **No kill-after-reap:** exit is always observed via `waitid(P_PID, pid, ..., WEXITED | WNOWAIT)`
//!   so the zombie leader keeps pinning its PID/PGID until all pipe draining and any `-pid`
//!   `SIGKILL` have completed; `waitpid` runs only at the very end.
//! - **Single-threaded `poll(2)` I/O:** stdin writes and stdout/stderr reads run concurrently in
//!   one non-blocking `poll(2)` loop with zero auxiliary reader threads, so even a `setsid()`-escaped
//!   grandchild holding a pipe open cannot leak OS threads.
//! - **macOS `posix_spawn` for `DetachedSession`:** avoids multithreaded `fork()` hazards on macOS
//!   while preserving `SIGPIPE = SIG_DFL` and an empty signal mask.
//! - **Mandatory output bounds and layer-boundary metrics:** every captured stream has an explicit
//!   byte cap and emits `steve_process_runs_total`, `steve_process_duration_seconds`, and
//!   `steve_process_group_kills_total`.

use std::fmt;
use std::io;
use std::os::fd::{AsRawFd, OwnedFd, RawFd};
use std::os::unix::process::CommandExt;
#[cfg(target_os = "macos")]
use std::os::unix::process::ExitStatusExt;
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use zeroize::Zeroizing;

/// Grace period after the direct child exits for stdout/stderr pipes to reach EOF before
/// treating an open pipe as a lingering descendant and sending `SIGKILL` to `-pgid`.
/// Chosen to let normal children flush buffered stdio (<10 ms) while bounding recovery latency.
pub const PIPE_DRAIN_GRACE: Duration = Duration::from_millis(200);

/// Maximum wait to reap the direct child after sending `SIGKILL` to its process group.
/// Chosen at 5 s to tolerate transient kernel scheduling/I/O delays without hanging callers on
/// permanent uninterruptible (D-state) processes.
pub const PROCESS_KILL_REAP_TIMEOUT: Duration = Duration::from_secs(5);

/// Maximum wait for stdout/stderr pipes to observe EOF after `SIGKILL` is sent to `-pgid`.
/// Chosen at 2 s: kernel fd teardown completes in milliseconds, while a `setsid()`-escaped
/// grandchild fails promptly without blocking the caller indefinitely.
pub const PIPE_POST_KILL_TIMEOUT: Duration = Duration::from_secs(2);

/// Default per-stream capture ceiling (16 MiB) for general maintenance/git subprocesses.
/// Large enough for normal git diffs or diagnostic JSON while preventing unbounded OOM; narrow
/// callers (keyhelper, npm_root, shell probes, candidate checks) specify tighter named limits.
pub const DEFAULT_MAX_OUTPUT_BYTES: usize = 16 * 1024 * 1024;

/// Wake-up interval for `poll(2)` while pipes are idle so `waitid(WNOHANG)` and cancellation
/// flags are checked promptly; active pipes wake `poll(2)` immediately with zero polling delay.
const POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Maximum secret stdout buffer size pre-allocated upfront so `Zeroizing<Vec<u8>>` never triggers
/// a heap reallocation that would leave unzeroized key copies in freed allocator chunks.
const ZEROIZE_PREALLOC_CAP: usize = 64 * 1024;

/// Stack buffer size per non-blocking `read(2)` iteration.
const READ_CHUNK_BYTES: usize = 4096;

/// How [`run_bounded`] isolates the child's process group / session so timeouts and pipe-drain
/// cleanup can terminate the entire descendant tree safely.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProcessIsolation {
    /// Start a dedicated process group (`pgid == pid`; uses `posix_spawn` on macOS via std).
    ProcessGroup,
    /// Start a detached session (`setsid`, no controlling `/dev/tty`, `pgid == pid`).
    /// Uses `POSIX_SPAWN_SETSID` on macOS (zero `fork()`) and `pre_exec(setsid)` on Linux.
    DetachedSession,
}

/// How [`run_bounded`] configures standard input.
#[derive(Clone, Copy, Debug, Default)]
pub enum BoundedStdin<'a> {
    /// Redirect stdin to `/dev/null` so the child cannot block waiting for a terminal or parent.
    #[default]
    Null,
    /// Deliver these exact bytes over a non-blocking pipe bounded by the overall deadline.
    Bytes(&'a [u8]),
    /// Preserve the stdin already configured on the `Command` (e.g. a private file descriptor).
    /// Supported with [`ProcessIsolation::ProcessGroup`].
    Preserve,
}

/// What [`run_bounded`] does when a descendant keeps stdout or stderr open past [`PIPE_DRAIN_GRACE`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum HeldPipePolicy {
    /// Kill `-pgid` while unreaped and return the captured output if EOF arrives within
    /// [`PIPE_POST_KILL_TIMEOUT`] (otherwise fail with [`CommandError::PipeDrainTimedOut`]).
    #[default]
    RecoverAfterGroupKill,
    /// Kill `-pgid` while unreaped, wait briefly for teardown, and always fail with
    /// [`CommandError::PipeDrainTimedOut`].
    FailAfterGroupKill,
}

/// Execution contract for [`run_bounded`], [`run_bounded_zeroizing`], and [`run_bounded_async`].
#[derive(Clone, Copy, Debug)]
pub struct BoundedOptions<'a> {
    /// Bounded low-cardinality label for `steve_process_*` metrics (never a dynamic path or ID).
    pub operation: &'static str,
    pub timeout: Duration,
    pub isolation: ProcessIsolation,
    pub stdin: BoundedStdin<'a>,
    /// When `true`, `BrokenPipe` / `WriteZero` while delivering [`BoundedStdin::Bytes`] stops
    /// writing and lets the child's exit code surface instead of failing with `StdinWrite`.
    pub ignore_stdin_broken_pipe: bool,
    pub capture_output: bool,
    /// Maximum bytes accepted per captured stream (`stdout` and `stderr` are bounded separately).
    pub max_output_bytes: usize,
    /// Send `SIGKILL` to `-pgid` before reaping even if pipes closed cleanly (for example,
    /// interactive shell probes whose rc scripts may leave background helpers behind).
    pub kill_group_after_exit: bool,
    /// Policy when stdout/stderr do not reach EOF within [`PIPE_DRAIN_GRACE`] after leader exit.
    pub held_pipe_policy: HeldPipePolicy,
    /// Set to `true` when combining [`ProcessIsolation::DetachedSession`] with `Command::env_clear`
    /// on macOS (Rust's `Command::get_envs` does not report whether `env_clear` was called).
    pub detached_env_clear: bool,
}

impl<'a> BoundedOptions<'a> {
    pub fn capture(operation: &'static str, timeout: Duration) -> Self {
        Self {
            operation,
            timeout,
            isolation: ProcessIsolation::ProcessGroup,
            stdin: BoundedStdin::Null,
            ignore_stdin_broken_pipe: false,
            capture_output: true,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
            kill_group_after_exit: false,
            held_pipe_policy: HeldPipePolicy::RecoverAfterGroupKill,
            detached_env_clear: false,
        }
    }

    pub fn silent(operation: &'static str, timeout: Duration) -> Self {
        Self {
            operation,
            timeout,
            isolation: ProcessIsolation::ProcessGroup,
            stdin: BoundedStdin::Null,
            ignore_stdin_broken_pipe: false,
            capture_output: false,
            max_output_bytes: DEFAULT_MAX_OUTPUT_BYTES,
            kill_group_after_exit: false,
            held_pipe_policy: HeldPipePolicy::RecoverAfterGroupKill,
            detached_env_clear: false,
        }
    }
}

/// Output of [`run_bounded_zeroizing`]: `stdout` is zeroized on drop and pre-allocated when
/// `max_output_bytes <= 64 KiB` to avoid heap reallocations of secret material.
pub struct ZeroizingOutput {
    pub status: ExitStatus,
    pub stdout: Zeroizing<Vec<u8>>,
    pub stderr: Vec<u8>,
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
            } => write!(
                f,
                "{program} {stream} pipe did not close within {}ms after exit",
                grace.as_millis()
            ),
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
            Self::TimedOut { .. }
            | Self::Cancelled { .. }
            | Self::PipeDrainTimedOut { .. }
            | Self::OutputLimitExceeded { .. } => None,
        }
    }
}

/// Target of the initial graceful `SIGTERM` in [`stop_child_group_sync`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TermTarget {
    /// Send `SIGTERM` to the leader PID only (letting it shut down its own children cleanly).
    Leader,
    /// Send `SIGTERM` to the entire process group (`-pid`).
    Group,
}

/// Observe whether direct child `pid` has exited WITHOUT reaping it, so its zombie entry keeps
/// pinning its PID and process-group number (`pgid == pid`) against PID reuse.
pub fn exited_unreaped(pid: u32, nonblocking: bool) -> io::Result<bool> {
    let options = libc::WEXITED | libc::WNOWAIT | if nonblocking { libc::WNOHANG } else { 0 };
    loop {
        // SAFETY: POSIX requires zeroing `siginfo_t` so `si_pid == 0` reliably indicates not
        // exited when `WNOHANG` returns 0.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        // SAFETY: `info` is a valid writable `siginfo_t`.
        let result = unsafe { libc::waitid(libc::P_PID, pid as libc::id_t, &mut info, options) };
        if result == 0 {
            #[cfg(target_os = "macos")]
            let observed = info.si_pid;
            #[cfg(not(target_os = "macos"))]
            // SAFETY: `waitid` succeeded and populated `info`.
            let observed = unsafe { info.si_pid() };
            return Ok(observed != 0);
        }
        let error = io::Error::last_os_error();
        if error.kind() != io::ErrorKind::Interrupted {
            return Err(error);
        }
    }
}

/// Gracefully stop a direct child running in its own process group (`pgid == pid`):
/// 1. If not already exited (observed without reaping via `waitid(WNOWAIT)`), send `SIGTERM` to
///    `term_target` and wait up to `grace` without reaping.
/// 2. If still alive past `grace`, send `SIGKILL` to `-pid` and `pid`.
/// 3. While the leader is STILL unreaped (pinning `pgid == pid`), send `SIGKILL` to `-pid` to
///    clear any lingering descendants in the group.
/// 4. Finally reap the direct child within `kill_reap_timeout` and set `*reaped = true`.
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
        let target = match term_target {
            TermTarget::Leader => pid_i32,
            TermTarget::Group => -pid_i32,
        };
        // SAFETY: unreaped child owns `pid` and its process group `-pid`.
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
                Ok(false) => {
                    let Some(rem) = deadline.checked_duration_since(Instant::now()) else {
                        // Still alive after grace: force-kill both group and leader while unreaped.
                        // SAFETY: unreaped child still pins `pid` and `-pid`.
                        unsafe {
                            libc::kill(-pid_i32, libc::SIGKILL);
                            libc::kill(pid_i32, libc::SIGKILL);
                        }
                        break;
                    };
                    std::thread::sleep(POLL_INTERVAL.min(rem));
                }
                Err(e) if e.raw_os_error() == Some(libc::ECHILD) => {
                    *reaped = true;
                    return Ok(None);
                }
                Err(e) => return Err(e),
            }
        }
    }

    // Leader is still unreaped (zombie or dying), so `-pid` cannot have been reused.
    if pid_i32 > 1 {
        // SAFETY: unreaped leader still pins `pgid == pid`.
        unsafe {
            libc::kill(-pid_i32, libc::SIGKILL);
        }
    }

    let reap_deadline = Instant::now() + kill_reap_timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                *reaped = true;
                return Ok(Some(status));
            }
            Ok(None) => {
                let Some(rem) = reap_deadline.checked_duration_since(Instant::now()) else {
                    return Ok(None);
                };
                std::thread::sleep(POLL_INTERVAL.min(rem));
            }
            Err(e) if e.raw_os_error() == Some(libc::ECHILD) => {
                *reaped = true;
                return Ok(None);
            }
            Err(e) => return Err(e),
        }
    }
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
}

impl OwnedChild {
    fn from_std(child: Child) -> Self {
        let pid = child.id();
        Self {
            inner: SpawnedChild::Std(child),
            pid,
            reaped: false,
        }
    }

    #[cfg(target_os = "macos")]
    fn from_raw_pid(pid: libc::pid_t) -> Self {
        Self {
            inner: SpawnedChild::RawPid(pid),
            pid: pid as u32,
            reaped: false,
        }
    }

    fn exited_unreaped(&self) -> io::Result<bool> {
        if self.reaped {
            return Ok(true);
        }
        exited_unreaped(self.pid, true)
    }

    fn kill_group(&self) {
        if !self.reaped && self.pid > 1 {
            // SAFETY: unreaped child was started in its own process group (`pgid == pid`).
            unsafe {
                libc::kill(-(self.pid as libc::pid_t), libc::SIGKILL);
            }
        }
    }

    fn kill_group_and_leader(&self) {
        if !self.reaped && self.pid > 1 {
            let pid = self.pid as libc::pid_t;
            // SAFETY: unreaped child still pins both `-pid` and `pid` even if it changed groups.
            unsafe {
                libc::kill(-pid, libc::SIGKILL);
                libc::kill(pid, libc::SIGKILL);
            }
        }
    }

    fn try_reap(&mut self) -> io::Result<Option<ExitStatus>> {
        if self.reaped {
            return Ok(None);
        }
        match &mut self.inner {
            SpawnedChild::Std(child) => {
                let status = child.try_wait()?;
                if status.is_some() {
                    self.reaped = true;
                }
                Ok(status)
            }
            #[cfg(target_os = "macos")]
            SpawnedChild::RawPid(pid) => {
                let mut status: libc::c_int = 0;
                loop {
                    // SAFETY: `*pid` is an unreaped direct child owned by this struct.
                    let rc = unsafe { libc::waitpid(*pid, &mut status, libc::WNOHANG) };
                    if rc == 0 {
                        return Ok(None);
                    }
                    if rc == *pid {
                        self.reaped = true;
                        return Ok(Some(ExitStatus::from_raw(status)));
                    }
                    let err = io::Error::last_os_error();
                    if err.kind() == io::ErrorKind::Interrupted {
                        continue;
                    }
                    return Err(err);
                }
            }
        }
    }

    fn reap_bounded(&mut self, timeout: Duration) -> io::Result<Option<ExitStatus>> {
        let deadline = Instant::now() + timeout;
        loop {
            match self.try_reap() {
                Ok(Some(status)) => return Ok(Some(status)),
                Ok(None) => {
                    let Some(rem) = deadline.checked_duration_since(Instant::now()) else {
                        return Ok(None);
                    };
                    std::thread::sleep(POLL_INTERVAL.min(rem));
                }
                Err(e) => return Err(e),
            }
        }
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if !self.reaped {
            self.kill_group_and_leader();
            let _ = self.reap_bounded(PROCESS_KILL_REAP_TIMEOUT);
        }
    }
}

fn set_nonblocking(fd: RawFd) -> io::Result<()> {
    // SAFETY: caller passes a valid open descriptor owned for both `fcntl` calls.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

fn record_outcome(operation: &'static str, result: &'static str, started: Instant) {
    metrics::counter!(
        "steve_process_runs_total",
        "operation" => operation,
        "result" => result
    )
    .increment(1);
    metrics::histogram!(
        "steve_process_duration_seconds",
        "operation" => operation
    )
    .record(started.elapsed().as_secs_f64());
}

fn record_group_kill(operation: &'static str, reason: &'static str) {
    metrics::counter!(
        "steve_process_group_kills_total",
        "operation" => operation,
        "reason" => reason
    )
    .increment(1);
}

enum ReadAction {
    Continue,
    Eof,
    LimitExceeded,
    Error(io::Error),
}

fn drain_nonblocking_fd(
    fd: RawFd,
    buf: &mut Vec<u8>,
    max_bytes: usize,
    zeroize_chunk: bool,
) -> ReadAction {
    let mut chunk = Zeroizing::new([0u8; READ_CHUNK_BYTES]);
    loop {
        // SAFETY: `fd` is an open non-blocking pipe read end and `chunk` is writable.
        let n = unsafe { libc::read(fd, chunk.as_mut_ptr().cast::<libc::c_void>(), chunk.len()) };
        if n == 0 {
            if !zeroize_chunk {
                // `Zeroizing` drops anyway; no extra branch needed.
            }
            return ReadAction::Eof;
        }
        if n > 0 {
            let count = n as usize;
            if buf.len().saturating_add(count) > max_bytes {
                return ReadAction::LimitExceeded;
            }
            buf.extend_from_slice(&chunk[..count]);
            continue;
        }
        let err = io::Error::last_os_error();
        match err.kind() {
            io::ErrorKind::Interrupted => continue,
            io::ErrorKind::WouldBlock => return ReadAction::Continue,
            _ => return ReadAction::Error(err),
        }
    }
}

enum WriteAction {
    Continue,
    Done,
    BrokenPipe(io::Error),
    Error(io::Error),
}

fn write_nonblocking_fd(fd: RawFd, bytes: &[u8], offset: &mut usize) -> WriteAction {
    while *offset < bytes.len() {
        let slice = &bytes[*offset..];
        // SAFETY: `fd` is an open non-blocking pipe write end and `slice` is readable.
        let n = unsafe { libc::write(fd, slice.as_ptr().cast::<libc::c_void>(), slice.len()) };
        if n == 0 {
            return WriteAction::BrokenPipe(io::Error::new(
                io::ErrorKind::WriteZero,
                "child closed stdin",
            ));
        }
        if n > 0 {
            *offset += n as usize;
            continue;
        }
        let err = io::Error::last_os_error();
        match err.kind() {
            io::ErrorKind::Interrupted => continue,
            io::ErrorKind::WouldBlock => return WriteAction::Continue,
            io::ErrorKind::BrokenPipe => return WriteAction::BrokenPipe(err),
            _ => return WriteAction::Error(err),
        }
    }
    WriteAction::Done
}

fn poll_once(fds: &mut [libc::pollfd], wait: Duration) -> io::Result<libc::c_int> {
    let timeout_ms = wait.as_millis().clamp(1, i32::MAX as u128) as libc::c_int;
    loop {
        // SAFETY: `fds` is a valid slice of `pollfd` descriptors.
        let rc = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, timeout_ms) };
        if rc >= 0 {
            return Ok(rc);
        }
        let err = io::Error::last_os_error();
        if err.kind() != io::ErrorKind::Interrupted {
            return Err(err);
        }
    }
}

struct DrainState {
    stdout_fd: Option<OwnedFd>,
    stderr_fd: Option<OwnedFd>,
    stdout_buf: Zeroizing<Vec<u8>>,
    stderr_buf: Vec<u8>,
    max_output_bytes: usize,
    zeroize_stdout: bool,
}

enum DrainOutcome {
    Closed,
    TimedOut,
    Cancelled,
    LimitExceeded(&'static str),
    Error(&'static str, io::Error),
}

impl DrainState {
    fn new(
        stdout_fd: Option<OwnedFd>,
        stderr_fd: Option<OwnedFd>,
        max_output_bytes: usize,
        zeroize_stdout: bool,
    ) -> Self {
        let stdout_vec = if zeroize_stdout {
            Vec::with_capacity(max_output_bytes.min(ZEROIZE_PREALLOC_CAP))
        } else {
            Vec::new()
        };
        Self {
            stdout_fd,
            stderr_fd,
            stdout_buf: Zeroizing::new(stdout_vec),
            stderr_buf: Vec::new(),
            max_output_bytes,
            zeroize_stdout,
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

    fn poll_step(
        &mut self,
        wait: Duration,
    ) -> Result<(), (&'static str, Result<usize, io::Error>)> {
        let mut pfds: [libc::pollfd; 2] = [libc::pollfd {
            fd: -1,
            events: 0,
            revents: 0,
        }; 2];
        let mut kinds: [u8; 2] = [0; 2];
        let mut count = 0usize;
        if let Some(fd) = &self.stdout_fd {
            pfds[count] = libc::pollfd {
                fd: fd.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            kinds[count] = 1;
            count += 1;
        }
        if let Some(fd) = &self.stderr_fd {
            pfds[count] = libc::pollfd {
                fd: fd.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            kinds[count] = 2;
            count += 1;
        }
        if count == 0 {
            return Ok(());
        }
        if let Err(err) = poll_once(&mut pfds[..count], wait) {
            return Err(("stdout", Err(err)));
        }
        for i in 0..count {
            let revents = pfds[i].revents;
            if revents == 0 {
                continue;
            }
            if revents & libc::POLLNVAL != 0 {
                if kinds[i] == 1 {
                    self.stdout_fd = None;
                } else {
                    self.stderr_fd = None;
                }
                continue;
            }
            if kinds[i] == 1 {
                if let Some(fd) = &self.stdout_fd {
                    match drain_nonblocking_fd(
                        fd.as_raw_fd(),
                        &mut self.stdout_buf,
                        self.max_output_bytes,
                        self.zeroize_stdout,
                    ) {
                        ReadAction::Continue => {}
                        ReadAction::Eof => self.stdout_fd = None,
                        ReadAction::LimitExceeded => {
                            return Err(("stdout", Ok(self.max_output_bytes)));
                        }
                        ReadAction::Error(err) => return Err(("stdout", Err(err))),
                    }
                }
            } else if let Some(fd) = &self.stderr_fd {
                match drain_nonblocking_fd(
                    fd.as_raw_fd(),
                    &mut self.stderr_buf,
                    self.max_output_bytes,
                    false,
                ) {
                    ReadAction::Continue => {}
                    ReadAction::Eof => self.stderr_fd = None,
                    ReadAction::LimitExceeded => {
                        return Err(("stderr", Ok(self.max_output_bytes)));
                    }
                    ReadAction::Error(err) => return Err(("stderr", Err(err))),
                }
            }
        }
        Ok(())
    }

    fn drain_until(&mut self, deadline: Instant, cancelled: Option<&AtomicBool>) -> DrainOutcome {
        while self.any_open() {
            if cancelled.is_some_and(|c| c.load(Ordering::Relaxed)) {
                return DrainOutcome::Cancelled;
            }
            let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                return DrainOutcome::TimedOut;
            };
            match self.poll_step(POLL_INTERVAL.min(remaining)) {
                Ok(()) => {}
                Err((stream, Ok(_))) => return DrainOutcome::LimitExceeded(stream),
                Err((stream, Err(err))) => return DrainOutcome::Error(stream, err),
            }
        }
        DrainOutcome::Closed
    }
}

fn spawn_for_bounded(
    command: &mut Command,
    program: &str,
    opts: BoundedOptions<'_>,
) -> Result<
    (
        OwnedChild,
        Option<OwnedFd>,
        Option<OwnedFd>,
        Option<OwnedFd>,
    ),
    CommandError,
> {
    #[cfg(target_os = "macos")]
    if matches!(opts.isolation, ProcessIsolation::DetachedSession) {
        if matches!(opts.stdin, BoundedStdin::Preserve) {
            return Err(CommandError::StdinConfigure {
                program: program.to_owned(),
                source: io::Error::new(
                    io::ErrorKind::Unsupported,
                    "BoundedStdin::Preserve is not supported with DetachedSession on macOS",
                ),
            });
        }
        let args: Vec<&std::ffi::OsStr> = command.get_args().collect();
        let deltas: Vec<(std::ffi::OsString, Option<std::ffi::OsString>)> = command
            .get_envs()
            .map(|(k, v)| (k.to_os_string(), v.map(std::ffi::OsStr::to_os_string)))
            .collect();
        let cleared: Vec<(std::ffi::OsString, std::ffi::OsString)> = deltas
            .iter()
            .filter_map(|(k, v)| v.as_ref().map(|val| (k.clone(), val.clone())))
            .collect();
        let env_spec = if opts.detached_env_clear {
            super::macos_spawn::EnvSpec::Clear(&cleared)
        } else {
            super::macos_spawn::EnvSpec::InheritWithDeltas(&deltas)
        };
        let stdin_spec = match opts.stdin {
            BoundedStdin::Null => super::macos_spawn::StdioSpec::Null,
            BoundedStdin::Bytes(_) => super::macos_spawn::StdioSpec::Pipe,
            BoundedStdin::Preserve => unreachable!(),
        };
        let io_spec = if opts.capture_output {
            super::macos_spawn::StdioSpec::Pipe
        } else {
            super::macos_spawn::StdioSpec::Null
        };
        let spawned = super::macos_spawn::spawn(super::macos_spawn::SpawnSpec {
            program: command.get_program(),
            args: &args,
            env: env_spec,
            cwd: command.get_current_dir(),
            isolation: ProcessIsolation::DetachedSession,
            stdin: stdin_spec,
            stdout: io_spec,
            stderr: io_spec,
            extra_inherit_fds: &[],
        })
        .map_err(|source| CommandError::Spawn {
            program: program.to_owned(),
            source,
        })?;
        return Ok((
            OwnedChild::from_raw_pid(spawned.pid),
            spawned.stdin,
            spawned.stdout,
            spawned.stderr,
        ));
    }

    match opts.isolation {
        ProcessIsolation::ProcessGroup => {
            command.process_group(0);
        }
        ProcessIsolation::DetachedSession => {
            #[cfg(not(target_os = "macos"))]
            // SAFETY: `libc::setsid` is async-signal-safe per POSIX.
            unsafe {
                command.pre_exec(|| {
                    if libc::setsid() == -1 {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        }
    }
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

    let mut child = command.spawn().map_err(|source| CommandError::Spawn {
        program: program.to_owned(),
        source,
    })?;
    let stdin_fd = if matches!(opts.stdin, BoundedStdin::Bytes(_)) {
        child.stdin.take().map(OwnedFd::from)
    } else {
        None
    };
    let (stdout_fd, stderr_fd) = if opts.capture_output {
        (
            child.stdout.take().map(OwnedFd::from),
            child.stderr.take().map(OwnedFd::from),
        )
    } else {
        (None, None)
    };
    Ok((OwnedChild::from_std(child), stdin_fd, stdout_fd, stderr_fd))
}

fn run_bounded_core(
    command: &mut Command,
    program: &str,
    opts: BoundedOptions<'_>,
    zeroize_stdout: bool,
    cancelled: Option<&AtomicBool>,
) -> Result<ZeroizingOutput, CommandError> {
    let started = Instant::now();
    let deadline = started + opts.timeout;
    let (mut child, mut stdin_fd, stdout_fd, stderr_fd) =
        match spawn_for_bounded(command, program, opts) {
            Ok(tuple) => tuple,
            Err(err) => {
                record_outcome(opts.operation, "error", started);
                return Err(err);
            }
        };

    let stdin_bytes = match opts.stdin {
        BoundedStdin::Bytes(b) => b,
        _ => &[],
    };
    if stdin_bytes.is_empty() {
        stdin_fd = None;
    }

    if let Some(fd) = &stdin_fd
        && let Err(source) = set_nonblocking(fd.as_raw_fd())
    {
        drop(stdin_fd);
        drop(stdout_fd);
        drop(stderr_fd);
        child.kill_group_and_leader();
        record_group_kill(opts.operation, "error");
        let _ = child.reap_bounded(PROCESS_KILL_REAP_TIMEOUT);
        record_outcome(opts.operation, "error", started);
        return Err(CommandError::StdinConfigure {
            program: program.to_owned(),
            source,
        });
    }
    let pipe_setup_err = stdout_fd
        .as_ref()
        .and_then(|fd| set_nonblocking(fd.as_raw_fd()).err().map(|e| ("stdout", e)))
        .or_else(|| {
            stderr_fd
                .as_ref()
                .and_then(|fd| set_nonblocking(fd.as_raw_fd()).err().map(|e| ("stderr", e)))
        });
    if let Some((stream, source)) = pipe_setup_err {
        drop(stdin_fd);
        drop(stdout_fd);
        drop(stderr_fd);
        child.kill_group_and_leader();
        record_group_kill(opts.operation, "error");
        let _ = child.reap_bounded(PROCESS_KILL_REAP_TIMEOUT);
        record_outcome(opts.operation, "error", started);
        return Err(CommandError::PipeRead {
            program: program.to_owned(),
            stream,
            source,
        });
    }

    let mut drain = DrainState::new(stdout_fd, stderr_fd, opts.max_output_bytes, zeroize_stdout);
    let mut stdin_offset = 0usize;

    let abort_child = |child: &mut OwnedChild,
                       stdin_fd: &mut Option<OwnedFd>,
                       drain: &mut DrainState,
                       result_label: &'static str,
                       kill_reason: &'static str| {
        *stdin_fd = None;
        drain.close_all();
        child.kill_group_and_leader();
        record_group_kill(opts.operation, kill_reason);
        let _ = child.reap_bounded(PROCESS_KILL_REAP_TIMEOUT);
        record_outcome(opts.operation, result_label, started);
    };

    // Phase 1: deliver stdin, drain stdout/stderr concurrently, and observe exit WITHOUT reaping.
    loop {
        if cancelled.is_some_and(|c| c.load(Ordering::Relaxed)) {
            abort_child(
                &mut child,
                &mut stdin_fd,
                &mut drain,
                "cancelled",
                "cancelled",
            );
            return Err(CommandError::Cancelled {
                program: program.to_owned(),
            });
        }
        match child.exited_unreaped() {
            Ok(true) => break,
            Ok(false) => {}
            Err(source) => {
                abort_child(&mut child, &mut stdin_fd, &mut drain, "error", "error");
                return Err(CommandError::Wait {
                    program: program.to_owned(),
                    source,
                });
            }
        }
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            abort_child(&mut child, &mut stdin_fd, &mut drain, "timeout", "timeout");
            return Err(CommandError::TimedOut {
                program: program.to_owned(),
                timeout: opts.timeout,
            });
        };

        let mut pfds = [libc::pollfd {
            fd: -1,
            events: 0,
            revents: 0,
        }; 3];
        let mut kinds = [0u8; 3]; // 0 = stdin, 1 = stdout, 2 = stderr
        let mut count = 0usize;
        if let Some(fd) = &stdin_fd {
            pfds[count] = libc::pollfd {
                fd: fd.as_raw_fd(),
                events: libc::POLLOUT,
                revents: 0,
            };
            kinds[count] = 0;
            count += 1;
        }
        if let Some(fd) = &drain.stdout_fd {
            pfds[count] = libc::pollfd {
                fd: fd.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            kinds[count] = 1;
            count += 1;
        }
        if let Some(fd) = &drain.stderr_fd {
            pfds[count] = libc::pollfd {
                fd: fd.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            };
            kinds[count] = 2;
            count += 1;
        }
        if count == 0 {
            std::thread::sleep(POLL_INTERVAL.min(remaining));
            continue;
        }
        if let Err(source) = poll_once(&mut pfds[..count], POLL_INTERVAL.min(remaining)) {
            abort_child(&mut child, &mut stdin_fd, &mut drain, "error", "error");
            return Err(CommandError::Wait {
                program: program.to_owned(),
                source,
            });
        }
        for i in 0..count {
            let revents = pfds[i].revents;
            if revents == 0 {
                continue;
            }
            match kinds[i] {
                0 => {
                    if let Some(fd) = &stdin_fd {
                        match write_nonblocking_fd(fd.as_raw_fd(), stdin_bytes, &mut stdin_offset) {
                            WriteAction::Continue => {}
                            WriteAction::Done => stdin_fd = None,
                            WriteAction::BrokenPipe(source) => {
                                stdin_fd = None;
                                if !opts.ignore_stdin_broken_pipe {
                                    abort_child(
                                        &mut child,
                                        &mut stdin_fd,
                                        &mut drain,
                                        "error",
                                        "error",
                                    );
                                    return Err(CommandError::StdinWrite {
                                        program: program.to_owned(),
                                        source,
                                    });
                                }
                            }
                            WriteAction::Error(source) => {
                                abort_child(
                                    &mut child,
                                    &mut stdin_fd,
                                    &mut drain,
                                    "error",
                                    "error",
                                );
                                return Err(CommandError::StdinWrite {
                                    program: program.to_owned(),
                                    source,
                                });
                            }
                        }
                    }
                }
                1 => {
                    if revents & libc::POLLNVAL != 0 {
                        drain.stdout_fd = None;
                    } else if let Some(fd) = &drain.stdout_fd {
                        match drain_nonblocking_fd(
                            fd.as_raw_fd(),
                            &mut drain.stdout_buf,
                            drain.max_output_bytes,
                            drain.zeroize_stdout,
                        ) {
                            ReadAction::Continue => {}
                            ReadAction::Eof => drain.stdout_fd = None,
                            ReadAction::LimitExceeded => {
                                abort_child(
                                    &mut child,
                                    &mut stdin_fd,
                                    &mut drain,
                                    "output_limit",
                                    "output_limit",
                                );
                                return Err(CommandError::OutputLimitExceeded {
                                    program: program.to_owned(),
                                    stream: "stdout",
                                    limit: drain.max_output_bytes,
                                });
                            }
                            ReadAction::Error(source) => {
                                abort_child(
                                    &mut child,
                                    &mut stdin_fd,
                                    &mut drain,
                                    "error",
                                    "error",
                                );
                                return Err(CommandError::PipeRead {
                                    program: program.to_owned(),
                                    stream: "stdout",
                                    source,
                                });
                            }
                        }
                    }
                }
                _ => {
                    if revents & libc::POLLNVAL != 0 {
                        drain.stderr_fd = None;
                    } else if let Some(fd) = &drain.stderr_fd {
                        match drain_nonblocking_fd(
                            fd.as_raw_fd(),
                            &mut drain.stderr_buf,
                            drain.max_output_bytes,
                            false,
                        ) {
                            ReadAction::Continue => {}
                            ReadAction::Eof => drain.stderr_fd = None,
                            ReadAction::LimitExceeded => {
                                abort_child(
                                    &mut child,
                                    &mut stdin_fd,
                                    &mut drain,
                                    "output_limit",
                                    "output_limit",
                                );
                                return Err(CommandError::OutputLimitExceeded {
                                    program: program.to_owned(),
                                    stream: "stderr",
                                    limit: drain.max_output_bytes,
                                });
                            }
                            ReadAction::Error(source) => {
                                abort_child(
                                    &mut child,
                                    &mut stdin_fd,
                                    &mut drain,
                                    "error",
                                    "error",
                                );
                                return Err(CommandError::PipeRead {
                                    program: program.to_owned(),
                                    stream: "stderr",
                                    source,
                                });
                            }
                        }
                    }
                }
            }
        }
    }

    // Leader has exited and is STILL UNREAPED (its zombie entry pins pgid == pid).
    stdin_fd = None;

    // Phase 2: drain stdout/stderr under one shared `PIPE_DRAIN_GRACE` window BEFORE reaping.
    match drain.drain_until(Instant::now() + PIPE_DRAIN_GRACE, cancelled) {
        DrainOutcome::Closed | DrainOutcome::TimedOut => {}
        DrainOutcome::Cancelled => {
            abort_child(
                &mut child,
                &mut stdin_fd,
                &mut drain,
                "cancelled",
                "cancelled",
            );
            return Err(CommandError::Cancelled {
                program: program.to_owned(),
            });
        }
        DrainOutcome::LimitExceeded(stream) => {
            abort_child(
                &mut child,
                &mut stdin_fd,
                &mut drain,
                "output_limit",
                "output_limit",
            );
            return Err(CommandError::OutputLimitExceeded {
                program: program.to_owned(),
                stream,
                limit: drain.max_output_bytes,
            });
        }
        DrainOutcome::Error(stream, source) => {
            abort_child(&mut child, &mut stdin_fd, &mut drain, "error", "error");
            return Err(CommandError::PipeRead {
                program: program.to_owned(),
                stream,
                source,
            });
        }
    }

    let pipes_held = drain.any_open();
    if pipes_held || opts.kill_group_after_exit {
        // Still unreaped: `-pid` is guaranteed to address only this process group.
        child.kill_group();
        record_group_kill(
            opts.operation,
            if pipes_held {
                "pipe_drain"
            } else {
                "post_exit"
            },
        );
    }

    if pipes_held {
        let held_stream = drain.open_stream_name();
        match drain.drain_until(Instant::now() + PIPE_POST_KILL_TIMEOUT, cancelled) {
            DrainOutcome::Closed => {
                if opts.held_pipe_policy == HeldPipePolicy::FailAfterGroupKill {
                    let _ = child.reap_bounded(PROCESS_KILL_REAP_TIMEOUT);
                    record_outcome(opts.operation, "pipe_drain_timeout", started);
                    return Err(CommandError::PipeDrainTimedOut {
                        program: program.to_owned(),
                        stream: held_stream,
                        grace: PIPE_DRAIN_GRACE,
                    });
                }
            }
            DrainOutcome::TimedOut => {
                let stream = drain.open_stream_name();
                drain.close_all();
                let _ = child.reap_bounded(PROCESS_KILL_REAP_TIMEOUT);
                record_outcome(opts.operation, "pipe_drain_timeout", started);
                return Err(CommandError::PipeDrainTimedOut {
                    program: program.to_owned(),
                    stream,
                    grace: PIPE_DRAIN_GRACE,
                });
            }
            DrainOutcome::Cancelled => {
                drain.close_all();
                let _ = child.reap_bounded(PROCESS_KILL_REAP_TIMEOUT);
                record_outcome(opts.operation, "cancelled", started);
                return Err(CommandError::Cancelled {
                    program: program.to_owned(),
                });
            }
            DrainOutcome::LimitExceeded(stream) => {
                drain.close_all();
                let _ = child.reap_bounded(PROCESS_KILL_REAP_TIMEOUT);
                record_outcome(opts.operation, "output_limit", started);
                return Err(CommandError::OutputLimitExceeded {
                    program: program.to_owned(),
                    stream,
                    limit: drain.max_output_bytes,
                });
            }
            DrainOutcome::Error(stream, source) => {
                drain.close_all();
                let _ = child.reap_bounded(PROCESS_KILL_REAP_TIMEOUT);
                record_outcome(opts.operation, "error", started);
                return Err(CommandError::PipeRead {
                    program: program.to_owned(),
                    stream,
                    source,
                });
            }
        }
    }

    // Phase 3: all pipes closed and any `-pid` SIGKILL already sent — now reap the zombie.
    let status = match child.reap_bounded(PROCESS_KILL_REAP_TIMEOUT) {
        Ok(Some(status)) => status,
        Ok(None) => {
            record_outcome(opts.operation, "error", started);
            return Err(CommandError::Wait {
                program: program.to_owned(),
                source: io::Error::new(
                    io::ErrorKind::TimedOut,
                    "exited child could not be reaped within PROCESS_KILL_REAP_TIMEOUT",
                ),
            });
        }
        Err(source) => {
            record_outcome(opts.operation, "error", started);
            return Err(CommandError::Wait {
                program: program.to_owned(),
                source,
            });
        }
    };

    record_outcome(
        opts.operation,
        if status.success() { "ok" } else { "nonzero" },
        started,
    );
    Ok(ZeroizingOutput {
        status,
        stdout: drain.stdout_buf,
        stderr: drain.stderr_buf,
    })
}

/// Runs `command` synchronously in an isolated process group or detached session with a hard
/// wall-clock deadline, bounded non-blocking stdin/stdout/stderr handling, safe pre-reap
/// process-group cleanup, and `steve_process_*` metrics.
pub fn run_bounded(
    command: &mut Command,
    program: &str,
    opts: BoundedOptions<'_>,
) -> Result<Output, CommandError> {
    let mut out = run_bounded_core(command, program, opts, false, None)?;
    Ok(Output {
        status: out.status,
        stdout: std::mem::take(&mut *out.stdout),
        stderr: out.stderr,
    })
}

/// Same as [`run_bounded`], returning `stdout` inside `Zeroizing<Vec<u8>>` (pre-allocated when
/// `max_output_bytes <= 64 KiB` so secret payloads do not reallocate onto unzeroized heap chunks).
pub fn run_bounded_zeroizing(
    command: &mut Command,
    program: &str,
    opts: BoundedOptions<'_>,
) -> Result<ZeroizingOutput, CommandError> {
    run_bounded_core(command, program, opts, true, None)
}

struct CancelOnDrop(Arc<AtomicBool>);
impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
}

/// Async counterpart of [`run_bounded`], sharing the exact same single-threaded `poll(2)` +
/// `waitid(WNOWAIT)` engine off the async worker threads and cancelling the child process group
/// promptly if the returned future is dropped.
pub async fn run_bounded_async(
    mut command: Command,
    program: impl Into<String>,
    opts: BoundedOptions<'_>,
) -> Result<Output, CommandError> {
    let program = program.into();
    let owned_stdin: Option<Zeroizing<Vec<u8>>> = match opts.stdin {
        BoundedStdin::Bytes(bytes) => Some(Zeroizing::new(bytes.to_vec())),
        BoundedStdin::Null | BoundedStdin::Preserve => None,
    };
    let stdin_is_preserve = matches!(opts.stdin, BoundedStdin::Preserve);
    let operation = opts.operation;
    let timeout = opts.timeout;
    let isolation = opts.isolation;
    let ignore_stdin_broken_pipe = opts.ignore_stdin_broken_pipe;
    let capture_output = opts.capture_output;
    let max_output_bytes = opts.max_output_bytes;
    let kill_group_after_exit = opts.kill_group_after_exit;
    let held_pipe_policy = opts.held_pipe_policy;
    let detached_env_clear = opts.detached_env_clear;

    let cancelled = Arc::new(AtomicBool::new(false));
    let _guard = CancelOnDrop(cancelled.clone());
    let task_cancel = cancelled.clone();
    let program_for_join = program.clone();

    tokio::task::spawn_blocking(move || {
        let stdin = match owned_stdin.as_deref() {
            Some(bytes) => BoundedStdin::Bytes(bytes.as_slice()),
            None if stdin_is_preserve => BoundedStdin::Preserve,
            None => BoundedStdin::Null,
        };
        let owned_opts = BoundedOptions {
            operation,
            timeout,
            isolation,
            stdin,
            ignore_stdin_broken_pipe,
            capture_output,
            max_output_bytes,
            kill_group_after_exit,
            held_pipe_policy,
            detached_env_clear,
        };
        let mut out = run_bounded_core(
            &mut command,
            &program,
            owned_opts,
            false,
            Some(&task_cancel),
        )?;
        Ok(Output {
            status: out.status,
            stdout: std::mem::take(&mut *out.stdout),
            stderr: out.stderr,
        })
    })
    .await
    .map_err(|err| CommandError::Wait {
        program: program_for_join,
        source: io::Error::other(format!("bounded process task failed: {err}")),
    })?
}

/// Shared non-blocking `poll(2)` + `waitid(WNOWAIT)` driver for an already-spawned testkit child
/// (used by `testkit::process::Command::output_within`). Returns captured partial stdout/stderr on
/// timeout so test failures always show diagnostic output.
#[cfg(feature = "testkit")]
pub(crate) struct TestkitOutputError {
    pub kind: TestkitOutputErrorKind,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
}

#[cfg(feature = "testkit")]
pub(crate) enum TestkitOutputErrorKind {
    TimedOut(Duration),
    PipeDrainTimedOut(&'static str),
    OutputLimitExceeded(&'static str, usize),
    Io(io::Error),
}

#[cfg(feature = "testkit")]
pub(crate) fn collect_spawned_within(
    child: Child,
    stdout_fd: Option<OwnedFd>,
    stderr_fd: Option<OwnedFd>,
    timeout: Duration,
    max_output_bytes: usize,
    mut on_before_reap: impl FnMut(),
) -> Result<Output, TestkitOutputError> {
    let mut child = OwnedChild::from_std(child);
    for fd in [&stdout_fd, &stderr_fd].into_iter().flatten() {
        if let Err(err) = set_nonblocking(fd.as_raw_fd()) {
            on_before_reap();
            child.kill_group_and_leader();
            let _ = child.reap_bounded(PROCESS_KILL_REAP_TIMEOUT);
            return Err(TestkitOutputError {
                kind: TestkitOutputErrorKind::Io(err),
                stdout: Vec::new(),
                stderr: Vec::new(),
            });
        }
    }
    let mut drain = DrainState::new(stdout_fd, stderr_fd, max_output_bytes, false);
    let deadline = Instant::now() + timeout;

    loop {
        match child.exited_unreaped() {
            Ok(true) => break,
            Ok(false) => {}
            Err(err) => {
                on_before_reap();
                child.kill_group_and_leader();
                drain.close_all();
                let _ = child.reap_bounded(PROCESS_KILL_REAP_TIMEOUT);
                return Err(TestkitOutputError {
                    kind: TestkitOutputErrorKind::Io(err),
                    stdout: std::mem::take(&mut *drain.stdout_buf),
                    stderr: drain.stderr_buf,
                });
            }
        }
        let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
            on_before_reap();
            child.kill_group_and_leader();
            let _ = drain.drain_until(Instant::now() + PIPE_DRAIN_GRACE, None);
            drain.close_all();
            let _ = child.reap_bounded(PROCESS_KILL_REAP_TIMEOUT);
            return Err(TestkitOutputError {
                kind: TestkitOutputErrorKind::TimedOut(timeout),
                stdout: std::mem::take(&mut *drain.stdout_buf),
                stderr: drain.stderr_buf,
            });
        };
        if drain.any_open() {
            match drain.poll_step(POLL_INTERVAL.min(remaining)) {
                Ok(()) => {}
                Err((stream, Ok(limit))) => {
                    on_before_reap();
                    child.kill_group_and_leader();
                    drain.close_all();
                    let _ = child.reap_bounded(PROCESS_KILL_REAP_TIMEOUT);
                    return Err(TestkitOutputError {
                        kind: TestkitOutputErrorKind::OutputLimitExceeded(stream, limit),
                        stdout: std::mem::take(&mut *drain.stdout_buf),
                        stderr: drain.stderr_buf,
                    });
                }
                Err((_, Err(err))) => {
                    on_before_reap();
                    child.kill_group_and_leader();
                    drain.close_all();
                    let _ = child.reap_bounded(PROCESS_KILL_REAP_TIMEOUT);
                    return Err(TestkitOutputError {
                        kind: TestkitOutputErrorKind::Io(err),
                        stdout: std::mem::take(&mut *drain.stdout_buf),
                        stderr: drain.stderr_buf,
                    });
                }
            }
        } else {
            std::thread::sleep(POLL_INTERVAL.min(remaining));
        }
    }

    match drain.drain_until(Instant::now() + PIPE_DRAIN_GRACE, None) {
        DrainOutcome::Closed | DrainOutcome::TimedOut => {}
        DrainOutcome::LimitExceeded(stream) => {
            on_before_reap();
            child.kill_group_and_leader();
            drain.close_all();
            let _ = child.reap_bounded(PROCESS_KILL_REAP_TIMEOUT);
            return Err(TestkitOutputError {
                kind: TestkitOutputErrorKind::OutputLimitExceeded(stream, max_output_bytes),
                stdout: std::mem::take(&mut *drain.stdout_buf),
                stderr: drain.stderr_buf,
            });
        }
        DrainOutcome::Error(_, err) => {
            on_before_reap();
            child.kill_group_and_leader();
            drain.close_all();
            let _ = child.reap_bounded(PROCESS_KILL_REAP_TIMEOUT);
            return Err(TestkitOutputError {
                kind: TestkitOutputErrorKind::Io(err),
                stdout: std::mem::take(&mut *drain.stdout_buf),
                stderr: drain.stderr_buf,
            });
        }
        DrainOutcome::Cancelled => unreachable!(),
    }

    // Trigger testkit watchdog teardown and clear the group BEFORE reaping the leader.
    on_before_reap();
    if drain.any_open() {
        child.kill_group();
        if !matches!(
            drain.drain_until(Instant::now() + PIPE_POST_KILL_TIMEOUT, None),
            DrainOutcome::Closed
        ) {
            let stream = drain.open_stream_name();
            drain.close_all();
            let _ = child.reap_bounded(PROCESS_KILL_REAP_TIMEOUT);
            return Err(TestkitOutputError {
                kind: TestkitOutputErrorKind::PipeDrainTimedOut(stream),
                stdout: std::mem::take(&mut *drain.stdout_buf),
                stderr: drain.stderr_buf,
            });
        }
    }

    let status = match child.reap_bounded(PROCESS_KILL_REAP_TIMEOUT) {
        Ok(Some(status)) => status,
        Ok(None) => {
            return Err(TestkitOutputError {
                kind: TestkitOutputErrorKind::Io(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "child could not be reaped",
                )),
                stdout: std::mem::take(&mut *drain.stdout_buf),
                stderr: drain.stderr_buf,
            });
        }
        Err(err) => {
            return Err(TestkitOutputError {
                kind: TestkitOutputErrorKind::Io(err),
                stdout: std::mem::take(&mut *drain.stdout_buf),
                stderr: drain.stderr_buf,
            });
        }
    };

    Ok(Output {
        status,
        stdout: std::mem::take(&mut *drain.stdout_buf),
        stderr: drain.stderr_buf,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn run_bounded_kills_process_group_on_timeout_without_reap_race() {
        let dir = std::env::temp_dir().join(format!(
            "steve-rt-proc-timeout-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&dir).unwrap();
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
        .expect_err("sleeping command must time out");
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
        .expect("pipe-holding grandchild must be killed before reaping");
        assert!(out.status.success());
        assert_eq!(String::from_utf8_lossy(&out.stdout), "payload-ok");
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
        .expect_err("excessive stdout must fail immediately");
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
    fn ignore_stdin_broken_pipe_preserves_early_exit_code() {
        let mut cmd = Command::new("/bin/sh");
        cmd.args(["-c", "exit 11"]);
        let payload = vec![b'A'; 128 * 1024];
        let out = run_bounded_zeroizing(
            &mut cmd,
            "/bin/sh",
            BoundedOptions {
                stdin: BoundedStdin::Bytes(&payload),
                ignore_stdin_broken_pipe: true,
                max_output_bytes: 4096,
                ..BoundedOptions::capture("test_epipe", Duration::from_secs(5))
            },
        )
        .expect("early exit must not be masked by EPIPE");
        assert_eq!(out.status.code(), Some(11));
    }

    #[test]
    fn detached_session_isolates_session_and_resets_signals() {
        use std::os::unix::process::ExitStatusExt;

        // 1. Detached session isolates sid == pgid != parent sid.
        let mut cmd = Command::new("/usr/bin/python3");
        cmd.args([
            "-c",
            "import os; print(os.getpid(), os.getpgrp(), os.getsid(0))",
        ]);
        let parent_sid = unsafe { libc::getsid(0) };
        let out = run_bounded(
            &mut cmd,
            "/usr/bin/python3",
            BoundedOptions {
                isolation: ProcessIsolation::DetachedSession,
                kill_group_after_exit: true,
                ..BoundedOptions::capture("test_detached", Duration::from_secs(10))
            },
        )
        .expect("detached session spawn");
        assert!(
            out.status.success(),
            "{:?}",
            String::from_utf8_lossy(&out.stderr)
        );
        let text = String::from_utf8_lossy(&out.stdout);
        let parts: Vec<i32> = text
            .split_whitespace()
            .map(|s| s.parse::<i32>().unwrap())
            .collect();
        assert_eq!(parts.len(), 3, "unexpected output: {text}");
        let (pid, pgid, sid) = (parts[0], parts[1], parts[2]);
        assert_eq!(pid, pgid, "detached child must lead its own process group");
        assert_eq!(pgid, sid, "detached child must lead its own session");
        assert_ne!(
            sid, parent_sid,
            "detached child must not share parent session"
        );

        // 2. SIGPIPE must be restored to SIG_DFL (not inherited as Rust's SIG_IGN).
        let mut sigpipe_cmd = Command::new("/bin/sh");
        sigpipe_cmd.args(["-c", "kill -PIPE $$"]);
        let sigpipe_out = run_bounded(
            &mut sigpipe_cmd,
            "/bin/sh",
            BoundedOptions {
                isolation: ProcessIsolation::DetachedSession,
                ..BoundedOptions::capture("test_sigpipe", Duration::from_secs(5))
            },
        )
        .expect("sigpipe test spawn");
        assert_eq!(
            sigpipe_out.status.signal(),
            Some(libc::SIGPIPE),
            "SIGPIPE must be reset to SIG_DFL under DetachedSession"
        );

        // 3. Signal mask must be cleared even when the calling thread masks SIGUSR1.
        let mut block: libc::sigset_t = unsafe { std::mem::zeroed() };
        let mut prev: libc::sigset_t = unsafe { std::mem::zeroed() };
        unsafe {
            libc::sigemptyset(&mut block);
            libc::sigaddset(&mut block, libc::SIGUSR1);
            assert_eq!(libc::pthread_sigmask(libc::SIG_BLOCK, &block, &mut prev), 0);
        }
        let mut mask_cmd = Command::new("/bin/sh");
        mask_cmd.args(["-c", "kill -USR1 $$"]);
        let mask_out = run_bounded(
            &mut mask_cmd,
            "/bin/sh",
            BoundedOptions {
                isolation: ProcessIsolation::DetachedSession,
                ..BoundedOptions::capture("test_sigmask", Duration::from_secs(5))
            },
        );
        unsafe {
            libc::pthread_sigmask(libc::SIG_SETMASK, &prev, std::ptr::null_mut());
        }
        let mask_out = mask_out.expect("sigmask test spawn");
        assert_eq!(
            mask_out.status.signal(),
            Some(libc::SIGUSR1),
            "signal mask must be cleared under DetachedSession"
        );
    }

    #[tokio::test]
    async fn run_bounded_async_cancels_and_kills_group_on_drop() {
        let dir = std::env::temp_dir().join(format!(
            "steve-rt-proc-cancel-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&dir).unwrap();
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
        let timed = tokio::time::timeout(Duration::from_millis(250), fut).await;
        assert!(timed.is_err(), "outer timeout must cancel future");
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
    fn stop_child_group_sync_kills_descendants_before_reaping_and_is_idempotent() {
        let dir = std::env::temp_dir().join(format!(
            "steve-rt-stop-sync-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        fs::create_dir(&dir).unwrap();
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
        let status = stop_child_group_sync(
            &mut child,
            &mut reaped,
            TermTarget::Group,
            Duration::from_millis(50),
            PROCESS_KILL_REAP_TIMEOUT,
        )
        .unwrap();
        assert!(status.is_some());
        assert!(reaped);
        let deadline = Instant::now() + Duration::from_secs(2);
        while unsafe { libc::kill(desc, 0) } == 0 && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(10));
        }
        assert_eq!(unsafe { libc::kill(desc, 0) }, -1);
        // Second call is a no-op because `reaped` is already true.
        assert!(
            stop_child_group_sync(
                &mut child,
                &mut reaped,
                TermTarget::Group,
                Duration::from_millis(50),
                PROCESS_KILL_REAP_TIMEOUT,
            )
            .unwrap()
            .is_none()
        );
        let _ = fs::remove_dir_all(&dir);
    }
}
