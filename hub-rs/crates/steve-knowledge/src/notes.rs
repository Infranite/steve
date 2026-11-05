//! Notes: Markdown written in the knowledge page, stored in
//! `kb_docs.body` and indexed in the same transaction as the save, so a saved note is searchable
//! immediately.

use serde_json::json;
use steve_modules::Modules;
use steve_store::base::{self, Level, ObjRef, Visibility};
use steve_store::{Result, Tx, uid};

use crate::extract_text::markdown;
use crate::index::{self, BoxFuture, DocLoader, Loaded};
use crate::repo::{self, DocRow, NewDoc, SourceRow};
use crate::sources;
use crate::state::Guard;

/// Reads a note's text from its row (for re-indexing through the queue).
pub struct NotesLoader;

impl DocLoader for NotesLoader {
    fn kinds(&self) -> &[&str] {
        &["notes"]
    }
    fn load<'a>(
        &'a self,
        _: &'a Modules,
        doc: &'a DocRow,
        _: &'a SourceRow,
    ) -> BoxFuture<'a, Loaded> {
        let text = doc.body.clone().unwrap_or_default();
        Box::pin(async move { Loaded::Text(markdown(&text)) })
    }
}

/// A new note.
#[derive(Debug, Clone, Default)]
pub struct NewNote {
    pub title: String,
    pub body: String,
    pub visibility: Option<Visibility>,
    pub level: Option<Level>,
    pub about: Vec<ObjRef>,
}

/// (Re)indexes a note from its stored body. Front matter tags are kept on the document.
fn index_note(tx: &Tx, guard: &dyn Guard, doc_id: &str, now: i64, force: bool) -> Result<()> {
    let Some(doc) = repo::doc(tx, doc_id)? else {
        return Ok(());
    };
    let Some(src) = repo::source(tx, &doc.source_id)? else {
        return Ok(());
    };
    let ex = markdown(doc.body.as_deref().unwrap_or(""));
    if !ex.front.tags.is_empty() {
        tx.prepare_cached("UPDATE kb_docs SET tags = ? WHERE id = ?")?
            .execute(rusqlite::params![
                serde_json::to_string(&ex.front.tags)?,
                doc.id
            ])?;
    }
    index::index_doc(tx, guard, &doc, &src, &ex, now, force)?;
    Ok(())
}

/// Creates and indexes a note in `ws` written by `me`. Returns the document id.
pub fn create(
    tx: &Tx,
    guard: &dyn Guard,
    ws: &str,
    me: &str,
    n: &NewNote,
    now: i64,
) -> Result<String> {
    let src = sources::builtin(tx, ws, "notes", me, now)?;
    let id = uid("kd");
    repo::insert_doc(
        tx,
        ws,
        &NewDoc {
            id: id.clone(),
            source_id: src.id.clone(),
            r#ref: format!("note:{id}"),
            title: n.title.clone(),
            mime: "text/markdown".into(),
            body: Some(n.body.clone()),
            size: n.body.len() as i64,
            visibility: n.visibility.clone(),
            level: n.level,
            created_by: me.into(),
            ..Default::default()
        },
        now,
    )?;
    repo::set_doc_about(tx, ws, &id, &n.about)?;
    index_note(tx, guard, &id, now, true)?;
    base::audit(
        tx,
        ws,
        me,
        "kb_note.create",
        &id,
        &json!({ "title": n.title }),
    )?;
    Ok(id)
}

/// Changes title and/or body; writes a revision (`object_type=kb_note`) with both versions.
pub fn update(
    tx: &Tx,
    guard: &dyn Guard,
    doc: &DocRow,
    me: &str,
    title: Option<&str>,
    body: Option<&str>,
    now: i64,
) -> Result<()> {
    let new_title = title.unwrap_or(&doc.title);
    let old_body = doc.body.as_deref().unwrap_or("");
    let new_body = body.unwrap_or(old_body);
    if new_title == doc.title && new_body == old_body {
        return Ok(());
    }
    repo::set_doc_text(tx, &doc.id, new_title, new_body, now)?;
    let version = base::revisions(tx, &doc.workspace_id, "kb_note", &doc.id)?.len() as i64 + 1;
    base::revision(
        tx,
        &doc.workspace_id,
        "kb_note",
        &doc.id,
        version,
        &json!({ "title": doc.title, "body": old_body }),
        &json!({ "title": new_title, "body": new_body }),
        me,
    )?;
    // The title is part of every chunk's FTS row and label, so a title change re-chunks too.
    index_note(tx, guard, &doc.id, now, new_title != doc.title)?;
    base::audit(
        tx,
        &doc.workspace_id,
        me,
        "kb_note.update",
        &doc.id,
        &json!({}),
    )?;
    Ok(())
}
