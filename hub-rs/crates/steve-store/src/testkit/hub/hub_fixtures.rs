//! Seed data and persisted assertions for Hub tests.

use crate::{Result, Store};

pub fn attempt_a_execution_running(store: &Store, sid: &str) -> Result<String> {
    store.reader(|db| Ok(db.query_row("SELECT a.execution FROM runtime_attempts a JOIN runtime_turns t ON t.turn_id=a.turn_id WHERE t.session_id=? AND a.state='running'", rusqlite::params![sid], |r|r.get(0))?))
}

pub fn audit_count_selected(store: &Store) -> Result<i64> {
    store.reader(|db| Ok(db.query_row("SELECT count(*) FROM audit WHERE action IN ('bot.create', 'settings.models', 'mm.create')", rusqlite::params![], |r| r.get(0))?))
}

pub fn binding_principal(store: &Store, sid: &str) -> Result<String> {
    store.reader(|db| {
        Ok(db.query_row(
            "SELECT principal FROM runtime_sessions WHERE session_id=?",
            rusqlite::params![sid],
            |r| r.get(0),
        )?)
    })
}

pub fn command_count_git_apply(store: &Store) -> Result<i64> {
    store.reader(|db| {
        Ok(db.query_row(
            "SELECT COUNT(*) FROM node_commands WHERE method='git_apply'",
            rusqlite::params![],
            |r| r.get(0),
        )?)
    })
}

pub fn event_count_s1_steve_v1_matching_text(store: &Store) -> Result<i64> {
    store.reader(|db| Ok(db.query_row("SELECT COUNT(*) FROM events WHERE session_id='s1' AND format='steve.v1' AND data LIKE '%答案%'", rusqlite::params![], |r| r.get(0))?))
}

pub fn items_id_(store: &Store) -> Result<String> {
    store.reader(|db| {
        Ok(db.query_row(
            "SELECT id FROM items WHERE title = '外部客户端的说明'",
            rusqlite::params![],
            |r| r.get(0),
        )?)
    })
}

pub fn job_count(store: &Store) -> Result<i64> {
    store.reader(|db| {
        Ok(db.query_row(
            "SELECT COUNT(*) FROM runtime_jobs",
            rusqlite::params![],
            |r| r.get(0),
        )?)
    })
}

pub fn messages_count_default_notify(store: &Store, text: &str) -> Result<i64> {
    store.reader(|db| Ok(db.query_row("SELECT count(*) FROM messages WHERE workspace_id = 'default' AND space = 'notify' AND text = ?", rusqlite::params![text], |r| r.get(0))?))
}

pub fn node_count(store: &Store) -> Result<i64> {
    store.reader(|db| {
        Ok(
            db.query_row("SELECT COUNT(*) FROM nodes", rusqlite::params![], |r| {
                r.get(0)
            })?,
        )
    })
}

pub fn queued_count_s1(store: &Store) -> Result<i64> {
    store.reader(|db| {
        Ok(db.query_row(
            "SELECT COUNT(*) FROM queued WHERE session_id='s1'",
            rusqlite::params![],
            |r| r.get(0),
        )?)
    })
}

pub fn remove_fail_commit(store: &Store) -> Result<()> {
    store.transaction(|tx| {
        tx.execute_batch("DROP TRIGGER fail_commit")?;
        Ok(())
    })
}

pub fn remove_fail_projection(store: &Store) -> Result<()> {
    store.transaction(|tx| {
        tx.execute_batch("DROP TRIGGER fail_projection")?;
        Ok(())
    })
}

pub fn seed_bots(store: &Store) -> Result<()> {
    store.transaction(|tx| {
        tx.execute_batch("INSERT OR IGNORE INTO bots (id, workspace_id, name, harness, updated) VALUES ('dev', 'default', '开发', 'claude-code', 1);")?;
        Ok(())
    })
}

pub fn seed_items_and_sessions(store: &Store) -> Result<()> {
    store.transaction(|tx| {
        tx.execute(
            "INSERT INTO items(id,workspace_id,no,title,status,stage,created,updated) \
                         VALUES ('i1','default',1,'test','run','执行',1,1)",
            rusqlite::params![],
        )?;
        tx.execute(
            "INSERT INTO sessions(id,workspace_id,item_id,harness,cwd,status,created,updated) \
                         VALUES ('s1','default','i1','claude-code','','active',1,1)",
            rusqlite::params![],
        )?;
        Ok(())
    })
}

pub fn seed_node_commands(store: &Store) -> Result<()> {
    store.transaction(|tx| {
        tx.execute_batch("INSERT INTO node_commands(operation_id,node_id,method,request,digest,state,created,updated) VALUES ('op_t','n_a','start_turn','{}','d','active',1,1);")?;
        Ok(())
    })
}

pub fn seed_node_vitals(store: &Store) -> Result<()> {
    store.transaction(|tx| {
        tx.execute(
            "INSERT INTO node_vitals(node_id,at,data) VALUES('expired',0,'{}')",
            rusqlite::params![],
        )?;
        Ok(())
    })
}

pub fn seed_nodes(store: &Store) -> Result<()> {
    store.transaction(|tx| {
        tx.execute_batch("INSERT INTO nodes(node_id,owner,name,pubkey,fingerprint,state,created) VALUES ('n_a','u','a',zeroblob(32),'fa','active',1);")?;
        Ok(())
    })
}

pub fn seed_projects_and_items(
    store: &Store,
    w: &str,
    row2_key1: &str,
    row2_w: &str,
    row2_key3: &str,
) -> Result<()> {
    store.transaction(|tx| {
        tx.execute("INSERT INTO projects(id,workspace_id,name,path,kind,device_id,created,updated) VALUES ('p1','default','p1',?1,'git','hub',1,1)", rusqlite::params![w])?;
        tx.execute("INSERT INTO items (id, workspace_id, no, title, status, stage, cwd, branch, project_id, created, updated)
                         VALUES (?1, 'default', 1, 't', 'wait', 'x', ?2, ?3, 'p1', 1, 1)", rusqlite::params![row2_key1, row2_w, row2_key3])?;
        Ok(())
    })
}

pub fn seed_sessions(store: &Store, id: &str) -> Result<()> {
    store.transaction(|tx| {
        tx.execute("INSERT INTO sessions(id,workspace_id,harness,cwd,status,created,updated) VALUES (?,'default','claude-code','.','active',1,1)", rusqlite::params![id])?;
        Ok(())
    })
}

pub fn seed_users_and_workspace_members(store: &Store, user: &str, row2_user: &str) -> Result<()> {
    store.transaction(|tx| {
        tx.execute("INSERT OR IGNORE INTO users(id,workspace_id,subject,name,role,created) VALUES (?1,'default',?1,?1,'owner',1)", rusqlite::params![user])?;
        tx.execute("INSERT OR IGNORE INTO workspace_members(workspace_id,user_id,role,created) VALUES ('default',?1,'owner',1)", rusqlite::params![row2_user])?;
        Ok(())
    })
}

pub fn seed_workspaces_and_items(store: &Store) -> Result<()> {
    store.transaction(|tx| {
        tx.execute("INSERT INTO workspaces(id,name,created) VALUES('elsewhere','Other workspace',1)", rusqlite::params![])?;
        tx.execute("INSERT INTO items(id,workspace_id,no,title,status,stage,created,updated) VALUES('foreign-item','elsewhere',1,'Private item','done','结束',1,1)", rusqlite::params![])?;
        Ok(())
    })
}

pub fn set_devices_max_sessions_32(store: &Store) -> Result<()> {
    store.transaction(|tx| {
        tx.execute(
            "UPDATE devices SET max_sessions=32 WHERE id='hub'",
            rusqlite::params![],
        )?;
        Ok(())
    })
}

pub fn set_diagnostic_events_at(store: &Store) -> Result<()> {
    store.transaction(|tx| {
        tx.execute(
            "UPDATE diagnostic_events SET at=1 WHERE item_id='i1'",
            rusqlite::params![],
        )?;
        Ok(())
    })
}

pub fn set_items_now_effect(store: &Store) -> Result<()> {
    store.transaction(|tx| {
        tx.execute(
            "UPDATE items SET now='effect committed' WHERE id='i1'",
            rusqlite::params![],
        )?;
        Ok(())
    })
}

pub fn set_items_phase_closed(store: &Store, id: &str) -> Result<()> {
    store.transaction(|tx| {
        tx.execute("UPDATE items SET phase='closed',status='fail',outcome='failed',closed_reason='old review failed' WHERE id=?", rusqlite::params![id])?;
        Ok(())
    })
}

pub fn set_mems_visibility_private(store: &Store, theirs: &str) -> Result<()> {
    store.transaction(|tx| {
        tx.execute(
            "UPDATE mems SET visibility = 'private', created_by = 'usr_another' WHERE id = ?",
            rusqlite::params![theirs],
        )?;
        Ok(())
    })
}

pub fn set_node_commands_state_finished(store: &Store, op: &str) -> Result<()> {
    store.transaction(|tx| {
        tx.execute(
            "UPDATE node_commands SET state='finished' WHERE operation_id=?",
            rusqlite::params![op],
        )?;
        Ok(())
    })
}

pub fn set_sessions_bot_id_dev(store: &Store) -> Result<()> {
    store.transaction(|tx| {
        tx.execute(
            "UPDATE sessions SET bot_id = 'dev' WHERE id = 's1'",
            rusqlite::params![],
        )?;
        Ok(())
    })
}

pub fn set_users_role_member(store: &Store, id: &str) -> Result<()> {
    store.transaction(|tx| {
        tx.execute(
            "UPDATE users SET role='member' WHERE id=?",
            rusqlite::params![id],
        )?;
        Ok(())
    })
}

pub fn demote_workspace_owners(store: &Store) -> Result<()> {
    store.transaction(|tx| {
        tx.execute(
            "UPDATE users SET role='member' WHERE role='owner'",
            rusqlite::params![],
        )?;
        Ok(())
    })
}

pub fn set_users_role_owner(store: &Store) -> Result<()> {
    store.transaction(|tx| {
        tx.execute(
            "UPDATE users SET role='owner' WHERE role='member'",
            rusqlite::params![],
        )?;
        Ok(())
    })
}

pub fn setting_value_harness_updates_remote(store: &Store) -> Result<String> {
    store.reader(|db| {
        Ok(db.query_row(
            "SELECT value FROM settings WHERE key='harness.updates.remote'",
            rusqlite::params![],
            |r| r.get(0),
        )?)
    })
}
