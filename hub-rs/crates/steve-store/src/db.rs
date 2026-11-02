//! Opening the database and the migration runner (hub/src/db.ts).
//!
//! The migration strings are the TS ones byte for byte (extracted from db.ts), because SQLite keeps the
//! CREATE text in `sqlite_master` and both Hubs must be able to open the same file.

use std::path::Path;

use rusqlite::{Connection, OpenFlags, OptionalExtension};

use crate::{Result, StoreError};

/// `MIGRATIONS` from db.ts, in order. Index `i` brings the schema from version `i` to `i + 1`.
///
/// Adding a TS migration (1–3 are the TS strings): only once it is on TS main, so both Hubs agree on what the version means.
/// 1. copy the template literal byte for byte into `migrations/00N.sql` (the text between the backticks,
///    leading newline and indentation included) and append it here;
/// 2. regenerate `tests/fixtures/ts_schema.txt` by running the TS db.ts on an empty STEVE_HOME;
/// 3. add the new columns to the row types; `row_types_follow_column_order` and the prod-copy JSON
///    comparison fail until the types match.
pub const MIGRATIONS: &[&str] = &[
    include_str!("../migrations/001.sql"),
    include_str!("../migrations/002.sql"),
    include_str!("../migrations/003.sql"),
    // From here on the migrations belong to the new modules and exist only in the Rust Hub (TS Hubs open
    // such a database unchanged, see `migrate`). Numbering (modules/build-plan.md §0): 4 shared base +
    // contacts, 5 accounts & vault, 6 signals & automation + schedules, 7 channels + Feishu, 8 knowledge,
    // 9 email, 10 GitHub, 11 channel poll cursors, 12 spaces outlive their connection. 4–11 are
    // `IF NOT EXISTS` and alter nothing existing; 12 rebuilds `spaces` (see `migrate`). Once these
    // reach a production database they are frozen, changes go into a new migration.
    include_str!("../migrations/004.sql"),
    include_str!("../migrations/005.sql"),
    include_str!("../migrations/006.sql"),
    include_str!("../migrations/007.sql"),
    include_str!("../migrations/008.sql"),
    include_str!("../migrations/009.sql"),
    include_str!("../migrations/010.sql"),
    include_str!("../migrations/011.sql"),
    include_str!("../migrations/012.sql"),
    // 13 is a comment-only placeholder that stays empty (the delivery queue takes the next free number);
    // versions are positions in this list, so it keeps the later ones in place. 14: YOLO (modules/yolo.md
    // §6). 15: statistics. 16: thread routing (modules/thread-routing.md §7.1). All new tables only.
    include_str!("../migrations/013.sql"),
    include_str!("../migrations/014.sql"),
    include_str!("../migrations/015.sql"),
    include_str!("../migrations/016.sql"),
    // 17: reasoning effort (conditional columns in run_pending for TS Hub compatibility).
    include_str!("../migrations/017.sql"),
    // 18: ownership (modules/ownership.md §2): the `stewards` table and its one-time backfill.
    include_str!("../migrations/018.sql"),
    // 19: a space's attention (实时/不提醒/屏蔽) and voice, the `space_prefs` table (domain-model.md §1.4).
    include_str!("../migrations/019.sql"),
    // 20: IM reactions (`chan_reactions`, channels-personas.md §2.11).
    include_str!("../migrations/020.sql"),
    // 21: the durable outbox (`outbox_events`, consumers, receipts, leases; [`crate::outbox`]).
    include_str!("../migrations/021.sql"),
    // 22: lossless, session-owned large event payloads.
    include_str!("../migrations/022.sql"),
    // 23: Feishu / Lark app registrations (`chan_app_registrations`, the PersonalAgent bootstrap).
    include_str!("../migrations/023.sql"),
    // 24: durable restart intents and dispatch receipts.
    include_str!("../migrations/024.sql"),
];

/// The schema version this build writes (`MIGRATIONS.length`).
pub const SCHEMA_VERSION: i64 = MIGRATIONS.len() as i64;

/// Prepared statements kept per connection.
const STATEMENT_CACHE: usize = 512;

/// Pragmas every connection gets. journal_mode is persistent in the file; these are per connection.
pub(crate) fn pragmas(c: &Connection) -> rusqlite::Result<()> {
    c.pragma_update(None, "foreign_keys", "ON")?;
    c.pragma_update(None, "busy_timeout", 5000)?;
    // WAL + NORMAL: no fsync per commit (only at checkpoints), which is what lets the bus keep up with
    // thousands of writes a second. The database stays consistent; a power loss (not a crash) can
    // drop the last commits. Accepted by the user on 2026-11-01 (the default FULL synced every commit).
    c.pragma_update(None, "synchronous", "NORMAL")?;
    // the default of 16 cached statements is far fewer than the repositories use
    c.set_prepared_statement_cache_capacity(STATEMENT_CACHE);
    Ok(())
}

/// Opens (creating if needed) the write connection and migrates it.
pub(crate) fn open_writer(path: &Path) -> Result<Connection> {
    let c = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE
            | OpenFlags::SQLITE_OPEN_CREATE
            | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )?;
    let mode: String = c.pragma_update_and_check(None, "journal_mode", "WAL", |r| r.get(0))?;
    if !mode.eq_ignore_ascii_case("wal") {
        tracing::warn!(mode, "journal_mode WAL not accepted");
    }
    pragmas(&c)?;
    migrate(&c)?;
    Ok(c)
}

/// The db.ts runner verbatim: read the first `schema_version` row (0 if none), run the missing migrations
/// and record the new version, all in one transaction. Like TS, the UPDATE runs on every open.
///
/// A database newer than this build (e.g. after a release rollback) is opened as is, with a warning:
/// migrations are additive, and writing the version down would make the newer build rerun one.
///
/// Pending migrations run with `foreign_keys` off, because a table rebuild (012) drops a table other
/// tables reference; `foreign_key_check` must then be empty before the transaction commits. The
/// pragma cannot change inside a transaction, so it is switched around it and always restored.
pub fn migrate(c: &Connection) -> Result<()> {
    c.execute_batch("CREATE TABLE IF NOT EXISTS schema_version (v INTEGER NOT NULL)")?;
    let cur: i64 = c
        .query_row("SELECT v FROM schema_version", [], |r| r.get(0))
        .optional()?
        .unwrap_or(0);
    if cur > SCHEMA_VERSION {
        tracing::warn!(
            "database schema v{cur} is newer than this Hub (v{SCHEMA_VERSION}); opening it unchanged"
        );
        return Ok(());
    }
    if cur == SCHEMA_VERSION {
        return run_pending(c, cur);
    }
    let fks: i64 = c.query_row("PRAGMA foreign_keys", [], |r| r.get(0))?;
    c.pragma_update(None, "foreign_keys", "OFF")?;
    let ran = run_pending(c, cur);
    let restored = c.pragma_update(None, "foreign_keys", fks);
    ran?;
    restored?;
    tracing::info!(from = cur, to = SCHEMA_VERSION, "migrated database");
    Ok(())
}

/// The migrations after `cur` and the version row, in one transaction.
fn run_pending(c: &Connection, cur: i64) -> Result<()> {
    let tx = c.unchecked_transaction()?;
    for m in MIGRATIONS.iter().skip(cur.max(0) as usize) {
        tx.execute_batch(m)?;
    }
    for table in ["bots", "sessions"] {
        let present: bool = tx.query_row(
            &format!("SELECT EXISTS(SELECT 1 FROM pragma_table_info('{table}') WHERE name = 'reasoning_effort')"),
            [], |r| r.get(0),
        )?;
        if !present {
            tx.execute_batch(&format!(
                "ALTER TABLE {table} ADD COLUMN reasoning_effort TEXT"
            ))?;
        }
    }
    let broken: Vec<String> = if cur == SCHEMA_VERSION {
        Vec::new()
    } else {
        tx.prepare("SELECT \"table\" || ' -> ' || parent FROM pragma_foreign_key_check")?
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    if !broken.is_empty() {
        return Err(StoreError::Migration(format!(
            "foreign keys broken after migrating to v{SCHEMA_VERSION}: {}",
            broken.join(", ")
        )));
    }
    if cur == 0 {
        tx.execute(
            "INSERT INTO schema_version (v) VALUES (?)",
            [SCHEMA_VERSION],
        )?;
    } else {
        tx.execute("UPDATE schema_version SET v = ?", [SCHEMA_VERSION])?;
    }
    tx.commit()?;
    Ok(())
}
