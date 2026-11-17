//! 105: what the sweep would do (#334), after 104 (#327). The file, the `db.rs` include and the LOCK
//! line are at their position, and the log's key holds the scope revision and the target: the same
//! reason on the same turn about another operation, card or wait, or on another goal, is another row.

use rusqlite::Connection;
use steve_store::db;

fn one<T: rusqlite::types::FromSql>(c: &Connection, sql: &str) -> T {
    c.query_row(sql, [], |r| r.get(0)).unwrap()
}

#[test]
fn the_file_is_at_its_position() {
    assert_eq!(db::MIGRATIONS[104], include_str!("../migrations/105.sql"));
    assert!(db::MIGRATIONS[104].contains("CREATE TABLE item_would_do"));
    assert!(db::MIGRATIONS[103].contains("CREATE TABLE item_delivery_facts"));
}

#[test]
fn applies_on_a_104_database_and_keys_by_scope_and_target() {
    let c = Connection::open_in_memory().unwrap();
    for m in &db::MIGRATIONS[..105] {
        c.execute_batch(m).unwrap();
    }
    let n: i64 = one(&c, "SELECT COUNT(*) FROM item_would_do");
    assert_eq!(n, 0, "empty at migration");
    let insert = |id: &str, scope: i64, target: &str| {
        c.execute(
            "INSERT INTO item_would_do(id,item_id,cycle,session_id,turn_id,scope_revision,target,action,reason,row,code,due_at,first_seen)
             VALUES (?1,'i1',1,'s1','t1',?2,?3,'card','required_failed',9,'closing',1,1)",
            rusqlite::params![id, scope, target],
        )
    };
    c.execute_batch("PRAGMA foreign_keys=OFF").unwrap();
    insert("a", 1, "op-build").unwrap();
    insert("b", 1, "op-build-2").expect("another target is another row");
    insert("c", 2, "op-build").expect("another scope is another row");
    assert!(
        insert("d", 1, "op-build").is_err(),
        "the same key is one row"
    );
    // defaults for a decision about the item as a whole
    c.execute(
        "INSERT INTO item_would_do(id,item_id,cycle,action,reason,row,code,due_at,first_seen)
         VALUES ('e','i1',1,'wake','follow_up',7,'follow_up',1,1)",
        [],
    )
    .unwrap();
    let (scope, target): (i64, String) = c
        .query_row(
            "SELECT scope_revision,target FROM item_would_do WHERE id='e'",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .unwrap();
    assert_eq!((scope, target.as_str()), (0, ""));
}
