//! Embedded, hash-locked migrations. No source checkout is needed at runtime.
use crate::{
    layout::{self, Layout, META_DOMAIN},
    open::{db_error, incompatible},
};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior};
use sha2::{Digest, Sha256};
use steve_types::Error;

/// Monotonic storage work stages within an observed call, not persisted schema versions.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Phase {
    LegacyReplay,
    Validation,
    InstallContract,
    DomainMigration,
}
/// Cumulative VM work within one observed call. complete is emitted only after
/// that call succeeds and its last transaction commits; it never asserts health.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Progress {
    pub phase: Phase,
    pub domain: Option<&'static str>,
    pub vm_steps: u64,
    pub complete: bool,
}
pub type Observer = std::sync::Arc<dyn Fn(Progress) + Send + Sync>;
/// Observers must be quick and nonblocking. This owns the connection's progress
/// handler for the call, removing it on success, error or unwind. No raw SQL/data
/// is sent to the observer. Replacing an existing handler is not supported.
pub(crate) struct ProgressTracker<'a> {
    connection: &'a Connection,
    observer: Observer,
    state: std::sync::Arc<std::sync::Mutex<Progress>>,
}
const PROGRESS_VM_STEPS: i32 = 10_000;
impl<'a> ProgressTracker<'a> {
    pub(crate) fn new(
        connection: &'a Connection,
        observer: Observer,
        phase: Phase,
    ) -> Result<Self, Error> {
        let state = std::sync::Arc::new(std::sync::Mutex::new(Progress {
            phase,
            domain: None,
            vm_steps: 0,
            complete: false,
        }));
        let (progress, report) = (state.clone(), observer.clone());
        connection
            .progress_handler(
                PROGRESS_VM_STEPS,
                Some(move || {
                    let snapshot = {
                        let mut s = progress.lock().unwrap_or_else(|e| e.into_inner());
                        s.vm_steps = s.vm_steps.saturating_add(PROGRESS_VM_STEPS as u64);
                        *s
                    };
                    report(snapshot);
                    false
                }),
            )
            .map_err(db_error)?;
        let tracker = Self {
            connection,
            observer,
            state,
        };
        tracker.report(phase, None, false);
        Ok(tracker)
    }
    pub(crate) fn report(&self, phase: Phase, domain: Option<&'static str>, complete: bool) {
        let snapshot = {
            let mut s = self.state.lock().unwrap_or_else(|e| e.into_inner());
            s.phase = phase;
            s.domain = domain;
            s.complete = complete;
            *s
        };
        (self.observer)(snapshot);
    }
    pub(crate) fn finish(&self) {
        let mut snapshot = *self.state.lock().unwrap_or_else(|e| e.into_inner());
        snapshot.complete = true;
        (self.observer)(snapshot);
    }
}
impl Drop for ProgressTracker<'_> {
    fn drop(&mut self) {
        let _ = self.connection.progress_handler(0, None::<fn() -> bool>);
    }
}

/// Contract schema is tracked in meta so domain_schema retains exactly three domain rows.
pub const CONTRACT_SCHEMA_KEY: &str = "contract_schema";
#[derive(Clone, Copy, Debug)]
pub struct Migration {
    pub version: u32,
    pub name: &'static str,
    pub sql: &'static str,
}
#[derive(Clone, Copy, Debug)]
pub struct MigrationSet {
    pub domain: &'static str,
    pub migrations: &'static [Migration],
    pub lock: &'static str,
}
macro_rules! migrations {
    ($domain:literal, $file:literal, $sql:expr) => {
        MigrationSet {
            domain: $domain,
            migrations: &[Migration {
                version: 1,
                name: $file,
                sql: $sql,
            }],
            lock: include_str!(concat!("../schema/", $domain, "/LOCK")),
        }
    };
}
/// The contract must exist before stamping domain versions; domain order preserves the baseline.
pub const CATALOG: &[MigrationSet] = &[
    migrations!("contract", "0001.sql", crate::schema::CONTRACT),
    migrations!("core", "0001_baseline.sql", crate::schema::CORE),
    migrations!("stream", "0001_baseline.sql", crate::schema::STREAM),
    migrations!("comms", "0001_baseline.sql", crate::schema::COMMS),
];
/// Reject missing, extra, reordered or modified migrations before any persistent action.
pub fn verify_lock(migrations: &[Migration], lock: &str) -> Result<(), Error> {
    let lines: Vec<_> = lock.lines().collect();
    if lines.len() != migrations.len() || migrations.is_empty() {
        return Err(incompatible(
            "migration LOCK entries do not match embedded migrations",
        ));
    }
    for (i, (migration, line)) in migrations.iter().zip(lines).enumerate() {
        let hash: String = Sha256::digest(migration.sql.as_bytes())
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        if migration.version as usize != i + 1
            || migration.name.get(..4) != Some(&format!("{:04}", migration.version))
            || line != format!("{} {hash}", migration.name)
        {
            return Err(incompatible(
                "embedded migration does not match its LOCK hash or version",
            ));
        }
    }
    Ok(())
}
pub fn verify_locks() -> Result<(), Error> {
    for set in CATALOG {
        verify_lock(set.migrations, set.lock)?;
    }
    Ok(())
}
fn head(set: &MigrationSet) -> u32 {
    set.migrations.last().map_or(0, |m| m.version)
}
fn version(conn: &Connection, domain: &str) -> Result<u32, Error> {
    if domain == "contract" {
        layout::meta(conn, META_DOMAIN, CONTRACT_SCHEMA_KEY)?
            .parse()
            .map_err(|_| incompatible("invalid contract schema version"))
    } else {
        let v: Option<u32> = conn
            .query_row(
                "SELECT version FROM domain_schema WHERE domain=?1",
                [domain],
                |r| r.get(0),
            )
            .optional()
            .map_err(db_error)?;
        Ok(v.unwrap_or(0))
    }
}
fn stamp(conn: &Connection, domain: &str, version: u32) -> Result<(), Error> {
    if domain == "contract" {
        layout::put_meta(conn, META_DOMAIN, CONTRACT_SCHEMA_KEY, &version.to_string())
    } else {
        conn.execute("INSERT INTO domain_schema(domain,version) VALUES(?1,?2) ON CONFLICT(domain) DO UPDATE SET version=excluded.version", rusqlite::params![domain,version]).map_err(db_error)?;
        Ok(())
    }
}
/// Conversion uses this after installing the complete current structure, inside its transaction.
pub fn stamp_current(conn: &Connection) -> Result<(), Error> {
    for set in CATALOG {
        stamp(conn, set.domain, head(set))?;
    }
    Ok(())
}
/// Read-only preflight. Admin requires exact heads; runtime permits only known older schemas.
pub fn check_versions(conn: &Connection, exact: bool) -> Result<(), Error> {
    let state = crate::open::inspect(conn)?;
    if !matches!(state, crate::compat::StorageState::Current { .. }) {
        return Err(incompatible(
            "storage is not current; start or upgrade the Hub first",
        ));
    }
    crate::compat::StorageCapabilities::compiled().can_open(&state, exact)
}
/// Atomically initialize all domains and the manifest; a failed initial schema leaves no
/// half-initialized metadata. Each domain's pending migrations are contained in this transaction.
pub fn initialize(conn: &mut Connection, layout: &Layout) -> Result<(), Error> {
    verify_locks()?;
    let tx = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(db_error)?;
    for set in CATALOG {
        for migration in set.migrations {
            tx.execute_batch(migration.sql).map_err(db_error)?;
        }
    }
    stamp_current(&tx)?;
    layout.write_file_meta(&tx, layout::CORE_FILE)?;
    tx.commit().map_err(db_error)
}
/// Apply a domain's entire pending set in one BEGIN IMMEDIATE. Exposed for migration tests
/// and future embedded sets; callers must preflight all sets before opening writable files.
pub fn apply_set(conn: &mut Connection, set: &MigrationSet) -> Result<(), Error> {
    apply_set_on(conn, set)
}
/// Apply one set with live VM progress; completion follows the set's commit.
pub fn apply_set_observed(
    conn: &mut Connection,
    set: &MigrationSet,
    observer: Observer,
) -> Result<(), Error> {
    let progress = ProgressTracker::new(conn, observer, Phase::DomainMigration)?;
    progress.report(Phase::DomainMigration, Some(set.domain), false);
    apply_set_on(conn, set)?;
    progress.finish();
    Ok(())
}
fn apply_set_on(conn: &Connection, set: &MigrationSet) -> Result<(), Error> {
    verify_lock(set.migrations, set.lock)?;
    let current = version(conn, set.domain)?;
    if current > head(set) {
        return Err(incompatible("storage schema requires a newer program"));
    }
    if current == head(set) {
        return Ok(());
    }
    // Public entry points hold an exclusive mutable connection; observed calls
    // additionally share it with their progress tracker. Internal calls run in
    // sequence, outside transactions. SQLite rejects nesting; RAII rolls back
    // failures, so new_unchecked only relaxes the Rust borrow requirement.
    let tx: Transaction<'_> =
        Transaction::new_unchecked(conn, TransactionBehavior::Immediate).map_err(db_error)?;
    for migration in set.migrations.iter().filter(|m| m.version > current) {
        tx.execute_batch(migration.sql).map_err(db_error)?;
    }
    stamp(&tx, set.domain, head(set))?;
    tx.commit().map_err(db_error)
}
/// No-op at the current head: it never rewrites version rows or planner statistics.
pub fn migrate(conn: &mut Connection) -> Result<(), Error> {
    migrate_on(conn, None)
}
/// Observed domain migration, separately callable after layout conversion. It
/// reports one cumulative VM counter across all domains and completes only after
/// every pending domain transaction succeeds. A no-op head still reports success.
pub fn migrate_observed(conn: &mut Connection, observer: Observer) -> Result<(), Error> {
    let progress = ProgressTracker::new(conn, observer, Phase::DomainMigration)?;
    migrate_on(conn, Some(&progress))?;
    progress.finish();
    Ok(())
}
pub(crate) fn migrate_on(
    conn: &Connection,
    progress: Option<&ProgressTracker<'_>>,
) -> Result<(), Error> {
    verify_locks()?;
    check_versions(conn, false)?;
    for set in CATALOG {
        if let Some(progress) = progress {
            progress.report(Phase::DomainMigration, Some(set.domain), false);
        }
        apply_set_on(conn, set)?;
    }
    Ok(())
}
