//! Public transactions for module crates, whose repositories are free functions over a connection
//! ("函数收 `&rusqlite::Connection`/`&Transaction`").

use std::cell::{Cell, RefCell};
use std::ops::Deref;

use rusqlite::Connection;

use crate::{ChangeEvent, Store, StoreError};

/// One write transaction. Derefs to the connection; events published through it are held back and go
/// out only after the commit, in order, so a rolled-back write never reaches a browser.
pub struct Tx<'a> {
    c: &'a Connection,
    events: RefCell<Vec<ChangeEvent>>,
    diagnostics: RefCell<Vec<crate::diagnostics::Record>>,
    /// An outbox event was emitted: wake the dispatcher after the commit.
    outbox: Cell<bool>,
}

impl Tx<'_> {
    /// Queues an event for after the commit.
    pub fn publish(&self, e: ChangeEvent) {
        self.events.borrow_mut().push(e);
    }

    pub(crate) fn mark_outbox(&self) {
        self.outbox.set(true);
    }

    /// Metadata is queued only after commit, never for a rolled-back business write.
    pub(crate) fn diagnostic(&self, event: crate::diagnostics::Record) {
        self.diagnostics.borrow_mut().push(event);
    }
}

impl<'a> Tx<'a> {
    /// Wraps a transaction the store already opened (for hooks inside a repository write).
    pub(crate) fn wrap(c: &'a Connection) -> Self {
        Tx {
            c,
            events: RefCell::new(Vec::new()),
            diagnostics: RefCell::new(Vec::new()),
            outbox: Cell::new(false),
        }
    }

    /// The queued events, to publish after the commit.
    pub(crate) fn take_events(self) -> Vec<ChangeEvent> {
        self.events.into_inner()
    }
}

impl Deref for Tx<'_> {
    type Target = Connection;
    fn deref(&self) -> &Connection {
        self.c
    }
}

impl Store {
    /// Runs `f` in one write transaction: committed if it returns `Ok`, rolled back otherwise.
    ///
    /// The write lock is held for the whole closure, so `f` must not call the `store.x()` repositories
    /// (they take the same lock and would deadlock) and must not block on I/O; use the free functions in
    /// [`crate::base`] or the module's own repo functions on `tx`.
    pub fn transaction<T, E: From<StoreError>>(
        &self,
        f: impl FnOnce(&Tx) -> Result<T, E>,
    ) -> Result<T, E> {
        let c = self.lock();
        let tx = crate::immediate(&c).map_err(StoreError::from)?;
        let t = Tx {
            c: &tx,
            events: RefCell::new(Vec::new()),
            diagnostics: RefCell::new(Vec::new()),
            outbox: Cell::new(false),
        };
        let out = f(&t)?;
        let (events, outbox) = (t.events.into_inner(), t.outbox.get());
        let diagnostics = t.diagnostics.into_inner();
        tx.commit().map_err(StoreError::from)?;
        for event in diagnostics {
            self.diagnostics().record(event);
        }
        for e in events {
            self.inner.bus.publish(e);
        }
        if outbox {
            self.inner.outbox_wake.notify_one();
        }
        Ok(out)
    }

    /// Runs `f` on a pooled read-only connection (module read paths).
    pub fn reader<T, E: From<StoreError>>(
        &self,
        f: impl FnOnce(&Connection) -> Result<T, E>,
    ) -> Result<T, E> {
        let c = self.inner.readers.get().map_err(StoreError::from)?;
        f(&c)
    }
}
