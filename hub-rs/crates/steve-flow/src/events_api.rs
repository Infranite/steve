//! REST of signals and sources, relative to `/api`:
//!
//! | route | |
//! | --- | --- |
//! | `GET /signals?range=today\|7d&q=&source=&outcome=` | `{stats, items}` |
//! | `GET /signals/:id` | `Signal` + `runs` |
//! | `GET /signals/:id/raw` | `{payload}`; `410` after the retention period |
//! | `GET /sources` | `Source[]` |
//! | `PATCH /sources/:adapter` | `Source` |
//! | `POST /sources/:adapter/refresh-scopes` | `Source` |

use std::sync::Arc;

use axum::extract::{Path, Query};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use steve_modules::{ApiError, ApiResult, Body, Caller, Modules};

use crate::FlowCx;
use crate::cron::DEFAULT_TZ;
use crate::signals::{self, Filter, RAW_GONE, Range};
use crate::sources::{self, SourcePatch};

type Cx = Extension<Arc<FlowCx>>;

pub fn routes() -> Router<Modules> {
    Router::new()
        .route("/signals", get(list))
        .route("/signals/{id}", get(detail))
        .route("/signals/{id}/raw", get(raw))
        .route("/sources", get(source_list))
        .route("/sources/{adapter}", axum::routing::patch(source_patch))
        .route("/sources/{adapter}/refresh-scopes", post(refresh))
}

#[derive(Debug, Default, Deserialize)]
struct ListQuery {
    range: Option<String>,
    q: Option<String>,
    source: Option<String>,
    outcome: Option<String>,
}

async fn list(
    Extension(cx): Cx,
    who: Caller,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<Value>> {
    let range = Range::parse(q.range.as_deref()).ok_or_else(|| ApiError::invalid("range"))?;
    if q.q.as_deref().is_some_and(|s| s.chars().count() > 200) {
        return Err(ApiError::invalid("q"));
    }
    let viewer = cx.modules.viewer(&who)?;
    let f = Filter {
        since: range.since(cx.now(), DEFAULT_TZ),
        q: q.q,
        source: q.source,
        outcome: q.outcome,
    };
    Ok(Json(cx.store().reader(|c| signals::list(c, &viewer, &f))?))
}

async fn detail(Extension(cx): Cx, who: Caller, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let viewer = cx.modules.viewer(&who)?;
    Ok(Json(
        cx.store().reader(|c| signals::detail(c, &viewer, &id))?,
    ))
}

async fn raw(Extension(cx): Cx, who: Caller, Path(id): Path<String>) -> ApiResult<Response> {
    let viewer = cx.modules.viewer(&who)?;
    Ok(
        match cx.store().reader(|c| signals::raw(c, &viewer, &id))? {
            Some(p) => Json(json!({ "payload": p })).into_response(),
            None => (StatusCode::GONE, Json(json!({ "error": RAW_GONE }))).into_response(),
        },
    )
}

async fn source_list(Extension(cx): Cx, who: Caller) -> ApiResult<Json<Value>> {
    Ok(Json(serde_json::to_value(sources::list(&cx, &who)?)?))
}

async fn source_patch(
    Extension(cx): Cx,
    who: Caller,
    Path(adapter): Path<String>,
    Body(p): Body<SourcePatch>,
) -> ApiResult<Json<Value>> {
    let v = sources::patch(&cx, &who, &adapter, &p)?;
    Ok(Json(serde_json::to_value(v)?))
}

async fn refresh(
    Extension(cx): Cx,
    who: Caller,
    Path(adapter): Path<String>,
) -> ApiResult<Json<Value>> {
    let v = sources::refresh_scopes(&cx, &who, &adapter).await?;
    Ok(Json(serde_json::to_value(v)?))
}
