//! Test subprocesses owned by a CLOEXEC lifeline, including when the test is SIGKILLed.
//! The shell execs the target in place: socket activation, child PIDs and PTYs keep their meaning.
//! Only groups whose live leaders are reachable in the teardown snapshot are covered. Descendants
//! reparented after their group leader exits, double-fork daemons and post-snapshot spawns require
//! separate fixture ownership. This is not a process-container or subreaper contract.
use std::ffi::OsStr;
use std::io::{self, Read, Write};
use std::net::Shutdown;
use std::ops::{Deref, DerefMut};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{ExitStatus, Output, Stdio};
use std::sync::{Mutex, MutexGuard};
use std::time::{Duration, Instant};

const FD_ENV: &str = "_STEVE_TEST_LIFELINE_FD";
const PARENT_ENV: &str = "_STEVE_TEST_PARENT_PID";
const SIGKILL: i32 = 9;
const SIGTERM: i32 = 15;
/// Maximum wait for the watchdog subshell to finish killing descendant groups before falling back
/// to direct process-group SIGKILL.
const WATCHDOG_TEARDOWN_TIMEOUT: Duration = Duration::from_secs(3);
/// Bounded wait to reap an async child after SIGKILL in `AsyncProcessGuard::drop`.
const ASYNC_DROP_REAP_TIMEOUT: Duration = Duration::from_millis(500);
static SPAWN_LOCK: Mutex<()> = Mutex::new(());
// length(array) needs mawk >= 1.3.4 (or the system BSD/GNU awk).
const GROUPS: &str = r#"
    { parent[$1]=$2; group[$1]=$3 }
    END {
        seen[root]=1; depth[root]=0
        do {
            changed=0
            for (p in parent) if (!seen[p] && seen[parent[p]]) { seen[p]=1; depth[p]=depth[parent[p]]+1; changed=1 }
        } while (changed)
        # Reading a missing awk key creates it: membership alone is not reachability.
        for (p in parent) if (seen[p] && group[p]>0 && group[p]!=root && seen[group[p]]) groups[group[p]]=1
        while (length(groups)) {
            deepest=""
            for (g in groups) if (deepest=="" || depth[g]>depth[deepest]) deepest=g
            print deepest
            delete groups[deepest]
        }
    }
"#;
// Keep the watcher in the owner group as its PID pin. Kill only the owner process first,
// freeze descendants before any child-group death can trigger a restart, and retain stopped
// parents until their children have been removed. A failed STOP is never followed by a stale kill.
// This is also exercised with a recording kill builtin, without sending real signals.
const TEARDOWN: &str = r#"
kill -KILL "$owner" 2>/dev/null || :
stopped=""
for group in $groups; do
    if kill -STOP -- "-$group" 2>/dev/null; then stopped="$stopped $group"; fi
done
for group in $stopped; do kill -KILL -- "-$group" 2>/dev/null || :; done
kill -KILL -- "-$owner"
"#;
const WATCH: &str = r#"
owner=$$
parent=$_STEVE_TEST_PARENT_PID
life=$_STEVE_TEST_LIFELINE_FD
selector=$1
teardown=$2
shift 2
unset _STEVE_TEST_LIFELINE_FD _STEVE_TEST_PARENT_PID
(
    trap '' HUP INT TERM
    # Bash 3 reports both timeout and EOF as status 1. Explicit release writes a line;
    # on EOF/timeout inspect the original direct child's parent, not a cached PID's existence.
    while ! IFS= read -r -t 1 -u "$life"; do
        kill -0 "$owner" 2>/dev/null || break
        current=$(/bin/ps -p "$owner" -o ppid= 2>/dev/null | /usr/bin/awk '{$1=$1; print}')
        # A transient fork/ps failure is not evidence that parentage changed.
        [ -z "$current" ] || [ "$current" = "$parent" ] || break
    done
    groups=$(/bin/ps -A -o pid=,ppid=,pgid= 2>/dev/null | /usr/bin/awk -v root="$owner" "$selector")
    eval "$teardown"
) </dev/null >/dev/null 2>&1 &
eval "exec $life<&-"
exec "$@"
"#;

unsafe extern "C" {
    fn fcntl(fd: i32, cmd: i32, ...) -> i32;
    fn kill(pid: i32, signal: i32) -> i32;
    fn getpgid(pid: i32) -> i32;
}

#[derive(Debug)]
struct LifeWriter {
    stream: UnixStream,
    fired: bool,
}
impl LifeWriter {
    fn new(stream: UnixStream) -> Self {
        Self {
            stream,
            fired: false,
        }
    }
    /// Wake the watchdog and wait (bounded by [`WATCHDOG_TEARDOWN_TIMEOUT`]) for it to finish
    /// killing descendant groups and `-$owner` (signalled by EOF when the watchdog exits).
    fn teardown(mut self) -> bool {
        self.fired = true;
        let _ = self.stream.write_all(b"\n");
        let _ = self.stream.shutdown(Shutdown::Write);
        let _ = self
            .stream
            .set_read_timeout(Some(WATCHDOG_TEARDOWN_TIMEOUT));
        let mut buf = [0u8; 16];
        loop {
            match self.stream.read(&mut buf) {
                Ok(0) => return true,
                Ok(_) => continue,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => return false,
            }
        }
    }
    /// Disarm without triggering descendant teardown (used only for intentional crash simulation).
    fn disarm(mut self) {
        self.fired = true;
    }
}
impl AsRawFd for LifeWriter {
    fn as_raw_fd(&self) -> i32 {
        self.stream.as_raw_fd()
    }
}
impl Drop for LifeWriter {
    fn drop(&mut self) {
        if !self.fired {
            // Release the same channel even if an uncontrolled spawn inherited another writer.
            let _ = self.stream.write_all(b"\n");
            let _ = self.stream.shutdown(Shutdown::Write);
        }
    }
}
struct Lifeline {
    _read: OwnedFd,
    write: LifeWriter,
}
struct Prepared {
    life: Lifeline,
    _lock: MutexGuard<'static, ()>,
}
impl Prepared {
    fn new() -> io::Result<Self> {
        let lock = SPAWN_LOCK.lock().unwrap_or_else(|e| e.into_inner());
        Ok(Self {
            life: Lifeline::new()?,
            _lock: lock,
        })
    }
    fn release(self) -> LifeWriter {
        let Self { life, _lock } = self;
        let Lifeline { _read, write } = life;
        // The next guarded spawn must not inherit the preceding spawn's read descriptor.
        drop(_read);
        drop(_lock);
        write
    }
}
impl Lifeline {
    fn new() -> io::Result<Self> {
        // Both ends are CLOEXEC on return, but macOS sets that flag after socketpair.
        // Prepared serializes this window with guarded spawns. Uncontrolled spawns are
        // covered by the watchdog's coarse parentage check, not by this process-local lock.
        let (read, write) = UnixStream::pair()?;
        // SAFETY: F_DUPFD (0 on supported Unix platforms) duplicates a live descriptor. Keep
        // it above socket-activation descriptors 3..; OwnedFd closes the parent's copy.
        let fd = unsafe { fcntl(read.as_raw_fd(), 0, 64) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            _read: unsafe { OwnedFd::from_raw_fd(fd) },
            write: LifeWriter::new(write),
        })
    }
}
fn command(program: impl AsRef<OsStr>, watch: &str) -> std::process::Command {
    let mut c = std::process::Command::new("/bin/bash");
    // Privileged mode ignores BASH_ENV and imported functions without changing the target's env.
    c.args([
        "--noprofile",
        "--norc",
        "-p",
        "-c",
        watch,
        "steve-test-watchdog",
        GROUPS,
        TEARDOWN,
    ])
    .arg(program);
    c
}
fn kill_group(pid: u32) {
    // SAFETY: every guarded spawn starts its own group; never address the test's own group.
    assert!(pid > 1 && pid <= i32::MAX as u32);
    unsafe { kill(-(pid as i32), 9) };
}

macro_rules! builders {
    () => {
        pub fn arg(&mut self, arg: impl AsRef<OsStr>) -> &mut Self {
            self.inner.arg(arg);
            self
        }
        pub fn args<I, S>(&mut self, args: I) -> &mut Self
        where
            I: IntoIterator<Item = S>,
            S: AsRef<OsStr>,
        {
            self.inner.args(args);
            self
        }
        pub fn env(&mut self, key: impl AsRef<OsStr>, value: impl AsRef<OsStr>) -> &mut Self {
            self.inner.env(key, value);
            self
        }
        pub fn envs<I, K, V>(&mut self, envs: I) -> &mut Self
        where
            I: IntoIterator<Item = (K, V)>,
            K: AsRef<OsStr>,
            V: AsRef<OsStr>,
        {
            self.inner.envs(envs);
            self
        }
        pub fn env_clear(&mut self) -> &mut Self {
            self.inner.env_clear();
            self
        }
        pub fn env_remove(&mut self, key: impl AsRef<OsStr>) -> &mut Self {
            self.inner.env_remove(key);
            self
        }
        pub fn current_dir(&mut self, dir: impl AsRef<Path>) -> &mut Self {
            self.inner.current_dir(dir);
            self
        }
        pub fn stdin(&mut self, io: impl Into<Stdio>) -> &mut Self {
            self.note_stdio(0);
            self.inner.stdin(io);
            self
        }
        pub fn stdout(&mut self, io: impl Into<Stdio>) -> &mut Self {
            self.note_stdio(1);
            self.inner.stdout(io);
            self
        }
        pub fn stderr(&mut self, io: impl Into<Stdio>) -> &mut Self {
            self.note_stdio(2);
            self.inner.stderr(io);
            self
        }
        pub fn process_group(&mut self, group: i32) -> &mut Self {
            assert_eq!(group, 0, "test children must own their process group");
            self.inner.process_group(0);
            self
        }
        fn prepare(&mut self) -> io::Result<Prepared> {
            let prepared = Prepared::new()?;
            self.inner
                .env(FD_ENV, prepared.life._read.as_raw_fd().to_string())
                .env(PARENT_ENV, std::process::id().to_string())
                .process_group(0);
            Ok(prepared)
        }
    };
}

/// A std command whose builder chains cannot bypass the lifeline at spawn/output/status.
pub struct Command {
    inner: std::process::Command,
    stdio: [bool; 3],
}
impl Command {
    pub fn new(program: impl AsRef<OsStr>) -> Self {
        Self {
            inner: command(program, WATCH),
            stdio: [false; 3],
        }
    }
    builders!();
    fn note_stdio(&mut self, index: usize) {
        self.stdio[index] = true;
    }
    pub fn spawn(&mut self) -> io::Result<ProcessGuard> {
        let prepared = self.prepare()?;
        let child = self.inner.spawn()?;
        let life = prepared.release();
        Ok(ProcessGuard {
            pid: child.id(),
            inner: Some(child),
            life: Some(life),
        })
    }
    pub fn output(&mut self) -> io::Result<Output> {
        if !self.stdio[0] {
            self.inner.stdin(Stdio::null());
        }
        if !self.stdio[1] {
            self.inner.stdout(Stdio::piped());
        }
        if !self.stdio[2] {
            self.inner.stderr(Stdio::piped());
        }
        let child = self.spawn();
        // Restore the defaults for a later spawn; explicitly configured IO is untouched.
        if !self.stdio[0] {
            self.inner.stdin(Stdio::inherit());
        }
        if !self.stdio[1] {
            self.inner.stdout(Stdio::inherit());
        }
        if !self.stdio[2] {
            self.inner.stderr(Stdio::inherit());
        }
        child?.wait_with_output()
    }
    /// Run the guarded command with a hard wall-clock deadline, bounded pipe draining, and safe
    /// pre-reap teardown. On timeout or error, returns a human-readable diagnostic containing any
    /// partial stdout/stderr already captured.
    pub fn output_within(&mut self, timeout: Duration) -> Result<Output, String> {
        if !self.stdio[0] {
            self.inner.stdin(Stdio::null());
        }
        if !self.stdio[1] {
            self.inner.stdout(Stdio::piped());
        }
        if !self.stdio[2] {
            self.inner.stderr(Stdio::piped());
        }
        let prepared = self
            .prepare()
            .map_err(|e| format!("prepare test command: {e}"))?;
        let spawned = self.inner.spawn();
        if !self.stdio[0] {
            self.inner.stdin(Stdio::inherit());
        }
        if !self.stdio[1] {
            self.inner.stdout(Stdio::inherit());
        }
        if !self.stdio[2] {
            self.inner.stderr(Stdio::inherit());
        }
        let mut child = spawned.map_err(|e| format!("spawn test command: {e}"))?;
        let mut life = Some(prepared.release());
        let pid = child.id();
        let stdout_fd = child.stdout.take().map(OwnedFd::from);
        let stderr_fd = child.stderr.take().map(OwnedFd::from);
        crate::process::bounded::collect_spawned_within(
            child,
            stdout_fd,
            stderr_fd,
            timeout,
            crate::process::DEFAULT_MAX_OUTPUT_BYTES,
            || {
                if let Some(writer) = life.take() {
                    writer.teardown();
                }
            },
        )
        .map_err(|err| {
            let reason = match err.kind {
                crate::process::bounded::TestkitOutputErrorKind::TimedOut(d) => {
                    format!("timed out after {d:?}")
                }
                crate::process::bounded::TestkitOutputErrorKind::PipeDrainTimedOut(stream) => {
                    format!("{stream} pipe did not close after exit")
                }
                crate::process::bounded::TestkitOutputErrorKind::OutputLimitExceeded(
                    stream,
                    limit,
                ) => format!("{stream} exceeded {limit} bytes"),
                crate::process::bounded::TestkitOutputErrorKind::Io(e) => format!("I/O error: {e}"),
            };
            format!(
                "test command (pid {pid}) {reason}\nstdout:\n{}\nstderr:\n{}",
                String::from_utf8_lossy(&err.stdout),
                String::from_utf8_lossy(&err.stderr),
            )
        })
    }
    pub fn status(&mut self) -> io::Result<ExitStatus> {
        self.spawn()?.wait()
    }
}

/// Guard for a test child process in its own process group.
///
/// Kills the entire process group (and reachable descendant groups via the lifeline watchdog)
/// on explicit [`kill`](Self::kill), normal exit, and [`Drop`] (including unwind on panic), and
/// reaps the direct child so no zombies or orphan descendants leak.
#[derive(Debug)]
pub struct ProcessGuard {
    inner: Option<std::process::Child>,
    pid: u32,
    life: Option<LifeWriter>,
}
impl ProcessGuard {
    /// Spawn a `std::process::Command` directly in its own process group (`process_group(0)`)
    /// without injecting a watchdog subshell into the group.
    ///
    /// Prefer [`Command::spawn`] by default so descendant process groups are also covered on test
    /// SIGKILL; use `spawn_direct` only when the test itself inspects the child's exact process
    /// group membership (e.g. `procs::started_by` or self-killing orphan tests).
    pub fn spawn_direct(command: &mut std::process::Command) -> io::Result<Self> {
        command.process_group(0);
        let child = command.spawn()?;
        Ok(Self {
            pid: child.id(),
            inner: Some(child),
            life: None,
        })
    }
    fn finish(&mut self) {
        if let Some(life) = self.life.take() {
            life.teardown();
        }
    }
    fn still_unreaped(&mut self) -> bool {
        self.inner
            .as_mut()
            .is_some_and(|c| matches!(c.try_wait(), Ok(None)))
    }
    pub fn id(&self) -> u32 {
        self.pid
    }
    /// Kill the child's entire process group and reachable descendant groups via the watchdog,
    /// falling back to direct process-group SIGKILL if still running.
    pub fn kill(&mut self) -> io::Result<()> {
        if let Some(life) = self.life.take() {
            life.teardown();
        }
        if self.still_unreaped() {
            kill_group(self.pid);
            let _ = self.inner.as_mut().unwrap().kill();
        }
        Ok(())
    }
    /// Simulate an abrupt crash of ONLY this child's process group (killing the watchdog with it)
    /// so detached descendant process groups intentionally survive for crash-recovery tests, then
    /// reap the direct child.
    pub fn simulate_crash(&mut self) -> io::Result<()> {
        if self.still_unreaped() {
            kill_group(self.pid);
            let _ = self.inner.as_mut().unwrap().wait();
        }
        if let Some(life) = self.life.take() {
            life.disarm();
        }
        Ok(())
    }
    /// Ask the process to exit gracefully with `SIGTERM`, waiting up to `grace` before forcing
    /// full process-group and descendant teardown with `SIGKILL` and reaping.
    pub fn terminate(&mut self, grace: Duration) {
        if !self.still_unreaped() {
            self.finish();
            return;
        }
        // SAFETY: unreaped child pins its PID.
        unsafe { kill(self.pid as i32, SIGTERM) };
        let deadline = Instant::now() + grace;
        while Instant::now() < deadline {
            if !self.still_unreaped() {
                self.finish();
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = self.kill();
        if let Some(child) = &mut self.inner {
            let _ = child.wait();
        }
    }
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
        let result = self.inner.as_mut().unwrap().wait();
        if result.is_ok() {
            self.finish();
        }
        result
    }
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        let result = self.inner.as_mut().unwrap().try_wait();
        if matches!(result, Ok(Some(_))) {
            self.finish();
        }
        result
    }
    pub fn wait_with_output(mut self) -> io::Result<Output> {
        let result = self.inner.take().unwrap().wait_with_output();
        self.finish();
        result
    }
}
impl Deref for ProcessGuard {
    type Target = std::process::Child;
    fn deref(&self) -> &Self::Target {
        self.inner.as_ref().unwrap()
    }
}
impl DerefMut for ProcessGuard {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.inner.as_mut().unwrap()
    }
}
impl Drop for ProcessGuard {
    fn drop(&mut self) {
        self.finish();
        if self.still_unreaped() {
            kill_group(self.pid);
        }
        if let Some(child) = &mut self.inner {
            let _ = child.wait();
        }
    }
}

/// RAII guard for a detached process group (e.g. an orphaned fixture group in a crash-recovery
/// test). Sends `SIGKILL` to `-pgid` on [`Drop`] (including unwind on panic) unless disarmed.
#[derive(Debug)]
pub struct GroupGuard(Option<i32>);

impl GroupGuard {
    /// Guard process group `pgid` (must be `> 1`, never the test's own group).
    pub fn new(pgid: i32) -> Self {
        let own = unsafe { getpgid(0) };
        if pgid > 1 && pgid != own {
            Self(Some(pgid))
        } else {
            Self(None)
        }
    }
    /// Resolve and guard the process group currently owning `pid`.
    pub fn for_pid(pid: i32) -> Self {
        if pid <= 1 {
            return Self(None);
        }
        let pgid = unsafe { getpgid(pid) };
        if pgid > 1 {
            Self::new(pgid)
        } else {
            Self::new(pid)
        }
    }
    /// Kill a single known process group immediately (must be `> 1` and not the test's own group).
    pub fn kill_pgid(pgid: i32) {
        let own = unsafe { getpgid(0) };
        if pgid > 1 && pgid != own {
            // SAFETY: caller proved ownership of `pgid`; never signals the test's own group.
            unsafe { kill(-pgid, SIGKILL) };
        }
    }
    pub fn disarm(&mut self) {
        self.0 = None;
    }
    pub fn kill_now(&mut self) {
        if let Some(pgid) = self.0.take() {
            Self::kill_pgid(pgid);
        }
    }
}

impl Drop for GroupGuard {
    fn drop(&mut self) {
        self.kill_now();
    }
}

/// Tokio counterpart, using exactly the same shell and lifeline as [`Command`].
pub struct AsyncCommand {
    inner: tokio::process::Command,
}
impl AsyncCommand {
    pub fn new(program: impl AsRef<OsStr>) -> Self {
        Self {
            inner: command(program, WATCH).into(),
        }
    }
    builders!();
    fn note_stdio(&mut self, _: usize) {}
    pub fn kill_on_drop(&mut self, _: bool) -> &mut Self {
        // The EOF watchdog owns teardown, including descendants, rather than a leader-only kill.
        self.inner.kill_on_drop(false);
        self
    }
    pub fn spawn(&mut self) -> io::Result<AsyncProcessGuard> {
        let prepared = self.prepare()?;
        self.inner.kill_on_drop(false);
        let child = self.inner.spawn()?;
        let life = prepared.release();
        Ok(AsyncProcessGuard {
            pid: child.id().expect("spawned child PID"),
            inner: Some(child),
            life: Some(life),
        })
    }
    pub async fn output(&mut self) -> io::Result<Output> {
        let (life, output) = {
            let prepared = self.prepare()?;
            self.inner.kill_on_drop(false);
            // Tokio 1.54 output() calls spawn synchronously, then returns its wait future.
            let output = self.inner.output();
            (prepared.release(), output)
        };
        let _life = life;
        output.await
    }
    pub async fn status(&mut self) -> io::Result<ExitStatus> {
        self.spawn()?.wait().await
    }
}
#[derive(Debug)]
pub struct AsyncProcessGuard {
    inner: Option<tokio::process::Child>,
    pid: u32,
    life: Option<LifeWriter>,
}
impl AsyncProcessGuard {
    fn finish(&mut self) {
        if let Some(life) = self.life.take() {
            life.teardown();
        }
    }
    fn still_unreaped(&mut self) -> bool {
        self.inner
            .as_mut()
            .is_some_and(|c| c.id().is_some() && matches!(c.try_wait(), Ok(None)))
    }
    pub fn id(&self) -> Option<u32> {
        self.inner.as_ref().unwrap().id()
    }
    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        let result = self.inner.as_mut().unwrap().try_wait();
        if matches!(result, Ok(Some(_))) {
            self.finish();
        }
        result
    }
    pub async fn wait(&mut self) -> io::Result<ExitStatus> {
        let result = self.inner.as_mut().unwrap().wait().await;
        if result.is_ok() {
            self.finish();
        }
        result
    }
    pub fn start_kill(&mut self) -> io::Result<()> {
        if let Some(life) = self.life.take() {
            life.teardown();
        }
        if self.still_unreaped() {
            kill_group(self.pid);
            let _ = self.inner.as_mut().unwrap().start_kill();
        }
        Ok(())
    }
    pub async fn kill(&mut self) -> io::Result<()> {
        self.start_kill()?;
        self.wait().await?;
        Ok(())
    }
    pub async fn wait_with_output(mut self) -> io::Result<Output> {
        let result = self.inner.take().unwrap().wait_with_output().await;
        self.finish();
        result
    }
}
impl Deref for AsyncProcessGuard {
    type Target = tokio::process::Child;
    fn deref(&self) -> &Self::Target {
        self.inner.as_ref().unwrap()
    }
}
impl DerefMut for AsyncProcessGuard {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.inner.as_mut().unwrap()
    }
}
impl Drop for AsyncProcessGuard {
    fn drop(&mut self) {
        self.finish();
        if self.still_unreaped() {
            kill_group(self.pid);
            let _ = self.inner.as_mut().unwrap().start_kill();
        }
        if let Some(child) = &mut self.inner
            && child.id().is_some()
        {
            let deadline = Instant::now() + ASYNC_DROP_REAP_TIMEOUT;
            while Instant::now() < deadline {
                if !matches!(child.try_wait(), Ok(None)) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(5));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testkit::eventually_sync;
    use std::{fs, io::Write, path::PathBuf};

    struct Fixture(PathBuf);
    impl Fixture {
        fn new() -> Self {
            let dir = std::env::temp_dir().join(format!(
                "steve-process-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
            ));
            fs::create_dir(&dir).unwrap();
            Self(dir)
        }
        fn pids(&self, name: &str) -> Vec<i32> {
            fs::read_to_string(self.0.join(name))
                .unwrap_or_default()
                .split_whitespace()
                .filter_map(|p| p.parse().ok())
                .collect()
        }
    }
    impl Drop for Fixture {
        fn drop(&mut self) {
            // Also cover assertion failures in this test of the cleanup mechanism itself.
            for name in ["ready", "unrelated"] {
                for pid in self.pids(name) {
                    if pid > 1 && unsafe { get_group(pid) } == pid {
                        unsafe {
                            kill(-pid, 9);
                        }
                    }
                }
            }
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    // kill(0) also sees zombies. Orphan/group disappearance relies on init (or the
    // runner's subreaper) reaping them; Linux CI runs these tests on an Ubuntu VM.
    fn gone(pid: i32) -> bool {
        (unsafe { kill(pid, 0) }) == -1 && io::Error::last_os_error().raw_os_error() == Some(3)
    }
    fn target(path: &Path) -> Command {
        let mut c = Command::new("/bin/sh");
        c.args([
            "-c",
            "/bin/sleep 600 & child=$!; printf '%s %s\\n' \"$$\" \"$child\" > \"$1\"; wait",
            "fixture",
        ])
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
        c
    }

    #[tokio::test]
    async fn failed_parent_probe_does_not_kill_a_healthy_owner() {
        let fixture = Fixture::new();
        // Replace only the parentage probe; exercise the actual timeout loop and teardown.
        let watch = WATCH
            .replacen(
                "owner=$$",
                r#"failed_probe() { printf 'probe\n' >> "$PROBES"; return 1; }
owner=$$"#,
                1,
            )
            .replace("/bin/ps -p \"$owner\" -o ppid=", "failed_probe");
        let mut command = Command {
            inner: command("/bin/sleep", &watch),
            stdio: [false; 3],
        };
        let mut child = command
            .arg("600")
            .env("PROBES", fixture.0.join("probes"))
            .spawn()
            .unwrap();
        eventually_sync("two failed parent probes while owner stays alive", || {
            child.try_wait().unwrap().is_some()
                || fs::read_to_string(fixture.0.join("probes"))
                    .unwrap_or_default()
                    .lines()
                    .count()
                    >= 2
        })
        .await;
        assert!(child.try_wait().unwrap().is_none());
        drop(child);
    }

    #[test]
    fn releasing_the_spawn_lock_first_closes_the_inheritable_reader() {
        let prepared = Prepared::new().unwrap();
        let reader = prepared.life._read.as_raw_fd();
        let contender = std::thread::spawn(move || {
            let _lock = SPAWN_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            // F_GETFD is 1 on the supported Unix platforms. No descriptor is allocated here.
            unsafe { fcntl(reader, 1) }
        });
        let writer = prepared.release();
        assert_eq!(contender.join().unwrap(), -1);
        drop(writer);
    }

    #[test]
    fn group_selection_never_includes_unrelated_or_shared_groups() {
        let mut selector = std::process::Command::new("/usr/bin/awk");
        selector
            .args(["-v", "root=10", GROUPS])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        let mut child = selector.spawn().unwrap();
        child.stdin.take().unwrap().write_all(
            b"1 0 1\n14 13 14\n13 12 12\n12 11 12\n11 10 10\n10 1 10\n20 1 20\n21 20 21\n22 999 22\n30 10 20\n"
        ).unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success());
        assert_eq!(String::from_utf8(out.stdout).unwrap().trim(), "14\n12");
        let out = std::process::Command::new("/usr/bin/awk")
            .args(["-v", "root=777", GROUPS])
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(out.status.success() && out.stdout.is_empty());
    }

    #[test]
    fn teardown_kills_owner_before_frozen_descendants_and_keeps_its_pin_until_last() {
        let script =
            format!(r#"kill() {{ printf '%s\n' "$*"; }}; owner=10; groups='14 12'; {TEARDOWN}"#);
        let out = std::process::Command::new("/bin/bash")
            .args(["--noprofile", "--norc", "-p", "-c", &script])
            .output()
            .unwrap();
        assert!(out.status.success());
        assert_eq!(
            String::from_utf8(out.stdout)
                .unwrap()
                .lines()
                .collect::<Vec<_>>(),
            [
                "-KILL 10",
                "-STOP -- -14",
                "-STOP -- -12",
                "-KILL -- -14",
                "-KILL -- -12",
                "-KILL -- -10"
            ]
        );
    }

    #[test]
    fn teardown_does_not_kill_a_group_that_could_not_be_stopped() {
        let script = format!(
            r#"kill() {{ printf '%s\n' "$*"; [ "$*" != '-STOP -- -12' ]; }}; owner=10; groups='14 12'; {TEARDOWN}"#
        );
        let out = std::process::Command::new("/bin/bash")
            .args(["--noprofile", "--norc", "-p", "-c", &script])
            .output()
            .unwrap();
        assert!(out.status.success());
        assert!(
            !String::from_utf8(out.stdout)
                .unwrap()
                .contains("-KILL -- -12")
        );
    }

    #[test]
    fn parent_helper() {
        let Some(dir) = std::env::var_os("STEVE_TEST_PROCESS_PARENT") else {
            return;
        };
        let dir = PathBuf::from(dir);
        if std::env::var("STEVE_TEST_PROCESS_ESCAPED").as_deref() == Ok("1") {
            let _child = Command::new(std::env::current_exe().unwrap())
                .args([
                    "--exact",
                    "testkit::process::tests::descendant_helper",
                    "--nocapture",
                ])
                .env("STEVE_TEST_PROCESS_DESCENDANT", &dir)
                .stdin(Stdio::null())
                .stdout(Stdio::null())
                .stderr(Stdio::inherit())
                .spawn()
                .unwrap();
            loop {
                std::thread::park();
            }
        } else if std::env::var("STEVE_TEST_PROCESS_ASYNC").as_deref() == Ok("1") {
            tokio::runtime::Builder::new_current_thread().enable_all().build().unwrap().block_on(async {
                let mut c = AsyncCommand::new("/bin/sh");
                c.args(["-c", "/bin/sleep 600 & child=$!; printf '%s %s\\n' \"$$\" \"$child\" > \"$1\"; wait", "fixture"])
                    .arg(dir.join("ready")).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
                let _child = c.spawn().unwrap();
                std::future::pending::<()>().await;
            });
        } else {
            let _child = target(&dir.join("ready")).spawn().unwrap();
            // This raw fixture must outlive the parent and must not inherit the guarded child's
            // write end. It is deliberately outside that child's tree and is reaped by Fixture.
            // Deliberately model a macOS writer inheritance race in an uncontrolled spawn.
            let inherited = if std::env::var("STEVE_TEST_PROCESS_LEAK").as_deref() == Ok("1") {
                let fd = unsafe { fcntl(_child.life.as_ref().unwrap().as_raw_fd(), 0, 64) };
                assert!(fd >= 0);
                Some(unsafe { OwnedFd::from_raw_fd(fd) })
            } else {
                None
            };
            let other = std::process::Command::new("/bin/sleep")
                .arg("600")
                .process_group(0)
                .spawn()
                .unwrap();
            drop(inherited);
            fs::write(dir.join("unrelated"), other.id().to_string()).unwrap();
            loop {
                std::thread::park();
            }
        }
    }

    #[test]
    fn descendant_helper() {
        let Some(dir) = std::env::var_os("STEVE_TEST_PROCESS_DESCENDANT") else {
            return;
        };
        let child = std::process::Command::new("/bin/sleep")
            .arg("600")
            .process_group(0)
            .spawn()
            .unwrap();
        fs::write(
            PathBuf::from(dir).join("ready"),
            format!("{} {}", std::process::id(), child.id()),
        )
        .unwrap();
        loop {
            std::thread::park();
        }
    }

    async fn parent_death(asynchronous: bool, escaped: bool, leaked: bool) {
        let fixture = Fixture::new();
        let mut parent = Command::new(std::env::current_exe().unwrap());
        parent
            .args([
                "--exact",
                "testkit::process::tests::parent_helper",
                "--nocapture",
            ])
            .env("STEVE_TEST_PROCESS_PARENT", &fixture.0)
            .env("STEVE_TEST_PROCESS_LEAK", if leaked { "1" } else { "0" })
            .env(
                "STEVE_TEST_PROCESS_ASYNC",
                if asynchronous { "1" } else { "0" },
            )
            .env(
                "STEVE_TEST_PROCESS_ESCAPED",
                if escaped { "1" } else { "0" },
            )
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit())
            .process_group(0);
        let mut parent = parent.spawn().unwrap();
        eventually_sync("the guarded target and descendant ready", || {
            fixture.pids("ready").len() == 2
        })
        .await;
        let pids = fixture.pids("ready");
        assert_eq!(pids[0], unsafe { get_group(pids[0]) });
        if !asynchronous && !escaped {
            eventually_sync("the unrelated process ready", || {
                fixture.pids("unrelated").len() == 1
            })
            .await;
        }
        // Kill only the parent, not its group; neither Drop nor a signal handler can help.
        assert_eq!(unsafe { kill(parent.id() as i32, 9) }, 0);
        parent.wait().unwrap();
        eventually_sync(
            "SIGKILL parent removes the target, descendant and process group",
            || pids.iter().all(|p| gone(*p)) && gone(-pids[0]) && (!escaped || gone(-pids[1])),
        )
        .await;
        fs::remove_file(fixture.0.join("ready")).unwrap();
        if !asynchronous && !escaped {
            assert!(
                !gone(fixture.pids("unrelated")[0]),
                "an unrelated child remains alive without keeping the lifeline open"
            );
        }
    }
    unsafe extern "C" {
        #[link_name = "getpgid"]
        fn get_group(pid: i32) -> i32;
    }

    #[tokio::test]
    async fn sigkill_parent_reaps_std_child_and_its_group_without_drop() {
        parent_death(false, false, false).await;
    }
    #[tokio::test]
    async fn sigkill_parent_reaps_tokio_child_and_its_group_without_drop() {
        parent_death(true, false, false).await;
    }

    #[tokio::test]
    async fn sigkill_parent_also_removes_descendants_in_separate_groups() {
        parent_death(false, true, false).await;
    }

    #[tokio::test]
    async fn sigkill_parent_is_detected_even_with_an_inherited_writer() {
        parent_death(false, false, true).await;
    }

    #[test]
    fn reaped_std_child_never_signals_a_reused_cached_group_number() {
        let mut canary = Command::new("/bin/sleep").arg("600").spawn().unwrap();
        let mut done = Command::new("/usr/bin/true").spawn().unwrap();
        done.inner.as_mut().unwrap().wait().unwrap();
        done.pid = canary.id();
        done.finish();
        assert!(canary.try_wait().unwrap().is_none());
        assert!(done.life.is_none());
        canary.kill().unwrap();
        assert!(canary.life.is_none());
        canary.wait().unwrap();
    }

    #[tokio::test]
    async fn reaped_tokio_child_never_signals_a_reused_cached_group_number() {
        let mut canary = AsyncCommand::new("/bin/sleep").arg("600").spawn().unwrap();
        let mut done = AsyncCommand::new("/usr/bin/true").spawn().unwrap();
        done.inner.as_mut().unwrap().wait().await.unwrap();
        done.pid = canary.id().unwrap();
        done.finish();
        assert!(canary.try_wait().unwrap().is_none());
        assert!(done.life.is_none());
        canary.start_kill().unwrap();
        assert!(canary.life.is_none());
        canary.wait().await.unwrap();
    }

    #[tokio::test]
    async fn normal_drop_removes_the_group() {
        let fixture = Fixture::new();
        let child = target(&fixture.0.join("ready")).spawn().unwrap();
        eventually_sync("normal child ready", || fixture.pids("ready").len() == 2).await;
        let pid = child.id();
        drop(child);
        eventually_sync("normal Drop removes the process group", || {
            gone(-(pid as i32))
        })
        .await;
        fs::remove_file(fixture.0.join("ready")).unwrap();
    }

    #[test]
    fn panic_reaps_child_and_its_process_group() {
        let fixture = Fixture::new();
        let ready = fixture.0.join("ready");
        let observed = std::sync::Mutex::new(Vec::new());
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let guard = target(&ready).spawn().unwrap();
            crate::testkit::eventually_blocking("panic fixture ready", || {
                fixture.pids("ready").len() == 2
            });
            let pids = fixture.pids("ready");
            assert_eq!(pids[0], guard.id() as i32);
            assert_eq!(pids[0], unsafe { get_group(pids[0]) });
            *observed.lock().unwrap() = pids;
            let _keep_until_unwind = guard;
            panic!("intentional test panic");
        }));
        assert!(
            outcome.is_err(),
            "closure must panic to exercise unwind Drop"
        );
        let pids = observed.into_inner().unwrap();
        assert_eq!(pids.len(), 2);
        crate::testkit::eventually_blocking(
            "panic unwind reaped child, grandchild, and process group",
            || pids.iter().all(|p| gone(*p)) && gone(-pids[0]),
        );
        fs::remove_file(ready).unwrap();
    }

    #[tokio::test]
    async fn panic_in_async_task_reaps_async_process_guard_and_group() {
        let fixture = Fixture::new();
        let ready = fixture.0.join("ready");
        let pids = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let pids_in = pids.clone();
        let ready_in = ready.clone();
        let handle = tokio::spawn(async move {
            let mut cmd = AsyncCommand::new("/bin/sh");
            cmd.args([
                "-c",
                "/bin/sleep 600 & child=$!; printf '%s %s\\n' \"$$\" \"$child\" > \"$1\"; wait",
                "fixture",
            ])
            .arg(&ready_in)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
            let guard = cmd.spawn().unwrap();
            eventually_sync("async panic fixture ready", || {
                fs::read_to_string(&ready_in)
                    .unwrap_or_default()
                    .split_whitespace()
                    .count()
                    == 2
            })
            .await;
            let recorded: Vec<i32> = fs::read_to_string(&ready_in)
                .unwrap()
                .split_whitespace()
                .filter_map(|p| p.parse().ok())
                .collect();
            assert_eq!(recorded[0], guard.id().unwrap() as i32);
            *pids_in.lock().unwrap() = recorded;
            let _keep_until_unwind = guard;
            panic!("intentional async test panic");
        });
        assert!(handle.await.unwrap_err().is_panic());
        let recorded = pids.lock().unwrap().clone();
        assert_eq!(recorded.len(), 2);
        eventually_sync(
            "async panic unwind reaped child, grandchild, and process group",
            || recorded.iter().all(|p| gone(*p)) && gone(-recorded[0]),
        )
        .await;
        fs::remove_file(ready).unwrap();
    }

    #[test]
    fn std_preserves_stdio_exit_code_and_command_reuse() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "printf output; printf error >&2; test -z \"${HOME+x}${_STEVE_TEST_LIFELINE_FD+x}${_STEVE_TEST_PARENT_PID+x}\" || exit 99; exit 23"])
            .env_clear().env("PATH", "/no-executables");
        for _ in 0..2 {
            let out = command.output().unwrap();
            assert_eq!(out.status.code(), Some(23));
            assert_eq!(out.stdout, b"output");
            assert_eq!(out.stderr, b"error");
        }
        let mut child = Command::new("/bin/sh")
            .args(["-c", "read -r line; printf '%s' \"$line\""])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(b"input\n").unwrap();
        assert_eq!(child.wait_with_output().unwrap().stdout, b"input");
    }
    #[tokio::test]
    async fn tokio_preserves_output_and_status() {
        let out = AsyncCommand::new("/bin/sh")
            .args(["-c", "printf output; printf error >&2; exit 23"])
            .env_clear()
            .env("PATH", "/no-executables")
            .output()
            .await
            .unwrap();
        assert_eq!(out.status.code(), Some(23));
        assert_eq!(out.stdout, b"output");
        assert_eq!(out.stderr, b"error");
        assert!(
            AsyncCommand::new("/usr/bin/true")
                .status()
                .await
                .unwrap()
                .success()
        );
    }
    #[test]
    fn std_output_honors_explicit_stdio_and_restores_spawn_defaults() {
        let fixture = Fixture::new();
        let path = fixture.0.join("output");
        let out = Command::new("/bin/sh")
            .args(["-c", "printf explicit"])
            .stdout(std::fs::File::create(&path).unwrap())
            .output()
            .unwrap();
        assert!(out.stdout.is_empty());
        assert_eq!(fs::read(&path).unwrap(), b"explicit");
        let mut command = Command::new("/usr/bin/true");
        command.output().unwrap();
        let mut child = command.spawn().unwrap();
        assert!(child.stdout.is_none() && child.stderr.is_none());
        child.wait().unwrap();
        let out = Command::new("/bin/sh")
            .args(["-c", "read -r line; test -z \"$line\""])
            .output()
            .unwrap();
        assert!(out.status.success(), "default output stdin is EOF");
    }

    #[tokio::test]
    async fn std_output_wait_does_not_hold_the_spawn_lock() {
        let fixture = Fixture::new();
        let path = fixture.0.join("ready");
        let (finished, result) = std::sync::mpsc::channel();
        let output = std::thread::spawn(move || {
            let out = Command::new("/bin/sh")
                .args([
                    "-c",
                    "printf '%s' \"$$\" > \"$1\"; exec /bin/sleep 600",
                    "fixture",
                ])
                .arg(path)
                .output();
            finished.send(out).unwrap();
        });
        eventually_sync("long std output ready", || fixture.pids("ready").len() == 1).await;
        let next = tokio::task::spawn_blocking(|| Command::new("/usr/bin/true").status().unwrap());
        eventually_sync("another spawn proceeds while std output waits", || {
            next.is_finished()
        })
        .await;
        assert!(next.await.unwrap().success());
        unsafe {
            kill(-fixture.pids("ready")[0], 9);
        }
        eventually_sync("std output exits", || result.try_recv().is_ok()).await;
        output.join().unwrap();
        fs::remove_file(fixture.0.join("ready")).unwrap();
    }

    #[tokio::test]
    async fn tokio_output_is_send_and_cancellation_releases_its_group() {
        struct Abort(tokio::task::AbortHandle);
        impl Drop for Abort {
            fn drop(&mut self) {
                self.0.abort();
            }
        }
        let fixture = Fixture::new();
        let path = fixture.0.join("ready");
        let output = tokio::spawn(async move {
            AsyncCommand::new("/bin/sh")
                .args([
                    "-c",
                    "printf '%s' \"$$\" > \"$1\"; exec /bin/sleep 600",
                    "fixture",
                ])
                .arg(path)
                .output()
                .await
        });
        let _abort = Abort(output.abort_handle());
        eventually_sync("long Tokio output ready", || {
            fixture.pids("ready").len() == 1
        })
        .await;
        let next = tokio::task::spawn_blocking(|| Command::new("/usr/bin/true").status().unwrap());
        eventually_sync("another spawn proceeds while Tokio output waits", || {
            next.is_finished()
        })
        .await;
        assert!(next.await.unwrap().success());
        output.abort();
        assert!(output.await.unwrap_err().is_cancelled());
        eventually_sync("cancelled output group exits", || {
            gone(-fixture.pids("ready")[0])
        })
        .await;
        fs::remove_file(fixture.0.join("ready")).unwrap();
    }
}
