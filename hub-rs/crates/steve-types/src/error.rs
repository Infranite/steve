//! Shared error contract. Transport and provider conversions belong to their own crates.

use serde::{Deserialize, Serialize};
use std::{collections::BTreeMap, fmt};

/// The command owner enforces this budget, then falls back to Later with the same operation ID.
pub const NOW_MAX_ATTEMPTS: u8 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum Code {
    Invalid,
    Unauthenticated,
    Forbidden,
    NotFound,
    Conflict,
    Stale,
    Gap,
    TooLarge,
    NotConfigured,
    Unsupported,
    Busy,
    RateLimited,
    Exhausted,
    Unavailable,
    Timeout,
    OutcomeUnknown,
    Incompatible,
    Internal,
}

impl Code {
    pub const fn http_status(self) -> u16 {
        match self {
            Self::Invalid => 400,
            Self::Unauthenticated => 401,
            Self::Forbidden => 403,
            Self::NotFound => 404,
            Self::Conflict => 409,
            Self::Stale => 409,
            Self::Gap => 409,
            Self::TooLarge => 413,
            Self::NotConfigured => 409,
            Self::Unsupported => 501,
            Self::Busy => 409,
            Self::RateLimited => 429,
            Self::Exhausted => 507,
            Self::Unavailable => 503,
            Self::Timeout => 504,
            Self::OutcomeUnknown => 502,
            Self::Incompatible => 500,
            Self::Internal => 500,
        }
    }

    /// Used only when the upstream did not supply an explicit retry classification.
    pub const fn default_retry(self) -> Retry {
        match self {
            Self::Busy
            | Self::RateLimited
            | Self::Exhausted
            | Self::Unavailable
            | Self::Timeout => Retry::Later { after_ms: None },
            Self::OutcomeUnknown => Retry::Now,
            _ => Retry::Never,
        }
    }
}

/// Retries preserve the original intent/operation ID; intermediate layers only pass this through.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum Retry {
    Never,
    Later { after_ms: Option<u64> },
    Now,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum Param {
    Str(String),
    Int(i64),
}

/// Ordered by key for stable wire output. Values are display arguments, never parsed messages.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct Params(pub BTreeMap<String, Param>);

/// Wave 4 adds domain-owned reasons. Wave 1 deliberately has no reason variants.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub enum Reason {}

/// Exported to TypeScript as `SteveError`: a type named `Error` would shadow the global one in the web.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
#[cfg_attr(test, ts(rename = "SteveError"))]
pub struct Error {
    pub code: Code,
    pub retry: Retry,
    pub reason: Option<Reason>,
    pub params: Params,
    /// Safe to show and log: no secrets or paths outside the workspace; never parsed by code.
    pub safe_detail: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct HttpErrorBody {
    pub error: String,
    pub code: Code,
    pub retry: Retry,
    pub reason: Option<Reason>,
    pub params: Params,
}

impl Error {
    pub fn to_http(&self) -> HttpErrorBody {
        HttpErrorBody {
            error: self.safe_detail.clone(),
            code: self.code,
            retry: self.retry,
            reason: self.reason.clone(),
            params: self.params.clone(),
        }
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.safe_detail.fmt(f)
    }
}
impl std::error::Error for Error {}
