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
pub mod account_profiles;
pub mod account_rows;
pub mod accounts;
pub(crate) mod alarms;
pub mod approval;
mod attribute_rows;
pub mod audit;
pub(crate) mod automation;
mod backend;
pub mod backup_policy;
mod bot_rows;
pub mod bots;
mod catalog_rows;
pub mod changesets;
pub mod channel_feed;
pub mod channel_messages;
pub mod channel_names;
pub mod channel_personas;
pub mod channel_positions;
pub mod channel_sends;
pub mod channel_spaces;
pub mod channel_trigger_report;
pub mod channel_write_policy;
mod connection_rows;
pub mod connections;
mod contacts;
#[cfg(feature = "testkit")]
pub mod core_fixture;
pub mod core_usage;
pub mod decisions;
mod digest_notes;
pub mod feishu_registration;
pub mod files;
mod flow_rows;
mod flow_sources;
mod flow_triggers;
mod flow_waits;
pub mod flows;
pub mod github;
mod grant_rows;
pub mod grants;
pub mod item_delivery;
pub mod items;
mod knowledge_references;
pub mod links;
pub mod mail;
mod match_rules;
mod memory_index;
mod merge_proposals;
pub(crate) mod message_sources;
pub mod node_commands;
pub mod node_directory;
pub mod node_environment;
pub mod node_sessions;
pub mod node_turns;
pub mod notifications;
pub mod notify;
pub mod operations;
pub mod parties;
mod party_rows;
mod passkey_rows;
#[cfg(test)]
mod postgres;
mod project_rows;
pub mod projects;
pub(crate) mod read_models;
pub mod realtime_output;
pub mod reply_basis;
mod revisions;
mod run_rows;
pub mod secret_uses;
pub mod sessions;

pub mod settings;
mod signal_rows;
pub mod sources;
mod sqlite;
mod steward_rows;
mod systems;
pub mod tags;
mod team_rows;
pub mod thread;
mod thread_triggers;
mod user_rows;
pub mod vault_items;
pub mod vault_keys;
pub mod vault_uses;
pub mod waits;
pub mod workflow_runs;
mod workspace_settings;
mod workspaces;

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use r2d2::PooledConnection;
use r2d2_sqlite::SqliteConnectionManager;
use rusqlite::Connection;
use sea_query::QueryStatementWriter;

pub use account_rows::Accounts;
pub use attribute_rows::{AttrDef, AttrValue, Attributes, NewDef, NewValue};
pub(crate) use backend::{Backend, Dialect, Json, Row};
pub use bot_rows::{BotPrompt, BotRef, BotRows, ReplyBot};
pub use catalog_rows::{DeviceIdentity, DeviceRows, PersonaIdentity, PersonaRows};
pub use connection_rows::{ConnRow, ConnectionBrief, DirectoryConnection, HealthState};
pub use contacts::{AccountIdentity, Contacts};
pub use decisions::DecisionRows;
pub use digest_notes::{DigestEntry, DigestNotes};
pub use flow_rows::{FlowRow, FlowRows, NewFlowRow, NewTrigger, SchedulePatch};
pub use flow_sources::{FlowSources, SourceConfig};
pub use flow_triggers::{DueTrigger, FlowTriggers};
pub use flow_waits::{FlowWaits, NewWait};
pub use grant_rows::{FlowGrant, GrantRows, NewVaultGrant, VaultGrant, VaultGrantRecord};
pub use knowledge_references::{
    FileInfo, FileReads, ItemEvidence, MessageEvidence, ProjectRef, References, SessionEvidence,
};
pub use match_rules::{MatchRule, MatchRules, PrefixConfig};
pub use memory_index::{
    Added as AddedMemoryIndex, Index as MemoryIndexData, IndexedRange, MemoryIndex,
    Range as MemoryRange,
};
pub use merge_proposals::{ContactMerge, ContactMerges, MergeProposals, NewMergeProposal};
pub use party_rows::{ContactQuery, NewParty, Party, PartyPatch};
pub use passkey_rows::{PasskeyInfo, PasskeyRows};
pub use project_rows::{ProjectRows, TagRows};
pub use revisions::Revisions;
pub use run_rows::{NewRun, NewStep, RunRow, RunRows, RunStep, StartupRecord};
pub use signal_rows::{
    NewCorrection, NewSignal, SignalFilter, SignalRow, SignalRows, SignalTriage,
};
pub(crate) use sqlite::Sqlite;
pub use steward_rows::{Key as StewardKey, Steward, Stewards};
pub use systems::{System, Systems, builtin_system_name, is_builtin_system};
pub use team_rows::{Details as TeamDetails, MembershipRow, SyncedTeam, TeamRef, Teams};
pub use user_rows::{AuthSessions, UserRows};
pub use workspace_settings::{HTTP_ALLOW_PRIVATE, WorkspaceSettings};
pub use workspaces::{Members, Workspaces};

use crate::{BusEvent, Result, Store, StoreError};

/// A read snapshot.
pub struct R<'a> {
    db: &'a mut dyn Backend,
    conn: &'a Connection,
    windows: Option<&'a crate::repo::window_cache::WindowCaches>,
}

/// The write transaction (or a scope of it).
pub struct W<'a> {
    db: &'a mut dyn Backend,
    store: &'a Store,
    // SQLite repositories reuse the writer for SQLite-specific queries and fixtures.
    pub(super) conn: &'a Connection,
}

pub(crate) struct ReadConnection {
    conn: PooledConnection<SqliteConnectionManager>,
    _active: Option<crate::instrumentation::ReadPoolActiveGuard>,
}

impl ReadConnection {
    pub(crate) fn conn(&self) -> &Connection {
        &self.conn
    }
}

thread_local! {
    static READ_SNAPSHOTS: RefCell<Vec<(usize, Rc<ReadConnection>)>> = const { RefCell::new(Vec::new()) };
}

struct ReadSnapshot(usize);

impl Drop for ReadSnapshot {
    fn drop(&mut self) {
        READ_SNAPSHOTS.with_borrow_mut(|snapshots| {
            snapshots.retain(|(id, _)| *id != self.0);
        });
    }
}

impl Store {
    pub(crate) fn acquire_reader(&self) -> Result<ReadConnection> {
        let wait = crate::instrumentation::ReadPoolWait::start();
        match self.inner.readers.get() {
            Ok(conn) => {
                let active = wait.map(crate::instrumentation::ReadPoolWait::acquired);
                crate::instrumentation::store_acquired();
                Ok(ReadConnection {
                    conn,
                    _active: active,
                })
            }
            Err(e) => {
                if let Some(w) = wait {
                    w.failed();
                }
                Err(StoreError::from(e))
            }
        }
    }

    pub(crate) fn active_snapshot(&self) -> Option<Rc<ReadConnection>> {
        let id = Arc::as_ptr(&self.inner) as usize;
        READ_SNAPSHOTS.with_borrow(|snapshots| {
            snapshots
                .iter()
                .find(|(key, _)| *key == id)
                .map(|(_, c)| Rc::clone(c))
        })
    }
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
    /// on this thread it reads that write (and sees what it wrote so far). A nested read reuses
    /// the outer snapshot.
    pub fn read<T, E: From<StoreError>>(
        &self,
        f: impl FnOnce(&mut R<'_>) -> Result<T, E>,
    ) -> Result<T, E> {
        if let Some(c) = self.active_conn() {
            return f(&mut R {
                db: &mut Sqlite(c),
                conn: c,
                windows: Some(&self.inner.windows),
            });
        }
        if let Some(c) = self.active_snapshot() {
            return f(&mut R {
                db: &mut Sqlite(c.conn()),
                conn: c.conn(),
                windows: Some(&self.inner.windows),
            });
        }
        let c = Rc::new(self.acquire_reader()?);
        read_snapshot(c.conn(), Some(&self.inner.windows), |r| {
            let id = Arc::as_ptr(&self.inner) as usize;
            READ_SNAPSHOTS.with_borrow_mut(|snapshots| snapshots.push((id, Rc::clone(&c))));
            let _snapshot = ReadSnapshot(id);
            f(r)
        })
    }

    /// Read a database file without starting the Hub or modifying the file.
    pub fn read_file<T>(
        path: &std::path::Path,
        f: impl FnOnce(&mut R<'_>) -> Result<T>,
    ) -> Result<T> {
        let c = Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)?;
        c.pragma_update(None, "query_only", true)?;
        read_snapshot(&c, None, f)
    }
}

fn read_snapshot<T, E: From<StoreError>>(
    c: &Connection,
    windows: Option<&crate::repo::window_cache::WindowCaches>,
    f: impl FnOnce(&mut R<'_>) -> Result<T, E>,
) -> Result<T, E> {
    let mut db = Sqlite(c);
    db.begin_read()?;
    let _end = EndRead(c);
    let out = f(&mut R {
        db: &mut db,
        conn: c,
        windows,
    });
    let end = db.batch("COMMIT");
    let v = out?;
    end?;
    Ok(v)
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
    #[cfg(feature = "testkit")]
    pub(crate) fn fixture_connection(&self) -> &Connection {
        self.conn
    }

    /// Published after the commit; dropped if this write, or the scope it is in, rolls back.
    pub fn emit(&mut self, e: impl Into<BusEvent>) {
        self.store.emit_event(e.into());
    }

    pub(crate) fn diagnostic(&self, event: crate::diagnostics::Record) {
        self.store.stage_diagnostic(event);
    }

    pub(crate) fn outbox_changed(&self) {
        let staged = self.store.try_stage_outbox();
        debug_assert!(staged, "a contract write has an active transaction");
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
    pub(crate) trait Q {
        fn connection(&self) -> &Connection;

        fn db(&mut self) -> &mut dyn Backend;

        fn window_caches(&self) -> Option<&crate::repo::window_cache::WindowCaches>;

        fn scan<S: QueryStatementWriter>(
            &mut self,
            s: &S,
            mut visit: impl FnMut(&Row<'_>) -> Result<()>,
        ) -> Result<()> {
            let db = self.db();
            let (sql, values) = db.dialect().render(s)?;
            db.query(&sql, &values, &mut visit)
        }

        fn all<S: QueryStatementWriter, T>(
            &mut self,
            s: &S,
            mut f: impl FnMut(&Row<'_>) -> Result<T>,
        ) -> Result<Vec<T>> {
            let mut out = vec![];
            self.scan(s, |r| {
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
#[allow(private_bounds)]
pub trait Read: sealed::Q {}

fn next_deadline(
    r: &mut impl Read,
    query: &mut sea_query::SelectStatement,
    at: sea_query::Expr,
    now: i64,
) -> Result<Option<i64>> {
    use sea_query::{Expr, ExprTrait, Func};
    let invalid = r.db().dialect().invalid_integer(at.clone());
    let (next, malformed): (Option<i64>, i64) = r
        .one(
            query
                .expr(Func::min(Expr::case(invalid.clone().not(), at)))
                .expr(Func::count(Expr::case(
                    invalid,
                    Expr::Constant(1_i64.into()),
                ))),
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?
        .unwrap_or_default();
    Ok(if malformed > 0 {
        Some(next.map_or(now, |at| std::cmp::min(at, now)))
    } else {
        next
    })
}

impl sealed::Q for R<'_> {
    fn connection(&self) -> &Connection {
        self.conn
    }

    fn db(&mut self) -> &mut dyn Backend {
        self.db
    }

    fn window_caches(&self) -> Option<&crate::repo::window_cache::WindowCaches> {
        self.windows
    }
}
impl Read for R<'_> {}

impl sealed::Q for W<'_> {
    fn connection(&self) -> &Connection {
        self.conn
    }

    fn db(&mut self) -> &mut dyn Backend {
        self.db
    }

    fn window_caches(&self) -> Option<&crate::repo::window_cache::WindowCaches> {
        Some(&self.store.inner.windows)
    }
}
impl Read for W<'_> {}

pub mod node_jobs;
pub mod node_write;
pub mod state_batches;

pub mod bot_admin;
pub mod deliveries;
pub mod tool_admission;
pub mod tool_journal;
