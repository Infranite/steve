//! Offline administrative opens must be inert until an explicit repository write.
use rusqlite::{Connection, config::DbConfig};
use std::{
    fs,
    time::{Duration, UNIX_EPOCH},
};
use steve_store::{
    FacadeMode, NoPublisher, Store,
    diagnostics::{Record, RecordStatus},
};

fn admin(path: &std::path::Path) -> steve_store::Result<Store> {
    let engine = steve_store_sqlite::open::open_admin(path.parent().unwrap())?;
    Store::from_engine(engine, NoPublisher, FacadeMode::Admin)
}
fn seed(path: &std::path::Path) {
    let mut c = Connection::open(path).unwrap();
    steve_store_sqlite::migrate::initialize(
        &mut c,
        &steve_store_sqlite::layout::Layout::single_file(),
    )
    .unwrap();
    c.execute_batch("DELETE FROM schema_compat; INSERT INTO schema_compat(rowid,min_reader) VALUES(29,1000);
        CREATE TRIGGER protect_meta BEFORE UPDATE ON meta BEGIN SELECT RAISE(ABORT,'metadata write'); END;
        CREATE TRIGGER protect_compat BEFORE DELETE ON schema_compat BEGIN SELECT RAISE(ABORT,'compat write'); END;").unwrap();
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
    let store = admin(&path).unwrap();
    assert!(store.access().tokens(None).unwrap().is_empty());
    assert_eq!(
        store.diagnostics().record(Record::default()),
        RecordStatus::Unavailable
    );
    store
        .with_conn(|c| {
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
            assert!(!c.query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='schema_version')",
                [],
                |r| r.get::<_, bool>(0)
            )?);
            assert_eq!(
                c.query_row("SELECT rowid,min_reader FROM schema_compat", [], |r| Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?
                )))?,
                (29, steve_store_sqlite::compat::LEGACY_READER_FENCE)
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
    let store = admin(&path).unwrap();
    assert!(store.access().tokens(None).unwrap().is_empty());
    drop(store);
    other.execute_batch("ROLLBACK").unwrap();
}

#[test]
fn empty_legacy_partial_and_future_headers_are_refused_without_migrating() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("steve.sqlite3");
    assert!(admin(&missing).is_err());
    assert!(!missing.exists());
    for sql in [
        "",
        "CREATE TABLE schema_version(v INTEGER NOT NULL); INSERT INTO schema_version VALUES(113)",
        "CREATE TABLE schema_version(v INTEGER NOT NULL); INSERT INTO schema_version VALUES(114)",
        "CREATE TABLE schema_version(v); INSERT INTO schema_version VALUES(113),(113)",
        "CREATE TABLE meta(domain TEXT,key TEXT,value TEXT)",
    ] {
        let case = tempfile::tempdir().unwrap();
        let path = case.path().join("steve.sqlite3");
        let c = Connection::open(&path).unwrap();
        c.execute_batch(sql).unwrap();
        drop(c);
        let before = fs::read(&path).unwrap();
        assert!(admin(&path).is_err());
        assert_eq!(fs::read(path).unwrap(), before);
    }
    for mutation in [
        "UPDATE domain_schema SET version=999 WHERE domain='core'",
        "UPDATE meta SET value='999' WHERE key='min_reader'",
        "UPDATE schema_compat SET min_reader=1001",
    ] {
        let case = tempfile::tempdir().unwrap();
        let path = case.path().join("steve.sqlite3");
        let mut c = Connection::open(&path).unwrap();
        steve_store_sqlite::migrate::initialize(
            &mut c,
            &steve_store_sqlite::layout::Layout::single_file(),
        )
        .unwrap();
        c.execute_batch(mutation).unwrap();
        drop(c);
        let before = fs::read(&path).unwrap();
        assert!(admin(&path).is_err());
        assert_eq!(fs::read(path).unwrap(), before);
    }
}
