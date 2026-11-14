//! Opening the database and the migration runner.

use std::path::Path;

use rusqlite::{Connection, OpenFlags, OptionalExtension, Transaction, TransactionBehavior};

use crate::{Result, StoreError};

/// The migrations, in order. Index `i` brings the schema from version `i` to `i + 1`. A released one is
/// frozen (`migrations/LOCK`); a change goes into a new migration, with its columns added to the row
/// types (`row_types_follow_column_order` fails until they match).
pub const MIGRATIONS: &[&str] = &[
    include_str!("../migrations/001.sql"),
    include_str!("../migrations/002.sql"),
    include_str!("../migrations/003.sql"),
    // 1–3 are the schema of the TypeScript Hub this one replaced. Numbering from there: 4 shared base +
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
    // versions are positions in this list, so it keeps the later ones in place. 14: YOLO.
    // 15: statistics. 16: thread routing. All new tables only.
    include_str!("../migrations/013.sql"),
    include_str!("../migrations/014.sql"),
    include_str!("../migrations/015.sql"),
    include_str!("../migrations/016.sql"),
    // 17: reasoning effort (conditional columns in run_pending for TS Hub compatibility).
    include_str!("../migrations/017.sql"),
    // 18: ownership: the `stewards` table and its one-time backfill.
    include_str!("../migrations/018.sql"),
    // 19: a space's attention (实时/不提醒/屏蔽) and voice, the `space_prefs` table.
    include_str!("../migrations/019.sql"),
    // 20: IM reactions (`chan_reactions`).
    include_str!("../migrations/020.sql"),
    // 21: the durable outbox (`outbox_events`, consumers, receipts, leases; [`crate::outbox`]).
    include_str!("../migrations/021.sql"),
    // 22: lossless, session-owned large event payloads.
    include_str!("../migrations/022.sql"),
    // 23: Feishu / Lark app registrations (`chan_app_registrations`, the PersonalAgent bootstrap).
    include_str!("../migrations/023.sql"),
    // 24: durable restart intents and dispatch receipts.
    include_str!("../migrations/024.sql"),
    // 25: provider lookup results and retry schedule.
    include_str!("../migrations/025.sql"),
    // 26: personal conversation-list preferences.
    include_str!("../migrations/026.sql"),
    // 27: the item lifecycle (phase, cycle, revision, closing intent, outcome) and its closed cycles.
    include_str!("../migrations/027.sql"),
    // 28: GitHub deliveries `received` until processed, with their source and payload.
    include_str!("../migrations/028.sql"),
    // 29: approved GitHub writes as rows, with an unknown state.
    include_str!("../migrations/029.sql"),
    // 30: what a channel send needs to be retried and accounted for.
    include_str!("../migrations/030.sql"),
    // 31: merge lanes: one per target, their requests and units (the merge window).
    include_str!("../migrations/031.sql"),
    // 32: model prices synced from Models.dev and the state of the sync.
    include_str!("../migrations/032.sql"),
    // 33: outputs delivered as records of the work, and the user's notes on outputs (ST-38).
    include_str!("../migrations/033.sql"),
    // 34: Node dispatch intents, execution projections and durable event inbox.
    include_str!("../migrations/034.sql"),
    // 35: indexes only, for lookups that scanned whole tables (send queue, tokens, subjects, stats polls).
    include_str!("../migrations/035.sql"),
    // 36: indexes for the coordinator's lookups in the Node event inbox and its pruning by age.
    include_str!("../migrations/036.sql"),
    // 37: Node permission cards (expiry, one answer each) and a Node session's desired config.
    include_str!("../migrations/037.sql"),
    // 38: paired Nodes, pairing codes (hashed) and the Node audit trail (N2).
    include_str!("../migrations/038.sql"),
    // 39: per-stream fault and the quarantine of conflicting Node events (N1-6).
    include_str!("../migrations/039.sql"),
    // 40: Node jobs (think calls, research runs), their one-model attempts, and commands that target them (N0d).
    include_str!("../migrations/040.sql"),
    // 41: an extension adapter's own poll cursors (`chan_cursors.token`).
    include_str!("../migrations/041.sql"),
    // 42: the connection a message was sent through, the provider's create time of a send, and
    // the remote's delivery ids per connection (`chan_message_seen`).
    include_str!("../migrations/042.sql"),
    // 43: the content key of a message (matching copies without a provider id) and the connection
    // that supplied a copy without one.
    include_str!("../migrations/043.sql"),
    // 44: the `message` row that began a Node message, by session and message id (a text delta's role).
    include_str!("../migrations/044.sql"),
    // 45: when a failing outbox delivery began failing; archived sessions compacted under keepSess.
    include_str!("../migrations/045.sql"),
    // 46: Steve's own seeded project releases through the merge lane, not ops/release.sh.
    include_str!("../migrations/046.sql"),
    // 47: a channel account's undo window before an approved message goes out (`send_delay_s`).
    include_str!("../migrations/047.sql"),
    // 48: the model a turn is booked under (`agent_turns.model`), for the rebuild and the hourly view.
    include_str!("../migrations/048.sql"),
    // 49: the inbound watermark: when a connection was last turned on, how late a channel message
    // may arrive, the history counts, and which messages were only recorded as history.
    include_str!("../migrations/049.sql"),
    // 50: path model S1: who a run works for (`items.requested_by`, `runtime_sessions.principal`,
    // `audience`), `bots.owner`, and `node_grants`, `node_roots`, `node_env`, with their backfill.
    include_str!("../migrations/050.sql"),
    // 51: durable multi-Bot work graphs, their unit attempts and replanning history.
    include_str!("../migrations/051.sql"),
    // 52: per-item prerequisite waiting and durable automatic continuation.
    include_str!("../migrations/052.sql"),
    // 53: path model S2: each Node session's view and its difference from today's roots, in shadow.
    include_str!("../migrations/053.sql"),
    // 54: bounded, item-scoped diagnostic capture and metadata.
    include_str!("../migrations/054.sql"),
    // 55: access tokens and the clients signed in with them (who connects, with what, by which route).
    include_str!("../migrations/055.sql"),
    include_str!("../migrations/056.sql"),
    include_str!("../migrations/057.sql"),
    include_str!("../migrations/058.sql"),
    include_str!("../migrations/059.sql"),
    // 59: Steve's own calls booked under their harness adapter (`claude-cli` → `claude-code`).
    include_str!("../migrations/060.sql"),
    // 60: a research run's session under a neutral name, with the harness adapter it belongs to.
    include_str!("../migrations/061.sql"),
    // 62: a Bot's own MCP servers (`bot_mcp_servers`); their secrets stay in the vault.
    include_str!("../migrations/062.sql"),
    // 63: a Bot's skills (`bot_skills`), written where its harness reads them when a session opens.
    include_str!("../migrations/063.sql"),
    // 64: what agents changed in the memories (`mem_changes`) and merges (`mem_supersedes`).
    include_str!("../migrations/064.sql"),
    // 65: the capabilities library: MCP servers, skills and A2A agents per workspace, assigned per
    // Bot (`bot_capabilities`); 62's and 63's rows move into it.
    include_str!("../migrations/065.sql"),
    // 66: how each session's last turn ended and whether you stopped it (`session_stops`), and each
    // turn by attempt (`session_turns`): its end judged once, a pause's stop kept.
    include_str!("../migrations/066.sql"),
    // 67: each device's concurrent sessions (`max_sessions`), enforced by the Hub: 32 by default,
    // 1–256, on the Hub's `devices` row and on each paired Node's `nodes` row.
    include_str!("../migrations/067.sql"),
    // 68: deterministic conversation digest intake and durable knowledge indexes.
    include_str!("../migrations/068.sql"),
    // 69: tool search: what each Bot keeps loaded (`bot_tool_pins`) and how often each tool was found
    // and called (`tool_usage`).
    include_str!("../migrations/069.sql"),
    // 70: expiring connector references bound to workspace, connection and account.
    include_str!("../migrations/070.sql"),
    // 71: durable output intents and manual/learned channel preferences.
    include_str!("../migrations/071.sql"),
];

/// 027's item columns and their one-time backfill (applied by `run_pending` when `items.phase` is missing).
pub const ITEM_LIFECYCLE: &str = include_str!("../migrations/027_items.sql");

/// The schema version this build writes (`MIGRATIONS.length`).
pub const SCHEMA_VERSION: i64 = MIGRATIONS.len() as i64;

/// Where a database stands: its schema and the oldest schema a build must know to open it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Compat {
    pub schema: i64,
    pub min_reader: i64,
}

/// Read [`Compat`] off a connection. A database no build stamped yet (before 0.2.14) is readable only by
/// builds that know its schema.
pub fn compat(c: &Connection) -> Result<Compat> {
    let has = |t: &str| -> Result<bool> {
        Ok(c.query_row(
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?)",
            [t],
            |r| r.get(0),
        )?)
    };
    let schema: i64 = if has("schema_version")? {
        c.query_row("SELECT v FROM schema_version", [], |r| r.get(0))
            .optional()?
            .unwrap_or(0)
    } else {
        0
    };
    let stamped: Option<i64> = if has("schema_compat")? {
        c.query_row("SELECT min_reader FROM schema_compat", [], |r| r.get(0))
            .optional()?
    } else {
        None
    };
    Ok(Compat {
        schema,
        min_reader: stamped.unwrap_or(schema),
    })
}

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
    // sorts and GROUP BYs build their temp b-trees in memory instead of temp files; reads of the file
    // go through a shared mapping instead of a copy per connection
    c.pragma_update(None, "temp_store", "MEMORY")?;
    c.pragma_update(None, "mmap_size", MMAP_SIZE)?;
    // 16 MiB page cache per reader (the default is 2 MB); the writer gets more in `open_writer`
    c.pragma_update(None, "cache_size", -16_384)?;
    Ok(())
}

/// Bytes of the database file mapped into memory per connection.
const MMAP_SIZE: i64 = 256 * 1024 * 1024;

/// What the WAL file is truncated back to after a checkpoint (unbounded by default, so one spike such
/// as a migration or a big prune would keep it large for good).
const JOURNAL_SIZE_LIMIT: i64 = 64 * 1024 * 1024;

/// Refreshes the planner's statistics where they are missing or stale (`PRAGMA optimize`), bounded so
/// it stays cheap on a large database. Only on the writer: the readers are read-only.
pub(crate) fn optimize(c: &Connection, at_open: bool) -> rusqlite::Result<()> {
    // 0x10002 also looks at tables never analyzed, which is what a database opened for the first
    // time after an upgrade needs; afterwards the plain form only redoes tables that changed a lot
    c.execute_batch(if at_open {
        "PRAGMA analysis_limit = 400; PRAGMA optimize = 0x10002;"
    } else {
        "PRAGMA analysis_limit = 400; PRAGMA optimize;"
    })
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
    c.pragma_update(None, "cache_size", -32_768)?;
    c.pragma_update(None, "journal_size_limit", JOURNAL_SIZE_LIMIT)?;
    migrate(&c)?;
    optimize(&c, true)?;
    Ok(c)
}

/// The db.ts runner verbatim: read the first `schema_version` row (0 if none), run the missing migrations
/// and record the new version, all in one transaction. Like TS, the UPDATE runs on every open.
///
/// A database is opened only when this build knows the schema recorded in `schema_compat` (the newest
/// any build that opened it knew): one a newer build migrated is refused before anything is written, so an
/// old binary (a rollback) can't misread or damage it. A database whose recorded schema is newer than its
/// `schema_version` (none so far) is opened as is: writing the version down would make the newer build
/// rerun a migration.
///
/// Pending migrations run with `foreign_keys` off, because a table rebuild (012) drops a table other
/// tables reference; `foreign_key_check` must then be empty before the transaction commits. The
/// pragma cannot change inside a transaction, so it is switched around it and always restored.
pub fn migrate(c: &Connection) -> Result<()> {
    let Compat {
        schema: cur,
        min_reader: needs,
    } = compat(c)?;
    if needs > SCHEMA_VERSION {
        return Err(StoreError::Incompatible {
            schema: cur,
            min_reader: needs,
            ours: SCHEMA_VERSION,
        });
    }
    c.execute_batch(
        "CREATE TABLE IF NOT EXISTS schema_version (v INTEGER NOT NULL);
         CREATE TABLE IF NOT EXISTS schema_compat (min_reader INTEGER NOT NULL);",
    )?;
    if cur > SCHEMA_VERSION {
        tracing::warn!(
            "database schema v{cur} is newer than this Hub (v{SCHEMA_VERSION}) but asks only for v{needs}; opening it unchanged"
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

/// The migrations after `cur` and the version row, in one transaction. IMMEDIATE: another process
/// (the upgrade supervisor) may open the same file, and the write lock is taken up front, where the
/// busy timeout applies, instead of at the first write.
fn run_pending(c: &Connection, cur: i64) -> Result<()> {
    let tx = Transaction::new_unchecked(c, TransactionBehavior::Immediate)?;
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
    /* 027's item columns: once, where there are items without them (a module-only schema has no items) */
    let items: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'items')",
        [],
        |r| r.get(0),
    )?;
    if items {
        let phase: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('items') WHERE name = 'phase')",
            [],
            |r| r.get(0),
        )?;
        if !phase {
            tx.execute_batch(ITEM_LIFECYCLE)?;
        }
        tx.execute_batch(
            "CREATE INDEX IF NOT EXISTS items_closing ON items(phase) WHERE phase = 'closing'",
        )?;
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
    // every migration needs a build that knows it (letting older builds use one would also need a record
    // of what they wrote since, which nothing keeps); the stamp only ever rises
    let stamp = compat(&tx)?.min_reader.max(SCHEMA_VERSION);
    tx.execute("DELETE FROM schema_compat", [])?;
    tx.execute("INSERT INTO schema_compat (min_reader) VALUES (?)", [stamp])?;
    tx.commit()?;
    Ok(())
}
