//! The login-related part of `hub/src/config.ts`.

use std::io::Read;
use std::{env, fs, io};

/// OIDC client settings; any of them may be unset, as in the TS config.
#[derive(Debug, Clone, Default)]
pub struct OidcConfig {
    /// `STEVE_OIDC_ISSUER`
    pub issuer: Option<String>,
    /// `STEVE_OIDC_CLIENT_ID`
    pub client_id: Option<String>,
    /// `STEVE_OIDC_CLIENT_SECRET_FILE` (only trailing newlines removed) or `STEVE_OIDC_CLIENT_SECRET`
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

/// A strict 0/1 switch shared by authentication and Hub configuration.
pub fn flag(vars: impl Fn(&str) -> Option<String>, name: &str) -> io::Result<bool> {
    match vars(name).as_deref() {
        None | Some("0") => Ok(false),
        Some("1") => Ok(true),
        Some(_) => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("{name}: expected 0 or 1"),
        )),
    }
}

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

    /// Reads login configuration, rejecting conflicting or unsafe secret sources.
    pub fn from_env() -> io::Result<Self> {
        Self::from_vars(|k| env::var(k).ok())
    }

    /// [`AuthConfig::from_env`] over any variable source (tests pass a map).
    pub fn from_vars(var: impl Fn(&str) -> Option<String>) -> io::Result<Self> {
        let client_secret = Self::secret(&var, "STEVE_OIDC_CLIENT_SECRET")?;
        let public_url = var("STEVE_PUBLIC_URL");
        let public_urls = var("STEVE_PUBLIC_URLS");
        let oidc = OidcConfig {
            issuer: var("STEVE_OIDC_ISSUER"),
            client_id: var("STEVE_OIDC_CLIENT_ID"),
            client_secret,
        };
        Ok(Self::new(
            public_url.as_deref(),
            public_urls.as_deref(),
            oidc,
            flag(&var, "STEVE_DEV_LOGIN")?,
        ))
    }

    /// Read a supported secret, preserving spaces and removing only final CR/LF.
    /// Direct forms are allowed only for names scrubbed from harness environments;
    /// OIDC's secret is scrubbed, while the OpenViking key must remain file-only.
    pub fn secret(vars: impl Fn(&str) -> Option<String>, name: &str) -> io::Result<Option<String>> {
        let invalid = |message: &str| io::Error::new(io::ErrorKind::InvalidInput, message);
        let direct = match name {
            "STEVE_OIDC_CLIENT_SECRET" => vars(name),
            "STEVE_OPENVIKING_KEY" if vars(name).is_none() => None,
            "STEVE_OPENVIKING_KEY" => {
                return Err(invalid(&format!(
                    "{name}: only the file source is supported"
                )));
            }
            _ => return Err(invalid(&format!("{name}: unsupported secret variable"))),
        };
        let file_name = format!("{name}_FILE");
        let Some(path) = vars(&file_name) else {
            return Ok(direct);
        };
        let read = || -> io::Result<String> {
            if direct.is_some() {
                return Err(invalid("cannot be set together with the direct variable"));
            }
            if path.is_empty() {
                return Err(invalid("expected a nonempty file path"));
            }
            // Reject special files before open: opening a FIFO could otherwise wait for a writer.
            if !fs::metadata(&path)?.is_file() {
                return Err(invalid("expected a regular file"));
            }
            let mut file = fs::File::open(&path)?;
            let meta = file.metadata()?;
            if !meta.is_file() {
                return Err(invalid("expected a regular file"));
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                if meta.permissions().mode() & 0o177 != 0 {
                    return Err(invalid("permissions must not be broader than 0600"));
                }
            }
            let mut value = String::new();
            file.read_to_string(&mut value)?;
            Ok(value.trim_end_matches(['\r', '\n']).to_owned())
        };
        read()
            .map(Some)
            .map_err(|e| io::Error::new(e.kind(), format!("{file_name}: {e}")))
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
