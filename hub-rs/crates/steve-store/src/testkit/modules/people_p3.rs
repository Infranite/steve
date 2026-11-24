use super::{execute, scalar};
use crate::{Result, Store};

pub fn seed_items_and_bots(store: &Store) -> Result<()> {
    execute(store, "INSERT INTO bots (id, workspace_id, name, harness, updated) VALUES ('bot_dev', 'default', 'Dev Bot', 'claude', 1), ('bot_off', 'default', 'Off Bot', 'claude', 1);
         UPDATE bots SET enabled = 0 WHERE id = 'bot_off';
         INSERT INTO bot_workspaces (bot_id, workspace_id, created) VALUES ('bot_dev', 'default', 1), ('bot_off', 'default', 1);
         INSERT INTO items(id, workspace_id, no, kind, title, phase, created, updated, outcome) VALUES ('ST-1', 'default', 1, 'task', '报价单', 'active', 1, 1, NULL),('ST-2', 'default', 2, 'task', '交付', 'active', 2, 2, NULL),('ST-3', 'default', 3, 'task', '旧事', 'closed', 3, 3, 'completed'),('W2-1', 'w2', 1, 'task', '别处', 'active', 4, 4, NULL);
         UPDATE items SET archived = 1 WHERE id = 'ST-3';")
}

pub fn promote_mention(store: &Store) -> Result<()> {
    execute(
        store,
        "UPDATE links SET source = 'mention', confidence = 1 WHERE subject_id = 'ST-2'",
    )
}

pub fn candidate_memory(store: &Store) -> Result<String> {
    scalar(store, "SELECT id FROM mems WHERE text = '周五不开会'", [])
}

pub fn memory_details(store: &Store, mm: &str) -> Result<String> {
    scalar(
        store,
        "SELECT status || '|' || writer || '|' || visibility || '|' || created_by || '|' || source_type || '|' || source_id FROM mems WHERE id = ?",
        rusqlite::params![mm],
    )
}

pub fn memory_subject(store: &Store, mm: &str) -> Result<String> {
    scalar(
        store,
        "SELECT object_id FROM mem_about WHERE mem_id = ?",
        rusqlite::params![mm],
    )
}

pub fn memory_origin(store: &Store, mm: &str) -> Result<String> {
    scalar(
        store,
        "SELECT origin || '|' || origin_id || '|' || origin_label FROM mem_candidates WHERE mem_id = ?",
        rusqlite::params![mm],
    )
}

pub fn memory_decision(store: &Store, mm: &str) -> Result<String> {
    scalar(
        store,
        "SELECT d.kind || '|' || d.subject_type || '|' || d.question || '|' || d.requested_by || '|' || d.options FROM decisions d JOIN mems m ON m.decision_id = d.id WHERE m.id = ?",
        rusqlite::params![mm],
    )
}

pub fn open_memory_decisions(store: &Store) -> Result<i64> {
    scalar(
        store,
        "SELECT COUNT(*) FROM decisions WHERE subject_type = 'mem' AND status = 'open'",
        [],
    )
}

pub fn unbound_candidates(store: &Store) -> Result<i64> {
    scalar(
        store,
        "SELECT COUNT(*) FROM mems WHERE status = 'candidate' AND decision_id IS NULL",
        [],
    )
}

pub fn memory_count(store: &Store) -> Result<i64> {
    scalar(store, "SELECT COUNT(*) FROM mems", [])
}

pub fn candidate_count(store: &Store) -> Result<i64> {
    scalar(store, "SELECT COUNT(*) FROM mem_candidates", [])
}

pub fn memory_audits(store: &Store) -> Result<i64> {
    scalar(
        store,
        "SELECT COUNT(*) FROM audit WHERE action IN ('mm.create', 'mm.patch', 'mm.delete')",
        [],
    )
}

pub fn only_memory(store: &Store) -> Result<String> {
    scalar(store, "SELECT id FROM mems", [])
}

pub fn decision_result(store: &Store) -> Result<String> {
    scalar(store, "SELECT status || '|' || result FROM decisions", [])
}

pub fn item_timeline(store: &Store) -> Result<String> {
    scalar(
        store,
        "SELECT group_concat(actor || ' ' || kind || ' ' || text, ' / ') FROM timeline WHERE item_id = 'ST-1'",
        [],
    )
}

pub fn removal_timeline_count(store: &Store) -> Result<i64> {
    scalar(
        store,
        "SELECT COUNT(*) FROM timeline WHERE text = '移除了 Sarah'",
        [],
    )
}

pub fn related_source_and_confidence(
    store: &Store,
    item: &str,
    party: &str,
) -> Result<Option<(String, f64)>> {
    store.write_locked(|c| {
            Ok(c.query_row(
                "SELECT source, confidence FROM links WHERE subject_type = 'item' AND subject_id = ?1 AND predicate = 'related' AND object_id = ?2",
                [item, party],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .ok())
        })
}
