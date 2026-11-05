//! The open-platform REST client: base URLs per domain, `{code, msg, data}`
//! answers, and the one retry after an invalid-token error (re-mint once, then `reauth`).
//!
//! Tokens come from a [`TokenSource`]: a connection ([`ConnCx`], cached by the connections
//! framework until 5 minutes before expiry) or [`VerifyTokens`] while a new credential is being
//! verified. Nothing here logs a token or a response body.

use std::time::Duration;

use async_trait::async_trait;
use reqwest::Method;
use serde_json::Value;
use steve_modules::accounts::connections::{ConnCx, Health, ProviderError};
use tokio::sync::Mutex;
use zeroize::Zeroizing;

/// Feishu (China) or Lark (international): `connections.config.domain = feishu|lark`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Domain {
    #[default]
    Feishu,
    Lark,
}

impl Domain {
    /// `config.domain`; anything but `lark` is Feishu.
    pub fn from_config(config: &Value) -> Self {
        match config.get("domain").and_then(Value::as_str) {
            Some("lark") => Self::Lark,
            _ => Self::Feishu,
        }
    }
    pub fn code(self) -> &'static str {
        match self {
            Self::Feishu => "feishu",
            Self::Lark => "lark",
        }
    }
    /// The open-platform host.
    pub fn open_base(self) -> &'static str {
        match self {
            Self::Feishu => "https://open.feishu.cn",
            Self::Lark => "https://open.larksuite.com",
        }
    }
    /// The accounts host (user authorization page, device flow).
    pub fn accounts_base(self) -> &'static str {
        match self {
            Self::Feishu => "https://accounts.feishu.cn",
            Self::Lark => "https://accounts.larksuite.com",
        }
    }
}

/// The open-platform base of a connection: `config.api_base` (only present in tests — the
/// connections framework drops it from user input outside them), else the process-wide test base
/// ([`FeishuEnv`], `STEVE_FEISHU_API_BASE`), else the domain's host.
pub fn api_base(config: &Value) -> String {
    match config.get("api_base").and_then(Value::as_str) {
        Some(b) if !b.trim().is_empty() => b.trim().trim_end_matches('/').to_string(),
        _ => default_base().unwrap_or_else(|| Domain::from_config(config).open_base().to_string()),
    }
}

/// The accounts base of a connection (the fake serves both hosts on its one base).
pub fn accounts_base(config: &Value) -> String {
    match config.get("api_base").and_then(Value::as_str) {
        Some(b) if !b.trim().is_empty() => b.trim().trim_end_matches('/').to_string(),
        _ => default_base()
            .unwrap_or_else(|| Domain::from_config(config).accounts_base().to_string()),
    }
}

// ---------------------------------------------------------------------------------------------
// The test base from the environment.

static DEFAULT_BASE: std::sync::RwLock<Option<String>> = std::sync::RwLock::new(None);

fn default_base() -> Option<String> {
    DEFAULT_BASE
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .clone()
}

/// The Feishu part of the Hub's environment:
///
/// - `STEVE_FEISHU_API_BASE` (e.g. a `steve-fake-feishu`): every Feishu host — open platform,
///   accounts, long connection — of connections without their own `config.api_base`, and of
///   [`super::FeishuUser`] (OAuth, device code, token). Honoured only when `STEVE_HOME` is set
///   and is not `~/.steve` (the same guard as GitHub's `STEVE_GITHUB_API_BASE` and the
///   connections' `api_base`), so a real Hub always talks to Feishu.
/// - `STEVE_FORBID_REAL_HOSTS=1`: [`forbid_real_hosts`] (a UI test must fail rather than reach
///   open.feishu.cn). Always honoured: it can only make the Hub safer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FeishuEnv {
    pub api_base: Option<String>,
    pub forbid_real_hosts: bool,
}

impl FeishuEnv {
    pub fn from_env(var: impl Fn(&str) -> Option<String>) -> Self {
        let home = var("STEVE_HOME");
        let real = var("HOME").map(|h| format!("{}/.steve", h.trim_end_matches('/')));
        let allow = matches!((&home, &real), (Some(h), Some(r)) if h.trim_end_matches('/') != r);
        let api_base = var("STEVE_FEISHU_API_BASE")
            .map(|v| v.trim().trim_end_matches('/').to_string())
            .filter(|v| allow && !v.is_empty());
        let forbid_real_hosts = var("STEVE_FORBID_REAL_HOSTS")
            .is_some_and(|v| matches!(v.trim(), "1" | "true" | "yes"));
        Self {
            api_base,
            forbid_real_hosts,
        }
    }

    /// Applies it to this process: the default base of every Feishu client, the host guard. Call
    /// once at startup, before the providers are built.
    pub fn install(&self) {
        *DEFAULT_BASE.write().unwrap_or_else(|e| e.into_inner()) = self.api_base.clone();
        if self.forbid_real_hosts {
            forbid_real_hosts();
        }
        if let Some(b) = &self.api_base {
            tracing::warn!(base = %b, "feishu: STEVE_FEISHU_API_BASE in use (tests only)");
        }
    }

    /// The `feishu_user` provider on the test base, or on the real hosts of `domain`.
    pub fn user_provider(&self, domain: Domain) -> super::FeishuUser {
        match &self.api_base {
            Some(b) => super::FeishuUser::with_base(b),
            None => super::FeishuUser::new(domain),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Test guard: no real Feishu from tests.

static FORBID_REAL_HOSTS: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// From now on, in this process, any request to a real Feishu / Lark host panics. The fake Feishu
/// calls this when it starts (a test that forgot to hand a client the fake's base must fail, not
/// talk to open.feishu.cn); unit tests of this crate are always guarded. Never called by the Hub.
pub fn forbid_real_hosts() {
    FORBID_REAL_HOSTS.store(true, std::sync::atomic::Ordering::SeqCst);
}

fn real_hosts_forbidden() -> bool {
    cfg!(test) || FORBID_REAL_HOSTS.load(std::sync::atomic::Ordering::SeqCst)
}

/// Real open-platform, accounts and long-connection hosts (and their subdomains).
const REAL_HOST_SUFFIXES: &[&str] = &[
    "feishu.cn",
    "feishu.net",
    "feishucdn.com",
    "larksuite.com",
    "larkoffice.com",
    "larksuitecdn.com",
    "byteoversea.com",
];

/// Whether `url` points at a real Feishu / Lark host.
pub fn is_real_host(url: &str) -> bool {
    let Ok(u) = url::Url::parse(url) else {
        return false;
    };
    let Some(host) = u.host_str() else {
        return false;
    };
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    REAL_HOST_SUFFIXES
        .iter()
        .any(|s| host == *s || host.ends_with(&format!(".{s}")))
}

/// Called before every outgoing request: refuses a non-loopback host under
/// `STEVE_FORBID_REAL_HOSTS=1` ([`steve_modules::net`], the guard shared by every client), after
/// the in-process test belt of [`guard_real_feishu`].
pub(crate) fn guard_url(url: &str) -> Result<(), steve_modules::net::HostForbidden> {
    guard_real_feishu(url);
    steve_modules::net::check_url(url)
}

/// Panics when real Feishu hosts are forbidden in this process ([`forbid_real_hosts`], unit
/// tests) and `url` is one.
pub(crate) fn guard_real_feishu(url: &str) {
    if real_hosts_forbidden() && is_real_host(url) {
        // Only the host: the path may carry an app id, never a token, but keep it short anyway.
        let host = url::Url::parse(url)
            .ok()
            .and_then(|u| u.host_str().map(String::from))
            .unwrap_or_default();
        panic!("测试里不许连真实飞书（{host}）：给这个客户端传假飞书的 base URL");
    }
}

/// Error codes meaning “this token is no good, get another” (missing, tenant token invalid or
/// expired, user token invalid, user token expired).
pub const TOKEN_INVALID_CODES: &[i64] = &[99991661, 99991663, 99991668, 99991677];
/// The app (or the user's grant) lacks a scope.
pub const NO_SCOPE_CODES: &[i64] = &[99991672, 99991679];
/// Gateway frequency limit, IM send limit.
pub const RATE_LIMIT_CODES: &[i64] = &[99991400, 230020];

/// What went wrong talking to Feishu. Messages are short Chinese summaries; Feishu's own `msg`
/// is kept (it never carries a token) but cut to 200 characters.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FeishuError {
    /// The token was refused, also after one fresh token.
    #[error("飞书不认这个令牌（{code}）")]
    TokenInvalid { code: i64 },
    /// A scope isn't enabled (or not granted by the user). `scopes`: the ones Feishu named
    /// (`error.permission_violations`, any one of them is enough); `help_url`: Feishu's own link to
    /// the app's permission page with them preselected (`error.helps`), when it gave one.
    #[error("飞书应用没有开通这个权限（{code}）{}", scope_list(scopes))]
    NoScope {
        code: i64,
        msg: String,
        scopes: Vec<String>,
        help_url: Option<String>,
    },
    /// HTTP 429 or a frequency-limit code. `reset`: `x-ogw-ratelimit-reset`.
    #[error("飞书限流了，请稍后再试")]
    RateLimited { code: i64, reset: Option<Duration> },
    /// Any other non-zero `code`.
    #[error("飞书拒绝了请求（{code}：{msg}）")]
    Api { code: i64, msg: String, status: u16 },
    #[error("飞书暂时不可用（HTTP {0}）")]
    Server(u16),
    #[error("飞书暂时连不上（{0}）")]
    Network(&'static str),
    /// Refused by the test guard (`STEVE_FORBID_REAL_HOSTS=1`), nothing sent.
    #[error("{0}")]
    HostForbidden(String),
    #[error("飞书的回答看不懂")]
    Decode,
    /// Getting a token failed (mint refused, credential missing).
    #[error("{0}")]
    Token(ProviderError),
}

impl FeishuError {
    /// The Feishu `code`, when there is one.
    pub fn code(&self) -> Option<i64> {
        match self {
            Self::TokenInvalid { code }
            | Self::NoScope { code, .. }
            | Self::RateLimited { code, .. }
            | Self::Api { code, .. } => Some(*code),
            _ => None,
        }
    }
    pub fn is_no_scope(&self) -> bool {
        matches!(self, Self::NoScope { .. })
    }
}

impl From<FeishuError> for ProviderError {
    fn from(e: FeishuError) -> Self {
        match e {
            FeishuError::TokenInvalid { .. } => ProviderError::Auth(e.to_string()),
            FeishuError::NoScope { .. } | FeishuError::Api { .. } => {
                ProviderError::Rejected(e.to_string())
            }
            FeishuError::RateLimited { .. }
            | FeishuError::Server(_)
            | FeishuError::Network(_)
            | FeishuError::Decode => ProviderError::Transient(e.to_string()),
            FeishuError::Token(p) => p,
            FeishuError::HostForbidden(m) => ProviderError::Config(m),
        }
    }
}

fn scope_list(scopes: &[String]) -> String {
    if scopes.is_empty() {
        String::new()
    } else {
        format!("：{}", scopes.join("、"))
    }
}

/// Whether `s` looks like a Feishu scope (`im:chat:readonly`, `contact:user.email:readonly`).
pub fn is_scope(s: &str) -> bool {
    !s.is_empty()
        && s.len() <= 100
        && s.contains(':')
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '.' | '_' | '-'))
}

/// Whether `url` is a link to an app's permission page on the real open platform
/// (`https://open.feishu.cn/app/<app_id>/auth?q=…`, Lark `open.larksuite.com`): the only links
/// from an error body Steve keeps and shows.
pub fn is_grant_link(url: &str) -> bool {
    let Ok(u) = url::Url::parse(url) else {
        return false;
    };
    u.scheme() == "https"
        && matches!(u.host_str(), Some("open.feishu.cn" | "open.larksuite.com"))
        && u.path().starts_with("/app/")
        && u.path().trim_end_matches('/').ends_with("/auth")
}

/// The scopes and the permission-page link of a missing-scope answer. Feishu puts the scopes in
/// `error.permission_violations[].subject` and the link in `error.helps[].url` (open-platform
/// doc “如何为应用申请所需权限”, the Go SDK's `CodeError`); older answers only name them in `msg`
/// (“… One of the following scopes is required: [a, b] … https://open.feishu.cn/app/…”).
pub fn no_scope_detail(v: &Value) -> (Vec<String>, Option<String>) {
    let err = v.get("error");
    let mut scopes: Vec<String> = err
        .and_then(|e| e.get("permission_violations"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|x| x.get("subject").and_then(Value::as_str))
        .map(str::trim)
        .filter(|s| is_scope(s))
        .map(String::from)
        .collect();
    let msg = v.get("msg").and_then(Value::as_str).unwrap_or_default();
    if scopes.is_empty()
        && let Some(start) = msg.find('[')
        && let Some(len) = msg[start..].find(']')
    {
        scopes = msg[start + 1..start + len]
            .split(',')
            .map(str::trim)
            .filter(|s| is_scope(s))
            .map(String::from)
            .collect();
    }
    scopes.dedup();
    let in_msg = [
        "https://open.feishu.cn/app/",
        "https://open.larksuite.com/app/",
    ]
    .iter()
    .find_map(|p| msg.find(p))
    .map(|i| {
        msg[i..]
            .chars()
            .take_while(|c| c.is_ascii_graphic() && !matches!(c, '"' | '\'' | ')' | ']'))
            .collect::<String>()
    });
    let help_url = err
        .and_then(|e| e.get("helps"))
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|h| h.get("url").and_then(Value::as_str))
        .map(String::from)
        .chain(in_msg)
        .find(|u| is_grant_link(u));
    (scopes, help_url)
}

/// Where the bearer token of a call comes from.
#[async_trait]
pub trait TokenSource: Send + Sync {
    /// `Bearer <token>`.
    async fn bearer(&self) -> Result<Zeroizing<String>, ProviderError>;
    /// The token was refused: the next [`TokenSource::bearer`] gets a fresh one.
    async fn invalidate(&self);
    /// A fresh token was refused too.
    async fn refused_again(&self, code: i64);
    /// A call was refused for a missing scope ([`FeishuError::NoScope`]): a connection records it,
    /// so the 接入 page shows it without a manual check. Default: nothing.
    async fn no_scope(&self, scopes: &[String], help_url: Option<&str>) {
        let _ = (scopes, help_url);
    }
}

/// A connection's token: cached by the framework, re-minted (tenant) or refreshed (user) after
/// [`TokenSource::invalidate`]; a second refusal puts the connection in `reauth`.
#[async_trait]
impl TokenSource for ConnCx {
    async fn bearer(&self) -> Result<Zeroizing<String>, ProviderError> {
        Ok(self.access_token().await?.bearer())
    }
    async fn invalidate(&self) {
        self.invalidate_token().await;
    }
    async fn refused_again(&self, code: i64) {
        self.report(Health::Auth(format!("飞书不认这个令牌（{code}）")))
            .await;
    }
    async fn no_scope(&self, scopes: &[String], help_url: Option<&str>) {
        super::verify::record_refused(self, scopes, help_url).await;
    }
}

/// While a credential is verified the framework mints a fresh token on every
/// `access_token()`; this keeps one for all of verify's calls.
pub struct VerifyTokens<'a> {
    cx: &'a ConnCx,
    cached: Mutex<Option<Zeroizing<String>>>,
}

impl<'a> VerifyTokens<'a> {
    pub fn new(cx: &'a ConnCx) -> Self {
        Self {
            cx,
            cached: Mutex::new(None),
        }
    }
}

#[async_trait]
impl TokenSource for VerifyTokens<'_> {
    async fn bearer(&self) -> Result<Zeroizing<String>, ProviderError> {
        let mut c = self.cached.lock().await;
        if let Some(t) = c.as_ref() {
            return Ok(t.clone());
        }
        let t = self.cx.access_token().await?.bearer();
        *c = Some(t.clone());
        Ok(t)
    }
    async fn invalidate(&self) {
        *self.cached.lock().await = None;
        self.cx.invalidate_token().await;
    }
    async fn refused_again(&self, _code: i64) {}
}

const TIMEOUT: Duration = Duration::from_secs(30);

/// A client for one open-platform base URL.
#[derive(Clone)]
pub struct FeishuClient {
    http: reqwest::Client,
    base: String,
}

impl FeishuClient {
    pub fn new(http: reqwest::Client, base: impl Into<String>) -> Self {
        Self {
            http,
            base: base.into().trim_end_matches('/').to_string(),
        }
    }

    /// The client of a connection (its `api_base` or domain, the shared HTTP pool).
    pub fn for_cx(cx: &ConnCx) -> Self {
        Self::new(cx.http().clone(), api_base(cx.config()))
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    pub fn http(&self) -> &reqwest::Client {
        &self.http
    }

    /// `GET path?query`.
    pub async fn get(
        &self,
        tokens: &dyn TokenSource,
        path: &str,
        query: &[(&str, &str)],
    ) -> Result<Value, FeishuError> {
        self.request(tokens, Method::GET, path, query, None).await
    }

    /// `POST path?query` with a JSON body.
    pub async fn post(
        &self,
        tokens: &dyn TokenSource,
        path: &str,
        query: &[(&str, &str)],
        body: &Value,
    ) -> Result<Value, FeishuError> {
        self.request(tokens, Method::POST, path, query, Some(body))
            .await
    }

    /// One call with `code == 0` checked; returns the whole answer object (most endpoints put the
    /// result under `data`, a few — `bot/v3/info` — at the top: see [`data`]). After an
    /// invalid-token code the token is invalidated and the call made once more with a fresh one.
    pub async fn request(
        &self,
        tokens: &dyn TokenSource,
        method: Method,
        path: &str,
        query: &[(&str, &str)],
        body: Option<&Value>,
    ) -> Result<Value, FeishuError> {
        let bearer = tokens.bearer().await.map_err(FeishuError::Token)?;
        let r = match self.once(&bearer, &method, path, query, body).await {
            Err(FeishuError::TokenInvalid { code }) => {
                tracing::debug!(code, path, "feishu: token refused, getting a fresh one");
                tokens.invalidate().await;
                let bearer = tokens.bearer().await.map_err(FeishuError::Token)?;
                let r = self.once(&bearer, &method, path, query, body).await;
                if let Err(FeishuError::TokenInvalid { code }) = &r {
                    tokens.refused_again(*code).await;
                }
                r
            }
            r => r,
        };
        if let Err(FeishuError::NoScope {
            scopes, help_url, ..
        }) = &r
        {
            tracing::warn!(path, scopes = ?scopes, "feishu: refused for a missing permission");
            tokens.no_scope(scopes, help_url.as_deref()).await;
        }
        r
    }

    async fn once(
        &self,
        bearer: &str,
        method: &Method,
        path: &str,
        query: &[(&str, &str)],
        body: Option<&Value>,
    ) -> Result<Value, FeishuError> {
        let url = format!("{}{path}", self.base);
        guard_url(&url).map_err(|e| FeishuError::HostForbidden(e.to_string()))?;
        let mut req = self
            .http
            .request(method.clone(), url)
            .header(reqwest::header::AUTHORIZATION, bearer)
            .timeout(TIMEOUT);
        if !query.is_empty() {
            req = req.query(query);
        }
        if let Some(b) = body {
            req = req.json(b);
        }
        let res = req
            .send()
            .await
            .map_err(|e| FeishuError::Network(short(&e)))?;
        read_answer(res).await
    }
}

/// Classifies an answer: `code == 0` and 2xx → the object; errors by code, then by status.
pub async fn read_answer(res: reqwest::Response) -> Result<Value, FeishuError> {
    let status = res.status().as_u16();
    let reset = res
        .headers()
        .get("x-ogw-ratelimit-reset")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.trim().parse::<u64>().ok())
        .map(Duration::from_secs);
    let bytes = res
        .bytes()
        .await
        .map_err(|e| FeishuError::Network(short(&e)))?;
    let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    classify(status, reset, v)
}

fn classify(status: u16, reset: Option<Duration>, v: Value) -> Result<Value, FeishuError> {
    let code = v.get("code").and_then(Value::as_i64);
    let msg: String = v
        .get("msg")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .chars()
        .take(200)
        .collect();
    match code {
        Some(0) if (200..300).contains(&status) => Ok(v),
        Some(c) if TOKEN_INVALID_CODES.contains(&c) => Err(FeishuError::TokenInvalid { code: c }),
        Some(c) if NO_SCOPE_CODES.contains(&c) => {
            let (scopes, help_url) = no_scope_detail(&v);
            Err(FeishuError::NoScope {
                code: c,
                msg,
                scopes,
                help_url,
            })
        }
        Some(c) if RATE_LIMIT_CODES.contains(&c) => {
            Err(FeishuError::RateLimited { code: c, reset })
        }
        _ if status == 429 => Err(FeishuError::RateLimited {
            code: code.unwrap_or(0),
            reset,
        }),
        _ if status >= 500 => Err(FeishuError::Server(status)),
        Some(c) => Err(FeishuError::Api {
            code: c,
            msg,
            status,
        }),
        None if status == 401 => Err(FeishuError::TokenInvalid { code: 0 }),
        None if (200..300).contains(&status) => Err(FeishuError::Decode),
        None => Err(FeishuError::Server(status)),
    }
}

/// `data` of an answer, or the answer itself when it has none.
pub fn data(v: &Value) -> &Value {
    v.get("data").unwrap_or(v)
}

/// A reqwest error, without its URL.
pub(crate) fn short(e: &reqwest::Error) -> &'static str {
    if e.is_timeout() {
        "超时"
    } else if e.is_connect() {
        "连接失败"
    } else {
        "网络错误"
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use steve_modules::accounts::connections::ConnectionProvider;

    #[test]
    fn domains_and_bases() {
        assert_eq!(api_base(&json!({})), "https://open.feishu.cn");
        assert_eq!(
            api_base(&json!({"domain": "lark"})),
            "https://open.larksuite.com"
        );
        assert_eq!(
            accounts_base(&json!({"domain": "lark"})),
            "https://accounts.larksuite.com"
        );
        assert_eq!(
            api_base(&json!({"api_base": "http://127.0.0.1:9/"})),
            "http://127.0.0.1:9"
        );
    }

    #[test]
    fn env_base_only_under_the_test_guard() {
        let env = |home: &'static str, base: Option<&'static str>, forbid: Option<&'static str>| {
            FeishuEnv::from_env(move |k| match k {
                "HOME" => Some("/Users/user".into()),
                "STEVE_HOME" => Some(home.into()),
                "STEVE_FEISHU_API_BASE" => base.map(String::from),
                "STEVE_FORBID_REAL_HOSTS" => forbid.map(String::from),
                _ => None,
            })
        };
        assert_eq!(
            env("/Users/user/.steve", Some("http://127.0.0.1:9"), None).api_base,
            None,
            "never on the real ~/.steve"
        );
        assert_eq!(
            env("/Users/user/.steve/", Some("http://127.0.0.1:9"), None).api_base,
            None
        );
        let e = env("/tmp/t", Some(" http://127.0.0.1:9/ "), Some("1"));
        assert_eq!(e.api_base.as_deref(), Some("http://127.0.0.1:9"));
        assert!(e.forbid_real_hosts);
        assert!(!env("/tmp/t", Some(""), Some("0")).forbid_real_hosts);
        assert_eq!(env("/tmp/t", Some(""), None).api_base, None);
        let u = e.user_provider(Domain::Feishu);
        assert_eq!(
            u.oauth().map(|s| s.token_url.as_str()),
            Some("http://127.0.0.1:9/open-apis/authen/v2/oauth/token")
        );
    }

    #[test]
    fn real_hosts_are_recognised() {
        for u in [
            "https://open.feishu.cn/open-apis/x",
            "https://accounts.feishu.cn/",
            "https://open.larksuite.com",
            "wss://msg-frontier.feishu.cn/ws/v2?x=1",
            "https://OPEN.FEISHU.CN./a",
        ] {
            assert!(is_real_host(u), "{u}");
        }
        for u in [
            "http://127.0.0.1:9/open-apis",
            "https://notfeishu.cn/",
            "https://feishu.cn.example.com/",
            "garbage",
        ] {
            assert!(!is_real_host(u), "{u}");
        }
    }

    #[test]
    #[should_panic(expected = "测试里不许连真实飞书")]
    fn unit_tests_never_reach_real_feishu() {
        let _ = guard_url("https://open.feishu.cn/open-apis/bot/v3/info");
    }

    /// Under `STEVE_FORBID_REAL_HOSTS=1` (a test Hub, no panic belt) any non-loopback base is
    /// refused before connecting, Feishu or not.
    #[tokio::test]
    async fn real_hosts_refused_under_the_flag() {
        if !steve_modules::net::forbidding_child(
            "adapters::feishu::client::tests::real_hosts_refused_under_the_flag",
        ) {
            return;
        }
        struct T;
        #[async_trait]
        impl TokenSource for T {
            async fn bearer(&self) -> Result<Zeroizing<String>, ProviderError> {
                Ok(Zeroizing::new("Bearer t".into()))
            }
            async fn invalidate(&self) {}
            async fn refused_again(&self, _: i64) {}
        }
        let http = steve_modules::net::dead_end_client();
        let c = FeishuClient::new(http.clone(), "https://feishu-proxy.example.com");
        let e = c.get(&T, "/open-apis/bot/v3/info", &[]).await.err();
        let want = "测试环境禁止连接外部主机：feishu-proxy.example.com";
        assert_eq!(e, Some(FeishuError::HostForbidden(want.into())));
        let e = super::super::auth::mint_tenant_token(
            &http,
            "https://feishu-proxy.example.com",
            "cli_x",
            "secret",
            0,
        )
        .await
        .err();
        assert_eq!(e, Some(ProviderError::Config(want.into())));
        let e = super::super::ws::fetch_endpoint(
            &http,
            "https://feishu-proxy.example.com",
            "cli_x",
            "secret",
        )
        .await
        .err()
        .map(|e| e.to_string());
        assert_eq!(e, Some(format!("feishu ws client error 0: {want}")));
        assert_eq!(guard_url("http://127.0.0.1:9/open-apis"), Ok(()));
    }

    #[tokio::test]
    #[should_panic(expected = "测试里不许连真实飞书")]
    async fn a_client_on_the_default_base_panics_in_tests() {
        struct T;
        #[async_trait]
        impl TokenSource for T {
            async fn bearer(&self) -> Result<Zeroizing<String>, ProviderError> {
                Ok(Zeroizing::new("Bearer t".into()))
            }
            async fn invalidate(&self) {}
            async fn refused_again(&self, _: i64) {}
        }
        let c = FeishuClient::new(reqwest::Client::new(), api_base(&json!({})));
        let _ = c.get(&T, "/open-apis/bot/v3/info", &[]).await;
    }

    #[test]
    fn classifies_answers() {
        assert!(classify(200, None, json!({"code": 0})).is_ok());
        assert_eq!(
            classify(400, None, json!({"code": 99991663, "msg": "x"})),
            Err(FeishuError::TokenInvalid { code: 99991663 })
        );
        assert!(classify(400, None, json!({"code": 99991672})).is_err_and(|e| e.is_no_scope()));
        assert_eq!(
            classify(429, Some(Duration::from_secs(2)), json!({"code": 99991400})),
            Err(FeishuError::RateLimited {
                code: 99991400,
                reset: Some(Duration::from_secs(2))
            })
        );
        assert_eq!(
            classify(502, None, Value::Null),
            Err(FeishuError::Server(502))
        );
        assert_eq!(classify(200, None, Value::Null), Err(FeishuError::Decode));
        assert!(matches!(
            classify(400, None, json!({"code": 230002, "msg": "bot not in chat"})),
            Err(FeishuError::Api { code: 230002, .. })
        ));
        // code 0 with a 4xx status is still a failure
        assert!(classify(400, None, json!({"code": 0})).is_err());
    }

    /// The answers of the open-platform docs: “如何为应用申请所需权限” (99991672, the link in
    /// `error.helps`) and “如何解决 99991679 错误” (user scopes, no link), and a msg-only one.
    #[test]
    fn reads_the_missing_scopes_and_the_link() {
        let doc = json!({
          "code": 99991672,
          "msg": "Access denied. One of the following scopes is required: [contact:user.id:readonly]",
          "error": {
            "log_id": "202307211618216F3E9EB64DC7E1234",
            "permission_violations": [{"type": "action_scope_required", "subject": "contact:user.id:readonly"}],
            "helps": [{
              "url": "https://open.feishu.cn/app/cli_a41bc75349bcxxxx/auth?q=contact:user.id:readonly&op_from=openapi",
              "description": "Learn more about scopes and how to add them: [contact:user.id:readonly]"
            }]
          }
        });
        assert_eq!(
            no_scope_detail(&doc),
            (
                vec!["contact:user.id:readonly".to_string()],
                Some("https://open.feishu.cn/app/cli_a41bc75349bcxxxx/auth?q=contact:user.id:readonly&op_from=openapi".into())
            )
        );
        let user = json!({"code": 99991679, "msg": "Unauthorized. … required one of these privileges: [sheets:spreadsheet, drive:drive].应用未获取所需的用户授权：[sheets:spreadsheet, drive:drive]",
            "error": {"permission_violations": [{"subject": "sheets:spreadsheet", "type": "action_privilege_required"}, {"type": "action_privilege_required", "subject": "drive:drive"}]}});
        assert_eq!(
            no_scope_detail(&user),
            (
                vec!["sheets:spreadsheet".into(), "drive:drive".into()],
                None
            )
        );
        let msg_only = json!({"code": 99991672, "msg": "Access denied. One of the following scopes is required: [im:chat:readonly, im:chat].应用尚未开通所需的应用身份权限：[im:chat:readonly, im:chat]，点击链接申请并开通任一权限即可：https://open.feishu.cn/app/cli_x/auth?q=im:chat:readonly,im:chat&op_from=openapi&token_type=tenant"});
        assert_eq!(
            no_scope_detail(&msg_only),
            (
                vec!["im:chat:readonly".into(), "im:chat".into()],
                Some("https://open.feishu.cn/app/cli_x/auth?q=im:chat:readonly,im:chat&op_from=openapi&token_type=tenant".into())
            )
        );
        // a link anywhere else is not kept
        let other = json!({"code": 99991672, "msg": "x", "error": {"helps": [{"url": "https://evil.example.com/app/cli_x/auth"}, {"url": "http://open.feishu.cn/app/cli_x/auth"}]}});
        assert_eq!(no_scope_detail(&other), (Vec::<String>::new(), None));
        assert!(matches!(
            classify(400, None, doc),
            Err(FeishuError::NoScope {
                help_url: Some(_),
                ..
            })
        ));
    }

    #[test]
    fn maps_to_provider_errors() {
        assert!(matches!(
            ProviderError::from(FeishuError::TokenInvalid { code: 1 }),
            ProviderError::Auth(_)
        ));
        assert!(matches!(
            ProviderError::from(FeishuError::Server(500)),
            ProviderError::Transient(_)
        ));
        assert!(matches!(
            ProviderError::from(FeishuError::NoScope {
                code: 1,
                msg: String::new(),
                scopes: Vec::new(),
                help_url: None,
            }),
            ProviderError::Rejected(_)
        ));
    }
}
