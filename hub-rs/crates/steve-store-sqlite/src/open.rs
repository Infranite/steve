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
        inspect_current(conn)
    } else {
        inspect_legacy_or_empty(conn)
    }
}
fn inspect_current(conn: &Connection) -> Result<StorageState, Error> {
    for table in schema::CONTRACT_TABLES {
        if object_kind(conn, table)?.as_deref() != Some("table") {
            return Err(incompatible("storage contract metadata is incomplete"));
        }
    }
    check_reader_fence(conn)?;
    check_current_metadata(conn)?;
    let layout = Layout::read_state(conn)?;
    let domains = conn
        .prepare("SELECT domain,version FROM domain_schema")
        .map_err(db_error)?
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .map_err(db_error)?
        .collect::<rusqlite::Result<Vec<(String, u32)>>>()
        .map_err(db_error)?;
    if domains.len() != crate::layout::DOMAINS.len() {
        return Err(incompatible("duplicate or incomplete domain schema"));
    }
    let domains = domains.into_iter().collect();
    crate::compat::require_domains(&domains)?;
    let contract_schema = crate::layout::meta(
        conn,
        crate::layout::META_DOMAIN,
        migrate::CONTRACT_SCHEMA_KEY,
    )?
    .parse()
    .map_err(|_| incompatible("invalid contract schema version"))?;
    Ok(StorageState::Current {
        layout,
        domains,
        contract_schema,
    })
}
fn check_current_metadata(conn: &Connection) -> Result<(), Error> {
    let (invalid, duplicate): (bool, bool) = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM meta
                WHERE domain NOT IN (?1,?2,?3) OR typeof(domain)!='text'
                   OR typeof(key)!='text' OR typeof(value)!='text'),
                EXISTS(SELECT 1 FROM meta GROUP BY domain,key HAVING count(*)>1)",
            crate::layout::DOMAINS.map(crate::layout::domain_name),
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(db_error)?;
    if invalid || duplicate {
        return Err(incompatible("invalid or duplicate storage metadata"));
    }
    Ok(())
}
fn check_reader_fence(conn: &Connection) -> Result<(), Error> {
    if object_kind(conn, "schema_version")?.is_some() {
        return Err(incompatible("mixed legacy and current storage metadata"));
    }
    if object_kind(conn, "schema_compat")?.as_deref() != Some("table") {
        return Err(incompatible(
            "current storage is missing its legacy reader fence",
        ));
    }
    let columns = conn
        .prepare("PRAGMA table_info(schema_compat)")
        .map_err(db_error)?
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, bool>(3)?,
                r.get::<_, Option<String>>(4)?,
                r.get::<_, u32>(5)?,
            ))
        })
        .map_err(db_error)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(db_error)?;
    if columns != [("min_reader".into(), "INTEGER".into(), true, None, 0)] {
        return Err(incompatible("invalid legacy reader fence shape"));
    }
    let (rows, minimum): (u32, i64) = conn
        .query_row(
            "SELECT count(*), coalesce(min(min_reader),0) FROM schema_compat",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(db_error)?;
    if rows != 1 || minimum != crate::compat::LEGACY_READER_FENCE {
        return Err(incompatible("invalid legacy reader fence"));
    }
    Ok(())
}
fn inspect_legacy_or_empty(conn: &Connection) -> Result<StorageState, Error> {
    for table in schema::CONTRACT_TABLES.into_iter().filter(|t| *t != "meta") {
        if object_kind(conn, table)?.is_some() {
            return Err(incompatible("storage contract metadata is incomplete"));
        }
    }
    if object_kind(conn, "schema_version")?.as_deref() == Some("table") {
        check_legacy_reader(conn)?;
        // Distinguish future schema/min_reader from unsupported older inputs.
        // version validates both headers without changing persistent state.
        let schema_version = crate::legacy::version(conn).map_err(|_| incompatible("legacy storage requires a newer program or valid version metadata (需要更新的程序)"))?;
        return Ok(StorageState::Legacy { schema_version });
    }
    let count: u32 = conn
        .query_row(
            "SELECT count(*) FROM sqlite_schema WHERE lower(substr(name,1,7)) != 'sqlite_'",
            [],
            |r| r.get(0),
        )
        .map_err(db_error)?;
    if count != 0 {
        return Err(incompatible("unrecognized nonempty storage schema"));
    }
    Ok(StorageState::Empty)
}
fn check_legacy_reader(conn: &Connection) -> Result<(), Error> {
    match object_kind(conn, "schema_compat")?.as_deref() {
        None => return Ok(()),
        Some("table") => {}
        Some(_) => return Err(incompatible("invalid legacy reader object")),
    }
    let (rows, valid): (u32, u32) = conn
        .query_row(
            "SELECT count(*), count(CASE WHEN typeof(min_reader)='integer'
                AND min_reader>=0 THEN 1 END) FROM schema_compat",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(db_error)?;
    if rows != 1 || valid != 1 {
        return Err(incompatible("invalid legacy minimum reader"));
    }
    Ok(())
}
fn preflight(dir: &Path, state: &StorageState, exact: bool) -> Result<(), Error> {
    StorageCapabilities::compiled().can_open(state, exact)?;
    if let StorageState::Current { layout, .. } = state {
        layout.verify_files(dir)?;
    }
    Ok(())
}
fn open(dir: &Path, runtime: bool, observer: Option<migrate::Observer>) -> Result<Engine, Error> {
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
    let mut writer = open_writer(&path, runtime, matches!(&state, StorageState::Empty))?;
    let layout = prepare_writer(&dir, &mut writer, state, runtime, observer)?;
    Engine::from_opened(dir, layout, CORE_FILE.into(), writer, lock, runtime, None)
}
fn open_writer(path: &Path, runtime: bool, empty: bool) -> Result<Connection, Error> {
    let flags = OpenFlags::SQLITE_OPEN_READ_WRITE
        | OpenFlags::SQLITE_OPEN_URI
        | OpenFlags::SQLITE_OPEN_NO_MUTEX
        | if runtime && empty {
            OpenFlags::SQLITE_OPEN_CREATE
        } else {
            OpenFlags::empty()
        };
    let writer = Connection::open_with_flags(path, flags).map_err(db_error)?;
    if !runtime {
        pragmas::no_checkpoint_on_close(&writer).map_err(db_error)?;
    }
    Ok(writer)
}
fn prepare_writer(
    dir: &Path,
    writer: &mut Connection,
    state: StorageState,
    runtime: bool,
    observer: Option<migrate::Observer>,
) -> Result<Layout, Error> {
    if matches!(state, StorageState::Empty) {
        let layout = Layout::single_file();
        migrate::initialize(writer, &layout)?;
        pragmas::runtime_writer(writer).map_err(db_error)?;
        return Ok(layout);
    }
    let phase = if matches!(&state, StorageState::Legacy { .. }) {
        migrate::Phase::LegacyReplay
    } else {
        migrate::Phase::DomainMigration
    };
    let progress = observer
        .map(|o| migrate::ProgressTracker::new(writer, o, phase))
        .transpose()?;
    let layout = match state {
        StorageState::Legacy { .. } => prepare_legacy(dir, writer, progress.as_ref())?,
        StorageState::Current { layout, .. } => layout,
        StorageState::Empty => return Err(incompatible("unexpected empty storage")),
    };
    prepare_current(writer, runtime, progress.as_ref())?;
    if let Some(progress) = progress {
        progress.finish();
    }
    Ok(layout)
}
fn prepare_current(
    writer: &Connection,
    runtime: bool,
    progress: Option<&migrate::ProgressTracker<'_>>,
) -> Result<(), Error> {
    if runtime {
        pragmas::runtime_writer(writer).map_err(db_error)?;
        migrate::migrate_on(writer, progress)?;
    } else {
        pragmas::connection(writer, true).map_err(db_error)?;
    }
    Ok(())
}
fn prepare_legacy(
    dir: &Path,
    writer: &Connection,
    progress: Option<&migrate::ProgressTracker<'_>>,
) -> Result<Layout, Error> {
    // Same lifetime-held storage.lock; no second writer or WAL before conversion.
    crate::convert::convert_on(writer, progress)?;
    let state = inspect(writer)?;
    preflight(dir, &state, false)?;
    if let StorageState::Current { layout, .. } = state {
        Ok(layout)
    } else {
        Err(incompatible("conversion did not install current storage"))
    }
}
/// Hub open: lifetime-held process lock, read-only compatibility preflight, then WAL and
/// pending migrations. Legacy conversion shares the same lifetime-held lock.
/// Callers acquire instance.lock before this storage lock, never in reverse order.
pub fn open_runtime(dir: impl AsRef<Path>) -> Result<Engine, Error> {
    open(dir.as_ref(), true, None)
}
/// Offline administration: lifetime-held lock and exact schema checks, no migration,
/// metadata stamping, optimize, diagnostic task, or persistent pragma changes.
pub fn open_admin(dir: impl AsRef<Path>) -> Result<Engine, Error> {
    open(dir.as_ref(), false, None)
}

/// Runtime open with one cumulative conversion/domain-migration progress stream.
/// The caller holds instance.lock before entering; this acquires storage.lock
/// exactly once. complete denotes committed storage preparation, not Hub health.
/// Empty initialization has no conversion work and emits no progress events.
pub fn open_runtime_observed(
    dir: impl AsRef<Path>,
    observer: migrate::Observer,
) -> Result<Engine, Error> {
    open(dir.as_ref(), true, Some(observer))
}
