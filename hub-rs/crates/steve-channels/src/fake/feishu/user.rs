//! User identity: OAuth authorization code + PKCE, refresh with
//! single-use refresh tokens, and the device flow lark-cli uses.
//!
//! The real authorize page and the device-flow endpoints live on `accounts.feishu.cn`; the
//! fake serves them on its one base URL. The authorize page answers at once with a 302, as if
//! the user had clicked "授权"; a device authorization waits for [`FakeFeishu::device_approve`].
//!
//! Token responses are flat (no `data`); errors are `{code, error, error_description}` with
//! HTTP 400 (authen v2) — the device-flow endpoints answer the same way without `code`.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use axum::Json;
use axum::body::Bytes;
use axum::extract::{OriginalUri, Query, State};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use super::api::{Auth, bad, enc, finish, gate};
use super::{Endpoint, FakeFeishu, Runtime, Shared, TokenInfo, TokenKind, codes};

/// A pending authorization code.
#[derive(Debug, Clone)]
pub(crate) struct OAuthCode {
    pub open_id: String,
    pub challenge: Option<String>,
    pub redirect_uri: String,
    pub scopes: Vec<String>,
}

/// An issued refresh token. Kept after use so a second use can be told apart from garbage.
#[derive(Debug, Clone)]
pub(crate) struct RefreshInfo {
    pub open_id: String,
    pub scopes: Vec<String>,
    pub expires_at: Instant,
    pub used: bool,
    pub revoked: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DeviceStatus {
    Pending,
    Approved(String),
    Denied,
    /// Tokens were handed out; the device code is spent.
    Done,
}

#[derive(Debug, Clone)]
pub(crate) struct DeviceAuth {
    pub user_code: String,
    pub scopes: Vec<String>,
    pub status: DeviceStatus,
    pub expires_at: Instant,
    pub last_poll: Option<Instant>,
}

type Q = Query<HashMap<String, String>>;
type St = State<Arc<Shared>>;

/// Granted scopes: requested ∩ enabled; nothing requested means every enabled scope.
fn grant(requested: Option<&str>, enabled: &[String]) -> Vec<String> {
    match requested.map(str::trim).filter(|s| !s.is_empty()) {
        None => enabled.to_vec(),
        Some(r) => {
            let mut out: Vec<String> = Vec::new();
            for s in r.split_whitespace() {
                if enabled.iter().any(|e| e == s) && !out.iter().any(|o| o == s) {
                    out.push(s.to_string());
                }
            }
            out
        }
    }
}

/// Issues an access token (and a refresh token when `offline_access` was granted and the
/// console switch is on). Returns the flat token response.
fn issue(rt: &mut Runtime, refresh_enabled: bool, open_id: &str, scopes: &[String]) -> Value {
    let access = format!("u-{}", rt.id(""));
    let ttl = rt.token_ttl;
    rt.tokens.insert(
        access.clone(),
        TokenInfo {
            kind: TokenKind::User {
                open_id: open_id.to_string(),
                scopes: scopes.to_vec(),
            },
            expires_at: Instant::now() + ttl,
            revoked: false,
        },
    );
    let mut v = json!({
        "code": 0,
        "access_token": access,
        "expires_in": ttl.as_secs(),
        // Feishu echoes offline_access in the scope even when it issues no refresh token.
        "scope": scopes.join(" "),
        "token_type": "Bearer",
    });
    if refresh_enabled && scopes.iter().any(|s| s == "offline_access") {
        let refresh = format!("ur-{}", rt.id(""));
        let rttl = rt.refresh_ttl;
        rt.refresh_tokens.insert(
            refresh.clone(),
            RefreshInfo {
                open_id: open_id.to_string(),
                scopes: scopes.to_vec(),
                expires_at: Instant::now() + rttl,
                used: false,
                revoked: false,
            },
        );
        v["refresh_token"] = json!(refresh);
        v["refresh_token_expires_in"] = json!(rttl.as_secs());
    }
    v
}

fn oauth_error(code: Option<i64>, error: &str, desc: &str) -> (i64, Value) {
    let mut v = json!({"error": error, "error_description": desc});
    if let Some(c) = code {
        v["code"] = json!(c);
    }
    (code.unwrap_or(-1), v)
}

/// Records the result code and renders a flat OAuth response.
fn flat(sh: &Shared, idx: usize, r: Result<Value, (i64, Value)>) -> Response {
    let (code, status, v) = match r {
        Ok(v) => (0, StatusCode::OK, v),
        Err((c, v)) => (c, StatusCode::BAD_REQUEST, v),
    };
    sh.rt().requests[idx].code = code;
    (status, axum::Json(v)).into_response()
}

/// PKCE S256: base64url(sha256(verifier)) without padding.
fn pkce_s256(verifier: &str) -> String {
    use base64::Engine as _;
    use sha2::Digest as _;
    base64::engine::general_purpose::URL_SAFE_NO_PAD
        .encode(sha2::Sha256::digest(verifier.as_bytes()))
}

/// Parses a JSON or form body into string fields (v2 takes JSON, the device flow forms).
fn fields(headers: &HeaderMap, body: &Bytes) -> Option<HashMap<String, String>> {
    let is_form = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|c| c.starts_with("application/x-www-form-urlencoded"));
    if is_form {
        let s = std::str::from_utf8(body).ok()?;
        return Some(
            s.split('&')
                .filter_map(|kv| kv.split_once('='))
                .map(|(k, v)| (dec(k), dec(v)))
                .collect(),
        );
    }
    let v: Value = serde_json::from_slice(body).ok()?;
    let o = v.as_object()?;
    Some(
        o.iter()
            .filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string())))
            .collect(),
    )
}

/// Form-decodes `+` and `%XX`.
fn dec(s: &str) -> String {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        match b[i] {
            b'+' => out.push(b' '),
            b'%' if i + 2 < b.len() => {
                match u8::from_str_radix(std::str::from_utf8(&b[i + 1..i + 3]).unwrap_or(""), 16) {
                    Ok(x) => {
                        out.push(x);
                        i += 2;
                    }
                    Err(_) => out.push(b'%'),
                }
            }
            c => out.push(c),
        }
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

// ---------------------------------------------------------------------------------------------
// Authorization code + PKCE.

pub(crate) async fn authorize(
    State(sh): St,
    m: Method,
    OriginalUri(u): OriginalUri,
    h: HeaderMap,
    Query(q): Q,
) -> Response {
    let ctx = match gate(&sh, Endpoint::OAuthAuthorize, Auth::None, &m, &u, &h) {
        Ok(c) => c,
        Err(r) => return r,
    };
    let app = sh.world().app.clone();
    let (Some(client_id), Some(redirect)) = (q.get("client_id"), q.get("redirect_uri")) else {
        return finish(
            &sh,
            &ctx,
            Err(bad(
                codes::FIELD_VALIDATION,
                "client_id and redirect_uri are required",
            )),
        );
    };
    if client_id != &app.app_id {
        return finish(
            &sh,
            &ctx,
            Err(bad(codes::FIELD_VALIDATION, "invalid client_id")),
        );
    }
    let open_id = sh.world().oauth_user_open_id.clone();
    let scopes = grant(q.get("scope").map(String::as_str), &app.user_scopes);
    let code = {
        let mut rt = sh.rt();
        let code = rt.id("");
        rt.oauth_codes.insert(
            code.clone(),
            OAuthCode {
                open_id,
                challenge: q.get("code_challenge").cloned(),
                redirect_uri: redirect.clone(),
                scopes,
            },
        );
        rt.requests[ctx.idx].code = 0;
        code
    };
    let sep = if redirect.contains('?') { '&' } else { '?' };
    let mut loc = format!("{redirect}{sep}code={}", enc(&code));
    if let Some(s) = q.get("state") {
        loc.push_str(&format!("&state={}", enc(s)));
    }
    let mut r = StatusCode::FOUND.into_response();
    if let Ok(v) = HeaderValue::from_str(&loc) {
        r.headers_mut().insert(header::LOCATION, v);
    }
    r
}

/// `POST /open-apis/authen/v2/oauth/token`: `authorization_code` and `refresh_token` grants.
pub(crate) async fn token(
    State(sh): St,
    m: Method,
    OriginalUri(u): OriginalUri,
    h: HeaderMap,
    body: Bytes,
) -> Response {
    let ctx = match gate(&sh, Endpoint::OAuthToken, Auth::None, &m, &u, &h) {
        Ok(c) => c,
        Err(r) => return r,
    };
    let r = (|| {
        let Some(v) = fields(&h, &body) else {
            return Err(oauth_error(
                Some(codes::FIELD_VALIDATION),
                "invalid_request",
                "body must be JSON",
            ));
        };
        let f = |k: &str| v.get(k).map(String::as_str).filter(|s| !s.is_empty());
        let app = sh.world().app.clone();
        if f("client_id") != Some(app.app_id.as_str())
            || f("client_secret") != Some(app.app_secret.as_str())
        {
            return Err(oauth_error(
                Some(codes::OAUTH_INVALID_CLIENT),
                "invalid_client",
                "client_id or client_secret is invalid",
            ));
        }
        let mut rt = sh.rt();
        match f("grant_type") {
            Some("authorization_code") => {
                let code = f("code").unwrap_or("");
                // Codes are single-use, even when the exchange fails.
                let Some(c) = rt.oauth_codes.remove(code) else {
                    return Err(oauth_error(
                        Some(codes::OAUTH_INVALID_GRANT),
                        "invalid_grant",
                        "authorization code is invalid or used",
                    ));
                };
                if let Some(r) = f("redirect_uri")
                    && r != c.redirect_uri
                {
                    return Err(oauth_error(
                        Some(codes::OAUTH_INVALID_GRANT),
                        "invalid_grant",
                        "redirect_uri mismatch",
                    ));
                }
                if let Some(ch) = &c.challenge
                    && f("code_verifier").map(pkce_s256).as_ref() != Some(ch)
                {
                    return Err(oauth_error(
                        Some(codes::OAUTH_INVALID_GRANT),
                        "invalid_grant",
                        "code_verifier mismatch",
                    ));
                }
                Ok(issue(&mut rt, app.refresh_enabled, &c.open_id, &c.scopes))
            }
            Some("refresh_token") => {
                let tok = f("refresh_token").unwrap_or("");
                let Some(info) = rt.refresh_tokens.get_mut(tok) else {
                    return Err(oauth_error(
                        Some(codes::REFRESH_TOKEN_INVALID),
                        "invalid_grant",
                        "refresh token is invalid",
                    ));
                };
                if info.revoked {
                    return Err(oauth_error(
                        Some(codes::REFRESH_TOKEN_REVOKED),
                        "invalid_grant",
                        "refresh token has been revoked",
                    ));
                }
                if info.used {
                    return Err(oauth_error(
                        Some(codes::REFRESH_TOKEN_REUSED),
                        "invalid_grant",
                        "refresh token has been used",
                    ));
                }
                if info.expires_at <= Instant::now() {
                    return Err(oauth_error(
                        Some(codes::REFRESH_TOKEN_EXPIRED),
                        "invalid_grant",
                        "refresh token has expired",
                    ));
                }
                info.used = true;
                let (open_id, scopes) = (info.open_id.clone(), info.scopes.clone());
                Ok(issue(&mut rt, app.refresh_enabled, &open_id, &scopes))
            }
            _ => Err(oauth_error(
                Some(codes::OAUTH_UNSUPPORTED_GRANT),
                "unsupported_grant_type",
                "grant_type is not supported",
            )),
        }
    })();
    flat(&sh, ctx.idx, r)
}

// ---------------------------------------------------------------------------------------------
// Device flow (lark-cli `internal/auth/device_flow.go`).

const DEVICE_GRANT: &str = "urn:ietf:params:oauth:grant-type:device_code";

/// Basic `app_id:app_secret` check.
fn basic_ok(h: &HeaderMap, app_id: &str, secret: &str) -> bool {
    use base64::Engine as _;
    let Some(v) = h
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Basic "))
    else {
        return false;
    };
    base64::engine::general_purpose::STANDARD
        .decode(v.trim())
        .ok()
        .and_then(|b| String::from_utf8(b).ok())
        == Some(format!("{app_id}:{secret}"))
}

/// `POST /oauth/v1/device_authorization` (form `client_id`, `scope`; Basic auth).
pub(crate) async fn device_authorization(
    State(sh): St,
    m: Method,
    OriginalUri(u): OriginalUri,
    h: HeaderMap,
    body: Bytes,
) -> Response {
    let ctx = match gate(&sh, Endpoint::DeviceAuthorization, Auth::None, &m, &u, &h) {
        Ok(c) => c,
        Err(r) => return r,
    };
    let host = h
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("127.0.0.1")
        .to_string();
    let r = (|| {
        let app = sh.world().app.clone();
        let v = fields(&h, &body).unwrap_or_default();
        if !basic_ok(&h, &app.app_id, &app.app_secret)
            || v.get("client_id").map(String::as_str) != Some(app.app_id.as_str())
        {
            return Err(oauth_error(
                None,
                "invalid_client",
                "client authentication failed",
            ));
        }
        let scopes = grant(v.get("scope").map(String::as_str), &app.user_scopes);
        let mut rt = sh.rt();
        let device_code = rt.id("dc_");
        let n: u32 = rand::random_range(0..100_000_000);
        let user_code = format!("{:04}-{:04}", n / 10_000, n % 10_000);
        let ttl = rt.device_ttl;
        let interval = rt.device_interval;
        rt.device.insert(
            device_code.clone(),
            DeviceAuth {
                user_code: user_code.clone(),
                scopes,
                status: DeviceStatus::Pending,
                expires_at: Instant::now() + ttl,
                last_poll: None,
            },
        );
        let uri = format!("http://{host}/oauth/v1/device/verify");
        Ok(json!({
            "device_code": device_code,
            "user_code": user_code,
            "verification_uri": uri,
            "verification_uri_complete": format!("{uri}?user_code={user_code}"),
            "expires_in": ttl.as_secs(),
            "interval": interval,
        }))
    })();
    flat(&sh, ctx.idx, r)
}

/// `POST /oauth/v3/token` with the device-code grant (form body, client secret in the form).
pub(crate) async fn device_token(
    State(sh): St,
    m: Method,
    OriginalUri(u): OriginalUri,
    h: HeaderMap,
    body: Bytes,
) -> Response {
    let ctx = match gate(&sh, Endpoint::DeviceToken, Auth::None, &m, &u, &h) {
        Ok(c) => c,
        Err(r) => return r,
    };
    let r = (|| {
        let app = sh.world().app.clone();
        let v = fields(&h, &body).unwrap_or_default();
        let f = |k: &str| v.get(k).map(String::as_str).unwrap_or("");
        if f("client_id") != app.app_id || f("client_secret") != app.app_secret {
            return Err(oauth_error(
                None,
                "invalid_client",
                "client authentication failed",
            ));
        }
        if f("grant_type") != DEVICE_GRANT {
            return Err(oauth_error(
                None,
                "unsupported_grant_type",
                "grant_type is not supported",
            ));
        }
        let mut rt = sh.rt();
        let interval = std::time::Duration::from_secs(rt.device_interval);
        let now = Instant::now();
        let Some(d) = rt.device.get_mut(f("device_code")) else {
            return Err(oauth_error(None, "invalid_grant", "device_code is invalid"));
        };
        if d.expires_at <= now {
            return Err(oauth_error(
                None,
                "expired_token",
                "device_code has expired",
            ));
        }
        let too_fast = d
            .last_poll
            .is_some_and(|t| now.duration_since(t) < interval);
        d.last_poll = Some(now);
        match d.status.clone() {
            DeviceStatus::Done => Err(oauth_error(None, "invalid_grant", "device_code is used")),
            DeviceStatus::Denied => Err(oauth_error(
                None,
                "access_denied",
                "the user denied the authorization",
            )),
            DeviceStatus::Pending if too_fast => {
                Err(oauth_error(None, "slow_down", "polling too frequently"))
            }
            DeviceStatus::Pending => Err(oauth_error(
                None,
                "authorization_pending",
                "the user has not authorized yet",
            )),
            DeviceStatus::Approved(open_id) => {
                d.status = DeviceStatus::Done;
                let scopes = d.scopes.clone();
                Ok(issue(&mut rt, app.refresh_enabled, &open_id, &scopes))
            }
        }
    })();
    flat(&sh, ctx.idx, r)
}

// ---------------------------------------------------------------------------------------------
// Test API.

/// Settles the pending device authorization with `user_code` (`None`: the newest pending one).
/// Returns its user code.
fn device_set(sh: &Shared, user_code: Option<&str>, approve: bool) -> Option<String> {
    let status = if approve {
        DeviceStatus::Approved(sh.world().oauth_user_open_id.clone())
    } else {
        DeviceStatus::Denied
    };
    let mut rt = sh.rt();
    let d = rt
        .device
        .values_mut()
        .filter(|d| d.status == DeviceStatus::Pending)
        .filter(|d| user_code.is_none_or(|c| d.user_code == c))
        .max_by_key(|d| d.expires_at)?;
    d.status = status;
    Some(d.user_code.clone())
}

/// `POST /_fake/device/approve` and `/_fake/device/deny` `{user_code?}` (form, JSON or query;
/// without one: the newest pending authorization) → `{ok, user_code}`, `404` when nothing is
/// pending. For UI tests that finish “授权你本人” with the device code on screen.
pub(crate) async fn ctl_device_approve(State(sh): St, q: Q, h: HeaderMap, body: Bytes) -> Response {
    ctl_device(&sh, &q, &h, &body, true)
}

pub(crate) async fn ctl_device_deny(State(sh): St, q: Q, h: HeaderMap, body: Bytes) -> Response {
    ctl_device(&sh, &q, &h, &body, false)
}

fn ctl_device(sh: &Shared, q: &Q, h: &HeaderMap, body: &Bytes, approve: bool) -> Response {
    let from_body = if body.is_empty() {
        None
    } else {
        fields(h, body).and_then(|f| f.get("user_code").cloned())
    };
    let code = from_body
        .or_else(|| q.get("user_code").cloned())
        .filter(|c| !c.trim().is_empty());
    match device_set(sh, code.as_deref().map(str::trim), approve) {
        Some(c) => Json(json!({"ok": true, "user_code": c})).into_response(),
        None => (
            StatusCode::NOT_FOUND,
            Json(json!({"ok": false, "error": "no pending device authorization"})),
        )
            .into_response(),
    }
}

impl FakeFeishu {
    /// The user confirms a device authorization (as [`super::World::oauth_user_open_id`]).
    pub fn device_approve(&self, user_code: &str) -> bool {
        device_set(&self.shared, Some(user_code), true).is_some()
    }

    /// The user declines a device authorization.
    pub fn device_deny(&self, user_code: &str) -> bool {
        device_set(&self.shared, Some(user_code), false).is_some()
    }

    /// `interval` handed out by device authorizations (seconds; 0 disables `slow_down`).
    pub fn set_device_interval(&self, secs: u64) {
        self.shared.rt().device_interval = secs;
    }

    /// Lifetime of device codes.
    pub fn set_device_ttl(&self, ttl: std::time::Duration) {
        self.shared.rt().device_ttl = ttl;
    }

    /// `refresh_token_expires_in` of newly issued refresh tokens.
    pub fn set_refresh_ttl(&self, ttl: std::time::Duration) {
        self.shared.rt().refresh_ttl = ttl;
    }

    /// Every refresh token issued so far is now expired (offline for too long).
    pub fn expire_refresh_tokens(&self) {
        let now = Instant::now();
        for r in self.shared.rt().refresh_tokens.values_mut() {
            r.expires_at = now;
        }
    }

    /// The user withdrew the grant: refresh tokens are revoked, access tokens invalid.
    pub fn revoke_user_grant(&self) {
        let mut rt = self.shared.rt();
        for r in rt.refresh_tokens.values_mut() {
            r.revoked = true;
        }
        for t in rt.tokens.values_mut() {
            if matches!(t.kind, TokenKind::User { .. }) {
                t.revoked = true;
            }
        }
    }

    /// Per-user send / reply limit per second (default 5).
    pub fn set_user_send_qps(&self, n: usize) {
        self.shared.rt().user_send_qps = n;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn grant_intersects_and_defaults() {
        let enabled: Vec<String> = ["a", "b", "offline_access"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        assert_eq!(grant(Some("b x b"), &enabled), ["b"]);
        assert_eq!(grant(None, &enabled), enabled);
        assert_eq!(grant(Some("  "), &enabled), enabled);
    }

    #[test]
    fn form_decoding() {
        assert_eq!(dec("a+b%20c%3A"), "a b c:");
        assert_eq!(dec("100%"), "100%");
    }
}
