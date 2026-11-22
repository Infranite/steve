//! SQL for sources, documents, marks and uses. Only plain row access here; the
//! chunk/FTS writes are in [`crate::index`], retrieval in [`crate::retrieve`].

use rusqlite::types::Value as Sql;
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde_json::{Value, json};
use steve_store::base::{Level, ObjRef, Viewer, Visibility, bind};
use steve_store::{Result, Tx};

pub use steve_store::knowledge::{DocFilter, DocRow, NewDoc, SourceRow};

fn source_row(r: &Row) -> rusqlite::Result<SourceRow> {
    Ok(SourceRow {
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

fn doc_row(r: &Row) -> rusqlite::Result<DocRow> {
    Ok(DocRow {
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

fn json_or(s: String, d: Value) -> Value {
    serde_json::from_str(&s).unwrap_or(d)
}

// ---- sources

pub fn source(c: &Connection, id: &str) -> Result<Option<SourceRow>> {
    Ok(c.prepare_cached("SELECT * FROM kb_sources WHERE id = ?")?
        .query_row([id], source_row)
        .optional()?)
}

pub fn sources(c: &Connection, ws: &str) -> Result<Vec<SourceRow>> {
    let mut st =
        c.prepare_cached("SELECT * FROM kb_sources WHERE workspace_id = ?
          ORDER BY CASE kind WHEN 'notes' THEN 0 WHEN 'files' THEN 1 WHEN 'items' THEN 2 WHEN 'chat' THEN 3
                   WHEN 'mems' THEN 4 WHEN 'bot_mems' THEN 5 ELSE 6 END, created, id")?;
    let rows = st.query_map([ws], source_row)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

// ---- documents

pub fn doc(c: &Connection, id: &str) -> Result<Option<DocRow>> {
    Ok(c.prepare_cached("SELECT * FROM kb_docs WHERE id = ?")?
        .query_row([id], doc_row)
        .optional()?)
}

pub fn doc_by_ref(c: &Connection, source_id: &str, r#ref: &str) -> Result<Option<DocRow>> {
    Ok(
        c.prepare_cached("SELECT * FROM kb_docs WHERE source_id = ? AND ref = ?")?
            .query_row([source_id, r#ref], doc_row)
            .optional()?,
    )
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
        let mut d = doc_row(r)?;
        // `visibility` above is the effective one (for the scope check); restore the own one.
        d.visibility = r
            .get::<_, Option<String>>("own_vis")?
            .map(|v| Visibility::from_db(&v));
        Ok(d)
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
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

// ---- K2: folder and file sources

/// Folder sources, of one workspace or (`None`) of all, oldest first.
pub fn folder_sources(c: &Connection, ws: Option<&str>) -> Result<Vec<SourceRow>> {
    let mut st = c.prepare_cached(
        "SELECT * FROM kb_sources WHERE kind = 'folder' AND (?1 IS NULL OR workspace_id = ?1) ORDER BY created, id",
    )?;
    let rows = st.query_map([ws], source_row)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
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
