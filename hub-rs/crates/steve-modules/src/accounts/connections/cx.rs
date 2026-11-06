//! What an adapter sees of its connection ([`ConnCx`]) and of a flow being started ([`StartCx`]).
//! Secrets are lent, not handed over: [`ConnCx::with_credential`] (closure), or one value at a time in
//! a [`Zeroizing`] string for the HTTP header that needs it ([`ConnCx::secret`],
//! [`ConnCx::access_token`]).

use std::fmt;

use serde_json::Value;
use zeroize::Zeroizing;

use super::oauth::{OAuthClient, OAuthSpec};
use super::provider::{Health, Paired, ProviderError};
use super::service::Connections;
use crate::vault::VaultError;
use crate::vault::items::Credential;

/// Credential field keys of OAuth connections (same for every provider).
pub const ACCESS_TOKEN: &str = "access_token";
pub const REFRESH_TOKEN: &str = "refresh_token";
/// Unix ms, as decimal text.
pub const EXPIRES_AT: &str = "expires_at";
pub const REFRESH_EXPIRES_AT: &str = "refresh_expires_at";
pub const SCOPE: &str = "scope";

/// A bearer token on loan. No `Clone`, `Display`, `Serialize`; `Debug` is opaque; zeroed on drop.
pub struct AccessToken(pub(crate) Zeroizing<String>);

impl AccessToken {
    pub fn expose(&self) -> &str {
        &self.0
    }
    /// `Bearer <token>` for an `Authorization` header.
    pub fn bearer(&self) -> Zeroizing<String> {
        Zeroizing::new(format!("Bearer {}", self.0.as_str()))
    }
}

impl fmt::Debug for AccessToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("AccessToken(…)")
    }
}

pub(crate) fn vault_err(e: VaultError) -> ProviderError {
    match e {
        VaultError::Api(crate::ApiError::NotFound(_)) => {
            ProviderError::Auth("凭据不见了，需要重新接入".into())
        }
        e => ProviderError::Config(e.to_string()),
    }
}

/// One connection, as its adapter sees it. Cheap to clone.
#[derive(Clone)]
pub struct ConnCx {
    pub(crate) conns: Connections,
    pub(crate) ws: String,
    pub(crate) id: String,
    pub(crate) adapter: String,
    pub(crate) item: Option<String>,
    pub(crate) config: Value,
    pub(crate) scopes: Vec<String>,
    /// The connection it is paired with ([`super::ConnectionProvider::pairing`]), when it was made.
    pub(crate) paired: Option<Paired>,
    /// Not in `connections` yet (verify during connect): health reports are dropped.
    pub(crate) pending: bool,
}

impl fmt::Debug for ConnCx {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ConnCx")
            .field("ws", &self.ws)
            .field("id", &self.id)
            .field("adapter", &self.adapter)
            .finish()
    }
}

impl ConnCx {
    pub fn ws(&self) -> &str {
        &self.ws
    }
    /// The connection id (`con_…`), also for a connection still being created.
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn adapter(&self) -> &str {
        &self.adapter
    }
    /// `connections.config` (no secrets): endpoints, tenant, `api_base` in tests.
    pub fn config(&self) -> &Value {
        &self.config
    }
    /// A string from `config`.
    pub fn config_str(&self, key: &str) -> Option<&str> {
        self.config.get(key).and_then(Value::as_str)
    }
    /// The connection this one is paired with (`config.app_connection`), as it was when this
    /// context was made: its id, adapter and whether it is usable. A provider that attests who an
    /// account is ([`super::ConnectionProvider::claims_self`]) may ask it.
    pub fn paired(&self) -> Option<&Paired> {
        self.paired.as_ref()
    }
    /// The scopes the provider granted.
    pub fn scopes(&self) -> &[String] {
        &self.scopes
    }
    /// The shared HTTP client (rustls, connection pool).
    pub fn http(&self) -> &reqwest::Client {
        self.conns.http()
    }
    pub fn connections(&self) -> &Connections {
        &self.conns
    }

    /// Lends the decrypted credential to `f` (blocking: one SQLite read and the decryption; call it
    /// from `spawn_blocking` when that matters). Use is logged once per day (`mode=internal`).
    pub fn with_credential<R>(&self, f: impl FnOnce(&Credential) -> R) -> Result<R, ProviderError> {
        let item = self
            .item
            .as_deref()
            .ok_or_else(|| ProviderError::Config("这个连接没有凭据".into()))?;
        self.conns
            .vault()
            .with_credential(&self.ws, item, &self.id, "连接", f)
            .map_err(vault_err)
    }

    /// One credential field, by key (off the runtime).
    pub async fn secret(&self, key: &str) -> Result<Zeroizing<String>, ProviderError> {
        let cx = self.clone();
        let key = key.to_string();
        tokio::task::spawn_blocking(move || {
            cx.with_credential(|c| c.text(&key).map(|v| Zeroizing::new(v.to_string())))?
                .ok_or_else(|| ProviderError::Config(format!("凭据里没有 {key}")))
        })
        .await
        .map_err(|_| ProviderError::Config("任务失败".into()))?
    }

    /// A valid bearer token: refreshes stored OAuth tokens 5 minutes before they expire (the new set
    /// committed to the vault first), mints and caches short-lived ones, or reads the token field.
    pub async fn access_token(&self) -> Result<AccessToken, ProviderError> {
        self.conns.access_token(self).await
    }

    /// The provider said 401 to the current token: the next [`ConnCx::access_token`] refreshes or
    /// re-mints. If that one fails too, report [`Health::Auth`].
    pub async fn invalidate_token(&self) {
        self.conns.invalidate(self).await;
    }

    /// Records the outcome of real work (a sync, a send) in the connection's health, like a check:
    /// `Auth` → `reauth` + decision, `Failed` ×3 → `error`, `Ok` resets.
    pub async fn report(&self, h: Health) {
        if !self.pending {
            self.conns.record(&self.ws, &self.id, h).await;
        }
    }
}

/// A flow being started or a token being refreshed: who, where, and the request's `config`.
pub struct StartCx<'a> {
    pub(crate) conns: &'a Connections,
    pub ws: &'a str,
    pub config: &'a Value,
}

impl StartCx<'_> {
    /// `STEVE_OAUTH_<provider>_CLIENT_ID` and `_SECRET`, when both are set and non-empty.
    pub fn env_client(&self, spec: &OAuthSpec) -> Option<OAuthClient> {
        let p = spec.provider.to_uppercase();
        let id = self.conns.env(&format!("STEVE_OAUTH_{p}_CLIENT_ID"))?;
        let secret = self.conns.env(&format!("STEVE_OAUTH_{p}_CLIENT_SECRET"))?;
        (!id.is_empty() && !secret.is_empty()).then(|| OAuthClient {
            id,
            secret: Zeroizing::new(secret),
        })
    }

    pub fn connections(&self) -> &Connections {
        self.conns
    }

    /// A variable of the Hub's environment (what [`ConnectionProvider::check_config`] allows, for
    /// example an endpoint host list).
    ///
    /// [`ConnectionProvider::check_config`]: super::ConnectionProvider::check_config
    pub fn var(&self, key: &str) -> Option<String> {
        self.conns.env(key)
    }
}
