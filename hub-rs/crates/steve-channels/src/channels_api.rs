//! REST of channel accounts and space members, relative to `/api`,
//! behind the `Caller` layer:
//!
//! | route | |
//! | --- | --- |
//! | `GET /channels` | `{providers: [{system, name, features, adapter_installed}], accounts: ChannelAccount[]}` |
//! | `GET /channels/:connection_id` | `ChannelAccount` with `spaces: Space[]` (short form) instead of the count |
//! | `POST /channels` | a registered channel side's adapter: credential form `{adapter, fields:{…}, persona_id?, config?, app_connection?}` → `ChannelAccount`; OAuth `{adapter, app_connection?, caps?, config?}` → `{url}` |
//! | `PATCH /channels/:connection_id` | `{duty?: ≤4000, rules?, trigger?: {on?, words?, where?, mark?}, persona_id?}` → `ChannelAccount` |
//! | `GET /spaces/:id/members?offset=&limit=` | `{total, offset, limit, members: Member[]}` (limit ≤200, default 50) |
//! | `POST /channels/cleanup/preview`, `POST /channels/cleanup`, `GET /channels/cleanup/leftovers` | see [`crate::cleanup`] |
//!
//! `ChannelAccount` (see [`crate::channel_accounts::AccountOut`]) carries the permission check of
//! `verify`: `permissions`, `missing_permissions`, `self_built`, `permissions_checked_at`.
//!
//! Mount: `api.merge(steve_channels::channels_api::routes(channels.clone()))`.

use std::collections::BTreeMap;

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

use steve_modules::accounts::connections::{AuthKind, PAIR_KEY};

use crate::channel_accounts::{self as accounts, ChannelPatch};
use crate::spaces::Mode;
use crate::{Channels, members, spaces};

#[derive(Clone)]
struct Api {
    ch: Channels,
}

type Cx = Extension<Api>;

/// The channel account and member routes.
pub fn routes(ch: Channels) -> Router<Modules> {
    Router::new()
        .route("/channels", get(list).post(create))
        .route("/channels/{id}", get(detail).patch(patch))
        .route("/spaces/{id}/members", get(members_page))
        .layer(Extension(Api { ch: ch.clone() }))
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
            let trigger = spaces::your_trigger(c, &v.ws)?;
            let list: Vec<Value> = spaces::list(c, v, Some(id), None)?
                .iter()
                .map(|s| -> ApiResult<Value> {
                    let set = spaces::settings(c, &s.id)?;
                    let full = spaces::out_with(c, s, &trigger)?;
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
    /// The connection this one pairs with ([`PAIR_KEY`]; default: the only candidate).
    #[serde(default)]
    app_connection: Option<String>,
    #[serde(default)]
    caps: Option<Vec<String>>,
}

/// `POST /channels`: a connection of a registered channel side, made the way its provider is
/// authorized: a credential form (`fields`) makes the connection at once, OAuth answers the URL to
/// send you to. A pair (`app_connection`) is checked like on every connect path.
async fn create(
    Extension(a): Cx,
    who: Caller,
    headers: HeaderMap,
    Body(b): Body<CreateBody>,
) -> VaultResult<Json<Value>> {
    let p =
        a.ch.connections()
            .provider(&b.adapter)
            .filter(|_| accounts::is_channel_adapter(&a.ch, &b.adapter))
            .ok_or_else(|| ApiError::invalid("adapter"))?;
    let mode = match a.ch.decl(&b.adapter) {
        Some(d) => d.mode,
        None if p.integration().identity == "self" => Mode::Own,
        None => Mode::Bot,
    };
    let mut config = b.config.clone().unwrap_or(Value::Null);
    if let Some(pair) = b.app_connection.as_deref().filter(|x| !x.trim().is_empty()) {
        if !config.is_object() {
            config = json!({});
        }
        config[PAIR_KEY] = json!(pair);
    }
    match p.auth() {
        AuthKind::Credential => create_form(&a, &who, b, mode, config).await,
        AuthKind::OAuth => start_oauth(&a, &who, &headers, b, config).await,
        _ => Err(ApiError::invalid("adapter").into()),
    }
}

async fn create_form(
    a: &Api,
    who: &Caller,
    b: CreateBody,
    mode: Mode,
    config: Value,
) -> VaultResult<Json<Value>> {
    if b.caps.is_some() {
        return Err(ApiError::invalid("caps").into());
    }
    let persona = b.persona_id.filter(|p| !p.trim().is_empty());
    if persona.is_some() && mode == Mode::Own {
        return Err(ApiError::invalid("persona_id").into());
    }
    let v = viewer(&a.ch, who)?;
    // checked before anything is created at the provider's side or in the vault
    if let Some(p) = &persona {
        a.ch.store().reader(|c| crate::personas::own(c, &v.ws, p))?;
    }
    let conn =
        a.ch.connections()
            .create(who, &b.adapter, &b.fields, config)
            .await?;
    let now = a.ch.now();
    a.ch.store()
        .transaction(|tx| accounts::created(tx, &v, &conn.id, mode, persona.as_deref(), now))?;
    Ok(Json(account_json(&a.ch, &v, &conn.id, false)?))
}

async fn start_oauth(
    a: &Api,
    who: &Caller,
    headers: &HeaderMap,
    b: CreateBody,
    config: Value,
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
    let origin = conns.origin(headers);
    let url = conns
        .oauth_start(who, &origin, &b.adapter, b.caps, config, None)
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
