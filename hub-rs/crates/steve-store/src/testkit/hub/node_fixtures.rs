//! Seed data and persisted assertions for Hub tests.

use crate::{Result, Store};

pub fn binding_count(store: &Store, sid: &str) -> Result<i64> {
    store.sqlite_read(|db| {
        Ok(db.query_row(
            "SELECT COUNT(*) FROM runtime_sessions WHERE session_id=?",
            rusqlite::params![sid],
            |r| r.get(0),
        )?)
    })
}

pub fn binding_principal_audience_s1(store: &Store) -> Result<(String, String)> {
    store.sqlite_read(|db| {
        Ok(db.query_row(
            "SELECT principal, audience FROM runtime_sessions WHERE session_id='s1'",
            rusqlite::params![],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?)
    })
}

pub fn clear_queued(store: &Store) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute("DELETE FROM queued", rusqlite::params![])?;
        Ok(())
    })
}

pub fn command_retry_at(store: &Store, key1: &str) -> Result<i64> {
    store.sqlite_read(|db| {
        Ok(db.query_row(
            "SELECT retry_at FROM node_commands WHERE target_attempt=?",
            rusqlite::params![key1],
            |r| r.get(0),
        )?)
    })
}

pub fn fixture_prepare_state(store: &Store) -> Result<(String, bool, i64)> {
    store.sqlite_read(|db| Ok(db.query_row("SELECT state,maybe_sent,lease_generation FROM node_commands WHERE operation_id='prepare:s1:r0'", rusqlite::params![], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?)))?))
}

pub fn cancel_command_targets(store: &Store) -> Result<(String, String)> {
    store.sqlite_read(|db| Ok(db.query_row("SELECT target_attempt, json_extract(request,'$.body.target') FROM node_commands WHERE method='cancel'", rusqlite::params![], |r| Ok((r.get(0)?, r.get(1)?)))?))
}

pub fn create_no_links(store: &Store) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute_batch(
            "CREATE TRIGGER no_links BEFORE UPDATE OF generation ON nodes
                 BEGIN SELECT RAISE(ABORT, 'disk I/O error'); END;",
        )?;
        Ok(())
    })
}

pub fn item_deps_created_i1_dep(store: &Store) -> Result<i64> {
    store.sqlite_read(|db| {
        Ok(db.query_row(
            "SELECT created FROM item_deps WHERE item_id='i1' AND dep_id='dep'",
            rusqlite::params![],
            |r| r.get(0),
        )?)
    })
}

pub fn items_now_i1(store: &Store) -> Result<String> {
    store.sqlite_read(|db| {
        Ok(db.query_row(
            "SELECT now FROM items WHERE id='i1'",
            rusqlite::params![],
            |r| r.get(0),
        )?)
    })
}

pub fn remove_no_links(store: &Store) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute_batch("DROP TRIGGER no_links;")?;
        Ok(())
    })
}

pub fn seed_files(store: &Store, key1: &str) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute("INSERT INTO files(id,workspace_id,sha256,name,mime,size,created_by,created) VALUES ('f1','default',?,'a.txt','text/plain',3,'u',1)", rusqlite::params![key1])?;
        Ok(())
    })
}

pub fn seed_items_and_item_deps(store: &Store) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute("INSERT INTO items(id,workspace_id,no,title,status,stage,created,updated) VALUES('dep','default',2,'prerequisite','run','执行',1,1)", rusqlite::params![])?;
        tx.conn.execute("INSERT INTO item_deps(item_id,dep_id,created) VALUES('i1','dep',123)", rusqlite::params![])?;
        Ok(())
    })
}

pub fn seed_items_and_sessions(store: &Store, row2_key1: &str) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute("INSERT INTO items(id,workspace_id,no,title,status,stage,created,updated) VALUES ('i1','default',1,'test','run','执行',1,1)", rusqlite::params![])?;
        tx.conn.execute("INSERT INTO sessions(id,workspace_id,item_id,harness,cwd,status,created,updated) VALUES (?,'default','i1','claude-code','','active',1,1)", rusqlite::params![row2_key1])?;
        Ok(())
    })
}

pub fn seed_items_and_sessions_for_turn_ended(store: &Store) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute("INSERT INTO items(id,workspace_id,no,title,status,stage,created,updated) VALUES ('i1','default',1,'test','run','执行',1,1)", rusqlite::params![])?;
        tx.conn.execute("INSERT INTO sessions(id,workspace_id,item_id,harness,cwd,status,created,updated) VALUES ('s1','default','i1','claude-code','','active',1,1)", rusqlite::params![])?;
        Ok(())
    })
}

pub fn seed_items_and_set_sessions_item_id_i1(store: &Store) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute("INSERT INTO items(id,workspace_id,no,title,status,stage,created,updated) VALUES ('i1','default',1,'test','run','执行',1,1)", rusqlite::params![])?;
        tx.conn.execute("UPDATE sessions SET item_id='i1' WHERE id='s1'", rusqlite::params![])?;
        tx.conn.execute("INSERT INTO decisions(id,workspace_id,item_id,session_id,kind,subject_type,subject_id,question,status,requested_by,created) VALUES ('d1','default','i1','s1','ask','item','i1','input','open','steve',1)", rusqlite::params![])?;
        Ok(())
    })
}

pub fn seed_nodes(store: &Store) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute("INSERT INTO nodes(node_id,owner,name,pubkey,fingerprint,state,created) VALUES ('n1','u1','Node',zeroblob(32),'fp','active',1)", rusqlite::params![])?;
        Ok(())
    })
}

pub fn seed_nodes_and_sessions(store: &Store) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute("INSERT INTO nodes(node_id,owner,name,pubkey,fingerprint,state,max_sessions,created) VALUES ('n1','u','Node',zeroblob(32),'fp','active',1,1)", rusqlite::params![])?;
        tx.conn.execute("INSERT INTO sessions(id,workspace_id,harness,cwd,status,created,updated) VALUES ('s2','default','claude-code','.','active',1,1)", rusqlite::params![])?;
        Ok(())
    })
}

pub fn seed_projects_and_items(store: &Store, key1: &str) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute("INSERT INTO projects(id,workspace_id,name,path,kind,worktree,created,updated) VALUES('p1','default','later',?,'dir',0,1,1)", rusqlite::params![key1])?;
        tx.conn.execute_batch("INSERT INTO items(id,workspace_id,no,title,status,stage,project_id,created,updated) VALUES ('i1','default',1,'t','run','执行','p1',1,1),('i2','default',2,'t','run','执行',NULL,1,1);
                INSERT INTO sessions(id,workspace_id,item_id,harness,cwd,status,created,updated) VALUES ('s1','default','i1','claude-code','','active',1,1),('s2','default','i2','claude-code','','active',1,1);")?;
        Ok(())
    })
}

pub fn seed_projects_and_set_items_project_id_p1(store: &Store, key1: &str) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute("INSERT INTO projects(id,workspace_id,name,path,kind,worktree,created,updated) VALUES('p1','default','project',?,'git',1,1,1)", rusqlite::params![key1])?;
        tx.conn.execute("UPDATE items SET project_id='p1' WHERE id='i1'", rusqlite::params![])?;
        Ok(())
    })
}

pub fn seed_existing_item_project(
    store: &Store,
    key1: &str,
    row2_key1: &str,
    row3_key1: &str,
) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute("INSERT INTO projects(id,workspace_id,name,path,kind,worktree,created,updated) VALUES('p1','default','project',?,'git',1,1,1)", rusqlite::params![key1])?;
        tx.conn.execute("UPDATE items SET project_id='p1', branch='steve/i1', cwd=?1 WHERE id='i1'", rusqlite::params![row2_key1])?;
        tx.conn.execute("UPDATE sessions SET cwd=? WHERE id='s1'", rusqlite::params![row3_key1])?;
        Ok(())
    })
}

pub fn seed_queued(store: &Store, key1: &str) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute("INSERT INTO queued(id,session_id,text,via,created) VALUES ('legacy',?,'old startup','steve',1)", rusqlite::params![key1])?;
        Ok(())
    })
}

pub fn seed_sessions(store: &Store, session: &str) -> Result<()> {
    store.write(|w| seed_active_session(w, session))
}

pub fn seed_sessions_for_hub_at(store: &Store, key1: &str, key2: &str) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute("INSERT OR IGNORE INTO sessions(id,workspace_id,harness,cwd,status,created,updated) VALUES (?1,'default','claude',?2,'running',1,1)", rusqlite::params![key1, key2])?;
        Ok(())
    })
}

pub fn bind_vault_fixture_workspace(
    store: &Store,
    session: &str,
    workspace: &steve_node_api::WorkspaceRef,
) -> Result<()> {
    store.write(|w| {
        w.conn.execute(
            "INSERT OR IGNORE INTO runtime_sessions(session_id,node_id,workspace_ref,node_session_id,config_revision,phase,updated)
             VALUES (?1,'local',?2,?1,'r','ready',0)",
            rusqlite::params![session, serde_json::to_string(workspace)?],
        )?;
        Ok(())
    })
}

pub fn seed_starting_session(store: &Store, key1: &str) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute("INSERT INTO sessions(id,workspace_id,harness,cwd,status,created,updated) VALUES (?,'default','claude-code','.','starting',1,1)", rusqlite::params![key1])?;
        Ok(())
    })
}

pub fn seed_thread_runs(store: &Store, run: &str, ws: &str) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute("INSERT INTO thread_runs (id, workspace_id, message_id, status, created) VALUES (?, ?, 'msg_1', 'running', 1)", rusqlite::params![run, ws])?;
        Ok(())
    })
}

pub fn seed_users(store: &Store) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute("INSERT INTO users(id,workspace_id,subject,name,role,created) VALUES ('u2','default','u2','u2','member',1)", rusqlite::params![])?;
        Ok(())
    })
}

pub fn seed_workspaces(store: &Store) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute_batch("INSERT INTO workspaces(id,name,created) VALUES ('w2','w2',1);
                 INSERT INTO sessions(id,workspace_id,harness,cwd,status,created,updated) VALUES ('s3','w2','claude-code','.','active',1,1);")?;
        Ok(())
    })
}

pub fn seed_bot_foreign_workspace(store: &Store) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute(
            "INSERT INTO workspaces(id,name,created,item_prefix) VALUES('other','Other',1,'OT')",
            rusqlite::params![],
        )?;
        Ok(())
    })
}

pub fn set_bots_enabled(store: &Store) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute(
            "UPDATE bots SET enabled=0 WHERE id='shared_bot'",
            rusqlite::params![],
        )?;
        Ok(())
    })
}

pub fn set_items(store: &Store) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute(
            "UPDATE items SET source='[\"work\",\"workflow\"]' WHERE id='i1'",
            rusqlite::params![],
        )?;
        Ok(())
    })
}

pub fn set_items_and_clear_workflow_units(store: &Store) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute(
            "UPDATE items SET source='[\"协作工作\",\"workflow\"]' WHERE id='child'",
            rusqlite::params![],
        )?;
        tx.conn.execute(
            "DELETE FROM workflow_units WHERE item_id='child'",
            rusqlite::params![],
        )?;
        Ok(())
    })
}

pub fn assign_management_bot(store: &Store) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute(
            "UPDATE items SET bot_id='management_bot' WHERE id='i1'",
            rusqlite::params![],
        )?;
        tx.conn.execute(
            "UPDATE sessions SET bot_id='management_bot' WHERE id='s1'",
            rusqlite::params![],
        )?;
        Ok(())
    })
}

pub fn seed_prerequisite_and_manager(store: &Store) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute("UPDATE items SET bot_id='manager' WHERE id='i1'", rusqlite::params![])?;
        tx.conn.execute("UPDATE sessions SET bot_id='manager' WHERE id='s1'", rusqlite::params![])?;
        tx.conn.execute("INSERT INTO items(id,workspace_id,no,title,status,stage,created,updated) VALUES('dep','default',2,'prerequisite','run','执行',1,1)", rusqlite::params![])?;
        Ok(())
    })
}

pub fn assign_shared_bot(store: &Store) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute(
            "UPDATE items SET bot_id='shared_bot' WHERE id='i1'",
            rusqlite::params![],
        )?;
        tx.conn.execute(
            "UPDATE sessions SET bot_id='shared_bot' WHERE id='s1'",
            rusqlite::params![],
        )?;
        Ok(())
    })
}

pub fn assign_foreign_session_scope(store: &Store) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute(
            "UPDATE items SET source='[\"\",\"\"]' WHERE id='i1'",
            rusqlite::params![],
        )?;
        Ok(())
    })
}

pub fn set_messages_space_chan(store: &Store, origin: &str) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute(
            "UPDATE messages SET space='chan:shared' WHERE id=?",
            rusqlite::params![origin],
        )?;
        Ok(())
    })
}

pub fn set_node_commands_maybe_sent(store: &Store) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute(
            "UPDATE node_commands SET maybe_sent=1 WHERE operation_id='op_x3'",
            rusqlite::params![],
        )?;
        Ok(())
    })
}

pub fn mark_yielded_exec_possibly_sent(store: &Store) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute(
            "UPDATE node_commands SET maybe_sent=1 WHERE operation_id='op_x4'",
            rusqlite::params![],
        )?;
        Ok(())
    })
}

pub fn set_node_commands_retry_at(store: &Store) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute(
            "UPDATE node_commands SET retry_at=0 WHERE state='pending'",
            rusqlite::params![],
        )?;
        Ok(())
    })
}

pub fn set_node_commands_state_accepted(store: &Store, first: &str) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute(
            "UPDATE node_commands SET state='accepted' WHERE operation_id=?",
            rusqlite::params![first],
        )?;
        Ok(())
    })
}

pub fn set_node_pairings_expires(store: &Store) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute(
            "UPDATE node_pairings SET expires=1 WHERE claimed_at IS NULL",
            rusqlite::params![],
        )?;
        Ok(())
    })
}

pub fn set_nodes_state_revoked(store: &Store) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute(
            "UPDATE nodes SET state='revoked' WHERE node_id='n1'",
            rusqlite::params![],
        )?;
        Ok(())
    })
}

pub fn set_runtime_attempts_cancel_requested(store: &Store) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute(
            "UPDATE runtime_attempts SET cancel_requested=1",
            rusqlite::params![],
        )?;
        Ok(())
    })
}

pub fn set_runtime_attempts_state_finished(store: &Store) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute(
            "UPDATE runtime_attempts SET state='finished'",
            rusqlite::params![],
        )?;
        Ok(())
    })
}

pub fn set_runtime_attempts_state_unknown(store: &Store) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute(
            "UPDATE runtime_attempts SET state='unknown'",
            rusqlite::params![],
        )?;
        Ok(())
    })
}

pub fn set_runtime_sessions_node_id_elsewhere(store: &Store, key1: &str) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute(
            "UPDATE runtime_sessions SET node_id='elsewhere' WHERE session_id=?",
            rusqlite::params![key1],
        )?;
        Ok(())
    })
}

pub fn set_runtime_sessions_node_revision_r0(store: &Store) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute(
            "UPDATE runtime_sessions SET node_revision='r0'",
            rusqlite::params![],
        )?;
        Ok(())
    })
}

pub fn set_sessions_and_set_items_branch_steve(store: &Store, key1: &str) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute(
            "UPDATE sessions SET cwd=? WHERE id='s1'",
            rusqlite::params![key1],
        )?;
        tx.conn.execute(
            "UPDATE items SET branch='steve/i1' WHERE id='i1'",
            rusqlite::params![],
        )?;
        Ok(())
    })
}

pub fn set_sessions_readonly(store: &Store) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute(
            "UPDATE sessions SET readonly=1 WHERE id='s1'",
            rusqlite::params![],
        )?;
        Ok(())
    })
}

pub fn set_sessions_status_ended(store: &Store) -> Result<()> {
    store.write(|tx| {
        tx.conn
            .execute("UPDATE sessions SET status='ended'", rusqlite::params![])?;
        Ok(())
    })
}

pub fn set_sessions_status_ended_and_sessions(
    store: &Store,
    key1: &str,
    row2_key1: &str,
) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute("UPDATE sessions SET status='ended' WHERE id=?", rusqlite::params![key1])?;
        tx.conn.execute("INSERT INTO sessions(id,workspace_id,harness,cwd,status,created,updated) VALUES (?,'default','claude-code','.','starting',1,1)", rusqlite::params![row2_key1])?;
        Ok(())
    })
}

pub fn end_tool_session(store: &Store) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute(
            "UPDATE sessions SET status='ended' WHERE id='s1'",
            rusqlite::params![],
        )?;
        Ok(())
    })
}

pub fn set_sessions_status_idle(store: &Store) -> Result<()> {
    store.write(|tx| {
        tx.conn
            .execute("UPDATE sessions SET status='idle'", rusqlite::params![])?;
        Ok(())
    })
}

pub fn set_sessions_workspace_id_foreign(store: &Store) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute(
            "UPDATE sessions SET workspace_id='foreign'",
            rusqlite::params![],
        )?;
        Ok(())
    })
}

pub fn set_thread_runs_status_done(store: &Store) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute(
            "UPDATE thread_runs SET status='done' WHERE id='tr_t'",
            rusqlite::params![],
        )?;
        Ok(())
    })
}

pub fn set_workflows_paused(store: &Store) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute(
            "UPDATE workflows SET paused=1 WHERE item_id='parent'",
            rusqlite::params![],
        )?;
        Ok(())
    })
}

pub fn resume_workflow(store: &Store) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute(
            "UPDATE workflows SET paused=0 WHERE item_id='parent'",
            rusqlite::params![],
        )?;
        Ok(())
    })
}

pub fn stream_fault_record(store: &Store, stream: &str) -> Result<(String, String, String)> {
    store.sqlite_read(|db| Ok(db.query_row("SELECT fault, session_id, fault_reason FROM node_streams WHERE stream=? AND fault_event IS NOT NULL", rusqlite::params![stream], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?))
}

pub fn view_shadow_v_digest_v_extra_count_s1(store: &Store) -> Result<(String, Option<i64>)> {
    store.sqlite_read(|db| {
        Ok(db.query_row(
            "SELECT v.digest, v.extra_count FROM view_shadow v JOIN runtime_sessions r
                  ON r.session_id=v.session_id AND r.view_digest=v.digest WHERE v.session_id='s1'",
            rusqlite::params![],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?)
    })
}

pub fn ordered_attempt_executions(store: &Store) -> Result<Vec<String>> {
    store.sqlite_read(|db| {
        let mut q = db.prepare("SELECT execution FROM runtime_attempts ORDER BY created, rowid")?;
        Ok(q.query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    })
}

pub fn stream_watermarks(store: &Store) -> Result<Vec<(String, i64)>> {
    store.sqlite_read(|db| {
        let mut q = db.prepare("SELECT stream,applied FROM node_streams ORDER BY stream")?;
        Ok(q.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<_>>()?)
    })
}

pub fn attempt_statistics(store: &Store) -> Result<Vec<(Option<i64>, Option<String>)>> {
    store.sqlite_read(|db| {
        let mut q = db.prepare("SELECT t.ended, t.stop_reason FROM agent_turns t LEFT JOIN runtime_attempts a ON a.attempt_id = t.node_attempt_id ORDER BY a.created, a.rowid")?;
        Ok(q.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<_>>()?)
    })
}

pub fn permission_card_ids(store: &Store) -> Result<Vec<String>> {
    store.sqlite_read(|db| {
        let mut q = db.prepare(
            "SELECT id FROM decisions WHERE subject_type='tool_call' ORDER BY created, rowid",
        )?;
        Ok(q.query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    })
}

pub fn configuration_states(store: &Store) -> Result<Vec<String>> {
    store.sqlite_read(|db| {
        let mut q = db.prepare("SELECT state FROM node_commands WHERE method='configure_session' ORDER BY created, rowid")?;
        Ok(q.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?)
    })
}

pub fn planned_prompt_texts(store: &Store) -> Result<Vec<String>> {
    store.sqlite_read(|db| {
        let mut q = db.prepare("SELECT json_extract(request,'$.body.run.prompt[0].text') FROM node_commands WHERE method='start_turn' ORDER BY created, rowid")?;
        Ok(q.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?)
    })
}

pub fn stream_epochs(store: &Store) -> Result<Vec<String>> {
    store.sqlite_read(|db| {
        let mut q = db.prepare("SELECT DISTINCT epoch FROM node_streams")?;
        Ok(q.query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    })
}

pub fn observed_revisions(store: &Store) -> Result<Vec<String>> {
    store.sqlite_read(|db| {
        let mut q = db.prepare("SELECT json_extract(request,'$.body.expected') FROM node_commands WHERE method='configure_session'
                 UNION SELECT json_extract(receipt,'$.payload.state.result.revision') FROM node_commands WHERE method='open_session'")?;
        Ok(q.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<_>>()?)
    })
}

pub fn active_execution(store: &Store, session: &str) -> Result<Option<String>> {
    store.sqlite_read(|db| Ok(db.query_row("SELECT a.execution FROM runtime_attempts a JOIN runtime_turns t USING(turn_id) WHERE t.session_id=? AND a.state='running'", [session], |r| r.get(0)).ok()))
}

pub fn fault_stream(store: &Store, stream: &str) -> Result<usize> {
    store.write(|tx| {
        Ok(tx.conn.execute(
            "UPDATE node_streams SET fault='failed' WHERE stream=?",
            [stream],
        )?)
    })
}

pub fn forget_stream(store: &Store, stream: &str) -> Result<usize> {
    store.write(|tx| {
        Ok(tx
            .conn
            .execute("UPDATE node_streams SET applied=0 WHERE stream=?", [stream])?)
    })
}

pub fn seed_principal_session(store: &Store, node_owner: &str) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute_batch(&format!("INSERT INTO users(id,workspace_id,subject,name,role,created) VALUES ('u','default','u','u','owner',1);
                     INSERT INTO workspace_members(workspace_id,user_id,role,created) VALUES ('default','u','owner',1);
                     INSERT INTO nodes(node_id,owner,name,pubkey,fingerprint,state,created) VALUES ('n1','{node_owner}','n1',zeroblob(32),'f1','active',1);
                     INSERT INTO items(id,workspace_id,no,title,status,stage,created,updated,requested_by) VALUES ('i1','default',1,'t','run','执行',1,1,'u');
                     UPDATE sessions SET item_id='i1', bot_id='dev' WHERE id='s1';"))?;
        Ok(())
    })
}

#[derive(Clone, Copy, Debug)]
pub enum WorkflowPermissionChange {
    Pause,
    Cancel,
    NewGoal,
    RemoveUnit,
}

pub const WORKFLOW_PERMISSION_CHANGES: [WorkflowPermissionChange; 4] = [
    WorkflowPermissionChange::Pause,
    WorkflowPermissionChange::Cancel,
    WorkflowPermissionChange::NewGoal,
    WorkflowPermissionChange::RemoveUnit,
];

pub fn change_workflow_permission(store: &Store, change: WorkflowPermissionChange) -> Result<()> {
    let sql = match change {
        WorkflowPermissionChange::Pause => "UPDATE workflows SET paused=1",
        WorkflowPermissionChange::Cancel => "UPDATE workflow_units SET status='cancelled'",
        WorkflowPermissionChange::NewGoal => {
            "UPDATE item_goals SET version=2 WHERE item_id='parent'"
        }
        WorkflowPermissionChange::RemoveUnit => "DELETE FROM workflow_units",
    };
    store.write(|tx| {
        tx.conn.execute_batch(sql)?;
        Ok(())
    })
}

pub fn ordered_attempt_states(store: &Store) -> Result<Vec<String>> {
    store.sqlite_read(|db| {
        let mut q = db.prepare("SELECT state FROM runtime_attempts ORDER BY created, rowid")?;
        Ok(q.query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    })
}

pub fn limit_audit_count(store: &Store) -> Result<i64> {
    store.sqlite_read(|db| {
        Ok(db.query_row(
            "SELECT COUNT(*) FROM node_audit WHERE what='limit'",
            [],
            |r| r.get(0),
        )?)
    })
}

pub fn seed_wakeup_sessions(store: &Store, node: &str) -> Result<()> {
    store.write(|tx| {
            for s in ["s_wait", "s_idle"] {
                tx.conn.execute(
                    "INSERT INTO runtime_sessions(session_id,node_id,workspace_ref,node_session_id,config_revision,phase,updated) VALUES (?1,?2,'{}',?1,'r','ready',1)",
                    [s, node],
                )?;
            }
            tx.conn.execute("INSERT INTO queued(id,session_id,text,via,created) VALUES ('q1','s_wait','hi','u',1)", [])?;
            tx.conn.execute("UPDATE nodes SET last_seen=0 WHERE node_id=?", [node])?;
            Ok(())
        })
}

pub fn node_texts(store: &Store, node: &str) -> Result<Vec<String>> {
    store.sqlite_read(|db| {
        let mut q =
            db.prepare("SELECT data FROM events WHERE source_node=? AND kind='text' ORDER BY seq")?;
        Ok(q.query_map([node], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    })
}

pub fn set_link_member(store: &Store, on: bool) -> Result<()> {
    store.write(|tx| {
                if on {
                    tx.conn.execute_batch(
                        "INSERT OR IGNORE INTO users(id,workspace_id,subject,name,role,created) VALUES ('m','default','m','m','member',1);
                         INSERT INTO workspace_members(workspace_id,user_id,role,created) VALUES ('default','m','member',2);",
                    )?;
                } else {
                    tx.conn.execute_batch("DELETE FROM workspace_members WHERE user_id='m'")?;
                }
                Ok(())
            })
}

pub fn file_count(store: &Store) -> Result<i64> {
    store.sqlite_read(|db| Ok(db.query_row("SELECT count(*) FROM files", [], |r| r.get(0))?))
}

pub fn host_node_texts(store: &Store) -> Result<i64> {
    store.sqlite_read(|db| {
        Ok(db.query_row(
            "SELECT COUNT(*) FROM events WHERE format='steve.v1' AND data LIKE '%host wired%'",
            [],
            |r| r.get(0),
        )?)
    })
}

pub fn release_fixture_session(w: &mut crate::W<'_>) -> Result<()> {
    use sea_query::{Expr, ExprTrait, Query};
    w.exec(
        Query::update()
            .table("runtime_sessions")
            .value("phase", "released")
            .and_where(Expr::col("session_id").eq("s1")),
    )?;
    Ok(())
}

pub fn submitted_midturn_message_count(store: &Store, session: &str, text: &str) -> Result<i64> {
    store.sqlite_read(|db| {
        Ok(db.query_row(
            "SELECT count(*) FROM session_dispatch WHERE session_id=? AND state='submitted'
                   AND text=?",
            [session, text],
            |r| r.get(0),
        )?)
    })
}

pub fn midturn_event_count(store: &Store, session: &str) -> Result<i64> {
    store.sqlite_read(|db| {
        Ok(db.query_row(
            "SELECT count(*) FROM events WHERE session_id=? AND kind='user'
                   AND json_extract(data,'$.mid_turn')=1",
            [session],
            |r| r.get(0),
        )?)
    })
}

pub fn block_tools_for_prerequisite(store: &Store, blocked: bool) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute(
            if blocked {
                "UPDATE items SET status='block' WHERE id='i1'"
            } else {
                "UPDATE items SET status='run',source='[\"item\",\"workflow\"]' WHERE id='i1'"
            },
            [],
        )?;
        Ok(())
    })
}

pub fn replace_prerequisite_manager(store: &Store, replace: bool) -> Result<()> {
    store.write(|tx| {
                        tx.conn.execute("INSERT INTO item_deps(item_id,dep_id,created) VALUES('i1','dep',1)", [])?;
                        if replace {
                            tx.conn.execute("INSERT INTO sessions(id,workspace_id,item_id,bot_id,no,harness,cwd,status,created,updated) VALUES('s2','default','i1','manager',2,'test-adapter','','active',2,2)", [])?;
                        }
                        Ok(())
                    })
}

pub fn job_spec_json(store: &Store) -> Result<Vec<String>> {
    store.sqlite_read(|db| {
        let mut q = db.prepare("SELECT spec FROM runtime_job_attempts ORDER BY created, rowid")?;
        Ok(q.query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    })
}

pub fn job_specs(store: &Store) -> Result<Vec<steve_node_api::JobSpec>> {
    job_spec_json(store)?
        .into_iter()
        .map(|row| Ok(serde_json::from_str(&row)?))
        .collect()
}

pub fn research_run_trace(
    store: &Store,
    run: &str,
) -> Result<
    Vec<(
        String,
        String,
        String,
        bool,
        Option<String>,
        Option<String>,
        Option<String>,
    )>,
> {
    store.sqlite_read(|db| {
        let mut q = db.prepare(
            "SELECT a.attempt_id,a.model,a.state,a.cancel_requested,
                         json_extract(a.completion,'$.outcome'),
                         c.method,c.state FROM runtime_job_attempts a
                         JOIN runtime_jobs j ON j.job_id=a.job_id
                         LEFT JOIN node_commands c ON c.target_job_attempt=a.attempt_id
                         WHERE j.thread_run_id=? ORDER BY a.created,a.rowid,c.created,c.rowid",
        )?;
        Ok(q.query_map([run], |r| {
            Ok((
                r.get(0)?,
                r.get(1)?,
                r.get(2)?,
                r.get(3)?,
                r.get(4)?,
                r.get(5)?,
                r.get(6)?,
            ))
        })?
        .collect::<rusqlite::Result<_>>()?)
    })
}

pub fn job_cancel_requested(store: &Store) -> Result<bool> {
    store.sqlite_read(|db| {
        Ok(db.query_row(
            "SELECT cancel_requested FROM runtime_job_attempts",
            [],
            |r| r.get(0),
        )?)
    })
}

pub fn coordinator_dump(store: &Store) -> Result<String> {
    store
            .sqlite_read(|c| {
                let mut out = String::new();
                for sql in [
                    "SELECT operation_id||' '||method||' '||state||' '||COALESCE(receipt,'') FROM node_commands",
                    "SELECT what||' '||COALESCE(safe_detail,'') FROM node_audit",
                    "SELECT session_id||' '||phase FROM runtime_sessions",
                ] {
                    let mut st = c.prepare(sql)?;
                    for r in st.query_map([], |r| r.get::<_, String>(0))? {
                        out.push_str(&r?);
                        out.push('\n');
                    }
                }
                Ok(out)
            })
}

pub fn diagnostic_notice_count(store: &Store, code: &str) -> Result<i64> {
    store.sqlite_read(|db| Ok(db.query_row("SELECT COUNT(*) FROM events WHERE session_id='s1' AND kind='system' AND json_extract(data,'$.code')=?", [code], |r| r.get(0))?))
}

pub fn tools_missing_watermark(store: &Store) -> Result<i64> {
    store.sqlite_read(|db| {
        Ok(db.query_row(
            "SELECT applied FROM node_streams WHERE stream='tools-missing'",
            [],
            |r| r.get(0),
        )?)
    })
}

pub fn is_running_attempt(store: &Store, attempt: &str) -> Result<bool> {
    store.sqlite_read(|db| {
        Ok(db.query_row(
            "SELECT state='running' FROM runtime_attempts WHERE attempt_id=?",
            [attempt],
            |r| r.get(0),
        )?)
    })
}

pub fn adapter_exit_watermark(store: &Store) -> Result<i64> {
    store.sqlite_read(|db| {
        Ok(db.query_row(
            "SELECT COALESCE((SELECT applied FROM node_streams WHERE stream='exit-evidence'),0)",
            [],
            |r| r.get(0),
        )?)
    })
}

pub fn attempt_and_queue_counts(store: &Store) -> Result<(i64, i64)> {
    store.sqlite_read(|db| {
        Ok(db.query_row(
            "SELECT (SELECT COUNT(*) FROM runtime_attempts),
                        (SELECT COUNT(*) FROM queued WHERE session_id='s1')",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?)
    })
}

pub fn retry_execution(store: &Store) -> Result<String> {
    store.sqlite_read(|db| {
        Ok(db.query_row(
            "SELECT execution FROM runtime_attempts WHERE ordinal=1",
            [],
            |r| r.get(0),
        )?)
    })
}

pub fn text_roles(store: &Store) -> Result<Vec<String>> {
    store.sqlite_read(|db| {
        let mut q = db.prepare(
            "SELECT json_extract(data,'$.role') FROM events WHERE kind='text' ORDER BY seq",
        )?;
        Ok(q.query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    })
}

pub fn paused_session_state(store: &Store) -> Result<(i64, i64, String, String)> {
    store.sqlite_read(|c| {
        Ok((
            c.query_row(
                "SELECT COUNT(*) FROM queued WHERE session_id='s1'",
                [],
                |r| r.get(0),
            )?,
            c.query_row("SELECT COUNT(*) FROM runtime_attempts", [], |r| r.get(0))?,
            c.query_row(
                "SELECT node_id FROM runtime_sessions WHERE session_id='s1'",
                [],
                |r| r.get(0),
            )?,
            c.query_row("SELECT status FROM sessions WHERE id='s1'", [], |r| {
                r.get(0)
            })?,
        ))
    })
}

pub fn seed_active_session(w: &mut crate::W<'_>, session: &str) -> Result<()> {
    use sea_query::Query;
    w.exec(
        Query::insert()
            .into_table("sessions")
            .columns([
                "id",
                "workspace_id",
                "harness",
                "cwd",
                "status",
                "created",
                "updated",
            ])
            .values_panic([
                session.into(),
                "default".into(),
                "claude-code".into(),
                ".".into(),
                "active".into(),
                1_i64.into(),
                1_i64.into(),
            ]),
    )?;
    Ok(())
}

pub fn seed_retry_blocker(store: &Store, state: &str) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute("INSERT INTO items(id,workspace_id,no,title,status,stage,created,updated) VALUES ('i1','default',1,'test','run','执行',1,1)", [])?;
        tx.conn.execute("UPDATE sessions SET item_id='i1' WHERE id='s1'", [])?;
        match state {
            "closing" => { tx.conn.execute("UPDATE items SET phase='closing' WHERE id='i1'", [])?; }
            "question" => { tx.conn.execute("INSERT INTO decisions(id,workspace_id,item_id,session_id,kind,subject_type,subject_id,question,status,requested_by,created) VALUES ('d1','default','i1','s1','ask','item','i1','input','open','steve',1)", [])?; }
            "ended" => { tx.conn.execute("UPDATE sessions SET status='ended' WHERE id='s1'", [])?; }
            _ => {}
        }
        Ok(())
    })
}
