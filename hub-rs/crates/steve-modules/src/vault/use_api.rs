//! REST of slice V4: grants and the use log.
//!
//! - `POST /vault/items/:id/grants {bot_id, scope: {project_id}|{site}|{scratch:true}, hours: 1|8|24}`
//! - `DELETE /grants/:id` → `{ok:true}`
//! - `GET /vault/uses?item=&bot=&limit=` (`limit` ≤ 200, default 50) → `SecretUseOut[]`

use axum::extract::{Path, Query, State};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::json;

use super::grants::NewGrant;
use super::use_repo as urepo;
use super::{Vault, VaultError, VaultResult};
use crate::{ApiError, Body, Caller};

const LIMIT_MAX: i64 = 200;
const LIMIT_DEFAULT: i64 = 50;

/// The V4 routes. Merge under `/api` after `require_user`, next to the V1/V2 routes.
pub fn routes<S: Clone + Send + Sync + 'static>(vault: Vault) -> Router<S> {
    Router::new()
        .route("/vault/items/{id}/grants", post(create_grant))
        .route("/grants/{id}", delete(revoke_grant))
        .route("/vault/uses", get(uses))
        .with_state(vault)
}

async fn create_grant(
    State(v): State<Vault>,
    who: Caller,
    Path(id): Path<String>,
    Body(b): Body<NewGrant>,
) -> VaultResult<Response> {
    let out = v
        .blocking(move |v| v.create_grant(&who.ws, &who.me, &id, b))
        .await?;
    Ok(Json(out).into_response())
}

async fn revoke_grant(
    State(v): State<Vault>,
    who: Caller,
    Path(id): Path<String>,
) -> VaultResult<Response> {
    v.blocking(move |v| v.revoke_grant(&who.ws, &who.me, &id))
        .await?;
    Ok(Json(json!({ "ok": true })).into_response())
}

#[derive(Debug, Default, Deserialize)]
struct UsesParams {
    item: Option<String>,
    bot: Option<String>,
    limit: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ActorOut {
    #[serde(rename = "type")]
    pub kind: String,
    pub id: String,
    pub name: String,
}

/// `SecretUseOut`, plus `exit_code` and `finished`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SecretUseOut {
    pub id: String,
    pub item_id: String,
    pub item_title: String,
    pub actor: ActorOut,
    pub purpose: String,
    pub mode: String,
    pub status: String,
    pub work_item_id: Option<String>,
    pub exit_code: Option<i64>,
    pub created: i64,
    pub finished: Option<i64>,
}

fn blank(s: Option<String>) -> Option<String> {
    s.filter(|x| !x.trim().is_empty())
}

impl Vault {
    /// `GET /vault/uses`: uses of items the caller sees, newest first.
    pub fn list_uses(
        &self,
        ws: &str,
        user: &str,
        item: Option<&str>,
        bot: Option<&str>,
        limit: i64,
    ) -> VaultResult<Vec<SecretUseOut>> {
        let rows = self
            .store()
            .reader(|c| urepo::list_uses(c, ws, user, item, bot, limit))?;
        Ok(rows
            .into_iter()
            .map(|r| SecretUseOut {
                actor: ActorOut {
                    name: r
                        .actor_name
                        .unwrap_or_else(|| match r.u.actor_type.as_str() {
                            "steve" => "Steve".into(),
                            _ => r.u.actor_id.clone(),
                        }),
                    kind: r.u.actor_type,
                    id: r.u.actor_id,
                },
                id: r.u.id,
                item_id: r.u.item_id,
                item_title: r.item_title,
                purpose: r.u.purpose,
                mode: r.u.mode,
                status: r.u.status,
                work_item_id: r.u.work_item_id,
                exit_code: r.u.exit_code,
                created: r.u.created,
                finished: r.u.finished,
            })
            .collect())
    }
}

async fn uses(
    State(v): State<Vault>,
    who: Caller,
    Query(p): Query<UsesParams>,
) -> VaultResult<Response> {
    let limit = match blank(p.limit) {
        None => LIMIT_DEFAULT,
        Some(l) => l
            .trim()
            .parse::<i64>()
            .ok()
            .filter(|l| (1..=LIMIT_MAX).contains(l))
            .ok_or_else(|| VaultError::from(ApiError::invalid("limit")))?,
    };
    let (item, bot) = (blank(p.item), blank(p.bot));
    let out = v
        .blocking(move |v| v.list_uses(&who.ws, &who.me, item.as_deref(), bot.as_deref(), limit))
        .await?;
    Ok(Json(out).into_response())
}
