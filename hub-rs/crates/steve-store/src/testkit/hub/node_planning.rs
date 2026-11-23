//! Node planning fixtures that can reserve a slot before a command has been created.

use crate::{Result, Store, runtime};

pub fn session(store: &Store, id: &str, phase: &str, attempt: Option<&str>) -> Result<()> {
    store.write(|tx| {
        let status = if attempt.is_some() { "running" } else { "idle" };
        tx.conn.execute(
            "INSERT INTO sessions(id,workspace_id,harness,cwd,status,created,updated)
             VALUES (?,'default','fixture','.',?,1,1)",
            [id, status],
        )?;
        tx.conn.execute(
            "INSERT INTO runtime_sessions(session_id,node_id,workspace_ref,node_session_id,config_revision,phase,active_attempt,updated)
             VALUES (?1,'local','{}',?1,'r1',?2,?3,1)",
            rusqlite::params![id, phase, attempt],
        )?;
        Ok(())
    })
}

pub fn clear_sessions(store: &Store) -> Result<()> {
    store.write(|tx| {
        tx.conn
            .execute_batch("DELETE FROM runtime_sessions; DELETE FROM sessions;")?;
        Ok(())
    })
}

pub fn queue_and_attempt_count(store: &Store, session: &str) -> Result<(i64, i64)> {
    store.sqlite_read(|c| {
        Ok(c.query_row(
            "SELECT (SELECT COUNT(*) FROM queued WHERE session_id=?),
                    (SELECT COUNT(*) FROM runtime_attempts)",
            [session],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?)
    })
}

pub fn reservations(store: &Store, node: &str) -> Result<i64> {
    store.sqlite_read(|c| {
        Ok(c.query_row(
            "SELECT COUNT(*) FROM runtime_sessions WHERE node_id=? AND active_attempt IS NOT NULL",
            [node],
            |r| r.get(0),
        )?)
    })
}

pub fn session_room(store: &Store, node: &str) -> Result<runtime::SessionRoom> {
    store.sqlite_read(|c| runtime::session_room_on(c, node))
}

pub fn clone_prepare(store: &Store, operation: &str, new_operation: &str) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute(
            "INSERT INTO node_commands(operation_id,node_id,method,request,digest,state,receipt,created,updated)
             SELECT ?2,node_id,method,json_set(request,'$.id',?2),digest,state,receipt,created,updated
             FROM node_commands WHERE operation_id=?1",
            [operation, new_operation],
        )?;
        Ok(())
    })
}

pub fn make_commands_due(store: &Store) -> Result<()> {
    store.write(|tx| {
        tx.conn
            .execute("UPDATE node_commands SET retry_at=0,lease_until=NULL", [])?;
        Ok(())
    })
}

pub fn retire_control(w: &mut crate::W<'_>, target: &str) -> Result<()> {
    use crate::contract::Q;
    use sea_query::Values;
    let conn = w.db();
    conn.exec(
        "INSERT INTO node_commands(operation_id,node_id,method,request,digest,state,created,updated)
         VALUES (?1,'n1','recover',json_object('body',json_object('target',?2)),'test','finished',1,1)",
        &Values(vec![format!("recover_{target}").into(), target.into()]),
    )?;
    conn.batch("UPDATE runtime_sessions SET phase='opening'")?;
    if target == "op_prep_s1" {
        conn.batch("UPDATE node_commands SET state='pending' WHERE operation_id='op_open_s1'")?;
    }
    Ok(())
}

pub fn seed_rejected_prepare(store: &Store, operation: &str, request: &str) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute("INSERT INTO node_commands(operation_id,node_id,method,request,digest,state,created,updated) VALUES(?,'local','prepare_workspace',?,'d','rejected',1,1)", [operation, request])?;
        Ok(())
    })
}

pub fn expire_command_window(store: &Store, operation: &str, before: i64) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute("UPDATE node_commands SET request=json_set(request,'$.issued_at',?1-1,'$.accept_before',?1) WHERE operation_id=?2", rusqlite::params![before, operation])?;
        Ok(())
    })
}
