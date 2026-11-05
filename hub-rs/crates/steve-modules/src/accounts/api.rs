//! REST handlers of slice A1 (systems, accounts, match rules). Thin: parse,
//! run the service off the runtime, shape the answer.

use axum::extract::{Path, Query, State};
use axum::routing::{get, patch};
use axum::{Json, Router};
use serde::Serialize;
use serde_json::{Value, json};

use super::service::{self as svc, AccountsParams};
use crate::{ApiResult, Body, Caller, Modules};

fn to<T: Serialize>(v: T) -> ApiResult<Json<Value>> {
    Ok(Json(serde_json::to_value(v)?))
}

/// The A1 routes, relative to `/api`.
pub fn routes() -> Router<Modules> {
    Router::new()
        .route("/systems", get(systems))
        .route("/accounts", get(list).post(create))
        .route(
            "/accounts/{id}",
            get(get_one).patch(patch_one).delete(delete_one),
        )
        .route("/match-rules", get(rules))
        .route("/match-rules/{id}", patch(patch_rule))
}

async fn systems(State(m): State<Modules>, who: Caller) -> ApiResult<Json<Value>> {
    m.blocking(move |m| to(svc::systems(m, &who)?)).await
}

async fn list(
    State(m): State<Modules>,
    who: Caller,
    Query(q): Query<AccountsParams>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| to(svc::list(m, &who, q)?)).await
}

async fn get_one(
    State(m): State<Modules>,
    who: Caller,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| to(svc::get(m, &who, &id)?)).await
}

async fn create(
    State(m): State<Modules>,
    who: Caller,
    Body(b): Body<svc::NewAccountBody>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| to(svc::create(m, &who, b)?)).await
}

async fn patch_one(
    State(m): State<Modules>,
    who: Caller,
    Path(id): Path<String>,
    Body(b): Body<svc::AccountPatchBody>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| to(svc::patch(m, &who, &id, b)?)).await
}

async fn delete_one(
    State(m): State<Modules>,
    who: Caller,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| {
        svc::delete(m, &who, &id)?;
        Ok(Json(json!({ "ok": true })))
    })
    .await
}

async fn rules(State(m): State<Modules>, who: Caller) -> ApiResult<Json<Value>> {
    m.blocking(move |m| to(svc::rules(m, &who)?)).await
}

async fn patch_rule(
    State(m): State<Modules>,
    who: Caller,
    Path(id): Path<String>,
    Body(b): Body<svc::RulePatch>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| to(svc::patch_rule(m, &who, &id, b)?))
        .await
}
