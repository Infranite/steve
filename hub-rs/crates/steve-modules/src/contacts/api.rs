//! REST handlers of slice P1 (contacts.md §3.2: contacts, jobs, attributes, attribute definitions,
//! organizations). Thin: parse, run the service off the runtime, shape the answer.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::routing::{get, patch, post};
use axum::{Extension, Json, Router};
use serde::Serialize;
use serde_json::{Value, json};

use super::Ports;
use super::service::{self as svc, ContactsParams};
use crate::{ApiResult, Body, Caller, Modules};

type P = Extension<Arc<Ports>>;

fn ok() -> Json<Value> {
    Json(json!({ "ok": true }))
}

fn to<T: Serialize>(v: T) -> ApiResult<Json<Value>> {
    Ok(Json(serde_json::to_value(v)?))
}

/// The contacts routes (P1, and the teams of P2), relative to `/api`. `ports` connects the accounts module (and later others); pass
/// `Ports::default()` while they are not there.
pub fn routes(ports: Ports) -> Router<Modules> {
    Router::new()
        .route("/contacts", get(list_contacts).post(create_contact))
        .route(
            "/contacts/{id}",
            get(get_contact).patch(patch_contact).delete(delete_contact),
        )
        .route("/me/contact", get(me_contact))
        .route("/contacts/{id}/confirm", post(confirm_contact))
        .route("/contacts/{id}/jobs", post(add_job))
        .route(
            "/contacts/{id}/jobs/{org_id}",
            patch(patch_job).delete(delete_job),
        )
        .route("/parties/{id}/attrs", post(add_attr))
        .route("/attrs/{id}", patch(patch_attr).delete(delete_attr))
        .route("/attr-defs", get(list_defs).post(create_def))
        .route("/attr-defs/{id}", patch(patch_def).delete(delete_def))
        .route("/orgs", get(list_orgs).post(create_org))
        .route(
            "/orgs/{id}",
            get(get_org).patch(patch_org).delete(delete_org),
        )
        .merge(super::teams::api::routes())
        .merge(p3_routes())
        .merge(super::merge::routes())
        .merge(super::stewards_api::routes())
        .layer(Extension(Arc::new(ports)))
}

async fn list_contacts(
    State(m): State<Modules>,
    Extension(ports): P,
    who: Caller,
    Query(q): Query<ContactsParams>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| to(svc::list_contacts(m, &ports, &who, q)?))
        .await
}

async fn get_contact(
    State(m): State<Modules>,
    who: Caller,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| to(svc::get_contact(m, &who, &id)?))
        .await
}

async fn me_contact(State(m): State<Modules>, who: Caller) -> ApiResult<Json<Value>> {
    m.blocking(move |m| to(svc::me_contact(m, &who)?)).await
}

async fn create_contact(
    State(m): State<Modules>,
    who: Caller,
    Body(b): Body<svc::NewContact>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| to(svc::create_contact(m, &who, b)?))
        .await
}

async fn patch_contact(
    State(m): State<Modules>,
    who: Caller,
    Path(id): Path<String>,
    Body(b): Body<svc::ContactPatch>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| to(svc::patch_contact(m, &who, &id, b)?))
        .await
}

async fn confirm_contact(
    State(m): State<Modules>,
    who: Caller,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| to(svc::confirm_contact(m, &who, &id)?))
        .await
}

async fn delete_contact(
    State(m): State<Modules>,
    Extension(ports): P,
    who: Caller,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| {
        svc::delete_contact(m, &ports, &who, &id)?;
        Ok(ok())
    })
    .await
}

async fn add_job(
    State(m): State<Modules>,
    who: Caller,
    Path(id): Path<String>,
    Body(b): Body<svc::NewJob>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| to(svc::add_job(m, &who, &id, b)?))
        .await
}

async fn patch_job(
    State(m): State<Modules>,
    who: Caller,
    Path((id, org_id)): Path<(String, String)>,
    Body(b): Body<svc::JobPatch>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| to(svc::patch_job(m, &who, &id, &org_id, b)?))
        .await
}

async fn delete_job(
    State(m): State<Modules>,
    who: Caller,
    Path((id, org_id)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| {
        svc::delete_job(m, &who, &id, &org_id)?;
        Ok(ok())
    })
    .await
}

async fn add_attr(
    State(m): State<Modules>,
    who: Caller,
    Path(id): Path<String>,
    Body(b): Body<svc::NewAttr>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| to(svc::add_attr(m, &who, &id, b)?))
        .await
}

async fn patch_attr(
    State(m): State<Modules>,
    who: Caller,
    Path(id): Path<String>,
    Body(b): Body<svc::AttrPatch>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| {
        svc::patch_attr(m, &who, &id, b)?;
        Ok(ok())
    })
    .await
}

async fn delete_attr(
    State(m): State<Modules>,
    who: Caller,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| {
        svc::delete_attr(m, &who, &id)?;
        Ok(ok())
    })
    .await
}

async fn list_defs(State(m): State<Modules>, who: Caller) -> ApiResult<Json<Value>> {
    m.blocking(move |m| to(svc::list_defs(m, &who)?)).await
}

async fn create_def(
    State(m): State<Modules>,
    who: Caller,
    Body(b): Body<svc::NewDefBody>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| to(svc::create_def(m, &who, b)?)).await
}

async fn patch_def(
    State(m): State<Modules>,
    who: Caller,
    Path(id): Path<String>,
    Body(b): Body<svc::DefPatch>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| to(svc::patch_def(m, &who, &id, b)?))
        .await
}

async fn delete_def(
    State(m): State<Modules>,
    who: Caller,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| {
        svc::delete_def(m, &who, &id)?;
        Ok(ok())
    })
    .await
}

async fn list_orgs(State(m): State<Modules>, who: Caller) -> ApiResult<Json<Value>> {
    m.blocking(move |m| to(svc::list_orgs(m, &who)?)).await
}

async fn get_org(
    State(m): State<Modules>,
    who: Caller,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| to(svc::get_org(m, &who, &id)?)).await
}

async fn create_org(
    State(m): State<Modules>,
    who: Caller,
    Body(b): Body<svc::NewOrg>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| to(svc::create_org(m, &who, b)?)).await
}

async fn patch_org(
    State(m): State<Modules>,
    who: Caller,
    Path(id): Path<String>,
    Body(b): Body<svc::OrgPatch>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| to(svc::patch_org(m, &who, &id, b)?))
        .await
}

async fn delete_org(
    State(m): State<Modules>,
    who: Caller,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| {
        svc::delete_org(m, &who, &id)?;
        Ok(ok())
    })
    .await
}

// ---------------------------------------------------------------- slice P3

use super::{item_people as ip, memory as mm, mentions as mn};
use axum::routing::delete;

fn p3_routes() -> Router<Modules> {
    Router::new()
        .route("/parties/{id}/memories", get(list_mems).post(create_mem))
        .route("/mems/{id}", patch(patch_mem).delete(delete_mem))
        .route("/items/{id}/people", get(item_people).post(add_item_person))
        .route("/items/{id}/people/{party_id}", delete(remove_item_person))
        .route("/parties/{id}/items", get(party_items))
        .route("/parties/{id}/timeline", get(party_timeline))
        .route("/parties/search", get(search_parties))
        .route("/mention-options", get(mention_options))
}

async fn list_mems(
    State(m): State<Modules>,
    who: Caller,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| to(mm::list(m, &who, &id)?)).await
}

async fn create_mem(
    State(m): State<Modules>,
    who: Caller,
    Path(id): Path<String>,
    Body(b): Body<mm::NewMemBody>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| to(mm::create(m, &who, &id, b)?)).await
}

async fn patch_mem(
    State(m): State<Modules>,
    who: Caller,
    Path(id): Path<String>,
    Body(b): Body<mm::MemPatchBody>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| to(mm::patch(m, &who, &id, b)?)).await
}

async fn delete_mem(
    State(m): State<Modules>,
    who: Caller,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| {
        mm::delete(m, &who, &id)?;
        Ok(ok())
    })
    .await
}

async fn item_people(
    State(m): State<Modules>,
    who: Caller,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| to(ip::list(m, &who, &id)?)).await
}

async fn add_item_person(
    State(m): State<Modules>,
    who: Caller,
    Path(id): Path<String>,
    Body(b): Body<ip::AddBody>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| to(ip::add(m, &who, &id, b)?)).await
}

async fn remove_item_person(
    State(m): State<Modules>,
    who: Caller,
    Path((id, party_id)): Path<(String, String)>,
    Query(q): Query<ip::RemoveParams>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| {
        ip::remove(m, &who, &id, &party_id, q)?;
        Ok(ok())
    })
    .await
}

async fn party_items(
    State(m): State<Modules>,
    who: Caller,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| to(ip::party_items(m, &who, &id)?))
        .await
}

async fn party_timeline(
    State(m): State<Modules>,
    Extension(ports): P,
    who: Caller,
    Path(id): Path<String>,
    Query(q): Query<ip::TimelineParams>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| to(ip::timeline(m, &ports, &who, &id, q)?))
        .await
}

async fn search_parties(
    State(m): State<Modules>,
    who: Caller,
    Query(q): Query<mn::Q>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| to(mn::search(m, &who, q)?)).await
}

async fn mention_options(
    State(m): State<Modules>,
    who: Caller,
    Query(q): Query<mn::Q>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| to(mn::mention_options(m, &who, q)?))
        .await
}
