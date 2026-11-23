//! Owner approval policy and the existing applied/desired mode fact, scoped to the workspace.
use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, State},
    routing::get,
};
use serde::Deserialize;
use serde_json::{Value, json};
use steve_core::AgentRuntime;
use steve_modules::{ApiError, ApiResult, Caller};
use steve_store::{Store, StoreError, approval};
use steve_types::ApprovalMode;

use crate::wire::{approval::ApprovalModes, core::RoutedRuntime};

#[derive(Clone)]
struct Api {
    db: Store,
    runtime: Arc<RoutedRuntime>,
    modes: ApprovalModes,
    diagnostic: bool,
}

pub(crate) fn routes(
    db: Store,
    runtime: Arc<RoutedRuntime>,
    modes: ApprovalModes,
    diagnostic: bool,
) -> Router {
    Router::new()
        .route("/approval-policy", get(get_policy).put(set))
        .route("/approval-policy/sessions/{id}", get(session))
        .with_state(Api {
            db,
            runtime,
            modes,
            diagnostic,
        })
}

fn error(e: StoreError) -> ApiError {
    match e {
        StoreError::Conflict(s) => ApiError::conflict(s),
        StoreError::Rejected(s) => ApiError::Forbidden(s),
        _ => ApiError::Internal(e.to_string()),
    }
}

async fn run(f: impl FnOnce() -> ApiResult<Value> + Send + 'static) -> ApiResult<Json<Value>> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .map(Json)
}

async fn get_policy(State(api): State<Api>, who: Caller) -> ApiResult<Json<Value>> {
    run(move || {
        api.db
            .read(|r| {
                Ok(json!({
                    "policy": approval::policy(r, &who.ws)?,
                    "editable": !api.diagnostic && approval::owner(r, &who.ws, &who.me)?,
                }))
            })
            .map_err(error)
    })
    .await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Update {
    revision: i64,
    mode: ApprovalMode,
}

async fn set(
    State(api): State<Api>,
    who: Caller,
    Json(body): Json<Update>,
) -> ApiResult<Json<Value>> {
    run(move || {
        if api.diagnostic {
            return Err(ApiError::Forbidden("diagnostic copy is read-only".into()));
        }
        // Transaction binds the requested revision and owner. Session config is durable, goes after
        // the active turn, and is also reconciled from policy at the next idle/open boundary.
        let policy = api
            .db
            .write(|w| approval::set(w, &who.ws, &who.me, body.revision, body.mode))
            .map_err(error)?;
        for s in api.db.sessions().list(Some(&who.ws)).map_err(error)? {
            if let Err(e) = api.runtime.sync_approval(&s.id) {
                tracing::warn!(session = %s.id, %e, "approval policy will be reconciled before the next turn");
            }
        }
        Ok(json!({ "policy": policy, "editable": true }))
    }).await
}

async fn session(
    State(api): State<Api>,
    who: Caller,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    run(move || {
        let row = api
            .db
            .sessions()
            .get(&id)
            .map_err(error)?
            .filter(|s| s.workspace_id == who.ws)
            .ok_or_else(|| ApiError::not_found("session"))?;
        let desired = api.modes.desired(&api.db, &id).map_err(error)?;
        // An ended or imported read-only session cannot apply a policy, but that does not
        // mean its adapter lacks native auto mode.
        let native_id = match &desired {
            Some(d) => d.native_id.clone(),
            None => api.modes.native_mode(&row.harness),
        };
        let actual = api.runtime.mode_state(&id);
        Ok(json!({
            "status": match desired {
                Some(ref d) if d.mode == steve_node_api::PermissionMode::Auto => "on",
                _ if native_id.is_some() => "off",
                _ => "unsupported",
            },
            "target": desired.as_ref().map(|d| d.mode),
            "native_id": native_id,
            "applied": actual.as_ref().and_then(|m| m.applied.as_deref()),
            "desired": actual.as_ref().and_then(|m| m.desired.as_deref()),
            "refused": actual.as_ref().and_then(|m| m.refused.as_deref()),
            "unknown": actual.as_ref().is_some_and(|m| m.unknown),
            "released": actual.as_ref().is_some_and(|m| m.released),
        }))
    })
    .await
}
