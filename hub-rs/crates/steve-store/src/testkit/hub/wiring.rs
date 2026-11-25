//! Storage fixtures for the Hub wiring tests.

use std::path::Path;

use crate::Result;

pub fn seed_wired_connection(
    path: &Path,
    user: &str,
    party: &str,
    adapter: &str,
    system: &str,
    handle: &str,
) -> Result<String> {
    let db = super::api::writable(path)?;
    let (account, connection) = (format!("acc_{adapter}"), format!("con_{adapter}"));
    db.execute("INSERT INTO accounts (id, workspace_id, system_id, family, party_id, kind, handle, handle_display, source, created, updated)
             VALUES (?1, 'default', ?2, ?2, ?3, 'personal', ?4, ?4, 'connect', 1, 1)", [&account, system, party, handle])?;
    db.execute("INSERT INTO connections (id, workspace_id, account_id, adapter, auth, caps, status, config, created_by, created, updated)
             VALUES (?1, 'default', ?2, ?3, 'credential', '[\"events\"]', 'ok', '{\"api_base\":\"http://127.0.0.1:9\"}', ?4, 1, 1)", [&connection, &account, adapter, user])?;
    Ok(connection)
}

pub fn external_event_outcome(path: &Path, external: &str) -> Result<(String, String)> {
    Ok(super::inspect_database(path)?.query_row(
        "SELECT outcome, outcome_label FROM ext_events WHERE external_id=?",
        [external],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?)
}

pub fn channel_reply_trace(path: &Path) -> Result<(Vec<String>, Vec<String>)> {
    let db = super::inspect_database(path)?;
    let dump = |sql| -> Result<Vec<String>> {
        let mut st = db.prepare(sql)?;
        let n = st.column_count();
        Ok(st
            .query_map([], |r| {
                Ok((0..n)
                    .map(|i| {
                        format!(
                            "{:?}",
                            r.get_ref(i).map(|v| v
                                .as_str()
                                .map(String::from)
                                .unwrap_or_else(|_| format!("{v:?}")))
                        )
                    })
                    .collect::<Vec<_>>()
                    .join(" | "))
            })?
            .collect::<rusqlite::Result<_>>()?)
    };
    Ok((
        dump("SELECT direction, status, text FROM chan_messages")?,
        dump("SELECT subject_type, status, question, draft FROM decisions")?,
    ))
}

pub fn mail_triage_flow(path: &Path) -> Option<String> {
    super::inspect_database(path)
        .ok()?
        .query_row(
            "SELECT id FROM flows WHERE origin = 'template:mail-triage'",
            [],
            |r| r.get(0),
        )
        .ok()
}

pub fn secret_use_decision(path: &Path) -> Option<String> {
    super::inspect_database(path)
        .ok()?
        .query_row(
            "SELECT id FROM decisions WHERE subject_type = 'secret_use' AND status = 'open'",
            [],
            |r| r.get(0),
        )
        .ok()
}

pub fn settled_secret_use(path: &Path) -> Option<String> {
    super::inspect_database(path)
        .ok()?
        .query_row(
            "SELECT status FROM secret_uses WHERE status IN ('ok', 'failed', 'error') LIMIT 1",
            [],
            |r| r.get(0),
        )
        .ok()
}

pub fn latest_secret_use_status(path: &Path) -> Option<String> {
    super::inspect_database(path)
        .ok()?
        .query_row(
            "SELECT status FROM secret_uses ORDER BY created DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .ok()
}

pub fn latest_secret_use_exit_code(path: &Path) -> Option<i64> {
    super::inspect_database(path)
        .ok()?
        .query_row(
            "SELECT exit_code FROM secret_uses ORDER BY created DESC LIMIT 1",
            [],
            |r| r.get(0),
        )
        .ok()
}

pub fn sent_channel_message(path: &Path) -> Option<String> {
    super::inspect_database(path)
        .ok()?
        .query_row(
            "SELECT id FROM chan_messages WHERE direction = 'out' AND status = 'sent'",
            [],
            |r| r.get(0),
        )
        .ok()
}

pub fn personal_feishu_connection(path: &Path) -> Option<String> {
    super::inspect_database(path)
        .ok()?
        .query_row(
            "SELECT id FROM connections WHERE adapter = 'feishu_user'",
            [],
            |r| r.get(0),
        )
        .ok()
}

pub fn channel_message_decision(path: &Path) -> Option<String> {
    super::inspect_database(path)
        .ok()?
        .query_row(
            "SELECT id FROM decisions WHERE subject_type = 'chan_message' AND status = 'open'",
            [],
            |r| r.get(0),
        )
        .ok()
}

pub fn connection_space_count(path: &Path, connection: &str) -> Option<i64> {
    super::inspect_database(path)
        .ok()?
        .query_row(
            "SELECT count(*) FROM spaces WHERE connection_id = ?",
            [connection],
            |r| r.get(0),
        )
        .ok()
}

pub fn disconnected_space_count(path: &Path) -> Option<i64> {
    super::inspect_database(path).ok()?.query_row("SELECT count(*) FROM spaces WHERE connection_id IS NULL AND status IN ('left', 'archived')", [], |r| r.get(0)).ok()
}

pub fn issue_item_count(path: &Path) -> Option<i64> {
    super::inspect_database(path)
        .ok()?
        .query_row(
            "SELECT COUNT(*) FROM items WHERE title LIKE '%乱码-I7%'",
            [],
            |r| r.get(0),
        )
        .ok()
}

pub fn default_workspace_member(path: &Path) -> Result<String> {
    Ok(super::inspect_database(path)?.query_row(
        "SELECT user_id FROM workspace_members WHERE workspace_id='default'",
        [],
        |r| r.get(0),
    )?)
}

pub fn seed_connector_session(path: &Path, user: &str, hash: &str) -> Result<()> {
    let db = super::api::writable(path)?;
    db.execute("INSERT INTO items(id, workspace_id, no, title, phase, bot_id, requested_by, created, updated) VALUES ('connector-item','default',9001,'connector fixture','active','steve',?1,1,1)", [user])?;
    db.execute("INSERT INTO audit (workspace_id,ts,actor,action,target) VALUES ('default',1,?1,'item.create','connector-item')", [user])?;
    db.execute("INSERT INTO sessions (id,workspace_id,item_id,bot_id,harness,cwd,status,token_hash,created,updated) VALUES ('connector-session','default','connector-item','steve','claude-code','/nonexistent','idle',?1,1,1)", [hash])?;
    Ok(())
}

pub fn seed_connector_output_source(path: &Path, connection: &str) -> Result<()> {
    let db = super::api::writable(path)?;
    db.execute("INSERT INTO spaces(id,workspace_id,system_id,connection_id,kind,external_id,name,created,updated) SELECT 'output-space','default',a.system_id,c.id,'dm','output-dm','Output',1,1 FROM connections c JOIN accounts a ON a.id=c.account_id WHERE c.id=?1", [connection])?;
    db.execute("INSERT INTO space_identities(space_id,connection_id,workspace_id,joined) VALUES ('output-space',?1,'default',1)", [connection])?;
    db.execute("INSERT INTO space_settings(space_id,workspace_id,disclosure,updated) VALUES ('output-space','default','public',1)", [])?;
    db.execute("INSERT INTO chan_messages(id,workspace_id,space_id,direction,external_id,text,status,created) VALUES ('output-source','default','output-space','in','source-external','Progress?','received',1)", [])?;
    db.execute("INSERT INTO links(workspace_id,subject_type,subject_id,predicate,object_type,object_id,source,confidence,created) VALUES ('default','item','connector-item','from','chan_message','output-source','fixture',1.0,1)", [])?;
    Ok(())
}

pub fn connector_reference_count(path: &Path) -> Result<i64> {
    Ok(super::inspect_database(path)?.query_row(
        "SELECT COUNT(*) FROM connector_refs",
        [],
        |r| r.get(0),
    )?)
}

pub fn channel_message_count(path: &Path) -> Result<i64> {
    Ok(super::inspect_database(path)?
        .query_row("SELECT COUNT(*) FROM chan_messages", [], |r| r.get(0))?)
}

pub fn output_delivery_count(path: &Path) -> Result<i64> {
    Ok(super::inspect_database(path)?.query_row(
        "SELECT COUNT(*) FROM output_deliveries",
        [],
        |r| r.get(0),
    )?)
}

pub fn expire_connector_refs(path: &Path) -> Result<()> {
    super::api::writable(path)?.execute("UPDATE connector_refs SET expires=1", [])?;
    Ok(())
}

pub fn seed_connector_user(path: &Path) -> Result<()> {
    super::api::writable(path)?.execute("INSERT INTO users (id,workspace_id,subject,name,role,created) VALUES ('connector-other','default','connector-other','Other','member',1)", [])?;
    Ok(())
}

pub fn seed_connector_membership(path: &Path) -> Result<()> {
    super::api::writable(path)?.execute("INSERT INTO workspace_members (workspace_id,user_id,role,created) VALUES ('default','connector-other','member',1)", [])?;
    Ok(())
}

pub fn set_connector_requester(path: &Path, user: Option<&str>) -> Result<()> {
    super::api::writable(path)?.execute(
        "UPDATE items SET requested_by=? WHERE id='connector-item'",
        [user],
    )?;
    Ok(())
}

pub fn bind_connector_principal(path: &Path, principal: &str) -> Result<()> {
    super::api::writable(path)?.execute("INSERT INTO runtime_sessions(session_id,node_id,workspace_ref,node_session_id,config_revision,phase,updated,principal) VALUES ('connector-session','node','{}','connector-session','r','ready',1,?1)", [principal])?;
    Ok(())
}

pub fn unbind_connector_principal(path: &Path) -> Result<()> {
    super::api::writable(path)?.execute(
        "DELETE FROM runtime_sessions WHERE session_id='connector-session'",
        [],
    )?;
    Ok(())
}

pub fn set_connector_session_item(path: &Path, item: Option<&str>) -> Result<()> {
    super::api::writable(path)?.execute(
        "UPDATE sessions SET item_id=? WHERE id='connector-session'",
        [item],
    )?;
    Ok(())
}

pub fn remove_connector_membership(path: &Path, user: &str) -> Result<()> {
    super::api::writable(path)?.execute(
        "DELETE FROM workspace_members WHERE workspace_id='default' AND user_id=?1",
        [user],
    )?;
    Ok(())
}

pub fn seed_channel_owner(store: &crate::Store, ws: &str, user: &str) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute("INSERT INTO users (id, workspace_id, subject, name, role, created) VALUES (?, ?, 'dev', 'Dev', 'owner', 1)", [user, ws])?;
        tx.conn.execute("INSERT INTO workspace_members (workspace_id, user_id, role, created) VALUES (?, ?, 'owner', 1)", [ws, user])?;
        Ok(())
    })
}

pub fn set_channel_trigger(
    w: &mut crate::W<'_>,
    connection: &str,
    trigger: &serde_json::Value,
) -> Result<()> {
    use sea_query::{Expr, ExprTrait, Query};
    w.exec(
        Query::update()
            .table("channel_settings")
            .value("trigger", trigger.to_string())
            .and_where(Expr::col("connection_id").eq(connection)),
    )?;
    Ok(())
}

pub fn item_count(store: &crate::Store) -> Result<i64> {
    store.sqlite_read(|db| Ok(db.query_row("SELECT count(*) FROM items", [], |r| r.get(0))?))
}

pub fn tool_route_count(store: &crate::Store) -> Result<i64> {
    store.sqlite_read(|db| {
        Ok(db.query_row(
            "SELECT count(*) FROM thread_reply_receipts WHERE route_outcome = 'needs_tools' AND item_id IS NOT NULL",
            [],
            |r| r.get(0),
        )?)
    })
}

pub fn steve_channel_item_messages(store: &crate::Store) -> Result<i64> {
    store.sqlite_read(|db| Ok(db.query_row("SELECT count(*) FROM messages WHERE space LIKE 'chan:%' AND author = 'steve' AND item_id IS NOT NULL", [], |r| r.get(0))?))
}

pub fn outbound_channel_messages(store: &crate::Store) -> Result<i64> {
    store.sqlite_read(|db| {
        Ok(db.query_row(
            "SELECT count(*) FROM chan_messages WHERE direction='out'",
            [],
            |r| r.get(0),
        )?)
    })
}

pub fn failed_reaction_clears(store: &crate::Store) -> Result<i64> {
    store.sqlite_read(|db| {
        Ok(db.query_row(
            "SELECT count(*) FROM chan_reactions WHERE state='cleared' AND error IS NOT NULL",
            [],
            |r| r.get(0),
        )?)
    })
}

pub fn ready_local_session(path: &std::path::Path, item: &str) -> Result<Option<String>> {
    use rusqlite::OptionalExtension;
    Ok(super::inspect_database(path)?
        .query_row(
            "SELECT s.id FROM sessions s JOIN runtime_sessions r ON r.session_id=s.id
         WHERE s.item_id=? AND r.node_id='local' AND r.phase='ready'",
            [item],
            |row| row.get(0),
        )
        .optional()?)
}
