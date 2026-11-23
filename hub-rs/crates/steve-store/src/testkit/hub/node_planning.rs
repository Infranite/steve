//! Node planning fixtures that can reserve a slot before a command has been created.

use crate::{Result, Store, runtime};

pub fn session(store: &Store, id: &str, phase: &str, attempt: Option<&str>) -> Result<()> {
    store.transaction(|tx| {
        let status = if attempt.is_some() { "running" } else { "idle" };
        tx.execute(
            "INSERT INTO sessions(id,workspace_id,harness,cwd,status,created,updated)
             VALUES (?,'default','fixture','.',?,1,1)",
            [id, status],
        )?;
        tx.execute(
            "INSERT INTO runtime_sessions(session_id,node_id,workspace_ref,node_session_id,config_revision,phase,active_attempt,updated)
             VALUES (?1,'local','{}',?1,'r1',?2,?3,1)",
            rusqlite::params![id, phase, attempt],
        )?;
        Ok(())
    })
}

pub fn clear_sessions(store: &Store) -> Result<()> {
    store.transaction(|tx| {
        tx.execute_batch("DELETE FROM runtime_sessions; DELETE FROM sessions;")?;
        Ok(())
    })
}

pub fn queue_and_attempt_count(store: &Store, session: &str) -> Result<(i64, i64)> {
    store.reader(|c| {
        Ok(c.query_row(
            "SELECT (SELECT COUNT(*) FROM queued WHERE session_id=?),
                    (SELECT COUNT(*) FROM runtime_attempts)",
            [session],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?)
    })
}

pub fn reservations(store: &Store, node: &str) -> Result<i64> {
    store.reader(|c| {
        Ok(c.query_row(
            "SELECT COUNT(*) FROM runtime_sessions WHERE node_id=? AND active_attempt IS NOT NULL",
            [node],
            |r| r.get(0),
        )?)
    })
}

pub fn session_room(store: &Store, node: &str) -> Result<runtime::SessionRoom> {
    store.reader(|c| runtime::session_room(c, node))
}
