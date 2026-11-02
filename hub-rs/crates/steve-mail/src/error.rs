//! Errors of the mail module: [`MailError`] for the local mail store and the jobs, [`AdapterError`]
//! for talking to a provider.

use steve_modules::ApiError;
use steve_modules::accounts::connections::ProviderError;
use steve_modules::vault::VaultError;
use steve_store::StoreError;

/// What went wrong talking to a mail provider. Messages are short Chinese summaries meant for the
/// user (they end up in `400 验证没有通过：<原因>` and in the connection's health); never a password,
/// token or a server response that could carry one.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AdapterError {
    /// The server refused the credential: the connection needs `reauth`.
    #[error("{0}")]
    Auth(String),
    /// The server answered but refused (encryption not offered, folder missing).
    #[error("{0}")]
    Rejected(String),
    /// Network, timeout, dropped connection: retried with backoff.
    #[error("{0}")]
    Transient(String),
    /// Steve's side is not set up (missing field, vault locked).
    #[error("{0}")]
    Config(String),
    /// The message is no longer on the server (`refetch`).
    #[error("{0}")]
    Gone(String),
    /// The local store failed while the adapter fed it.
    #[error("{0}")]
    Store(String),
    #[error("这个接入方式不支持这个操作")]
    Unsupported,
}

impl From<AdapterError> for ProviderError {
    fn from(e: AdapterError) -> Self {
        match e {
            AdapterError::Auth(m) => Self::Auth(m),
            AdapterError::Rejected(m) | AdapterError::Gone(m) => Self::Rejected(m),
            AdapterError::Transient(m) | AdapterError::Store(m) => Self::Transient(m),
            AdapterError::Config(m) => Self::Config(m),
            AdapterError::Unsupported => Self::Unsupported,
        }
    }
}

impl From<ProviderError> for AdapterError {
    fn from(e: ProviderError) -> Self {
        match e {
            ProviderError::Auth(m) => Self::Auth(m),
            ProviderError::Rejected(m) => Self::Rejected(m),
            ProviderError::Transient(m) => Self::Transient(m),
            ProviderError::Config(m) => Self::Config(m),
            ProviderError::Unsupported => Self::Unsupported,
        }
    }
}

impl From<MailError> for AdapterError {
    fn from(e: MailError) -> Self {
        Self::Store(e.to_string())
    }
}

/// Errors of the local mail store and the jobs.
#[derive(Debug, thiserror::Error)]
pub enum MailError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error("sqlite: {0}")]
    Sql(#[from] rusqlite::Error),
    #[error(transparent)]
    Api(#[from] ApiError),
    #[error("io: {0}")]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Adapter(#[from] AdapterError),
    #[error("{0}")]
    Other(String),
}

impl From<VaultError> for MailError {
    fn from(e: VaultError) -> Self {
        match e {
            VaultError::Api(a) => Self::Api(a),
            e => Self::Other(e.to_string()),
        }
    }
}

impl From<MailError> for VaultError {
    fn from(e: MailError) -> Self {
        match e {
            MailError::Api(a) => VaultError::Api(a),
            e => VaultError::Api(ApiError::Internal(e.to_string())),
        }
    }
}

pub type MailResult<T> = Result<T, MailError>;
