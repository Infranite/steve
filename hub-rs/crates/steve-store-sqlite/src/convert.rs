//! In-place layout-1 to layout-2 conversion. The caller owns the storage lock;
//! this module opens no files, selects no journal mode and starts no workers.
use crate::{
    compat::{LEGACY_INPUT, StorageCapabilities, StorageState},
    layout::{CORE_FILE, Layout},
    legacy, migrate,
    open::{db_error, incompatible},
    schema,
};
use rusqlite::Connection;
use steve_types::{Code, Error};

/// Preserve validation errors across the frozen runner's transaction boundary.
#[derive(Debug, thiserror::Error)]
enum MigrationError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Validation(#[from] Error),
}

/// Deleted counts include previously revoked/expired non-owner credentials.
/// Independent clients referencing deleted tokens survive with token_id cleared.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Outcome {
    AlreadyCurrent,
    Converted {
        from_legacy_version: i64,
        deleted_tokens: usize,
        deleted_clients: usize,
        deleted_sessions: usize,
    },
}

/// Blocking API for the sole storage owner, before engines/pools start. Requires
/// autocommit and the caller's exclusive storage lock. Replay, validation, contract
/// installation and credential cleanup commit together through the sole legacy
/// runner. A crash cannot strand legacy 114 when only 113 is an accepted input.
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
    let from_legacy_version = legacy_input(conn)?;
    let mut deleted = None;
    // Reuse the frozen runner's transaction and validation callback, never a
    // second runner. Its final version/fence stamps are in the same commit too.
    legacy::migrate_to_head_with(conn, |tx| {
        if let Some(p) = progress {
            p.report(migrate::Phase::Validation, None, false);
        }
        verify_legacy(tx).map_err(MigrationError::Validation)?;
        if let Some(p) = progress {
            p.report(migrate::Phase::InstallContract, None, false);
        }
        deleted = Some(install_layout_on(tx).map_err(MigrationError::Validation)?);
        Ok::<_, MigrationError>(())
    })
    .map_err(migration_error)?;
    let (deleted_tokens, deleted_clients, deleted_sessions) =
        deleted.expect("legacy runner invokes precommit validation");
    Ok(Outcome::Converted {
        from_legacy_version,
        deleted_tokens,
        deleted_clients,
        deleted_sessions,
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

fn is_current(conn: &Connection) -> Result<bool, Error> {
    let state = crate::open::inspect(conn)?;
    StorageCapabilities::compiled().can_open(&state, false)?;
    Ok(matches!(state, StorageState::Current { .. }))
}

/// Standalone legacy replay/verification for diagnostic callers. Conversion
/// itself never commits this intermediate phase: a head beyond 113 is not a new
/// conversion input. Runtime callers must use convert or the observed combination.
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
    let version = legacy_input(conn)?;
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
        .map_err(migration_error)?;
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

fn install_layout_on(tx: &Connection) -> Result<(usize, usize, usize), Error> {
    // The sole legacy runner owns the enclosing BEGIN IMMEDIATE and rollback.
    // Its precommit hook installs everything before the runner's final stamps.
    require_owner(tx)?;
    tx.execute_batch(schema::CONTRACT).map_err(db_error)?;
    // Retain schema_version until the real previous-release probe determines
    // whether removal is safe. The fence is mandatory in both shapes.
    Layout::single_file().write_file_meta(tx, CORE_FILE)?;
    migrate::stamp_current(tx)?;
    // FK inventory of the frozen baseline: only clients.token_id references
    // these three credential tables. Clients are independent entities, including
    // owner clients referencing a non-owner token: preserve them and clear the FK.
    tx.execute("UPDATE clients SET token_id=NULL WHERE token_id IN (SELECT id FROM access_tokens WHERE NOT EXISTS (SELECT 1 FROM users WHERE users.id=access_tokens.user_id AND users.role='owner'))", []).map_err(db_error)?;
    let deleted_clients = tx.execute("DELETE FROM clients WHERE NOT EXISTS (SELECT 1 FROM users WHERE users.id=clients.user_id AND users.role='owner')", []).map_err(db_error)?;
    let deleted_tokens = tx.execute("DELETE FROM access_tokens WHERE NOT EXISTS (SELECT 1 FROM users WHERE users.id=access_tokens.user_id AND users.role='owner')", []).map_err(db_error)?;
    let deleted_sessions = tx.execute("DELETE FROM auth_sessions WHERE NOT EXISTS (SELECT 1 FROM users WHERE users.id=auth_sessions.user_id AND users.role='owner')", []).map_err(db_error)?;
    Ok((deleted_tokens, deleted_clients, deleted_sessions))
}

fn require_owner(conn: &Connection) -> Result<(), Error> {
    let has_owner: bool = conn
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
    Ok(())
}

fn migration_error(error: MigrationError) -> Error {
    match error {
        MigrationError::Sqlite(e) => incompatible(&format!("legacy migration failed: {e}")),
        MigrationError::Validation(e) => e,
    }
}
fn legacy_input(conn: &Connection) -> Result<i64, Error> {
    let version = legacy::version(conn)
        .map_err(|e| incompatible(&format!("legacy version rejected: {e}")))?;
    if version != LEGACY_INPUT {
        return Err(incompatible(
            "legacy conversion accepts only schema 113; first upgrade to the previous published release (先升级到上一个发布版)",
        ));
    }
    require_owner(conn)?;
    Ok(version)
}
