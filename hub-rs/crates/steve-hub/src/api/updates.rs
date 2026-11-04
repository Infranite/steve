use super::{AppState, Ctx};
use crate::{error::ApiError, upgrades::Updates};
use axum::{
    Extension, Json,
    extract::{Path, State},
};
use serde::Deserialize;
use serde_json::{Value, json};
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
        Err(ApiError::Json(403, "仅实例管理员可升级 Steve".into()))
    }
}
pub async fn status(
    State(st): State<AppState>,
    Extension(cx): Extension<Ctx>,
) -> Result<Json<Value>, ApiError> {
    let u = service(&st)?;
    let mut v = u.status().map_err(err)?;
    v["capability"]["can_execute"] = json!(u.execution_reason().is_none() && owner(&cx).is_ok());
    v["install"] = json!(u.install(&st.cfg.steve_repo));
    if owner(&cx).is_err() {
        // paths on this machine are the instance manager's
        v["install"] = json!({"kind": v["install"]["kind"]});
        v["backup"] = Value::Null;
        v["capability"]["reason"] = json!("仅实例管理员可批准和执行升级");
        if v["operation"].is_object() {
            v["operation"]["previous"] = Value::Null;
            v["operation"]["target"] = Value::Null;
        }
    };
    Ok(Json(v))
}
pub async fn check(State(st): State<AppState>) -> Result<Json<crate::upgrades::Check>, ApiError> {
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
        u.spawn_stage(op.clone())
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
    u.spawn_stage(op.clone());
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
    #[test]
    fn workspace_owner_does_not_grant_instance_upgrade() {
        let mut cx = Ctx {
            user: serde_json::from_value(json!({"id":"u","role":"member"})).unwrap(),
            me: "u".into(),
            ws: "owned-workspace".into(),
        };
        assert!(owner(&cx).is_err());
        cx.user.insert("role".into(), json!("owner"));
        assert!(owner(&cx).is_ok());
    }
}
