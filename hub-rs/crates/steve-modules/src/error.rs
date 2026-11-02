//! Errors of the new module APIs (contacts.md §0.4). Body `{"error":"<中文>"}`; unlike the old API,
//! domain errors are never 500: validation and rule refusals are 400, owner-only actions 403,
//! missing/foreign/invisible 404, state conflicts 409.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;
use steve_store::StoreError;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ApiError {
    /// `400 参数不对：<field>、<field>` (zod-style dotted paths).
    #[error("参数不对：{}", .0.join("、"))]
    Invalid(Vec<String>),
    /// `404 没有这个<noun>`: not there, in another workspace, or not visible to the caller. A noun
    /// in Latin letters gets a space, as the TS Hub writes it (“没有这个 Bot”).
    #[error("没有这个{}{}", noun_gap(.0), .0)]
    NotFound(String),
    /// `409`: already handled, already merged, duplicate name, stale draft.
    #[error("{0}")]
    Conflict(String),
    /// `400`: a domain rule refused (“不能删除你自己”).
    #[error("{0}")]
    Rule(String),
    /// `403`: allowed for someone else (“只有工作空间的所有者能改内置来源”).
    #[error("{0}")]
    Forbidden(String),
    /// `401`: no caller on the request (the integrator's auth layer is missing).
    #[error("unauthenticated")]
    Unauthenticated,
    /// `500`: storage or a bug. The message is logged, the client gets a generic one.
    #[error("{0}")]
    Internal(String),
}

/// The space between “没有这个” and a noun in Latin letters.
fn noun_gap(noun: &str) -> &'static str {
    if noun.starts_with(|c: char| c.is_ascii_alphanumeric()) {
        " "
    } else {
        ""
    }
}

impl ApiError {
    pub fn invalid(field: impl Into<String>) -> Self {
        Self::Invalid(vec![field.into()])
    }
    pub fn not_found(noun: impl Into<String>) -> Self {
        Self::NotFound(noun.into())
    }
    pub fn conflict(msg: impl Into<String>) -> Self {
        Self::Conflict(msg.into())
    }
    pub fn rule(msg: impl Into<String>) -> Self {
        Self::Rule(msg.into())
    }
    pub fn forbidden(msg: impl Into<String>) -> Self {
        Self::Forbidden(msg.into())
    }

    pub fn status(&self) -> u16 {
        match self {
            Self::Invalid(_) | Self::Rule(_) => 400,
            Self::Unauthenticated => 401,
            Self::Forbidden(_) => 403,
            Self::NotFound(_) => 404,
            Self::Conflict(_) => 409,
            Self::Internal(_) => 500,
        }
    }
}

impl From<StoreError> for ApiError {
    fn from(e: StoreError) -> Self {
        match e {
            // the store's own “没有这个…” messages are complete sentences already
            StoreError::NotFound(m) => Self::NotFound(m.trim_start_matches("没有这个").to_string()),
            e => Self::Internal(e.to_string()),
        }
    }
}

impl From<rusqlite::Error> for ApiError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Internal(e.to_string())
    }
}

impl From<serde_json::Error> for ApiError {
    fn from(e: serde_json::Error) -> Self {
        Self::Internal(e.to_string())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let code = StatusCode::from_u16(self.status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let msg = match &self {
            Self::Internal(m) => {
                tracing::error!("module api: {m}");
                "出了点问题，请稍后再试".to_string()
            }
            e => e.to_string(),
        };
        (code, Json(json!({ "error": msg }))).into_response()
    }
}

pub type ApiResult<T> = Result<T, ApiError>;
