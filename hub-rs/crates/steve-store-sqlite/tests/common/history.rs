//! Public frozen SQL reproduces incremental installations without a binary database.
use rusqlite::Connection;
use std::path::Path;

/// A historical installation opened after 050 (including conditional columns),
/// then upgraded by the one frozen runner. Physical order differs from a fresh head.
pub fn after_050(conn: &Connection) {
    conn.execute_batch("CREATE TABLE schema_version(v INTEGER NOT NULL); CREATE TABLE schema_compat(min_reader INTEGER NOT NULL);").unwrap();
    let archive = Path::new(env!("CARGO_MANIFEST_DIR")).join("legacy/migrations");
    for n in 1..=50 {
        conn.execute_batch(&std::fs::read_to_string(archive.join(format!("{n:03}.sql"))).unwrap())
            .unwrap();
    }
    for table in ["bots", "sessions"] {
        conn.execute_batch(&format!(
            "ALTER TABLE {table} ADD COLUMN reasoning_effort TEXT"
        ))
        .unwrap();
    }
    conn.execute_batch(&std::fs::read_to_string(archive.join("027_items.sql")).unwrap())
        .unwrap();
    conn.execute_batch("CREATE INDEX IF NOT EXISTS items_closing ON items(phase) WHERE phase = 'closing'; INSERT INTO schema_version(v) VALUES(50); INSERT INTO schema_compat(min_reader) VALUES(50);").unwrap();
    steve_store_sqlite::legacy::migrate_to_head(conn).unwrap();
}
