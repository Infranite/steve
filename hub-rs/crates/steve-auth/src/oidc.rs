//! The OIDC client: lazy discovery, authorization URL, code exchange and ID token checks.

use openidconnect::{
    AuthorizationCode, ClientId, ClientSecret, CsrfToken, IssuerUrl, Nonce, PkceCodeChallenge,
    PkceCodeVerifier, RedirectUrl, Scope, TokenResponse,
    core::{CoreAuthenticationFlow, CoreClient, CoreJsonWebKeySet, CoreProviderMetadata},
    reqwest,
};
use tokio::sync::OnceCell;

use crate::{AuthError, OidcConfig};

pub struct Oidc {
    config: OidcConfig,
    http: reqwest::Client,
    /// Discovery result, cached forever after the first success (like the TS `cfg`).
    provider: OnceCell<CoreProviderMetadata>,
}

/// What `/auth/login` must remember for the callback.
pub struct Start {
    pub url: String,
    pub verifier: String,
    pub state: String,
    pub nonce: String,
}

/// The authorization response parameters.
pub struct Callback {
    pub code: Option<String>,
    pub state: Option<String>,
    pub error: Option<String>,
}

pub struct Claims {
    pub sub: String,
    pub name: String,
    pub email: Option<String>,
}

impl Oidc {
    pub fn new(config: OidcConfig) -> Result<Self, AuthError> {
        // No redirects: discovery and token requests must hit exactly the configured issuer.
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()?;
        Ok(Self {
            config,
            http,
            provider: OnceCell::new(),
        })
    }

    pub async fn provider(&self) -> Result<CoreProviderMetadata, String> {
        let (Some(issuer), Some(_)) = (&self.config.issuer, &self.config.client_id) else {
            return Err("OIDC 未配置".into());
        };
        self.provider
            .get_or_try_init(|| async {
                guard(issuer)?;
                let issuer = IssuerUrl::new(issuer.clone()).map_err(|e| e.to_string())?;
                CoreProviderMetadata::discover_async(issuer, &self.http)
                    .await
                    .map_err(|e| format!("discovery: {}", describe(&e)))
            })
            .await
            .cloned()
    }

    fn client(
        &self,
        provider: CoreProviderMetadata,
        origin: &str,
    ) -> Result<
        CoreClient<
            openidconnect::EndpointSet,
            openidconnect::EndpointNotSet,
            openidconnect::EndpointNotSet,
            openidconnect::EndpointNotSet,
            openidconnect::EndpointMaybeSet,
            openidconnect::EndpointMaybeSet,
        >,
        String,
    > {
        let id = ClientId::new(self.config.client_id.clone().unwrap_or_default());
        let secret = self.config.client_secret.clone().map(ClientSecret::new);
        let redirect =
            RedirectUrl::new(format!("{origin}/auth/callback")).map_err(|e| e.to_string())?;
        Ok(CoreClient::from_provider_metadata(provider, id, secret).set_redirect_uri(redirect))
    }

    /// PKCE S256 verifier, state and nonce (32 random bytes each, as openid-client), and the
    /// authorization URL with `redirect_uri=<origin>/auth/callback`, `scope=openid profile`.
    pub fn start(&self, provider: &CoreProviderMetadata, origin: &str) -> Result<Start, String> {
        let client = self.client(provider.clone(), origin)?;
        let verifier = PkceCodeVerifier::new(CsrfToken::new_random_len(32).into_secret());
        let challenge = PkceCodeChallenge::from_code_verifier_sha256(&verifier);
        let state = CsrfToken::new_random_len(32);
        let nonce = Nonce::new_random_len(32);
        let (s, n) = (state.clone(), nonce.clone());
        let (url, _, _) = client
            .authorize_url(
                CoreAuthenticationFlow::AuthorizationCode,
                move || s,
                move || n,
            )
            .add_scope(Scope::new("profile".into()))
            .set_pkce_challenge(challenge)
            .url();
        Ok(Start {
            url: url.into(),
            verifier: verifier.into_secret(),
            state: state.into_secret(),
            nonce: nonce.secret().clone(),
        })
    }

    /// Checks the authorization response, redeems the code and verifies the ID token
    /// (signature, issuer, audience, expiry, nonce).
    pub async fn finish(
        &self,
        provider: &CoreProviderMetadata,
        origin: &str,
        cb: Callback,
        expected_state: &str,
        verifier: &str,
        nonce: &str,
    ) -> Result<Claims, String> {
        if let Some(e) = cb.error {
            return Err(format!("authorization error: {e}"));
        }
        if cb.state.as_deref() != Some(expected_state) {
            return Err("state mismatch".into());
        }
        let code = cb.code.ok_or("no code in the callback")?;
        guard(provider.jwks_uri().url().as_str())?;
        if let Some(t) = provider.token_endpoint() {
            guard(t.url().as_str())?;
        }
        // Keys are re-read on every login so a key rotation at the IdP needs no restart.
        let jwks = CoreJsonWebKeySet::fetch_async(provider.jwks_uri(), &self.http)
            .await
            .map_err(|e| format!("jwks: {}", describe(&e)))?;
        let client = self.client(provider.clone().set_jwks(jwks), origin)?;
        let token = client
            .exchange_code(AuthorizationCode::new(code))
            .map_err(|e| e.to_string())?
            .set_pkce_verifier(PkceCodeVerifier::new(verifier.to_owned()))
            .request_async(&self.http)
            .await
            .map_err(|e| format!("token: {}", describe(&e)))?;
        let id_token = token
            .id_token()
            .ok_or("no id_token in the token response")?;
        let claims = id_token
            .claims(&client.id_token_verifier(), &Nonce::new(nonce.to_owned()))
            .map_err(|e| format!("id_token: {e}"))?;
        let name = claims
            .name()
            .and_then(|n| n.get(None))
            .map(|n| n.to_string())
            .or_else(|| claims.preferred_username().map(|u| u.to_string()))
            .or_else(|| claims.email().map(|e| e.to_string()))
            .unwrap_or_else(|| "我".into());
        Ok(Claims {
            sub: claims.subject().to_string(),
            name,
            email: claims.email().map(|e| e.to_string()),
        })
    }
}

/// The error with its causes, for the log line.
fn describe(e: &dyn std::error::Error) -> String {
    let mut s = e.to_string();
    let mut cur = e.source();
    while let Some(c) = cur {
        s.push_str(": ");
        s.push_str(&c.to_string());
        cur = c.source();
    }
    s
}

/// `STEVE_FORBID_REAL_HOSTS=1` (test Hubs): the OIDC client talks to loopback hosts only, like
/// every module client (`steve_modules::net`, which this crate does not depend on). Read once.
fn forbid_real_hosts() -> bool {
    static FLAG: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *FLAG.get_or_init(|| std::env::var("STEVE_FORBID_REAL_HOSTS").is_ok_and(|v| v.trim() == "1"))
}

fn guard(url: &str) -> Result<(), String> {
    guard_with(forbid_real_hosts(), url)
}

/// `Err` for a URL whose host is not `localhost` or a loopback address while `forbid` is set.
fn guard_with(forbid: bool, url: &str) -> Result<(), String> {
    if !forbid {
        return Ok(());
    }
    let host = url::Url::parse(url)
        .ok()
        .and_then(|u| u.host_str().map(str::to_string))
        .unwrap_or_default();
    let h = host.trim_start_matches('[').trim_end_matches(']');
    let loopback = h.eq_ignore_ascii_case("localhost")
        || h.parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.to_canonical().is_loopback());
    if loopback {
        Ok(())
    } else {
        Err(format!("测试环境禁止连接外部主机：{host}"))
    }
}

#[cfg(test)]
mod forbid_tests {
    use super::guard_with;

    #[test]
    fn only_loopback_under_the_flag() {
        assert!(guard_with(true, "http://127.0.0.1:9000/realms/x").is_ok());
        assert!(guard_with(true, "http://localhost/x").is_ok());
        assert!(guard_with(true, "http://[::1]:1/x").is_ok());
        assert_eq!(
            guard_with(true, "https://auth.example.com/application/o/steve/").unwrap_err(),
            "测试环境禁止连接外部主机：auth.example.com"
        );
        assert!(guard_with(true, "not a url").is_err());
        assert!(guard_with(false, "https://auth.example.com/").is_ok());
    }
}
