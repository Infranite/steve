//! Frozen legacy schema replay, for conversion and baseline verification only.
//! It deliberately excludes connection pragmas, pools, diagnostics and background workers.

use rusqlite::{Connection, OptionalExtension, Result, Transaction, TransactionBehavior};

pub(crate) const MIGRATIONS: &[&str] = &[
    include_str!("../legacy/migrations/001.sql"),
    include_str!("../legacy/migrations/002.sql"),
    include_str!("../legacy/migrations/003.sql"),
    // 1–3 are the schema of the TypeScript Hub this one replaced. Numbering from there: 4 shared base +
    // contacts, 5 accounts & vault, 6 signals & automation + schedules, 7 channels + Feishu, 8 knowledge,
    // 9 email, 10 GitHub, 11 channel poll cursors, 12 spaces outlive their connection. 4–11 are
    // `IF NOT EXISTS` and alter nothing existing; 12 rebuilds `spaces` (see `migrate`). Once these
    // reach a production database they are frozen, changes go into a new migration.
    include_str!("../legacy/migrations/004.sql"),
    include_str!("../legacy/migrations/005.sql"),
    include_str!("../legacy/migrations/006.sql"),
    include_str!("../legacy/migrations/007.sql"),
    include_str!("../legacy/migrations/008.sql"),
    include_str!("../legacy/migrations/009.sql"),
    include_str!("../legacy/migrations/010.sql"),
    include_str!("../legacy/migrations/011.sql"),
    include_str!("../legacy/migrations/012.sql"),
    // 13 is a comment-only placeholder that stays empty (the delivery queue takes the next free number);
    // versions are positions in this list, so it keeps the later ones in place. 14: YOLO.
    // 15: statistics. 16: thread routing. All new tables only.
    include_str!("../legacy/migrations/013.sql"),
    include_str!("../legacy/migrations/014.sql"),
    include_str!("../legacy/migrations/015.sql"),
    include_str!("../legacy/migrations/016.sql"),
    // 17: reasoning effort (conditional columns in run_pending for TS Hub compatibility).
    include_str!("../legacy/migrations/017.sql"),
    // 18: ownership: the `stewards` table and its one-time backfill.
    include_str!("../legacy/migrations/018.sql"),
    // 19: a space's attention (实时/不提醒/屏蔽) and voice, the `space_prefs` table.
    include_str!("../legacy/migrations/019.sql"),
    // 20: IM reactions (`chan_reactions`).
    include_str!("../legacy/migrations/020.sql"),
    // 21: the durable outbox (`outbox_events`, consumers, receipts, leases; [`crate::outbox`]).
    include_str!("../legacy/migrations/021.sql"),
    // 22: lossless, session-owned large event payloads.
    include_str!("../legacy/migrations/022.sql"),
    // 23: Feishu / Lark app registrations (`chan_app_registrations`, the PersonalAgent bootstrap).
    include_str!("../legacy/migrations/023.sql"),
    // 24: durable restart intents and dispatch receipts.
    include_str!("../legacy/migrations/024.sql"),
    // 25: provider lookup results and retry schedule.
    include_str!("../legacy/migrations/025.sql"),
    // 26: personal conversation-list preferences.
    include_str!("../legacy/migrations/026.sql"),
    // 27: the item lifecycle (phase, cycle, revision, closing intent, outcome) and its closed cycles.
    include_str!("../legacy/migrations/027.sql"),
    // 28: GitHub deliveries `received` until processed, with their source and payload.
    include_str!("../legacy/migrations/028.sql"),
    // 29: approved GitHub writes as rows, with an unknown state.
    include_str!("../legacy/migrations/029.sql"),
    // 30: what a channel send needs to be retried and accounted for.
    include_str!("../legacy/migrations/030.sql"),
    // 31: merge lanes: one per target, their requests and units (the merge window).
    include_str!("../legacy/migrations/031.sql"),
    // 32: model prices synced from Models.dev and the state of the sync.
    include_str!("../legacy/migrations/032.sql"),
    // 33: outputs delivered as records of the work, and the user's notes on outputs (ST-38).
    include_str!("../legacy/migrations/033.sql"),
    // 34: Node dispatch intents, execution projections and durable event inbox.
    include_str!("../legacy/migrations/034.sql"),
    // 35: indexes only, for lookups that scanned whole tables (send queue, tokens, subjects, stats polls).
    include_str!("../legacy/migrations/035.sql"),
    // 36: indexes for the coordinator's lookups in the Node event inbox and its pruning by age.
    include_str!("../legacy/migrations/036.sql"),
    // 37: Node permission cards (expiry, one answer each) and a Node session's desired config.
    include_str!("../legacy/migrations/037.sql"),
    // 38: paired Nodes, pairing codes (hashed) and the Node audit trail (N2).
    include_str!("../legacy/migrations/038.sql"),
    // 39: per-stream fault and the quarantine of conflicting Node events (N1-6).
    include_str!("../legacy/migrations/039.sql"),
    // 40: Node jobs (think calls, research runs), their one-model attempts, and commands that target them (N0d).
    include_str!("../legacy/migrations/040.sql"),
    // 41: an extension adapter's own poll cursors (`chan_cursors.token`).
    include_str!("../legacy/migrations/041.sql"),
    // 42: the connection a message was sent through, the provider's create time of a send, and
    // the remote's delivery ids per connection (`chan_message_seen`).
    include_str!("../legacy/migrations/042.sql"),
    // 43: the content key of a message (matching copies without a provider id) and the connection
    // that supplied a copy without one.
    include_str!("../legacy/migrations/043.sql"),
    // 44: the `message` row that began a Node message, by session and message id (a text delta's role).
    include_str!("../legacy/migrations/044.sql"),
    // 45: when a failing outbox delivery began failing; archived sessions compacted under keepSess.
    include_str!("../legacy/migrations/045.sql"),
    // 46: Steve's own seeded project releases through the merge lane, not ops/release.sh.
    include_str!("../legacy/migrations/046.sql"),
    // 47: a channel account's undo window before an approved message goes out (`send_delay_s`).
    include_str!("../legacy/migrations/047.sql"),
    // 48: the model a turn is booked under (`agent_turns.model`), for the rebuild and the hourly view.
    include_str!("../legacy/migrations/048.sql"),
    // 49: the inbound watermark: when a connection was last turned on, how late a channel message
    // may arrive, the history counts, and which messages were only recorded as history.
    include_str!("../legacy/migrations/049.sql"),
    // 50: path model S1: who a run works for (`items.requested_by`, `runtime_sessions.principal`,
    // `audience`), `bots.owner`, and `node_grants`, `node_roots`, `node_env`, with their backfill.
    include_str!("../legacy/migrations/050.sql"),
    // 51: durable multi-Bot work graphs, their unit attempts and replanning history.
    include_str!("../legacy/migrations/051.sql"),
    // 52: per-item prerequisite waiting and durable automatic continuation.
    include_str!("../legacy/migrations/052.sql"),
    // 53: path model S2: each Node session's view and its difference from today's roots, in shadow.
    include_str!("../legacy/migrations/053.sql"),
    // 54: bounded, item-scoped diagnostic capture and metadata.
    include_str!("../legacy/migrations/054.sql"),
    // 55: access tokens and the clients signed in with them (who connects, with what, by which route).
    include_str!("../legacy/migrations/055.sql"),
    include_str!("../legacy/migrations/056.sql"),
    include_str!("../legacy/migrations/057.sql"),
    include_str!("../legacy/migrations/058.sql"),
    include_str!("../legacy/migrations/059.sql"),
    // 59: Steve's own calls booked under their harness adapter (`claude-cli` → `claude-code`).
    include_str!("../legacy/migrations/060.sql"),
    // 60: a research run's session under a neutral name, with the harness adapter it belongs to.
    include_str!("../legacy/migrations/061.sql"),
    // 62: a Bot's own MCP servers (`bot_mcp_servers`); their secrets stay in the vault.
    include_str!("../legacy/migrations/062.sql"),
    // 63: a Bot's skills (`bot_skills`), written where its harness reads them when a session opens.
    include_str!("../legacy/migrations/063.sql"),
    // 64: what agents changed in the memories (`mem_changes`) and merges (`mem_supersedes`).
    include_str!("../legacy/migrations/064.sql"),
    // 65: the capabilities library: MCP servers, skills and A2A agents per workspace, assigned per
    // Bot (`bot_capabilities`); 62's and 63's rows move into it.
    include_str!("../legacy/migrations/065.sql"),
    // 66: how each session's last turn ended and whether you stopped it (`session_stops`), and each
    // turn by attempt (`session_turns`): its end judged once, a pause's stop kept.
    include_str!("../legacy/migrations/066.sql"),
    // 67: each device's concurrent sessions (`max_sessions`), enforced by the Hub: 32 by default,
    // 1–256, on the Hub's `devices` row and on each paired Node's `nodes` row.
    include_str!("../legacy/migrations/067.sql"),
    // 68: deterministic conversation digest intake and durable knowledge indexes.
    include_str!("../legacy/migrations/068.sql"),
    // 69: tool search: what each Bot keeps loaded (`bot_tool_pins`) and how often each tool was found
    // and called (`tool_usage`).
    include_str!("../legacy/migrations/069.sql"),
    // 70: expiring connector references bound to workspace, connection and account.
    include_str!("../legacy/migrations/070.sql"),
    // 71: durable output intents and manual/learned channel preferences.
    include_str!("../legacy/migrations/071.sql"),
    // 72: explicit personal notification policies and inbox, linked to E1 delivery receipts.
    include_str!("../legacy/migrations/072.sql"),
    include_str!("../legacy/migrations/073.sql"),
    // 74: persistent realtime output estimates and replay-safe block offsets.
    include_str!("../legacy/migrations/074.sql"),
    // 75: uncollected baselines, indexed coverage gaps and safe persisted stop boundaries.
    include_str!("../legacy/migrations/075.sql"),
    // 76: bound uncollected upgrade attempts with durable Node stop / unsent evidence.
    include_str!("../legacy/migrations/076.sql"),
    // 77: device configuration compare-and-swap revision.
    include_str!("../legacy/migrations/077.sql"),
    // 78–81: live attempt bills, durable gateway ownership/inbox and fair result recovery.
    include_str!("../legacy/migrations/078.sql"),
    include_str!("../legacy/migrations/079.sql"),
    include_str!("../legacy/migrations/080.sql"),
    include_str!("../legacy/migrations/081.sql"),
    // 82: immutable Hub-owned file deliveries and user-authorized expiring shares.
    include_str!("../legacy/migrations/082.sql"),
    // 83: a work unit's retries keep its child item; retired attempts are keyed by (item, attempt).
    include_str!("../legacy/migrations/083.sql"),
    // 84: a Node's choice whether its owner's sessions load the owner's personal harness config.
    include_str!("../legacy/migrations/084.sql"),
    include_str!("../legacy/migrations/085.sql"),
    // 86: the merge lane and the merge queue go (pull requests land changes); their open cards expire.
    include_str!("../legacy/migrations/086.sql"),
    // 87: deliveries without an item, their managing user and the interactive HTML switch.
    include_str!("../legacy/migrations/087.sql"),
    // 88: one space per platform chat: identities numbered per space with their platform and owner;
    // what each message is seen by.
    include_str!("../legacy/migrations/088.sql"),
    // 89: writes a channel refused for good (read-only connections, chats that refuse writes),
    // remembered for a day.
    include_str!("../legacy/migrations/089.sql"),
    // 90: the design gate is a switch on the item, off by default; only the owner turns it on.
    include_str!("../legacy/migrations/090.sql"),
    // 91: the model access setting for service sessions goes (harnesses use their own sign-in).
    include_str!("../legacy/migrations/091.sql"),
    // 92: the workflow statistics (#242): a session's first tool call; closing invalidations and pull
    // request merges become item marks (their timeline lines before, once).
    include_str!("../legacy/migrations/092.sql"),
    // 93: how far each user has read a channel conversation (unread counts in 对话).
    include_str!("../legacy/migrations/093.sql"),
    // 94: each Node's machine (`host`) and how its latest link arrived (`link`), for when it is away.
    include_str!("../legacy/migrations/094.sql"),
    // 95: the personal harness configuration default moves from each workspace to the Hub.
    include_str!("../legacy/migrations/095.sql"),
    // 96: per space, whether Steve replies for you on your own account (`space_prefs.reply_as_you`), and the
    // later message whose turn answers a message (`chan_messages.turn_of`).
    include_str!("../legacy/migrations/096.sql"),
    // 97: the composer's answer kept with a draft in your name (`chan_messages.composed`), per space what a bot's
    // message becomes on your own account (`space_prefs.bot_notices`), and how long an unanswered card about your
    // own account stays open (`channel_settings.card_ttl_h`, none by default).
    include_str!("../legacy/migrations/097.sql"),
    // 98: prospective final reply ordering/read state and scoped attention page capabilities (#290).
    include_str!("../legacy/migrations/098.sql"),
    // 99: todos become goals and dated todos (kind, parent, target date, date-only deadlines,
    // reminders, snooze, priority, star, recurrence).
    include_str!("../legacy/migrations/099.sql"),
    // 100: the calendar read cache: calendars per connection and person, events, attendees, meetings.
    include_str!("../legacy/migrations/100.sql"),
    // 101: a starred contact's message to you becomes a personal notification (`personal_notifications.kind`,
    // `message_id`; `notification_policies.starred_in_quiet`), and the feed reads it as its own kind (`feed_entries`
    // rebuilt to allow `starred`; 098's triggers on it created again).
    include_str!("../legacy/migrations/101.sql"),
    // 102: alarms (`time_alarms`, each fires at most once; `time_alarm_dirty`, the subjects to plan again, written
    // by triggers on todos); a personal notification's one topic (`personal_notifications.topic`, 101's `kind`
    // folded in and legacy), and the feed kind `reminder`.
    include_str!("../legacy/migrations/102.sql"),
    // 103: the morning brief: its schedule's alarms planned again by triggers, `briefings` (one per member
    // and local day), the topic `briefing.ready` and the feed kind `briefing`.
    include_str!("../legacy/migrations/103.sql"),
    // 104: shadow facts of an item's work (#327): delivery facts with their provenance, waits, judge
    // results and the operator's overrides of the named work policy. Nothing acts on them yet.
    include_str!("../legacy/migrations/104.sql"),
    // 105: what the sweep would do for each open item (#334), in shadow: a log of decisions keyed by
    // (item, cycle, session, turn, scope revision, action, reason, target). Nothing carries them out yet.
    include_str!("../legacy/migrations/105.sql"),
    // 106: the completion judge in shadow (#335): judge candidates per (item, cycle, session, turn,
    // scope revision) and what a judgement rests on. Nothing acts on them.
    include_str!("../legacy/migrations/106.sql"),
    include_str!("../legacy/migrations/107.sql"),
    // 108: removing a Node from its machine (#325): each removal its owner asked for, and where it
    // is. Additive: a new, empty table.
    include_str!("../legacy/migrations/108.sql"),
    // 109: immutable deliveries captured from replies keep their original session scope.
    include_str!("../legacy/migrations/109.sql"),
    // 110: opt-in Bot execution bindings and workspace Team presets.
    include_str!("../legacy/migrations/110.sql"),
    // 111: verified live receipts for personal chat triggers (no historical replay).
    include_str!("../legacy/migrations/111.sql"),
    include_str!("../legacy/migrations/112.sql"),
    // 113: external-channel messages that can never be accepted keep their input (dead letters).
    include_str!("../legacy/migrations/113.sql"),
    include_str!("../legacy/migrations/114.sql"),
];

/// 027's item columns and their one-time backfill (applied by `run_pending` when `items.phase` is missing).
pub(crate) const ITEM_LIFECYCLE: &str = include_str!("../legacy/migrations/027_items.sql");

/// The schema version this build writes (`MIGRATIONS.length`).
pub(crate) const SCHEMA_VERSION: i64 = MIGRATIONS.len() as i64;

/// Reproduce the schema and seeds of the old `Store::open` on an empty database.
pub fn final_structure(conn: &Connection) -> Result<()> {
    let objects: i64 = conn.query_row(
        "SELECT count(*) FROM sqlite_schema WHERE name NOT LIKE 'sqlite_%'",
        [],
        |r| r.get(0),
    )?;
    if objects != 0 {
        return Err(rusqlite::Error::InvalidParameterName(
            "legacy baseline requires an empty database".into(),
        ));
    }
    migrate(conn, 0)
}

/// Replay pending migrations using the old runner's schema semantics.
/// Structural fingerprint verification belongs to the converter's following verification phase.
/// No historical support window is imposed; an unstamped empty database starts at zero.
pub fn migrate_to_head(conn: &Connection) -> Result<()> {
    let present: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='schema_version')",
        [],
        |r| r.get(0),
    )?;
    let (count, cur): (i64, i64) = if present {
        conn.query_row(
            "SELECT count(*), coalesce(min(v),0) FROM schema_version",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?
    } else {
        (0, 0)
    };
    if count > 1
        || !(0..=SCHEMA_VERSION).contains(&cur)
        || minimum_reader(conn, cur)? > SCHEMA_VERSION
    {
        return Err(rusqlite::Error::InvalidParameterName(format!(
            "cannot migrate legacy schema {cur} with head {SCHEMA_VERSION}"
        )));
    }
    migrate(conn, cur)
}

fn minimum_reader(c: &Connection, fallback: i64) -> Result<i64> {
    let present: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE type='table' AND name='schema_compat')",
        [],
        |r| r.get(0),
    )?;
    if present {
        Ok(
            c.query_row("SELECT min_reader FROM schema_compat", [], |r| r.get(0))
                .optional()?
                .unwrap_or(fallback),
        )
    } else {
        Ok(fallback)
    }
}

fn migrate(c: &Connection, cur: i64) -> Result<()> {
    // The caller must not already own a transaction: foreign_keys cannot change within one.
    if !c.is_autocommit() {
        return Err(rusqlite::Error::InvalidParameterName(
            "legacy migration requires autocommit".into(),
        ));
    }
    let fks: i64 = c.query_row("PRAGMA foreign_keys", [], |r| r.get(0))?;
    if cur != SCHEMA_VERSION {
        c.pragma_update(None, "foreign_keys", "OFF")?;
    }
    let ran = (|| {
        // Keep the old metadata DDL, including its sqlite_schema spelling.
        c.execute_batch(
            "CREATE TABLE IF NOT EXISTS schema_version (v INTEGER NOT NULL);
         CREATE TABLE IF NOT EXISTS schema_compat (min_reader INTEGER NOT NULL);",
        )?;
        run_pending(c, cur)
    })();
    let restored = c.pragma_update(None, "foreign_keys", fks);
    ran?;
    restored
}

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
        return Err(rusqlite::Error::InvalidParameterName(format!(
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
    let stamp = minimum_reader(&tx, cur)?.max(SCHEMA_VERSION);
    tx.execute("DELETE FROM schema_compat", [])?;
    tx.execute("INSERT INTO schema_compat (min_reader) VALUES (?)", [stamp])?;
    tx.commit()?;
    Ok(())
}
