//! Workspace membership and path-scope fixtures.

use crate::{Result, Store};

fn workspaces(store: &Store) -> Result<()> {
    store.transaction(|tx| {
        tx.execute_batch(
            "INSERT INTO workspaces(id,name,created) VALUES ('solo','solo',1),('team','team',1),('other','other',1);
             INSERT INTO users(id,workspace_id,subject,name,role,created) VALUES
               ('u1','team','u1','u1','owner',1),('u2','team','u2','u2','member',1),('u3','other','u3','u3','owner',1);
             INSERT INTO workspace_members(workspace_id,user_id,role,created) VALUES
               ('solo','u1','owner',1),('team','u1','owner',1),('team','u2','member',2),('other','u3','owner',1);",
        )?;
        Ok(())
    })
}

pub fn principals(store: &Store) -> Result<()> {
    workspaces(store)?;
    store.transaction(|tx| {
        tx.execute_batch(
            "INSERT INTO items(id,workspace_id,requested_by,no,title,status,stage,created,updated) VALUES
               ('i1','solo',NULL,1,'i1','run','define',1,1),('i2','team','u2',2,'i2','run','define',1,1),
               ('i3','team',NULL,3,'i3','run','define',1,1),('i4','team','u9',4,'i4','run','define',1,1),
               ('i6','solo','u1',6,'i6','run','define',1,1);
             INSERT INTO sessions(id,workspace_id,item_id,cwd,harness,status,created,updated) VALUES
               ('s1','solo','i1','.','fixture','ready',1,1),('s2','team','i2','.','fixture','ready',1,1),
               ('s3','team','i3','.','fixture','ready',1,1),('s4','team','i4','.','fixture','ready',1,1),
               ('s5','team',NULL,'.','fixture','ready',1,1),('s6','solo','i6','.','fixture','ready',1,1);
             INSERT INTO runtime_sessions(session_id,node_id,workspace_ref,node_session_id,config_revision,phase,updated)
               SELECT id,'local','{}',id,'r1','ready',1 FROM sessions;",
        )?;
        Ok(())
    })
}

pub fn views(store: &Store) -> Result<()> {
    workspaces(store)?;
    store.transaction(|tx| {
        tx.execute_batch(
            "INSERT INTO projects(id,workspace_id,name,path,worktree,created,updated) VALUES
               ('pg','team','pg','/w/git-proj',1,1,1),('pd','team','pd','/w/dir-proj',0,1,1),
               ('po','other','po','/w/other-proj',1,1,1);
             INSERT INTO items(id,workspace_id,requested_by,cwd,project_id,no,title,status,stage,created,updated) VALUES
               ('i1','team','u2','/h/worktrees/ST-1','pg',1,'i1','run','define',1,1),
               ('i2','team',NULL,'/w/dir-proj','pd',2,'i2','run','define',1,1);
             INSERT INTO sessions(id,workspace_id,item_id,cwd,harness,status,created,updated) VALUES
               ('s1','team','i1','/h/worktrees/ST-1','fixture','ready',1,1),
               ('s2','team','i2','/w/dir-proj/','fixture','ready',1,1);
             INSERT INTO runtime_sessions(session_id,node_id,workspace_ref,node_session_id,config_revision,phase,updated)
               SELECT id,'local','{}',id,'r1','ready',1 FROM sessions;
             INSERT INTO kb_sources(id,workspace_id,kind,name,config,visibility,created_by,created,updated) VALUES
               ('k1','team','folder','shared','{\"path\":\"/docs/shared\"}','workspace','u1',1,1),
               ('k2','team','folder','u1','{\"path\":\"/docs/u1-private\"}','private','u1',1,1),
               ('k3','team','folder','u2','{\"path\":\"/docs/u2-private\"}','private','u2',1,1),
               ('k4','other','folder','elsewhere','{\"path\":\"/docs/elsewhere\"}','workspace','u3',1,1),
               ('k5','team','folder','team','{\"path\":\"/docs/team-t1\"}','team:t1','u3',1,1),
               ('k6','team','folder','invalid','not json','workspace','u1',1,1);",
        )?;
        Ok(())
    })
}

pub fn remove_member(store: &Store, ws: &str, user: &str) -> Result<()> {
    store.transaction(|tx| {
        tx.execute(
            "DELETE FROM workspace_members WHERE workspace_id=? AND user_id=?",
            [ws, user],
        )?;
        Ok(())
    })
}

pub fn set_requester(store: &Store, item: &str, requester: &str) -> Result<()> {
    store.transaction(|tx| {
        tx.execute(
            "UPDATE items SET requested_by=? WHERE id=?",
            [requester, item],
        )?;
        Ok(())
    })
}

pub struct Shadow {
    pub extra_count: Option<i64>,
    pub extra: Option<String>,
    pub missing_count: Option<i64>,
    pub missing: Option<String>,
    pub digest: String,
    pub session_digest: String,
}

pub fn shadow(store: &Store, session: &str) -> Result<Shadow> {
    store.reader(|c| {
        Ok(c.query_row(
            "SELECT v.extra_count,v.extra,v.missing_count,v.missing,v.digest,r.view_digest
             FROM view_shadow v JOIN runtime_sessions r USING(session_id) WHERE v.session_id=?",
            [session],
            |row| {
                Ok(Shadow {
                    extra_count: row.get(0)?,
                    extra: row.get(1)?,
                    missing_count: row.get(2)?,
                    missing: row.get(3)?,
                    digest: row.get(4)?,
                    session_digest: row.get(5)?,
                })
            },
        )?)
    })
}

pub fn bind_session(w: &mut crate::W<'_>, session: &str, node: &str) -> Result<()> {
    use sea_query::Query;
    w.exec(
        Query::insert()
            .into_table("runtime_sessions")
            .columns([
                "session_id",
                "node_id",
                "workspace_ref",
                "node_session_id",
                "config_revision",
                "phase",
                "updated",
            ])
            .values_panic([
                session.into(),
                node.into(),
                "{}".into(),
                session.into(),
                "r".into(),
                "ready".into(),
                1_i64.into(),
            ]),
    )?;
    Ok(())
}

pub fn seed_principal_binding(
    store: &Store,
    session: &str,
    node: &str,
    principal: Option<&str>,
    directory: Option<&str>,
) -> Result<()> {
    store.transaction(|tx| {
        tx.execute("INSERT INTO runtime_sessions
                   (session_id, node_id, workspace_ref, node_session_id, config_revision, phase, updated, principal, state_dir)
                 VALUES (?1, ?2, '{}', ?1, 'r1', 'ready', 1, ?3, ?4)", rusqlite::params![session, node, principal, directory])?;
        Ok(())
    })
}

pub fn set_fixture_acp_ids(store: &Store, local: &str, remote: &str) -> Result<()> {
    store.transaction(|tx| {
        tx.execute(
            "UPDATE sessions SET acp_session_id = 'acp-123' WHERE id IN (?1, ?2)",
            [local, remote],
        )?;
        Ok(())
    })
}

pub fn seed_item_scope_workspace(store: &Store) -> Result<()> {
    store.transaction(|tx| {
        tx.execute(
            "INSERT INTO workspaces (id,name,created,item_prefix) VALUES ('other','Other',1,'OT')",
            [],
        )?;
        Ok(())
    })
}

pub fn move_to_unverified_channel(store: &Store, message: &str) -> Result<()> {
    store.transaction(|tx| {
        tx.execute(
            "UPDATE messages SET space='chan:unverified',meta=? WHERE id=?",
            [
                &serde_json::json!({"chan":{"owner_only":true,"connection":"unverified"}})
                    .to_string(),
                message,
            ],
        )?;
        Ok(())
    })
}

pub fn mark_workflow_item(store: &Store, item: &str) -> Result<()> {
    store.transaction(|tx| {
        tx.execute(
            "UPDATE items SET source=? WHERE id=?",
            [r#"["collaboration","workflow"]"#, item],
        )?;
        Ok(())
    })
}

#[derive(Clone, Copy, Debug)]
pub enum SessionViolation {
    ReadOnly,
    Imported,
    Ended,
    ForeignWorkspace,
    ForeignBot,
    RemoteDevice,
}

pub const SESSION_VIOLATIONS: [SessionViolation; 6] = [
    SessionViolation::ReadOnly,
    SessionViolation::Imported,
    SessionViolation::Ended,
    SessionViolation::ForeignWorkspace,
    SessionViolation::ForeignBot,
    SessionViolation::RemoteDevice,
];

pub fn violate_session_scope(
    store: &Store,
    session: &str,
    violation: SessionViolation,
) -> Result<()> {
    let sql = match violation {
        SessionViolation::ReadOnly => "UPDATE sessions SET readonly=1 WHERE id=?",
        SessionViolation::Imported => "UPDATE sessions SET source='Imported session' WHERE id=?",
        SessionViolation::Ended => "UPDATE sessions SET status='ended' WHERE id=?",
        SessionViolation::ForeignWorkspace => "UPDATE sessions SET workspace_id='other' WHERE id=?",
        SessionViolation::ForeignBot => "UPDATE sessions SET bot_id='bot_other' WHERE id=?",
        SessionViolation::RemoteDevice => {
            "UPDATE sessions SET device_id='external-device' WHERE id=?"
        }
    };
    store.transaction(|tx| {
        tx.execute(sql, [session])?;
        Ok(())
    })
}

pub fn seed_item_dependency(store: &Store, item: &str, upstream: &str) -> Result<()> {
    store.transaction(|tx| {
        tx.execute("UPDATE items SET status='run' WHERE id=?", [item])?;
        tx.execute(
            "INSERT INTO item_deps(item_id,dep_id,created) VALUES(?,?,1)",
            [item, upstream],
        )?;
        Ok(())
    })
}

#[derive(Clone, Copy, Debug)]
pub enum ItemCondition {
    Paused,
    Archived,
    WorkflowUnit,
}

pub const ITEM_CONDITIONS: [ItemCondition; 3] = [
    ItemCondition::Paused,
    ItemCondition::Archived,
    ItemCondition::WorkflowUnit,
];

pub fn set_item_condition(
    store: &Store,
    id: &str,
    condition: ItemCondition,
    set: bool,
) -> Result<()> {
    store.transaction(|tx| {
        match condition {
            ItemCondition::Paused => tx.execute(
                "UPDATE items SET status=? WHERE id=?",
                [if set { "pause" } else { "run" }, id],
            )?,
            ItemCondition::Archived => tx.execute(
                "UPDATE items SET archived=? WHERE id=?",
                rusqlite::params![set, id],
            )?,
            ItemCondition::WorkflowUnit => tx.execute(
                "UPDATE items SET source=? WHERE id=?",
                [
                    if set {
                        r#"["work","workflow"]"#
                    } else {
                        r#"["",""]"#
                    },
                    id,
                ],
            )?,
        };
        Ok(())
    })
}

#[derive(Clone, Copy, Debug)]
pub enum BotCondition {
    Disabled,
    Deleted,
    Unregistered,
}

pub const BOT_CONDITIONS: [BotCondition; 3] = [
    BotCondition::Disabled,
    BotCondition::Deleted,
    BotCondition::Unregistered,
];

pub fn limit_bot_scope(store: &Store, condition: BotCondition) -> Result<()> {
    let sql = match condition {
        BotCondition::Disabled => "UPDATE bots SET enabled=0 WHERE id='bot_default'",
        BotCondition::Deleted => "UPDATE bots SET deleted=1 WHERE id='bot_default'",
        BotCondition::Unregistered => {
            "DELETE FROM bot_workspaces WHERE bot_id='bot_default' AND workspace_id='default'"
        }
    };
    store.transaction(|tx| {
        tx.execute(sql, [])?;
        Ok(())
    })
}

pub fn seed_digest_identity_bot(store: &Store, ws: &str) -> Result<()> {
    store.transaction(|tx| {
        tx.execute("INSERT INTO bots (id,workspace_id,name,harness,updated) VALUES ('steve',?,'Digest','fixture',1)", [ws])?;
        Ok(())
    })
}

pub fn party_records(store: &Store, ws: &str, user: &str) -> Result<i64> {
    store.reader(|db| {
        Ok(db.query_row(
            "SELECT COUNT(*) FROM parties WHERE workspace_id=? AND user_id=?",
            [ws, user],
            |r| r.get(0),
        )?)
    })
}

pub fn seed_thread_group(store: &Store, item: &str) -> Result<()> {
    store.transaction(|tx| {
        tx.execute_batch(
            "INSERT INTO messages (id, workspace_id, space, author, text, created) VALUES
                   ('msg_grp_1', 'default', 'chan:spc_t', 'u', '帮我把 X 改成 Y', 1),
                   ('msg_grp_2', 'default', 'chan:spc_t', 'u', '进展如何', 2)",
        )?;
        tx.execute(
            "                 UPDATE thread_items SET origin_msg = 'msg_grp_1' WHERE item_id = ?;",
            [item],
        )?;
        Ok(())
    })
}
