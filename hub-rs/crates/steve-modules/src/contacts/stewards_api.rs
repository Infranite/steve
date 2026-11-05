//! REST of ownership, relative to `/api`:
//!
//! - `GET /people/:id/holdings` — 名下: own accounts by system, what the person is responsible for;
//! - `GET /stewards/unowned` — objects without a confirmed 负责人;
//! - `GET|PUT|DELETE /stewards/:type/:id` — an object's stewards; `PUT {person_id, role?, replace?}`,
//!   `DELETE ?person_id=&role=`. Both answer `{ok, undo, object, stewards}`; `undo` is a changeset id
//!   for `POST /api/config/:id/undo`.

use axum::extract::{Path, Query, State};
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;
use serde_json::{Value, json};

use super::stewards::{self as st, RemoveQuery, SetBody};
use crate::{ApiResult, Body, Caller, Modules};

fn to<T: Serialize>(v: T) -> ApiResult<Json<Value>> {
    Ok(Json(serde_json::to_value(v)?))
}

pub(crate) fn routes() -> Router<Modules> {
    Router::new()
        .route("/people/{id}/holdings", get(holdings))
        .route("/stewards/unowned", get(unowned))
        .route(
            "/stewards/{object_type}/{id}",
            get(list).put(put).delete(delete),
        )
}

async fn holdings(
    State(m): State<Modules>,
    who: Caller,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| to(st::holdings(m, &who, &id)?)).await
}

async fn unowned(State(m): State<Modules>, who: Caller) -> ApiResult<Json<Value>> {
    m.blocking(move |m| Ok(Json(json!({ "objects": st::unowned(m, &who)? }))))
        .await
}

async fn list(
    State(m): State<Modules>,
    who: Caller,
    Path((t, id)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| to(st::list_object(m, &who, &t, &id)?))
        .await
}

async fn put(
    State(m): State<Modules>,
    who: Caller,
    Path((t, id)): Path<(String, String)>,
    Body(b): Body<SetBody>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| to(st::set(m, &who, &t, &id, b)?)).await
}

async fn delete(
    State(m): State<Modules>,
    who: Caller,
    Path((t, id)): Path<(String, String)>,
    Query(q): Query<RemoveQuery>,
) -> ApiResult<Json<Value>> {
    m.blocking(move |m| to(st::remove(m, &who, &t, &id, q)?))
        .await
}
