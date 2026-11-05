//! API errors and their responses (api.ts `onError`, Hono defaults).

use axum::http::{StatusCode, header};
use axum::response::{IntoResponse, Response};
use serde_json::json;

use crate::backend::BackendError;

/// Every failure a handler can return, mapped to the TS Hub's status and body.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ApiError {
    /// zod validation: `400 {"error":"参数不对：<path>、<path>"}`, in the request's language.
    #[error("{}", invalid_message(.0))]
    Invalid(Vec<String>),
    /// `{"error": msg}` with a status (explicit `c.json({error}, n)`, `own()` 404s, thrown errors as 500).
    #[error("{1}")]
    Json(u16, String),
    /// A plain-text response (`404 Not Found`, `Internal Server Error`).
    #[error("{1}")]
    Text(u16, String),
}

impl ApiError {
    pub fn bad_request(msg: impl Into<String>) -> Self {
        ApiError::Json(400, msg.into())
    }

    pub fn not_found(msg: impl Into<String>) -> Self {
        ApiError::Json(404, msg.into())
    }

    /// A thrown `Error(msg)`.
    pub fn internal(msg: impl Into<String>) -> Self {
        ApiError::Json(500, msg.into())
    }

    /// Hono's default not-found response.
    pub fn no_route() -> Self {
        ApiError::Text(404, "404 Not Found".into())
    }
}

impl From<BackendError> for ApiError {
    fn from(e: BackendError) -> Self {
        ApiError::Json(e.status, e.message)
    }
}

/// "参数不对：a、b" / "Invalid parameters: a, b".
fn invalid_message(paths: &[String]) -> String {
    let fields = paths.join(&steve_i18n::t!("hub.error.list_sep"));
    steve_i18n::t!("hub.error.invalid", fields = fields)
}

fn status(n: u16) -> StatusCode {
    StatusCode::from_u16(n).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR)
}

/// `c.json(v)`: Hono sends `application/json` without a charset.
pub fn json_response(code: u16, body: &serde_json::Value) -> Response {
    let text = serde_json::to_string(body).unwrap_or_else(|_| "null".into());
    (
        status(code),
        [(header::CONTENT_TYPE, "application/json")],
        text,
    )
        .into_response()
}

/// `c.text(s, n)`.
pub fn text_response(code: u16, body: impl Into<String>) -> Response {
    (
        status(code),
        [(header::CONTENT_TYPE, "text/plain; charset=UTF-8")],
        body.into(),
    )
        .into_response()
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        match self {
            ApiError::Invalid(_) => json_response(400, &json!({ "error": self.to_string() })),
            ApiError::Json(code, msg) => json_response(code, &json!({ "error": msg })),
            ApiError::Text(code, msg) => text_response(code, msg),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invalid_message_format() {
        assert_eq!(
            ApiError::Invalid(vec!["text".into()]).to_string(),
            "参数不对：text"
        );
        assert_eq!(
            ApiError::Invalid(vec!["name".into(), "avatar.color".into()]).to_string(),
            "参数不对：name、avatar.color"
        );
        assert_eq!(
            ApiError::Invalid(vec![String::new()]).to_string(),
            "参数不对："
        );
    }

    #[tokio::test]
    async fn invalid_message_in_english() {
        let e = ApiError::Invalid(vec!["name".into(), "avatar.color".into()]);
        let en = steve_i18n::scope(steve_i18n::EN, async { e.to_string() }).await;
        assert_eq!(en, "Invalid parameters: name, avatar.color");
        assert_eq!(e.to_string(), "参数不对：name、avatar.color");
    }

    #[test]
    fn backend_errors_keep_status() {
        assert_eq!(
            ApiError::from(BackendError::msg("这个决定已经处理过了")),
            ApiError::Json(500, "这个决定已经处理过了".into())
        );
        assert_eq!(
            ApiError::from(BackendError::not_found("没有这个事项")),
            ApiError::Json(404, "没有这个事项".into())
        );
    }
}
