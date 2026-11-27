//! Owner permission policy and the per-session effective/desired mode fact, scoped to the workspace.
use std::sync::Arc;

use axum::{
    Json, Router,
    extract::{Path, State},
    routing::get,
};
use serde::{Deserialize, Serialize};
use steve_core::AgentRuntime;
use steve_modules::{ApiError, ApiResult, Caller};
use steve_store::{Store, StoreError, approval};
use steve_types::{
    ApprovalMode, PermissionPolicyView, SessionPermissionStatus, SessionPermissionView,
};

use crate::harnesses::HubHarnesses;
use crate::wire::{approval::ApprovalModes, core::RoutedRuntime};

#[derive(Clone)]
struct Api {
    db: Store,
    runtime: Arc<RoutedRuntime>,
    modes: ApprovalModes,
    harnesses: Arc<HubHarnesses>,
    diagnostic: bool,
}

pub(crate) fn routes(
    db: Store,
    runtime: Arc<RoutedRuntime>,
    modes: ApprovalModes,
    harnesses: Arc<HubHarnesses>,
    diagnostic: bool,
) -> Router {
    Router::new()
        .route("/permission-policy", get(get_policy).put(set))
        .route("/permission-policy/sessions/{id}", get(session))
        .with_state(Api {
            db,
            runtime,
            modes,
            harnesses,
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

async fn run<T: Serialize + Send + 'static>(
    f: impl FnOnce() -> ApiResult<T> + Send + 'static,
) -> ApiResult<Json<T>> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| ApiError::Internal(e.to_string()))?
        .map(Json)
}

async fn get_policy(State(api): State<Api>, who: Caller) -> ApiResult<Json<PermissionPolicyView>> {
    run(move || {
        let (policy, editable) = api
            .db
            .read(|r| {
                Ok((
                    approval::policy(r, &who.ws)?,
                    !api.diagnostic && approval::owner(r, &who.ws, &who.me)?,
                ))
            })
            .map_err(error)?;
        let discovered = api.harnesses.list();
        let (harnesses, unconfined_harnesses) = api.modes.harness_summary(&discovered);
        Ok(PermissionPolicyView {
            policy,
            editable,
            harnesses,
            unconfined_harnesses,
        })
    })
    .await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Update {
    revision: i64,
    mode: ApprovalMode,
    #[serde(default)]
    confirmed: bool,
}

async fn set(
    State(api): State<Api>,
    who: Caller,
    Json(body): Json<Update>,
) -> ApiResult<Json<PermissionPolicyView>> {
    run(move || {
        if api.diagnostic {
            return Err(ApiError::Forbidden("diagnostic copy is read-only".into()));
        }
        if body.mode == ApprovalMode::FullAccess && !body.confirmed {
            return Err(ApiError::Invalid(vec!["confirmed".into()]));
        }
        // Transaction binds the requested revision and owner. Session config is durable, goes after
        // the active turn, and is also reconciled from policy at the next idle/open boundary.
        let policy = api
            .db
            .write(|w| approval::set(w, &who.ws, &who.me, body.revision, body.mode))
            .map_err(error)?;
        if policy.mode == ApprovalMode::FullAccess && policy.revision != body.revision {
            ::metrics::counter!("steve_permission_full_access_confirmations_total").increment(1);
        }
        for s in api.db.sessions().list(Some(&who.ws)).map_err(error)? {
            if let Err(e) = api.runtime.sync_approval(&s.id) {
                tracing::warn!(session = %s.id, %e, "permission policy will be reconciled before the next turn");
            }
        }
        let discovered = api.harnesses.list();
        let (harnesses, unconfined_harnesses) = api.modes.harness_summary(&discovered);
        Ok(PermissionPolicyView {
            policy,
            editable: true,
            harnesses,
            unconfined_harnesses,
        })
    })
    .await
}

async fn session(
    State(api): State<Api>,
    who: Caller,
    Path(id): Path<String>,
) -> ApiResult<Json<SessionPermissionView>> {
    run(move || {
        let row = api
            .db
            .sessions()
            .get(&id)
            .map_err(error)?
            .filter(|s| s.workspace_id == who.ws)
            .ok_or_else(|| ApiError::not_found("session"))?;
        let desired = api.modes.desired(&api.db, &id).map_err(error)?;
        // An ended or imported read-only session cannot apply a policy, but still reports
        // whether its harness supports the workspace's current permission level.
        let (policy, policy_supported, native_id) = match &desired {
            Some(d) => (d.policy, d.policy_supported, d.native_id.clone()),
            None => {
                let policy = api
                    .db
                    .read(|r| approval::policy(r, &who.ws))
                    .map_err(error)?
                    .mode;
                let (supported, id) = api.modes.policy_support(&row.harness, policy);
                (policy, supported, id)
            }
        };
        let actual = api.runtime.mode_state(&id);
        let target = desired.as_ref().map(|d| d.mode.as_str().to_owned());
        let unknown = actual.as_ref().is_some_and(|m| m.unknown);
        let released = actual.as_ref().is_some_and(|m| m.released);
        let applied = actual.as_ref().and_then(|m| m.applied.clone());
        let effective_applied = (!unknown && !released).then(|| applied.clone()).flatten();
        let desired_mode = actual.as_ref().and_then(|m| m.desired.clone());
        let refused = actual.as_ref().and_then(|m| m.refused.clone());
        let status = if !policy_supported {
            SessionPermissionStatus::Unsupported
        } else if refused.is_some() && (target.is_none() || effective_applied != target) {
            SessionPermissionStatus::Refused
        } else if target.is_none() || effective_applied == target {
            SessionPermissionStatus::Applied
        } else {
            SessionPermissionStatus::Pending
        };
        Ok(SessionPermissionView {
            policy,
            status,
            target,
            native_id,
            applied,
            desired: desired_mode,
            refused,
            unknown,
            released,
        })
    })
    .await
}
