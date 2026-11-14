//! Writing the retrieval index: `kb_chunks` rows and their `kb_fts` twins,
//! always in the same transaction so the FTS index never points at a missing or stale chunk.
//!
//! Two ways in: [`index_doc`] for a document whose text is at hand (notes, inline on save), and the
//! [`IndexQueue`] worker for documents whose text has to be read from elsewhere (folder files,
//! uploads: K2's [`DocLoader`]s). The worker is serial and commits every 20 documents.

use std::future::Future;
use std::pin::Pin;

use futures_util::StreamExt;
use rusqlite::params;
use steve_modules::Modules;
use steve_store::base::{Level, Visibility};
use steve_store::{Result, Tx, sha};
use tokio::sync::mpsc;

use crate::chunk::{Chunk, TRUNCATED_ERROR, chunk};
use crate::extract_text::Extracted;
use crate::repo::{self, DocRow, SourceRow};
use crate::seg::seg;
use crate::state::Knowledge;

/// Documents per transaction in the queue worker.
pub const BATCH: usize = 20;

/// Display text of a chunk is cut to this many characters (`kb_chunks.text`).
pub const MAX_CHUNK_TEXT: usize = 1200;

/// What every index row of one object shares.
#[derive(Debug, Clone)]
pub struct ChunkMeta<'a> {
    pub ws: &'a str,
    /// `chunk` | `mem` | `bot_mem`
    pub obj_type: &'a str,
    pub obj_id: &'a str,
    /// Indexed in the FTS `title` column (weight 2).
    pub title: &'a str,
    pub visibility: &'a Visibility,
    pub level: Level,
    pub created_by: &'a str,
    pub source_kind: &'a str,
    pub label: &'a str,
    pub updated: i64,
}

/// Replaces all index rows of one object with `chunks`. Returns how many were written.
pub fn replace(tx: &Tx, m: &ChunkMeta, chunks: &[Chunk]) -> Result<usize> {
    remove_rows(tx, m.obj_type, m.obj_id)?;
    crate::openviking::changed(tx, m.ws, m.obj_type, m.obj_id)?;
    let title = seg(m.title);
    let mut ins = tx.prepare_cached(
        "INSERT INTO kb_chunks (workspace_id, obj_type, obj_id, seq, heading, text, start_at, end_at, visibility, level, created_by, source_kind, label, updated)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )?;
    let mut fts = tx.prepare_cached("INSERT INTO kb_fts (rowid, seg, title) VALUES (?, ?, ?)")?;
    for c in chunks {
        let text: String = c.text.chars().take(MAX_CHUNK_TEXT).collect();
        ins.execute(params![
            m.ws,
            m.obj_type,
            m.obj_id,
            c.seq as i64,
            c.heading,
            text,
            c.start as i64,
            c.end as i64,
            m.visibility.code(),
            m.level.code(),
            m.created_by,
            m.source_kind,
            m.label,
            m.updated
        ])?;
        let rowid = tx.last_insert_rowid();
        fts.execute(params![rowid, seg(&c.index_text()), title])?;
    }
    Ok(chunks.len())
}

/// The workspace of an object's index rows (none: not indexed).
fn ws_of(tx: &Tx, obj_type: &str, obj_id: &str) -> Result<Option<String>> {
    use rusqlite::OptionalExtension;
    Ok(tx
        .prepare_cached(
            "SELECT workspace_id FROM kb_chunks WHERE obj_type = ? AND obj_id = ? LIMIT 1",
        )?
        .query_row([obj_type, obj_id], |r| r.get(0))
        .optional()?)
}

/// Deletes the index rows of one object; a store mirroring the index follows
/// ([`crate::openviking::changed`]).
pub fn remove(tx: &Tx, obj_type: &str, obj_id: &str) -> Result<usize> {
    if let Some(ws) = ws_of(tx, obj_type, obj_id)? {
        crate::openviking::changed(tx, &ws, obj_type, obj_id)?;
    }
    remove_rows(tx, obj_type, obj_id)
}

/// FTS entries first: `contentless_delete` deletes by rowid.
fn remove_rows(tx: &Tx, obj_type: &str, obj_id: &str) -> Result<usize> {
    tx.prepare_cached(
        "DELETE FROM kb_fts WHERE rowid IN (SELECT id FROM kb_chunks WHERE obj_type = ?1 AND obj_id = ?2)",
    )?
    .execute([obj_type, obj_id])?;
    tx.prepare_cached(
        "DELETE FROM kb_vectors WHERE chunk_id IN (SELECT id FROM kb_chunks WHERE obj_type = ?1 AND obj_id = ?2)",
    )?
    .execute([obj_type, obj_id])?;
    Ok(tx
        .prepare_cached("DELETE FROM kb_chunks WHERE obj_type = ? AND obj_id = ?")?
        .execute([obj_type, obj_id])?)
}

/// Rewrites visibility and level of an object's index rows (scope changes).
pub fn rescope(
    tx: &Tx,
    obj_type: &str,
    obj_id: &str,
    visibility: &Visibility,
    level: Level,
) -> Result<usize> {
    let n = tx
        .prepare_cached(
            "UPDATE kb_chunks SET visibility = ?1, level = ?2 WHERE obj_type = ?3 AND obj_id = ?4
               AND (visibility <> ?1 OR level <> ?2)",
        )?
        .execute(params![visibility.code(), level.code(), obj_type, obj_id])?;
    if n > 0
        && let Some(ws) = ws_of(tx, obj_type, obj_id)?
    {
        crate::openviking::changed(tx, &ws, obj_type, obj_id)?;
    }
    Ok(n)
}

/// Rewrites the scope of every document that follows `source` (after the source's visibility or
/// level changed); documents with their own setting keep it. For `mems` nothing follows
/// (memories carry their own); `bot_mems` entries all follow.
pub fn rescope_source(tx: &Tx, source: &SourceRow) -> Result<usize> {
    if source.kind == "mems" {
        return Ok(0);
    }
    if source.kind == "bot_mems" {
        let n = tx
            .prepare_cached(
                "UPDATE kb_chunks SET visibility = ?1, level = ?2 WHERE workspace_id = ?3 AND obj_type = 'bot_mem'
                   AND (visibility <> ?1 OR level <> ?2)",
            )?
            .execute(params![
                source.visibility.code(),
                source.level.code(),
                source.workspace_id
            ])?;
        let ids: Vec<String> = {
            let mut st = tx.prepare_cached(
                "SELECT DISTINCT obj_id FROM kb_chunks WHERE workspace_id = ? AND obj_type = 'bot_mem'",
            )?;
            st.query_map([&source.workspace_id], |r| r.get(0))?
                .collect::<rusqlite::Result<_>>()?
        };
        if n > 0 {
            for id in ids {
                crate::openviking::changed(tx, &source.workspace_id, "bot_mem", &id)?;
            }
        }
        return Ok(n);
    }
    let mut n = 0;
    let docs: Vec<DocRow> = {
        let mut st = tx.prepare_cached("SELECT id FROM kb_docs WHERE source_id = ?")?;
        let ids = st
            .query_map([&source.id], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        ids.iter()
            .filter_map(|id| repo::doc(tx, id).transpose())
            .collect::<Result<Vec<_>>>()?
    };
    for d in docs {
        n += rescope(
            tx,
            "chunk",
            &d.id,
            &d.eff_visibility(source),
            d.eff_level(source),
        )?;
    }
    Ok(n)
}

/// `"<prefix> · <title>"`, the citation label of a document (“笔记 · Acme 合同说明”); an item's
/// document is its 事项记录, a day of the main thread its 会话记录 (ST-41).
/// The label is stored (and read by models) in zh-CN; [`doc_label_shown`] words it for a person.
pub fn doc_label(source: &SourceRow, title: &str) -> String {
    let prefix = steve_i18n::stored(|| match source.kind.as_str() {
        "notes" => steve_i18n::t!("knowledge.doc_prefix.notes"),
        "files" => steve_i18n::t!("knowledge.doc_prefix.files"),
        "items" => steve_i18n::t!("knowledge.doc_prefix.items"),
        "chat" => steve_i18n::t!("knowledge.doc_prefix.chat"),
        _ => source.name.clone(),
    });
    format!("{prefix} · {title}")
}

const DOC_PREFIXES: &[&str] = &[
    "knowledge.doc_prefix.notes",
    "knowledge.doc_prefix.files",
    "knowledge.doc_prefix.items",
    "knowledge.doc_prefix.chat",
];

/// A stored [`doc_label`] in the request's language: its fixed prefix, and a day of the main thread as
/// its title; a source's own name and a document's own title as written.
pub fn doc_label_shown(stored: &str) -> String {
    let Some((prefix, title)) = stored.split_once(" · ") else {
        return stored.to_owned();
    };
    let prefix = steve_i18n::reword!(prefix, DOC_PREFIXES).unwrap_or_else(|| prefix.to_owned());
    format!("{prefix} · {}", crate::sources::chat_day_shown(title))
}

/// Indexes a document from its extracted text: hashed, chunked, written.
/// Unchanged text (same hash, already indexed) only gets its scope and label refreshed unless `force`.
pub fn index_doc(
    tx: &Tx,
    doc: &DocRow,
    source: &SourceRow,
    ex: &Extracted,
    now: i64,
    force: bool,
) -> Result<usize> {
    let text = &ex.text;
    let hash = sha(text);
    let label = doc_label(source, &doc.title);
    let meta = ChunkMeta {
        ws: &doc.workspace_id,
        obj_type: "chunk",
        obj_id: &doc.id,
        title: &doc.title,
        visibility: &doc.eff_visibility(source),
        level: doc.eff_level(source),
        created_by: &doc.created_by,
        source_kind: &source.kind,
        label: &label,
        updated: now,
    };
    if !force && doc.indexed.is_some() && doc.sha256 == hash && doc.status == "active" {
        let relabeled = tx
            .prepare_cached(
                "UPDATE kb_chunks SET label = ?1 WHERE obj_type = 'chunk' AND obj_id = ?2 AND label <> ?1",
            )?
            .execute(params![label, doc.id])?;
        if rescope(tx, "chunk", &doc.id, meta.visibility, meta.level)? == 0 && relabeled > 0 {
            crate::openviking::changed(tx, &doc.workspace_id, "chunk", &doc.id)?;
        }
        return Ok(doc.chunks as usize);
    }
    let mut chunked = chunk(&text, ex.kind);
    if chunked.chunks.is_empty() && !doc.title.is_empty() {
        // An empty body is still findable by its title.
        chunked.chunks.push(Chunk {
            seq: 0,
            heading: String::new(),
            text: String::new(),
            start: 0,
            end: 0,
        });
    }
    let n = replace(tx, &meta, &chunked.chunks)?;
    let err = chunked.truncated.then_some(TRUNCATED_ERROR);
    repo::set_doc_indexed(tx, &doc.id, &hash, n as i64, "active", err, now)?;
    Ok(n)
}

/// Marks a document as not indexable: its index rows go, the status and the Chinese reason stay.
pub fn fail_doc(tx: &Tx, doc: &DocRow, status: &str, error: &str, now: i64) -> Result<()> {
    remove(tx, "chunk", &doc.id)?;
    repo::set_doc_indexed(tx, &doc.id, "", 0, status, Some(error), now)
}

/// Removes a document and everything indexed from it.
pub fn delete_doc(tx: &Tx, doc_id: &str) -> Result<()> {
    remove(tx, "chunk", doc_id)?;
    repo::delete_doc_row(tx, doc_id)
}

pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// What a load produced.
#[derive(Debug)]
pub enum Loaded {
    Text(Extracted),
    /// No full text for this kind of file (`status=skipped`, findable by title).
    Skipped(String),
    /// Extraction failed (`status=failed`); the message is the Chinese reason.
    Failed(String),
    /// The document's origin is gone (`status=deleted`, index rows removed).
    Gone,
    /// Not read this time (the device holding it is offline, or the file changed while it was
    /// read): the document and its index rows stay as they are, and it is read again: `now`
    /// queues it at once, otherwise its source's next scan does.
    Retry {
        now: bool,
    },
}

/// Reads the text of documents of some source kinds. Runs on the async runtime, off the write lock.
pub trait DocLoader: Send + Sync {
    fn kinds(&self) -> &[&str];
    /// Whether this loader reads `doc` (of one of its kinds). Lets another module own some documents
    /// of a built-in source (mail threads added to `files`); the first loader that
    /// accepts wins, so such loaders are registered before the built-in one.
    fn accepts(&self, doc: &DocRow) -> bool {
        let _ = doc;
        true
    }
    fn load<'a>(
        &'a self,
        m: &'a Modules,
        doc: &'a DocRow,
        source: &'a SourceRow,
    ) -> BoxFuture<'a, Loaded>;
}

/// Starts the index worker (job `kb.index`) and connects [`Knowledge::enqueue`] to it.
/// Call once at startup; a second call returns `None`.
pub fn spawn_worker(m: Modules, kb: Knowledge) -> Option<tokio::task::JoinHandle<()>> {
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    kb.queue.set(tx).ok()?;
    Some(tokio::spawn(async move {
        while let Some(first) = rx.recv().await {
            let mut ids = vec![first];
            while ids.len() < BATCH {
                match rx.try_recv() {
                    Ok(id) if !ids.contains(&id) => ids.push(id),
                    Ok(_) => {}
                    Err(_) => break,
                }
            }
            if let Err(e) = run_batch(&m, &kb, &ids).await {
                tracing::warn!("kb.index: batch failed: {e}");
            }
        }
    }))
}

impl Knowledge {
    /// Queues a document for (re)indexing by the worker. Without a running worker this only logs.
    pub fn enqueue(&self, doc_id: &str) {
        match self.queue.get() {
            Some(q) if q.send(doc_id.to_string()).is_ok() => {}
            _ => tracing::warn!("kb.index: no worker, {doc_id} not queued"),
        }
    }
}

/// Runs synchronous store work (and the chunking inside it) on the blocking pool, so a long
/// write does not hold up a runtime worker. A panic inside carries on as a panic here.
pub(crate) async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> Result<T> + Send + 'static,
) -> Result<T> {
    tokio::task::spawn_blocking(f)
        .await
        .unwrap_or_else(|e| std::panic::resume_unwind(e.into_panic()))
}

/// How many documents of a batch load at once.
const LOAD_PARALLEL: usize = 4;

/// Loads and writes one batch: every document in one transaction, then one `kb` event each.
pub async fn run_batch(m: &Modules, kb: &Knowledge, ids: &[String]) -> Result<()> {
    let found = {
        let (m, ids) = (m.clone(), ids.to_vec());
        blocking(move || {
            m.store.reader(|c| {
                let mut found = Vec::new();
                for id in &ids {
                    let Some(d) = repo::doc(c, id)? else { continue };
                    if d.status == "deleted" {
                        continue;
                    }
                    if let Some(s) = repo::source(c, &d.source_id)? {
                        found.push((d, s));
                    }
                }
                Ok(found)
            })
        })
        .await?
    };
    // a few loads side by side (file reads, isolated extraction), in batch order
    let work: Vec<(DocRow, SourceRow, Loaded)> = futures_util::stream::iter(found)
        .map(|(doc, src)| async move {
            let loaded = match kb.loader(&src.kind, &doc) {
                Some(l) => l.load(m, &doc, &src).await,
                None => Loaded::Failed("这种来源还不能入库".into()),
            };
            (doc, src, loaded)
        })
        .buffered(LOAD_PARALLEL)
        .collect()
        .await;
    let now = m.clock.now();
    let store = m.store.clone();
    let done = blocking(move || {
        store.transaction(|tx| {
            let mut done = Vec::new();
            let mut again = Vec::new();
            for (doc, src, loaded) in &work {
                // Re-read: the document may have changed while its text was loading.
                let Some(doc) = repo::doc(tx, &doc.id)? else {
                    continue;
                };
                match loaded {
                    Loaded::Text(ex) => {
                        // A wiki page or uploaded Markdown may set its own title, tags and scope.
                        let fronted;
                        let d = if matches!(src.kind.as_str(), "folder" | "files") {
                            fronted =
                                crate::sources::folder::apply_front(tx, &doc, src, &ex.front, now)?;
                            &fronted
                        } else {
                            &doc
                        };
                        index_doc(tx, d, src, ex, now, false)?;
                    }
                    Loaded::Skipped(why) => {
                        // Title-only: one empty chunk carries the title into the index.
                        let label = doc_label(src, &doc.title);
                        let meta = ChunkMeta {
                            ws: &doc.workspace_id,
                            obj_type: "chunk",
                            obj_id: &doc.id,
                            title: &doc.title,
                            visibility: &doc.eff_visibility(src),
                            level: doc.eff_level(src),
                            created_by: &doc.created_by,
                            source_kind: &src.kind,
                            label: &label,
                            updated: now,
                        };
                        let empty = Chunk {
                            seq: 0,
                            heading: String::new(),
                            text: String::new(),
                            start: 0,
                            end: 0,
                        };
                        replace(tx, &meta, &[empty])?;
                        repo::set_doc_indexed(tx, &doc.id, "", 0, "skipped", Some(why), now)?;
                    }
                    Loaded::Failed(why) => fail_doc(tx, &doc, "failed", why, now)?,
                    Loaded::Gone => {
                        remove(tx, "chunk", &doc.id)?;
                        repo::set_doc_indexed(tx, &doc.id, "", 0, "deleted", None, now)?;
                    }
                    Loaded::Retry { now: at_once } => {
                        // the next scan takes it for changed, whatever it finds
                        repo::forget_seen(tx, &doc.id)?;
                        if *at_once {
                            again.push(doc.id.clone());
                        }
                        continue;
                    }
                }
                done.push((doc.workspace_id.clone(), doc.id.clone()));
            }
            Ok::<_, steve_store::StoreError>((done, again))
        })
    })
    .await?;
    let (done, again) = done;
    for (ws, id) in done {
        kb.notify(&ws, &id);
    }
    for id in again {
        kb.enqueue(&id);
    }
    Ok(())
}
