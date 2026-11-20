//! Test-only historical input constructor. Never upgrades an input to the current head.
use rusqlite::Connection;

pub fn at(conn: &Connection, version: i64) {
    conn.execute_batch("CREATE TABLE schema_version(v INTEGER NOT NULL); CREATE TABLE schema_compat(min_reader INTEGER NOT NULL);").unwrap();
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("legacy/migrations");
    for n in 1..=version {
        conn.execute_batch(&std::fs::read_to_string(root.join(format!("{n:03}.sql"))).unwrap())
            .unwrap();
    }
    // The old runner supplied these conditional columns outside numbered SQL.
    if version >= 113 {
        for table in ["bots", "sessions"] {
            conn.execute_batch(&format!(
                "ALTER TABLE {table} ADD COLUMN reasoning_effort TEXT"
            ))
            .unwrap();
        }
        conn.execute_batch(&std::fs::read_to_string(root.join("027_items.sql")).unwrap())
            .unwrap();
        conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS items_closing ON items(phase) WHERE phase = 'closing'",
        )
        .unwrap();
    }
    conn.execute("INSERT INTO schema_version(v) VALUES(?1)", [version])
        .unwrap();
    conn.execute(
        "INSERT INTO schema_compat(min_reader) VALUES(?1)",
        [version],
    )
    .unwrap();
}
