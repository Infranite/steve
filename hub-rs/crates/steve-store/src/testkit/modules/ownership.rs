use super::execute;
use crate::{Result, Store};

pub fn seed_steward_bots(store: &Store) -> Result<()> {
    execute(store, "INSERT INTO bots (id, workspace_id, name, main, harness, updated) VALUES ('steve', 'default', 'Steve', 1, 'claude-code', 1), ('old', 'default', '旧的', 0, 'claude-code', 1);
         UPDATE bots SET deleted = 1 WHERE id = 'old';
         INSERT INTO personas (id, workspace_id, name, bot_id, created, updated) VALUES ('per_s', 'default', 'Steve', 'steve', 1, 1);
         INSERT OR IGNORE INTO devices (id, name, roles, seen, created) VALUES ('hub', 'Mac mini', '[\"Hub\"]', 1, 1);
         INSERT OR IGNORE INTO devices (id, name, roles, seen, created) VALUES ('dev_laptop', '笔记本', '[]', 1, 1);")
}

pub fn seed_feishu_system(store: &Store) -> Result<()> {
    execute(
        store,
        "INSERT OR IGNORE INTO systems (id, workspace_id, name, kind, family, created) VALUES ('feishu', '*', '飞书', 'comm', 'feishu_user', 1);",
    )
}

pub fn steward_rows(store: &Store) -> Result<Vec<String>> {
    store.write_locked(|c| {
            let mut st = c.prepare(
                "SELECT object_type || ' ' || object_id || ' ' || person_id || ' ' || role || ' ' || source || ' ' || status FROM stewards ORDER BY 1",
            )?;
            let v = st
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<String>>>()?;
            Ok(v)
        })
}

pub fn changeset_status_and_changes(store: &Store, id: &str) -> Result<(String, String)> {
    store.write_locked(|c| {
        Ok(c.query_row(
            "SELECT status, changes FROM changesets WHERE id = ?",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?)
    })
}

pub fn merge_id(store: &Store) -> Result<String> {
    store.write_locked(|c| Ok(c.query_row("SELECT id FROM merges", [], |r| r.get(0))?))
}

pub fn contact_kind(store: &Store, party: &str) -> Result<String> {
    store.write_locked(|c| {
        Ok(c.query_row(
            "SELECT contact_kind FROM parties WHERE id = ?",
            [party],
            |r| r.get(0),
        )?)
    })
}

pub fn open_steward_decisions(store: &Store) -> Result<i64> {
    store.write_locked(|c| {
        Ok(c.query_row(
            "SELECT count(*) FROM decisions WHERE subject_type = 'steward' AND status = 'open'",
            [],
            |r| r.get(0),
        )?)
    })
}

pub fn latest_applied_changeset(store: &Store) -> Result<String> {
    store.write_locked(|c| {
            Ok(c.query_row(
                "SELECT id FROM changesets WHERE origin = 'steve' AND status = 'applied' ORDER BY created DESC LIMIT 1",
                [],
                |r| r.get(0),
            )?)
        })
}
