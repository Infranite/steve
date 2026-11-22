//! The storage contract: [`Store::write`] / [`Store::read`] closures and typed repositories on
//! their handles ([`W`], [`R`]), over a private [`Backend`] per database.
//!
//! - One outermost `write` is one transaction. The Hub has one writer (the write lock); events
//!   go only through [`W::emit`], are published after the commit in commit order, and are dropped
//!   on rollback, including a [`W::scope`] that rolled back.
//! - No SQL outside this crate: statements run through methods of a sealed trait that only this
//!   crate can name; repositories are the only way in.
//! - SQL is written once with sea-query and rendered by the backend's [`Dialect`]. Writing it,
//!   keep to what every backend types the same way: integers are 64-bit (`i64`, BIGINT), JSON is
//!   TEXT (`jsonb` on a server database), and values are never bound in a `SELECT` list (a server
//!   database types such a parameter as text): read first, then write.
//!
//! Domains not on the contract yet still use [`crate::Tx`] and the repositories of [`Store`]; they
//! share the transaction through [`W::legacy`], and a `write` inside one of their transactions
//! joins it as a scope.

mod backend;
mod contacts;
mod memory_index;
mod sqlite;
mod workspace_settings;
mod workspaces;

use rusqlite::Connection;
use sea_query::QueryStatementWriter;

pub(crate) use backend::{Backend, Dialect, Json, Row};
pub use contacts::Contacts;
pub use memory_index::{
    Added as AddedMemoryIndex, Index as MemoryIndexData, IndexedRange, MemoryIndex,
    Range as MemoryRange,
};
pub(crate) use sqlite::Sqlite;
pub(crate) use workspace_settings::WorkspaceSettings;
pub use workspaces::Members;

use crate::{BusEvent, Result, Store, StoreError, Tx};

/// A read snapshot.
pub struct R<'a> {
    db: &'a mut dyn Backend,
}

/// The write transaction (or a scope of it).
pub struct W<'a> {
    db: &'a mut dyn Backend,
    store: &'a Store,
    conn: &'a Connection,
}

impl Store {
    /// One write transaction: committed if `f` returns `Ok`, rolled back otherwise. What `f`
    /// emitted is published after the commit, in order. Inside another write on this thread it
    /// is a [`W::scope`] of that write. The closure can return domain errors alongside storage errors.
    #[track_caller]
    pub fn write<T, E: From<StoreError>>(
        &self,
        f: impl FnOnce(&mut W<'_>) -> Result<T, E>,
    ) -> Result<T, E> {
        if let Some(c) = self.active_conn() {
            let mut db = Sqlite(c);
            let mut w = W {
                db: &mut db,
                store: self,
                conn: c,
            };
            return w.scope(f);
        }
        self.root(|conn, db| {
            f(&mut W {
                db,
                store: self,
                conn,
            })
        })
    }

    /// One read-only snapshot on a pooled connection; never waits for the writer. Inside a write
    /// on this thread it reads that write (and sees what it wrote so far).
    pub fn read<T>(&self, f: impl FnOnce(&mut R<'_>) -> Result<T>) -> Result<T> {
        if let Some(c) = self.active_conn() {
            return f(&mut R { db: &mut Sqlite(c) });
        }
        let c = self.inner.readers.get()?;
        crate::instrumentation::store_acquired();
        let mut db = Sqlite(&c);
        db.begin_read()?;
        // a pooled connection goes back without its snapshot, however `f` ended (even a panic)
        let _end = EndRead(&c);
        let out = f(&mut R { db: &mut db });
        let end = db.batch("COMMIT");
        let v = out?;
        end?;
        Ok(v)
    }
}

struct EndRead<'c>(&'c Connection);

impl Drop for EndRead<'_> {
    fn drop(&mut self) {
        if !self.0.is_autocommit() {
            let _ = self.0.execute_batch("ROLLBACK");
        }
    }
}

impl<'a> W<'a> {
    /// Published after the commit; dropped if this write, or the scope it is in, rolls back.
    pub fn emit(&mut self, e: impl Into<BusEvent>) {
        self.store.emit_event(e.into());
    }

    /// A nested scope (SAVEPOINT): if `f` fails, it alone is rolled back, with what it emitted,
    /// and the write goes on.
    pub fn scope<T, E: From<StoreError>>(
        &mut self,
        f: impl FnOnce(&mut Self) -> Result<T, E>,
    ) -> Result<T, E> {
        let scope = self.store.open_scope(self.db)?;
        let out = f(self);
        self.store.close_scope(self.db, scope, out)
    }

    /// This transaction as the domains not on the contract yet take it: the same connection and
    /// the same staging (what it publishes follows this write's commit or rollback, and the
    /// scope it is in). Goes away with the last of them.
    pub fn legacy(&self) -> Tx<'a> {
        Tx::bound(self.conn, self.store)
    }

    pub(crate) fn exec<S: QueryStatementWriter>(&mut self, s: &S) -> Result<u64> {
        exec_on(self.db, s)
    }

    pub(crate) fn dialect(&self) -> Dialect {
        self.db.dialect()
    }
}

/// Runs a statement on `db` (for store internals that share one between the contract and code
/// not on it yet).
pub(crate) fn exec_on<S: QueryStatementWriter>(db: &mut dyn Backend, s: &S) -> Result<u64> {
    let (sql, values) = db.dialect().render(s)?;
    db.exec(&sql, &values)
}

mod sealed {
    use super::*;

    /// Running statements: only this crate can name it, so no SQL outside the store.
    pub trait Q {
        fn db(&mut self) -> &mut dyn Backend;

        fn all<S: QueryStatementWriter, T>(
            &mut self,
            s: &S,
            mut f: impl FnMut(&Row<'_>) -> Result<T>,
        ) -> Result<Vec<T>> {
            let db = self.db();
            let (sql, values) = db.dialect().render(s)?;
            let mut out = vec![];
            db.query(&sql, &values, &mut |r| {
                out.push(f(r)?);
                Ok(())
            })?;
            Ok(out)
        }

        fn one<S: QueryStatementWriter, T>(
            &mut self,
            s: &S,
            f: impl FnMut(&Row<'_>) -> Result<T>,
        ) -> Result<Option<T>> {
            Ok(self.all(s, f)?.into_iter().next())
        }
    }
}

pub(crate) use sealed::Q;

/// Both a read snapshot and the write transaction can read (the write sees its own writes).
pub trait Read: sealed::Q {}

impl sealed::Q for R<'_> {
    fn db(&mut self) -> &mut dyn Backend {
        self.db
    }
}
impl Read for R<'_> {}

impl sealed::Q for W<'_> {
    fn db(&mut self) -> &mut dyn Backend {
        self.db
    }
}
impl Read for W<'_> {}
