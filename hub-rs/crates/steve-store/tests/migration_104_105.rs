//! 104 (the shadow facts of an item's work, #327) and 105 (what the sweep would do, #334) on a database
//! at the schema before them: the upgrade runs both, in their positions, and every table the shadow
//! view and the sweep read is there afterwards. A database left without `item_would_do` would fail every
//! sweep tick and `GET /api/items/{id}` with "no such table".

use rusqlite::Connection;
use steve_store::db;

fn one<T: rusqlite::types::FromSql>(c: &Connection, sql: &str) -> T {
    c.query_row(sql, [], |r| r.get(0)).unwrap()
}

/// The file each migration comes from is the one at its position: renumbering on a rebase must move the
/// file, the `db.rs` include and the LOCK line together.
#[test]
fn the_files_are_at_their_positions() {
    assert_eq!(db::MIGRATIONS[103], include_str!("../migrations/104.sql"));
    assert_eq!(db::MIGRATIONS[104], include_str!("../migrations/105.sql"));
    assert!(db::MIGRATIONS[103].contains("CREATE TABLE item_delivery_facts"));
    assert!(db::MIGRATIONS[104].contains("CREATE TABLE item_would_do"));
}

#[test]
fn a_103_database_upgrades_to_the_facts_and_the_sweep_log() {
    let c = Connection::open_in_memory().unwrap();
    for m in &db::MIGRATIONS[..103] {
        c.execute_batch(m).unwrap();
    }
    c.execute_batch(
        "CREATE TABLE schema_version(v INTEGER NOT NULL); INSERT INTO schema_version VALUES(103);
         CREATE TABLE schema_compat(min_reader INTEGER NOT NULL); INSERT INTO schema_compat VALUES(103);",
    )
    .unwrap();
    let has = |c: &Connection, name: &str| -> bool {
        one::<i64>(
            c,
            &format!("SELECT count(*) FROM sqlite_master WHERE name = '{name}'"),
        ) == 1
    };
    for table in [
        "item_delivery_facts",
        "item_waits",
        "item_judgements",
        "work_policy",
        "item_would_do",
    ] {
        assert!(!has(&c, table), "{table} before 104");
    }
    db::migrate(&c).unwrap();
    for name in [
        "item_delivery_facts",
        "item_waits",
        "item_judgements",
        "work_policy",
        "runtime_turns_session",
        "item_would_do",
        "item_would_do_item",
    ] {
        assert!(has(&c, name), "{name} after the upgrade");
    }
    assert_eq!(
        one::<i64>(&c, "SELECT v FROM schema_version"),
        db::SCHEMA_VERSION
    );
    assert_eq!(
        one::<i64>(&c, "SELECT min_reader FROM schema_compat"),
        db::SCHEMA_VERSION
    );
    // the sweep's read of an item's log works on the upgraded database
    assert_eq!(
        one::<i64>(
            &c,
            "SELECT count(*) FROM item_would_do WHERE item_id = 'itm_x'"
        ),
        0
    );
}
