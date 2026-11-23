use crate::{Result, Store};

pub fn shared_skill_across_workspaces(store: &Store) -> Result<()> {
    store.with_conn(|c| {
        c.execute_batch(
            "INSERT INTO workspaces (id,name,created) VALUES ('home-a','A',1),('home-b','B',1);
             INSERT INTO bots (id,workspace_id,name,harness,updated) VALUES ('bot-a','home-a','A','codex',1),('bot-b','home-b','B','codex',1);
             INSERT INTO bot_capabilities (bot_id,kind,entry_id,enabled,created) VALUES ('bot-a','skill','shared',1,1),('bot-b','skill','shared',1,1);"
        )?;
        Ok(())
    })
}

pub fn capability_sessions(store: &Store) -> Result<()> {
    store.with_conn(|c| {
        c.execute_batch(
            "INSERT OR IGNORE INTO bots (id, workspace_id, name, harness, updated) VALUES ('dev', 'default', '开发', 'claude-code', 1), ('doc', 'default', '文档', 'codex', 1);
             INSERT INTO bot_workspaces (bot_id, workspace_id, created) VALUES ('dev', 'default', 1), ('doc', 'default', 1);
             INSERT INTO sessions (id, workspace_id, bot_id, harness, cwd, status, created, updated) VALUES ('s_dev', 'default', 'dev', 'claude-code', '.', 'active', 1, 1), ('s_doc', 'default', 'doc', 'codex', '.', 'active', 1, 1);"
        )?;
        Ok(())
    })
}

pub fn active_credentials(store: &Store) -> Result<i64> {
    store.reader(|c| {
        Ok(c.query_row(
            "SELECT COUNT(*) FROM vault_items WHERE status = 'active' AND template = 'connection_credential'",
            [], |row| row.get(0),
        )?)
    })
}

pub fn audit_actions(store: &Store, target: &str) -> Result<Vec<String>> {
    store.reader(|c| {
        let mut query =
            c.prepare_cached("SELECT action FROM audit WHERE target = ? ORDER BY id")?;
        let actions = query.query_map([target], |row| row.get(0))?;
        Ok(actions.collect::<rusqlite::Result<_>>()?)
    })
}

pub mod accounts;

pub mod directory;

pub fn notification_timezone(store: &Store, ws: &str, user: &str, zone: &str) -> Result<()> {
    store.with_conn(|c| {
        c.execute("INSERT INTO notification_policies (workspace_id,user_id,enabled,mode,timezone,consented_at,consent_since_seq,updated) VALUES (?,?,1,'now',?,1,0,1) ON CONFLICT(workspace_id,user_id) DO UPDATE SET timezone=excluded.timezone", rusqlite::params![ws,user,zone])?;
        Ok(())
    })
}

pub mod connections;
pub mod connections_pairing;
pub mod contact_profiles;
pub mod contacts;
pub mod contacts_changes;
pub mod merge;
pub mod ownership;
pub mod people_p3;
pub mod teams;

fn execute(store: &Store, sql: &str) -> Result<()> {
    store.with_conn(|c| Ok(c.execute_batch(sql)?))
}

fn scalar<T: rusqlite::types::FromSql>(
    store: &Store,
    sql: &str,
    params: impl rusqlite::Params,
) -> Result<T> {
    store.reader(|c| Ok(c.query_row(sql, params, |r| r.get(0))?))
}

fn row(
    store: &Store,
    sql: &str,
    params: impl rusqlite::Params,
) -> Result<Option<Vec<serde_json::Value>>> {
    store.reader(|c| {
        let mut query = c.prepare(sql)?;
        let columns = query.column_count();
        let mut rows = query.query(params)?;
        let Some(row) = rows.next()? else {
            return Ok(None);
        };
        Ok(Some(
            (0..columns)
                .map(|i| row.get_ref(i).map(json_value))
                .collect::<rusqlite::Result<_>>()?,
        ))
    })
}

fn value(store: &Store, sql: &str, params: impl rusqlite::Params) -> Result<serde_json::Value> {
    store.reader(|c| Ok(c.query_row(sql, params, |r| r.get_ref(0).map(json_value))?))
}

fn json_value(value: rusqlite::types::ValueRef<'_>) -> serde_json::Value {
    use rusqlite::types::ValueRef;
    use serde_json::{Value, json};
    match value {
        ValueRef::Null => Value::Null,
        ValueRef::Integer(value) => json!(value),
        ValueRef::Real(value) => json!(value),
        ValueRef::Text(value) => json!(String::from_utf8_lossy(value)),
        ValueRef::Blob(value) => json!(format!("blob:{}", value.len())),
    }
}

pub mod connections_secrets;

pub mod changes;
