//! Vault errors. Like [`ApiError`] (body `{"error":"<中文>"}`), plus `503` for a key store that can't be
//! read right now. No variant carries key material or plaintext.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;

use super::crypto::CryptoError;
use super::keystore::KeyStoreError;
use crate::ApiError;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VaultError {
    /// `400`: nothing to read or rotate yet.
    #[error("保险箱还没有启用")]
    NotInitialized,
    /// `409`: `POST /vault/init` twice.
    #[error("保险箱已经启用过了")]
    AlreadyInitialized,
    /// `409`: the key store already has a different key under the name we would write. Never
    /// overwritten: it may belong to another copy of the database.
    #[error("钥匙串里已经有另一把同名的主密钥，没有覆盖它")]
    KeyExists,
    /// `409`: a rotation is still re-wrapping data keys.
    #[error("主密钥正在轮换，请稍后再试")]
    Rotating,
    /// `400`: bad format, checksum, or no `vault_keys` row with that fingerprint.
    #[error("恢复码不对")]
    BadRecoveryCode,
    /// `503` (§3.4).
    #[error("保险箱暂时打不开：Mac mini 的钥匙串被锁了")]
    Locked,
    /// `503`: the key is gone from the key store (new machine, keychain reset); recover with the code.
    #[error("保险箱暂时打不开：钥匙串里没有主密钥，请用恢复码恢复")]
    KeyMissing,
    /// `503`: the keychain refused this binary.
    #[error("保险箱暂时打不开：钥匙串没有授权这个版本的 Steve 读取主密钥（发布时没有用固定签名）")]
    Denied,
    /// `401` (§4.2): registering a passkey needs a login from the last 10 minutes.
    #[error("请重新登录后再登记")]
    StaleLogin,
    #[error("{0}")]
    Api(ApiError),
}

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
