//! Test subprocesses owned by a CLOEXEC lifeline, including when the test is SIGKILLed.
//! The shell execs the target in place: socket activation, child PIDs and PTYs keep their meaning.
use std::ffi::OsStr;
use std::io;
use std::ops::{Deref, DerefMut};
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};
use std::os::unix::net::UnixStream;
use std::os::unix::process::CommandExt;
use std::path::Path;
use std::process::{ExitStatus, Output, Stdio};

const FD_ENV: &str = "_STEVE_TEST_LIFELINE_FD";
const GROUPS: &str = r#"
    { parent[$1]=$2; group[$1]=$3 }
    END {
        seen[root]=1
        do {
            changed=0
            for (p in parent) if (!seen[p] && seen[parent[p]]) { seen[p]=1; changed=1 }
        } while (changed)
        # Reading a missing awk key creates it: membership alone is not reachability.
        for (p in parent) if (seen[p] && group[p]>0 && group[p]!=root && seen[group[p]]) groups[group[p]]=1
        for (g in groups) print g
    }
"#;
const WATCH: &str = r#"
owner=$$
life=$_STEVE_TEST_LIFELINE_FD
selector=$1
shift
unset _STEVE_TEST_LIFELINE_FD
(
    trap '' HUP INT TERM
    IFS= read -r -u "$life" || :
    # The selector emits only groups reached from the target, never unrelated processes.
    groups=$(/bin/ps -axo pid=,ppid=,pgid= | /usr/bin/awk -v root="$owner" "$selector")
    for group in $groups; do kill -KILL -- "-$group" 2>/dev/null || :; done
    kill -KILL -- "-$owner"
) </dev/null >/dev/null 2>&1 &
eval "exec $life<&-"
exec "$@"
"#;

unsafe extern "C" {
    fn fcntl(fd: i32, cmd: i32, ...) -> i32;
    fn kill(pid: i32, signal: i32) -> i32;
}

struct Lifeline {
    _read: OwnedFd,
    write: UnixStream,
}
impl Lifeline {
    fn new() -> io::Result<Self> {
        // UnixStream::pair and its owned write end are close-on-exec. A parallel spawn
        // cannot keep another test's parent alive. Only the duplicated read end is inherited.
        let (read, write) = UnixStream::pair()?;
        // SAFETY: F_DUPFD (0 on supported Unix platforms) duplicates a live descriptor. Keep
        // it above socket-activation descriptors 3..; OwnedFd closes the parent's copy.
        let fd = unsafe { fcntl(read.as_raw_fd(), 0, 64) };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        Ok(Self {
            _read: unsafe { OwnedFd::from_raw_fd(fd) },
            write,
        })
    }
}
fn command(program: impl AsRef<OsStr>) -> std::process::Command {
    let mut c = std::process::Command::new("/bin/bash");
    // Privileged mode ignores BASH_ENV and imported functions without changing the target's env.
    c.args([
        "--noprofile",
        "--norc",
        "-p",
        "-c",
        WATCH,
        "steve-test-watchdog",
        GROUPS,
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
            self.inner.stdin(io);
            self
        }
        pub fn stdout(&mut self, io: impl Into<Stdio>) -> &mut Self {
            self.inner.stdout(io);
            self
        }
        pub fn stderr(&mut self, io: impl Into<Stdio>) -> &mut Self {
            self.inner.stderr(io);
            self
        }
        pub fn process_group(&mut self, group: i32) -> &mut Self {
            assert_eq!(group, 0, "test children must own their process group");
            self.inner.process_group(0);
            self
        }
        fn prepare(&mut self) -> io::Result<Lifeline> {
            let life = Lifeline::new()?;
            self.inner
                .env(FD_ENV, life._read.as_raw_fd().to_string())
                .process_group(0);
            Ok(life)
        }
    };
}

/// A std command whose builder chains cannot bypass the lifeline at spawn/output/status.
pub struct Command {
    inner: std::process::Command,
}
impl Command {
    pub fn new(program: impl AsRef<OsStr>) -> Self {
        Self {
            inner: command(program),
        }
    }
    builders!();
    pub fn spawn(&mut self) -> io::Result<Child> {
        let life = self.prepare()?;
        let child = self.inner.spawn()?;
        Ok(Child {
            pid: child.id(),
            inner: Some(child),
            life: Some(life.write),
        })
    }
    pub fn output(&mut self) -> io::Result<Output> {
        let _life = self.prepare()?;
        self.inner.output()
    }
    pub fn status(&mut self) -> io::Result<ExitStatus> {
        self.spawn()?.wait()
    }
}

/// Owns both the ordinary wait/reap path and the parent-death lifeline.
#[derive(Debug)]
pub struct Child {
    inner: Option<std::process::Child>,
    pid: u32,
    life: Option<UnixStream>,
}
impl Child {
    fn finish(&mut self) {
        if self.life.is_some() {
            kill_group(self.pid);
            self.life.take();
        }
    }
    pub fn id(&self) -> u32 {
        self.pid
    }
    pub fn kill(&mut self) -> io::Result<()> {
        if self.life.is_some() {
            kill_group(self.pid);
        }
        self.inner.as_mut().unwrap().kill()
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
impl Deref for Child {
    type Target = std::process::Child;
    fn deref(&self) -> &Self::Target {
        self.inner.as_ref().unwrap()
    }
}
impl DerefMut for Child {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.inner.as_mut().unwrap()
    }
}
impl Drop for Child {
    fn drop(&mut self) {
        self.finish();
        if let Some(child) = &mut self.inner {
            let _ = child.wait();
        }
    }
}

/// Tokio counterpart, using exactly the same shell and lifeline as [`Command`].
pub struct AsyncCommand {
    inner: tokio::process::Command,
}
impl AsyncCommand {
    pub fn new(program: impl AsRef<OsStr>) -> Self {
        Self {
            inner: command(program).into(),
        }
    }
    builders!();
    pub fn kill_on_drop(&mut self, kill: bool) -> &mut Self {
        self.inner.kill_on_drop(kill);
        self
    }
    pub fn spawn(&mut self) -> io::Result<AsyncChild> {
        let life = self.prepare()?;
        let child = self.inner.spawn()?;
        Ok(AsyncChild {
            pid: child.id().expect("spawned child PID"),
            inner: Some(child),
            life: Some(life.write),
        })
    }
    pub async fn output(&mut self) -> io::Result<Output> {
        let _life = self.prepare()?;
        self.inner.output().await
    }
    pub async fn status(&mut self) -> io::Result<ExitStatus> {
        self.spawn()?.wait().await
    }
}
#[derive(Debug)]
pub struct AsyncChild {
    inner: Option<tokio::process::Child>,
    pid: u32,
    life: Option<UnixStream>,
}
impl AsyncChild {
    fn finish(&mut self) {
        if self.life.is_some() {
            kill_group(self.pid);
            self.life.take();
        }
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
        if self.life.is_some() {
            kill_group(self.pid);
        }
        self.inner.as_mut().unwrap().start_kill()
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
impl Deref for AsyncChild {
    type Target = tokio::process::Child;
    fn deref(&self) -> &Self::Target {
        self.inner.as_ref().unwrap()
    }
}
impl DerefMut for AsyncChild {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.inner.as_mut().unwrap()
    }
}
impl Drop for AsyncChild {
    fn drop(&mut self) {
        self.finish();
        if let Some(child) = &mut self.inner {
            let _ = child.start_kill();
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

    #[test]
    fn group_selection_never_includes_unrelated_or_shared_groups() {
        let mut selector = std::process::Command::new("/usr/bin/awk");
        selector
            .args(["-v", "root=10", GROUPS])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped());
        let mut child = selector.spawn().unwrap();
        child.stdin.take().unwrap().write_all(
            b"1 0 1\n13 12 12\n12 11 12\n11 10 10\n10 1 10\n20 1 20\n21 20 21\n22 999 22\n30 10 20\n"
        ).unwrap();
        let out = child.wait_with_output().unwrap();
        assert!(out.status.success());
        assert_eq!(String::from_utf8(out.stdout).unwrap().trim(), "12");
        let out = std::process::Command::new("/usr/bin/awk")
            .args(["-v", "root=777", GROUPS])
            .stdin(Stdio::null())
            .output()
            .unwrap();
        assert!(out.status.success() && out.stdout.is_empty());
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
            let other = std::process::Command::new("/bin/sleep")
                .arg("600")
                .process_group(0)
                .spawn()
                .unwrap();
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

    async fn parent_death(asynchronous: bool, escaped: bool) {
        let fixture = Fixture::new();
        let mut parent = Command::new(std::env::current_exe().unwrap());
        parent
            .args([
                "--exact",
                "testkit::process::tests::parent_helper",
                "--nocapture",
            ])
            .env("STEVE_TEST_PROCESS_PARENT", &fixture.0)
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
        parent_death(false, false).await;
    }
    #[tokio::test]
    async fn sigkill_parent_reaps_tokio_child_and_its_group_without_drop() {
        parent_death(true, false).await;
    }

    #[tokio::test]
    async fn sigkill_parent_also_removes_descendants_in_separate_groups() {
        parent_death(false, true).await;
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
    fn std_preserves_stdio_exit_code_and_command_reuse() {
        let mut command = Command::new("/bin/sh");
        command.args(["-c", "printf output; printf error >&2; test -z \"${HOME+x}${_STEVE_TEST_LIFELINE_FD+x}\" || exit 99; exit 23"])
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
}
