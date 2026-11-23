//! Database setup and fault injection for Hub HTTP tests.

use std::path::Path;

use crate::Result;

pub(super) fn writable(path: &Path) -> Result<rusqlite::Connection> {
    let c = rusqlite::Connection::open(path)?;
    c.busy_timeout(std::time::Duration::from_secs(5))?;
    Ok(c)
}

pub fn workflow_item_scopes(path: &Path) -> Result<()> {
    writable(path)?.execute_batch(
        "INSERT INTO workspaces(id,name,created) VALUES('other','Other',1);
         INSERT INTO items(id,workspace_id,no,title,status,stage,created,updated)
         VALUES('local-item','default',990,'Local','run','执行',1,1),
               ('other-item','other',991,'Other','run','执行',1,1);",
    )?;
    Ok(())
}

pub fn remove_work_policy(path: &Path) -> Result<()> {
    writable(path)?.execute_batch("DROP TABLE work_policy")?;
    Ok(())
}

pub fn fail_client_mint(path: &Path) -> Result<()> {
    writable(path)?.execute_batch(
        "CREATE TRIGGER test_fail_client_mint
             BEFORE INSERT ON clients
             BEGIN
                 SELECT RAISE(ABORT, 'simulated mint failure');
             END;",
    )?;
    Ok(())
}

pub fn allow_client_mint(path: &Path) -> Result<()> {
    writable(path)?.execute_batch("DROP TRIGGER test_fail_client_mint;")?;
    Ok(())
}

pub fn demote_users(path: &Path) -> Result<()> {
    writable(path)?.execute_batch("UPDATE users SET role = 'member'")?;
    Ok(())
}

pub fn seed_thread_history(path: &Path, ws: &str) -> Result<()> {
    let c = writable(path)?;
    for i in 0..260i64 {
        let (text, item, meta) = match i {
            5 => ("讨论对话页的历史定位", None, None),
            6 => (
                "这条被收进事项卡片了",
                None,
                Some(serde_json::json!({ "hidden": true }).to_string()),
            ),
            7 => ("交给 ST-9 去做", Some("ST-9"), None),
            8 => ("百分号 100% 完成", None, None),
            _ => ("普通消息", None, None),
        };
        c.execute(
                "INSERT INTO messages (id, workspace_id, space, author, text, item_id, meta, created) VALUES (?, ?, 'steve', ?, ?, ?, ?, ?)",
                rusqlite::params![format!("m{i:03}"), ws, if i % 2 == 0 { "me" } else { "steve" }, text, item, meta, 1_000 + i],
            )
            ?;
    }
    Ok(())
}

pub fn seed_midturn_approval(path: &Path, session: &str) -> Result<()> {
    writable(path)?.execute("INSERT INTO decisions (id, workspace_id, kind, subject_type, subject_id, item_id, session_id, question, status, requested_by, created)
             SELECT 'dec_midturn', workspace_id, 'approve', 'action', item_id, item_id, id, '可以推送吗？', 'open', 'steve', 1 FROM sessions WHERE id = ?1", [session])?;
    Ok(())
}

pub fn seed_annotations(path: &Path, item: &str) -> Result<()> {
    let c = writable(path)?;
    c.execute("INSERT INTO artifacts (id, item_id, kind, title, location, created) VALUES
               ('art_a', ?1, '文件', '截图', '/tmp/shots', 1), ('art_b', ?1, '文件', '截图', '/tmp/shots', 2)", [item])?;
    c.execute(
        "             INSERT INTO artifact_roles (artifact_id, role) VALUES ('art_a', 'record')",
        [],
    )?;
    Ok(())
}

/// Holds the writer lock while an HTTP request reads the old committed owner.
pub struct PendingOwnerRevocation(rusqlite::Connection);

impl PendingOwnerRevocation {
    pub fn begin(path: &Path, owner: &str) -> Result<Self> {
        let c = writable(path)?;
        c.execute_batch("BEGIN IMMEDIATE")?;
        c.execute("UPDATE users SET role='member' WHERE id=?", [owner])?;
        Ok(Self(c))
    }

    pub fn commit(self) -> Result<()> {
        self.0.execute_batch("COMMIT")?;
        Ok(())
    }
}

pub fn restore_owner(path: &Path, owner: &str) -> Result<()> {
    writable(path)?.execute("UPDATE users SET role='owner' WHERE id=?", [owner])?;
    Ok(())
}

pub fn seed_output_item(path: &Path, project: &str) -> Result<()> {
    writable(path)?.execute(
        "INSERT INTO items(id,workspace_id,no,title,status,stage,branch,project_id,created,updated)
         VALUES('i1','default',1,'x','doing','work','steve/st-1',?,1,1)",
        [project],
    )?;
    Ok(())
}

pub fn seed_output_artifact(
    path: &Path,
    id: &str,
    kind: &str,
    title: &str,
    location: &str,
) -> Result<()> {
    writable(path)?.execute("INSERT INTO artifacts(id,item_id,kind,title,location,created) VALUES(?,?,?,?,NULLIF(?,''),1)", [id, "i1", kind, title, location])?;
    Ok(())
}

pub fn seed_durable_report(path: &Path, report: &str) -> Result<()> {
    let c = writable(path)?;
    c.execute(
        "INSERT INTO items(id,workspace_id,no,title,status,stage,created,updated)
        VALUES('i1','default',1,'报告','done','done',1,1)",
        [],
    )?;
    c.execute(
        "INSERT INTO artifacts(id,item_id,kind,title,location,created)
        VALUES('report','i1','报告','交付正文',?,1)",
        [report],
    )?;
    Ok(())
}

pub fn put_project_on_offline_node(path: &Path) -> Result<()> {
    writable(path)?.execute_batch("UPDATE projects SET device_id='n_gone', path='work/repo'")?;
    Ok(())
}

pub fn seed_attribute_decision(path: &Path) -> Result<()> {
    writable(path)?.execute_batch("INSERT INTO attr_defs (id, workspace_id, applies, key, label, type, options, default_level, source, status, created)
           VALUES ('ad_undo', 'default', 'contact', 'x_undo', '撤销测试', 'text', '[]', 'fact', 'agent', 'proposed', 1);
         INSERT INTO decisions (id, workspace_id, kind, subject_type, subject_id, question, options, status, requested_by, created)
           VALUES ('dec_undo', 'default', 'confirm', 'attr_def', 'ad_undo', '加这个属性吗？', '[\"yes\",\"no\"]', 'open', 'steve', 1);")?;
    Ok(())
}

pub fn fixture_attribute_status(path: &Path) -> Result<String> {
    Ok(super::inspect_database(path)?.query_row(
        "SELECT status FROM attr_defs WHERE id = 'ad_undo'",
        [],
        |r| r.get(0),
    )?)
}

pub fn seed_mcp_fixture_session(path: &Path, item: &str, hash: &str) -> Result<()> {
    writable(path)?.execute(
        "INSERT INTO sessions(id,workspace_id,item_id,bot_id,harness,cwd,status,token_hash,created,updated)
         VALUES ('s_tools','default',?1,'steve','claude-code','','idle',?2,1,1)",
        [item, hash],
    )?;
    Ok(())
}

pub fn seed_conversation_preferences(path: &Path) -> Result<()> {
    writable(path)?.execute_batch("INSERT INTO spaces (id, workspace_id, system_id, kind, external_id, name, created, updated) VALUES ('spc_pref', 'default', 'feishu', 'group', 'pref', 'Conversation preference test', 1, 1);
        INSERT INTO messages (id, workspace_id, space, author, text, created) VALUES ('msg_pref', 'default', 'chan:spc_pref', 'me', 'keep this history', 1);")?;
    Ok(())
}

pub fn extension_systems(
    store: &crate::Store,
    id: &str,
) -> Result<Vec<(String, String, String, String)>> {
    store.sqlite_read(|db| {
        let mut s =
            db.prepare("SELECT workspace_id, name, family, plugin FROM systems WHERE id = ?")?;
        Ok(
            s.query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
                .collect::<rusqlite::Result<_>>()?,
        )
    })
}

pub fn seed_attention_reply(
    store: &crate::Store,
    ws: &str,
    user: &str,
    suffix: &str,
    status: &str,
    space: &str,
) -> Result<String> {
    let msg = format!("question_{suffix}");
    let reply = format!("reply_{suffix}");
    let run = format!("run_{suffix}");
    store.write(|tx| -> Result<()> {
            tx.conn.execute("INSERT INTO messages(id,workspace_id,space,author,text,created) VALUES(?,?,?,'me','question',10)",rusqlite::params![msg,ws,space])?;
            tx.conn.execute("INSERT INTO thread_runs(id,workspace_id,message_id,user_id,status,created) VALUES(?,?,?,?,'running',10)",rusqlite::params![run,ws,msg,user])?;
            tx.conn.execute("INSERT INTO messages(id,workspace_id,space,author,text,meta,created) VALUES(?,?,?,'steve','final',?,20)",rusqlite::params![reply,ws,space,serde_json::json!({"run":run,"origin":msg,"run_receipt":true}).to_string()])?;
            tx.conn.execute("UPDATE thread_runs SET status=?,reply_id=?,ended=20 WHERE id=?",rusqlite::params![status,reply,run])?;
            Ok(())
        })?;
    Ok(reply)
}

pub fn seed_feed_report(store: &crate::Store, ws: &str, user: &str) -> Result<()> {
    store.write(|tx|->Result<()> {
            tx.scope(|w| w.publish_report(&crate::feed::RunReport { ws: ws, flow: "flow", run: "run", node: "n", content: &crate::feed::Content{title:"Report".into(),markdown:"Markdown".into(),..Default::default()}, result: &serde_json::json!({"text":"Markdown","digest":0}), now: 10 }))?;
            tx.conn.execute("INSERT INTO personal_notifications(id,workspace_id,user_id,event_id,delivery_key,mode,body,state,created) VALUES('private',?1,?2,'event','key','log','Private title\nPrivate body','held',10)",rusqlite::params![ws, user])?;
            Ok(())
        })
}

pub fn seed_channel_record(path: &Path, at: i64) -> Result<()> {
    writable(path)?.execute(
        "INSERT INTO chan_messages (id, workspace_id, space_id, direction, text, status, created)
         VALUES ('cm_pref', 'default', 'spc_pref', 'in', 'hello from the chat', 'received', ?)",
        [at],
    )?;
    Ok(())
}
