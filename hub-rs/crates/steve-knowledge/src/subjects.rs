//! Subjects: what Steve knows about one person, project, bot or item, and where it learned it.
//! A subject is an `about` object of a memory or document; a project also gathers what came from its
//! items (their records, and the memories their deliveries left). `GET /kb/subjects` lists them,
//! `GET /kb/subjects/{type}/{id}` gives one subject's memories, documents and recent uses.

use std::collections::HashMap;

use axum::extract::{Path, State};
use axum::routing::get;
use axum::{Json, Router};
use rusqlite::Connection;
use serde_json::{Value, json};
use steve_modules::{ApiError, ApiResult, Caller, Modules};
use steve_store::StoreError;
use steve_store::base::mems::{self, Mem, MemFilter};
use steve_store::base::{ObjRef, Viewer};

use crate::provenance::{self, add_used, use_counts};
use crate::repo::{self, DocFilter, DocRow, SourceRow};
use crate::service::{doc_brief, mem_json, prepare};

/// How many recent uses a subject shows.
const USES_SHOWN: usize = 12;

pub fn routes() -> Router<Modules> {
    Router::new()
        .route("/kb/subjects", get(list_route))
        .route("/kb/subjects/{kind}/{id}", get(get_route))
}

async fn list_route(State(m): State<Modules>, who: Caller) -> ApiResult<Json<Value>> {
    m.blocking(move |m| list(m, &who).map(Json)).await
}

async fn get_route(
    State(m): State<Modules>,
    who: Caller,
    Path((kind, id)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| one(m, &who, &ObjRef::new(&kind, &id)).map(Json))
        .await
}

/// Where a subject's own page is.
pub fn subject_href(r: &ObjRef) -> String {
    match r.kind.as_str() {
        "party" => format!("#/contacts/{}", r.id),
        "item" => format!("#/items/{}", r.id),
        "bot" => format!("#/bots/{}", r.id),
        "project" => format!("#/projects/{}", r.id),
        "space" => format!("#/channels/space/{}", r.id),
        "persona" => format!("#/channels/personas/{}", r.id),
        "me" => "#/contacts/me".into(),
        _ => String::new(),
    }
}

/// Everything visible to the viewer, gathered once: memories, documents with their sources, and
/// which project each item belongs to.
struct Seen {
    mems: Vec<Mem>,
    docs: Vec<(DocRow, SourceRow)>,
    project_of: HashMap<String, String>,
}

fn seen(c: &Connection, viewer: &Viewer) -> Result<Seen, StoreError> {
    let mems = mems::all(
        c,
        viewer,
        &MemFilter {
            include_never: true,
            ..MemFilter::default()
        },
    )?;
    let rows = repo::docs_visible(
        c,
        viewer,
        &DocFilter {
            limit: 5000,
            ..DocFilter::default()
        },
    )?;
    let mut sources: Vec<SourceRow> = Vec::new();
    let mut docs = Vec::with_capacity(rows.len());
    for d in rows {
        if !sources.iter().any(|s| s.id == d.source_id)
            && let Some(s) = repo::source(c, &d.source_id)?
        {
            sources.push(s);
        }
        if let Some(s) = sources.iter().find(|s| s.id == d.source_id) {
            docs.push((d, s.clone()));
        }
    }
    let mut st = c.prepare_cached(
        "SELECT id, project_id FROM items WHERE workspace_id = ? AND project_id IS NOT NULL",
    )?;
    let project_of = st
        .query_map([&viewer.ws], |r| Ok((r.get(0)?, r.get(1)?)))?
        .collect::<rusqlite::Result<HashMap<String, String>>>()?;
    Ok(Seen {
        mems,
        docs,
        project_of,
    })
}

/// The subjects a memory is about: its `about` objects, and the project of the item it came from.
fn mem_subjects(m: &Mem, s: &Seen) -> Vec<ObjRef> {
    let mut out = m.about.clone();
    if m.source_type == "item"
        && let Some(p) = m.source_id.as_deref().and_then(|i| s.project_of.get(i))
    {
        let r = ObjRef::new("project", p);
        if !out.contains(&r) {
            out.push(r);
        }
    }
    out
}

/// The subjects a document is about: its `about` rows, and for an item's record the item's project.
fn doc_subjects(
    c: &Connection,
    d: &DocRow,
    src: &SourceRow,
    s: &Seen,
) -> Result<Vec<ObjRef>, StoreError> {
    let mut out = repo::doc_about(c, &d.id)?;
    if src.kind == "items"
        && let Some(p) = s.project_of.get(&d.r#ref)
    {
        let r = ObjRef::new("project", p);
        if !out.contains(&r) {
            out.push(r);
        }
    }
    Ok(out)
}

#[derive(Default)]
struct Tally {
    mems: i64,
    candidates: i64,
    docs: i64,
    last: i64,
}

/// `GET /kb/subjects`: `[{type, id, name, href, mems, candidates, docs, last}]`, most known first.
pub fn list(m: &Modules, who: &Caller) -> ApiResult<Value> {
    prepare(m, who)?;
    let viewer = m.viewer(who)?;
    Ok(m.store.reader(|c| {
        let s = seen(c, &viewer)?;
        let mut order: Vec<ObjRef> = Vec::new();
        let mut tally: HashMap<ObjRef, Tally> = HashMap::new();
        let mut bump = |r: ObjRef, f: &dyn Fn(&mut Tally)| {
            if !tally.contains_key(&r) {
                order.push(r.clone());
            }
            f(tally.entry(r).or_default());
        };
        for x in &s.mems {
            for r in mem_subjects(x, &s) {
                let (cand, at) = (x.status == "candidate", x.updated);
                bump(r, &|t| {
                    t.mems += 1;
                    t.candidates += cand as i64;
                    t.last = t.last.max(at);
                });
            }
        }
        for (d, src) in &s.docs {
            for r in doc_subjects(c, d, src, &s)? {
                let at = d.updated;
                bump(r, &|t| {
                    t.docs += 1;
                    t.last = t.last.max(at);
                });
            }
        }
        let mut out = Vec::new();
        for r in order {
            let t = &tally[&r];
            out.push(json!({
                "type": r.kind, "id": r.id, "name": repo::ref_name(c, &r)?.unwrap_or_default(),
                "href": subject_href(&r), "mems": t.mems, "candidates": t.candidates, "docs": t.docs, "last": t.last,
            }));
        }
        out.sort_by(|a, b| {
            let n = |v: &Value| v["mems"].as_i64().unwrap_or(0) + v["docs"].as_i64().unwrap_or(0);
            n(b).cmp(&n(a))
                .then_with(|| b["last"].as_i64().cmp(&a["last"].as_i64()))
        });
        Ok::<_, StoreError>(Value::Array(out))
    })?)
}

/// `GET /kb/subjects/{type}/{id}`: `{type, id, name, href, mems: [Mem…], docs: [DocBrief…],
/// uses: […]}` — its memories (each with where it came from and how often it was used), the
/// documents about it, and where any of them was used lately.
pub fn one(m: &Modules, who: &Caller, r: &ObjRef) -> ApiResult<Value> {
    prepare(m, who)?;
    let viewer = m.viewer(who)?;
    m.store.reader(|c| {
        let name = repo::ref_name(c, r)?;
        if name.is_none() && r.kind != "me" {
            return Err(ApiError::not_found("对象"));
        }
        let s = seen(c, &viewer)?;
        let mem_uses = use_counts(c, &who.ws, "mem")?;
        let doc_uses = use_counts(c, &who.ws, "chunk")?;
        let mut objs = Vec::new();
        let mut mems_out = Vec::new();
        for x in s.mems.iter().filter(|x| mem_subjects(x, &s).contains(r)) {
            let mut v = mem_json(c, x);
            add_used(&mut v, &mem_uses);
            objs.push(("mem".to_string(), x.id.clone()));
            mems_out.push(v);
        }
        let mut docs_out = Vec::new();
        for (d, src) in &s.docs {
            if doc_subjects(c, d, src, &s)?.contains(r) {
                let mut v = doc_brief(d, src);
                add_used(&mut v, &doc_uses);
                objs.push(("chunk".to_string(), d.id.clone()));
                docs_out.push(v);
            }
        }
        let uses = provenance::recent_uses(c, &who.ws, &objs, USES_SHOWN)?;
        Ok(json!({
            "type": r.kind, "id": r.id, "name": name.unwrap_or_default(), "href": subject_href(r),
            "mems": mems_out, "docs": docs_out, "uses": uses,
        }))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hrefs_by_type() {
        assert_eq!(
            subject_href(&ObjRef::new("party", "ct_1")),
            "#/contacts/ct_1"
        );
        assert_eq!(
            subject_href(&ObjRef::new("project", "prj_1")),
            "#/projects/prj_1"
        );
        assert_eq!(subject_href(&ObjRef::new("x", "1")), "");
    }

    #[test]
    fn a_project_gathers_what_its_items_left() {
        let s = Seen {
            mems: Vec::new(),
            docs: Vec::new(),
            project_of: HashMap::from([("ST-1".to_string(), "prj_1".to_string())]),
        };
        let mut m = test_mem();
        m.source_type = "item".into();
        m.source_id = Some("ST-1".into());
        m.about = vec![ObjRef::new("party", "ct_1")];
        assert_eq!(
            mem_subjects(&m, &s),
            vec![
                ObjRef::new("party", "ct_1"),
                ObjRef::new("project", "prj_1")
            ]
        );
        m.source_id = Some("ST-2".into());
        assert_eq!(mem_subjects(&m, &s), vec![ObjRef::new("party", "ct_1")]);
    }

    fn test_mem() -> Mem {
        Mem {
            id: "mm_1".into(),
            workspace_id: "w".into(),
            kind: "fact".into(),
            text: "t".into(),
            source_type: "me".into(),
            source_id: None,
            source_label: String::new(),
            visibility: steve_store::base::Visibility::Private,
            level: steve_store::base::Level::Conclusion,
            status: "confirmed".into(),
            valid_until: None,
            writer: "me".into(),
            decision_id: None,
            created_by: "u".into(),
            created: 0,
            updated: 0,
            about: Vec::new(),
        }
    }
}
