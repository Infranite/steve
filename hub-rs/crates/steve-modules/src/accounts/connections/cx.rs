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

/// Back-off for connections whose credential the vault could not open because the key store can't
/// be read now (a locked keychain, a missing key, a refused binary): a retry loop meets the same
/// error without asking the key store or writing a use row, 30 s after the first failure, doubling
/// up to 5 minutes. A use that works clears it.
#[derive(Default)]
pub(crate) struct VaultWait(std::collections::HashMap<String, (i64, u32, VaultError)>);

const VAULT_WAIT_MS: i64 = 30_000;
const VAULT_WAIT_MAX_MS: i64 = 300_000;

impl VaultWait {
    /// The error to answer with instead of asking the vault, while `conn` waits.
    pub(crate) fn waiting(&self, conn: &str, now: i64) -> Option<VaultError> {
        self.0
            .get(conn)
            .filter(|(until, _, _)| now < *until)
            .map(|(_, _, e)| e.clone())
    }

    /// What a use came to.
    pub(crate) fn record<R>(&mut self, conn: &str, now: i64, r: &Result<R, VaultError>) {
        match r {
            Err(e @ (VaultError::Locked | VaultError::KeyMissing | VaultError::Denied)) => {
                let fails = self.0.get(conn).map_or(0, |(_, f, _)| *f) + 1;
                let wait = (VAULT_WAIT_MS << (fails - 1).min(8)).min(VAULT_WAIT_MAX_MS);
                self.0
                    .insert(conn.to_string(), (now + wait, fails, e.clone()));
            }
            Ok(_) => {
                self.0.remove(conn);
            }
            Err(_) => {}
        }
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
        let now = steve_store::now();
        if let Some(e) = self.conns.vault_wait().waiting(&self.id, now) {
            return Err(vault_err(e));
        }
        let r = self
            .conns
            .vault()
            .with_credential(&self.ws, item, &self.id, "连接", f);
        self.conns.vault_wait().record(&self.id, now, &r);
        r.map_err(vault_err)
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

#[cfg(test)]
mod vault_wait_tests {
    use super::*;

    #[test]
    fn a_locked_key_store_is_asked_again_only_after_a_growing_wait() {
        let mut w = VaultWait::default();
        let locked: Result<(), VaultError> = Err(VaultError::Locked);
        assert!(w.waiting("con_1", 0).is_none());
        w.record("con_1", 0, &locked);
        assert_eq!(w.waiting("con_1", 1_000), Some(VaultError::Locked));
        assert!(w.waiting("con_2", 1_000).is_none(), "per connection");
        assert!(w.waiting("con_1", 30_000).is_none());
        // the next failure waits twice as long, up to five minutes
        w.record("con_1", 30_000, &locked);
        assert!(w.waiting("con_1", 89_999).is_some());
        assert!(w.waiting("con_1", 90_000).is_none());
        for i in 0..20 {
            w.record("con_1", i, &locked);
        }
        assert!(w.waiting("con_1", 19 + 299_999).is_some());
        assert!(w.waiting("con_1", 19 + 300_000).is_none());
        // a use that works clears it; other errors don't start a wait
        w.record("con_1", 0, &Ok(()));
        assert!(w.waiting("con_1", 1).is_none());
        w.record("con_1", 0, &Err::<(), _>(VaultError::NotInitialized));
        assert!(w.waiting("con_1", 1).is_none());
        w.record("con_1", 0, &Err::<(), _>(VaultError::KeyMissing));
        assert_eq!(w.waiting("con_1", 1), Some(VaultError::KeyMissing));
    }
}
