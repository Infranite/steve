//! Verified, schema-independent copies from one pinned SQLite read snapshot.
use rusqlite::{
    Connection, OpenFlags,
    backup::{Backup, StepResult},
};
use sha2::{Digest, Sha256};
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt};
use std::{
    fs::{self, File, OpenOptions},
    io::{self, Read, Seek},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::{Duration, Instant},
};

#[derive(Clone, Debug)]
pub struct CopyOptions {
    pub pages_per_step: u32,
    /// Elapsed budget for the entire operation, including verification and publication.
    /// Checked between steps/phases; a currently executing SQLite call is not interrupted.
    pub deadline: Option<Duration>,
}
impl Default for CopyOptions {
    fn default() -> Self {
        Self {
            pages_per_step: 1024,
            deadline: None,
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Progress {
    pub copied_pages: u32,
    pub total_pages: u32,
}
#[derive(Debug, Default)]
pub struct CancelFlag(AtomicBool);
impl CancelFlag {
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}
#[derive(Clone, Debug)]
pub struct CopiedFile {
    pub path: PathBuf,
    pub bytes: u64,
    pub sha256: String,
    pub pages: u32,
    pub quick_check: String,
    pub elapsed: Duration,
}
#[derive(Debug, thiserror::Error)]
pub enum BackupError {
    #[error("backup cancelled")]
    Cancelled,
    #[error("backup deadline exceeded")]
    DeadlineExceeded,
    #[error("pages_per_step must be in 1..=i32::MAX and the source must have a file name")]
    InvalidOptions,
    #[error("backup database is busy or locked; retry the operation")]
    Busy,
    #[error("backup database checksum mismatch")]
    ChecksumMismatch,
    #[error("backup quick_check failed: {0}")]
    QuickCheckFailed(String),
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
}
fn checkpoint(
    elapsed: Duration,
    opts: &CopyOptions,
    cancel: &CancelFlag,
) -> Result<(), BackupError> {
    if cancel.is_cancelled() {
        return Err(BackupError::Cancelled);
    }
    if opts.deadline.is_some_and(|deadline| elapsed >= deadline) {
        return Err(BackupError::DeadlineExceeded);
    }
    Ok(())
}
/// Own only paths created by this invocation. Drop runs after SQLite handles have closed,
/// including during callback unwinding. Never remove a pre-existing destination/partial.
struct Cleanup {
    partial: PathBuf,
    published: Option<PathBuf>,
    armed: bool,
}
impl Drop for Cleanup {
    fn drop(&mut self) {
        if self.armed {
            let _ = fs::remove_file(&self.partial);
            if let Some(path) = &self.published {
                let _ = fs::remove_file(path);
            }
            // Make failure cleanup durable when possible; preserve the original error.
            if let Some(parent) = self.partial.parent() {
                let _ = File::open(parent).and_then(|f| f.sync_all());
            }
        }
    }
}
/// Publish an already verified file without replacing an existing destination, including
/// a racing publisher. Both names share one directory/filesystem: hard_link creates the
/// final name atomically, then removing partial completes publication. The caller fsyncs
/// the directory and withdraws its own final name if durability or budget checks fail.
fn publish(from: &Path, to: &Path) -> io::Result<()> {
    fs::hard_link(from, to)?;
    if let Err(error) = fs::remove_file(from) {
        let _ = fs::remove_file(to);
        return Err(error);
    }
    Ok(())
}
/// Copy `src` to `dest_dir/<source file name>`; existing final files and partials are errors.
/// The caller owns offline exclusion (storage.lock). No Engine, schema gate, migration,
/// journal-mode change, retention policy, or manifest is involved. Progress runs after each
/// successful step, including Done. Concurrent WAL commits do not move the pinned snapshot.
pub fn backup_file(
    src: &Path,
    dest_dir: &Path,
    opts: &CopyOptions,
    progress: &dyn Fn(Progress),
    cancel: &CancelFlag,
) -> Result<CopiedFile, BackupError> {
    let started = Instant::now();
    backup_with_elapsed(src, dest_dir, opts, progress, cancel, &|| started.elapsed())
}
// Keep the clock seam private: callers always use Instant; tests can deterministically
// expire a deadline after a real SQLite step without sleeping or racing wall time.
fn backup_with_elapsed(
    src: &Path,
    dest_dir: &Path,
    opts: &CopyOptions,
    progress: &dyn Fn(Progress),
    cancel: &CancelFlag,
    elapsed: &dyn Fn() -> Duration,
) -> Result<CopiedFile, BackupError> {
    let step = i32::try_from(opts.pages_per_step)
        .ok()
        .filter(|n| *n > 0)
        .ok_or(BackupError::InvalidOptions)?;
    let name = src.file_name().ok_or(BackupError::InvalidOptions)?;
    checkpoint(elapsed(), opts, cancel)?;
    let source = Connection::open_with_flags(
        src,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    source.busy_timeout(Duration::ZERO)?;
    source.execute_batch("BEGIN; SELECT 1 FROM sqlite_schema LIMIT 1;")?;
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    builder.mode(0o700);
    builder.create(dest_dir)?;
    let destination = dest_dir.join(name);
    match fs::symlink_metadata(&destination) {
        Ok(_) => return Err(io::Error::from(io::ErrorKind::AlreadyExists).into()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error.into()),
    }
    let mut partial_name = name.to_os_string();
    partial_name.push(".partial");
    let partial = dest_dir.join(partial_name);
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    options.mode(0o600);
    let mut file = options.open(&partial)?;
    let mut cleanup = Cleanup {
        partial: partial.clone(),
        published: None,
        armed: true,
    };
    let mut target = Connection::open_with_flags(
        &partial,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    target.busy_timeout(Duration::ZERO)?;
    let pages;
    {
        let backup = Backup::new(&source, &mut target)?;
        loop {
            checkpoint(elapsed(), opts, cancel)?;
            let result = backup.step(step)?;
            match result {
                StepResult::More | StepResult::Done => {}
                _ => return Err(BackupError::Busy),
            }
            let state = backup.progress();
            let current = Progress {
                copied_pages: (state.pagecount - state.remaining) as u32,
                total_pages: state.pagecount as u32,
            };
            progress(current);
            checkpoint(elapsed(), opts, cancel)?;
            if result == StepResult::Done {
                pages = current.total_pages;
                break;
            }
        }
    }
    quick_check(&target)?;
    target.close().map_err(|(_, error)| error)?;
    source.close().map_err(|(_, error)| error)?;
    checkpoint(elapsed(), opts, cancel)?;
    file.rewind()?;
    let (sha256, bytes) = checksum(&mut file, || checkpoint(elapsed(), opts, cancel))?;
    file.sync_all()?;
    checkpoint(elapsed(), opts, cancel)?;
    publish(&partial, &destination)?;
    cleanup.published = Some(destination.clone());
    File::open(dest_dir)?.sync_all()?;
    // If a deadline/cancel raced with publication, withdraw only this invocation's file.
    checkpoint(elapsed(), opts, cancel)?;
    cleanup.armed = false;
    Ok(CopiedFile {
        path: destination,
        bytes,
        sha256,
        pages,
        quick_check: "ok".into(),
        elapsed: elapsed(),
    })
}

fn checksum(
    mut file: impl Read,
    check: impl Fn() -> Result<(), BackupError>,
) -> Result<(String, u64), BackupError> {
    let mut hash = Sha256::new();
    let mut bytes = 0u64;
    let mut buffer = [0u8; 65_536];
    loop {
        check()?;
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
        bytes += count as u64;
    }
    Ok((
        hash.finalize().iter().map(|b| format!("{b:02x}")).collect(),
        bytes,
    ))
}

/// Check a closed standalone SQLite snapshot without opening or creating WAL sidecars.
pub fn verify_file(path: &Path, expected_sha256: &str) -> Result<(), BackupError> {
    let (actual, _) = checksum(File::open(path)?, || Ok(()))?;
    if actual != expected_sha256 {
        return Err(BackupError::ChecksumMismatch);
    }
    let encoded: String = path
        .as_os_str()
        .as_encoded_bytes()
        .iter()
        .map(|byte| format!("%{byte:02X}"))
        .collect();
    let connection = Connection::open_with_flags(
        format!("file:{encoded}?immutable=1"),
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
    )?;
    quick_check(&connection)
}

/// Close out WAL while offline so the original database stays usable until replacement.
pub fn close_wal(path: &Path) -> Result<(), BackupError> {
    let connection = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    let mode: String =
        connection.query_row("PRAGMA journal_mode = DELETE", [], |row| row.get(0))?;
    if mode != "delete" {
        return Err(BackupError::Busy);
    }
    connection.close().map_err(|(_, error)| error)?;
    Ok(())
}

fn quick_check(target: &Connection) -> Result<(), BackupError> {
    let checks = target
        .prepare("PRAGMA quick_check")
        .and_then(|mut query| {
            query
                .query_map([], |row| row.get::<_, String>(0))?
                .collect::<rusqlite::Result<Vec<_>>>()
        })
        .map_err(|error| BackupError::QuickCheckFailed(error.to_string()))?;
    if checks != ["ok"] {
        return Err(BackupError::QuickCheckFailed(checks.join("; ")));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn deadline_between_steps_removes_the_existing_partial() {
        let temp = tempfile::tempdir().unwrap();
        let source = temp.path().join("source.db");
        let conn = Connection::open(&source).unwrap();
        conn.execute_batch(
            "CREATE TABLE data(body BLOB); INSERT INTO data VALUES(zeroblob(65536));",
        )
        .unwrap();
        drop(conn);
        let destination = temp.path().join("copy");
        let elapsed = std::cell::Cell::new(Duration::ZERO);
        let result = backup_with_elapsed(
            &source,
            &destination,
            &CopyOptions {
                pages_per_step: 1,
                deadline: Some(Duration::from_secs(1)),
            },
            &|progress| {
                assert!(progress.copied_pages < progress.total_pages);
                assert!(destination.join("source.db.partial").exists());
                elapsed.set(Duration::from_secs(2));
            },
            &CancelFlag::default(),
            &|| elapsed.get(),
        );
        assert!(matches!(result, Err(BackupError::DeadlineExceeded)));
        assert!(fs::read_dir(destination).unwrap().next().is_none());
    }
}
