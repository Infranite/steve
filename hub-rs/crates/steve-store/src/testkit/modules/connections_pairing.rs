use super::value;
use crate::{Result, Store};

pub fn paired_app(store: &Store, key: &str) -> Result<serde_json::Value> {
    value(
        store,
        "SELECT json_extract(config, '$.app_connection') FROM connections WHERE id = ?",
        rusqlite::params![key],
    )
}

pub fn owner_contact(store: &Store, key: &str) -> Result<serde_json::Value> {
    value(
        store,
        "SELECT id FROM parties WHERE user_id = ? AND type = 'contact'",
        rusqlite::params![key],
    )
}

pub fn connection_count(store: &Store) -> Result<serde_json::Value> {
    value(store, "SELECT COUNT(*) FROM connections", [])
}

pub fn adapter_connections(store: &Store, key: &str) -> Result<serde_json::Value> {
    value(
        store,
        "SELECT COUNT(*) FROM connections WHERE adapter = ?",
        rusqlite::params![key],
    )
}

pub fn pairing_audits(store: &Store, key: &str) -> Result<serde_json::Value> {
    value(
        store,
        "SELECT COUNT(*) FROM audit WHERE action = 'connection.pair' AND target = ?",
        rusqlite::params![key],
    )
}

pub fn party_status(store: &Store, key: &str) -> Result<serde_json::Value> {
    value(
        store,
        "SELECT status FROM parties WHERE id = ?",
        rusqlite::params![key],
    )
}

pub fn merged_into(store: &Store, key: &str) -> Result<serde_json::Value> {
    value(
        store,
        "SELECT merged_into FROM parties WHERE id = ?",
        rusqlite::params![key],
    )
}

pub fn party_for_account(store: &Store, key: &str) -> Result<serde_json::Value> {
    value(
        store,
        "SELECT party_id FROM accounts WHERE id = ?",
        rusqlite::params![key],
    )
}

pub fn github_party(store: &Store, key: &str) -> Result<serde_json::Value> {
    value(
        store,
        "SELECT party_id FROM accounts WHERE system_id = 'github' AND handle = ?",
        rusqlite::params![key],
    )
}

pub fn connection_config(store: &Store, key: &str) -> Result<serde_json::Value> {
    value(
        store,
        "SELECT config FROM connections WHERE id = ?",
        rusqlite::params![key],
    )
}

pub fn connection_status(store: &Store, key: &str) -> Result<serde_json::Value> {
    value(
        store,
        "SELECT status FROM connections WHERE id = ?",
        rusqlite::params![key],
    )
}

pub fn open_decision_count(store: &Store, key: &str) -> Result<serde_json::Value> {
    value(
        store,
        "SELECT COUNT(*) FROM decisions WHERE subject_id = ? AND status = 'open'",
        rusqlite::params![key],
    )
}

pub fn revoke_connection(store: &Store, id: &str) -> Result<usize> {
    store.with_conn(|c| {
        Ok(c.execute(
            "UPDATE connections SET status = 'revoked' WHERE id = ?",
            [id],
        )?)
    })
}

pub fn enable_connection(store: &Store, id: &str) -> Result<usize> {
    store.with_conn(|c| Ok(c.execute("UPDATE connections SET status = 'ok' WHERE id = ?", [id])?))
}

pub fn confirm_contact(store: &Store, id: &str) -> Result<usize> {
    store.with_conn(|c| {
        Ok(c.execute(
            "UPDATE parties SET contact_kind = 'real', source = 'me' WHERE id = ?",
            [id],
        )?)
    })
}

pub fn clear_pair(store: &Store, id: &str) -> Result<usize> {
    store.with_conn(|c| {
        Ok(c.execute(
            "UPDATE connections SET config = json_set(config, '$.app_connection', '') WHERE id = ?",
            [id],
        )?)
    })
}

pub fn unregister_adapter(store: &Store, id: &str) -> Result<()> {
    store.transaction(|tx| {
        tx.execute(
            "UPDATE connections SET adapter = 'test_gone' WHERE id = ?",
            [id],
        )?;
        Ok::<_, crate::StoreError>(())
    })
}
