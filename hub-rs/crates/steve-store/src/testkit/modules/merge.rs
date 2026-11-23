use super::{execute, scalar};
use crate::{Result, Store};

pub fn seed_items(store: &Store) -> Result<()> {
    execute(store, "INSERT INTO items (id, workspace_id, no, kind, title, status, stage, created, updated) VALUES
           ('ST-1', 'default', 1, 'task', '报价单', 'run', '定义', 1, 1),
           ('ST-2', 'default', 2, 'task', '交付', 'run', '定义', 2, 2);
         INSERT INTO tags (id, workspace_id, name, cat, description, created_by, created) VALUES
           ('tag_a', 'default', 'A', '客户', '', 'usr_dev', 1), ('tag_b', 'default', 'B', '客户', '', 'usr_dev', 1);")
}

pub fn link_shared_memory(store: &Store, arg1: &str, arg2: &str) -> Result<()> {
    execute(
        store,
        &format!(
            "INSERT INTO mem_about (mem_id, workspace_id, object_type, object_id) VALUES ('{arg1}', 'default', 'party', '{arg2}')"
        ),
    )
}

pub fn merge_audits(store: &Store) -> Result<i64> {
    scalar(
        store,
        "SELECT COUNT(*) FROM audit WHERE action = 'party.merge'",
        [],
    )
}

pub fn merge_payload(store: &Store) -> Result<String> {
    scalar(store, "SELECT moved FROM merges", [])
}

pub fn party_status(store: &Store, arg1: &str) -> Result<String> {
    scalar(
        store,
        "SELECT status FROM parties WHERE id = ?",
        rusqlite::params![arg1],
    )
}

pub fn split_audits(store: &Store) -> Result<i64> {
    scalar(
        store,
        "SELECT COUNT(*) FROM audit WHERE action = 'party.split'",
        [],
    )
}

pub fn timezone_attr(store: &Store) -> Result<String> {
    scalar(store, "SELECT id FROM attr_values WHERE key = 'tz'", [])
}

pub fn proposal_decision(store: &Store, mp: &str) -> Result<String> {
    scalar(
        store,
        "SELECT decision_id FROM merge_proposals WHERE id = ?",
        rusqlite::params![mp],
    )
}

pub fn proposal_status(store: &Store, mp: &str) -> Result<String> {
    scalar(
        store,
        "SELECT status FROM merge_proposals WHERE id = ?",
        rusqlite::params![mp],
    )
}

pub fn sender_party(store: &Store) -> Result<String> {
    scalar(
        store,
        "SELECT party_id FROM accounts WHERE handle = 'sarah.c.design@gmail.com'",
        [],
    )
}

pub fn proposal_count(store: &Store) -> Result<i64> {
    scalar(store, "SELECT COUNT(*) FROM merge_proposals", [])
}

pub fn open_proposal_count(store: &Store) -> Result<i64> {
    scalar(
        store,
        "SELECT COUNT(*) FROM merge_proposals WHERE status = 'open'",
        [],
    )
}

pub fn only_attribute(store: &Store) -> Result<String> {
    scalar(store, "SELECT id FROM attr_values", [])
}

pub fn seed_candidate(store: &Store, id: &str, sarah: &str, v: &str) -> Result<()> {
    execute(
        store,
        &format!(
            "INSERT INTO attr_values (id, workspace_id, party_id, key, label, value, source, source_label, level, status, created, updated) VALUES ('{id}', 'default', '{sarah}', 'phone', '电话', '{v}', 'steve', '邮件签名', 'reasoning', 'candidate', 2, 2)"
        ),
    )
}

pub fn candidate_status(store: &Store) -> Result<String> {
    scalar(
        store,
        "SELECT status FROM attr_values WHERE id = 'av_c1'",
        [],
    )
}

pub fn attr_status(store: &Store, old: &str) -> Result<String> {
    scalar(
        store,
        "SELECT status FROM attr_values WHERE id = ?",
        rusqlite::params![old],
    )
}

pub fn first_candidate_count(store: &Store) -> Result<i64> {
    scalar(
        store,
        "SELECT COUNT(*) FROM attr_values WHERE id = 'av_c1'",
        [],
    )
}

pub fn candidate_details(store: &Store) -> Result<String> {
    scalar(
        store,
        "SELECT value || '/' || status || '/' || source_label FROM attr_values WHERE id = 'av_c1'",
        [],
    )
}

pub fn second_candidate_count(store: &Store) -> Result<i64> {
    scalar(
        store,
        "SELECT COUNT(*) FROM attr_values WHERE id = 'av_c2'",
        [],
    )
}

pub fn seed_attr_definition(store: &Store) -> Result<()> {
    execute(
        store,
        "INSERT INTO attr_defs (id, workspace_id, applies, key, label, type, default_level, source, status, created) VALUES ('ad_p', 'default', 'contact', 'wechat', '微信', 'text', 'reasoning', 'steve', 'proposed', 1)",
    )
}

pub fn attr_def_status(store: &Store) -> Result<String> {
    scalar(store, "SELECT status FROM attr_defs WHERE id = 'ad_p'", [])
}

pub fn attr_def_count(store: &Store) -> Result<i64> {
    scalar(
        store,
        "SELECT COUNT(*) FROM attr_defs WHERE id = 'ad_p'",
        [],
    )
}

pub fn attr_def_details(store: &Store) -> Result<String> {
    scalar(
        store,
        "SELECT key || '/' || label || '/' || status FROM attr_defs WHERE id = 'ad_p'",
        [],
    )
}

pub fn status_and_merge_target(store: &Store, party: &str) -> Result<(String, String)> {
    store.write_locked(|c| {
        Ok(c.query_row(
            "SELECT status, merged_into FROM parties WHERE id = ?",
            [party],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?)
    })
}

fn strings(store: &Store, sql: &str) -> Vec<String> {
    store
        .write_locked(|c| {
            let mut st = c.prepare(sql)?;
            let v = st
                .query_map([], |r| r.get(0))?
                .collect::<rusqlite::Result<Vec<String>>>()?;
            Ok(v)
        })
        .expect("query")
}

pub fn snapshot(store: &Store, p: &str) -> serde_json::Value {
    let q = |sql: &str| strings(store, &sql.replace("$P", p));
    serde_json::json!({
        "attrs": q("SELECT key || '=' || value FROM attr_values WHERE party_id = '$P' ORDER BY 1"),
        "out": q("SELECT predicate || '>' || object_id FROM links WHERE subject_type = 'party' AND subject_id = '$P' ORDER BY 1"),
        "in": q("SELECT subject_id || '>' || predicate FROM links WHERE object_type = 'party' AND object_id = '$P' ORDER BY 1"),
        "mems": q("SELECT mem_id FROM mem_about WHERE object_type = 'party' AND object_id = '$P' ORDER BY 1"),
        "teams": q("SELECT team_id || ':' || role FROM team_members WHERE contact_id = '$P' ORDER BY 1"),
        "tags": q("SELECT tag_id FROM entity_tags WHERE object_type = 'party' AND object_id = '$P' ORDER BY 1"),
        "accounts": q("SELECT id FROM accounts WHERE party_id = '$P' ORDER BY 1"),
    })
}

pub fn merge_target(store: &Store, party: &str) -> Result<Option<String>> {
    scalar(
        store,
        "SELECT merged_into FROM parties WHERE id = ?",
        [party],
    )
}
