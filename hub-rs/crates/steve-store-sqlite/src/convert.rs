//! In-place layout-1 to layout-2 conversion. The caller owns the storage lock;
//! this module opens no files, selects no journal mode and starts no workers.
use crate::{
    layout::{CORE_FILE, Layout},
    legacy, migrate,
    open::{db_error, incompatible},
    schema,
};
use rusqlite::{Connection, OptionalExtension, TransactionBehavior};
use steve_types::{Code, Error};

/// Newly revoked counts exclude credentials already carrying a revocation timestamp.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    AlreadyCurrent,
    Converted {
        from_legacy_version: i64,
        revoked_tokens: usize,
        revoked_clients: usize,
    },
}

/// Blocking API for the sole storage owner, before engines/pools start. Requires
/// autocommit and the caller's exclusive storage lock. The two committed phases are
/// independently retryable; validation writes nothing and never replaces user data.
pub fn convert(conn: &mut Connection) -> Result<Outcome, Error> {
    require_autocommit(conn)?;
    migrate::verify_locks()?;
    if is_current(conn)? {
        return Ok(Outcome::AlreadyCurrent);
    }
    let from_legacy_version = migrate_legacy(conn)?;
    verify_legacy(conn)?;
    let (revoked_tokens, revoked_clients) = install_layout(conn)?;
    Ok(Outcome::Converted {
        from_legacy_version,
        revoked_tokens,
        revoked_clients,
    })
}

fn require_autocommit(conn: &Connection) -> Result<(), Error> {
    if !conn.is_autocommit() {
        return Err(steve_storage_api::error(
            Code::Invalid,
            "conversion requires autocommit",
        ));
    }
    Ok(())
}

fn object_kind(conn: &Connection, name: &str) -> Result<Option<String>, Error> {
    conn.query_row(
        "SELECT type FROM sqlite_schema WHERE name=?1 COLLATE NOCASE",
        [name],
        |r| r.get(0),
    )
    .optional()
    .map_err(db_error)
}

fn is_current(conn: &Connection) -> Result<bool, Error> {
    if let Some(kind) = object_kind(conn, "meta")? {
        if kind != "table" {
            return Err(incompatible("unrecognized storage metadata"));
        }
        for table in schema::CONTRACT_TABLES {
            if object_kind(conn, table)?.as_deref() != Some("table") {
                return Err(incompatible("storage contract metadata is incomplete"));
            }
        }
        for table in schema::LEGACY_TABLES {
            if object_kind(conn, table)?.is_some() {
                return Err(incompatible("mixed legacy and current storage metadata"));
            }
        }
        Layout::read(conn)?;
        migrate::check_versions(conn, true)?;
        return Ok(true);
    }
    for table in schema::CONTRACT_TABLES.into_iter().skip(1) {
        if object_kind(conn, table)?.is_some() {
            return Err(incompatible("storage contract metadata is incomplete"));
        }
    }
    match object_kind(conn, "schema_version")?.as_deref() {
        Some("table") => {}
        None => {
            let objects: u32 = conn
                .query_row(
                    "SELECT count(*) FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%'",
                    [],
                    |r| r.get(0),
                )
                .map_err(db_error)?;
            if objects != 0 {
                return Err(incompatible(
                    "unversioned nonempty storage cannot be converted",
                ));
            }
        }
        _ => return Err(incompatible("invalid legacy version metadata")),
    }
    Ok(false)
}

/// Phase 1, also exposed for callers testing a restart between the two transactions.
/// The returned version is the input to this invocation (a resumed call sees the head).
/// A head database needs only read-only verification, so rejected head schemas remain
/// byte-for-byte unchanged instead of having their old version rows rewritten first.
/// Older inputs also validate structure before the old runner commits; the separate
/// read-only phase still verifies the committed head and runs quick_check.
pub fn migrate_legacy(conn: &Connection) -> Result<i64, Error> {
    require_autocommit(conn)?;
    if is_current(conn)? {
        return Err(steve_storage_api::error(
            Code::Invalid,
            "storage is already converted",
        ));
    }
    let version = legacy::version(conn)
        .map_err(|e| incompatible(&format!("legacy version rejected: {e}")))?;
    if version != legacy::SCHEMA_VERSION {
        legacy::migrate_to_head_with(conn, |tx| {
            schema::verify_baseline(tx)
                .map_err(|e| rusqlite::Error::InvalidParameterName(e.to_string()))
        })
        .map_err(|e| incompatible(&format!("legacy migration failed: {e}")))?;
    }
    Ok(version)
}

fn verify_legacy(conn: &Connection) -> Result<(), Error> {
    schema::verify_baseline(conn)?;
    let mut stmt = conn.prepare("PRAGMA quick_check").map_err(db_error)?;
    let checks = stmt
        .query_map([], |r| r.get::<_, String>(0))
        .map_err(db_error)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(db_error)?;
    if checks != ["ok"] {
        return Err(incompatible("legacy storage quick_check failed"));
    }
    Ok(())
}

fn install_layout(conn: &mut Connection) -> Result<(usize, usize), Error> {
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(db_error)?;
    tx.execute_batch(schema::CONTRACT).map_err(db_error)?;
    tx.execute_batch("DROP TABLE schema_version; DROP TABLE IF EXISTS schema_compat;")
        .map_err(db_error)?;
    Layout::single_file().write_file_meta(&tx, CORE_FILE)?;
    migrate::stamp_current(&tx)?;
    let now: i64 = tx
        .query_row(
            "SELECT CAST(unixepoch('subsec') * 1000 AS INTEGER)",
            [],
            |r| r.get(0),
        )
        .map_err(db_error)?;
    // Instance ownership is users.role, not workspace membership or token ancestry.
    let revoked_tokens = tx.execute("UPDATE access_tokens SET revoked=?1 WHERE revoked IS NULL AND NOT EXISTS (SELECT 1 FROM users WHERE users.id=access_tokens.user_id AND users.role='owner')", [now]).map_err(db_error)?;
    let revoked_clients = tx.execute("UPDATE clients SET revoked=?1 WHERE revoked IS NULL AND NOT EXISTS (SELECT 1 FROM users WHERE users.id=clients.user_id AND users.role='owner')", [now]).map_err(db_error)?;
    tx.commit().map_err(db_error)?;
    Ok((revoked_tokens, revoked_clients))
}
