//! A memory as an index of key information: besides its text, the conversations it came from, the people and
//! the matter it is about, and the message range it rests on.
//!
//! Extends the existing memory rows, no second store: the conversation and the matter are `mem_about` rows
//! (object types `space` / `mail_thread` and `matter`, next to `party`, `project`, `item` …), the message range
//! a `mem_ranges` row (conversation, first and last Steve message id and time, the packet). `steve_memory_list`
//! returns them; `steve_im_read` reads the range again.

use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use steve_store::{StoreError, Tx};

use crate::conv::ConvRef;

type Result<T> = std::result::Result<T, StoreError>;

/// The message range a memory rests on.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Range {
    pub conv: String,
    pub first_id: String,
    pub last_id: String,
    pub first_at: i64,
    pub last_at: i64,
}

/// What [`link`] added (so undo removes exactly that).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Added {
    pub mem: String,
    /// `(type, id)` rows of `mem_about` that were not there.
    pub about: Vec<(String, String)>,
    pub range: Option<Range>,
}

/// Indexes memory `mem` of `ws`: about the conversation, the matter and the parties, resting on `range`.
pub fn link(
    tx: &Tx,
    ws: &str,
    mem: &str,
    conv: &ConvRef,
    matter: Option<&str>,
    parties: &[String],
    range: Option<&Range>,
    packet: Option<&str>,
) -> Result<Added> {
    let exists: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM mems WHERE workspace_id = ? AND id = ?)",
        params![ws, mem],
        |r| r.get(0),
    )?;
    if !exists || crate::conv::info(tx, ws, conv)?.is_none() {
        return Err(StoreError::Hook(
            "unknown digest memory or conversation".into(),
        ));
    }
    let matter = match matter {
        Some(m) => Some(
            crate::matter::resolve(tx, ws, m)?
                .ok_or_else(|| StoreError::Hook("unknown digest matter".into()))?
                .0,
        ),
        None => None,
    };
    for p in parties {
        let exists: bool = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM parties WHERE workspace_id = ? AND id = ?)",
            params![ws, p],
            |r| r.get(0),
        )?;
        if !exists {
            return Err(StoreError::Hook("unknown digest party".into()));
        }
    }
    if let Some(p) = packet {
        let p = crate::queue::get(tx, ws, p)?
            .ok_or_else(|| StoreError::Hook("unknown digest packet".into()))?;
        if p.conv != conv.key() {
            return Err(StoreError::Hook(
                "digest packet conversation mismatch".into(),
            ));
        }
    }
    if let Some(r) = range {
        let msgs = crate::conv::by_ids(tx, ws, conv, &[r.first_id.clone(), r.last_id.clone()])?;
        if r.conv != conv.key()
            || msgs.len() != 2
            || msgs[0].at != r.first_at
            || msgs[1].at != r.last_at
            || (r.first_at, &r.first_id) > (r.last_at, &r.last_id)
        {
            return Err(StoreError::Hook("invalid digest message range".into()));
        }
    }
    let mut added = Added {
        mem: mem.into(),
        ..Default::default()
    };
    let mut about: Vec<(String, String)> = vec![(conv.kind.clone(), conv.id.clone())];
    if let Some(m) = matter {
        about.push(("matter".into(), m));
    }
    for p in parties {
        about.push(("party".into(), p.clone()));
    }
    for (t, id) in about {
        let n = tx.execute(
            "INSERT OR IGNORE INTO mem_about (mem_id, workspace_id, object_type, object_id) VALUES (?, ?, ?, ?)",
            params![mem, ws, t, id],
        )?;
        if n > 0 {
            added.about.push((t, id));
        }
    }
    if let Some(r) = range {
        let n = tx.execute(
            "INSERT OR IGNORE INTO mem_ranges (mem_id, workspace_id, conv, first_id, last_id, first_at, last_at, packet_id)
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            params![mem, ws, r.conv, r.first_id, r.last_id, r.first_at, r.last_at, packet],
        )?;
        if n > 0 {
            added.range = Some(r.clone());
        }
    }
    tx.execute(
        "UPDATE mems SET updated = ? WHERE id = ? AND workspace_id = ?",
        params![steve_store::now(), mem, ws],
    )?;
    tx.publish(steve_store::ChangeEvent::Mm {
        id: mem.into(),
        ws: ws.into(),
    });
    Ok(added)
}

/// Can the original insertion still be undone without removing a later link's index?
/// A merge can retarget `about`, and another indexing operation can add a range relying on it.
pub fn can_unlink(c: &Connection, ws: &str, a: &Added) -> Result<bool> {
    for (kind, id) in &a.about {
        let exists: bool = c.query_row(
            "SELECT EXISTS(SELECT 1 FROM mem_about WHERE mem_id = ? AND workspace_id = ? AND object_type = ? AND object_id = ?)",
            params![a.mem, ws, kind, id], |r| r.get(0),
        )?;
        if !exists {
            return Ok(false);
        }
        if crate::conv::TYPES.contains(&kind.as_str()) {
            let conv = ConvRef::new(kind, id).key();
            let other: bool = c.query_row(
                "SELECT EXISTS(SELECT 1 FROM mem_ranges WHERE mem_id = ?1 AND workspace_id = ?2 AND conv = ?3
                    AND (?4 IS NULL OR first_id <> ?4))",
                params![a.mem, ws, conv, a.range.as_ref().filter(|r| r.conv == conv).map(|r| &r.first_id)],
                |r| r.get(0),
            )?;
            if other {
                return Ok(false);
            }
        }
    }
    if let Some(r) = &a.range {
        let unchanged: bool = c.query_row(
            "SELECT EXISTS(SELECT 1 FROM mem_ranges WHERE mem_id = ? AND workspace_id = ? AND conv = ?
                AND first_id = ? AND last_id = ? AND first_at = ? AND last_at = ?)",
            params![a.mem, ws, r.conv, r.first_id, r.last_id, r.first_at, r.last_at],
            |r| r.get(0),
        )?;
        if !unchanged {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Removes what [`link`] added, after the caller checked [`can_unlink`] in the same transaction.
pub fn unlink(tx: &Tx, ws: &str, a: &Added) -> Result<()> {
    for (t, id) in &a.about {
        tx.execute(
            "DELETE FROM mem_about WHERE mem_id = ? AND workspace_id = ? AND object_type = ? AND object_id = ?",
            params![a.mem, ws, t, id],
        )?;
    }
    if let Some(r) = &a.range {
        tx.execute(
            "DELETE FROM mem_ranges WHERE mem_id = ? AND workspace_id = ? AND conv = ? AND first_id = ?",
            params![a.mem, ws, r.conv, r.first_id],
        )?;
    }
    tx.publish(steve_store::ChangeEvent::Mm {
        id: a.mem.clone(),
        ws: ws.into(),
    });
    Ok(())
}

/// Structured index returned beside a memory's text; caller must apply memory visibility first.
pub fn index(c: &Connection, ws: &str, mem: &str) -> Result<Value> {
    let about: Vec<Value> = c.prepare_cached("SELECT object_type, object_id FROM mem_about WHERE workspace_id = ? AND mem_id = ? ORDER BY object_type, object_id")?
        .query_map(params![ws, mem], |r| Ok(json!({"type": r.get::<_, String>(0)?, "id": r.get::<_, String>(1)?})))?
        .collect::<rusqlite::Result<_>>()?;
    let ranges: Vec<Value> = c.prepare_cached("SELECT conv, first_id, last_id, first_at, last_at, packet_id FROM mem_ranges WHERE workspace_id = ? AND mem_id = ? ORDER BY first_at, conv, first_id")?
        .query_map(params![ws, mem], |r| Ok(json!({"conv": r.get::<_, String>(0)?, "first_id": r.get::<_, String>(1)?, "last_id": r.get::<_, String>(2)?,
            "first_at": r.get::<_, i64>(3)?, "last_at": r.get::<_, i64>(4)?, "packet_id": r.get::<_, Option<String>>(5)?})))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(json!({"about": about, "ranges": ranges}))
}
