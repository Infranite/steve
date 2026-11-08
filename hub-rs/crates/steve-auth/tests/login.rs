//! Login flows against a tiny in-process OIDC provider (never the real IdP).

use std::{
    collections::HashMap,
    net::SocketAddr,
    sync::{Arc, Mutex},
};

use axum::{
    Form, Json, Router,
    body::{Body, to_bytes},
    extract::{State, connect_info::MockConnectInfo},
    http::{HeaderMap, Request, StatusCode, header},
    middleware,
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::Engine;
use openidconnect::{
    Audience, EmptyAdditionalClaims, EndUserName, EndUserUsername, IssuerUrl, JsonWebKeyId,
    LocalizedClaim, Nonce, PkceCodeChallenge, PkceCodeVerifier, PrivateSigningKey, StandardClaims,
    SubjectIdentifier,
    core::{
        CoreIdToken, CoreIdTokenClaims, CoreJsonWebKeySet, CoreJwsSigningAlgorithm,
        CoreRsaPrivateSigningKey,
    },
};
use serde_json::{Value, json};
use steve_auth::{Auth, AuthConfig, CurrentUser, OidcConfig, UserStore, require_user};
use tower::ServiceExt;

// ---- in-memory store -------------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq)]
struct User {
    id: String,
    subject: String,
    name: String,
    email: Option<String>,
    role: String,
}

#[derive(Default)]
struct Mem {
    users: Mutex<Vec<User>>,
    sessions: Mutex<HashMap<String, String>>,
    audit: Mutex<Vec<String>>,
}

#[derive(Clone, Default)]
struct MemStore(Arc<Mem>);

impl UserStore for MemStore {
    type User = User;
    type Error = std::convert::Infallible;
    fn user_id_by_subject(&self, subject: &str) -> Result<Option<String>, Self::Error> {
        Ok(self
            .0
            .users
            .lock()
            .unwrap()
            .iter()
            .find(|u| u.subject == subject)
            .map(|u| u.id.clone()))
    }
    fn user_count(&self) -> Result<i64, Self::Error> {
        Ok(self.0.users.lock().unwrap().len() as i64)
    }
    fn create_owner(
        &self,
        subject: &str,
        name: &str,
        email: Option<&str>,
    ) -> Result<String, Self::Error> {
        let mut users = self.0.users.lock().unwrap();
        let id = format!("usr_{}", users.len() + 1);
        users.push(User {
            id: id.clone(),
            subject: subject.into(),
            name: name.into(),
            email: email.map(Into::into),
            role: "owner".into(),
        });
        self.0
            .audit
            .lock()
            .unwrap()
            .push(format!("{id} user.create {id} {{\"role\":\"owner\"}}"));
        Ok(id)
    }
    fn create_session(&self, user_id: &str, token: &str, ttl_ms: i64) -> Result<(), Self::Error> {
        assert_eq!(ttl_ms, 30 * 24 * 3600 * 1000);
        self.0
            .sessions
            .lock()
            .unwrap()
            .insert(token.into(), user_id.into());
        Ok(())
    }
    fn session_user(&self, token: &str) -> Result<Option<User>, Self::Error> {
        let uid = self.0.sessions.lock().unwrap().get(token).cloned();
        Ok(uid.and_then(|id| {
            self.0
                .users
                .lock()
                .unwrap()
                .iter()
                .find(|u| u.id == id)
                .cloned()
        }))
    }
    fn drop_session(&self, token: &str) -> Result<(), Self::Error> {
        self.0.sessions.lock().unwrap().remove(token);
        Ok(())
    }
}

// ---- fake OIDC provider ----------------------------------------------------------------------

const CLIENT_ID: &str = "steve";
const CLIENT_SECRET: &str = "s3cret";

struct Grant {
    challenge: String,
    redirect_uri: String,
    nonce: String,
    sub: String,
    name: Option<String>,
}

struct Provider {
    issuer: String,
    key: CoreRsaPrivateSigningKey,
    grants: Mutex<HashMap<String, Grant>>,
    /// Sign ID tokens with this nonce instead of the requested one.
    wrong_nonce: Mutex<bool>,
}

async fn discovery(State(p): State<Arc<Provider>>) -> Json<Value> {
    Json(json!({
        "issuer": p.issuer,
        "authorization_endpoint": p.endpoint("authorize"),
        "token_endpoint": p.endpoint("token"),
        "jwks_uri": p.endpoint("jwks"),
        "response_types_supported": ["code"],
        "subject_types_supported": ["public"],
        "id_token_signing_alg_values_supported": ["RS256"],
        "code_challenge_methods_supported": ["S256"],
    }))
}

async fn jwks(State(p): State<Arc<Provider>>) -> Json<CoreJsonWebKeySet> {
    Json(CoreJsonWebKeySet::new(vec![p.key.as_verification_key()]))
}

async fn token(
    State(p): State<Arc<Provider>>,
    headers: HeaderMap,
    Form(f): Form<HashMap<String, String>>,
) -> Response {
    let basic = format!(
        "Basic {}",
        base64::engine::general_purpose::STANDARD.encode(format!("{CLIENT_ID}:{CLIENT_SECRET}"))
    );
    if headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        != Some(basic.as_str())
    {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error": "invalid_client"})),
        )
            .into_response();
    }
    let bad = |d: &str| {
        (
            StatusCode::BAD_REQUEST,
            Json(json!({"error": "invalid_grant", "error_description": d})),
        )
            .into_response()
    };
    let Some(g) = f
        .get("code")
        .and_then(|c| p.grants.lock().unwrap().remove(c))
    else {
        return bad("unknown code");
    };
    if f.get("grant_type").map(String::as_str) != Some("authorization_code") {
        return bad("grant_type");
    }
    if f.get("redirect_uri") != Some(&g.redirect_uri) {
        return bad("redirect_uri");
    }
    let verifier = PkceCodeVerifier::new(f.get("code_verifier").cloned().unwrap_or_default());
    if PkceCodeChallenge::from_code_verifier_sha256(&verifier).as_str() != g.challenge {
        return bad("pkce");
    }
    let nonce = if *p.wrong_nonce.lock().unwrap() {
        "other".to_owned()
    } else {
        g.nonce
    };
    let now = chrono::Utc::now();
    let mut std = StandardClaims::new(SubjectIdentifier::new(g.sub.clone()))
        .set_preferred_username(Some(EndUserUsername::new(format!("{}-login", g.sub))))
        .set_email(Some(openidconnect::EndUserEmail::new(format!(
            "{}@lab.test",
            g.sub
        ))));
    if let Some(n) = g.name {
        let mut l = LocalizedClaim::new();
        l.insert(None, EndUserName::new(n));
        std = std.set_name(Some(l));
    }
    let claims = CoreIdTokenClaims::new(
        IssuerUrl::new(p.issuer.clone()).unwrap(),
        vec![Audience::new(CLIENT_ID.into())],
        now + chrono::Duration::minutes(5),
        now,
        std,
        EmptyAdditionalClaims {},
    )
    .set_nonce(Some(Nonce::new(nonce)));
    let id_token = CoreIdToken::new(
        claims,
        &p.key,
        CoreJwsSigningAlgorithm::RsaSsaPkcs1V15Sha256,
        None,
        None,
    )
    .unwrap();
    Json(json!({"access_token": "at", "token_type": "Bearer", "expires_in": 300, "id_token": id_token.to_string()})).into_response()
}

async fn start_provider() -> Arc<Provider> {
    start_provider_at("").await
}

async fn start_provider_at(path: &str) -> Arc<Provider> {
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let issuer = format!("http://{}{path}", l.local_addr().unwrap());
    let key = CoreRsaPrivateSigningKey::from_pem(
        include_str!("fixtures/test_rsa_key.pem"),
        Some(JsonWebKeyId::new("k1".into())),
    )
    .unwrap();
    let p = Arc::new(Provider {
        issuer,
        key,
        grants: Mutex::default(),
        wrong_nonce: Mutex::new(false),
    });
    let prefix = path.trim_end_matches('/');
    let app = Router::new()
        .route(
            &format!("{prefix}/.well-known/openid-configuration"),
            get(discovery),
        )
        .route(&format!("{prefix}/jwks"), get(jwks))
        .route(&format!("{prefix}/token"), post(token))
        .with_state(p.clone());
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    p
}

impl Provider {
    fn endpoint(&self, name: &str) -> String {
        format!("{}/{name}", self.issuer.trim_end_matches('/'))
    }

    /// What the IdP does when the user approves: checks the request and redirects back with a code.
    fn approve(&self, authorize_url: &str, sub: &str, name: Option<&str>) -> String {
        let url = url::Url::parse(authorize_url).unwrap();
        assert!(
            authorize_url.starts_with(&format!("{}?", self.endpoint("authorize"))),
            "{authorize_url}"
        );
        let q: HashMap<_, _> = url.query_pairs().into_owned().collect();
        assert_eq!(q["response_type"], "code");
        assert_eq!(q["client_id"], CLIENT_ID);
        assert_eq!(q["scope"], "openid profile");
        assert_eq!(q["code_challenge_method"], "S256");
        assert_eq!(q["state"].len(), 43, "32 random bytes, base64url");
        assert_eq!(q["nonce"].len(), 43);
        let code = format!("code-{sub}");
        self.grants.lock().unwrap().insert(
            code.clone(),
            Grant {
                challenge: q["code_challenge"].clone(),
                redirect_uri: q["redirect_uri"].clone(),
                nonce: q["nonce"].clone(),
                sub: sub.into(),
                name: name.map(Into::into),
            },
        );
        let state = url::form_urlencoded::byte_serialize(q["state"].as_bytes()).collect::<String>();
        format!("{}?code={code}&state={state}", q["redirect_uri"])
    }
}

// ---- the Steve side --------------------------------------------------------------------------

fn config(issuer: Option<&str>, dev: bool) -> AuthConfig {
    AuthConfig::new(
        Some("https://steve.example.test"),
        Some("https://steve.example.test,http://steve.home:8440"),
        OidcConfig {
            issuer: issuer.map(Into::into),
            client_id: Some(CLIENT_ID.into()),
            client_secret: Some(CLIENT_SECRET.into()),
        },
        dev,
    )
}

fn app(auth: &Auth<MemStore>, peer: &str) -> Router {
    let api = Router::new()
        .route(
            "/me",
            get(|CurrentUser(u): CurrentUser<MemStore>| async move { u.name }),
        )
        .layer(middleware::from_fn_with_state(
            auth.clone(),
            require_user::<MemStore>,
        ))
        .with_state(auth.clone());
    // the extractor alone, with Auth as router state
    let direct = Router::new()
        .route(
            "/whoami",
            get(|CurrentUser(u): CurrentUser<MemStore>| async move { u.id }),
        )
        .with_state(auth.clone());
    Router::new()
        .nest("/auth", auth.router())
        .nest("/api", api)
        .merge(direct)
        .layer(MockConnectInfo(peer.parse::<SocketAddr>().unwrap()))
}

struct Res {
    status: StatusCode,
    headers: HeaderMap,
    body: String,
}

impl Res {
    fn cookies(&self) -> Vec<String> {
        self.headers
            .get_all(header::SET_COOKIE)
            .iter()
            .map(|v| v.to_str().unwrap().to_owned())
            .collect()
    }
    fn location(&self) -> &str {
        self.headers
            .get(header::LOCATION)
            .unwrap()
            .to_str()
            .unwrap()
    }
}

async fn send(app: &Router, method: &str, uri: &str, headers: &[(&str, &str)]) -> Res {
    let mut req = Request::builder().method(method).uri(uri);
    for (k, v) in headers {
        req = req.header(*k, *v);
    }
    let res = app
        .clone()
        .oneshot(req.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let (status, headers) = (res.status(), res.headers().clone());
    let body =
        String::from_utf8(to_bytes(res.into_body(), 1 << 20).await.unwrap().to_vec()).unwrap();
    Res {
        status,
        headers,
        body,
    }
}

/// `name=value` of a `Set-Cookie` line.
fn pair(set_cookie: &str) -> &str {
    set_cookie.split(';').next().unwrap()
}

/// Runs /auth/login → IdP → /auth/callback; returns the callback response.
async fn login_as(app: &Router, p: &Provider, host: &str, sub: &str, name: Option<&str>) -> Res {
    let r = send(
        app,
        "GET",
        "/auth/login?back=%2Fitems%2FST-1",
        &[("host", host)],
    )
    .await;
    assert_eq!(r.status, StatusCode::FOUND, "{}", r.body);
    let login_cookie = r.cookies()[0].clone();
    let back = p.approve(r.location(), sub, name);
    let path = back.split_once("/auth/callback").unwrap().1;
    send(
        app,
        "GET",
        &format!("/auth/callback{path}"),
        &[("host", host), ("cookie", pair(&login_cookie))],
    )
    .await
}

#[tokio::test]
async fn issuer_paths_and_trailing_slashes_are_preserved_and_must_match_exactly() {
    for path in ["/", "/tenant/steve", "/tenant/steve/"] {
        let p = start_provider_at(path).await;
        let store = MemStore::default();
        let auth = Auth::new(store.clone(), config(Some(&p.issuer), false)).unwrap();
        let r = login_as(
            &app(&auth, "127.0.0.1:1"),
            &p,
            "steve.home:8440",
            "alice",
            None,
        )
        .await;
        assert_eq!(r.status, StatusCode::FOUND, "{path}: {}", r.body);
        assert_eq!(r.location(), "/items/ST-1");
        assert_eq!(store.0.users.lock().unwrap()[0].role, "owner");
        assert_eq!(store.0.sessions.lock().unwrap().len(), 1);

        if path == "/" {
            continue;
        }
        // Both spellings request the same discovery URL; only the exact issuer
        // identifier differs. A new client must still refuse that mismatch.
        let mismatched = match p.issuer.strip_suffix('/') {
            Some(without_slash) => without_slash.to_owned(),
            None => format!("{}/", p.issuer),
        };
        let rejected = MemStore::default();
        let auth = Auth::new(rejected.clone(), config(Some(&mismatched), false)).unwrap();
        let r = send(&app(&auth, "127.0.0.1:1"), "GET", "/auth/login", &[]).await;
        assert_eq!(r.status, StatusCode::INTERNAL_SERVER_ERROR, "{path}");
        assert!(!r.headers.contains_key(header::LOCATION));
        assert!(r.cookies().is_empty());
        assert!(rejected.0.users.lock().unwrap().is_empty());
        assert!(rejected.0.sessions.lock().unwrap().is_empty());
    }
}

#[tokio::test]
async fn first_subject_becomes_owner() {
    let p = start_provider().await;
    let store = MemStore::default();
    let auth = Auth::new(store.clone(), config(Some(&p.issuer), false)).unwrap();
    let app = app(&auth, "10.0.0.5:4000");

    let r = send(&app, "GET", "/api/me", &[]).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    assert_eq!(
        r.body,
        r#"{"error":"unauthenticated","login":"/auth/login"}"#
    );

    // login from the second origin: the redirect_uri and the way back use it
    let r = send(&app, "GET", "/auth/login", &[("host", "Steve.Home:8440")]).await;
    assert_eq!(r.status, StatusCode::FOUND);
    let c = r.cookies();
    assert_eq!(c.len(), 1);
    assert!(c[0].starts_with("steve_login="));
    assert!(
        c[0].ends_with("; Max-Age=600; Path=/auth; HttpOnly; Secure; SameSite=Lax"),
        "{}",
        c[0]
    );
    let q: HashMap<_, _> = url::Url::parse(r.location())
        .unwrap()
        .query_pairs()
        .into_owned()
        .collect();
    assert_eq!(q["redirect_uri"], "http://steve.home:8440/auth/callback");
    // x-forwarded-host wins over host; unknown hosts fall back to the first origin
    let r = send(
        &app,
        "GET",
        "/auth/login",
        &[
            ("host", "steve.home:8440"),
            ("x-forwarded-host", "evil.test, steve.home:8440"),
        ],
    )
    .await;
    let q: HashMap<_, _> = url::Url::parse(r.location())
        .unwrap()
        .query_pairs()
        .into_owned()
        .collect();
    assert_eq!(
        q["redirect_uri"],
        "https://steve.example.test/auth/callback"
    );

    let r = login_as(&app, &p, "steve.home:8440", "alice", Some("Alice")).await;
    assert_eq!(r.status, StatusCode::FOUND, "{}", r.body);
    assert_eq!(r.location(), "/items/ST-1");
    let c = r.cookies();
    assert_eq!(c[0], "steve_login=; Max-Age=0; Path=/auth");
    assert!(c[1].starts_with("steve_session="));
    assert!(
        c[1].ends_with("; Max-Age=2592000; Path=/; HttpOnly; Secure; SameSite=Lax"),
        "{}",
        c[1]
    );
    let session = pair(&c[1]).to_owned();
    assert_eq!(session.len() - "steve_session=".len(), 43);

    let users = store.0.users.lock().unwrap().clone();
    assert_eq!(
        users,
        [User {
            id: "usr_1".into(),
            subject: "alice".into(),
            name: "Alice".into(),
            email: Some("alice@lab.test".into()),
            role: "owner".into()
        }]
    );
    assert_eq!(
        store.0.audit.lock().unwrap().as_slice(),
        [r#"usr_1 user.create usr_1 {"role":"owner"}"#]
    );

    let r = send(
        &app,
        "GET",
        "/api/me",
        &[("cookie", &format!("a=b; {session}"))],
    )
    .await;
    assert_eq!((r.status, r.body.as_str()), (StatusCode::OK, "Alice"));
    let r = send(&app, "GET", "/whoami", &[("cookie", &session)]).await;
    assert_eq!((r.status, r.body.as_str()), (StatusCode::OK, "usr_1"));

    // same subject again: same user, a second session
    let r = login_as(&app, &p, "steve.example.test", "alice", Some("Alice")).await;
    assert_eq!(r.status, StatusCode::FOUND);
    assert_eq!(store.0.users.lock().unwrap().len(), 1);
    assert_eq!(store.0.sessions.lock().unwrap().len(), 2);

    // anyone else is refused
    let r = login_as(&app, &p, "steve.example.test", "mallory", None).await;
    assert_eq!(
        (r.status, r.body.as_str()),
        (StatusCode::FORBIDDEN, "这个账号还没有被邀请使用 Steve")
    );
    assert_eq!(r.cookies(), ["steve_login=; Max-Age=0; Path=/auth"]);
    assert_eq!(store.0.users.lock().unwrap().len(), 1);

    // logout drops the session
    let r = send(&app, "POST", "/auth/logout", &[("cookie", &session)]).await;
    assert_eq!(
        (r.status, r.body.as_str()),
        (StatusCode::OK, r#"{"ok":true}"#)
    );
    assert_eq!(r.cookies(), ["steve_session=; Max-Age=0; Path=/"]);
    let r = send(&app, "GET", "/api/me", &[("cookie", &session)]).await;
    assert_eq!(r.status, StatusCode::UNAUTHORIZED);
    let r = send(&app, "POST", "/auth/logout", &[]).await;
    assert_eq!(r.status, StatusCode::OK);
}

#[tokio::test]
async fn name_falls_back_to_preferred_username() {
    let p = start_provider().await;
    let store = MemStore::default();
    let auth = Auth::new(store.clone(), config(Some(&p.issuer), false)).unwrap();
    let r = login_as(&app(&auth, "10.0.0.5:1"), &p, "x", "bob", None).await;
    assert_eq!(r.status, StatusCode::FOUND);
    assert_eq!(r.location(), "/items/ST-1");
    assert_eq!(store.0.users.lock().unwrap()[0].name, "bob-login");
}

#[tokio::test]
async fn bad_callbacks() {
    let p = start_provider().await;
    let store = MemStore::default();
    let auth = Auth::new(store.clone(), config(Some(&p.issuer), false)).unwrap();
    let app = app(&auth, "10.0.0.5:1");
    let expired = "登录已过期，请重新登录";

    // no login cookie, or an unknown one
    let r = send(&app, "GET", "/auth/callback?code=x&state=y", &[]).await;
    assert_eq!(
        (r.status, r.body.as_str()),
        (StatusCode::BAD_REQUEST, expired)
    );
    // Hono's `c.text`, header byte for byte
    assert_eq!(
        r.headers.get("content-type").and_then(|v| v.to_str().ok()),
        Some("text/plain; charset=UTF-8")
    );
    let r = send(
        &app,
        "GET",
        "/auth/callback?code=x&state=y",
        &[("cookie", "steve_login=nope")],
    )
    .await;
    assert_eq!(r.body, expired);
    assert!(r.cookies().is_empty());

    // wrong state: 登录失败, and the login is used up
    let r = send(&app, "GET", "/auth/login", &[]).await;
    let cookie = pair(&r.cookies()[0]).to_owned();
    let back = p.approve(r.location(), "alice", None);
    let tampered = back.replace("state=", "state=x");
    let path = tampered.split_once("/auth/callback").unwrap().1;
    let r = send(
        &app,
        "GET",
        &format!("/auth/callback{path}"),
        &[("cookie", &cookie)],
    )
    .await;
    assert_eq!(
        (r.status, r.body.as_str()),
        (StatusCode::BAD_REQUEST, "登录失败")
    );
    assert_eq!(r.cookies(), ["steve_login=; Max-Age=0; Path=/auth"]);
    let path = back.split_once("/auth/callback").unwrap().1;
    let r = send(
        &app,
        "GET",
        &format!("/auth/callback{path}"),
        &[("cookie", &cookie)],
    )
    .await;
    assert_eq!(r.body, expired);

    // the IdP reports an error
    let r = send(&app, "GET", "/auth/login", &[]).await;
    let cookie = pair(&r.cookies()[0]).to_owned();
    let r = send(
        &app,
        "GET",
        "/auth/callback?error=access_denied",
        &[("cookie", &cookie)],
    )
    .await;
    assert_eq!(r.body, "登录失败");

    // an ID token for another login (nonce mismatch)
    *p.wrong_nonce.lock().unwrap() = true;
    let r = login_as(&app, &p, "x", "alice", None).await;
    assert_eq!(
        (r.status, r.body.as_str()),
        (StatusCode::BAD_REQUEST, "登录失败")
    );
    assert!(store.0.users.lock().unwrap().is_empty());
}

#[tokio::test]
async fn back_must_be_a_path() {
    let p = start_provider().await;
    let auth = Auth::new(MemStore::default(), config(Some(&p.issuer), false)).unwrap();
    let app = app(&auth, "10.0.0.5:1");
    let r = send(&app, "GET", "/auth/login?back=https://evil.test/", &[]).await;
    let cookie = pair(&r.cookies()[0]).to_owned();
    let back = p.approve(r.location(), "alice", None);
    let path = back.split_once("/auth/callback").unwrap().1;
    let r = send(
        &app,
        "GET",
        &format!("/auth/callback{path}"),
        &[("cookie", &cookie)],
    )
    .await;
    assert_eq!(r.location(), "/");

    // protocol-relative and backslash forms leave the site too; a real path is kept
    for (back, want) in [
        ("//evil.test/", "/"),
        ("/%5Cevil.test/", "/"),
        ("/%0A//evil.test", "/"),
        ("/items/7?tab=chat", "/items/7?tab=chat"),
    ] {
        let r = send(&app, "GET", &format!("/auth/login?back={back}"), &[]).await;
        let cookie = pair(&r.cookies()[0]).to_owned();
        let cb = p.approve(r.location(), "alice", None);
        let path = cb.split_once("/auth/callback").unwrap().1;
        let r = send(
            &app,
            "GET",
            &format!("/auth/callback{path}"),
            &[("cookie", &cookie)],
        )
        .await;
        assert_eq!(r.location(), want, "{back}");
    }
}

#[tokio::test]
async fn oidc_not_configured() {
    let auth = Auth::new(MemStore::default(), config(None, false)).unwrap();
    let r = send(&app(&auth, "127.0.0.1:1"), "GET", "/auth/login", &[]).await;
    assert_eq!(
        (r.status, r.body.as_str()),
        (StatusCode::INTERNAL_SERVER_ERROR, "Internal Server Error")
    );
    let r = send(&app(&auth, "127.0.0.1:1"), "GET", "/api/me", &[]).await;
    assert_eq!(r.body, r#"{"error":"unauthenticated","login":null}"#);
    // discovery failure is a 500 too (nothing listens there)
    let auth = Auth::new(
        MemStore::default(),
        config(Some("http://127.0.0.1:9"), false),
    )
    .unwrap();
    let r = send(&app(&auth, "127.0.0.1:1"), "GET", "/auth/login", &[]).await;
    assert_eq!(r.status, StatusCode::INTERNAL_SERVER_ERROR);
}

/// What a browser on this machine sends when it talks to the Hub directly.
const LOCAL: &[(&str, &str)] = &[("host", "127.0.0.1:8440")];

#[tokio::test]
async fn dev_login() {
    let store = MemStore::default();
    let off = Auth::new(store.clone(), config(None, false)).unwrap();
    let r = send(&app(&off, "127.0.0.1:1"), "GET", "/auth/dev", LOCAL).await;
    assert_eq!(
        (r.status, r.body.as_str()),
        (StatusCode::NOT_FOUND, "not found")
    );

    let auth = Auth::new(store.clone(), config(None, true)).unwrap();
    let r = send(&app(&auth, "127.0.0.1:1"), "GET", "/api/me", &[]).await;
    assert_eq!(r.body, r#"{"error":"unauthenticated","login":"/auth/dev"}"#);
    let r = send(&app(&auth, "192.168.1.2:1"), "GET", "/auth/dev", LOCAL).await;
    assert_eq!(r.status, StatusCode::NOT_FOUND);

    // a loopback peer is not enough: a proxy on this machine (forwarding headers) or a
    // non-loopback Host (DNS rebinding, a tunnel) means the browser is somewhere else
    for headers in [
        &[][..],
        &[("host", "steve.example.test")][..],
        &[
            ("host", "127.0.0.1:8440"),
            ("x-forwarded-for", "203.0.113.9"),
        ][..],
        &[("host", "localhost:8440"), ("forwarded", "for=203.0.113.9")][..],
        &[
            ("host", "127.0.0.1:8440"),
            ("x-forwarded-host", "steve.example.test"),
        ][..],
    ] {
        let r = send(&app(&auth, "127.0.0.1:1"), "GET", "/auth/dev", headers).await;
        assert_eq!(r.status, StatusCode::NOT_FOUND, "{headers:?}");
    }
    assert!(store.0.users.lock().unwrap().is_empty());

    for peer in ["127.0.0.1:1", "[::1]:1", "[::ffff:127.0.0.1]:1"] {
        let r = send(&app(&auth, peer), "GET", "/auth/dev", LOCAL).await;
        assert_eq!(r.status, StatusCode::FOUND, "{peer}");
        assert_eq!(r.location(), "/");
        let c = r.cookies();
        assert_eq!(c.len(), 1);
        // https public URL → Secure
        assert!(c[0].ends_with("; Max-Age=2592000; Path=/; HttpOnly; Secure; SameSite=Lax"));
        let r = send(
            &app(&auth, peer),
            "GET",
            "/api/me",
            &[("cookie", pair(&c[0]))],
        )
        .await;
        assert_eq!(r.body, "本机");
    }
    assert_eq!(store.0.users.lock().unwrap().len(), 1);
    assert_eq!(store.0.users.lock().unwrap()[0].subject, "dev:local");

    // another owner exists → refused
    let other = MemStore::default();
    other.create_owner("alice", "Alice", None).unwrap();
    let auth = Auth::new(
        other,
        AuthConfig::new(None, None, OidcConfig::default(), true),
    )
    .unwrap();
    let r = send(&app(&auth, "127.0.0.1:1"), "GET", "/auth/dev", LOCAL).await;
    assert_eq!(
        (r.status, r.body.as_str()),
        (StatusCode::FORBIDDEN, "已有所有者，本机登录不可用")
    );
}

#[tokio::test]
async fn http_public_url_means_no_secure_flag() {
    let auth = Auth::new(
        MemStore::default(),
        AuthConfig::new(None, None, OidcConfig::default(), true),
    )
    .unwrap();
    let r = send(&app(&auth, "127.0.0.1:1"), "GET", "/auth/dev", LOCAL).await;
    assert!(r.cookies()[0].ends_with("; Max-Age=2592000; Path=/; HttpOnly; SameSite=Lax"));
}
