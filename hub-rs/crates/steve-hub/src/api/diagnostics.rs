//! Item diagnostic reads share ordinary workspace access. Capture is instance-owner only.

use std::collections::HashMap;

use axum::{
    Extension,
    body::Bytes,
    extract::{Path, Query, State},
};
use serde::Deserialize;
use serde_json::{Value, json};
use steve_store::Store;
use tracing::Instrument;

use super::{ApiResult, AppState, Ctx, Noun, body, ok, own};
use crate::{
    error::ApiError,
    wire::diagnostics::{self, Args},
};

fn owner(cx: &Ctx) -> bool {
    cx.user.get("role").and_then(Value::as_str) == Some("owner")
}

fn store(st: &AppState) -> Result<Store, ApiError> {
    st.modules
        .as_ref()
        .map(|m| m.store.clone())
        .ok_or_else(|| ApiError::Json(503, "Diagnostics unavailable.".into()))
}

async fn item(st: &AppState, cx: &Ctx, id: &str) -> Result<(), ApiError> {
    let (ws, id) = (cx.ws.clone(), id.to_string());
    st.read(move |r| {
        own(r.item_get(&id)?, &ws, Noun::Item)?;
        Ok(())
    })
    .await
}

async fn view(st: &AppState, cx: &Ctx, id: &str) -> ApiResult {
    let state = st.domain.diagnostic_snapshot(id).await?;
    let (store, ws, id, can_capture) = (store(st)?, cx.ws.clone(), id.to_string(), owner(cx));
    super::blocking(move || {
        ok(diagnostics::explain(&store, &ws, &id, state, can_capture)
            .map_err(|e| ApiError::Json(503, e))?)
    })
    .await
}

pub async fn explain(
    State(st): State<AppState>,
    Extension(cx): Extension<Ctx>,
    Path(id): Path<String>,
) -> ApiResult {
    item(&st, &cx, &id).await?;
    view(&st, &cx, &id).await
}

pub async fn events(
    State(st): State<AppState>,
    Extension(cx): Extension<Ctx>,
    Path(id): Path<String>,
    Query(q): Query<HashMap<String, String>>,
) -> ApiResult {
    item(&st, &cx, &id).await?;
    let mut args = json!({"action":"events","item_id":id});
    for (key, value) in q {
        match key.as_str() {
            "limit" | "since" | "until" => {
                args[&key] = json!(
                    value
                        .parse::<i64>()
                        .map_err(|_| ApiError::bad_request("Invalid diagnostic range."))?
                )
            }
            "kinds" => args[&key] = json!(value.split(',').map(str::to_string).collect::<Vec<_>>()),
            "cursor" | "session_id" | "attempt_id" | "event_id" | "severity" => {
                args[&key] = json!(value)
            }
            _ => return Err(ApiError::bad_request("Unknown diagnostic filter.")),
        }
    }
    let args = Args::parse(args).map_err(ApiError::bad_request)?;
    let (store, ws) = (store(&st)?, cx.ws);
    super::blocking(move || {
        ok(diagnostics::events(&store, &ws, &args).map_err(ApiError::bad_request)?)
    })
    .await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Capture {
    enabled: bool,
    minutes: Option<u32>,
}

pub async fn capture(
    State(st): State<AppState>,
    Extension(cx): Extension<Ctx>,
    Path(id): Path<String>,
    bytes: Bytes,
) -> ApiResult {
    if !owner(&cx) {
        return Err(ApiError::Json(
            403,
            "Only the instance owner can change diagnostic capture.".into(),
        ));
    }
    item(&st, &cx, &id).await?;
    let span = tracing::info_span!("http.request", method = "POST", route = "/api/items/{id}/debug",
        workspace_id = %cx.ws, user_id = %cx.me, item_id = %id, succeeded = tracing::field::Empty);
    let result = capture_authorized(st, cx, id, bytes)
        .instrument(span.clone())
        .await;
    span.record("succeeded", result.is_ok());
    result
}

async fn capture_authorized(st: AppState, cx: Ctx, id: String, bytes: Bytes) -> ApiResult {
    let input: Capture = serde_json::from_value(body(&bytes)?)
        .map_err(|_| ApiError::bad_request("Invalid diagnostic capture settings."))?;
    if input.minutes.is_some_and(|m| m == 0 || m > 60) {
        return Err(ApiError::bad_request(
            "Capture duration must be between 1 and 60 minutes.",
        ));
    }
    let (store, ws, actor, target) = (store(&st)?, cx.ws.clone(), cx.me.clone(), id.clone());
    let minutes = if input.enabled {
        input.minutes
    } else {
        Some(0)
    };
    let span = tracing::Span::current();
    super::blocking(move || {
        span.in_scope(|| {
            store
                .diagnostics()
                .capture(&ws, &target, &actor, minutes)
                .map_err(|_| ApiError::Json(503, "Could not change diagnostic capture.".into()))?;
            Ok(())
        })
    })
    .await?;
    view(&st, &cx, &id).await
}
