//! REST of slice V1: `GET /vault/status`, `POST /vault/init`,
//! `POST /vault/recover`, `POST /vault/rotate`. Responses that carry a recovery code are
//! `cache-control: no-store`. Request bodies are never `Debug`-printed.

use axum::extract::State;
use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::json;
use zeroize::Zeroizing;

use super::error::VaultResult;
use super::service::Vault;
use crate::{Body, Caller};

/// The V1 routes, with the vault as their state. Merge under `/api` after `require_user`.
pub fn routes<S: Clone + Send + Sync + 'static>(vault: Vault) -> Router<S> {
    Router::new()
        .route("/vault/status", get(status))
        .route("/vault/init", post(init))
        .route("/vault/recover", post(recover))
        .route("/vault/rotate", post(rotate))
        .with_state(vault)
}

fn no_store(code: StatusCode, body: String) -> Response {
    (
        code,
        [
            (header::CACHE_CONTROL, "no-store"),
            (header::CONTENT_TYPE, "application/json"),
        ],
        body,
    )
        .into_response()
}

/// `{"recovery_code": …}` plus extra fields, built without an intermediate `Value` holding the code.
fn code_body(code: &str, extra: &[(&str, serde_json::Value)]) -> String {
    let mut s = String::from("{");
    for (k, v) in extra {
        s.push_str(&format!("{}:{},", json!(k), v));
    }
    // base32 and dashes only, so no escaping is needed
    s.push_str("\"recovery_code\":\"");
    s.push_str(code);
    s.push_str("\"}");
    s
}

async fn status(State(v): State<Vault>, who: Caller) -> VaultResult<Response> {
    let s = v.blocking(move |v| v.status(&who.ws)).await?;
    Ok(Json(s).into_response())
}

async fn init(State(v): State<Vault>, who: Caller) -> VaultResult<Response> {
    let code = v.blocking(move |v| v.init(&who.ws, &who.me)).await?;
    Ok(no_store(StatusCode::OK, code_body(code.expose(), &[])))
}

/// No `Debug`: the code is the master key.
#[derive(Deserialize)]
struct RecoverIn {
    code: Zeroizing<String>,
}

async fn recover(
    State(v): State<Vault>,
    who: Caller,
    Body(b): Body<RecoverIn>,
) -> VaultResult<Response> {
    let version = v
        .blocking(move |v| v.recover(&who.ws, &b.code, &who.me))
        .await?;
    Ok(no_store(
        StatusCode::OK,
        json!({ "ok": true, "kek_version": version }).to_string(),
    ))
}

async fn rotate(State(v): State<Vault>, who: Caller) -> VaultResult<Response> {
    let r = v.rotate(&who.ws, &who.me).await?;
    Ok(no_store(
        StatusCode::ACCEPTED,
        code_body(
            r.recovery_code.expose(),
            &[
                ("job_id", json!(r.job_id)),
                ("kek_version", json!(r.version)),
            ],
        ),
    ))
}
