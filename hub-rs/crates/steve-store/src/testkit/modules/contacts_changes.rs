use super::{execute, scalar};
use crate::{Result, Store};
use serde_json::Value;

pub fn changeset_ops(store: &Store, cs: &str) -> Result<String> {
    scalar(
        store,
        "SELECT ops FROM changesets WHERE id = ?",
        rusqlite::params![cs],
    )
}

pub fn globex_kind(store: &Store) -> Result<String> {
    scalar(
        store,
        "SELECT org_kind FROM parties WHERE name = 'Globex'",
        [],
    )
}

pub fn only_team(store: &Store) -> Result<String> {
    scalar(store, "SELECT id FROM parties WHERE type = 'team'", [])
}

pub fn account_count(store: &Store, arg1: &str) -> Result<i64> {
    scalar(
        store,
        "SELECT COUNT(*) FROM accounts WHERE party_id = ?",
        rusqlite::params![arg1],
    )
}

pub fn hooli_count(store: &Store) -> Result<i64> {
    scalar(
        store,
        "SELECT COUNT(*) FROM parties WHERE lower(name) = 'hooli'",
        [],
    )
}

pub fn party_status(store: &Store, hooli: &str) -> Result<String> {
    scalar(
        store,
        "SELECT status FROM parties WHERE id = ?",
        rusqlite::params![hooli],
    )
}

pub fn pied_piper_id(store: &Store) -> Result<String> {
    scalar(
        store,
        "SELECT id FROM parties WHERE name = 'Pied Piper'",
        [],
    )
}

pub fn seed_synced_team(store: &Store) -> Result<()> {
    execute(store, "INSERT INTO parties (id, workspace_id, type, name, source, created_by, created, updated) VALUES ('tm_rd', 'default', 'team', '研发', 'sync', 'usr_dev', 1, 1);
         INSERT INTO team_details (party_id, workspace_id, kind, sync_connection, external_id, synced) VALUES ('tm_rd', 'default', 'chart', 'con_fs', 'dep_rd', 1);")
}

pub fn synced_team_purpose(store: &Store) -> Result<String> {
    scalar(
        store,
        "SELECT purpose FROM team_details WHERE party_id = 'tm_rd'",
        [],
    )
}

pub fn changeset_title(store: &Store, arg1: &str) -> Result<String> {
    scalar(
        store,
        "SELECT title FROM changesets WHERE id = ?",
        rusqlite::params![arg1],
    )
}

pub fn party_memory(store: &Store, party: &str) -> Result<(String, String, String)> {
    store.with_conn(|c| {
            Ok(c.query_row(
                "SELECT m.text, m.status, m.source_type FROM mems m JOIN mem_about a ON a.mem_id = m.id WHERE a.object_id = ?",
                [party],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )?)
        })
}

pub fn snapshot(store: &Store) -> serde_json::Value {
    let rows = |sql: &str| -> Value {
        store
            .with_conn(|c| {
                let mut st = c.prepare(sql)?;
                let n = st.column_count();
                let rows = st
                    .query_map([], |r| {
                        let mut row = Vec::new();
                        for i in 0..n {
                            let v: rusqlite::types::Value = r.get(i)?;
                            row.push(match v {
                                rusqlite::types::Value::Null => Value::Null,
                                rusqlite::types::Value::Integer(i) => serde_json::json!(i),
                                rusqlite::types::Value::Real(f) => serde_json::json!(f),
                                rusqlite::types::Value::Text(s) => serde_json::json!(s),
                                rusqlite::types::Value::Blob(_) => serde_json::json!("blob"),
                            });
                        }
                        Ok(Value::Array(row))
                    })?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(Value::Array(rows))
            })
            .expect("rows")
    };
    serde_json::json!({
        "parties": rows("SELECT id, type, name, aliases, rels, important, note, org_kind, status FROM parties ORDER BY id"),
        "links": rows("SELECT subject_id, predicate, object_id, props FROM links ORDER BY subject_id, object_id"),
        "mems": rows("SELECT id, kind, text FROM mems ORDER BY id"),
        "attrs": rows("SELECT id, key, label, value, status FROM attr_values ORDER BY id"),
        "defs": rows("SELECT key FROM attr_defs WHERE workspace_id != '*' ORDER BY key"),
        "teams": rows("SELECT party_id, kind, purpose FROM team_details ORDER BY party_id"),
        "members": rows("SELECT team_id, contact_id, role, source FROM team_members ORDER BY team_id, contact_id"),
        "domains": rows("SELECT domain, org_id FROM org_domains ORDER BY domain"),
    })
}
