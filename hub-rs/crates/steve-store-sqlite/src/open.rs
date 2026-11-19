//! Physical opens. dir is the data directory (normally <home>/data), never a global default.
use crate::{
    compat::{StorageCapabilities, StorageState},
    engine::Engine,
    layout::{CORE_FILE, Layout},
    migrate, pragmas, schema,
};
use rusqlite::{Connection, OpenFlags};
use std::{fs::File, path::Path, sync::Arc};
use steve_types::{Code, Error};

pub(crate) fn incompatible(detail: &str) -> Error {
    steve_storage_api::error(Code::Incompatible, detail)
}
pub(crate) fn db_error(error: rusqlite::Error) -> Error {
    let code = match error.sqlite_error_code() {
        Some(rusqlite::ErrorCode::DatabaseBusy | rusqlite::ErrorCode::DatabaseLocked) => Code::Busy,
        Some(rusqlite::ErrorCode::ReadOnly) => Code::Forbidden,
        _ => Code::Internal,
    };
    steve_storage_api::error(code, "SQLite storage operation failed")
}
fn io_error() -> Error {
    steve_storage_api::error(
        Code::Unavailable,
        "storage directory or file is inaccessible",
    )
}
pub(crate) fn acquire_lock(dir: &Path, create: bool) -> Result<Arc<File>, Error> {
    if create {
        std::fs::create_dir_all(dir).map_err(|_| io_error())?;
    }
    let file = File::options()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(dir.join("storage.lock"))
        .map_err(|_| io_error())?;
    file.try_lock().map_err(|e| match e {
        std::fs::TryLockError::WouldBlock => {
            steve_storage_api::error(Code::Busy, "storage is held by another process")
        }
        std::fs::TryLockError::Error(_) => io_error(),
    })?;
    Ok(Arc::new(file))
}
/// A diagnostic/backup-source connection: no lock, migration, metadata, or pragma writes.
/// Older schemas are deliberately allowed. It never creates a missing database.
pub fn open_readonly(path: impl AsRef<Path>) -> Result<Connection, Error> {
    Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(db_error)
}
fn object_kind(conn: &Connection, name: &str) -> Result<Option<String>, Error> {
    use rusqlite::OptionalExtension;
    conn.query_row(
        "SELECT type FROM sqlite_schema WHERE name=?1 COLLATE NOCASE",
        [name],
        |r| r.get(0),
    )
    .optional()
    .map_err(db_error)
}
/// The sole read-only metadata discriminator. No locks, pragmas, or migrations.
/// Unknown/partial/mixed metadata fails closed; readonly connections remain usable
/// for raw diagnosis even when this structured inspection refuses the schema.
pub fn inspect(conn: &Connection) -> Result<StorageState, Error> {
    if object_kind(conn, "meta")?.is_some() {
        for table in schema::CONTRACT_TABLES {
            if object_kind(conn, table)?.as_deref() != Some("table") {
                return Err(incompatible("storage contract metadata is incomplete"));
            }
        }
        // The sole allowed old metadata is an explicit old-reader fence, plus
        // the runner head row retained pending previous-release verification.
        if object_kind(conn, "schema_compat")?.as_deref() != Some("table") {
            return Err(incompatible(
                "current storage is missing its legacy reader fence",
            ));
        }
        let (rows, minimum): (u32, i64) = conn
            .query_row(
                "SELECT count(*), coalesce(min(min_reader),0) FROM schema_compat",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .map_err(db_error)?;
        if rows != 1 || minimum <= crate::compat::LEGACY_INPUT {
            return Err(incompatible("mixed legacy and current storage metadata"));
        }
        if object_kind(conn, "schema_version")?.is_some() {
            let (rows, version): (u32, i64) = conn
                .query_row(
                    "SELECT count(*), coalesce(min(v),0) FROM schema_version",
                    [],
                    |r| Ok((r.get(0)?, r.get(1)?)),
                )
                .map_err(db_error)?;
            if rows != 1
                || !(crate::compat::LEGACY_INPUT..=crate::legacy::SCHEMA_VERSION).contains(&version)
            {
                return Err(incompatible("mixed legacy and current storage metadata"));
            }
        }
        let layout = Layout::read_state(conn)?;
        let domains = conn
            .prepare("SELECT domain,version FROM domain_schema")
            .map_err(db_error)?
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .map_err(db_error)?
            .collect::<rusqlite::Result<std::collections::BTreeMap<String, u32>>>()
            .map_err(db_error)?;
        if domains.len() != 3
            || crate::layout::DOMAINS
                .iter()
                .any(|d| !domains.contains_key(crate::layout::domain_name(*d)))
        {
            return Err(incompatible("incomplete or unknown domain schema"));
        }
        let contract_schema = crate::layout::meta(
            conn,
            crate::layout::META_DOMAIN,
            migrate::CONTRACT_SCHEMA_KEY,
        )?
        .parse()
        .map_err(|_| incompatible("invalid contract schema version"))?;
        return Ok(StorageState::Current {
            layout,
            domains,
            contract_schema,
        });
    }
    for table in schema::CONTRACT_TABLES.into_iter().filter(|t| *t != "meta") {
        if object_kind(conn, table)?.is_some() {
            return Err(incompatible("storage contract metadata is incomplete"));
        }
    }
    if object_kind(conn, "schema_version")?.as_deref() == Some("table") {
        let schema_version = crate::legacy::version(conn).map_err(|_| incompatible("legacy version rejected; first upgrade to the previous published release (先升级到上一个发布版)"))?;
        return Ok(StorageState::Legacy { schema_version });
    }
    let count: u32 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%'",
            [],
            |r| r.get(0),
        )
        .map_err(db_error)?;
    if count != 0 {
        return Err(incompatible("unrecognized nonempty storage schema"));
    }
    Ok(StorageState::Empty)
}
fn preflight(dir: &Path, state: &StorageState, exact: bool) -> Result<(), Error> {
    StorageCapabilities::compiled().can_open(state, exact)?;
    if let StorageState::Current { layout, .. } = state {
        layout.verify_files(dir)?;
    }
    Ok(())
}
fn open(dir: &Path, runtime: bool) -> Result<Engine, Error> {
    // Verify every embedded directory before creating a database or selecting WAL mode.
    migrate::verify_locks()?;
    if !runtime && !dir.exists() {
        return Err(incompatible(
            "storage is not initialized; start or upgrade the Hub first",
        ));
    }
    let lock = acquire_lock(dir, runtime)?;
    let dir = dir.canonicalize().map_err(|_| io_error())?;
    let path = dir.join(CORE_FILE);
    let state = if path.exists() {
        inspect(&open_readonly(&path)?)?
    } else {
        StorageState::Empty
    };
    preflight(&dir, &state, !runtime)?;
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
        | OpenFlags::SQLITE_OPEN_URI
        | OpenFlags::SQLITE_OPEN_NO_MUTEX
        | if runtime && matches!(&state, StorageState::Empty) {
            OpenFlags::SQLITE_OPEN_CREATE
        } else {
            OpenFlags::empty()
        };
    let mut writer = Connection::open_with_flags(&path, flags).map_err(db_error)?;
    // Suppress last-close checkpoint from the instant an admin writer exists,
    // including any failing assembly path. No persistent pragma has run yet.
    if !runtime {
        pragmas::no_checkpoint_on_close(&writer).map_err(db_error)?;
    }
    let layout = match state {
        StorageState::Empty => {
            let layout = Layout::single_file();
            migrate::initialize(&mut writer, &layout)?;
            layout
        }
        StorageState::Legacy { .. } => {
            // Same storage.lock, before WAL or connection tuning. Conversion
            // never acquires the lock again or opens another writer.
            crate::convert::convert(&mut writer)?;
            let state = inspect(&writer)?;
            preflight(&dir, &state, false)?;
            match state {
                StorageState::Current { layout, .. } => layout,
                _ => unreachable!(),
            }
        }
        StorageState::Current { layout, .. } => layout,
    };
    if runtime {
        pragmas::runtime_writer(&writer).map_err(db_error)?;
        migrate::migrate(&mut writer)?;
    } else {
        pragmas::connection(&writer, true).map_err(db_error)?;
    }
    Engine::from_opened(dir, layout, CORE_FILE.into(), writer, lock, runtime, None)
}
/// Hub open: lifetime-held process lock, read-only compatibility preflight, then WAL and
/// pending migrations. Legacy conversion shares the same lifetime-held lock.
/// Callers acquire instance.lock before this storage lock, never in reverse order.
pub fn open_runtime(dir: impl AsRef<Path>) -> Result<Engine, Error> {
    open(dir.as_ref(), true)
}
/// Offline administration: lifetime-held lock and exact schema checks, no migration,
/// metadata stamping, optimize, diagnostic task, or persistent pragma changes.
pub fn open_admin(dir: impl AsRef<Path>) -> Result<Engine, Error> {
    open(dir.as_ref(), false)
}
