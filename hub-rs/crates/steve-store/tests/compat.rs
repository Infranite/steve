//! The reader gate (`schema_compat`): a database records the oldest schema a build must know to open it,
//! every open checks it before writing anything, and the stamp never goes down. Plus the lock on released
//! migration files: once a migration may have reached a database it can't change.

use rusqlite::Connection;
use sha2::{Digest, Sha256};
use steve_store::{StoreError, db};

fn fresh() -> Connection {
    let c = Connection::open_in_memory().unwrap();
    db::migrate(&c).unwrap();
    c
}

fn one(c: &Connection, sql: &str) -> i64 {
    c.query_row(sql, [], |r| r.get(0)).unwrap()
}

#[test]
fn a_migrated_database_records_the_schema_it_needs() {
    let c = fresh();
    assert_eq!(
        db::compat(&c).unwrap(),
        db::Compat {
            schema: db::SCHEMA_VERSION,
            min_reader: db::SCHEMA_VERSION,
        }
    );
    assert_eq!(one(&c, "SELECT count(*) FROM schema_compat"), 1);
    db::migrate(&c).unwrap();
    assert_eq!(
        one(&c, "SELECT count(*) FROM schema_compat"),
        1,
        "one row, every open"
    );
}

#[test]
fn a_database_that_needs_a_newer_build_is_refused_untouched() {
    let c = fresh();
    let ahead = db::SCHEMA_VERSION + 1;
    c.execute_batch(&format!(
        "UPDATE schema_version SET v = {ahead}; UPDATE schema_compat SET min_reader = {ahead};"
    ))
    .unwrap();
    let before: i64 = one(&c, "SELECT total_changes()");
    match db::migrate(&c) {
        Err(StoreError::Incompatible {
            schema,
            min_reader,
            ours,
        }) => {
            assert_eq!(
                (schema, min_reader, ours),
                (ahead, ahead, db::SCHEMA_VERSION)
            );
        }
        r => panic!("opened a database that needs v{ahead}: {r:?}"),
    }
    assert_eq!(one(&c, "SELECT total_changes()"), before, "nothing written");
    assert_eq!(one(&c, "SELECT v FROM schema_version"), ahead);
}

#[test]
fn a_newer_database_no_build_stamped_is_refused() {
    // 0.2.13 and older don't stamp: what such a build migrated needs a build that knows its schema
    let c = fresh();
    let ahead = db::SCHEMA_VERSION + 1;
    c.execute_batch(&format!(
        "DROP TABLE schema_compat; UPDATE schema_version SET v = {ahead};"
    ))
    .unwrap();
    assert!(matches!(
        db::migrate(&c),
        Err(StoreError::Incompatible { .. })
    ));
    assert_eq!(
        one(
            &c,
            "SELECT count(*) FROM sqlite_master WHERE name = 'schema_compat'"
        ),
        0
    );
}

#[test]
fn an_older_unstamped_database_is_migrated_and_stamped() {
    let c = fresh();
    c.execute_batch("DROP TABLE schema_compat").unwrap();
    db::migrate(&c).unwrap();
    assert_eq!(
        one(&c, "SELECT min_reader FROM schema_compat"),
        db::SCHEMA_VERSION
    );
}

#[test]
fn a_newer_database_that_asks_only_for_this_schema_is_opened_unchanged() {
    let c = fresh();
    let ahead = db::SCHEMA_VERSION + 1;
    c.execute_batch(&format!("UPDATE schema_version SET v = {ahead}"))
        .unwrap();
    db::migrate(&c).unwrap();
    assert_eq!(
        one(&c, "SELECT v FROM schema_version"),
        ahead,
        "not written down"
    );
    assert_eq!(
        one(&c, "SELECT min_reader FROM schema_compat"),
        db::SCHEMA_VERSION
    );
}

#[test]
fn the_stamp_never_goes_down() {
    let c = fresh();
    // a stamp an earlier, stricter build left (still one this build can read)
    c.execute_batch(&format!(
        "UPDATE schema_compat SET min_reader = {}",
        db::SCHEMA_VERSION
    ))
    .unwrap();
    db::migrate(&c).unwrap();
    assert_eq!(
        one(&c, "SELECT min_reader FROM schema_compat"),
        db::SCHEMA_VERSION
    );
}

/// `migrations/LOCK`: the sha256 of every migration file. A released migration is frozen (changes go into a
/// new one), so a changed hash fails; a new migration adds its line.
#[test]
fn released_migrations_are_frozen() {
    let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("migrations");
    let lock = std::fs::read_to_string(dir.join("LOCK")).unwrap();
    let locked: Vec<(&str, &str)> = lock
        .lines()
        .filter(|l| !l.trim().is_empty() && !l.starts_with('#'))
        .map(|l| l.split_once(' ').expect("`<file> <sha256>`"))
        .collect();
    let mut files: Vec<String> = std::fs::read_dir(&dir)
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.ends_with(".sql"))
        .collect();
    files.sort();
    // one NNN.sql per migration; parts of one (027_items.sql) are locked too
    let numbered = files
        .iter()
        .filter(|f| f.len() == "000.sql".len() && f[..3].bytes().all(|b| b.is_ascii_digit()))
        .count();
    assert_eq!(
        numbered as i64,
        db::SCHEMA_VERSION,
        "one NNN.sql per migration"
    );
    assert_eq!(
        locked.iter().map(|(f, _)| *f).collect::<Vec<_>>(),
        files,
        "LOCK lists every migration file, in order"
    );
    for (f, want) in locked {
        let got: String = Sha256::digest(std::fs::read(dir.join(f)).unwrap())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        assert_eq!(
            got, want,
            "{f} changed after release: put the change in a new migration"
        );
    }
}

#[test]
fn connector_reference_migration_is_registered_and_upgrades_v69_once() {
    let c = fresh();
    assert_eq!(
        one(
            &c,
            "SELECT count(*) FROM sqlite_master WHERE name='connector_refs' AND type='table'"
        ),
        1
    );
    // Recreate the immediately previous schema without guessing the conditional runner columns.
    c.execute_batch("DROP TABLE connector_refs; UPDATE schema_version SET v=69; UPDATE schema_compat SET min_reader=69;").unwrap();
    db::migrate(&c).unwrap();
    assert_eq!(
        one(
            &c,
            "SELECT count(*) FROM sqlite_master WHERE name='connector_refs' AND type='table'"
        ),
        1
    );
    assert_eq!(
        one(
            &c,
            "SELECT count(*) FROM pragma_foreign_key_list('connector_refs')"
        ),
        3
    );
    assert_eq!(
        one(
            &c,
            "SELECT count(*) FROM sqlite_master WHERE name IN ('connector_refs_connection','connector_refs_expiry')"
        ),
        2
    );
    db::migrate(&c).unwrap();
    assert_eq!(db::compat(&c).unwrap().schema, db::SCHEMA_VERSION);
    assert_eq!(one(&c, "SELECT count(*) FROM pragma_foreign_key_check"), 0);
}
