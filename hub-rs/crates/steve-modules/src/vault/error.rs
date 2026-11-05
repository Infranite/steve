//! Vault errors. Like [`ApiError`] (body `{"error":"<中文>"}`), plus `503` for a key store that can't be
//! read right now. No variant carries key material or plaintext.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

use super::crypto::CryptoError;
use super::keystore::KeyStoreError;
use crate::ApiError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum VaultError {
    /// `400`: nothing to read or rotate yet.
    NotInitialized,
    /// `409`: `POST /vault/init` twice.
    AlreadyInitialized,
    /// `409`: the key store already has a different key under the name we would write. Never
    /// overwritten: it may belong to another copy of the database.
    KeyExists,
    /// `409`: a rotation is still re-wrapping data keys.
    Rotating,
    /// `400`: bad format, checksum, or no `vault_keys` row with that fingerprint.
    BadRecoveryCode,
    /// `503`.
    Locked,
    /// `503`: the key is gone from the key store (new machine, keychain reset); recover with the code.
    KeyMissing,
    /// `503`: the keychain refused this binary.
    Denied,
    /// `401`: registering a passkey needs a login from the last 10 minutes.
    StaleLogin,
    Api(ApiError),
}

/// The message in the request's language (zh-CN: "保险箱还没有启用").
impl std::fmt::Display for VaultError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        use steve_i18n::t;
        let m = match self {
            Self::NotInitialized => t!("modules.vault.not_initialized"),
            Self::AlreadyInitialized => t!("modules.vault.already_initialized"),
            Self::KeyExists => t!("modules.vault.key_exists"),
            Self::Rotating => t!("modules.vault.rotating"),
            Self::BadRecoveryCode => t!("modules.vault.bad_recovery_code"),
            Self::Locked => t!("modules.vault.locked"),
            Self::KeyMissing => t!("modules.vault.key_missing"),
            Self::Denied => t!("modules.vault.denied"),
            Self::StaleLogin => t!("modules.vault.stale_login"),
            Self::Api(e) => return e.fmt(f),
        };
        f.write_str(&m)
    }
}

impl std::error::Error for VaultError {}

impl VaultError {
    pub fn status(&self) -> u16 {
        match self {
            Self::NotInitialized | Self::BadRecoveryCode => 400,
            Self::AlreadyInitialized | Self::KeyExists | Self::Rotating => 409,
            Self::StaleLogin => 401,
            Self::Locked | Self::KeyMissing | Self::Denied => 503,
            Self::Api(e) => e.status(),
        }
    }

    pub(crate) fn internal(msg: impl Into<String>) -> Self {
        Self::Api(ApiError::Internal(msg.into()))
    }
}

impl From<ApiError> for VaultError {
    fn from(e: ApiError) -> Self {
        Self::Api(e)
    }
}

impl From<steve_store::StoreError> for VaultError {
    fn from(e: steve_store::StoreError) -> Self {
        Self::Api(e.into())
    }
}

impl From<rusqlite::Error> for VaultError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Api(e.into())
    }
}

impl From<KeyStoreError> for VaultError {
    fn from(e: KeyStoreError) -> Self {
        match e {
            KeyStoreError::Locked => Self::Locked,
            KeyStoreError::Denied => Self::Denied,
            e => Self::internal(format!("key store: {e}")),
        }
    }
}

impl From<CryptoError> for VaultError {
    fn from(e: CryptoError) -> Self {
        Self::internal(format!("vault crypto: {e:?}"))
    }
}

impl IntoResponse for VaultError {
    fn into_response(self) -> Response {
        match self {
            Self::Api(e) => e.into_response(),
            e => {
                let code =
                    StatusCode::from_u16(e.status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
                (code, Json(json!({ "error": e.to_string() }))).into_response()
            }
        }
    }
}

pub type VaultResult<T> = Result<T, VaultError>;
