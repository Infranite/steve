//! `/api/yolo`: the workspace switch, project and item scopes, resuming after the
//! breaker (Y0), and the ledger (Y6): the digest, undo, and the cool-down buttons “现在” / “我来定”.
//! Mounted with the module routes, so the caller comes from the `Caller` layer and errors are the
//! module kind (`400 参数不对：…`, `404 没有这个…`, `409 …`).

use axum::Json;
use axum::Router;
use axum::body::Bytes;
use axum::extract::{Path, Query, State};
use axum::routing::{get, post, put};
use serde::Deserialize;
use serde_json::Value;
use steve_core::yolo::digest;
use steve_core::yolo::{Engine, Switch};
use steve_modules::{ApiError, ApiResult, Caller};

/// The routes, with their state already applied.
pub fn routes(engine: Engine) -> Router {
    Router::new()
        .route("/yolo", get(status).put(switch))
        .route("/yolo/scopes/{ty}/{id}", put(scope))
        .route("/yolo/resume", post(resume))
        .route("/yolo/actions", get(actions))
        .route("/yolo/actions/{id}/undo", post(undo))
        .route("/yolo/actions/{id}/now", post(now))
        .route("/yolo/actions/{id}/cancel", post(cancel))
        .with_state(engine)
}

/// The JSON body; an empty body is `{}`.
fn body(b: &Bytes) -> ApiResult<Value> {
    if b.iter().all(u8::is_ascii_whitespace) {
        return Ok(Value::Object(Default::default()));
    }
    serde_json::from_slice(b).map_err(|_| ApiError::invalid("body"))
}

async fn run(f: impl FnOnce() -> ApiResult<Value> + Send + 'static) -> ApiResult<Json<Value>> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| ApiError::Internal(format!("task failed: {e}")))?
        .map(Json)
}

async fn status(State(e): State<Engine>, who: Caller) -> ApiResult<Json<Value>> {
    run(move || e.yolo().status(&who.ws)).await
}

async fn switch(State(e): State<Engine>, who: Caller, b: Bytes) -> ApiResult<Json<Value>> {
    let s = Switch::parse(&body(&b)?)?;
    run(move || e.yolo().put(&who.ws, &who.me, s)).await
}

async fn scope(
    State(e): State<Engine>,
    who: Caller,
    Path((ty, id)): Path<(String, String)>,
    b: Bytes,
) -> ApiResult<Json<Value>> {
    let b = body(&b)?;
    run(move || e.yolo().put_scope(&who.ws, &who.me, &ty, &id, &b)).await
}

async fn resume(State(e): State<Engine>, who: Caller) -> ApiResult<Json<Value>> {
    run(move || e.yolo().resume(&who.ws, &who.me)).await
}

#[derive(Deserialize)]
struct ActionsQuery {
    window: Option<String>,
    item: Option<String>,
    outcome: Option<String>,
}

async fn actions(
    State(e): State<Engine>,
    who: Caller,
    Query(q): Query<ActionsQuery>,
) -> ApiResult<Json<Value>> {
    let nonempty = |s: Option<String>| s.filter(|s| !s.is_empty());
    let q = digest::Query {
        window: nonempty(q.window),
        item: nonempty(q.item),
        outcome: nonempty(q.outcome),
    };
    run(move || e.digest(&who.ws, &q)).await
}

async fn undo(
    State(e): State<Engine>,
    who: Caller,
    Path(id): Path<String>,
    b: Bytes,
) -> ApiResult<Json<Value>> {
    let b = body(&b)?;
    let text = match b.get("text") {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(_) => return Err(ApiError::invalid("text")),
    };
    run(move || {
        let a = e.undo(&who.ws, &who.me, &id, text.as_deref())?;
        e.action_json(&a)
    })
    .await
}

async fn now(
    State(e): State<Engine>,
    who: Caller,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    run(move || {
        let a = e.run_now(&who.ws, &id)?;
        e.action_json(&a)
    })
    .await
}

async fn cancel(
    State(e): State<Engine>,
    who: Caller,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    run(move || {
        let a = e.cancel(&who.ws, &id)?;
        e.action_json(&a)
    })
    .await
}
