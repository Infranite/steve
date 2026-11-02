//! Decisions opened and settled by the modules inside their own transaction (§0.1 point 6). They are
//! rows of the existing `decisions` table (`item_id` may be null) and publish `{type:"decision"}`.

use crate::repo::Cached;
use rusqlite::{Connection, params};
use serde_json::Value;

use crate::repo::to_json;
use crate::types::truthy;
use crate::{ChangeEvent, Decision, NewDecision, Result, Tx, now, uid};

pub fn get(c: &Connection, id: &str) -> Result<Option<Decision>> {
    let r = c
        .prepare_cached("SELECT * FROM decisions WHERE id = ?")?
        .query_row([id], Decision::from_row);
    match r {
        Ok(d) => Ok(Some(d)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// `steve_base::decisions::open(tx, NewDecision)`: same columns and defaults as `decisions.create`.
pub fn open(tx: &Tx, p: &NewDecision) -> Result<Decision> {
    let id = uid("dec");
    tx.prepare_cached(
        "INSERT INTO decisions (id, workspace_id, kind, subject_type, subject_id, item_id, session_id, question, draft, options, status, requested_by, created) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'open', ?, ?)",
    )?
    .execute(params![
        id,
        p.workspace_id,
        p.kind,
        p.subject_type,
        p.subject_id,
        p.item_id,
        p.session_id,
        p.question,
        p.draft.as_deref().unwrap_or(""),
        to_json(&p.options.clone().unwrap_or_default())?,
        p.requested_by,
        now()
    ])?;
    tx.publish(ChangeEvent::Decision {
        id: id.clone(),
        item_id: Some(p.item_id.clone()),
    });
    crate::repo::must(get(tx, &id)?)
}

/// The open decision about one subject, if any (e.g. “同一连接最多一个打开的”).
pub fn open_for(
    c: &Connection,
    ws: &str,
    subject_type: &str,
    subject_id: &str,
) -> Result<Option<Decision>> {
    let r = c
        .prepare_cached(
            "SELECT * FROM decisions WHERE workspace_id = ? AND subject_type = ? AND subject_id = ? AND status = 'open' ORDER BY created LIMIT 1",
        )?
        .query_row(params![ws, subject_type, subject_id], Decision::from_row);
    match r {
        Ok(d) => Ok(Some(d)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Settles an open decision (`resolved` or `expired`) like `decisions.resolve`; returns `false` if it
/// was not open. `undo` is stored only if truthy.
pub fn settle(tx: &Tx, id: &str, result: &Value, status: &str, undo: &Value) -> Result<bool> {
    let undo = if truthy(undo) {
        Some(to_json(undo)?)
    } else {
        None
    };
    let n = tx.run(
        "UPDATE decisions SET status = ?, result = ?, resolved = ?, undo = ? WHERE id = ? AND status = 'open'",
        params![status, to_json(result)?, now(), undo, id],
    )?;
    if n > 0 {
        let item_id = get(tx, id)?.and_then(|d| d.item_id);
        tx.publish(ChangeEvent::Decision {
            id: id.into(),
            item_id: Some(item_id),
        });
    }
    Ok(n > 0)
}
