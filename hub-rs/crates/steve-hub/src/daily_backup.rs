//! Daily scheduling for the shared backup implementation. Once at/after 03:30 local,
//! with the existing startup delay, retry interval and STEVE_BACKUP switch.
use anyhow::Result;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use steve_store_sqlite::backup::{self, CancelFlag, Reason};
const FROM_MINUTE: u32 = 3 * 60 + 30;
const TICK: Duration = Duration::from_secs(600);
const FIRST: Duration = Duration::from_secs(300);
const RETRY_TICKS: u32 = 6;
pub fn dir(home: &Path) -> PathBuf {
    crate::backup::root(home)
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

fn at_local_day(ms: u64) -> String {
    local(ms / 1000).0
}
pub fn due(home: &Path, day: &str, minute: u32) -> bool {
    minute >= FROM_MINUTE
        && !backup::list(&dir(home))
            .unwrap_or_default()
            .iter()
            .any(|m| m.reason == Reason::Daily && at_local_day(m.created_ms) == day)
}
#[derive(Debug, PartialEq)]
pub struct Made {
    pub database: PathBuf,
    pub files: Option<PathBuf>,
}
pub fn make(home: &Path, db: &Path, _day: &str) -> Result<Made> {
    let plan = crate::backup::plan(home, Reason::Daily);
    let m = backup::backup(db, &plan, &|_| {}, &CancelFlag::default())?;
    Ok(Made {
        database: m.database_path(),
        files: m.files.artifacts.first().map(|a| m.directory.join(&a.name)),
    })
}
pub fn status(home: &Path, db: &Path, on: bool, keep_days: u64) -> Value {
    let out = dir(home);
    let all = backup::list(&out).unwrap_or_default();
    let last = all
        .iter()
        .rev()
        .find(|m| m.reason == Reason::Daily)
        .map(|m| {
            // What the file archive takes on disk now; a removed archive counts zero.
            let files_bytes: u64 = m
                .files
                .artifacts
                .iter()
                .filter_map(|a| std::fs::metadata(m.directory.join(&a.name)).ok())
                .map(|f| f.len())
                .sum();
            json!({"name":m.id,"at":m.created_ms,"bytes":m.database.bytes,"files_bytes":files_bytes})
        });
    let retention = backup::retention_status(&out).unwrap_or_else(|_| backup::RetentionStatus {
        failures: 1,
        max_bytes: backup::TOTAL_BYTES,
        ..Default::default()
    });
    json!({"database":db,"dir":out,"daily":on,"keep_days":keep_days,"last":last,"purposes":retention.purposes,"retention":{"bytes":retention.bytes,"manual_bytes":retention.manual_bytes,"retiring_bytes":retention.retiring_bytes,"retiring_count":retention.retiring_count,"failures":crate::backup::retention_failures(home),"inventory_failures":retention.failures,"max_bytes":retention.max_bytes,"over_limit":retention.over_limit,"rules":backup::RULES}})
}
/// Re-read settings immediately before a scheduled generation; disabling never deletes copies.
pub fn make_if_enabled(
    store: &steve_store::Store,
    home: &Path,
    db: &Path,
    day: &str,
) -> Result<Option<Made>> {
    if !crate::backup::daily_settings(store)?.enabled {
        return Ok(None);
    }
    make(home, db, day).map(Some)
}
/// The background loop (a Hub job): looks every ten minutes whether the day's copy is due.
pub async fn run(home: PathBuf, db: PathBuf, store: steve_store::Store, on: bool) {
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
            let store_for_copy = store.clone();
            match tokio::task::spawn_blocking(move || {
                make_if_enabled(&store_for_copy, &h, &d, &day)
            })
            .await
            {
                Ok(Ok(None)) => {}
                Ok(Ok(Some(m))) => tracing::info!(
                    "daily backup: {}{}",
                    m.database.display(),
                    if m.files.is_some() {
                        " and its files"
                    } else {
                        ""
                    }
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
    use std::{fs, os::unix::fs::PermissionsExt};
    const NOT_COPIED: &str = "keychain-pass";

    /// Only the owner reads what the copies hold.
    fn owner_only(p: &Path) -> bool {
        fs::metadata(p).is_ok_and(|m| m.permissions().mode() & 0o077 == 0)
    }

    fn current_database(path: &Path) -> rusqlite::Connection {
        let mut connection = rusqlite::Connection::open(path).unwrap();
        steve_store_sqlite::migrate::initialize(
            &mut connection,
            &steve_store_sqlite::layout::Layout::single_file(),
        )
        .unwrap();
        connection
    }

    fn home() -> (tempfile::TempDir, PathBuf) {
        let t = tempfile::tempdir().unwrap();
        let db = t.path().join("data/steve.sqlite3");
        fs::create_dir_all(t.path().join("data/files/a")).unwrap();
        let c = current_database(&db);
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
    fn the_local_day_and_minute() {
        let (day, minute) = local(now_secs());
        assert_eq!(day.len(), 8);
        assert!(day.bytes().all(|b| b.is_ascii_digit()) && minute < 24 * 60);
    }

    #[test]
    fn due_from_half_past_three_until_the_days_copy_is_there() {
        let (t, db) = home();
        assert!(!due(t.path(), &local(now_secs()).0, FROM_MINUTE - 1));
        assert!(due(t.path(), &local(now_secs()).0, FROM_MINUTE));
        make(t.path(), &db, "20260102").unwrap();
        assert!(!due(t.path(), &local(now_secs()).0, 23 * 60));
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
        fs::create_dir_all(h.join("workspaces/ST-8/outputs")).unwrap();
        fs::write(h.join("workspaces/ST-8/outputs/report.md"), "design").unwrap();
        fs::create_dir_all(h.join("deliveries/objects")).unwrap();
        fs::write(h.join("deliveries/objects/digest"), "immutable delivery").unwrap();

        let m = make(h, &db, "20260102").unwrap();
        assert!(
            m.database
                .parent()
                .unwrap()
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("daily-")
        );
        let c = rusqlite::Connection::open(&m.database).unwrap();
        let x: String = c.query_row("SELECT x FROM t", [], |r| r.get(0)).unwrap();
        assert_eq!(x, "kept");
        let files = m.files.expect("files");
        assert_eq!(files, m.database.parent().unwrap().join("files.tar"));
        assert_eq!(
            names(&files),
            [
                "data/files",
                "data/files/a",
                "data/files/a/note.txt",
                "deliveries",
                "deliveries/objects",
                "deliveries/objects/digest",
                "secrets",
                "secrets/owner.token",
                "signing/keys",
                "signing/keys/id",
                "steve.env",
                "workspaces",
                "workspaces/ST-8",
                "workspaces/ST-8/outputs",
                "workspaces/ST-8/outputs/report.md",
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

    /// A pinned read transaction lets bounded backup steps finish while another connection
    /// keeps committing to WAL.
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
    fn foreign_and_interrupted_files_are_not_deleted() {
        let (t, db) = home();
        let out = t.path().join("backups");
        fs::create_dir_all(&out).unwrap();
        fs::write(out.join("notes.hub.partial"), "foreign").unwrap();
        fs::create_dir(out.join("daily-interrupted.partial")).unwrap();
        make(t.path(), &db, "today").unwrap();
        assert!(out.join("notes.hub.partial").exists());
        assert!(out.join("daily-interrupted.partial").exists());
    }
    #[test]
    fn a_home_with_nothing_but_the_database_has_no_tar() {
        let t = tempfile::tempdir().unwrap();
        let db = t.path().join("steve.sqlite3");
        current_database(&db)
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
        assert!(!t.path().join("backups").exists());
    }

    #[test]
    fn only_completed_directories_participate_in_retention() {
        let (t, db) = home();
        let made = make(t.path(), &db, "today").unwrap();
        let path = made.database.parent().unwrap().join("manifest.json");
        let mut manifest: backup::Manifest =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        manifest.created_ms = 0;
        fs::write(path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        make(t.path(), &db, "newest").unwrap();
        let root = dir(t.path());
        fs::write(root.join("daily-notes.txt"), "foreign").unwrap();
        let removed = backup::retain_at(
            &root,
            backup::plan::now_ms() + (backup::DEFAULT_DAILY_KEEP_DAYS + 1) * 86400000,
            steve_store_sqlite::backup::DEFAULT_DAILY_KEEP_DAYS,
        )
        .unwrap();
        assert_eq!(removed.removed, 1);
        assert!(!made.database.exists());
        assert!(root.join("daily-notes.txt").exists());
    }
    #[test]
    fn daily_backup_only_publishes_and_the_retention_runner_cleans_later() {
        let (t, db) = home();
        let first = make(t.path(), &db, "first").unwrap();
        let path = first.database.parent().unwrap().join("manifest.json");
        let mut manifest: backup::Manifest =
            serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        manifest.created_ms = 0;
        fs::write(path, serde_json::to_vec(&manifest).unwrap()).unwrap();
        let latest = make(t.path(), &db, "latest").unwrap();
        assert!(first.database.exists());
        assert_eq!(
            backup::retain_at(
                &dir(t.path()),
                backup::plan::now_ms(),
                steve_store_sqlite::backup::DEFAULT_DAILY_KEEP_DAYS
            )
            .unwrap()
            .removed,
            1
        );
        assert!(!first.database.exists() && latest.database.exists());
    }
    #[test]
    fn status_names_the_places_and_the_newest_copy() {
        let (t, db) = home();
        let v = status(t.path(), &db, true, backup::DEFAULT_DAILY_KEEP_DAYS);
        assert_eq!(v["database"], db.to_string_lossy().as_ref());
        assert_eq!(
            v["dir"],
            t.path().join("backups").to_string_lossy().as_ref()
        );
        assert_eq!((&v["daily"], &v["last"]), (&json!(true), &Value::Null));
        make(t.path(), &db, "20260102").unwrap();
        make(t.path(), &db, "20260103").unwrap();
        let v = status(t.path(), &db, false, backup::DEFAULT_DAILY_KEEP_DAYS);
        assert_eq!(v["daily"], false);
        assert_eq!(v["keep_days"], backup::DEFAULT_DAILY_KEEP_DAYS);
        assert_eq!(v["retention"]["max_bytes"], backup::TOTAL_BYTES);
        assert_eq!(v["retention"]["over_limit"], false);
        assert!(v["last"]["name"].as_str().unwrap().starts_with("daily-"));
        assert!(v["last"]["bytes"].as_u64().unwrap() > 0 && v["last"]["at"].as_u64().unwrap() > 0);
    }
}
