//! The knowledge REST operations of slice K1: search, sources (read only),
//! documents, notes and memories. Ownership first, then the store; handlers stay thin.

use std::time::Instant;

use rusqlite::Connection;
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};
use steve_modules::{ApiError, ApiResult, Caller, Check, Modules};
use steve_store::StoreError;
use steve_store::base::mems::{self, Mem, MemFilter, NewMem};
use steve_store::base::{self, Level, ObjRef, Viewer, Visibility};

use crate::index;
use crate::notes::{self, NewNote};
use crate::repo::{self, DocFilter, DocRow, SourceRow};
use crate::retrieve::{self, Purpose, Query};
use crate::sources;
use crate::state::Knowledge;
use crate::sync;

/// `400` for a search text that looks like a secret.
pub const NOUN_DOC: &str = "文档";
pub const NOUN_NOTE: &str = "笔记";

const MEM_KINDS: [&str; 5] = ["fact", "preference", "rule", "summary", "event"];

/// `null` and absent are different (`null` = back to following the source).
fn nullable<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
    d: D,
) -> Result<Option<Option<T>>, D::Error> {
    Ok(Some(Option::deserialize(d)?))
}

/// Creates the built-in sources on a workspace's first knowledge request.
pub fn prepare(m: &Modules, who: &Caller) -> ApiResult<()> {
    if m.store.reader(|c| sources::has_builtin(c, &who.ws))? {
        return Ok(());
    }
    let now = m.clock.now();
    m.store
        .transaction(|tx| sources::ensure_builtin(tx, &who.ws, &who.me, now))?;
    Ok(())
}

fn ref_noun(kind: &str) -> &'static str {
    match kind {
        "party" => "联系人",
        "item" => "事项",
        "bot" => "Bot",
        "project" => "项目",
        "space" => "空间",
        "persona" => "对外身份",
        _ => "对象",
    }
}

/// Every `about` object must exist in the workspace (`404 没有这个<名词>`).
fn check_about(c: &Connection, who: &Caller, about: &[ObjRef]) -> ApiResult<()> {
    for a in about {
        if !repo::ref_exists(c, &who.ws, &who.me, a)? {
            return Err(ApiError::not_found(ref_noun(&a.kind)));
        }
    }
    Ok(())
}

// ---- search

#[derive(Debug, Default, Deserialize)]
pub struct SearchParams {
    pub q: Option<String>,
    pub kind: Option<String>,
    pub limit: Option<String>,
}

/// `GET /kb/search` (`Purpose::Search`, viewer = me); records the use.
pub fn search(m: &Modules, kb: &Knowledge, who: &Caller, p: SearchParams) -> ApiResult<Value> {
    let started = Instant::now();
    let q = p.q.unwrap_or_default();
    let limit = match p.limit.as_deref() {
        None | Some("") => 10,
        Some(s) => s
            .parse::<usize>()
            .ok()
            .filter(|n| (1..=20).contains(n))
            .ok_or_else(|| ApiError::invalid("limit"))?,
    };
    Check::new().len("q", q.trim(), 1, 200).done()?;
    if !kb.guard.scan(&q).is_empty() {
        return Err(ApiError::rule(steve_i18n::t!(
            "knowledge.search.secret_query"
        )));
    }
    prepare(m, who)?;
    let kinds: Option<Vec<String>> = p
        .kind
        .filter(|k| !k.is_empty())
        .map(|k| k.split(',').map(|s| s.trim().to_string()).collect());
    let viewer = m.viewer(who)?;
    let now = m.clock.now();
    let found = m.store.reader(|c| {
        let kinds_ref: Option<Vec<&str>> = kinds
            .as_ref()
            .map(|v| v.iter().map(String::as_str).collect());
        let mut query = Query::new(kb.store(), &viewer, &q, Purpose::Search);
        query.k = limit;
        query.now = now;
        query.kinds = kinds_ref.as_deref();
        retrieve::retrieve_all(c, &query)
    })?;
    let hits = found.hits;
    m.store
        .transaction(|tx| retrieve::record_use(tx, &who.ws, ("search", &who.me), &q, &hits))?;
    Ok(json!({
        "hits": hits,
        "took_ms": started.elapsed().as_millis() as u64,
        "semantic_down": found.semantic_down,
    }))
}

// ---- sources

/// `GET /kb/sources`.
pub fn list_sources(m: &Modules, who: &Caller) -> ApiResult<Value> {
    prepare(m, who)?;
    Ok(m.store.reader(|c| {
        let list = repo::sources(c, &who.ws)?
            .iter()
            .map(|s| sources::source_json(c, s))
            .collect::<Result<Vec<_>, StoreError>>()?;
        Ok::<_, StoreError>(Value::Array(list))
    })?)
}

// ---- documents

/// `own_doc(ws, me, id)`: exists, not deleted, in the caller's workspace, visible to them.
pub fn own_doc(
    c: &Connection,
    viewer: &Viewer,
    id: &str,
    noun: &str,
) -> ApiResult<(DocRow, SourceRow)> {
    let found = match repo::doc(c, id)? {
        Some(d) if d.workspace_id == viewer.ws && d.status != "deleted" => {
            repo::source(c, &d.source_id)?.map(|s| (d, s))
        }
        _ => None,
    };
    match found {
        Some((d, s)) if viewer.sees(&d.eff_visibility(&s), &d.created_by) => Ok((d, s)),
        _ => Err(ApiError::not_found(noun)),
    }
}

/// `DocBrief`.
pub fn doc_brief(d: &DocRow, s: &SourceRow) -> Value {
    json!({
        "id": d.id,
        "source": {"id": s.id, "kind": s.kind, "name": s.name},
        "title": d.title,
        "ref": d.r#ref,
        "visibility": d.eff_visibility(s),
        "level": d.eff_level(s),
        "inherited": {"visibility": d.visibility.is_none(), "level": d.level.is_none()},
        "status": d.status,
        "chunks": d.chunks,
        "updated": d.updated,
    })
}

/// `Doc`: the brief plus body (notes only), about, tags, preview, uses, error.
pub fn doc_json(c: &Connection, d: &DocRow, s: &SourceRow) -> ApiResult<Value> {
    let mut v = doc_json_bare(c, d, s)?;
    add_uses(c, &mut v, &d.workspace_id, &d.id)?;
    Ok(v)
}

/// `doc_json` without `uses`. Counting uses scans the retrieval log, so the write paths build
/// this inside the transaction and add the count after commit, on a reader.
fn doc_json_bare(c: &Connection, d: &DocRow, s: &SourceRow) -> ApiResult<Value> {
    let mut v = doc_brief(d, s);
    let about = repo::doc_about(c, &d.id)?
        .into_iter()
        .map(|a| {
            let name = repo::ref_name(c, &a)?.unwrap_or_default();
            Ok(json!({"type": a.kind, "id": a.id, "name": name}))
        })
        .collect::<Result<Vec<_>, StoreError>>()?;
    let preview: Vec<Value> = repo::doc_chunks(c, &d.id, 50)?
        .into_iter()
        .map(|(seq, heading, text)| json!({"seq": seq, "heading": heading, "text": text}))
        .collect();
    let o = v
        .as_object_mut()
        .ok_or_else(|| ApiError::Internal("doc shape".into()))?;
    if s.kind == "notes" {
        o.insert("body".into(), json!(d.body.clone().unwrap_or_default()));
    }
    o.insert("about".into(), json!(about));
    o.insert("tags".into(), json!(d.tags));
    o.insert("preview".into(), json!(preview));
    o.insert("error".into(), json!(d.error));
    Ok(v)
}

fn add_uses(c: &Connection, v: &mut Value, ws: &str, id: &str) -> ApiResult<()> {
    let n = repo::uses_of(c, ws, id)?;
    if let Some(o) = v.as_object_mut() {
        o.insert("uses".into(), json!(n));
    }
    Ok(())
}

/// After a write: the doc built in the transaction, with its use count read outside it.
fn with_uses(m: &Modules, ws: &str, mut v: Value) -> ApiResult<Value> {
    let id = v
        .get("id")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    m.store.reader(|c| add_uses(c, &mut v, ws, &id))?;
    Ok(v)
}

#[derive(Debug, Default, Deserialize)]
pub struct DocsParams {
    pub source: Option<String>,
    pub q: Option<String>,
    pub before: Option<String>,
    pub limit: Option<String>,
}

/// `GET /kb/docs`.
pub fn list_docs(m: &Modules, who: &Caller, p: DocsParams) -> ApiResult<Value> {
    let limit = match p.limit.as_deref() {
        None | Some("") => 50,
        Some(s) => s
            .parse::<i64>()
            .ok()
            .filter(|n| (1..=200).contains(n))
            .ok_or_else(|| ApiError::invalid("limit"))?,
    };
    let before = match p.before.as_deref() {
        None | Some("") => None,
        Some(s) => Some(s.parse::<i64>().map_err(|_| ApiError::invalid("before"))?),
    };
    prepare(m, who)?;
    let viewer = m.viewer(who)?;
    Ok(m.store.reader(|c| {
        let f = DocFilter {
            source: p.source.filter(|s| !s.is_empty()),
            q: p.q,
            before,
            limit,
        };
        let docs = repo::docs_visible(c, &viewer, &f)?;
        let mut out = Vec::with_capacity(docs.len());
        let mut cache: Vec<SourceRow> = Vec::new();
        for d in &docs {
            if !cache.iter().any(|s| s.id == d.source_id)
                && let Some(s) = repo::source(c, &d.source_id)?
            {
                cache.push(s);
            }
            if let Some(s) = cache.iter().find(|s| s.id == d.source_id) {
                out.push(doc_brief(d, s));
            }
        }
        Ok::<_, StoreError>(Value::Array(out))
    })?)
}

/// `GET /kb/docs/:id`.
pub fn get_doc(m: &Modules, who: &Caller, id: &str) -> ApiResult<Value> {
    let viewer = m.viewer(who)?;
    m.store.reader(|c| {
        let (d, s) = own_doc(c, &viewer, id, NOUN_DOC)?;
        doc_json(c, &d, &s)
    })
}

#[derive(Debug, Default, Deserialize)]
pub struct DocPatch {
    #[serde(default, deserialize_with = "nullable")]
    pub visibility: Option<Option<Visibility>>,
    #[serde(default, deserialize_with = "nullable")]
    pub level: Option<Option<Level>>,
    #[serde(default)]
    pub about: Option<Vec<ObjRef>>,
}

/// `PATCH /kb/docs/:id`: own scope (`null` = follow the source again) and `about`.
pub fn patch_doc(
    m: &Modules,
    kb: &Knowledge,
    who: &Caller,
    id: &str,
    b: DocPatch,
) -> ApiResult<Value> {
    Check::new()
        .that("about", b.about.as_ref().is_none_or(|a| a.len() <= 20))
        .done()?;
    let viewer = m.viewer(who)?;
    let now = m.clock.now();
    let out = m.store.transaction(|tx| {
        let (d, s) = own_doc(tx, &viewer, id, NOUN_DOC)?;
        if let Some(about) = &b.about {
            check_about(tx, who, about)?;
            repo::set_doc_about(tx, &who.ws, &d.id, about)?;
        }
        let vis = b.visibility.clone().unwrap_or(d.visibility.clone());
        let level = b.level.unwrap_or(d.level);
        if vis != d.visibility || level != d.level {
            repo::set_doc_scope(tx, &d.id, vis.as_ref(), level, now)?;
            let eff_vis = vis.clone().unwrap_or_else(|| s.visibility.clone());
            index::rescope(tx, "chunk", &d.id, &eff_vis, level.unwrap_or(s.level))?;
        }
        base::audit(
            tx,
            &who.ws,
            &who.me,
            "kb_doc.patch",
            &d.id,
            &json!({"visibility": vis, "level": level, "about": b.about.as_ref().map(Vec::len)}),
        )?;
        let (d, s) = own_doc_any(tx, &d.id)?;
        doc_json_bare(tx, &d, &s)
    })?;
    let out = with_uses(m, &who.ws, out)?;
    kb.notify(&who.ws, id);
    Ok(out)
}

/// The row again after a write, without the visibility check (the caller may have narrowed it).
fn own_doc_any(c: &Connection, id: &str) -> ApiResult<(DocRow, SourceRow)> {
    let d = repo::doc(c, id)?.ok_or_else(|| ApiError::not_found(NOUN_DOC))?;
    let s = repo::source(c, &d.source_id)?.ok_or_else(|| ApiError::not_found(NOUN_DOC))?;
    Ok((d, s))
}

/// `DELETE /kb/docs/:id`: notes are deleted, files leave the knowledge base; folder, item and chat
/// documents follow their origin and cannot be deleted here.
pub fn delete_doc(m: &Modules, kb: &Knowledge, who: &Caller, id: &str) -> ApiResult<Value> {
    let viewer = m.viewer(who)?;
    m.store.transaction(|tx| {
        let (d, s) = own_doc(tx, &viewer, id, NOUN_DOC)?;
        match s.kind.as_str() {
            "notes" | "files" => {}
            "folder" => {
                return Err(ApiError::rule(steve_i18n::t!(
                    "knowledge.doc.delete_folder"
                )));
            }
            "items" => {
                return Err(ApiError::rule(steve_i18n::t!("knowledge.doc.delete_item")));
            }
            "chat" => {
                return Err(ApiError::rule(steve_i18n::t!("knowledge.doc.delete_chat")));
            }
            _ => return Err(ApiError::rule(steve_i18n::t!("knowledge.doc.delete_other"))),
        }
        index::delete_doc(tx, &d.id)?;
        let action = if s.kind == "notes" {
            "kb_note.delete"
        } else {
            "kb_doc.remove"
        };
        base::audit(tx, &who.ws, &who.me, action, &d.id, &json!({}))?;
        Ok::<_, ApiError>(())
    })?;
    kb.notify(&who.ws, id);
    Ok(json!({"ok": true}))
}

// ---- notes

#[derive(Debug, Deserialize)]
pub struct NoteBody {
    pub title: String,
    #[serde(default)]
    pub body: String,
    #[serde(default)]
    pub visibility: Option<Visibility>,
    #[serde(default)]
    pub level: Option<Level>,
    #[serde(default)]
    pub about: Vec<ObjRef>,
}

/// `POST /kb/notes`.
pub fn create_note(m: &Modules, kb: &Knowledge, who: &Caller, b: NoteBody) -> ApiResult<Value> {
    Check::new()
        .len("title", b.title.trim(), 1, 120)
        .len("body", &b.body, 0, 200_000)
        .that("about", b.about.len() <= 20)
        .done()?;
    let now = m.clock.now();
    let out = m.store.transaction(|tx| {
        check_about(tx, who, &b.about)?;
        let id = notes::create(
            tx,
            kb.guard.as_ref(),
            &who.ws,
            &who.me,
            &NewNote {
                title: b.title.trim().to_string(),
                body: b.body.clone(),
                visibility: b.visibility.clone(),
                level: b.level,
                about: b.about.clone(),
            },
            now,
        )?;
        let (d, s) = own_doc_any(tx, &id)?;
        doc_json_bare(tx, &d, &s)
    })?;
    let out = with_uses(m, &who.ws, out)?;
    if let Some(id) = out.get("id").and_then(Value::as_str) {
        kb.notify(&who.ws, id);
    }
    Ok(out)
}

#[derive(Debug, Default, Deserialize)]
pub struct NotePut {
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub body: Option<String>,
}

/// `PUT /kb/notes/:id`.
pub fn update_note(
    m: &Modules,
    kb: &Knowledge,
    who: &Caller,
    id: &str,
    b: NotePut,
) -> ApiResult<Value> {
    let title = b.title.as_deref().map(str::trim);
    Check::new()
        .opt_len("title", title, 1, 120)
        .opt_len("body", b.body.as_deref(), 0, 200_000)
        .done()?;
    let viewer = m.viewer(who)?;
    let now = m.clock.now();
    let out = m.store.transaction(|tx| {
        let (d, s) = own_doc(tx, &viewer, id, NOUN_NOTE)?;
        if s.kind != "notes" {
            return Err(ApiError::not_found(NOUN_NOTE));
        }
        notes::update(
            tx,
            kb.guard.as_ref(),
            &d,
            &who.me,
            title,
            b.body.as_deref(),
            now,
        )?;
        let (d, s) = own_doc_any(tx, id)?;
        doc_json_bare(tx, &d, &s)
    })?;
    let out = with_uses(m, &who.ws, out)?;
    kb.notify(&who.ws, id);
    Ok(out)
}

// ---- memories

/// `Mem`, with `source_type` and the name of every `about` object (like
/// `Doc.about`; an unknown object has an empty name). A fixed stored `source_label`
/// (“你写的”) comes in the request's language.
pub fn mem_json(c: &Connection, m: &Mem) -> Value {
    let about: Vec<Value> = m
        .about
        .iter()
        .map(|a| {
            let name = repo::ref_name(c, a).ok().flatten().unwrap_or_default();
            json!({"type": a.kind, "id": a.id, "name": name})
        })
        .collect();
    json!({
        "id": m.id, "kind": m.kind, "text": m.text, "source_type": m.source_type,
        "source_label": steve_modules::shown::source_label(&m.source_label), "level": m.level, "status": m.status,
        "visibility": m.visibility, "about": about, "created": m.created,
    })
}

#[derive(Debug, Default, Deserialize)]
pub struct MemsParams {
    pub status: Option<String>,
    pub kind: Option<String>,
    pub about_type: Option<String>,
    pub about_id: Option<String>,
    pub q: Option<String>,
}

/// `GET /kb/mems`: every memory the caller can see (including `never` ones: this is their
/// management view), filtered.
pub fn list_mems(m: &Modules, who: &Caller, p: MemsParams) -> ApiResult<Value> {
    let viewer = m.viewer(who)?;
    let f = MemFilter {
        statuses: p.status.iter().filter(|s| !s.is_empty()).cloned().collect(),
        include_never: true,
        limit: 0,
    };
    let (list, names) = m.store.reader(|c| {
        let list = match (&p.about_type, &p.about_id) {
            (Some(t), Some(i)) if !t.is_empty() && !i.is_empty() => {
                mems::about(c, &viewer, &ObjRef::new(t, i), &f)?
            }
            _ => mems::all(c, &viewer, &f)?,
        };
        let shown: Vec<Value> = list.iter().map(|x| mem_json(c, x)).collect();
        Ok::<_, StoreError>((list, shown))
    })?;
    let q =
        p.q.as_deref()
            .map(str::to_lowercase)
            .filter(|q| !q.is_empty());
    let out: Vec<Value> = list
        .iter()
        .zip(names)
        .filter(|(x, _)| {
            p.kind
                .as_deref()
                .is_none_or(|k| k.is_empty() || x.kind == k)
        })
        .filter(|(x, _)| {
            q.as_deref()
                .is_none_or(|q| x.text.to_lowercase().contains(q))
        })
        .map(|(_, v)| v)
        .collect();
    Ok(Value::Array(out))
}

#[derive(Debug, Deserialize)]
pub struct MemBody {
    pub text: String,
    #[serde(default)]
    pub kind: Option<String>,
    #[serde(default)]
    pub level: Option<Level>,
    #[serde(default)]
    pub visibility: Option<Visibility>,
    #[serde(default)]
    pub about: Vec<ObjRef>,
}

/// `POST /kb/mems`: written by the user, so confirmed right away (`source_type=me`), and indexed
/// in the same transaction.
pub fn create_mem(m: &Modules, who: &Caller, b: MemBody) -> ApiResult<Value> {
    let kind = b.kind.clone().unwrap_or_else(|| "fact".into());
    Check::new()
        .len("text", b.text.trim(), 1, 2000)
        .one_of("kind", &kind, &MEM_KINDS)
        .that("about", b.about.len() <= 10)
        .done()?;
    prepare(m, who)?;
    let now = m.clock.now();
    let mem = m.store.transaction(|tx| {
        check_about(tx, who, &b.about)?;
        let mem = mems::create(
            tx,
            &who.ws,
            &NewMem {
                kind,
                text: b.text.trim().to_string(),
                source_type: "me".into(),
                visibility: b.visibility.clone().unwrap_or(Visibility::Private),
                level: b.level.unwrap_or(Level::Conclusion),
                status: "confirmed".into(),
                writer: "me".into(),
                created_by: who.me.clone(),
                about: b.about.clone(),
                ..Default::default()
            },
        )?;
        sync::sync_mem(tx, &mem.id, now)?;
        base::audit(
            tx,
            &who.ws,
            &who.me,
            "mem.create",
            &mem.id,
            &json!({"kind": mem.kind}),
        )?;
        Ok::<_, ApiError>(mem)
    })?;
    Ok(m.store.reader(|c| Ok::<_, StoreError>(mem_json(c, &mem)))?)
}
