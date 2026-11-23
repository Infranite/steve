//! Shared error contract. Transport and provider conversions belong to their own crates.

use serde::{Deserialize, Serialize};
use std::{
    borrow::Cow,
    collections::{BTreeMap, BTreeSet},
    fmt,
};

/// The command owner enforces this budget, then falls back to Later with the same operation ID.
pub const NOW_MAX_ATTEMPTS: u8 = 3;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, strum::IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
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
    pub fn as_str(self) -> &'static str {
        self.into()
    }

    /// HTTP edge fallback; domain-specific codes require an explicit contract body.
    pub const fn from_http_status(status: u16) -> Self {
        match status {
            401 => Self::Unauthenticated,
            403 => Self::Forbidden,
            404 => Self::NotFound,
            409 => Self::Conflict,
            413 => Self::TooLarge,
            429 => Self::RateLimited,
            501 => Self::Unsupported,
            503 => Self::Unavailable,
            504 => Self::Timeout,
            400..=499 => Self::Invalid,
            _ => Self::Internal,
        }
    }

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
            // The operation may already have taken effect: ask (reconcile), do not resend. A
            // producer that knows a resend is safe after reconciliation sets `retry` itself.
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

impl From<&str> for Param {
    fn from(value: &str) -> Self {
        Self::Str(value.to_owned())
    }
}

impl From<String> for Param {
    fn from(value: String) -> Self {
        Self::Str(value)
    }
}

impl From<&String> for Param {
    fn from(value: &String) -> Self {
        Self::Str(value.clone())
    }
}

// Only lossless integer conversions: wider unsigned values need an explicit checked conversion.
macro_rules! param_int {
    ($($ty:ty),* $(,)?) => { $(
        impl From<$ty> for Param {
            fn from(value: $ty) -> Self {
                Self::Int(i64::from(value))
            }
        }
    )* };
}
param_int!(i8, i16, i32, i64, u8, u16, u32);

/// Ordered by key for stable wire output. Values are display arguments, never parsed messages.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
#[cfg_attr(test, derive(ts_rs::TS))]
pub struct Params(pub BTreeMap<String, Param>);

/// Open wire value: even malformed or retired strings survive deserialization unchanged.
#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
#[cfg_attr(test, derive(ts_rs::TS), ts(type = "string"))]
pub struct Reason(Cow<'static, str>);

impl Reason {
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// A valid reason has one nonempty domain and code, separated by exactly one dot.
    pub fn domain(&self) -> Option<&str> {
        valid_reason(self.as_str()).then(|| self.0.split_once('.').unwrap().0)
    }

    pub fn is<R: ReasonCode>(&self, reason: R) -> bool {
        self.as_str() == reason.def().reason
    }

    pub fn known<R: ReasonCode>(&self) -> Option<R> {
        self.domain()?;
        R::from_reason(self.as_str())
    }

    /// For macro expansions in downstream crates; only accepts a domain-owned reason code.
    #[doc(hidden)]
    pub fn __from_code<R: ReasonCode>(reason: R) -> Self {
        Self(Cow::Borrowed(reason.def().reason))
    }
}

pub struct ReasonDef {
    pub reason: &'static str,
    pub code: Code,
    pub params: &'static [&'static str],
}

pub struct ReasonSet {
    pub domain: &'static str,
    pub defs: &'static [ReasonDef],
    pub retired: &'static [&'static str],
}

pub trait ReasonCode: Copy + Eq + Into<Reason> + 'static {
    const SET: ReasonSet;
    fn def(self) -> &'static ReasonDef;
    /// Reverse lookup generated by `reasons!`; excludes tombstones.
    #[doc(hidden)]
    fn from_reason(reason: &str) -> Option<Self>;
}

/// Declare stable reason codes, their error codes and display parameters in a domain file.
/// Retired names are suffixes, kept only as tombstones to prevent reuse.
/// The display parameter name `detail` is reserved for the final safe-detail argument.
///
/// ```compile_fail
/// steve_types::reasons! {
///     pub enum ExampleReason = "test" {
///         Moved = moved [Stale] (branch, now),
///     }
///     retired { }
/// }
/// // Both branch and now are required before the safe detail.
/// ExampleReason::moved("main", "the branch moved");
/// ```
#[macro_export]
macro_rules! reasons {
    (
        $(#[$meta:meta])*
        $vis:vis enum $name:ident = $domain:literal {
            $($(#[$variant_meta:meta])* $variant:ident = $constructor:ident [$code:ident] ($($param:ident),* $(,)?)),* $(,)?
        }
        retired { $($retired:literal),* $(,)? }
    ) => {
        $(#[$meta])*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        $vis enum $name { $($(#[$variant_meta])* $variant),* }

        impl $crate::ReasonCode for $name {
            const SET: $crate::ReasonSet = $crate::ReasonSet {
                domain: $domain,
                defs: &[$($crate::ReasonDef {
                    reason: concat!($domain, ".", stringify!($constructor)),
                    code: $crate::Code::$code,
                    params: &[$(stringify!($param)),*],
                }),*],
                retired: &[$(concat!($domain, ".", $retired)),*],
            };

            fn def(self) -> &'static $crate::ReasonDef {
                match self {
                    $(Self::$variant => &Self::SET.defs[Self::$variant as usize]),*
                }
            }

            fn from_reason(reason: &str) -> Option<Self> {
                match reason {
                    $(concat!($domain, ".", stringify!($constructor)) => Some(Self::$variant),)*
                    _ => None,
                }
            }
        }

        impl From<$name> for $crate::Reason {
            fn from(reason: $name) -> Self {
                Self::__from_code(reason)
            }
        }

        impl $name {
            $(pub fn $constructor(
                $($param: impl Into<$crate::Param>,)*
                detail: impl Into<String>,
            ) -> $crate::Error {
                $crate::Error {
                    code: $crate::Code::$code,
                    retry: $crate::Code::$code.default_retry(),
                    reason: Some(Self::$variant.into()),
                    params: $crate::Params([$(
                        (stringify!($param).to_owned(), $param.into()),
                    )*].into()),
                    safe_detail: detail.into(),
                    gateway: None,
                }
            })*
        }
    };
}

pub const ALL_REASONS: &[&[ReasonSet]] = &[
    crate::automation::REASONS,
    crate::autonomy::REASONS,
    crate::channels::REASONS,
    crate::config::REASONS,
    crate::devices::REASONS,
    crate::integrations::REASONS,
    crate::knowledge::REASONS,
    crate::people::REASONS,
    crate::stats::REASONS,
    crate::vault::REASONS,
    crate::work::REASONS,
];

fn valid_segment(segment: &str) -> bool {
    !segment.is_empty()
        && segment
            .bytes()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == b'_')
}

fn valid_reason(reason: &str) -> bool {
    reason.len() <= 64
        && reason
            .split_once('.')
            .is_some_and(|(domain, code)| valid_segment(domain) && valid_segment(code))
}

/// Check producers, never readers: unknown wire values must remain readable.
#[doc(hidden)]
pub fn check_registry(all: &[&[ReasonSet]]) -> Result<(), String> {
    let mut domains = BTreeSet::new();
    let mut reasons = BTreeSet::new();
    for set in all.iter().flat_map(|sets| sets.iter()) {
        if !valid_segment(set.domain) || !domains.insert(set.domain) {
            return Err(format!(
                "invalid or duplicate reason domain: {}",
                set.domain
            ));
        }
        for reason in set
            .defs
            .iter()
            .map(|def| def.reason)
            .chain(set.retired.iter().copied())
        {
            if !valid_reason(reason) || reason.split_once('.').map(|(d, _)| d) != Some(set.domain) {
                return Err(format!("invalid reason in {}: {reason}", set.domain));
            }
            if !reasons.insert(reason) {
                return Err(format!("duplicate reason: {reason}"));
            }
        }
        for def in set.defs {
            let mut params = BTreeSet::new();
            for param in def.params {
                if *param == "detail" {
                    return Err(format!("reserved parameter in {}: {param}", def.reason));
                }
                if !param.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
                    || !valid_segment(param)
                    || !params.insert(param)
                {
                    return Err(format!(
                        "invalid or duplicate parameter in {}: {param}",
                        def.reason
                    ));
                }
            }
        }
    }
    Ok(())
}

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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[cfg_attr(test, ts(optional))]
    pub gateway: Option<crate::gateway::GatewayFailure>,
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
    pub fn new(code: Code, detail: impl Into<String>) -> Self {
        Self {
            code,
            retry: Retry::Never,
            reason: None,
            params: Params::default(),
            safe_detail: detail.into(),
            gateway: None,
        }
    }

    pub fn later(code: Code, detail: impl Into<String>) -> Self {
        Self::new(code, detail).retry(Retry::Later { after_ms: None })
    }

    pub fn for_gateway(mut self, failure: crate::gateway::GatewayFailure) -> Self {
        self.gateway = Some(failure);
        self
    }

    pub fn with_param(mut self, name: &str, value: impl Into<Param>) -> Self {
        self.params.0.insert(name.to_owned(), value.into());
        self
    }

    pub fn outcome_unknown(operation: impl fmt::Display, detail: impl Into<String>) -> Self {
        Self::new(Code::OutcomeUnknown, detail)
            .with_param("operation", operation.to_string())
            .with_param("state", "unknown")
    }

    /// Override retry advice without changing the cause or classification.
    pub fn retry(mut self, retry: Retry) -> Self {
        self.retry = retry;
        self
    }

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
