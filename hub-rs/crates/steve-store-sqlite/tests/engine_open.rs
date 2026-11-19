use rusqlite::Connection;
use steve_store_sqlite::{
    layout::{self, CORE_FILE, Layout, META_DOMAIN},
    migrate, open, pragmas,
};
use steve_types::Code;

#[test]
fn fresh_database_has_all_domains_and_reopens_without_metadata_writes() {
    let dir = tempfile::tempdir().unwrap();
    let engine = open::open_runtime(dir.path()).unwrap();
    let c = engine.read_conn().unwrap();
    let count: u32 = c
        .query_row(
            "SELECT count(*) FROM domain_schema WHERE version=1",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 3);
    for table in [
        "users",
        "events",
        "chan_messages",
        "meta",
        "uow_applied",
        "cursor",
    ] {
        assert_eq!(
            c.query_row(
                "SELECT count(*) FROM sqlite_schema WHERE name=?1",
                [table],
                |r| r.get::<_, u32>(0)
            )
            .unwrap(),
            1
        );
    }
    // Empty initialization uses baselines directly, never the old runner.
    assert_eq!(
        c.query_row(
            "SELECT count(*) FROM sqlite_schema WHERE name='schema_version'",
            [],
            |r| r.get::<_, u32>(0)
        )
        .unwrap(),
        0
    );
    assert!(
        c.query_row("SELECT min_reader FROM schema_compat", [], |r| r
            .get::<_, i64>(0))
            .unwrap()
            > 113
    );
    let original = engine.layout().clone();
    assert_eq!(original.generation, 2);
    assert_ne!(original.store_generation, original.files[CORE_FILE]);
    assert_eq!(Layout::read(&c).unwrap(), original);
    assert!((4..=16).contains(&engine.readers().max_size()));
    for (name, expected) in [
        ("foreign_keys", 1),
        ("busy_timeout", 5_000),
        ("synchronous", 1),
        ("temp_store", 2),
        ("cache_size", -16_384),
        ("mmap_size", pragmas::MMAP_SIZE),
        ("wal_autocheckpoint", 0),
    ] {
        assert_eq!(
            c.pragma_query_value(None, name, |r| r.get::<_, i64>(0))
                .unwrap(),
            expected,
            "{name}"
        );
    }
    assert!(c.is_readonly("main").unwrap());
    drop(c);
    let writer = Connection::open(engine.path()).unwrap();
    writer.execute_batch("CREATE TRIGGER engine_no_versions BEFORE UPDATE ON domain_schema BEGIN SELECT RAISE(ABORT,'versions rewritten'); END;
        CREATE TRIGGER engine_no_meta BEFORE UPDATE ON meta BEGIN SELECT RAISE(ABORT,'meta rewritten'); END;").unwrap();
    drop(writer);
    drop(engine);
    let engine = open::open_runtime(dir.path()).unwrap();
    assert_eq!(*engine.layout(), original);
    drop(engine);
    assert_eq!(*open::open_admin(dir.path()).unwrap().layout(), original);
}

#[test]
fn future_min_reader_is_rejected_before_wal_or_any_database_write() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(CORE_FILE);
    let mut c = Connection::open(&path).unwrap();
    c.execute_batch(steve_store_sqlite::schema::CONTRACT)
        .unwrap();
    let tx = c.transaction().unwrap();
    Layout::single_file()
        .write_file_meta(&tx, CORE_FILE)
        .unwrap();
    migrate::stamp_current(&tx).unwrap();
    layout::put_meta(&tx, META_DOMAIN, "min_reader", "3").unwrap();
    tx.commit().unwrap();
    drop(c);
    let bytes = std::fs::read(&path).unwrap();
    for result in [open::open_runtime(dir.path()), open::open_admin(dir.path())] {
        assert_eq!(result.err().unwrap().code, Code::Incompatible);
        assert_eq!(std::fs::read(&path).unwrap(), bytes);
        assert!(!dir.path().join("steve.sqlite3-wal").exists());
    }
    let c = open::open_readonly(&path).unwrap();
    assert_eq!(
        c.pragma_query_value(None, "journal_mode", |r| r.get::<_, String>(0))
            .unwrap(),
        "delete"
    );
}

#[test]
fn legacy_and_unknown_databases_require_conversion_but_readonly_can_inspect_them() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(CORE_FILE);
    let c = Connection::open(&path).unwrap();
    c.execute_batch(
        "CREATE TABLE schema_version(v INTEGER); INSERT INTO schema_version VALUES(112);",
    )
    .unwrap();
    drop(c);
    let bytes = std::fs::read(&path).unwrap();
    let error = open::open_runtime(dir.path()).err().unwrap();
    assert_eq!(error.code, Code::Incompatible);
    assert!(error.safe_detail.contains("先升级到上一个发布版"));
    let c = open::open_readonly(&path).unwrap();
    assert_eq!(
        c.query_row("SELECT v FROM schema_version", [], |r| r.get::<_, u32>(0))
            .unwrap(),
        112
    );
    assert!(
        c.execute("INSERT INTO schema_version VALUES(113)", [])
            .is_err()
    );
    drop(c);
    assert_eq!(std::fs::read(&path).unwrap(), bytes);
    assert!(!dir.path().join("steve.sqlite3-wal").exists());
    assert!(open::open_readonly(dir.path().join("missing.sqlite3")).is_err());
    assert!(!dir.path().join("missing.sqlite3").exists());
}

#[test]
fn exclusive_lock_lives_with_engine_and_admin_requires_the_current_schema() {
    let dir = tempfile::tempdir().unwrap();
    let engine = open::open_runtime(dir.path()).unwrap();
    let error = open::open_runtime(dir.path()).err().unwrap();
    assert_eq!(error.code, Code::Busy);
    assert!(error.safe_detail.contains("another process"));
    assert_eq!(open::open_admin(dir.path()).err().unwrap().code, Code::Busy);
    drop(engine);
    let c = Connection::open(dir.path().join(CORE_FILE)).unwrap();
    c.execute(
        "UPDATE domain_schema SET version=0 WHERE domain='stream'",
        [],
    )
    .unwrap();
    c.execute_batch("PRAGMA journal_mode=DELETE").unwrap();
    drop(c);
    let before = std::fs::read(dir.path().join(CORE_FILE)).unwrap();
    assert_eq!(
        open::open_admin(dir.path()).err().unwrap().code,
        Code::Incompatible
    );
    assert_eq!(std::fs::read(dir.path().join(CORE_FILE)).unwrap(), before);
}

#[test]
fn admin_does_not_migrate_stamp_optimize_or_select_wal() {
    let dir = tempfile::tempdir().unwrap();
    drop(open::open_runtime(dir.path()).unwrap());
    let path = dir.path().join(CORE_FILE);
    let c = Connection::open(&path).unwrap();
    c.execute_batch("ANALYZE; DELETE FROM sqlite_stat1; PRAGMA journal_mode=DELETE;")
        .unwrap();
    drop(c);
    let before = std::fs::read(&path).unwrap();
    let engine = open::open_admin(dir.path()).unwrap();
    assert_eq!(
        engine
            .read_conn()
            .unwrap()
            .query_row("SELECT count(*) FROM sqlite_stat1", [], |r| r
                .get::<_, u32>(0))
            .unwrap(),
        0
    );
    drop(engine);
    assert_eq!(std::fs::read(&path).unwrap(), before);
    assert!(!dir.path().join("steve.sqlite3-wal").exists());
}

#[test]
fn runtime_converts_113_under_its_lifetime_lock_and_admin_refuses_legacy() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join(CORE_FILE);
    let c = Connection::open(&path).unwrap();
    steve_store_sqlite::legacy::final_structure(&c).unwrap();
    c.execute_batch("INSERT INTO workspaces(id,name,created) VALUES('w','fixture',0); INSERT INTO users(id,workspace_id,subject,name,role,created) VALUES('o','w','o','o','owner',0)").unwrap();
    drop(c);
    let before = std::fs::read(&path).unwrap();
    assert!(
        open::open_admin(dir.path())
            .err()
            .unwrap()
            .safe_detail
            .contains("Hub")
    );
    assert_eq!(std::fs::read(&path).unwrap(), before);
    let e = open::open_runtime(dir.path()).unwrap();
    assert!(matches!(
        open::inspect(&e.read_conn().unwrap()).unwrap(),
        steve_store_sqlite::compat::StorageState::Current { .. }
    ));
    assert_eq!(open::open_admin(dir.path()).err().unwrap().code, Code::Busy);
    let c = e.read_conn().unwrap();
    assert!(
        c.query_row("SELECT min_reader FROM schema_compat", [], |r| r
            .get::<_, i64>(0))
            .unwrap()
            > 113
    );
    assert_eq!(
        c.pragma_query_value(None, "journal_mode", |r| r.get::<_, String>(0))
            .unwrap(),
        "wal"
    );
    drop(c);
    drop(e);
    drop(open::open_admin(dir.path()).unwrap());
}
#[test]
fn admin_last_close_preserves_existing_database_and_wal_bytes() {
    let dir = tempfile::tempdir().unwrap();
    drop(open::open_runtime(dir.path()).unwrap());
    let path = dir.path().join(CORE_FILE);
    let wal = dir.path().join("steve.sqlite3-wal");
    let c = Connection::open(&path).unwrap();
    c.set_db_config(
        rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE,
        true,
    )
    .unwrap();
    c.execute_batch("PRAGMA wal_autocheckpoint=0; CREATE TABLE engine_admin_wal(x); INSERT INTO engine_admin_wal VALUES(1)").unwrap();
    drop(c); // Leave real committed WAL frames with no keeper writer.
    let before = [std::fs::read(&path).unwrap(), std::fs::read(&wal).unwrap()];
    assert!(!before[1].is_empty());
    let e = open::open_admin(dir.path()).unwrap();
    assert_eq!(
        e.read_conn()
            .unwrap()
            .query_row("SELECT x FROM engine_admin_wal", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
    drop(e);
    assert_eq!(std::fs::read(&path).unwrap(), before[0]);
    assert_eq!(std::fs::read(&wal).unwrap(), before[1]);
}

#[test]
fn observed_runtime_open_reports_one_conversion_and_migration_stream_under_lock() {
    use std::sync::{Arc, Mutex};
    use steve_store_sqlite::migrate::Phase;
    for has_owner in [false, true] {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(CORE_FILE);
        let c = Connection::open(&path).unwrap();
        steve_store_sqlite::legacy::final_structure(&c).unwrap();
        if has_owner {
            c.execute_batch("INSERT INTO workspaces(id,name,created) VALUES('w','fixture',0); INSERT INTO users(id,workspace_id,subject,name,role,created) VALUES('o','w','o','o','owner',0)").unwrap();
        }
        drop(c);
        let before = std::fs::read(&path).unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let log = events.clone();
        let data_dir = dir.path().to_owned();
        let result = open::open_runtime_observed(
            dir.path(),
            Arc::new(move |p| {
                // Each event is emitted with the very same lifetime lock still held.
                assert_eq!(open::open_admin(&data_dir).err().unwrap().code, Code::Busy);
                log.lock().unwrap().push(p);
            }),
        );
        let log = events.lock().unwrap();
        assert!(
            log.windows(2)
                .all(|pair| pair[0].phase <= pair[1].phase && pair[0].vm_steps <= pair[1].vm_steps)
        );
        if has_owner {
            let e = result.unwrap();
            for phase in [
                Phase::LegacyReplay,
                Phase::Validation,
                Phase::InstallContract,
                Phase::DomainMigration,
            ] {
                assert!(log.iter().any(|p| p.phase == phase));
            }
            assert_eq!(log.iter().filter(|p| p.complete).count(), 1);
            assert!(log.last().unwrap().complete);
            assert_eq!(log.last().unwrap().phase, Phase::DomainMigration);
            assert!(matches!(
                open::inspect(&e.read_conn().unwrap()).unwrap(),
                steve_store_sqlite::compat::StorageState::Current { .. }
            ));
        } else {
            assert!(result.is_err());
            assert!(log.iter().all(|p| !p.complete));
            assert_eq!(std::fs::read(&path).unwrap(), before);
        }
    }
}
