//! Node tags (B7): the owner labels each Node, the Hub's own (`local`) included; an item may ask
//! for a tag, and its new sessions go to a Node that carries it.
//!
//! - `GET /api/node-tags`: `{tags: {<node>: [..]}, item: ..}` for every Node.
//! - `PUT /api/node-tags/{node}` `{tags: [..]}`: the instance owner's only.
//! - `PUT /api/items/{id}/node-tag` `{tag: "gpu" | null}`.

use super::{AppState, Ctx};
use crate::error::ApiError;
use axum::{
    Extension, Json,
    extract::{Path, State},
};
use serde::Deserialize;
use serde_json::{Value, json};

fn store(st: &AppState) -> Result<steve_store::Store, ApiError> {
    st.modules
        .as_ref()
        .map(|m| m.store.clone())
        .ok_or_else(|| ApiError::Json(503, "no store".into()))
}

fn internal(e: impl std::fmt::Display) -> ApiError {
    ApiError::Json(500, e.to_string())
}

pub async fn list(State(st): State<AppState>) -> Result<Json<Value>, ApiError> {
    let s = store(&st)?;
    let tags = tokio::task::spawn_blocking(move || s.reader(steve_store::nodes::all_tags))
        .await
        .map_err(internal)?
        .map_err(internal)?;
    Ok(Json(json!({ "tags": tags })))
}

#[derive(Deserialize)]
pub struct Tags {
    tags: Vec<String>,
}

pub async fn set(
    State(st): State<AppState>,
    Extension(cx): Extension<Ctx>,
    Path(node): Path<String>,
    Json(b): Json<Tags>,
) -> Result<Json<Value>, ApiError> {
    if cx.user.get("role").and_then(Value::as_str) != Some("owner") {
        return Err(ApiError::Json(
            403,
            steve_i18n::t!("hub.node.tags_owner_only"),
        ));
    }
    if b.tags.len() > 32 {
        return Err(ApiError::bad_request(steve_i18n::t!("hub.node.tag_bad")));
    }
    let s = store(&st)?;
    let kept = tokio::task::spawn_blocking(move || {
        s.transaction(|tx| steve_store::nodes::set_tags(tx, &node, &b.tags))
    })
    .await
    .map_err(internal)?
    .map_err(internal)?;
    Ok(Json(json!({ "tags": kept })))
}

#[derive(Deserialize)]
pub struct ItemTag {
    tag: Option<String>,
}

pub async fn set_item(
    State(st): State<AppState>,
    Path(item): Path<String>,
    Json(b): Json<ItemTag>,
) -> Result<Json<Value>, ApiError> {
    let s = store(&st)?;
    let ok = tokio::task::spawn_blocking(move || {
        s.transaction(|tx| {
            steve_store::nodes::set_item_tag(
                tx,
                &item,
                b.tag.as_deref().filter(|t| !t.trim().is_empty()),
            )
        })
    })
    .await
    .map_err(internal)?
    .map_err(internal)?;
    if !ok {
        return Err(ApiError::bad_request(steve_i18n::t!("hub.node.tag_bad")));
    }
    Ok(Json(json!({ "ok": true })))
}
