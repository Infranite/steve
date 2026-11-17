//! What the sweep would do (migration 101, #334), in shadow: one row per decision of the sweep, keyed
//! by (item, cycle, session, turn, action, reason). Seeing the same decision again — on the next
//! pass or after a restart — is the same row; a decision that stopped holding is marked cleared, and
//! counted as a new episode when it comes back. Nothing here carries an action out. Also the two
//! reads the sweep needs that no other repository has: the open task items, and when an item was
//! last paused through the item action.

use rusqlite::params;
use serde::Serialize;

use super::{all, one};
use crate::{Result, Store, StoreError, uid};

/// What the sweep may do.
pub const ACTIONS: [&str; 3] = ["wake", "card", "remind"];

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WouldDoRow {
    pub id: String,
    pub item_id: String,
    pub cycle: i64,
    pub session_id: String,
    pub turn_id: String,
    pub action: String,
    pub reason: String,
    pub detail: Option<String>,
    pub row: i64,
    pub code: String,
    pub due_at: i64,
    pub first_seen: i64,
    pub cleared_at: Option<i64>,
    pub episodes: i64,
    pub executed: bool,
}

#[derive(Debug, Clone, Default)]
pub struct NewWouldDo<'a> {
    pub item_id: &'a str,
    pub cycle: i64,
    pub session_id: &'a str,
    pub turn_id: &'a str,
    pub action: &'a str,
    pub reason: &'a str,
    pub detail: Option<&'a str>,
    pub row: i64,
    pub code: &'a str,
    pub due_at: i64,
}

fn row(r: &rusqlite::Row) -> rusqlite::Result<WouldDoRow> {
    Ok(WouldDoRow {
        id: r.get("id")?,
        item_id: r.get("item_id")?,
        cycle: r.get("cycle")?,
        session_id: r.get("session_id")?,
        turn_id: r.get("turn_id")?,
        action: r.get("action")?,
        reason: r.get("reason")?,
        detail: r.get("detail")?,
        row: r.get("row")?,
        code: r.get("code")?,
        due_at: r.get("due_at")?,
        first_seen: r.get("first_seen")?,
        cleared_at: r.get("cleared_at")?,
        episodes: r.get("episodes")?,
        executed: r.get::<_, i64>("executed")? != 0,
    })
}

pub struct WouldDo<'a>(&'a Store);

impl Store {
    pub fn would_do(&self) -> WouldDo<'_> {
        WouldDo(self)
    }
}

impl WouldDo<'_> {
    /// Records the sweep's decision for `w.item_id` at `at` and clears the item's other open ones
    /// (the sweep decides at most one thing per item). Returns the row's id and whether it is new or
    /// came back (`false`: the same decision was already open — nothing changed).
    pub fn record(&self, w: &NewWouldDo<'_>, at: i64) -> Result<(String, bool)> {
        if !ACTIONS.contains(&w.action) {
            return Err(StoreError::Conflict(format!(
                "unknown sweep action: {}",
                w.action
            )));
        }
        self.0.transaction(|tx| {
            let id = uid("wd");
            let inserted = tx.execute(
                "INSERT INTO item_would_do(id,item_id,cycle,session_id,turn_id,action,reason,detail,row,code,due_at,first_seen)
                 VALUES (?,?,?,?,?,?,?,?,?,?,?,?)
                 ON CONFLICT(item_id,cycle,session_id,turn_id,action,reason) DO UPDATE
                   SET cleared_at=NULL,episodes=episodes+1,detail=excluded.detail,row=excluded.row,
                       code=excluded.code,due_at=excluded.due_at
                   WHERE cleared_at IS NOT NULL",
                params![id, w.item_id, w.cycle, w.session_id, w.turn_id, w.action, w.reason,
                    w.detail, w.row, w.code, w.due_at, at],
            )? > 0;
            let id: String = tx.query_row(
                "SELECT id FROM item_would_do WHERE item_id=? AND cycle=? AND session_id=? AND turn_id=? AND action=? AND reason=?",
                params![w.item_id, w.cycle, w.session_id, w.turn_id, w.action, w.reason],
                |r| r.get(0),
            )?;
            tx.execute(
                "UPDATE item_would_do SET cleared_at=? WHERE item_id=? AND cleared_at IS NULL AND id<>?",
                params![at, w.item_id, id],
            )?;
            Ok::<_, StoreError>((id, inserted))
        })
    }

    /// The sweep would do nothing for `item` now: its open decisions are cleared. Returns how many.
    pub fn clear(&self, item: &str, at: i64) -> Result<usize> {
        self.0.transaction(|tx| {
            Ok::<_, StoreError>(tx.execute(
                "UPDATE item_would_do SET cleared_at=? WHERE item_id=? AND cleared_at IS NULL",
                params![at, item],
            )?)
        })
    }

    /// Clears the open decisions of items the sweep no longer looks at (closed, archived, or no
    /// longer a task). Returns how many.
    pub fn clear_gone(&self, at: i64) -> Result<usize> {
        self.0.transaction(|tx| {
            Ok::<_, StoreError>(tx.execute(
                "UPDATE item_would_do SET cleared_at=? WHERE cleared_at IS NULL AND item_id IN
                   (SELECT id FROM items WHERE phase='closed' OR archived<>0 OR kind<>'task')",
                [at],
            )?)
        })
    }

    /// The newest decisions about `item`, open or cleared, newest first.
    pub fn log(&self, item: &str, limit: usize) -> Result<Vec<WouldDoRow>> {
        self.0.read(|c| {
            all(
                c,
                "SELECT * FROM item_would_do WHERE item_id=? ORDER BY first_seen DESC,rowid DESC LIMIT ?",
                params![item, limit as i64],
                row,
            )
        })
    }

    /// Every open task item, in every workspace (`ready`, `active` or `closing`, not archived).
    pub fn open_items(&self) -> Result<Vec<String>> {
        self.0.read(|c| {
            all(
                c,
                "SELECT id FROM items WHERE kind='task' AND phase<>'closed' AND archived=0 ORDER BY id",
                [],
                |r| r.get(0),
            )
        })
    }

    /// When `item` was last paused with the item action (its audit record), if ever.
    pub fn paused_at(&self, item: &str) -> Result<Option<i64>> {
        self.0.read(|c| {
            one(
                c,
                "SELECT ts FROM audit WHERE target=? AND action='item.pause' ORDER BY id DESC LIMIT 1",
                [item],
                |r| r.get::<_, i64>(0),
            )
        })
    }
}
