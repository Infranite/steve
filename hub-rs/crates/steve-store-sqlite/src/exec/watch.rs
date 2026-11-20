//! Only preupdate_hook observes table changes. Tests against bundled SQLite confirm
//! per-row unconditional DELETEs, REPLACE's implicit DELETE followed by INSERT, and
//! WITHOUT ROWID writes all produce hints; none of these three needs a blind-spot
//! exception. Trigger writes are observed too. No update/commit/rollback hook is used.
//! A subscription created during a writer turn conservatively adds its table keys
//! to that turn; its next writer turn installs the hook if it is still subscribed.
use rusqlite::{
    Connection,
    hooks::{Action, PreUpdateCase},
};
use std::{
    cell::RefCell,
    collections::BTreeSet,
    sync::{Arc, Mutex, Weak},
};
use steve_storage_api::CommitWatches;

type Tables = Mutex<BTreeSet<String>>;
thread_local! { static ACTIVE: RefCell<Vec<(usize, Weak<Tables>)>> = const { RefCell::new(Vec::new()) }; }

pub(crate) struct Capture<'a> {
    connection: &'a Connection,
    watches: &'a CommitWatches,
    tables: Arc<Mutex<BTreeSet<String>>>,
    active: bool,
}
impl<'a> Capture<'a> {
    pub(crate) fn begin(
        connection: &'a Connection,
        watches: &'a CommitWatches,
    ) -> Result<Self, rusqlite::Error> {
        let tables = Arc::new(Mutex::new(BTreeSet::new()));
        let active = watches.has_subscribers();
        if active {
            let changed = tables.clone();
            connection.preupdate_hook(Some(
                move |_: Action, database: &str, table: &str, _: &PreUpdateCase| {
                    if database == "main" {
                        changed
                            .lock()
                            .unwrap_or_else(|e| e.into_inner())
                            .insert(table.to_owned());
                    }
                },
            ))?;
        }
        // Identity is the subscription registry, never a connection pointer. TLS
        // merely shares owned table names with registrations inside this writer turn.
        ACTIVE.with(|scopes| {
            scopes.borrow_mut().push((
                std::ptr::from_ref(watches) as usize,
                Arc::downgrade(&tables),
            ))
        });
        Ok(Self {
            connection,
            watches,
            tables,
            active,
        })
    }
    pub(crate) fn subscribed(watches: &CommitWatches, tables: &[String]) {
        ACTIVE.with(|scopes| {
            if let Some(changed) = scopes
                .borrow()
                .iter()
                .rev()
                .find(|(id, _)| *id == std::ptr::from_ref(watches) as usize)
                .and_then(|(_, changed)| changed.upgrade())
            {
                // Earlier statements may predate the first subscriber and hook.
                // Publication still waits for the outer commit; rollback stays quiet.
                changed
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .extend(tables.iter().cloned());
            }
        });
    }
    pub(crate) fn finish(self, committed: bool) {
        if committed {
            let tables: Vec<_> = self
                .tables
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .iter()
                .cloned()
                .collect();
            // Rolled-back savepoints may leave extra tables here: hints may over-fire.
            self.watches.notify(&tables);
        }
    }
}
impl Drop for Capture<'_> {
    fn drop(&mut self) {
        ACTIVE.with(|scopes| {
            scopes
                .borrow_mut()
                .retain(|(_, tables)| !Weak::ptr_eq(tables, &Arc::downgrade(&self.tables)))
        });
        if self.active {
            let _ = self
                .connection
                .preupdate_hook(None::<fn(Action, &str, &str, &PreUpdateCase)>);
        }
    }
}

/// Legacy callbacks own COMMIT/ROLLBACK, including autocommit SQL. One idle,
/// read-only observer per physical writer detects successful file commits using
/// data_version, without a commit/rollback hook or a reserved read-pool slot. It
/// never keeps a read transaction open. Foreign-process commits may add extra hints.
pub(crate) struct Observer(Mutex<Connection>);
pub(crate) struct Observation<'a> {
    connection: std::sync::MutexGuard<'a, Connection>,
    before: i64,
}
impl Observer {
    pub(crate) fn new(engine: &crate::engine::Engine) -> Result<Self, steve_storage_api::Error> {
        let c = Connection::open_with_flags(engine.path(), crate::pragmas::readonly_flags())
            .map_err(|e| steve_storage_api::Error::from(crate::domains::sql_error(e)))?;
        c.busy_timeout(std::time::Duration::from_secs(5))
            .map_err(|e| steve_storage_api::Error::from(crate::domains::sql_error(e)))?;
        Ok(Self(Mutex::new(c)))
    }
    pub(crate) fn begin(&self) -> Result<Observation<'_>, steve_storage_api::Error> {
        // Always observe: a callback may subscribe after its earlier SQL statements.
        let connection = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let before = version(&connection)
            .map_err(|e| steve_storage_api::Error::from(crate::domains::sql_error(e)))?;
        Ok(Observation { connection, before })
    }
}
impl Observation<'_> {
    pub(crate) fn committed(&self) -> bool {
        // An observer failure after a durable commit must not lose a hint.
        version(&self.connection) != Ok(self.before)
    }
}
fn version(c: &Connection) -> rusqlite::Result<i64> {
    c.query_row("PRAGMA data_version", [], |r| r.get(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn no_subscribers_does_not_install_or_replace_hook() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE t(id INTEGER)").unwrap();
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = count.clone();
        c.preupdate_hook(Some(
            move |_: Action, _: &str, _: &str, _: &PreUpdateCase| {
                seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            },
        ))
        .unwrap();
        let watches = CommitWatches::default();
        let capture = Capture::begin(&c, &watches).unwrap();
        c.execute("INSERT INTO t VALUES(1)", []).unwrap();
        capture.finish(true);
        c.execute("INSERT INTO t VALUES(2)", []).unwrap();
        assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 2);
    }
}

#[cfg(test)]
mod blind_spots {
    use super::*;
    #[test]
    fn preupdate_reports_each_deleted_row_for_truncate_and_replace_and_without_rowid() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE rowid_probe(id INTEGER PRIMARY KEY); INSERT INTO rowid_probe VALUES(1),(2); CREATE TABLE wr(id TEXT PRIMARY KEY) WITHOUT ROWID;").unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let recorded = events.clone();
        c.preupdate_hook(Some(
            move |action: Action, _: &str, table: &str, _: &PreUpdateCase| {
                recorded.lock().unwrap().push((action, table.to_owned()));
            },
        ))
        .unwrap();
        c.execute("DELETE FROM rowid_probe", []).unwrap();
        assert_eq!(
            *events.lock().unwrap(),
            vec![
                (Action::SQLITE_DELETE, "rowid_probe".to_owned()),
                (Action::SQLITE_DELETE, "rowid_probe".to_owned())
            ]
        );
        c.execute("INSERT INTO rowid_probe VALUES(1)", []).unwrap();
        events.lock().unwrap().clear();
        c.execute("INSERT OR REPLACE INTO rowid_probe VALUES(1)", [])
            .unwrap();
        assert_eq!(
            *events.lock().unwrap(),
            vec![
                (Action::SQLITE_DELETE, "rowid_probe".to_owned()),
                (Action::SQLITE_INSERT, "rowid_probe".to_owned())
            ],
            "REPLACE must report its implicit delete independently of its insert"
        );
        events.lock().unwrap().clear();
        c.execute("INSERT INTO wr VALUES('key')", []).unwrap();
        assert_eq!(
            *events.lock().unwrap(),
            vec![(Action::SQLITE_INSERT, "wr".to_owned())]
        );
    }
}
