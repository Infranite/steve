//! The whole HTTP surface and the boot sequence (main.ts).

use std::net::{IpAddr, SocketAddr};

use axum::Router;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::header;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use serde_json::{Value, json};

use crate::api::{self, AppState};
use crate::backend::BackendError;
use crate::error::json_response;

/// Whether the TCP peer is this machine: 127/8, `::1` or an IPv4-mapped 127/8 (main.ts and auth.ts match
/// `^(127\.|::1|::ffff:127\.)` on the text, which also let `::1234:…` through).
pub fn is_loopback(addr: &SocketAddr) -> bool {
    match addr.ip() {
        IpAddr::V4(v4) => v4.is_loopback(),
        IpAddr::V6(v6) => {
            v6.is_loopback() || v6.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback())
        }
    }
}

/// Everything under `/mcp` (prefix match, so `/mcpx` too) bypasses the rest of the app: loopback peers
/// go to the MCP service, anyone else gets an empty 403.
async fn mcp_gate(State(mcp): State<Option<Router>>, req: Request, next: Next) -> Response {
    if !req.uri().path().starts_with("/mcp") {
        return next.run(req).await;
    }
    let peer = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0);
    if !peer.as_ref().is_some_and(is_loopback) {
        return axum::http::StatusCode::FORBIDDEN.into_response();
    }
    match mcp {
        Some(svc) => {
            use tower::ServiceExt;
            match svc.oneshot(req).await {
                Ok(r) => r,
                Err(e) => match e {},
            }
        }
        None => crate::error::ApiError::no_route().into_response(),
    }
}

/// The release script's hand-off before it restarts the Hub: loopback only, with the token the Hub gave that
/// release. `POST` starts holding new turns, `GET` only reports.
async fn drain_call(st: AppState, start: bool, req: Request) -> Response {
    let peer = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0);
    let auth = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default();
    let token = match auth.strip_prefix("Bearer ") {
        Some(t) if peer.as_ref().is_some_and(is_loopback) => t.to_string(),
        _ => return crate::error::text_response(404, "not found"),
    };
    match st.domain.drain(&token, start).await {
        Ok(Some(r)) => json_response(200, &Value::Object(r)),
        Ok(None) => crate::error::text_response(404, "not found"),
        // no domain wired means no release in progress
        Err(e) if e.message.starts_with(crate::backend::UNIMPLEMENTED) => {
            crate::error::text_response(404, "not found")
        }
        Err(e) => crate::error::ApiError::from(e).into_response(),
    }
}

/// Queue-capable chat routes remain available; other new mutations receive a retryable
/// maintenance response. In-flight requests hold a permit until their response is produced.
async fn maintenance_gate(State(st): State<AppState>, req: Request, next: Next) -> Response {
    let path = req.uri().path();
    let read = matches!(
        *req.method(),
        axum::http::Method::GET | axum::http::Method::HEAD | axum::http::Method::OPTIONS
    );
    let queues = path == "/api/thread"
        || path == "/api/files"
        || (path.starts_with("/api/sessions/") && path.ends_with("/messages"))
        || path.ends_with("/stop")
        || (path.starts_with("/api/sessions/") && path.ends_with("/cancel"))
        || (*req.method() == axum::http::Method::DELETE && path.starts_with("/api/thread/runs/"))
        || path.starts_with("/api/thread/queued/");
    if read || queues || path.starts_with("/internal/") {
        return next.run(req).await;
    }
    let Some(modules) = &st.modules else {
        return next.run(req).await;
    };
    let permit = modules.store.try_activity("http_mutation");
    let _permit = match permit {
        Some(p) => p,
        None => {
            let token = req
                .headers()
                .get(header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("Bearer "));
            // a Node turn waiting for a permission holds the drain until it is answered (D1), so
            // its answer must get through
            let card = (*req.method() == axum::http::Method::POST)
                .then(|| path.strip_prefix("/api/decisions/"))
                .flatten()
                .filter(|id| !id.is_empty() && !id.contains('/'));
            let answer = card.and_then(|id| {
                modules
                    .store
                    .continuing_activity("node_permission_answer", || {
                        crate::wire::node_runtime::waiting_card(&modules.store, id)
                    })
            });
            let continuation = answer.or_else(|| {
                modules.store.continuing_activity("mcp_existing_turn", || {
                    path.starts_with("/mcp")
                        && token.is_some_and(|token| {
                            modules
                                .store
                                .sessions()
                                .by_token(token)
                                .ok()
                                .flatten()
                                .is_some_and(|s| {
                                    matches!(s.status.as_str(), "running" | "starting")
                                })
                                || modules
                                    .store
                                    .thread()
                                    .run_by_token(token)
                                    .ok()
                                    .flatten()
                                    .is_some_and(|r| r.status == "running")
                        })
                })
            });
            let Some(permit) = continuation else {
                return json_response(
                    503,
                    &json!({"error": steve_i18n::t!("hub.maintenance.preparing"), "maintenance": true}),
                );
            };
            permit
        }
    };
    modules.store.admitted(next.run(req)).await
}

/// Supervisor control channel, independent of the owner-facing web session.
async fn upgrade_drain_call(st: AppState, action: &'static str, req: Request) -> Response {
    let peer = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0);
    let expected: String = std::fs::read(st.cfg.home.join("updates/control-token.json"))
        .ok()
        .and_then(|b| serde_json::from_slice(&b).ok())
        .unwrap_or_default();
    let actual = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or_default();
    if expected.len() < 32 || actual != expected || !peer.as_ref().is_some_and(is_loopback) {
        return crate::error::text_response(404, "not found");
    }
    match st.domain.upgrade_drain(action).await {
        Ok(v) => json_response(200, &Value::Object(v)),
        Err(e) => crate::error::ApiError::from(e).into_response(),
    }
}

/// healthz, `/auth` (from steve-auth), `/internal/release/drain`, `/api`, static web; `/mcp` in front of all of it.
/// `root`: module routes at the root that read the session without requiring it.
pub fn app(
    st: AppState,
    auth: Router,
    mcp: Option<Router>,
    modules: Router,
    root: Router,
) -> Router {
    let maintenance_state = st.clone();
    let version = st.cfg.version.clone();
    let fallback_state = st.clone();
    let cors_state = st.clone();
    let root = root.layer(middleware::from_fn_with_state(
        st.clone(),
        api::optional_user,
    ));
    // remote Nodes (N2): mounted unless STEVE_NODE_LISTEN=0 or a NO_RESUME copy (wired with the Hub's coordinators)
    let nodes = st.nodes.clone();
    let (base, modules) = match nodes {
        Some(svc) => (
            crate::nodes::node_router(svc.clone()),
            modules.merge(crate::nodes::api_router(svc)),
        ),
        None => (Router::new(), modules.merge(crate::nodes::off_router())),
    };
    base.merge(root)
        // `version` is the Hub (ops/release.sh waits for it after a restart); the web build is released on its own
        .route("/healthz", get({
            let cfg = st.cfg.clone();
            let store = st.modules.as_ref().map(|m| m.store.clone());
            let (local, remote) = (st.node.clone(), st.nodes.is_some());
            move || async move {
                // the outbox (owed / dead deliveries) is information; `ok` stays the liveness answer.
                // `drain`: the durable maintenance drain is on (business answers 503) and a release script
                // must lift it. Not `maintenance`, which only the supervisor's candidate gate reports.
                // `node`: the Node path for rollout and rollback decisions (node_runtime::health)
                let drain = store.as_ref().map(|s| s.maintenance());
                let (outbox, node) = match store {
                    Some(s) => tokio::task::spawn_blocking(move || {
                        let node = crate::wire::node_runtime::health(&s, local.as_deref(), remote)
                            .inspect_err(|e| tracing::warn!("healthz node: {e}"))
                            .ok();
                        (s.reader(steve_store::outbox::stats).ok(), node)
                    })
                    .await
                    .unwrap_or_default(),
                    None => (None, None),
                };
                json_response(200, &json!({ "ok": true, "version": version, "build_commit": crate::build_identity().map(|(_, commit)| commit), "web_version": crate::web::web_version(&cfg), "drain": drain, "outbox": outbox, "node": node, "recovery": crate::recovery::status() }))
            }
        }))
        .nest("/auth", auth.merge(crate::access::auth_routes(st.clone())))
        .route(
            "/internal/release/drain",
            post({
                let st = st.clone();
                move |req: Request| drain_call(st, true, req)
            })
            .get({
                let st = st.clone();
                move |req: Request| drain_call(st, false, req)
            }),
        )
        .route("/internal/upgrade/drain", post({ let st=st.clone(); move |req: Request| upgrade_drain_call(st,"start",req) })
            .get({ let st=st.clone(); move |req: Request| upgrade_drain_call(st,"status",req) })
            .delete({ let st=st.clone(); move |req: Request| upgrade_drain_call(st,"cancel",req) }))
        .nest("/api", api::router(st, modules))
        .fallback(move |req: Request| {
            let st = fallback_state.clone();
            async move { crate::web::serve(&st.cfg, req).await }
        })
        .layer(middleware::from_fn_with_state(mcp, mcp_gate))
        .layer(middleware::from_fn_with_state(maintenance_state, maintenance_gate))
        .layer(compression())
        .layer(middleware::from_fn(vary_encoding))
        .layer(middleware::from_fn(slow_requests))
        .layer(middleware::from_fn(locale))
        // outermost: a cross-origin web client's preflight is answered before anything asks for a login
        .layer(middleware::from_fn_with_state(cors_state, crate::access::cors))
}

/// The Hub's cookies under names of its own, as it is served on `port`. A browser keeps cookies by host and not by
/// port: two Hubs on one machine would read each other's sign-in and overwrite it. So a cookie this Hub sets
/// leaves with the port after its name (`steve_client_8440`), and comes back under the name the code reads. A
/// `steve_…` cookie without this port is another Hub's, and is not passed on.
pub fn own_cookies(app: Router, port: u16) -> Router {
    app.layer(middleware::from_fn(move |req: Request, next: Next| {
        port_cookies(port, req, next)
    }))
}

async fn port_cookies(port: u16, mut req: Request, next: Next) -> Response {
    let suffix = format!("_{port}");
    if req.headers().contains_key(header::COOKIE) {
        let kept = cookies_in(
            req.headers()
                .get_all(header::COOKIE)
                .iter()
                .filter_map(|v| v.to_str().ok()),
            &suffix,
        );
        req.headers_mut().remove(header::COOKIE);
        if let Some(v) = kept.and_then(|k| k.parse().ok()) {
            req.headers_mut().insert(header::COOKIE, v);
        }
    }
    let mut res = next.run(req).await;
    if res.headers().contains_key(header::SET_COOKIE) {
        let set: Vec<_> = res
            .headers()
            .get_all(header::SET_COOKIE)
            .iter()
            .cloned()
            .collect();
        res.headers_mut().remove(header::SET_COOKIE);
        for v in set {
            let named = v
                .to_str()
                .ok()
                .and_then(|c| cookie_out(c, &suffix))
                .and_then(|c| c.parse().ok());
            res.headers_mut()
                .append(header::SET_COOKIE, named.unwrap_or(v));
        }
    }
    res
}

/// The `Cookie` header as the Hub reads it: its own cookies without the suffix, other Hubs' left out.
fn cookies_in<'a>(sent: impl Iterator<Item = &'a str>, suffix: &str) -> Option<String> {
    let kept: Vec<String> = sent
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| {
            let kv = kv.trim();
            let (name, value) = kv.split_once('=')?;
            if !name.starts_with("steve_") {
                return Some(kv.to_string());
            }
            Some(format!("{}={value}", name.strip_suffix(suffix)?))
        })
        .collect();
    (!kept.is_empty()).then(|| kept.join("; "))
}

/// A `Set-Cookie` value with the suffix after the name; `None` for a cookie that is not the Hub's.
fn cookie_out(set: &str, suffix: &str) -> Option<String> {
    let (name, rest) = set.split_once('=')?;
    name.starts_with("steve_")
        .then(|| format!("{name}{suffix}={rest}"))
}

/// The language of the text this request answers with (steve-i18n): `?lang=` (the SSE stream, which can't set
/// a header), else `Accept-Language`, else zh-CN. `/mcp` is always zh-CN: what it answers goes to a model.
async fn locale(req: Request, next: Next) -> Response {
    let path = req.uri().path();
    let lang = if path.starts_with("/mcp") {
        steve_i18n::ZH_CN
    } else {
        let query = req.uri().query().unwrap_or_default();
        query
            .split('&')
            .find_map(|kv| kv.strip_prefix("lang="))
            .and_then(steve_i18n::match_tag)
            .unwrap_or_else(|| {
                req.headers()
                    .get(header::ACCEPT_LANGUAGE)
                    .and_then(|v| v.to_str().ok())
                    .map_or(steve_i18n::ZH_CN, steve_i18n::from_accept_language)
            })
    };
    steve_i18n::scope(lang, next.run(req)).await
}

/// Every compressible answer varies by `Accept-Encoding`, also the ones sent as is (a HEAD, a small body), so a
/// cache never hands a gzip body to a client that can't read it or the other way round.
async fn vary_encoding(req: Request, next: Next) -> Response {
    let mut res = next.run(req).await;
    let h = res.headers();
    let varies = h.get_all(header::VARY).iter().any(|v| {
        v.to_str()
            .is_ok_and(|v| v.to_ascii_lowercase().contains("accept-encoding"))
    });
    if !varies && compressible(h) {
        res.headers_mut().append(
            header::VARY,
            header::HeaderValue::from_static("accept-encoding"),
        );
    }
    res
}

/// Content types worth gzipping (read whole, text-like).
fn compressible(h: &axum::http::HeaderMap) -> bool {
    let ct = h
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    matches!(
        ct.split(';').next().unwrap_or("").trim(),
        "application/json"
            | "text/html"
            | "text/css"
            | "text/javascript"
            | "application/javascript"
            | "image/svg+xml"
            | "application/wasm"
            | "application/manifest+json"
    )
}

/// A request slower than this (time to the response head) is logged with its route and status.
const SLOW_MS: u128 = 300;

/// Logs API calls that answer slowly, so "Steve feels slow" can be traced to a route without a profiler.
/// Streams count until their head is sent; assets are not timed.
async fn slow_requests(req: Request, next: Next) -> Response {
    let path = req.uri().path().to_string();
    if !path.starts_with("/api/") && !path.starts_with("/auth/") {
        return next.run(req).await;
    }
    let method = req.method().clone();
    let t0 = std::time::Instant::now();
    let res = next.run(req).await;
    let ms = t0.elapsed().as_millis();
    if ms >= SLOW_MS {
        tracing::warn!(%method, path = %path, status = res.status().as_u16(), ms, "slow request");
    }
    res
}

/// gzip for what compresses well and is read whole: the web build (scripts, styles, html, svg, wasm) and JSON
/// answers above 1 KiB. Event streams, media and file downloads are left alone (opt-in by content type), so SSE
/// stays unbuffered and ranges keep working. The bundle goes from ~1.8 MB to ~0.56 MB on the wire.
fn compression()
-> tower_http::compression::CompressionLayer<impl tower_http::compression::Predicate> {
    use tower_http::compression::predicate::{Predicate, SizeAbove};
    fn text(
        _: axum::http::StatusCode,
        _: axum::http::Version,
        h: &axum::http::HeaderMap,
        _: &axum::http::Extensions,
    ) -> bool {
        compressible(h)
    }
    tower_http::compression::CompressionLayer::new()
        .gzip(true)
        .compress_when(SizeAbove::new(1024).and(text as fn(_, _, &_, &_) -> bool))
}

/// A boot step whose backend method the integrator hasn't wired yet is skipped with a warning; any other
/// failure stops the Hub like a throw at import time in TS.
fn boot_step(what: &str, r: Result<(), BackendError>) -> anyhow::Result<()> {
    match r {
        Ok(()) => Ok(()),
        Err(e) if e.message.starts_with(crate::backend::UNIMPLEMENTED) => {
            tracing::warn!("{what}: {e}");
            Ok(())
        }
        Err(e) => Err(anyhow::anyhow!("{what}: {e}")),
    }
}

/// `ensureWorkspace(); seed(); registerHub(); recover(); watchReleases(); housekeeping()` + hourly.
pub async fn boot(st: &AppState) -> anyhow::Result<()> {
    let b = st.backend.clone();
    let r =
        tokio::task::spawn_blocking(move || b.writer().and_then(|w| w.ensure_workspace())).await?;
    boot_step("ensure workspace", r)?;
    boot_step("seed", st.domain.seed().await)?;
    // registerHub runs scutil/sw_vers/sysctl: in the background, so the Hub listens at once
    let dev = st.clone();
    tokio::spawn(async move {
        let (name, os, version) = crate::system::hub_device(&dev.cfg.version).await;
        let b = dev.backend.clone();
        let r = tokio::task::spawn_blocking(move || {
            b.writer()
                .and_then(|w| crate::system::register_hub(w.as_ref(), &name, &os, &version))
        })
        .await;
        match r {
            Ok(Ok(())) => {
                // the Hub device is “我”'s (where “我” exists; otherwise it is
                // linked when “我” is created)
                if let Some(m) = dev.modules.clone() {
                    let r = tokio::task::spawn_blocking(move || {
                        m.store.transaction(|tx| {
                            steve_modules::contacts::stewards::adopt_everywhere(tx, "device", "hub")
                        })
                    })
                    .await;
                    if let Ok(Err(e)) = r {
                        tracing::warn!("hub device 负责人: {e}");
                    }
                }
            }
            Ok(Err(e)) => tracing::error!("register hub: {e}"),
            Err(e) => tracing::error!("register hub: {e}"),
        }
    });
    // recover() reports a finished release first, as in TS
    boot_step("recover", st.domain.recover().await)?;
    if !st.domain.spawn_background() {
        crate::jobs::spawn_release_watcher(st.clone(), st.cfg.release_file());
        crate::jobs::spawn_housekeeping(st.clone());
    }
    // closings waiting for something the turn-end callback may never report (queued turns, the hold)
    crate::jobs::spawn_lifecycle_tick(st.clone());
    // warm the runtimes cache so the first /api/state doesn't wait for `claude --version`
    let rt = st.clone();
    tokio::spawn(async move {
        rt.runtimes.get(false).await;
    });
    Ok(())
}

#[cfg(test)]
mod cookie_tests {
    use axum::Router;
    use axum::body::Body;
    use axum::http::{HeaderMap, Request, header};
    use axum::routing::get;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    #[test]
    fn a_cookie_is_the_hubs_by_the_port_after_its_name() {
        let read = |sent: &[&str]| super::cookies_in(sent.iter().copied(), "_8440");
        assert_eq!(
            read(&["theme=dark; steve_client_8440=a; steve_ws_8440=w"]).as_deref(),
            Some("theme=dark; steve_client=a; steve_ws=w")
        );
        // another Hub on this host, a name from before the ports, a port that only ends alike
        assert_eq!(
            read(&[
                "steve_client_9000=b; steve_client=old",
                "steve_session_18440=c"
            ]),
            None
        );
        assert_eq!(
            read(&["steve_client_9000=b", "steve_session_8440=s=1"]).as_deref(),
            Some("steve_session=s=1")
        );
        assert_eq!(
            super::cookie_out("steve_client=x; Max-Age=1; Path=/; HttpOnly", "_8440").as_deref(),
            Some("steve_client_8440=x; Max-Age=1; Path=/; HttpOnly")
        );
        assert_eq!(
            super::cookie_out("sidebar_state=true; Path=/", "_8440"),
            None
        );
    }

    #[tokio::test]
    async fn two_hubs_on_one_host_keep_their_own_cookies() {
        let hub = |port: u16| {
            let app = Router::new().route(
                "/",
                get(|h: HeaderMap| async move {
                    let sent = h
                        .get(header::COOKIE)
                        .and_then(|v| v.to_str().ok())
                        .unwrap_or_default()
                        .to_string();
                    (
                        axum::response::AppendHeaders([
                            (header::SET_COOKIE, "steve_client=new; Path=/; HttpOnly"),
                            (header::SET_COOKIE, "other=1; Path=/"),
                        ]),
                        sent,
                    )
                }),
            );
            super::own_cookies(app, port)
        };
        // what a browser holds for the host after signing in to both
        let jar = "steve_client_8440=a; steve_client_9000=b; other=1";
        for (port, mine) in [(8440, "a"), (9000, "b")] {
            let r = hub(port)
                .oneshot(
                    Request::get("/")
                        .header(header::COOKIE, jar)
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let set: Vec<String> = r
                .headers()
                .get_all(header::SET_COOKIE)
                .iter()
                .map(|v| v.to_str().unwrap().to_string())
                .collect();
            assert_eq!(
                set,
                [
                    format!("steve_client_{port}=new; Path=/; HttpOnly"),
                    "other=1; Path=/".to_string()
                ]
            );
            let body = r.into_body().collect().await.unwrap().to_bytes();
            assert_eq!(body, format!("steve_client={mine}; other=1"));
        }
    }
}

#[cfg(test)]
mod compression_tests {
    use axum::Router;
    use axum::body::Body;
    use axum::http::{Request, header};
    use axum::routing::get;
    use http_body_util::BodyExt;
    use std::io::Read;
    use tower::ServiceExt;

    fn router() -> Router {
        let big = "x".repeat(4096);
        let (a, b, c, d) = (big.clone(), big.clone(), big.clone(), big);
        Router::new()
            .route(
                "/json",
                get(move || async move { ([(header::CONTENT_TYPE, "application/json")], a) }),
            )
            .route(
                "/js",
                get(move || async move {
                    (
                        [(header::CONTENT_TYPE, "text/javascript; charset=utf-8")],
                        b,
                    )
                }),
            )
            .route(
                "/sse",
                get(move || async move { ([(header::CONTENT_TYPE, "text/event-stream")], c) }),
            )
            .route(
                "/png",
                get(move || async move { ([(header::CONTENT_TYPE, "image/png")], d) }),
            )
            .route(
                "/small",
                get(|| async { ([(header::CONTENT_TYPE, "application/json")], "{}") }),
            )
            .layer(super::compression())
            .layer(axum::middleware::from_fn(super::vary_encoding))
    }

    async fn get_gz(path: &str) -> (Option<String>, Vec<u8>) {
        let r = router()
            .oneshot(
                Request::get(path)
                    .header(header::ACCEPT_ENCODING, "gzip, br")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        let enc = r
            .headers()
            .get(header::CONTENT_ENCODING)
            .map(|v| v.to_str().unwrap().to_string());
        (
            enc,
            r.into_body().collect().await.unwrap().to_bytes().to_vec(),
        )
    }

    #[tokio::test]
    async fn text_is_gzipped_streams_and_media_are_not() {
        for p in ["/json", "/js"] {
            let (enc, body) = get_gz(p).await;
            assert_eq!(enc.as_deref(), Some("gzip"), "{p}");
            let mut out = String::new();
            flate2::read::GzDecoder::new(&body[..])
                .read_to_string(&mut out)
                .unwrap();
            assert_eq!(out, "x".repeat(4096));
        }
        for p in ["/sse", "/png", "/small"] {
            let (enc, _) = get_gz(p).await;
            assert_eq!(enc, None, "{p}");
        }
        // a HEAD of the same script is sent as is but varies like the GET; a stream does not
        for (p, vary) in [
            ("/js", true),
            ("/small", true),
            ("/sse", false),
            ("/png", false),
        ] {
            let r = router()
                .oneshot(
                    Request::head(p)
                        .header(header::ACCEPT_ENCODING, "gzip")
                        .body(Body::empty())
                        .unwrap(),
                )
                .await
                .unwrap();
            let v: Vec<_> = r.headers().get_all(header::VARY).iter().collect();
            assert_eq!(v.len(), usize::from(vary), "{p} {v:?}");
        }
        let r = router()
            .oneshot(
                Request::get("/js")
                    .header(header::ACCEPT_ENCODING, "gzip")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            r.headers().get_all(header::VARY).iter().count(),
            1,
            "one Vary on a gzip answer"
        );
    }
}

#[cfg(test)]
mod loopback_tests {
    use super::*;

    #[test]
    fn only_real_loopback_peers() {
        let is = |ip: &str| is_loopback(&SocketAddr::new(ip.parse().expect("ip"), 1));
        for ip in ["127.0.0.1", "127.0.0.2", "::1", "::ffff:127.0.0.1"] {
            assert!(is(ip), "{ip}");
        }
        for ip in ["10.0.0.1", "::1:0:0:1", "::1234:5678", "::ffff:10.0.0.1"] {
            assert!(!is(ip), "{ip}");
        }
    }
}
