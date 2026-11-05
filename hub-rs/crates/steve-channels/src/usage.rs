//! What the channel module tells the connections framework about a connection
//! (`ConnectionUsage`): how many spaces it has, and what goes when it is disconnected.
//!
//! `channel_settings` and `chan_cursors` reference the connection and hold no history: they are
//! deleted with it. Spaces and their messages are kept: they become `left` (archived ones stay archived), remember the account (`spaces.account_id`, so a
//! personal account's spaces stay private), lose their settings, and the connection row's delete
//! sets `spaces.connection_id` to NULL (migration 012).
//!
//! Register: `Connections::builder(..).usage(Arc::new(steve_channels::usage::ChannelUsage))`.

use rusqlite::params;
use steve_modules::accounts::connections::{ConnectionUsage, Usage};
use steve_modules::vault::VaultResult;
use steve_store::Tx;

pub struct ChannelUsage;

/// Active spaces the connection sees (`Usage.spaces`).
fn active_spaces(c: &rusqlite::Connection, ws: &str, conn: &str) -> VaultResult<i64> {
    Ok(c.query_row(
        "SELECT count(*) FROM spaces s WHERE s.workspace_id = ?1 AND s.status = 'active'
            AND (EXISTS (SELECT 1 FROM space_identities i WHERE i.space_id = s.id AND i.connection_id = ?2 AND i.left_at IS NULL)
                 OR (s.connection_id = ?2 AND NOT EXISTS (SELECT 1 FROM space_identities i WHERE i.space_id = s.id)))",
        params![ws, conn],
        |r| r.get(0),
    )?)
}

impl ConnectionUsage for ChannelUsage {
    fn usage(&self, c: &rusqlite::Connection, ws: &str, connection_id: &str) -> VaultResult<Usage> {
        Ok(Usage {
            spaces: active_spaces(c, ws, connection_id)?,
            ..Usage::default()
        })
    }

    fn on_disconnect(&self, tx: &Tx, ws: &str, connection_id: &str) -> VaultResult<()> {
        let now = steve_store::now();
        // the spaces this identity sees (or, from before 019, was first seen through)
        let ids = tx
            .prepare(
                "SELECT s.id FROM spaces s WHERE s.workspace_id = ?1
                    AND (s.connection_id = ?2 OR EXISTS (SELECT 1 FROM space_identities i WHERE i.space_id = s.id AND i.connection_id = ?2))
                  ORDER BY s.id",
            )?
            .query_map(params![ws, connection_id], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        tx.execute(
            "UPDATE space_identities SET left_at = ?2 WHERE connection_id = ?1 AND left_at IS NULL",
            params![connection_id, now],
        )?;
        for id in &ids {
            // still seen by another identity: it stays as it is, that identity speaks now
            let others: bool = tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM space_identities WHERE space_id = ? AND left_at IS NULL)",
                [id],
                |r| r.get(0),
            )?;
            if others {
                crate::spaces::event(tx, ws, id);
                continue;
            }
            tx.execute("DELETE FROM space_settings WHERE space_id = ?", [id])?;
            tx.execute("DELETE FROM space_prefs WHERE space_id = ?", [id])?;
            tx.execute(
                "UPDATE spaces SET status = CASE WHEN status = 'archived' THEN status ELSE 'left' END,
                    account_id = coalesce(account_id, (SELECT account_id FROM connections WHERE id = ?2)), updated = ?3
                  WHERE id = ?1",
                params![id, connection_id, now],
            )?;
            crate::spaces::event(tx, ws, id);
        }
        tx.execute(
            "DELETE FROM chan_cursors WHERE connection_id = ?",
            [connection_id],
        )?;
        tx.execute(
            "DELETE FROM channel_settings WHERE connection_id = ?",
            [connection_id],
        )?;
        Ok(())
    }
}
