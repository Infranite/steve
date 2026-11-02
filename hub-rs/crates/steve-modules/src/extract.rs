//! Request extractors for module handlers: who is calling ([`Caller`]), validated JSON bodies
//! ([`Body`]), and a small checker for length/count/pattern rules ([`Check`]).

use axum::extract::{FromRequest, FromRequestParts, Request};
use axum::http::request::Parts;
use serde::de::DeserializeOwned;

use crate::error::ApiError;

/// The signed-in user and current workspace. steve-hub's `require_user` layer resolves them; the
/// integrator inserts a `Caller` into the request extensions next to its own `Ctx` (one line), so the
/// module routers do not depend on steve-hub.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Caller {
    pub ws: String,
    pub me: String,
}

impl<S: Send + Sync> FromRequestParts<S> for Caller {
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, _: &S) -> Result<Self, ApiError> {
        parts
            .extensions
            .get::<Caller>()
            .cloned()
            .ok_or(ApiError::Unauthenticated)
    }
}

/// A JSON body deserialized with field paths: a wrong or missing field gives `400 参数不对：<path>`
/// (zod's dotted form, `members.0.contact_id`); a body that is not JSON gives `参数不对：body`.
#[derive(Debug, Clone)]
pub struct Body<T>(pub T);

impl<S: Send + Sync, T: DeserializeOwned> FromRequest<S> for Body<T> {
    type Rejection = ApiError;

    async fn from_request(req: Request, state: &S) -> Result<Self, ApiError> {
        let bytes = axum::body::Bytes::from_request(req, state)
            .await
            .map_err(|_| ApiError::invalid("body"))?;
        parse(&bytes).map(Body)
    }
}

/// [`Body`]'s parser, for bodies that arrive some other way (MCP tool arguments).
pub fn parse<T: DeserializeOwned>(bytes: &[u8]) -> Result<T, ApiError> {
    let de = &mut serde_json::Deserializer::from_slice(bytes);
    serde_path_to_error::deserialize(de).map_err(|e| {
        if e.inner().is_syntax() || e.inner().is_eof() {
            return ApiError::invalid("body");
        }
        let mut path = dotted(&e.path().to_string());
        // serde reports a missing field at its parent; name the field itself like zod does
        if let Some(f) = missing_field(&e.inner().to_string()) {
            path = if path.is_empty() {
                f
            } else {
                format!("{path}.{f}")
            };
        }
        ApiError::invalid(if path.is_empty() { "body".into() } else { path })
    })
}

/// `members[0].contact_id` → `members.0.contact_id`; serde's `.` for the root → empty.
fn dotted(p: &str) -> String {
    if p == "." {
        return String::new();
    }
    p.replace('[', ".").replace(']', "")
}

fn missing_field(msg: &str) -> Option<String> {
    let rest = msg.strip_prefix("missing field `")?;
    Some(rest.split('`').next()?.to_string())
}

/// Collects failing field names, then answers once with all of them (zod reports every issue).
#[derive(Debug, Default)]
pub struct Check(Vec<String>);

impl Check {
    pub fn new() -> Self {
        Self::default()
    }

    /// Fails `field` if `cond` is false.
    pub fn that(&mut self, field: &str, cond: bool) -> &mut Self {
        if !cond {
            self.0.push(field.into());
        }
        self
    }

    /// Length in characters (zod counts UTF-16 units; equal for the text these limits guard).
    pub fn len(&mut self, field: &str, s: &str, min: usize, max: usize) -> &mut Self {
        let n = s.encode_utf16().count();
        self.that(field, (min..=max).contains(&n))
    }

    /// Optional string: checked only when present.
    pub fn opt_len(&mut self, field: &str, s: Option<&str>, min: usize, max: usize) -> &mut Self {
        if let Some(s) = s {
            self.len(field, s, min, max);
        }
        self
    }

    /// At most `max` entries, each `1..=item_max` characters (`aliases: str[≤10]`).
    pub fn list(&mut self, field: &str, xs: &[String], max: usize, item_max: usize) -> &mut Self {
        if xs.len() > max {
            self.0.push(field.into());
        }
        for (i, x) in xs.iter().enumerate() {
            self.len(&format!("{field}.{i}"), x, 1, item_max);
        }
        self
    }

    /// Matches `re` (anchored by the caller).
    pub fn pattern(&mut self, field: &str, s: &str, re: &regex::Regex) -> &mut Self {
        self.that(field, re.is_match(s))
    }

    /// One of the allowed codes.
    pub fn one_of(&mut self, field: &str, s: &str, allowed: &[&str]) -> &mut Self {
        self.that(field, allowed.contains(&s))
    }

    pub fn done(&mut self) -> Result<(), ApiError> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(ApiError::Invalid(std::mem::take(&mut self.0)))
        }
    }
}
