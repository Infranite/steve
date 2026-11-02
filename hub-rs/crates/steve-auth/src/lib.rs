//! Login for the Steve Hub (replaces `hub/src/auth.ts`).
//!
//! OIDC (authorization code + PKCE) against the lab's Authentik, where the owner group is
//! enforced. Browsers only hold an opaque cookie; the DB keeps its sha256. The first subject to
//! log in becomes the workspace owner; later unknown subjects are refused until invited.
//!
//! Wiring in the Hub:
//! ```ignore
//! let auth = Auth::new(store, AuthConfig::from_env()?)?;
//! let api = Router::new() /* … */
//!     .layer(axum::middleware::from_fn_with_state(auth.clone(), steve_auth::require_user::<MyStore>));
//! let app = Router::new().nest("/auth", auth.router()).nest("/api", api);
//! axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>()).await?;
//! ```
//! Handlers then take [`CurrentUser`] (the router state must give `Auth<St>: FromRef<_>`, e.g. a
//! `#[derive(FromRef)]` field). `/auth/dev` needs `ConnectInfo<SocketAddr>` (loopback only).

mod config;
mod cookies;
mod oidc;

use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

use axum::{
    Json, Router,
    extract::{ConnectInfo, FromRef, FromRequestParts, Request, State},
    http::{HeaderMap, StatusCode, header, request::Parts},
    middleware::Next,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use openidconnect::CsrfToken;
use serde_json::json;

pub use config::{AuthConfig, OidcConfig};

/// Cookie holding the browser's session token.
pub const SESSION: &str = "steve_session";
/// Cookie holding the key of a login in progress.
pub const LOGIN: &str = "steve_login";
/// Session lifetime: 30 days.
pub const TTL: Duration = Duration::from_secs(30 * 24 * 3600);
/// How long a started login may take before the callback is refused.
const LOGIN_TTL: Duration = Duration::from_secs(600);

/// The user and auth-session storage this crate needs; `steve_store::Store` provides all of it
/// (`users()`, `auth_sessions()`, `audit()`). Calls are synchronous like the store.
pub trait UserStore: Send + Sync + 'static {
    /// The full `users` row, handed to request handlers.
    type User: Clone + Send + Sync + 'static;
    type Error: std::fmt::Display;

    /// `users.bySubject(sub)?.id`
    fn user_id_by_subject(&self, subject: &str) -> Result<Option<String>, Self::Error>;
    /// `users.count()`
    fn user_count(&self) -> Result<i64, Self::Error>;
    /// `users.create(sub, name, email, "owner")` then `audit(id, "user.create", id, {role: "owner"})`.
    fn create_owner(
        &self,
        subject: &str,
        name: &str,
        email: Option<&str>,
    ) -> Result<String, Self::Error>;
    /// `authSessions.create(userId, token, ttlMs)` (stores `sha256hex(token)`).
    fn create_session(&self, user_id: &str, token: &str, ttl_ms: i64) -> Result<(), Self::Error>;
    /// `authSessions.user(token)`: the user behind an unexpired token.
    fn session_user(&self, token: &str) -> Result<Option<Self::User>, Self::Error>;
    /// `authSessions.drop(token)`
    fn drop_session(&self, token: &str) -> Result<(), Self::Error>;
}

#[derive(Debug, thiserror::Error)]
pub enum AuthError {
    #[error("http client: {0}")]
    Http(#[from] openidconnect::reqwest::Error),
}

/// A login between `/auth/login` and `/auth/callback`; kept in memory only, like the TS map.
struct PendingLogin {
    verifier: String,
    state: String,
    nonce: String,
    back: String,
    origin: String,
    at: Instant,
}

struct Inner<St> {
    store: St,
    config: AuthConfig,
    oidc: oidc::Oidc,
    logins: Mutex<HashMap<String, PendingLogin>>,
}

/// Login state shared by the routes and the extractor. Cheap to clone.
pub struct Auth<St> {
    inner: Arc<Inner<St>>,
}

impl<St> Clone for Auth<St> {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
        }
    }
}

/// `c.text(msg, code)`: Hono labels plain text `text/plain; charset=UTF-8` (axum's default is
/// lowercase `utf-8`); the contract test compares the header byte for byte.
fn text(code: StatusCode, msg: &'static str) -> Response {
    (
        code,
        [(header::CONTENT_TYPE, "text/plain; charset=UTF-8")],
        msg,
    )
        .into_response()
}

fn internal_error(what: &str, e: impl std::fmt::Display) -> Response {
    tracing::error!("{what}: {e}");
    text(StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error")
}

fn redirect(to: &str) -> Response {
    (StatusCode::FOUND, [(header::LOCATION, to.to_owned())]).into_response()
}

fn query_param(parts: &Parts, key: &str) -> Option<String> {
    url::form_urlencoded::parse(parts.uri.query().unwrap_or("").as_bytes())
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.into_owned())
}

/// Same peer test as the TS regex `^(127\.|::1|::ffff:127\.)`.
fn is_loopback(addr: &SocketAddr) -> bool {
    match addr {
        SocketAddr::V4(a) => a.ip().is_loopback(),
        SocketAddr::V6(a) => {
            a.ip().is_loopback() || a.ip().to_ipv4_mapped().is_some_and(|v4| v4.is_loopback())
        }
    }
}

impl<St: UserStore> Auth<St> {
    pub fn new(store: St, config: AuthConfig) -> Result<Self, AuthError> {
        let oidc = oidc::Oidc::new(config.oidc.clone())?;
        Ok(Self {
            inner: Arc::new(Inner {
                store,
                config,
                oidc,
                logins: Mutex::default(),
            }),
        })
    }

    pub fn store(&self) -> &St {
        &self.inner.store
    }

    pub fn config(&self) -> &AuthConfig {
        &self.inner.config
    }

    /// `/login`, `/callback`, `/logout`, `/dev`; nest it under `/auth`. No session required.
    pub fn router(&self) -> Router {
        Router::new()
            .route("/login", get(login::<St>))
            .route("/callback", get(callback::<St>))
            .route("/logout", post(logout::<St>))
            .route("/dev", get(dev::<St>))
            .with_state(self.clone())
    }

    /// The user behind the request's `steve_session` cookie.
    pub fn user(&self, headers: &HeaderMap) -> Result<Option<St::User>, St::Error> {
        match cookies::get(headers, SESSION) {
            Some(t) => self.inner.store.session_user(&t),
            None => Ok(None),
        }
    }

    /// `{"error":"unauthenticated","login": …}` with status 401.
    pub fn unauthenticated(&self) -> Response {
        let c = &self.inner.config;
        let login = if c.oidc.issuer.is_some() {
            Some("/auth/login")
        } else if c.dev_login {
            Some("/auth/dev")
        } else {
            None
        };
        (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "unauthenticated", "login": login})),
        )
            .into_response()
    }

    /// The configured origin matching the Host the browser used; anything else falls back to
    /// the primary origin.
    fn origin_of(&self, headers: &HeaderMap) -> String {
        let c = &self.inner.config;
        let host = headers
            .get("x-forwarded-host")
            .or_else(|| headers.get(header::HOST))
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .split(',')
            .next()
            .unwrap_or("")
            .trim()
            .to_lowercase();
        c.public_urls
            .iter()
            .find(|u| url_host(u).is_some_and(|h| h == host))
            .or(c.public_urls.first())
            .unwrap_or(&c.public_url)
            .clone()
    }

    /// `login()` in auth.ts: known subject → its id; no users yet → a new owner; else `None`.
    fn login_subject(
        &self,
        sub: &str,
        name: &str,
        email: Option<&str>,
    ) -> Result<Option<String>, St::Error> {
        let s = &self.inner.store;
        if let Some(id) = s.user_id_by_subject(sub)? {
            return Ok(Some(id));
        }
        if s.user_count()? > 0 {
            return Ok(None);
        }
        s.create_owner(sub, name, email).map(Some)
    }

    /// New session token in the store and its cookie.
    fn start_session(&self, user_id: &str) -> Result<String, St::Error> {
        let token = CsrfToken::new_random_len(32).into_secret();
        self.inner
            .store
            .create_session(user_id, &token, TTL.as_millis() as i64)?;
        Ok(cookies::set(
            SESSION,
            &token,
            "/",
            TTL.as_secs(),
            self.inner.config.secure(),
        ))
    }
}

/// `new URL(u).host`: lowercase host plus the port when it is not the scheme's default.
fn url_host(u: &str) -> Option<String> {
    let url = url::Url::parse(u).ok()?;
    let host = url.host_str()?.to_lowercase();
    Some(match url.port() {
        Some(p) => format!("{host}:{p}"),
        None => host,
    })
}

async fn login<St: UserStore>(State(a): State<Auth<St>>, req: Request) -> Response {
    let (parts, _) = req.into_parts();
    let provider = match a.inner.oidc.provider().await {
        Ok(p) => p,
        Err(e) => return internal_error("oidc login", e),
    };
    let origin = a.origin_of(&parts.headers);
    let start = match a.inner.oidc.start(&provider, &origin) {
        Ok(s) => s,
        Err(e) => return internal_error("oidc login", e),
    };
    let browser = CsrfToken::new_random_len(16).into_secret();
    {
        let mut logins = a.inner.logins.lock().unwrap_or_else(|e| e.into_inner());
        // Abandoned logins would otherwise pile up (the TS map never forgets them).
        logins.retain(|_, l| l.at.elapsed() <= LOGIN_TTL);
        logins.insert(
            browser.clone(),
            PendingLogin {
                verifier: start.verifier,
                state: start.state,
                nonce: start.nonce,
                back: query_param(&parts, "back").unwrap_or_else(|| "/".into()),
                origin,
                at: Instant::now(),
            },
        );
    }
    let cookie = cookies::set(
        LOGIN,
        &browser,
        "/auth",
        LOGIN_TTL.as_secs(),
        a.inner.config.secure(),
    );
    let mut res = redirect(&start.url);
    cookies::append(&mut res, cookie);
    res
}

async fn callback<St: UserStore>(State(a): State<Auth<St>>, req: Request) -> Response {
    let (parts, _) = req.into_parts();
    let pending = cookies::get(&parts.headers, LOGIN).and_then(|b| {
        let mut logins = a.inner.logins.lock().unwrap_or_else(|e| e.into_inner());
        match logins.get(&b) {
            Some(l) if l.at.elapsed() <= LOGIN_TTL => logins.remove(&b),
            _ => None,
        }
    });
    let Some(l) = pending else {
        return text(StatusCode::BAD_REQUEST, "登录已过期，请重新登录");
    };
    let clear = cookies::clear(LOGIN, "/auth");
    let mut res = match finish(&a, &parts, &l).await {
        Ok(Some((session_cookie, back))) => {
            let mut r = redirect(&back);
            cookies::append(&mut r, clear);
            cookies::append(&mut r, session_cookie);
            return r;
        }
        Ok(None) => text(StatusCode::FORBIDDEN, "这个账号还没有被邀请使用 Steve"),
        Err(e) => {
            tracing::error!("oidc callback {e}");
            text(StatusCode::BAD_REQUEST, "登录失败")
        }
    };
    cookies::append(&mut res, clear);
    res
}

/// Code exchange and owner check. `Ok(None)`: the subject is not invited.
async fn finish<St: UserStore>(
    a: &Auth<St>,
    parts: &Parts,
    l: &PendingLogin,
) -> Result<Option<(String, String)>, String> {
    let provider = a.inner.oidc.provider().await.map_err(|e| e.to_string())?;
    let q = |k| query_param(parts, k);
    let claims = a
        .inner
        .oidc
        .finish(
            &provider,
            &l.origin,
            oidc::Callback {
                code: q("code"),
                state: q("state"),
                error: q("error"),
            },
            &l.state,
            &l.verifier,
            &l.nonce,
        )
        .await?;
    let Some(id) = a
        .login_subject(&claims.sub, &claims.name, claims.email.as_deref())
        .map_err(|e| e.to_string())?
    else {
        return Ok(None);
    };
    let cookie = a.start_session(&id).map_err(|e| e.to_string())?;
    // [TS bug kept] `//host` also starts with "/" (open redirect); fix in both Hubs together.
    let back = if l.back.starts_with('/') {
        l.back.clone()
    } else {
        "/".into()
    };
    Ok(Some((cookie, back)))
}

async fn logout<St: UserStore>(State(a): State<Auth<St>>, headers: HeaderMap) -> Response {
    if let Some(t) = cookies::get(&headers, SESSION)
        && let Err(e) = a.inner.store.drop_session(&t)
    {
        return internal_error("logout", e);
    }
    let mut res = Json(json!({"ok": true})).into_response();
    cookies::append(&mut res, cookies::clear(SESSION, "/"));
    res
}

/// Development: loopback-only one-click login, disabled unless `STEVE_DEV_LOGIN=1`.
async fn dev<St: UserStore>(State(a): State<Auth<St>>, req: Request) -> Response {
    let (mut parts, _) = req.into_parts();
    let peer = ConnectInfo::<SocketAddr>::from_request_parts(&mut parts, &())
        .await
        .ok()
        .map(|c| c.0);
    if !a.inner.config.dev_login || !peer.is_some_and(|p| is_loopback(&p)) {
        return text(StatusCode::NOT_FOUND, "not found");
    }
    let s = &a.inner.store;
    let id = match a.login_subject("dev:local", "本机", None) {
        Ok(Some(id)) => Some(id),
        Ok(None) => match s.user_id_by_subject("dev:local") {
            Ok(id) => id,
            Err(e) => return internal_error("dev login", e),
        },
        Err(e) => return internal_error("dev login", e),
    };
    let Some(id) = id else {
        return text(StatusCode::FORBIDDEN, "已有所有者，本机登录不可用");
    };
    match a.start_session(&id) {
        Ok(cookie) => {
            let mut res = redirect("/");
            cookies::append(&mut res, cookie);
            res
        }
        Err(e) => internal_error("dev login", e),
    }
}

/// The logged-in user. Put [`require_user`] in front of the routes, or let the extractor look
/// the cookie up itself (it needs `Auth<St>: FromRef<S>`); either way a missing or expired
/// session answers the TS 401 JSON.
pub struct CurrentUser<St: UserStore>(pub St::User);

impl<St: UserStore> Clone for CurrentUser<St> {
    fn clone(&self) -> Self {
        Self(self.0.clone())
    }
}

impl<S, St> FromRequestParts<S> for CurrentUser<St>
where
    S: Send + Sync,
    St: UserStore,
    Auth<St>: FromRef<S>,
{
    type Rejection = Response;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Response> {
        if let Some(u) = parts.extensions.get::<CurrentUser<St>>() {
            return Ok(u.clone());
        }
        let auth = Auth::<St>::from_ref(state);
        match auth.user(&parts.headers) {
            Ok(Some(u)) => Ok(CurrentUser(u)),
            Ok(None) => Err(auth.unauthenticated()),
            Err(e) => Err(internal_error("requireUser", e)),
        }
    }
}

/// `requireUser`: answers 401 without a valid session, else stores [`CurrentUser`] in the
/// request extensions. Use with `middleware::from_fn_with_state(auth, require_user::<St>)`.
pub async fn require_user<St: UserStore>(
    State(auth): State<Auth<St>>,
    mut req: Request,
    next: Next,
) -> Response {
    match auth.user(req.headers()) {
        Ok(Some(u)) => {
            req.extensions_mut().insert(CurrentUser::<St>(u));
            next.run(req).await
        }
        Ok(None) => auth.unauthenticated(),
        Err(e) => internal_error("requireUser", e),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_peers() {
        for (a, ok) in [
            ("127.0.0.1:1", true),
            ("127.9.9.9:1", true),
            ("[::1]:1", true),
            ("[::ffff:127.0.0.1]:1", true),
            ("10.0.0.1:1", false),
            ("[::ffff:10.0.0.1]:1", false),
            ("[fe80::1]:1", false),
        ] {
            assert_eq!(is_loopback(&a.parse().unwrap()), ok, "{a}");
        }
    }

    #[test]
    fn url_host_like_js() {
        assert_eq!(
            url_host("https://Steve.Example.test").as_deref(),
            Some("steve.example.test")
        );
        assert_eq!(
            url_host("http://127.0.0.1:8440").as_deref(),
            Some("127.0.0.1:8440")
        );
        assert_eq!(url_host("https://x.dev:443").as_deref(), Some("x.dev"));
        assert_eq!(url_host("nope"), None);
    }
}
