//! The local restic backend. A future destination driver supplies a restic REST endpoint
//! here while snapshot, retention and restore policy stay in the Hub.
use crate::metrics::backup::{self as telemetry, EngineCommand, Event, Failure as Code};
use anyhow::{Context, Result, ensure};
#[cfg(not(target_os = "macos"))]
use std::os::unix::process::CommandExt;
#[cfg(target_os = "macos")]
use std::os::unix::process::ExitStatusExt;
#[cfg(not(target_os = "macos"))]
use std::process::{Child, Command, Stdio};
use std::{
    ffi::OsStr,
    fs::{self, File, OpenOptions},
    io::{self, ErrorKind, Read, Seek, Write},
    os::{
        fd::AsRawFd,
        unix::fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
    },
    path::{Path, PathBuf},
    process::ExitStatus,
    time::{Duration, Instant},
};
use steve_modules::vault::{keystore, secret::Kek};

/// Polling interval while waiting for non-blocking password writes or restic exit (20ms balances
/// prompt deadline detection with negligible CPU during multi-minute restic transfers).
const ENGINE_POLL_INTERVAL: Duration = Duration::from_millis(20);
/// Maximum time spent reaping the direct restic child after `SIGKILL` is sent to its process
/// group (unified with `steve_runtime::process::PROCESS_KILL_REAP_TIMEOUT` = 5s).
const ENGINE_KILL_REAP_TIMEOUT: Duration = steve_runtime::process::PROCESS_KILL_REAP_TIMEOUT;
/// Maximum JSON output accepted from a single restic invocation (32 MiB accommodates large
/// repository snapshot listings while bounding Hub memory).
const MAX_ENGINE_OUTPUT: u64 = 32 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub(super) enum Failure {
    #[error("backup_repository_locked")]
    Locked,
    #[error("backup_source_incomplete")]
    Incomplete,
    #[error("backup_deadline_exceeded")]
    Deadline,
    #[error("backup_engine_missing: {}", .0.display())]
    EngineMissing(PathBuf),
    #[error("backup_engine_invalid: {}", .0.display())]
    InvalidEngine(PathBuf),
    #[error("backup_repository_command_failed ({0}, exit {1:?})")]
    Command(&'static str, Option<i32>),
}

fn is_executable_regular_file(path: &Path) -> bool {
    path.is_absolute()
        && fs::symlink_metadata(path).is_ok_and(|m| m.is_file() && m.mode() & 0o111 != 0)
}

fn resolve_binary_with(
    exe: &Path,
    configured: Option<&OsStr>,
    release_build: bool,
) -> Result<PathBuf> {
    let sibling = exe.with_file_name("restic");
    // Release builds strictly require the bundled sibling binary next to steve-hub.
    if release_build {
        ensure!(
            is_executable_regular_file(&sibling),
            Failure::EngineMissing(sibling)
        );
        return Ok(sibling);
    }
    // Dev/test builds: explicit STEVE_BACKUP_ENGINE takes precedence so local runs and CI shards
    // locate restic deterministically without scanning ambient PATH or copying next to deps/.
    if let Some(configured) = configured.filter(|v| !v.is_empty()) {
        let path = PathBuf::from(configured);
        ensure!(path.is_absolute(), Failure::InvalidEngine(path));
        let resolved = fs::canonicalize(&path).map_err(|_| Failure::EngineMissing(path.clone()))?;
        ensure!(
            is_executable_regular_file(&resolved),
            Failure::EngineMissing(path)
        );
        return Ok(resolved);
    }
    if is_executable_regular_file(&sibling) {
        return Ok(sibling);
    }
    // Cargo unit/integration test binaries live under `target/<profile>/deps/`; check the profile
    // directory (`target/<profile>/restic`) where CI stages the checksum-pinned engine.
    if exe
        .parent()
        .is_some_and(|parent| parent.file_name() == Some(OsStr::new("deps")))
        && let Some(profile_dir) = exe.parent().and_then(Path::parent)
    {
        let staged = profile_dir.join("restic");
        if is_executable_regular_file(&staged) {
            return Ok(staged);
        }
    }
    Err(Failure::EngineMissing(sibling).into())
}

fn resolve_binary() -> Result<PathBuf> {
    let exe = std::env::current_exe()?.canonicalize()?;
    let configured = std::env::var_os("STEVE_BACKUP_ENGINE");
    resolve_binary_with(
        &exe,
        configured.as_deref(),
        option_env!("STEVE_BUILD_VERSION").is_some(),
    )
}

fn kill_process_group(pid: libc::pid_t) {
    if pid > 1 {
        // SAFETY: restic is always started in its own process group (`pgid == pid`), so `-pid`
        // terminates only that invocation and any descendants it created.
        unsafe {
            libc::kill(-pid, libc::SIGKILL);
        }
    }
}

fn set_nonblocking(fd: libc::c_int) -> io::Result<()> {
    // SAFETY: caller passes an open descriptor owned for the duration of both fcntl calls.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags < 0 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } != 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

/// Write `bytes` over a non-blocking pipe bounded by `deadline`. Returns `Ok(true)` if all bytes
/// were delivered, or `Ok(false)` if the child closed stdin early (`BrokenPipe`/`WriteZero`).
fn write_slice_bounded(stdin: &mut File, bytes: &[u8], deadline: Instant) -> Result<bool> {
    let mut written = 0;
    while written < bytes.len() {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or(Failure::Deadline)?;
        match stdin.write(&bytes[written..]) {
            Ok(0) => return Ok(false),
            Ok(n) => written += n,
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                std::thread::sleep(ENGINE_POLL_INTERVAL.min(remaining));
            }
            // A command may exit before consuming stdin (for example, on invalid args).
            Err(error) if error.kind() == ErrorKind::BrokenPipe => return Ok(false),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(true)
}

/// Deliver the base64 password followed by `\n` in two writes so `Zeroizing<String>` is NEVER
/// pushed to or reallocated (which would leave an unzeroized copy of the backup key on the heap).
fn write_password_line_bounded(stdin: &mut File, password: &[u8], deadline: Instant) -> Result<()> {
    set_nonblocking(stdin.as_raw_fd())?;
    if write_slice_bounded(stdin, password, deadline)? {
        let _ = write_slice_bounded(stdin, b"\n", deadline)?;
    }
    Ok(())
}

#[cfg(target_os = "macos")]
mod spawn {
    use super::*;
    use std::ffi::OsString;
    use steve_runtime::process::{
        ProcessIsolation,
        macos_spawn::{self, EnvSpec, SpawnSpec, StdioSpec},
    };

    pub(super) struct Spawned {
        pid: libc::pid_t,
        reaped: bool,
    }

    impl Spawned {
        /// Spawn restic on macOS via the shared `steve_runtime::process::macos_spawn` wrapper
        /// (avoiding multithreaded `fork()` hazards while setting `POSIX_SPAWN_SETSIGDEF`,
        /// `POSIX_SPAWN_SETSIGMASK`, `POSIX_SPAWN_SETPGROUP`, `POSIX_SPAWN_CLOEXEC_DEFAULT`, and
        /// explicit inheritance of the repository lock fd).
        pub(super) fn launch(
            binary: &Path,
            argv: &[&OsStr],
            tmpdir: &Path,
            cwd: &Path,
            stdout_file: &File,
            lock_file: &File,
        ) -> Result<(Self, File)> {
            let env = [(OsString::from("TMPDIR"), tmpdir.as_os_str().to_os_string())];
            let spawned = macos_spawn::spawn(SpawnSpec {
                program: binary.as_os_str(),
                args: argv,
                env: EnvSpec::Clear(&env),
                cwd: Some(cwd),
                isolation: ProcessIsolation::ProcessGroup,
                stdin: StdioSpec::Pipe,
                stdout: StdioSpec::Fd(stdout_file.as_raw_fd()),
                stderr: StdioSpec::Null,
                extra_inherit_fds: &[lock_file.as_raw_fd()],
            })?;
            let stdin = File::from(spawned.stdin.context("restic password pipe missing")?);
            Ok((
                Self {
                    pid: spawned.pid,
                    reaped: false,
                },
                stdin,
            ))
        }

        pub(super) fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
            if self.reaped {
                return Ok(None);
            }
            let mut status: libc::c_int = 0;
            loop {
                // SAFETY: `self.pid` is an unreaped direct child owned by this struct.
                let rc = unsafe { libc::waitpid(self.pid, &mut status, libc::WNOHANG) };
                if rc == 0 {
                    return Ok(None);
                }
                if rc == self.pid {
                    self.reaped = true;
                    return Ok(Some(ExitStatus::from_raw(status)));
                }
                let err = io::Error::last_os_error();
                if err.kind() == ErrorKind::Interrupted {
                    continue;
                }
                return Err(err);
            }
        }

        pub(super) fn kill_group_and_reap(&mut self) {
            if self.reaped {
                return;
            }
            kill_process_group(self.pid);
            // SAFETY: unreaped child still pins its PID.
            unsafe {
                libc::kill(self.pid, libc::SIGKILL);
            }
            let deadline = Instant::now() + ENGINE_KILL_REAP_TIMEOUT;
            while !self.reaped && Instant::now() < deadline {
                match self.try_wait() {
                    Ok(Some(_)) | Err(_) => break,
                    Ok(None) => std::thread::sleep(ENGINE_POLL_INTERVAL),
                }
            }
        }
    }

    impl Drop for Spawned {
        fn drop(&mut self) {
            if !self.reaped {
                self.kill_group_and_reap();
            }
        }
    }
}

#[cfg(not(target_os = "macos"))]
mod spawn {
    use super::*;
    use std::os::fd::OwnedFd;

    pub(super) struct Spawned {
        child: Child,
        pid: libc::pid_t,
        reaped: bool,
    }

    impl Spawned {
        pub(super) fn launch(
            binary: &Path,
            argv: &[&OsStr],
            tmpdir: &Path,
            cwd: &Path,
            stdout_file: &File,
            lock_file: &File,
        ) -> Result<(Self, File)> {
            let mut command = Command::new(binary);
            command
                .env_clear()
                .env("TMPDIR", tmpdir)
                .current_dir(cwd)
                .args(argv)
                .stdin(Stdio::piped())
                .stdout(stdout_file.try_clone()?)
                .stderr(Stdio::null())
                .process_group(0);
            let fd = lock_file.as_raw_fd();
            // SAFETY: fcntl is async-signal-safe. The inherited fd keeps the installation lock
            // held until restic exits if the Hub is killed during a command.
            unsafe {
                command.pre_exec(move || {
                    if libc::fcntl(fd, libc::F_DUPFD, 3) < 0 {
                        Err(io::Error::last_os_error())
                    } else {
                        Ok(())
                    }
                });
            }
            let mut child = command.spawn()?;
            let pid = child.id() as libc::pid_t;
            let stdin = child.stdin.take().context("restic password pipe missing")?;
            let stdin = File::from(OwnedFd::from(stdin));
            Ok((
                Self {
                    child,
                    pid,
                    reaped: false,
                },
                stdin,
            ))
        }

        pub(super) fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
            if self.reaped {
                return Ok(None);
            }
            let status = self.child.try_wait()?;
            if status.is_some() {
                self.reaped = true;
            }
            Ok(status)
        }

        pub(super) fn kill_group_and_reap(&mut self) {
            if self.reaped {
                return;
            }
            kill_process_group(self.pid);
            let _ = self.child.kill();
            let deadline = Instant::now() + ENGINE_KILL_REAP_TIMEOUT;
            while !self.reaped && Instant::now() < deadline {
                match self.try_wait() {
                    Ok(Some(_)) | Err(_) => break,
                    Ok(None) => std::thread::sleep(ENGINE_POLL_INTERVAL),
                }
            }
        }
    }

    impl Drop for Spawned {
        fn drop(&mut self) {
            if !self.reaped {
                self.kill_group_and_reap();
            }
        }
    }
}

pub(super) struct Engine {
    home: PathBuf,
    binary: PathBuf,
    base: PathBuf,
    lock: File,
    deadline: Instant,
}

impl Engine {
    pub(super) fn lock(home: &Path, deadline: Instant) -> Result<Self> {
        let binary = resolve_binary()?;
        Self::lock_with_binary(home, binary, deadline)
    }

    fn lock_with_binary(home: &Path, binary: PathBuf, deadline: Instant) -> Result<Self> {
        let home_canonical = home.canonicalize()?;
        let base = home_canonical.join("backups");
        fs::DirBuilder::new()
            .mode(0o700)
            .recursive(true)
            .create(&base)?;
        private(&base)?;
        let lock = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
            .open(base.join(".restic.lock"))?;
        ensure!(
            lock.metadata()?.is_file(),
            "backup lock is not a regular file"
        );
        lock.try_lock().map_err(|err| match err {
            std::fs::TryLockError::WouldBlock => anyhow::Error::new(Failure::Locked),
            std::fs::TryLockError::Error(e) => e.into(),
        })?;
        for name in [".restic-cache", ".restic-work"] {
            let dir = base.join(name);
            fs::DirBuilder::new()
                .mode(0o700)
                .recursive(true)
                .create(&dir)?;
            private(&dir)?;
        }
        Ok(Self {
            home: home.to_path_buf(),
            binary,
            base,
            lock,
            deadline,
        })
    }

    pub(super) fn home(&self) -> &Path {
        &self.home
    }
    pub(super) fn repository(&self) -> PathBuf {
        self.base.join("restic")
    }
    pub(super) fn work(&self) -> PathBuf {
        self.base.join(".restic-work")
    }

    pub(super) fn reset_work(&self) -> Result<()> {
        for entry in fs::read_dir(self.work())? {
            let entry = entry?;
            if entry.file_type()?.is_dir() {
                fs::remove_dir_all(entry.path())?;
            } else {
                fs::remove_file(entry.path())?;
            }
        }
        Ok(())
    }

    pub(super) fn remaining(&self) -> Result<Duration> {
        self.deadline
            .checked_duration_since(Instant::now())
            .ok_or(Failure::Deadline.into())
    }

    pub(super) fn run(
        &self,
        key: &Kek,
        operation: EngineCommand,
        args: &[&OsStr],
        cwd: &Path,
    ) -> Result<Vec<u8>> {
        self.run_at(&self.repository(), key, operation, args, cwd)
    }

    pub(super) fn run_at(
        &self,
        repository: &Path,
        key: &Kek,
        operation: EngineCommand,
        args: &[&OsStr],
        cwd: &Path,
    ) -> Result<Vec<u8>> {
        let started = Instant::now();
        let result = self.execute_at(repository, key, operation, args, cwd);
        let failure = result
            .as_ref()
            .err()
            .map(|error| match error.downcast_ref::<Failure>() {
                Some(Failure::Locked) => Code::Locked,
                Some(Failure::Incomplete) => Code::SourceIncomplete,
                Some(Failure::Deadline) => Code::Deadline,
                _ => Code::Failed,
            });
        telemetry::emit(
            &self.home,
            Event::Engine {
                command: operation,
                elapsed: started.elapsed(),
                failure,
            },
        );
        result
    }

    fn execute_at(
        &self,
        repository: &Path,
        key: &Kek,
        operation: EngineCommand,
        args: &[&OsStr],
        cwd: &Path,
    ) -> Result<Vec<u8>> {
        self.remaining()?;
        // An unlinked file avoids a full stdout pipe blocking the child. stderr can contain
        // source paths or backend details. Only explicitly decoded results cross this boundary.
        let mut output = tempfile::tempfile_in(self.work())?;
        let cache_dir = self.base.join(".restic-cache");
        let mut argv: Vec<&OsStr> = Vec::with_capacity(8 + args.len());
        argv.extend([
            OsStr::new("--repo"),
            repository.as_os_str(),
            OsStr::new("--password-file"),
            OsStr::new("/dev/stdin"),
            OsStr::new("--cache-dir"),
            cache_dir.as_os_str(),
            OsStr::new("--json"),
            OsStr::new(operation.code()),
        ]);
        argv.extend_from_slice(args);

        let (mut spawned, mut stdin) =
            spawn::Spawned::launch(&self.binary, &argv, &self.work(), cwd, &output, &self.lock)?;
        let result = (|| -> Result<ExitStatus> {
            let password = keystore::encode(key);
            write_password_line_bounded(&mut stdin, password.as_bytes(), self.deadline)?;
            drop(stdin);
            loop {
                if let Some(status) = spawned.try_wait()? {
                    return Ok(status);
                }
                std::thread::sleep(self.remaining()?.min(ENGINE_POLL_INTERVAL));
            }
        })();
        let status = match result {
            Ok(status) => status,
            Err(error) => {
                spawned.kill_group_and_reap();
                return Err(error);
            }
        };
        match status.code() {
            Some(0) => {}
            Some(3) if operation == EngineCommand::Backup => return Err(Failure::Incomplete.into()),
            Some(11) => return Err(Failure::Locked.into()),
            code => return Err(Failure::Command(operation.code(), code).into()),
        }
        ensure!(
            output.metadata()?.len() <= MAX_ENGINE_OUTPUT,
            "restic output exceeded its limit"
        );
        output.rewind()?;
        let mut data = Vec::new();
        output.read_to_end(&mut data)?;
        Ok(data)
    }
}

pub(super) fn private_tempdir(parent: &Path, prefix: &str) -> io::Result<tempfile::TempDir> {
    tempfile::Builder::new()
        .prefix(prefix)
        .permissions(fs::Permissions::from_mode(0o700))
        .tempdir_in(parent)
}

pub(crate) fn private(dir: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(dir)?;
    ensure!(
        meta.is_dir() && meta.uid() == crate::admin::euid() && meta.mode() & 0o077 == 0,
        "backup directory must be private and owned by the Hub user: {}",
        dir.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn executable(path: &Path, body: &str) {
        fs::write(path, body).unwrap();
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }

    #[test]
    fn resolve_binary_prefers_explicit_env_in_dev_and_restricts_release_to_sibling() {
        let dir = tempfile::tempdir_in("/tmp").unwrap();
        let deps = dir.path().join("target/debug/deps");
        fs::create_dir_all(&deps).unwrap();
        let exe = deps.join("steve_hub-test");
        executable(&exe, "#!/bin/sh\nexit 0\n");

        let explicit = dir.path().join("explicit-restic");
        executable(&explicit, "#!/bin/sh\nexit 0\n");
        let profile_restic = dir.path().join("target/debug/restic");
        executable(&profile_restic, "#!/bin/sh\nexit 0\n");

        // Explicit STEVE_BACKUP_ENGINE wins in dev builds and resolves symlinks.
        let link = dir.path().join("restic-link");
        std::os::unix::fs::symlink(&explicit, &link).unwrap();
        assert_eq!(
            resolve_binary_with(&exe, Some(link.as_os_str()), false).unwrap(),
            fs::canonicalize(&explicit).unwrap()
        );

        // Relative STEVE_BACKUP_ENGINE is refused with typed InvalidEngine error.
        let err = resolve_binary_with(&exe, Some(OsStr::new("restic")), false).unwrap_err();
        assert!(matches!(
            err.downcast_ref::<Failure>(),
            Some(Failure::InvalidEngine(_))
        ));

        // Without env var, deps/ test binary finds target/debug/restic, then fails deterministically
        // without scanning ambient PATH or Homebrew.
        assert_eq!(
            resolve_binary_with(&exe, None, false).unwrap(),
            profile_restic
        );
        fs::remove_file(&profile_restic).unwrap();
        assert!(matches!(
            resolve_binary_with(&exe, None, false)
                .unwrap_err()
                .downcast_ref::<Failure>(),
            Some(Failure::EngineMissing(_))
        ));

        // Release builds ignore STEVE_BACKUP_ENGINE completely.
        let rel_err = resolve_binary_with(&exe, Some(explicit.as_os_str()), true).unwrap_err();
        assert!(matches!(
            rel_err.downcast_ref::<Failure>(),
            Some(Failure::EngineMissing(_))
        ));
    }

    #[test]
    fn engine_deadline_kills_entire_process_group_and_returns_typed_deadline() {
        let dir = tempfile::tempdir_in("/tmp").unwrap();
        let pid_file = dir.path().join("grandchild.pid");
        let fake_restic = dir.path().join("fake-restic");
        executable(
            &fake_restic,
            &format!(
                "#!/bin/sh\n/bin/sleep 60 & printf '%s' \"$!\" > '{}'\nwait\n",
                pid_file.display()
            ),
        );
        let key = Kek::generate().unwrap();
        let engine = Engine::lock_with_binary(
            dir.path(),
            fake_restic,
            Instant::now() + Duration::from_millis(250),
        )
        .unwrap();
        let started = Instant::now();
        let err = engine
            .run(&key, EngineCommand::Snapshots, &[], dir.path())
            .expect_err("hanging restic must time out");
        assert!(
            matches!(err.downcast_ref::<Failure>(), Some(Failure::Deadline)),
            "expected Failure::Deadline, got {err:?}"
        );
        assert!(
            started.elapsed() < Duration::from_secs(3),
            "engine timeout took {:?}",
            started.elapsed()
        );
        if let Ok(raw) = fs::read_to_string(&pid_file)
            && let Ok(pid) = raw.trim().parse::<libc::pid_t>()
        {
            let until = Instant::now() + Duration::from_secs(2);
            while unsafe { libc::kill(pid, 0) } == 0 && Instant::now() < until {
                std::thread::sleep(Duration::from_millis(10));
            }
            assert_eq!(
                unsafe { libc::kill(pid, 0) },
                -1,
                "restic descendant {pid} survived deadline SIGKILL"
            );
        }
    }
}
