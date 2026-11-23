//! Mutations that exercise attachment access against deleted or reclassified rows.

use steve_node_api::TurnSpec;

use crate::{Result, Store};

pub fn replace_turn_spec(store: &Store, spec: &TurnSpec) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute(
            "UPDATE runtime_attempts SET spec=? WHERE attempt_id=?",
            [&serde_json::to_string(spec)?, spec.run.attempt.as_str()],
        )?;
        Ok(())
    })
}

pub fn file_workspace(store: &Store, file: &str, ws: &str) -> Result<()> {
    store.write(|tx| {
        tx.conn
            .execute("UPDATE files SET workspace_id=? WHERE id=?", [ws, file])?;
        Ok(())
    })
}

pub fn remove_file(store: &Store, file: &str) -> Result<()> {
    store.write(|tx| {
        tx.conn.execute("DELETE FROM files WHERE id=?", [file])?;
        Ok(())
    })
}
