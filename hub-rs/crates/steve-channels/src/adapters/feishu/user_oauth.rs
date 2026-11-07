//! You on Feishu: the identity side of the `feishu_user`
//! connection.
//!
//! The user OAuth round-trip proves which Feishu user (`union_id`) the signed-in Steve user is, so
//! their account goes to “我” — and the bot, which sees the same `union_id` (and, being the same app,
//! the same `open_id`) as the sender of your messages, must land on that same contact:
//!
//! - authorized first, seen by the bot later: the account is “我”'s, [`identify_sender`] (A1
//!   `identify`, step 1) finds it;
//! - seen by the bot first: `identify` made an `unconfirmed` contact for that `union_id`. When you then
//!   authorize, the connection framework merges that contact into “我” before the account is
//!   attached (`steve_modules::accounts::claim`, run for every provider that proves who signed in).

use axum::http::HeaderMap;
use axum::routing::post;
use axum::{Extension, Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use steve_modules::accounts::connections::Connections;
use steve_modules::accounts::connections::EXTRA_SCOPES;
use steve_modules::accounts::identify::{ExtraHandle, Hints, Identified, identify};
use steve_modules::vault::VaultResult;
use steve_modules::{ApiError, ApiResult, Caller, Modules};
use steve_store::Tx;

use super::auth::{USER_ADAPTER, USER_SYSTEM};

/// The ids Feishu gives a sender (`event.sender.sender_id`, `user_info`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FeishuIds {
    pub union_id: String,
    pub open_id: String,
}

/// Who sent a message the bot (connection `bot_connection`) received: A1 `identify` on system
/// `feishu` with the `union_id` as the handle and the `open_id` as an extra handle under the bot's
/// connection (a message with only the `open_id` still matches). Your own messages land on “我”
/// once you authorized; `actor` is the bot connection's owner.
pub fn identify_sender(
    tx: &Tx,
    ws: &str,
    actor: &str,
    ids: &FeishuIds,
    bot_connection: Option<&str>,
    display_name: Option<&str>,
) -> ApiResult<Identified> {
    let (handle, extra) = match (ids.union_id.trim(), ids.open_id.trim()) {
        ("", "") => return Err(ApiError::invalid("sender_id")),
        ("", open) => (open, None),
        (union, "") => (union, None),
        (union, open) => (union, Some(open)),
    };
    let hints = Hints {
        display_name: display_name.map(String::from),
        extra: extra
            .map(|h| ExtraHandle {
                handle: h.to_string(),
                connection_id: bot_connection.map(String::from),
            })
            .into_iter()
            .collect(),
        source_label: Some("飞书".into()),
        ..Default::default()
    };
    identify(tx, ws, actor, USER_SYSTEM, handle, &hints)
}

// ---------------------------------------------------------------------------------------------
// Start routes.

#[derive(Clone)]
struct UserFlows {
    conns: Connections,
}

#[derive(Deserialize)]
struct OAuthStartBody {
    app_connection: String,
    #[serde(default)]
    caps: Option<Vec<String>>,
    /// Scopes to ask for on top of the usual ones (re-authorizing for what a call was refused).
    #[serde(default)]
    scopes: Vec<String>,
}

/// `POST /channels/feishu/user/oauth/start {app_connection, caps?, scopes?}` → `{url}`: the
/// “授权你本人” and “重新授权” buttons (`scopes`: the missing ones, asked for besides the usual). Like
/// `/connections/oauth/start` for `feishu_user`, with the scopes. Relative to `/api`, behind the
/// `Caller` layer.
pub fn user_routes(conns: Connections) -> Router<Modules> {
    Router::new()
        .route("/channels/feishu/user/oauth/start", post(oauth_start))
        .layer(Extension(UserFlows { conns }))
}

async fn oauth_start(
    Extension(f): Extension<UserFlows>,
    who: Caller,
    headers: HeaderMap,
    body: steve_modules::Body<OAuthStartBody>,
) -> VaultResult<Json<Value>> {
    let b = body.0;
    // the app connection must be visible to the caller, in their workspace
    f.conns
        .get(&who, &b.app_connection)
        .map_err(|_| ApiError::not_found("连接"))?;
    let origin = f.conns.origin(&headers);
    let url = f
        .conns
        .oauth_start(
            &who,
            &origin,
            USER_ADAPTER,
            b.caps,
            json!({
                "app_connection": b.app_connection,
                EXTRA_SCOPES: b.scopes,
            }),
            None,
        )
        .await?;
    Ok(Json(json!({ "url": url })))
}
