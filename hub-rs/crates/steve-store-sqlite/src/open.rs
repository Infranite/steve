//! Physical opens. dir is the data directory (normally <home>/data), never a global default.
use crate::{
    engine::Engine,
    layout::{CORE_FILE, Layout},
    migrate, pragmas,
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
fn has_table(conn: &Connection, name: &str) -> Result<bool, Error> {
    conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name=?1)",
        [name],
        |r| r.get(0),
    )
    .map_err(db_error)
}
fn inspect(conn: &Connection) -> Result<Option<Layout>, Error> {
    if has_table(conn, "meta")? {
        return Layout::read(conn).map(Some);
    }
    if has_table(conn, "schema_version")? {
        return Err(incompatible(
            "legacy storage requires conversion before opening (needs conversion)",
        ));
    }
    let count: u32 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%'",
            [],
            |r| r.get(0),
        )
        .map_err(db_error)?;
    if count != 0 {
        return Err(incompatible(
            "unrecognized storage schema; conversion is required",
        ));
    }
    Ok(None)
}
fn open(dir: &Path, runtime: bool) -> Result<Engine, Error> {
    // Verify every embedded directory before creating a database or selecting WAL mode.
    migrate::verify_locks()?;
    let lock = acquire_lock(dir, runtime)?;
    let dir = dir.canonicalize().map_err(|_| io_error())?;
    let path = dir.join(CORE_FILE);
    let layout = if path.exists() {
        let probe = open_readonly(&path)?;
        let layout = inspect(&probe)?;
        if let Some(layout) = &layout {
            migrate::check_versions(&probe, !runtime)?;
            layout.verify_files(&dir)?;
        }
        layout
    } else {
        None
    };
    if !runtime && layout.is_none() {
        return Err(incompatible(
            "storage is not initialized; start the Hub first",
        ));
    }
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
        | OpenFlags::SQLITE_OPEN_URI
        | OpenFlags::SQLITE_OPEN_NO_MUTEX
        | if runtime {
            OpenFlags::SQLITE_OPEN_CREATE
        } else {
            OpenFlags::empty()
        };
    let mut writer = Connection::open_with_flags(&path, flags).map_err(db_error)?;
    if runtime {
        pragmas::runtime_writer(&writer).map_err(db_error)?;
    } else {
        pragmas::connection(&writer, true).map_err(db_error)?;
    }
    let layout = match layout {
        Some(layout) => {
            if runtime {
                migrate::migrate(&mut writer)?;
            }
            layout
        }
        None => {
            let layout = Layout::single_file();
            migrate::initialize(&mut writer, &layout)?;
            layout
        }
    };
    Engine::from_opened(dir, layout, CORE_FILE.into(), writer, lock, runtime, None)
}
/// Hub open: lifetime-held process lock, read-only compatibility preflight, then WAL and
/// pending migrations. Legacy conversion is intentionally delegated to the converter.
pub fn open_runtime(dir: impl AsRef<Path>) -> Result<Engine, Error> {
    open(dir.as_ref(), true)
}
/// Offline administration: lifetime-held lock and exact schema checks, no migration,
/// metadata stamping, optimize, diagnostic task, or persistent pragma changes.
pub fn open_admin(dir: impl AsRef<Path>) -> Result<Engine, Error> {
    open(dir.as_ref(), false)
}
