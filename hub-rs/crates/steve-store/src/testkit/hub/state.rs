//! Records used to test the Hub's state and transcript projections.

use crate::{Result, Store, WS};

pub fn unsupported_events(store: &Store, session: &str, formats: &[&str]) -> Result<()> {
    store.write(|tx| {
        for format in formats {
            tx.conn.execute(
                "INSERT INTO events(session_id, ts, kind, data, format) VALUES (?, 1, 'agent_message_chunk', ?, ?)",
                [session, r#"{"sessionUpdate":"agent_message_chunk","content":{"text":"not read"}}"#, format],
            )?;
        }
        Ok(())
    })
}

pub fn failed_workflow(store: &Store) -> Result<()> {
    store.write(|tx| {
        for (id, no) in [("ST-1", 1), ("ST-2", 2)] {
            tx.conn.execute(
                "INSERT INTO items (id, workspace_id, no, kind, title, status, stage, created, updated)
                    VALUES (?1, ?2, ?3, 'task', 'test', 'run', '定义', 1, 1)",
                rusqlite::params![id, WS, no],
            )?;
        }
        tx.conn.execute_batch(
            "INSERT INTO workflows (item_id, revision, cycle, paused, max_parallel, max_attempts, created, updated)
                 VALUES ('ST-1', 1, 1, 1, 1, 3, 1, 1);
             INSERT INTO workflow_units (workflow_id, id, position, spec, status, item_id)
                 VALUES ('ST-1', 'review', 0, '{}', 'failed', 'ST-2');",
        )?;
        Ok(())
    })
}

pub fn set_item_cycle(store: &Store, item: &str, cycle: i64) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute(
            "UPDATE items SET cycle=? WHERE id=?",
            rusqlite::params![cycle, item],
        )?;
        Ok(())
    })
}
