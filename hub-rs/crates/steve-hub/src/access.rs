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
            reply(
                StatusCode::OK,
                json!({
                    "client_id": c.id,
                    "credential": credential(&c.id, &secret),
                    "user": {"id": u.id, "name": u.name},
                    "hub": {"version": st.cfg.version, "api": API_VERSION},
                }),
            )
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

/// `/hello` and `/token`, nested under `/auth` with the login routes; no session required.
pub fn auth_routes(st: AppState) -> Router {
    Router::new()
        .route("/hello", get(hello))
        .route("/token", post(sign_in))
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
