//! The thin GitHub REST client: fixed headers, ETag / `If-None-Match` for reads
//! (a 304 does not count against the rate limit), waiting out rate limits as the headers say, and
//! on a connection ([`GhConn`]) one re-mint after a 401 before handing over to the connection's
//! health (`reauth`). No octocrab: the dozen endpoints Steve uses are plain JSON.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use reqwest::Method;
use reqwest::header::{self, HeaderMap};
use serde_json::Value;
use steve_modules::accounts::connections::{ConnCx, Health, ProviderError, TokenKind};

/// `X-GitHub-Api-Version` of every request.
pub const API_VERSION: &str = "2022-11-28";
/// Where the API lives unless a test guard says otherwise (`config.api_base`).
pub const DEFAULT_API_BASE: &str = "https://api.github.com";
pub const USER_AGENT: &str = "steve-hub";
/// Longest rate-limit wait done in place; beyond it the caller gets [`GhError::RateLimited`].
pub const MAX_WAIT: Duration = Duration::from_secs(60);
/// The longest one API request may take, answer included (GitHub answers in well under a few
/// seconds; this bounds a hanging connection, not a slow but working one).
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// The longest a log download ([`GhConn::get_text`]) may take: a large log is many megabytes.
pub const DOWNLOAD_TIMEOUT: Duration = Duration::from_secs(120);

/// What went wrong talking to GitHub. Messages are Chinese and never carry a token.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum GhError {
    /// 401: the token (or App JWT) is not accepted.
    #[error("GitHub 不认这个令牌")]
    Unauthorized,
    /// Rate limited for longer than [`MAX_WAIT`] (or still limited after one wait).
    #[error("GitHub 限流了，{wait_secs} 秒后再试")]
    RateLimited { wait_secs: u64 },
    /// Any other non-2xx answer, with GitHub's `message`.
    #[error("GitHub 返回 {status}：{message}")]
    Status { status: u16, message: String },
    /// The connection could not be opened: nothing was sent.
    #[error("连不上 GitHub")]
    Unreachable,
    /// Sent, but no (complete) answer: timed out or cut off. For a write, it may have happened.
    #[error("GitHub 没有回应")]
    Network,
    /// Refused by the test guard (`STEVE_FORBID_REAL_HOSTS=1`), nothing sent.
    #[error("{0}")]
    HostForbidden(String),
    #[error("GitHub 的回答看不懂")]
    Decode,
    /// Getting the connection's token failed (vault, minting).
    #[error("{0}")]
    Token(ProviderError),
}

impl GhError {
    pub fn status(&self) -> Option<u16> {
        match self {
            Self::Unauthorized => Some(401),
            Self::Status { status, .. } => Some(*status),
            _ => None,
        }
    }
}

impl From<GhError> for ProviderError {
    fn from(e: GhError) -> Self {
        match e {
            GhError::Unauthorized => ProviderError::Auth(e.to_string()),
            GhError::Token(p) => p,
            GhError::HostForbidden(m) => ProviderError::Config(m),
            GhError::Status { status, .. } if status < 500 && status != 429 => {
                ProviderError::Rejected(e.to_string())
            }
            e => ProviderError::Transient(e.to_string()),
        }
    }
}

/// One answer. `status` is 304 (with `body: Null`) when `If-None-Match` matched.
#[derive(Debug, Clone)]
pub struct GhResponse {
    pub status: u16,
    pub etag: Option<String>,
    pub body: Value,
    /// `rel="next"` of the `Link` header, for pagination.
    pub next: Option<String>,
}

impl GhResponse {
    pub fn not_modified(&self) -> bool {
        self.status == 304
    }
}

/// A request without connection logic: the App JWT calls, the manifest conversion, PAT checks.
#[derive(Clone)]
pub struct GhClient {
    http: reqwest::Client,
    base: String,
    max_wait: Duration,
}

impl GhClient {
    /// `base` without trailing `/` (e.g. [`DEFAULT_API_BASE`]).
    pub fn new(http: reqwest::Client, base: &str) -> Self {
        Self {
            http,
            base: base.trim_end_matches('/').to_string(),
            max_wait: MAX_WAIT,
        }
    }

    pub fn with_max_wait(mut self, d: Duration) -> Self {
        self.max_wait = d;
        self
    }

    pub fn base(&self) -> &str {
        &self.base
    }

    /// `path` (`/repos/…`) on the base. Absolute URLs (pagination links) are only followed on the
    /// same base, so a token never goes elsewhere.
    pub fn url(&self, path: &str) -> Result<String, GhError> {
        if path.starts_with('/') {
            Ok(format!("{}{path}", self.base))
        } else if path.starts_with(&format!("{}/", self.base)) {
            Ok(path.to_string())
        } else {
            Err(GhError::Status {
                status: 400,
                message: "不在 GitHub API 上的地址".into(),
            })
        }
    }

    /// Sends one request. `auth` is the whole `Authorization` value (`Bearer …`) or none.
    /// A rate limit with a wait up to the client's maximum is waited out once and retried.
    pub async fn request(
        &self,
        method: Method,
        path: &str,
        auth: Option<&str>,
        body: Option<&Value>,
        if_none_match: Option<&str>,
    ) -> Result<GhResponse, GhError> {
        let url = self.url(path)?;
        steve_modules::net::check_url(&url).map_err(|e| GhError::HostForbidden(e.to_string()))?;
        let mut waited = false;
        loop {
            let mut req = self
                .http
                .request(method.clone(), &url)
                .header(header::ACCEPT, "application/vnd.github+json")
                .header("X-GitHub-Api-Version", API_VERSION)
                .header(header::USER_AGENT, USER_AGENT);
            if let Some(a) = auth {
                req = req.header(header::AUTHORIZATION, a);
            }
            if let Some(e) = if_none_match {
                req = req.header(header::IF_NONE_MATCH, e);
            }
            if let Some(b) = body {
                req = req.json(b);
            }
            // a hanging request must not hold the module's queues: answered, or Network
            req = req.timeout(REQUEST_TIMEOUT);
            let res = req.send().await.map_err(|e| {
                if e.is_connect() {
                    GhError::Unreachable
                } else {
                    GhError::Network
                }
            })?;
            let status = res.status().as_u16();
            let headers = res.headers().clone();
            if let Some(wait) = rate_wait(status, &headers) {
                if !waited && wait <= self.max_wait {
                    tracing::info!(secs = wait.as_secs(), "github rate limited, waiting");
                    tokio::time::sleep(wait).await;
                    waited = true;
                    continue;
                }
                return Err(GhError::RateLimited {
                    wait_secs: wait.as_secs().max(1),
                });
            }
            let etag = headers
                .get(header::ETAG)
                .and_then(|v| v.to_str().ok())
                .map(String::from);
            let next = headers
                .get(header::LINK)
                .and_then(|v| v.to_str().ok())
                .and_then(next_link);
            if status == 304 {
                return Ok(GhResponse {
                    status,
                    etag: etag.or_else(|| if_none_match.map(String::from)),
                    body: Value::Null,
                    next,
                });
            }
            let bytes = res.bytes().await.map_err(|_| GhError::Network)?;
            let body = if bytes.is_empty() {
                Value::Null
            } else {
                match serde_json::from_slice(&bytes) {
                    Ok(v) => v,
                    // a garbled read is an error (it would otherwise be cached under its ETag);
                    // a write that GitHub did accept must not look failed and be repeated
                    Err(_) if method == Method::GET && (200..=299).contains(&status) => {
                        return Err(GhError::Decode);
                    }
                    Err(_) => Value::Null,
                }
            };
            return match status {
                200..=299 => Ok(GhResponse {
                    status,
                    etag,
                    body,
                    next,
                }),
                401 => Err(GhError::Unauthorized),
                s => Err(GhError::Status {
                    status: s,
                    message: message_of(&body),
                }),
            };
        }
    }
}

/// GitHub's `message`, cut short (it never holds a token, but keep error rows small).
fn message_of(body: &Value) -> String {
    let m = body["message"].as_str().unwrap_or("请求失败");
    m.chars().take(200).collect()
}

/// How long a 403/429 says to wait: `retry-after` seconds, or until `x-ratelimit-reset` when
/// `x-ratelimit-remaining` is 0. `None`: not a rate limit (a 403 for a missing permission).
fn rate_wait(status: u16, h: &HeaderMap) -> Option<Duration> {
    if status != 403 && status != 429 {
        return None;
    }
    let num = |k: &str| {
        h.get(k)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.trim().parse::<i64>().ok())
    };
    if let Some(s) = num("retry-after") {
        return Some(Duration::from_secs(s.max(0) as u64));
    }
    if num("x-ratelimit-remaining") == Some(0) {
        let now = chrono::Utc::now().timestamp();
        let reset = num("x-ratelimit-reset").unwrap_or(now + 60);
        return Some(Duration::from_secs((reset - now).max(1) as u64));
    }
    None
}

/// The `rel="next"` URL of a `Link` header.
fn next_link(link: &str) -> Option<String> {
    link.split(',').find_map(|part| {
        let (url, rest) = part.split_once(';')?;
        rest.contains("rel=\"next\"").then(|| {
            url.trim()
                .trim_start_matches('<')
                .trim_end_matches('>')
                .to_string()
        })
    })
}

/// The base URL of a connection: `config.api_base` (only ever set under the test guard, see
/// [`crate::GitHubEnv`] and `ConnectionsConfig::allow_api_base`), else github.com.
pub fn api_base_of(cx: &ConnCx) -> &str {
    api_base_or(cx, DEFAULT_API_BASE)
}

/// `config.api_base`, else `fallback` (the module's [`crate::GitHubEnv::api_base`]).
pub fn api_base_or<'a>(cx: &'a ConnCx, fallback: &'a str) -> &'a str {
    cx.config_str("api_base")
        .filter(|b| !b.is_empty())
        .unwrap_or(fallback)
}

/// Cached bodies of conditional reads, per connection and path. Bounded; when full it starts over
/// (a cold entry only costs one counted request).
#[derive(Clone, Default)]
pub struct EtagCache(Arc<Mutex<EtagMap>>);

/// (connection, path) → (etag, body).
type EtagMap = HashMap<(String, String), (String, Value)>;

const ETAG_CACHE_MAX: usize = 2048;

impl EtagCache {
    fn lock(&self) -> std::sync::MutexGuard<'_, EtagMap> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }
    fn get(&self, conn: &str, path: &str) -> Option<(String, Value)> {
        self.lock().get(&(conn.into(), path.into())).cloned()
    }
    fn put(&self, conn: &str, path: &str, etag: String, body: Value) {
        let mut m = self.lock();
        if m.len() >= ETAG_CACHE_MAX {
            m.clear();
        }
        m.insert((conn.into(), path.into()), (etag, body));
    }
    /// Drops a connection's entries (disconnect, new credential).
    pub fn forget(&self, conn: &str) {
        self.lock().retain(|(c, _), _| c != conn);
    }
    pub fn len(&self) -> usize {
        self.lock().len()
    }
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// The client bound to one connection: its token (installation token or PAT) on every request.
#[derive(Clone)]
pub struct GhConn {
    cx: ConnCx,
    client: GhClient,
    etags: EtagCache,
}

impl GhConn {
    pub fn new(cx: ConnCx, etags: EtagCache) -> Self {
        let base = api_base_of(&cx).to_string();
        Self::on(cx, etags, &base)
    }

    /// On an explicit API base (the module's `STEVE_GITHUB_API_BASE` for a connection whose config
    /// has none).
    pub fn on(cx: ConnCx, etags: EtagCache, base: &str) -> Self {
        let client = GhClient::new(cx.http().clone(), base);
        Self { cx, client, etags }
    }

    pub fn with_max_wait(mut self, d: Duration) -> Self {
        self.client = self.client.with_max_wait(d);
        self
    }

    pub fn cx(&self) -> &ConnCx {
        &self.cx
    }

    pub fn client(&self) -> &GhClient {
        &self.client
    }

    /// Any request with the connection's token. On a 401 an installation token is minted again
    /// and the request repeated once; a second 401 (or a PAT's first) puts the connection in
    /// `reauth`.
    pub async fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
        if_none_match: Option<&str>,
    ) -> Result<GhResponse, GhError> {
        let minted = self
            .cx
            .connections()
            .provider(self.cx.adapter())
            .is_some_and(|p| p.token_kind() == TokenKind::Minted);
        let mut retried = false;
        loop {
            let token = self.cx.access_token().await.map_err(GhError::Token)?;
            let r = self
                .client
                .request(
                    method.clone(),
                    path,
                    Some(token.bearer().as_str()),
                    body,
                    if_none_match,
                )
                .await;
            match r {
                Err(GhError::Unauthorized) if minted && !retried => {
                    self.cx.invalidate_token().await;
                    retried = true;
                }
                Err(GhError::Unauthorized) => {
                    self.cx
                        .report(Health::Auth("GitHub 不认这个连接的令牌了".into()))
                        .await;
                    return Err(GhError::Unauthorized);
                }
                r => return r,
            }
        }
    }

    /// A read through the ETag cache: the cached body when GitHub answers 304.
    pub async fn get(&self, path: &str) -> Result<Value, GhError> {
        let cached = self.etags.get(self.cx.id(), path);
        let r = self
            .send(
                Method::GET,
                path,
                None,
                cached.as_ref().map(|(e, _)| e.as_str()),
            )
            .await?;
        if r.not_modified()
            && let Some((_, body)) = cached
        {
            return Ok(body);
        }
        if let Some(e) = r.etag
            && !r.body.is_null()
        {
            self.etags.put(self.cx.id(), path, e, r.body.clone());
        }
        Ok(r.body)
    }

    /// A read with an ETag the caller keeps itself (polling state in the database): `None` body
    /// when nothing changed.
    pub async fn get_if_changed(
        &self,
        path: &str,
        etag: Option<&str>,
    ) -> Result<GhResponse, GhError> {
        self.send(Method::GET, path, None, etag).await
    }

    /// Every page of a list (`per_page=100` added when missing). `key`: the array's field for
    /// wrapped lists (`repositories` of `/installation/repositories`), `None` for plain arrays.
    /// [`GhConn::get_all`], but a list longer than its page limit is an error rather than cut:
    /// for deciding something is not there.
    pub async fn get_all_complete(
        &self,
        path: &str,
        key: Option<&str>,
    ) -> Result<Vec<Value>, GhError> {
        let (out, complete) = self.pages(path, key).await?;
        if complete {
            Ok(out)
        } else {
            Err(GhError::Decode)
        }
    }

    pub async fn get_all(&self, path: &str, key: Option<&str>) -> Result<Vec<Value>, GhError> {
        Ok(self.pages(path, key).await?.0)
    }

    /// Every page of a list; `true` when the last page was reached.
    async fn pages(&self, path: &str, key: Option<&str>) -> Result<(Vec<Value>, bool), GhError> {
        let mut url = if path.contains("per_page=") {
            path.to_string()
        } else if path.contains('?') {
            format!("{path}&per_page=100")
        } else {
            format!("{path}?per_page=100")
        };
        let mut out = Vec::new();
        // GitHub caps most lists far below this; it only stops a misbehaving server.
        for _ in 0..100 {
            let r = self.send(Method::GET, &url, None, None).await?;
            let mut body = r.body;
            let list = match key {
                Some(k) => body.get_mut(k).map(Value::take).unwrap_or(Value::Null),
                None => body,
            };
            match list {
                Value::Array(v) => out.extend(v),
                _ => return Err(GhError::Decode),
            }
            match r.next {
                Some(n) => url = n,
                None => return Ok((out, true)),
            }
        }
        Ok((out, false))
    }

    pub async fn post(&self, path: &str, body: &Value) -> Result<Value, GhError> {
        Ok(self.send(Method::POST, path, Some(body), None).await?.body)
    }

    pub async fn put(&self, path: &str, body: &Value) -> Result<Value, GhError> {
        Ok(self.send(Method::PUT, path, Some(body), None).await?.body)
    }

    pub async fn patch(&self, path: &str, body: &Value) -> Result<Value, GhError> {
        Ok(self.send(Method::PATCH, path, Some(body), None).await?.body)
    }

    pub async fn delete(&self, path: &str) -> Result<(), GhError> {
        self.send(Method::DELETE, path, None, None)
            .await
            .map(|_| ())
    }

    /// A plain-text download behind a redirect (`/actions/jobs/{id}/logs` answers 302 to a
    /// short-lived signed URL). The token goes only to the API; the redirect target is fetched
    /// without it. At most `max` bytes are kept (the end of the text, where failures usually are).
    pub async fn get_text(&self, path: &str, max: usize) -> Result<String, GhError> {
        let url = self.client.url(path)?;
        let minted = self
            .cx
            .connections()
            .provider(self.cx.adapter())
            .is_some_and(|p| p.token_kind() == TokenKind::Minted);
        let mut retried = false;
        let res = loop {
            let token = self.cx.access_token().await.map_err(GhError::Token)?;
            let res = self
                .client
                .http
                .get(&url)
                .header(header::ACCEPT, "application/vnd.github+json")
                .header("X-GitHub-Api-Version", API_VERSION)
                .header(header::USER_AGENT, USER_AGENT)
                .header(header::AUTHORIZATION, token.bearer().as_str())
                .timeout(DOWNLOAD_TIMEOUT)
                .send()
                .await
                .map_err(|_| GhError::Network)?;
            if res.status().as_u16() == 401 && minted && !retried {
                self.cx.invalidate_token().await;
                retried = true;
                continue;
            }
            break res;
        };
        let status = res.status().as_u16();
        let mut res = match status {
            200 => res,
            301 | 302 | 303 | 307 | 308 => {
                let loc = res
                    .headers()
                    .get(header::LOCATION)
                    .and_then(|v| v.to_str().ok())
                    .and_then(|l| url::Url::parse(&url).ok()?.join(l).ok())
                    .filter(|u| matches!(u.scheme(), "https" | "http"))
                    .ok_or(GhError::Decode)?;
                let r = self
                    .client
                    .http
                    .get(loc)
                    .header(header::USER_AGENT, USER_AGENT)
                    .timeout(DOWNLOAD_TIMEOUT)
                    .send()
                    .await
                    .map_err(|_| GhError::Network)?;
                if !r.status().is_success() {
                    return Err(GhError::Status {
                        status: r.status().as_u16(),
                        message: "日志下载失败".into(),
                    });
                }
                r
            }
            401 => {
                self.cx
                    .report(Health::Auth("GitHub 不认这个连接的令牌了".into()))
                    .await;
                return Err(GhError::Unauthorized);
            }
            s => {
                let body: Value = res.json().await.unwrap_or(Value::Null);
                return Err(GhError::Status {
                    status: s,
                    message: message_of(&body),
                });
            }
        };
        // stream the log, keeping only a rolling tail: job logs can be hundreds of MB
        let mut buf: Vec<u8> = Vec::with_capacity(max.saturating_mul(2).min(1 << 20));
        while let Some(chunk) = res.chunk().await.map_err(|_| GhError::Network)? {
            buf.extend_from_slice(&chunk);
            if buf.len() > max.saturating_mul(2) {
                buf.drain(..buf.len() - max);
            }
        }
        let tail = &buf[buf.len().saturating_sub(max)..];
        Ok(String::from_utf8_lossy(tail).into_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use reqwest::header::HeaderValue;

    #[test]
    fn next_link_is_parsed() {
        let l = r#"<https://api.github.com/user/repos?page=2>; rel="next", <https://api.github.com/user/repos?page=5>; rel="last""#;
        assert_eq!(
            next_link(l).as_deref(),
            Some("https://api.github.com/user/repos?page=2")
        );
        assert_eq!(next_link(r#"<x>; rel="prev""#), None);
    }

    #[test]
    fn rate_waits_follow_the_headers() {
        let mut h = HeaderMap::new();
        assert_eq!(rate_wait(403, &h), None, "a permission 403");
        h.insert("retry-after", HeaderValue::from_static("7"));
        assert_eq!(rate_wait(429, &h), Some(Duration::from_secs(7)));
        assert_eq!(rate_wait(500, &h), None);
        let mut h = HeaderMap::new();
        h.insert("x-ratelimit-remaining", HeaderValue::from_static("0"));
        let reset = chrono::Utc::now().timestamp() + 30;
        h.insert("x-ratelimit-reset", HeaderValue::from(reset));
        let w = rate_wait(403, &h).expect("limited");
        assert!(w.as_secs() >= 28 && w.as_secs() <= 30, "{w:?}");
    }

    #[test]
    fn urls_stay_on_the_base() {
        let c = GhClient::new(reqwest::Client::new(), "http://127.0.0.1:1/");
        assert_eq!(c.url("/user").unwrap(), "http://127.0.0.1:1/user");
        assert!(c.url("http://127.0.0.1:1/user?page=2").is_ok());
        assert!(c.url("https://evil.example/user").is_err());
        assert!(c.url("http://127.0.0.1:10/user").is_err());
    }

    /// A garbled 2xx body is an error for a read (it would otherwise be cached as `null` under
    /// its ETag) but not for a write GitHub accepted (it must not be repeated).
    #[tokio::test]
    async fn a_garbled_body_fails_a_read_not_a_write() {
        let app = axum::Router::new().route(
            "/x",
            axum::routing::any(|| async { ([(header::ETAG, "\"e1\"")], "{\"truncated\": [") }),
        );
        let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", l.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(l, app).await });
        let c = GhClient::new(reqwest::Client::new(), &base);
        let r = c.request(Method::GET, "/x", None, None, None).await;
        assert_eq!(r.err(), Some(GhError::Decode));
        let r = c
            .request(Method::POST, "/x", None, Some(&Value::Null), None)
            .await
            .expect("a write that went through");
        assert_eq!((r.status, r.body), (200, Value::Null));
    }

    /// App JWT calls, installation tokens and PAT requests all go through `request`: a real
    /// GitHub is refused under `STEVE_FORBID_REAL_HOSTS=1` before connecting.
    #[tokio::test]
    async fn real_github_refused_under_the_flag() {
        if !steve_modules::net::forbidding_child("api::tests::real_github_refused_under_the_flag") {
            return;
        }
        let c = GhClient::new(steve_modules::net::dead_end_client(), DEFAULT_API_BASE);
        let want = GhError::HostForbidden("测试环境禁止连接外部主机：api.github.com".into());
        let e = c
            .request(
                Method::POST,
                "/app/installations/1/access_tokens",
                Some("Bearer fake-jwt"),
                None,
                None,
            )
            .await;
        assert_eq!(e.err(), Some(want.clone()));
        let e = c.request(
            Method::GET,
            "/user",
            Some("Bearer github_pat_FAKE"),
            None,
            None,
        );
        assert_eq!(e.await.err(), Some(want.clone()));
        assert!(matches!(
            ProviderError::from(want),
            ProviderError::Config(_)
        ));
    }
}
