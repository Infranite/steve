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
use steve_store::thread;
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
             WHERE type = 'table'
               AND name NOT LIKE 'sqlite_%'
               AND name NOT LIKE 'kb_fts_%'
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

/// Expected schema-table and row-count changes made on purpose by migrations 120–127.
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
        self.created_tables.remove(table);
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

/// Exact backfill counts expected from migration 127, evaluated on the pre-migration database so
/// post-migration checks prove neither `reply_message_id` nor `dispatch_ref_id` dropped links.
struct Thread127Backfill {
    total_receipts: usize,
    /// Distinct non-empty reply_ids available without the `route_log.reply_id` fallback on
    /// `thread_runs` (i.e., what only `thread_runs.reply_id` + non-run `route_log.reply_id` had).
    legacy_direct_reply_ids: usize,
    /// Distinct non-empty candidate reply_ids across `COALESCE(thread_runs.reply_id, route_log.reply_id)`
    /// unioned with non-run `route_log.reply_id`. Every distinct candidate maps to one receipt row.
    backfillable_reply_ids: usize,
    /// Receipts whose `(workspace_id, origin_message_id)` has a valid `thread_items` row
    /// referencing an existing `items.id` (migrated to `thread_dispatch_refs` as `create_item`).
    backfillable_dispatch_refs: usize,
    /// Receipts with `item_id IS NULL` and no origin dispatch (inline answers, asks, configs, etc.).
    unbackfillable_dispatch_no_item: usize,
    /// Receipts with `item_id IS NOT NULL` whose `(workspace_id, origin_message_id)` has no valid
    /// `create_item` row in `thread_items` (for example, `supplement` / `suggest` appends targeting
    /// an item created by an earlier origin, or a deleted item).
    unbackfillable_dispatch_other_origin_or_deleted: usize,
    /// High-water mark of `thread_reply_commits` in `sqlite_sequence` before table rebuild.
    reply_commits_seq: i64,
}

fn expected_thread_127_backfill(path: &Path) -> Thread127Backfill {
    let q = |sql: &str| cli_count(path, sql);

    let total_receipts = q(
        "SELECT (SELECT count(*) FROM thread_runs)
              + (SELECT count(*) FROM route_log rl
                  WHERE NOT EXISTS (SELECT 1 FROM thread_runs r WHERE r.message_id = rl.message_id))",
    );
    let legacy_direct_reply_ids = q("SELECT count(DISTINCT reply_id) FROM (
           SELECT NULLIF(r.reply_id, '') AS reply_id FROM thread_runs r
           UNION
           SELECT NULLIF(rl.reply_id, '') AS reply_id
           FROM route_log rl
           WHERE NOT EXISTS (SELECT 1 FROM thread_runs r WHERE r.message_id = rl.message_id)
         ) WHERE reply_id IS NOT NULL");
    let backfillable_reply_ids = q("SELECT count(DISTINCT reply_id) FROM (
           SELECT COALESCE(NULLIF(r.reply_id, ''), NULLIF(rl.reply_id, '')) AS reply_id
           FROM thread_runs r
           LEFT JOIN route_log rl ON rl.message_id = r.message_id
           UNION
           SELECT NULLIF(rl.reply_id, '') AS reply_id
           FROM route_log rl
           WHERE NOT EXISTS (SELECT 1 FROM thread_runs r WHERE r.message_id = rl.message_id)
         ) WHERE reply_id IS NOT NULL");

    const ALL_RECEIPTS_CTE: &str = "
        WITH all_receipts AS (
          SELECT r.workspace_id, r.message_id AS origin_message_id, rl.item_id
          FROM thread_runs r
          LEFT JOIN route_log rl ON rl.message_id = r.message_id
          UNION ALL
          SELECT rl.workspace_id, rl.message_id AS origin_message_id, rl.item_id
          FROM route_log rl
          WHERE NOT EXISTS (SELECT 1 FROM thread_runs r WHERE r.message_id = rl.message_id)
        ) ";

    let backfillable_dispatch_refs = q(&format!(
        "{ALL_RECEIPTS_CTE}
         SELECT count(*) FROM all_receipts ar
         WHERE EXISTS (
           SELECT 1 FROM thread_items ti
           WHERE ti.workspace_id = ar.workspace_id
             AND ti.origin_msg = ar.origin_message_id
             AND EXISTS (SELECT 1 FROM items i WHERE i.id = ti.item_id)
         )"
    ));
    let unbackfillable_dispatch_no_item = q(&format!(
        "{ALL_RECEIPTS_CTE}
         SELECT count(*) FROM all_receipts ar
         WHERE ar.item_id IS NULL
           AND NOT EXISTS (
             SELECT 1 FROM thread_items ti
             WHERE ti.workspace_id = ar.workspace_id
               AND ti.origin_msg = ar.origin_message_id
               AND EXISTS (SELECT 1 FROM items i WHERE i.id = ti.item_id)
           )"
    ));
    let unbackfillable_dispatch_other_origin_or_deleted = q(&format!(
        "{ALL_RECEIPTS_CTE}
         SELECT count(*) FROM all_receipts ar
         WHERE ar.item_id IS NOT NULL
           AND NOT EXISTS (
             SELECT 1 FROM thread_items ti
             WHERE ti.workspace_id = ar.workspace_id
               AND ti.origin_msg = ar.origin_message_id
               AND EXISTS (SELECT 1 FROM items i WHERE i.id = ti.item_id)
           )"
    ));
    assert_eq!(
        backfillable_dispatch_refs
            + unbackfillable_dispatch_no_item
            + unbackfillable_dispatch_other_origin_or_deleted,
        total_receipts,
        "every pre-127 receipt must fall into exactly one dispatch backfill bucket"
    );

    let reply_commits_seq: i64 = cli(
        path,
        "SELECT COALESCE(
           (SELECT seq FROM sqlite_sequence WHERE name = 'thread_reply_commits'),
           (SELECT MAX(seq) FROM thread_reply_commits),
           0
         )",
    )
    .parse()
    .unwrap();

    Thread127Backfill {
        total_receipts,
        legacy_direct_reply_ids,
        backfillable_reply_ids,
        backfillable_dispatch_refs,
        unbackfillable_dispatch_no_item,
        unbackfillable_dispatch_other_origin_or_deleted,
        reply_commits_seq,
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

    // 125: Durable daily aggregates for low-cardinality telemetry counters.
    if v_before < 125 {
        changes.create_table("telemetry_daily", 0);
    }

    // 126: Remove retired dynamic work-graph tables, steve_workflow tool pins, and stale
    // close-request workflow_revision keys.
    if v_before < 126 {
        for table in [
            "workflow_review_findings",
            "workflow_units",
            "workflow_history",
            "workflow_attempts",
            "workflows",
        ] {
            changes.drop_table(table);
        }
        if v_before >= 69 {
            changes.remove_rows(
                "bot_tool_pins",
                q("SELECT count(*) FROM bot_tool_pins WHERE kind = 'tool' AND target = 'steve_workflow'"),
            );
        }
    }

    // 127: WaitContract columns and constraints (in-place rebuilds, no table/row count change).

    // 128: Steve coordinator (#544): strip main bot from teams, rewrite steve_fallback, create
    // `item_placement_waits`.
    if v_before < 128 {
        changes.create_table("item_placement_waits", 0);
    }

    // 129: Person avatar (#545): link workspace members to contact parties (reusing a unique
    // unlinked name match, otherwise inserting a new contact row), add `decisions.resolved_by`,
    // and backfill single-member `'me'` timeline actors.
    if v_before < 129 {
        changes.add_rows(
            "parties",
            q("SELECT count(*)
               FROM workspace_members wm
               JOIN users u ON u.id = wm.user_id
               WHERE NOT EXISTS (
                 SELECT 1 FROM parties p
                 WHERE p.workspace_id = wm.workspace_id AND p.user_id = wm.user_id
               )
                 AND NOT (
                   (SELECT count(*) FROM parties p_same
                    WHERE p_same.workspace_id = wm.workspace_id
                      AND p_same.type = 'contact'
                      AND p_same.status = 'active'
                      AND p_same.user_id IS NULL
                      AND lower(trim(p_same.name)) = lower(trim(u.name))) = 1
                   AND (SELECT count(*) FROM workspace_members wm2
                        JOIN users u2 ON u2.id = wm2.user_id
                        WHERE wm2.workspace_id = wm.workspace_id
                          AND NOT EXISTS (
                            SELECT 1 FROM parties p3
                            WHERE p3.workspace_id = wm2.workspace_id AND p3.user_id = wm2.user_id
                          )
                          AND lower(trim(u2.name)) = lower(trim(u.name))) = 1
                 )"),
        );
    }

    // 130: Durable thread submit idempotency, reply tasks, reply receipts, origin-scoped dispatch
    // & delivery refs, and typed notices (#546 W3.4).
    // Drops legacy `thread_resume`, `thread_runs`, `route_log`, `thread_items`; migrates
    // `thread_runs` + non-run `route_log` into settled `thread_reply_tasks` & `thread_reply_receipts`,
    // `thread_items` (whose item still exists) into `thread_dispatch_refs`, `messages(space='notify')`
    // and `settings.noticeDismissed` into `thread_notices` (deleting the migrated `notify` messages
    // and legacy `thread.pending.*` / `noticeDismissed` / `thread.postback.*` settings), and rebuilds
    // `thread_reply_commits` against `thread_reply_receipts(id)` (pruning only orphaned rows).
    if v_before < 130 {
        for table in ["thread_resume", "thread_runs", "route_log", "thread_items"] {
            changes.drop_table(table);
        }
        let migrated_receipts = q(
            "SELECT (SELECT count(*) FROM thread_runs)
                  + (SELECT count(*) FROM route_log rl
                      WHERE NOT EXISTS (SELECT 1 FROM thread_runs r WHERE r.message_id = rl.message_id))",
        );
        let migrated_dispatches = q("SELECT count(*) FROM thread_items ti
             WHERE EXISTS (SELECT 1 FROM items i WHERE i.id = ti.item_id)");
        let notify_messages = q("SELECT count(*) FROM messages WHERE space = 'notify'");
        let dismissed_alerts = q(
            "SELECT count(DISTINCT s.workspace_id || ':alert:' || CAST(j.value AS TEXT))
             FROM settings s, json_each(s.value) j
             WHERE s.key = 'noticeDismissed'
               AND json_valid(s.value)
               AND json_type(s.value) = 'array'
               AND j.type = 'text'
               AND NOT EXISTS (
                 SELECT 1 FROM messages m
                 WHERE m.workspace_id = s.workspace_id
                   AND m.space = 'notify'
                   AND m.id = CAST(j.value AS TEXT)
               )",
        );
        changes.create_table("thread_submit_keys", 0);
        changes.create_table("thread_reply_tasks", migrated_receipts);
        changes.create_table("thread_reply_receipts", migrated_receipts);
        changes.create_table("thread_dispatch_refs", migrated_dispatches);
        changes.create_table("thread_forwarded", 0);
        changes.create_table("thread_delivery_refs", 0);
        changes.create_table("thread_notices", notify_messages + dismissed_alerts);
        changes.remove_rows("messages", notify_messages);
        changes.remove_rows(
            "settings",
            q("SELECT count(*) FROM settings
               WHERE key LIKE 'thread.pending.%'
                  OR key = 'noticeDismissed'
                  OR key LIKE 'thread.postback.%'"),
        );
        changes.remove_rows(
            "thread_reply_commits",
            q("SELECT count(*) FROM thread_reply_commits c
               WHERE NOT EXISTS (SELECT 1 FROM thread_runs r WHERE r.id = c.run_id)
                  OR NOT EXISTS (SELECT 1 FROM messages m WHERE m.id = c.message_id)"),
        );
    }

    // 131: Knowledge redefinition (#550): creates `project_proposals`, migrates codebase
    // engineering facts out of `mems` into `project_proposals`, and withdraws `items`/`chat`
    // work-record sources from the knowledge index.
    if v_before < 131 {
        const ENG_CTE: &str = "
            WITH _mig131_eng AS (
              SELECT m.id AS mem_id, picked.proj_id AS project_id
              FROM (
                SELECT
                  m.id,
                  COALESCE(
                    (SELECT a.object_id FROM mem_about a
                       JOIN projects p ON p.id = a.object_id AND p.workspace_id = m.workspace_id AND p.kind = 'git'
                      WHERE a.mem_id = m.id AND a.object_type = 'project'
                      LIMIT 1),
                    (SELECT i.project_id FROM items i
                       JOIN projects p ON p.id = i.project_id AND p.workspace_id = m.workspace_id AND p.kind = 'git'
                      WHERE m.source_type = 'item' AND i.id = m.source_id
                      LIMIT 1),
                    (SELECT p.id FROM projects p
                      WHERE p.workspace_id = m.workspace_id AND p.kind = 'git' AND p.archived = 0
                        AND instr(lower(m.text), lower(p.name)) > 0
                      ORDER BY length(p.name) DESC
                      LIMIT 1)
                  ) AS proj_id
                FROM mems m
                WHERE m.kind = 'fact'
                  AND m.source_type != 'me'
                  AND NOT EXISTS (
                    SELECT 1 FROM mem_about a WHERE a.mem_id = m.id AND a.object_type = 'party'
                  )
                  AND (
                    m.source_type = 'item'
                    OR (m.status = 'candidate' AND NOT EXISTS (SELECT 1 FROM mem_about a WHERE a.mem_id = m.id))
                  )
              ) AS picked
              JOIN mems m ON m.id = picked.id
              WHERE picked.proj_id IS NOT NULL
            ) ";
        let eng_count = q(&format!("{ENG_CTE} SELECT count(*) FROM _mig131_eng"));
        changes.create_table("project_proposals", eng_count);
        changes.remove_rows("mems", eng_count);
        changes.remove_rows(
            "mem_candidates",
            q(&format!(
                "{ENG_CTE} SELECT count(*) FROM mem_candidates WHERE mem_id IN (SELECT mem_id FROM _mig131_eng)"
            )),
        );
        changes.remove_rows(
            "mem_supersedes",
            q(&format!(
                "{ENG_CTE} SELECT count(*) FROM mem_supersedes
                 WHERE mem_id IN (SELECT mem_id FROM _mig131_eng)
                    OR old_id IN (SELECT mem_id FROM _mig131_eng)"
            )),
        );
        changes.remove_rows(
            "mem_about",
            q(&format!(
                "{ENG_CTE} SELECT count(*) FROM mem_about WHERE mem_id IN (SELECT mem_id FROM _mig131_eng)"
            )),
        );
        let removed_chunks = q(&format!(
            "{ENG_CTE} SELECT count(*) FROM kb_chunks
             WHERE (obj_type = 'mem' AND obj_id IN (SELECT mem_id FROM _mig131_eng))
                OR source_kind IN ('items', 'chat')"
        ));
        changes.remove_rows("kb_chunks", removed_chunks);
        changes.remove_rows(
            "kb_fts",
            q(&format!(
                "{ENG_CTE} SELECT count(*) FROM kb_fts
                 WHERE rowid IN (
                   SELECT id FROM kb_chunks
                   WHERE (obj_type = 'mem' AND obj_id IN (SELECT mem_id FROM _mig131_eng))
                      OR source_kind IN ('items', 'chat')
                 )"
            )),
        );
        changes.remove_rows(
            "kb_vectors",
            q(&format!(
                "{ENG_CTE} SELECT count(*) FROM kb_vectors
                 WHERE chunk_id IN (
                   SELECT id FROM kb_chunks
                   WHERE (obj_type = 'mem' AND obj_id IN (SELECT mem_id FROM _mig131_eng))
                      OR source_kind IN ('items', 'chat')
                 )"
            )),
        );
        changes.remove_rows(
            "kb_doc_about",
            q("SELECT count(*) FROM kb_doc_about
               WHERE doc_id IN (
                 SELECT d.id FROM kb_docs d
                   JOIN kb_sources s ON s.id = d.source_id
                  WHERE s.kind IN ('items', 'chat')
               )"),
        );
        changes.remove_rows(
            "kb_docs",
            q("SELECT count(*) FROM kb_docs
               WHERE source_id IN (SELECT id FROM kb_sources WHERE kind IN ('items', 'chat'))"),
        );
        changes.remove_rows(
            "kb_sources",
            q("SELECT count(*) FROM kb_sources WHERE kind IN ('items', 'chat')"),
        );
        changes.remove_rows(
            "kb_marks",
            q("SELECT count(*) FROM kb_marks WHERE name LIKE 'sync:chat:%'"),
        );
    }

    // 132: Agent handoff fallback surfaces (#553).
    if v_before < 132 {
        changes.create_table("bot_fallback_surfaces", 0);
    }

    // 133: Backup v2 (#507): dedicated `backup_policy` singleton moved out of workspace `settings`.
    if v_before < 133 {
        let policy_rows = q("WITH legacy(value) AS (
               SELECT CASE WHEN json_valid(value) THEN value ELSE '{}' END
               FROM settings WHERE workspace_id = 'default' AND key = 'backup.daily'
             )
             SELECT count(*) FROM legacy
             WHERE json_type(value, '$.enabled') IN ('true', 'false')
               AND json_type(value, '$.keep_days') = 'integer'
               AND json_extract(value, '$.keep_days') BETWEEN 1 AND 365");
        changes.create_table("backup_policy", policy_rows);
        changes.remove_rows(
            "settings",
            q("SELECT count(*) FROM settings WHERE workspace_id = 'default' AND key = 'backup.daily'"),
        );
    }

    // 134: Remove legacy raw ACP event rows (#563).
    if v_before < 134 {
        changes.remove_rows(
            "events",
            q("SELECT count(*) FROM events
               WHERE kind IN (
                 'tool_call',
                 'tool_call_update',
                 'agent_message_chunk',
                 'user_message_chunk',
                 'agent_thought_chunk'
               )
                  OR format != 'steve.v1'"),
        );
    }

    // 135: Retire YOLO audit rows and backfill explicit `bot_node_bindings` (#563).
    if v_before < 135 {
        changes.remove_rows(
            "audit",
            q("SELECT count(*) FROM audit WHERE action LIKE 'yolo.%'"),
        );
        changes.add_rows(
            "bot_node_bindings",
            q("SELECT count(*) FROM bot_workspaces w
               WHERE NOT EXISTS (
                 SELECT 1 FROM bot_node_bindings b
                 WHERE b.workspace_id = w.workspace_id AND b.bot_id = w.bot_id
               )"),
        );
    }

    // 136..=137: Normalize `bots.perm_mode` and drop legacy `project_dir`/`worktree` columns
    // (no table/row count change).

    // 138: Clear cached harness update state saved before typed update reasons (#571).
    if v_before < 138 {
        changes.remove_rows(
            "settings",
            q("SELECT count(*) FROM settings WHERE key LIKE 'harness.updates.%'"),
        );
    }

    // 139..=140: `mems.pinned`; canonical five-level `reasoning_effort` (no table/row count change).

    // 141: Script workflow ledger (WF-3).
    if v_before < 141 {
        for table in [
            "wf_runs",
            "wf_script_versions",
            "wf_calls",
            "wf_call_attempts",
            "wf_version_calls",
        ] {
            changes.create_table(table, 0);
        }
    }

    // 142: Retire non-terminal children of the removed dynamic work-graph engine. Mirrors the
    // migration's row effects; history of closed items is untouched.
    if v_before < 142 {
        const LEGACY: &str =
            "(SELECT id, cycle FROM items WHERE phase <> 'closed' AND json_valid(source)
                                 AND json_extract(source, '$[1]') = 'workflow')";
        changes.add_rows(
            "item_cycles",
            q(&format!(
                "SELECT count(*) FROM {LEGACY} l
                 WHERE NOT EXISTS (SELECT 1 FROM item_cycles c WHERE c.item_id = l.id AND c.cycle = l.cycle)"
            )),
        );
        changes.add_rows(
            "session_dispatch",
            q(&format!(
                "SELECT count(*) FROM queued q JOIN sessions s ON s.id = q.session_id
                 JOIN {LEGACY} l ON l.id = s.item_id
                 WHERE q.via NOT IN ('steve', 'resumed:steve')
                   AND NOT EXISTS (SELECT 1 FROM session_dispatch d WHERE d.id = q.id)"
            )),
        );
        changes.remove_rows(
            "queued",
            q(&format!(
                "SELECT count(*) FROM queued WHERE session_id IN
                   (SELECT s.id FROM sessions s JOIN {LEGACY} l ON l.id = s.item_id)"
            )),
        );
        changes.remove_rows(
            "session_resume",
            q(&format!(
                "SELECT count(*) FROM session_resume WHERE session_id IN
                   (SELECT s.id FROM sessions s JOIN {LEGACY} l ON l.id = s.item_id)"
            )),
        );
        changes.remove_rows(
            "item_dependency_waits",
            q(&format!(
                "SELECT count(*) FROM item_dependency_waits w WHERE w.state = 'waiting'
                   AND EXISTS (SELECT 1 FROM {LEGACY} l WHERE l.id = w.item_id AND l.cycle = w.cycle)"
            )),
        );
        // `item_placement_waits` is created by a later migration than some prod copies have.
        if q(
            "SELECT count(*) FROM sqlite_master WHERE type = 'table' AND name = 'item_placement_waits'",
        ) > 0
        {
            changes.remove_rows(
                "item_placement_waits",
                q(&format!(
                    "SELECT count(*) FROM item_placement_waits WHERE item_id IN (SELECT id FROM {LEGACY})"
                )),
            );
        }
    }

    // 143: Rebind `work.tidy` to `work.item.lifecycle`; its old receipts and leases are dropped
    // after the cursor is advanced past them.
    if v_before < 143 {
        changes.remove_rows(
            "outbox_receipts",
            q("SELECT count(*) FROM outbox_receipts WHERE consumer_id = 'work.tidy'"),
        );
        changes.remove_rows(
            "outbox_leases",
            q("SELECT count(*) FROM outbox_leases WHERE consumer_id = 'work.tidy'"),
        );
    }

    // 144: Attribute usage and daily statistics to a Node (`stat_daily` rebuilt one row per row).

    // 145: File-as-truth memory architecture (sidecar memory_entries, evidence, observation buffer,
    // rejected_patterns tombstones, memory_revisions; project_proposals adds nullable project_id + target_path).
    if v_before < 145 {
        changes.create_table("memory_entries", 0);
        changes.create_table("memory_evidence", 0);
        changes.create_table("observations", 0);
        changes.create_table("rejected_patterns", 0);
        changes.create_table("memory_revisions", 0);
    }

    // 146: Channel reactions, reply sources, reply bursts and the configurable [AI] mark.
    if v_before < 146 {
        changes.create_table("chan_message_reactions", 0);
        changes.create_table("chan_reply_sources", 0);
    }

    // 147: Three-level workspace permissions (`ask`, `auto`, `full_access`) and `approval_turns.mode`
    // (no table/row count change).

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
    let thread_130_backfill = (v_before < 130).then(|| expected_thread_127_backfill(&path));

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

    // Migration 127 backfill verification:
    // - `reply_message_id` must preserve every unique `COALESCE(thread_runs.reply_id, route_log.reply_id)`
    //   and non-run `route_log.reply_id` (recovering historical answer runs whose `thread_runs.reply_id`
    //   was NULL while `route_log.reply_id` was populated).
    // - `dispatch_ref_id` must link every receipt whose `(workspace_id, origin_message_id)` has a
    //   migrated `create_item` row in `thread_dispatch_refs`, leaving NULL only for non-item turns,
    //   supplements/appends to items created by earlier origins, or deleted items.
    if let Some(ref bf) = thread_130_backfill {
        let actual_reply_non_null =
            n("SELECT count(*) FROM thread_reply_receipts WHERE reply_message_id IS NOT NULL");
        let actual_reply_null =
            n("SELECT count(*) FROM thread_reply_receipts WHERE reply_message_id IS NULL");
        let actual_dispatch_non_null =
            n("SELECT count(*) FROM thread_reply_receipts WHERE dispatch_ref_id IS NOT NULL");
        let actual_dispatch_null =
            n("SELECT count(*) FROM thread_reply_receipts WHERE dispatch_ref_id IS NULL");
        assert_eq!(
            actual_reply_non_null, bf.backfillable_reply_ids,
            "migration 127 reply_message_id backfill count mismatch (legacy_direct={}, backfillable={}, actual={})",
            bf.legacy_direct_reply_ids, bf.backfillable_reply_ids, actual_reply_non_null
        );
        assert!(
            actual_reply_non_null >= bf.legacy_direct_reply_ids,
            "migration 127 must not lose direct reply_message_id links"
        );
        assert_eq!(
            actual_reply_null,
            bf.total_receipts - bf.backfillable_reply_ids,
            "migration 127 unbackfillable reply_message_id count mismatch"
        );
        assert_eq!(
            actual_dispatch_non_null, bf.backfillable_dispatch_refs,
            "migration 127 dispatch_ref_id backfill count mismatch"
        );
        assert_eq!(
            actual_dispatch_null,
            bf.unbackfillable_dispatch_no_item + bf.unbackfillable_dispatch_other_origin_or_deleted,
            "migration 127 unbackfillable dispatch_ref_id count mismatch"
        );
        // Every backfilled dispatch_ref_id resolves to a matching create_item dispatch ref.
        assert_eq!(
            n("SELECT count(*) FROM thread_reply_receipts r
               JOIN thread_dispatch_refs d ON d.id = r.dispatch_ref_id
               WHERE d.workspace_id = r.workspace_id
                 AND d.effect_kind = 'create_item'
                 AND d.origin_message_id IN (
                   r.origin_message_id,
                   CASE WHEN r.item_id IS NOT NULL THEN r.origin_message_id || ':legacy:' || r.item_id END
                 )"),
            actual_dispatch_non_null,
            "every backfilled dispatch_ref_id must match its origin create_item dispatch ref"
        );
        if bf.reply_commits_seq > 0 {
            let seq_after: i64 = cli(
                &path,
                "SELECT seq FROM sqlite_sequence WHERE name = 'thread_reply_commits'",
            )
            .parse()
            .unwrap();
            assert_eq!(
                seq_after, bf.reply_commits_seq,
                "thread_reply_commits sqlite_sequence high-water mark changed"
            );
        }
        eprintln!(
            "prod_copy m127 backfill: receipts={} reply_message_id={}/{} (legacy_direct={}, +{} recovered) dispatch_ref_id={}/{} (unbackfillable: no_item={}, other_origin_or_deleted={})",
            bf.total_receipts,
            actual_reply_non_null,
            bf.total_receipts,
            bf.legacy_direct_reply_ids,
            actual_reply_non_null - bf.legacy_direct_reply_ids,
            actual_dispatch_non_null,
            bf.total_receipts,
            bf.unbackfillable_dispatch_no_item,
            bf.unbackfillable_dispatch_other_origin_or_deleted,
        );
    }

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
    let mut active_thread_tasks = 0;
    let mut rev0_receipts = 0;
    let mut create_item_dispatches = 0;
    let mut thread_notices = 0;
    let mut dismissed_notice_alerts = 0;
    let mut notice_host_messages = 0;

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

        // Durable thread repository (W3.4 / migration 127) workspace-scoped readback.
        active_thread_tasks += s
            .read(|r| thread::list_active_tasks(r, &w.id))
            .unwrap()
            .len();
        rev0_receipts += s
            .read(|r| thread::list_receipts_range(r, &w.id, i64::MIN, i64::MAX))
            .unwrap()
            .len();
        let ws_dispatches = s
            .read(|r| thread::dispatches_of_workspace(r, &w.id))
            .unwrap();
        for d in &ws_dispatches {
            let by_origin = s
                .read(|r| {
                    thread::dispatch_by_origin(
                        r,
                        &d.workspace_id,
                        &d.origin_message_id,
                        &d.effect_kind,
                    )
                })
                .unwrap()
                .expect("dispatch_by_origin must find workspace dispatch");
            assert_eq!(&by_origin, d);
            if let Some(ref item_id) = d.target_item_id {
                let by_item = s
                    .read(|r| thread::dispatch_by_item(r, item_id))
                    .unwrap()
                    .expect("dispatch_by_item must find create_item dispatch");
                assert_eq!(&by_item, d);
            }
        }
        create_item_dispatches += ws_dispatches.len();

        thread_notices += s
            .read(|r| thread::list_notices(r, &w.id, i64::MAX))
            .unwrap()
            .len();
        dismissed_notice_alerts += s
            .read(|r| thread::dismissed_alert_ids(r, &w.id))
            .unwrap()
            .len();
        let host_ids = s
            .read(|r| thread::notice_host_message_ids(r, &w.id))
            .unwrap();
        assert_eq!(
            host_ids,
            s.read(|r| r.message_views().notices(&w.id)).unwrap(),
            "MessageViews::notices must match thread::notice_host_message_ids"
        );
        notice_host_messages += host_ids.len();

        tags += s.tags().list(&w.id).unwrap().len();
        projects += s.projects().list(&w.id, false).unwrap().len()
            + s.projects().list(&w.id, true).unwrap().len();
        ws_settings_count += s.settings().all(&w.id).unwrap().len();
    }

    // Full per-row readback across all migration 127 thread repository tables.
    assert_eq!(
        s.read(|r| thread::list_queued_spaces(r)).unwrap().len(),
        n(
            "SELECT count(*) FROM (SELECT DISTINCT workspace_id, space FROM thread_reply_tasks WHERE status = 'queued')"
        )
    );

    assert_eq!(
        active_thread_tasks,
        n("SELECT count(*) FROM thread_reply_tasks WHERE status IN ('queued', 'claimed')")
    );
    let task_keys: Vec<(String, String)> = s
        .fixture_write(|c| {
            let mut st = c.prepare("SELECT id, origin_message_id FROM thread_reply_tasks")?;
            let rows = st
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .unwrap();
    let mut task_origins = BTreeSet::new();
    for (task_id, origin_id) in &task_keys {
        let task = s
            .read(|r| thread::task_by_id(r, task_id))
            .unwrap()
            .unwrap_or_else(|| panic!("thread::task_by_id missing {task_id}"));
        assert_eq!(task.origin_message_id, *origin_id);
        task_origins.insert(origin_id.clone());
    }
    let mut tasks_by_origin_count = 0;
    for origin_id in &task_origins {
        tasks_by_origin_count += s
            .read(|r| thread::tasks_of_origin(r, origin_id))
            .unwrap()
            .len();
    }
    assert_eq!(
        tasks_by_origin_count,
        n("SELECT count(*) FROM thread_reply_tasks")
    );

    assert_eq!(
        rev0_receipts,
        n("SELECT count(*) FROM thread_reply_receipts WHERE reply_revision = 0")
    );
    let receipt_keys: Vec<(String, String, String)> = s
        .fixture_write(|c| {
            let mut st =
                c.prepare("SELECT id, task_id, origin_message_id FROM thread_reply_receipts")?;
            let rows = st
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .unwrap();
    let mut receipt_origins = BTreeSet::new();
    for (receipt_id, task_id, origin_id) in &receipt_keys {
        let by_id = s
            .read(|r| thread::receipt_by_id(r, receipt_id))
            .unwrap()
            .unwrap_or_else(|| panic!("thread::receipt_by_id missing {receipt_id}"));
        let by_task = s
            .read(|r| thread::receipt_by_task(r, task_id))
            .unwrap()
            .unwrap_or_else(|| panic!("thread::receipt_by_task missing {task_id}"));
        assert_eq!(by_id, by_task);
        assert_eq!(by_id.origin_message_id, *origin_id);
        receipt_origins.insert(origin_id.clone());
    }
    let mut receipts_by_origin_count = 0;
    for origin_id in &receipt_origins {
        let first = s
            .read(|r| thread::receipt_by_origin(r, origin_id))
            .unwrap()
            .unwrap_or_else(|| panic!("thread::receipt_by_origin missing {origin_id}"));
        let latest = s
            .read(|r| thread::latest_receipt_by_origin(r, origin_id))
            .unwrap()
            .unwrap_or_else(|| panic!("thread::latest_receipt_by_origin missing {origin_id}"));
        let all = s
            .read(|r| thread::receipts_of_origin(r, origin_id))
            .unwrap();
        assert_eq!(all.first(), Some(&first));
        assert_eq!(all.last(), Some(&latest));
        receipts_by_origin_count += all.len();
    }
    assert_eq!(
        receipts_by_origin_count,
        n("SELECT count(*) FROM thread_reply_receipts")
    );
    assert_eq!(
        n("SELECT count(*) FROM thread_reply_receipts WHERE json_valid(route_signals)"),
        n("SELECT count(*) FROM thread_reply_receipts"),
        "all thread_reply_receipts.route_signals JSON valid"
    );
    let receipt_spaces: Vec<(String, String, bool)> = s
        .fixture_write(|c| {
            let mut st = c.prepare(
                "SELECT workspace_id, space,
                        MAX(CASE WHEN outcome = 'answered' AND harness_session IS NOT NULL THEN 1 ELSE 0 END)
                 FROM thread_reply_receipts
                 GROUP BY workspace_id, space",
            )?;
            let rows = st
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, i64>(2)? != 0)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .unwrap();
    for (ws, space, expect_resumable) in &receipt_spaces {
        let resumable = s
            .read(|r| thread::last_resumable_receipt(r, ws, space))
            .unwrap();
        assert_eq!(
            resumable.is_some(),
            *expect_resumable,
            "last_resumable_receipt mismatch for ({ws}, {space})"
        );
    }

    assert_eq!(
        create_item_dispatches,
        n(
            "SELECT count(*) FROM thread_dispatch_refs WHERE effect_kind = 'create_item' AND target_item_id IS NOT NULL"
        )
    );
    let dispatch_keys: Vec<(String, String, String)> = s
        .fixture_write(|c| {
            let mut st = c.prepare(
                "SELECT workspace_id, origin_message_id, effect_kind FROM thread_dispatch_refs",
            )?;
            let rows = st
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .unwrap();
    for (ws, origin_id, kind) in &dispatch_keys {
        assert!(
            s.read(|r| thread::dispatch_by_origin(r, ws, origin_id, kind))
                .unwrap()
                .is_some(),
            "thread::dispatch_by_origin missing ({ws}, {origin_id}, {kind})"
        );
    }
    assert_eq!(
        dispatch_keys.len(),
        n("SELECT count(*) FROM thread_dispatch_refs")
    );

    assert_eq!(
        thread_notices,
        n("SELECT count(*) FROM thread_notices WHERE kind != 'alert'")
    );
    assert_eq!(
        dismissed_notice_alerts,
        n("SELECT count(*) FROM thread_notices WHERE closed_at IS NOT NULL")
    );
    assert_eq!(
        notice_host_messages,
        n("SELECT count(*) FROM thread_notices WHERE host_message_id IS NOT NULL")
    );

    let delivery_origins: Vec<(String, String, String)> = s
        .fixture_write(|c| {
            let mut st = c.prepare(
                "SELECT DISTINCT workspace_id, space, origin_message_id FROM thread_delivery_refs",
            )?;
            let rows = st
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .unwrap();
    let mut delivery_refs = 0;
    for (ws, space, origin_id) in &delivery_origins {
        delivery_refs += s
            .read(|r| thread::delivery_refs_of_origin(r, ws, space, origin_id))
            .unwrap()
            .len();
    }
    assert_eq!(
        delivery_refs,
        n("SELECT count(*) FROM thread_delivery_refs")
    );

    let forwarded_items: Vec<String> = s
        .fixture_write(|c| {
            let mut st = c.prepare("SELECT item_id FROM thread_forwarded")?;
            let rows = st
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
        .unwrap();
    for item_id in &forwarded_items {
        assert!(
            s.read(|r| thread::peek_forwarded(r, item_id))
                .unwrap()
                .is_some(),
            "thread::peek_forwarded missing {item_id}"
        );
    }
    assert_eq!(
        n("SELECT count(*) FROM thread_reply_commits c
           JOIN thread_reply_receipts r ON r.id = c.receipt_id
           JOIN messages m ON m.id = c.message_id"),
        n("SELECT count(*) FROM thread_reply_commits"),
        "all thread_reply_commits join cleanly to thread_reply_receipts and messages"
    );

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
        n(
            "SELECT count(*) FROM messages WHERE space = 'steve' AND COALESCE(json_extract(meta, '$.withdrawn'), 0) != 1"
        )
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
