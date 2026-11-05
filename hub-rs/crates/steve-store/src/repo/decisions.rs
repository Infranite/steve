//! Decisions (the single source of 待处理) and the main thread with Steve.

use super::Cached;
use rusqlite::params;
use serde::Deserialize;
use serde_json::Value;

use super::{all, one, to_json};
use crate::types::truthy;
use crate::{ChangeEvent, Decision, Message, Result, Store, now, uid};

impl Store {
    pub fn decisions(&self) -> Decisions<'_> {
        Decisions(self)
    }
    pub fn messages(&self) -> Messages<'_> {
        Messages(self)
    }
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct NewDecision {
    pub workspace_id: String,
    /// `accept` | `ask` | `approve` | `fail` | `propose`
    pub kind: String,
    pub subject_type: String,
    pub subject_id: String,
    pub item_id: Option<String>,
    pub session_id: Option<String>,
    pub question: String,
    pub draft: Option<String>,
    pub options: Option<Vec<String>>,
    pub requested_by: String,
}

pub struct Decisions<'a>(&'a Store);

const DECISION: &str = "SELECT * FROM decisions WHERE id = ?";

fn insert(c: &rusqlite::Connection, p: &NewDecision) -> Result<Decision> {
    let id = uid("dec");
    c.run(
        "INSERT INTO decisions (id, workspace_id, kind, subject_type, subject_id, item_id, session_id, question, draft, options, status, requested_by, created) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 'open', ?, ?)",
        params![
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
        ],
    )?;
    one(c, DECISION, [&id], Decision::from_row)?
        .ok_or_else(|| crate::StoreError::Conflict("decision vanished".into()))
}

/// [`Decisions::create`] inside the caller's transaction; published after the commit.
pub fn decision_create_in(tx: &crate::Tx<'_>, p: &NewDecision) -> Result<Decision> {
    let d = insert(tx, p)?;
    tx.publish(ChangeEvent::Decision {
        id: d.id.clone(),
        item_id: Some(p.item_id.clone()),
    });
    Ok(d)
}

/// [`Decisions::resolve`] inside the caller's transaction: settles it if it is open. `true`: it was.
pub fn decision_resolve_in(
    tx: &crate::Tx<'_>,
    id: &str,
    result: &Value,
    status: &str,
) -> Result<bool> {
    let n = tx.execute(
        "UPDATE decisions SET status = ?, result = ?, resolved = ? WHERE id = ? AND status = 'open'",
        params![status, to_json(result)?, now(), id],
    )?;
    let item: Option<Option<String>> = tx
        .query_row("SELECT item_id FROM decisions WHERE id = ?", [id], |r| {
            r.get(0)
        })
        .ok();
    tx.publish(ChangeEvent::Decision {
        id: id.into(),
        item_id: item,
    });
    Ok(n > 0)
}

/// A card whose answer was never applied (the Node refused it, or the request ended first) is
/// expired, even if it was already marked resolved: it never claims an answer that did not happen.
pub fn decision_expire_unapplied_in(tx: &crate::Tx<'_>, id: &str, result: &Value) -> Result<bool> {
    let n = tx.execute(
        "UPDATE decisions SET status = 'expired', result = ?, resolved = ? WHERE id = ? AND status IN ('open','resolved')",
        params![to_json(result)?, now(), id],
    )?;
    let item: Option<Option<String>> = tx
        .query_row("SELECT item_id FROM decisions WHERE id = ?", [id], |r| {
            r.get(0)
        })
        .ok();
    tx.publish(ChangeEvent::Decision {
        id: id.into(),
        item_id: item,
    });
    Ok(n > 0)
}

fn changed(w: &crate::W, id: &str) -> Result<Option<Decision>> {
    let d = one(w.c, DECISION, [id], Decision::from_row)?;
    w.publish(ChangeEvent::Decision {
        id: id.into(),
        item_id: d.as_ref().map(|d| d.item_id.clone()),
    });
    Ok(d)
}

impl Decisions<'_> {
    /// Open decisions, oldest first.
    pub fn open(&self, ws: &str) -> Result<Vec<Decision>> {
        self.0.read(|c| all(c, "SELECT * FROM decisions WHERE workspace_id = ? AND status = 'open' ORDER BY created", [ws], Decision::from_row))
    }

    /// Settled decisions resolved after `since`, newest first, at most 100.
    pub fn recent(&self, ws: &str, since: i64) -> Result<Vec<Decision>> {
        self.0.read(|c| {
            all(
                c,
                "SELECT * FROM decisions WHERE workspace_id = ? AND status != 'open' AND resolved > ? ORDER BY resolved DESC LIMIT 100",
                params![ws, since],
                Decision::from_row,
            )
        })
    }

    pub fn get(&self, id: &str) -> Result<Option<Decision>> {
        self.0.read(|c| one(c, DECISION, [id], Decision::from_row))
    }

    pub fn of_item(&self, item_id: &str) -> Result<Vec<Decision>> {
        self.0.read(|c| {
            all(
                c,
                "SELECT * FROM decisions WHERE item_id = ? ORDER BY created",
                [item_id],
                Decision::from_row,
            )
        })
    }

    pub fn create(&self, p: &NewDecision) -> Result<Option<Decision>> {
        self.0.write(|w| {
            let d = insert(w.c, p)?;
            w.publish(ChangeEvent::Decision {
                id: d.id.clone(),
                item_id: Some(p.item_id.clone()),
            });
            Ok(Some(d))
        })
    }

    /// Settles an open decision (no-op if it is not open, but still publishes). `status` is `resolved`
    /// or `expired`; `undo` (the item fields to restore) is stored only if truthy, as in TS.
    pub fn resolve(
        &self,
        id: &str,
        result: &Value,
        status: &str,
        undo: &Value,
    ) -> Result<Option<Decision>> {
        self.0.write(|w| {
            let undo = if truthy(undo) { Some(to_json(undo)?) } else { None };
            w.c.run(
                "UPDATE decisions SET status = ?, result = ?, resolved = ?, undo = ? WHERE id = ? AND status = 'open'",
                params![status, to_json(result)?, now(), undo, id],
            )?;
            changed(w, id)
        })
    }

    /// Replaces the result of a decision already settled, e.g. what became of an approved release after it was
    /// answered. Status, resolved time and undo stay; publishes like any change.
    pub fn set_result(&self, id: &str, result: &Value) -> Result<Option<Decision>> {
        self.0.write(|w| {
            w.c.run(
                "UPDATE decisions SET result = ? WHERE id = ? AND status != 'open'",
                params![to_json(result)?, id],
            )?;
            changed(w, id)
        })
    }

    /// Merge annotations into the current settled result under the writer lock. An answer snapshot
    /// may predate a background result (for example a release failure); never replace that result.
    pub fn merge_result(&self, id: &str, annotations: &Value) -> Result<Option<Decision>> {
        self.0.write(|w| {
            w.c.run(
                "UPDATE decisions SET result = json_patch(CASE WHEN json_type(result) = 'object' THEN result ELSE '{}' END, ?) WHERE id = ? AND status != 'open'",
                params![to_json(annotations)?, id],
            )?;
            changed(w, id)
        })
    }

    /// Back to open, clearing result, resolved time and undo.
    pub fn reopen(&self, id: &str) -> Result<Option<Decision>> {
        self.0.write(|w| {
            w.c.run("UPDATE decisions SET status = 'open', result = NULL, resolved = NULL, undo = NULL WHERE id = ?", [id])?;
            changed(w, id)
        })
    }
}

/// One thread per workspace and `space` (only `steve` today).
pub struct Messages<'a>(&'a Store);

impl Messages<'_> {
    /// The last `limit` messages, oldest first (TS defaults: space `steve`, limit 200).
    pub fn list(&self, ws: &str, space: &str, limit: i64) -> Result<Vec<Message>> {
        self.0.read(|c| {
            let mut v = all(
                c,
                "SELECT * FROM messages WHERE workspace_id = ? AND space = ? ORDER BY created DESC LIMIT ?",
                params![ws, space, limit],
                Message::from_row,
            )?;
            v.reverse();
            Ok(v)
        })
    }

    /// The newest `limit` messages of a conversation up to `until` (created, inclusive), oldest first:
    /// what was said around a message however much was said after it.
    pub fn list_until(
        &self,
        ws: &str,
        space: &str,
        until: i64,
        limit: i64,
    ) -> Result<Vec<Message>> {
        self.0.read(|c| {
            let mut v = all(
                c,
                "SELECT * FROM messages WHERE workspace_id = ? AND space = ? AND created <= ? ORDER BY created DESC LIMIT ?",
                params![ws, space, until, limit],
                Message::from_row,
            )?;
            v.reverse();
            Ok(v)
        })
    }

    /// Older history, oldest first: messages created before `before` and from `after` on (inclusive), the newest
    /// `limit` of them. Paging back is `after = 0`; filling the gap down to a message is `after = its created`.
    pub fn list_before(
        &self,
        ws: &str,
        space: &str,
        before: i64,
        after: i64,
        limit: i64,
    ) -> Result<Vec<Message>> {
        self.0.read(|c| {
            let mut v = all(
                c,
                "SELECT * FROM messages WHERE workspace_id = ? AND space = ? AND created < ? AND created >= ? ORDER BY created DESC LIMIT ?",
                params![ws, space, before, after, limit],
                Message::from_row,
            )?;
            v.reverse();
            Ok(v)
        })
    }

    /// Messages whose text contains `q` (ASCII case-insensitive) or that belong to item `q`, newest first.
    pub fn search(&self, ws: &str, space: &str, q: &str, limit: i64) -> Result<Vec<Message>> {
        let like = format!(
            "%{}%",
            q.replace('\\', "\\\\")
                .replace('%', "\\%")
                .replace('_', "\\_")
        );
        self.0.read(|c| {
            all(
                c,
                "SELECT * FROM messages WHERE workspace_id = ? AND space = ? AND (text LIKE ? ESCAPE '\\' OR upper(coalesce(item_id, '')) = upper(?)) ORDER BY created DESC LIMIT ?",
                params![ws, space, like, q, limit],
                Message::from_row,
            )
        })
    }

    /// The answer an item was taken from (its offer, marked `promoted` with the item), if any.
    pub fn promoted_to(&self, ws: &str, item: &str) -> Result<Option<Message>> {
        self.0.read(|c| {
            one(
                c,
                "SELECT * FROM messages WHERE workspace_id = ? AND json_extract(meta, '$.promoted') = ? ORDER BY created LIMIT 1",
                params![ws, item],
                Message::from_row,
            )
        })
    }

    /// Returns the id. `meta` is stored only if truthy (TS defaults: no item, meta null, space `steve`).
    pub fn add(
        &self,
        ws: &str,
        author: &str,
        text: &str,
        item_id: Option<&str>,
        meta: &Value,
        space: &str,
    ) -> Result<String> {
        self.0.write(|w| {
            let id = uid("msg");
            let meta = if truthy(meta) { Some(to_json(meta)?) } else { None };
            w.c.run(
                "INSERT INTO messages (id, workspace_id, space, author, text, item_id, meta, created) VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                params![id, ws, space, author, text, item_id, meta, now()],
            )?;
            w.publish(ChangeEvent::Message { id: id.clone() });
            Ok(id)
        })
    }

    pub fn get(&self, id: &str) -> Result<Option<Message>> {
        self.0.read(|c| {
            one(
                c,
                "SELECT * FROM messages WHERE id = ?",
                [id],
                Message::from_row,
            )
        })
    }

    pub fn patch_meta(&self, id: &str, meta: &Value) -> Result<()> {
        self.0.write(|w| {
            w.c.run(
                "UPDATE messages SET meta = ? WHERE id = ?",
                params![to_json(meta)?, id],
            )?;
            w.publish(ChangeEvent::Message { id: id.into() });
            Ok(())
        })
    }

    /// Rewrites a message's text and meta (thread flow-back merging a reply into the one before).
    pub fn patch_text_meta(&self, id: &str, text: &str, meta: &Value) -> Result<()> {
        self.0.write(|w| {
            w.c.run(
                "UPDATE messages SET text = ?, meta = ? WHERE id = ?",
                params![text, to_json(meta)?, id],
            )?;
            w.publish(ChangeEvent::Message { id: id.into() });
            Ok(())
        })
    }

    /// The newest message by `author` in the `steve` space. (TS returns `meta` unparsed here; nothing
    /// serializes this one, so it is parsed like everywhere else.)
    pub fn last_of(&self, ws: &str, author: &str) -> Result<Option<Message>> {
        self.0.read(|c| {
            one(
                c,
                "SELECT * FROM messages WHERE workspace_id = ? AND space = 'steve' AND author = ? ORDER BY created DESC LIMIT 1",
                [ws, author],
                Message::from_row,
            )
        })
    }
}

/// Settles the item's open decisions as `expired` with `result`, inside the caller's transaction
/// (`Decisions::resolve` for each, like a finished item's “nothing left to decide”). Returns how many.
pub fn decisions_expire_open_in(tx: &crate::Tx, item_id: &str, result: &Value) -> Result<usize> {
    let ids: Vec<String> = {
        let mut st = tx.prepare_cached(
            "SELECT id FROM decisions WHERE item_id = ? AND status = 'open' ORDER BY created",
        )?;
        st.query_map([item_id], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    let result = to_json(result)?;
    for id in &ids {
        tx.run(
            "UPDATE decisions SET status = 'expired', result = ?, resolved = ?, undo = NULL WHERE id = ? AND status = 'open'",
            params![result, now(), id],
        )?;
        tx.publish(ChangeEvent::Decision {
            id: id.clone(),
            item_id: Some(Some(item_id.into())),
        });
    }
    Ok(ids.len())
}
