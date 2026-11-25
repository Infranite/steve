//! Installation backup policy shared by management, the daily job, and destructive operations.
mod engine;
mod files;
mod repository;

use crate::metrics::backup::{
    self as telemetry, Event, Failure as Code, Observation, Operation, Phase,
};
use anyhow::{Context, Result, ensure};
pub(crate) use engine::private as private_directory;
use engine::{Failure, private_tempdir};
use repository::{Repository, Snapshot};
use serde::Serialize;
use std::{
    ffi::CString,
    fs::{self, DirBuilder, File},
    io,
    os::unix::fs::{DirBuilderExt, PermissionsExt},
    path::{Path, PathBuf},
    time::{Duration, Instant},
};
use steve_store::backup::{self as storage, CancelFlag, CopyOptions};
pub use steve_store::backup_policy::DailySettings;
pub use steve_types::backup::{
    BackupPurpose as Purpose, BackupReceipt, SnapshotId, VerifiedDatabase,
};

const DATABASE_ENTRY: &str = "/data/steve.sqlite3";
const CHECKSUM_ENTRY: &str = "/data/steve.sqlite3.sha256";
const UPGRADE_BUDGET: Duration = Duration::from_secs(120);
pub(crate) const BACKUP_BUDGET: Duration = Duration::from_secs(3600);
const STATUS_BUDGET: Duration = Duration::from_secs(15);
const DAY_MS: u64 = 86_400_000;
// Allow space for both the staged SQLite copy and a full write into restic.
const DATABASE_SPACE_COPIES: u64 = 2;

#[derive(Clone, Copy, PartialEq, Eq)]
enum FileScope {
    All,
    State,
    None,
}

fn scope(purpose: Purpose) -> FileScope {
    match purpose {
        Purpose::Daily | Purpose::PreDestructive | Purpose::Manual => FileScope::All,
        Purpose::PreUpgrade => FileScope::State,
        Purpose::Diagnostic => FileScope::None,
    }
}

fn limits(purpose: Purpose, daily_days: u64) -> Option<(u64, usize)> {
    match purpose {
        Purpose::Daily => Some((daily_days, DailySettings::MAX_KEEP_DAYS as usize + 1)),
        Purpose::PreUpgrade => Some((90, 8)),
        Purpose::PreDestructive => Some((30, 8)),
        Purpose::Diagnostic => Some((7, 4)),
        Purpose::Manual => None,
    }
}

pub fn root(home: &Path) -> PathBuf {
    home.join("backups/restic")
}

pub fn create(home: &Path, db: &Path, purpose: Purpose) -> Result<BackupReceipt> {
    let _telemetry = telemetry::scope(home);
    let start = Instant::now();
    let budget = if purpose == Purpose::PreUpgrade {
        UPGRADE_BUDGET
    } else {
        BACKUP_BUDGET
    };
    let result = (|| {
        let repo =
            Repository::open(home, start + budget, true)?.context("backup repository missing")?;
        // Finish any interrupted publication before taking this request's fresh snapshot.
        repo.recover_publication()?;
        let stage = repo.work().join("snapshot");
        DirBuilder::new().mode(0o700).create(&stage)?;
        let stage = Scratch(stage);
        let data = stage.0.join("data");
        let snapshot_started = Instant::now();
        let copied = (|| -> Result<_> {
            let copied = storage::backup_file(
                db,
                &data,
                &CopyOptions {
                    deadline: Some(repo.remaining()?),
                    ..CopyOptions::default()
                },
                &|_| {},
                &CancelFlag::default(),
            )?;
            let staged_db = stage.0.join(&DATABASE_ENTRY[1..]);
            if copied.path != staged_db {
                fs::rename(&copied.path, &staged_db)?;
            }
            fs::write(stage.0.join(&CHECKSUM_ENTRY[1..]), copied.sha256.as_bytes())?;
            Ok(copied)
        })();
        telemetry::emit(
            home,
            Event::Phase {
                phase: Phase::SqliteSnapshot,
                purpose: Some(purpose),
                elapsed: snapshot_started.elapsed(),
            },
        );
        let copied = copied?;
        let mut required = vec![DATABASE_ENTRY.into(), CHECKSUM_ENTRY.into()];
        let stage_started = Instant::now();
        let staged = files::stage(home, &stage.0.join("files"), scope(purpose));
        telemetry::emit(
            home,
            Event::Phase {
                phase: Phase::FileStage,
                purpose: Some(purpose),
                elapsed: stage_started.elapsed(),
            },
        );
        let staged = staged?;
        telemetry::emit(
            home,
            Event::Staged {
                purpose,
                files: staged.files,
                bytes: staged.bytes,
                skipped_caches: staged.skipped_caches,
            },
        );
        required.extend(staged.required);
        let database = VerifiedDatabase {
            sha256: copied.sha256,
            bytes: copied.bytes,
        };
        let (snapshot, written_bytes) = repo.backup(&stage.0, purpose, &required, &database)?;
        observe_capacity(&repo.path());
        Ok(BackupReceipt {
            snapshot_id: snapshot.id.clone(),
            purpose,
            created_ms: snapshot.created_ms(),
            database,
            written_bytes,
        })
    })();
    let written = result.as_ref().ok().map(|receipt| receipt.written_bytes);
    record(
        home,
        Operation::Backup,
        Some(purpose),
        start,
        &result,
        written,
    );
    if result.is_err() {
        observe_capacity(&root(home));
    }
    result
}

struct Scratch(PathBuf);
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn valid_snapshot(repo: &Repository, id: &SnapshotId) -> Result<Purpose> {
    repo.snapshots()?
        .into_iter()
        .find(|s| &s.id == id)
        .and_then(|s| s.purpose())
        .context("backup_snapshot_not_found")
}

fn restore_verified(
    home: &Path,
    repo: &Repository,
    id: &SnapshotId,
    purpose: Purpose,
    target: &Path,
    db_only: bool,
) -> Result<(PathBuf, u64)> {
    let bytes = repo.restore(id, purpose, target, db_only)?;
    let verify_started = Instant::now();
    let verified = (|| -> Result<PathBuf> {
        let db = target.join(&DATABASE_ENTRY[1..]);
        let checksum = target.join(&CHECKSUM_ENTRY[1..]);
        ensure!(
            fs::symlink_metadata(target.join("data"))?.is_dir()
                && fs::symlink_metadata(&db)?.is_file()
                && fs::symlink_metadata(&checksum).is_ok_and(|m| m.is_file() && m.len() == 64),
            "backup_required_file_missing"
        );
        storage::verify_file(&db, &fs::read_to_string(checksum)?)?;
        Ok(db)
    })();
    telemetry::emit(
        home,
        Event::Phase {
            phase: Phase::SqliteVerify,
            purpose: Some(purpose),
            elapsed: verify_started.elapsed(),
        },
    );
    Ok((verified?, bytes))
}

/// Restore every backed-up file into a newly allocated private directory within `parent`.
pub fn restore_to(home: &Path, id: &SnapshotId, parent: &Path) -> Result<PathBuf> {
    restore_operation(home, id, |repo, purpose| {
        let parent = parent.canonicalize()?;
        let path = repo.path();
        ensure!(
            !parent.starts_with(path.parent().context("missing backup directory")?),
            "restore destination overlaps the backup directory"
        );
        let stage = private_tempdir(&parent, "steve-restore-")?;
        let (_, bytes) = restore_verified(home, repo, id, purpose, stage.path(), false)?;
        Ok((stage.keep(), bytes))
    })
}

/// Restore the database with the installation instance lock held by the offline caller. Keep the failed probe
/// next to the database, then publish the verified replacement with one same-filesystem rename.
pub fn restore_database(home: &Path, db: &Path, id: &SnapshotId) -> Result<()> {
    restore_operation(home, id, |repo, purpose| {
        let parent = db.parent().context("missing database parent")?;
        let stage = private_tempdir(parent, ".steve-restore-")?;
        let (verified, bytes) = restore_verified(home, repo, id, purpose, stage.path(), true)?;
        File::open(&verified)?.sync_all()?;
        if db.try_exists()? {
            let mut wal_path = db.as_os_str().to_os_string();
            wal_path.push("-wal");
            if Path::new(&wal_path).try_exists()? {
                storage::close_wal(db)?;
            }
            let failed = private_tempdir(parent, "steve-before-restore-")?;
            let previous = failed.path().join("steve.sqlite3");
            fs::copy(db, &previous)?;
            fs::set_permissions(&previous, fs::Permissions::from_mode(0o600))?;
            File::open(previous)?.sync_all()?;
            File::open(failed.path())?.sync_all()?;
            let _ = failed.keep();
            File::open(parent)?.sync_all()?;
        }
        fs::rename(verified, db)?;
        File::open(parent)?.sync_all()?;
        Ok(((), bytes))
    })
}

fn restore_operation<T>(
    home: &Path,
    id: &SnapshotId,
    restore: impl FnOnce(&Repository, Purpose) -> Result<(T, u64)>,
) -> Result<T> {
    let _telemetry = telemetry::scope(home);
    let start = Instant::now();
    let mut purpose = None;
    let result = (|| {
        let repo = Repository::open(home, start + BACKUP_BUDGET, false)?
            .context("backup_repository_missing")?;
        let found = valid_snapshot(&repo, id)?;
        purpose = Some(found);
        restore(&repo, found)
    })();
    let written = result.as_ref().ok().map(|(_, bytes)| *bytes);
    record(home, Operation::Restore, purpose, start, &result, written);
    result.map(|(value, _)| value)
}

/// Retain a snapshot if it is the latest of its automatic purpose, or within both limits.
fn expired(snapshots: &[Snapshot], now: u64, daily_days: u64, purpose: Purpose) -> Vec<SnapshotId> {
    let Some((days, copies)) = limits(purpose, daily_days) else {
        return Vec::new();
    };
    let mut own: Vec<_> = snapshots
        .iter()
        .filter(|s| s.purpose() == Some(purpose))
        .collect();
    own.sort_unstable_by(|a, b| {
        (b.created_ms(), b.id.as_str()).cmp(&(a.created_ms(), a.id.as_str()))
    });
    own.into_iter()
        .enumerate()
        .skip(1)
        .filter(|(index, snap)| {
            *index >= copies || now.saturating_sub(snap.created_ms()) > days * DAY_MS
        })
        .map(|(_, snap)| snap.id.clone())
        .collect()
}

pub fn retain(store: &steve_store::Store, home: &Path, now: u64) -> Result<()> {
    let _telemetry = telemetry::scope(home);
    let start = Instant::now();
    let result = (|| {
        let Some(repo) = Repository::open(home, start + BACKUP_BUDGET, false)? else {
            return Ok(());
        };
        repo.recover_publication()?;
        let daily = daily_settings(store)?.keep_days;
        let snapshots = repo.snapshots()?;
        let by_purpose =
            Purpose::ALL.map(|purpose| (purpose, expired(&snapshots, now, daily, purpose)));
        let pending: Vec<_> = snapshots
            .into_iter()
            .filter(Snapshot::pending)
            .map(|s| s.id)
            .collect();
        if !pending.is_empty() || by_purpose.iter().any(|(_, ids)| !ids.is_empty()) {
            // Persist intent before forget so a kill between forget and prune still reclaims space.
            repo.require_prune()?;
            for (purpose, ids) in by_purpose {
                if !ids.is_empty() {
                    let start = Instant::now();
                    let count = ids.len() as u64;
                    let result = repo.forget(Some(purpose), &ids);
                    record(
                        home,
                        Operation::Cleanup,
                        Some(purpose),
                        start,
                        &result,
                        None,
                    );
                    result?;
                    telemetry::emit(
                        home,
                        Event::Deleted {
                            purpose: Some(purpose),
                            count,
                        },
                    );
                }
            }
            if !pending.is_empty() {
                let count = pending.len() as u64;
                repo.forget(None, &pending)?;
                telemetry::emit(
                    home,
                    Event::Deleted {
                        purpose: None,
                        count,
                    },
                );
            }
        }
        let before = capacity(&repo.path()).ok().map(|c| c.bytes);
        let pruned = repo.prune_if_needed()?;
        let after = observe_capacity(&repo.path()).map(|c| c.bytes);
        if pruned && let (Some(before), Some(after)) = (before, after) {
            telemetry::emit(
                home,
                Event::Reclaimed {
                    bytes: before.saturating_sub(after),
                },
            );
        }
        Ok(())
    })();
    record(home, Operation::Cleanup, None, start, &result, None);
    result
}

fn record<T>(
    home: &Path,
    operation: Operation,
    purpose: Option<Purpose>,
    start: Instant,
    result: &Result<T>,
    bytes: Option<u64>,
) {
    telemetry::report(
        home,
        Observation {
            operation,
            purpose,
            elapsed: start.elapsed(),
            bytes,
            failure: result.as_ref().err().map(error_code),
        },
    );
}

pub(super) fn error_code(error: &anyhow::Error) -> Code {
    match error.downcast_ref::<Failure>() {
        Some(Failure::Locked) => Code::Locked,
        Some(Failure::Incomplete) => Code::SourceIncomplete,
        Some(Failure::Deadline) => Code::Deadline,
        None if matches!(
            error.downcast_ref(),
            Some(storage::BackupError::DeadlineExceeded)
        ) =>
        {
            Code::Deadline
        }
        _ => Code::Failed,
    }
}

#[derive(Debug, Serialize)]
pub struct Capacity {
    pub bytes: u64,
    pub available_bytes: u64,
}

fn capacity(path: &Path) -> Result<Capacity> {
    fn bytes(path: &Path) -> io::Result<u64> {
        let mut total = 0u64;
        for entry in fs::read_dir(path)? {
            let entry = entry?;
            let meta = entry.metadata()?;
            if meta.is_file() {
                total = total.saturating_add(meta.len());
            } else if meta.is_dir() {
                total = total.saturating_add(bytes(&entry.path())?);
            }
        }
        Ok(total)
    }
    Ok(Capacity {
        bytes: bytes(path)?,
        available_bytes: available_space(path)?,
    })
}

fn available_space(path: &Path) -> io::Result<u64> {
    let name = CString::new(path.as_os_str().as_encoded_bytes())?;
    let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: valid NUL-terminated path and a buffer written on success.
    let stat = unsafe {
        if libc::statvfs(name.as_ptr(), stat.as_mut_ptr()) != 0 {
            return Err(io::Error::last_os_error());
        }
        stat.assume_init()
    };
    Ok(u64::from(stat.f_bavail).saturating_mul(u64::from(stat.f_frsize)))
}

pub(crate) fn observe_capacity(path: &Path) -> Option<Capacity> {
    let capacity = capacity(path).ok()?;
    metrics::gauge!("steve_backup_repository_bytes").set(capacity.bytes as f64);
    Some(capacity)
}

fn estimated_database_space(db: &Path) -> io::Result<u64> {
    let mut wal = db.as_os_str().to_os_string();
    wal.push("-wal");
    let bytes = fs::metadata(db)?.len();
    let wal_bytes = match fs::metadata(wal) {
        Ok(meta) => meta.len(),
        Err(error) if error.kind() == io::ErrorKind::NotFound => 0,
        Err(error) => return Err(error),
    };
    Ok(bytes
        .saturating_add(wal_bytes)
        .saturating_mul(DATABASE_SPACE_COPIES))
}

pub(crate) fn observe_destination(home: &Path, db: &Path) {
    // Before the repository exists, use the nearest existing parent on its destination volume.
    let available = [root(home).as_path(), home.join("backups").as_path(), home]
        .into_iter()
        .map(available_space)
        .find(|result| !matches!(result, Err(error) if error.kind() == io::ErrorKind::NotFound))
        .and_then(Result::ok);
    metrics::gauge!("steve_backup_destination_available_bytes")
        .set(available.map_or(f64::NAN, |bytes| bytes as f64));
    metrics::gauge!("steve_backup_destination_required_bytes")
        .set(estimated_database_space(db).map_or(f64::NAN, |bytes| bytes as f64));
}

#[derive(Debug, Serialize)]
pub struct Latest {
    pub snapshot_id: SnapshotId,
    pub at: u64,
}

#[derive(Debug, Serialize)]
pub struct Inventory {
    pub last: Option<Latest>,
    pub repository: Option<Capacity>,
    pub error: Option<&'static str>,
}

#[derive(Debug, Serialize)]
pub struct SnapshotInfo {
    pub snapshot_id: SnapshotId,
    pub purpose: Purpose,
    pub created_ms: u64,
}

pub fn list(home: &Path) -> Result<Vec<SnapshotInfo>> {
    let _telemetry = telemetry::scope(home);
    let Some(repo) = Repository::open(home, Instant::now() + STATUS_BUDGET, false)? else {
        return Ok(Vec::new());
    };
    let mut list = repo
        .snapshots()?
        .into_iter()
        .filter_map(|snapshot| {
            Some(SnapshotInfo {
                purpose: snapshot.purpose()?,
                created_ms: snapshot.created_ms(),
                snapshot_id: snapshot.id,
            })
        })
        .collect::<Vec<_>>();
    list.sort_unstable_by_key(|snapshot| std::cmp::Reverse(snapshot.created_ms));
    Ok(list)
}

pub fn inventory(home: &Path) -> Inventory {
    let mut inventory = Inventory {
        last: None,
        repository: None,
        error: None,
    };
    let result = (|| {
        let Some(repo) = Repository::open(home, Instant::now() + STATUS_BUDGET, false)? else {
            return Ok(());
        };
        let snapshots = repo.snapshots()?;
        inventory.last = snapshots
            .into_iter()
            .filter(|s| s.purpose() == Some(Purpose::Daily))
            .max_by_key(Snapshot::created_ms)
            .map(|s| Latest {
                at: s.created_ms(),
                snapshot_id: s.id,
            });
        inventory.repository = observe_capacity(&repo.path());
        Ok(())
    })();
    if let Err(e) = result {
        inventory.error = Some(error_code(&e).code());
    }
    inventory
}

pub fn daily_settings(store: &steve_store::Store) -> steve_store::Result<DailySettings> {
    store.read(|r| steve_store::backup_policy::daily(r))
}

pub fn save_daily_settings(
    store: &steve_store::Store,
    settings: DailySettings,
    author: &str,
) -> steve_store::Result<()> {
    store.write(|w| steve_store::backup_policy::set_daily(w, settings, author))
}

/// The channel cleanup adapter owns installation paths; it opens an independent snapshot,
/// so a backup never takes the Store's writer mutex.
pub struct ChannelBackup {
    home: PathBuf,
    database: PathBuf,
}
impl ChannelBackup {
    pub fn new(home: PathBuf, database: PathBuf) -> Self {
        Self { home, database }
    }
}
impl steve_channels::PreDestructiveBackup for ChannelBackup {
    fn backup(&self) -> steve_channels::ChannelResult<SnapshotId> {
        create(&self.home, &self.database, Purpose::PreDestructive)
            .map(|receipt| receipt.snapshot_id)
            .map_err(|error| steve_modules::ApiError::Internal(format!("backup_failed: {error}")))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_space_estimate_includes_uncheckpointed_wal_bytes() {
        let home = tempfile::tempdir_in("/tmp").unwrap();
        let db = home.path().join("fixture.sqlite3");
        fs::write(&db, [0; 4096]).unwrap();
        assert_eq!(estimated_database_space(&db).unwrap(), 8192);
        fs::write(db.with_extension("sqlite3-wal"), [0; 1024]).unwrap();
        assert_eq!(estimated_database_space(&db).unwrap(), 10240);
    }

    #[test]
    fn retention_reclaims_expired_daily_payload_and_keeps_manual_and_latest() {
        let home = tempfile::tempdir_in("/tmp").unwrap();
        let path = home.path();
        let db = path.join("fixture.db");
        let store = steve_store::Store::open(&db, steve_store::NoPublisher).unwrap();
        steve_store::testkit::hub::backup::seed_retention_payload(&store).unwrap();
        let expired = create(path, &db, Purpose::Daily).unwrap();
        steve_store::testkit::hub::backup::clear_retention_payload(&store).unwrap();
        let newest = create(path, &db, Purpose::Daily).unwrap();
        let manual = create(path, &db, Purpose::Manual).unwrap();
        let before = capacity(&root(path)).unwrap().bytes;
        retain(&store, path, u64::MAX).unwrap();
        let ids: Vec<_> = list(path)
            .unwrap()
            .into_iter()
            .map(|s| s.snapshot_id)
            .collect();
        assert!(!ids.contains(&expired.snapshot_id));
        assert!(ids.contains(&newest.snapshot_id));
        assert!(ids.contains(&manual.snapshot_id));
        assert!(before.saturating_sub(capacity(&root(path)).unwrap().bytes) > 1_000_000);
        let stage = tempfile::tempdir_in("/tmp").unwrap();
        for id in ids {
            assert!(restore_to(path, &id, stage.path()).is_ok());
        }
    }
}
