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
//!   authorize, [`claim_self`] merges that contact into “我” (a normal, splittable merge) before the
//!   account is attached, instead of the connect failing with “这个账号已经记在「…」名下”. A contact
//!   someone confirmed by hand is never merged automatically; the connect then says who holds it.
//!
//! Which Steve user authorized is not visible to `verify` (the framework calls it with the connection
//! only), so the start routes put a single-use ticket ([`SelfClaims`]) into the flow's config:
//! `config.self_claim`. A flow started without one (the generic `/connections/oauth/start`) still
//! connects; it just doesn't merge.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use axum::http::HeaderMap;
use axum::routing::post;
use axum::{Extension, Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use steve_modules::accounts::connections::Connections;
use steve_modules::accounts::connections::EXTRA_SCOPES;
use steve_modules::accounts::connections::oauth::random_token;
use steve_modules::accounts::identify::{ExtraHandle, Hints, Identified, identify};
use steve_modules::accounts::{repo as arepo, service as asvc};
use steve_modules::contacts::PartyHooks;
use steve_modules::contacts::merge::merge_tx;
use steve_modules::contacts::profile;
use steve_modules::contacts::repo::Party;
use steve_modules::vault::VaultResult;
use steve_modules::{ApiError, ApiResult, Caller, Modules};
use steve_store::Tx;
use steve_store::base::Viewer;

use super::auth::{USER_ADAPTER, USER_SYSTEM};

/// `config` key of the claim ticket.
pub const SELF_CLAIM: &str = "self_claim";
/// A ticket lives as long as the framework's OAuth state (10 minutes) plus the device code's
/// longest lifetime; 30 minutes covers both.
const TICKET_TTL_MS: i64 = 30 * 60_000;

struct Ticket {
    ws: String,
    user: String,
    expires_at: i64,
}

/// Single-use tickets “this flow was started by user X of workspace W” (in memory, like the
/// framework's OAuth states: a Hub restart only means no merge for flows in flight).
#[derive(Clone, Default)]
pub struct SelfClaims {
    tickets: Arc<Mutex<HashMap<String, Ticket>>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl SelfClaims {
    /// A new ticket for `who`. `now`: Unix ms.
    pub fn issue(&self, who: &Caller, now: i64) -> ApiResult<String> {
        let t = format!(
            "scl_{}",
            random_token(16).map_err(|e| ApiError::rule(e.to_string()))?
        );
        let mut m = lock(&self.tickets);
        m.retain(|_, x| x.expires_at > now);
        m.insert(
            t.clone(),
            Ticket {
                ws: who.ws.clone(),
                user: who.me.clone(),
                expires_at: now + TICKET_TTL_MS,
            },
        );
        Ok(t)
    }

    /// The user of a ticket issued in `ws`, once; expired, unknown or foreign tickets give `None`.
    pub fn take(&self, ticket: &str, ws: &str, now: i64) -> Option<String> {
        let t = lock(&self.tickets).remove(ticket)?;
        (t.ws == ws && t.expires_at > now).then_some(t.user)
    }
}

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

/// May an account on `p` be claimed for “我” without asking: an active contact nobody confirmed by
/// hand — `unconfirmed` (made by `identify`), or made `real` only by the names run on the provider's
/// name (`source` chat / sync, no kind or name a person set). The names run promotes the contact
/// the bot made for you some 20 s after your first message, long before you authorize.
fn claimable(tx: &Tx, ws: &str, p: &Party) -> ApiResult<bool> {
    if p.kind != "contact" || p.status != "active" || p.is_me() {
        return Ok(false);
    }
    Ok(match p.contact_kind.as_deref() {
        Some("unconfirmed") => true,
        Some("real") if matches!(p.source.as_str(), "chat" | "sync") => {
            !profile::kind_edited(tx, ws, &p.id)? && !profile::name_edited(tx, ws, &p.id)?
        }
        _ => false,
    })
}

/// Before the OAuth-proven Feishu identity `ids` is attached to `me` (“我” of `actor`): merges the
/// unconfirmed contact holding its `union_id` or `open_id`, if any, into `me`. Returns the merge ids
/// (at most two, when the two ids sat on different contacts).
pub fn claim_self(
    tx: &Tx,
    hooks: &[Arc<dyn PartyHooks>],
    viewer: &Viewer,
    actor: &str,
    me: &Party,
    ids: &FeishuIds,
) -> ApiResult<Vec<String>> {
    let ws = viewer.ws.as_str();
    let system = asvc::own_system(tx, ws, USER_SYSTEM)?;
    let mut merged = Vec::new();
    for raw in [ids.union_id.as_str(), ids.open_id.as_str()] {
        let Some(h) = asvc::normalize(&system, raw) else {
            continue;
        };
        let Some(account) = arepo::by_handle(tx, ws, &system.family, &h.handle)? else {
            continue;
        };
        let Some(a) = arepo::get(tx, &account)? else {
            continue;
        };
        let Some(p) = asvc::current_party(tx, &a.party_id)? else {
            continue;
        };
        if p.id == me.id || !claimable(tx, ws, &p)? || !viewer.sees(&p.visibility, &p.created_by) {
            continue;
        }
        let id = merge_tx(tx, hooks, viewer, actor, &p.id, &me.id)?;
        tracing::info!(merge = %id, "feishu: your unconfirmed contact merged into 我");
        merged.push(id);
    }
    Ok(merged)
}

/// [`claim_self`] for user `user` of `ws`, in its own transaction, off the runtime.
pub async fn claim_self_for(
    m: Modules,
    hooks: Vec<Arc<dyn PartyHooks>>,
    ws: String,
    user: String,
    ids: FeishuIds,
) -> ApiResult<Vec<String>> {
    tokio::task::spawn_blocking(move || {
        let who = Caller { ws, me: user };
        let (viewer, me) = asvc::start(&m, &who)?;
        m.store
            .transaction(|tx| claim_self(tx, &hooks, &viewer, &who.me, &me, &ids))
    })
    .await
    .map_err(|_| ApiError::rule("任务失败"))?
}

// ---------------------------------------------------------------------------------------------
// Start routes.

#[derive(Clone)]
struct UserFlows {
    conns: Connections,
    claims: SelfClaims,
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
/// “授权你本人” and “重新授权” buttons (`scopes`: the missing ones, asked for besides the usual). Like `/connections/oauth/start` for `feishu_user`, plus the claim ticket, so a contact
/// the bot already made for you is merged into “我”. Relative to `/api`, behind the `Caller` layer.
/// `claims` must be the `feishu_user` provider's ([`super::FeishuUser::claims`]).
pub fn user_routes(conns: Connections, claims: SelfClaims) -> Router<Modules> {
    Router::new()
        .route("/channels/feishu/user/oauth/start", post(oauth_start))
        .layer(Extension(UserFlows { conns, claims }))
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
    let ticket = f.claims.issue(&who, f.conns.modules().clock.now())?;
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
                SELF_CLAIM: ticket,
                EXTRA_SCOPES: b.scopes,
            }),
            None,
        )
        .await?;
    Ok(Json(json!({ "url": url })))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn who(ws: &str, me: &str) -> Caller {
        Caller {
            ws: ws.into(),
            me: me.into(),
        }
    }

    #[test]
    fn tickets_are_single_use_per_workspace_and_expire() {
        let c = SelfClaims::default();
        let t = c.issue(&who("w", "u"), 0).expect("ticket");
        assert_eq!(c.take(&t, "other", 1), None, "another workspace");
        // a failed take spends it too
        assert_eq!(c.take(&t, "w", 1), None);
        let t = c.issue(&who("w", "u"), 0).expect("ticket");
        assert_eq!(c.take(&t, "w", 1).as_deref(), Some("u"));
        assert_eq!(c.take(&t, "w", 2), None, "single use");
        let t = c.issue(&who("w", "u"), 0).expect("ticket");
        assert_eq!(c.take(&t, "w", TICKET_TTL_MS), None, "expired");
        assert_eq!(c.take("scl_guess", "w", 1), None);
    }
}
