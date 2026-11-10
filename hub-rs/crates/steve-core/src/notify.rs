//! Notifications (notify.ts): things that happened (a release went out, items were archived) rather than
//! conversation. They are `messages` rows in space `notify` with `meta.notice = {kind, level}`; a failure
//! without an item also leaves one line in the conversation pointing back at it (an item's failure is its
//! fail card, and its card in the conversation says released or failed).

use crate::Core;
use crate::error::Result;
use crate::repo::Db;
use serde::Serialize;
use serde_json::{Value, json};

const SPACE: &str = "notify";

/// One notification (`GET /api/state` `notifications[]`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Notice {
    pub id: String,
    /// release | archive | system
    pub kind: String,
    /// info | success | fail
    pub level: String,
    pub text: String,
    pub item_id: Option<String>,
    pub created: i64,
}

/// Record a notification; a failure also goes into the conversation. Returns the notification's id.
pub fn notify(
    db: &Db,
    ws: &str,
    kind: &str,
    level: &str,
    text: &str,
    item: Option<&str>,
) -> Result<String> {
    Ok(db
        .store()
        .transaction(|tx| notify_in(tx, ws, kind, level, text, item))?)
}

/// [`notify`] inside the caller's transaction: what happened and its notification are written
/// together, or neither is.
pub fn notify_in(
    tx: &steve_store::Tx,
    ws: &str,
    kind: &str,
    level: &str,
    text: &str,
    item: Option<&str>,
) -> steve_store::Result<String> {
    let id = steve_store::base::message_add(
        tx,
        ws,
        "steve",
        text,
        item,
        &json!({"notice": {"kind": kind, "level": level}}),
        SPACE,
    )?;
    /* notices stay in the notifications: an item's failure is already its fail card (the card in the
     * conversation, the pending strip), so only a failure without an item leaves a line here */
    if level == "fail" && item.is_none() {
        steve_store::base::message_add(
            tx,
            ws,
            "steve",
            text,
            None,
            &json!({"notice": {"kind": kind, "level": level, "ref": id}}),
            crate::thread::MAIN,
        )?;
    }
    Ok(id)
}

impl Core {
    /// Notifications of a workspace, newest first (≤100).
    pub fn notifications(&self, ws: &str) -> Result<Vec<Notice>> {
        let mut rows = self.store().messages_in(ws, SPACE, 100)?;
        /* newest first; among equal times the later insert first (TS: reverse, then a stable sort) */
        rows.reverse();
        rows.sort_by_key(|m| std::cmp::Reverse(m.created));
        Ok(rows
            .into_iter()
            .take(100)
            .map(|m| {
                let n = m.meta.get("notice");
                let field = |k: &str| {
                    n.and_then(|n| n.get(k))
                        .and_then(Value::as_str)
                        .map(String::from)
                };
                Notice {
                    kind: field("kind").unwrap_or_else(|| "release".into()),
                    level: field("level").unwrap_or_else(|| "success".into()),
                    id: m.id,
                    text: m.text,
                    item_id: m.item_id,
                    created: m.created,
                }
            })
            .collect())
    }
}
