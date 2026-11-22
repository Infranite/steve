//! The local restic backend. A future destination driver supplies a restic REST endpoint
//! here while snapshot, retention and restore policy stay in the Hub.
use anyhow::{Context, Result, ensure};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, ErrorKind, Read, Seek, Write},
    os::{
        fd::AsRawFd,
        unix::{
            fs::{DirBuilderExt, MetadataExt, OpenOptionsExt, PermissionsExt},
            process::CommandExt,
        },
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
    time::Instant,
};
use steve_modules::vault::{keystore, secret::Kek};

#[derive(Debug, thiserror::Error)]
pub(super) enum Failure {
    #[error("backup_repository_locked")]
    Locked,
    #[error("backup_source_incomplete")]
    Incomplete,
    #[error("backup_deadline_exceeded")]
    Deadline,
    #[error("backup_repository_command_failed ({0}, exit {1:?})")]
    Command(&'static str, Option<i32>),
}

pub(super) struct Engine {
    binary: PathBuf,
    base: PathBuf,
    lock: File,
    deadline: Instant,
}

impl Engine {
    pub(super) fn lock(home: &Path, deadline: Instant) -> Result<Self> {
        let base = home.canonicalize()?.join("backups");
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
        let binary = std::env::current_exe()?
            .canonicalize()?
            .with_file_name("restic");
        ensure!(
            binary.is_absolute()
                && fs::symlink_metadata(&binary)
                    .is_ok_and(|m| m.is_file() && m.mode() & 0o111 != 0),
            "backup_engine_missing: {}",
            binary.display()
        );
        for name in [".restic-cache", ".restic-work"] {
            let dir = base.join(name);
            fs::DirBuilder::new()
                .mode(0o700)
                .recursive(true)
                .create(&dir)?;
            private(&dir)?;
        }
        Ok(Self {
            binary,
            base,
            lock,
            deadline,
        })
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

    pub(super) fn remaining(&self) -> Result<std::time::Duration> {
        self.deadline
            .checked_duration_since(Instant::now())
            .ok_or(Failure::Deadline.into())
    }

    pub(super) fn run(
        &self,
        key: &Kek,
        operation: &'static str,
        args: &[&std::ffi::OsStr],
        cwd: &Path,
    ) -> Result<Vec<u8>> {
        self.run_at(&self.repository(), key, operation, args, cwd)
    }

    pub(super) fn run_at(
        &self,
        repository: &Path,
        key: &Kek,
        operation: &'static str,
        args: &[&std::ffi::OsStr],
        cwd: &Path,
    ) -> Result<Vec<u8>> {
        self.remaining()?;
        // An unlinked file avoids a full stdout pipe blocking the child. stderr can contain
        // source paths or backend details. Only explicitly decoded results cross this boundary.
        let mut output = tempfile::tempfile_in(self.work())?;
        let mut command = Command::new(&self.binary);
        command
            .env_clear()
            .env("TMPDIR", self.work())
            .current_dir(cwd)
            .arg("--repo")
            .arg(repository)
            .args(["--password-file", "/dev/stdin", "--cache-dir"])
            .arg(self.base.join(".restic-cache"))
            .arg("--json")
            .arg(operation)
            .args(args)
            .stdin(Stdio::piped())
            .stdout(output.try_clone()?)
            .stderr(Stdio::null());
        let fd = self.lock.as_raw_fd();
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
        let result = (|| -> Result<_> {
            let mut stdin = child.stdin.take().context("restic password pipe missing")?;
            let password = keystore::encode(key);
            // A command may exit before consuming stdin (for example, on invalid args).
            if let Err(error) = stdin
                .write_all(password.as_bytes())
                .and_then(|()| stdin.write_all(b"\n"))
                && error.kind() != ErrorKind::BrokenPipe
            {
                return Err(error.into());
            }
            drop(stdin);
            loop {
                if let Some(status) = child.try_wait()? {
                    return Ok(status);
                }
                std::thread::sleep(self.remaining()?.min(std::time::Duration::from_millis(20)));
            }
        })();
        let status = match result {
            Ok(status) => status,
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        };
        match status.code() {
            Some(0) => {}
            Some(3) if operation == "backup" => return Err(Failure::Incomplete.into()),
            Some(11) => return Err(Failure::Locked.into()),
            code => return Err(Failure::Command(operation, code).into()),
        }
        const MAX_OUTPUT: u64 = 32 * 1024 * 1024;
        ensure!(
            output.metadata()?.len() <= MAX_OUTPUT,
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

pub(super) fn private(dir: &Path) -> Result<()> {
    let meta = fs::symlink_metadata(dir)?;
    ensure!(
        meta.is_dir() && meta.uid() == crate::admin::euid() && meta.mode() & 0o077 == 0,
        "backup directory must be private and owned by the Hub user: {}",
        dir.display()
    );
    Ok(())
}
