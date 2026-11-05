//! Shared by the A2 tests: adapters written the way their slices will (Gmail and Outlook on the fake
//! Gmail/Graph of steve-mail, the Feishu app bot and user OAuth on the fake Feishu of steve-channels,
//! the GitHub App installation on the fake GitHub of steve-github), a harness, and request helpers.

#![allow(dead_code)]

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use axum::Router;
use axum::body::Body;
use axum::http::{HeaderMap, Request, header};
use serde_json::{Value, json};
use steve_modules::Caller;
use steve_modules::accounts::connections::{
    self, AccountInfo, AuthKind, ConnCx, ConnectionProvider, Connections, ConnectionsConfig,
    FieldDef, Minted, OAuthClient, OAuthSpec, Owner, ProviderError, StartCx, TokenKind, TokenStyle,
};
use steve_modules::testkit::{ME, TestEnv, WS};
use steve_modules::vault::Vault;
use steve_modules::vault::keystore::FileKeyStore;
use tempfile::TempDir;
use tower::ServiceExt;
use zeroize::Zeroizing;

pub const GOOGLE_ID: &str = "google-client-id";
pub const GOOGLE_SECRET: &str = "PLAINTEXT-google-client-secret";
pub const MS_ID: &str = "ms-client-id";
pub const MS_SECRET: &str = "PLAINTEXT-ms-client-secret";
pub const ORIGIN: &str = "http://hub.test:8440";
pub const ORIGIN2: &str = "https://steve.example.com";

async fn get_json(cx: &ConnCx, url: &str, bearer: &str) -> Result<Value, ProviderError> {
    let res = cx
        .http()
        .get(url)
        .header(header::AUTHORIZATION, bearer)
        .send()
        .await
        .map_err(|_| ProviderError::Transient("连不上".into()))?;
    match res.status().as_u16() {
        200..=299 => res
            .json()
            .await
            .map_err(|_| ProviderError::Transient("回答不对".into())),
        401 => Err(ProviderError::Auth("令牌无效".into())),
        s if s >= 500 => Err(ProviderError::Transient(format!("HTTP {s}"))),
        s => Err(ProviderError::Rejected(format!("HTTP {s}"))),
    }
}

// ------------------------------------------------------------------ Gmail / Outlook (A3/E2 shape)

pub struct MailOAuth {
    pub id: &'static str,
    pub system: &'static str,
    pub spec: OAuthSpec,
    /// The profile URL (`…/users/me/profile` or `…/v1.0/me`).
    pub me_url: String,
    pub field: &'static str,
}

impl MailOAuth {
    pub fn gmail(f: &steve_mail::fake::FakeGmail) -> Self {
        let mut spec = OAuthSpec::new(
            "GOOGLE",
            "Google",
            &f.authorize_url(),
            &f.token_url(),
            &[
                "openid",
                "email",
                "https://www.googleapis.com/auth/gmail.modify",
                "https://www.googleapis.com/auth/gmail.compose",
            ],
        );
        spec.extra_params = vec![
            ("access_type".into(), "offline".into()),
            ("prompt".into(), "consent".into()),
        ];
        spec.revoke_url = None;
        Self {
            id: "gmail",
            system: "gmail",
            spec,
            me_url: format!("{}users/me/profile", f.api_base()),
            field: "emailAddress",
        }
    }

    pub fn outlook(f: &steve_mail::fake::FakeGraph) -> Self {
        let mut spec = OAuthSpec::new(
            "MICROSOFT",
            "Microsoft",
            &f.authorize_url(),
            &f.token_url(),
            &["offline_access", "User.Read", "Mail.ReadWrite", "Mail.Send"],
        );
        spec.scope_on_refresh = true;
        Self {
            id: "outlook",
            system: "outlook",
            spec,
            me_url: format!("http://{}/v1.0/me", f.addr()),
            field: "mail",
        }
    }
}

#[async_trait]
impl ConnectionProvider for MailOAuth {
    fn id(&self) -> &str {
        self.id
    }
    fn system(&self) -> &str {
        self.system
    }
    fn name(&self) -> &str {
        self.id
    }
    fn caps(&self) -> &[&'static str] {
        &["mail.read", "mail.draft", "mail.send", "events"]
    }
    fn auth(&self) -> AuthKind {
        AuthKind::OAuth
    }
    fn oauth(&self) -> Option<&OAuthSpec> {
        Some(&self.spec)
    }
    /// `mail.send` only with a send scope.
    fn available_caps(&self, scopes: &[String]) -> Vec<String> {
        let send = scopes
            .iter()
            .any(|s| s.contains("gmail.compose") || s == "Mail.Send");
        self.caps()
            .iter()
            .filter(|c| send || **c != "mail.send")
            .map(|c| c.to_string())
            .collect()
    }
    async fn verify(&self, cx: &ConnCx) -> Result<AccountInfo, ProviderError> {
        let t = cx.access_token().await?;
        let v = match get_json(cx, &self.me_url, &t.bearer()).await {
            Err(ProviderError::Auth(_)) => {
                cx.invalidate_token().await;
                let t = cx.access_token().await?;
                get_json(cx, &self.me_url, &t.bearer()).await?
            }
            r => r?,
        };
        let email = v[self.field]
            .as_str()
            .ok_or_else(|| ProviderError::Rejected("没有邮箱地址".into()))?;
        Ok(AccountInfo::new(email, email))
    }
}

// ------------------------------------------------------------------ Feishu app bot (F1 shape)

pub struct FeishuApp {
    pub fields: Vec<FieldDef>,
}

impl FeishuApp {
    pub fn new() -> Self {
        Self {
            fields: vec![
                FieldDef::plain("app_id", "App ID"),
                FieldDef::secret("app_secret", "App Secret"),
                FieldDef::secret("verification_token", "Verification Token").optional(),
                FieldDef::secret("encrypt_key", "Encrypt Key").optional(),
            ],
        }
    }
}

fn base(cx: &ConnCx) -> String {
    cx.config_str("api_base")
        .unwrap_or("https://open.feishu.cn")
        .trim_end_matches('/')
        .to_string()
}

#[async_trait]
impl ConnectionProvider for FeishuApp {
    fn id(&self) -> &str {
        "feishu_app"
    }
    fn system(&self) -> &str {
        "feishu_app"
    }
    fn name(&self) -> &str {
        "飞书应用机器人"
    }
    fn caps(&self) -> &[&'static str] {
        &["msg.receive", "msg.send", "directory.read", "events"]
    }
    fn auth(&self) -> AuthKind {
        AuthKind::Credential
    }
    fn fields(&self) -> &[FieldDef] {
        &self.fields
    }
    fn token_kind(&self) -> TokenKind {
        TokenKind::Minted
    }
    async fn mint(&self, cx: &ConnCx) -> Result<Minted, ProviderError> {
        let secret = cx.secret("app_secret").await?;
        let body = Zeroizing::new(
            json!({"app_id": cx.config_str("app_id").unwrap_or_default(), "app_secret": secret.as_str()})
                .to_string(),
        );
        let v: Value = cx
            .http()
            .post(format!(
                "{}/open-apis/auth/v3/tenant_access_token/internal",
                base(cx)
            ))
            .header(header::CONTENT_TYPE, "application/json")
            .body(body.to_string())
            .send()
            .await
            .map_err(|_| ProviderError::Transient("连不上飞书".into()))?
            .json()
            .await
            .map_err(|_| ProviderError::Transient("回答不对".into()))?;
        if v["code"].as_i64() != Some(0) {
            return Err(ProviderError::Auth("App ID 或 App Secret 不对".into()));
        }
        let expire = v["expire"].as_i64().unwrap_or(7200);
        Ok(Minted {
            token: Zeroizing::new(v["tenant_access_token"].as_str().unwrap_or("").into()),
            expires_at: cx.connections().modules().clock.now() + expire * 1000,
        })
    }
    async fn verify(&self, cx: &ConnCx) -> Result<AccountInfo, ProviderError> {
        let t = cx.access_token().await?;
        let v = get_json(
            cx,
            &format!("{}/open-apis/bot/v3/info", base(cx)),
            &t.bearer(),
        )
        .await?;
        let bot = &v["bot"];
        let mut a = AccountInfo::new(
            cx.config_str("app_id").unwrap_or_default(),
            bot["app_name"].as_str().unwrap_or_default(),
        );
        a.kind = "app_bot".into();
        a.owner = Owner::SelfOrg;
        if let Some(o) = bot["open_id"].as_str() {
            a.extra_handles.push(o.into());
        }
        Ok(a)
    }
}

// ------------------------------------------------------------------ Feishu user OAuth (F6 shape)

pub struct FeishuUser {
    pub spec: OAuthSpec,
}

impl FeishuUser {
    pub fn new(base: &str) -> Self {
        let mut spec = OAuthSpec::new(
            "FEISHU",
            "飞书",
            &format!("{base}/open-apis/authen/v1/authorize"),
            &format!("{base}/open-apis/authen/v2/oauth/token"),
            &["offline_access", "im:message.send_as_user"],
        );
        spec.style = TokenStyle::Json;
        Self { spec }
    }
}

#[async_trait]
impl ConnectionProvider for FeishuUser {
    fn id(&self) -> &str {
        "feishu_user"
    }
    fn system(&self) -> &str {
        "feishu"
    }
    fn name(&self) -> &str {
        "飞书 · 你本人"
    }
    fn caps(&self) -> &[&'static str] {
        &["msg.send_as_user", "directory.read"]
    }
    fn auth(&self) -> AuthKind {
        AuthKind::OAuth
    }
    fn oauth(&self) -> Option<&OAuthSpec> {
        Some(&self.spec)
    }
    fn reauth_question(&self, _: &str) -> String {
        "飞书需要重新授权".into()
    }
    /// The app connection's App ID and Secret, not `STEVE_OAUTH_FEISHU_*`.
    async fn oauth_client(&self, cx: &StartCx<'_>) -> Result<Option<OAuthClient>, ProviderError> {
        let Some(app) = cx.config.get("app_connection").and_then(Value::as_str) else {
            return Ok(None);
        };
        let app = cx
            .connections()
            .cx(cx.ws, app)
            .map_err(|_| ProviderError::Config("飞书应用连接不见了".into()))?;
        Ok(Some(OAuthClient {
            id: app.config_str("app_id").unwrap_or_default().into(),
            secret: app.secret("app_secret").await?,
        }))
    }
    async fn verify(&self, cx: &ConnCx) -> Result<AccountInfo, ProviderError> {
        let t = cx.access_token().await?;
        let base = self
            .spec
            .token_url
            .split("/open-apis")
            .next()
            .unwrap_or_default()
            .to_string();
        let v = get_json(
            cx,
            &format!("{base}/open-apis/authen/v1/user_info"),
            &t.bearer(),
        )
        .await?;
        let d = if v.get("data").is_some() {
            &v["data"]
        } else {
            &v
        };
        let mut a = AccountInfo::new(
            d["union_id"].as_str().unwrap_or_default(),
            d["name"].as_str().unwrap_or_default(),
        );
        if let Some(o) = d["open_id"].as_str() {
            a.extra_handles.push(o.into());
        }
        Ok(a)
    }
}

// ------------------------------------------------------------------ GitHub App installation (G1 shape)

pub struct GitHubApp;

fn unix() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or_default()
}

impl GitHubApp {
    async fn jwt(cx: &ConnCx) -> Result<Zeroizing<String>, ProviderError> {
        let pem = cx.secret("private_key").await?;
        let key = jsonwebtoken::EncodingKey::from_rsa_pem(pem.as_bytes())
            .map_err(|_| ProviderError::Auth("私钥不对".into()))?;
        let now = unix();
        let claims = json!({"iat": now - 60, "exp": now + 540, "iss": cx.config()["app_id"]});
        jsonwebtoken::encode(
            &jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256),
            &claims,
            &key,
        )
        .map(|t| Zeroizing::new(format!("Bearer {t}")))
        .map_err(|_| ProviderError::Auth("签不了 JWT".into()))
    }
    fn api(cx: &ConnCx) -> String {
        cx.config_str("api_base")
            .unwrap_or("https://api.github.com")
            .to_string()
    }
}

#[async_trait]
impl ConnectionProvider for GitHubApp {
    fn id(&self) -> &str {
        "github_app"
    }
    fn system(&self) -> &str {
        "github"
    }
    fn name(&self) -> &str {
        "GitHub App"
    }
    fn caps(&self) -> &[&'static str] {
        &["repo.read", "repo.write", "events"]
    }
    fn auth(&self) -> AuthKind {
        AuthKind::Managed
    }
    fn token_kind(&self) -> TokenKind {
        TokenKind::Minted
    }
    async fn mint(&self, cx: &ConnCx) -> Result<Minted, ProviderError> {
        let jwt = Self::jwt(cx).await?;
        let id = &cx.config()["installation_id"];
        let res = cx
            .http()
            .post(format!(
                "{}/app/installations/{id}/access_tokens",
                Self::api(cx)
            ))
            .header(header::AUTHORIZATION, jwt.as_str())
            .header(header::ACCEPT, "application/vnd.github+json")
            .send()
            .await
            .map_err(|_| ProviderError::Transient("连不上 GitHub".into()))?;
        if res.status().as_u16() == 401 {
            return Err(ProviderError::Auth("GitHub 不认这个 App 的私钥".into()));
        }
        let v: Value = res
            .json()
            .await
            .map_err(|_| ProviderError::Transient("回答不对".into()))?;
        Ok(Minted {
            token: Zeroizing::new(v["token"].as_str().unwrap_or_default().into()),
            expires_at: cx.connections().modules().clock.now() + 55 * 60_000,
        })
    }
    async fn verify(&self, cx: &ConnCx) -> Result<AccountInfo, ProviderError> {
        let jwt = Self::jwt(cx).await?;
        let id = &cx.config()["installation_id"];
        let v = get_json(
            cx,
            &format!("{}/app/installations/{id}", Self::api(cx)),
            &jwt,
        )
        .await?;
        let slug = v["app_slug"].as_str().unwrap_or_default();
        let mut a = AccountInfo::new(format!("{slug}[bot]"), format!("{slug}[bot]"));
        a.kind = "app_bot".into();
        a.owner = Owner::SelfOrg;
        a.config = json!({"account_login": v["account"]["login"]});
        Ok(a)
    }
}

// ------------------------------------------------------------------ harness

pub struct H {
    pub env: TestEnv,
    pub _keys: TempDir,
    pub vault: Vault,
    pub conns: Connections,
    pub api: Router,
    pub cb: Router,
}

pub fn setup(ps: Vec<Arc<dyn ConnectionProvider>>) -> H {
    let env = TestEnv::new(connections::register);
    let keys = tempfile::tempdir().expect("tmp");
    let vault = Vault::new(
        env.store().clone(),
        Arc::new(FileKeyStore::open(keys.path()).expect("keys")),
    );
    vault.init(WS, ME).expect("vault init");
    let cfg = ConnectionsConfig::new(vec![ORIGIN.into(), ORIGIN2.into()], true, |k| match k {
        "STEVE_OAUTH_GOOGLE_CLIENT_ID" => Some(GOOGLE_ID.into()),
        "STEVE_OAUTH_GOOGLE_CLIENT_SECRET" => Some(GOOGLE_SECRET.into()),
        "STEVE_OAUTH_MICROSOFT_CLIENT_ID" => Some(MS_ID.into()),
        "STEVE_OAUTH_MICROSOFT_CLIENT_SECRET" => Some(MS_SECRET.into()),
        _ => None,
    });
    let mut b = Connections::builder(env.modules.clone(), vault.clone(), cfg);
    for p in ps {
        b = b.provider(p);
    }
    let conns = b.build();
    let api = connections::routes(conns.clone()).with_state(env.modules.clone());
    let cb = connections::callback_routes(conns.clone()).with_state(env.modules.clone());
    H {
        env,
        _keys: keys,
        vault,
        conns,
        api,
        cb,
    }
}

/// One request; returns status, JSON body (or the text as a string), headers.
pub async fn send(
    r: &Router,
    who: Option<&Caller>,
    method: &str,
    path: &str,
    body: Option<Value>,
    host: Option<&str>,
) -> (u16, Value, HeaderMap) {
    let mut req = Request::builder().method(method).uri(path);
    if body.is_some() {
        req = req.header(header::CONTENT_TYPE, "application/json");
    }
    if let Some(h) = host {
        req = req.header(header::HOST, h);
    }
    let mut req = req
        .body(match body {
            Some(b) => Body::from(b.to_string()),
            None => Body::empty(),
        })
        .expect("request");
    if let Some(c) = who {
        req.extensions_mut().insert(c.clone());
    }
    let res = r.clone().oneshot(req).await.expect("infallible");
    let status = res.status().as_u16();
    let headers = res.headers().clone();
    let bytes = axum::body::to_bytes(res.into_body(), 1 << 20)
        .await
        .expect("body");
    let v = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes)
            .unwrap_or_else(|_| Value::String(String::from_utf8_lossy(&bytes).into()))
    };
    (status, v, headers)
}

/// The consent screen of a fake: follows the authorize URL once and returns the redirect it gives
/// (our callback URL with `code` and `state`).
pub async fn consent(url: &str) -> url::Url {
    let res = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .expect("client")
        .get(url)
        .send()
        .await
        .expect("authorize");
    assert!(res.status().is_redirection(), "authorize: {}", res.status());
    let loc = res.headers()[header::LOCATION].to_str().expect("location");
    url::Url::parse(loc).expect("callback url")
}

/// `path?query` of a URL.
pub fn path_of(u: &url::Url) -> String {
    format!("{}?{}", u.path(), u.query().unwrap_or_default())
}

pub fn query(u: &url::Url, k: &str) -> Option<String> {
    u.query_pairs()
        .find(|(a, _)| a == k)
        .map(|(_, v)| v.to_string())
}

/// Start → consent → callback as `who`; returns the callback's `Location`.
pub async fn oauth_round_trip(h: &H, who: &Caller, start: Value) -> String {
    let (s, v, _) = send(
        &h.api,
        Some(who),
        "POST",
        "/connections/oauth/start",
        Some(start),
        Some("hub.test:8440"),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    let back = consent(v["url"].as_str().expect("url")).await;
    let (s, _, hd) = send(&h.cb, Some(who), "GET", &path_of(&back), None, None).await;
    assert_eq!(s, 302);
    hd[header::LOCATION].to_str().expect("loc").to_string()
}

pub fn account_of(location: &str) -> String {
    location
        .strip_prefix("/#/accounts/")
        .unwrap_or_else(|| panic!("not a success redirect: {location}"))
        .to_string()
}
