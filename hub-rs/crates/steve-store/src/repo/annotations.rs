//! The user's notes on an item's outputs (ST-38): drafts kept until sent, then the line of the item they
//! were sent as. A send is taken by its key, so the same send repeated (a double click, a retry after a
//! lost answer) reaches the item once.

use rusqlite::{Row, params};
use serde::Serialize;
use serde_json::Value;

use super::{Cached, all, one};
use crate::{ChangeEvent, Result, Store, now, uid};

/// A send interrupted (the Hub stopped between taking the drafts and writing the line) frees them after this.
const SENDING_STALE_MS: i64 = 5 * 60 * 1000;

impl Store {
    pub fn annotations(&self) -> Annotations<'_> {
        Annotations(self)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Annotation {
    pub id: String,
    pub item_id: String,
    pub artifact_id: String,
    pub path: String,
    /// `{"type":"region","x","y","w","h"}` (fractions of the image), `{"type":"text","quote",…}`,
    /// `{"type":"page","page"}` or `{"type":"file"}`.
    pub anchor: Value,
    pub note: String,
    /// `draft` | `sending` | `sent`
    pub status: String,
    pub send_key: Option<String>,
    pub timeline_id: Option<i64>,
    pub created: i64,
    pub updated: i64,
    pub sent: Option<i64>,
}

impl Annotation {
    fn from_row(r: &Row) -> rusqlite::Result<Self> {
        let anchor: String = r.get("anchor")?;
        Ok(Annotation {
            id: r.get("id")?,
            item_id: r.get("item_id")?,
            artifact_id: r.get("artifact_id")?,
            path: r.get("path")?,
            anchor: serde_json::from_str(&anchor).unwrap_or(Value::Null),
            note: r.get("note")?,
            status: r.get("status")?,
            send_key: r.get("send_key")?,
            timeline_id: r.get("timeline_id")?,
            created: r.get("created")?,
            updated: r.get("updated")?,
            sent: r.get("sent")?,
        })
    }
}

/// What a send took: the drafts it now owns, or (`again`) the ones an earlier send with the same key took.
#[derive(Debug, Clone, PartialEq)]
pub struct Claim {
    pub again: bool,
    pub taken: Vec<Annotation>,
}

const ONE: &str = "SELECT * FROM annotations WHERE id = ? AND item_id = ?";

pub struct Annotations<'a>(&'a Store);

impl Annotations<'_> {
    pub fn list(&self, item_id: &str) -> Result<Vec<Annotation>> {
        self.0.read(|c| {
            all(
                c,
                "SELECT * FROM annotations WHERE item_id = ? ORDER BY created, id",
                [item_id],
                Annotation::from_row,
            )
        })
    }

    pub fn add(
        &self,
        item_id: &str,
        artifact_id: &str,
        path: &str,
        anchor: &Value,
        note: &str,
    ) -> Result<Annotation> {
        self.0.write(|w| {
            let (id, t) = (uid("ann"), now());
            w.c.run(
                "INSERT INTO annotations (id, item_id, artifact_id, path, anchor, note, status, created, updated) VALUES (?, ?, ?, ?, ?, ?, 'draft', ?, ?)",
                params![id, item_id, artifact_id, path, anchor.to_string(), note, t, t],
            )?;
            w.publish(ChangeEvent::Item { id: item_id.into() });
            super::must(one(w.c, ONE, [&id, item_id], Annotation::from_row)?)
        })
    }

    /// A draft's new text; `None` when there is no such draft (gone, or already sent).
    pub fn edit(&self, item_id: &str, id: &str, note: &str) -> Result<Option<Annotation>> {
        self.0.write(|w| {
            let n = w.c.run(
                "UPDATE annotations SET note = ?, updated = ? WHERE id = ? AND item_id = ? AND status = 'draft'",
                params![note, now(), id, item_id],
            )?;
            if n == 0 {
                return Ok(None);
            }
            w.publish(ChangeEvent::Item { id: item_id.into() });
            one(w.c, ONE, [id, item_id], Annotation::from_row)
        })
    }

    /// Drops a draft; false when there is no such draft. What was sent stays.
    pub fn remove(&self, item_id: &str, id: &str) -> Result<bool> {
        self.0.write(|w| {
            let n = w.c.run(
                "DELETE FROM annotations WHERE id = ? AND item_id = ? AND status = 'draft'",
                [id, item_id],
            )?;
            if n > 0 {
                w.publish(ChangeEvent::Item { id: item_id.into() });
            }
            Ok(n > 0)
        })
    }

    /// Takes the drafts `ids` for the send `key`. A key seen before takes nothing and returns what it took
    /// then (`again`); drafts another send took already are left out.
    pub fn claim(&self, item_id: &str, ids: &[String], key: &str) -> Result<Claim> {
        self.0.write(|w| {
            let tx = w.tx()?;
            let before = all(
                &tx,
                "SELECT * FROM annotations WHERE item_id = ? AND send_key = ? ORDER BY created, id",
                [item_id, key],
                Annotation::from_row,
            )?;
            if !before.is_empty() {
                return Ok(Claim {
                    again: true,
                    taken: before,
                });
            }
            let t = now();
            for id in ids {
                tx.run(
                    "UPDATE annotations SET status = 'sending', send_key = ?, updated = ? WHERE id = ? AND item_id = ? AND (status = 'draft' OR (status = 'sending' AND updated < ?))",
                    params![key, t, id, item_id, t - SENDING_STALE_MS],
                )?;
            }
            let taken = all(
                &tx,
                "SELECT * FROM annotations WHERE item_id = ? AND send_key = ? ORDER BY created, id",
                [item_id, key],
                Annotation::from_row,
            )?;
            tx.commit()?;
            if !taken.is_empty() {
                w.publish(ChangeEvent::Item { id: item_id.into() });
            }
            Ok(Claim {
                again: false,
                taken,
            })
        })
    }

    /// The send `key` went out (`sent`, as the item's line `timeline_id` when it is known), or failed: its
    /// drafts are drafts again, to be sent once more.
    pub fn settle(
        &self,
        item_id: &str,
        key: &str,
        sent: bool,
        timeline_id: Option<i64>,
    ) -> Result<()> {
        self.0.write(|w| {
            let t = now();
            match sent {
                true => w.c.run(
                    "UPDATE annotations SET status = 'sent', sent = ?, timeline_id = ?, updated = ? WHERE item_id = ? AND send_key = ? AND status = 'sending'",
                    params![t, timeline_id, t, item_id, key],
                )?,
                false => w.c.run(
                    "UPDATE annotations SET status = 'draft', send_key = NULL, updated = ? WHERE item_id = ? AND send_key = ? AND status = 'sending'",
                    params![t, item_id, key],
                )?,
            };
            w.publish(ChangeEvent::Item { id: item_id.into() });
            Ok(())
        })
    }
}
