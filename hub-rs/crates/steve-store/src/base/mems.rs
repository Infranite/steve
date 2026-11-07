//! Memories (`mems` + `mem_about`): facts, preferences, rules, summaries and events about parties,
//! items, spaces, bots, personas or the user. Every read goes through a [`Viewer`]; `never` memories are
//! left out unless a caller explicitly asks (the owner's own management page).

use crate::repo::Cached;
use rusqlite::types::Value as Sql;
use rusqlite::{Connection, Row, params};
use serde::Serialize;

use super::{Level, ObjRef, Viewer, Visibility, bind};
use crate::repo::Sets;
use crate::{ChangeEvent, Result, Tx, now, uid};

/// A `mems` row with its `about` objects.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Mem {
    pub id: String,
    pub workspace_id: String,
    /// `fact` | `preference` | `rule` | `summary` | `event`
    pub kind: String,
    pub text: String,
    /// `me` | `message` | `item` | `artifact` | `event` | `plugin` | `sync` | `steve`
    pub source_type: String,
    pub source_id: Option<String>,
    pub source_label: String,
    pub visibility: Visibility,
    pub level: Level,
    /// `candidate` | `confirmed` | `stale`
    pub status: String,
    pub valid_until: Option<i64>,
    /// `me` | `steve` | `<bot id>` | `plugin:<id>`
    pub writer: String,
    pub decision_id: Option<String>,
    pub created_by: String,
    pub created: i64,
    pub updated: i64,
    pub about: Vec<ObjRef>,
}

impl Mem {
    fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get("id")?,
            workspace_id: r.get("workspace_id")?,
            kind: r.get("kind")?,
            text: r.get("text")?,
            source_type: r.get("source_type")?,
            source_id: r.get("source_id")?,
            source_label: r.get("source_label")?,
            visibility: Visibility::from_db(&r.get::<_, String>("visibility")?),
            level: Level::from_db(&r.get::<_, String>("level")?),
            status: r.get("status")?,
            valid_until: r.get("valid_until")?,
            writer: r.get("writer")?,
            decision_id: r.get("decision_id")?,
            created_by: r.get("created_by")?,
            created: r.get("created")?,
            updated: r.get("updated")?,
            about: Vec::new(),
        })
    }
}

/// A new memory. Defaults follow the table: `private`, `conclusion`, `confirmed`.
#[derive(Debug, Clone)]
pub struct NewMem {
    pub kind: String,
    pub text: String,
    pub source_type: String,
    pub source_id: Option<String>,
    pub source_label: String,
    pub visibility: Visibility,
    pub level: Level,
    pub status: String,
    pub valid_until: Option<i64>,
    pub writer: String,
    pub decision_id: Option<String>,
    pub created_by: String,
    pub about: Vec<ObjRef>,
}

impl Default for NewMem {
    fn default() -> Self {
        Self {
            kind: "fact".into(),
            text: String::new(),
            source_type: "me".into(),
            source_id: None,
            source_label: String::new(),
            visibility: Visibility::Private,
            level: Level::Conclusion,
            status: "confirmed".into(),
            valid_until: None,
            writer: "me".into(),
            decision_id: None,
            created_by: String::new(),
            about: Vec::new(),
        }
    }
}

/// Fields a memory edit may change; `None` = unchanged.
#[derive(Debug, Clone, Default)]
pub struct MemPatch {
    pub text: Option<String>,
    pub level: Option<Level>,
    pub status: Option<String>,
    pub visibility: Option<Visibility>,
    pub decision_id: Option<Option<String>>,
    pub valid_until: Option<Option<i64>>,
}

/// Which memories a list returns.
#[derive(Debug, Clone, Default)]
pub struct MemFilter {
    /// Only these statuses; empty = all.
    pub statuses: Vec<String>,
    /// Include `never` memories (only for the owner's management views, never for prompts or MCP).
    pub include_never: bool,
    /// At most this many (0 = no limit).
    pub limit: i64,
}

impl MemFilter {
    /// Confirmed, without `never`: what goes into prompts and MCP answers.
    pub fn usable() -> Self {
        Self {
            statuses: vec!["confirmed".into()],
            ..Self::default()
        }
    }
}

fn about_of(c: &Connection, id: &str) -> Result<Vec<ObjRef>> {
    let mut st = c.prepare_cached(
        "SELECT object_type, object_id FROM mem_about WHERE mem_id = ? ORDER BY rowid",
    )?;
    let rows = st.query_map([id], |r| {
        Ok(ObjRef {
            kind: r.get(0)?,
            id: r.get(1)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

fn with_about(c: &Connection, mut m: Mem) -> Result<Mem> {
    m.about = about_of(c, &m.id)?;
    Ok(m)
}

/// Any memory by id, without a visibility check (for services that already checked, and handlers).
pub fn get(c: &Connection, id: &str) -> Result<Option<Mem>> {
    let m = c
        .prepare_cached("SELECT * FROM mems WHERE id = ?")?
        .query_row([id], Mem::from_row);
    match m {
        Ok(m) => Ok(Some(with_about(c, m)?)),
        Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
        Err(e) => Err(e.into()),
    }
}

/// The ownership check `own_mem(ws, me, id)`: exists, in the viewer's workspace, visible to them.
pub fn visible(c: &Connection, viewer: &Viewer, id: &str) -> Result<Option<Mem>> {
    Ok(get(c, id)?
        .filter(|m| m.workspace_id == viewer.ws && viewer.sees(&m.visibility, &m.created_by)))
}

/// Memories about one object visible to `viewer`, newest first.
pub fn about(c: &Connection, viewer: &Viewer, object: &ObjRef, f: &MemFilter) -> Result<Vec<Mem>> {
    let scope = viewer.sql("m");
    let mut sql = format!(
        "SELECT m.* FROM mems m JOIN mem_about a ON a.mem_id = m.id
         WHERE a.workspace_id = ? AND a.object_type = ? AND a.object_id = ? AND m.workspace_id = ? AND {}",
        scope.sql
    );
    let mut p: Vec<Sql> = vec![
        viewer.ws.clone().into(),
        object.kind.clone().into(),
        object.id.clone().into(),
        viewer.ws.clone().into(),
    ];
    p.extend(scope.params);
    filter_tail(&mut sql, &mut p, f);
    list(c, &sql, p)
}

/// All memories of the workspace visible to `viewer`, newest first.
pub fn all(c: &Connection, viewer: &Viewer, f: &MemFilter) -> Result<Vec<Mem>> {
    let scope = viewer.sql("m");
    let mut sql = format!(
        "SELECT m.* FROM mems m WHERE m.workspace_id = ? AND {}",
        scope.sql
    );
    let mut p: Vec<Sql> = vec![viewer.ws.clone().into()];
    p.extend(scope.params);
    filter_tail(&mut sql, &mut p, f);
    list(c, &sql, p)
}

fn filter_tail(sql: &mut String, p: &mut Vec<Sql>, f: &MemFilter) {
    if !f.include_never {
        sql.push_str(" AND m.level != 'never'");
    }
    if !f.statuses.is_empty() {
        sql.push_str(&format!(
            " AND m.status IN ({})",
            vec!["?"; f.statuses.len()].join(", ")
        ));
        p.extend(f.statuses.iter().cloned().map(Sql::Text));
    }
    sql.push_str(" ORDER BY m.updated DESC, m.id");
    if f.limit > 0 {
        sql.push_str(" LIMIT ?");
        p.push(Sql::Integer(f.limit));
    }
}

fn list(c: &Connection, sql: &str, p: Vec<Sql>) -> Result<Vec<Mem>> {
    let mut st = c.prepare(sql)?;
    let rows = st
        .query_map(bind(p), Mem::from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.into_iter().map(|m| with_about(c, m)).collect()
}

/// Writes a memory and its `about` rows. Publishes `{type:"mm"}`.
pub fn create(tx: &Tx, ws: &str, m: &NewMem) -> Result<Mem> {
    let id = uid("mm");
    let t = now();
    tx.prepare_cached(
        "INSERT INTO mems (id, workspace_id, kind, text, source_type, source_id, source_label, visibility, level, status, valid_until, writer, decision_id, created_by, created, updated)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )?
    .execute(params![
        id,
        ws,
        m.kind,
        m.text,
        m.source_type,
        m.source_id,
        m.source_label,
        m.visibility.code(),
        m.level.code(),
        m.status,
        m.valid_until,
        m.writer,
        m.decision_id,
        m.created_by,
        t,
        t
    ])?;
    for a in &m.about {
        tx.prepare_cached(
            "INSERT OR IGNORE INTO mem_about (mem_id, workspace_id, object_type, object_id) VALUES (?, ?, ?, ?)",
        )?
        .execute(params![id, ws, a.kind, a.id])?;
    }
    tx.publish(ChangeEvent::Mm {
        id: id.clone(),
        ws: ws.into(),
    });
    crate::repo::must(get(tx, &id)?)
}

/// Applies a patch (bumping `updated`); `None` if there is no such memory. Publishes `{type:"mm"}`.
pub fn patch(tx: &Tx, id: &str, p: &MemPatch) -> Result<Option<Mem>> {
    let Some(old) = get(tx, id)? else {
        return Ok(None);
    };
    let mut s = Sets::default();
    s.val("text", p.text.clone())
        .val("level", p.level.map(|l| l.code().to_string()))
        .val("status", p.status.clone())
        .val("visibility", p.visibility.as_ref().map(Visibility::code))
        .null("decision_id", p.decision_id.clone())
        .null("valid_until", p.valid_until);
    if !s.is_empty() {
        s.val("updated", Some(now()));
        tx.run(
            &format!("UPDATE mems SET {} WHERE id = ?", s.sql()),
            bind(s.params([Sql::Text(id.into())])),
        )?;
        tx.publish(ChangeEvent::Mm {
            id: id.into(),
            ws: old.workspace_id.clone(),
        });
    }
    get(tx, id)
}

/// Deletes a memory and its `about` rows (physically). Returns whether it existed.
pub fn delete(tx: &Tx, id: &str) -> Result<bool> {
    let Some(old) = get(tx, id)? else {
        return Ok(false);
    };
    tx.run("DELETE FROM mem_about WHERE mem_id = ?", [id])?;
    tx.run("DELETE FROM mems WHERE id = ?", [id])?;
    tx.publish(ChangeEvent::Mm {
        id: id.into(),
        ws: old.workspace_id,
    });
    Ok(true)
}

/// Deletes the memories that are about `object` and nothing else (deleting a contact); memories
/// shared with other objects stay. Returns how many were deleted (audit records only the count).
pub fn delete_only_about(tx: &Tx, ws: &str, object: &ObjRef) -> Result<usize> {
    let ids: Vec<String> = tx
        .prepare_cached(
            "SELECT a.mem_id FROM mem_about a WHERE a.workspace_id = ? AND a.object_type = ? AND a.object_id = ?
               AND NOT EXISTS (SELECT 1 FROM mem_about b WHERE b.mem_id = a.mem_id AND (b.object_type != a.object_type OR b.object_id != a.object_id))",
        )?
        .query_map(params![ws, object.kind, object.id], |r| r.get(0))?
        .collect::<rusqlite::Result<_>>()?;
    for id in &ids {
        delete(tx, id)?;
    }
    Ok(ids.len())
}

/// Moves `about` rows from one object to another (merging parties) and returns the memory ids moved, for
/// `merges.moved`. `only` limits the move to these memories (splitting a merge moves them back).
pub fn retarget_about(
    tx: &Tx,
    ws: &str,
    object_type: &str,
    from: &str,
    into: &str,
    only: Option<&[String]>,
) -> Result<Vec<String>> {
    let ids: Vec<String> = tx
        .prepare_cached(
            "SELECT mem_id FROM mem_about WHERE workspace_id = ? AND object_type = ? AND object_id = ? ORDER BY mem_id",
        )?
        .query_map(params![ws, object_type, from], |r| r.get(0))?
        .collect::<rusqlite::Result<Vec<String>>>()?
        .into_iter()
        .filter(|id| only.is_none_or(|o| o.contains(id)))
        .collect();
    for id in &ids {
        tx.run(
            "INSERT OR IGNORE INTO mem_about (mem_id, workspace_id, object_type, object_id) VALUES (?, ?, ?, ?)",
            params![id, ws, object_type, into],
        )?;
        tx.run(
            "DELETE FROM mem_about WHERE mem_id = ? AND object_type = ? AND object_id = ?",
            params![id, object_type, from],
        )?;
        tx.publish(ChangeEvent::Mm {
            id: id.clone(),
            ws: ws.into(),
        });
    }
    Ok(ids)
}

/// Candidates waiting since before `before` (made then, or restored then; the 30-day expiry job),
/// oldest first: `(id, decision_id)`.
pub fn candidates_before(
    c: &Connection,
    ws: &str,
    before: i64,
) -> Result<Vec<(String, Option<String>)>> {
    let mut st = c.prepare_cached(
        "SELECT id, decision_id FROM mems WHERE workspace_id = ? AND status = 'candidate'
           AND max(created, coalesce((SELECT k.created FROM mem_candidates k WHERE k.mem_id = mems.id), 0)) < ?
         ORDER BY created",
    )?;
    let rows = st.query_map(params![ws, before], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}
