use super::row;
use crate::{Result, Store};

pub fn connection_for_account(store: &Store, key: &str) -> Result<Option<Vec<serde_json::Value>>> {
    row(
        store,
        "SELECT id, status, caps, credential_item, adapter, health, config FROM connections WHERE account_id = ?",
        rusqlite::params![key],
    )
}

pub fn connected_user(store: &Store, key: &str) -> Result<Option<Vec<serde_json::Value>>> {
    row(
        store,
        "SELECT a.handle, a.source, a.verified, p.user_id, a.system_id FROM accounts a JOIN parties p ON p.id = a.party_id WHERE a.id = ?",
        rusqlite::params![key],
    )
}

pub fn credential(store: &Store, key: &str) -> Result<Option<Vec<serde_json::Value>>> {
    row(
        store,
        "SELECT v.scope, i.template, i.account_id, i.dek_wrapped IS NOT NULL FROM vault_items i JOIN vaults v ON v.id = i.vault_id WHERE i.id = ?",
        rusqlite::params![key],
    )
}

pub fn grant_fields_with_bad_encryption(
    store: &Store,
    key: &str,
) -> Result<Option<Vec<serde_json::Value>>> {
    row(
        store,
        "SELECT COUNT(*) FROM vault_fields WHERE item_id = ? AND (value_plain IS NOT NULL OR value_ct IS NULL)",
        rusqlite::params![key],
    )
}

pub fn destroyed_credential(store: &Store, key: &str) -> Result<Option<Vec<serde_json::Value>>> {
    row(
        store,
        "SELECT dek_wrapped, status FROM vault_items WHERE id = ?",
        rusqlite::params![key],
    )
}

pub fn workspace_connections(store: &Store, key: &str) -> Result<Option<Vec<serde_json::Value>>> {
    row(
        store,
        "SELECT COUNT(*) FROM connections WHERE workspace_id = ?",
        rusqlite::params![key],
    )
}

pub fn wrapped_key(store: &Store, key: &str) -> Result<Option<Vec<serde_json::Value>>> {
    row(
        store,
        "SELECT dek_wrapped FROM vault_items WHERE id = ?",
        rusqlite::params![key],
    )
}

pub fn bot_account(store: &Store, key: &str) -> Result<Option<Vec<serde_json::Value>>> {
    row(
        store,
        "SELECT a.handle, a.kind, p.type, p.org_kind FROM accounts a JOIN parties p ON p.id = a.party_id WHERE a.id = ?",
        rusqlite::params![key],
    )
}

pub fn connected_address(store: &Store, key: &str) -> Result<Option<Vec<serde_json::Value>>> {
    row(
        store,
        "SELECT a.handle, a.system_id, p.user_id FROM accounts a JOIN parties p ON p.id = a.party_id WHERE a.id = ?",
        rusqlite::params![key],
    )
}

pub fn claimed_account(store: &Store, key: &str) -> Result<Option<Vec<serde_json::Value>>> {
    row(
        store,
        "SELECT a.handle, a.kind, p.org_kind FROM accounts a JOIN parties p ON p.id = a.party_id WHERE a.id = ?",
        rusqlite::params![key],
    )
}

pub fn retained_credential(store: &Store, key: &str) -> Result<Option<Vec<serde_json::Value>>> {
    row(
        store,
        "SELECT dek_wrapped, status, (SELECT COUNT(*) FROM vault_fields f WHERE f.item_id = vault_items.id AND f.value_ct IS NOT NULL) FROM vault_items WHERE id = ?",
        rusqlite::params![key],
    )
}

pub fn connection_exists(store: &Store, key: &str) -> Result<Option<Vec<serde_json::Value>>> {
    row(
        store,
        "SELECT id FROM connections WHERE id = ?",
        rusqlite::params![key],
    )
}

pub fn account_exists(store: &Store, key: &str) -> Result<Option<Vec<serde_json::Value>>> {
    row(
        store,
        "SELECT id FROM accounts WHERE id = ?",
        rusqlite::params![key],
    )
}

pub fn linked_handle(store: &Store, key: &str) -> Result<Option<Vec<serde_json::Value>>> {
    row(
        store,
        "SELECT handle FROM account_handles WHERE connection_id = ?",
        rusqlite::params![key],
    )
}

pub fn account_handle(store: &Store, key: &str) -> Result<Option<Vec<serde_json::Value>>> {
    row(
        store,
        "SELECT handle FROM accounts WHERE id = ?",
        rusqlite::params![key],
    )
}

pub fn contact_for_account(store: &Store, key: &str) -> Result<Option<Vec<serde_json::Value>>> {
    row(
        store,
        "SELECT p.id, p.name, p.contact_kind FROM accounts a JOIN parties p ON p.id = a.party_id WHERE a.id = ?",
        rusqlite::params![key],
    )
}

pub fn owner_party(store: &Store, key: &str) -> Result<Option<Vec<serde_json::Value>>> {
    row(
        store,
        "SELECT id FROM parties WHERE user_id = ?",
        rusqlite::params![key],
    )
}

pub fn steward(store: &Store, t: &str, key: &str) -> Result<Option<Vec<serde_json::Value>>> {
    row(
        store,
        "SELECT person_id, source FROM stewards WHERE object_type = ? AND object_id = ?",
        rusqlite::params![t, key],
    )
}

pub fn party_for_account(store: &Store, key: &str) -> Result<Option<Vec<serde_json::Value>>> {
    row(
        store,
        "SELECT party_id FROM accounts WHERE id = ?",
        rusqlite::params![key],
    )
}

pub fn party_steward(store: &Store, key: &str) -> Result<Option<Vec<serde_json::Value>>> {
    row(
        store,
        "SELECT person_id, source FROM stewards WHERE object_type = 'party' AND object_id = ?",
        rusqlite::params![key],
    )
}

pub fn open_connection_decisions(store: &Store, con: &str) -> Result<Vec<serde_json::Value>> {
    store.reader(|c| {
            let mut st = c.prepare(
                "SELECT question, options, kind FROM decisions WHERE subject_type = 'connection' AND subject_id = ? AND status = 'open'",
            )?;
            let rows = st.query_map([con], |r| {
                Ok(serde_json::json!([r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?]))
            })?;
            Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
        })
}

pub fn open_decision(store: &Store, con: &str) -> Result<String> {
    store.reader(|c| {
        Ok(c.query_row(
            "SELECT id FROM decisions WHERE subject_id = ? AND status = 'open'",
            [&con],
            |r| r.get::<_, String>(0),
        )?)
    })
}

pub fn enabled_at(store: &Store, con: &str) -> Result<Option<i64>> {
    store.reader(|c| {
        Ok(c.query_row(
            "SELECT enabled_at FROM connections WHERE id = ?",
            [&con],
            |r| r.get::<_, Option<i64>>(0),
        )?)
    })
}

pub fn owned_handle_count(store: &Store, acc: &str, app_con: &str) -> Result<i64> {
    store.reader(|c| {
        Ok(c.query_row(
            "SELECT COUNT(*) FROM account_handles WHERE account_id = ? AND connection_id = ?",
            rusqlite::params![acc, app_con],
            |r| r.get::<_, i64>(0),
        )?)
    })
}

pub fn active_connection_credentials(store: &Store) -> Result<i64> {
    store.reader(|c| {
            Ok(c.query_row(
                "SELECT COUNT(*) FROM vault_items WHERE template = 'connection_credential' AND status = 'active'",
                [],
                |r| r.get::<_, i64>(0),
            )?)
        })
}

pub fn seed_contact_account(store: &Store) -> Result<()> {
    store.with_conn(|c| {
            c.execute_batch(
                "INSERT INTO parties (id, workspace_id, type, name, contact_kind, created_by, created, updated) VALUES ('ct_zhang', 'default', 'contact', '张三', 'real', 'usr_dev', 1, 1);
                 INSERT INTO accounts (id, workspace_id, system_id, family, party_id, kind, handle, handle_display, source, created, updated) VALUES ('acc_z', 'default', 'mail', 'email', 'ct_zhang', 'work_email', 'zhang@acme.com', 'zhang@acme.com', 'sync', 1, 1);
                 INSERT INTO account_handles (workspace_id, family, handle, account_id, created) VALUES ('default', 'email', 'zhang@acme.com', 'acc_z', 1);",
            )?;
            Ok(())
        })
}
