use crate::{Result, Store};
use rusqlite::params;

pub fn merge_marker(store: &Store, id: &str, into: Option<&str>) -> Result<()> {
    store.write_locked(|c| {
        let status = if into.is_some() { "merged" } else { "active" };
        c.execute(
            "UPDATE parties SET status = ?, merged_into = ? WHERE id = ?",
            params![status, into, id],
        )?;
        Ok(())
    })
}

pub fn workspace_jira(store: &Store) -> Result<()> {
    store.write_locked(|c| {
        c.execute("INSERT INTO systems (id, workspace_id, name, kind, family, handle_rule, created) VALUES ('jira', 'default', 'Jira', 'code', 'other', ?, 1)", ["{\"normalize\":\"lower\"}"])?;
        Ok(())
    })
}

pub fn gmail_connection(store: &Store, account: &str, read_mail: bool) -> Result<()> {
    store.write_locked(|c| {
        let caps = if read_mail { "[\"mail.read\"]" } else { "[]" };
        c.execute("INSERT INTO connections (id, workspace_id, account_id, adapter, auth, caps, created_by, created, updated) VALUES ('con_1', 'default', ?, 'gmail', 'oauth', ?, 'usr_dev', 1, 1)", [account, caps])?;
        Ok(())
    })
}

pub fn linked_vault_items(store: &Store, account: &str) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute_batch("INSERT INTO vaults (id, workspace_id, name, scope, owner_user, created) VALUES ('vlt_s', 'default', '共享', 'shared', NULL, 1), ('vlt_p', 'default', '个人', 'personal', 'usr_other', 1), ('vlt_x', 'default', '系统', 'system', NULL, 1)")?;
        tx.conn.execute("INSERT INTO vault_items (id, workspace_id, vault_id, template, title, account_id, sensitivity, created_by, created, updated) VALUES
           ('vi_s', 'default', 'vlt_s', 'login', 'AWS 登录', ?1, 'sensitive', 'usr_dev', 1, 1),
           ('vi_p', 'default', 'vlt_p', 'login', '别人的', ?1, 'sensitive', 'usr_other', 2, 2),
           ('vi_x', 'default', 'vlt_x', 'connection_credential', '凭据', ?1, 'sensitive', 'usr_dev', 3, 3)",[account])?;
        Ok(())
    })
}

pub fn account_count(store: &Store) -> Result<i64> {
    store.sqlite_read(|c| Ok(c.query_row("SELECT COUNT(*) FROM accounts", [], |r| r.get(0))?))
}

pub fn proposal_count(store: &Store) -> Result<i64> {
    store
        .sqlite_read(|c| Ok(c.query_row("SELECT COUNT(*) FROM merge_proposals", [], |r| r.get(0))?))
}

pub fn handle_count(store: &Store) -> Result<i64> {
    store
        .sqlite_read(|c| Ok(c.query_row("SELECT COUNT(*) FROM account_handles", [], |r| r.get(0))?))
}

pub fn vault_item_count(store: &Store, linked: bool) -> Result<i64> {
    store.sqlite_read(|c| {
        Ok(c.query_row(
            "SELECT COUNT(*) FROM vault_items WHERE ? = 0 OR account_id IS NOT NULL",
            [linked],
            |r| r.get(0),
        )?)
    })
}

pub struct Proposal {
    pub from_id: String,
    pub into_id: String,
    pub account: Option<String>,
    pub reason: String,
    pub confidence: f64,
    pub status: String,
    pub decision: Option<String>,
}

pub fn proposal(store: &Store, id: &str) -> Result<Proposal> {
    store.sqlite_read(|c| Ok(c.query_row(
        "SELECT from_id, into_id, account_id, reason, confidence, status, decision_id FROM merge_proposals WHERE id = ?",
        [id], |r| Ok(Proposal { from_id:r.get(0)?, into_id:r.get(1)?, account:r.get(2)?,
            reason:r.get(3)?, confidence:r.get(4)?, status:r.get(5)?, decision:r.get(6)? }),
    )?))
}

pub fn sole_handle(store: &Store) -> Result<(String, String)> {
    store.sqlite_read(|c| {
        Ok(
            c.query_row("SELECT handle, handle_display FROM accounts", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?,
        )
    })
}

pub fn account_storage_text(store: &Store) -> Result<String> {
    store.sqlite_read(|c| {
        let mut out = String::new();
        for table in ["accounts", "account_handles", "audit", "revisions"] {
            let mut statement = c.prepare(&format!("SELECT * FROM {table}"))?;
            let columns = statement.column_count();
            let mut rows = statement.query([])?;
            while let Some(row) = rows.next()? {
                for index in 0..columns {
                    if let Ok(text) = row.get::<_, String>(index) {
                        out.push_str(&text);
                    }
                }
            }
        }
        Ok(out)
    })
}

pub fn last_audit(store: &Store, target: Option<&str>) -> Result<(String, String)> {
    store.sqlite_read(|c| {
        Ok(c.query_row(
        "SELECT action, detail FROM audit WHERE ?1 IS NULL OR target = ?1 ORDER BY id DESC LIMIT 1",
        [target], |r| Ok((r.get(0)?,r.get(1)?)),
    )?)
    })
}

pub fn account_revision(store: &Store, id: &str) -> Result<String> {
    store.sqlite_read(|c| {
        Ok(c.query_row(
            "SELECT patch FROM revisions WHERE object_type = 'account' AND object_id = ?",
            [id],
            |r| r.get(0),
        )?)
    })
}

pub fn phone_accounts(store: &Store) -> Result<Vec<(String, String, String, String)>> {
    store.sqlite_read(|c| {
        let mut statement = c.prepare_cached(
            "SELECT handle, kind, source, source_label FROM accounts WHERE system_id = 'phone'",
        )?;
        let rows = statement.query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    })
}
