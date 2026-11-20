//! Table hints for this Store and its clones, published only at `Capture::finish`.
//! update_hook misses truncate-optimized DELETE and WITHOUT ROWID: production
//! guards prohibit unconditional DELETE of watched tables and WITHOUT ROWID schemas.
//! REPLACE still hints via INSERT; FK cascades/triggers hint their rowid tables.
//! FTS shadow writes are not a promise to observe a virtual table's logical name.
//! Independent connections/processes and remote stores require the 60-second poll.
use rusqlite::{
    Connection,
    hooks::Action,
    trace::{TraceEvent, TraceEventCodes},
};
use std::{
    any::{Any, TypeId},
    cell::RefCell,
    collections::BTreeSet,
    sync::{Arc, Mutex, Weak},
};
use steve_storage_api::CommitWatches;

// Owned facade attachments. Each physical commit remains a distinct batch.
// Merge only after confirmation: merging across a savepoint before rollback
// would contaminate its parent's attachment, which cannot then be truncated.
type OwnedAttachment = Box<dyn Any + Send>;
type MergeAttachment = Box<dyn Fn(&mut dyn Any, OwnedAttachment) + Send>;
type PublishAttachment = Box<dyn FnOnce(OwnedAttachment) + Send>;

pub(crate) struct Effect {
    key: (TypeId, usize),
    value: OwnedAttachment,
    merge: MergeAttachment,
    publish: PublishAttachment,
}
impl Effect {
    pub(crate) fn new<T: Send + 'static>(
        owner: usize,
        value: T,
        merge: fn(&mut T, T),
        publish: fn(T),
    ) -> Self {
        Self {
            key: (TypeId::of::<T>(), owner),
            value: Box::new(value),
            merge: Box::new(move |parent, child| {
                merge(
                    parent.downcast_mut::<T>().expect("attachment type"),
                    *child.downcast::<T>().expect("attachment type"),
                )
            }),
            publish: Box::new(move |value| {
                publish(*value.downcast::<T>().expect("attachment type"))
            }),
        }
    }
}
pub(crate) fn publish_effects(commits: Vec<Vec<Effect>>) {
    for effects in commits {
        let mut batches: Vec<Effect> = Vec::new();
        for effect in effects {
            if let Some(parent) = batches.iter_mut().find(|parent| parent.key == effect.key) {
                (parent.merge)(&mut *parent.value, effect.value);
            } else {
                batches.push(effect);
            }
        }
        for effect in batches {
            (effect.publish)(effect.value);
        }
    }
}

#[derive(Default)]
struct Changes {
    tables: BTreeSet<String>,
    candidate: BTreeSet<String>,
    committed: BTreeSet<String>,
    installed: bool,
    updates_installed: bool,
    next_effect: u64,
    pending_effects: Vec<(u64, Effect)>,
    candidate_effects: Vec<(u64, Effect)>,
    confirmed_effects: Vec<Vec<Effect>>,
}
impl Changes {
    fn propose_commit(&mut self) {
        let tables = std::mem::take(&mut self.tables);
        self.candidate.extend(tables);
        self.candidate_effects.append(&mut self.pending_effects);
    }
}
struct Active {
    registry: usize,
    // Scoped, thread-local borrow, never sent to another thread or retained after
    // Capture::drop. The Capture owns an immutable borrow of this Connection.
    connection: *const Connection,
    changes: Weak<Mutex<Changes>>,
}
thread_local! { static ACTIVE: RefCell<Vec<Active>> = const { RefCell::new(Vec::new()) }; }

pub(crate) struct Capture<'a> {
    connection: &'a Connection,
    watches: &'a CommitWatches,
    changes: Arc<Mutex<Changes>>,
}
fn install(
    connection: &Connection,
    changes: &Arc<Mutex<Changes>>,
    tables: bool,
) -> rusqlite::Result<()> {
    if tables
        && !changes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .updates_installed
    {
        changes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .updates_installed = true;
        let changed = changes.clone();
        connection.update_hook(Some(
            move |_: Action, database: &str, table: &str, _: i64| {
                if database == "main" {
                    changed
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .tables
                        .insert(table.to_owned());
                }
            },
        ))?;
    }
    if changes.lock().unwrap_or_else(|e| e.into_inner()).installed {
        return Ok(());
    }
    changes.lock().unwrap_or_else(|e| e.into_inner()).installed = true;
    let changed = changes.clone();
    connection.commit_hook(Some(move || {
        let mut changed = changed.lock().unwrap_or_else(|e| e.into_inner());
        // This is only a candidate: IO/constraint failures may still roll it back.
        changed.propose_commit();
        false
    }))?;
    let changed = changes.clone();
    connection.rollback_hook(Some(move || {
        let mut changed = changed.lock().unwrap_or_else(|e| e.into_inner());
        changed.tables.clear();
        changed.candidate.clear();
        changed.pending_effects.clear();
        changed.candidate_effects.clear();
        // A later failed transaction must not erase earlier successful autocommits.
    }))?;
    // SQLite invokes PROFILE after sqlite3_step finishes, including after rollback
    // callbacks on failed commits. Confirm here, never notify from any callback.
    // Captures do not nest: same-Engine guards and apply_in reuse their root.
    connection.trace_v2(
        TraceEventCodes::SQLITE_TRACE_PROFILE,
        Some(statement_finished),
    );
    Ok(())
}
fn statement_finished(event: TraceEvent<'_>) {
    if let TraceEvent::Profile(_, _) = event {
        ACTIVE.with(|scopes| {
            if let Some(scope) = scopes.borrow().last()
                && let Some(changed) = scope.changes.upgrade()
            {
                // SAFETY: the current Capture owns this scoped TLS borrow until
                // Drop. This only reads sqlite3_get_autocommit, never executes SQL.
                // A failed COMMIT that leaves a transaction open is not confirmed.
                let connection = unsafe { &*scope.connection };
                if connection.is_autocommit() {
                    let mut changed = changed.lock().unwrap_or_else(|e| e.into_inner());
                    let candidate = std::mem::take(&mut changed.candidate);
                    changed.committed.extend(candidate);
                    let mut effects = std::mem::take(&mut changed.candidate_effects);
                    // A deferred transaction containing only attachments skips
                    // commit_hook. SQLite's root rollback clears pending even
                    // without writes (!db->autoCommit in sqlite3RollbackAll).
                    // Thus surviving attachments at autocommit belong to this
                    // completed commit, never the next transaction's candidate.
                    effects.append(&mut changed.pending_effects);
                    if !effects.is_empty() {
                        changed
                            .confirmed_effects
                            .push(effects.into_iter().map(|(_, effect)| effect).collect());
                    }
                }
            }
        });
    }
}
impl<'a> Capture<'a> {
    pub(crate) fn begin(
        connection: &'a Connection,
        watches: &'a CommitWatches,
    ) -> rusqlite::Result<Self> {
        let capture = Self {
            connection,
            watches,
            changes: Arc::default(),
        };
        ACTIVE.with(|scopes| {
            assert!(
                scopes.borrow().is_empty(),
                "nested commit captures must reuse their root"
            );
            scopes.borrow_mut().push(Active {
                registry: std::ptr::from_ref(watches) as usize,
                connection: std::ptr::from_ref(connection),
                changes: Arc::downgrade(&capture.changes),
            });
        });
        if watches.has_subscribers() {
            install(connection, &capture.changes, true)?;
        }
        Ok(capture)
    }
    pub(crate) fn subscribed(watches: &CommitWatches, tables: &[String]) {
        ACTIVE.with(|scopes| {
            if let Some(scope) = scopes
                .borrow()
                .iter()
                .rev()
                .find(|s| s.registry == std::ptr::from_ref(watches) as usize)
                && let Some(changed) = scope.changes.upgrade()
            {
                // SAFETY: only the current thread can read this TLS entry. Its
                // Capture keeps the Connection borrowed and removes the entry
                // before that borrow ends, including on unwind. Registration runs
                // in the owning writer callback, outside SQLite hook execution.
                let connection = unsafe { &*scope.connection };
                install(connection, &changed, true).expect("install writer commit capture");
                // Earlier writes may predate the first subscriber and update hook.
                // A later root rollback clears these conservative table names.
                let mut changed = changed.lock().unwrap_or_else(|e| e.into_inner());
                if connection.is_autocommit() {
                    // An earlier autocommit may already have finished before the
                    // first subscriber. A later rollback cannot erase this hint.
                    changed.committed.extend(tables.iter().cloned());
                } else {
                    changed.tables.extend(tables.iter().cloned());
                }
            }
        });
    }
    pub(crate) fn defer(&self, effect: Effect) {
        install(self.connection, &self.changes, false).expect("install facade commit capture");
        let mut changed = self.changes.lock().unwrap_or_else(|e| e.into_inner());
        let id = changed.next_effect;
        changed.next_effect = id
            .checked_add(1)
            .expect("commit attachment sequence exhausted");
        changed.pending_effects.push((id, effect));
    }
    pub(crate) fn effect_scope(&self) -> EffectScope {
        let start = self
            .changes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .next_effect;
        EffectScope {
            changes: self.changes.clone(),
            start,
            accepted: false,
        }
    }
    pub(crate) fn take_effects(&self) -> Vec<Vec<Effect>> {
        std::mem::take(
            &mut self
                .changes
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .confirmed_effects,
        )
    }
    pub(crate) fn finish(self) {
        let tables: Vec<_> = self
            .changes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .committed
            .iter()
            .cloned()
            .collect();
        // A rolled-back savepoint may add extra tables. Root rollback never does.
        // This is the sole notification exit for UOW, raw SQL and callback COMMIT.
        self.watches.notify(&tables);
    }
}
// Paired with a savepoint owned by atomic/with_root. SQLite's rollback hook
// does not observe ROLLBACK TO, so the owner discards its uncommitted attachments.
// Never truncate confirmed effects: a callback can commit and begin another tx.
pub(crate) struct EffectScope {
    changes: Arc<Mutex<Changes>>,
    start: u64,
    accepted: bool,
}
impl EffectScope {
    pub(crate) fn accept(mut self) {
        self.accepted = true;
    }
}
impl Drop for EffectScope {
    fn drop(&mut self) {
        if !self.accepted {
            let mut changed = self.changes.lock().unwrap_or_else(|e| e.into_inner());
            changed.pending_effects.retain(|(id, _)| *id < self.start);
            changed.candidate_effects.retain(|(id, _)| *id < self.start);
        }
    }
}
impl Drop for Capture<'_> {
    fn drop(&mut self) {
        ACTIVE.with(|scopes| {
            scopes
                .borrow_mut()
                .retain(|s| !Weak::ptr_eq(&s.changes, &Arc::downgrade(&self.changes)))
        });
        if self
            .changes
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .installed
        {
            if self
                .changes
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .updates_installed
            {
                let _ = self
                    .connection
                    .update_hook(None::<fn(Action, &str, &str, i64)>);
            }
            let _ = self.connection.commit_hook(None::<fn() -> bool>);
            let _ = self.connection.rollback_hook(None::<fn()>);
            self.connection.trace_v2(TraceEventCodes::empty(), None);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn no_subscribers_does_not_install_or_replace_hooks() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE t(id INTEGER)").unwrap();
        let count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = count.clone();
        c.update_hook(Some(move |_: Action, _: &str, _: &str, _: i64| {
            seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }))
        .unwrap();
        let watches = CommitWatches::default();
        let capture = Capture::begin(&c, &watches).unwrap();
        assert!(!capture.changes.lock().unwrap().installed);
        c.execute("INSERT INTO t VALUES(1)", []).unwrap();
        capture.finish();
        c.execute("INSERT INTO t VALUES(2)", []).unwrap();
        assert_eq!(count.load(std::sync::atomic::Ordering::SeqCst), 2);
    }
}

#[cfg(test)]
mod bundled_observations {
    use super::*;
    #[test]
    fn update_hook_reports_replace_insert_but_not_implicit_delete_or_without_rowid() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE rowid_probe(id INTEGER PRIMARY KEY); INSERT INTO rowid_probe VALUES(1),(2); CREATE TABLE wr(id TEXT PRIMARY KEY) WITHOUT ROWID").unwrap();
        let events = Arc::new(Mutex::new(Vec::new()));
        let recorded = events.clone();
        c.update_hook(Some(move |action: Action, _: &str, table: &str, _: i64| {
            recorded.lock().unwrap().push((action, table.to_owned()));
        }))
        .unwrap();
        c.execute("DELETE FROM rowid_probe", []).unwrap();
        assert!(
            events.lock().unwrap().is_empty(),
            "known truncate blind spot, G2-R2"
        );
        c.execute("INSERT INTO rowid_probe VALUES(1)", []).unwrap();
        events.lock().unwrap().clear();
        c.execute("INSERT OR REPLACE INTO rowid_probe VALUES(1)", [])
            .unwrap();
        assert_eq!(
            *events.lock().unwrap(),
            vec![(Action::SQLITE_INSERT, "rowid_probe".into())],
            "REPLACE hints through INSERT, not its implicit deletion"
        );
        events.lock().unwrap().clear();
        c.execute("INSERT INTO wr VALUES('key')", []).unwrap();
        c.execute("DELETE FROM wr WHERE id='key'", []).unwrap();
        assert!(
            events.lock().unwrap().is_empty(),
            "WITHOUT ROWID is prohibited by the schema guard"
        );
    }
    #[test]
    fn fts_shadow_events_do_not_claim_logical_virtual_table_notifications() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE VIRTUAL TABLE fts_probe USING fts5(text)")
            .unwrap();
        let events = Arc::new(Mutex::new(BTreeSet::new()));
        let recorded = events.clone();
        c.update_hook(Some(move |_: Action, _: &str, table: &str, _: i64| {
            recorded.lock().unwrap().insert(table.to_owned());
        }))
        .unwrap();
        c.execute("INSERT INTO fts_probe(text) VALUES('term')", [])
            .unwrap();
        let tables = events.lock().unwrap();
        assert!(!tables.contains("fts_probe"));
        assert!(tables.contains("fts_probe_data"));
        assert!(tables.contains("fts_probe_docsize"));
        assert!(
            !tables.contains("fts_probe_config"),
            "WITHOUT ROWID FTS config is a shadow, not a domain table"
        );
    }
}

#[cfg(test)]
mod failed_candidates {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    struct Count(Arc<AtomicUsize>);
    impl steve_storage_api::CommitHint for Count {
        fn is_alive(&self) -> bool {
            true
        }
        fn notify(&self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }
    #[test]
    fn rejected_commit_candidate_is_cleared_but_keeps_earlier_success() {
        let c = Connection::open_in_memory().unwrap();
        c.execute_batch("CREATE TABLE t(id INTEGER PRIMARY KEY)")
            .unwrap();
        let watches = CommitWatches::default();
        let count = Arc::new(AtomicUsize::new(0));
        watches.subscribe_hint(&["t".into()], Arc::new(Count(count.clone())));
        let published = Arc::new(AtomicUsize::new(0));
        let attachment = || {
            Effect::new(
                0,
                published.clone(),
                |_, _| {},
                |seen| {
                    seen.fetch_add(1, Ordering::SeqCst);
                },
            )
        };
        for earlier_success in [true, false] {
            let capture = Capture::begin(&c, &watches).unwrap();
            if earlier_success {
                capture.defer(attachment());
                c.execute("INSERT INTO t VALUES(1)", []).unwrap();
            }
            // Inject failure after the production candidate transition. Unlike a
            // deferred FK violation, this actually reaches the commit hook; SQLite
            // then runs the real rollback and statement-finished callbacks.
            capture.defer(attachment());
            let changed = capture.changes.clone();
            c.commit_hook(Some(move || {
                changed.lock().unwrap().propose_commit();
                true
            }))
            .unwrap();
            assert!(c.execute("INSERT INTO t VALUES(2)", []).is_err());
            assert!(c.is_autocommit());
            assert_eq!(count.load(Ordering::SeqCst), usize::from(!earlier_success));
            publish_effects(capture.take_effects());
            assert_eq!(
                published.load(Ordering::SeqCst),
                1,
                "only the prior physical commit publishes its attachment"
            );
            capture.finish();
            assert_eq!(
                count.load(Ordering::SeqCst),
                1,
                "only the earlier durable insert can notify"
            );
        }
        assert_eq!(
            c.query_row("SELECT count(*) FROM t", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
    }
    #[test]
    fn busy_commit_keeps_transaction_open_and_cannot_confirm_a_candidate() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("busy.sqlite3");
        let c = Connection::open(&path).unwrap();
        c.execute_batch("CREATE TABLE t(id INTEGER PRIMARY KEY); INSERT INTO t VALUES(1)")
            .unwrap();
        c.busy_timeout(std::time::Duration::ZERO).unwrap();
        let reader = Connection::open(&path).unwrap();
        reader.execute_batch("BEGIN; SELECT * FROM t").unwrap();
        let watches = CommitWatches::default();
        let count = Arc::new(AtomicUsize::new(0));
        watches.subscribe_hint(&["t".into()], Arc::new(Count(count.clone())));
        let capture = Capture::begin(&c, &watches).unwrap();
        c.execute_batch("BEGIN IMMEDIATE; INSERT INTO t VALUES(2)")
            .unwrap();
        assert_eq!(
            c.execute_batch("COMMIT").unwrap_err().sqlite_error_code(),
            Some(rusqlite::ErrorCode::DatabaseBusy)
        );
        assert!(!c.is_autocommit());
        c.execute_batch("ROLLBACK").unwrap();
        capture.finish();
        assert_eq!(count.load(Ordering::SeqCst), 0);
        reader.execute_batch("ROLLBACK").unwrap();
    }
}
