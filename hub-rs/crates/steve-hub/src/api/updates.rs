use super::{AppState, Ctx};
use crate::{error::ApiError, upgrades::Updates};
use axum::{
    Extension, Json,
    extract::{Path, State},
};
use serde::Deserialize;
use serde_json::Value;
fn service(st: &AppState) -> Result<Updates, ApiError> {
    Updates::new(&st.cfg).map_err(err)
}
fn err(e: impl std::fmt::Display) -> ApiError {
    ApiError::bad_request(e.to_string())
}
fn owner(cx: &Ctx) -> Result<(), ApiError> {
    if cx.user.get("role").and_then(Value::as_str) == Some("owner") {
        Ok(())
    } else {
        Err(ApiError::Json(
            403,
            steve_i18n::t!("hub.upgrade.owner_only"),
        ))
    }
}
pub async fn status(
    State(st): State<AppState>,
    Extension(cx): Extension<Ctx>,
) -> Result<Json<crate::upgrades::UpdateStatus>, ApiError> {
    let u = service(&st)?;
    let is_owner = owner(&cx).is_ok();
    let mut v = u.status().map_err(err)?;
    v.capability.can_execute = u.execution_reason().is_none() && is_owner;
    v.capability.owner = is_owner;
    let install = u.install(&st.cfg.steve_repo);
    if is_owner {
        v.install = crate::upgrades::InstallView::owner(install);
    } else {
        // paths on this machine are the instance manager's
        v.install = crate::upgrades::InstallView::redacted(install.kind, install.legacy_service);
        v.backup = None;
        v.capability.reason = steve_i18n::t!("hub.upgrade.owner_only_reason");
        if let Some(op) = v.operation.as_mut() {
            op.redact_paths();
        }
        for op in &mut v.history {
            op.redact_paths();
        }
    }
    Ok(Json(v))
}
/// Owner only: a check spends the instance's share of GitHub's unauthenticated rate limit.
pub async fn check(
    State(st): State<AppState>,
    Extension(cx): Extension<Ctx>,
) -> Result<Json<crate::upgrades::Check>, ApiError> {
    owner(&cx)?;
    Ok(Json(service(&st)?.check().await.map_err(err)?))
}
#[derive(Deserialize)]
pub struct Request {
    version: String,
    idempotency_key: String,
}
pub async fn request(
    State(st): State<AppState>,
    Extension(cx): Extension<Ctx>,
    Json(body): Json<Request>,
) -> Result<(axum::http::StatusCode, Json<crate::upgrades::Operation>), ApiError> {
    owner(&cx)?;
    let u = service(&st)?;
    let op = u
        .request(&body.version, &body.idempotency_key, &cx.me, true)
        .map_err(err)?;
    if op.phase == "staging" {
        u.start(op.clone())
    };
    Ok((axum::http::StatusCode::ACCEPTED, Json(op)))
}
pub async fn get(
    State(st): State<AppState>,
    Path(id): Path<String>,
    Extension(cx): Extension<Ctx>,
) -> Result<Json<crate::upgrades::Operation>, ApiError> {
    let mut op = service(&st)?.operation(&id).map_err(err)?;
    if owner(&cx).is_err() {
        op.previous = None;
        op.target = None;
        op.runner = None;
        op.backup_snapshot = None;
        op.drain = None;
    }
    Ok(Json(op))
}
pub async fn approve(
    State(st): State<AppState>,
    Extension(cx): Extension<Ctx>,
    Path(id): Path<String>,
) -> Result<(axum::http::StatusCode, Json<crate::upgrades::Operation>), ApiError> {
    owner(&cx)?;
    let u = service(&st)?;
    let op = u.approve(&id, &cx.me).map_err(err)?;
    u.start(op.clone());
    Ok((axum::http::StatusCode::ACCEPTED, Json(op)))
}

pub async fn reject(
    State(st): State<AppState>,
    Extension(cx): Extension<Ctx>,
    Path(id): Path<String>,
) -> Result<Json<crate::upgrades::Operation>, ApiError> {
    owner(&cx)?;
    Ok(Json(service(&st)?.reject(&id).map_err(err)?))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn workspace_owner_does_not_grant_instance_upgrade() {
        let mut cx = Ctx {
            user: serde_json::from_value(json!({"id":"u","role":"member"})).unwrap(),
            me: "u".into(),
            ws: "owned-workspace".into(),
            client: None,
        };
        assert!(owner(&cx).is_err());
        cx.user.insert("role".into(), json!("owner"));
        assert!(owner(&cx).is_ok());
    }
}

/// Routes mounted behind the shared API login and body-limit layers.
pub(crate) fn routes() -> axum::Router<super::AppState> {
    use super::*;
    Router::new()
        .route("/updates", get(updates::status).post(updates::request))
        .route("/updates/check", post(updates::check))
        .route("/updates/{id}", get(updates::get))
        .route("/updates/{id}/approve", post(updates::approve))
        .route("/updates/{id}/reject", post(updates::reject))
}
