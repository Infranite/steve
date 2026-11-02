use thiserror::Error;

/// Errors of the domain layer. `Msg` carries the user-facing Chinese text the API returns verbatim as
/// `{"error": msg}` (the TS Hub serves these as 500); the others are infrastructure failures.
#[derive(Debug, Error)]
pub enum CoreError {
    #[error("{0}")]
    Msg(String),
    #[error(transparent)]
    Store(#[from] steve_store::StoreError),
    #[error(transparent)]
    Db(#[from] rusqlite::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    /// A module refused or failed (e.g. a module decision's `before_undo`); the API serves it with its
    /// own status (`409 邮件已经发出，不能撤回`).
    #[error(transparent)]
    Module(#[from] steve_modules::ApiError),
    /// A refusal with its own status (`404 没有这条消息`, `409 已经处理过了`).
    #[error("{1}")]
    Status(u16, String),
}

pub type Result<T> = std::result::Result<T, CoreError>;

/// A user-facing error with a fixed message.
pub(crate) fn msg(s: impl Into<String>) -> CoreError {
    CoreError::Msg(s.into())
}
