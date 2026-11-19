//! Daily scheduling for the shared backup implementation. Once at/after 03:30 local,
//! with the existing startup delay, retry interval and STEVE_BACKUP switch.
use anyhow::Result;
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use steve_store_sqlite::backup::{self, BackupPlan, CancelFlag, Reason};
pub const KEEP_DAYS: u64 = 14;
const FROM_MINUTE: u32 = 3 * 60 + 30;
const TICK: Duration = Duration::from_secs(600);
const FIRST: Duration = Duration::from_secs(300);
const RETRY_TICKS: u32 = 6;
pub fn enabled(var: Option<&str>) -> bool {
    !var.is_some_and(|v| v.trim().eq_ignore_ascii_case("off"))
}
pub fn dir(home: &Path) -> PathBuf {
    home.join("backups")
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
    pub removed: usize,
}
pub fn make(home: &Path, db: &Path, _day: &str) -> Result<Made> {
    let mut plan = BackupPlan::new(dir(home), Reason::Daily);
    plan.keep = backup::Keep::All;
    plan.with_files = Some(crate::admin::client::backup::files(home));
    let m = backup::backup(db, &plan, &|_| {}, &CancelFlag::default())?;
    let removed = backup::prune(&plan.dest_root, &Reason::Daily)?;
    Ok(Made {
        database: m.database_path(),
        files: m.files.artifacts.first().map(|a| m.directory.join(&a.name)),
        removed,
    })
}
pub fn status(home: &Path, db: &Path, on: bool) -> Value {
    let out = dir(home);
    let all = backup::list(&out).unwrap_or_default();
    let last = all
        .iter()
        .rev()
        .find(|m| m.reason == Reason::Daily)
        .map(|m| json!({"name":m.id,"at":m.created_ms,"bytes":m.database.bytes}));
    json!({"database":db,"dir":out,"daily":on,"keep_days":KEEP_DAYS,"last":last,"purposes":backup::usage(&out).unwrap_or_default()})
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
    use std::{fs, os::unix::fs::PermissionsExt};
    const NOT_COPIED: &str = "keychain-pass";

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
        assert_eq!(m.removed, 0);
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
    fn only_completed_directories_participate_in_retention() {
        let (t, db) = home();
        let made = make(t.path(), &db, "today").unwrap();
        let root = dir(t.path());
        fs::write(root.join("daily-notes.txt"), "foreign").unwrap();
        let removed = backup::prune_at(
            &root,
            &Reason::Daily,
            backup::plan::now_ms() + (KEEP_DAYS + 1) * 86400000,
        )
        .unwrap();
        assert_eq!(removed, 1);
        assert!(!made.database.exists());
        assert!(root.join("daily-notes.txt").exists());
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
        assert!(v["last"]["name"].as_str().unwrap().starts_with("daily-"));
        assert!(v["last"]["bytes"].as_u64().unwrap() > 0 && v["last"]["at"].as_u64().unwrap() > 0);
    }
}
