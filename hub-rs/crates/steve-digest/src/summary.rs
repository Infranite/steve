//! A conversation's summary: a text part (the digest Bot's, rewritten once per window after its chunks) and a
//! rule part (an alert conversation's rollup, written by code). Every version is kept: undo goes back one, the
//! page shows the newest.

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde_json::Value;
use steve_store::{StoreError, Tx};

use crate::conv::ConvRef;

type Result<T> = std::result::Result<T, StoreError>;

#[derive(Debug, Clone, Serialize)]
pub struct Summary {
    pub version: i64,
    pub text: String,
    pub rules: Value,
    pub packet_id: Option<String>,
    pub writer: String,
    pub created: i64,
}

pub fn latest(c: &Connection, ws: &str, conv: &ConvRef) -> Result<Option<Summary>> {
    Ok(c.prepare_cached(
        "SELECT version, text, rules, packet_id, writer, created FROM conv_summaries
              WHERE workspace_id = ? AND conv = ? ORDER BY version DESC LIMIT 1",
    )?
    .query_row(params![ws, conv.key()], |r| {
        Ok(Summary {
            version: r.get(0)?,
            text: r.get(1)?,
            rules: serde_json::from_str(&r.get::<_, String>(2)?).unwrap_or(Value::Null),
            packet_id: r.get(3)?,
            writer: r.get(4)?,
            created: r.get(5)?,
        })
    })
    .optional()?)
}

/// Writes a new version: `text` and/or `rules` replaced, the other part kept. Returns the version.
pub fn write(
    tx: &Tx,
    ws: &str,
    conv: &ConvRef,
    text: Option<&str>,
    rules: Option<&Value>,
    packet: Option<&str>,
    writer: &str,
    now: i64,
) -> Result<i64> {
    let old = latest(tx, ws, conv)?;
    let version = old.as_ref().map_or(1, |o| o.version + 1);
    let text = text
        .map(str::to_string)
        .or_else(|| old.as_ref().map(|o| o.text.clone()))
        .unwrap_or_default();
    let rules = rules
        .cloned()
        .or_else(|| old.as_ref().map(|o| o.rules.clone()))
        .filter(|r| !r.is_null());
    tx.execute(
        "INSERT INTO conv_summaries (workspace_id, conv, version, text, rules, packet_id, writer, created) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            ws,
            conv.key(),
            version,
            text,
            rules.map(|r| r.to_string()).unwrap_or_default(),
            packet,
            writer,
            now
        ],
    )?;
    crate::event(tx, ws, &conv.key());
    Ok(version)
}

/// Removes version `version` (undo of the write that made it, while it is the newest). Returns whether it went.
pub fn remove_latest(tx: &Tx, ws: &str, conv: &ConvRef, version: i64) -> Result<bool> {
    let newest = latest(tx, ws, conv)?.map(|s| s.version);
    if newest != Some(version) {
        return Ok(false);
    }
    tx.execute(
        "DELETE FROM conv_summaries WHERE workspace_id = ? AND conv = ? AND version = ?",
        params![ws, conv.key(), version],
    )?;
    crate::event(tx, ws, &conv.key());
    Ok(true)
}
