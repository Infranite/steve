//! REST handlers of slice K1, relative to `/api`. Thin: parse, run the
//! service off the runtime, answer.

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, patch, post, put};
use axum::{Extension, Json, Router};
use serde_json::Value;
use steve_modules::{ApiResult, Body, Caller, Modules};

use crate::service as svc;
use crate::sources::{files, folder};
use crate::state::Knowledge;

type Kb = Extension<Knowledge>;

/// The knowledge routes: search, sources, documents, notes, uploads, memories. K3 adds the
/// candidates, K4 `/kb/uses`.
pub fn routes(kb: Knowledge) -> Router<Modules> {
    Router::new()
        .route("/kb/search", get(search))
        .route("/kb/sources", get(list_sources).post(create_source))
        .route(
            "/kb/sources/{id}",
            patch(patch_source).delete(delete_source),
        )
        .route("/kb/sources/{id}/sync", post(sync_source))
        .route("/kb/files", post(add_file))
        .route("/kb/docs", get(list_docs))
        .route(
            "/kb/docs/{id}",
            get(get_doc).patch(patch_doc).delete(delete_doc),
        )
        .route("/kb/notes", post(create_note))
        .route("/kb/notes/{id}", put(update_note))
        .route("/kb/mems", get(list_mems).post(create_mem))
        .route("/kb/uses", get(list_uses))
        .route("/kb/providers", get(providers))
        .merge(crate::candidates::routes())
        .merge(crate::subjects::routes())
        .merge(crate::steward::routes())
        .layer(Extension(kb))
}

async fn search(
    State(m): State<Modules>,
    Extension(kb): Kb,
    who: Caller,
    Query(p): Query<svc::SearchParams>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| svc::search(m, &kb, &who, p).map(Json))
        .await
}

async fn list_sources(State(m): State<Modules>, who: Caller) -> ApiResult<Json<Value>> {
    m.blocking(move |m| svc::list_sources(m, &who).map(Json))
        .await
}

async fn list_docs(
    State(m): State<Modules>,
    who: Caller,
    Query(p): Query<svc::DocsParams>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| svc::list_docs(m, &who, p).map(Json))
        .await
}

async fn get_doc(
    State(m): State<Modules>,
    who: Caller,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| svc::get_doc(m, &who, &id).map(Json))
        .await
}

async fn patch_doc(
    State(m): State<Modules>,
    Extension(kb): Kb,
    who: Caller,
    Path(id): Path<String>,
    Body(b): Body<svc::DocPatch>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| svc::patch_doc(m, &kb, &who, &id, b).map(Json))
        .await
}

async fn delete_doc(
    State(m): State<Modules>,
    Extension(kb): Kb,
    who: Caller,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| svc::delete_doc(m, &kb, &who, &id).map(Json))
        .await
}

async fn create_note(
    State(m): State<Modules>,
    Extension(kb): Kb,
    who: Caller,
    Body(b): Body<svc::NoteBody>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| svc::create_note(m, &kb, &who, b).map(Json))
        .await
}

async fn update_note(
    State(m): State<Modules>,
    Extension(kb): Kb,
    who: Caller,
    Path(id): Path<String>,
    Body(b): Body<svc::NotePut>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| svc::update_note(m, &kb, &who, &id, b).map(Json))
        .await
}

async fn list_mems(
    State(m): State<Modules>,
    who: Caller,
    Query(p): Query<svc::MemsParams>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| svc::list_mems(m, &who, p).map(Json))
        .await
}

async fn create_mem(
    State(m): State<Modules>,
    who: Caller,
    Body(b): Body<svc::MemBody>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| svc::create_mem(m, &who, b).map(Json))
        .await
}

async fn providers(
    State(m): State<Modules>,
    Extension(kb): Kb,
    who: Caller,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| crate::provenance::providers(m, &kb, &who).map(Json))
        .await
}

async fn list_uses(
    State(m): State<Modules>,
    who: Caller,
    Query(p): Query<crate::uses::UsesParams>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| crate::uses::list(m, &who, p).map(Json))
        .await
}

/// Creates the folder source, then starts its first scan in the background.
async fn create_source(
    State(m): State<Modules>,
    Extension(kb): Kb,
    who: Caller,
    Body(b): Body<folder::SourceBody>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| {
        let s = folder::create(m, &kb, &who, b)?;
        folder::spawn_sync(m, &kb, &s.id)?;
        kb.notify(&who.ws, &s.id);
        let s = m
            .store
            .reader(|c| crate::repo::source(c, &s.id))?
            .unwrap_or(s);
        Ok(Json(
            m.store.reader(|c| crate::sources::source_json(c, &s))?,
        ))
    })
    .await
}

async fn patch_source(
    State(m): State<Modules>,
    Extension(kb): Kb,
    who: Caller,
    Path(id): Path<String>,
    Body(b): Body<folder::SourcePatch>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| {
        let (v, rescan) = folder::patch(m, &kb, &who, &id, b)?;
        if rescan {
            // Already syncing: the next scheduled scan picks the new rules up.
            folder::spawn_sync(m, &kb, &id)?;
        }
        Ok(Json(v))
    })
    .await
}

async fn delete_source(
    State(m): State<Modules>,
    Extension(kb): Kb,
    who: Caller,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| folder::delete(m, &kb, &who, &id).map(Json))
        .await
}

/// `202 {job_id}`; `409 正在同步`.
async fn sync_source(
    State(m): State<Modules>,
    Extension(kb): Kb,
    who: Caller,
    Path(id): Path<String>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let src = m
        .blocking(move |m| folder::request_sync(m, &who, &id))
        .await?;
    let job = steve_store::uid("job");
    let (m2, kb2) = (m.clone(), kb.clone());
    tokio::spawn(async move {
        if let Err(e) = folder::sync(&m2, &kb2, src).await {
            tracing::debug!("kb.folder_scan: {e}");
        }
    });
    Ok((
        StatusCode::ACCEPTED,
        Json(serde_json::json!({ "job_id": job })),
    ))
}

/// Adds the attachment and indexes it before answering (“加入即入库”).
async fn add_file(
    State(m): State<Modules>,
    Extension(kb): Kb,
    who: Caller,
    Body(b): Body<files::FileBody>,
) -> ApiResult<Json<Value>> {
    let who2 = who.clone();
    let (id, new) = m.blocking(move |m| files::add(m, &who2, &b)).await?;
    if new {
        if let Err(e) = crate::index::run_batch(&m, &kb, std::slice::from_ref(&id)).await {
            tracing::warn!("kb.index: {id}: {e}");
        }
        kb.notify(&who.ws, &id);
    }
    m.blocking(move |m| svc::get_doc(m, &who, &id).map(Json))
        .await
}
