//! Writing the retrieval index: `kb_chunks` rows and their `kb_fts` twins,
//! always in the same transaction so the FTS index never points at a missing or stale chunk.
//!
//! Two ways in: [`index_doc`] for a document whose text is at hand (notes, inline on save), and the
//! [`IndexQueue`] worker for documents whose text has to be read from elsewhere (folder files,
//! uploads: K2's [`DocLoader`]s). The worker is serial and commits every 20 documents.

use std::future::Future;
use std::pin::Pin;

use futures_util::StreamExt;
use steve_modules::Modules;
use steve_store::base::{Level, Visibility};
use steve_store::knowledge::{ChunkMeta as StoredChunkMeta, Chunks, Documents, IndexChunk};
use steve_store::{Result, Tx, W, sha};
use tokio::sync::mpsc;

use crate::chunk::{Chunk, TRUNCATED_ERROR, chunk};
use crate::extract_text::Extracted;
use crate::repo::{DocRow, SourceRow};
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
pub fn replace(w: &mut W<'_>, m: &ChunkMeta<'_>, chunks: &[Chunk]) -> Result<usize> {
    Chunks(w).remove(m.obj_type, m.obj_id)?;
    crate::openviking::changed(w, m.ws, m.obj_type, m.obj_id)?;
    let meta = StoredChunkMeta {
        ws: m.ws,
        kind: m.obj_type,
        id: m.obj_id,
        visibility: m.visibility,
        level: m.level,
        created_by: m.created_by,
        source_kind: m.source_kind,
        label: m.label,
        updated: m.updated,
    };
    let title = seg(m.title);
    for c in chunks {
        Chunks(w).append(
            &meta,
            &title,
            IndexChunk {
                seq: c.seq as i64,
                heading: &c.heading,
                text: c.text.chars().take(MAX_CHUNK_TEXT).collect(),
                start: c.start as i64,
                end: c.end as i64,
                terms: seg(&c.index_text()),
            },
        )?;
    }
    Ok(chunks.len())
}

/// Deletes the index rows and publishes the change to the mirror in the same write.
pub fn remove(w: &mut W<'_>, obj_type: &str, obj_id: &str) -> Result<usize> {
    if let Some(ws) = Chunks(w).workspace(obj_type, obj_id)? {
        crate::openviking::changed(w, &ws, obj_type, obj_id)?;
    }
    Chunks(w).remove(obj_type, obj_id)
}

pub fn rescope(
    w: &mut W<'_>,
    obj_type: &str,
    obj_id: &str,
    vis: &Visibility,
    level: Level,
) -> Result<usize> {
    let n = Chunks(w).rescope(obj_type, obj_id, vis, level)?;
    if n > 0
        && let Some(ws) = Chunks(w).workspace(obj_type, obj_id)?
    {
        crate::openviking::changed(w, &ws, obj_type, obj_id)?;
    }
    Ok(n)
}

pub fn rescope_source(w: &mut W<'_>, source: &SourceRow) -> Result<usize> {
    if source.kind == "mems" {
        return Ok(0);
    }
    if source.kind == "bot_mems" {
        let n = Chunks(w).rescope_bot_memories(
            &source.workspace_id,
            &source.visibility,
            source.level,
        )?;
        if n > 0 {
            for id in Chunks(w).object_ids(&source.workspace_id, "bot_mem")? {
                crate::openviking::changed(w, &source.workspace_id, "bot_mem", &id)?;
            }
        }
        return Ok(n);
    }
    let mut n = 0;
    for d in Documents(w).docs_of_source(&source.id)? {
        n += rescope(
            w,
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

/// Indexes text from an integration already inside the caller's transaction.
pub fn index_doc(
    tx: &Tx<'_>,
    doc: &DocRow,
    source: &SourceRow,
    extracted: &Extracted,
    now: i64,
    force: bool,
) -> Result<usize> {
    tx.write(|w| index_document(w, doc, source, extracted, now, force))
}

/// Indexes a document from its extracted text: hashed, chunked, written.
/// Unchanged text (same hash, already indexed) only gets its scope and label refreshed unless `force`.
pub fn index_document(
    tx: &mut W<'_>,
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
        let relabeled = Chunks(tx).relabel("chunk", &doc.id, &label)?;
        if rescope(tx, "chunk", &doc.id, meta.visibility, meta.level)? == 0 && relabeled {
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
    Documents(tx).set_doc_indexed(&doc.id, &hash, n as i64, "active", err, now)?;
    Ok(n)
}

/// Marks a document as not indexable: its index rows go, the status and the Chinese reason stay.
pub fn fail_doc(tx: &mut W<'_>, doc: &DocRow, status: &str, error: &str, now: i64) -> Result<()> {
    remove(tx, "chunk", &doc.id)?;
    Documents(tx).set_doc_indexed(&doc.id, "", 0, status, Some(error), now)
}

/// Removes a document and everything indexed from it.
pub fn delete_doc(tx: &mut W<'_>, doc_id: &str) -> Result<()> {
    remove(tx, "chunk", doc_id)?;
    Documents(tx).delete_doc_row(doc_id)
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

/// Connects [`Knowledge::enqueue`] during assembly and returns the worker to start after boot.
/// A second preparation returns `None`; queued documents wait until the worker starts.
pub fn worker(m: Modules, kb: Knowledge) -> Option<impl std::future::Future<Output = ()> + Send> {
    let (tx, mut rx) = mpsc::unbounded_channel::<String>();
    kb.queue.set(tx).ok()?;
    Some(async move {
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
    })
}

/// Starts the prepared index worker (`kb.index`) for standalone callers.
pub fn spawn_worker(m: Modules, kb: Knowledge) -> Option<tokio::task::JoinHandle<()>> {
    worker(m, kb).map(tokio::spawn)
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
            m.store.read(|c| {
                let mut found = Vec::new();
                for id in &ids {
                    let Some(d) = Documents(c).doc(id)? else {
                        continue;
                    };
                    if d.status == "deleted" {
                        continue;
                    }
                    if let Some(s) = Documents(c).source(&d.source_id)? {
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
    let again = blocking(move || commit_loaded_batch(&store, work, now)).await?;
    for id in again {
        kb.enqueue(&id);
    }
    Ok(())
}

fn commit_loaded_batch(
    store: &steve_store::Store,
    work: Vec<(DocRow, SourceRow, Loaded)>,
    now: i64,
) -> Result<Vec<String>> {
    store.write(|tx| {
        let mut again = Vec::new();
        for (doc, src, loaded) in &work {
            // Re-read: the document may have changed while its text was loading.
            let Some(doc) = Documents(tx).doc(&doc.id)? else {
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
                    index_document(tx, d, src, ex, now, false)?;
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
                    Documents(tx).set_doc_indexed(&doc.id, "", 0, "skipped", Some(why), now)?;
                }
                Loaded::Failed(why) => fail_doc(tx, &doc, "failed", why, now)?,
                Loaded::Gone => {
                    remove(tx, "chunk", &doc.id)?;
                    Documents(tx).set_doc_indexed(&doc.id, "", 0, "deleted", None, now)?;
                }
                Loaded::Retry { now: at_once } => {
                    // the next scan takes it for changed, whatever it finds
                    Documents(tx).forget_seen(&doc.id)?;
                    if *at_once {
                        again.push(doc.id.clone());
                    }
                    continue;
                }
            }
            crate::state::emit_change(tx, &doc.workspace_id, &doc.id);
        }
        Ok(again)
    })
}
