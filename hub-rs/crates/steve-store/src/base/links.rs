//! Generic semantic links (`links`, migration 1) inside a transaction: the relations
//! (`works_at`, `reports_to`, `raised_by`, `related`, …) and the other modules' `from` links.
//! A link is unique per (workspace, subject, predicate, object).

use crate::repo::Cached;
use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use super::ObjRef;
use crate::repo::to_json;
use crate::{Link, Result, Tx, now};

/// The primary key of a link, recorded in `merges.moved` so a split can move it back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LinkKey {
    pub subject: ObjRef,
    pub predicate: String,
    pub object: ObjRef,
}

/// A link to write. `confidence < 1` marks an inference (“Steve 推断”).
#[derive(Debug, Clone)]
pub struct NewLink {
    pub subject: ObjRef,
    pub predicate: String,
    pub object: ObjRef,
    pub props: Value,
    pub source: String,
    pub confidence: f64,
}

impl NewLink {
    pub fn new(subject: ObjRef, predicate: &str, object: ObjRef, source: &str) -> Self {
        Self {
            subject,
            predicate: predicate.into(),
            object,
            props: Value::Object(Map::new()),
            source: source.into(),
            confidence: 1.0,
        }
    }
}

/// Inserts unless the same link exists; returns whether a row was inserted.
pub fn add(tx: &Tx, ws: &str, l: &NewLink) -> Result<bool> {
    Ok(tx.prepare_cached(
        "INSERT OR IGNORE INTO links (workspace_id, subject_type, subject_id, predicate, object_type, object_id, props, source, confidence, created) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )?
    .execute(params![
        ws,
        l.subject.kind,
        l.subject.id,
        l.predicate,
        l.object.kind,
        l.object.id,
        to_json(&l.props)?,
        l.source,
        l.confidence,
        now()
    ])? > 0)
}

/// One link by key.
pub fn get(c: &Connection, ws: &str, k: &LinkKey) -> Result<Option<Link>> {
    let r = c
        .prepare_cached(
            "SELECT * FROM links WHERE workspace_id = ? AND subject_type = ? AND subject_id = ? AND predicate = ? AND object_type = ? AND object_id = ?",
        )?
        .query_row(
            params![ws, k.subject.kind, k.subject.id, k.predicate, k.object.kind, k.object.id],
            Link::from_row,
        );
    match r {
        Ok(l) => Ok(Some(l)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// Replaces the props of a link; returns whether it exists.
pub fn set_props(tx: &Tx, ws: &str, k: &LinkKey, props: &Value) -> Result<bool> {
    Ok(tx.run(
        "UPDATE links SET props = ? WHERE workspace_id = ? AND subject_type = ? AND subject_id = ? AND predicate = ? AND object_type = ? AND object_id = ?",
        params![to_json(props)?, ws, k.subject.kind, k.subject.id, k.predicate, k.object.kind, k.object.id],
    )? > 0)
}

/// Deletes a link; returns whether it existed.
pub fn remove(tx: &Tx, ws: &str, k: &LinkKey) -> Result<bool> {
    Ok(tx.run(
        "DELETE FROM links WHERE workspace_id = ? AND subject_type = ? AND subject_id = ? AND predicate = ? AND object_type = ? AND object_id = ?",
        params![ws, k.subject.kind, k.subject.id, k.predicate, k.object.kind, k.object.id],
    )? > 0)
}

/// Links whose subject is `subject`, optionally with one predicate.
pub fn from(
    c: &Connection,
    ws: &str,
    subject: &ObjRef,
    predicate: Option<&str>,
) -> Result<Vec<Link>> {
    let mut st = c.prepare_cached(
        "SELECT * FROM links WHERE workspace_id = ? AND subject_type = ? AND subject_id = ? AND (?4 IS NULL OR predicate = ?4) ORDER BY created, predicate, object_type, object_id",
    )?;
    let rows = st.query_map(
        params![ws, subject.kind, subject.id, predicate],
        Link::from_row,
    )?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Links whose object is `object` (uses the `links_object` index), optionally with one predicate.
pub fn to(c: &Connection, ws: &str, object: &ObjRef, predicate: Option<&str>) -> Result<Vec<Link>> {
    let mut st = c.prepare_cached(
        "SELECT * FROM links WHERE workspace_id = ? AND object_type = ? AND object_id = ? AND (?4 IS NULL OR predicate = ?4) ORDER BY created, predicate, subject_type, subject_id",
    )?;
    let rows = st.query_map(
        params![ws, object.kind, object.id, predicate],
        Link::from_row,
    )?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// A link moved by [`retarget`]: its key before and after. Stored in `merges.moved` so [`restore`] can
/// undo exactly this move on a split.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MovedLink {
    pub before: LinkKey,
    pub after: LinkKey,
}

fn insert_as(tx: &Tx, ws: &str, k: &LinkKey, l: &Link) -> Result<bool> {
    Ok(tx.run(
        "INSERT OR IGNORE INTO links (workspace_id, subject_type, subject_id, predicate, object_type, object_id, props, source, confidence, created) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![ws, k.subject.kind, k.subject.id, k.predicate, k.object.kind, k.object.id, to_json(&l.props)?, l.source, l.confidence, l.created],
    )? > 0)
}

/// Moves every link where `from_ref` is either end over to `into_ref` (merging): insert-or-ignore
/// the rewritten row (props, source, confidence and time kept), then delete the old one. Links that
/// already existed on `into_ref` are dropped from `from_ref` and not listed (a split cannot bring them back).
pub fn retarget(tx: &Tx, ws: &str, from_ref: &ObjRef, into_ref: &ObjRef) -> Result<Vec<MovedLink>> {
    let mut rows = self::from(tx, ws, from_ref, None)?;
    rows.extend(to(tx, ws, from_ref, None)?);
    let mut moved = Vec::new();
    for l in rows {
        let before = LinkKey {
            subject: ObjRef::new(&l.subject_type, &l.subject_id),
            predicate: l.predicate.clone(),
            object: ObjRef::new(&l.object_type, &l.object_id),
        };
        if !remove(tx, ws, &before)? {
            continue; // a link between from_ref and itself, already handled from the other end
        }
        let swap = |r: &ObjRef| {
            if r == from_ref {
                into_ref.clone()
            } else {
                r.clone()
            }
        };
        let after = LinkKey {
            subject: swap(&before.subject),
            predicate: before.predicate.clone(),
            object: swap(&before.object),
        };
        if insert_as(tx, ws, &after, &l)? {
            moved.push(MovedLink { before, after });
        }
    }
    Ok(moved)
}

/// Undoes [`retarget`] for the given moves whose `after` row still exists (splitting a merge).
/// Returns how many were moved back.
pub fn restore(tx: &Tx, ws: &str, moved: &[MovedLink]) -> Result<usize> {
    let mut n = 0;
    for m in moved {
        let Some(l) = get(tx, ws, &m.after)? else {
            continue;
        };
        remove(tx, ws, &m.after)?;
        if insert_as(tx, ws, &m.before, &l)? {
            n += 1;
        }
    }
    Ok(n)
}
