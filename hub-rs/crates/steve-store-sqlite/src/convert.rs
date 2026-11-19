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

/// Preserve validation errors across the frozen runner's transaction boundary.
#[derive(Debug, thiserror::Error)]
enum MigrationError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Validation(#[from] Error),
}

/// Newly revoked counts exclude credentials already carrying a revocation timestamp.
/// Auth sessions have no revocation field: non-owner sessions are deleted and counted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    AlreadyCurrent,
    Converted {
        from_legacy_version: i64,
        revoked_tokens: usize,
        revoked_clients: usize,
        revoked_sessions: usize,
    },
}

/// Blocking API for the sole storage owner, before engines/pools start. Requires
/// autocommit and the caller's exclusive storage lock. The two committed phases are
/// independently retryable; validation writes nothing and never replaces user data.
pub fn convert(conn: &mut Connection) -> Result<Outcome, Error> {
    convert_on(conn, None)
}
/// Observed layout conversion for the supervisor. A complete event means this
/// conversion call succeeded, not that subsequent domain upgrades or health checks
/// finished; use migrate::migrate_observed for the following domain stage.
pub fn convert_observed(
    conn: &mut Connection,
    observer: migrate::Observer,
) -> Result<Outcome, Error> {
    let progress = migrate::ProgressTracker::new(conn, observer, migrate::Phase::LegacyReplay)?;
    let outcome = convert_on(conn, Some(&progress))?;
    progress.report(
        if matches!(outcome, Outcome::AlreadyCurrent) {
            migrate::Phase::Validation
        } else {
            migrate::Phase::InstallContract
        },
        None,
        true,
    );
    Ok(outcome)
}
/// Supervisor entry point for one cumulative conversion + domain-upgrade progress
/// stream. complete is emitted exactly once, only after both operations succeed.
/// AlreadyCurrent skips conversion but still applies known pending domain upgrades.
/// The caller still owns storage.lock; this API does not open files or select WAL.
pub fn convert_and_migrate_observed(
    conn: &mut Connection,
    observer: migrate::Observer,
) -> Result<Outcome, Error> {
    let progress = migrate::ProgressTracker::new(conn, observer, migrate::Phase::LegacyReplay)?;
    let outcome = convert_on(conn, Some(&progress))?;
    migrate::migrate_on(conn, Some(&progress))?;
    progress.finish();
    Ok(outcome)
}
fn convert_on(
    conn: &Connection,
    progress: Option<&migrate::ProgressTracker<'_>>,
) -> Result<Outcome, Error> {
    require_autocommit(conn)?;
    migrate::verify_locks()?;
    if is_current(conn)? {
        return Ok(Outcome::AlreadyCurrent);
    }
    let from_legacy_version = migrate_legacy_with(conn, progress)?;
    if let Some(p) = progress {
        p.report(migrate::Phase::Validation, None, false);
    }
    verify_legacy(conn)?;
    if let Some(p) = progress {
        p.report(migrate::Phase::InstallContract, None, false);
    }
    let (revoked_tokens, revoked_clients, revoked_sessions) = install_layout(conn)?;
    Ok(Outcome::Converted {
        from_legacy_version,
        revoked_tokens,
        revoked_clients,
        revoked_sessions,
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
        migrate::check_versions(conn, false)?;
        return Ok(true);
    }
    check_legacy_layout(conn)?;
    Ok(false)
}

fn check_legacy_layout(conn: &Connection) -> Result<(), Error> {
    for table in schema::CONTRACT_TABLES.into_iter().filter(|t| *t != "meta") {
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
    Ok(())
}

/// Phase 1, also exposed for callers testing a restart between the two transactions.
/// The returned version is the input to this invocation (a resumed call sees the head).
/// A head database needs only read-only verification, so rejected head schemas remain
/// byte-for-byte unchanged instead of having their old version rows rewritten first.
/// Older inputs validate structure and quick_check before the old runner commits;
/// the separate read-only phase repeats both checks on the committed head.
pub fn migrate_legacy(conn: &Connection) -> Result<i64, Error> {
    migrate_legacy_with(conn, None)
}
fn migrate_legacy_with(
    conn: &Connection,
    progress: Option<&migrate::ProgressTracker<'_>>,
) -> Result<i64, Error> {
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
        if let Some(p) = progress {
            p.report(migrate::Phase::LegacyReplay, None, false);
        }
        legacy::migrate_to_head_with(conn, |tx| {
            // Precommit validation belongs to replay; Validation starts only
            // after this legacy transaction commits, keeping stages monotonic.
            verify_legacy(tx).map_err(MigrationError::Validation)?;
            Ok::<_, MigrationError>(())
        })
        .map_err(|e| match e {
            MigrationError::Sqlite(e) => incompatible(&format!("legacy migration failed: {e}")),
            MigrationError::Validation(e) => e,
        })?;
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

fn install_layout(conn: &Connection) -> Result<(usize, usize, usize), Error> {
    // convert_on requires autocommit and the caller exclusively owns storage.lock.
    // The shared borrow also lets its progress tracker outlive this transaction;
    // SQLite rejects nesting and RAII rolls back any uncommitted installation.
    let tx = rusqlite::Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
        .map_err(db_error)?;
    let has_owner: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM users WHERE role='owner')",
            [],
            |r| r.get(0),
        )
        .map_err(db_error)?;
    if !has_owner {
        return Err(incompatible(
            "legacy storage has no instance owner; conversion refused",
        ));
    }
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
    let mut revoked = [0; 2];
    for (count, table) in revoked.iter_mut().zip(["access_tokens", "clients"]) {
        *count = tx.execute(
            &format!("UPDATE {table} SET revoked=?1 WHERE revoked IS NULL AND NOT EXISTS (SELECT 1 FROM users WHERE users.id={table}.user_id AND users.role='owner')"),
            [now],
        ).map_err(db_error)?;
    }
    // Match AuthSessions::drop: deleting the hash immediately invalidates the session.
    let revoked_sessions = tx.execute(
        "DELETE FROM auth_sessions WHERE NOT EXISTS (SELECT 1 FROM users WHERE users.id=auth_sessions.user_id AND users.role='owner')",
        [],
    ).map_err(db_error)?;
    tx.commit().map_err(db_error)?;
    Ok((revoked[0], revoked[1], revoked_sessions))
}
