//! REST of slice V2 (accounts-vault.md §5.2): vaults, templates, items, fields, and the two reveal
//! steps. Request bodies with values are never `Debug`-printed (§10 rule 7); the only response with a
//! secret is `reveal/finish`, `cache-control: no-store`.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use zeroize::Zeroizing;

use super::repo::ItemFilter;
use super::reveal::{REVEAL_TTL_S, RevealGate, RevealTarget};
use super::service::{ItemPatch, NewField, NewItem};
use crate::vault::webauthn::request_host;
use crate::vault::{Vault, VaultError, VaultResult};
use crate::{Body, Caller};

#[derive(Clone)]
struct St {
    vault: Vault,
    gate: Arc<dyn RevealGate>,
}

/// The V2 routes. `gate` is a [`crate::vault::webauthn::Passkeys`] (tests: [`super::NoPasskeys`] or a stub). Merge under
/// `/api` after `require_user`, next to [`crate::vault::routes`].
pub fn routes<S: Clone + Send + Sync + 'static>(
    vault: Vault,
    gate: Arc<dyn RevealGate>,
) -> Router<S> {
    Router::new()
        .route("/vaults", get(vaults))
        .route("/vault/templates", get(templates))
        .route("/vault/items", get(list).post(create))
        .route("/vault/items/{id}", get(detail).patch(patch).delete(delete))
        .route("/vault/items/{id}/fields", post(add_field))
        .route(
            "/vault/items/{id}/fields/{fid}",
            put(put_field).delete(delete_field),
        )
        .route(
            "/vault/items/{id}/fields/{fid}/reveal/start",
            post(reveal_start),
        )
        .route(
            "/vault/items/{id}/fields/{fid}/reveal/finish",
            post(reveal_finish),
        )
        .with_state(St { vault, gate })
}

async fn vaults(State(s): State<St>, who: Caller) -> VaultResult<Response> {
    let out = s
        .vault
        .blocking(move |v| v.list_vaults(&who.ws, &who.me))
        .await?;
    Ok(Json(out).into_response())
}

async fn templates(State(s): State<St>, who: Caller) -> VaultResult<Response> {
    let out = s
        .vault
        .blocking(move |v| v.templates(&who.ws, &who.me))
        .await?;
    Ok(Json(out).into_response())
}

#[derive(Debug, Default, Deserialize)]
struct ListParams {
    vault: Option<String>,
    template: Option<String>,
    q: Option<String>,
    account: Option<String>,
}

fn blank(s: Option<String>) -> Option<String> {
    s.filter(|x| !x.trim().is_empty())
}

async fn list(
    State(s): State<St>,
    who: Caller,
    Query(p): Query<ListParams>,
) -> VaultResult<Response> {
    let f = ItemFilter {
        vault: blank(p.vault),
        template: blank(p.template),
        account: blank(p.account),
        q: blank(p.q),
    };
    let out = s
        .vault
        .blocking(move |v| v.list_items(&who.ws, &who.me, &f))
        .await?;
    Ok(Json(out).into_response())
}

async fn detail(State(s): State<St>, who: Caller, Path(id): Path<String>) -> VaultResult<Response> {
    let out = s
        .vault
        .blocking(move |v| v.get_item(&who.ws, &who.me, &id))
        .await?;
    Ok(Json(out).into_response())
}

async fn create(State(s): State<St>, who: Caller, Body(b): Body<NewItem>) -> VaultResult<Response> {
    let out = s
        .vault
        .blocking(move |v| v.create_item(&who.ws, &who.me, b))
        .await?;
    Ok(Json(out).into_response())
}

async fn patch(
    State(s): State<St>,
    who: Caller,
    Path(id): Path<String>,
    Body(b): Body<ItemPatch>,
) -> VaultResult<Response> {
    let out = s
        .vault
        .blocking(move |v| v.patch_item(&who.ws, &who.me, &id, b))
        .await?;
    Ok(Json(out).into_response())
}

async fn delete(State(s): State<St>, who: Caller, Path(id): Path<String>) -> VaultResult<Response> {
    let n = s
        .vault
        .blocking(move |v| v.delete_item(&who.ws, &who.me, &id))
        .await?;
    Ok(Json(json!({ "ok": true, "revoked_grants": n })).into_response())
}

async fn add_field(
    State(s): State<St>,
    who: Caller,
    Path(id): Path<String>,
    Body(b): Body<NewField>,
) -> VaultResult<Response> {
    let out = s
        .vault
        .blocking(move |v| v.add_field(&who.ws, &who.me, &id, b))
        .await?;
    Ok(Json(out).into_response())
}

/// No `Debug`: the value may be a secret.
#[derive(Deserialize)]
struct ValueIn {
    value: Zeroizing<String>,
}

async fn put_field(
    State(s): State<St>,
    who: Caller,
    Path((id, fid)): Path<(String, String)>,
    Body(b): Body<ValueIn>,
) -> VaultResult<Response> {
    let out = s
        .vault
        .blocking(move |v| v.put_field(&who.ws, &who.me, &id, &fid, b.value))
        .await?;
    Ok(Json(out).into_response())
}

async fn delete_field(
    State(s): State<St>,
    who: Caller,
    Path((id, fid)): Path<(String, String)>,
) -> VaultResult<Response> {
    let out = s
        .vault
        .blocking(move |v| v.delete_field(&who.ws, &who.me, &id, &fid))
        .await?;
    Ok(Json(out).into_response())
}

fn target(who: Caller, id: String, fid: String, h: &HeaderMap) -> RevealTarget {
    RevealTarget {
        ws: who.ws,
        user: who.me,
        item_id: id,
        field_id: fid,
        host: request_host(h),
    }
}

async fn reveal_start(
    State(s): State<St>,
    who: Caller,
    headers: HeaderMap,
    Path((id, fid)): Path<(String, String)>,
) -> VaultResult<Response> {
    let t = target(who, id, fid, &headers);
    let c = t.clone();
    s.vault.blocking(move |v| v.reveal_check(&c)).await?;
    let challenge = s.gate.start(&t).await?;
    Ok(([(header::CACHE_CONTROL, "no-store")], Json(challenge)).into_response())
}

#[derive(Deserialize)]
struct FinishIn {
    #[serde(default)]
    assertion: Value,
    #[serde(default)]
    copy: bool,
}

async fn reveal_finish(
    State(s): State<St>,
    who: Caller,
    headers: HeaderMap,
    Path((id, fid)): Path<(String, String)>,
    Body(b): Body<FinishIn>,
) -> VaultResult<Response> {
    let t = target(who, id, fid, &headers);
    let c = t.clone();
    s.vault.blocking(move |v| v.reveal_check(&c)).await?;
    let verified = match s.gate.finish(&t, &b.assertion).await {
        Ok(x) => x,
        Err(e) => {
            let why = e.to_string();
            let c = t.clone();
            s.vault
                .blocking(move |v| v.reveal_refused(&c, &why))
                .await?;
            return Err(e);
        }
    };
    let value = s
        .vault
        .blocking(move |v| v.reveal(verified, b.copy))
        .await?;
    let text = std::str::from_utf8(value.expose())
        .map_err(|_| VaultError::internal("vault field is not text"))?;
    // {"value":<escaped>,"ttl":30}, written straight into a buffer that is sized once
    let mut body = Zeroizing::new(Vec::with_capacity(text.len() * 6 + 32));
    body.extend_from_slice(b"{\"value\":");
    serde_json::to_writer(&mut *body, text).map_err(|_| VaultError::internal("encode"))?;
    body.extend_from_slice(format!(",\"ttl\":{REVEAL_TTL_S}}}").as_bytes());
    Ok((
        StatusCode::OK,
        [
            (header::CACHE_CONTROL, "no-store"),
            (header::CONTENT_TYPE, "application/json"),
        ],
        body.to_vec(),
    )
        .into_response())
}
