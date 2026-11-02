//! Shared plumbing of the HTTP fakes (Gmail, Graph): serving an axum router
//! on a random port, bearer tokens and a fake OAuth token endpoint.

use std::collections::{BTreeMap, BTreeSet};
use std::net::SocketAddr;
use std::sync::Mutex;

use axum::Router;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Redirect, Response};
use serde_json::{Value, json};
use tokio::net::TcpListener;

use super::{FakeError, Lifeline, lock};

/// Binds 127.0.0.1:0 and serves `router` until the returned lifeline drops.
pub(crate) async fn serve(router: Router) -> Result<(SocketAddr, Lifeline), FakeError> {
    let listener = TcpListener::bind("127.0.0.1:0").await?;
    let addr = listener.local_addr()?;
    let life = Lifeline::new();
    let task = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, router).await {
            tracing::debug!("fake http server stopped: {e}");
        }
    });
    life.set_task(task);
    Ok((addr, life))
}

/// One request to the token endpoint, as the fake received it.
#[derive(Debug, Clone, PartialEq)]
pub struct TokenRequest {
    pub grant_type: String,
    /// All form fields (`client_id`, `code`, `redirect_uri`, …).
    pub form: BTreeMap<String, String>,
}

#[derive(Debug, Default)]
struct OAuthState {
    access: BTreeSet<String>,
    refresh: BTreeSet<String>,
    codes: BTreeSet<String>,
    n: u64,
    requests: Vec<TokenRequest>,
}

/// Bearer tokens and the OAuth endpoints shared by both HTTP fakes.
#[derive(Debug)]
pub(crate) struct OAuth {
    state: Mutex<OAuthState>,
    /// `"google"` or `"microsoft"`: decides the refresh-token rotation
    /// (Microsoft rotates on every refresh, Google keeps the old one).
    rotate_refresh: bool,
}

/// The access token every HTTP fake accepts from the start.
pub const INITIAL_ACCESS_TOKEN: &str = "fake-access-token";
/// The refresh token every HTTP fake accepts from the start.
pub const INITIAL_REFRESH_TOKEN: &str = "fake-refresh-token";

impl OAuth {
    pub(crate) fn new(rotate_refresh: bool) -> Self {
        let mut st = OAuthState::default();
        st.access.insert(INITIAL_ACCESS_TOKEN.into());
        st.refresh.insert(INITIAL_REFRESH_TOKEN.into());
        Self {
            state: Mutex::new(st),
            rotate_refresh,
        }
    }

    pub(crate) fn authorized(&self, headers: &HeaderMap) -> bool {
        let Some(v) = headers
            .get(header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
        else {
            return false;
        };
        let Some(token) = v
            .strip_prefix("Bearer ")
            .or_else(|| v.strip_prefix("bearer "))
        else {
            return false;
        };
        lock(&self.state).access.contains(token.trim())
    }

    /// Invalidates every access token (refresh tokens stay valid).
    pub(crate) fn expire_access(&self) {
        lock(&self.state).access.clear();
    }

    /// Invalidates refresh tokens too (the user revoked access).
    pub(crate) fn revoke_all(&self) {
        let mut st = lock(&self.state);
        st.access.clear();
        st.refresh.clear();
    }

    pub(crate) fn requests(&self) -> Vec<TokenRequest> {
        lock(&self.state).requests.clone()
    }

    /// `GET …/authorize`: the consent screen, already clicked — redirects to
    /// `redirect_uri` with a fresh `code` and the caller's `state`.
    pub(crate) fn authorize(&self, q: &BTreeMap<String, String>) -> Response {
        let Some(redirect) = q.get("redirect_uri") else {
            return (StatusCode::BAD_REQUEST, "missing redirect_uri").into_response();
        };
        let code = {
            let mut st = lock(&self.state);
            st.n += 1;
            let c = format!("fake-code-{}", st.n);
            st.codes.insert(c.clone());
            c
        };
        let sep = if redirect.contains('?') { '&' } else { '?' };
        let mut url = format!("{redirect}{sep}code={code}");
        if let Some(state) = q.get("state") {
            url.push_str("&state=");
            url.push_str(&urlencode(state));
        }
        Redirect::to(&url).into_response()
    }

    /// `POST …/token` for `authorization_code` and `refresh_token` grants.
    pub(crate) fn token(&self, form: BTreeMap<String, String>) -> Response {
        let grant = form.get("grant_type").cloned().unwrap_or_default();
        let mut st = lock(&self.state);
        st.requests.push(TokenRequest {
            grant_type: grant.clone(),
            form: form.clone(),
        });
        let valid = match grant.as_str() {
            "authorization_code" => form.get("code").is_some_and(|c| st.codes.remove(c)),
            "refresh_token" => form
                .get("refresh_token")
                .is_some_and(|r| st.refresh.contains(r)),
            _ => {
                return (
                    StatusCode::BAD_REQUEST,
                    axum::Json(json!({"error": "unsupported_grant_type"})),
                )
                    .into_response();
            }
        };
        if !valid {
            return (
                StatusCode::BAD_REQUEST,
                axum::Json(json!({
                    "error": "invalid_grant",
                    "error_description": "Token has been expired or revoked."
                })),
            )
                .into_response();
        }
        st.n += 1;
        let access = format!("fake-access-{}", st.n);
        st.access.insert(access.clone());
        let mut body = json!({
            "access_token": access,
            "expires_in": 3599,
            "token_type": "Bearer",
            "scope": form.get("scope").cloned().unwrap_or_default(),
        });
        let new_refresh = grant == "authorization_code" || self.rotate_refresh;
        if new_refresh {
            if grant == "refresh_token"
                && let Some(old) = form.get("refresh_token")
            {
                st.refresh.remove(old);
            }
            let r = format!("fake-refresh-{}", st.n);
            st.refresh.insert(r.clone());
            body["refresh_token"] = Value::String(r);
        }
        axum::Json(body).into_response()
    }
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                (b as char).to_string()
            }
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// Query string with repeated keys kept (`labelIds=A&labelIds=B`).
pub(crate) type Query = Vec<(String, String)>;

pub(crate) fn q_get<'a>(q: &'a Query, key: &str) -> Option<&'a str> {
    q.iter().find(|(k, _)| k == key).map(|(_, v)| v.as_str())
}

pub(crate) fn q_all<'a>(q: &'a Query, key: &str) -> Vec<&'a str> {
    q.iter()
        .filter(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
        .collect()
}

/// Unix seconds → `2026-10-26T01:12:00Z`.
pub(crate) fn iso(ts: i64) -> String {
    mail_parser::DateTime::from_timestamp(ts).to_rfc3339()
}
