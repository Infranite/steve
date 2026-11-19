//! Connection-local tuning is separate from persistent WAL selection so compatibility
//! checks and administrative opens cannot accidentally change the database header.
use crate::layout::{CORE_FILE, Layout};
use rusqlite::{Connection, OpenFlags};
use std::path::Path;

pub const STATEMENT_CACHE: usize = 512;
pub const MMAP_SIZE: i64 = 256 * 1024 * 1024;
pub const JOURNAL_SIZE_LIMIT: i64 = 64 * 1024 * 1024;
/// A reader must never gain write access through its main database or attachments.
pub fn readonly_flags() -> OpenFlags {
    OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI | OpenFlags::SQLITE_OPEN_NO_MUTEX
}
/// Legacy tuning, applied to every pooled or writer connection. No persistent pragmas.
pub fn connection(conn: &Connection, writer: bool) -> rusqlite::Result<()> {
    conn.pragma_update(None, "foreign_keys", "ON")?;
    conn.pragma_update(None, "busy_timeout", 5_000)?;
    conn.pragma_update(None, "synchronous", "NORMAL")?;
    conn.pragma_update(None, "temp_store", "MEMORY")?;
    conn.pragma_update(None, "mmap_size", MMAP_SIZE)?;
    conn.pragma_update(None, "cache_size", if writer { -32_768 } else { -16_384 })?;
    conn.pragma_update(None, "wal_autocheckpoint", 0)?;
    conn.set_prepared_statement_cache_capacity(STATEMENT_CACHE);
    Ok(())
}
/// Only runtime opens call this, after physical compatibility and catalog validation.
pub fn runtime_writer(conn: &Connection) -> rusqlite::Result<()> {
    let mode: String = conn.pragma_update_and_check(None, "journal_mode", "WAL", |r| r.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        return Err(rusqlite::Error::InvalidQuery);
    }
    connection(conn, true)?;
    conn.pragma_update(None, "journal_size_limit", JOURNAL_SIZE_LIMIT)
}
/// Percent-encode URI metacharacters, UTF-8 and non-UTF-8 path bytes, retaining path separators.
fn readonly_uri(path: &Path) -> String {
    let mut uri = String::from("file:");
    for b in path.as_os_str().as_encoded_bytes() {
        if b.is_ascii_alphanumeric() || b"/-_.~:".contains(b) {
            uri.push(char::from(*b));
        } else {
            uri.push_str(&format!("%{b:02X}"));
        }
    }
    uri.push_str("?mode=ro");
    uri
}
/// Attach every peer as db_0, db_1, ... in sorted filename order. All SQL parameters are
/// bound; the only interpolated identifiers are generated numeric aliases. SQLite's
/// unqualified-name lookup retains main-first semantics for legacy read-only joins.
pub fn attach_readonly(
    conn: &Connection,
    dir: &Path,
    layout: &Layout,
    own_file: &str,
) -> rusqlite::Result<()> {
    for (i, file) in layout
        .files
        .keys()
        .filter(|f| f.as_str() != own_file)
        .enumerate()
    {
        conn.execute(
            &format!("ATTACH DATABASE ?1 AS db_{i}"),
            [readonly_uri(&dir.join(file))],
        )?;
    }
    Ok(())
}
/// Shared main filename for pool constructors.
pub const READER_MAIN: &str = CORE_FILE;
