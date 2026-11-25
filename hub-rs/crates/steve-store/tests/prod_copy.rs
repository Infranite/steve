//! Checks against a backup of the production database. Skipped unless `STEVE_STORE_PROD_COPY` points at one:
//!
//!   sqlite3 ~/.steve/data/steve.sqlite3 ".backup '/tmp/x/steve.sqlite3'"
//!   STEVE_STORE_PROD_COPY=/tmp/x/steve.sqlite3 cargo test -p steve-store --test prod_copy
//!
//! The test copies that file again into its own temp dir, so the given copy is never written.

use steve_store::testkit::SqliteFixtures;
mod common;

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};
use std::process::Command;

use common::*;
use steve_store::views::{DecisionPage, Page, SettledCursor};
use steve_store::*;

fn prod_copy() -> Option<(tempfile::TempDir, PathBuf)> {
    let src = PathBuf::from(std::env::var_os("STEVE_STORE_PROD_COPY")?);
    let home = std::env::var("HOME").unwrap_or_default();
    let real = std::fs::canonicalize(&src).unwrap();
    assert!(
        !real.starts_with(Path::new(&home).join(".steve")),
        "point STEVE_STORE_PROD_COPY at a backup, never at ~/.steve"
    );
    let dir = tempfile::tempdir().unwrap();
    let dst = dir.path().join("steve.sqlite3");
    // `.backup` again rather than a file copy, so a -wal next to the source is included.
    let st = Command::new("sqlite3")
        .arg(&real)
        .arg(format!(".backup '{}'", dst.display()))
        .status()
        .unwrap();
    assert!(st.success());
    Some((dir, dst))
}

fn cli(path: &Path, sql: &str) -> String {
    let out = Command::new("sqlite3").arg(path).arg(sql).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

fn cli_count(path: &Path, sql: &str) -> usize {
    cli(path, sql).parse().unwrap()
}

/// Every non-internal table in `sqlite_master` paired with its current row count.
/// Checking all tables rather than a fixed early list ensures migrations that drop, create,
/// or prune newer tables (e.g. `item_operations`, `session_resume`, `item_dependency_waits`)
/// cannot silently lose or fabricate rows.
fn table_counts(c: &rusqlite::Connection) -> BTreeMap<String, usize> {
    let mut st = c
        .prepare(
            "SELECT name FROM sqlite_master
             WHERE type = 'table' AND name NOT LIKE 'sqlite_%'
             ORDER BY name",
        )
        .unwrap();
    let names: Vec<String> = st
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    names
        .into_iter()
        .map(|name| {
            let n = count(c, &format!("SELECT count(*) FROM \"{name}\"")) as usize;
            (name, n)
        })
        .collect()
}

/// Expected schema-table and row-count changes made on purpose by migrations 120–124.
/// Every other table present before migration must survive with its exact row count.
#[derive(Default)]
struct MigrationChanges {
    dropped_tables: BTreeSet<String>,
    created_tables: BTreeMap<String, usize>,
    removed_rows: BTreeMap<String, usize>,
    added_rows: BTreeMap<String, usize>,
}

impl MigrationChanges {
    fn drop_table(&mut self, table: &str) {
        self.dropped_tables.insert(table.to_owned());
    }

    fn create_table(&mut self, table: &str, rows: usize) {
        self.created_tables.insert(table.to_owned(), rows);
    }

    fn remove_rows(&mut self, table: &str, n: usize) {
        if n > 0 {
            *self.removed_rows.entry(table.to_owned()).or_default() += n;
        }
    }

    fn add_rows(&mut self, table: &str, n: usize) {
        if n > 0 {
            *self.added_rows.entry(table.to_owned()).or_default() += n;
        }
    }
}

/// Computes the exact tables dropped/created and rows added/removed on purpose by migrations
/// newer than `v_before`, evaluated against the pre-migration database at `path`.
fn expected_migration_changes(path: &Path, v_before: i64) -> MigrationChanges {
    let mut changes = MigrationChanges::default();
    let q = |sql: &str| cli_count(path, sql);

    // 120: Bot selection and session placement facts. Replaces `item_dispatch` with
    // `item_team_snapshots` (copying rows with a team) and adds `bot_placements` / `bot_proposals`.
    if v_before < 120 {
        changes.drop_table("item_dispatch");
        changes.create_table("bot_placements", 0);
        changes.create_table("bot_proposals", 0);
        changes.create_table(
            "item_team_snapshots",
            q("SELECT count(*) FROM item_dispatch WHERE team IS NOT NULL"),
        );
    }

    // 121: Retired Steve's own YOLO engine (#522) and added native harness approval tables.
    if v_before < 121 {
        changes.drop_table("yolo_actions");
        changes.drop_table("yolo_windows");
        changes.drop_table("yolo_scopes");
        changes.create_table("approval_policies", 0);
        changes.create_table("approval_turns", 0);
        changes.remove_rows(
            "revisions",
            q("SELECT count(*) FROM revisions WHERE object_type = 'yolo_scope'"),
        );
        changes.remove_rows(
            "settings",
            q("SELECT count(*) FROM settings WHERE key LIKE 'yolo.%' OR key IN ('yolo', 'models.yolo')"),
        );
    }

    // 122: Ordinary Node errors, intent identity, stall evidence, and scoped review findings.
    // Unsent `start_turn` commands rejected for `node.protocol_changed` insert `realtime_output_attempts`
    // rows if absent (disjoint from 121's `resolve_permission` rejections).
    if v_before < 122 {
        changes.create_table("runtime_turn_progress", 0);
        changes.create_table("workflow_review_findings", 0);
        changes.add_rows(
            "realtime_output_attempts",
            q("SELECT count(DISTINCT a.attempt_id)
               FROM runtime_attempts a
               JOIN runtime_turns t USING(turn_id)
               JOIN sessions s ON s.id = t.session_id
               JOIN node_commands c ON c.operation_id = a.operation_id
                 AND c.node_id = a.node_id
                 AND c.target_attempt = a.attempt_id
                 AND c.method = 'start_turn'
               WHERE s.readonly = 0
                 AND c.state = 'pending'
                 AND c.maybe_sent = 0
                 AND c.lease_generation = 0
                 AND c.receipt IS NULL
                 AND NOT EXISTS (
                   SELECT 1 FROM realtime_output_attempts r WHERE r.attempt_id = a.attempt_id
                 )"),
        );
    }

    // 123: Item lifecycle and delivery cutover.
    // Drops shadow completion-judge / sweep / legacy delivery-fact tables; creates delivery ref/proof
    // tables (backfilling `item_delivery_refs` from valid `gh_prs`); deletes retired `hooks.accepted`
    // operations; finalizes owner-finished closing items (cancelling/deleting their `queued` rows,
    // deleting their `session_resume` and waiting `item_dependency_waits`, inserting missing
    // `item_cycles` / `session_dispatch`); discards Steve-queued prompts of other closing complete
    // items; and backfills `item_dependency_waits` for blocked ready/active items.
    if v_before < 123 {
        for table in [
            "item_would_do",
            "item_judgements",
            "item_judge_candidates",
            "item_waits",
            "work_policy",
            "item_delivery_facts",
        ] {
            changes.drop_table(table);
        }
        changes.create_table("item_delivery_proofs", 0);
        changes.create_table(
            "item_delivery_refs",
            q("SELECT count(*) FROM (
                 SELECT DISTINCT i.id, i.cycle,
                   COALESCE((SELECT MAX(version) FROM item_goals WHERE item_id = i.id), 0),
                   p.repo_id, p.number
                 FROM gh_prs p
                 JOIN items i ON i.id = p.item_id AND i.workspace_id = p.workspace_id
                 WHERE p.number > 0
                   AND length(p.head_sha) IN (40, 64)
                   AND p.head_sha NOT GLOB '*[^0-9a-f]*'
               )"),
        );

        const CLOSING_CTES: &str = "
            WITH owner_finishes AS (
              SELECT id, cycle FROM items
              WHERE phase = 'closing'
                AND json_extract(closing, '$.kind') = 'complete'
                AND json_extract(closing, '$.reason') = '你标记了完成'
                AND json_extract(closing, '$.policy') = 'human'
                AND json_extract(closing, '$.accepted') = 1
                AND json_extract(closing, '$.decision') IS NULL
            ),
            discarded_closings AS (
              SELECT id, cycle FROM items
              WHERE phase = 'closing'
                AND json_extract(closing, '$.kind') = 'complete'
                AND id NOT IN (SELECT id FROM owner_finishes)
            ) ";

        changes.add_rows(
            "item_cycles",
            q(&format!(
                "{CLOSING_CTES}
                 SELECT count(*) FROM owner_finishes f
                 WHERE NOT EXISTS (
                   SELECT 1 FROM item_cycles c WHERE c.item_id = f.id AND c.cycle = f.cycle
                 )"
            )),
        );
        changes.add_rows(
            "session_dispatch",
            q(&format!(
                "{CLOSING_CTES}
                 SELECT count(*) FROM queued q
                 JOIN sessions s ON s.id = q.session_id
                 JOIN owner_finishes f ON f.id = s.item_id
                 WHERE q.via NOT IN ('steve', 'resumed:steve')
                   AND NOT EXISTS (SELECT 1 FROM session_dispatch d WHERE d.id = q.id)"
            )),
        );
        changes.remove_rows(
            "queued",
            q(&format!(
                "{CLOSING_CTES}
                 SELECT count(*) FROM queued
                 WHERE session_id IN (
                   SELECT s.id FROM sessions s JOIN owner_finishes f ON f.id = s.item_id
                 )
                 OR (
                   via IN ('steve', 'resumed:steve')
                   AND session_id IN (
                     SELECT s.id FROM sessions s JOIN discarded_closings d ON d.id = s.item_id
                   )
                 )"
            )),
        );
        changes.remove_rows(
            "session_resume",
            q(&format!(
                "{CLOSING_CTES}
                 SELECT count(*) FROM session_resume
                 WHERE session_id IN (
                   SELECT s.id FROM sessions s JOIN owner_finishes f ON f.id = s.item_id
                 )"
            )),
        );
        changes.remove_rows(
            "item_dependency_waits",
            q(&format!(
                "{CLOSING_CTES}
                 SELECT count(*) FROM item_dependency_waits w
                 WHERE w.state = 'waiting'
                   AND EXISTS (
                     SELECT 1 FROM owner_finishes f WHERE f.id = w.item_id AND f.cycle = w.cycle
                   )"
            )),
        );
        changes.add_rows(
            "item_dependency_waits",
            q(&format!(
                "{CLOSING_CTES}
                 SELECT count(*) FROM items i
                 WHERE i.status = 'block'
                   AND (i.phase IN ('ready', 'active') OR i.id IN (SELECT id FROM discarded_closings))
                   AND NOT EXISTS (
                     SELECT 1 FROM item_dependency_waits w
                     WHERE w.item_id = i.id AND w.cycle = i.cycle
                   )"
            )),
        );
        changes.remove_rows(
            "item_operations",
            q("SELECT count(*) FROM item_operations WHERE kind = 'hooks.accepted'"),
        );
    }

    // 124: Decision revisions, choice receipts, natural keys, prompt templates, and typed undo.
    // Rewrites columns in place without inserting or deleting rows in existing tables.
    if v_before < 124 {
        changes.create_table("decision_choice_receipts", 0);
    }

    changes
}

#[test]
fn opens_prod_copy_unchanged_and_reads_everything() {
    let Some((_dir, path)) = prod_copy() else {
        eprintln!("STEVE_STORE_PROD_COPY not set; skipped");
        return;
    };
    let counts_before = {
        let c = rusqlite::Connection::open(&path).unwrap();
        table_counts(&c)
    };
    let schema_before = schema_of(&path);
    let v_before: i64 = cli(&path, "SELECT v FROM schema_version").parse().unwrap();
    assert!(
        (2..=db::SCHEMA_VERSION).contains(&v_before),
        "prod copy at v{v_before}"
    );
    let expected = expected_migration_changes(&path, v_before);

    let s = Store::open(&path, NoPublisher).unwrap();
    if v_before == db::SCHEMA_VERSION {
        assert_eq!(
            schema_of(&path),
            schema_before,
            "opening at the current version must not change the schema"
        );
    }
    assert_eq!(
        shape_of(&path),
        shape_of(fresh().store.path()),
        "migrated prod schema equals a fresh schema"
    );
    s.fixture_write(|c| {
        assert_eq!(
            c.query_row("PRAGMA integrity_check", [], |r| r.get::<_, String>(0))?,
            "ok"
        );
        assert_eq!(
            count(c, "SELECT count(*) FROM pragma_foreign_key_check"),
            0,
            "foreign_key_check is empty after migrating"
        );

        let counts_after = table_counts(c);

        // 1. Every table present before migration is either explicitly dropped or preserved with
        //    its exact expected row delta.
        for (table, &before) in &counts_before {
            if expected.dropped_tables.contains(table) {
                assert!(
                    !counts_after.contains_key(table),
                    "expected migration to drop table `{table}`, but it still exists"
                );
                continue;
            }
            let after = *counts_after
                .get(table)
                .unwrap_or_else(|| panic!("table `{table}` disappeared unexpectedly"));
            let added = expected.added_rows.get(table).copied().unwrap_or(0);
            let removed = expected.removed_rows.get(table).copied().unwrap_or(0);
            assert_eq!(
                after + removed,
                before + added,
                "table `{table}` row count mismatch (before={before}, after={after}, expected_added={added}, expected_removed={removed})"
            );
        }

        // 2. Every table created during migration is accounted for with its exact initial count.
        let actual_created: BTreeSet<&String> = counts_after
            .keys()
            .filter(|t| !counts_before.contains_key(*t))
            .collect();
        let expected_created: BTreeSet<&String> = expected.created_tables.keys().collect();
        assert_eq!(
            actual_created, expected_created,
            "newly created tables differ from expected migration additions"
        );
        for (table, &expected_rows) in &expected.created_tables {
            assert_eq!(
                counts_after[table], expected_rows,
                "newly created table `{table}` row count mismatch"
            );
        }
        Ok(())
    })
    .unwrap();
    drop(s);
    assert_eq!(
        cli(&path, "SELECT v FROM schema_version"),
        db::SCHEMA_VERSION.to_string()
    );
    let s = Store::open(&path, NoPublisher).unwrap();

    // Every row decodes through the repositories and read views, and counts match the sqlite3 CLI.
    let n = |sql: &str| cli_count(&path, sql);
    let wss = s.workspaces().all().unwrap();
    assert_eq!(wss.len(), n("SELECT count(*) FROM workspaces"));

    // Items: decode every task row through both the legacy Items repo (`Item::from_row`) and the
    // typed read views (`ItemViews::hot` + paginated `ItemViews::archived`).
    let mut items = 0;
    let mut item_view_ids: HashSet<String> = s
        .read(|r| r.item_views().hot())
        .unwrap()
        .into_iter()
        .map(|v| v.id.to_string())
        .collect();

    // Sessions: `Sessions::list(Some(ws))` caps at LIMIT 300 and `list(None)` caps at LIMIT 1000,
    // so full enumeration uses `SessionViews::hot` (non-ended) + cursor-paginated `SessionViews::ended`,
    // then decodes every session ID through `Sessions::get` (`Session::from_row`).
    let mut session_ids: HashSet<String> = s
        .read(|r| r.session_views().hot())
        .unwrap()
        .into_iter()
        .map(|v| v.id.to_string())
        .collect();

    // Decisions: `Decisions::recent(ws, since)` caps at LIMIT 100 (`repo/decisions.rs`), so once a
    // workspace has >100 settled cards it cannot enumerate them all. Full enumeration uses W3.3's
    // `DecisionViews::open` + cursor-paginated `DecisionViews::settled`, and then strictly decodes
    // every ID through both `DecisionRows::get` (no silent JSON fallbacks) and `Decisions::get`.
    let open_decision_views = s.read(|r| r.decision_views().open()).unwrap();
    assert_eq!(
        open_decision_views.len(),
        n("SELECT count(*) FROM decisions WHERE status = 'open'")
    );
    let mut decision_ids: HashSet<String> = open_decision_views
        .into_iter()
        .map(|d| d.id.to_string())
        .collect();
    let mut settled_decision_count = 0;
    let mut repo_open_decisions = 0;

    let mut messages = 0;
    let mut message_view_ids: HashSet<String> = HashSet::new();
    let mut tags = 0;
    let mut projects = 0;
    let mut ws_settings_count = 0;

    for w in &wss {
        items += s.items().all(&w.id).unwrap().len();

        let mut before = None;
        loop {
            let (page, next) = s
                .read(|r| {
                    r.item_views().archived(
                        &w.id,
                        &Page {
                            before: before.clone(),
                            limit: 200,
                        },
                    )
                })
                .unwrap();
            for item in page {
                assert!(
                    item_view_ids.insert(item.id.to_string()),
                    "duplicate item view id {}",
                    item.id
                );
            }
            match next {
                Some(cursor) => {
                    before = Some(SettledCursor::parse(&cursor).expect("valid item cursor"));
                }
                None => break,
            }
        }

        let mut ws_ended_sessions = 0;
        let mut before = None;
        loop {
            let (page, next) = s
                .read(|r| {
                    r.session_views().ended(
                        &w.id,
                        &Page {
                            before: before.clone(),
                            limit: 200,
                        },
                    )
                })
                .unwrap();
            ws_ended_sessions += page.len();
            for ses in page {
                assert!(
                    session_ids.insert(ses.id.to_string()),
                    "duplicate session view id {}",
                    ses.id
                );
            }
            match next {
                Some(cursor) => {
                    before = Some(SettledCursor::parse(&cursor).expect("valid session cursor"));
                }
                None => break,
            }
        }
        // Still exercise `Sessions::list(Some(ws))` and verify its exact LIMIT 300 contract.
        let ws_total_sessions: usize = s
            .fixture_write(|c| {
                Ok(c.query_row(
                    "SELECT count(*) FROM sessions WHERE workspace_id = ?",
                    [&w.id],
                    |r| r.get::<_, i64>(0),
                )? as usize)
            })
            .unwrap();
        assert!(ws_ended_sessions <= ws_total_sessions);
        assert_eq!(
            s.sessions().list(Some(&w.id)).unwrap().len(),
            ws_total_sessions.min(300)
        );

        repo_open_decisions += s.decisions().open(&w.id).unwrap().len();
        let mut ws_settled_decisions = 0;
        let mut before = None;
        loop {
            let (page, next) = s
                .read(|r| {
                    r.decision_views().settled(
                        &w.id,
                        &DecisionPage {
                            since: -1,
                            before: before.clone(),
                            limit: 200,
                        },
                    )
                })
                .unwrap();
            ws_settled_decisions += page.len();
            for dec in page {
                assert!(
                    decision_ids.insert(dec.id.to_string()),
                    "duplicate decision view id {}",
                    dec.id
                );
            }
            match next {
                Some(cursor) => before = Some(cursor),
                None => break,
            }
        }
        settled_decision_count += ws_settled_decisions;
        // Still exercise `Decisions::recent(ws, -1)` and verify its exact LIMIT 100 contract.
        assert_eq!(
            s.decisions().recent(&w.id, -1).unwrap().len(),
            ws_settled_decisions.min(100)
        );

        messages += s.messages().list(&w.id, "steve", i64::MAX).unwrap().len();
        let mut before = None;
        loop {
            let (page, next) = s
                .read(|r| {
                    r.message_views().history(
                        &w.id,
                        &Page {
                            before: before.clone(),
                            limit: 200,
                        },
                    )
                })
                .unwrap();
            for msg in page {
                assert!(
                    message_view_ids.insert(msg.id.to_string()),
                    "duplicate message view id {}",
                    msg.id
                );
            }
            match next {
                Some(cursor) => {
                    before = Some(SettledCursor::parse(&cursor).expect("valid message cursor"));
                }
                None => break,
            }
        }

        tags += s.tags().list(&w.id).unwrap().len();
        projects += s.projects().list(&w.id, false).unwrap().len()
            + s.projects().list(&w.id, true).unwrap().len();
        ws_settings_count += s.settings().all(&w.id).unwrap().len();
    }

    assert_eq!(items, n("SELECT count(*) FROM items WHERE kind = 'task'"));
    assert_eq!(
        item_view_ids.len(),
        n("SELECT count(*) FROM items WHERE kind = 'task'")
    );

    assert_eq!(session_ids.len(), n("SELECT count(*) FROM sessions"));
    for ses_id in &session_ids {
        assert!(
            s.sessions().get(ses_id).unwrap().is_some(),
            "missing session {ses_id}"
        );
    }

    assert_eq!(
        repo_open_decisions,
        n("SELECT count(*) FROM decisions WHERE status = 'open'")
    );
    assert_eq!(
        settled_decision_count,
        n("SELECT count(*) FROM decisions WHERE status != 'open' AND resolved > -1")
    );
    assert_eq!(
        decision_ids.len(),
        n(
            "SELECT count(*) FROM decisions WHERE status = 'open' OR (status != 'open' AND resolved IS NOT NULL)"
        )
    );
    // Any settled row without `resolved` (for example an open YOLO decision expired by 121) is
    // outside `DecisionViews::settled`, so collect its ID separately to verify every row decodes.
    let unresolved_closed_ids: Vec<String> = s
        .fixture_write(|c| {
            let mut st =
                c.prepare("SELECT id FROM decisions WHERE status != 'open' AND resolved IS NULL")?;
            let ids = st
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(ids)
        })
        .unwrap();
    assert_eq!(
        decision_ids.len() + unresolved_closed_ids.len(),
        n("SELECT count(*) FROM decisions")
    );
    // Strictly decode every decision through W3.3's typed `DecisionRows` contract (no silent
    // JSON fallback) and through `Decisions::get`.
    for dec_id in decision_ids.iter().chain(&unresolved_closed_ids) {
        assert!(
            s.read(|r| DecisionRows(r).get(dec_id)).unwrap().is_some(),
            "strict DecisionRows decode failed or missing for {dec_id}"
        );
        assert!(
            s.decisions().get(dec_id).unwrap().is_some(),
            "Decisions::get missing for {dec_id}"
        );
    }

    assert_eq!(
        messages,
        n("SELECT count(*) FROM messages WHERE space = 'steve'")
    );
    assert_eq!(
        message_view_ids.len(),
        n("SELECT count(*) FROM messages WHERE space = 'steve'")
    );
    assert_eq!(tags, n("SELECT count(*) FROM tags"));
    assert_eq!(projects, n("SELECT count(*) FROM projects"));
    assert_eq!(
        ws_settings_count,
        n("SELECT count(*) FROM settings WHERE workspace_id IN (SELECT id FROM workspaces)")
    );
    assert_eq!(
        s.bots().list(None).unwrap().len(),
        n("SELECT count(*) FROM bots WHERE deleted = 0")
    );
    assert_eq!(
        s.devices().list().unwrap().len(),
        n("SELECT count(*) FROM devices")
    );
    assert_eq!(
        s.users().count().unwrap() as usize,
        n("SELECT count(*) FROM users")
    );
    if n("SELECT count(*) FROM users WHERE role = 'owner'") > 0 {
        assert!(s.access().owner().unwrap().is_some());
    }
    assert_eq!(
        s.access().tokens(None).unwrap().len(),
        n("SELECT count(*) FROM access_tokens")
    );
    assert_eq!(
        s.access().clients(None).unwrap().len(),
        n("SELECT count(*) FROM clients")
    );
    // Enumerate events across every session (not `sessions().list(None)` which caps at 1000).
    let mut events = 0;
    for ses_id in &session_ids {
        events += s.events().after(ses_id, 0, i64::MAX).unwrap().len();
    }
    assert_eq!(events, n("SELECT count(*) FROM events"));
    assert_eq!(
        s.peek_counter("item:ST").unwrap().to_string(),
        cli(
            &path,
            "SELECT coalesce(max(value), 0) FROM counters WHERE workspace_id = '*' AND name = 'item:ST'"
        )
    );
    // no JSON column fell back to its default where the text is not the default
    assert_eq!(
        n(
            "SELECT count(*) FROM items WHERE json_valid(plan) AND json_valid(source) AND (closing IS NULL OR json_valid(closing))"
        ),
        n("SELECT count(*) FROM items"),
        "all item plan/source/closing JSON valid"
    );
}

/// The schema with each table's columns compared as a set, not in declaration order: the production
/// database gained `items.phase` and `sessions.reasoning_effort` through the rerunnable migrations
/// that add a column only when it is missing (027), after later columns already existed, so its
/// declaration order differs from a fresh database while every column, type, default and key is the
/// same. Indexes, triggers and views are still compared by their exact SQL.
fn shape_of(path: &Path) -> String {
    let c = rusqlite::Connection::open(path).unwrap();
    let mut out = Vec::new();
    let mut st = c
        .prepare("select type, name, sql from sqlite_master order by type, name")
        .unwrap();
    let rows: Vec<(String, String, Option<String>)> = st
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    for (kind, name, sql) in rows {
        if kind != "table" {
            out.push(format!("{kind}|{name}|{}", sql.unwrap_or_default()));
            continue;
        }
        let mut cols: Vec<String> = c
            .prepare(&format!("PRAGMA table_xinfo(\"{name}\")"))
            .unwrap()
            .query_map([], |r| {
                Ok(format!(
                    "{}:{}:{}:{}:{}:{}",
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)?,
                    r.get::<_, Option<String>>(4)?.unwrap_or_default(),
                    r.get::<_, i64>(5)?,
                    r.get::<_, i64>(6)?
                ))
            })
            .unwrap()
            .map(Result::unwrap)
            .collect();
        cols.sort();
        // Table-level constraints (UNIQUE, FOREIGN KEY, CHECK) follow the column list in the SQL.
        let sql = sql.unwrap_or_default();
        let constraints: Vec<&str> = sql
            .split(',')
            .map(str::trim)
            .filter(|p| {
                let p = p.to_ascii_uppercase();
                p.starts_with("UNIQUE")
                    || p.starts_with("FOREIGN KEY")
                    || p.starts_with("CHECK")
                    || p.starts_with("PRIMARY KEY")
            })
            .collect();
        out.push(format!(
            "table|{name}|{}|{}",
            cols.join(","),
            constraints.join(",")
        ));
    }
    out.join("\n")
}
