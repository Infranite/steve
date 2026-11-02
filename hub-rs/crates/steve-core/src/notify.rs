//! Notifications (notify.ts): things that happened (a release went out, items were archived) rather than
//! conversation. They are `messages` rows in space `notify` with `meta.notice = {kind, level}`; a failure
//! without an item also leaves one line in the conversation pointing back at it (an item's failure is its
//! fail card, and its card in the conversation says released or failed). Release results written before this, as plain
//! Steve messages in the conversation, are recognised by their text; the successful ones leave the conversation.

use crate::Core;
use crate::error::Result;
use crate::repo::{Db, HubEvent, Message};
use crate::util::now;
use regex::Regex;
use serde::Serialize;
use serde_json::{Value, json};
use std::sync::LazyLock;

const SPACE: &str = "notify";
const SEEN: &str = "notifySeen";
static LEGACY_OK: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^已发布新版本 ").expect("valid regex"));
static LEGACY_FAIL: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^发布 .*失败，已回到 ").expect("valid regex"));

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

fn legacy(m: &Message) -> bool {
    m.author == "steve"
        && m.meta.is_null()
        && (LEGACY_OK.is_match(&m.text) || LEGACY_FAIL.is_match(&m.text))
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
    let id = db.message_add_in(
        ws,
        SPACE,
        "steve",
        text,
        item,
        &json!({"notice": {"kind": kind, "level": level}}),
    )?;
    /* notices stay in the notifications: an item's failure is already its fail card (the card in the
     * conversation, the pending strip), so only a failure without an item leaves a line here */
    if level == "fail" && item.is_none() {
        db.message_add(
            ws,
            "steve",
            text,
            None,
            Some(&json!({"notice": {"kind": kind, "level": level, "ref": id}})),
        )?;
    }
    Ok(id)
}

impl Core {
    /// Notifications of a workspace, newest first (≤100), legacy release messages included.
    pub fn notifications(&self, ws: &str) -> Result<Vec<Notice>> {
        let db = self.store();
        let mut rows = db.messages_in(ws, SPACE, 100)?;
        rows.extend(db.messages_in(ws, "steve", 500)?.into_iter().filter(legacy));
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
                    level: field("level").unwrap_or_else(|| {
                        if LEGACY_FAIL.is_match(&m.text) {
                            "fail"
                        } else {
                            "success"
                        }
                        .into()
                    }),
                    id: m.id,
                    text: m.text,
                    item_id: m.item_id,
                    created: m.created,
                }
            })
            .collect())
    }

    /// The conversation without the notices that used to be written into it (successful releases).
    pub fn thread(&self, ws: &str) -> Result<Vec<Message>> {
        Ok(self
            .store()
            .messages(ws)?
            .into_iter()
            .filter(|m| !(legacy(m) && LEGACY_OK.is_match(&m.text)) && !crate::thread::hidden(m))
            .collect())
    }

    /// The read cursor (ms); the first look after the upgrade counts everything so far as seen.
    pub fn notify_seen(&self, ws: &str) -> Result<i64> {
        let db = self.store();
        let cur: Option<Value> = db.store().settings().get(ws, SEEN, None)?;
        if let Some(v) = cur.and_then(|v| v.as_f64()) {
            return Ok(v as i64);
        }
        let at = now();
        db.exec(
            "INSERT OR IGNORE INTO settings (workspace_id, key, value, updated) VALUES (?, ?, ?, ?)",
            &[&ws, &SEEN, &at.to_string(), &at],
        )?;
        Ok(at)
    }

    /// Everything up to `at` (default now, never later than now) has been seen; no revision row.
    pub fn mark_notifications_seen(&self, ws: &str, at: Option<i64>) -> Result<i64> {
        let t = now();
        let at = at.unwrap_or(t).min(t);
        self.store().exec(
            "INSERT INTO settings (workspace_id, key, value, updated) VALUES (?, ?, ?, ?) ON CONFLICT(workspace_id, key) DO UPDATE SET value = excluded.value, updated = excluded.updated",
            &[&ws, &SEEN, &at.to_string(), &t],
        )?;
        self.store().publish(HubEvent::Settings { id: ws.into() });
        Ok(at)
    }
}
