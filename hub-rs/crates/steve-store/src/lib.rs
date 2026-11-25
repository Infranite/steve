//! SQLite storage for the Steve Hub: the Rust port of hub/src/db.ts and hub/src/repo.ts.
//!
//! [`Store::open`] runs the db.ts migrations verbatim; the repositories (`store.items()`, `store.bots()`, …)
//! mirror the repo.ts objects one to one and publish a [`BusEvent`] after every write.
//!
//! Connections: one write connection behind a `std::sync::Mutex`, plus an r2d2 pool of read-only
//! connections (one per core, 4 to 16). SQLite allows a single writer anyway and every write here is a few statements, so a
//! mutex costs nothing and keeps the TS guarantee that writes (and their events) happen in one order;
//! Offline administration uses one lazy reader instead.
//! WAL lets the pooled readers serve `view reads` and event paging in parallel without waiting on it.
//! A write followed by a read in the same operation runs on the write connection, so it sees its own data.
//!
//! The API is synchronous like better-sqlite3; calls take microseconds,
//! async callers can run them inline or in `spawn_blocking` for the big reads.

steve_i18n::init!();

/// Physical backup and restore of an offline or WAL-mode SQLite installation. These storage
/// maintenance operations do not open a Store or run migrations, and never take its writer lock.
pub mod backup {
    pub use steve_store_sqlite::backup::offline::{
        BackupError, CancelFlag, CopiedFile, CopyOptions, Progress, backup_file, close_wal,
        verify_file,
    };
}
pub mod base;
pub mod bot_dispatch;
mod bus;
pub mod calendar;
pub mod caps;
pub mod compact;
mod contract;
pub use contract::approval;
pub use contract::audit as audit_repo;
pub use contract::backup_policy;
pub use contract::channel_feed;
pub use contract::channel_messages as channel_message_repo;
pub use contract::channel_names;
pub use contract::channel_personas;
pub use contract::channel_positions;
pub use contract::channel_sends;
pub use contract::channel_spaces;
pub use contract::channel_trigger_report;
pub use contract::channel_write_policy;
pub use contract::feishu_registration;
pub use contract::notifications as notification_repo;
pub use contract::notify;
pub use contract::reply_basis;
pub use contract::{
    Accounts, AuthSessions, ConnRow, ConnectionBrief, HealthState, MatchRule, MatchRules, NewParty,
    NewVaultGrant, Party, PartyPatch, PasskeyInfo, PasskeyRows, PrefixConfig, System, Systems,
    UserRows, VaultGrant, VaultGrantRecord, account_profiles, account_rows, builtin_system_name,
    is_builtin_system, secret_uses, vault_items, vault_keys,
};
pub mod db;
pub mod diagnostics;
pub mod digest;
mod event_payload;
mod ingest;
pub mod item_gate;
pub mod knowledge;
pub mod maintenance;
pub mod outbox;
mod repo;
pub mod seeded;
pub mod statistics;
mod tx;
mod types;
pub mod views;

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{SystemTime, UNIX_EPOCH};

use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::{Connection, OpenFlags};
use sha2::{Digest, Sha256};

pub use bus::{FnPublisher, NoPublisher, Publisher};
pub use contract::alarms::{Alarm, Alarms, NewAlarm};
pub use contract::automation::{ScheduleRow, Schedules};
pub use contract::changesets as changeset_repo;
#[cfg(feature = "testkit")]
pub use contract::core_fixture;
pub use contract::core_usage;
pub use contract::item_delivery;
pub use contract::message_sources::{
    Attachment, ChannelMessage, ChannelSources, Connections, MailMessage, MailSources,
    MessageSelection,
};
pub use contract::node_commands;
pub use contract::node_directory;
pub use contract::node_environment;
pub use contract::node_sessions;
pub use contract::node_turns;
pub use contract::operations::{self as operation_repo, ItemOperation};
pub use contract::read_models::{
    AttentionReads, FeedReads, ItemEventFields, ItemHead, ItemReads, NotificationPolicies,
    QuietPolicy, TimelineRows, TimezoneSettings,
};
pub use contract::realtime_output;
pub use contract::tags as tag_repo;
pub use contract::thread;
pub use contract::thread::{
    CommitOutcome, CommitReplyInput, DeliveryRefClaim, DeliveryRefRow, DispatchClaim, DispatchRef,
    EditQueuedOutcome, NewRevisionTask, ReceiptDispatch, RecoveredTask, ReplyReceipt,
    StopTaskOutcome, SubmitInput, SubmitOutcome, ThreadNotice, ThreadTask, WithdrawOutcome,
};
pub use contract::vault_uses as vault_use_repo;
pub use contract::waits as wait_repo;
pub use contract::workflow_runs::{
    self, FinalizedAttempt, NewWfAttempt, NewWfCall, ReviewGateFailure, ReviewGateVerdict,
    WfAttemptStatus, WfCall, WfCallAttempt, WfCallStatus, WfRun, WfRunStatus, WfScriptVersion,
    WfVersionCall, WorkflowRuns,
};
pub use contract::{
    AccountIdentity, AddedMemoryIndex, BotPrompt, BotRef, BotRows, Contacts, DecisionRows,
    DigestEntry, DigestNotes, DueTrigger, FileInfo, FileReads, FlowGrant, FlowRow, FlowRows,
    FlowSources, FlowTriggers, FlowWaits, GrantRows, HTTP_ALLOW_PRIVATE, IndexedRange,
    ItemEvidence, Members, MemoryIndex, MemoryIndexData, MemoryRange, MessageEvidence,
    NewCorrection, NewFlowRow, NewRun, NewSignal, NewStep, NewTrigger, NewWait, ProjectRef,
    ProjectRows, R, Read, References, ReplyBot, Revisions, RunRow, RunRows, RunStep, SchedulePatch,
    SessionEvidence, SignalFilter, SignalRow, SignalRows, SignalTriage, SourceConfig,
    StartupRecord, TagRows, W, WorkspaceSettings, Workspaces,
};
pub use contract::{AttrDef, AttrValue, Attributes, ContactQuery, NewDef, NewValue};
pub use contract::{
    ContactMerge, ContactMerges, DirectoryConnection, MergeProposals, NewMergeProposal,
};
pub use contract::{DeviceIdentity, DeviceRows, PersonaIdentity, PersonaRows};
pub use contract::{MembershipRow, SyncedTeam, TeamDetails, TeamRef, Teams};
pub use contract::{Steward, StewardKey, Stewards};
pub use contract::{
    accounts, bots, connections, decisions as decision_repo, files as file_repo,
    flows as flow_repo, github, grants, items as item_repo, links as link_repo, mail, parties,
    projects, sessions as session_repo, settings, sources,
};
pub use ingest::*;
pub use repo::*;
pub use steve_types::BusEvent;
pub use types::*;
pub use views::Views;

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
    /// Offline administration must never initialize or upgrade a database.
    #[error("离线管理需要本版本的数据库头部；请先启动或升级 Hub：{0}")]
    AdminSchema(String),
    /// The write contradicts the item's lifecycle (a terminal status outside the lifecycle, a status
    /// change on a closed item); the message is user-facing.
    #[error("{0}")]
    Conflict(String),
    /// The database itself failed (I/O, a full disk, corruption, a lock held elsewhere), on the
    /// storage contract ([`Store::write`]): the same write can succeed later.
    #[error("{0}")]
    Storage(String),
    /// The database refused this write (a constraint, a type, a statement it cannot run), on the
    /// storage contract: the same write fails again.
    #[error("{0}")]
    Rejected(String),
}

/// SQLite result codes of the database failing rather than of the statement: the one list for
/// [`StoreError::is_storage_failure`] and the contract's SQLite backend.
pub(crate) fn storage_failure(code: rusqlite::ErrorCode) -> bool {
    use rusqlite::ErrorCode::*;
    matches!(
        code,
        DatabaseBusy
            | DatabaseLocked
            | OutOfMemory
            | ReadOnly
            | OperationInterrupted
            | SystemIoFailure
            | DatabaseCorrupt
            | DiskFull
            | CannotOpen
            | FileLockingProtocolFailed
            | NotADatabase
    )
}

impl StoreError {
    /// The database itself failed (I/O, a full disk, corruption, a lock held elsewhere), not
    /// the write being attempted: retrying the same write later can succeed.
    /// Everything else is a failure of this write ([`StoreError::Rejected`] on the contract).
    pub fn is_storage_failure(&self) -> bool {
        match self {
            Self::Pool(_) | Self::Storage(_) => true,
            Self::Sqlite(e) => e.sqlite_error_code().is_some_and(storage_failure),
            _ => false,
        }
    }
}

/// Runs inside the transaction that deletes an item: `(tx, workspace_id, item_id)`.
pub type ItemDeletedHook =
    dyn Fn(&mut W<'_>, &str, &str) -> std::result::Result<(), String> + Send + Sync;

pub type Result<T, E = StoreError> = std::result::Result<T, E>;

/// A failed session open requires an explicit human retry. Used by both its durable stop and
/// its decision card, so background queue flushing and YOLO cannot restart it.
pub const SESSION_START_FAILURE: &str = "session_start";

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
    commit_wakes: Arc<Mutex<CommitWakes>>,
    diagnostics: std::sync::OnceLock<diagnostics::Recorder>,
    windows: repo::window_cache::WindowCaches,
    write_txs: std::sync::atomic::AtomicU64,
}

#[derive(Default)]
struct CommitWakes {
    hooked: bool,
    listeners: Vec<(Vec<&'static str>, std::sync::Weak<tokio::sync::Notify>)>,
    changed: std::collections::BTreeSet<String>,
    committed: std::collections::BTreeSet<String>,
}

/// The private SQLite writer lease and its event publisher.
pub(crate) struct WriteConn<'a> {
    pub c: &'a Connection,
    store: &'a Store,
}

impl WriteConn<'_> {
    pub fn publish(&self, e: impl Into<BusEvent>) {
        self.store.emit_event(e.into())
    }

    /// A write transaction on the write connection (or a nested `SAVEPOINT` inside [`Store::write`]).
    pub fn tx(&self) -> rusqlite::Result<tx::WriteTx<'_>> {
        tx::WriteTx::begin(self.c)
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
        Self::open_observed(path.as_ref(), bus, None, false)
    }

    /// Opens an existing database at exactly this build's schema head for offline administration.
    /// The caller must hold the Hub instance lock. Repository writes remain available, but opening
    /// never migrates, optimizes, initializes workspaces or starts diagnostic recording. One lazy
    /// read connection is enough for a CLI.
    pub fn open_admin(path: impl AsRef<Path>) -> Result<Self> {
        Self::open_observed(path.as_ref(), NoPublisher, None, true)
    }

    /// Like [`Self::open`], with a callback for actual migration work (also within long statements).
    /// The caller owns reporting; ordinary opens install no progress handler.
    pub fn open_with_migration_progress(
        path: impl AsRef<Path>,
        bus: impl Publisher + 'static,
        observer: db::MigrationObserver,
    ) -> Result<Self> {
        Self::open_observed(path.as_ref(), bus, Some(observer), false)
    }

    fn open_observed(
        path: &Path,
        bus: impl Publisher + 'static,
        observer: Option<db::MigrationObserver>,
        admin: bool,
    ) -> Result<Self> {
        let path = path.to_path_buf();
        let writer = if admin {
            db::open_admin_writer(&path)?
        } else {
            db::open_writer(&path, observer)?
        };
        let manager = SqliteConnectionManager::file(&path)
            .with_flags(OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)
            .with_init(|c| db::pragmas(c));
        // a checkout test (an empty statement per `get`) finds nothing on a local file
        let readers = r2d2::Pool::builder()
            .max_size(if admin { 1 } else { readers() })
            .min_idle(if admin { Some(0) } else { None })
            .test_on_check_out(false)
            .build(manager)?;
        contract::Backend::lock_instance(&mut contract::Sqlite(&writer))?;
        let maintenance = Arc::new(maintenance::Gate::load(&writer)?);
        let diagnostic_until = if admin {
            0
        } else {
            diagnostics::active_until(&writer).unwrap_or(0)
        };
        let store = Self {
            inner: Arc::new(Inner {
                maintenance,
                writer: Mutex::new(writer),
                readers,
                bus: Box::new(bus),
                path,
                item_deleted: std::sync::OnceLock::new(),
                outbox_wake: Arc::new(tokio::sync::Notify::new()),
                commit_wakes: Arc::default(),
                diagnostics: std::sync::OnceLock::new(),
                windows: repo::window_cache::WindowCaches::new(),
                write_txs: std::sync::atomic::AtomicU64::new(0),
            }),
        };
        if !admin
            && let Some(recorder) =
                diagnostics::Recorder::start(Arc::downgrade(&store.inner), diagnostic_until)
        {
            let _ = store.inner.diagnostics.set(recorder);
        }
        Ok(store)
    }

    /// Sets what else goes when an item is deleted (the knowledge module's document and index rows),
    /// in the same transaction as the delete. Set once; `false` if already set.
    pub fn on_item_deleted(
        &self,
        f: impl Fn(&mut W<'_>, &str, &str) -> std::result::Result<(), String> + Send + Sync + 'static,
    ) -> bool {
        self.inner.item_deleted.set(Box::new(f)).is_ok()
    }

    pub(crate) fn item_deleted_hook(&self) -> Option<&ItemDeletedHook> {
        self.inner.item_deleted.get().map(|b| b.as_ref())
    }

    /// Connect only to a DueSource's Notify. This content-free hint must never identify a row
    /// or control a business-logic branch. Hints may coalesce or be spurious; in-process watched
    /// row changes through the authoritative Store (or its clones) wake after the outermost commit.
    /// Independently opened Stores and other processes are not observed. Correctness must still
    /// rely on due_loop's 60-second fallback.
    /// DomainEvent carries business facts; it must not be a second wake path for a due loop.
    ///
    /// Known update_hook blind spots include truncate-optimized unconditional DELETEs and
    /// WITHOUT ROWID writes. These omissions are not permission for silent mutations of a
    /// subscribed table. REPLACE skips callbacks for its implicit deleted rows, but its
    /// replacement INSERT normally hints the same rowid table; this is a table-level hint,
    /// not a per-row deletion event.
    /// A rolled-back savepoint can leave a harmless extra hint when its outer write commits.
    /// Each SQLite commit seals its hint separately, so a later autocommit statement failure or
    /// rollback in the same writer closure cannot erase an earlier successful commit.
    /// Hooks install lazily for live subscribers and detach before the next writer lease
    /// without subscribers. Subscribing inside a write scope also hints at its outer commit.
    pub fn commit_wake(&self, tables: &[&'static str]) -> Arc<tokio::sync::Notify> {
        let wake = Arc::new(tokio::sync::Notify::new());
        let register = |c: &Connection| {
            self.inner
                .commit_wakes
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .listeners
                .push((tables.to_vec(), Arc::downgrade(&wake)));
            self.sync_commit_hooks(c);
        };
        if let Some(c) = self.active_conn() {
            register(c);
            // Earlier statements may predate hook installation. A conservative hint is safe.
            let mut state = self
                .inner
                .commit_wakes
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let pending = if c.is_autocommit() {
                &mut state.committed
            } else {
                &mut state.changed
            };
            pending.extend(tables.iter().map(|t| (*t).to_owned()));
        } else {
            let c = self.lock();
            register(c.connection());
        }
        wake
    }

    // Registration and hook installation share the unique writer lease, so a new
    // subscription cannot race an already-running transaction on another thread.
    fn sync_commit_hooks(&self, c: &Connection) {
        let mut state = self
            .inner
            .commit_wakes
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        state.listeners.retain(|(_, w)| w.strong_count() > 0);
        let needed = !state.listeners.is_empty();
        if needed == state.hooked {
            return;
        }
        if needed {
            let wakes = self.inner.commit_wakes.clone();
            c.update_hook(Some(move |_, _: &str, table: &str, _| {
                let mut wakes = wakes.lock().unwrap_or_else(|e| e.into_inner());
                if !wakes.changed.contains(table)
                    && wakes
                        .listeners
                        .iter()
                        .any(|(tables, _)| tables.contains(&table))
                {
                    wakes.changed.insert(table.to_owned());
                }
            }))
            .expect("writer lease permits installing update hook");
            let wakes = self.inner.commit_wakes.clone();
            c.commit_hook(Some(move || {
                let mut state = wakes.lock().unwrap_or_else(|e| e.into_inner());
                let changed = std::mem::take(&mut state.changed);
                state.committed.extend(changed);
                false // A hint must never veto a business commit.
            }))
            .expect("writer lease permits installing commit hook");
            let wakes = self.inner.commit_wakes.clone();
            c.rollback_hook(Some(move || {
                wakes
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .changed
                    .clear();
            }))
            .expect("writer lease permits installing rollback hook");
        } else {
            c.update_hook(None::<fn(rusqlite::hooks::Action, &str, &str, i64)>)
                .expect("remove update hook");
            c.commit_hook(None::<fn() -> bool>)
                .expect("remove commit hook");
            c.rollback_hook(None::<fn()>).expect("remove rollback hook");
            state.changed.clear();
            state.committed.clear();
        }
        state.hooked = needed;
    }

    fn notify_committed(&self) {
        let mut wakes = self
            .inner
            .commit_wakes
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let changed = std::mem::take(&mut wakes.committed);
        wakes.listeners.retain(|(tables, w)| {
            if let Some(wake) = w.upgrade() {
                if tables.iter().any(|t| changed.contains(*t)) {
                    wake.notify_one();
                }
                true
            } else {
                false
            }
        });
    }

    /// Wakes the outbox dispatcher: notified (one permit) after each commit that emitted an event.
    pub fn outbox_wake(&self) -> Arc<tokio::sync::Notify> {
        self.inner.outbox_wake.clone()
    }

    pub fn path(&self) -> &Path {
        &self.inner.path
    }

    pub(crate) fn inner_id(&self) -> usize {
        Arc::as_ptr(&self.inner) as usize
    }

    /// Publishes an event that is not tied to a store write (`steve.thinking`, imports finishing).
    pub fn publish(&self, e: impl Into<BusEvent>) {
        self.emit_event(e.into())
    }

    /// Number of outermost SQLite writer lock acquisitions performed on this `Store`.
    pub fn write_tx_count(&self) -> u64 {
        self.inner
            .write_txs
            .load(std::sync::atomic::Ordering::Relaxed)
    }

    /// Monotonic version of the in-process attention window cache.
    pub fn attention_window_version(&self) -> u64 {
        self.inner.windows.attention.version()
    }

    #[track_caller]
    pub(crate) fn lock(&self) -> WriteGuard<'_> {
        let timing = LockTiming::start("store", std::panic::Location::caller());
        // A panic mid-write leaves SQLite consistent (the transaction rolls back), so poisoning is ignored.
        let conn = self.inner.writer.lock().unwrap_or_else(|e| e.into_inner());
        self.inner
            .write_txs
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        instrumentation::store_acquired();
        self.sync_commit_hooks(&conn);
        WriteGuard {
            conn,
            timing: timing.map(LockTiming::acquired),
        }
    }

    #[track_caller]
    pub(crate) fn write_conn<T>(&self, f: impl FnOnce(&WriteConn) -> Result<T>) -> Result<T> {
        if let Some(c) = self.active_conn() {
            return f(&WriteConn { c, store: self });
        }
        let c = self.lock();
        let result = f(&WriteConn {
            c: c.connection(),
            store: self,
        });
        if c.connection().is_autocommit() {
            self.notify_committed();
        }
        result
    }

    pub(crate) fn read_conn<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        self.sqlite_read(f)
    }

    /// Refreshes the query planner's statistics where they went stale (`PRAGMA optimize`, bounded), on
    /// the write connection. Cheap when nothing changed; meant for an hourly housekeeping pass.
    #[track_caller]
    pub fn optimize(&self) -> Result<()> {
        if let Some(c) = self.active_conn() {
            return Ok(db::optimize(c, false)?);
        }
        Ok(db::optimize(self.lock().connection(), false)?)
    }

    /// Runs `f` on the write connection under the write lock, for one-off maintenance and tests.
    /// Prefer a repository method; this exists so callers never need a second connection.
    #[track_caller]
    pub(crate) fn write_locked<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        if let Some(c) = self.active_conn() {
            return f(c);
        }
        let c = self.lock();
        let result = f(c.connection());
        if c.connection().is_autocommit() {
            self.notify_committed();
        }
        result
    }
}

/// Runs a multi-statement read under a single snapshot. When `c` is already inside an active
/// transaction (such as a read inside [`Store::write`]), reuses that transaction instead of
/// starting a nested `BEGIN DEFERRED`.
pub(crate) fn read_snapshot<T>(
    c: &Connection,
    f: impl FnOnce(&Connection) -> Result<T>,
) -> Result<T> {
    if !c.is_autocommit() {
        return f(c);
    }
    let tx = rusqlite::Transaction::new_unchecked(c, rusqlite::TransactionBehavior::Deferred)?;
    let out = f(&tx)?;
    tx.commit()?;
    Ok(out)
}

// Shared lock-boundary mechanics; recorder installation belongs to the embedding application.
pub mod instrumentation {
    /// Whether the Hub installed its in-memory recorder. Off: one flag load, no clock or TLS.
    static LOCK_METRICS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

    pub fn enable_lock_metrics() {
        // Publish the already-installed recorder before any worker caches its first handle.
        LOCK_METRICS.store(true, std::sync::atomic::Ordering::Release);
    }

    tokio::task_local! {
        static REQUEST: std::sync::Arc<Request>;
    }

    #[derive(Default)]
    struct Request {
        calls: std::sync::atomic::AtomicU64,
        rev: std::sync::atomic::AtomicU64,
    }

    /// One HTTP request's context, shared with carried closures: its successful store connection
    /// acquisitions, and the newest bus revision it committed.
    #[derive(Clone, Default)]
    pub struct RequestCalls(std::sync::Arc<Request>);

    impl RequestCalls {
        pub async fn scope<F: std::future::Future>(&self, future: F) -> F::Output {
            REQUEST.scope(self.0.clone(), future).await
        }

        pub fn count(&self) -> u64 {
            self.0.calls.load(std::sync::atomic::Ordering::Relaxed)
        }

        /// The revision of the newest batch the request committed; 0 when it committed none.
        pub fn rev(&self) -> u64 {
            self.0.rev.load(std::sync::atomic::Ordering::Relaxed)
        }
    }

    /// Carry this request's context into synchronous work on another thread. Like i18n's
    /// carrier, sync_scope restores the worker's previous context on return or unwind.
    pub fn carry<R>(f: impl FnOnce() -> R) -> impl FnOnce() -> R {
        let request = REQUEST.try_with(std::sync::Arc::clone).ok();
        move || match request {
            Some(request) => REQUEST.sync_scope(request, f),
            None => f(),
        }
    }

    pub(crate) fn store_acquired() {
        if !LOCK_METRICS.load(std::sync::atomic::Ordering::Acquire) {
            return;
        }
        let _ = REQUEST.try_with(|r| {
            r.calls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        });
    }

    /// A batch committed on this thread was published at revision `rev`.
    pub fn committed(rev: u64) {
        let _ = REQUEST.try_with(|r| {
            r.rev.fetch_max(rev, std::sync::atomic::Ordering::Relaxed);
        });
    }

    /// Same opt-in gate as lock timing; only the first transaction on a thread registers a handle.
    pub(crate) fn transaction_started() {
        if !LOCK_METRICS.load(std::sync::atomic::Ordering::Acquire) {
            return;
        }
        thread_local! {
            static TRANSACTIONS: std::cell::OnceCell<metrics::Counter> = const { std::cell::OnceCell::new() };
        }
        TRANSACTIONS.with(|counter| {
            counter
                .get_or_init(|| metrics::counter!("steve_store_transactions_total"))
                .increment(1);
        });
    }

    /// Low-cardinality SQL operation category for storage backend metrics.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    #[repr(usize)]
    pub(crate) enum SqlOp {
        Query = 0,
        Exec = 1,
        Batch = 2,
    }

    impl SqlOp {
        const COUNT: usize = 3;
        const fn as_str(self) -> &'static str {
            match self {
                Self::Query => "query",
                Self::Exec => "exec",
                Self::Batch => "batch",
            }
        }
    }

    /// Bounded table family derived deterministically from SQL without allocating or exposing SQL text.
    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    #[repr(usize)]
    pub(crate) enum TableFamily {
        Items = 0,
        Sessions = 1,
        Decisions = 2,
        Messages = 3,
        Mail = 4,
        Calendar = 5,
        Flows = 6,
        Knowledge = 7,
        People = 8,
        Config = 9,
        Nodes = 10,
        Vault = 11,
        Github = 12,
        Stats = 13,
        Tx = 14,
        Other = 15,
    }

    impl TableFamily {
        const COUNT: usize = 16;
        const fn as_str(self) -> &'static str {
            match self {
                Self::Items => "items",
                Self::Sessions => "sessions",
                Self::Decisions => "decisions",
                Self::Messages => "messages",
                Self::Mail => "mail",
                Self::Calendar => "calendar",
                Self::Flows => "flows",
                Self::Knowledge => "knowledge",
                Self::People => "people",
                Self::Config => "config",
                Self::Nodes => "nodes",
                Self::Vault => "vault",
                Self::Github => "github",
                Self::Stats => "stats",
                Self::Tx => "tx",
                Self::Other => "other",
            }
        }
    }

    fn next_sql_token(bytes: &[u8], mut pos: usize) -> Option<(&[u8], usize)> {
        loop {
            while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
                pos += 1;
            }
            if pos + 1 < bytes.len() && bytes[pos] == b'-' && bytes[pos + 1] == b'-' {
                pos += 2;
                while pos < bytes.len() && bytes[pos] != b'\n' {
                    pos += 1;
                }
                continue;
            }
            if pos + 1 < bytes.len() && bytes[pos] == b'/' && bytes[pos + 1] == b'*' {
                pos += 2;
                while pos + 1 < bytes.len() && !(bytes[pos] == b'*' && bytes[pos + 1] == b'/') {
                    pos += 1;
                }
                pos = (pos + 2).min(bytes.len());
                continue;
            }
            break;
        }
        if pos >= bytes.len() {
            return None;
        }
        let b = bytes[pos];
        if matches!(b, b'"' | b'`' | b'\'') {
            let quote = b;
            let start = pos + 1;
            pos = start;
            while pos < bytes.len() && bytes[pos] != quote {
                pos += 1;
            }
            let end = pos;
            if pos < bytes.len() {
                pos += 1;
            }
            return Some((&bytes[start..end], pos));
        }
        if b.is_ascii_alphanumeric() || b == b'_' {
            let start = pos;
            while pos < bytes.len()
                && (bytes[pos].is_ascii_alphanumeric() || bytes[pos] == b'_' || bytes[pos] == b'.')
            {
                pos += 1;
            }
            return Some((&bytes[start..pos], pos));
        }
        Some((&bytes[pos..pos + 1], pos + 1))
    }

    fn classify_table_ident(raw: &[u8]) -> TableFamily {
        // Strip optional schema prefix (e.g. `main.items`).
        let ident = match raw.iter().rposition(|&b| b == b'.') {
            Some(i) => &raw[i + 1..],
            None => raw,
        };
        let mut buf = [0u8; 40];
        let n = ident.len().min(buf.len());
        for (dst, &src) in buf[..n].iter_mut().zip(&ident[..n]) {
            *dst = src.to_ascii_lowercase();
        }
        let s = std::str::from_utf8(&buf[..n]).unwrap_or("");
        if s == "items"
            || s.starts_with("item_")
            || s.starts_with("work_")
            || s.starts_with("deliver")
            || s == "operations"
        {
            TableFamily::Items
        } else if s == "sessions"
            || s.starts_with("session_")
            || s == "events"
            || s.starts_with("event_")
            || s == "queued"
            || s.starts_with("runtime_")
            || s.starts_with("node_turn")
            || s.starts_with("node_session")
            || s == "turns"
            || s == "attempts"
        {
            TableFamily::Sessions
        } else if s == "decisions" || s.starts_with("decision_") || s.starts_with("approval") {
            TableFamily::Decisions
        } else if s == "messages"
            || s.starts_with("message_")
            || s.starts_with("thread_")
            || s.starts_with("chan_")
            || s.starts_with("channel_")
        {
            TableFamily::Messages
        } else if s.starts_with("mail_") || s == "mail" {
            TableFamily::Mail
        } else if s.starts_with("calendar") || s.starts_with("cal_") {
            TableFamily::Calendar
        } else if s == "flows"
            || s.starts_with("flow_")
            || s == "schedules"
            || s == "alarms"
            || s == "time_alarms"
            || s == "triggers"
            || s == "runs"
            || s.starts_with("run_")
        {
            TableFamily::Flows
        } else if s.starts_with("kb_")
            || s == "mems"
            || s.starts_with("mem_")
            || s == "bot_memories"
            || s.starts_with("memory_")
            || s == "files"
        {
            TableFamily::Knowledge
        } else if s == "users"
            || s.starts_with("auth_")
            || s == "passkeys"
            || s == "parties"
            || s.starts_with("party_")
            || s.starts_with("contact")
            || s == "accounts"
            || s.starts_with("account_")
            || s == "teams"
            || s.starts_with("team_")
            || s == "stewards"
            || s.starts_with("merge_")
            || s.starts_with("attr")
            || s == "personas"
        {
            TableFamily::People
        } else if s == "workspaces"
            || s.starts_with("workspace_")
            || s == "settings"
            || s == "bots"
            || s.starts_with("bot_")
            || s == "projects"
            || s.starts_with("project_")
            || s == "tags"
            || s.starts_with("tag_")
            || s == "changesets"
            || s == "connections"
            || s.starts_with("connection_")
            || s == "sources"
            || s == "match_rules"
            || s == "revisions"
            || s == "systems"
            || s == "links"
        {
            TableFamily::Config
        } else if s == "nodes"
            || s.starts_with("node_")
            || s == "devices"
            || s.starts_with("device_")
            || s == "capabilities"
            || s.starts_with("caps_")
        {
            TableFamily::Nodes
        } else if s.starts_with("vault_")
            || s == "secrets"
            || s.starts_with("secret_")
            || s == "grants"
            || s.starts_with("grant_")
        {
            TableFamily::Vault
        } else if s.starts_with("github_") || s.starts_with("gh_") {
            TableFamily::Github
        } else if s.starts_with("stats_")
            || s.starts_with("telemetry_")
            || s.starts_with("usage_")
            || s.starts_with("price_")
            || s.starts_with("digest_")
            || s == "signals"
            || s.starts_with("signal_")
            || s == "notifications"
            || s.starts_with("audit")
            || s.starts_with("outbox")
            || s.starts_with("diagnostic")
        {
            TableFamily::Stats
        } else {
            TableFamily::Other
        }
    }

    pub(crate) fn table_family(sql: &str) -> TableFamily {
        let bytes = sql.as_bytes();
        let Some((first, mut pos)) = next_sql_token(bytes, 0) else {
            return TableFamily::Other;
        };
        if first.eq_ignore_ascii_case(b"BEGIN")
            || first.eq_ignore_ascii_case(b"COMMIT")
            || first.eq_ignore_ascii_case(b"END")
            || first.eq_ignore_ascii_case(b"ROLLBACK")
            || first.eq_ignore_ascii_case(b"SAVEPOINT")
            || first.eq_ignore_ascii_case(b"RELEASE")
        {
            return TableFamily::Tx;
        }
        if first.eq_ignore_ascii_case(b"INSERT") || first.eq_ignore_ascii_case(b"REPLACE") {
            while let Some((tok, next)) = next_sql_token(bytes, pos) {
                pos = next;
                if tok.eq_ignore_ascii_case(b"INTO") {
                    return next_sql_token(bytes, pos)
                        .map_or(TableFamily::Other, |(t, _)| classify_table_ident(t));
                }
            }
            return TableFamily::Other;
        }
        if first.eq_ignore_ascii_case(b"UPDATE") {
            if let Some((tok, next)) = next_sql_token(bytes, pos) {
                if tok.eq_ignore_ascii_case(b"OR") {
                    // UPDATE OR IGNORE/REPLACE/ABORT/FAIL/ROLLBACK <table>
                    if let Some((_, after_action)) = next_sql_token(bytes, next) {
                        return next_sql_token(bytes, after_action)
                            .map_or(TableFamily::Other, |(t, _)| classify_table_ident(t));
                    }
                    return TableFamily::Other;
                }
                return classify_table_ident(tok);
            }
            return TableFamily::Other;
        }
        if first.eq_ignore_ascii_case(b"DELETE")
            || first.eq_ignore_ascii_case(b"SELECT")
            || first.eq_ignore_ascii_case(b"WITH")
        {
            // Scan a bounded number of tokens to avoid O(N) scanning huge SQL payloads.
            for _ in 0..64 {
                let Some((tok, next)) = next_sql_token(bytes, pos) else {
                    break;
                };
                pos = next;
                if tok.eq_ignore_ascii_case(b"FROM") {
                    if let Some((table, _)) = next_sql_token(bytes, pos)
                        && table != b"("
                    {
                        return classify_table_ident(table);
                    }
                }
            }
        }
        TableFamily::Other
    }

    struct QueryHandles {
        ok: metrics::Counter,
        err: metrics::Counter,
        duration: metrics::Histogram,
    }

    /// Zero-allocation boundary meter for `Backend::{query, exec, batch}`.
    pub(crate) struct QueryMeter {
        op: SqlOp,
        family: TableFamily,
        started: std::time::Instant,
    }

    impl QueryMeter {
        #[inline]
        pub(crate) fn start(op: SqlOp, sql: &str) -> Option<Self> {
            if !LOCK_METRICS.load(std::sync::atomic::Ordering::Acquire) {
                return None;
            }
            Some(Self {
                op,
                family: table_family(sql),
                started: std::time::Instant::now(),
            })
        }

        #[inline]
        pub(crate) fn finish(self, ok: bool) {
            let elapsed = self.started.elapsed().as_secs_f64();
            let op = self.op;
            let family = self.family;
            let idx = (op as usize) * TableFamily::COUNT + (family as usize);
            thread_local! {
                static SLOTS: [std::cell::OnceCell<QueryHandles>; SqlOp::COUNT * TableFamily::COUNT] =
                    [const { std::cell::OnceCell::new() }; SqlOp::COUNT * TableFamily::COUNT];
            }
            SLOTS.with(|slots| {
                let handles = slots[idx].get_or_init(|| {
                    let op_s = op.as_str();
                    let fam_s = family.as_str();
                    QueryHandles {
                        ok: metrics::counter!("steve_store_queries_total", "op" => op_s, "family" => fam_s, "result" => "ok"),
                        err: metrics::counter!("steve_store_queries_total", "op" => op_s, "family" => fam_s, "result" => "err"),
                        duration: metrics::histogram!("steve_store_query_duration_seconds", "op" => op_s, "family" => fam_s),
                    }
                });
                if ok {
                    handles.ok.increment(1);
                } else {
                    handles.err.increment(1);
                }
                handles.duration.record(elapsed);
            });
        }
    }

    struct ReadPoolHandles {
        wait: metrics::Histogram,
        ok: metrics::Counter,
        err: metrics::Counter,
        active: metrics::Gauge,
    }

    fn with_read_pool_handles<R>(f: impl FnOnce(&ReadPoolHandles) -> R) -> R {
        thread_local! {
            static HANDLES: std::cell::OnceCell<ReadPoolHandles> = const { std::cell::OnceCell::new() };
        }
        HANDLES.with(|cell| {
            let handles = cell.get_or_init(|| ReadPoolHandles {
                wait: metrics::histogram!("steve_store_read_pool_wait_seconds"),
                ok: metrics::counter!("steve_store_read_pool_acquisitions_total", "result" => "ok"),
                err: metrics::counter!("steve_store_read_pool_acquisitions_total", "result" => "err"),
                active: metrics::gauge!("steve_store_read_pool_active"),
            });
            f(handles)
        })
    }

    /// Keeps `steve_store_read_pool_active` accurate across the pooled connection's lifetime.
    pub(crate) struct ReadPoolActiveGuard {
        active: metrics::Gauge,
    }

    impl Drop for ReadPoolActiveGuard {
        fn drop(&mut self) {
            self.active.decrement(1.0);
        }
    }

    pub(crate) struct ReadPoolWait {
        started: std::time::Instant,
    }

    impl ReadPoolWait {
        #[inline]
        pub(crate) fn start() -> Option<Self> {
            if !LOCK_METRICS.load(std::sync::atomic::Ordering::Acquire) {
                return None;
            }
            Some(Self {
                started: std::time::Instant::now(),
            })
        }

        #[inline]
        pub(crate) fn acquired(self) -> ReadPoolActiveGuard {
            let elapsed = self.started.elapsed().as_secs_f64();
            with_read_pool_handles(|h| {
                h.wait.record(elapsed);
                h.ok.increment(1);
                h.active.increment(1.0);
                ReadPoolActiveGuard {
                    active: h.active.clone(),
                }
            })
        }

        #[inline]
        pub(crate) fn failed(self) {
            let elapsed = self.started.elapsed().as_secs_f64();
            with_read_pool_handles(|h| {
                h.wait.record(elapsed);
                h.err.increment(1);
            });
        }
    }

    #[derive(Debug, Clone, Copy, PartialEq, Eq)]
    #[repr(usize)]
    pub(crate) enum TxKind {
        Root = 0,
        Savepoint = 1,
    }

    impl TxKind {
        const COUNT: usize = 2;
        const fn as_str(self) -> &'static str {
            match self {
                Self::Root => "root",
                Self::Savepoint => "savepoint",
            }
        }
    }

    struct TxHandles {
        commit_count: metrics::Counter,
        rollback_count: metrics::Counter,
        commit_duration: metrics::Histogram,
        rollback_duration: metrics::Histogram,
    }

    /// Tracks transaction duration and commit/rollback outcome for root transactions and savepoints.
    pub(crate) struct TxMeter {
        kind: TxKind,
        started: std::time::Instant,
        committed: bool,
    }

    impl TxMeter {
        #[inline]
        pub(crate) fn start(kind: TxKind) -> Option<Self> {
            transaction_started();
            if !LOCK_METRICS.load(std::sync::atomic::Ordering::Acquire) {
                return None;
            }
            Some(Self {
                kind,
                started: std::time::Instant::now(),
                committed: false,
            })
        }

        #[inline]
        pub(crate) fn commit(mut self) {
            self.committed = true;
            // Drop records commit outcome and duration.
        }
    }

    impl Drop for TxMeter {
        fn drop(&mut self) {
            let elapsed = self.started.elapsed().as_secs_f64();
            let kind = self.kind;
            let committed = self.committed;
            thread_local! {
                static SLOTS: [std::cell::OnceCell<TxHandles>; TxKind::COUNT] =
                    [const { std::cell::OnceCell::new() }; TxKind::COUNT];
            }
            SLOTS.with(|slots| {
                let h = slots[kind as usize].get_or_init(|| {
                    let k = kind.as_str();
                    TxHandles {
                        commit_count: metrics::counter!("steve_store_transaction_outcomes_total", "kind" => k, "outcome" => "commit"),
                        rollback_count: metrics::counter!("steve_store_transaction_outcomes_total", "kind" => k, "outcome" => "rollback"),
                        commit_duration: metrics::histogram!("steve_store_transaction_duration_seconds", "kind" => k, "outcome" => "commit"),
                        rollback_duration: metrics::histogram!("steve_store_transaction_duration_seconds", "kind" => k, "outcome" => "rollback"),
                    }
                });
                if committed {
                    h.commit_count.increment(1);
                    h.commit_duration.record(elapsed);
                } else {
                    h.rollback_count.increment(1);
                    h.rollback_duration.record(elapsed);
                }
            });
        }
    }

    /// Bounded set of built-in harness IDs registered at Hub startup from the single adapter catalog
    /// (`steve-harness-spec`); unknown or custom harnesses fold to `custom`.
    static KNOWN_HARNESSES: std::sync::OnceLock<Vec<&'static str>> = std::sync::OnceLock::new();

    /// Registers the bounded set of known harness identifiers so `steve_model_*` labels stay bounded
    /// without duplicating adapter names outside the adapter layer.
    pub fn register_known_harnesses(ids: impl IntoIterator<Item = &'static str>) {
        let _ = KNOWN_HARNESSES.set(ids.into_iter().collect());
    }

    /// Bounded built-in model catalog for metric labels; arbitrary user strings fold to `custom`.
    const BUILTIN_MODELS: &[&str] = &[
        "claude-opus-5-5",
        "claude-sonnet-5-5",
        "claude-haiku-5-5",
        "claude-opus-4-6",
        "claude-sonnet-4-6",
        "claude-opus-4-5",
        "claude-sonnet-4-5",
        "claude-haiku-4-5",
        "claude-opus-4-1",
        "claude-sonnet-4",
        "claude-3-7-sonnet",
        "claude-3-5-sonnet",
        "claude-3-5-haiku",
        "gpt-6.1-sol",
        "gpt-6-sol",
        "gpt-6-luna",
        "gpt-5.6-luna",
        "gpt-5.4",
        "gpt-5",
        "gpt-5-mini",
        "gpt-5-nano",
        "gpt-4.1",
        "gpt-4.1-mini",
        "gpt-4o",
        "gpt-4o-mini",
        "o1",
        "o3",
        "o3-mini",
        "o4-mini",
        "gemini-3-pro",
        "gemini-2.5-pro",
        "gemini-2.5-flash",
        "gemini-2.5-flash-lite",
        "gemini-2.0-flash",
        "mistral-medium-3.5",
        "codestral",
        "devstral",
        "deepseek-r1",
        "deepseek-v3",
        "deepseek-chat",
        "deepseek-reasoner",
        "qwen3-coder",
        "qwen-2.5-coder",
        "kimi-k2",
        "glm-4.5",
        "grok-4",
        "grok-3",
        "minimax-m1",
    ];

    /// Normalizes a harness identifier to a bounded static label without allocating.
    pub fn normalize_harness(raw: &str) -> &'static str {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return "unknown";
        }
        KNOWN_HARNESSES
            .get()
            .and_then(|ids| {
                ids.iter()
                    .copied()
                    .find(|&h| h.eq_ignore_ascii_case(trimmed))
            })
            .unwrap_or("custom")
    }

    /// Normalizes a model identifier against the built-in catalog without allocating.
    /// Strips optional `<provider>/` route prefixes, `[…]` context suffixes, and trailing
    /// `-YYYYMMDD` date stamps; unrecognized values fold to `custom`.
    pub fn normalize_model(raw: &str) -> &'static str {
        let mut s = raw.trim();
        if s.is_empty() {
            return "unknown";
        }
        if let Some((_, rest)) = s.split_once('/') {
            s = rest.trim();
        }
        if let Some(i) = s.find('[')
            && s.ends_with(']')
        {
            s = s[..i].trim_end();
        }
        if s.is_empty() {
            return "unknown";
        }
        if let Some(&known) = BUILTIN_MODELS.iter().find(|&&m| m.eq_ignore_ascii_case(s)) {
            return known;
        }
        // Strip trailing `-YYYYMMDD` date stamp (9 ASCII bytes) if present.
        if s.len() > 9 {
            let (head, tail) = s.split_at(s.len() - 9);
            if tail.as_bytes()[0] == b'-' && tail.as_bytes()[1..].iter().all(u8::is_ascii_digit) {
                if let Some(&known) = BUILTIN_MODELS
                    .iter()
                    .find(|&&m| m.eq_ignore_ascii_case(head))
                {
                    return known;
                }
            }
        }
        "custom"
    }

    /// One finished agent turn or gateway call (`steve_model_*`), recorded once it committed: by
    /// the coordinator after an attempt's end, by the gateway after its call's finish.
    #[derive(Debug, Clone, Default)]
    pub struct ModelCall {
        /// `agent` or `gateway`.
        pub source: &'static str,
        pub harness: String,
        /// The model booked when a usage row names none.
        pub model: String,
        /// `ok`, or a reason code.
        pub result: &'static str,
        pub seconds: f64,
        pub usage: Vec<crate::ModelUsage>,
        /// Peak prompt context tokens observed during an agent turn, when reported.
        pub context_peak: Option<u64>,
    }

    impl ModelCall {
        pub fn record(&self) {
            let source = self.source;
            let harness = normalize_harness(&self.harness);
            let call_model = normalize_model(&self.model);
            let result = self.result;

            type CallKey = (&'static str, &'static str, &'static str, &'static str);
            type DurKey = (&'static str, &'static str);
            type TokKey = (&'static str, &'static str, &'static str, &'static str);
            type CostKey = (&'static str, &'static str, &'static str);

            #[derive(Default)]
            struct ModelCache {
                calls: std::collections::HashMap<CallKey, metrics::Counter>,
                durations: std::collections::HashMap<DurKey, metrics::Histogram>,
                context_peaks: std::collections::HashMap<&'static str, metrics::Histogram>,
                tokens: std::collections::HashMap<TokKey, metrics::Counter>,
                costs: std::collections::HashMap<CostKey, metrics::Counter>,
            }

            thread_local! {
                static CACHE: std::cell::RefCell<ModelCache> =
                    std::cell::RefCell::new(ModelCache::default());
            }

            CACHE.with(|cell| {
                let mut cache = cell.borrow_mut();
                cache
                    .calls
                    .entry((source, harness, call_model, result))
                    .or_insert_with(|| {
                        metrics::counter!(
                            "steve_model_calls_total",
                            "source" => source,
                            "harness" => harness,
                            "model" => call_model,
                            "result" => result,
                        )
                    })
                    .increment(1);
                cache
                    .durations
                    .entry((source, harness))
                    .or_insert_with(|| {
                        metrics::histogram!(
                            "steve_model_call_seconds",
                            "source" => source,
                            "harness" => harness,
                        )
                    })
                    .record(self.seconds);
                if source == "agent"
                    && let Some(peak) = self.context_peak.filter(|&n| n > 0)
                {
                    cache
                        .context_peaks
                        .entry(harness)
                        .or_insert_with(|| {
                            metrics::histogram!(
                                "steve_turn_context_peak_tokens",
                                "harness" => harness,
                            )
                        })
                        .record(peak as f64);
                }
                for u in &self.usage {
                    let model = if u.model.is_empty() {
                        call_model
                    } else {
                        normalize_model(&u.model)
                    };
                    let t = &u.tokens;
                    for (kind, n) in [
                        ("input", t.input),
                        ("output", t.output),
                        ("cache_read", t.cache_read),
                        ("cache_write", t.cache_write),
                    ] {
                        if let Some(n) = n.and_then(|n| u64::try_from(n).ok()).filter(|n| *n > 0) {
                            cache
                                .tokens
                                .entry((source, harness, model, kind))
                                .or_insert_with(|| {
                                    metrics::counter!(
                                        "steve_model_tokens_total",
                                        "source" => source,
                                        "harness" => harness,
                                        "model" => model,
                                        "kind" => kind,
                                    )
                                })
                                .increment(n);
                        }
                    }
                    if let Some(usd) = u.cost_reported.filter(|c| c.is_finite() && *c > 0.0) {
                        cache
                            .costs
                            .entry((source, harness, model))
                            .or_insert_with(|| {
                                metrics::counter!(
                                    "steve_model_cost_micro_usd_total",
                                    "source" => source,
                                    "harness" => harness,
                                    "model" => model,
                                )
                            })
                            .increment((usd * 1e6).round() as u64);
                    }
                }
            });
        }
    }

    /// Shared by the store and core lock boundaries. Handles are registered once per source location
    /// and thread. Only that cold registration formats the bounded source label; repeated acquisitions
    /// clone handles without allocating strings or taking another application lock.
    pub struct LockTiming {
        since: std::time::Instant,
        wait: metrics::Histogram,
        hold: metrics::Histogram,
        count: metrics::Counter,
    }

    impl LockTiming {
        pub fn start(
            kind: &'static str,
            caller: &'static std::panic::Location<'static>,
        ) -> Option<Self> {
            if !LOCK_METRICS.load(std::sync::atomic::Ordering::Acquire) {
                return None;
            }
            type Handles = (metrics::Histogram, metrics::Histogram, metrics::Counter);
            thread_local! {
                static HANDLES: std::cell::RefCell<std::collections::HashMap<
                    (&'static str, &'static std::panic::Location<'static>), Handles
                >> = std::cell::RefCell::new(std::collections::HashMap::new());
            }
            let (wait, hold, count) = HANDLES.with(|cache| {
                cache
                    .borrow_mut()
                    .entry((kind, caller))
                    .or_insert_with(|| {
                        let site = format!("{}:{}", caller.file(), caller.line());
                        (
                            metrics::histogram!("steve_lock_wait_seconds", "lock" => kind, "caller" => site.clone()),
                            metrics::histogram!("steve_lock_hold_seconds", "lock" => kind, "caller" => site.clone()),
                            metrics::counter!("steve_lock_acquisitions_total", "lock" => kind, "caller" => site),
                        )
                    })
                    .clone()
            });
            Some(Self {
                since: std::time::Instant::now(),
                wait,
                hold,
                count,
            })
        }

        pub fn acquired(mut self) -> Self {
            self.wait.record(self.since.elapsed().as_secs_f64());
            self.count.increment(1);
            self.since = std::time::Instant::now();
            self
        }
    }

    impl Drop for LockTiming {
        fn drop(&mut self) {
            self.hold.record(self.since.elapsed().as_secs_f64());
        }
    }
    #[cfg(test)]
    mod tests {
        use super::*;
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };

        struct Sink {
            registrations: AtomicUsize,
            samples: Arc<AtomicUsize>,
        }
        struct Count(Arc<AtomicUsize>);
        impl metrics::CounterFn for Count {
            fn increment(&self, value: u64) {
                self.0.fetch_add(value as usize, Ordering::Relaxed);
            }
            fn absolute(&self, value: u64) {
                self.0.store(value as usize, Ordering::Relaxed);
            }
        }
        impl metrics::Recorder for Sink {
            fn describe_counter(
                &self,
                _: metrics::KeyName,
                _: Option<metrics::Unit>,
                _: metrics::SharedString,
            ) {
            }
            fn describe_gauge(
                &self,
                _: metrics::KeyName,
                _: Option<metrics::Unit>,
                _: metrics::SharedString,
            ) {
            }
            fn describe_histogram(
                &self,
                _: metrics::KeyName,
                _: Option<metrics::Unit>,
                _: metrics::SharedString,
            ) {
            }
            fn register_counter(
                &self,
                key: &metrics::Key,
                _: &metrics::Metadata<'_>,
            ) -> metrics::Counter {
                assert_eq!(key.name(), "steve_store_transactions_total");
                self.registrations.fetch_add(1, Ordering::Relaxed);
                metrics::Counter::from_arc(Arc::new(Count(self.samples.clone())))
            }
            fn register_gauge(
                &self,
                _: &metrics::Key,
                _: &metrics::Metadata<'_>,
            ) -> metrics::Gauge {
                metrics::Gauge::noop()
            }
            fn register_histogram(
                &self,
                _: &metrics::Key,
                _: &metrics::Metadata<'_>,
            ) -> metrics::Histogram {
                metrics::Histogram::noop()
            }
        }

        #[test]
        fn transactions_are_disabled_until_enabled_and_register_once_per_thread() {
            std::thread::spawn(|| {
                let sink = Sink {
                    registrations: AtomicUsize::new(0),
                    samples: Arc::new(AtomicUsize::new(0)),
                };
                metrics::with_local_recorder(&sink, || {
                    transaction_started();
                    assert_eq!(sink.registrations.load(Ordering::Relaxed), 0);
                    enable_lock_metrics();
                    for _ in 0..3 {
                        transaction_started();
                    }
                    assert_eq!(sink.registrations.load(Ordering::Relaxed), 1);
                    assert_eq!(sink.samples.load(Ordering::Relaxed), 3);
                });
            })
            .join()
            .unwrap();
        }

        #[test]
        fn sql_table_family_and_model_labels_are_bounded_without_sql_or_raw_strings() {
            assert_eq!(
                table_family("SELECT id FROM items WHERE workspace_id = ?"),
                TableFamily::Items
            );
            assert_eq!(
                table_family("/* comment */ INSERT OR IGNORE INTO \"events\" (id) VALUES (?)"),
                TableFamily::Sessions
            );
            assert_eq!(
                table_family("UPDATE mail_drafts SET status = ? WHERE id = ?"),
                TableFamily::Mail
            );
            assert_eq!(
                table_family("DELETE FROM flow_waits WHERE id = ?"),
                TableFamily::Flows
            );
            assert_eq!(table_family("BEGIN IMMEDIATE"), TableFamily::Tx);
            assert_eq!(table_family("SAVEPOINT steve_sp_42"), TableFamily::Tx);
            assert_eq!(table_family("PRAGMA optimize"), TableFamily::Other);

            register_known_harnesses(["alpha-harness", "beta-harness"]);
            assert_eq!(normalize_harness("alpha-harness"), "alpha-harness");
            assert_eq!(normalize_harness("BETA-HARNESS"), "beta-harness");
            assert_eq!(normalize_harness("my-private-harness"), "custom");
            assert_eq!(normalize_harness("  "), "unknown");

            assert_eq!(normalize_model("claude-opus-5-5"), "claude-opus-5-5");
            assert_eq!(
                normalize_model("kiro/claude-sonnet-4-5-20250929[1m]"),
                "claude-sonnet-4-5"
            );
            assert_eq!(normalize_model("passthrough/gpt-6.1-sol"), "gpt-6.1-sol");
            assert_eq!(
                normalize_model(&"user-typed-unbounded-model-".repeat(7)),
                "custom"
            );
            assert_eq!(normalize_model(""), "unknown");
        }
    }
}

use instrumentation::LockTiming;

pub(crate) struct WriteGuard<'a> {
    conn: MutexGuard<'a, Connection>,
    timing: Option<LockTiming>,
}

impl WriteGuard<'_> {
    pub(crate) fn connection(&self) -> &Connection {
        &self.conn
    }
}

impl Drop for WriteGuard<'_> {
    fn drop(&mut self) {
        drop(self.timing.take());
    }
}

/// Bounded retention slices on the store's writer connection; scheduling belongs to the Hub.
pub mod retention {
    use super::{Result, Store};
    pub use steve_store_sqlite::retention::{
        Budget, DEFAULT_BUDGET, SliceReport,
        policies::{ALL, Kind},
    };
    impl Store {
        /// Exactly one writer-grant lifetime. The scheduler invokes this separately for each slice.
        pub fn retention_slice(
            &self,
            kind: Kind,
            now: i64,
            budget: Budget,
            after: Option<i64>,
        ) -> Result<SliceReport<i64>> {
            let policy = kind.policy();
            self.write_locked(|c| {
                Ok(steve_store_sqlite::retention::run_slice(
                    c,
                    policy.as_ref(),
                    now,
                    budget,
                    after.as_ref(),
                )?)
            })
        }
        pub(crate) fn retention_payload_slice<G>(
            &self,
            session: &str,
            upper: i64,
            now: i64,
            budget: Budget,
            after: Option<i64>,
            guard: &mut impl FnMut() -> Option<G>,
        ) -> Result<Option<SliceReport<i64>>> {
            use std::{cell::RefCell, collections::BTreeMap};
            use steve_store_sqlite::retention as r;
            let cache = RefCell::new(BTreeMap::new());
            let action = |c: &rusqlite::Connection, row: &r::Row| {
                let id = r::policies::integer(row, "rowid")?;
                self.apply_retention_event(
                    c,
                    cache
                        .borrow()
                        .get(&id)
                        .ok_or_else(|| r::invalid("missing prepared event"))?,
                )
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))
            };
            let policy = r::policies::SessionEvents::new(session, upper, &action);
            let prepared = self.sqlite_read(|c| {
                super::read_snapshot(c, |snapshot| {
                    let candidates =
                        r::before_candidates(snapshot, &policy, now, budget, after.as_ref())?;
                    Ok(r::prepare(&policy, candidates, |row| {
                        let event = self
                            .prepare_retention_event(snapshot, row)
                            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
                        cache
                            .borrow_mut()
                            .insert(r::policies::integer(row, "rowid")?, event);
                        Ok(true)
                    }))
                })
            })?;
            let Some(_guard) = guard() else {
                return Ok(None);
            };
            let report = self
                .write_locked(|c| Ok(r::run_slice(c, &prepared, now, budget, after.as_ref())?))?;
            Ok(Some(report))
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;
        use rusqlite::hooks::{AuthAction, AuthContext, Authorization, TransactionOperation};
        use std::sync::{
            Arc,
            atomic::{AtomicUsize, Ordering},
        };

        #[test]
        fn payload_page_uses_one_read_snapshot_for_all_candidates() {
            let dir = tempfile::tempdir().unwrap();
            let store = Store::open(dir.path().join("steve.sqlite3"), crate::NoPublisher).unwrap();
            store.ensure_workspace().unwrap();
            let session = store
                .sessions()
                .create(&crate::NewSession {
                    workspace_id: crate::WS.into(),
                    harness: "fixture".into(),
                    cwd: "/tmp".into(),
                    ..Default::default()
                })
                .unwrap();
            for _ in 0..3 {
                store.events().add(&session.id,"user",serde_json::json!({"text":"screenshot","images":[{"type":"image","mimeType":"image/png","data":"QUJD".repeat(3000)}]})).unwrap();
            }
            let upto = store.events().last_seq(&session.id).unwrap();
            let snapshots = Arc::new(AtomicUsize::new(0));
            // Instrument every pooled reader, including readers the page may check out later.
            let readers = (0..store.inner.readers.max_size())
                .map(|_| store.inner.readers.get().unwrap())
                .collect::<Vec<_>>();
            for reader in &readers {
                let count = snapshots.clone();
                reader
                    .authorizer(Some(move |context: AuthContext<'_>| {
                        if matches!(
                            context.action,
                            AuthAction::Transaction {
                                operation: TransactionOperation::Begin
                            }
                        ) {
                            count.fetch_add(1, Ordering::SeqCst);
                        }
                        Authorization::Allow
                    }))
                    .unwrap();
            }
            drop(readers);
            let report = store
                .retention_payload_slice(
                    &session.id,
                    upto,
                    crate::now(),
                    DEFAULT_BUDGET,
                    None,
                    &mut || Some(()),
                )
                .unwrap()
                .unwrap();
            assert_eq!((report.affected, report.failures), (3, 0));
            assert_eq!(
                snapshots.load(Ordering::SeqCst),
                1,
                "candidate selection and all hydration share a page snapshot"
            );
        }
    }
}

#[cfg(feature = "testkit")]
pub mod testkit;

pub use contract::bot_admin;
pub use contract::deliveries as delivery_repo;
pub use contract::node_jobs;
pub use contract::node_write;
pub use contract::state_batches;
pub use contract::{tool_admission, tool_journal};
