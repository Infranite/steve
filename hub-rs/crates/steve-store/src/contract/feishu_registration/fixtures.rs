use crate::{Result, Store};
use rusqlite::params;

pub enum RegistrationRows {
    All,
    WithCredential,
    WithTicket,
    WithTicketOrCredential,
}

pub fn registrations(store: &Store, rows: RegistrationRows) -> Result<i64> {
    let filter = match rows {
        RegistrationRows::All => "1",
        RegistrationRows::WithCredential => "vault_item IS NOT NULL",
        RegistrationRows::WithTicket => "device_item IS NOT NULL",
        RegistrationRows::WithTicketOrCredential => {
            "device_item IS NOT NULL OR vault_item IS NOT NULL"
        }
    };
    store.with_conn(|c| {
        Ok(c.query_row(
            &format!("SELECT count(*) FROM chan_app_registrations WHERE {filter}"),
            [],
            |r| r.get(0),
        )?)
    })
}

pub fn connections(store: &Store, adapter: Option<&str>) -> Result<i64> {
    store.with_conn(|c| {
        Ok(c.query_row(
            "SELECT count(*) FROM connections WHERE ?1 IS NULL OR adapter = ?1",
            [adapter],
            |r| r.get(0),
        )?)
    })
}

pub fn connection_id(store: &Store, adapter: &str) -> Result<String> {
    store.with_conn(|c| {
        Ok(c.query_row(
            "SELECT id FROM connections WHERE adapter = ?",
            [adapter],
            |r| r.get(0),
        )?)
    })
}

pub fn connection_domain(store: &Store, adapter: &str) -> Result<String> {
    store.with_conn(|c| {
        Ok(c.query_row(
            "SELECT json_extract(config, '$.domain') FROM connections WHERE adapter = ?",
            [adapter],
            |r| r.get(0),
        )?)
    })
}

pub fn connection_owner_and_credential(store: &Store, adapter: &str) -> Result<(String, String)> {
    store.with_conn(|c| {
        Ok(c.query_row(
            "SELECT created_by, credential_item FROM connections WHERE adapter = ?",
            [adapter],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?)
    })
}

pub fn credential_stamp(store: &Store) -> Result<(String, i64)> {
    store.with_conn(|c| {
        Ok(c.query_row(
            "SELECT credential_item, updated FROM connections",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?)
    })
}

pub fn granted_scopes(store: &Store, account: &str, adapter: &str) -> Result<Vec<String>> {
    let raw: String = store.with_conn(|c| {
        Ok(c.query_row(
            "SELECT granted_scopes FROM connections WHERE account_id = ? AND adapter = ?",
            [account, adapter],
            |r| r.get(0),
        )?)
    })?;
    Ok(serde_json::from_str(&raw)?)
}

pub fn change_app_connection(store: &Store, id: &str, app: &str) -> Result<()> {
    store.with_conn(|c| {
        c.execute(
            "UPDATE connections SET config = json_set(config, '$.app_connection', ?) WHERE id = ?",
            params![app, id],
        )?;
        Ok(())
    })
}

pub fn active_credentials(store: &Store, title: &str) -> Result<i64> {
    store.with_conn(|c| {
        Ok(c.query_row(
            "SELECT count(*) FROM vault_items WHERE status = 'active' AND title LIKE ?",
            [title],
            |r| r.get(0),
        )?)
    })
}

pub fn expire(store: &Store, id: &str) -> Result<()> {
    store.with_conn(|c| {
        c.execute(
            "UPDATE chan_app_registrations SET expires_at = 1 WHERE id = ?",
            [id],
        )?;
        Ok(())
    })
}

pub fn interrupt_verification(store: &Store, id: &str) -> Result<()> {
    store.with_conn(|c| {
        c.execute(
            "UPDATE chan_app_registrations SET state = 'verifying', error = NULL, error_kind = NULL WHERE id = ?",
            [id],
        )?;
        Ok(())
    })
}

pub fn remove_member(store: &Store, ws: &str, user: &str) -> Result<()> {
    store.with_conn(|c| {
        c.execute(
            "DELETE FROM workspace_members WHERE workspace_id = ? AND user_id = ?",
            params![ws, user],
        )?;
        Ok(())
    })
}
