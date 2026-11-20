//! The frozen synthetic installation preserves real incremental physical column order.
use rusqlite::Connection;
pub fn after_050(conn: &Connection) {
    conn.execute_batch(include_str!("../../legacy/113_fixture.sql"))
        .unwrap();
}
