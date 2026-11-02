//! REST handlers of slice P2 (contacts.md §3.2, the `/teams` rows). Merged into [`super::super::routes`],
//! which adds the [`Ports`] extension.

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, post, put};
use axum::{Extension, Json, Router};
use serde::Serialize;
use serde_json::{Value, json};

use super::super::Ports;
use super::{self as svc, MemberIn};
use crate::{ApiResult, Body, Caller, Modules};

type P = Extension<Arc<Ports>>;

fn to<T: Serialize>(v: T) -> ApiResult<Json<Value>> {
    Ok(Json(serde_json::to_value(v)?))
}

/// The team routes, relative to `/api` (without the `Ports` layer).
pub fn routes() -> Router<Modules> {
    Router::new()
        .route("/teams", get(list_teams).post(create_team))
        .route("/teams/sync", post(sync_teams))
        .route(
            "/teams/{id}",
            get(get_team).patch(patch_team).delete(dissolve_team),
        )
        .route("/teams/{id}/members", put(put_members))
}

async fn list_teams(
    State(m): State<Modules>,
    Extension(ports): P,
    who: Caller,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| to(svc::list_teams(m, &ports, &who)?))
        .await
}

async fn get_team(
    State(m): State<Modules>,
    Extension(ports): P,
    who: Caller,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| to(svc::get_team(m, &ports, &who, &id)?))
        .await
}

async fn create_team(
    State(m): State<Modules>,
    Extension(ports): P,
    who: Caller,
    Body(b): Body<svc::NewTeam>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| to(svc::create_team(m, &ports, &who, b)?))
        .await
}

async fn patch_team(
    State(m): State<Modules>,
    Extension(ports): P,
    who: Caller,
    Path(id): Path<String>,
    Body(b): Body<svc::TeamPatch>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| to(svc::patch_team(m, &ports, &who, &id, b)?))
        .await
}

async fn put_members(
    State(m): State<Modules>,
    Extension(ports): P,
    who: Caller,
    Path(id): Path<String>,
    Body(b): Body<Vec<MemberIn>>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| to(svc::put_members(m, &ports, &who, &id, b)?))
        .await
}

async fn dissolve_team(
    State(m): State<Modules>,
    who: Caller,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| {
        svc::dissolve_team(m, &who, &id)?;
        Ok(Json(json!({ "ok": true })))
    })
    .await
}

/// `202 {job_id}`. An empty body is the same as `{}`.
async fn sync_teams(
    State(m): State<Modules>,
    Extension(ports): P,
    who: Caller,
    body: axum::body::Bytes,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let b: svc::SyncBody = if body.iter().all(u8::is_ascii_whitespace) {
        svc::SyncBody::default()
    } else {
        crate::parse(&body)?
    };
    let job = m
        .blocking(move |m| svc::sync_teams(m, &ports, &who, b))
        .await?;
    Ok((StatusCode::ACCEPTED, Json(json!({ "job_id": job }))))
}
