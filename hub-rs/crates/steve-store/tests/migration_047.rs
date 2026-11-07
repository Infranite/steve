//! 047: `channel_settings.send_delay_s`, a channel account's undo window. A row that does not set it
//! keeps NULL (the default for its identity), values outside 0–60 are refused.

use rusqlite::Connection;
use steve_store::db;

#[test]
fn applies_on_a_046_database_and_defaults_to_null() {
    let c = Connection::open_in_memory().unwrap();
    c.execute_batch(
        "CREATE TABLE schema_version(v INTEGER NOT NULL); INSERT INTO schema_version VALUES(46);",
    )
    .unwrap();
    for m in &db::MIGRATIONS[..46] {
        c.execute_batch(m).unwrap();
    }
    db::migrate(&c).unwrap();
    db::migrate(&c).unwrap();
    // a row as the code before 047 writes it (its connection left out of this test)
    c.execute_batch(
        "PRAGMA foreign_keys=OFF; INSERT INTO channel_settings(connection_id, workspace_id, mode, rules, updated) VALUES ('c1', 'default', 'bot', '[]', 1);",
    )
    .unwrap();
    let v: Option<i64> = c
        .query_row(
            "SELECT send_delay_s FROM channel_settings WHERE connection_id = 'c1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(v, None);
    c.execute(
        "UPDATE channel_settings SET send_delay_s = 0 WHERE connection_id = 'c1'",
        [],
    )
    .unwrap();
    assert!(
        c.execute(
            "UPDATE channel_settings SET send_delay_s = 61 WHERE connection_id = 'c1'",
            [],
        )
        .is_err()
    );
    assert_eq!(db::compat(&c).unwrap().schema, db::SCHEMA_VERSION);
}
