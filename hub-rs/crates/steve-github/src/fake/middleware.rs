//! Cross-cutting behaviour of api.github.com: call recording, injected
//! faults, rate-limit headers, and ETag / `If-None-Match` → 304.

use axum::body::{Body, to_bytes};
use axum::extract::{Request, State};
use axum::http::{HeaderValue, Method, StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::model::{Call, Fault};
use super::routes::{AppState, DOCS};
use super::time::now;

const BODY_LIMIT: usize = 8 << 20;

/// Requests that don't count against the rate limit on github.com.
fn uncounted(path: &str) -> bool {
    path.starts_with("/_fake/") || path == "/rate_limit" || path.starts_with("/app-manifests/")
}

pub async fn layer(State(s): State<AppState>, req: Request, next: Next) -> Response {
    let (parts, body) = req.into_parts();
    let bytes = to_bytes(body, BODY_LIMIT).await.unwrap_or_default();
    let method = parts.method.clone();
    let path = parts.uri.path().to_string();
    let query = parts.uri.query().unwrap_or_default().to_string();
    let header_str = |name: header::HeaderName| {
        parts
            .headers
            .get(name)
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
    };
    let authorization = header_str(header::AUTHORIZATION);
    let if_none_match = header_str(header::IF_NONE_MATCH);

    let (fault, exhausted) = {
        let mut w = s.lock();
        let pos = w
            .faults
            .iter()
            .position(|f| f.path_contains.as_deref().is_none_or(|p| path.contains(p)));
        let fault = pos.and_then(|i| w.faults.remove(i)).map(|f| f.fault);
        if w.rate.reset <= now() {
            w.rate.remaining = w.rate.limit;
            w.rate.reset = now() + 3600;
        }
        (fault, w.rate.remaining == 0 && !uncounted(&path))
    };

    let mut resp = if let Some(Fault::LostAnswer) = fault {
        next.run(Request::from_parts(parts, Body::from(bytes.clone())))
            .await;
        StatusCode::BAD_GATEWAY.into_response()
    } else if let Some(f) = fault {
        fault_response(&s, f)
    } else if exhausted {
        fault_response(&s, Fault::RateLimited)
    } else {
        next.run(Request::from_parts(parts, Body::from(bytes.clone())))
            .await
    };

    if method == Method::GET && resp.status() == StatusCode::OK {
        resp = etag(resp, if_none_match.as_deref()).await;
    }

    let status = resp.status().as_u16();
    {
        let mut w = s.lock();
        // 304s and requests rejected by a rate limit are not counted.
        if !uncounted(&path)
            && status != 304
            && !exhausted
            && w.rate.remaining > 0
            && authorization.is_some()
        {
            w.rate.remaining -= 1;
        }
        if !uncounted(&path) {
            let r = w.rate.clone();
            let h = resp.headers_mut();
            for (k, v) in [
                ("x-ratelimit-limit", r.limit.to_string()),
                ("x-ratelimit-remaining", r.remaining.to_string()),
                ("x-ratelimit-reset", r.reset.to_string()),
                ("x-ratelimit-used", (r.limit - r.remaining).to_string()),
                ("x-ratelimit-resource", "core".to_string()),
            ] {
                // Fault responses may already carry their own values.
                if !h.contains_key(k)
                    && let Ok(v) = HeaderValue::from_str(&v)
                {
                    h.insert(k, v);
                }
            }
        }
        w.calls.push(Call {
            method: method.to_string(),
            path,
            query,
            authorization,
            if_none_match,
            body: serde_json::from_slice(&bytes).unwrap_or(Value::Null),
            status,
        });
    }
    resp
}

/// Weak ETag over the body; a matching `If-None-Match` turns it into a 304.
async fn etag(resp: Response, if_none_match: Option<&str>) -> Response {
    let (mut parts, body) = resp.into_parts();
    let Ok(bytes) = to_bytes(body, BODY_LIMIT).await else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let tag = format!("W/\"{}\"", hex::encode(Sha256::digest(&bytes)));
    let matches =
        if_none_match.is_some_and(|inm| inm.split(',').any(|t| t.trim() == tag || t.trim() == "*"));
    if let Ok(v) = HeaderValue::from_str(&tag) {
        parts.headers.insert(header::ETAG, v);
    }
    if matches {
        parts.status = StatusCode::NOT_MODIFIED;
        parts.headers.remove(header::CONTENT_TYPE);
        parts.headers.remove(header::CONTENT_LENGTH);
        return Response::from_parts(parts, Body::empty());
    }
    Response::from_parts(parts, Body::from(bytes))
}

fn fault_response(s: &AppState, f: Fault) -> Response {
    let body = |status: u16, msg: &str| json!({"message": msg, "documentation_url": DOCS, "status": status.to_string()});
    match f {
        Fault::RateLimited => {
            let (reset, limit) = {
                let w = s.lock();
                (w.rate.reset, w.rate.limit)
            };
            let mut r = (
                StatusCode::FORBIDDEN,
                axum::Json(body(403, "API rate limit exceeded for installation.")),
            )
                .into_response();
            let h = r.headers_mut();
            h.insert("x-ratelimit-remaining", HeaderValue::from_static("0"));
            h.insert("x-ratelimit-used", HeaderValue::from(limit));
            h.insert("x-ratelimit-reset", HeaderValue::from(reset));
            r
        }
        Fault::RetryAfter { status, seconds } => {
            let code = StatusCode::from_u16(status).unwrap_or(StatusCode::TOO_MANY_REQUESTS);
            let msg = "You have exceeded a secondary rate limit. Please wait a few minutes before you try again.";
            let mut r = (code, axum::Json(body(status, msg))).into_response();
            r.headers_mut()
                .insert(header::RETRY_AFTER, HeaderValue::from(seconds));
            r
        }
        Fault::BadCredentials => (
            StatusCode::UNAUTHORIZED,
            axum::Json(body(401, "Bad credentials")),
        )
            .into_response(),
        Fault::Status { status, message } => {
            let code = StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
            (code, axum::Json(body(status, &message))).into_response()
        }
        // carried out by the caller, which only uses this for the status
        Fault::LostAnswer => StatusCode::BAD_GATEWAY.into_response(),
    }
}
