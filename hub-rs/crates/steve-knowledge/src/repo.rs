//! SQL for sources, documents, marks and uses. Only plain row access here; the
//! chunk/FTS writes are in [`crate::index`], retrieval in [`crate::retrieve`].

use rusqlite::types::Value as Sql;
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde_json::{Value, json};
use steve_store::base::{Level, ObjRef, Viewer, Visibility, bind};
use steve_store::{Result, Tx};

/// A `kb_sources` row.
#[derive(Debug, Clone, PartialEq)]
pub struct SourceRow {
    pub id: String,
    pub workspace_id: String,
    pub kind: String,
    pub name: String,
    pub config: Value,
    pub visibility: Visibility,
    pub level: Level,
    pub paused: bool,
    pub status: String,
    pub error: Option<String>,
    pub stats: Value,
    pub last_sync: Option<i64>,
    pub revision: i64,
    pub created_by: String,
    pub created: i64,
    pub updated: i64,
}

impl SourceRow {
    fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get("id")?,
            workspace_id: r.get("workspace_id")?,
            kind: r.get("kind")?,
            name: r.get("name")?,
            config: json_or(r.get("config")?, json!({})),
            visibility: Visibility::from_db(&r.get::<_, String>("visibility")?),
            level: Level::from_db(&r.get::<_, String>("level")?),
            paused: r.get::<_, i64>("paused")? != 0,
            status: r.get("status")?,
            error: r.get("error")?,
            stats: json_or(r.get("stats")?, json!({})),
            last_sync: r.get("last_sync")?,
            revision: r.get("revision")?,
            created_by: r.get("created_by")?,
            created: r.get("created")?,
            updated: r.get("updated")?,
        })
    }
}

/// A `kb_docs` row.
#[derive(Debug, Clone, PartialEq)]
pub struct DocRow {
    pub id: String,
    pub workspace_id: String,
    pub source_id: String,
    pub r#ref: String,
    pub title: String,
    pub mime: String,
    pub body: Option<String>,
    pub file_id: Option<String>,
    pub sha256: String,
    pub size: i64,
    pub mtime: Option<i64>,
    /// `None` = follows the source.
    pub visibility: Option<Visibility>,
    pub level: Option<Level>,
    pub tags: Vec<String>,
    pub status: String,
    pub error: Option<String>,
    pub chunks: i64,
    pub created_by: String,
    pub created: i64,
    pub updated: i64,
    pub indexed: Option<i64>,
}

impl DocRow {
    fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            id: r.get("id")?,
            workspace_id: r.get("workspace_id")?,
            source_id: r.get("source_id")?,
            r#ref: r.get("ref")?,
            title: r.get("title")?,
            mime: r.get("mime")?,
            body: r.get("body")?,
            file_id: r.get("file_id")?,
            sha256: r.get("sha256")?,
            size: r.get("size")?,
            mtime: r.get("mtime")?,
            visibility: r
                .get::<_, Option<String>>("visibility")?
                .map(|v| Visibility::from_db(&v)),
            level: r
                .get::<_, Option<String>>("level")?
                .map(|v| Level::from_db(&v)),
            tags: serde_json::from_str(&r.get::<_, String>("tags")?).unwrap_or_default(),
            status: r.get("status")?,
            error: r.get("error")?,
            chunks: r.get("chunks")?,
            created_by: r.get("created_by")?,
            created: r.get("created")?,
            updated: r.get("updated")?,
            indexed: r.get("indexed")?,
        })
    }

    /// Effective visibility: the document's own, else the source's.
    pub fn eff_visibility(&self, s: &SourceRow) -> Visibility {
        self.visibility
            .clone()
            .unwrap_or_else(|| s.visibility.clone())
    }

    pub fn eff_level(&self, s: &SourceRow) -> Level {
        self.level.unwrap_or(s.level)
    }
}

fn json_or(s: String, d: Value) -> Value {
    serde_json::from_str(&s).unwrap_or(d)
}

// ---- sources

pub fn source(c: &Connection, id: &str) -> Result<Option<SourceRow>> {
    Ok(c.prepare_cached("SELECT * FROM kb_sources WHERE id = ?")?
        .query_row([id], SourceRow::from_row)
        .optional()?)
}

/// The first source of a kind (the built-in ones are unique per workspace).
pub fn source_of_kind(c: &Connection, ws: &str, kind: &str) -> Result<Option<SourceRow>> {
    Ok(c.prepare_cached(
        "SELECT * FROM kb_sources WHERE workspace_id = ? AND kind = ? ORDER BY created, id LIMIT 1",
    )?
    .query_row([ws, kind], SourceRow::from_row)
    .optional()?)
}

pub fn sources(c: &Connection, ws: &str) -> Result<Vec<SourceRow>> {
    let mut st =
        c.prepare_cached("SELECT * FROM kb_sources WHERE workspace_id = ?
          ORDER BY CASE kind WHEN 'notes' THEN 0 WHEN 'files' THEN 1 WHEN 'items' THEN 2 WHEN 'chat' THEN 3
                   WHEN 'mems' THEN 4 WHEN 'bot_mems' THEN 5 ELSE 6 END, created, id")?;
    let rows = st.query_map([ws], SourceRow::from_row)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

#[allow(clippy::too_many_arguments)]
pub fn insert_source(
    tx: &Tx,
    id: &str,
    ws: &str,
    kind: &str,
    name: &str,
    config: &Value,
    visibility: &Visibility,
    level: Level,
    created_by: &str,
    now: i64,
) -> Result<()> {
    tx.prepare_cached(
        "INSERT INTO kb_sources (id, workspace_id, kind, name, config, visibility, level, created_by, created, updated)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )?
    .execute(params![
        id,
        ws,
        kind,
        name,
        config.to_string(),
        visibility.code(),
        level.code(),
        created_by,
        now,
        now
    ])?;
    Ok(())
}

/// Live counts per source: `(docs, chunks)` of active documents, and for `mems`/`bot_mems` the
/// number of their index entries.
pub fn source_counts(c: &Connection, s: &SourceRow) -> Result<(i64, i64)> {
    let obj = match s.kind.as_str() {
        "mems" => Some("mem"),
        "bot_mems" => Some("bot_mem"),
        _ => None,
    };
    if let Some(t) = obj {
        let n: i64 = c
            .prepare_cached(
                "SELECT COUNT(*) FROM kb_chunks WHERE workspace_id = ? AND obj_type = ?",
            )?
            .query_row([&s.workspace_id, t], |r| r.get(0))?;
        return Ok((n, n));
    }
    Ok(c.prepare_cached(
        "SELECT COUNT(*), COALESCE(SUM(chunks), 0) FROM kb_docs WHERE source_id = ? AND status = 'active'",
    )?
    .query_row([&s.id], |r| Ok((r.get(0)?, r.get(1)?)))?)
}

/// Ids of the paused sources of a workspace, and their kinds.
pub fn paused(c: &Connection, ws: &str) -> Result<Vec<(String, String)>> {
    let mut st =
        c.prepare_cached("SELECT id, kind FROM kb_sources WHERE workspace_id = ? AND paused = 1")?;
    let rows = st.query_map([ws], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

// ---- documents

pub fn doc(c: &Connection, id: &str) -> Result<Option<DocRow>> {
    Ok(c.prepare_cached("SELECT * FROM kb_docs WHERE id = ?")?
        .query_row([id], DocRow::from_row)
        .optional()?)
}

pub fn doc_by_ref(c: &Connection, source_id: &str, r#ref: &str) -> Result<Option<DocRow>> {
    Ok(
        c.prepare_cached("SELECT * FROM kb_docs WHERE source_id = ? AND ref = ?")?
            .query_row([source_id, r#ref], DocRow::from_row)
            .optional()?,
    )
}

/// A new document (status `active`, not yet indexed).
#[derive(Debug, Clone, Default)]
pub struct NewDoc {
    pub id: String,
    pub source_id: String,
    pub r#ref: String,
    pub title: String,
    pub mime: String,
    pub body: Option<String>,
    pub file_id: Option<String>,
    pub size: i64,
    pub mtime: Option<i64>,
    pub visibility: Option<Visibility>,
    pub level: Option<Level>,
    pub tags: Vec<String>,
    pub created_by: String,
}

pub fn insert_doc(tx: &Tx, ws: &str, d: &NewDoc, now: i64) -> Result<()> {
    tx.prepare_cached(
        "INSERT INTO kb_docs (id, workspace_id, source_id, ref, title, mime, body, file_id, size, mtime, visibility, level, tags, created_by, created, updated)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )?
    .execute(params![
        d.id,
        ws,
        d.source_id,
        d.r#ref,
        d.title,
        if d.mime.is_empty() { "text/markdown" } else { &d.mime },
        d.body,
        d.file_id,
        d.size,
        d.mtime,
        d.visibility.as_ref().map(Visibility::code),
        d.level.map(Level::code),
        serde_json::to_string(&d.tags)?,
        d.created_by,
        now,
        now
    ])?;
    Ok(())
}

pub fn set_doc_text(tx: &Tx, id: &str, title: &str, body: &str, now: i64) -> Result<()> {
    tx.prepare_cached(
        "UPDATE kb_docs SET title = ?, body = ?, size = ?, updated = ? WHERE id = ?",
    )?
    .execute(params![title, body, body.len() as i64, now, id])?;
    Ok(())
}

pub fn set_doc_scope(
    tx: &Tx,
    id: &str,
    visibility: Option<&Visibility>,
    level: Option<Level>,
    now: i64,
) -> Result<()> {
    tx.prepare_cached("UPDATE kb_docs SET visibility = ?, level = ?, updated = ? WHERE id = ?")?
        .execute(params![
            visibility.map(Visibility::code),
            level.map(Level::code),
            now,
            id
        ])?;
    Ok(())
}

/// After indexing: hash, chunk count, status and error.
pub fn set_doc_indexed(
    tx: &Tx,
    id: &str,
    sha256: &str,
    chunks: i64,
    status: &str,
    error: Option<&str>,
    now: i64,
) -> Result<()> {
    tx.prepare_cached(
        "UPDATE kb_docs SET sha256 = ?, chunks = ?, status = ?, error = ?, indexed = ? WHERE id = ?",
    )?
    .execute(params![sha256, chunks, status, error, now, id])?;
    Ok(())
}

pub fn delete_doc_row(tx: &Tx, id: &str) -> Result<()> {
    tx.execute("DELETE FROM kb_doc_about WHERE doc_id = ?", [id])?;
    tx.execute("DELETE FROM kb_docs WHERE id = ?", [id])?;
    Ok(())
}

pub fn doc_about(c: &Connection, id: &str) -> Result<Vec<ObjRef>> {
    let mut st = c.prepare_cached(
        "SELECT object_type, object_id FROM kb_doc_about WHERE doc_id = ? ORDER BY rowid",
    )?;
    let rows = st.query_map([id], |r| {
        Ok(ObjRef {
            kind: r.get(0)?,
            id: r.get(1)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn set_doc_about(tx: &Tx, ws: &str, id: &str, about: &[ObjRef]) -> Result<()> {
    tx.execute("DELETE FROM kb_doc_about WHERE doc_id = ?", [id])?;
    let mut st = tx.prepare_cached(
        "INSERT OR IGNORE INTO kb_doc_about (doc_id, workspace_id, object_type, object_id) VALUES (?, ?, ?, ?)",
    )?;
    for a in about {
        st.execute(params![id, ws, a.kind, a.id])?;
    }
    Ok(())
}

/// Filters of the document list.
#[derive(Debug, Clone, Default)]
pub struct DocFilter {
    pub source: Option<String>,
    /// Matches title and ref (path), case-insensitive substring.
    pub q: Option<String>,
    /// Only documents updated before this (paging, newest first).
    pub before: Option<i64>,
    pub limit: i64,
}

/// Documents the viewer can see (effective visibility), newest first; deleted ones left out.
pub fn docs_visible(c: &Connection, viewer: &Viewer, f: &DocFilter) -> Result<Vec<DocRow>> {
    let scope = viewer.sql("v");
    let mut sql = format!(
        "WITH v AS (SELECT d.*, COALESCE(d.visibility, s.visibility) AS eff_vis FROM kb_docs d JOIN kb_sources s ON s.id = d.source_id WHERE d.workspace_id = ? AND d.status <> 'deleted')
         SELECT v.* FROM (SELECT id, workspace_id, source_id, ref, title, mime, body, file_id, sha256, size, mtime, eff_vis AS visibility, level, tags, status, error, chunks, created_by, created, updated, indexed, visibility AS own_vis FROM v) v WHERE {}",
        scope.sql
    );
    let mut p: Vec<Sql> = vec![Sql::Text(viewer.ws.clone())];
    p.extend(scope.params);
    if let Some(s) = &f.source {
        sql.push_str(" AND v.source_id = ?");
        p.push(Sql::Text(s.clone()));
    }
    if let Some(q) = f.q.as_deref().filter(|q| !q.is_empty()) {
        sql.push_str(
            " AND (instr(lower(v.title), lower(?)) > 0 OR instr(lower(v.ref), lower(?)) > 0)",
        );
        p.push(Sql::Text(q.into()));
        p.push(Sql::Text(q.into()));
    }
    if let Some(b) = f.before {
        sql.push_str(" AND v.updated < ?");
        p.push(Sql::Integer(b));
    }
    sql.push_str(" ORDER BY v.updated DESC, v.id DESC LIMIT ?");
    p.push(Sql::Integer(if f.limit > 0 { f.limit } else { 50 }));
    let mut st = c.prepare(&sql)?;
    let rows = st.query_map(bind(p), |r| {
        let mut d = DocRow::from_row(r)?;
        // `visibility` above is the effective one (for the scope check); restore the own one.
        d.visibility = r
            .get::<_, Option<String>>("own_vis")?
            .map(|v| Visibility::from_db(&v));
        Ok(d)
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// `(seq, heading, text)` of a document's first `limit` chunks.
pub fn doc_chunks(c: &Connection, doc_id: &str, limit: i64) -> Result<Vec<(i64, String, String)>> {
    let mut st = c.prepare_cached(
        "SELECT seq, heading, text FROM kb_chunks WHERE obj_type = 'chunk' AND obj_id = ? ORDER BY seq LIMIT ?",
    )?;
    let rows = st.query_map(params![doc_id, limit], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?))
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// How many `kb_uses` rows cite this object.
pub fn uses_of(c: &Connection, ws: &str, obj_id: &str) -> Result<i64> {
    let pat = format!("%\"obj_id\":{}%", serde_json::to_string(obj_id)?);
    Ok(
        c.prepare_cached("SELECT COUNT(*) FROM kb_uses WHERE workspace_id = ? AND hits LIKE ?")?
            .query_row(params![ws, pat], |r| r.get(0))?,
    )
}

// ---- marks

pub fn mark(c: &Connection, ws: &str, name: &str) -> Result<Option<String>> {
    Ok(
        c.prepare_cached("SELECT value FROM kb_marks WHERE workspace_id = ? AND name = ?")?
            .query_row([ws, name], |r| r.get(0))
            .optional()?,
    )
}

pub fn set_mark(tx: &Tx, ws: &str, name: &str, value: &str, now: i64) -> Result<()> {
    tx.prepare_cached(
        "INSERT INTO kb_marks (workspace_id, name, value, updated) VALUES (?, ?, ?, ?)
         ON CONFLICT (workspace_id, name) DO UPDATE SET value = excluded.value, updated = excluded.updated",
    )?
    .execute(params![ws, name, value, now])?;
    Ok(())
}

/// Members of a workspace.
pub fn member_count(c: &Connection, ws: &str) -> Result<i64> {
    Ok(
        c.prepare_cached("SELECT COUNT(*) FROM workspace_members WHERE workspace_id = ?")?
            .query_row([ws], |r| r.get(0))?,
    )
}

/// Does `r` name an object of this workspace the user may refer to? Used for `about` lists.
pub fn ref_exists(c: &Connection, ws: &str, me: &str, r: &ObjRef) -> Result<bool> {
    ref_exists_for(c, &Viewer::load(c, ws, me)?, r)
}

/// Like `ref_exists`, with the caller's registered source scopes as well as team scopes.
pub fn ref_exists_for(c: &Connection, viewer: &Viewer, r: &ObjRef) -> Result<bool> {
    if r.kind == "mail_thread" {
        return Ok(mail_thread_name(c, viewer, &r.id)?.is_some());
    }
    let sql = match r.kind.as_str() {
        "me" => return Ok(r.id == viewer.user || r.id == "me"),
        "party" => "SELECT 1 FROM parties WHERE id = ?1 AND workspace_id = ?2",
        "item" => "SELECT 1 FROM items WHERE id = ?1 AND workspace_id = ?2",
        "bot" => "SELECT 1 FROM bot_workspaces WHERE bot_id = ?1 AND workspace_id = ?2",
        "project" => "SELECT 1 FROM projects WHERE id = ?1 AND workspace_id = ?2",
        "space" => "SELECT 1 FROM spaces WHERE id = ?1 AND workspace_id = ?2",
        "matter" => "SELECT 1 FROM matters WHERE id = ?1 AND workspace_id = ?2",
        "persona" => "SELECT 1 FROM personas WHERE id = ?1 AND workspace_id = ?2",
        _ => return Ok(false),
    };
    Ok(c.prepare_cached(sql)?
        .query_row(params![r.id, viewer.ws], |_| Ok(()))
        .optional()?
        .is_some())
}

fn mail_thread_name(c: &Connection, viewer: &Viewer, id: &str) -> Result<Option<String>> {
    let row: Option<(String, String, String)> = c
        .prepare_cached("SELECT subject, visibility, created_by FROM mail_threads WHERE workspace_id = ? AND id = ?")?
        .query_row(params![viewer.ws, id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))
        .optional()?;
    Ok(row.and_then(|(name, visibility, by)| {
        viewer
            .sees(&Visibility::from_db(&visibility), &by)
            .then_some(name)
    }))
}

/// Display a reference for this viewer, including historical/shared `about` rows.
pub fn ref_name_for(c: &Connection, viewer: &Viewer, r: &ObjRef) -> Result<Option<String>> {
    if r.kind == "mail_thread" {
        return mail_thread_name(c, viewer, &r.id);
    }
    if !ref_exists_for(c, viewer, r)? {
        return Ok(None);
    }
    ref_name(c, r)
}

/// A display name without a viewer. Source-private names are deliberately omitted: callers such as
/// the shared memory index must not persist a mail subject in a more widely visible memory's text.
pub fn ref_name(c: &Connection, r: &ObjRef) -> Result<Option<String>> {
    let sql = match r.kind.as_str() {
        "me" => return Ok(Some("我".into())),
        "party" => "SELECT name FROM parties WHERE id = ?",
        "item" => "SELECT title FROM items WHERE id = ?",
        "bot" => "SELECT name FROM bots WHERE id = ?",
        "project" => "SELECT name FROM projects WHERE id = ?",
        "space" => "SELECT name FROM spaces WHERE id = ?",
        "matter" => "SELECT title FROM matters WHERE id = ?",
        "persona" => "SELECT name FROM personas WHERE id = ?",
        _ => return Ok(None),
    };
    Ok(c.prepare_cached(sql)
        .and_then(|mut st| st.query_row([&r.id], |row| row.get(0)).optional())
        .unwrap_or(None))
}

// ---- K2: folder and file sources

/// Folder sources, of one workspace or (`None`) of all, oldest first.
pub fn folder_sources(c: &Connection, ws: Option<&str>) -> Result<Vec<SourceRow>> {
    let mut st = c.prepare_cached(
        "SELECT * FROM kb_sources WHERE kind = 'folder' AND (?1 IS NULL OR workspace_id = ?1) ORDER BY created, id",
    )?;
    let rows = st.query_map([ws], SourceRow::from_row)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Writes the editable fields of a source (name, config, scope, paused, revision).
pub fn update_source(tx: &Tx, s: &SourceRow, now: i64) -> Result<()> {
    tx.prepare_cached(
        "UPDATE kb_sources SET name = ?, config = ?, visibility = ?, level = ?, paused = ?, revision = ?, updated = ? WHERE id = ?",
    )?
    .execute(params![
        s.name,
        s.config.to_string(),
        s.visibility.code(),
        s.level.code(),
        s.paused as i64,
        s.revision,
        now,
        s.id
    ])?;
    Ok(())
}

/// Marks a folder source as syncing unless it already is, is paused or is gone. `true` = claimed.
pub fn claim_sync(tx: &Tx, id: &str, now: i64) -> Result<bool> {
    Ok(tx
        .prepare_cached(
            "UPDATE kb_sources SET status = 'syncing', updated = ? WHERE id = ? AND status <> 'syncing' AND paused = 0",
        )?
        .execute(params![now, id])?
        == 1)
}

/// Ends a sync: status, error, and on success the stats and `last_sync`.
pub fn finish_sync(
    tx: &Tx,
    id: &str,
    status: &str,
    error: Option<&str>,
    stats: Option<&Value>,
    now: i64,
) -> Result<()> {
    match stats {
        Some(st) => tx
            .prepare_cached(
                "UPDATE kb_sources SET status = ?, error = ?, stats = ?, last_sync = ?, updated = ? WHERE id = ?",
            )?
            .execute(params![status, error, st.to_string(), now, now, id])?,
        None => tx
            .prepare_cached(
                "UPDATE kb_sources SET status = ?, error = ?, updated = ? WHERE id = ?",
            )?
            .execute(params![status, error, now, id])?,
    };
    Ok(())
}

/// After a restart nothing is syncing any more.
pub fn reset_syncing(tx: &Tx) -> Result<usize> {
    Ok(tx
        .prepare_cached("UPDATE kb_sources SET status = 'ok' WHERE status = 'syncing'")?
        .execute([])?)
}

/// Deletes a source row and its marks (its documents must be gone already).
pub fn delete_source_row(tx: &Tx, id: &str) -> Result<()> {
    tx.execute("DELETE FROM kb_sources WHERE id = ?", [id])?;
    Ok(())
}

/// Every document of a source, deleted ones included.
pub fn docs_of_source(c: &Connection, source_id: &str) -> Result<Vec<DocRow>> {
    let mut st = c.prepare_cached("SELECT * FROM kb_docs WHERE source_id = ? ORDER BY ref")?;
    let rows = st.query_map([source_id], DocRow::from_row)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// A file seen again by the scanner: new size and mtime, back to `active` (the index decides the rest).
pub fn set_doc_seen(tx: &Tx, id: &str, size: i64, mtime: i64, now: i64) -> Result<()> {
    tx.prepare_cached(
        "UPDATE kb_docs SET size = ?, mtime = ?, status = 'active', error = NULL, updated = ? WHERE id = ?",
    )?
    .execute(params![size, mtime, now, id])?;
    Ok(())
}

/// Forgets when a document's file was last seen (`mtime`), so the next scan reads it again.
pub fn forget_seen(tx: &Tx, id: &str) -> Result<()> {
    tx.prepare_cached("UPDATE kb_docs SET mtime = NULL WHERE id = ?")?
        .execute([id])?;
    Ok(())
}

/// What a document's front matter set (title, tags, own scope).
pub fn set_doc_front(
    tx: &Tx,
    id: &str,
    title: &str,
    tags: &[String],
    visibility: Option<&Visibility>,
    level: Option<Level>,
    now: i64,
) -> Result<()> {
    tx.prepare_cached(
        "UPDATE kb_docs SET title = ?, tags = ?, visibility = ?, level = ?, updated = ? WHERE id = ?",
    )?
    .execute(params![
        title,
        serde_json::to_string(tags)?,
        visibility.map(Visibility::code),
        level.map(Level::code),
        now,
        id
    ])?;
    Ok(())
}

pub fn delete_mark(tx: &Tx, ws: &str, name: &str) -> Result<()> {
    tx.prepare_cached("DELETE FROM kb_marks WHERE workspace_id = ? AND name = ?")?
        .execute([ws, name])?;
    Ok(())
}

/// An uploaded file (`files`) of the workspace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileInfo {
    pub id: String,
    pub workspace_id: String,
    pub sha256: String,
    pub name: String,
    pub mime: String,
    pub size: i64,
}

pub fn file_in_ws(c: &Connection, ws: &str, id: &str) -> Result<Option<FileInfo>> {
    Ok(c.prepare_cached(
        "SELECT id, workspace_id, sha256, name, mime, size FROM files WHERE id = ? AND workspace_id = ?",
    )?
    .query_row([id, ws], |r| {
        Ok(FileInfo {
            id: r.get(0)?,
            workspace_id: r.get(1)?,
            sha256: r.get(2)?,
            name: r.get(3)?,
            mime: r.get(4)?,
            size: r.get(5)?,
        })
    })
    .optional()?)
}

/// Does a `team:`/`space:` visibility name an object of this workspace? (`workspace`/`private`: yes.)
pub fn scope_exists(c: &Connection, ws: &str, v: &Visibility) -> Result<bool> {
    let (sql, id) = match v {
        Visibility::Workspace | Visibility::Private => return Ok(true),
        Visibility::Team(id) => (
            "SELECT 1 FROM team_details WHERE party_id = ? AND workspace_id = ?",
            id,
        ),
        Visibility::Space(id) => ("SELECT 1 FROM spaces WHERE id = ? AND workspace_id = ?", id),
    };
    Ok(c.prepare_cached(sql)?
        .query_row(params![id, ws], |_| Ok(()))
        .optional()?
        .is_some())
}
