//! REST of slice A2 (accounts-vault.md §5.2 connections) and the OAuth callback. Bodies with secrets
//! have no `Debug` and are never logged (§10 rule 7).

use std::collections::BTreeMap;

use axum::extract::{Path, Query};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Extension, Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use zeroize::Zeroizing;

use super::service::{CALLBACK_PATH, Connections};
use crate::vault::VaultResult;
use crate::{Body, Caller, Modules};

fn to<T: Serialize>(v: T) -> VaultResult<Json<Value>> {
    Ok(Json(
        serde_json::to_value(v).map_err(crate::ApiError::from)?,
    ))
}

/// The connection routes, relative to `/api`, behind the `Caller` layer.
pub fn routes(conns: Connections) -> Router<Modules> {
    Router::new()
        .route("/connections", get(list).post(create))
        .route("/connections/adapters", get(adapters))
        .route("/connections/integrations", get(integrations))
        .route("/connections/oauth/start", post(oauth_start))
        .route("/connections/device/start", post(device_start))
        .route("/connections/device/{id}", get(device_status))
        .route(
            "/connections/{id}",
            get(get_one).patch(patch_one).delete(delete_one),
        )
        .route("/connections/{id}/reauth", post(reauth))
        .route("/connections/{id}/credential", put(put_credential))
        .route("/connections/{id}/test", post(test))
        .route("/connections/{id}/usage", get(usage))
        .layer(Extension(conns))
}

/// `GET /auth/connect/callback`, at the root (not under `/api`): a top-level browser navigation.
/// The integrator mounts it where the session cookie is read and inserts the `Caller` when there is
/// a session, without answering 401 when there isn't (the handler redirects with an error instead).
pub fn callback_routes(conns: Connections) -> Router<Modules> {
    Router::new()
        .route(CALLBACK_PATH, get(callback))
        .layer(Extension(conns))
}

async fn list(Extension(c): Extension<Connections>, who: Caller) -> VaultResult<Json<Value>> {
    to(c.list(&who)?)
}

async fn get_one(
    Extension(c): Extension<Connections>,
    who: Caller,
    Path(id): Path<String>,
) -> VaultResult<Json<Value>> {
    to(c.get(&who, &id)?)
}

async fn adapters(Extension(c): Extension<Connections>, _who: Caller) -> VaultResult<Json<Value>> {
    to(c.adapters())
}

async fn integrations(
    Extension(c): Extension<Connections>,
    who: Caller,
) -> VaultResult<Json<Value>> {
    to(c.integrations(&who)?)
}

#[derive(Deserialize)]
struct StartBody {
    adapter: String,
    #[serde(default)]
    caps: Option<Vec<String>>,
    #[serde(default)]
    config: Option<Value>,
}

async fn oauth_start(
    Extension(c): Extension<Connections>,
    who: Caller,
    headers: HeaderMap,
    Body(b): Body<StartBody>,
) -> VaultResult<Json<Value>> {
    let origin = c.origin(&headers);
    let url = c
        .oauth_start(
            &who,
            &origin,
            &b.adapter,
            b.caps,
            b.config.unwrap_or(Value::Null),
            None,
        )
        .await?;
    Ok(Json(json!({ "url": url })))
}

async fn device_start(
    Extension(c): Extension<Connections>,
    who: Caller,
    Body(b): Body<StartBody>,
) -> VaultResult<Json<Value>> {
    to(
        c.device_start(&who, &b.adapter, b.caps, b.config.unwrap_or(Value::Null))
            .await?,
    )
}

async fn device_status(
    Extension(c): Extension<Connections>,
    who: Caller,
    Path(id): Path<String>,
) -> VaultResult<Json<Value>> {
    Ok(Json(c.device_status(&who, &id)?))
}

/// `POST /connections`. No `Debug`: `fields` holds secrets.
#[derive(Deserialize)]
struct CreateBody {
    adapter: String,
    #[serde(default)]
    fields: BTreeMap<String, Zeroizing<String>>,
    #[serde(default)]
    config: Option<Value>,
}

async fn create(
    Extension(c): Extension<Connections>,
    who: Caller,
    Body(b): Body<CreateBody>,
) -> VaultResult<Json<Value>> {
    to(
        c.create(&who, &b.adapter, &b.fields, b.config.unwrap_or(Value::Null))
            .await?,
    )
}

#[derive(Deserialize)]
struct PatchBody {
    #[serde(default)]
    paused: Option<bool>,
    #[serde(default)]
    caps: Option<Vec<String>>,
}

async fn patch_one(
    Extension(c): Extension<Connections>,
    who: Caller,
    Path(id): Path<String>,
    Body(b): Body<PatchBody>,
) -> VaultResult<Json<Value>> {
    to(c.patch(&who, &id, b.paused, b.caps)?)
}

async fn delete_one(
    Extension(c): Extension<Connections>,
    who: Caller,
    Path(id): Path<String>,
) -> VaultResult<Json<Value>> {
    let usage = c.disconnect(&who, &id).await?;
    Ok(Json(json!({ "ok": true, "usage": usage })))
}

async fn reauth(
    Extension(c): Extension<Connections>,
    who: Caller,
    headers: HeaderMap,
    Path(id): Path<String>,
) -> VaultResult<Json<Value>> {
    let origin = c.origin(&headers);
    Ok(Json(c.reauth(&who, &origin, &id).await?))
}

/// `PUT /connections/:id/credential`. No `Debug`.
#[derive(Deserialize)]
struct CredentialBody {
    #[serde(default)]
    fields: BTreeMap<String, Zeroizing<String>>,
}

async fn put_credential(
    Extension(c): Extension<Connections>,
    who: Caller,
    Path(id): Path<String>,
    Body(b): Body<CredentialBody>,
) -> VaultResult<Json<Value>> {
    to(c.put_credential(&who, &id, &b.fields).await?)
}

async fn test(
    Extension(c): Extension<Connections>,
    who: Caller,
    Path(id): Path<String>,
) -> VaultResult<Json<Value>> {
    to(c.test(&who, &id).await?)
}

async fn usage(
    Extension(c): Extension<Connections>,
    who: Caller,
    Path(id): Path<String>,
) -> VaultResult<Json<Value>> {
    to(c.usage(&who, &id)?)
}

#[derive(Deserialize)]
struct CallbackQuery {
    state: Option<String>,
    code: Option<String>,
    error: Option<String>,
}

async fn callback(
    Extension(c): Extension<Connections>,
    who: Option<Extension<Caller>>,
    Query(q): Query<CallbackQuery>,
) -> Response {
    let loc = c
        .oauth_callback(
            who.as_ref().map(|w| &w.0),
            q.state.as_deref(),
            q.code.as_deref(),
            q.error.as_deref(),
        )
        .await;
    (
        StatusCode::FOUND,
        [
            (header::LOCATION, loc),
            (header::CACHE_CONTROL, "no-store".to_string()),
            (header::REFERRER_POLICY, "no-referrer".to_string()),
        ],
    )
        .into_response()
}
