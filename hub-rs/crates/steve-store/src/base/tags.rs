//! Tags on contacts, organizations, teams and spaces (`entity_tags`). The tags themselves are the
//! existing `tags` table; items keep using `item_tags`.

use crate::repo::Cached;
use rusqlite::{Connection, params};

use super::ObjRef;
use crate::{Result, Tx, now};

/// Tag ids on one object, in the order they were added.
pub fn of(c: &Connection, ws: &str, object: &ObjRef) -> Result<Vec<String>> {
    let mut st = c.prepare_cached(
        "SELECT tag_id FROM entity_tags WHERE workspace_id = ? AND object_type = ? AND object_id = ? ORDER BY created, tag_id",
    )?;
    let rows = st.query_map(params![ws, object.kind, object.id], |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Ids of the objects of `object_type` carrying `tag_id` (list filters like `?tag=`).
pub fn objects(c: &Connection, ws: &str, tag_id: &str, object_type: &str) -> Result<Vec<String>> {
    let mut st = c.prepare_cached(
        "SELECT object_id FROM entity_tags WHERE workspace_id = ? AND tag_id = ? AND object_type = ? ORDER BY object_id",
    )?;
    let rows = st.query_map(params![ws, tag_id, object_type], |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Replaces the tags of an object. Ids that are not tags of this workspace are dropped silently
/// (“`tags` 过滤为本工作空间的标签”), and so are the item categories (items only); duplicates once. Returns the ids kept, in the given order.
/// Tags that stay keep their original `created`, so the order of [`of`] is stable across edits.
pub fn set(tx: &Tx, ws: &str, object: &ObjRef, tag_ids: &[String]) -> Result<Vec<String>> {
    let mut kept: Vec<String> = Vec::new();
    for id in tag_ids {
        if kept.contains(id) {
            continue;
        }
        let ok = tx
            .prepare_cached("SELECT 1 FROM tags WHERE id = ? AND workspace_id = ? AND cat != ?")?
            .exists(params![id, ws, crate::kinds::KIND_CAT])?;
        if ok {
            kept.push(id.clone());
        }
    }
    let old = of(tx, ws, object)?;
    for id in old.iter().filter(|id| !kept.contains(id)) {
        tx.run(
            "DELETE FROM entity_tags WHERE workspace_id = ? AND object_type = ? AND object_id = ? AND tag_id = ?",
            params![ws, object.kind, object.id, id],
        )?;
    }
    let t = now();
    for (i, id) in kept.iter().enumerate() {
        tx.run(
            "INSERT OR IGNORE INTO entity_tags (workspace_id, tag_id, object_type, object_id, created) VALUES (?, ?, ?, ?, ?)",
            params![ws, id, object.kind, object.id, t + i as i64],
        )?;
    }
    Ok(kept)
}

/// Removes every tag of an object.
pub fn clear(tx: &Tx, ws: &str, object: &ObjRef) -> Result<usize> {
    Ok(tx.run(
        "DELETE FROM entity_tags WHERE workspace_id = ? AND object_type = ? AND object_id = ?",
        params![ws, object.kind, object.id],
    )?)
}

/// Moves the tags of `from` to `into` (merging parties); returns the tag ids moved for `merges.moved`.
/// A tag `into` already has stays once. `only` limits the move (splitting moves them back).
pub fn retarget(
    tx: &Tx,
    ws: &str,
    object_type: &str,
    from: &str,
    into: &str,
    only: Option<&[String]>,
) -> Result<Vec<String>> {
    let ids: Vec<String> = of(tx, ws, &ObjRef::new(object_type, from))?
        .into_iter()
        .filter(|id| only.is_none_or(|o| o.contains(id)))
        .collect();
    for id in &ids {
        tx.run(
            "INSERT OR IGNORE INTO entity_tags (workspace_id, tag_id, object_type, object_id, created) VALUES (?, ?, ?, ?, ?)",
            params![ws, id, object_type, into, now()],
        )?;
        tx.run(
            "DELETE FROM entity_tags WHERE workspace_id = ? AND object_type = ? AND object_id = ? AND tag_id = ?",
            params![ws, object_type, from, id],
        )?;
    }
    Ok(ids)
}
