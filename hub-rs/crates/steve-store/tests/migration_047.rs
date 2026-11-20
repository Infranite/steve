//! Current channel settings and connection lifetime constraints.

use rusqlite::Connection;
use steve_store::db;

#[test]
fn send_delay_defaults_to_null_and_rejects_out_of_range_values() {
    let c = Connection::open_in_memory().unwrap();
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
}
