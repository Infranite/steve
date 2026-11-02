//! REST of channel accounts (channels-personas.md §5.2) and space members, relative to `/api`,
//! behind the `Caller` layer:
//!
//! | route | |
//! | --- | --- |
//! | `GET /channels` | `{providers: [{system, name, features, adapter_installed}], accounts: ChannelAccount[]}` |
//! | `GET /channels/:connection_id` | `ChannelAccount` with `spaces: Space[]` (short form) instead of the count |
//! | `POST /channels` | `{adapter:"feishu_app", fields:{…}, persona_id?, config?}` → `ChannelAccount`; `{adapter:"feishu_user", app_connection?, caps?}` → `{url}` (OAuth) |
//! | `PATCH /channels/:connection_id` | `{duty?: ≤4000, rules?, trigger?: {on?, words?, where?, mark?}, persona_id?}` → `ChannelAccount` |
//! | `GET /spaces/:id/members?offset=&limit=` | `{total, offset, limit, members: Member[]}` (limit ≤200, default 50) |
//! | `POST /channels/cleanup/preview`, `POST /channels/cleanup`, `GET /channels/cleanup/leftovers` | see [`crate::cleanup`] |
//!
//! `ChannelAccount` (see [`crate::channel_accounts::AccountOut`]) carries the permission check of
//! `verify`: `permissions`, `missing_permissions`, `self_built`, `permissions_checked_at`.
//!
//! Mount: `api.merge(steve_channels::channels_api::routes(channels.clone(), Some(feishu_user.clone())))`.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::{Path, Query};
use axum::http::HeaderMap;
use axum::routing::get;
use axum::{Extension, Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use steve_modules::vault::VaultResult;
use steve_modules::{ApiError, ApiResult, Body, Caller, Modules};
use steve_store::base::Viewer;
use zeroize::Zeroizing;

use crate::adapters::feishu::FeishuUser;
use crate::adapters::feishu::auth::{APP_ADAPTER, USER_ADAPTER};
use crate::adapters::feishu::user_oauth::SELF_CLAIM;
use crate::channel_accounts::{self as accounts, ChannelPatch};
use crate::{Channels, members, spaces};

#[derive(Clone)]
struct Api {
    ch: Channels,
    user: Option<Arc<FeishuUser>>,
}

type Cx = Extension<Api>;

/// The channel account and member routes. `user`: the `feishu_user` provider (the one registered
/// with `Connections`), so “授权你本人” through `POST /channels` also merges the contact the bot
/// already made for you into “我”; without it the OAuth flow runs without that merge.
pub fn routes(ch: Channels, user: Option<Arc<FeishuUser>>) -> Router<Modules> {
    Router::new()
        .route("/channels", get(list).post(create))
        .route("/channels/{id}", get(detail).patch(patch))
        .route("/spaces/{id}/members", get(members_page))
        .layer(Extension(Api {
            ch: ch.clone(),
            user,
        }))
        .merge(crate::cleanup::routes(ch))
}

fn viewer(ch: &Channels, who: &Caller) -> ApiResult<Viewer> {
    ch.modules().viewer(who)
}

async fn list(Extension(a): Cx, who: Caller) -> ApiResult<Json<Value>> {
    let v = viewer(&a.ch, &who)?;
    let accounts = a.ch.store().reader(|c| accounts::list(&a.ch, c, &v))?;
    Ok(Json(json!({
        "providers": accounts::providers(&a.ch),
        "accounts": accounts,
    })))
}

fn account_json(ch: &Channels, v: &Viewer, id: &str, with_spaces: bool) -> ApiResult<Value> {
    ch.store().reader(|c| -> ApiResult<Value> {
        let acc = accounts::out(ch, c, v, id)?;
        let mut out = serde_json::to_value(acc)?;
        if with_spaces {
            let list: Vec<Value> = spaces::list(c, v, Some(id), None)?
                .iter()
                .map(|s| -> ApiResult<Value> {
                    let set = spaces::settings(c, &s.id)?;
                    let full = spaces::out(c, s)?;
                    Ok(json!({
                        "id": s.id, "kind": s.kind, "name": s.name, "title": full.title, "self_chat": full.self_chat,
                        "attention": set.attention, "member_count": full.member_count,
                        "has_external": s.has_external, "status": s.status,
                        "persona": crate::bindings::space_persona(c, s)?,
                        "muted": set.muted, "configured": set.configured, "own_rules": set.rules.is_some(),
                        "flow_id": set.flow_id, "last_message": s.last_message,
                    }))
                })
                .collect::<ApiResult<_>>()?;
            out["spaces"] = Value::Array(list);
        }
        Ok(out)
    })
}

async fn detail(Extension(a): Cx, who: Caller, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let v = viewer(&a.ch, &who)?;
    Ok(Json(account_json(&a.ch, &v, &id, true)?))
}

async fn patch(
    Extension(a): Cx,
    who: Caller,
    Path(id): Path<String>,
    Body(p): Body<ChannelPatch>,
) -> ApiResult<Json<Value>> {
    let v = viewer(&a.ch, &who)?;
    let now = a.ch.now();
    a.ch.store()
        .transaction(|tx| accounts::patch(&a.ch, tx, &v, &id, &p, now))?;
    Ok(Json(account_json(&a.ch, &v, &id, false)?))
}

/// `POST /channels`. No `Debug`: `fields` holds secrets.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateBody {
    adapter: String,
    #[serde(default)]
    fields: BTreeMap<String, Zeroizing<String>>,
    #[serde(default)]
    config: Option<Value>,
    #[serde(default)]
    persona_id: Option<String>,
    /// `feishu_user`: the bot connection whose app you authorize (default: the only one).
    #[serde(default)]
    app_connection: Option<String>,
    #[serde(default)]
    caps: Option<Vec<String>>,
}

async fn create(
    Extension(a): Cx,
    who: Caller,
    headers: HeaderMap,
    Body(b): Body<CreateBody>,
) -> VaultResult<Json<Value>> {
    match b.adapter.as_str() {
        APP_ADAPTER => create_bot(&a, &who, b).await,
        USER_ADAPTER => start_user(&a, &who, &headers, b).await,
        _ => Err(ApiError::invalid("adapter").into()),
    }
}

async fn create_bot(a: &Api, who: &Caller, b: CreateBody) -> VaultResult<Json<Value>> {
    if b.app_connection.is_some() || b.caps.is_some() {
        return Err(ApiError::invalid(if b.caps.is_some() {
            "caps"
        } else {
            "app_connection"
        })
        .into());
    }
    let v = viewer(&a.ch, who)?;
    let persona = b.persona_id.filter(|p| !p.trim().is_empty());
    // checked before anything is created at Feishu's side or in the vault
    if let Some(p) = &persona {
        a.ch.store().reader(|c| crate::personas::own(c, &v.ws, p))?;
    }
    let conn =
        a.ch.connections()
            .create(who, APP_ADAPTER, &b.fields, b.config.unwrap_or(Value::Null))
            .await?;
    let now = a.ch.now();
    a.ch.store()
        .transaction(|tx| accounts::created_bot(tx, &v, &conn.id, persona.as_deref(), now))?;
    Ok(Json(account_json(&a.ch, &v, &conn.id, false)?))
}

async fn start_user(
    a: &Api,
    who: &Caller,
    headers: &HeaderMap,
    b: CreateBody,
) -> VaultResult<Json<Value>> {
    if !b.fields.is_empty() || b.persona_id.is_some() {
        return Err(ApiError::invalid(if b.persona_id.is_some() {
            "persona_id"
        } else {
            "fields"
        })
        .into());
    }
    let conns = a.ch.connections();
    let app = match b.app_connection.filter(|x| !x.trim().is_empty()) {
        Some(id) => {
            let c = conns
                .get(who, &id)
                .map_err(|_| ApiError::not_found("连接"))?;
            if c.adapter != APP_ADAPTER {
                return Err(ApiError::invalid("app_connection").into());
            }
            c.id
        }
        None => {
            let bots: Vec<String> = conns
                .list(who)?
                .into_iter()
                .filter(|c| c.adapter == APP_ADAPTER && c.status != "revoked")
                .map(|c| c.id)
                .collect();
            match bots.as_slice() {
                [one] => one.clone(),
                [] => return Err(ApiError::rule("先接入飞书应用机器人，再授权你本人").into()),
                _ => return Err(ApiError::invalid("app_connection").into()),
            }
        }
    };
    let mut config = json!({"app_connection": app});
    if let Some(u) = &a.user {
        config[SELF_CLAIM] = json!(u.claims().issue(who, a.ch.now())?);
    }
    let origin = conns.origin(headers);
    let url = conns
        .oauth_start(who, &origin, USER_ADAPTER, b.caps, config, None)
        .await?;
    Ok(Json(json!({ "url": url })))
}

#[derive(Debug, Default, Deserialize)]
struct MembersQ {
    offset: Option<String>,
    limit: Option<String>,
}

fn num(s: Option<String>, field: &str, ok: impl Fn(usize) -> bool) -> ApiResult<Option<usize>> {
    match s.map(|s| s.trim().to_string()).filter(|s| !s.is_empty()) {
        None => Ok(None),
        Some(s) => s
            .parse::<usize>()
            .ok()
            .filter(|n| ok(*n))
            .map(Some)
            .ok_or_else(|| ApiError::invalid(field)),
    }
}

async fn members_page(
    Extension(a): Cx,
    who: Caller,
    Path(id): Path<String>,
    Query(q): Query<MembersQ>,
) -> ApiResult<Json<Value>> {
    let offset = num(q.offset, "offset", |_| true)?.unwrap_or(0);
    let limit =
        num(q.limit, "limit", |n| (1..=members::PAGE_MAX).contains(&n))?.unwrap_or(members::PAGE);
    let v = viewer(&a.ch, &who)?;
    let page =
        a.ch.store()
            .reader(|c| members::page(c, &v, &id, offset, limit))?;
    Ok(Json(serde_json::to_value(page)?))
}
