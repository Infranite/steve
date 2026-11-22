//! API errors and their responses (api.ts `onError`, Hono defaults).
//!
//! Wave 1 adds the shared contract to plain `{"error": …}` JSON errors inside compression, including
//! direct `json!` replies and modules/vault/mail/flow error types. Their status, message and extra
//! fields stay intact.
//! Wave 4 moves domain handlers to `steve_types::Error`; this middleware then remains a fallback.
//! Plain text and non-error responses retain their existing transport behavior.
//! All fallback 401s share the unauthenticated code. Until Wave 4 distinguishes them with domain
//! reasons, clients read the `error`/`login` fields for session sign-in; module 401s stay errors.

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
    #[error(transparent)]
    Contract(#[from] steve_types::Error),
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
            ApiError::Contract(e) => {
                (status(e.code.http_status()), axum::Json(e.to_http())).into_response()
            }
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

/// Add the shared contract only to JSON error objects without one. This does not make retry decisions.
pub async fn error_contract(req: axum::extract::Request, next: axum::middleware::Next) -> Response {
    use axum::body::{Body, to_bytes};
    use steve_types::{Code, Retry};

    let response = next.run(req).await;
    // The outer compression layer owns encoding. Never inspect an already encoded body.
    if response.status().as_u16() < 400 || response.headers().contains_key(header::CONTENT_ENCODING)
    {
        return response;
    }
    let is_json = response
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .is_some_and(|v| {
            let v = v.trim().to_ascii_lowercase();
            v == "application/json" || (v.starts_with("application/") && v.ends_with("+json"))
        });
    if !is_json {
        return response;
    }
    let (mut parts, body) = response.into_parts();
    let bytes = match to_bytes(body, usize::MAX).await {
        Ok(bytes) => bytes,
        Err(error) => {
            return Response::from_parts(
                parts,
                Body::from_stream(futures::stream::once(async {
                    Err::<bytes::Bytes, _>(error)
                })),
            );
        }
    };
    let unchanged = |parts| Response::from_parts(parts, Body::from(bytes.clone()));
    let Ok(serde_json::Value::Object(mut object)) = serde_json::from_slice(&bytes) else {
        return unchanged(parts);
    };
    if !object
        .get("error")
        .is_some_and(serde_json::Value::is_string)
        || object.contains_key("code")
    {
        return unchanged(parts);
    }
    let code = Code::from_http_status(parts.status.as_u16());
    let retry = parts.headers.get(header::RETRY_AFTER).map_or_else(
        || code.default_retry(),
        |value| Retry::Later {
            after_ms: value.to_str().ok().and_then(retry_after_ms),
        },
    );
    // Existing extension fields (including login/token) always win.
    object.insert("code".into(), json!(code));
    object.entry("retry").or_insert_with(|| json!(retry));
    object.entry("reason").or_insert(serde_json::Value::Null);
    object.entry("params").or_insert_with(|| json!({}));
    let output = serde_json::to_vec(&object).expect("JSON object is serializable");
    // A transformed representation has a new length and no longer matches the original validator.
    parts.headers.remove(header::ETAG);
    parts.headers.remove(header::CONTENT_LENGTH);
    parts
        .headers
        .insert(header::CONTENT_LENGTH, output.len().into());
    Response::from_parts(parts, Body::from(output))
}

/// Hub retry hints use nonnegative integer seconds; other formats leave the delay unknown.
fn retry_after_ms(value: &str) -> Option<u64> {
    let value = value.trim();
    if value.is_empty() || !value.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    value.parse::<u64>().ok().map(|s| s.saturating_mul(1000))
}

#[cfg(test)]
mod contract_tests {
    use super::*;
    use axum::{
        Router,
        body::{Body, to_bytes},
        http::Request,
        middleware,
        routing::get,
    };
    use serde_json::Value;
    use tower::ServiceExt;

    async fn normalize(response: Response) -> Response {
        let response = std::sync::Arc::new(tokio::sync::Mutex::new(Some(response)));
        Router::new()
            .route(
                "/",
                get(move || {
                    let response = response.clone();
                    async move { response.lock().await.take().unwrap() }
                }),
            )
            .layer(middleware::from_fn(error_contract))
            .oneshot(Request::get("/").body(Body::empty()).unwrap())
            .await
            .unwrap()
    }

    async fn json(response: Response) -> Value {
        serde_json::from_slice(&to_bytes(response.into_body(), usize::MAX).await.unwrap()).unwrap()
    }

    #[tokio::test]
    async fn adds_every_http_fallback_without_changing_status_or_message() {
        for status in 400..=599 {
            let response = normalize(json_response(status, &json!({"error": "safe detail"}))).await;
            assert_eq!(response.status().as_u16(), status);
            assert_eq!(
                json(response).await,
                json!({
                    "error": "safe detail", "code": steve_types::Code::from_http_status(status),
                    "retry": steve_types::Code::from_http_status(status).default_retry(),
                    "reason": null, "params": {}
                })
            );
        }
    }

    #[tokio::test]
    async fn keeps_login_token_extra_fields_and_retry_after() {
        let mut response = json_response(
            401,
            &json!({"error":"unauthenticated", "login":"/auth/start", "token":true}),
        );
        response
            .headers_mut()
            .insert(header::RETRY_AFTER, "2".parse().unwrap());
        response
            .headers_mut()
            .insert("x-fixture", "kept".parse().unwrap());
        let response = normalize(response).await;
        assert_eq!(response.headers()["x-fixture"], "kept");
        assert_eq!(response.headers()[header::RETRY_AFTER], "2");
        let length = response.headers()[header::CONTENT_LENGTH]
            .to_str()
            .unwrap()
            .parse::<usize>()
            .unwrap();
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        assert_eq!(length, bytes.len());
        assert_eq!(
            serde_json::from_slice::<Value>(&bytes).unwrap(),
            json!({
                "error":"unauthenticated", "login":"/auth/start", "token":true,
                "code":"unauthenticated", "retry":{"kind":"later", "after_ms":2000},
                "reason":null, "params":{}
            })
        );
    }

    #[tokio::test]
    async fn existing_contract_and_non_error_bodies_are_byte_identical() {
        for (status, content_type, text) in [
            (
                400,
                "application/json",
                r#"{ "error":"x", "code":"conflict", "retry":{"kind":"now"}, "reason":null, "params":{"x":1} }"#,
            ),
            (404, "text/plain", "404 Not Found"),
            (500, "text/html", "<p>Internal Server Error</p>"),
            (400, "application/json", "not json"),
            (400, "application/json", r#"{"error":42}"#),
            (400, "application/json", r#"{"message":"x"}"#),
            (400, "application/json", r#"[{"error":"x"}]"#),
            (200, "application/json", r#"{"error":"x"}"#),
        ] {
            let response = normalize(
                (
                    StatusCode::from_u16(status).unwrap(),
                    [(header::CONTENT_TYPE, content_type)],
                    text,
                )
                    .into_response(),
            )
            .await;
            assert_eq!(response.status().as_u16(), status);
            assert_eq!(response.headers()[header::CONTENT_TYPE], content_type);
            assert_eq!(
                to_bytes(response.into_body(), usize::MAX).await.unwrap(),
                text
            );
        }
        let response = normalize(json_response(
            400,
            &json!({"error":"x", "params":{"x":1}, "reason":"existing", "retry":{"kind":"now"}}),
        ))
        .await;
        let body = json(response).await;
        assert_eq!(body["params"], json!({"x":1}));
        assert_eq!(body["reason"], "existing");
        assert_eq!(body["retry"], json!({"kind":"now"}));
    }

    #[tokio::test]
    async fn encoded_responses_are_byte_identical() {
        let text = r#"{ "error": "already encoded" }"#;
        for encoding in ["gzip", "br", "identity"] {
            let response = normalize(
                (
                    StatusCode::SERVICE_UNAVAILABLE,
                    [
                        (header::CONTENT_TYPE, "application/json"),
                        (header::CONTENT_ENCODING, encoding),
                    ],
                    text,
                )
                    .into_response(),
            )
            .await;
            assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
            assert_eq!(response.headers()[header::CONTENT_ENCODING], encoding);
            assert_eq!(
                to_bytes(response.into_body(), usize::MAX).await.unwrap(),
                text
            );
        }
    }

    #[tokio::test]
    async fn unrecognized_retry_after_still_means_later_with_unknown_delay() {
        for hint in ["Mon, 20 Jul 2026 00:00:03 GMT", "not a delay"] {
            let mut response = json_response(400, &json!({"error":"safe detail"}));
            response
                .headers_mut()
                .insert(header::RETRY_AFTER, hint.parse().unwrap());
            let body = json(normalize(response).await).await;
            assert_eq!(body["code"], "invalid");
            assert_eq!(body["retry"], json!({"kind":"later", "after_ms":null}));
        }
    }

    #[test]
    fn retry_after_only_accepts_unsigned_integer_seconds() {
        assert_eq!(retry_after_ms("0"), Some(0));
        assert_eq!(retry_after_ms("3"), Some(3000));
        assert_eq!(retry_after_ms(" 3 "), Some(3000));
        for invalid in [
            "",
            "-1",
            "+1",
            "3.5",
            "nonsense",
            "Mon, 20 Jul 2026 00:00:03 GMT",
        ] {
            assert_eq!(retry_after_ms(invalid), None, "{invalid}");
        }
    }
}
