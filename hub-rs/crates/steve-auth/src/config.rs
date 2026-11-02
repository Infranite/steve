//! The login-related part of `hub/src/config.ts`.

use std::{env, fs, io};

/// OIDC client settings; any of them may be unset, as in the TS config.
#[derive(Debug, Clone, Default)]
pub struct OidcConfig {
    /// `STEVE_OIDC_ISSUER`
    pub issuer: Option<String>,
    /// `STEVE_OIDC_CLIENT_ID`
    pub client_id: Option<String>,
    /// `STEVE_OIDC_CLIENT_SECRET_FILE` (read, trimmed) or `STEVE_OIDC_CLIENT_SECRET`
    pub client_secret: Option<String>,
}

#[derive(Debug, Clone)]
pub struct AuthConfig {
    /// `STEVE_PUBLIC_URL` without a trailing `/`; decides the cookies' `Secure` flag.
    pub public_url: String,
    /// `STEVE_PUBLIC_URLS`: every origin Steve is served on; login returns to the one the browser used.
    pub public_urls: Vec<String>,
    pub oidc: OidcConfig,
    /// `STEVE_DEV_LOGIN=1`: one-click login from loopback.
    pub dev_login: bool,
}

const DEFAULT_URL: &str = "http://127.0.0.1:8440";

impl AuthConfig {
    /// Builds the config the way config.ts does from raw env values.
    pub fn new(
        public_url: Option<&str>,
        public_urls: Option<&str>,
        oidc: OidcConfig,
        dev_login: bool,
    ) -> Self {
        let public_url = public_url.unwrap_or(DEFAULT_URL);
        let public_urls = public_urls
            .unwrap_or(public_url)
            .split(',')
            .map(|u| strip_slash(u.trim()).to_owned())
            .filter(|u| !u.is_empty())
            .collect();
        Self {
            public_url: strip_slash(public_url).to_owned(),
            public_urls,
            oidc,
            dev_login,
        }
    }

    /// Reads `STEVE_PUBLIC_URL(S)`, `STEVE_OIDC_*` and `STEVE_DEV_LOGIN`. Fails only when
    /// `STEVE_OIDC_CLIENT_SECRET_FILE` is set but unreadable (TS throws at startup too).
    pub fn from_env() -> io::Result<Self> {
        Self::from_vars(|k| env::var(k).ok())
    }

    /// [`AuthConfig::from_env`] over any variable source (tests pass a map).
    pub fn from_vars(var: impl Fn(&str) -> Option<String>) -> io::Result<Self> {
        let client_secret = match var("STEVE_OIDC_CLIENT_SECRET_FILE") {
            Some(p) => Some(fs::read_to_string(p)?.trim().to_owned()),
            None => var("STEVE_OIDC_CLIENT_SECRET"),
        };
        let oidc = OidcConfig {
            issuer: var("STEVE_OIDC_ISSUER"),
            client_id: var("STEVE_OIDC_CLIENT_ID"),
            client_secret,
        };
        Ok(Self::new(
            var("STEVE_PUBLIC_URL").as_deref(),
            var("STEVE_PUBLIC_URLS").as_deref(),
            oidc,
            var("STEVE_DEV_LOGIN").as_deref() == Some("1"),
        ))
    }

    pub(crate) fn secure(&self) -> bool {
        self.public_url.starts_with("https://")
    }
}

/// `.replace(/\/$/, "")`: one trailing slash only.
fn strip_slash(u: &str) -> &str {
    u.strip_suffix('/').unwrap_or(u)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_urls_like_config_ts() {
        let c = AuthConfig::new(None, None, OidcConfig::default(), false);
        assert_eq!(c.public_url, "http://127.0.0.1:8440");
        assert_eq!(c.public_urls, ["http://127.0.0.1:8440"]);
        let c = AuthConfig::new(
            Some("https://steve.example.test/"),
            Some(" https://steve.example.test/ ,, http://steve.home//"),
            OidcConfig::default(),
            false,
        );
        assert_eq!(c.public_url, "https://steve.example.test");
        assert_eq!(
            c.public_urls,
            ["https://steve.example.test", "http://steve.home/"]
        );
        assert!(c.secure());
    }
}
