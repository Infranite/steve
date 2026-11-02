//! The generic OAuth 2.0 client side (RFC 6749 authorization code with PKCE S256 (RFC 7636), the
//! refresh grant, the device authorization grant (RFC 8628), token revocation (RFC 7009)), configured
//! per provider by an [`OAuthSpec`]. Provider quirks are data: Feishu's v2 token endpoint takes JSON
//! and answers `{code, …}`; its device endpoint wants HTTP Basic client authentication.
//!
//! Tokens only ever live in [`Zeroizing`] strings; nothing here logs a request or response body.

use std::fmt;
use std::time::Duration;

use data_encoding::BASE64URL_NOPAD;
use serde_json::Value;
use sha2::{Digest, Sha256};
use zeroize::Zeroizing;

use super::cx::ConnCx;
use super::provider::ProviderError;

/// How the token endpoint wants its parameters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenStyle {
    /// `application/x-www-form-urlencoded` (RFC 6749).
    Form,
    /// A JSON object (Feishu `authen/v2/oauth/token`).
    Json,
}

/// How the client authenticates at the token and device endpoints.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientAuth {
    /// `client_id` and `client_secret` in the body.
    Body,
    /// `Authorization: Basic base64(id:secret)`.
    Basic,
}

/// One provider's OAuth endpoints and habits. Built by the adapter (tests point the URLs at a fake).
#[derive(Debug, Clone)]
pub struct OAuthSpec {
    /// The env name part: `STEVE_OAUTH_<provider>_CLIENT_ID/SECRET` (`GOOGLE`, `MICROSOFT`, `SLACK`).
    pub provider: String,
    /// Shown in `还没有配置 <name> 的 OAuth 应用` and in errors (`Google`, `Microsoft`, `飞书`).
    pub provider_name: String,
    pub authorize_url: String,
    pub token_url: String,
    /// RFC 8628 device authorization endpoint; `None` = no device flow.
    pub device_url: Option<String>,
    /// Where device-code polling goes when it isn't `token_url` (Feishu `oauth/v3/token`).
    pub device_token_url: Option<String>,
    /// RFC 7009 revocation endpoint (Google); `None` = nothing to call on disconnect.
    pub revoke_url: Option<String>,
    pub scopes: Vec<String>,
    /// Extra authorize parameters (`access_type=offline`, `prompt=consent`).
    pub extra_params: Vec<(String, String)>,
    pub style: TokenStyle,
    pub client_auth: ClientAuth,
    /// Send `scope` on the refresh grant too (Microsoft wants it; most ignore it).
    pub scope_on_refresh: bool,
}

impl OAuthSpec {
    /// A standard provider: form bodies, client secret in the body, PKCE.
    pub fn new(
        provider: &str,
        provider_name: &str,
        authorize_url: &str,
        token_url: &str,
        scopes: &[&str],
    ) -> Self {
        Self {
            provider: provider.into(),
            provider_name: provider_name.into(),
            authorize_url: authorize_url.into(),
            token_url: token_url.into(),
            device_url: None,
            device_token_url: None,
            revoke_url: None,
            scopes: scopes.iter().map(|s| s.to_string()).collect(),
            extra_params: Vec::new(),
            style: TokenStyle::Form,
            client_auth: ClientAuth::Body,
            scope_on_refresh: false,
        }
    }
}

/// An OAuth client registration. `Debug` hides the secret.
pub struct OAuthClient {
    pub id: String,
    pub secret: Zeroizing<String>,
}

impl fmt::Debug for OAuthClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OAuthClient").field("id", &self.id).finish()
    }
}

/// What a token endpoint gave. `Debug` hides the tokens.
pub struct TokenSet {
    pub access_token: Zeroizing<String>,
    /// `None`: the provider kept the old one (Google refreshes) or gave none (no `offline_access`).
    pub refresh_token: Option<Zeroizing<String>>,
    /// Seconds.
    pub expires_in: Option<i64>,
    /// Seconds (Feishu `refresh_token_expires_in`, Microsoft `refresh_token_expires_in`).
    pub refresh_expires_in: Option<i64>,
    /// The granted scopes as returned (space separated); `None` = as requested.
    pub scope: Option<String>,
}

impl fmt::Debug for TokenSet {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TokenSet")
            .field("refresh_token", &self.refresh_token.is_some())
            .field("expires_in", &self.expires_in)
            .field("scope", &self.scope)
            .finish()
    }
}

/// A random URL-safe string of `n` bytes of entropy (states, PKCE verifiers).
pub fn random_token(n: usize) -> Result<String, ProviderError> {
    let mut b = Zeroizing::new(vec![0u8; n]);
    getrandom::fill(&mut b).map_err(|_| ProviderError::Config("系统随机数不可用".into()))?;
    Ok(BASE64URL_NOPAD.encode(&b))
}

/// PKCE S256: `base64url(sha256(verifier))`.
pub fn pkce_challenge(verifier: &str) -> String {
    BASE64URL_NOPAD.encode(&Sha256::digest(verifier.as_bytes()))
}

/// The browser URL of the authorization request.
pub fn authorize_url(
    spec: &OAuthSpec,
    client_id: &str,
    redirect_uri: &str,
    state: &str,
    challenge: &str,
) -> Result<String, ProviderError> {
    let mut u = url::Url::parse(&spec.authorize_url)
        .map_err(|_| ProviderError::Config(format!("{} 的授权地址不对", spec.provider_name)))?;
    {
        let mut q = u.query_pairs_mut();
        q.append_pair("response_type", "code")
            .append_pair("client_id", client_id)
            .append_pair("redirect_uri", redirect_uri)
            .append_pair("state", state)
            .append_pair("code_challenge", challenge)
            .append_pair("code_challenge_method", "S256");
        if !spec.scopes.is_empty() {
            q.append_pair("scope", &spec.scopes.join(" "));
        }
        for (k, v) in &spec.extra_params {
            q.append_pair(k, v);
        }
    }
    Ok(u.into())
}

/// An error answer of a token endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EndpointError {
    pub status: u16,
    /// The OAuth `error` code (`invalid_grant`, `authorization_pending`…); empty if none.
    pub error: String,
}

enum Posted {
    Ok(Value),
    Err(EndpointError),
}

/// POSTs `params` to `url` as the spec wants, with the client's authentication.
async fn post(
    http: &reqwest::Client,
    spec: &OAuthSpec,
    url: &str,
    client: &OAuthClient,
    mut params: Vec<(&'static str, Zeroizing<String>)>,
) -> Result<Posted, ProviderError> {
    crate::net::check_url(url).map_err(|e| ProviderError::Config(e.to_string()))?;
    let mut req = http
        .post(url)
        .header(reqwest::header::ACCEPT, "application/json");
    match spec.client_auth {
        ClientAuth::Body => {
            params.push(("client_id", Zeroizing::new(client.id.clone())));
            params.push(("client_secret", client.secret.clone()));
        }
        ClientAuth::Basic => {
            req = req.basic_auth(&client.id, Some(client.secret.as_str()));
            params.push(("client_id", Zeroizing::new(client.id.clone())));
        }
    }
    req = match spec.style {
        TokenStyle::Form => {
            let pairs: Vec<(&str, &str)> = params.iter().map(|(k, v)| (*k, v.as_str())).collect();
            req.form(&pairs)
        }
        TokenStyle::Json => {
            let mut o = serde_json::Map::new();
            for (k, v) in &params {
                o.insert((*k).into(), Value::String(v.to_string()));
            }
            let body = Zeroizing::new(Value::Object(o).to_string());
            req.header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(body.to_string())
        }
    };
    let res = req
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| {
            ProviderError::Transient(format!(
                "{} 暂时连不上（{}）",
                spec.provider_name,
                short(&e)
            ))
        })?;
    let status = res.status().as_u16();
    let bytes = res.bytes().await.map_err(|e| {
        ProviderError::Transient(format!(
            "{} 的回答没有读完（{}）",
            spec.provider_name,
            short(&e)
        ))
    })?;
    let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    let error = v
        .get("error")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    // Feishu: `code` non-zero is an error even when the status is 200.
    let code_err = v
        .get("code")
        .and_then(Value::as_i64)
        .is_some_and(|c| c != 0);
    if (200..300).contains(&status) && error.is_empty() && !code_err && v.is_object() {
        return Ok(Posted::Ok(v));
    }
    if status >= 500 || status == 429 {
        return Err(ProviderError::Transient(format!(
            "{} 暂时不可用（HTTP {status}）",
            spec.provider_name
        )));
    }
    Ok(Posted::Err(EndpointError { status, error }))
}

/// A reqwest error without its URL's query (never holds a token here, but keep it short anyway).
fn short(e: &reqwest::Error) -> &'static str {
    if e.is_timeout() {
        "超时"
    } else if e.is_connect() {
        "连接失败"
    } else {
        "网络错误"
    }
}

fn token_set(v: &Value, name: &str) -> Result<TokenSet, ProviderError> {
    // Feishu v1 wraps in `data`; v2 and everyone else answer flat.
    let v = if v.get("access_token").is_none() {
        v.get("data").unwrap_or(v)
    } else {
        v
    };
    let s = |k: &str| v.get(k).and_then(Value::as_str).filter(|s| !s.is_empty());
    let n = |k: &str| {
        v.get(k).and_then(|x| {
            x.as_i64()
                .or_else(|| x.as_str().and_then(|s| s.parse().ok()))
        })
    };
    let access = s("access_token")
        .ok_or_else(|| ProviderError::Rejected(format!("{name} 没有给出访问令牌")))?;
    Ok(TokenSet {
        access_token: Zeroizing::new(access.into()),
        refresh_token: s("refresh_token").map(|r| Zeroizing::new(r.into())),
        expires_in: n("expires_in"),
        refresh_expires_in: n("refresh_token_expires_in").or_else(|| n("refresh_expires_in")),
        scope: s("scope").map(String::from),
    })
}

/// Errors that mean “the grant is gone, ask the user again”.
fn is_auth_error(e: &EndpointError) -> bool {
    matches!(
        e.error.as_str(),
        "invalid_grant" | "invalid_client" | "unauthorized_client" | "access_denied"
    ) || e.status == 401
}

fn endpoint_error(spec: &OAuthSpec, e: EndpointError) -> ProviderError {
    let what = if e.error.is_empty() {
        format!("HTTP {}", e.status)
    } else {
        e.error.clone()
    };
    if is_auth_error(&e) {
        ProviderError::Auth(format!("{} 的授权已失效（{what}）", spec.provider_name))
    } else {
        ProviderError::Rejected(format!("{} 拒绝了请求（{what}）", spec.provider_name))
    }
}

/// The authorization-code grant with the PKCE verifier.
pub async fn exchange_code(
    http: &reqwest::Client,
    spec: &OAuthSpec,
    client: &OAuthClient,
    code: &str,
    redirect_uri: &str,
    verifier: &str,
) -> Result<TokenSet, ProviderError> {
    let params = vec![
        ("grant_type", Zeroizing::new("authorization_code".into())),
        ("code", Zeroizing::new(code.into())),
        ("redirect_uri", Zeroizing::new(redirect_uri.into())),
        ("code_verifier", Zeroizing::new(verifier.into())),
    ];
    match post(http, spec, &spec.token_url, client, params).await? {
        Posted::Ok(v) => token_set(&v, &spec.provider_name),
        Posted::Err(e) => Err(endpoint_error(spec, e)),
    }
}

/// The refresh grant. Single-use refresh tokens (Feishu, Microsoft) come back rotated: the caller
/// must commit the new set before using it.
pub async fn refresh(
    http: &reqwest::Client,
    spec: &OAuthSpec,
    client: &OAuthClient,
    refresh_token: &str,
) -> Result<TokenSet, ProviderError> {
    let mut params = vec![
        ("grant_type", Zeroizing::new("refresh_token".into())),
        ("refresh_token", Zeroizing::new(refresh_token.into())),
    ];
    if spec.scope_on_refresh && !spec.scopes.is_empty() {
        params.push(("scope", Zeroizing::new(spec.scopes.join(" "))));
    }
    match post(http, spec, &spec.token_url, client, params).await? {
        Posted::Ok(v) => token_set(&v, &spec.provider_name),
        Posted::Err(e) => Err(endpoint_error(spec, e)),
    }
}

/// What the device authorization endpoint answered; `device_code` stays server-side.
pub struct DeviceStart {
    pub device_code: Zeroizing<String>,
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: Option<String>,
    /// Seconds.
    pub expires_in: i64,
    /// Seconds between polls.
    pub interval: i64,
}

/// RFC 8628 §3.1.
pub async fn device_start(
    http: &reqwest::Client,
    spec: &OAuthSpec,
    client: &OAuthClient,
) -> Result<DeviceStart, ProviderError> {
    let url = spec
        .device_url
        .as_deref()
        .ok_or(ProviderError::Unsupported)?;
    let mut params = Vec::new();
    if !spec.scopes.is_empty() {
        params.push(("scope", Zeroizing::new(spec.scopes.join(" "))));
    }
    let v = match post(http, spec, url, client, params).await? {
        Posted::Ok(v) => v,
        Posted::Err(e) => return Err(endpoint_error(spec, e)),
    };
    let v = if v.get("device_code").is_none() {
        v.get("data").cloned().unwrap_or(v)
    } else {
        v
    };
    let s = |k: &str| v.get(k).and_then(Value::as_str).map(String::from);
    let n = |k: &str| v.get(k).and_then(Value::as_i64);
    let bad = || ProviderError::Rejected(format!("{} 的设备码回答不完整", spec.provider_name));
    Ok(DeviceStart {
        device_code: Zeroizing::new(s("device_code").ok_or_else(bad)?),
        user_code: s("user_code").ok_or_else(bad)?,
        verification_uri: s("verification_uri")
            .or_else(|| s("verification_url"))
            .ok_or_else(bad)?,
        verification_uri_complete: s("verification_uri_complete"),
        expires_in: n("expires_in").unwrap_or(600).clamp(30, 1800),
        interval: n("interval").unwrap_or(5).clamp(1, 60),
    })
}

/// One poll of the device-code grant.
pub enum DevicePoll {
    Done(TokenSet),
    /// `authorization_pending`.
    Pending,
    /// `slow_down`: add 5 seconds to the interval.
    SlowDown,
    /// Expired, denied, or refused: the flow is over, with the user-facing reason.
    Failed(String),
}

/// RFC 8628 §3.4–3.5.
pub async fn device_poll(
    http: &reqwest::Client,
    spec: &OAuthSpec,
    client: &OAuthClient,
    device_code: &str,
) -> Result<DevicePoll, ProviderError> {
    let url = spec.device_token_url.as_deref().unwrap_or(&spec.token_url);
    let params = vec![
        (
            "grant_type",
            Zeroizing::new("urn:ietf:params:oauth:grant-type:device_code".into()),
        ),
        ("device_code", Zeroizing::new(device_code.into())),
    ];
    Ok(match post(http, spec, url, client, params).await? {
        Posted::Ok(v) => DevicePoll::Done(token_set(&v, &spec.provider_name)?),
        Posted::Err(e) => match e.error.as_str() {
            "authorization_pending" => DevicePoll::Pending,
            "slow_down" => DevicePoll::SlowDown,
            "expired_token" => DevicePoll::Failed("设备码已经过期，请重新开始".into()),
            "access_denied" => {
                DevicePoll::Failed(format!("你在 {} 那边拒绝了授权", spec.provider_name))
            }
            _ => DevicePoll::Failed(endpoint_error(spec, e).to_string()),
        },
    })
}

/// RFC 7009: revokes the refresh token (or the access token when there is none).
pub async fn revoke(cx: &ConnCx, spec: &OAuthSpec) -> Result<(), ProviderError> {
    let Some(url) = spec.revoke_url.as_deref() else {
        return Ok(());
    };
    let token = match cx.secret(super::cx::REFRESH_TOKEN).await {
        Ok(t) => t,
        Err(_) => cx.secret(super::cx::ACCESS_TOKEN).await?,
    };
    let body = Zeroizing::new(
        url::form_urlencoded::Serializer::new(String::new())
            .append_pair("token", &token)
            .finish(),
    );
    crate::net::check_url(url).map_err(|e| ProviderError::Config(e.to_string()))?;
    let res = cx
        .http()
        .post(url)
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/x-www-form-urlencoded",
        )
        .body(body.to_string())
        .timeout(Duration::from_secs(15))
        .send()
        .await
        .map_err(|e| ProviderError::Transient(format!("撤销授权失败（{}）", short(&e))))?;
    // 400 invalid_token: already revoked, fine.
    if res.status().is_success() || res.status().as_u16() == 400 {
        Ok(())
    } else {
        Err(ProviderError::Transient(format!(
            "撤销授权失败（HTTP {}）",
            res.status().as_u16()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec() -> OAuthSpec {
        let mut s = OAuthSpec::new(
            "google",
            "Google",
            "https://accounts.google.com/o/oauth2/v2/auth",
            "https://oauth2.googleapis.com/token",
            &["openid"],
        );
        s.device_url = Some("https://oauth2.googleapis.com/device/code".into());
        s
    }

    fn client() -> OAuthClient {
        OAuthClient {
            id: "fake-id".into(),
            secret: Zeroizing::new("fake-secret".into()),
        }
    }

    /// Token, refresh and device endpoints on a real host are refused before connecting.
    #[tokio::test]
    async fn real_token_endpoints_refused_under_the_flag() {
        if !crate::net::forbidding_child(
            "accounts::connections::oauth::tests::real_token_endpoints_refused_under_the_flag",
        ) {
            return;
        }
        let http = crate::net::dead_end_client();
        let want = ProviderError::Config("测试环境禁止连接外部主机：oauth2.googleapis.com".into());
        let e = refresh(&http, &spec(), &client(), "fake-refresh").await;
        assert_eq!(e.err(), Some(want.clone()));
        let e = exchange_code(&http, &spec(), &client(), "c", "http://127.0.0.1/cb", "v").await;
        assert_eq!(e.err(), Some(want.clone()));
        let e = device_start(&http, &spec(), &client()).await;
        assert_eq!(e.err(), Some(want.clone()));
        let e = device_poll(&http, &spec(), &client(), "dc").await;
        assert_eq!(e.err(), Some(want));
    }
}
