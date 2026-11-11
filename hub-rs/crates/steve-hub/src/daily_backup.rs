//! The daily copy under `<home>/backups`, made by the Hub itself so that every installation has one
//! whatever installed it: once a day, at or after 03:30 local time (a Hub that was not running then
//! makes it when it next is), kept 14 days.
//! - `daily-YYYYMMDD.sqlite3`: an online backup of the database, checked (`quick_check`) before it
//!   is kept;
//! - `daily-YYYYMMDD-files.tar`: what the database alone cannot bring back, readable by the owner
//!   only: attachments (`data/files`), the signing identity (`signing`, without the keychain's
//!   password: a copy in a file would unlock the identity for anyone who reads the backup), the
//!   secret files (`secrets`) and the configuration (`steve.env`), as far as they are in the home.
//!
//! Same disk, no restore drill: a copy against mistakes and a corrupt database, not disaster
//! recovery. The names are those of `ops/backup.sh`, the job of a source-based installation: where
//! both run, the day's copy is made once.
//!
//! `STEVE_BACKUP=off` turns it off (the copies before an upgrade are made either way).

use std::{
    fs,
    io::Write,
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use serde_json::{Value, json};

/// Days a daily copy is kept.
pub const KEEP_DAYS: u64 = 14;
/// The minute of the local day from which the day's copy is due: 03:30.
const FROM_MINUTE: u32 = 3 * 60 + 30;
const TICK: Duration = Duration::from_secs(600);
const FIRST: Duration = Duration::from_secs(300);
/// After a copy that failed (a full disk): not before this many ticks.
const RETRY_TICKS: u32 = 6;
/// What the tar takes of the home, where it is there.
const PARTS: [&str; 4] = ["data/files", "signing", "secrets", "steve.env"];
/// How often the files are tried for a day's copy.
const TAR_TRIES: u32 = 3;
/// What the name of a copy still being written ends in (SQLite's journal comes after it).
const PARTIAL: &str = ".hub.partial";
/// In `signing`: the keychain's password as an earlier installation kept it.
const NOT_COPIED: &str = "keychain-pass";

/// Whether a `STEVE_BACKUP` value leaves the daily copy on.
pub fn enabled(var: Option<&str>) -> bool {
    !var.is_some_and(|v| v.trim().eq_ignore_ascii_case("off"))
}

pub fn dir(home: &Path) -> PathBuf {
    home.join("backups")
}

fn db_name(day: &str) -> String {
    format!("daily-{day}.sqlite3")
}

fn tar_name(day: &str) -> String {
    format!("daily-{day}-files.tar")
}

/// The local day (`YYYYMMDD`) and minute of the day at `secs` since the epoch.
fn local(secs: u64) -> (String, u32) {
    let t = secs as libc::time_t;
    // SAFETY: localtime_r only writes the zeroed tm it is given.
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&t, &mut tm) };
    (
        format!(
            "{:04}{:02}{:02}",
            tm.tm_year + 1900,
            tm.tm_mon + 1,
            tm.tm_mday
        ),
        (tm.tm_hour * 60 + tm.tm_min) as u32,
    )
}

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

/// Whether the copy of `day` is still to be made at `minute` of that day.
pub fn due(home: &Path, day: &str, minute: u32) -> bool {
    minute >= FROM_MINUTE && !dir(home).join(db_name(day)).exists()
}

/// The day of a daily copy's file name, a database's or a tar's.
fn day_of(name: &str) -> Option<&str> {
    let rest = name.strip_prefix("daily-")?;
    let day = rest
        .strip_suffix("-files.tar")
        .or_else(|| rest.strip_suffix(".sqlite3"))?;
    (day.len() == 8 && day.bytes().all(|b| b.is_ascii_digit())).then_some(day)
}

/// What a day's copy made.
#[derive(Debug, PartialEq)]
pub struct Made {
    pub database: PathBuf,
    /// `None`: nothing of [`PARTS`] is in the home.
    pub files: Option<PathBuf>,
    /// Daily copies past [`KEEP_DAYS`] that were removed.
    pub removed: usize,
}

/// Makes the copy of `day` in `home`: the database first, kept only when it checks out; then the
/// files, whose failure leaves the database's copy standing; then the old copies go.
pub fn make(home: &Path, db: &Path, day: &str) -> Result<Made> {
    let out = dir(home);
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(&out)
        .with_context(|| format!("create {}", out.display()))?;
    // a directory that was there before (the job of a source-based installation makes it too) is
    // the owner's alone from now on
    fs::set_permissions(&out, fs::Permissions::from_mode(0o700))
        .with_context(|| format!("close {}", out.display()))?;
    clear_partials(&out);
    let database = out.join(db_name(day));
    let partial = out.join(format!("{}{PARTIAL}", db_name(day)));
    if let Err(e) = crate::upgrades::supervisor::backup(db, &partial) {
        clear_partials(&out);
        return Err(e.context("database copy"));
    }
    fs::rename(&partial, &database)?;
    // a file that goes away while the tar walks the attachments (an upload being moved into place)
    // fails that tar as a whole, and the day is not looked at again once its database is copied
    let mut files = None;
    for left in (0..TAR_TRIES).rev() {
        match tar(home, &out, day) {
            Ok(f) => {
                files = f;
                break;
            }
            Err(e) if left > 0 => tracing::info!("daily backup: the files once more: {e:#}"),
            Err(e) => tracing::warn!("daily backup: the files were not copied: {e:#}"),
        }
    }
    let removed = prune(&out, SystemTime::now());
    Ok(Made {
        database,
        files,
        removed,
    })
}

/// Removes what a copy that was interrupted (a Hub stopped in the middle of one) left in `out`: the
/// half-made file of any day and the journal SQLite kept beside it. Only one copy is made at a time.
fn clear_partials(out: &Path) {
    for entry in fs::read_dir(out).into_iter().flatten().flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with("daily-") && name.contains(PARTIAL) {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// The tar of [`PARTS`], `None` when the home has none of them.
fn tar(home: &Path, out: &Path, day: &str) -> Result<Option<PathBuf>> {
    let parts: Vec<&str> = PARTS
        .into_iter()
        .filter(|p| home.join(p).exists())
        .collect();
    if parts.is_empty() {
        return Ok(None);
    }
    let dest = out.join(tar_name(day));
    let partial = out.join(format!("{}{PARTIAL}", tar_name(day)));
    let _ = fs::remove_file(&partial);
    let written = (|| {
        let file = fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&partial)?;
        let mut b = tar::Builder::new(file);
        b.follow_symlinks(false);
        for part in parts {
            let path = home.join(part);
            if !path.is_dir() {
                b.append_path_with_name(&path, part)?;
            } else if part == "signing" {
                for entry in fs::read_dir(&path)? {
                    let entry = entry?;
                    let name = entry.file_name();
                    if name == NOT_COPIED {
                        continue;
                    }
                    let inside = Path::new(part).join(&name);
                    if entry.file_type()?.is_dir() {
                        b.append_dir_all(&inside, entry.path())?;
                    } else {
                        b.append_path_with_name(entry.path(), &inside)?;
                    }
                }
            } else {
                b.append_dir_all(part, &path)?;
            }
        }
        let mut file = b.into_inner()?;
        file.flush()?;
        file.sync_all()
    })();
    if let Err(e) = written {
        let _ = fs::remove_file(&partial);
        return Err(e).with_context(|| format!("write {}", dest.display()));
    }
    fs::rename(&partial, &dest)?;
    Ok(Some(dest))
}

/// Removes the daily copies in `out` last written more than [`KEEP_DAYS`] before `now`; nothing
/// else there is touched (the copies made before an upgrade have their own keeper).
fn prune(out: &Path, now: SystemTime) -> usize {
    let Ok(entries) = fs::read_dir(out) else {
        return 0;
    };
    let keep = Duration::from_secs(KEEP_DAYS * 24 * 3600);
    let mut removed = 0;
    for entry in entries.flatten() {
        let name = entry.file_name();
        let old = day_of(&name.to_string_lossy()).is_some()
            && entry
                .metadata()
                .and_then(|m| m.modified())
                .is_ok_and(|at| now.duration_since(at).is_ok_and(|age| age > keep));
        if old && fs::remove_file(entry.path()).is_ok() {
            removed += 1;
        }
    }
    removed
}

/// `GET /api/system/backup`: where the database and its copies are, whether the daily copy is on,
/// and the newest one.
pub fn status(home: &Path, db: &Path, on: bool) -> Value {
    let out = dir(home);
    let last = fs::read_dir(&out)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let name = e.file_name().to_string_lossy().to_string();
            (name.ends_with(".sqlite3") && day_of(&name).is_some()).then_some((name, e))
        })
        .max_by(|a, b| a.0.cmp(&b.0))
        .and_then(|(name, e)| {
            let m = e.metadata().ok()?;
            let at = m.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
            Some(json!({"name": name, "at": at.as_millis() as u64, "bytes": m.len()}))
        });
    json!({
        "database": db.to_string_lossy(),
        "dir": out.to_string_lossy(),
        "daily": on,
        "keep_days": KEEP_DAYS,
        "last": last,
    })
}

/// The background loop (a Hub job): looks every ten minutes whether the day's copy is due.
pub async fn run(home: PathBuf, db: PathBuf, on: bool) {
    if !on {
        tracing::info!("daily backup turned off (STEVE_BACKUP=off)");
        return;
    }
    tokio::time::sleep(FIRST).await;
    let mut wait = 0;
    loop {
        let (day, minute) = local(now_secs());
        if wait > 0 {
            wait -= 1;
        } else if due(&home, &day, minute) {
            let (h, d) = (home.clone(), db.clone());
            match tokio::task::spawn_blocking(move || make(&h, &d, &day)).await {
                Ok(Ok(m)) => tracing::info!(
                    "daily backup: {}{}, {} old removed",
                    m.database.display(),
                    if m.files.is_some() {
                        " and its files"
                    } else {
                        ""
                    },
                    m.removed
                ),
                Ok(Err(e)) => {
                    tracing::warn!("daily backup: {e:#}; trying again in an hour");
                    wait = RETRY_TICKS;
                }
                Err(e) => {
                    tracing::warn!("daily backup: {e}; trying again in an hour");
                    wait = RETRY_TICKS;
                }
            }
        }
        tokio::time::sleep(TICK).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Only the owner reads what the copies hold.
    fn owner_only(p: &Path) -> bool {
        fs::metadata(p).is_ok_and(|m| m.permissions().mode() & 0o077 == 0)
    }

    fn home() -> (tempfile::TempDir, PathBuf) {
        let t = tempfile::tempdir().unwrap();
        let db = t.path().join("data/steve.sqlite3");
        fs::create_dir_all(t.path().join("data/files/a")).unwrap();
        let c = rusqlite::Connection::open(&db).unwrap();
        c.execute_batch("CREATE TABLE t(x); INSERT INTO t VALUES ('kept');")
            .unwrap();
        (t, db)
    }

    fn names(tar: &Path) -> Vec<String> {
        let mut a = tar::Archive::new(fs::File::open(tar).unwrap());
        let mut v: Vec<String> = a
            .entries()
            .unwrap()
            .map(|e| e.unwrap().path().unwrap().to_string_lossy().to_string())
            .map(|n| n.trim_end_matches('/').to_string())
            .collect();
        v.sort();
        v
    }

    #[test]
    fn off_only_when_said() {
        assert!(enabled(None));
        assert!(enabled(Some("")));
        assert!(enabled(Some("on")));
        assert!(!enabled(Some(" OFF ")));
    }

    #[test]
    fn the_local_day_and_minute() {
        let (day, minute) = local(now_secs());
        assert_eq!(day.len(), 8);
        assert!(day.bytes().all(|b| b.is_ascii_digit()) && minute < 24 * 60);
    }

    #[test]
    fn due_from_half_past_three_until_the_days_copy_is_there() {
        let (t, db) = home();
        assert!(!due(t.path(), "20260102", FROM_MINUTE - 1));
        assert!(due(t.path(), "20260102", FROM_MINUTE));
        make(t.path(), &db, "20260102").unwrap();
        assert!(!due(t.path(), "20260102", 23 * 60));
        assert!(due(t.path(), "20260103", FROM_MINUTE));
    }

    #[test]
    fn copies_the_database_and_the_files_for_the_owner_only() {
        let (t, db) = home();
        let h = t.path();
        fs::write(h.join("data/files/a/note.txt"), "attachment").unwrap();
        fs::write(h.join("steve.env"), "STEVE_PORT='8440'\n").unwrap();
        fs::create_dir_all(h.join("secrets")).unwrap();
        fs::write(h.join("secrets/owner.token"), "t").unwrap();
        fs::create_dir_all(h.join("signing/keys")).unwrap();
        fs::write(h.join("signing/keys/id"), "k").unwrap();
        fs::write(h.join("signing").join(NOT_COPIED), "p").unwrap();

        let m = make(h, &db, "20260102").unwrap();
        assert_eq!(m.database, h.join("backups/daily-20260102.sqlite3"));
        assert_eq!(m.removed, 0);
        let c = rusqlite::Connection::open(&m.database).unwrap();
        let x: String = c.query_row("SELECT x FROM t", [], |r| r.get(0)).unwrap();
        assert_eq!(x, "kept");
        let files = m.files.expect("files");
        assert_eq!(files, h.join("backups/daily-20260102-files.tar"));
        assert_eq!(
            names(&files),
            [
                "data/files",
                "data/files/a",
                "data/files/a/note.txt",
                "secrets",
                "secrets/owner.token",
                "signing/keys",
                "signing/keys/id",
                "steve.env",
            ]
        );
        for p in [&h.join("backups"), &m.database, &files] {
            assert!(owner_only(p), "{}", p.display());
        }
        // nothing half-made is left beside them
        let left: Vec<_> = fs::read_dir(h.join("backups"))
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .filter(|n| n.ends_with(".partial"))
            .collect();
        assert!(left.is_empty(), "{left:?}");
    }

    /// Beside a Hub that keeps writing: a copy taken in pieces starts over at every commit of
    /// another connection and never ends.
    #[test]
    fn the_copy_finishes_while_the_database_is_written() {
        use std::sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
            mpsc,
        };
        let (t, db) = home();
        let c = rusqlite::Connection::open(&db).unwrap();
        let mode: String = c
            .query_row("PRAGMA journal_mode = WAL", [], |r| r.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
        // some thousand pages: more than a few pieces
        c.execute_batch(
            "BEGIN;
             WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 4000)
             INSERT INTO t SELECT zeroblob(4000) FROM n;
             COMMIT;",
        )
        .unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        let writer = std::thread::spawn({
            let stop = stop.clone();
            move || {
                let mut written = 0u32;
                while !stop.load(Ordering::Relaxed) {
                    c.execute("INSERT INTO t VALUES ('more')", []).unwrap();
                    written += 1;
                }
                written
            }
        });
        let (tx, rx) = mpsc::channel();
        std::thread::spawn({
            let (h, db) = (t.path().to_path_buf(), db.clone());
            move || tx.send(make(&h, &db, "20260102"))
        });
        let made = rx.recv_timeout(Duration::from_secs(30));
        stop.store(true, Ordering::Relaxed);
        let written = writer.join().unwrap();
        let m = made.expect("the copy did not end beside a writer").unwrap();
        assert!(written > 0, "nothing was written meanwhile");
        let copy = rusqlite::Connection::open(&m.database).unwrap();
        let kept: i64 = copy
            .query_row("SELECT count(*) FROM t WHERE x = 'kept'", [], |r| r.get(0))
            .unwrap();
        assert_eq!(kept, 1);
    }

    #[test]
    fn what_an_interrupted_copy_left_goes_with_the_next() {
        let (t, db) = home();
        let out = t.path().join("backups");
        fs::create_dir_all(&out).unwrap();
        fs::set_permissions(&out, fs::Permissions::from_mode(0o755)).unwrap();
        let stale = [
            "daily-20251231.sqlite3.hub.partial",
            "daily-20251231.sqlite3.hub.partial-journal",
            "daily-20251231-files.tar.hub.partial",
        ];
        for n in stale {
            fs::write(out.join(n), "half").unwrap();
        }
        fs::write(out.join("notes.hub.partial"), "not a copy's").unwrap();
        make(t.path(), &db, "20260102").unwrap();
        for n in stale {
            assert!(!out.join(n).exists(), "{n}");
        }
        assert!(out.join("notes.hub.partial").exists());
        // a directory made by something else is the owner's alone afterwards
        assert!(owner_only(&out));
    }

    #[test]
    fn a_home_with_nothing_but_the_database_has_no_tar() {
        let t = tempfile::tempdir().unwrap();
        let db = t.path().join("steve.sqlite3");
        rusqlite::Connection::open(&db)
            .unwrap()
            .execute_batch("CREATE TABLE t(x);")
            .unwrap();
        let m = make(t.path(), &db, "20260102").unwrap();
        assert_eq!(m.files, None);
        assert!(m.database.exists());
    }

    #[test]
    fn a_missing_database_makes_nothing() {
        let t = tempfile::tempdir().unwrap();
        let e = make(t.path(), &t.path().join("none.sqlite3"), "20260102").unwrap_err();
        assert!(format!("{e:#}").contains("database"), "{e:#}");
        assert_eq!(fs::read_dir(t.path().join("backups")).unwrap().count(), 0);
    }

    #[test]
    fn only_old_daily_copies_are_removed() {
        let t = tempfile::tempdir().unwrap();
        let out = t.path();
        let all = [
            "daily-20260101.sqlite3",
            "daily-20260101-files.tar",
            "pre-0.2.9-20260101-000000.sqlite3",
            "daily-notes.txt",
        ];
        for n in all {
            fs::write(out.join(n), "x").unwrap();
        }
        assert_eq!(prune(out, SystemTime::now()), 0);
        let later = SystemTime::now() + Duration::from_secs((KEEP_DAYS + 1) * 24 * 3600);
        assert_eq!(prune(out, later), 2);
        let mut left: Vec<_> = fs::read_dir(out)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().to_string())
            .collect();
        left.sort();
        assert_eq!(
            left,
            ["daily-notes.txt", "pre-0.2.9-20260101-000000.sqlite3"]
        );
    }

    #[test]
    fn status_names_the_places_and_the_newest_copy() {
        let (t, db) = home();
        let v = status(t.path(), &db, true);
        assert_eq!(v["database"], db.to_string_lossy().as_ref());
        assert_eq!(
            v["dir"],
            t.path().join("backups").to_string_lossy().as_ref()
        );
        assert_eq!((&v["daily"], &v["last"]), (&json!(true), &Value::Null));
        make(t.path(), &db, "20260102").unwrap();
        make(t.path(), &db, "20260103").unwrap();
        let v = status(t.path(), &db, false);
        assert_eq!(v["daily"], false);
        assert_eq!(v["last"]["name"], "daily-20260103.sqlite3");
        assert!(v["last"]["bytes"].as_u64().unwrap() > 0 && v["last"]["at"].as_u64().unwrap() > 0);
    }
}
