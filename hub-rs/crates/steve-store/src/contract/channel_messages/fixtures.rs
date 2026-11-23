//! Persisted outcomes used by channel polling tests.

use rusqlite::{Row, params};
use serde_json::Value;

use crate::{Result, Store, StoreError};

#[derive(Debug)]
pub struct InboundMessage {
    pub external_id: Option<String>,
    pub chat_id: String,
    pub kind: String,
    pub text: Option<String>,
}

pub fn inbound_for_connection(store: &Store, connection: &str) -> Result<Vec<InboundMessage>> {
    store.reader(|c| {
        let mut statement = c.prepare(
            "SELECT m.external_id, s.external_id, s.kind, m.text
             FROM chan_messages m JOIN spaces s ON s.id = m.space_id
             WHERE s.connection_id = ? AND m.direction = 'in'
             ORDER BY m.created, m.external_id",
        )?;
        Ok(statement
            .query_map([connection], |row| {
                Ok(InboundMessage {
                    external_id: row.get(0)?,
                    chat_id: row.get(1)?,
                    kind: row.get(2)?,
                    text: row.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?)
    })
}

#[derive(Debug)]
pub struct Approval {
    pub status: String,
    pub on_behalf: String,
    pub kind: String,
    pub question: String,
}

pub fn approvals_for_connection(store: &Store, connection: &str) -> Result<Vec<Approval>> {
    store.reader(|c| {
        let mut statement = c.prepare(
            "SELECT m.status, m.on_behalf, d.kind, d.question
             FROM chan_messages m JOIN spaces s ON s.id = m.space_id
             JOIN decisions d ON d.id = m.decision_id
             WHERE s.connection_id = ? AND m.direction = 'out'",
        )?;
        Ok(statement
            .query_map([connection], |row| {
                Ok(Approval {
                    status: row.get(0)?,
                    on_behalf: row.get(1)?,
                    kind: row.get(2)?,
                    question: row.get(3)?,
                })
            })?
            .collect::<rusqlite::Result<_>>()?)
    })
}

#[derive(Debug)]
pub struct Signal {
    pub kind: String,
    pub payload: Option<Value>,
    pub outcome_label: String,
}

pub fn signals(store: &Store, external_id: &str) -> Result<Vec<Signal>> {
    store.reader(|c| {
        let mut statement =
            c.prepare("SELECT type, payload, outcome_label FROM ext_events WHERE external_id = ?")?;
        let rows = statement.query_map([external_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<String>>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        rows.map(|row| {
            let (kind, payload, outcome_label) = row?;
            Ok(Signal {
                kind,
                payload: payload.as_deref().map(serde_json::from_str).transpose()?,
                outcome_label,
            })
        })
        .collect()
    })
}

#[derive(Debug)]
pub struct MessageDetails {
    pub sender_account_id: Option<String>,
    pub sender_name: Option<String>,
    pub thread_key: Option<String>,
    pub reply_to: Option<String>,
}

pub fn matching_messages(store: &Store, external_id: &str) -> Result<Vec<MessageDetails>> {
    store.reader(|c| {
        let mut statement = c.prepare(
            "SELECT m.sender_account_id, coalesce(nullif(a.display_name, ''), a.handle_display),
                    m.thread_key, m.reply_to
             FROM chan_messages m LEFT JOIN accounts a ON a.id = m.sender_account_id
             WHERE m.external_id = ?",
        )?;
        Ok(statement
            .query_map([external_id], message_details)?
            .collect::<rusqlite::Result<_>>()?)
    })
}

fn message_details(row: &Row<'_>) -> rusqlite::Result<MessageDetails> {
    Ok(MessageDetails {
        sender_account_id: row.get(0)?,
        sender_name: row.get(1)?,
        thread_key: row.get(2)?,
        reply_to: row.get(3)?,
    })
}

pub fn total_messages(store: &Store) -> Result<i64> {
    store.reader(|c| Ok(c.query_row("SELECT count(*) FROM chan_messages", [], |row| row.get(0))?))
}

pub fn set_connection_permissions(
    store: &Store,
    connection: &str,
    permissions: &Value,
) -> Result<()> {
    let count = store.transaction::<_, StoreError>(|tx| {
        Ok(tx.execute(
            "UPDATE connections SET config = json_set(config, '$.permissions', json(?))
             WHERE id = ?",
            params![permissions.to_string(), connection],
        )?)
    })?;
    if count != 1 {
        return Err(StoreError::Rejected(format!(
            "missing fixture connection {connection}"
        )));
    }
    Ok(())
}

pub fn set_workspace_text(store: &Store, ws: &str, key: &str, value: &str) -> Result<()> {
    store.transaction(|tx| {
        tx.execute(
            "INSERT INTO settings (workspace_id, key, value, updated) VALUES (?, ?, ?, 1)
             ON CONFLICT (workspace_id, key) DO UPDATE SET value = excluded.value",
            params![ws, key, serde_json::to_string(value)?],
        )?;
        Ok(())
    })
}
