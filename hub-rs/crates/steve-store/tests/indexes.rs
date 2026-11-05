//! Migration 035: every index it adds is the one SQLite picks for the query it was added for. The
//! query texts are the production ones from the cited call sites (long column lists shortened to a
//! few columns, which does not change the plan's access path), checked with EXPLAIN QUERY PLAN on a
//! fresh database.

mod common;

use common::*;
use rusqlite::Connection;

/// The `detail` lines of the query plan.
fn plan(c: &Connection, sql: &str) -> Vec<String> {
    let mut st = c.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
    // parameters stay unbound (NULL): the plan does not depend on their values
    let mut rows = st.raw_query();
    let mut out = Vec::new();
    while let Some(r) = rows.next().unwrap() {
        out.push(r.get::<_, String>(3).unwrap());
    }
    out
}

/// Whether a plan line reads through `index` ("USING INDEX x (...)" or "USING COVERING INDEX x").
fn uses(plan: &[String], index: &str) -> bool {
    let want = format!("INDEX {index}");
    plan.iter().any(|d| {
        d.split(" (")
            .next()
            .is_some_and(|head| head.ends_with(&want))
    })
}

/// `(index, where the query lives, query)`.
const CASES: &[(&str, &str, &str)] = &[
    // ---------------------------------------------------------------- channels
    (
        "chan_messages_outbox",
        "steve-channels outbound.rs send_due",
        "SELECT m.id, d.id, d.result FROM chan_messages m JOIN decisions d ON d.id = m.decision_id
          WHERE m.direction = 'out' AND m.status = 'pending' AND (m.sent IS NULL OR m.sent < ?)
            AND d.status = 'resolved' AND d.subject_type = ? AND d.subject_id = m.id
          ORDER BY m.created LIMIT 50",
    ),
    (
        "chan_messages_outbox",
        "steve-channels outbound.rs resume_direct",
        "SELECT m.id, m.created, s.first_at FROM chan_messages m JOIN chan_send_state s ON s.message_id = m.id
          WHERE m.direction = 'out' AND m.status = 'pending' AND m.decision_id IS NULL
            AND ((m.sent IS NULL AND m.created < ?1) OR m.sent < ?2)
          ORDER BY m.created LIMIT 50",
    ),
    (
        "chan_messages_outbox",
        "steve-channels outbound.rs settle_unknown",
        "SELECT m.id, c.adapter, s.first_at FROM chan_messages m
           JOIN spaces sp ON sp.id = m.space_id JOIN connections c ON c.id = sp.connection_id
           LEFT JOIN chan_send_state s ON s.message_id = m.id
          WHERE m.direction = 'out' AND m.status = 'unknown'
            AND NOT EXISTS (SELECT 1 FROM decisions d WHERE d.subject_type = ?1
                              AND d.subject_id = m.id AND d.status = 'open')
          ORDER BY m.created LIMIT 50",
    ),
    (
        "decisions_subject",
        "steve-channels outbound.rs settle_unknown (open card probe)",
        "SELECT m.id, c.adapter, s.first_at FROM chan_messages m
           JOIN spaces sp ON sp.id = m.space_id JOIN connections c ON c.id = sp.connection_id
           LEFT JOIN chan_send_state s ON s.message_id = m.id
          WHERE m.direction = 'out' AND m.status = 'unknown'
            AND NOT EXISTS (SELECT 1 FROM decisions d WHERE d.subject_type = ?1
                              AND d.subject_id = m.id AND d.status = 'open')
          ORDER BY m.created LIMIT 50",
    ),
    (
        "chan_messages_space_created",
        "steve-channels messages.rs page",
        "SELECT m.id, m.text FROM chan_messages m LEFT JOIN accounts a ON a.id = m.sender_account_id
            LEFT JOIN parties p ON p.id = m.sender_party_id WHERE m.space_id = ?1 AND (?2 IS NULL OR m.created < ?2)
          ORDER BY m.created DESC, m.id DESC LIMIT ?3",
    ),
    (
        "chan_messages_space_created",
        "steve-channels messages.rs context_before",
        "SELECT coalesce(nullif(a.display_name, ''), a.handle_display, p.name, ''), m.text
           FROM chan_messages m LEFT JOIN accounts a ON a.id = m.sender_account_id
           LEFT JOIN parties p ON p.id = m.sender_party_id
          WHERE m.space_id = ?1 AND m.id <> ?2 AND m.text IS NOT NULL AND m.text <> '' AND m.status IN ('received', 'sent')
            AND m.created <= (SELECT created FROM chan_messages WHERE id = ?2)
          ORDER BY m.created DESC, m.id DESC LIMIT ?3",
    ),
    (
        "chan_messages_space_created",
        "steve-channels inbound.rs answered_since_people",
        "WITH people AS (
           SELECT coalesce(max(h.created), 0) AS at FROM chan_messages h
            WHERE h.space_id = ?1 AND h.direction = 'in' AND h.sender_account_id IS NOT NULL
              AND h.sender_account_id NOT IN (SELECT account_id FROM space_members WHERE space_id = ?1 AND role = 'bot')
              AND h.sender_account_id NOT IN (SELECT id FROM accounts WHERE kind = 'app_bot')
         ), first_bot AS (
           SELECT min(b.created) AS at FROM chan_messages b, people
            WHERE b.space_id = ?1 AND b.direction = 'in' AND b.sender_account_id = ?2 AND b.created > people.at
         )
         SELECT EXISTS(
           SELECT 1 FROM chan_messages o, first_bot
            WHERE o.space_id = ?1 AND o.direction = 'out' AND o.status IN ('sent', 'recalled')
              AND first_bot.at IS NOT NULL AND coalesce(o.sent, o.created) >= first_bot.at)",
    ),
    (
        "spaces_ext",
        "steve-channels spaces.rs find",
        "SELECT s.id FROM spaces s WHERE s.workspace_id = ?1 AND s.external_id = ?2 ORDER BY s.created, s.id",
    ),
    (
        "spaces_ext",
        "steve-channels messages.rs our_echo",
        "SELECT m.id, m.space_id, coalesce(m.text, '') FROM chan_messages m JOIN spaces s ON s.id = m.space_id
          WHERE s.workspace_id = ?1 AND s.external_id = ?2 AND m.direction = 'out'
            AND m.status IN ('pending', 'sent', 'unknown') AND m.external_id IS NULL AND coalesce(m.sent, m.created) >= ?3
          ORDER BY m.created DESC, m.id",
    ),
    (
        "account_handles_handle",
        "steve-channels inbound.rs handle_known",
        "SELECT EXISTS (SELECT 1 FROM account_handles WHERE workspace_id = ? AND handle = ?)",
    ),
    (
        "reply_basis_msg",
        "steve-core yolo/facts.rs",
        "SELECT m.on_behalf, m.status, s.has_external, cs.mode,
                (SELECT b.verdict FROM reply_basis b WHERE b.message_id = m.id ORDER BY b.created DESC LIMIT 1)
           FROM chan_messages m JOIN spaces s ON s.id = m.space_id
           LEFT JOIN channel_settings cs ON cs.connection_id = s.connection_id
          WHERE m.id = ? AND m.workspace_id = ?",
    ),
    // ---------------------------------------------------------------- decisions
    (
        "decisions_subject",
        "steve-store repo/lanes.rs batch_card",
        "SELECT status, coalesce(result, 'null') FROM decisions
          WHERE subject_type = 'delivery_batch' AND subject_id = ? AND kind = ? ORDER BY created DESC LIMIT 1",
    ),
    (
        "decisions_created_item",
        "steve-store repo/stats.rs refresh_pending",
        "SELECT EXISTS(SELECT 1 FROM item_stats WHERE dirty = 1) OR (?1 > 0 AND (
           EXISTS(SELECT 1 FROM decisions WHERE item_id IS NOT NULL AND created >= ?1)
           OR EXISTS(SELECT 1 FROM decisions WHERE item_id IS NOT NULL AND resolved >= ?1)))",
    ),
    (
        "decisions_resolved_item",
        "steve-store repo/stats.rs refresh_pending",
        "SELECT EXISTS(SELECT 1 FROM item_stats WHERE dirty = 1) OR (?1 > 0 AND (
           EXISTS(SELECT 1 FROM decisions WHERE item_id IS NOT NULL AND created >= ?1)
           OR EXISTS(SELECT 1 FROM decisions WHERE item_id IS NOT NULL AND resolved >= ?1)))",
    ),
    // ---------------------------------------------------------------- flows
    (
        "flow_runs_flow_started",
        "steve-flow flows.rs summary",
        "SELECT count(CASE WHEN started >= ? THEN 1 END), max(started) FROM flow_runs WHERE flow_id = ? AND dry_run = 0 AND status != 'skipped'",
    ),
    (
        "flow_runs_flow_started",
        "steve-flow runs.rs active_exists",
        "SELECT EXISTS (SELECT 1 FROM flow_runs WHERE flow_id = ? AND status IN ('running', 'waiting'))",
    ),
    (
        "flow_runs_flow_started",
        "steve-flow runs.rs page_of",
        "SELECT * FROM flow_runs WHERE flow_id = ?1 AND started < ?2 AND (?4 IS NULL OR dry_run = ?4)
           ORDER BY started DESC, rowid DESC LIMIT ?3",
    ),
    (
        "flow_runs_event",
        "steve-flow signals.rs detail",
        "SELECT id FROM flow_runs WHERE event_id = ? AND dry_run = 0 ORDER BY started, rowid",
    ),
    (
        "ext_events_conn_type",
        "steve-flow sources.rs stored_24h",
        "SELECT COUNT(*) FROM ext_events WHERE connection_id = ? AND type = ? AND received >= ?",
    ),
    // ---------------------------------------------------------------- mail, GitHub, vault
    (
        "mail_messages_conn_mid",
        "steve-mail repo.rs is_mine",
        "SELECT 1 FROM mail_messages WHERE connection_id = ? AND message_id = ? AND direction = 'out' LIMIT 1",
    ),
    (
        "mail_messages_conn_mid",
        "steve-mail repo.rs (message without Message-ID)",
        "SELECT id FROM mail_messages WHERE connection_id = ? AND message_id IS NULL
            AND from_addr = ? AND date = ? AND subject = ? LIMIT 1",
    ),
    (
        "mail_messages_expire",
        "steve-mail repo.rs expired",
        "SELECT id, raw_path FROM mail_messages WHERE workspace_id = ? AND date < ? AND purged IS NULL
          ORDER BY date LIMIT ?",
    ),
    (
        "mail_drafts_thread",
        "steve-mail threads.rs brief",
        "SELECT id, status FROM mail_drafts WHERE thread_id = ? AND status NOT IN ('sent', 'discarded') ORDER BY updated DESC, rowid DESC LIMIT 1",
    ),
    (
        "mail_drafts_thread",
        "steve-mail repo.rs thread_drafts",
        "SELECT id FROM mail_drafts WHERE thread_id = ? AND status != 'discarded' ORDER BY created, rowid",
    ),
    (
        "gh_deliveries_pending",
        "steve-github repo.rs received_deliveries",
        "SELECT d.delivery_id, d.event, i.source, i.payload
           FROM gh_deliveries d JOIN gh_delivery_inputs i ON i.delivery_id = d.delivery_id
          WHERE d.status = 'received' AND i.next_at <= ?
          ORDER BY d.received, d.rowid LIMIT ?",
    ),
    (
        "secret_uses_status",
        "steve-modules vault/use_repo.rs uses_in",
        "SELECT u.id FROM secret_uses u WHERE u.status = ? AND u.created < ? ORDER BY u.created, u.id",
    ),
    (
        "secret_uses_created",
        "steve-modules vault/use_repo.rs delete_uses_before",
        "DELETE FROM secret_uses WHERE created < ?",
    ),
    // ---------------------------------------------------------------- sessions, stats
    (
        "sessions_token",
        "steve-store repo/sessions.rs by_token",
        "SELECT * FROM sessions WHERE token_hash = ? AND status <> 'ended'",
    ),
    (
        "sessions_ws_updated",
        "steve-store repo/sessions.rs list(Some)",
        "SELECT * FROM sessions WHERE workspace_id = ? ORDER BY updated DESC LIMIT 300",
    ),
    (
        "sessions_updated",
        "steve-store repo/sessions.rs list(None)",
        "SELECT * FROM sessions ORDER BY updated DESC LIMIT 1000",
    ),
    (
        "thread_runs_token",
        "steve-store repo/thread.rs run_by_token",
        "SELECT * FROM thread_runs WHERE token_hash = ? AND status = 'running'",
    ),
    (
        "agent_turns_session",
        "steve-stats backfill.rs",
        "SELECT MIN(started) FROM agent_turns WHERE session_id = ? AND backfill = 0",
    ),
    (
        "agent_turns_session",
        "steve-stats views.rs",
        "SELECT COUNT(*) FROM agent_turns WHERE session_id = ? AND attempt = 0",
    ),
    (
        "usage_turn",
        "steve-hub wire/acp_stats.rs repair_codex",
        "SELECT t.id, t.session_id, t.started, t.ended, s.acp_session_id FROM agent_turns t
         JOIN sessions s ON s.id = t.session_id
         WHERE t.harness = 'codex' AND t.backfill = 0 AND t.ended IS NOT NULL AND s.acp_session_id IS NOT NULL
           AND NOT EXISTS (SELECT 1 FROM usage_records u WHERE u.turn_id = t.id)
         ORDER BY t.started",
    ),
    // ---------------------------------------------------------------- items
    (
        "audit_target",
        "steve-knowledge context.rs item_user",
        "SELECT a.actor FROM audit a JOIN workspace_members w ON w.workspace_id = a.workspace_id AND w.user_id = a.actor
          WHERE a.workspace_id = ? AND a.action = 'item.create' AND a.target = ? ORDER BY a.id LIMIT 1",
    ),
    (
        "audit_target",
        "steve-stats backfill.rs",
        "SELECT ts, actor, action FROM audit WHERE target = ? AND action IN ('item.drop', 'item.pause') ORDER BY ts",
    ),
    (
        "revisions_object",
        "steve-store repo/items.rs plan_rounds",
        "SELECT version, patch, created FROM revisions WHERE object_type = 'item_plan' AND object_id = ? ORDER BY version",
    ),
    (
        "revisions_object",
        "steve-store repo/items.rs add_plan_round",
        "SELECT MAX(version) FROM revisions WHERE object_type = 'item_plan' AND object_id = ?",
    ),
    (
        "revisions_object",
        "steve-store base/mod.rs revisions",
        "SELECT version, patch, author, created FROM revisions WHERE workspace_id = ? AND object_type = ? AND object_id = ? ORDER BY id DESC",
    ),
    (
        "artifacts_item",
        "steve-store repo/items.rs Artifacts::list",
        "SELECT * FROM artifacts WHERE item_id = ? ORDER BY created, rowid",
    ),
    (
        "artifacts_item",
        "steve-store repo/items.rs Artifacts::roles",
        "SELECT r.artifact_id, r.role FROM artifact_roles r JOIN artifacts a ON a.id = r.artifact_id WHERE a.item_id = ? ORDER BY a.created, a.rowid",
    ),
    (
        "item_tags_tag",
        "steve-core repo/mod.rs tag_used",
        "SELECT (SELECT count(*) FROM item_tags WHERE tag_id = ?1) + (SELECT count(*) FROM entity_tags WHERE tag_id = ?1)",
    ),
    (
        "item_tags_tag",
        "steve-store repo/catalog.rs tag counts",
        "SELECT t.id, count(it.item_id) n FROM tags t LEFT JOIN item_tags it ON it.tag_id = t.id WHERE t.workspace_id = ? GROUP BY t.id",
    ),
    (
        "item_tags_tag",
        "steve-store repo/catalog.rs remove",
        "DELETE FROM item_tags WHERE tag_id = ?",
    ),
    (
        "item_deps_dep",
        "steve-store repo/items.rs dependents",
        "SELECT item_id FROM item_deps WHERE dep_id = ?",
    ),
    // ---------------------------------------------------------------- YOLO
    (
        "yolo_actions_window",
        "steve-store repo/yolo.rs actions_of_window",
        "SELECT id FROM yolo_actions WHERE window_id = ? ORDER BY created, rowid",
    ),
    (
        "yolo_actions_window",
        "steve-store repo/yolo.rs counts",
        "SELECT outcome, count(*) FROM yolo_actions WHERE window_id = ? GROUP BY outcome",
    ),
    (
        "yolo_actions_window",
        "steve-store repo/yolo.rs cancel_scheduled",
        "SELECT id, decision_id FROM yolo_actions WHERE window_id = ? AND outcome = 'scheduled'",
    ),
    (
        "yolo_actions_decision",
        "steve-store repo/yolo.rs actions_of_decision",
        "SELECT id FROM yolo_actions WHERE decision_id = ? ORDER BY created, rowid",
    ),
];

#[test]
fn each_new_index_serves_its_query() {
    let t = fresh();
    t.store
        .with_conn(|c| {
            let mut wrong = Vec::new();
            for (index, at, sql) in CASES {
                let p = plan(c, sql);
                if !uses(&p, index) {
                    wrong.push(format!("{index} ({at}): {p:?}"));
                }
            }
            assert!(
                wrong.is_empty(),
                "not using the index:\n{}",
                wrong.join("\n")
            );
            Ok(())
        })
        .unwrap();
}

/// Queries whose best plan depends on the data: on an empty table the planner prefers another path,
/// so these are checked after ANALYZE on a database shaped like a real one (`PRAGMA optimize` at open
/// and hourly keeps those statistics).
const ANALYZED: &[(&str, &str, &str)] = &[
    (
        "timeline_me_ts",
        "steve-stats metrics.rs (the user's messages)",
        "SELECT COUNT(*) FROM timeline t JOIN items i ON i.id = t.item_id
         WHERE i.workspace_id = ?1 AND t.kind = 'me' AND t.actor = 'me' AND t.ts >= ?2 AND t.ts < ?3 AND (?4 IS NULL OR i.project_id = ?4) AND (?5 IS NULL OR i.bot_id = ?5)",
    ),
];

#[test]
fn indexes_that_need_statistics_serve_their_query_once_analyzed() {
    let t = fresh();
    t.store
        .with_conn(|c| {
            // one workspace, many items, many timeline rows per item and few of them the user's own
            for i in 0..200 {
                c.execute(
                    "INSERT INTO items (id, workspace_id, no, title, status, stage, created, updated)
                     VALUES (?1, 'default', ?2, 't', 'open', 'x', 1, 1)",
                    rusqlite::params![format!("ST-{i}"), i],
                )?;
                for j in 0..50 {
                    let (actor, kind) = if j == 0 { ("me", "me") } else { ("steve", "event") };
                    c.execute(
                        "INSERT INTO timeline (item_id, ts, actor, kind, text) VALUES (?, ?, ?, ?, '')",
                        rusqlite::params![format!("ST-{i}"), i * 100 + j, actor, kind],
                    )?;
                }
            }
            c.execute_batch("ANALYZE")?;
            for (index, at, sql) in ANALYZED {
                let p = plan(c, sql);
                assert!(uses(&p, index), "{index} ({at}): {p:?}");
            }
            Ok(())
        })
        .unwrap();
}

#[test]
fn every_index_of_035_has_a_query() {
    let names: Vec<&str> = steve_store::db::MIGRATIONS[34]
        .lines()
        .filter_map(|l| l.strip_prefix("CREATE INDEX IF NOT EXISTS "))
        .map(|l| l.split_whitespace().next().unwrap())
        .collect();
    assert!(!names.is_empty());
    for n in names {
        assert!(
            CASES.iter().chain(ANALYZED).any(|(i, _, _)| *i == n),
            "{n} has no query in CASES"
        );
    }
}
