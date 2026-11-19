//! Embedded, hash-locked migrations. No source checkout is needed at runtime.
use crate::{
    layout::{self, Layout, META_DOMAIN},
    open::{db_error, incompatible},
};
use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior};
use sha2::{Digest, Sha256};
use steve_types::Error;

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
    let unknown: u32 = conn
        .query_row(
            "SELECT count(*) FROM domain_schema WHERE domain NOT IN ('core','stream','comms')",
            [],
            |r| r.get(0),
        )
        .map_err(db_error)?;
    if unknown != 0 {
        return Err(incompatible("unknown domain schema"));
    }
    for set in CATALOG {
        let v = version(conn, set.domain)?;
        if v > head(set) || (exact && v != head(set)) {
            return Err(incompatible(
                "storage schema differs from this program; start or upgrade the Hub first",
            ));
        }
    }
    Ok(())
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
    verify_lock(set.migrations, set.lock)?;
    let current = version(conn, set.domain)?;
    if current > head(set) {
        return Err(incompatible("storage schema requires a newer program"));
    }
    if current == head(set) {
        return Ok(());
    }
    let tx: Transaction<'_> = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(db_error)?;
    for migration in set.migrations.iter().filter(|m| m.version > current) {
        tx.execute_batch(migration.sql).map_err(db_error)?;
    }
    stamp(&tx, set.domain, head(set))?;
    tx.commit().map_err(db_error)
}
/// No-op at the current head: it never rewrites version rows or planner statistics.
pub fn migrate(conn: &mut Connection) -> Result<(), Error> {
    verify_locks()?;
    check_versions(conn, false)?;
    for set in CATALOG {
        apply_set(conn, set)?;
    }
    Ok(())
}
