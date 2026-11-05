//! axum handlers for the open-platform endpoints plus `/_fake/*` control routes.

// Test-only code: handlers return ready `Response`s as errors, and immediately-called closures
// stand in for try blocks so each handler can record its result code in one place.
#![allow(clippy::result_large_err, clippy::redundant_closure_call)]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::Bytes;
use axum::extract::{OriginalUri, Path, Query, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use serde_json::{Value, json};

use super::codes;
use super::model::{Body, Chat, Member, Mention, Message, Sender, User, World};
use super::{Endpoint, RecordedRequest, SentMessage, Shared, TokenInfo, TokenKind, now_ms};

type Q = Query<HashMap<String, String>>;
type St = State<Arc<Shared>>;

/// Feishu's endpoints at the root, and the same ones for Lark under `/lark` (one world).
pub(crate) fn router(shared: Arc<Shared>) -> Router {
    let r = routes(shared);
    r.clone().nest("/lark", r)
}

fn routes(shared: Arc<Shared>) -> Router {
    Router::new()
        .route(
            "/open-apis/auth/v3/tenant_access_token/internal",
            post(tenant_token),
        )
        .route("/open-apis/bot/v3/info", get(bot_info))
        .route(
            "/open-apis/im/v1/messages",
            post(send_message).get(list_messages),
        )
        .route(
            "/open-apis/im/v1/messages/{message_id}",
            delete(delete_message),
        )
        .route(
            "/open-apis/im/v1/messages/{message_id}/reply",
            post(reply_message),
        )
        .route(
            "/open-apis/im/v1/messages/{message_id}/reactions",
            post(add_reaction),
        )
        .route(
            "/open-apis/im/v1/messages/{message_id}/reactions/{reaction_id}",
            delete(delete_reaction),
        )
        .route(
            "/open-apis/im/v1/messages/{message_id}/resources/{file_key}",
            get(get_resource),
        )
        .route("/open-apis/im/v1/chats", get(list_chats))
        .route("/open-apis/im/v1/chats/{chat_id}", get(get_chat))
        .route(
            "/open-apis/im/v1/chats/{chat_id}/members",
            get(chat_members),
        )
        .route("/open-apis/contact/v3/scopes", get(contact_scopes))
        .route(
            "/open-apis/contact/v3/departments/{department_id}/children",
            get(department_children),
        )
        .route(
            "/open-apis/contact/v3/departments/{department_id}",
            get(get_department),
        )
        .route(
            "/open-apis/contact/v3/users/find_by_department",
            get(users_by_department),
        )
        .route("/open-apis/contact/v3/users/batch", get(batch_users))
        .route("/open-apis/contact/v3/users/{user_id}", get(get_user))
        .route("/open-apis/tenant/v2/tenant/query", get(tenant_query))
        .route(
            "/open-apis/contact/v3/group/{group_id}/member/simplelist",
            get(group_members),
        )
        .route(
            "/open-apis/authen/v1/authorize",
            get(super::user::authorize),
        )
        .route("/open-apis/authen/v2/oauth/token", post(super::user::token))
        .route(
            "/oauth/v1/device_authorization",
            post(super::user::device_authorization),
        )
        .route("/oauth/v3/token", post(super::user::device_token))
        .route(
            "/oauth/v1/app/registration",
            post(super::registration::handle),
        )
        .route("/open-apis/im/v1/messages/search", post(search_messages))
        .route("/open-apis/im/v1/messages/mget", get(mget_messages))
        .route("/open-apis/authen/v1/user_info", get(user_info))
        .route(
            crate::adapters::feishu::pbbp2::ENDPOINT_PATH,
            post(super::ws::endpoint),
        )
        .route("/ws/v2", get(super::ws::upgrade))
        .route("/open-apis/event/v1/connection", get(event_connection))
        .route(
            "/open-apis/application/v6/applications/{app_id}",
            get(app_info),
        )
        .route("/_fake/ws/push", post(super::ws::ctl_push))
        .route("/_fake/ws/deliveries", get(super::ws::ctl_deliveries))
        .route("/_fake/ws/disconnect", post(super::ws::ctl_disconnect))
        .route("/_fake/sent", get(ctl_sent))
        .route("/_fake/reactions", get(ctl_reactions))
        .route("/_fake/push", post(ctl_push))
        .route("/_fake/url_verification", post(ctl_url_verification))
        .route("/_fake/redeliver/{event_id}", post(ctl_redeliver))
        .route("/_fake/expire_tokens", post(ctl_expire_tokens))
        .route("/_fake/deny", post(ctl_deny))
        .route("/_fake/allow", post(ctl_allow))
        .route("/_fake/user_scopes", post(ctl_user_scopes))
        .route(
            "/_fake/device/approve",
            post(super::user::ctl_device_approve),
        )
        .route("/_fake/device/deny", post(super::user::ctl_device_deny))
        .with_state(shared)
}

// ---------------------------------------------------------------------------------------------
// Request gate: logging, fault injection, auth.

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Auth {
    /// No token needed (token endpoint, OAuth).
    None,
    /// Tenant token only.
    Tenant,
    /// User token only.
    User,
    /// Tenant or user token.
    Any,
}

pub(super) enum Caller {
    Anonymous,
    Tenant,
    User(String),
}

pub(super) struct Ctx {
    pub idx: usize,
    pub caller: Caller,
    /// Scopes of a user token; empty for tenant tokens.
    pub scopes: Vec<String>,
}

/// A Feishu-style failure: HTTP status + `{code, msg}`.
pub(super) struct ApiErr {
    status: StatusCode,
    code: i64,
    msg: String,
    headers: Vec<(&'static str, String)>,
    /// The `error` object of the answer (missing scopes: `permission_violations`, `helps`).
    error: Option<Value>,
}

pub(super) fn err(status: StatusCode, code: i64, msg: impl Into<String>) -> ApiErr {
    ApiErr {
        status,
        code,
        msg: msg.into(),
        headers: Vec::new(),
        error: None,
    }
}

/// The scopes (any one will do) the open platform names when `ep` is refused for the app.
pub(super) fn scopes_of(ep: Endpoint) -> &'static [&'static str] {
    match ep {
        Endpoint::ListChats | Endpoint::GetChat | Endpoint::ChatMembers => {
            &["im:chat:readonly", "im:chat"]
        }
        Endpoint::ListMessages | Endpoint::MgetMessages => &["im:message.group_msg"],
        Endpoint::SendMessage | Endpoint::ReplyMessage | Endpoint::DeleteMessage => {
            &["im:message:send_as_bot", "im:message"]
        }
        Endpoint::GetResource => &["im:resource"],
        Endpoint::ContactScopes
        | Endpoint::DepartmentChildren
        | Endpoint::GetDepartment
        | Endpoint::UsersByDepartment
        | Endpoint::GetUser
        | Endpoint::GroupMembers => &["contact:contact.base:readonly"],
        Endpoint::BatchUsers => &["contact:user.base:readonly"],
        Endpoint::TenantQuery => &["tenant:tenant:readonly"],
        Endpoint::AppInfo => &["admin:app.info:readonly"],
        _ => &["im:message"],
    }
}

/// The 99991672 answer as the open platform gives it (doc “如何为应用申请所需权限”): the scopes in
/// `msg` and `error.permission_violations`, the permission page with them preselected in
/// `error.helps`.
pub(super) fn no_scope(app_id: &str, scopes: &[&str]) -> ApiErr {
    let list = scopes.join(", ");
    let url = format!(
        "https://open.feishu.cn/app/{app_id}/auth?q={}&op_from=openapi&token_type=tenant",
        scopes.join(",")
    );
    let mut e = bad(
        codes::NO_SCOPE,
        format!(
            "Access denied. One of the following scopes is required: [{list}].应用尚未开通所需的应用身份权限：[{list}]，点击链接申请并开通任一权限即可：{url}"
        ),
    );
    e.error = Some(json!({
        "log_id": "fake",
        "permission_violations": scopes.iter().map(|s| json!({"type": "action_scope_required", "subject": s})).collect::<Vec<_>>(),
        "helps": [{"url": url, "description": format!("Learn more about scopes and how to add them: [{list}]")}],
    }));
    e
}

pub(super) fn bad(code: i64, msg: impl Into<String>) -> ApiErr {
    err(StatusCode::BAD_REQUEST, code, msg)
}

impl IntoResponse for ApiErr {
    fn into_response(self) -> Response {
        let mut body = json!({"code": self.code, "msg": self.msg, "data": {}});
        if let Some(e) = self.error {
            body["error"] = e;
        }
        let mut r = (self.status, axum::Json(body)).into_response();
        for (k, v) in self.headers {
            if let Ok(v) = HeaderValue::from_str(&v) {
                r.headers_mut().insert(k, v);
            }
        }
        r
    }
}

fn bearer(headers: &HeaderMap) -> Option<String> {
    let v = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let t = v
        .strip_prefix("Bearer ")
        .or_else(|| v.strip_prefix("bearer "))?;
    Some(t.trim().to_string())
}

pub(super) fn gate(
    sh: &Shared,
    ep: Endpoint,
    auth: Auth,
    method: &Method,
    uri: &axum::http::Uri,
    headers: &HeaderMap,
) -> Result<Ctx, Response> {
    let token = bearer(headers);
    // before the runtime lock: never hold both
    let app_id = sh.world().app.app_id.clone();
    let mut rt = sh.rt();
    rt.seq += 1;
    let seq = rt.seq;
    rt.requests.push(RecordedRequest {
        endpoint: ep,
        method: method.to_string(),
        path: uri.path().to_string(),
        query: uri.query().unwrap_or("").to_string(),
        token: token.clone(),
        code: 0,
    });
    let idx = rt.requests.len() - 1;
    let fail = |rt: &mut super::Runtime, e: ApiErr| {
        rt.requests[idx].code = e.code;
        e.into_response()
    };

    let limited_global = rt.rate_limit_at.remove(&seq);
    let limited_ep = match rt.rate_limit_next.get_mut(&ep) {
        Some(n) if *n > 0 => {
            *n -= 1;
            true
        }
        _ => false,
    };
    if limited_global || limited_ep {
        let reset = rt.rate_limit_reset_secs;
        let mut e = err(
            StatusCode::TOO_MANY_REQUESTS,
            codes::RATE_LIMITED,
            "request trigger frequency limit",
        );
        e.headers.push(("x-ogw-ratelimit-limit", "50".into()));
        e.headers.push(("x-ogw-ratelimit-reset", reset.to_string()));
        return Err(fail(&mut rt, e));
    }

    let mut scopes = Vec::new();
    let caller = match auth {
        Auth::None => Caller::Anonymous,
        _ => {
            let Some(token) = token else {
                return Err(fail(&mut rt, bad(codes::MISSING_TOKEN, "Need a token")));
            };
            let info: Option<TokenInfo> = rt.tokens.get(&token).cloned();
            let Some(info) = info else {
                return Err(fail(
                    &mut rt,
                    bad(
                        codes::INVALID_TOKEN,
                        "Invalid access token for authorization. Please make a request with token attached",
                    ),
                ));
            };
            let expired = info.revoked || info.expires_at <= Instant::now();
            match (&info.kind, auth) {
                (TokenKind::Tenant, Auth::User) | (TokenKind::User { .. }, Auth::Tenant) => {
                    return Err(fail(
                        &mut rt,
                        bad(
                            codes::INVALID_TOKEN,
                            "Invalid access token for authorization. Please make a request with token attached",
                        ),
                    ));
                }
                (TokenKind::Tenant, _) if expired => {
                    return Err(fail(
                        &mut rt,
                        bad(
                            codes::INVALID_TOKEN,
                            "Invalid access token for authorization. Please make a request with token attached",
                        ),
                    ));
                }
                (TokenKind::User { .. }, _) if expired => {
                    return Err(fail(
                        &mut rt,
                        bad(codes::USER_TOKEN_EXPIRED, "token expire"),
                    ));
                }
                (TokenKind::Tenant, _) => Caller::Tenant,
                (TokenKind::User { open_id, scopes: s }, _) => {
                    scopes = s.clone();
                    Caller::User(open_id.clone())
                }
            }
        }
    };

    if rt.denied.contains(&ep)
        || (matches!(caller, Caller::Tenant) && rt.denied_tenant.contains(&ep))
    {
        return Err(fail(&mut rt, no_scope(&app_id, scopes_of(ep))));
    }
    Ok(Ctx {
        idx,
        caller,
        scopes,
    })
}

/// Wraps `data` in `{code:0, msg:"success", data}` or renders the error; records the code.
pub(super) fn finish(sh: &Shared, ctx: &Ctx, r: Result<Value, ApiErr>) -> Response {
    let code = r.as_ref().map(|_| 0).unwrap_or_else(|e| e.code);
    sh.rt().requests[ctx.idx].code = code;
    match r {
        Ok(data) => axum::Json(json!({"code": 0, "msg": "success", "data": data})).into_response(),
        Err(e) => e.into_response(),
    }
}

fn parse_json(body: &Bytes) -> Result<Value, ApiErr> {
    serde_json::from_slice::<Value>(body)
        .ok()
        .filter(Value::is_object)
        .ok_or_else(|| bad(codes::FIELD_VALIDATION, "request body is not a JSON object"))
}

fn str_field<'a>(v: &'a Value, k: &str) -> Option<&'a str> {
    v.get(k).and_then(Value::as_str).filter(|s| !s.is_empty())
}

fn required<'a>(v: &'a Value, k: &str) -> Result<&'a str, ApiErr> {
    str_field(v, k).ok_or_else(|| {
        bad(
            codes::FIELD_VALIDATION,
            format!("field validation failed: {k} is required"),
        )
    })
}

/// Offset pagination behind an opaque `page_token`.
fn paginate<T: Clone>(
    items: &[T],
    q: &HashMap<String, String>,
    default_size: usize,
    max_size: usize,
) -> Result<(Vec<T>, bool, String), ApiErr> {
    let size = match q.get("page_size") {
        Some(s) => s
            .parse::<usize>()
            .ok()
            .filter(|n| (1..=max_size).contains(n))
            .ok_or_else(|| {
                bad(
                    codes::FIELD_VALIDATION,
                    "field validation failed: page_size",
                )
            })?,
        None => default_size,
    };
    let start = match q.get("page_token").map(String::as_str) {
        None | Some("") => 0,
        Some(t) => t
            .strip_prefix("fakepage_")
            .and_then(|n| n.parse::<usize>().ok())
            .ok_or_else(|| {
                bad(
                    codes::FIELD_VALIDATION,
                    "field validation failed: page_token",
                )
            })?,
    };
    let end = (start + size).min(items.len());
    let page = items
        .get(start.min(items.len())..end)
        .unwrap_or(&[])
        .to_vec();
    let has_more = end < items.len();
    let token = if has_more {
        format!("fakepage_{end}")
    } else {
        String::new()
    };
    Ok((page, has_more, token))
}

fn page_json(items: Vec<Value>, has_more: bool, token: String) -> Value {
    let mut v = json!({"has_more": has_more, "items": items});
    if has_more {
        v["page_token"] = json!(token);
    }
    v
}

// ---------------------------------------------------------------------------------------------
// Tokens and bot.

/// Feishu hands back the cached token while it has ≥30 minutes left, a fresh one otherwise.
const TOKEN_REUSE_MIN: Duration = Duration::from_secs(30 * 60);

async fn tenant_token(
    State(sh): St,
    m: Method,
    OriginalUri(u): OriginalUri,
    h: HeaderMap,
    body: Bytes,
) -> Response {
    let ctx = match gate(&sh, Endpoint::TenantToken, Auth::None, &m, &u, &h) {
        Ok(c) => c,
        Err(r) => return r,
    };
    let r = (|| {
        let v = parse_json(&body)?;
        let app_id = required(&v, "app_id")?;
        let secret = required(&v, "app_secret")?;
        let app = sh.world().app.clone();
        if app_id != app.app_id || secret != app.app_secret {
            return Err(bad(codes::BAD_APP_SECRET, "app secret invalid"));
        }
        let mut rt = sh.rt();
        let now = Instant::now();
        let reuse = rt
            .tokens
            .iter()
            .filter(|(_, t)| matches!(t.kind, TokenKind::Tenant) && !t.revoked)
            .filter(|(_, t)| t.expires_at > now + TOKEN_REUSE_MIN)
            .map(|(k, t)| (k.clone(), t.expires_at))
            .next();
        let (token, expires_at) = match reuse {
            Some(x) => x,
            None => {
                let token = format!("t-{}", rt.id(""));
                let expires_at = now + rt.token_ttl;
                rt.tokens.insert(
                    token.clone(),
                    TokenInfo {
                        kind: TokenKind::Tenant,
                        expires_at,
                        revoked: false,
                    },
                );
                (token, expires_at)
            }
        };
        let expire = expires_at.saturating_duration_since(now).as_secs();
        Ok(json!({"code": 0, "msg": "ok", "tenant_access_token": token, "expire": expire}))
    })();
    // This endpoint answers flat, without `data`.
    sh.rt().requests[ctx.idx].code = r.as_ref().map(|_| 0).unwrap_or_else(|e| e.code);
    match r {
        Ok(v) => axum::Json(v).into_response(),
        Err(e) => (e.status, axum::Json(json!({"code": e.code, "msg": e.msg}))).into_response(),
    }
}

async fn bot_info(State(sh): St, m: Method, OriginalUri(u): OriginalUri, h: HeaderMap) -> Response {
    let ctx = match gate(&sh, Endpoint::BotInfo, Auth::Tenant, &m, &u, &h) {
        Ok(c) => c,
        Err(r) => return r,
    };
    let app = sh.world().app.clone();
    sh.rt().requests[ctx.idx].code = 0;
    // bot/v3/info puts `bot` at the top level instead of under `data`.
    axum::Json(json!({
        "code": 0,
        "msg": "ok",
        "bot": {
            "activate_status": app.activate_status,
            "app_name": app.bot_name,
            "avatar_url": app.bot_avatar_url,
            "ip_white_list": [],
            "open_id": app.bot_open_id,
        }
    }))
    .into_response()
}

/// App info (`application/v6`): `scene_type` 0 = 企业自建应用, 1 = 应用商店应用. Only the
/// calling app itself (or `me`) is visible.
async fn app_info(
    State(sh): St,
    m: Method,
    OriginalUri(u): OriginalUri,
    h: HeaderMap,
    Path(app_id): Path<String>,
) -> Response {
    let ctx = match gate(&sh, Endpoint::AppInfo, Auth::Tenant, &m, &u, &h) {
        Ok(c) => c,
        Err(r) => return r,
    };
    let app = sh.world().app.clone();
    let r = if app_id != app.app_id && app_id != "me" {
        Err(bad(codes::FIELD_VALIDATION, "app not found"))
    } else {
        Ok(json!({"app": {
            "app_id": app.app_id,
            "app_name": app.bot_name,
            "status": 1,
            "scene_type": if app.self_built { 0 } else { 1 },
            "primary_language": "zh_cn",
        }}))
    };
    finish(&sh, &ctx, r)
}

/// Online long connections of the app (lark-cli checks this before consuming events).
async fn event_connection(
    State(sh): St,
    m: Method,
    OriginalUri(u): OriginalUri,
    h: HeaderMap,
) -> Response {
    let ctx = match gate(&sh, Endpoint::EventConnection, Auth::Tenant, &m, &u, &h) {
        Ok(c) => c,
        Err(r) => return r,
    };
    let n = super::ws::online_count(&sh);
    finish(&sh, &ctx, Ok(json!({"online_instance_cnt": n})))
}

// ---------------------------------------------------------------------------------------------
// Messages.

/// uuid de-dup window (Feishu: same uuid within one hour sends once).
const UUID_WINDOW: Duration = Duration::from_secs(3600);

fn bot_sender(w: &World) -> Sender {
    Sender {
        id: w.app.app_id.clone(),
        id_type: "app_id".into(),
        sender_type: "app".into(),
        tenant_key: w.app.tenant_key.clone(),
    }
}

/// Validates `content` for `msg_type` and returns the stored form: `<at user_id="…">name</at>`
/// in text becomes `@_user_n` plus a `mentions` entry, as the message API shows it afterwards.
fn stored_content(
    w: &World,
    msg_type: &str,
    content: &str,
) -> Result<(String, Vec<Mention>), ApiErr> {
    let v: Value = serde_json::from_str(content)
        .ok()
        .filter(Value::is_object)
        .ok_or_else(|| {
            bad(
                codes::IM_INVALID_PARAM,
                "invalid message content: content must be a JSON string of an object",
            )
        })?;
    match msg_type {
        "text" => {
            let text = v.get("text").and_then(Value::as_str).ok_or_else(|| {
                bad(
                    codes::IM_INVALID_PARAM,
                    "invalid message content: text is required",
                )
            })?;
            let mut out = String::new();
            let mut mentions = Vec::new();
            let mut rest = text;
            while let Some(i) = rest.find("<at ") {
                out.push_str(&rest[..i]);
                let tail = &rest[i..];
                let Some(close) = tail.find("</at>") else {
                    break;
                };
                let tag = &tail[..close];
                let id = tag
                    .split("user_id=\"")
                    .nth(1)
                    .and_then(|s| s.split('"').next())
                    .unwrap_or("");
                let inner = tag.split('>').nth(1).unwrap_or("");
                let key = format!("@_user_{}", mentions.len() + 1);
                let name = if id == "all" {
                    "所有人".to_string()
                } else {
                    w.user_by_any_id(id)
                        .map(|u| u.name.clone())
                        .unwrap_or_else(|| inner.to_string())
                };
                out.push_str(&key);
                mentions.push(Mention {
                    key,
                    id: id.into(),
                    id_type: "open_id".into(),
                    name,
                    tenant_key: w.app.tenant_key.clone(),
                });
                rest = &tail[close + "</at>".len()..];
            }
            out.push_str(rest);
            Ok((json!({"text": out}).to_string(), mentions))
        }
        "post" | "image" | "file" | "audio" | "media" | "sticker" | "interactive"
        | "share_chat" | "share_user" | "system" => Ok((content.to_string(), Vec::new())),
        other => Err(bad(
            codes::IM_INVALID_PARAM,
            format!("invalid msg_type: {other}"),
        )),
    }
}

fn find_or_make_p2p(w: &mut World, user: &User) -> String {
    let existing = w
        .chats
        .iter()
        .find(|c| {
            c.chat_mode == "p2p"
                && w.members
                    .iter()
                    .any(|(cid, m)| cid == &c.chat_id && m.ids.open_id == user.ids.open_id)
        })
        .map(|c| c.chat_id.clone());
    if let Some(id) = existing {
        return id;
    }
    let id = format!("oc_p2p{}", user.ids.open_id.trim_start_matches("ou_"));
    w.chats.push(Chat {
        chat_id: id.clone(),
        name: String::new(),
        description: String::new(),
        chat_mode: "p2p".into(),
        chat_type: "private".into(),
        owner_open_id: String::new(),
        external: false,
        bot_in_chat: true,
        chat_status: "normal".into(),
    });
    w.members.push((
        id.clone(),
        Member {
            ids: user.ids.clone(),
            name: user.name.clone(),
            tenant_key: w.app.tenant_key.clone(),
        },
    ));
    id
}

fn ensure_bot_in(w: &World, chat_id: &str) -> Result<(), ApiErr> {
    match w.chat(chat_id) {
        Some(c) if c.bot_in_chat && c.chat_status == "normal" => Ok(()),
        Some(_) => Err(bad(
            codes::IM_BOT_NOT_IN_CHAT,
            "Bot/User can NOT be out of the chat.",
        )),
        None => Err(bad(
            codes::IM_INVALID_PARAM,
            "invalid receive_id: chat not found",
        )),
    }
}

/// Missing-user-scope error unless each group has one granted scope (user tokens only; tenant
/// scopes are modelled with [`super::FakeFeishu::deny`]).
fn need_scopes(ctx: &Ctx, groups: &[&[&str]]) -> Result<(), ApiErr> {
    if !matches!(ctx.caller, Caller::User(_)) {
        return Ok(());
    }
    for g in groups {
        if !g.iter().any(|s| ctx.scopes.iter().any(|x| x == s)) {
            let mut e = bad(
                codes::USER_SCOPE_MISSING,
                format!(
                    "Unauthorized. The user access token lacks one of the scopes: [{}]",
                    g.join(", ")
                ),
            );
            e.error = Some(json!({
                "permission_violations": g.iter().map(|s| json!({"type": "action_privilege_required", "subject": s})).collect::<Vec<_>>(),
            }));
            return Err(e);
        }
    }
    Ok(())
}

fn is_member(w: &World, chat_id: &str, open_id: &str) -> bool {
    w.chat_members(chat_id).any(|m| m.ids.open_id == open_id)
}

/// The chat, if the caller may act in it: the bot must be a member for tenant tokens, the user
/// for user tokens.
fn ensure_in<'a>(w: &'a World, caller: &Caller, chat_id: &str) -> Result<&'a Chat, ApiErr> {
    match caller {
        Caller::User(me) => {
            let c = w.chat(chat_id).ok_or_else(|| {
                bad(
                    codes::IM_INVALID_PARAM,
                    "invalid receive_id: chat not found",
                )
            })?;
            if c.chat_status != "normal" || !is_member(w, chat_id, me) {
                return Err(bad(
                    codes::IM_BOT_NOT_IN_CHAT,
                    "Bot/User can NOT be out of the chat.",
                ));
            }
            Ok(c)
        }
        _ => {
            ensure_bot_in(w, chat_id)?;
            w.chat(chat_id).ok_or_else(|| {
                bad(
                    codes::IM_INVALID_PARAM,
                    "invalid receive_id: chat not found",
                )
            })
        }
    }
}

/// Sending as a user: scopes, then the per-user rate (sliding one-second window).
fn check_send_as_user(sh: &Shared, ctx: &Ctx) -> Result<(), ApiErr> {
    let Caller::User(me) = &ctx.caller else {
        return Ok(());
    };
    need_scopes(ctx, &[&["im:message"], &["im:message.send_as_user"]])?;
    let mut rt = sh.rt();
    let limit = rt.user_send_qps;
    let now = Instant::now();
    let q = rt.user_sends.entry(me.clone()).or_default();
    while q
        .front()
        .is_some_and(|t| now.duration_since(*t) >= Duration::from_secs(1))
    {
        q.pop_front();
    }
    if q.len() >= limit {
        return Err(bad(
            codes::IM_SEND_RATE_LIMITED,
            "The request you send is too frequent. Each user can send at most 5 messages per second.",
        ));
    }
    q.push_back(now);
    Ok(())
}

/// The p2p chat between the calling user and `other`, created on first use (bot not in it).
fn find_or_make_user_p2p(w: &mut World, me: &str, other: &User) -> String {
    let existing = w
        .chats
        .iter()
        .find(|c| {
            c.chat_mode == "p2p"
                && !c.bot_in_chat
                && is_member(w, &c.chat_id, me)
                && is_member(w, &c.chat_id, &other.ids.open_id)
        })
        .map(|c| c.chat_id.clone());
    if let Some(id) = existing {
        return id;
    }
    let id = format!(
        "oc_up2p{}{}",
        me.trim_start_matches("ou_").get(..8).unwrap_or(""),
        other.ids.open_id.trim_start_matches("ou_")
    );
    w.chats.push(Chat {
        chat_id: id.clone(),
        name: String::new(),
        description: String::new(),
        chat_mode: "p2p".into(),
        chat_type: "private".into(),
        owner_open_id: String::new(),
        external: false,
        bot_in_chat: false,
        chat_status: "normal".into(),
    });
    let tk = w.app.tenant_key.clone();
    let me_user = w.user_by_open_id(me).cloned();
    for u in me_user.iter().chain(std::iter::once(other)) {
        w.members.push((
            id.clone(),
            Member {
                ids: u.ids.clone(),
                name: u.name.clone(),
                tenant_key: tk.clone(),
            },
        ));
    }
    id
}

struct Outgoing {
    reply_to: Option<String>,
    receive_id_type: Option<String>,
    receive_id: Option<String>,
    msg_type: String,
    content: String,
    uuid: Option<String>,
    reply_in_thread: bool,
}

/// Shared tail of send and reply: uuid de-dup, store, record.
fn deliver(sh: &Shared, caller: &Caller, chat_id: String, out: Outgoing) -> Result<Value, ApiErr> {
    let mut w = sh.world();
    let (stored, mentions) = stored_content(&w, &out.msg_type, &out.content)?;
    let as_user = match caller {
        Caller::User(id) => Some(id.clone()),
        _ => None,
    };
    let mut rt = sh.rt();
    let now = Instant::now();
    rt.uuids
        .retain(|_, (_, t)| now.duration_since(*t) < UUID_WINDOW);
    let mut rec = SentMessage {
        message_id: String::new(),
        reply_to: out.reply_to.clone(),
        receive_id_type: out.receive_id_type,
        receive_id: out.receive_id,
        chat_id: chat_id.clone(),
        msg_type: out.msg_type.clone(),
        content: out.content,
        uuid: out.uuid.clone(),
        reply_in_thread: out.reply_in_thread,
        as_user: as_user.clone(),
        deduplicated: false,
    };
    if let Some(prev) = out
        .uuid
        .as_ref()
        .and_then(|u| rt.uuids.get(u))
        .map(|(id, _)| id.clone())
    {
        rec.message_id = prev.clone();
        rec.deduplicated = true;
        rt.attempts.push(rec);
        let msg = w.messages.iter().find(|m| m.message_id == prev).cloned();
        return msg
            .map(|m| json!(m))
            .ok_or_else(|| bad(codes::IM_MESSAGE_NOT_FOUND, "message not found"));
    }

    let message_id = rt.id("om_");
    let (root_id, parent_id, thread_id) = match &out.reply_to {
        None => (None, None, None),
        Some(pid) => {
            let parent = w.messages.iter().find(|m| &m.message_id == pid);
            let root = parent
                .and_then(|p| p.root_id.clone())
                .unwrap_or_else(|| pid.clone());
            let thread = parent.and_then(|p| p.thread_id.clone()).or_else(|| {
                let topic = w.chat(&chat_id).is_some_and(|c| c.chat_mode == "topic");
                (out.reply_in_thread || topic).then(|| rt.id("omt_"))
            });
            // A thread opened by this reply belongs to the parent too.
            if let Some(t) = &thread
                && let Some(p) = w.messages.iter_mut().find(|m| &m.message_id == pid)
                && p.thread_id.is_none()
            {
                p.thread_id = Some(t.clone());
            }
            (Some(root), Some(pid.clone()), thread)
        }
    };
    let sender = match &as_user {
        Some(open_id) => Sender {
            id: open_id.clone(),
            id_type: "open_id".into(),
            sender_type: "user".into(),
            tenant_key: w.app.tenant_key.clone(),
        },
        None => bot_sender(&w),
    };
    let ms = now_ms().to_string();
    let msg = Message {
        message_id: message_id.clone(),
        root_id,
        parent_id,
        thread_id,
        msg_type: out.msg_type,
        create_time: ms.clone(),
        update_time: ms,
        deleted: false,
        updated: false,
        chat_id,
        sender,
        body: Body { content: stored },
        mentions,
        upper_message_id: None,
    };
    w.messages.push(msg.clone());
    rec.message_id = message_id.clone();
    if let Some(u) = &out.uuid {
        rt.uuids.insert(u.clone(), (message_id, now));
    }
    rt.attempts.push(rec.clone());
    rt.sent.push(rec);
    Ok(json!(msg))
}

async fn send_message(
    State(sh): St,
    m: Method,
    OriginalUri(u): OriginalUri,
    h: HeaderMap,
    Query(q): Q,
    body: Bytes,
) -> Response {
    let ctx = match gate(&sh, Endpoint::SendMessage, Auth::Any, &m, &u, &h) {
        Ok(c) => c,
        Err(r) => return r,
    };
    let r = (|| {
        let id_type = q.get("receive_id_type").cloned().ok_or_else(|| {
            bad(
                codes::FIELD_VALIDATION,
                "field validation failed: receive_id_type is required",
            )
        })?;
        let v = parse_json(&body)?;
        let receive_id = required(&v, "receive_id")?.to_string();
        let msg_type = required(&v, "msg_type")?.to_string();
        let content = required(&v, "content")?.to_string();
        let uuid = str_field(&v, "uuid").map(str::to_string);
        check_send_as_user(&sh, &ctx)?;
        let chat_id = {
            let mut w = sh.world();
            match id_type.as_str() {
                "chat_id" => {
                    ensure_in(&w, &ctx.caller, &receive_id)?;
                    receive_id.clone()
                }
                "open_id" | "union_id" | "user_id" | "email" => {
                    let user = w
                        .users
                        .iter()
                        .find(|u| match id_type.as_str() {
                            "email" => u.email == receive_id || u.enterprise_email == receive_id,
                            t => u.ids.by_type(t) == receive_id,
                        })
                        .cloned()
                        .ok_or_else(|| {
                            bad(
                                codes::IM_INVALID_PARAM,
                                "invalid receive_id: user not found",
                            )
                        })?;
                    match &ctx.caller {
                        Caller::User(me) => find_or_make_user_p2p(&mut w, me, &user),
                        _ => find_or_make_p2p(&mut w, &user),
                    }
                }
                _ => {
                    return Err(bad(
                        codes::FIELD_VALIDATION,
                        "field validation failed: receive_id_type",
                    ));
                }
            }
        };
        deliver(
            &sh,
            &ctx.caller,
            chat_id,
            Outgoing {
                reply_to: None,
                receive_id_type: Some(id_type),
                receive_id: Some(receive_id),
                msg_type,
                content,
                uuid,
                reply_in_thread: false,
            },
        )
    })();
    finish(&sh, &ctx, r)
}

async fn reply_message(
    State(sh): St,
    m: Method,
    OriginalUri(u): OriginalUri,
    h: HeaderMap,
    Path(message_id): Path<String>,
    body: Bytes,
) -> Response {
    let ctx = match gate(&sh, Endpoint::ReplyMessage, Auth::Any, &m, &u, &h) {
        Ok(c) => c,
        Err(r) => return r,
    };
    let r = (|| {
        let v = parse_json(&body)?;
        let msg_type = required(&v, "msg_type")?.to_string();
        let content = required(&v, "content")?.to_string();
        let uuid = str_field(&v, "uuid").map(str::to_string);
        let reply_in_thread = v
            .get("reply_in_thread")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        check_send_as_user(&sh, &ctx)?;
        let chat_id = {
            let w = sh.world();
            let parent = w
                .messages
                .iter()
                .find(|m| m.message_id == message_id && !m.deleted)
                .ok_or_else(|| {
                    bad(
                        codes::IM_MESSAGE_NOT_FOUND,
                        "The message is not found, maybe not exist or deleted.",
                    )
                })?;
            ensure_in(&w, &ctx.caller, &parent.chat_id)?;
            parent.chat_id.clone()
        };
        deliver(
            &sh,
            &ctx.caller,
            chat_id,
            Outgoing {
                reply_to: Some(message_id.clone()),
                receive_id_type: None,
                receive_id: None,
                msg_type,
                content,
                uuid,
                reply_in_thread,
            },
        )
    })();
    finish(&sh, &ctx, r)
}

/// Emoji types the fake accepts (a subset of the open platform's “表情文案说明”).
const EMOJI_TYPES: &[&str] = &[
    "OK",
    "THUMBSUP",
    "THANKS",
    "MUSCLE",
    "DONE",
    "OnIt",
    "Typing",
    "CrossMark",
    "CheckMark",
    "SMILE",
    "OneSecond",
    "Get",
    "LGTM",
];

async fn add_reaction(
    State(sh): St,
    m: Method,
    OriginalUri(u): OriginalUri,
    h: HeaderMap,
    Path(message_id): Path<String>,
    body: Bytes,
) -> Response {
    let ctx = match gate(&sh, Endpoint::AddReaction, Auth::Any, &m, &u, &h) {
        Ok(c) => c,
        Err(r) => return r,
    };
    let r = (|| {
        let v = parse_json(&body)?;
        let emoji = v["reaction_type"]["emoji_type"].as_str().unwrap_or("");
        if !EMOJI_TYPES.contains(&emoji) {
            return Err(bad(codes::IM_INVALID_PARAM, "invalid emoji_type"));
        }
        if message_id.trim().is_empty() {
            return Err(bad(codes::IM_MESSAGE_NOT_FOUND, "The message is not found"));
        }
        let as_user = match &ctx.caller {
            Caller::User(id) => Some(id.clone()),
            _ => None,
        };
        let mut rt = sh.rt();
        let id = rt.id("rc_");
        rt.reactions.push(super::FakeReaction {
            reaction_id: id.clone(),
            message_id: message_id.clone(),
            emoji_type: emoji.into(),
            as_user: as_user.clone(),
            deleted: false,
        });
        Ok(json!({
            "reaction_id": id,
            "operator": {"operator_id": as_user.unwrap_or_else(|| "cli_bot".into()), "operator_type": if matches!(ctx.caller, Caller::User(_)) { "user" } else { "app" }},
            "action_time": now_ms().to_string(),
            "reaction_type": {"emoji_type": emoji},
        }))
    })();
    finish(&sh, &ctx, r)
}

async fn delete_reaction(
    State(sh): St,
    m: Method,
    OriginalUri(u): OriginalUri,
    h: HeaderMap,
    Path((message_id, reaction_id)): Path<(String, String)>,
) -> Response {
    let ctx = match gate(&sh, Endpoint::DeleteReaction, Auth::Any, &m, &u, &h) {
        Ok(c) => c,
        Err(r) => return r,
    };
    let as_user = match &ctx.caller {
        Caller::User(id) => Some(id.clone()),
        _ => None,
    };
    let r = (|| {
        let mut rt = sh.rt();
        let Some(x) = rt
            .reactions
            .iter_mut()
            .find(|x| x.reaction_id == reaction_id && x.message_id == message_id && !x.deleted)
        else {
            return Err(bad(codes::IM_INVALID_PARAM, "reaction not found"));
        };
        // only the one who added it may take it off
        if x.as_user != as_user {
            return Err(bad(
                codes::IM_NOT_SENDER,
                "no permission to delete this reaction",
            ));
        }
        x.deleted = true;
        Ok(json!({"reaction_id": reaction_id, "reaction_type": {"emoji_type": x.emoji_type}}))
    })();
    finish(&sh, &ctx, r)
}

async fn ctl_reactions(State(sh): St) -> Response {
    axum::Json(json!({"reactions": sh.rt().reactions})).into_response()
}

async fn delete_message(
    State(sh): St,
    m: Method,
    OriginalUri(u): OriginalUri,
    h: HeaderMap,
    Path(message_id): Path<String>,
) -> Response {
    let ctx = match gate(&sh, Endpoint::DeleteMessage, Auth::Any, &m, &u, &h) {
        Ok(c) => c,
        Err(r) => return r,
    };
    let r = (|| {
        let mut w = sh.world();
        let app_id = w.app.app_id.clone();
        let me = match &ctx.caller {
            Caller::User(id) => id.clone(),
            _ => app_id,
        };
        let msg = w
            .messages
            .iter_mut()
            .find(|m| m.message_id == message_id)
            .ok_or_else(|| {
                bad(
                    codes::IM_MESSAGE_NOT_FOUND,
                    "The message is not found, maybe not exist or deleted.",
                )
            })?;
        if msg.deleted {
            return Err(bad(
                codes::IM_MESSAGE_DELETED,
                "The message has been deleted.",
            ));
        }
        if msg.sender.id != me {
            return Err(bad(
                codes::IM_NOT_SENDER,
                "Only the message sender can recall the message.",
            ));
        }
        msg.deleted = true;
        msg.update_time = now_ms().to_string();
        sh.rt().deleted.push(message_id.clone());
        Ok(json!({}))
    })();
    finish(&sh, &ctx, r)
}

/// What a user token may read in `chat`: history needs the
/// app not to share externally, the per-kind `get_as_user` scope, and membership.
fn user_may_read(w: &World, ctx: &Ctx, chat: &Chat) -> Result<(), ApiErr> {
    if !matches!(ctx.caller, Caller::User(_)) {
        return Ok(());
    }
    if w.app.external_sharing {
        return Err(bad(
            codes::USER_IDENTITY_SHARED_APP,
            "Apps with external sharing or linked organizations enabled cannot request this API with user identity.",
        ));
    }
    let kind_scope = if chat.chat_mode == "p2p" {
        "im:message.p2p_msg:get_as_user"
    } else {
        "im:message.group_msg:get_as_user"
    };
    need_scopes(
        ctx,
        &[&["im:message", "im:message:readonly"], &[kind_scope]],
    )?;
    if chat.chat_mode != "p2p" && !chat.bot_in_chat && w.user_read_requires_bot {
        return Err(bad(
            codes::IM_BOT_NOT_IN_CHAT,
            "Bot/User can NOT be out of the chat.",
        ));
    }
    Ok(())
}

/// Messages of one chat or thread the caller may read, oldest first.
fn readable(w: &World, ctx: &Ctx, kind: &str, id: &str) -> Result<Vec<Message>, ApiErr> {
    let chat_id = match kind {
        "chat" => id.to_string(),
        "thread" => w
            .messages
            .iter()
            .find(|m| m.thread_id.as_deref() == Some(id))
            .map(|m| m.chat_id.clone())
            .ok_or_else(|| {
                bad(
                    codes::IM_INVALID_PARAM,
                    "invalid container_id: thread not found",
                )
            })?,
        _ => {
            return Err(bad(
                codes::FIELD_VALIDATION,
                "field validation failed: container_id_type",
            ));
        }
    };
    let chat = ensure_in(w, &ctx.caller, &chat_id)?;
    user_may_read(w, ctx, chat)?;
    Ok(w.messages
        .iter()
        .filter(|m| match kind {
            "chat" => m.chat_id == chat_id,
            _ => m.thread_id.as_deref() == Some(id),
        })
        .cloned()
        .collect())
}

async fn list_messages(
    State(sh): St,
    m: Method,
    OriginalUri(u): OriginalUri,
    h: HeaderMap,
    Query(q): Q,
) -> Response {
    let ctx = match gate(&sh, Endpoint::ListMessages, Auth::Any, &m, &u, &h) {
        Ok(c) => c,
        Err(r) => return r,
    };
    let r = (|| {
        let kind = q.get("container_id_type").map(String::as_str).unwrap_or("");
        let id = q
            .get("container_id")
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                bad(
                    codes::FIELD_VALIDATION,
                    "field validation failed: container_id is required",
                )
            })?;
        let w = sh.world();
        let mut items = readable(&w, &ctx, kind, id)?;
        // start_time / end_time are seconds; create_time is milliseconds.
        let ms = |k: &str| {
            q.get(k)
                .and_then(|s| s.parse::<u128>().ok())
                .map(|s| s * 1000)
        };
        let (from, to) = (ms("start_time"), ms("end_time"));
        items.retain(|m| {
            let t = m.create_time.parse::<u128>().unwrap_or(0);
            from.is_none_or(|f| t >= f) && to.is_none_or(|e| t <= e)
        });
        items.sort_by_key(|m| m.create_time.parse::<u128>().unwrap_or(0));
        if q.get("sort_type").map(String::as_str) == Some("ByCreateTimeDesc") {
            items.reverse();
        }
        let (page, more, token) = paginate(&items, &q, 20, 50)?;
        Ok(page_json(
            page.into_iter().map(|m| json!(m)).collect(),
            more,
            token,
        ))
    })();
    finish(&sh, &ctx, r)
}

/// `POST /open-apis/im/v1/messages/search {query, filter}` → `items[{meta_data:{message_id}}]`.
/// Plain substring match over text; `filter.chat_ids`, `from_ids`, `chat_type` honoured.
async fn search_messages(
    State(sh): St,
    m: Method,
    OriginalUri(u): OriginalUri,
    h: HeaderMap,
    Query(q): Q,
    body: Bytes,
) -> Response {
    let ctx = match gate(&sh, Endpoint::SearchMessages, Auth::Any, &m, &u, &h) {
        Ok(c) => c,
        Err(r) => return r,
    };
    let r = (|| {
        need_scopes(&ctx, &[&["search:message"]])?;
        let v = parse_json(&body)?;
        let query = v
            .get("query")
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_lowercase();
        let filter = v.get("filter").cloned().unwrap_or(Value::Null);
        let list = |k: &str| -> Option<Vec<String>> {
            filter.get(k).and_then(Value::as_array).map(|a| {
                a.iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
        };
        let (chat_ids, from_ids) = (list("chat_ids"), list("from_ids"));
        let chat_type = filter.get("chat_type").and_then(Value::as_str);
        let w = sh.world();
        let visible = |c: &Chat| match &ctx.caller {
            Caller::User(me) => {
                c.chat_status == "normal"
                    && is_member(&w, &c.chat_id, me)
                    && (c.bot_in_chat || c.chat_mode == "p2p" || w.search_sees_no_bot_groups)
            }
            _ => c.bot_in_chat && c.chat_status == "normal",
        };
        let mut hits: Vec<&Message> = w
            .messages
            .iter()
            .filter(|m| !m.deleted)
            .filter(|m| w.chat(&m.chat_id).is_some_and(visible))
            .filter(|m| chat_ids.as_ref().is_none_or(|ids| ids.contains(&m.chat_id)))
            .filter(|m| {
                from_ids
                    .as_ref()
                    .is_none_or(|ids| ids.contains(&m.sender.id))
            })
            .filter(|m| {
                chat_type.is_none_or(|t| {
                    let p2p = w.chat(&m.chat_id).is_some_and(|c| c.chat_mode == "p2p");
                    (t == "p2p") == p2p
                })
            })
            .filter(|m| m.body.content.to_lowercase().contains(&query))
            .collect();
        hits.sort_by_key(|m| std::cmp::Reverse(m.create_time.parse::<u128>().unwrap_or(0)));
        let items: Vec<Value> = hits
            .iter()
            .map(|m| json!({"meta_data": {"message_id": m.message_id}}))
            .collect();
        let (page, more, token) = paginate(&items, &q, 20, 50)?;
        Ok(page_json(page, more, token))
    })();
    finish(&sh, &ctx, r)
}

/// `GET /open-apis/im/v1/messages/mget?message_ids=…&message_ids=…` (what lark-cli calls after a
/// search). Messages the caller can't read are left out.
async fn mget_messages(
    State(sh): St,
    m: Method,
    OriginalUri(u): OriginalUri,
    h: HeaderMap,
) -> Response {
    let ctx = match gate(&sh, Endpoint::MgetMessages, Auth::Any, &m, &u, &h) {
        Ok(c) => c,
        Err(r) => return r,
    };
    let r = (|| {
        let ids: Vec<String> = u
            .query()
            .unwrap_or("")
            .split('&')
            .filter_map(|kv| kv.split_once('='))
            .filter(|(k, _)| *k == "message_ids")
            .map(|(_, v)| v.to_string())
            .collect();
        if ids.is_empty() || ids.len() > 50 {
            return Err(bad(
                codes::FIELD_VALIDATION,
                "field validation failed: message_ids (1-50)",
            ));
        }
        let w = sh.world();
        let items: Vec<Value> = ids
            .iter()
            .filter_map(|id| w.messages.iter().find(|m| &m.message_id == id))
            .filter(|m| {
                w.chat(&m.chat_id).is_some_and(|c| {
                    ensure_in(&w, &ctx.caller, &c.chat_id).is_ok()
                        && user_may_read(&w, &ctx, c).is_ok()
                })
            })
            .map(|m| json!(m))
            .collect();
        Ok(json!({"items": items}))
    })();
    finish(&sh, &ctx, r)
}

async fn get_resource(
    State(sh): St,
    m: Method,
    OriginalUri(u): OriginalUri,
    h: HeaderMap,
    Path((message_id, file_key)): Path<(String, String)>,
    Query(q): Q,
) -> Response {
    let ctx = match gate(&sh, Endpoint::GetResource, Auth::Any, &m, &u, &h) {
        Ok(c) => c,
        Err(r) => return r,
    };
    let (delay, answer) = {
        let rt = sh.rt();
        (rt.resource_delay, rt.resource_answer.clone())
    };
    if !delay.is_zero() {
        tokio::time::sleep(delay).await;
    }
    if let Some((status, ct, body)) = answer {
        let mut resp =
            (StatusCode::from_u16(status).unwrap_or(StatusCode::OK), body).into_response();
        if let Ok(v) = HeaderValue::from_str(&ct) {
            resp.headers_mut().insert(header::CONTENT_TYPE, v);
        }
        return resp;
    }
    let kind = q.get("type").cloned().unwrap_or_default();
    let found = {
        let w = sh.world();
        w.resources
            .iter()
            .find(|r| r.message_id == message_id && r.file_key == file_key)
            .cloned()
    };
    let r = match (kind.as_str(), found) {
        ("image" | "file", Some(res)) => Ok(res),
        ("image" | "file", None) => Err(bad(codes::IM_RESOURCE_NOT_FOUND, "File not in message.")),
        _ => Err(bad(
            codes::FIELD_VALIDATION,
            "field validation failed: type must be image or file",
        )),
    };
    match r {
        Ok(res) => {
            sh.rt().requests[ctx.idx].code = 0;
            let mut resp = (StatusCode::OK, res.bytes).into_response();
            if let Ok(v) = HeaderValue::from_str(&res.content_type) {
                resp.headers_mut().insert(header::CONTENT_TYPE, v);
            }
            if let Some(name) = res.file_name
                && let Ok(v) =
                    HeaderValue::from_str(&format!("attachment; filename*=UTF-8''{}", enc(&name)))
            {
                resp.headers_mut().insert(header::CONTENT_DISPOSITION, v);
            }
            resp
        }
        Err(e) => finish(&sh, &ctx, Err(e)),
    }
}

// ---------------------------------------------------------------------------------------------
// Chats.

fn chat_json(w: &World, c: &Chat, id_type: &str) -> Value {
    let owner = w
        .user_by_open_id(&c.owner_open_id)
        .map(|u| u.ids.by_type(id_type).to_string())
        .unwrap_or_else(|| c.owner_open_id.clone());
    let users = w.chat_members(&c.chat_id).count();
    json!({
        "avatar": "https://s1-imfile.feishucdn.com/static-resource/v1/fake-chat-avatar",
        "name": c.name,
        "description": c.description,
        "i18n_names": {},
        "add_member_permission": "all_members",
        "share_card_permission": "allowed",
        "at_all_permission": "all_members",
        "edit_permission": "all_members",
        "owner_id_type": id_type,
        "owner_id": owner,
        "user_manager_id_list": [],
        "bot_manager_id_list": [],
        "chat_mode": c.chat_mode,
        "chat_type": c.chat_type,
        "chat_tag": "inner",
        "join_message_visibility": "all_members",
        "leave_message_visibility": "all_members",
        "membership_approval": "no_approval_required",
        "moderation_permission": "all_members",
        "external": c.external,
        "tenant_key": w.app.tenant_key,
        "user_count": users.to_string(),
        "bot_count": if c.bot_in_chat { "1" } else { "0" },
        "group_message_type": if c.chat_mode == "topic" { "thread" } else { "chat" },
        "chat_status": c.chat_status,
    })
}

fn id_type(q: &HashMap<String, String>, key: &str) -> String {
    q.get(key).cloned().unwrap_or_else(|| "open_id".into())
}

async fn get_chat(
    State(sh): St,
    m: Method,
    OriginalUri(u): OriginalUri,
    h: HeaderMap,
    Path(chat_id): Path<String>,
    Query(q): Q,
) -> Response {
    let ctx = match gate(&sh, Endpoint::GetChat, Auth::Any, &m, &u, &h) {
        Ok(c) => c,
        Err(r) => return r,
    };
    let r = (|| {
        let w = sh.world();
        let c = w
            .chat(&chat_id)
            .ok_or_else(|| bad(codes::CHAT_NOT_FOUND, "invalid chat_id"))?;
        if c.chat_status != "normal" {
            return Err(bad(codes::CHAT_DISBANDED, "chat is disbanded"));
        }
        ensure_in(&w, &ctx.caller, &chat_id)?;
        Ok(chat_json(&w, c, &id_type(&q, "user_id_type")))
    })();
    finish(&sh, &ctx, r)
}

async fn chat_members(
    State(sh): St,
    m: Method,
    OriginalUri(u): OriginalUri,
    h: HeaderMap,
    Path(chat_id): Path<String>,
    Query(q): Q,
) -> Response {
    let ctx = match gate(&sh, Endpoint::ChatMembers, Auth::Any, &m, &u, &h) {
        Ok(c) => c,
        Err(r) => return r,
    };
    let r = (|| {
        let w = sh.world();
        let c = w
            .chat(&chat_id)
            .ok_or_else(|| bad(codes::CHAT_NOT_FOUND, "invalid chat_id"))?;
        if c.chat_status != "normal" {
            return Err(bad(codes::CHAT_DISBANDED, "chat is disbanded"));
        }
        need_scopes(
            &ctx,
            &[&["im:chat.members:read", "im:chat:readonly", "im:chat"]],
        )?;
        ensure_in(&w, &ctx.caller, &chat_id)?;
        let t = id_type(&q, "member_id_type");
        let all: Vec<Value> = w
            .chat_members(&chat_id)
            .map(|mb| {
                json!({
                    "member_id_type": t,
                    "member_id": mb.ids.by_type(&t),
                    "name": mb.name,
                    "tenant_key": mb.tenant_key,
                })
            })
            .collect();
        let (page, more, token) = paginate(&all, &q, 20, 100)?;
        let mut v = page_json(page, more, token);
        v["member_total"] = json!(all.len());
        Ok(v)
    })();
    finish(&sh, &ctx, r)
}

/// Last activity of a chat: its newest message, else 0.
fn active_ms(w: &World, chat_id: &str) -> u128 {
    w.messages
        .iter()
        .filter(|m| m.chat_id == chat_id)
        .filter_map(|m| m.create_time.parse::<u128>().ok())
        .max()
        .unwrap_or(0)
}

/// `GET /im/v1/chats?types=p2p,group&sort_type=ByActiveTimeDesc`. Tenant tokens list the groups
/// the bot is in (never p2p chats); user tokens list the user's chats, p2p included when asked
/// for (lark-cli `im_chat_list.go`; `types` omitted = groups only).
async fn list_chats(
    State(sh): St,
    m: Method,
    OriginalUri(u): OriginalUri,
    h: HeaderMap,
    Query(q): Q,
) -> Response {
    let ctx = match gate(&sh, Endpoint::ListChats, Auth::Any, &m, &u, &h) {
        Ok(c) => c,
        Err(r) => return r,
    };
    let r = (|| {
        need_scopes(&ctx, &[&["im:chat:read", "im:chat:readonly"]])?;
        let types: Vec<&str> = match q.get("types").map(String::as_str) {
            None | Some("") => vec!["group"],
            Some(t) => t.split(',').map(str::trim).collect(),
        };
        if let Some(bad_t) = types.iter().find(|t| !["p2p", "group"].contains(t)) {
            return Err(bad(
                codes::FIELD_VALIDATION,
                format!("field validation failed: types contains {bad_t}"),
            ));
        }
        let w = sh.world();
        let t = id_type(&q, "user_id_type");
        let mut chats: Vec<&Chat> = w
            .chats
            .iter()
            .filter(|c| c.chat_status == "normal")
            .filter(|c| match &ctx.caller {
                Caller::User(me) => is_member(&w, &c.chat_id, me),
                _ => c.bot_in_chat && c.chat_mode != "p2p",
            })
            .filter(|c| {
                let k = if c.chat_mode == "p2p" { "p2p" } else { "group" };
                types.contains(&k)
            })
            .collect();
        if q.get("sort_type").map(String::as_str) == Some("ByActiveTimeDesc") {
            chats.sort_by_key(|c| std::cmp::Reverse(active_ms(&w, &c.chat_id)));
        }
        let me = match &ctx.caller {
            Caller::User(me) => Some(me.as_str()),
            _ => None,
        };
        let all: Vec<Value> = chats
            .iter()
            .map(|c| {
                let owner = w
                    .user_by_open_id(&c.owner_open_id)
                    .map(|u| u.ids.by_type(&t).to_string())
                    .unwrap_or_default();
                // A p2p chat is named after the other party.
                let name = if c.chat_mode == "p2p" && c.name.is_empty() {
                    w.chat_members(&c.chat_id)
                        .find(|m| Some(m.ids.open_id.as_str()) != me)
                        .map(|m| m.name.clone())
                        .unwrap_or_default()
                } else {
                    c.name.clone()
                };
                json!({
                    "chat_id": c.chat_id,
                    "avatar": "https://s1-imfile.feishucdn.com/static-resource/v1/fake-chat-avatar",
                    "name": name,
                    "description": c.description,
                    "owner_id": owner,
                    "owner_id_type": t,
                    "external": c.external,
                    "tenant_key": w.app.tenant_key,
                    "chat_status": c.chat_status,
                    "chat_mode": c.chat_mode,
                })
            })
            .collect();
        let (page, more, token) = paginate(&all, &q, 20, 100)?;
        Ok(page_json(page, more, token))
    })();
    finish(&sh, &ctx, r)
}

// ---------------------------------------------------------------------------------------------
// Contact v3.

fn dept_id(w: &World, open_id: &str, t: &str) -> String {
    if open_id == "0" {
        return "0".into();
    }
    match (t, w.department(open_id)) {
        ("department_id", Some(d)) => d.department_id.clone(),
        _ => open_id.to_string(),
    }
}

fn user_json(w: &World, u: &User, uid_t: &str, did_t: &str) -> Value {
    let leader = u
        .leader_open_id
        .as_deref()
        .and_then(|id| w.user_by_open_id(id))
        .map(|l| l.ids.by_type(uid_t).to_string());
    let mut v = json!({
        "union_id": u.ids.union_id,
        "user_id": u.ids.user_id,
        "open_id": u.ids.open_id,
        "name": u.name,
        "en_name": u.en_name,
        "email": u.email,
        "enterprise_email": u.enterprise_email,
        "job_title": u.job_title,
        "department_ids": u.department_ids.iter().map(|d| dept_id(w, d, did_t)).collect::<Vec<_>>(),
        "status": {
            "is_frozen": u.is_frozen,
            "is_resigned": u.is_resigned,
            "is_activated": u.is_activated,
            "is_exited": false,
            "is_unjoin": false,
        },
        "avatar": {"avatar_72": "https://s1-imfile.feishucdn.com/static-resource/v1/fake-user-avatar"},
    });
    if let Some(l) = leader {
        v["leader_user_id"] = json!(l);
    }
    v
}

fn dept_json(w: &World, d: &super::model::Department, uid_t: &str, did_t: &str) -> Value {
    let mut v = json!({
        "name": d.name,
        "i18n_name": {"zh_cn": d.name, "ja_jp": "", "en_us": ""},
        "parent_department_id": dept_id(w, &d.parent_department_id, did_t),
        "department_id": d.department_id,
        "open_department_id": d.open_department_id,
        "chat_id": "",
        "order": d.order,
        "member_count": w.users.iter().filter(|u| u.department_ids.contains(&d.open_department_id)).count(),
        "status": {"is_deleted": d.is_deleted},
    });
    if let Some(l) = d
        .leader_open_id
        .as_deref()
        .and_then(|id| w.user_by_open_id(id))
    {
        v["leader_user_id"] = json!(l.ids.by_type(uid_t));
    }
    v
}

async fn contact_scopes(
    State(sh): St,
    m: Method,
    OriginalUri(u): OriginalUri,
    h: HeaderMap,
    Query(q): Q,
) -> Response {
    let ctx = match gate(&sh, Endpoint::ContactScopes, Auth::Any, &m, &u, &h) {
        Ok(c) => c,
        Err(r) => return r,
    };
    let r = (|| {
        let w = sh.world();
        let uid_t = id_type(&q, "user_id_type");
        let did_t = q
            .get("department_id_type")
            .cloned()
            .unwrap_or_else(|| "open_department_id".into());
        let depts: Vec<String> = w
            .scope_department_ids
            .iter()
            .map(|d| dept_id(&w, d, &did_t))
            .collect();
        let users: Vec<String> = w
            .scope_user_ids
            .iter()
            .filter_map(|id| w.user_by_open_id(id))
            .map(|u| u.ids.by_type(&uid_t).to_string())
            .collect();
        // One page holds up to `page_size` of each list (Feishu pages the three lists together).
        let (depts, more_d, token_d) = paginate(&depts, &q, 50, 100)?;
        let (users, more_u, token_u) = paginate(&users, &q, 50, 100)?;
        let (groups, more_g, token_g) = paginate(&w.scope_group_ids, &q, 50, 100)?;
        let more = more_d || more_u || more_g;
        let mut v = json!({"department_ids": depts, "user_ids": users, "group_ids": groups, "has_more": more});
        if more {
            let token = [token_d, token_u, token_g]
                .into_iter()
                .find(|t| !t.is_empty())
                .unwrap_or_default();
            v["page_token"] = json!(token);
        }
        Ok(v)
    })();
    finish(&sh, &ctx, r)
}

async fn department_children(
    State(sh): St,
    m: Method,
    OriginalUri(u): OriginalUri,
    h: HeaderMap,
    Path(department_id): Path<String>,
    Query(q): Q,
) -> Response {
    let ctx = match gate(&sh, Endpoint::DepartmentChildren, Auth::Any, &m, &u, &h) {
        Ok(c) => c,
        Err(r) => return r,
    };
    let r = (|| {
        let w = sh.world();
        let uid_t = id_type(&q, "user_id_type");
        let did_t = q
            .get("department_id_type")
            .cloned()
            .unwrap_or_else(|| "open_department_id".into());
        let root = if department_id == "0" {
            "0".to_string()
        } else {
            let d = w
                .department(&department_id)
                .filter(|d| !d.is_deleted)
                .ok_or_else(|| bad(codes::CONTACT_NO_DEPT_AUTHORITY, "no dept authority error"))?;
            if !w.department_visible(&d.open_department_id) {
                return Err(bad(
                    codes::CONTACT_NO_DEPT_AUTHORITY,
                    "no dept authority error",
                ));
            }
            d.open_department_id.clone()
        };
        let fetch_child = q.get("fetch_child").map(String::as_str) == Some("true");
        // Breadth-first over the tree; `fetch_child=true` returns every descendant.
        let mut out = Vec::new();
        let mut frontier = vec![root];
        while let Some(parent) = frontier.pop() {
            for d in w
                .departments
                .iter()
                .filter(|d| d.parent_department_id == parent && !d.is_deleted)
            {
                if w.department_visible(&d.open_department_id) {
                    out.push(dept_json(&w, d, &uid_t, &did_t));
                }
                if fetch_child {
                    frontier.insert(0, d.open_department_id.clone());
                }
            }
        }
        let (page, more, token) = paginate(&out, &q, 10, 50)?;
        Ok(page_json(page, more, token))
    })();
    finish(&sh, &ctx, r)
}

async fn users_by_department(
    State(sh): St,
    m: Method,
    OriginalUri(u): OriginalUri,
    h: HeaderMap,
    Query(q): Q,
) -> Response {
    let ctx = match gate(&sh, Endpoint::UsersByDepartment, Auth::Any, &m, &u, &h) {
        Ok(c) => c,
        Err(r) => return r,
    };
    let r = (|| {
        let w = sh.world();
        let uid_t = id_type(&q, "user_id_type");
        let did_t = q
            .get("department_id_type")
            .cloned()
            .unwrap_or_else(|| "open_department_id".into());
        let dep = q
            .get("department_id")
            .filter(|s| !s.is_empty())
            .ok_or_else(|| {
                bad(
                    codes::FIELD_VALIDATION,
                    "field validation failed: department_id is required",
                )
            })?;
        let open = if dep == "0" {
            "0".to_string()
        } else {
            w.department(dep)
                .filter(|d| w.department_visible(&d.open_department_id))
                .map(|d| d.open_department_id.clone())
                .ok_or_else(|| bad(codes::CONTACT_NO_DEPT_AUTHORITY, "no dept authority error"))?
        };
        let all: Vec<Value> = w
            .users
            .iter()
            .filter(|u| {
                if open == "0" {
                    u.department_ids.is_empty()
                } else {
                    u.department_ids.contains(&open)
                }
            })
            .map(|u| user_json(&w, u, &uid_t, &did_t))
            .collect();
        let (page, more, token) = paginate(&all, &q, 10, 50)?;
        Ok(page_json(page, more, token))
    })();
    finish(&sh, &ctx, r)
}

async fn get_department(
    State(sh): St,
    m: Method,
    OriginalUri(u): OriginalUri,
    h: HeaderMap,
    Path(department_id): Path<String>,
    Query(q): Q,
) -> Response {
    let ctx = match gate(&sh, Endpoint::GetDepartment, Auth::Any, &m, &u, &h) {
        Ok(c) => c,
        Err(r) => return r,
    };
    let r = (|| {
        let w = sh.world();
        let uid_t = id_type(&q, "user_id_type");
        let did_t = q
            .get("department_id_type")
            .cloned()
            .unwrap_or_else(|| "open_department_id".into());
        let d = w
            .department(&department_id)
            .filter(|d| w.department_visible(&d.open_department_id))
            .ok_or_else(|| bad(codes::CONTACT_NO_DEPT_AUTHORITY, "no dept authority error"))?;
        Ok(json!({"department": dept_json(&w, d, &uid_t, &did_t)}))
    })();
    finish(&sh, &ctx, r)
}

async fn get_user(
    State(sh): St,
    m: Method,
    OriginalUri(u): OriginalUri,
    h: HeaderMap,
    Path(user_id): Path<String>,
    Query(q): Q,
) -> Response {
    let ctx = match gate(&sh, Endpoint::GetUser, Auth::Any, &m, &u, &h) {
        Ok(c) => c,
        Err(r) => return r,
    };
    let r = (|| {
        let w = sh.world();
        let uid_t = id_type(&q, "user_id_type");
        let did_t = q
            .get("department_id_type")
            .cloned()
            .unwrap_or_else(|| "open_department_id".into());
        let user = w
            .users
            .iter()
            .find(|x| x.ids.by_type(&uid_t) == user_id)
            .filter(|x| w.user_visible(x))
            .ok_or_else(|| bad(codes::CONTACT_NO_USER_AUTHORITY, "no user authority error"))?;
        Ok(json!({"user": user_json(&w, user, &uid_t, &did_t)}))
    })();
    finish(&sh, &ctx, r)
}

/// `GET /contact/v3/users/batch?user_ids=a&user_ids=b&user_id_type=`: the visible ones (Feishu leaves
/// the others out). Needs `contact:user.base:readonly`, as a user too.
async fn batch_users(
    State(sh): St,
    m: Method,
    OriginalUri(u): OriginalUri,
    h: HeaderMap,
    Query(q): Q,
) -> Response {
    let ctx = match gate(&sh, Endpoint::BatchUsers, Auth::Any, &m, &u, &h) {
        Ok(c) => c,
        Err(r) => return r,
    };
    let r = (|| {
        need_scopes(&ctx, &[&["contact:user.base:readonly"]])?;
        let ids: Vec<String> = url::form_urlencoded::parse(u.query().unwrap_or("").as_bytes())
            .filter(|(k, _)| k == "user_ids")
            .map(|(_, v)| v.into_owned())
            .collect();
        if ids.is_empty() || ids.len() > 50 {
            return Err(bad(codes::FIELD_VALIDATION, "user_ids: 1 to 50 ids"));
        }
        let w = sh.world();
        let uid_t = id_type(&q, "user_id_type");
        let did_t = q
            .get("department_id_type")
            .cloned()
            .unwrap_or_else(|| "open_department_id".into());
        let items: Vec<Value> = ids
            .iter()
            .filter_map(|id| w.users.iter().find(|x| x.ids.by_type(&uid_t) == id))
            .filter(|x| w.user_visible(x))
            .map(|x| user_json(&w, x, &uid_t, &did_t))
            .collect();
        Ok(json!({ "items": items }))
    })();
    finish(&sh, &ctx, r)
}

/// `GET /tenant/v2/tenant/query`: the tenant's name and key (tenant token).
async fn tenant_query(
    State(sh): St,
    m: Method,
    OriginalUri(u): OriginalUri,
    h: HeaderMap,
) -> Response {
    let ctx = match gate(&sh, Endpoint::TenantQuery, Auth::Tenant, &m, &u, &h) {
        Ok(c) => c,
        Err(r) => return r,
    };
    let r = {
        let w = sh.world();
        Ok(json!({ "tenant": {
            "name": w.tenant_name, "display_id": "F000001", "tenant_tag": 0,
            "tenant_key": w.app.tenant_key, "avatar": {},
        }}))
    };
    finish(&sh, &ctx, r)
}

async fn group_members(
    State(sh): St,
    m: Method,
    OriginalUri(u): OriginalUri,
    h: HeaderMap,
    Path(group_id): Path<String>,
    Query(q): Q,
) -> Response {
    let ctx = match gate(&sh, Endpoint::GroupMembers, Auth::Any, &m, &u, &h) {
        Ok(c) => c,
        Err(r) => return r,
    };
    let r = (|| {
        let w = sh.world();
        let mid_t = id_type(&q, "member_id_type");
        let g = w
            .groups
            .iter()
            .find(|g| g.group_id == group_id && w.scope_group_ids.contains(&g.group_id))
            .ok_or_else(|| bad(codes::CONTACT_GROUP_NOT_FOUND, "group not found"))?;
        let all: Vec<Value> = g
            .member_open_ids
            .iter()
            .filter_map(|id| w.user_by_open_id(id))
            .map(|x| {
                json!({
                    "member_id": x.ids.by_type(&mid_t),
                    "member_type": "user",
                    "member_id_type": mid_t,
                })
            })
            .collect();
        let (page, more, token) = paginate(&all, &q, 50, 100)?;
        let mut v = json!({"memberlist": page, "has_more": more});
        if more {
            v["page_token"] = json!(token);
        }
        Ok(v)
    })();
    finish(&sh, &ctx, r)
}

// ---------------------------------------------------------------------------------------------
// User OAuth (F6). The authorize page is accounts.feishu.cn in reality; here it lives on the same
// base URL and immediately redirects back as if the user clicked "授权".

/// Percent-encodes a query value (unreserved characters kept).
pub(super) fn enc(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

async fn user_info(
    State(sh): St,
    m: Method,
    OriginalUri(u): OriginalUri,
    h: HeaderMap,
) -> Response {
    let ctx = match gate(&sh, Endpoint::UserInfo, Auth::User, &m, &u, &h) {
        Ok(c) => c,
        Err(r) => return r,
    };
    let r = (|| {
        let Caller::User(open_id) = &ctx.caller else {
            return Err(bad(codes::INVALID_TOKEN, "user access token required"));
        };
        let w = sh.world();
        let u = w
            .user_by_open_id(open_id)
            .ok_or_else(|| bad(codes::USER_TOKEN_INVALID, "user not found"))?;
        Ok(json!({
            "name": u.name,
            "en_name": u.en_name,
            "avatar_url": "https://s1-imfile.feishucdn.com/static-resource/v1/fake-user-avatar",
            "open_id": u.ids.open_id,
            "union_id": u.ids.union_id,
            "email": u.email,
            "enterprise_email": u.enterprise_email,
            "user_id": u.ids.user_id,
            "tenant_key": w.app.tenant_key,
        }))
    })();
    finish(&sh, &ctx, r)
}

// ---------------------------------------------------------------------------------------------
// Control routes for out-of-process tests (the `steve-fake-feishu` binary). Localhost only.

fn sent_json(s: &SentMessage) -> Value {
    json!({
        "message_id": s.message_id,
        "reply_to": s.reply_to,
        "receive_id_type": s.receive_id_type,
        "receive_id": s.receive_id,
        "chat_id": s.chat_id,
        "msg_type": s.msg_type,
        "content": s.content,
        "uuid": s.uuid,
        "reply_in_thread": s.reply_in_thread,
        "as_user": s.as_user,
    })
}

async fn ctl_sent(State(sh): St) -> Response {
    let sent: Vec<Value> = sh.rt().sent.iter().map(sent_json).collect();
    axum::Json(json!({"sent": sent})).into_response()
}

fn ctl_pusher(sh: &Arc<Shared>) -> Result<super::EventPusher, Response> {
    let url = sh.rt().hook_url.clone();
    match url {
        Some(u) => Ok(super::EventPusher::new(sh.clone(), u)),
        None => Err((StatusCode::CONFLICT, "no hook url configured (--hook-url)").into_response()),
    }
}

fn delivery_json(d: &super::Delivery) -> Value {
    json!({"event_id": d.event_id, "status": d.status, "body": d.body})
}

async fn ctl_push(State(sh): St, body: Bytes) -> Response {
    let p = match ctl_pusher(&sh) {
        Ok(p) => p,
        Err(r) => return r,
    };
    let Ok(v) = serde_json::from_slice::<Value>(&body) else {
        return (
            StatusCode::BAD_REQUEST,
            "body must be {event_type, event} or a full envelope",
        )
            .into_response();
    };
    let res = match (v.get("event_type").and_then(Value::as_str), v.get("event")) {
        (Some(t), Some(e)) if v.get("header").is_none() => p.push(t, e.clone()).await,
        _ => p.push_envelope(v).await,
    };
    match res {
        Ok(d) => axum::Json(delivery_json(&d)).into_response(),
        Err(e) => (StatusCode::BAD_GATEWAY, e.to_string()).into_response(),
    }
}

async fn ctl_url_verification(State(sh): St) -> Response {
    let p = match ctl_pusher(&sh) {
        Ok(p) => p,
        Err(r) => return r,
    };
    match p.push_url_verification().await {
        Ok((challenge, d)) => {
            let mut v = delivery_json(&d);
            v["challenge"] = json!(challenge);
            axum::Json(v).into_response()
        }
        Err(e) => (StatusCode::BAD_GATEWAY, e.to_string()).into_response(),
    }
}

async fn ctl_redeliver(State(sh): St, Path(event_id): Path<String>) -> Response {
    let p = match ctl_pusher(&sh) {
        Ok(p) => p,
        Err(r) => return r,
    };
    match p.redeliver(&event_id).await {
        Ok(d) => axum::Json(delivery_json(&d)).into_response(),
        Err(e) => (StatusCode::BAD_GATEWAY, e.to_string()).into_response(),
    }
}

async fn ctl_expire_tokens(State(sh): St) -> Response {
    for t in sh.rt().tokens.values_mut() {
        t.revoked = true;
    }
    StatusCode::NO_CONTENT.into_response()
}

/// `{endpoint}` of the control routes: the snake_case name of an [`Endpoint`].
fn endpoint_named(name: &str) -> Option<Endpoint> {
    Some(match name {
        "list_chats" => Endpoint::ListChats,
        "get_chat" => Endpoint::GetChat,
        "chat_members" => Endpoint::ChatMembers,
        "list_messages" => Endpoint::ListMessages,
        "send_message" => Endpoint::SendMessage,
        "reply_message" => Endpoint::ReplyMessage,
        "get_resource" => Endpoint::GetResource,
        "contact_scopes" => Endpoint::ContactScopes,
        "department_children" => Endpoint::DepartmentChildren,
        "users_by_department" => Endpoint::UsersByDepartment,
        _ => return None,
    })
}

/// `POST /_fake/deny {endpoint}`: that endpoint answers 99991672 from now on.
async fn ctl_deny(State(sh): St, axum::Json(b): axum::Json<Value>) -> Response {
    match b
        .get("endpoint")
        .and_then(Value::as_str)
        .and_then(endpoint_named)
    {
        Some(ep) => {
            sh.rt().denied.insert(ep);
            StatusCode::NO_CONTENT.into_response()
        }
        None => StatusCode::BAD_REQUEST.into_response(),
    }
}

/// `POST /_fake/allow {endpoint}`: undoes `/_fake/deny`.
async fn ctl_allow(State(sh): St, axum::Json(b): axum::Json<Value>) -> Response {
    match b
        .get("endpoint")
        .and_then(Value::as_str)
        .and_then(endpoint_named)
    {
        Some(ep) => {
            sh.rt().denied.remove(&ep);
            StatusCode::NO_CONTENT.into_response()
        }
        None => StatusCode::BAD_REQUEST.into_response(),
    }
}

/// `POST /_fake/user_scopes {scopes}`: the user scopes enabled in the console (a grant is the
/// requested ones ∩ these).
async fn ctl_user_scopes(State(sh): St, axum::Json(b): axum::Json<Value>) -> Response {
    let Some(xs) = b.get("scopes").and_then(Value::as_array) else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    sh.world().app.user_scopes = xs
        .iter()
        .filter_map(Value::as_str)
        .map(String::from)
        .collect();
    StatusCode::NO_CONTENT.into_response()
}
