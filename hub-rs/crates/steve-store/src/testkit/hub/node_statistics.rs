//! Fixtures for boot-time interruption of model statistics.

use rusqlite::Connection;

use crate::{Result, Store};

pub fn mark_node_attempt(store: &Store, turn: &str, attempt: &str) -> Result<()> {
    let db = Connection::open(store.path())?;
    db.pragma_update(None, "foreign_keys", false)?;
    db.execute(
        "UPDATE agent_turns SET node_attempt_id = ? WHERE id = ?",
        [attempt, turn],
    )?;
    Ok(())
}

pub fn turn_end(store: &Store, turn: &str) -> Result<(Option<i64>, Option<String>)> {
    store.reader(|c| {
        Ok(c.query_row(
            "SELECT ended, stop_reason FROM agent_turns WHERE id = ?",
            [turn],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?)
    })
}

pub fn usage_count(store: &Store) -> Result<i64> {
    store.reader(|c| Ok(c.query_row("SELECT count(*) FROM usage_records", [], |r| r.get(0))?))
}
