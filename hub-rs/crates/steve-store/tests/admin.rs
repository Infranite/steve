//! Offline administrative opens must be inert until an explicit repository write.
use rusqlite::{Connection, config::DbConfig};
use std::{
    fs,
    time::{Duration, UNIX_EPOCH},
};
use steve_store::testkit::SqliteFixtures;
use steve_store::{
    Store, db,
    diagnostics::{Record, RecordStatus},
};

fn seed(path: &std::path::Path) {
    let c = Connection::open(path).unwrap();
    db::migrate(&c).unwrap();
    // A runtime reopen would restamp compatibility and replace its row; admin must preserve both.
    c.execute_batch(&format!("DELETE FROM schema_version; INSERT INTO schema_version(rowid,v) VALUES(17,{});
        DELETE FROM schema_compat; INSERT INTO schema_compat(rowid,min_reader) VALUES(29,{});
        CREATE TRIGGER protect_version BEFORE UPDATE ON schema_version BEGIN SELECT RAISE(ABORT,'version write'); END;
        CREATE TRIGGER protect_compat BEFORE DELETE ON schema_compat BEGIN SELECT RAISE(ABORT,'compat write'); END;",
        db::SCHEMA_VERSION, db::SCHEMA_VERSION-1)).unwrap();
}

#[test]
fn an_admin_list_does_not_initialize_optimize_record_or_checkpoint_pending_wal() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("steve.sqlite3");
    seed(&path);
    let c = Connection::open(&path).unwrap();
    c.pragma_update(None, "journal_mode", "WAL").unwrap();
    c.set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)
        .unwrap();
    c.execute_batch("CREATE TABLE pending_wal(value); INSERT INTO pending_wal VALUES('retained');")
        .unwrap();
    drop(c);
    let stamp = UNIX_EPOCH + Duration::from_secs(946684800);
    fs::File::options()
        .write(true)
        .open(&path)
        .unwrap()
        .set_modified(stamp)
        .unwrap();
    let bytes = fs::read(&path).unwrap();
    let wal = path.with_extension("sqlite3-wal");
    let frames = fs::read(&wal).unwrap();
    let store = Store::open_admin(&path).unwrap();
    assert!(store.access().tokens(None).unwrap().is_empty());
    assert_eq!(
        store.diagnostics().record(Record::default()),
        RecordStatus::Unavailable
    );
    store
        .fixture_write(|c| {
            assert_eq!(
                c.query_row("SELECT total_changes()", [], |r| r.get::<_, i64>(0))?,
                0
            );
            assert_eq!(
                c.query_row("PRAGMA analysis_limit", [], |r| r.get::<_, i64>(0))?,
                0
            );
            assert_eq!(
                c.query_row("SELECT count(*) FROM workspaces", [], |r| r
                    .get::<_, i64>(0))?,
                0
            );
            assert_eq!(
                c.query_row("SELECT rowid,v FROM schema_version", [], |r| Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?
                )))?,
                (17, db::SCHEMA_VERSION)
            );
            assert_eq!(
                c.query_row("SELECT rowid,min_reader FROM schema_compat", [], |r| Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?
                )))?,
                (29, db::SCHEMA_VERSION - 1)
            );
            assert_eq!(
                c.query_row("SELECT value FROM pending_wal", [], |r| r
                    .get::<_, String>(0))?,
                "retained"
            );
            Ok(())
        })
        .unwrap();
    drop(store);
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert_eq!(fs::metadata(&path).unwrap().modified().unwrap(), stamp);
    assert_eq!(
        fs::read(wal).unwrap(),
        frames,
        "list must not checkpoint or replace existing WAL"
    );
}

#[test]
fn opening_and_listing_do_not_acquire_sqlite_write_locks() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("steve.sqlite3");
    seed(&path);
    let other = Connection::open(&path).unwrap();
    other.execute_batch("BEGIN IMMEDIATE").unwrap();
    let store = Store::open_admin(&path).unwrap();
    assert!(store.access().tokens(None).unwrap().is_empty());
    drop(store);
    other.execute_batch("ROLLBACK").unwrap();
}

#[test]
fn non_head_missing_and_ambiguous_headers_are_refused_without_creating_or_migrating() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("missing/steve.sqlite3");
    assert!(Store::open_admin(&missing).is_err());
    assert!(!missing.exists());
    for (n, (version, min_reader)) in [
        (50, 50),
        (db::SCHEMA_VERSION + 1, db::SCHEMA_VERSION),
        (db::SCHEMA_VERSION, db::SCHEMA_VERSION + 1),
        (db::SCHEMA_VERSION, 0),
    ]
    .into_iter()
    .enumerate()
    {
        let directory = dir.path().join(format!("invalid-{n}"));
        fs::create_dir(&directory).unwrap();
        let path = directory.join("steve.sqlite3");
        let c = Connection::open(&path).unwrap();
        c.execute_batch(&format!("CREATE TABLE schema_version(v); INSERT INTO schema_version VALUES({version});
            CREATE TABLE schema_compat(min_reader); INSERT INTO schema_compat VALUES({min_reader});")).unwrap();
        drop(c);
        let bytes = fs::read(&path).unwrap();
        let error = Store::open_admin(&path).err().unwrap().to_string();
        assert!(error.contains("请先启动或升级 Hub"), "{error}");
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
    for sql in [
        format!(
            "CREATE TABLE schema_version(v); INSERT INTO schema_version VALUES({});",
            db::SCHEMA_VERSION
        ),
        format!(
            "CREATE TABLE schema_version(v); INSERT INTO schema_version VALUES({0}),({0}); CREATE TABLE schema_compat(min_reader); INSERT INTO schema_compat VALUES({0});",
            db::SCHEMA_VERSION
        ),
    ] {
        let directory = dir.path().join(steve_store::uid("header"));
        fs::create_dir(&directory).unwrap();
        let path = directory.join("steve.sqlite3");
        let c = Connection::open(&path).unwrap();
        c.execute_batch(&sql).unwrap();
        drop(c);
        let bytes = fs::read(&path).unwrap();
        assert!(
            Store::open_admin(&path)
                .err()
                .unwrap()
                .to_string()
                .contains("请先启动或升级 Hub")
        );
        assert_eq!(fs::read(path).unwrap(), bytes);
    }
}
