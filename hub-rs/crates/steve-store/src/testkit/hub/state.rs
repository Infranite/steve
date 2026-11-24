//! Records used to test the Hub's state and transcript projections.

use crate::{Result, Store};

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

pub fn set_item_cycle(store: &Store, item: &str, cycle: i64) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute(
            "UPDATE items SET cycle=? WHERE id=?",
            rusqlite::params![cycle, item],
        )?;
        Ok(())
    })
}
