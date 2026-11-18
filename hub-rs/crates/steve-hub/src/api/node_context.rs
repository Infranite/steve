//! The Hub's default for whether a Node owner's sessions load the owner's personal harness
//! configuration on that machine (user-level AGENTS.md / CLAUDE.md, skills, subagents, commands).
//! Each Node may choose for itself (`POST /api/nodes/{id}/context`); one without a choice follows
//! this. It is the Hub's, not a workspace's: a Node serves every workspace of its Hub.
//!
//! - `GET /api/node-context`: `{owner_context: "on" | "off"}`.
//! - `PUT /api/node-context` `{owner_context: "on" | "off"}`: the instance owner's only.

use super::{AppState, Ctx};
use crate::error::ApiError;
use axum::{Extension, Json, extract::State};
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

const fn word(on: bool) -> &'static str {
    if on { "on" } else { "off" }
}

pub async fn get(State(st): State<AppState>) -> Result<Json<Value>, ApiError> {
    let s = store(&st)?;
    let on =
        tokio::task::spawn_blocking(move || s.reader(steve_store::nodes::owner_context_default))
            .await
            .map_err(internal)?
            .map_err(internal)?;
    Ok(Json(json!({ "owner_context": word(on) })))
}

#[derive(Deserialize)]
pub struct Body {
    owner_context: String,
}

pub async fn put(
    State(st): State<AppState>,
    Extension(cx): Extension<Ctx>,
    Json(b): Json<Body>,
) -> Result<Json<Value>, ApiError> {
    if cx.user.get("role").and_then(Value::as_str) != Some("owner") {
        return Err(ApiError::Json(
            403,
            steve_i18n::t!("hub.node.context_owner_only"),
        ));
    }
    let on = match b.owner_context.as_str() {
        "on" => true,
        "off" => false,
        _ => {
            return Err(ApiError::bad_request(steve_i18n::t!(
                "hub.node.context_bad"
            )));
        }
    };
    let s = store(&st)?;
    let now = steve_store::now();
    tokio::task::spawn_blocking(move || {
        s.transaction(|tx| steve_store::nodes::set_owner_context_default(tx, on, now))
    })
    .await
    .map_err(internal)?
    .map_err(internal)?;
    Ok(Json(json!({ "owner_context": word(on) })))
}

/// Routes mounted behind the shared API login and body-limit layers.
pub(crate) fn routes() -> axum::Router<super::AppState> {
    use super::*;
    Router::new().route(
        "/node-context",
        get(node_context::get).put(node_context::put),
    )
}
