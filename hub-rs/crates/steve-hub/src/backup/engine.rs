//! The local restic backend. A future destination driver supplies a restic REST endpoint
//! here while snapshot, retention and restore policy stay in the Hub.
use crate::metrics::backup::{self as telemetry, EngineCommand, Event, Failure as Code};
use anyhow::{Context, Result, ensure};
#[cfg(not(target_os = "macos"))]
use std::os::unix::process::CommandExt;
#[cfg(not(target_os = "macos"))]
use std::process::{Child, Command, Stdio};
#[cfg(target_os = "macos")]
use std::{
    ffi::CString,
    os::{
        fd::{FromRawFd, OwnedFd, RawFd},
        unix::process::ExitStatusExt,
    },
};
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

/// Polling interval while waiting for non-blocking password writes or restic exit.
const ENGINE_POLL_INTERVAL: Duration = Duration::from_millis(20);
/// Maximum time spent reaping the direct restic child after `SIGKILL` is sent to its process group.
const ENGINE_KILL_REAP_TIMEOUT: Duration = Duration::from_secs(5);
/// Maximum JSON output accepted from a single restic invocation.
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
    dev_compiled: Option<&str>,
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
    // locate restic deterministically without copying it next to cargo's deps test binaries.
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
    if let Some(dev_engine) = dev_compiled {
        let candidate = PathBuf::from(dev_engine);
        if is_executable_regular_file(&candidate) {
            return Ok(candidate);
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
        option_env!("STEVE_DEV_BACKUP_ENGINE"),
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

fn write_password_bounded(stdin: &mut File, bytes: &[u8], deadline: Instant) -> Result<()> {
    set_nonblocking(stdin.as_raw_fd())?;
    let mut written = 0;
    while written < bytes.len() {
        let remaining = deadline
            .checked_duration_since(Instant::now())
            .ok_or(Failure::Deadline)?;
        match stdin.write(&bytes[written..]) {
            Ok(0) => return Ok(()),
            Ok(n) => written += n,
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(error) if error.kind() == ErrorKind::WouldBlock => {
                std::thread::sleep(ENGINE_POLL_INTERVAL.min(remaining));
            }
            // A command may exit before consuming stdin (for example, on invalid args).
            Err(error) if error.kind() == ErrorKind::BrokenPipe => return Ok(()),
            Err(error) => return Err(error.into()),
        }
    }
    Ok(())
}

#[cfg(target_os = "macos")]
mod spawn {
    use super::*;
    use libc::{
        POSIX_SPAWN_CLOEXEC_DEFAULT, POSIX_SPAWN_SETPGROUP, posix_spawn as sys_spawn,
        posix_spawn_file_actions_addchdir_np as fa_addchdir,
        posix_spawn_file_actions_adddup2 as fa_adddup2,
        posix_spawn_file_actions_addopen as fa_addopen,
        posix_spawn_file_actions_destroy as fa_destroy, posix_spawn_file_actions_init as fa_init,
        posix_spawn_file_actions_t as Fa, posix_spawnattr_destroy as attr_destroy,
        posix_spawnattr_init as attr_init, posix_spawnattr_setflags as attr_setflags,
        posix_spawnattr_setpgroup as attr_setpgroup, posix_spawnattr_t as Attr,
    };

    struct FileActions(Fa);
    impl FileActions {
        fn new() -> io::Result<Self> {
            let mut raw = std::mem::MaybeUninit::<Fa>::uninit();
            // SAFETY: initializes an opaque POSIX file-actions handle.
            let rc = unsafe { fa_init(raw.as_mut_ptr()) };
            if rc != 0 {
                return Err(io::Error::from_raw_os_error(rc));
            }
            // SAFETY: initialized by `fa_init`.
            Ok(Self(unsafe { raw.assume_init() }))
        }
    }
    impl Drop for FileActions {
        fn drop(&mut self) {
            // SAFETY: `self.0` was initialized by `fa_init`.
            unsafe {
                fa_destroy(&mut self.0);
            }
        }
    }

    struct SpawnAttr(Attr);
    impl SpawnAttr {
        fn new() -> io::Result<Self> {
            let mut raw = std::mem::MaybeUninit::<Attr>::uninit();
            // SAFETY: initializes an opaque POSIX spawnattr handle.
            let rc = unsafe { attr_init(raw.as_mut_ptr()) };
            if rc != 0 {
                return Err(io::Error::from_raw_os_error(rc));
            }
            // SAFETY: initialized by `attr_init`.
            Ok(Self(unsafe { raw.assume_init() }))
        }
    }
    impl Drop for SpawnAttr {
        fn drop(&mut self) {
            // SAFETY: `self.0` was initialized by `attr_init`.
            unsafe {
                attr_destroy(&mut self.0);
            }
        }
    }

    fn dup_cloexec_above_stdio(fd: RawFd) -> io::Result<OwnedFd> {
        // SAFETY: `F_DUPFD_CLOEXEC` duplicates an open descriptor to a fresh slot >= 3 with CLOEXEC.
        let duped = unsafe { libc::fcntl(fd, libc::F_DUPFD_CLOEXEC, 3) };
        if duped < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: `duped` is a newly created owned descriptor.
        Ok(unsafe { OwnedFd::from_raw_fd(duped) })
    }

    fn cstring_os(s: &OsStr) -> Result<CString> {
        CString::new(s.as_encoded_bytes()).context("argument contains interior NUL")
    }

    pub(super) struct Spawned {
        pid: libc::pid_t,
        reaped: bool,
    }

    impl Spawned {
        /// Spawn restic on macOS via `posix_spawn` (avoiding multithreaded `fork()` hazards after
        /// CoreFoundation/Security/libdispatch initialization) with process-group isolation,
        /// `CLOEXEC_DEFAULT`, and explicit inheritance of the repository lock fd.
        pub(super) fn launch(
            binary: &Path,
            argv: &[&OsStr],
            tmpdir: &Path,
            cwd: &Path,
            stdout_file: &File,
            lock_file: &File,
        ) -> Result<(Self, File)> {
            let mut pipe_fds = [0 as libc::c_int; 2];
            // SAFETY: valid 2-element array for pipe(2).
            if unsafe { libc::pipe(pipe_fds.as_mut_ptr()) } != 0 {
                return Err(io::Error::last_os_error().into());
            }
            // SAFETY: `pipe` succeeded and returned two owned descriptors.
            let raw_read = unsafe { OwnedFd::from_raw_fd(pipe_fds[0]) };
            let raw_write = unsafe { OwnedFd::from_raw_fd(pipe_fds[1]) };
            for fd in [raw_read.as_raw_fd(), raw_write.as_raw_fd()] {
                // SAFETY: `fd` is owned and valid.
                if unsafe { libc::fcntl(fd, libc::F_SETFD, libc::FD_CLOEXEC) } != 0 {
                    return Err(io::Error::last_os_error().into());
                }
            }

            // Ensure all source fds are >= 3 so adddup2(_, 0/1) and addopen(2) never alias them.
            let stdin_read = dup_cloexec_above_stdio(raw_read.as_raw_fd())?;
            drop(raw_read);
            let stdout_dup = dup_cloexec_above_stdio(stdout_file.as_raw_fd())?;
            let lock_dup = dup_cloexec_above_stdio(lock_file.as_raw_fd())?;
            let lock_target = [3, 4, 5, 6]
                .into_iter()
                .find(|&t| {
                    t != stdin_read.as_raw_fd()
                        && t != stdout_dup.as_raw_fd()
                        && t != lock_dup.as_raw_fd()
                })
                .expect("four slots cover at most three source fds");

            let mut actions = FileActions::new()?;
            let cwd_c = cstring_os(cwd.as_os_str())?;
            let check = |rc: libc::c_int| -> io::Result<()> {
                if rc == 0 {
                    Ok(())
                } else {
                    Err(io::Error::from_raw_os_error(rc))
                }
            };
            // SAFETY: all descriptors and C strings stay alive until `sys_spawn` returns.
            unsafe {
                check(fa_addchdir(&mut actions.0, cwd_c.as_ptr()))?;
                check(fa_adddup2(&mut actions.0, stdin_read.as_raw_fd(), 0))?;
                check(fa_adddup2(&mut actions.0, stdout_dup.as_raw_fd(), 1))?;
                check(fa_addopen(
                    &mut actions.0,
                    2,
                    c"/dev/null".as_ptr(),
                    libc::O_WRONLY,
                    0,
                ))?;
                // Duplicating lock_dup to a distinct target fd clears FD_CLOEXEC in the child
                // even under POSIX_SPAWN_CLOEXEC_DEFAULT, keeping the flock held until restic exits.
                check(fa_adddup2(
                    &mut actions.0,
                    lock_dup.as_raw_fd(),
                    lock_target,
                ))?;
            }

            let mut attr = SpawnAttr::new()?;
            let flags = (POSIX_SPAWN_SETPGROUP | POSIX_SPAWN_CLOEXEC_DEFAULT) as libc::c_short;
            // SAFETY: `attr` is initialized.
            unsafe {
                check(attr_setflags(&mut attr.0, flags))?;
                check(attr_setpgroup(&mut attr.0, 0))?;
            }

            let binary_c = cstring_os(binary.as_os_str())?;
            let mut argv_c = Vec::with_capacity(argv.len() + 1);
            argv_c.push(binary_c.clone());
            for arg in argv {
                argv_c.push(cstring_os(arg)?);
            }
            let mut argv_ptrs: Vec<*mut libc::c_char> = argv_c
                .iter()
                .map(|s| s.as_ptr().cast_mut())
                .chain(std::iter::once(std::ptr::null_mut()))
                .collect();

            let mut tmpdir_env = std::ffi::OsString::from("TMPDIR=");
            tmpdir_env.push(tmpdir);
            let env_c = [cstring_os(&tmpdir_env)?];
            let envp_ptrs: [*mut libc::c_char; 2] =
                [env_c[0].as_ptr().cast_mut(), std::ptr::null_mut()];

            let mut pid: libc::pid_t = 0;
            // SAFETY: all pointers and descriptors remain valid across the `posix_spawn` call.
            let rc = unsafe {
                sys_spawn(
                    &mut pid,
                    binary_c.as_ptr(),
                    &actions.0,
                    &attr.0,
                    argv_ptrs.as_mut_ptr(),
                    envp_ptrs.as_ptr(),
                )
            };
            if rc != 0 {
                return Err(io::Error::from_raw_os_error(rc).into());
            }
            Ok((Self { pid, reaped: false }, File::from(raw_write)))
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
            let mut password = keystore::encode(key);
            password.push('\n');
            write_password_bounded(&mut stdin, password.as_bytes(), self.deadline)?;
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
        let dev_compiled = dir.path().join("dev-restic");
        executable(&dev_compiled, "#!/bin/sh\nexit 0\n");

        // Explicit STEVE_BACKUP_ENGINE wins in dev builds and resolves symlinks.
        let link = dir.path().join("restic-link");
        std::os::unix::fs::symlink(&explicit, &link).unwrap();
        assert_eq!(
            resolve_binary_with(
                &exe,
                Some(link.as_os_str()),
                Some(dev_compiled.to_str().unwrap()),
                false
            )
            .unwrap(),
            fs::canonicalize(&explicit).unwrap()
        );

        // Relative STEVE_BACKUP_ENGINE is refused with typed InvalidEngine error.
        let err = resolve_binary_with(&exe, Some(OsStr::new("restic")), None, false).unwrap_err();
        assert!(matches!(
            err.downcast_ref::<Failure>(),
            Some(Failure::InvalidEngine(_))
        ));

        // Without env var, deps/ test binary finds target/debug/restic, then compile-time dev engine.
        assert_eq!(
            resolve_binary_with(&exe, None, Some(dev_compiled.to_str().unwrap()), false).unwrap(),
            profile_restic
        );
        fs::remove_file(&profile_restic).unwrap();
        assert_eq!(
            resolve_binary_with(&exe, None, Some(dev_compiled.to_str().unwrap()), false).unwrap(),
            dev_compiled
        );

        // Release builds ignore STEVE_BACKUP_ENGINE and STEVE_DEV_BACKUP_ENGINE completely.
        let rel_err = resolve_binary_with(
            &exe,
            Some(explicit.as_os_str()),
            Some(dev_compiled.to_str().unwrap()),
            true,
        )
        .unwrap_err();
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
