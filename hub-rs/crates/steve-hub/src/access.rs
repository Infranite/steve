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
static PAIR_FAILED: LazyLock<Mutex<HashMap<String, (Instant, u32)>>> =
    LazyLock::new(Default::default);

fn too_many_in(map: &Mutex<HashMap<String, (Instant, u32)>>, ip: &str) -> bool {
    let m = map.lock().unwrap_or_else(|e| e.into_inner());
    m.get(ip)
        .is_some_and(|(at, n)| at.elapsed() < TRIES_WINDOW && *n >= TRIES)
}

fn failed_in(map: &Mutex<HashMap<String, (Instant, u32)>>, ip: &str) {
    let mut m = map.lock().unwrap_or_else(|e| e.into_inner());
    let e = m.entry(ip.to_string()).or_insert((Instant::now(), 0));
    if e.0.elapsed() >= TRIES_WINDOW {
        *e = (Instant::now(), 0);
    }
    e.1 += 1;
}

fn too_many(ip: &str) -> bool {
    too_many_in(&FAILED, ip)
}

fn failed(ip: &str) {
    failed_in(&FAILED, ip);
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

/// Whether a cookie set in answer to this request gets `Secure`: the request came over TLS (a
/// proxy's `X-Forwarded-Proto: https`), or to the host of an https `STEVE_PUBLIC_URL` (a proxy that
/// does not say). Not for plain http to this machine or the local network, whatever the public URL
/// is: the browser drops a `Secure` cookie there, and the sign-in never sticks.
pub fn secure_request(cfg: &crate::config::Config, headers: &HeaderMap) -> bool {
    let tls = headers
        .get("x-forwarded-proto")
        .and_then(|v| v.to_str().ok())
        .is_some_and(|p| p.eq_ignore_ascii_case("https"));
    tls || (cfg.public_url.starts_with("https://")
        && headers
            .get(axum::http::header::HOST)
            .and_then(|v| v.to_str().ok())
            .and_then(|h| host_of(&format!("http://{h}")))
            .is_some_and(|h| host_of(&cfg.public_url) == Some(h)))
}

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
                "oidc": st.oidc_enabled(),
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
    let headers = req.headers().clone();
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
                retire_replaced(&store, &headers, &c.id).await;
                let secure = secure_request(&st.cfg, &headers);
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

/// A browser that signs in again is given a new client and a new cookie; the client its old cookie
/// named is left with a credential nobody holds. It is signed out here, with the pairings it had
/// open, so the list of clients keeps no entry that can never be used again.
async fn retire_replaced(store: &Store, headers: &HeaderMap, new_client: &str) {
    let Some(old) = crate::api::cookie(headers, COOKIE).filter(|c| !c.is_empty()) else {
        return;
    };
    let (store, new_client) = (store.clone(), new_client.to_string());
    let retired = tokio::task::spawn_blocking(move || {
        let (_, c) = store.access().client_user(&old).ok().flatten()?;
        if c.id == new_client {
            return None;
        }
        store.access().client_revoke(&c.id).ok();
        Some((c.user_id, c.id))
    })
    .await
    .ok()
    .flatten();
    if let Some((uid, cid)) = retired {
        drop_pending_pairings(|p| {
            p.user_id == uid && p.creator_client_id.as_deref() == Some(cid.as_str())
        });
    }
}

/// Signs this client out: it is revoked, any unclaimed pairings it created are dropped, and its cookie cleared.
async fn sign_out(State(st): State<AppState>, headers: HeaderMap) -> axum::response::Response {
    if let (Some(cred), Some(m)) = (credential_of(&headers), st.modules.as_ref()) {
        let store = m.store.clone();
        let revoked_client = tokio::task::spawn_blocking(move || {
            if let Ok(Some((_, c))) = store.access().client_user(&cred) {
                store.access().client_revoke(&c.id).ok();
                return Some((c.user_id, c.id));
            }
            None
        })
        .await
        .ok()
        .flatten();
        if let Some((uid, cid)) = revoked_client {
            drop_pending_pairings(|p| p.user_id == uid && p.creator_client_id.as_deref() == Some(cid.as_str()));
        }
    } else if let (Some(tok), true) = (
        crate::api::cookie(&headers, "steve_session"),
        st.modules.is_some(),
    ) {
        let b = st.backend.clone();
        let uid = tokio::task::spawn_blocking(move || {
            b.reader()
                .ok()
                .and_then(|r| r.user_by_session(&tok).ok().flatten())
                .and_then(|u| u.get("id").and_then(Value::as_str).map(str::to_string))
        })
        .await
        .ok()
        .flatten();
        if let Some(uid) = uid {
            drop_pending_pairings(|p| p.user_id == uid && p.creator_client_id.is_none());
        }
    }
    let mut res = reply(StatusCode::OK, json!({"ok": true}));
    if let Ok(v) = client_cookie("", 0, false, false).parse() {
        res.headers_mut().append(axum::http::header::SET_COOKIE, v);
    }
    res
}

/// `/hello`, `/token`, `/pair` and `/client/logout`, nested under `/auth` with the login routes; no
/// session required.
pub fn auth_routes(st: AppState) -> Router {
    Router::new()
        .route("/hello", get(hello))
        .route("/token", post(sign_in))
        .route("/pair", post(pair_sign_in))
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
    fn cookies_are_secure_over_tls_or_on_the_public_host_only() {
        let cfg = |url: &str| {
            let url = url.to_string();
            crate::config::Config::from_vars(move |k| match k {
                "STEVE_HOME" => Some("/tmp/steve-secure-test".into()),
                "STEVE_PUBLIC_URL" => Some(url.clone()),
                _ => None,
            })
            .expect("config")
        };
        let public = cfg("https://steve.example");
        // plain http on the local network or this machine: the browser would drop a Secure cookie
        assert!(!secure_request(
            &public,
            &h(&[("host", "192.168.0.33:8440")])
        ));
        assert!(!secure_request(&public, &h(&[("host", "127.0.0.1:8440")])));
        // the public host, through a proxy that says https or not
        assert!(secure_request(&public, &h(&[("host", "steve.example")])));
        assert!(secure_request(
            &public,
            &h(&[("host", "127.0.0.1:8440"), ("x-forwarded-proto", "https")])
        ));
        // no https public URL: only TLS the proxy reports
        let plain = cfg("http://127.0.0.1:8441");
        assert!(!secure_request(&plain, &h(&[("host", "127.0.0.1:8440")])));
        assert!(secure_request(
            &plain,
            &h(&[("host", "steve.example"), ("x-forwarded-proto", "https")])
        ));
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

    #[test]
    fn pairing_endpoints_prefer_public_url_else_lan_urls() {
        let mk = |host: &str, any: bool, pub_url: &str| {
            let host = host.to_string();
            let pub_url = pub_url.to_string();
            let allow = if any { "1" } else { "0" };
            crate::config::Config::from_vars(move |k| match k {
                "STEVE_HOME" => Some("/tmp/steve-pair-ep-test".into()),
                "STEVE_HOST" => Some(host.clone()),
                "STEVE_PORT" => Some("8440".into()),
                "STEVE_ALLOW_ANY_HOST" => Some(allow.into()),
                "STEVE_PUBLIC_URL" => Some(pub_url.clone()),
                _ => None,
            })
            .expect("config")
        };
        let ifs = [
            "192.168.1.42".parse().unwrap(),
            "100.64.0.9".parse().unwrap(),
        ];

        // Public HTTPS configured + wildcard listen: public_url is populated AND lan_urls fallback is included.
        let ep_pub = pair_endpoints_with(
            &mk("0.0.0.0", true, "https://steve.example"),
            &h(&[("host", "127.0.0.1:8440")]),
            &ifs,
        );
        assert_eq!(ep_pub.public_url.as_deref(), Some("https://steve.example"));
        assert_eq!(
            ep_pub.lan_urls,
            vec!["http://192.168.1.42:8440", "http://100.64.0.9:8440"]
        );
        assert!(!ep_pub.loopback_only);
        let cfg_pub = mk("0.0.0.0", true, "https://steve.example");
        let links = pair_links(&cfg_pub, "stp_abc", "My Hub", &ep_pub);
        assert_eq!(
            links.iter().map(|l| (l.kind, l.url.as_str())).collect::<Vec<_>>(),
            vec![
                ("public", "https://steve.example"),
                ("lan", "http://192.168.1.42:8440"),
                ("lan", "http://100.64.0.9:8440"),
            ],
            "one link per address, the public one first"
        );
        let query = "v=1&code=stp_abc&name=My%20Hub&pub=https%3A%2F%2Fsteve.example\
                     &lan=http%3A%2F%2F192.168.1.42%3A8440%2Chttp%3A%2F%2F100.64.0.9%3A8440";
        assert_eq!(links[0].link, format!("https://steve.example/#/pair?{query}"));
        assert_eq!(
            links[1].link,
            format!("http://192.168.1.42:8440/#/pair?{query}"),
            "every link names every address; only where the page is differs"
        );

        // No public URL, explicit LAN listen: public_url is None, lan_urls has the LAN address.
        let ep_lan = pair_endpoints_with(
            &mk("127.0.0.1,192.168.1.42", false, "http://127.0.0.1:8440"),
            &h(&[("host", "127.0.0.1:8440")]),
            &ifs,
        );
        assert_eq!(ep_lan.public_url, None);
        assert_eq!(ep_lan.lan_urls, vec!["http://192.168.1.42:8440"]);
        assert!(!ep_lan.loopback_only);

        // Pure loopback listen with loopback public_url: loopback_only is true.
        let ep_loop = pair_endpoints_with(
            &mk("127.0.0.1", false, "http://127.0.0.1:8440"),
            &h(&[("host", "127.0.0.1:8440")]),
            &ifs,
        );
        assert_eq!(ep_loop.public_url, None);
        assert!(ep_loop.lan_urls.is_empty());
        assert!(ep_loop.loopback_only);

        // A name under `localhost` is this machine to whoever asks: offered to nobody.
        let ep_named = pair_endpoints_with(
            &mk("127.0.0.1", false, "https://steve.localhost"),
            &h(&[("host", "steve.localhost")]),
            &ifs,
        );
        assert!(ep_named.loopback_only, "{ep_named:?}");

        // Names that resolve only inside a network are of the local network, not public.
        for (name, local) in [
            ("nas", true),
            ("nas.home", true),
            ("nas.home.arpa", true),
            ("nas.internal", true),
            ("nas.lan.", true),
            ("steve.example", false),
            ("localhome.example", false),
        ] {
            let ep = pair_endpoints_with(
                &mk("127.0.0.1", false, &format!("http://{name}:8440")),
                &h(&[("host", "127.0.0.1:8440")]),
                &ifs,
            );
            assert_eq!(ep.public_url.is_none(), local, "{name}: {ep:?}");
            assert_eq!(ep.lan_urls.is_empty(), !local, "{name}: {ep:?}");
        }
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
    matches!(
        path,
        "/auth/hello" | "/auth/token" | "/auth/pair" | "/auth/client/logout"
    )
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
    let tid = id.clone();
    let done = tokio::task::spawn_blocking(move || {
        let Some(t) = store.access().token(&tid)?.filter(|t| t.user_id == me) else {
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
    let uid = c.me.clone();
    drop_pending_pairings(|p| p.user_id == uid && p.token_id.as_deref() == Some(id.as_str()));
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
    let cid = id.clone();
    let done = tokio::task::spawn_blocking(move || {
        let Some(cl) = store.access().client(&cid)?.filter(|x| x.user_id == me) else {
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
    let uid = c.me.clone();
    drop_pending_pairings(|p| p.user_id == uid && p.creator_client_id.as_deref() == Some(id.as_str()));
    ORIGINS.lock().unwrap_or_else(|e| e.into_inner()).clear();
    Ok(reply(StatusCode::OK, json!({"ok": true})))
}

/* ───────── QR client pairing (mobile / companion clients) ───────── */

/// How long an unused pairing code stays valid (5 minutes).
const PAIR_TTL_MS: i64 = 5 * 60 * 1000;
/// How long a claimed pairing stays inspectable so the showing screen can confirm pairing.
const PAIR_KEEP_MS: i64 = 15 * 60 * 1000;
/// Maximum retained pairings per user (evicting oldest unclaimed/expired first).
const PAIR_MAX_PER_USER: usize = 16;
const PAIR_PREFIX: &str = "stp_";

#[derive(Clone)]
struct PendingPair {
    id: String,
    user_id: String,
    creator_client_id: Option<String>,
    token_id: Option<String>,
    created_at: i64,
    code_hash: String,
    expires: i64,
    claimed_at: Option<i64>,
    client: Option<Value>,
}

static PAIRINGS: LazyLock<Mutex<HashMap<String, PendingPair>>> = LazyLock::new(Default::default);

fn drop_pending_pairings(pred: impl Fn(&PendingPair) -> bool) {
    let mut m = PAIRINGS.lock().unwrap_or_else(|e| e.into_inner());
    m.retain(|_, p| p.client.is_some() || !pred(p));
}

/// A pairing code: 122 random bits, single use, five minutes, behind the claim lockout. Short so the
/// link that carries it makes a QR code a phone reads at arm's length.
fn pair_code() -> String {
    format!("{PAIR_PREFIX}{}", uuid::Uuid::new_v4().simple())
}

fn pair_hash(code: &str) -> String {
    use sha2::{Digest, Sha256};
    hex::encode(Sha256::digest(code.trim().as_bytes()))
}

fn clean_pairings(m: &mut HashMap<String, PendingPair>, now_ms: i64) {
    m.retain(|_, p| match p.claimed_at {
        Some(at) => now_ms - at < PAIR_KEEP_MS,
        None => now_ms < p.expires + 60_000,
    });
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairEndpoints {
    pub public_url: Option<String>,
    pub lan_urls: Vec<String>,
    pub loopback_only: bool,
}

/// A host another device reaches only from inside this network: a private or overlay address, a
/// name with no dot, or a name under a suffix that is reserved or widely used for local networks.
fn is_local_or_private_host(host: &str) -> bool {
    let h = host.trim_matches(['[', ']']).to_ascii_lowercase();
    let h = h.trim_end_matches('.');
    const LOCAL_SUFFIXES: [&str; 6] = [
        ".local",
        ".lan",
        ".home",
        ".home.arpa",
        ".internal",
        ".localdomain",
    ];
    if is_loopback_name(h) || LOCAL_SUFFIXES.iter().any(|s| h.ends_with(s)) {
        return true;
    }
    if let Ok(ip) = h.parse::<IpAddr>() {
        return ip.is_loopback()
            || ip.is_unspecified()
            || private(&ip)
            || tailnet(&ip)
            || matches!(ip, IpAddr::V6(v) if v.to_ipv4_mapped().is_some_and(|m| {
                let mip = IpAddr::V4(m);
                mip.is_loopback() || private(&mip) || tailnet(&mip)
            }));
    }
    !h.is_empty() && !h.contains('.')
}

/// `localhost` and every name under it, which resolve to this machine wherever they are asked.
fn is_loopback_name(h: &str) -> bool {
    h == "localhost" || h.ends_with(".localhost")
}

/// A host that names the machine it is asked on, so no other device can use it for this Hub.
fn is_loopback_host(host: &str) -> bool {
    let h = host.trim_matches(['[', ']']).to_ascii_lowercase();
    let h = h.trim_end_matches('.');
    is_loopback_name(h) || h.parse::<IpAddr>().is_ok_and(|ip| {
        ip.is_loopback()
            || matches!(ip, IpAddr::V6(v) if v.to_ipv4_mapped().is_some_and(|m| m.is_loopback()))
    })
}

/// Active non-loopback IPv4 interface addresses on this machine (LAN first, tailnet after).
#[cfg(unix)]
fn machine_lan_ipv4s() -> Vec<std::net::Ipv4Addr> {
    let mut lan = Vec::new();
    let mut tail = Vec::new();
    // SAFETY: `getifaddrs` initializes `addrs` on return code 0 and `freeifaddrs` frees the list
    // before returning; every pointer check guards against null `ifa_addr` entries.
    unsafe {
        let mut addrs: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&mut addrs) != 0 || addrs.is_null() {
            return lan;
        }
        let mut cur = addrs;
        while !cur.is_null() {
            let ifa = &*cur;
            let flags = ifa.ifa_flags as i32;
            let up = (flags & libc::IFF_UP) != 0 && (flags & libc::IFF_LOOPBACK) == 0;
            if up && !ifa.ifa_addr.is_null() && (*ifa.ifa_addr).sa_family as i32 == libc::AF_INET {
                let sin = &*(ifa.ifa_addr as *const libc::sockaddr_in);
                let ip = std::net::Ipv4Addr::from(u32::from_be(sin.sin_addr.s_addr));
                if !ip.is_loopback() && !ip.is_unspecified() && !ip.is_link_local() {
                    let addr = IpAddr::V4(ip);
                    if tailnet(&addr) {
                        if !tail.contains(&ip) {
                            tail.push(ip);
                        }
                    } else if !lan.contains(&ip) {
                        lan.push(ip);
                    }
                }
            }
            cur = ifa.ifa_next;
        }
        libc::freeifaddrs(addrs);
    }
    lan.extend(tail);
    lan
}

#[cfg(not(unix))]
fn machine_lan_ipv4s() -> Vec<std::net::Ipv4Addr> {
    Vec::new()
}

/// Resolves the public and internal/LAN URLs a companion device can use to reach this Hub.
/// When `public_url` is present, clients connect over the public URL by default and keep `lan_urls`
/// as fallback; when `public_url` is absent, clients connect via `lan_urls`.
pub fn pair_endpoints_with(
    cfg: &crate::config::Config,
    headers: &HeaderMap,
    if_ips: &[std::net::Ipv4Addr],
) -> PairEndpoints {
    let mut public_url: Option<String> = None;
    let mut lan_urls: Vec<String> = Vec::new();
    let push_lan = |list: &mut Vec<String>, u: String| {
        let u = u.trim_end_matches('/').to_string();
        if !u.is_empty() && !list.contains(&u) {
            list.push(u);
        }
    };

    let mut candidates = Vec::new();
    if !cfg.public_url.is_empty() {
        candidates.push(cfg.public_url.clone());
    }
    for u in &cfg.public_urls {
        if !candidates.contains(u) {
            candidates.push(u.clone());
        }
    }
    let hub_port_for = |u: &reqwest::Url| -> String {
        // When `STEVE_WEB_PORT` splits the web client onto its own port, `public_url` may name the
        // web port; API & `/auth/*` live on `cfg.listen.port`.
        if let Some(wp) = cfg.web_port
            && u.port() == Some(wp)
            && wp != cfg.listen.port
        {
            let mut copy = u.clone();
            let _ = copy.set_port(Some(cfg.listen.port));
            return copy.as_str().trim_end_matches('/').to_string();
        }
        u.as_str().trim_end_matches('/').to_string()
    };

    for raw in &candidates {
        let Ok(u) = reqwest::Url::parse(raw) else {
            continue;
        };
        let Some(host) = u.host_str() else { continue };
        if is_loopback_host(host) {
            continue;
        }
        let normalized = hub_port_for(&u);
        if is_local_or_private_host(host) {
            push_lan(&mut lan_urls, normalized);
        } else if public_url.is_none() {
            public_url = Some(normalized);
        }
    }

    // If the pairing creator itself reached the Hub through a non-loopback Host header (e.g. a
    // reverse proxy or LAN IP), include that origin too.
    if let Some(raw_host) = headers
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok())
    {
        let scheme = if secure_request(cfg, headers) {
            "https"
        } else {
            "http"
        };
        if let Ok(u) = reqwest::Url::parse(&format!("{scheme}://{raw_host}"))
            && let Some(host) = u.host_str()
            && !is_loopback_host(host)
        {
            let normalized = hub_port_for(&u);
            if is_local_or_private_host(host) {
                push_lan(&mut lan_urls, normalized);
            } else if public_url.is_none() {
                public_url = Some(normalized);
            }
        }
    }

    // Explicit non-loopback listen hosts (`STEVE_HOST=127.0.0.1,192.168.1.20` or tailnet IP).
    let port = cfg.listen.port;
    for h in &cfg.listen.hosts {
        if let Ok(ip) = h.parse::<IpAddr>() {
            if ip.is_loopback() || ip.is_unspecified() {
                continue;
            }
            let formatted = match ip {
                IpAddr::V4(v) => format!("http://{v}:{port}"),
                IpAddr::V6(v) => format!("http://[{v}]:{port}"),
            };
            push_lan(&mut lan_urls, formatted);
        } else if !is_loopback_host(h) {
            push_lan(&mut lan_urls, format!("http://{h}:{port}"));
        }
    }

    // Wildcard listen (`0.0.0.0` / `::`): every active LAN / tailnet IPv4 interface is reachable.
    if cfg.listen.any() {
        for ip in if_ips {
            push_lan(&mut lan_urls, format!("http://{ip}:{port}"));
        }
    }

    let loopback_only = public_url.is_none() && lan_urls.is_empty();
    PairEndpoints {
        public_url,
        lan_urls,
        loopback_only,
    }
}

/// Where the pages of the Hub at `endpoint` are: the same address, on the web client's own port
/// when it has one ([`Config::web_port`]).
fn page_of(cfg: &crate::config::Config, endpoint: &str) -> String {
    if let Some(wp) = cfg.web_port
        && wp != cfg.listen.port
        && let Ok(mut u) = reqwest::Url::parse(endpoint)
        && u.port() == Some(cfg.listen.port)
        && u.set_port(Some(wp)).is_ok()
    {
        return u.as_str().trim_end_matches('/').to_string();
    }
    endpoint.trim_end_matches('/').to_string()
}

/// One way to this Hub for a pairing: its address, whether that is the public one, and the pairing
/// link on that address.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct PairLink {
    pub url: String,
    pub kind: &'static str,
    pub link: String,
}

/// The pairing links of a code, one per address of the Hub, the public one first:
/// `<pages of that address>/#/pair?v=1&code=…&name=…&pub=…&lan=…`. A browser that opens one gets this
/// Hub's web client, which claims the code; an app reads the same link from the QR code. The
/// parameters are in the fragment, so the code is in no request line, log or `Referer`.
fn pair_links(
    cfg: &crate::config::Config,
    code: &str,
    name: &str,
    ep: &PairEndpoints,
) -> Vec<PairLink> {
    let mut q = reqwest::Url::parse("steve://pair").expect("static scheme");
    {
        let mut q = q.query_pairs_mut();
        q.append_pair("v", "1");
        q.append_pair("code", code);
        if !name.is_empty() {
            q.append_pair("name", name);
        }
        if let Some(pub_url) = &ep.public_url {
            q.append_pair("pub", pub_url);
        }
        if !ep.lan_urls.is_empty() {
            q.append_pair("lan", &ep.lan_urls.join(","));
        }
    }
    /* a space as %20: not every reader of a query takes `+` for one */
    let query = q.query().unwrap_or_default().replace('+', "%20");
    ep.public_url
        .iter()
        .map(|u| (u, "public"))
        .chain(ep.lan_urls.iter().map(|u| (u, "lan")))
        .map(|(u, kind)| PairLink {
            url: u.clone(),
            kind,
            link: format!("{}/#/pair?{query}", page_of(cfg, u)),
        })
        .collect()
}

#[derive(Deserialize, Default)]
pub struct PairCreateIn {
    #[serde(default)]
    name: String,
}

/// `POST /api/access/pairings`: creates a short-lived single-use QR pairing code for the signed-in user.
pub async fn pair_create(
    State(st): State<AppState>,
    headers: HeaderMap,
    axum::Extension(c): axum::Extension<crate::api::Ctx>,
    body: Option<Json<PairCreateIn>>,
) -> Result<axum::response::Response, crate::error::ApiError> {
    let store = store_of(&st)?;
    let creator_client_id = c.client.as_ref().map(|x| x.id.clone());
    let creator_token_id = if let Some(cid) = creator_client_id.clone() {
        let s_tok = store.clone();
        tokio::task::spawn_blocking(move || {
            Ok::<_, steve_store::StoreError>(s_tok.access().client(&cid)?.and_then(|cl| cl.token_id))
        })
        .await
        .map_err(internal)?
        .map_err(internal)?
    } else {
        None
    };
    let now_ms = steve_store::now();
    let expires = now_ms + PAIR_TTL_MS;
    let code = pair_code();
    let id = steve_store::uid("pair");
    let ep = pair_endpoints_with(&st.cfg, &headers, &machine_lan_ipv4s());
    let custom_name = body.map(|Json(b)| cut(&b.name, 60)).unwrap_or_default();
    let hub_name = if !custom_name.is_empty() {
        custom_name
    } else {
        ep.public_url
            .as_deref()
            .and_then(host_of)
            .or_else(|| ep.lan_urls.first().and_then(|u| host_of(u)))
            .unwrap_or_else(|| "Steve Hub".into())
    };
    let links = pair_links(&st.cfg, &code, &hub_name, &ep);
    {
        let mut m = PAIRINGS.lock().unwrap_or_else(|e| e.into_inner());
        clean_pairings(&mut m, now_ms);
        while m.values().filter(|p| p.user_id == c.me).count() >= PAIR_MAX_PER_USER {
            let evict_id = m
                .iter()
                .filter(|(_, p)| p.user_id == c.me)
                .min_by_key(|(_, p)| (p.client.is_some(), p.created_at))
                .map(|(k, _)| k.clone());
            let Some(evict_id) = evict_id else {
                break;
            };
            m.remove(&evict_id);
        }
        m.insert(
            id.clone(),
            PendingPair {
                id: id.clone(),
                user_id: c.me.clone(),
                creator_client_id,
                token_id: creator_token_id,
                created_at: now_ms,
                code_hash: pair_hash(&code),
                expires,
                claimed_at: None,
                client: None,
            },
        );
    }
    let me = c.me.clone();
    let pid = id.clone();
    tokio::task::spawn_blocking(move || {
        store
            .audit(&me, "pair.create", &pid, &json!({}), steve_store::WS)
            .ok();
    })
    .await
    .ok();
    Ok(reply(
        StatusCode::OK,
        json!({
            "id": id,
            "code": code,
            "name": hub_name,
            "expires": expires,
            "public_url": ep.public_url,
            "lan_urls": ep.lan_urls,
            "loopback_only": ep.loopback_only,
            "links": links,
        }),
    ))
}

/// `GET /api/access/pairings/{id}`: lets the QR modal poll whether the mobile device has paired.
pub async fn pair_status(
    axum::Extension(c): axum::Extension<crate::api::Ctx>,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> Result<axum::response::Response, crate::error::ApiError> {
    let now_ms = steve_store::now();
    let item = {
        let mut m = PAIRINGS.lock().unwrap_or_else(|e| e.into_inner());
        clean_pairings(&mut m, now_ms);
        m.get(&id).filter(|p| p.user_id == c.me).cloned()
    };
    let Some(p) = item else {
        return Err(crate::error::ApiError::not_found("pairing"));
    };
    let status = if p.client.is_some() {
        "claimed"
    } else if now_ms >= p.expires {
        "expired"
    } else {
        "pending"
    };
    Ok(reply(
        StatusCode::OK,
        json!({
            "id": p.id,
            "status": status,
            "expires": p.expires,
            "client": p.client,
        }),
    ))
}

#[derive(Deserialize)]
struct PairSignIn {
    code: String,
    #[serde(default)]
    client: ClientInfo,
    #[serde(default)]
    cookie: bool,
}

/// `POST /auth/pair {code, client, cookie?}`: exchanges a single-use pairing code for a client credential.
async fn pair_sign_in(State(st): State<AppState>, req: Request) -> axum::response::Response {
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
    let headers = req.headers().clone();
    let host = req
        .headers()
        .get(axum::http::header::HOST)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string);
    let key = ip
        .clone()
        .filter(|_| !route.starts_with("loopback"))
        .unwrap_or_default();
    if !key.is_empty() && too_many_in(&PAIR_FAILED, &key) {
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
    let Ok(b) = serde_json::from_slice::<PairSignIn>(&bytes) else {
        return reply(StatusCode::BAD_REQUEST, json!({"error": "body"}));
    };
    let now_ms = steve_store::now();
    let wanted = pair_hash(&b.code);
    let matched = {
        let mut m = PAIRINGS.lock().unwrap_or_else(|e| e.into_inner());
        clean_pairings(&mut m, now_ms);
        let found_id = m
            .iter()
            .find(|(_, p)| p.claimed_at.is_none() && now_ms < p.expires && p.code_hash == wanted)
            .map(|(k, _)| k.clone());
        found_id.and_then(|id| {
            let p = m.get_mut(&id)?;
            p.claimed_at = Some(now_ms);
            Some((
                p.id.clone(),
                p.user_id.clone(),
                p.creator_client_id.clone(),
                p.token_id.clone(),
            ))
        })
    };
    let Some((pair_id, user_id, creator_client_id, token_id)) = matched else {
        if !key.is_empty() {
            failed_in(&PAIR_FAILED, &key);
        }
        return reply(
            StatusCode::UNAUTHORIZED,
            json!({"error": steve_i18n::t!("hub.access.pair_expired")}),
        );
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
    let uid_copy = user_id.clone();
    let done = tokio::task::spawn_blocking(move || {
        if let Some(cid) = &creator_client_id {
            let Some(cl) = s2.access().client(cid)? else {
                return Ok(None);
            };
            if cl.revoked.is_some() {
                return Ok(None);
            }
        }
        if let Some(tid) = &token_id {
            let Some(tok) = s2.access().token(tid)? else {
                return Ok(None);
            };
            if tok.revoked.is_some() || tok.expires.is_some_and(|exp| exp <= now_ms) {
                return Ok(None);
            }
        }
        s2.access().client_create(
            &uid_copy,
            token_id.as_deref(),
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
            {
                let mut m = PAIRINGS.lock().unwrap_or_else(|e| e.into_inner());
                if let Some(p) = m.get_mut(&pair_id) {
                    p.client = Some(json!({
                        "id": c.id,
                        "kind": c.kind,
                        "name": c.name,
                        "platform": c.platform,
                        "route": route,
                    }));
                }
            }
            seen(&store, &c, &route, ip);
            store
                .audit(
                    &u.id,
                    "client.pair",
                    &c.id,
                    &json!({"kind": c.kind, "name": c.name, "route": route, "pair_id": pair_id}),
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
                retire_replaced(&store, &headers, &c.id).await;
                let secure = secure_request(&st.cfg, &headers);
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
            PAIRINGS
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&pair_id);
            reply(
                StatusCode::UNAUTHORIZED,
                json!({"error": steve_i18n::t!("hub.access.pair_expired")}),
            )
        }
        Ok(Err(e)) => {
            if let Some(p) = PAIRINGS
                .lock()
                .unwrap_or_else(|err| err.into_inner())
                .get_mut(&pair_id)
            {
                p.claimed_at = None;
            }
            tracing::error!("pair sign in: {e}");
            reply(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({"error": "internal"}),
            )
        }
        Err(e) => {
            if let Some(p) = PAIRINGS
                .lock()
                .unwrap_or_else(|err| err.into_inner())
                .get_mut(&pair_id)
            {
                p.claimed_at = None;
            }
            tracing::error!("pair sign in: {e}");
            reply(
                StatusCode::INTERNAL_SERVER_ERROR,
                json!({"error": "internal"}),
            )
        }
    }
}

/* ───────── single sign-on in the settings (A4b) ───────── */

/// Puts an OIDC configuration in force without a restart (steve-auth's `Auth`).
pub trait OidcSwitch: Send + Sync {
    fn set(
        &self,
        issuer: Option<String>,
        client_id: Option<String>,
        secret: Option<String>,
    ) -> Result<(), String>;
    fn enabled(&self) -> bool;
}

impl<St: steve_auth::UserStore> OidcSwitch for steve_auth::Auth<St> {
    fn set(
        &self,
        issuer: Option<String>,
        client_id: Option<String>,
        secret: Option<String>,
    ) -> Result<(), String> {
        self.set_oidc(steve_auth::OidcConfig {
            issuer,
            client_id,
            client_secret: secret,
        })
        .map_err(|e| e.to_string())
    }
    fn enabled(&self) -> bool {
        self.oidc_enabled()
    }
}

/// The settings key of single sign-on (`{issuer, client_id}`; the secret is a private file).
const OIDC_KEY: &str = "access.oidc";
/// Where the Hub keeps the client secret set in the settings (mode 0600).
const OIDC_SECRET: &str = "secrets/oidc-client-secret";

fn read_secret(home: &std::path::Path) -> Option<String> {
    std::fs::read_to_string(home.join(OIDC_SECRET))
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn write_secret(home: &std::path::Path, secret: &str) -> std::io::Result<()> {
    use std::io::Write;
    use std::os::unix::fs::OpenOptionsExt;
    let path = home.join(OIDC_SECRET);
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d)?;
    }
    let tmp = path.with_extension("tmp");
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .mode(0o600)
        .open(&tmp)?;
    f.write_all(secret.as_bytes())?;
    f.sync_all()?;
    std::fs::rename(tmp, path)
}

/// At start: single sign-on as the settings left it, when the environment configures none.
pub fn restore_oidc(
    store: &Store,
    home: &std::path::Path,
    auth: &dyn OidcSwitch,
) -> Result<(), String> {
    let v: Option<Value> = store
        .settings()
        .get(steve_store::WS, OIDC_KEY, None)
        .map_err(|e| e.to_string())?;
    let Some(v) = v.filter(|v| v["issuer"].is_string()) else {
        return Ok(());
    };
    auth.set(
        v["issuer"].as_str().map(str::to_string),
        v["client_id"].as_str().map(str::to_string),
        read_secret(home),
    )
}

fn owner_only(c: &crate::api::Ctx) -> Result<(), crate::error::ApiError> {
    if c.user.get("role").and_then(Value::as_str) == Some("owner") {
        Ok(())
    } else {
        Err(crate::error::ApiError::Json(
            403,
            steve_i18n::t!("hub.access.owner_only"),
        ))
    }
}

/// What the settings show: where the configuration comes from (`env` is read-only), the issuer and
/// client, whether a secret is kept, and the callback to register with the provider.
fn oidc_view(st: &AppState, store: &Store) -> Value {
    let callback = format!("{}/auth/callback", st.cfg.public_url);
    if st.cfg.oidc.issuer.is_some() {
        return json!({
            "source": "env", "issuer": st.cfg.oidc.issuer, "client_id": st.cfg.oidc.client_id,
            "has_secret": st.cfg.oidc.client_secret.is_some(), "callback": callback,
            "enabled": st.oidc_enabled(),
        });
    }
    let v: Value = store
        .settings()
        .get(steve_store::WS, OIDC_KEY, Value::Null)
        .unwrap_or(Value::Null);
    json!({
        "source": if v["issuer"].is_string() { "settings" } else { "none" },
        "issuer": v["issuer"], "client_id": v["client_id"],
        "has_secret": read_secret(&st.cfg.home).is_some(), "callback": callback,
        "enabled": st.oidc_enabled(),
    })
}

/// `GET /api/access/oidc`
pub async fn oidc_get(
    State(st): State<AppState>,
    axum::Extension(c): axum::Extension<crate::api::Ctx>,
) -> Result<axum::response::Response, crate::error::ApiError> {
    owner_only(&c)?;
    let store = store_of(&st)?;
    Ok(reply(StatusCode::OK, oidc_view(&st, &store)))
}

#[derive(Deserialize)]
pub struct OidcIn {
    issuer: String,
    client_id: String,
    /// left out: the one kept stays
    #[serde(default)]
    secret: Option<String>,
}

/// `PUT /api/access/oidc {issuer, client_id, secret?}`: single sign-on from now on (owner only; not
/// while the environment configures it).
pub async fn oidc_put(
    State(st): State<AppState>,
    axum::Extension(c): axum::Extension<crate::api::Ctx>,
    Json(b): Json<OidcIn>,
) -> Result<axum::response::Response, crate::error::ApiError> {
    owner_only(&c)?;
    if st.cfg.oidc.issuer.is_some() {
        return Err(crate::error::ApiError::Json(
            409,
            steve_i18n::t!("hub.access.oidc_env"),
        ));
    }
    let issuer = b.issuer.trim().trim_end_matches('/').to_string();
    let client_id = b.client_id.trim().to_string();
    let ok_issuer = reqwest::Url::parse(&issuer).is_ok_and(|u| {
        u.scheme() == "https"
            || (u.scheme() == "http"
                && matches!(u.host_str(), Some("localhost" | "127.0.0.1" | "[::1]")))
    });
    if !ok_issuer || client_id.is_empty() {
        return Err(crate::error::ApiError::bad_request(steve_i18n::t!(
            "hub.access.oidc_bad"
        )));
    }
    let Some(switch) = st.oidc.clone() else {
        return Err(crate::error::ApiError::Json(503, "unavailable".into()));
    };
    let store = store_of(&st)?;
    let home = st.cfg.home.clone();
    let (st2, me) = (st.clone(), c.me.clone());
    let view = tokio::task::spawn_blocking(move || -> Result<Value, String> {
        if let Some(s) = b.secret.as_deref().map(str::trim).filter(|s| !s.is_empty()) {
            write_secret(&home, s).map_err(|e| e.to_string())?;
        }
        let mut m = serde_json::Map::new();
        m.insert(
            OIDC_KEY.into(),
            json!({"issuer": issuer, "client_id": client_id}),
        );
        store
            .settings()
            .set(steve_store::WS, &m, &me)
            .map_err(|e| e.to_string())?;
        switch.set(
            Some(issuer.clone()),
            Some(client_id.clone()),
            read_secret(&home),
        )?;
        store
            .audit(
                &me,
                "oidc.set",
                "oidc",
                &json!({"issuer": issuer}),
                steve_store::WS,
            )
            .ok();
        Ok(oidc_view(&st2, &store))
    })
    .await
    .map_err(internal)?
    .map_err(internal)?;
    Ok(reply(StatusCode::OK, view))
}

/// `DELETE /api/access/oidc`: single sign-on off (sign-in by token stays).
pub async fn oidc_delete(
    State(st): State<AppState>,
    axum::Extension(c): axum::Extension<crate::api::Ctx>,
) -> Result<axum::response::Response, crate::error::ApiError> {
    owner_only(&c)?;
    if st.cfg.oidc.issuer.is_some() {
        return Err(crate::error::ApiError::Json(
            409,
            steve_i18n::t!("hub.access.oidc_env"),
        ));
    }
    let Some(switch) = st.oidc.clone() else {
        return Err(crate::error::ApiError::Json(503, "unavailable".into()));
    };
    let store = store_of(&st)?;
    let home = st.cfg.home.clone();
    let (st2, me) = (st.clone(), c.me.clone());
    let view = tokio::task::spawn_blocking(move || -> Result<Value, String> {
        let mut m = serde_json::Map::new();
        m.insert(OIDC_KEY.into(), Value::Null);
        store
            .settings()
            .set(steve_store::WS, &m, &me)
            .map_err(|e| e.to_string())?;
        let _ = std::fs::remove_file(home.join(OIDC_SECRET));
        switch.set(None, None, None)?;
        store
            .audit(&me, "oidc.off", "oidc", &json!({}), steve_store::WS)
            .ok();
        Ok(oidc_view(&st2, &store))
    })
    .await
    .map_err(internal)?
    .map_err(internal)?;
    Ok(reply(StatusCode::OK, view))
}

/// Notes which client a decision was answered from (A4c): `result.via` on the decision, and the audit.
/// Off the request; a browser session (no client) notes nothing.
pub fn note_via(st: &AppState, decision: &str, c: &crate::api::Ctx) {
    let (Some(client), Some(store)) = (
        c.client.clone(),
        st.modules.as_ref().map(|m| m.store.clone()),
    ) else {
        return;
    };
    let (id, me) = (decision.to_string(), c.me.clone());
    tokio::task::spawn_blocking(move || {
        let via = json!({"client": client.id, "kind": client.kind, "name": client.name, "route": client.route});
        let r = store.with_conn(|db| {
            db.execute(
                "UPDATE decisions SET result = json_set(coalesce(result, '{}'), '$.via', json(?)) WHERE id = ? AND status <> 'open'",
                rusqlite::params![via.to_string(), id],
            )?;
            Ok(())
        });
        if let Err(e) = r {
            tracing::warn!("decision {id} via: {e}");
        }
        store
            .audit(
                &me,
                "decision.resolve",
                &id,
                &json!({"via": via}),
                steve_store::WS,
            )
            .ok();
    });
}
