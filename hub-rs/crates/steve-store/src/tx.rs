//! Public transactions for module crates and re-entrant atomic write scopes for domain workflows.

use std::cell::{Cell, RefCell};
use std::ops::Deref;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU64, Ordering};

use rusqlite::Connection;

use crate::contract::{Backend, Sqlite};
use crate::{BusEvent, Store, StoreError};

static NEXT_SAVEPOINT_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Default)]
struct Staged {
    events: Vec<BusEvent>,
    diagnostics: Vec<crate::diagnostics::Record>,
    diagnostic_until: i64,
    outbox: bool,
}

impl Staged {
    fn merge(&mut self, child: Staged) {
        self.events.extend(child.events);
        self.diagnostics.extend(child.diagnostics);
        self.diagnostic_until = self.diagnostic_until.max(child.diagnostic_until);
        self.outbox |= child.outbox;
    }
}

struct ActiveWrite {
    conn: NonNull<Connection>,
    frames: Vec<Staged>,
}

thread_local! {
    static ACTIVE_WRITES: RefCell<Vec<(usize, ActiveWrite)>> = const { RefCell::new(Vec::new()) };
}

pub(crate) struct SavepointTx<'a> {
    c: &'a Connection,
    name: String,
    committed: bool,
}

impl Drop for SavepointTx<'_> {
    fn drop(&mut self) {
        if !self.committed {
            let _ = self.c.execute_batch(&format!(
                "ROLLBACK TO SAVEPOINT {0}; RELEASE SAVEPOINT {0}",
                self.name
            ));
        }
    }
}

/// Either an outer `BEGIN IMMEDIATE` transaction or a nested SQLite `SAVEPOINT` when `c` is
/// already inside an open transaction on this connection.
pub(crate) enum WriteTx<'a> {
    Root(rusqlite::Transaction<'a>),
    Savepoint(SavepointTx<'a>),
}

impl<'a> WriteTx<'a> {
    pub(crate) fn begin(c: &'a Connection) -> rusqlite::Result<Self> {
        crate::instrumentation::transaction_started();
        if c.is_autocommit() {
            Ok(Self::Root(crate::immediate(c)?))
        } else {
            let id = NEXT_SAVEPOINT_ID.fetch_add(1, Ordering::Relaxed);
            let name = format!("steve_sp_{id}");
            c.execute_batch(&format!("SAVEPOINT {name}"))?;
            Ok(Self::Savepoint(SavepointTx {
                c,
                name,
                committed: false,
            }))
        }
    }

    pub(crate) fn commit(self) -> rusqlite::Result<()> {
        match self {
            Self::Root(tx) => tx.commit(),
            Self::Savepoint(mut sp) => {
                sp.c.execute_batch(&format!("RELEASE SAVEPOINT {}", sp.name))?;
                sp.committed = true;
                Ok(())
            }
        }
    }
}

impl Deref for WriteTx<'_> {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        match self {
            Self::Root(tx) => tx,
            Self::Savepoint(sp) => sp.c,
        }
    }
}

struct RootScopeGuard<'c> {
    store_id: usize,
    conn: &'c Connection,
}

impl Drop for RootScopeGuard<'_> {
    fn drop(&mut self) {
        // not committed (an error or a panic in the write): nothing of it stays
        if !self.conn.is_autocommit() {
            let _ = self.conn.execute_batch("ROLLBACK");
        }
        let store_id = self.store_id;
        ACTIVE_WRITES.with(|cell| {
            let mut list = cell.borrow_mut();
            if let Some(pos) = list.iter().position(|(id, _)| *id == store_id) {
                list.swap_remove(pos);
            }
        });
    }
}

/// An open nested scope ([`Store::open_scope`]).
pub(crate) struct Scope {
    name: String,
    frame: NestedFrameGuard,
}

struct NestedFrameGuard {
    store_id: usize,
    armed: bool,
}

impl NestedFrameGuard {
    fn take(mut self) -> Staged {
        self.armed = false;
        let store_id = self.store_id;
        ACTIVE_WRITES.with(|cell| {
            let mut list = cell.borrow_mut();
            let (_, entry) = list
                .iter_mut()
                .find(|(id, _)| *id == store_id)
                .expect("active write entry must exist for nested frame");
            entry.frames.pop().expect("nested staged frame must exist")
        })
    }
}

impl Drop for NestedFrameGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let store_id = self.store_id;
        ACTIVE_WRITES.with(|cell| {
            let mut list = cell.borrow_mut();
            if let Some((_, entry)) = list.iter_mut().find(|(id, _)| *id == store_id) {
                let _ = entry.frames.pop();
            }
        });
    }
}

/// One write transaction. Derefs to the connection; events published through it are held back and go
/// out only after the commit, in order, so a rolled-back write never reaches a browser.
pub struct Tx<'a> {
    c: &'a Connection,
    store: Option<&'a Store>,
    events: RefCell<Vec<BusEvent>>,
    diagnostics: RefCell<Vec<crate::diagnostics::Record>>,
    /// New item capture windows become visible to producers only after commit.
    diagnostic_until: Cell<i64>,
    /// An outbox event was emitted: wake the dispatcher after the commit.
    outbox: Cell<bool>,
}

impl Tx<'_> {
    /// Queues an event for after the commit.
    pub fn publish(&self, e: impl Into<BusEvent>) {
        let e = e.into();
        if let Some(store) = self.store {
            if let Err(e) = store.try_stage_event(e) {
                self.events.borrow_mut().push(e);
            }
            return;
        }
        self.events.borrow_mut().push(e);
    }

    pub(crate) fn mark_outbox(&self) {
        if let Some(store) = self.store
            && store.try_stage_outbox()
        {
            return;
        }
        self.outbox.set(true);
    }

    /// Metadata is queued only after commit, never for a rolled-back business write.
    pub(crate) fn diagnostic(&self, event: crate::diagnostics::Record) {
        if let Some(store) = self.store {
            if let Err(event) = store.try_stage_diagnostic(event) {
                self.diagnostics.borrow_mut().push(event);
            }
            return;
        }
        self.diagnostics.borrow_mut().push(event);
    }

    pub(crate) fn capture_created(&self, ws: &str, item: &str, created: i64) {
        if let Some(until) = crate::diagnostics::capture_created(self.c, ws, item, created) {
            if let Some(store) = self.store
                && store.try_stage_diagnostic_until(until)
            {
                return;
            }
            self.diagnostic_until
                .set(self.diagnostic_until.get().max(until));
        }
    }
}

impl Tx<'_> {
    /// The storage contract's repositories in this transaction, as a scope of it
    /// ([`Store::write`] inside a write), for the domains not on the contract yet.
    pub fn write<T>(
        &self,
        f: impl FnOnce(&mut crate::W<'_>) -> crate::Result<T>,
    ) -> crate::Result<T> {
        match self.store {
            Some(store) => store.write(f),
            None => Err(StoreError::Hook(
                "a repository hook's transaction cannot open a contract write".into(),
            )),
        }
    }
}

impl<'a> Tx<'a> {
    /// Wraps a transaction the store already opened (for hooks inside a repository write).
    pub(crate) fn wrap(c: &'a Connection) -> Self {
        Tx {
            c,
            store: None,
            events: RefCell::new(Vec::new()),
            diagnostics: RefCell::new(Vec::new()),
            diagnostic_until: Cell::new(0),
            outbox: Cell::new(false),
        }
    }

    pub(crate) fn bound(c: &'a Connection, store: &'a Store) -> Self {
        Tx {
            c,
            store: Some(store),
            events: RefCell::new(Vec::new()),
            diagnostics: RefCell::new(Vec::new()),
            diagnostic_until: Cell::new(0),
            outbox: Cell::new(false),
        }
    }

    /// Detaches side effects collected on a wrapped `Tx` so `tx` can commit before flushing them.
    pub(crate) fn into_effects(self) -> WrappedEffects {
        WrappedEffects(Staged {
            events: self.events.into_inner(),
            diagnostics: self.diagnostics.into_inner(),
            diagnostic_until: self.diagnostic_until.get(),
            outbox: self.outbox.get(),
        })
    }
}

impl Deref for Tx<'_> {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        self.c
    }
}

impl Store {
    pub(crate) fn active_conn(&self) -> Option<&Connection> {
        let store_id = self.inner_id();
        let ptr = ACTIVE_WRITES.with(|cell| {
            let list = cell.borrow();
            list.iter()
                .find(|(id, entry)| *id == store_id && !entry.frames.is_empty())
                .map(|(_, entry)| entry.conn)
        })?;
        // SAFETY: `RootScopeGuard` keeps `ptr` live for the exact stack lifetime of the active
        // root `MutexGuard<Connection>` on this thread, and only shared `&Connection` references
        // are ever constructed.
        Some(unsafe { ptr.as_ref() })
    }

    pub(crate) fn try_stage_event(&self, e: BusEvent) -> Result<(), BusEvent> {
        let store_id = self.inner_id();
        ACTIVE_WRITES.with(|cell| {
            let mut list = cell.borrow_mut();
            if let Some((_, entry)) = list.iter_mut().find(|(id, _)| *id == store_id)
                && let Some(frame) = entry.frames.last_mut()
            {
                frame.events.push(e);
                return Ok(());
            }
            Err(e)
        })
    }

    pub(crate) fn emit_event(&self, e: BusEvent) {
        if let Err(e) = self.try_stage_event(e) {
            self.inner.bus.publish_batch(vec![e]);
        }
    }

    pub(crate) fn try_stage_diagnostic(
        &self,
        r: crate::diagnostics::Record,
    ) -> Result<(), crate::diagnostics::Record> {
        let store_id = self.inner_id();
        ACTIVE_WRITES.with(|cell| {
            let mut list = cell.borrow_mut();
            if let Some((_, entry)) = list.iter_mut().find(|(id, _)| *id == store_id)
                && let Some(frame) = entry.frames.last_mut()
            {
                frame.diagnostics.push(r);
                return Ok(());
            }
            Err(r)
        })
    }

    pub(crate) fn stage_diagnostic(&self, r: crate::diagnostics::Record) {
        if let Err(r) = self.try_stage_diagnostic(r) {
            let _ = self.diagnostics().record(r);
        }
    }

    pub(crate) fn try_stage_diagnostic_until(&self, until: i64) -> bool {
        let store_id = self.inner_id();
        ACTIVE_WRITES.with(|cell| {
            let mut list = cell.borrow_mut();
            if let Some((_, entry)) = list.iter_mut().find(|(id, _)| *id == store_id)
                && let Some(frame) = entry.frames.last_mut()
            {
                frame.diagnostic_until = frame.diagnostic_until.max(until);
                return true;
            }
            false
        })
    }

    pub(crate) fn stage_diagnostic_until(&self, until: i64) {
        if !self.try_stage_diagnostic_until(until) {
            self.diagnostics().activate_until(until);
        }
    }

    pub(crate) fn try_stage_outbox(&self) -> bool {
        let store_id = self.inner_id();
        ACTIVE_WRITES.with(|cell| {
            let mut list = cell.borrow_mut();
            if let Some((_, entry)) = list.iter_mut().find(|(id, _)| *id == store_id)
                && let Some(frame) = entry.frames.last_mut()
            {
                frame.outbox = true;
                return true;
            }
            false
        })
    }

    /// Opens a nested scope on the active write: a `SAVEPOINT` and a staging frame of its own.
    pub(crate) fn open_scope(&self, db: &mut dyn Backend) -> Result<Scope, StoreError> {
        crate::instrumentation::transaction_started();
        let name = format!(
            "steve_sp_{}",
            NEXT_SAVEPOINT_ID.fetch_add(1, Ordering::Relaxed)
        );
        db.batch(&format!("SAVEPOINT {name}"))?;
        let store_id = self.inner_id();
        ACTIVE_WRITES.with(|cell| {
            let mut list = cell.borrow_mut();
            let (_, entry) = list
                .iter_mut()
                .find(|(id, _)| *id == store_id)
                .expect("active write entry must exist");
            entry.frames.push(Staged::default());
        });
        Ok(Scope {
            name,
            frame: NestedFrameGuard {
                store_id,
                armed: true,
            },
        })
    }

    /// Closes a scope with what ran in it: kept (its staging merged into the enclosing scope's) when
    /// `out` is `Ok`, rolled back alone with everything it staged otherwise. A scope that cannot be
    /// closed fails as [`StoreError::Storage`], so the whole write rolls back.
    pub(crate) fn close_scope<T, E: From<StoreError>>(
        &self,
        db: &mut dyn Backend,
        scope: Scope,
        out: Result<T, E>,
    ) -> Result<T, E> {
        let Scope { name, frame } = scope;
        if out.is_ok() {
            if let Err(e) = db.batch(&format!("RELEASE SAVEPOINT {name}")) {
                let _ = db.batch(&format!(
                    "ROLLBACK TO SAVEPOINT {name}; RELEASE SAVEPOINT {name}"
                ));
                return Err(e.into());
            }
            let child = frame.take();
            let store_id = self.inner_id();
            ACTIVE_WRITES.with(|cell| {
                let mut list = cell.borrow_mut();
                let (_, entry) = list
                    .iter_mut()
                    .find(|(id, _)| *id == store_id)
                    .expect("active write entry must exist");
                entry
                    .frames
                    .last_mut()
                    .expect("parent staged frame must exist")
                    .merge(child);
            });
            return out;
        }
        drop(frame);
        db.batch(&format!(
            "ROLLBACK TO SAVEPOINT {name}; RELEASE SAVEPOINT {name}"
        ))
        .map_err(|e| StoreError::Storage(format!("rolling back a scope: {e}")))?;
        out
    }

    /// The outermost write: the writer lock, one transaction on the write connection (`f` gets it
    /// both as the legacy connection and as the contract's backend), committed if `f` returns `Ok`
    /// and rolled back otherwise (also on a panic). What was staged goes out after the commit.
    #[track_caller]
    pub(crate) fn root<T, E: From<StoreError>>(
        &self,
        f: impl FnOnce(&Connection, &mut dyn Backend) -> Result<T, E>,
    ) -> Result<T, E> {
        let store_id = self.inner_id();
        let c = self.lock();
        crate::instrumentation::transaction_started();
        let mut db = Sqlite(&c);
        db.begin_write()?;
        ACTIVE_WRITES.with(|cell| {
            cell.borrow_mut().push((
                store_id,
                ActiveWrite {
                    conn: NonNull::from(&*c),
                    frames: vec![Staged::default()],
                },
            ));
        });
        let root_guard = RootScopeGuard { store_id, conn: &c };
        let out = f(&c, &mut db)?;
        // on failure the guard rolls back
        db.batch("COMMIT")?;
        let staged = ACTIVE_WRITES.with(|cell| {
            let mut list = cell.borrow_mut();
            let pos = list
                .iter()
                .position(|(id, _)| *id == store_id)
                .expect("root write entry must exist");
            let (_, mut entry) = list.swap_remove(pos);
            entry.frames.pop().unwrap_or_default()
        });
        drop(root_guard);

        if staged.diagnostic_until > 0 {
            self.diagnostics().activate_until(staged.diagnostic_until);
        }
        for event in staged.diagnostics {
            self.diagnostics().record(event);
        }
        self.inner.bus.publish_batch(staged.events);
        self.notify_committed();
        if staged.outbox {
            self.inner.outbox_wake.notify_one();
        }
        Ok(out)
    }

    #[track_caller]
    fn atomic_with<T, E: From<StoreError>>(
        &self,
        f: impl FnOnce(&Connection) -> Result<T, E>,
    ) -> Result<T, E> {
        if let Some(c) = self.active_conn() {
            let mut db = Sqlite(c);
            let scope = self.open_scope(&mut db)?;
            let out = f(c);
            return self.close_scope(&mut db, scope, out);
        }
        self.root(|c, _| f(c))
    }

    /// Runs `f` inside an atomic, re-entrant SQLite write scope.
    ///
    /// All `store.x()` repository reads and writes (as well as nested [`Store::atomic`] and
    /// [`Store::transaction`] calls) inside `f` reuse the active write connection without
    /// deadlocking, see uncommitted writes immediately, and isolate nested scopes with SQLite
    /// `SAVEPOINT`s. `BusEvent`s, diagnostics, and outbox notifications are held back until the
    /// outermost transaction commits, and discarded on error or panic.
    #[track_caller]
    pub fn atomic<T, E: From<StoreError>>(&self, f: impl FnOnce() -> Result<T, E>) -> Result<T, E> {
        self.atomic_with(|_| f())
    }

    /// Runs `f` in one write transaction: committed if it returns `Ok`, rolled back otherwise.
    ///
    /// Re-entrant with [`Store::atomic`] and repository helpers; `f` must still avoid blocking on
    /// external network or subprocess I/O while holding the write lock.
    #[track_caller]
    pub fn transaction<T, E: From<StoreError>>(
        &self,
        f: impl FnOnce(&Tx) -> Result<T, E>,
    ) -> Result<T, E> {
        self.atomic_with(|c| {
            let t = Tx::bound(c, self);
            f(&t)
        })
    }

    /// Runs `f` on a pooled read-only connection (or the active write connection inside [`Store::atomic`]).
    pub fn reader<T, E: From<StoreError>>(
        &self,
        f: impl FnOnce(&Connection) -> Result<T, E>,
    ) -> Result<T, E> {
        if let Some(c) = self.active_conn() {
            return f(c);
        }
        let c = self.inner.readers.get().map_err(StoreError::from)?;
        crate::instrumentation::store_acquired();
        f(&c)
    }
}

pub(crate) struct WrappedEffects(Staged);

impl WrappedEffects {
    pub(crate) fn flush(self, store: &Store) {
        let s = self.0;
        if s.diagnostic_until > 0 {
            store.stage_diagnostic_until(s.diagnostic_until);
        }
        for d in s.diagnostics {
            store.stage_diagnostic(d);
        }
        let mut immediate = Vec::new();
        for e in s.events {
            if let Err(e) = store.try_stage_event(e) {
                immediate.push(e);
            }
        }
        if !immediate.is_empty() {
            store.inner.bus.publish_batch(immediate);
        }
        // Wrapped repository effects flush only after their transaction commits. Nested
        // effects wait for the outer write, which owns its notification.
        if store.active_conn().is_none() {
            store.notify_committed();
        }
        if s.outbox && !store.try_stage_outbox() {
            store.inner.outbox_wake.notify_one();
        }
    }
}
