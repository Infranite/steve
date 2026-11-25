//! Opening the database and the migration runner.

use std::path::Path;
use std::sync::{Arc, Mutex};

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
    // 72: explicit personal notification policies and inbox, linked to E1 delivery receipts.
    include_str!("../migrations/072.sql"),
    include_str!("../migrations/073.sql"),
    // 74: persistent realtime output estimates and replay-safe block offsets.
    include_str!("../migrations/074.sql"),
    // 75: uncollected baselines, indexed coverage gaps and safe persisted stop boundaries.
    include_str!("../migrations/075.sql"),
    // 76: bound uncollected upgrade attempts with durable Node stop / unsent evidence.
    include_str!("../migrations/076.sql"),
    // 77: device configuration compare-and-swap revision.
    include_str!("../migrations/077.sql"),
    // 78–81: live attempt bills, durable gateway ownership/inbox and fair result recovery.
    include_str!("../migrations/078.sql"),
    include_str!("../migrations/079.sql"),
    include_str!("../migrations/080.sql"),
    include_str!("../migrations/081.sql"),
    // 82: immutable Hub-owned file deliveries and user-authorized expiring shares.
    include_str!("../migrations/082.sql"),
    // 83: a work unit's retries keep its child item; retired attempts are keyed by (item, attempt).
    include_str!("../migrations/083.sql"),
    // 84: a Node's choice whether its owner's sessions load the owner's personal harness config.
    include_str!("../migrations/084.sql"),
    include_str!("../migrations/085.sql"),
    // 86: the merge lane and the merge queue go (pull requests land changes); their open cards expire.
    include_str!("../migrations/086.sql"),
    // 87: deliveries without an item, their managing user and the interactive HTML switch.
    include_str!("../migrations/087.sql"),
    // 88: one space per platform chat: identities numbered per space with their platform and owner;
    // what each message is seen by.
    include_str!("../migrations/088.sql"),
    // 89: writes a channel refused for good (read-only connections, chats that refuse writes),
    // remembered for a day.
    include_str!("../migrations/089.sql"),
    // 90: the design gate is a switch on the item, off by default; only the owner turns it on.
    include_str!("../migrations/090.sql"),
    // 91: the model access setting for service sessions goes (harnesses use their own sign-in).
    include_str!("../migrations/091.sql"),
    // 92: the workflow statistics (#242): a session's first tool call; closing invalidations and pull
    // request merges become item marks (their timeline lines before, once).
    include_str!("../migrations/092.sql"),
    // 93: how far each user has read a channel conversation (unread counts in 对话).
    include_str!("../migrations/093.sql"),
    // 94: each Node's machine (`host`) and how its latest link arrived (`link`), for when it is away.
    include_str!("../migrations/094.sql"),
    // 95: the personal harness configuration default moves from each workspace to the Hub.
    include_str!("../migrations/095.sql"),
    // 96: per space, whether Steve replies for you on your own account (`space_prefs.reply_as_you`), and the
    // later message whose turn answers a message (`chan_messages.turn_of`).
    include_str!("../migrations/096.sql"),
    // 97: the composer's answer kept with a draft in your name (`chan_messages.composed`), per space what a bot's
    // message becomes on your own account (`space_prefs.bot_notices`), and how long an unanswered card about your
    // own account stays open (`channel_settings.card_ttl_h`, none by default).
    include_str!("../migrations/097.sql"),
    // 98: prospective final reply ordering/read state and scoped attention page capabilities (#290).
    include_str!("../migrations/098.sql"),
    // 99: todos become goals and dated todos (kind, parent, target date, date-only deadlines,
    // reminders, snooze, priority, star, recurrence).
    include_str!("../migrations/099.sql"),
    // 100: the calendar read cache: calendars per connection and person, events, attendees, meetings.
    include_str!("../migrations/100.sql"),
    // 101: a starred contact's message to you becomes a personal notification (`personal_notifications.kind`,
    // `message_id`; `notification_policies.starred_in_quiet`), and the feed reads it as its own kind (`feed_entries`
    // rebuilt to allow `starred`; 098's triggers on it created again).
    include_str!("../migrations/101.sql"),
    // 102: alarms (`time_alarms`, each fires at most once; `time_alarm_dirty`, the subjects to plan again, written
    // by triggers on todos); a personal notification's one topic (`personal_notifications.topic`, 101's `kind`
    // folded in and legacy), and the feed kind `reminder`.
    include_str!("../migrations/102.sql"),
    // 103: the morning brief: its schedule's alarms planned again by triggers, `briefings` (one per member
    // and local day), the topic `briefing.ready` and the feed kind `briefing`.
    include_str!("../migrations/103.sql"),
    // 104: shadow facts of an item's work (#327): delivery facts with their provenance, waits, judge
    // results and the operator's overrides of the named work policy. Nothing acts on them yet.
    include_str!("../migrations/104.sql"),
    // 105: what the sweep would do for each open item (#334), in shadow: a log of decisions keyed by
    // (item, cycle, session, turn, scope revision, action, reason, target). Nothing carries them out yet.
    include_str!("../migrations/105.sql"),
    // 106: the completion judge in shadow (#335): judge candidates per (item, cycle, session, turn,
    // scope revision) and what a judgement rests on. Nothing acts on them.
    include_str!("../migrations/106.sql"),
    include_str!("../migrations/107.sql"),
    // 108: removing a Node from its machine (#325): each removal its owner asked for, and where it
    // is. Additive: a new, empty table.
    include_str!("../migrations/108.sql"),
    // 109: immutable deliveries captured from replies keep their original session scope.
    include_str!("../migrations/109.sql"),
    // 110: opt-in Bot execution bindings and workspace Team presets.
    include_str!("../migrations/110.sql"),
    // 111: verified live receipts for personal chat triggers (no historical replay).
    include_str!("../migrations/111.sql"),
    include_str!("../migrations/112.sql"),
    include_str!("../migrations/113.sql"),
    include_str!("../migrations/114.sql"),
    // 115: Node events are applied as they are received; the inbox and the quarantine go.
    include_str!("../migrations/115.sql"),
    // 116: one dispatch model; the pre-Team automatic spread list goes.
    include_str!("../migrations/116.sql"),
    // 117: data older builds left in shapes nothing reads any more, rewritten or removed.
    include_str!("../migrations/117.sql"),
    // 118: typed command lookups and durable session permission modes.
    include_str!("../migrations/118.sql"),
    // 119: Node telemetry now lives only in the Hub's in-memory recorder.
    include_str!("../migrations/119.sql"),
    // 120: immutable Bot selection and session placement facts.
    include_str!("../migrations/120.sql"),
    // 121: retire Steve's automatic decisions and add native harness approval.
    include_str!("../migrations/121.sql"),
    // 122: ordinary Node errors, intent identity, stall evidence, and scoped review findings.
    include_str!("../migrations/122.sql"),
    // 123: item lifecycle and delivery cutover.
    include_str!("../migrations/123.sql"),
    // 124: decision revisions, choice receipts, natural keys, prompt templates, and typed undo.
    include_str!("../migrations/124.sql"),
    // 125: durable daily aggregates for low-cardinality telemetry counters.
    include_str!("../migrations/125.sql"),
    // 126: remove retired dynamic work-graph tables and steve_workflow tool pins.
    include_str!("../migrations/126.sql"),
    // 127: unified WaitContract columns, bounds, and due indexes across durable wait tables.
    include_str!("../migrations/127.sql"),
    // 128: Steve as coordinator (strip main bot from Teams, migrate steve_fallback, wait queue).
    include_str!("../migrations/128.sql"),
    // 129: link workspace members to contact parties, add decisions.resolved_by, backfill legacy actors.
    include_str!("../migrations/129.sql"),
    // 130: durable thread submit keys, reply tasks/receipts, dispatch/delivery refs, and typed notices.
    include_str!("../migrations/130.sql"),
    // 131: redefine knowledge: project AGENTS.md proposals, reclassify facts, withdraw items/chat.
    include_str!("../migrations/131.sql"),
    // 132: per-Bot ordered fallback execution surfaces for automatic handoff.
    include_str!("../migrations/132.sql"),
    // 133: installation-wide daily backup policy.
    include_str!("../migrations/133.sql"),
    // 134: remove legacy raw ACP event rows and backfill sessions.first_tool_at from steve.v1 tool events.
    include_str!("../migrations/134.sql"),
    // 135: migrate retired YOLO rows to Steve settlement and backfill bot_node_bindings.
    include_str!("../migrations/135.sql"),
    // 136: migrate bots.perm_mode to Steve PermissionMode values and default to ask.
    include_str!("../migrations/136.sql"),
    // 137: drop retired bots.project_dir, bots.worktree, and items.project_dir columns.
    include_str!("../migrations/137.sql"),
    // 138: clear cached harness update state persisted before typed update reasons.
    include_str!("../migrations/138.sql"),
    // 139: add mems.pinned for priority-and-budget personal context assembly.
    include_str!("../migrations/139.sql"),
    // 140: canonicalize reasoning_effort to Steve's five levels and add sessions.effort_mapping.
    include_str!("../migrations/140.sql"),
    // 141: dynamic script workflow ledger (`wf_runs`, `wf_script_versions`, `wf_calls`, `wf_call_attempts`, `wf_version_calls`).
    include_str!("../migrations/141.sql"),
    // 142: one-time retirement of non-terminal legacy dynamic work-graph child items (`source[1] = "workflow"`).
    include_str!("../migrations/142.sql"),
    // 143: rebind work.tidy consumer to work.item.lifecycle so all close kinds clean worktrees.
    include_str!("../migrations/143.sql"),
];

/// 027's item columns and their one-time backfill (applied by `run_pending` when `items.phase` is missing).
pub const ITEM_LIFECYCLE: &str = include_str!("../migrations/027_items.sql");

/// The schema version this build writes (`MIGRATIONS.length`).
pub const SCHEMA_VERSION: i64 = MIGRATIONS.len() as i64;

/// Where a database stands: its schema and the oldest schema a build must know to open it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct Compat {
    pub schema: i64,
    pub min_reader: i64,
}

impl crate::Store {
    /// Schema compatibility stamps from the same snapshot of the opened database.
    pub fn schema_compat(&self) -> Result<Compat> {
        self.sqlite_read(|c| crate::read_snapshot(c, compat))
    }
}

/// Read the on-disk compatibility stamp, including committed WAL frames.
pub fn inspect_schema(path: &Path) -> Result<Compat> {
    if !path
        .try_exists()
        .map_err(|e| StoreError::Storage(e.to_string()))?
    {
        return Ok(Compat {
            schema: 0,
            min_reader: 0,
        });
    }
    inspect_header(path, |c| {
        let (schema, stamp) = read_schema(c)?;
        if schema != 0 && stamp.is_none() {
            return Err(StoreError::Storage(format!(
                "database schema {schema} has no schema_compat stamp; nothing was changed"
            )));
        }
        Ok(Compat {
            schema,
            min_reader: stamp.unwrap_or(0),
        })
    })
}

/// Identify the version of a database that will only be used to make a migrated copy.
pub fn inspect_schema_version(path: &Path) -> Result<i64> {
    inspect_header(path, schema_version)
}

fn inspect_header<T>(path: &Path, read: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
    let c = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    // Read the table definitions and their stamps from one snapshot.
    let tx = c.unchecked_transaction()?;
    read(&tx)
}

#[cfg(any(test, feature = "testkit"))]
pub mod testkit {
    use super::*;

    /// Write the schema header of a disposable upgrade fixture.
    pub fn set_schema(path: &Path, schema: i64, min_reader: Option<i64>) -> Result<()> {
        let mut c = Connection::open(path)?;
        let tx = c.transaction()?;
        tx.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_version(v BIGINT NOT NULL);
             CREATE TABLE IF NOT EXISTS schema_compat(min_reader BIGINT NOT NULL);
             DELETE FROM schema_version;
             DELETE FROM schema_compat;",
        )?;
        tx.execute("INSERT INTO schema_version VALUES(?)", [schema])?;
        if let Some(min_reader) = min_reader {
            tx.execute("INSERT INTO schema_compat VALUES(?)", [min_reader])?;
        }
        tx.commit()?;
        Ok(())
    }
}

#[cfg(test)]
#[test]
fn inspect_schema_reads_uncheckpointed_commits() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("store.sqlite3");
    testkit::set_schema(&path, 42, Some(42)).unwrap();
    let mut writer = Connection::open(&path).unwrap();
    writer.pragma_update(None, "journal_mode", "WAL").unwrap();
    let tx = writer.transaction().unwrap();
    tx.execute_batch("UPDATE schema_version SET v=43; UPDATE schema_compat SET min_reader=43;")
        .unwrap();
    assert_eq!(inspect_schema(&path).unwrap().schema, 42);
    tx.commit().unwrap();

    assert_eq!(
        inspect_schema(&path).unwrap(),
        Compat {
            schema: 43,
            min_reader: 43
        }
    );
    assert!(path.with_extension("sqlite3-wal").exists());
}

/// Read [`Compat`] off a connection. A database no build stamped yet (before 0.2.14) is readable only by
/// builds that know its schema.
pub fn compat(c: &Connection) -> Result<Compat> {
    let (schema, stamp) = read_schema(c)?;
    Ok(Compat {
        schema,
        min_reader: stamp.unwrap_or(schema),
    })
}

fn read_schema(c: &Connection) -> Result<(i64, Option<i64>)> {
    let schema = schema_version(c)?;
    let stamped = if has_table(c, "schema_compat")? {
        c.query_row("SELECT min_reader FROM schema_compat", [], |r| r.get(0))
            .optional()?
    } else {
        None
    };
    Ok((schema, stamped))
}

fn schema_version(c: &Connection) -> Result<i64> {
    if !has_table(c, "schema_version")? {
        return Ok(0);
    }
    Ok(
        c.query_row("SELECT v FROM schema_version", [], |r| r.get(0))
            .optional()?
            .unwrap_or(0),
    )
}

fn has_table(c: &Connection, name: &str) -> Result<bool> {
    Ok(c.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type='table' AND name=?)",
        [name],
        |r| r.get(0),
    )?)
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

/// Administrative writer: existing files only, strict read-only header validation, no migration.
/// Do not change journal mode or checkpoint on close: even a list must leave an existing WAL's
/// pending frames alone, rather than copying them into the main database on the last close.
pub(crate) fn open_admin_writer(path: &Path) -> Result<Connection> {
    let c = Connection::open_with_flags(
        path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(|e| StoreError::AdminSchema(e.to_string()))?;
    c.set_db_config(
        rusqlite::config::DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE,
        true,
    )?;
    let header = (|| -> rusqlite::Result<_> {
        let schema: (i64, i64) =
            c.query_row("SELECT min(v), count(*) FROM schema_version", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?;
        let compat: (i64, i64) = c.query_row(
            "SELECT min(min_reader), count(*) FROM schema_compat",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok((schema, compat))
    })()
    .map_err(|e| StoreError::AdminSchema(e.to_string()))?;
    let ((schema, versions), (min_reader, stamps)) = header;
    if versions != 1
        || stamps != 1
        || schema != SCHEMA_VERSION
        || !(1..=SCHEMA_VERSION).contains(&min_reader)
    {
        return Err(StoreError::AdminSchema(format!(
            "schema v{schema}, min_reader v{min_reader}, rows {versions}/{stamps}; expected schema v{SCHEMA_VERSION}"
        )));
    }
    // These pragmas affect this connection only. In particular there is no WAL switch or optimize.
    pragmas(&c)?;
    Ok(c)
}

/// Opens (creating if needed) the write connection and migrates it.
pub(crate) fn open_writer(path: &Path, observer: Option<MigrationObserver>) -> Result<Connection> {
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
    migrate_observed(&c, observer)?;
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
    migrate_observed(c, None)
}

/// Apply pending migrations to an existing offline database, such as a scratch backup copy.
pub fn migrate_file(path: &Path) -> Result<()> {
    let c = Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_WRITE)?;
    migrate(&c)
}

/// Actual SQLite execution progress, including inside a single long backfill statement. This is
/// optional and carries no filesystem or installer dependency. `complete` means the transaction
/// committed, not that the candidate is healthy. A failed migration never reports completion.
#[derive(Clone, Copy, Debug)]
pub struct MigrationProgress {
    pub migration: u32,
    pub target: u32,
    pub vm_steps: u64,
    pub complete: bool,
}
pub type MigrationObserver = Arc<dyn Fn(MigrationProgress) + Send + Sync>;

struct MigrationTracker<'a> {
    connection: &'a Connection,
    observer: MigrationObserver,
    state: Arc<Mutex<MigrationProgress>>,
}
impl<'a> MigrationTracker<'a> {
    fn new(connection: &'a Connection, observer: MigrationObserver) -> Result<Self> {
        let state = Arc::new(Mutex::new(MigrationProgress {
            migration: 0,
            target: SCHEMA_VERSION as u32,
            vm_steps: 0,
            complete: false,
        }));
        let (progress, report) = (state.clone(), observer.clone());
        connection.progress_handler(
            100_000,
            Some(move || {
                let snapshot = {
                    let mut state = progress.lock().unwrap_or_else(|e| e.into_inner());
                    state.vm_steps = state.vm_steps.saturating_add(100_000);
                    *state
                };
                report(snapshot);
                false
            }),
        )?;
        Ok(Self {
            connection,
            observer,
            state,
        })
    }
    fn report(&self, migration: u32, complete: bool) {
        let snapshot = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.migration = migration;
            state.complete = complete;
            *state
        };
        (self.observer)(snapshot);
    }
}
impl Drop for MigrationTracker<'_> {
    fn drop(&mut self) {
        let _ = self.connection.progress_handler(0, None::<fn() -> bool>);
    }
}

fn migrate_observed(c: &Connection, observer: Option<MigrationObserver>) -> Result<()> {
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
    let tracker = observer.map(|o| MigrationTracker::new(c, o)).transpose()?;
    if let Some(progress) = &tracker {
        progress.report((cur + 1).clamp(1, SCHEMA_VERSION) as u32, false);
    }
    if cur == SCHEMA_VERSION {
        run_pending(c, cur, tracker.as_ref())?;
        if let Some(progress) = &tracker {
            progress.report(SCHEMA_VERSION as u32, true);
        }
        return Ok(());
    }
    let fks: i64 = c.query_row("PRAGMA foreign_keys", [], |r| r.get(0))?;
    c.pragma_update(None, "foreign_keys", "OFF")?;
    let ran = run_pending(c, cur, tracker.as_ref());
    let restored = c.pragma_update(None, "foreign_keys", fks);
    ran?;
    restored?;
    if let Some(progress) = &tracker {
        progress.report(SCHEMA_VERSION as u32, true);
    }
    tracing::info!(from = cur, to = SCHEMA_VERSION, "migrated database");
    Ok(())
}

fn add_reasoning_effort(c: &Connection) -> Result<()> {
    for table in ["bots", "sessions"] {
        let present: bool = c.query_row(
            &format!("SELECT EXISTS(SELECT 1 FROM pragma_table_info('{table}') WHERE name = 'reasoning_effort')"),
            [], |r| r.get(0),
        )?;
        if !present {
            c.execute_batch(&format!(
                "ALTER TABLE {table} ADD COLUMN reasoning_effort TEXT"
            ))?;
        }
    }
    Ok(())
}

fn ensure_item_lifecycle(c: &Connection) -> Result<()> {
    /* 027's item columns: once, where there are items without them (a module-only schema has no items) */
    let items: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'items')",
        [],
        |r| r.get(0),
    )?;
    if items {
        let phase: bool = c.query_row(
            "SELECT EXISTS(SELECT 1 FROM pragma_table_info('items') WHERE name = 'phase')",
            [],
            |r| r.get(0),
        )?;
        if !phase {
            c.execute_batch(ITEM_LIFECYCLE)?;
        }
        c.execute_batch(
            "CREATE INDEX IF NOT EXISTS items_closing ON items(phase) WHERE phase = 'closing'",
        )?;
    }
    Ok(())
}

/// The migrations after `cur` and the version row, in one transaction. IMMEDIATE: another process
/// (the upgrade supervisor) may open the same file, and the write lock is taken up front, where the
/// busy timeout applies, instead of at the first write.
fn run_pending(c: &Connection, cur: i64, progress: Option<&MigrationTracker<'_>>) -> Result<()> {
    let tx = Transaction::new_unchecked(c, TransactionBehavior::Immediate)?;
    for (i, m) in MIGRATIONS.iter().enumerate().skip(cur.max(0) as usize) {
        if let Some(progress) = progress {
            progress.report((i + 1) as u32, false);
        }
        if i == 119 {
            // Historical migrations added reasoning effort after the numbered SQL. Finish it
            // before adding Bot specialties so fresh and upgraded databases have the same order.
            add_reasoning_effort(&tx)?;
        }
        if i == 121 {
            ensure_item_lifecycle(&tx)?;
        }
        if i == 135 || i == 137 {
            // 136 and 138 rebuild `bots` referencing `reasoning_effort`; ensure the column exists
            // even if the database started at v120..v137 without having run the i == 119 hook.
            add_reasoning_effort(&tx)?;
        }
        tx.execute_batch(m)?;
    }
    ensure_item_lifecycle(&tx)?;
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

#[cfg(test)]
mod migration_progress_tests {
    use super::*;

    fn old_database() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        for m in &MIGRATIONS[..73] {
            c.execute_batch(m).unwrap();
        }
        c.execute_batch("CREATE TABLE schema_version(v INTEGER NOT NULL); INSERT INTO schema_version VALUES(73);
            CREATE TABLE schema_compat(min_reader INTEGER NOT NULL); INSERT INTO schema_compat VALUES(73);
            INSERT INTO workspaces(id,name,created) VALUES('fixture','Fixture',1);
            INSERT INTO sessions(id,workspace_id,harness,cwd,status,created,updated) VALUES('session','fixture','fixture','.','idle',1,1);
            INSERT INTO runtime_sessions(session_id,node_id,workspace_ref,node_session_id,config_revision,phase,updated)
                VALUES('session','node','fixture','session','1','idle',1);
            INSERT INTO node_streams(node_id,stream,epoch) VALUES('node','events','epoch');").unwrap();
        for n in 0..30 {
            c.execute("INSERT INTO runtime_turns(turn_id,session_id,state,created,updated) VALUES(?1,'session','finished',1,1)", [format!("t{n}")]).unwrap();
            c.execute("INSERT INTO runtime_attempts(attempt_id,turn_id,ordinal,node_id,operation_id,execution,spec,digest,state,created,updated)
                VALUES(?1,?2,0,'node',?3,'fixture','{}','fixture','finished',1,1)", [format!("a{n}"), format!("t{n}"), format!("o{n}")]).unwrap();
        }
        c.execute_batch("WITH RECURSIVE numbers(n) AS (VALUES(0) UNION ALL SELECT n+1 FROM numbers WHERE n<3999)
            INSERT INTO node_event_inbox SELECT 'node','events','epoch',n+1,'e'||n,
                json_object('subject',json_object('of','execution','type','turn','session','session','turn','t'||n,'attempt','a'||n),
                    'body',json_object('type',CASE WHEN n<30 THEN 'attempt_ended' ELSE 'text_delta' END)),1,n+1 FROM numbers;
            ANALYZE; PRAGMA foreign_keys=ON;").unwrap();
        c
    }

    #[test]
    fn reports_vm_work_inside_original_076_and_completion_only_after_commit() {
        let c = old_database();
        let records = Arc::new(Mutex::new(Vec::new()));
        let observed = records.clone();
        migrate_observed(
            &c,
            Some(Arc::new(move |p| observed.lock().unwrap().push(p))),
        )
        .unwrap();
        let seen = records.lock().unwrap();
        let inside: Vec<_> = seen.iter().filter(|p| p.migration == 76).collect();
        assert!(
            inside.windows(2).any(|p| p[1].vm_steps > p[0].vm_steps),
            "no evidence within migration 076: {inside:?}"
        );
        assert!(
            seen.windows(2)
                .all(|p| p[1].vm_steps >= p[0].vm_steps && p[1].migration >= p[0].migration)
        );
        assert!(seen.last().unwrap().complete);
        assert!(seen[..seen.len() - 1].iter().all(|p| !p.complete));
        assert_eq!(compat(&c).unwrap().schema, SCHEMA_VERSION);
        assert_eq!(
            c.query_row(
                "SELECT count(*) FROM realtime_output_attempts WHERE stop_evidence='node_event'",
                [],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
            30
        );
    }

    #[test]
    fn failed_migration_rolls_back_restores_foreign_keys_and_removes_the_callback() {
        let c = old_database();
        for m in &MIGRATIONS[73..75] {
            c.execute_batch(m).unwrap();
        }
        c.execute_batch(
            "UPDATE schema_version SET v=75; UPDATE schema_compat SET min_reader=75;
            ALTER TABLE realtime_output_attempts ADD COLUMN stop_evidence TEXT;",
        )
        .unwrap();
        let records = Arc::new(Mutex::new(Vec::new()));
        let observed = records.clone();
        assert!(
            migrate_observed(
                &c,
                Some(Arc::new(move |p| observed.lock().unwrap().push(p)))
            )
            .is_err()
        );
        assert_eq!(compat(&c).unwrap().schema, 75);
        assert_eq!(
            c.pragma_query_value(None, "foreign_keys", |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        let count = records.lock().unwrap().len();
        assert!(records.lock().unwrap().iter().all(|p| !p.complete));
        c.query_row("WITH RECURSIVE n(x) AS (VALUES(1) UNION ALL SELECT x+1 FROM n WHERE x<100000) SELECT sum(x) FROM n", [], |r| r.get::<_, i64>(0)).unwrap();
        assert_eq!(
            records.lock().unwrap().len(),
            count,
            "observer leaked beyond the failed migration"
        );
    }
}

/// Process-wide SQLite settings, before the first connection: no memory statistics. With them on (the
/// default), every SQLite allocation takes one global mutex, which serializes the read pool.
pub fn tune_sqlite() {
    // SAFETY: sqlite3_config is only valid before sqlite3_initialize; this runs first thing in main
    let rc = unsafe {
        rusqlite::ffi::sqlite3_config(
            rusqlite::ffi::SQLITE_CONFIG_MEMSTATUS,
            0 as std::os::raw::c_int,
        )
    };
    if rc != rusqlite::ffi::SQLITE_OK {
        tracing::warn!("sqlite3_config(MEMSTATUS) = {rc}");
    }
}
