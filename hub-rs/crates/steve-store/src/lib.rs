//! SQLite storage for the Steve Hub: the Rust port of hub/src/db.ts and hub/src/repo.ts.
//!
//! [`Store::open`] runs the db.ts migrations verbatim; the repositories (`store.items()`, `store.bots()`, …)
//! mirror the repo.ts objects one to one and publish a [`ChangeEvent`] after every write.
//!
//! Connections: one write connection behind a `std::sync::Mutex`, plus an r2d2 pool of read-only
//! connections (one per core, 4 to 16). SQLite allows a single writer anyway and every write here is a few statements, so a
//! mutex costs nothing and keeps the TS guarantee that writes (and their events) happen in one order;
//! WAL lets the pooled readers serve `/api/state` and event paging in parallel without waiting on it.
//! A write followed by a read in the same operation runs on the write connection, so it sees its own data.
//!
//! The API is synchronous like better-sqlite3; calls take microseconds,
//! async callers can run them inline or in `spawn_blocking` for the big reads.

steve_i18n::init!();

pub mod base;
mod bus;
pub mod db;
mod event_payload;
pub mod maintenance;
pub mod outbox;
mod repo;
mod tx;
mod types;
pub mod workflow_gate;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{Connection, OpenFlags};
use sha2::{Digest, Sha256};

pub use bus::{ChangeEvent, FnPublisher, NoPublisher, Publisher};
pub use repo::*;
pub use tx::Tx;
pub use types::*;

/// The default workspace id (`WS` in repo.ts).
pub const WS: &str = "default";

/// Number of pooled read connections: one per core, at least 4 and at most 16. WAL readers do not
/// block each other or the writer, so a request waits on the pool only when this many reads are running.
fn readers() -> u32 {
    std::thread::available_parallelism()
        .map_or(4, |n| n.get() as u32)
        .clamp(4, 16)
}

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error(transparent)]
    Sqlite(#[from] rusqlite::Error),
    #[error(transparent)]
    Pool(#[from] r2d2::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// The TS `throw new Error("没有这个…")` cases; the message is user-facing.
    #[error("{0}")]
    NotFound(&'static str),
    /// A hook registered by a module (e.g. [`Store::on_item_deleted`]) refused; the write rolls back.
    #[error("{0}")]
    Hook(String),
    /// A migration left the database inconsistent; it was rolled back.
    #[error("{0}")]
    Migration(String),
    /// The database needs a newer build: opening it is refused, nothing was written.
    #[error(
        "数据库（schema v{schema}）需要至少认识 schema v{min_reader} 的版本才能打开，这个版本只认识到 v{ours}；没有打开，也没有改动它"
    )]
    Incompatible {
        schema: i64,
        min_reader: i64,
        ours: i64,
    },
    /// The write contradicts the item's lifecycle (a terminal status outside the lifecycle, a status
    /// change on a closed item); the message is user-facing.
    #[error("{0}")]
    Conflict(String),
}

/// Runs inside the transaction that deletes an item: `(tx, workspace_id, item_id)`.
pub type ItemDeletedHook = dyn Fn(&Tx, &str, &str) -> std::result::Result<(), String> + Send + Sync;

pub type Result<T, E = StoreError> = std::result::Result<T, E>;

/// Unix milliseconds, like `Date.now()`.
pub fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// `uid(p)`: prefix, underscore, first 16 hex chars of a random UUID v4.
pub fn uid(prefix: &str) -> String {
    let hex = uuid::Uuid::new_v4().simple().to_string();
    format!("{prefix}_{}", &hex[..16])
}

/// sha256 as lowercase hex; auth and MCP tokens are stored only in this form.
pub fn sha(s: &str) -> String {
    Sha256::digest(s.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Shared handle to the database. Cheap to clone.
#[derive(Clone)]
pub struct Store {
    inner: Arc<Inner>,
}

struct Inner {
    maintenance: Arc<maintenance::Gate>,
    writer: Mutex<Connection>,
    readers: r2d2::Pool<SqliteConnectionManager>,
    bus: Box<dyn Publisher>,
    path: PathBuf,
    item_deleted: std::sync::OnceLock<Box<ItemDeletedHook>>,
    /// Notified after a commit that emitted an outbox event ([`outbox`]).
    outbox_wake: Arc<tokio::sync::Notify>,
}

/// The write connection plus the publisher, handed to write closures so events go out under the lock.
pub(crate) struct W<'a> {
    pub c: &'a Connection,
    bus: &'a dyn Publisher,
}

impl W<'_> {
    pub fn publish(&self, e: ChangeEvent) {
        self.bus.publish(e)
    }

    /// A write transaction on the write connection, see [`immediate`].
    pub fn tx(&self) -> rusqlite::Result<rusqlite::Transaction<'_>> {
        immediate(self.c)
    }
}

/// `BEGIN IMMEDIATE`: takes the write lock at the start. The writer Mutex orders this process's
/// writes, but another process (the upgrade supervisor, the next release during a switch) can open the
/// same file; a DEFERRED transaction that read first and then writes after that process committed
/// fails at once with SQLITE_BUSY_SNAPSHOT, while BEGIN IMMEDIATE waits out the busy timeout.
/// Read-only transactions on the pooled readers stay DEFERRED (IMMEDIATE fails there).
pub(crate) fn immediate(c: &Connection) -> rusqlite::Result<rusqlite::Transaction<'_>> {
    rusqlite::Transaction::new_unchecked(c, rusqlite::TransactionBehavior::Immediate)
}

impl Store {
    /// Opens `path` (creating it), sets the pragmas, migrates, and starts the read pool.
    /// The TS Hub's path is `STEVE_HOME/data/steve.sqlite3`; the caller decides.
    pub fn open(path: impl AsRef<Path>, bus: impl Publisher + 'static) -> Result<Self> {
        let path = path.as_ref().to_path_buf();
        let writer = db::open_writer(&path)?;
        let manager = SqliteConnectionManager::file(&path)
            .with_flags(OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)
            .with_init(|c| db::pragmas(c));
        // a checkout test (an empty statement per `get`) finds nothing on a local file
        let readers = r2d2::Pool::builder()
            .max_size(readers())
            .test_on_check_out(false)
            .build(manager)?;
        let maintenance = Arc::new(maintenance::Gate::load(&writer)?);
        Ok(Self {
            inner: Arc::new(Inner {
                maintenance,
                writer: Mutex::new(writer),
                readers,
                bus: Box::new(bus),
                path,
                item_deleted: std::sync::OnceLock::new(),
                outbox_wake: Arc::new(tokio::sync::Notify::new()),
            }),
        })
    }

    /// Sets what else goes when an item is deleted (the knowledge module's document and index rows),
    /// in the same transaction as the delete. Set once; `false` if already set.
    pub fn on_item_deleted(
        &self,
        f: impl Fn(&Tx, &str, &str) -> std::result::Result<(), String> + Send + Sync + 'static,
    ) -> bool {
        self.inner.item_deleted.set(Box::new(f)).is_ok()
    }

    pub(crate) fn item_deleted_hook(&self) -> Option<&ItemDeletedHook> {
        self.inner.item_deleted.get().map(|b| b.as_ref())
    }

    /// Wakes the outbox dispatcher: notified (one permit) after each commit that emitted an event.
    pub fn outbox_wake(&self) -> Arc<tokio::sync::Notify> {
        self.inner.outbox_wake.clone()
    }

    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    /// Publishes an event that is not tied to a store write (`steve.thinking`, imports finishing).
    pub fn publish(&self, e: ChangeEvent) {
        self.inner.bus.publish(e)
    }

    fn lock(&self) -> MutexGuard<'_, Connection> {
        // A panic mid-write leaves SQLite consistent (the transaction rolls back), so poisoning is ignored.
        self.inner.writer.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(crate) fn write<T>(&self, f: impl FnOnce(&W) -> Result<T>) -> Result<T> {
        let c = self.lock();
        f(&W {
            c: &c,
            bus: self.inner.bus.as_ref(),
        })
    }

    pub(crate) fn read<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let c = self.inner.readers.get()?;
        f(&c)
    }

    /// Refreshes the query planner's statistics where they went stale (`PRAGMA optimize`, bounded), on
    /// the write connection. Cheap when nothing changed; meant for an hourly housekeeping pass.
    pub fn optimize(&self) -> Result<()> {
        Ok(db::optimize(&self.lock(), false)?)
    }

    /// Runs `f` on the write connection under the write lock, for one-off maintenance and tests.
    /// Prefer a repository method; this exists so callers never need a second connection.
    pub fn with_conn<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        let c = self.lock();
        f(&c)
    }
}
