//! Signing clients in with an access token (A1), and knowing which client a request comes from and by
//! which route.
//!
//! - `GET /auth/hello`: who this Hub is and how one signs in; no login.
//! - `POST /auth/token {user, token, client:{kind,name,platform,version}}`: a user's name and one of
//!   their access tokens give a new client its credential (`<client id>.<secret>`), returned once.
//!   The client sends it as `Authorization: Bearer …` from then on (`api::lookup_user`).
//! - `steve-hub --access …`: issue, list and revoke tokens and clients on this machine, also when the
//!   Hub does not answer.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use axum::extract::{ConnectInfo, Request, State};
use axum::http::{HeaderMap, StatusCode};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use steve_store::access::{Client, NewClient, credential};
use steve_store::{Store, User};

use crate::api::AppState;

/// The API generation clients check before they sign in (`/auth/hello`).
pub const API_VERSION: u32 = 1;

/// The client a request comes from, and how it came (put next to `Ctx`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ClientRef {
    pub id: String,
    pub kind: String,
    pub name: String,
    /// `loopback`, `lan`, `tailnet`, `proxy` or `public`, with `+tls` when the request was encrypted.
    pub route: String,
}

impl ClientRef {
    pub fn of(c: &Client, route: &str) -> Self {
        Self {
            id: c.id.clone(),
            kind: c.kind.clone(),
            name: c.name.clone(),
            route: route.into(),
        }
    }
}

fn private(ip: &IpAddr) -> bool {
    match ip {
        IpAddr::V4(v) => v.is_private() || v.is_link_local(),
        IpAddr::V6(v) => {
            (v.segments()[0] & 0xfe00) == 0xfc00 || (v.segments()[0] & 0xffc0) == 0xfe80
        }
    }
}

/// Tailscale's (and tailcat's) address range, 100.64.0.0/10.
fn tailnet(ip: &IpAddr) -> bool {
    matches!(ip, IpAddr::V4(v) if v.octets()[0] == 100 && (v.octets()[1] & 0xc0) == 64)
}

/// How a request reached the Hub, and the address it came from. A forwarding header counts only
/// from a proxy on this machine or the local network (anyone else could write one).
pub fn route_of(peer: Option<IpAddr>, headers: &HeaderMap) -> (String, Option<String>) {
    let h = |k: &str| headers.get(k).and_then(|v| v.to_str().ok());
    let tls = h("x-forwarded-proto").is_some_and(|p| p.eq_ignore_ascii_case("https"));
    let trusted = peer.is_some_and(|p| {
        p.is_loopback()
            || private(&p)
            || matches!(p, IpAddr::V6(v) if v.to_ipv4_mapped().is_some_and(|m| m.is_loopback()))
    });
    let forwarded = h("x-forwarded-for")
        .and_then(|v| v.split(',').next())
        .or_else(|| h("x-real-ip"))
        .map(|v| v.trim().to_string());
    let (route, ip) = match (forwarded, peer) {
        (Some(f), Some(_)) if trusted => ("proxy", Some(f)),
        (_, Some(p)) => {
            let p = match p {
                IpAddr::V6(v) => v.to_ipv4_mapped().map(IpAddr::V4).unwrap_or(p),
                p => p,
            };
            let r = if p.is_loopback() {
                "loopback"
            } else if tailnet(&p) {
                "tailnet"
            } else if private(&p) {
                "lan"
            } else {
                "public"
            };
            (r, Some(p.to_string()))
        }
        (_, None) => ("unknown", None),
    };
    (
        if tls {
            format!("{route}+tls")
        } else {
            route.to_string()
        },
        ip,
    )
}

/// How often a client's “last seen” is written, at most (unless its route changed).
const SEEN_EVERY: Duration = Duration::from_secs(60);
static SEEN: LazyLock<Mutex<HashMap<String, (Instant, String)>>> = LazyLock::new(Default::default);

/// Notes that client `c` came now by `route`, at most every [`SEEN_EVERY`] (off the request).
pub fn seen(store: &Store, c: &Client, route: &str, ip: Option<String>) {
    {
        let mut m = SEEN.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((at, r)) = m.get(&c.id)
            && at.elapsed() < SEEN_EVERY
            && r == route
        {
            return;
        }
        m.insert(c.id.clone(), (Instant::now(), route.to_string()));
    }
    let (s, id, route) = (store.clone(), c.id.clone(), route.to_string());
    tokio::task::spawn_blocking(move || {
        if let Err(e) = s.access().client_seen(&id, &route, ip.as_deref()) {
            tracing::warn!("client seen: {e}");
        }
    });
}

/// Failed sign-ins per address: after [`TRIES`] within [`TRIES_WINDOW`], 429 until it passes.
const TRIES: u32 = 10;
const TRIES_WINDOW: Duration = Duration::from_secs(600);
static FAILED: LazyLock<Mutex<HashMap<String, (Instant, u32)>>> = LazyLock::new(Default::default);

fn too_many(ip: &str) -> bool {
    let m = FAILED.lock().unwrap_or_else(|e| e.into_inner());
    m.get(ip)
        .is_some_and(|(at, n)| at.elapsed() < TRIES_WINDOW && *n >= TRIES)
}

fn failed(ip: &str) {
    let mut m = FAILED.lock().unwrap_or_else(|e| e.into_inner());
    let e = m.entry(ip.to_string()).or_insert((Instant::now(), 0));
    if e.0.elapsed() >= TRIES_WINDOW {
        *e = (Instant::now(), 0);
    }
    e.1 += 1;
}

/// The client kinds a sign-in may name; anything else is `other`.
const KINDS: [&str; 7] = ["web", "mac", "ios", "ipad", "android", "cli", "other"];

#[derive(Deserialize)]
struct SignIn {
    user: String,
    token: String,
    #[serde(default)]
    client: ClientInfo,
    /// A web page served by this Hub: the credential is also set as the [`COOKIE`] (HttpOnly), so the
    /// page's own requests and event stream carry it without script access to it.
    #[serde(default)]
    cookie: bool,
}

/// The cookie a same-origin web page keeps its client credential in.
pub const COOKIE: &str = "steve_client";
/// How long that cookie lasts: a year (the client is revoked, not the cookie timed out).
const COOKIE_AGE: i64 = 365 * 24 * 3600;

fn client_cookie(value: &str, max_age: i64, secure: bool, cross_site: bool) -> String {
    format!(
        "{COOKIE}={value}; Max-Age={max_age}; Path=/; HttpOnly{}; SameSite={}",
        if secure { "; Secure" } else { "" },
        if cross_site { "None" } else { "Lax" }
    )
}

/// The client credential of a request: `Authorization: Bearer`, else the [`COOKIE`].
pub fn credential_of(headers: &HeaderMap) -> Option<String> {
    bearer(headers).or_else(|| crate::api::cookie(headers, COOKIE).filter(|c| !c.is_empty()))
}

#[derive(Deserialize, Default)]
struct ClientInfo {
    #[serde(default)]
    kind: String,
    #[serde(default)]
    name: String,
    #[serde(default)]
    platform: String,
    #[serde(default)]
    version: String,
}

fn cut(s: &str, n: usize) -> String {
    s.trim().chars().take(n).collect()
}

fn reply(status: StatusCode, v: Value) -> axum::response::Response {
    use axum::response::IntoResponse;
    (status, Json(v)).into_response()
}

async fn hello(State(st): State<AppState>) -> axum::response::Response {
    reply(
        StatusCode::OK,
        json!({
            "name": "steve",
            "version": st.cfg.version,
            "api": API_VERSION,
            "login": {
                "token": st.modules.is_some(),
                "oidc": st.cfg.oidc.issuer.is_some(),
            },
        }),
    )
}

async fn sign_in(State(st): State<AppState>, req: Request) -> axum::response::Response {
    let peer = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip());
    let (route, ip) = route_of(peer, req.headers());
    let origin = req
        .headers()
        .get("origin")
        .and_then(|v| v.to_str().ok())
        .map(|o| o.trim_end_matches('/').to_string());
    let origin_seen = origin.clone();
    let host = req
        .headers()
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    /* this machine is not slowed down: what the limit guards against comes from elsewhere */
    let key = ip
        .clone()
        .filter(|_| !route.starts_with("loopback"))
        .unwrap_or_default();
    if !key.is_empty() && too_many(&key) {
        return reply(
            StatusCode::TOO_MANY_REQUESTS,
            json!({"error": steve_i18n::t!("hub.access.too_many")}),
        );
    }
    let Some(store) = st.modules.as_ref().map(|m| m.store.clone()) else {
        return reply(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"error": "unavailable"}),
        );
    };
    let Ok(bytes) = axum::body::to_bytes(req.into_body(), 64 * 1024).await else {
        return reply(StatusCode::BAD_REQUEST, json!({"error": "body"}));
    };
    let Ok(b) = serde_json::from_slice::<SignIn>(&bytes) else {
        return reply(StatusCode::BAD_REQUEST, json!({"error": "body"}));
    };
    let kind = if KINDS.contains(&b.client.kind.as_str()) {
        b.client.kind.clone()
    } else {
        "other".into()
    };
    let name = match cut(&b.client.name, 80) {
        n if n.is_empty() => kind.clone(),
        n => n,
    };
    let (platform, version) = (cut(&b.client.platform, 80), cut(&b.client.version, 40));
    let s2 = store.clone();
    let done = tokio::task::spawn_blocking(move || {
        s2.access().sign_in(
            &b.user,
            &b.token,
            &NewClient {
                kind: &kind,
                name: &name,
                platform: &platform,
                version: &version,
                origin: origin.as_deref(),
            },
        )
    })
    .await;
    match done {
        Ok(Ok(Some((u, c, secret)))) => {
            seen(&store, &c, &route, ip);
            store
                .audit(
                    &u.id,
                    "client.sign_in",
                    &c.id,
                    &json!({"kind": c.kind, "name": c.name, "route": route}),
                    steve_store::WS,
                )
                .ok();
            let cred = credential(&c.id, &secret);
            let mut res = reply(
                StatusCode::OK,
                json!({
                    "client_id": c.id,
                    "credential": cred,
                    "user": {"id": u.id, "name": u.name},
                    "hub": {"version": st.cfg.version, "api": API_VERSION},
                }),
            );
            if b.cookie {
                let secure = route.ends_with("+tls") || st.cfg.public_url.starts_with("https://");
                /* a page on another host (not only another port) is another site: its requests carry
                 * the cookie only as SameSite=None, which browsers take over TLS only */
                let cross_site = origin_seen.as_deref().is_some_and(|o| {
                    host_of(o)
                        != host
                            .as_deref()
                            .and_then(|h| host_of(&format!("http://{h}")))
                });
                if let Ok(v) =
                    client_cookie(&cred, COOKIE_AGE, secure, cross_site && secure).parse()
                {
                    res.headers_mut().append(axum::http::header::SET_COOKIE, v);
                }
            }
            if let Some(o) = &origin_seen {
                forget_origin(o);
            }
            res
        }
        Ok(Ok(None)) => {
            if !key.is_empty() {
                failed(&key);
            }
            reply(
                StatusCode::UNAUTHORIZED,
                json!({"error": steve_i18n::t!("hub.access.wrong")}),
            )
        }
        Ok(Err(e)) => {
            tracing::error!("sign in: {e}");
            reply(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({"error": "internal"}),
            )
        }
        Err(e) => {
            tracing::error!("sign in: {e}");
            reply(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({"error": "internal"}),
            )
        }
    }
}

/// Signs this client out: it is revoked, and its cookie cleared.
async fn sign_out(State(st): State<AppState>, headers: HeaderMap) -> axum::response::Response {
    if let (Some(cred), Some(m)) = (credential_of(&headers), st.modules.as_ref()) {
        let store = m.store.clone();
        let _ = tokio::task::spawn_blocking(move || {
            if let Ok(Some((_, c))) = store.access().client_user(&cred) {
                store.access().client_revoke(&c.id).ok();
            }
        })
        .await;
    }
    let mut res = reply(StatusCode::OK, json!({"ok": true}));
    if let Ok(v) = client_cookie("", 0, false, false).parse() {
        res.headers_mut().append(axum::http::header::SET_COOKIE, v);
    }
    res
}

/// `/hello`, `/token` and `/client/logout`, nested under `/auth` with the login routes; no session
/// required.
pub fn auth_routes(st: AppState) -> Router {
    Router::new()
        .route("/hello", get(hello))
        .route("/token", post(sign_in))
        .route("/client/logout", post(sign_out))
        .with_state(st)
}

/// The user and client behind a bearer credential, and the route the request came by.
pub fn bearer(headers: &HeaderMap) -> Option<String> {
    headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(|v| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// `steve-hub --access …` (see the module docs); the process exit code.
pub fn main(args: &[String]) -> i32 {
    match run(args) {
        Ok(()) => 0,
        Err(e) => {
            eprintln!("{e:#}");
            2
        }
    }
}

const USAGE: &str =
    "usage: steve-hub --access token create [--user NAME] [--name LABEL] [--days N] [--json]
       steve-hub --access token list [--json]
       steve-hub --access token revoke ID
       steve-hub --access client list [--json]
       steve-hub --access client revoke ID";

fn flag<'a>(args: &'a [String], name: &str) -> Option<&'a str> {
    args.iter()
        .position(|a| a == name)
        .and_then(|i| args.get(i + 1))
        .map(String::as_str)
}

/// The user a token is for: `--user`, else the owner. On a Hub nobody has signed in to yet, `--user`
/// (default: this account's name) becomes its owner.
fn user_for(store: &Store, name: Option<&str>) -> anyhow::Result<User> {
    if store.users().count()? == 0 {
        let name = name
            .map(str::to_string)
            .or_else(|| std::env::var("USER").ok())
            .unwrap_or_else(|| "owner".into());
        let id = store
            .users()
            .create(&format!("local:{name}"), &name, None, "owner")?;
        store.audit(
            &id,
            "user.create",
            &id,
            &json!({"role": "owner"}),
            steve_store::WS,
        )?;
        return Ok(store.users().get(&id)?.expect("just created"));
    }
    match name {
        Some(n) => store
            .access()
            .user_named(n)?
            .ok_or_else(|| anyhow::anyhow!("no single user is called {n}")),
        None => store
            .access()
            .owner()?
            .ok_or_else(|| anyhow::anyhow!("this Hub has no owner; pass --user")),
    }
}

fn run(args: &[String]) -> anyhow::Result<()> {
    let cfg = crate::config::Config::from_env()?;
    let store = Store::open(cfg.db_path(), steve_store::FnPublisher(|_| {}))?;
    store.ensure_workspace()?;
    let json = args.iter().any(|a| a == "--json");
    let words: Vec<&str> = args
        .iter()
        .map(String::as_str)
        .take_while(|a| !a.starts_with("--"))
        .collect();
    match words.as_slice() {
        ["token", "create"] => {
            let u = user_for(&store, flag(args, "--user"))?;
            let name = flag(args, "--name").unwrap_or("cli");
            let expires = flag(args, "--days")
                .map(|d| d.parse::<i64>())
                .transpose()?
                .map(|d| steve_store::now() + d * 86_400_000);
            let (t, token) = store.access().token_create(&u.id, name, expires)?;
            store.audit(
                &u.id,
                "token.create",
                &t.id,
                &json!({"name": name}),
                steve_store::WS,
            )?;
            if json {
                println!("{}", json!({"id": t.id, "user": u.name, "token": token}));
            } else {
                println!(
                    "user:  {}\ntoken: {token}\n(shown once; sign a client in with the user and this token)",
                    u.name
                );
            }
        }
        ["token", "list"] => {
            let ts = store.access().tokens(None)?;
            if json {
                println!("{}", serde_json::to_string(&ts)?);
            } else {
                for t in ts {
                    println!(
                        "{}  {}  {}{}",
                        t.id,
                        t.name,
                        t.user_id,
                        if t.revoked.is_some() {
                            "  (revoked)"
                        } else {
                            ""
                        }
                    );
                }
            }
        }
        ["token", "revoke", id] => {
            anyhow::ensure!(store.access().token_revoke(id)?, "no live token {id}");
            println!("revoked {id} and the clients signed in with it");
        }
        ["client", "list"] => {
            let cs = store.access().clients(None)?;
            if json {
                println!("{}", serde_json::to_string(&cs)?);
            } else {
                for c in cs {
                    println!(
                        "{}  {}  {}  {}{}",
                        c.id,
                        c.kind,
                        c.name,
                        c.last_route.as_deref().unwrap_or("-"),
                        if c.revoked.is_some() {
                            "  (revoked)"
                        } else {
                            ""
                        }
                    );
                }
            }
        }
        ["client", "revoke", id] => {
            anyhow::ensure!(store.access().client_revoke(id)?, "no live client {id}");
            println!("revoked {id}");
        }
        _ => anyhow::bail!("{USAGE}"),
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn h(pairs: &[(&'static str, &str)]) -> HeaderMap {
        let mut m = HeaderMap::new();
        for (k, v) in pairs {
            m.insert(*k, v.parse().unwrap());
        }
        m
    }

    #[test]
    fn routes_are_told_by_the_peer_and_trusted_forwarding() {
        let r =
            |peer: &str, hs: &[(&'static str, &str)]| route_of(Some(peer.parse().unwrap()), &h(hs));
        assert_eq!(r("127.0.0.1", &[]).0, "loopback");
        assert_eq!(r("::1", &[]).0, "loopback");
        assert_eq!(r("192.168.1.20", &[]).0, "lan");
        assert_eq!(r("10.1.2.3", &[]).0, "lan");
        assert_eq!(r("100.101.102.103", &[]).0, "tailnet");
        assert_eq!(r("100.200.1.1", &[]).0, "public", "outside 100.64/10");
        assert_eq!(r("8.8.8.8", &[]).0, "public");
        // a local proxy says who it forwards, and that it was https
        let (route, ip) = r(
            "127.0.0.1",
            &[
                ("x-forwarded-for", "203.0.113.9, 10.0.0.1"),
                ("x-forwarded-proto", "https"),
            ],
        );
        assert_eq!(
            (route.as_str(), ip.as_deref()),
            ("proxy+tls", Some("203.0.113.9"))
        );
        // a stranger's forwarding header is not believed
        let (route, ip) = r("8.8.8.8", &[("x-forwarded-for", "127.0.0.1")]);
        assert_eq!((route.as_str(), ip.as_deref()), ("public", Some("8.8.8.8")));
    }

    #[test]
    fn repeated_failures_from_one_address_are_slowed_down() {
        let ip = "203.0.113.77";
        for _ in 0..TRIES {
            assert!(!too_many(ip));
            failed(ip);
        }
        assert!(too_many(ip));
        assert!(!too_many("203.0.113.78"), "another address is not");
    }

    #[test]
    fn a_bearer_credential_is_read_from_the_header() {
        assert_eq!(
            bearer(&h(&[("authorization", "Bearer cli_1.abc")])).as_deref(),
            Some("cli_1.abc")
        );
        assert_eq!(bearer(&h(&[("authorization", "Basic x")])), None);
        assert_eq!(bearer(&h(&[])), None);
    }
}

/// The host of a URL or origin, without scheme or port (`http://10.0.0.5:8441` → `10.0.0.5`).
fn host_of(url: &str) -> Option<String> {
    reqwest::Url::parse(url).ok().and_then(|u| {
        u.host_str()
            .map(|h| h.trim_matches(['[', ']']).to_lowercase())
    })
}

/// The paths any web origin may call: what a page needs before it is signed in.
fn open_path(path: &str) -> bool {
    matches!(path, "/auth/hello" | "/auth/token" | "/auth/client/logout")
}

/// How long whether an origin may call is remembered.
const ORIGIN_TTL: Duration = Duration::from_secs(30);
static ORIGINS: LazyLock<Mutex<HashMap<String, (Instant, bool)>>> = LazyLock::new(Default::default);

/// Whether a page on `origin` may call this Hub with its credentials: one of `STEVE_CLIENT_ORIGINS`,
/// or the origin a live web client signed in from.
async fn origin_allowed(st: &AppState, origin: &str) -> bool {
    if st.cfg.client_origins.iter().any(|o| o == origin) {
        return true;
    }
    if let Some((at, ok)) = ORIGINS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .get(origin)
        .copied()
        && at.elapsed() < ORIGIN_TTL
    {
        return ok;
    }
    let Some(store) = st.modules.as_ref().map(|m| m.store.clone()) else {
        return false;
    };
    let o = origin.to_string();
    let ok =
        tokio::task::spawn_blocking(move || store.access().origin_signed_in(&o).unwrap_or(false))
            .await
            .unwrap_or(false);
    ORIGINS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .insert(origin.to_string(), (Instant::now(), ok));
    ok
}

/// Forgets what was decided about `origin` (a client signed in or out from it).
fn forget_origin(origin: &str) {
    ORIGINS
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(origin);
}

/// Cross-origin calls (A3): a web client served apart from the Hub calls it with its credentials.
/// Only the sign-in paths answer any origin; everything else answers the origins of
/// `STEVE_CLIENT_ORIGINS` and of signed-in web clients. A page of this Hub's own origin is not
/// cross-origin and passes untouched.
pub async fn cors(
    State(st): State<AppState>,
    req: Request,
    next: axum::middleware::Next,
) -> axum::response::Response {
    use axum::http::{HeaderValue, Method, header};
    use axum::response::IntoResponse;
    let Some(origin) = req
        .headers()
        .get(header::ORIGIN)
        .and_then(|v| v.to_str().ok())
        .map(|o| o.trim_end_matches('/').to_string())
    else {
        return next.run(req).await;
    };
    let host = req
        .headers()
        .get(header::HOST)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    /* the page came from this very address (scheme aside): no cross-origin call. Only the request's own
     * Host decides; the configured browser URL may be the web client's port (A3), another origin */
    let own = origin
        .split("://")
        .nth(1)
        .is_some_and(|h| h.eq_ignore_ascii_case(host));
    if own {
        return next.run(req).await;
    }
    let allowed = open_path(req.uri().path()) || origin_allowed(&st, &origin).await;
    let preflight = req.method() == Method::OPTIONS
        && req
            .headers()
            .contains_key(header::ACCESS_CONTROL_REQUEST_METHOD);
    let mark = |res: &mut axum::response::Response| {
        let h = res.headers_mut();
        if let Ok(v) = HeaderValue::from_str(&origin) {
            h.insert(header::ACCESS_CONTROL_ALLOW_ORIGIN, v);
        }
        h.insert(
            header::ACCESS_CONTROL_ALLOW_CREDENTIALS,
            HeaderValue::from_static("true"),
        );
        h.insert(
            header::ACCESS_CONTROL_EXPOSE_HEADERS,
            HeaderValue::from_static("x-steve-cursor, content-disposition"),
        );
        h.append(header::VARY, HeaderValue::from_static("origin"));
    };
    if preflight {
        if !allowed {
            return StatusCode::FORBIDDEN.into_response();
        }
        let mut res = StatusCode::NO_CONTENT.into_response();
        mark(&mut res);
        let h = res.headers_mut();
        h.insert(
            header::ACCESS_CONTROL_ALLOW_METHODS,
            HeaderValue::from_static("GET, POST, PUT, PATCH, DELETE, OPTIONS"),
        );
        h.insert(
            header::ACCESS_CONTROL_ALLOW_HEADERS,
            HeaderValue::from_static(
                "authorization, content-type, x-steve-ws, accept-language, last-event-id",
            ),
        );
        h.insert(
            header::ACCESS_CONTROL_MAX_AGE,
            HeaderValue::from_static("600"),
        );
        return res;
    }
    let mut res = next.run(req).await;
    /* a page not signed in yet must be able to read that it is not (a 401 says nothing else), or it
     * never shows its sign-in */
    if allowed || res.status() == StatusCode::UNAUTHORIZED {
        mark(&mut res);
    }
    res
}

#[cfg(test)]
mod cors_tests {
    use super::*;

    #[test]
    fn hosts_ignore_scheme_and_port() {
        assert_eq!(host_of("http://10.0.0.5:8441").as_deref(), Some("10.0.0.5"));
        assert_eq!(
            host_of("https://Steve.Example:443").as_deref(),
            Some("steve.example")
        );
        assert_eq!(host_of("http://[::1]:8441").as_deref(), Some("::1"));
        assert!(open_path("/auth/token") && !open_path("/api/me"));
    }
}

/* ───────── 设置 · 访问与客户端 (A4): the signed-in user's own tokens and clients ───────── */

fn store_of(st: &AppState) -> Result<Store, crate::error::ApiError> {
    st.modules
        .as_ref()
        .map(|m| m.store.clone())
        .ok_or_else(|| crate::error::ApiError::Json(503, "unavailable".into()))
}

fn internal(e: impl std::fmt::Display) -> crate::error::ApiError {
    crate::error::ApiError::Json(500, e.to_string())
}

/// `GET /api/access`: the caller's tokens and clients, and which client asks.
pub async fn overview(
    State(st): State<AppState>,
    axum::Extension(c): axum::Extension<crate::api::Ctx>,
) -> Result<axum::response::Response, crate::error::ApiError> {
    let store = store_of(&st)?;
    let me = c.me.clone();
    let (tokens, clients) = tokio::task::spawn_blocking(move || {
        Ok::<_, steve_store::StoreError>((
            store.access().tokens(Some(&me))?,
            store.access().clients(Some(&me))?,
        ))
    })
    .await
    .map_err(internal)?
    .map_err(internal)?;
    Ok(reply(
        StatusCode::OK,
        json!({
            "tokens": tokens,
            "clients": clients,
            "current": c.client.as_ref().map(|x| x.id.clone()),
            "route": c.client.as_ref().map(|x| x.route.clone()),
            "hub": {"version": st.cfg.version, "api": API_VERSION},
        }),
    ))
}

#[derive(Deserialize)]
pub struct NewToken {
    #[serde(default)]
    name: String,
    /// expires after this many days; none: never
    #[serde(default)]
    days: Option<i64>,
}

/// `POST /api/access/tokens {name, days?}`: a new token for the caller, its text returned once.
pub async fn token_create(
    State(st): State<AppState>,
    axum::Extension(c): axum::Extension<crate::api::Ctx>,
    Json(b): Json<NewToken>,
) -> Result<axum::response::Response, crate::error::ApiError> {
    let store = store_of(&st)?;
    let name = match cut(&b.name, 60) {
        n if n.is_empty() => "web".to_string(),
        n => n,
    };
    if b.days.is_some_and(|d| !(1..=3650).contains(&d)) {
        return Err(crate::error::ApiError::bad_request("days 1–3650"));
    }
    let expires = b.days.map(|d| steve_store::now() + d * 86_400_000);
    let me = c.me.clone();
    let (t, token) = tokio::task::spawn_blocking(move || {
        let r = store.access().token_create(&me, &name, expires)?;
        store.audit(
            &me,
            "token.create",
            &r.0.id,
            &json!({"name": name}),
            steve_store::WS,
        )?;
        Ok::<_, steve_store::StoreError>(r)
    })
    .await
    .map_err(internal)?
    .map_err(internal)?;
    Ok(reply(StatusCode::OK, json!({"token": token, "item": t})))
}

/// `DELETE /api/access/tokens/{id}`: revokes one of the caller's tokens and the clients it signed in.
pub async fn token_revoke(
    State(st): State<AppState>,
    axum::Extension(c): axum::Extension<crate::api::Ctx>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<axum::response::Response, crate::error::ApiError> {
    let store = store_of(&st)?;
    let me = c.me.clone();
    let done = tokio::task::spawn_blocking(move || {
        let Some(t) = store.access().token(&id)?.filter(|t| t.user_id == me) else {
            return Ok::<_, steve_store::StoreError>(false);
        };
        let ok = store.access().token_revoke(&t.id)?;
        store.audit(&me, "token.revoke", &t.id, &json!({}), steve_store::WS)?;
        Ok(ok)
    })
    .await
    .map_err(internal)?
    .map_err(internal)?;
    if !done {
        return Err(crate::error::ApiError::not_found("token"));
    }
    ORIGINS.lock().unwrap_or_else(|e| e.into_inner()).clear();
    Ok(reply(StatusCode::OK, json!({"ok": true})))
}

/// `DELETE /api/access/clients/{id}`: signs one of the caller's clients out.
pub async fn client_revoke(
    State(st): State<AppState>,
    axum::Extension(c): axum::Extension<crate::api::Ctx>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<axum::response::Response, crate::error::ApiError> {
    let store = store_of(&st)?;
    let me = c.me.clone();
    let done = tokio::task::spawn_blocking(move || {
        let Some(cl) = store.access().client(&id)?.filter(|x| x.user_id == me) else {
            return Ok::<_, steve_store::StoreError>(false);
        };
        let ok = store.access().client_revoke(&cl.id)?;
        store.audit(&me, "client.revoke", &cl.id, &json!({}), steve_store::WS)?;
        Ok(ok)
    })
    .await
    .map_err(internal)?
    .map_err(internal)?;
    if !done {
        return Err(crate::error::ApiError::not_found("client"));
    }
    ORIGINS.lock().unwrap_or_else(|e| e.into_inner()).clear();
    Ok(reply(StatusCode::OK, json!({"ok": true})))
}
