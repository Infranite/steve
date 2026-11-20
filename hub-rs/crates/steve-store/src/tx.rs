//! Public transactions for module crates and re-entrant atomic write scopes for domain workflows.

use std::cell::{Cell, RefCell};
use std::ops::Deref;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU64, Ordering};

use rusqlite::Connection;

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
    fn merge(&mut self, child: Self) {
        self.events.extend(child.events);
        self.diagnostics.extend(child.diagnostics);
        self.diagnostic_until = self.diagnostic_until.max(child.diagnostic_until);
        self.outbox |= child.outbox;
    }
    fn flush(self, store: &Store) {
        if self.diagnostic_until > 0 {
            store.diagnostics().activate_until(self.diagnostic_until);
        }
        for diagnostic in self.diagnostics {
            store.diagnostics().record(diagnostic);
        }
        if !self.events.is_empty() {
            store.inner.bus.publish_batch(self.events);
        }
        if self.outbox {
            store.inner.outbox_wake.notify_one();
        }
    }
}

struct ActiveWrite {
    conn: NonNull<Connection>,
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

struct RootScopeGuard {
    store_id: usize,
}

impl Drop for RootScopeGuard {
    fn drop(&mut self) {
        let store_id = self.store_id;
        ACTIVE_WRITES.with(|cell| {
            let mut list = cell.borrow_mut();
            if let Some(pos) = list.iter().position(|(id, _)| *id == store_id) {
                list.swap_remove(pos);
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

    fn bound(c: &'a Connection, store: &'a Store) -> Self {
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
                .find(|(id, _)| *id == store_id)
                .map(|(_, entry)| entry.conn)
        })?;
        // SAFETY: `RootScopeGuard` keeps `ptr` live for the exact stack lifetime of the active
        // root `MutexGuard<Connection>` on this thread, and only shared `&Connection` references
        // are ever constructed.
        Some(unsafe { ptr.as_ref() })
    }

    fn defer_staged(&self, staged: Staged) -> Option<Staged> {
        self.inner
            .engine
            .defer_until_commit(
                steve_storage_api::Domain::Core,
                self.inner_id(),
                (self.clone(), staged),
                |parent, child| parent.1.merge(child.1),
                |(store, staged)| staged.flush(&store),
            )
            .err()
            .map(|(_, staged)| staged)
    }
    pub(crate) fn try_stage_event(&self, event: BusEvent) -> Result<(), BusEvent> {
        match self.defer_staged(Staged {
            events: vec![event],
            ..Default::default()
        }) {
            None => Ok(()),
            Some(mut staged) => Err(staged.events.pop().expect("unstaged event")),
        }
    }
    pub(crate) fn emit_event(&self, event: BusEvent) {
        if let Err(event) = self.try_stage_event(event) {
            self.inner.bus.publish_batch(vec![event]);
        }
    }
    pub(crate) fn try_stage_diagnostic(
        &self,
        record: crate::diagnostics::Record,
    ) -> Result<(), crate::diagnostics::Record> {
        match self.defer_staged(Staged {
            diagnostics: vec![record],
            ..Default::default()
        }) {
            None => Ok(()),
            Some(mut staged) => Err(staged.diagnostics.pop().expect("unstaged diagnostic")),
        }
    }
    pub(crate) fn stage_diagnostic(&self, record: crate::diagnostics::Record) {
        if let Err(record) = self.try_stage_diagnostic(record) {
            self.diagnostics().record(record);
        }
    }
    pub(crate) fn try_stage_diagnostic_until(&self, until: i64) -> bool {
        self.defer_staged(Staged {
            diagnostic_until: until,
            ..Default::default()
        })
        .is_none()
    }
    pub(crate) fn stage_diagnostic_until(&self, until: i64) {
        if !self.try_stage_diagnostic_until(until) {
            self.diagnostics().activate_until(until);
        }
    }
    pub(crate) fn try_stage_outbox(&self) -> bool {
        self.defer_staged(Staged {
            outbox: true,
            ..Default::default()
        })
        .is_none()
    }

    #[track_caller]
    fn atomic_with<T, E: From<StoreError>>(
        &self,
        f: impl FnOnce(&Connection) -> Result<T, E>,
    ) -> Result<T, E> {
        let run = |c: &Connection| {
            self.inner
                .engine
                .with_effect_scope(steve_storage_api::Domain::Core, || {
                    let tx = WriteTx::begin(c).map_err(StoreError::from)?;
                    let out = f(&tx)?;
                    tx.commit().map_err(StoreError::from)?;
                    Ok(out)
                })
        };
        if let Some(c) = self.active_conn() {
            return run(c);
        }
        let c = self.lock().map_err(E::from)?;
        let store_id = self.inner_id();
        ACTIVE_WRITES.with(|cell| {
            cell.borrow_mut().push((
                store_id,
                ActiveWrite {
                    conn: NonNull::from(&*c),
                },
            ));
        });
        let _scope = RootScopeGuard { store_id };
        run(&c)
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
        self.read_or_active(f)
    }
}

pub(crate) struct WrappedEffects(Staged);

impl WrappedEffects {
    pub(crate) fn flush(self, store: &Store) {
        if let Some(staged) = store.defer_staged(self.0) {
            staged.flush(store);
        }
    }
}

#[cfg(test)]
mod commit_effect_tests {
    use crate::{
        NoPublisher, Store, WS,
        diagnostics::{Record, RecordStatus},
    };
    use std::panic::{AssertUnwindSafe, catch_unwind};
    use steve_storage_api::{Domain, Lane};

    #[test]
    fn raw_and_typed_transactions_defer_diagnostic_queue_and_capture_cache() {
        for typed in [false, true] {
            for finish in ["ROLLBACK", "COMMIT", "panic"] {
                let dir = tempfile::tempdir().unwrap();
                let store = Store::open(dir.path().join("steve.sqlite3"), NoPublisher).unwrap();
                store.ensure_workspace().unwrap();
                store.with_conn(|c| {
                    c.execute_batch("INSERT INTO items(id,workspace_id,no,title,status,stage,created,updated) VALUES('effect-item','default',1,'I','run','',0,0)")?;
                    Ok(())
                }).unwrap();
                let detail = || Record {
                    workspace_id: WS.into(),
                    item_id: Some("effect-item".into()),
                    kind: "dependency.checked".into(),
                    reason_code: "fixture_condition".into(),
                    detail: true,
                    ..Default::default()
                };
                let run = |c: &rusqlite::Connection| {
                    c.execute_batch("BEGIN IMMEDIATE").unwrap();
                    store
                        .atomic(|| -> Result<(), crate::StoreError> {
                            store.stage_diagnostic_until(crate::now() + 60_000);
                            store.stage_diagnostic(Record {
                                detail: false,
                                ..detail()
                            });
                            Ok(())
                        })
                        .unwrap();
                    assert_eq!(
                        store.diagnostics().record(detail()),
                        RecordStatus::Skipped,
                        "uncommitted capture cache escaped"
                    );
                    assert_eq!(
                        store.diagnostics().explain(WS, "effect-item").unwrap()["coverage"]["instance_pending"],
                        0,
                        "uncommitted diagnostic enqueued"
                    );
                    if finish == "panic" {
                        panic!("diagnostic owner panic");
                    }
                    c.execute_batch(finish).unwrap();
                };
                let result = catch_unwind(AssertUnwindSafe(|| {
                    if typed {
                        store
                            .domain_store()
                            .with_root(Domain::Core, Lane::Interactive, |c, _, _| {
                                run(c);
                                Ok(())
                            })
                            .unwrap();
                    } else {
                        let guard = store
                            .domain_store()
                            .root_guard(Domain::Core, Lane::Interactive)
                            .unwrap();
                        run(&guard);
                    }
                }));
                assert_eq!(result.is_err(), finish == "panic");
                assert_eq!(
                    store.diagnostics().record(detail()),
                    if finish == "COMMIT" {
                        RecordStatus::Queued
                    } else {
                        RecordStatus::Skipped
                    }
                );
                let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
                loop {
                    let view = store.diagnostics().explain(WS, "effect-item").unwrap();
                    if view["coverage"]["instance_pending"] == 0 {
                        break;
                    }
                    assert!(
                        std::time::Instant::now() < deadline,
                        "diagnostic worker did not drain: {view}"
                    );
                    std::thread::yield_now();
                }
                let records = store
                    .diagnostics()
                    .events(WS, "effect-item", crate::diagnostics::Filter::default())
                    .unwrap();
                assert_eq!(
                    records["events"].as_array().unwrap().len(),
                    usize::from(finish == "COMMIT")
                );
            }
        }
    }
}
