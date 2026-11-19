//! One writer per physical file and a shared-shape read pool. Writer ownership and
//! scheduling belong exclusively to the executor, never to domain callers.
use crate::{
    layout::{CORE_FILE, Layout},
    open::{db_error, incompatible},
    pragmas,
};
use r2d2::{Pool, PooledConnection};
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{Connection, OpenFlags};
use std::{
    collections::BTreeSet,
    fs::File,
    path::{Path, PathBuf},
    sync::{Arc, Mutex, MutexGuard},
};
use steve_types::{Code, Error};

/// Connection ownership. The process lock is declared last so writers/readers close first.
/// Pool handles may outlive Engine; their connections are strictly read-only.
pub struct Engine {
    writer: Mutex<Connection>,
    readers: Pool<SqliteConnectionManager>,
    layout: Layout,
    dir: PathBuf,
    file: String,
    runtime: bool,
    slot: WriterSlot,
    lock: Arc<File>,
}
/// Released after the connection fields, so a peer cannot open a replacement writer
/// until this writer is actually closed. All engines in a layout share the reservation set.
pub(crate) struct WriterSlot {
    active: Arc<Mutex<BTreeSet<String>>>,
    file: String,
}
impl WriterSlot {
    fn reserve(active: Arc<Mutex<BTreeSet<String>>>, file: &str) -> Result<Self, Error> {
        if !active
            .lock()
            .map_err(|_| steve_storage_api::error(Code::Internal, "writer registry was poisoned"))?
            .insert(file.into())
        {
            return Err(steve_storage_api::error(
                Code::Busy,
                "physical file already has an Engine writer",
            ));
        }
        Ok(Self {
            active,
            file: file.into(),
        })
    }
}
impl Drop for WriterSlot {
    fn drop(&mut self) {
        self.active
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&self.file);
    }
}
/// Pool capacity follows available CPUs, bounded independently of installation size.
pub fn reader_count() -> u32 {
    std::thread::available_parallelism().map_or(4, |n| n.get().clamp(4, 16) as u32)
}
impl Engine {
    pub(crate) fn from_opened(
        dir: PathBuf,
        layout: Layout,
        file: String,
        writer: Connection,
        lock: Arc<File>,
        runtime: bool,
        slot: Option<WriterSlot>,
    ) -> Result<Self, Error> {
        let slot = match slot {
            Some(slot) => slot,
            None => WriterSlot::reserve(Arc::new(Mutex::new(BTreeSet::new())), &file)?,
        };
        pragmas::attach_readonly(&writer, &dir, &layout, &file).map_err(db_error)?;
        let (reader_dir, reader_layout) = (dir.clone(), layout.clone());
        let manager = SqliteConnectionManager::file(dir.join(CORE_FILE))
            .with_flags(pragmas::readonly_flags())
            .with_init(move |c| {
                pragmas::connection(c, false)?;
                pragmas::attach_readonly(c, &reader_dir, &reader_layout, CORE_FILE)
            });
        let readers = Pool::builder()
            .max_size(reader_count())
            .min_idle(Some(1))
            .build(manager)
            .map_err(|_| {
                steve_storage_api::error(Code::Unavailable, "cannot initialize SQLite read pool")
            })?;
        Ok(Self {
            writer: Mutex::new(writer),
            readers,
            layout,
            dir,
            file,
            runtime,
            slot,
            lock,
        })
    }
    /// Executor-only seam. A grant/batch must hold the guard for its entire writer turn.
    /// The executor must avoid nested locking and schedule checkpoint calls outside grants.
    pub(crate) fn writer_conn(&self) -> Result<MutexGuard<'_, Connection>, Error> {
        self.writer
            .lock()
            .map_err(|_| steve_storage_api::error(Code::Internal, "storage writer was poisoned"))
    }
    pub fn layout(&self) -> &Layout {
        &self.layout
    }
    pub fn path(&self) -> PathBuf {
        self.dir.join(&self.file)
    }
    pub fn readers(&self) -> &Pool<SqliteConnectionManager> {
        &self.readers
    }
    pub fn read_conn(&self) -> Result<PooledConnection<SqliteConnectionManager>, Error> {
        self.readers
            .get()
            .map_err(|_| steve_storage_api::error(Code::Busy, "SQLite read pool is exhausted"))
    }
    /// Open another physical writer from the same validated manifest, sharing its lifetime
    /// lock. Future multi-file orchestration can build one executor per returned Engine.
    /// This does not authorize a transaction spanning files; every peer stays attached RO.
    pub fn open_peer(&self, file: &str) -> Result<Self, Error> {
        if file == self.file || !self.layout.files.contains_key(file) {
            return Err(incompatible("peer file is unknown or already this writer"));
        }
        let slot = WriterSlot::reserve(self.slot.active.clone(), file)?;
        self.layout.verify_files(&self.dir)?;
        let writer = Connection::open_with_flags(
            self.dir.join(file),
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_URI
                | OpenFlags::SQLITE_OPEN_NO_MUTEX,
        )
        .map_err(db_error)?;
        if self.runtime {
            pragmas::runtime_writer(&writer).map_err(db_error)?;
        } else {
            pragmas::connection(&writer, true).map_err(db_error)?;
        }
        Self::from_opened(
            self.dir.clone(),
            self.layout.clone(),
            file.into(),
            writer,
            self.lock.clone(),
            self.runtime,
            Some(slot),
        )
    }
    pub fn data_dir(&self) -> &Path {
        &self.dir
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn actual_engine_writer_uses_its_own_main_and_readonly_peer_attachments() {
        let dir = tempfile::tempdir().unwrap();
        let engine = crate::open::open_runtime(dir.path()).unwrap();
        let mut layout = engine.layout().clone();
        drop(engine);
        layout
            .domains
            .insert(steve_storage_api::Domain::Stream, "peer.sqlite3".into());
        layout
            .files
            .insert("peer.sqlite3".into(), uuid::Uuid::new_v4().to_string());
        let c = Connection::open(dir.path().join("peer.sqlite3")).unwrap();
        c.execute_batch(crate::schema::CONTRACT).unwrap();
        c.execute_batch("CREATE TABLE peer_only(id); INSERT INTO peer_only VALUES(7);")
            .unwrap();
        layout.write_file_meta(&c, "peer.sqlite3").unwrap();
        drop(c);
        let c = Connection::open(dir.path().join(CORE_FILE)).unwrap();
        layout.write_file_meta(&c, CORE_FILE).unwrap();
        drop(c);
        let engine = crate::open::open_runtime(dir.path()).unwrap();
        let writer = engine.writer_conn().unwrap();
        assert_eq!(
            writer
                .query_row("SELECT id FROM peer_only", [], |r| r.get::<_, u32>(0))
                .unwrap(),
            7
        );
        assert_eq!(
            writer
                .execute("INSERT INTO peer_only VALUES(8)", [])
                .unwrap_err()
                .sqlite_error_code(),
            Some(rusqlite::ErrorCode::ReadOnly)
        );
        assert_eq!(
            writer
                .pragma_query_value(None, "cache_size", |r| r.get::<_, i64>(0))
                .unwrap(),
            -32_768
        );
        assert_eq!(
            writer
                .pragma_query_value(None, "journal_size_limit", |r| r.get::<_, i64>(0))
                .unwrap(),
            pragmas::JOURNAL_SIZE_LIMIT
        );
        assert_eq!(
            writer
                .pragma_query_value(None, "wal_autocheckpoint", |r| r.get::<_, i64>(0))
                .unwrap(),
            0
        );
        drop(writer);
        engine
            .checkpoint(crate::checkpoint::Mode::Truncate)
            .unwrap();
        assert_eq!(
            engine
                .writer_conn()
                .unwrap()
                .pragma_query_value(None, "busy_timeout", |r| r.get::<_, u32>(0))
                .unwrap(),
            5_000
        );
        let peer = engine.open_peer("peer.sqlite3").unwrap();
        let writer = peer.writer_conn().unwrap();
        writer
            .execute("INSERT INTO main.peer_only VALUES(9)", [])
            .unwrap();
        assert!(
            writer
                .execute("CREATE TABLE db_0.forbidden(id)", [])
                .is_err()
        );
        let main: String = writer
            .query_row(
                "SELECT file FROM pragma_database_list WHERE name='main'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(Path::new(&main), peer.path());
    }
}
