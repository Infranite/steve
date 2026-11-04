//! `chan_send_state` (migration 30): what a pending or unknown outbound message needs beyond its
//! row. The authorization it goes out under, so a message without a decision (a rule's or a
//! trigger's reply) is found again after a restart; when it was first sent (the provider's dedupe
//! window counts from there, across restarts); and the first try's text and mentions, so a repeat
//! under the same dedupe key is the same message. Removed once it is sent or failed for certain.

use rusqlite::{Connection, OptionalExtension, params};
use steve_store::Tx;

use crate::ChannelResult;
use crate::deliver::Auth;

/// One message's state.
#[derive(Debug, Clone, PartialEq)]
pub struct SendState {
    pub auth: Auth,
    pub first_at: Option<i64>,
    pub attempts: i64,
}

/// Records the authorization of `message` unless it has one (a rule's reply when it is stored,
/// any send when it is claimed).
pub fn keep(tx: &Tx, message: &str, auth: &Auth) -> ChannelResult<()> {
    let a = serde_json::to_string(auth)
        .map_err(|e| steve_modules::ApiError::Internal(e.to_string()))?;
    tx.prepare_cached(
        "INSERT INTO chan_send_state (message_id, auth) VALUES (?, ?) ON CONFLICT(message_id) DO NOTHING",
    )?
    .execute(params![message, a])?;
    Ok(())
}

/// A try of `message`: the first one keeps `text` and `at` and its time; a later one gets what the
/// first sent. Returns what to send.
pub fn attempt(
    tx: &Tx,
    message: &str,
    auth: &Auth,
    text: &str,
    at: &[String],
    now: i64,
) -> ChannelResult<(String, Vec<String>)> {
    keep(tx, message, auth)?;
    let kept: Option<(Option<String>, Option<String>)> = tx
        .prepare_cached("SELECT text, at FROM chan_send_state WHERE message_id = ?")?
        .query_row([message], |r| Ok((r.get(0)?, r.get(1)?)))
        .optional()?;
    let out = match kept {
        Some((Some(t), a)) => (
            t,
            a.and_then(|a| serde_json::from_str(&a).ok())
                .unwrap_or_default(),
        ),
        _ => {
            let a = serde_json::to_string(at).unwrap_or_else(|_| "[]".into());
            tx.prepare_cached(
                "UPDATE chan_send_state SET text = ?, at = ?, first_at = coalesce(first_at, ?) WHERE message_id = ?",
            )?
            .execute(params![text, a, now, message])?;
            (text.to_string(), at.to_vec())
        }
    };
    tx.prepare_cached("UPDATE chan_send_state SET attempts = attempts + 1 WHERE message_id = ?")?
        .execute([message])?;
    Ok(out)
}

pub fn error(tx: &Tx, message: &str, e: &str) -> ChannelResult<()> {
    tx.prepare_cached("UPDATE chan_send_state SET last_error = ? WHERE message_id = ?")?
        .execute(params![e, message])?;
    Ok(())
}

pub fn get(c: &Connection, message: &str) -> ChannelResult<Option<SendState>> {
    let row: Option<(String, Option<i64>, i64)> = c
        .prepare_cached(
            "SELECT auth, first_at, attempts FROM chan_send_state WHERE message_id = ?",
        )?
        .query_row([message], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .optional()?;
    Ok(row.and_then(|(a, first_at, attempts)| {
        Some(SendState {
            auth: serde_json::from_str(&a).ok()?,
            first_at,
            attempts,
        })
    }))
}

/// Sent, or failed for certain: nothing left to try again.
pub fn forget(tx: &Tx, message: &str) -> ChannelResult<()> {
    tx.prepare_cached("DELETE FROM chan_send_state WHERE message_id = ?")?
        .execute([message])?;
    Ok(())
}
