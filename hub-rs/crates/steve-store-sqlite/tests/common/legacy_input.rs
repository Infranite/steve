//! Synthetic supported legacy inputs; no historical migration replay in fixtures.
use rusqlite::Connection;
pub fn at(conn: &Connection, version: i64) {
    conn.execute_batch(include_str!("../../legacy/113_fixture.sql"))
        .unwrap();
    if version >= 114 {
        conn.execute_batch(include_str!("../../legacy/migrations/114.sql"))
            .unwrap();
    }
    conn.execute("UPDATE schema_version SET v=?1", [version])
        .unwrap();
    conn.execute("UPDATE schema_compat SET min_reader=?1", [version])
        .unwrap();
}
