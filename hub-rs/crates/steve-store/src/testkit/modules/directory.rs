use crate::{Accounts, Result, Store, StoreError};
use rusqlite::params;

pub fn seed_connection(
    store: &Store,
    ws: &str,
    id: &str,
    account: &str,
    app: &str,
    caps: &str,
) -> Result<()> {
    store.write(|tx| {
        let org: String = tx.conn.query_row(
            "SELECT id FROM parties WHERE workspace_id = ? AND org_kind = 'self'", [ws], |r| r.get(0),
        )?;
        tx.conn.execute(
            "INSERT INTO accounts (id, workspace_id, system_id, family, party_id, kind, handle, handle_display, source, created, updated)
             VALUES (?1, ?2, 'feishu_app', 'feishu_app', ?3, 'app_bot', ?4, ?4, 'connect', 1, 1)",
            params![account,ws,org,app],
        )?;
        tx.conn.execute(
            "INSERT INTO connections (id, workspace_id, account_id, adapter, auth, caps, created_by, created, updated)
             VALUES (?, ?, ?, 'feishu', 'credential', ?, 'usr_dev', 1, 1)",
            params![id,ws,account,caps],
        )?;
        Ok(())
    })
}

pub fn handle_owner(
    store: &Store,
    ws: &str,
    family: &str,
    handle: &str,
) -> Result<crate::account_rows::Account> {
    store.read(|r| {
        let mut accounts = Accounts(r);
        let id = accounts
            .by_handle(ws, family, handle)?
            .ok_or_else(|| StoreError::Rejected(format!("missing fixture handle {handle}")))?;
        crate::repo::must(accounts.get(&id)?)
    })
}

pub fn team_id(store: &Store, external: &str) -> Result<String> {
    store.sqlite_read(|c| {
        Ok(c.query_row(
            "SELECT party_id FROM team_details WHERE external_id = ?",
            [external],
            |r| r.get(0),
        )?)
    })
}

pub fn members(store: &Store, external: &str) -> Result<Vec<(String, String)>> {
    store.sqlite_read(|c| {
        let mut st = c.prepare(
            "SELECT p.name, m.role FROM team_members m JOIN parties p ON p.id = m.contact_id
             JOIN team_details d ON d.party_id = m.team_id WHERE d.external_id = ? ORDER BY m.role != 'lead', p.name",
        )?;
        let rows = st.query_map([external],|r| Ok((r.get(0)?,r.get(1)?)))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    })
}

pub fn handle_connection(store: &Store, handle: &str) -> Result<String> {
    store.sqlite_read(|c| {
        Ok(c.query_row(
            "SELECT connection_id FROM account_handles WHERE handle = ?",
            [handle],
            |r| r.get(0),
        )?)
    })
}

pub fn job_properties(store: &Store, contact: &str, org: Option<&str>) -> Result<String> {
    store.sqlite_read(|c| Ok(c.query_row(
        "SELECT props FROM links WHERE subject_id = ? AND predicate = 'works_at' AND (?2 IS NULL OR object_id = ?2)",
        params![contact,org],|r|r.get(0),
    )?))
}

pub fn set_job_title(store: &Store, contact: &str, org: &str, title: Option<&str>) -> Result<()> {
    store.write_locked(|c| {
        c.execute("UPDATE links SET props = json_set(props, '$.title', ?) WHERE subject_id = ? AND object_id = ? AND predicate = 'works_at'",
            params![title,contact,org])?;
        Ok(())
    })
}

pub fn set_connection_adapter(store: &Store, id: &str, adapter: &str) -> Result<()> {
    store.write_locked(|c| {
        c.execute(
            "UPDATE connections SET adapter = ?, status = 'ok' WHERE id = ?",
            [adapter, id],
        )?;
        Ok(())
    })
}

pub fn party_count(
    store: &Store,
    kind: &str,
    ws: Option<&str>,
    status: Option<&str>,
) -> Result<i64> {
    store.sqlite_read(|c| Ok(c.query_row(
        "SELECT COUNT(*) FROM parties WHERE type = ? AND (?2 IS NULL OR workspace_id = ?2) AND (?3 IS NULL OR status = ?3)",
        params![kind,ws,status],|r|r.get(0),
    )?))
}

pub fn decision_count(store: &Store) -> Result<i64> {
    store.sqlite_read(|c| Ok(c.query_row("SELECT COUNT(*) FROM decisions", [], |r| r.get(0))?))
}

pub fn audit_count(store: &Store, action: &str) -> Result<i64> {
    store.sqlite_read(|c| {
        Ok(c.query_row(
            "SELECT COUNT(*) FROM audit WHERE action = ?",
            [action],
            |r| r.get(0),
        )?)
    })
}

pub fn audit_detail(store: &Store, action: &str) -> Result<String> {
    store.sqlite_read(|c| {
        Ok(c.query_row(
            "SELECT detail FROM audit WHERE action = ? ORDER BY rowid DESC LIMIT 1",
            [action],
            |r| r.get(0),
        )?)
    })
}

pub fn revision_count(store: &Store, id: &str) -> Result<i64> {
    store.sqlite_read(|c| {
        Ok(c.query_row(
            "SELECT COUNT(*) FROM revisions WHERE object_id = ?",
            [id],
            |r| r.get(0),
        )?)
    })
}

pub fn daily_clocks(store: &Store) -> Result<()> {
    store.write_locked(|c| {
        c.execute_batch("INSERT INTO settings (workspace_id,key,value,updated) VALUES
        ('default','tz','\"America/New_York\"',1), ('w2','tz','\"Europe/London\"',1);
        INSERT INTO notification_policies (workspace_id,user_id,timezone,enabled,mode,consented_at,consent_since_seq,updated)
        VALUES ('default','usr_dev','Asia/Tokyo',1,'now',1,0,1)")?;
        Ok(())
    })
}

pub fn party(store: &Store, handle: &str) -> Result<String> {
    super::scalar(
        store,
        "SELECT a.party_id FROM account_handles h JOIN accounts a ON a.id = h.account_id WHERE h.handle = ?",
        [handle],
    )
}

pub fn team_status(store: &Store, external: &str) -> Result<String> {
    super::scalar(
        store,
        "SELECT p.status FROM parties p JOIN team_details d ON d.party_id = p.id WHERE d.external_id = ?",
        [external],
    )
}

pub fn set_workspace_clocks(store: &Store, zone: &str) -> Result<()> {
    store.write_locked(|c| {
        c.execute(
            "INSERT OR REPLACE INTO settings(workspace_id,key,value,updated) SELECT id,'tz',?1,1000 FROM workspaces",
            [serde_json::to_string(zone)?],
        )?;
        Ok(())
    })
}

pub fn allow_directory(store: &Store, id: &str) -> Result<()> {
    store.write_locked(|c| {
        c.execute(
            r#"UPDATE connections SET caps='["directory.read"]' WHERE id=?"#,
            [id],
        )?;
        Ok(())
    })
}

pub fn bump_connection_revision(store: &Store, id: &str) -> Result<()> {
    store.write_locked(|c| {
        c.execute(
            "UPDATE connections SET revision=revision+1 WHERE id=?",
            [id],
        )?;
        Ok(())
    })
}

pub fn rename_workspace(store: &Store, ws: &str, name: &str) -> Result<()> {
    store.write_locked(|c| {
        c.execute("UPDATE workspaces SET name=? WHERE id=?", [name, ws])?;
        Ok(())
    })
}

pub fn damage_config(store: &Store, id: &str) -> Result<()> {
    store.write_locked(|c| {
        c.execute("UPDATE connections SET config='{' WHERE id=?", [id])?;
        Ok(())
    })
}
