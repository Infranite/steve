//! One resource of a message, `GET /open-apis/im/v1/messages/:message_id/resources/:file_key
//! ?type=image|file` (message-resource/get): as the connection's own identity (the bot's tenant
//! token, your user token on a self connection), to the connection's own API base, no redirects (the
//! shared client follows none), the body read in chunks and dropped past `max_bytes`.
//!
//! Only [`crate::media`] calls this, with a key and type it read from the stored message.

use reqwest::StatusCode;
use serde_json::Value;
use steve_modules::accounts::connections::ConnCx;

use super::client::{self, FeishuClient, TokenSource};
use super::ratelimit::Limiter;
use crate::media::MediaError;

/// Resource downloads per second of one connection (message-resource/get: 50/s, 1000/min).
pub const PER_SEC: u32 = 50;
/// An error envelope is read up to this; a longer body is not one.
const ENVELOPE_MAX: u64 = 64 * 1024;

/// The answer's headers must come within this (the whole fetch has the caller's limit).
const CONNECT: std::time::Duration = std::time::Duration::from_secs(10);

/// The bytes of `key` in message `ext_msg_id`; `image` picks `type=image`, else `type=file`.
pub async fn download(
    cx: &ConnCx,
    ext_msg_id: &str,
    key: &str,
    image: bool,
    max_bytes: u64,
) -> Result<Vec<u8>, MediaError> {
    let path = format!(
        "/open-apis/im/v1/messages/{}/resources/{}",
        seg(ext_msg_id)?,
        seg(key)?
    );
    let c = FeishuClient::for_cx(cx);
    let url = format!("{}{path}", c.base());
    client::guard_url(&url).map_err(|_| MediaError::Provider)?;
    let kind = if image { "image" } else { "file" };
    let mut fresh = false;
    let bucket = format!("res:{}", cx.id());
    loop {
        Limiter::global().acquire(&[(&bucket, PER_SEC)]).await;
        let bearer = cx.bearer().await.map_err(|_| MediaError::NoPermission)?;
        let res = c
            .http()
            .get(&url)
            .query(&[("type", kind)])
            .header(reqwest::header::AUTHORIZATION, bearer.as_str())
            .send();
        let res = tokio::time::timeout(CONNECT, res)
            .await
            .map_err(|_| MediaError::Timeout)?
            .map_err(|e| {
                if e.is_timeout() {
                    MediaError::Timeout
                } else {
                    MediaError::Provider
                }
            })?;
        let status = res.status();
        let json = res
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|t| t.trim_start().starts_with("application/json"));
        let err = if status.is_success() {
            let bytes = body(res, max_bytes).await?;
            // a 2xx JSON `{code ≠ 0, msg}` is a refusal, not the file (never shown as one)
            match json.then(|| envelope(&bytes)).flatten() {
                Some(v) => refusal(status, &v),
                None => return Ok(bytes),
            }
        } else {
            let v = body(res, ENVELOPE_MAX)
                .await
                .ok()
                .and_then(|b| serde_json::from_slice(&b).ok())
                .unwrap_or(Value::Null);
            refusal(status, &v)
        };
        if matches!(err, Refusal::Token) {
            if !fresh {
                cx.invalidate().await;
                fresh = true;
                continue;
            }
            cx.refused_again(0).await;
        }
        return Err(err.into());
    }
}

/// A path segment: Feishu ids and keys are `[A-Za-z0-9_-]`; anything else is refused before a
/// request is made.
fn seg(s: &str) -> Result<&str, MediaError> {
    let ok = !s.is_empty()
        && s.len() <= 256
        && s.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-');
    ok.then_some(s).ok_or(MediaError::NoKey)
}

async fn body(mut res: reqwest::Response, max: u64) -> Result<Vec<u8>, MediaError> {
    if res.content_length().is_some_and(|n| n > max) {
        return Err(MediaError::TooLarge);
    }
    let mut out = Vec::new();
    while let Some(chunk) = res.chunk().await.map_err(|e| {
        if e.is_timeout() {
            MediaError::Timeout
        } else {
            MediaError::Provider
        }
    })? {
        if out.len() as u64 + chunk.len() as u64 > max {
            return Err(MediaError::TooLarge);
        }
        out.extend_from_slice(&chunk);
    }
    Ok(out)
}

enum Refusal {
    Token,
    Mapped(MediaError),
}

impl From<Refusal> for MediaError {
    fn from(r: Refusal) -> Self {
        match r {
            Refusal::Token => MediaError::NoPermission,
            Refusal::Mapped(e) => e,
        }
    }
}

/// An error envelope: a small JSON object with an integer `code` other than 0.
fn envelope(b: &[u8]) -> Option<Value> {
    if b.len() as u64 > ENVELOPE_MAX {
        return None;
    }
    let v: Value = serde_json::from_slice(b).ok()?;
    v["code"].as_i64().filter(|c| *c != 0).map(|_| v)
}

/// A refused download (an error envelope `{code, msg}`; the provider's `msg` is not passed on).
fn refusal(status: StatusCode, v: &Value) -> Refusal {
    let code = v["code"].as_i64();
    Refusal::Mapped(match code {
        Some(c) if client::TOKEN_INVALID_CODES.contains(&c) => return Refusal::Token,
        Some(c) if client::NO_SCOPE_CODES.contains(&c) => MediaError::NoPermission,
        Some(c) if client::RATE_LIMIT_CODES.contains(&c) => MediaError::RateLimited,
        Some(234002) => MediaError::NoPermission,
        Some(234003) => MediaError::Gone,
        Some(234004) => MediaError::NotInChat,
        Some(234037) => MediaError::TooLarge,
        Some(234038) | Some(234009) => MediaError::Restricted,
        Some(234040) => MediaError::Invisible,
        Some(234043) => MediaError::Unsupported,
        _ if status == StatusCode::TOO_MANY_REQUESTS => MediaError::RateLimited,
        _ if status == StatusCode::UNAUTHORIZED && code.is_none() => return Refusal::Token,
        _ if status == StatusCode::NOT_FOUND => MediaError::Gone,
        _ => MediaError::Provider,
    })
}
