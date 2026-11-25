//! Deterministic UTF-8 canonical JSON serialization and safe-number validation.
//!
//! Guarantees:
//! - Object keys are emitted in strict UTF-8 byte order (independent of `serde_json`'s
//!   `preserve_order` feature, which Cargo may unify across the workspace).
//! - Non-finite numbers (`NaN`, `+Infinity`, `-Infinity`) are rejected.
//! - Integers (whether stored as `i64`, `u64`, or integral `f64`) must lie within ECMAScript's
//!   safe integer range `±(2^53 - 1)` (`[MIN_SAFE_INTEGER, MAX_SAFE_INTEGER]`).
//! - `-0.0` normalizes to `0`.
//! - Nesting depth is capped at [`MAX_JSON_DEPTH`].

use serde_json::{Number, Value};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::limits::{MAX_JSON_DEPTH, MAX_SAFE_INTEGER, MIN_SAFE_INTEGER};

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CanonicalError {
    #[error("non-finite floating point number is not allowed")]
    NonFiniteNumber,
    #[error(
        "integer {value} is outside JS safe integer range [{MIN_SAFE_INTEGER}, {MAX_SAFE_INTEGER}]"
    )]
    UnsafeInteger { value: String },
    #[error("JSON nesting depth exceeds maximum of {limit}")]
    MaxDepthExceeded { limit: usize },
}

/// Returns `true` when `n` is an exact ECMAScript safe integer (`±(2^53 - 1)`).
pub const fn is_safe_i64(n: i64) -> bool {
    n >= MIN_SAFE_INTEGER && n <= MAX_SAFE_INTEGER
}

/// Validates a raw `f64` from JavaScript or JSON and normalizes it into a `serde_json::Number`.
///
/// - Rejects `NaN` and `±Infinity`.
/// - Normalizes `-0.0` to `0`.
/// - Rejects integral floats outside `[MIN_SAFE_INTEGER, MAX_SAFE_INTEGER]` (including values like
///   `2^53 = 9007199254740992.0` or `1e20` where IEEE-754 loses integer precision).
/// - Emits integral safe floats as exact `i64` JSON integers (no `.0`).
pub fn validate_f64(f: f64) -> Result<Number, CanonicalError> {
    if !f.is_finite() {
        return Err(CanonicalError::NonFiniteNumber);
    }
    if f == 0.0 {
        return Ok(Number::from(0_i64));
    }
    if f.fract() == 0.0 {
        if f < MIN_SAFE_INTEGER as f64 || f > MAX_SAFE_INTEGER as f64 {
            return Err(CanonicalError::UnsafeInteger {
                value: format!("{f}"),
            });
        }
        return Ok(Number::from(f as i64));
    }
    Number::from_f64(f).ok_or(CanonicalError::NonFiniteNumber)
}

/// Validates a `serde_json::Number` against safe-integer and finite-float rules and returns a
/// normalized `serde_json::Number`.
pub fn validate_number(n: &Number) -> Result<Number, CanonicalError> {
    if let Some(i) = n.as_i64() {
        if !is_safe_i64(i) {
            return Err(CanonicalError::UnsafeInteger {
                value: i.to_string(),
            });
        }
        return Ok(Number::from(i));
    }
    if let Some(u) = n.as_u64() {
        if u > MAX_SAFE_INTEGER as u64 {
            return Err(CanonicalError::UnsafeInteger {
                value: u.to_string(),
            });
        }
        return Ok(Number::from(u as i64));
    }
    let f = n.as_f64().ok_or(CanonicalError::NonFiniteNumber)?;
    validate_f64(f)
}

/// Validates and recursively normalizes a `serde_json::Value` without serializing it.
pub fn validate_json_value(value: &Value) -> Result<(), CanonicalError> {
    validate_value_at(value, 0)
}

fn validate_value_at(value: &Value, depth: usize) -> Result<(), CanonicalError> {
    if depth > MAX_JSON_DEPTH {
        return Err(CanonicalError::MaxDepthExceeded {
            limit: MAX_JSON_DEPTH,
        });
    }
    match value {
        Value::Null | Value::Bool(_) | Value::String(_) => Ok(()),
        Value::Number(n) => validate_number(n).map(|_| ()),
        Value::Array(items) => {
            for item in items {
                validate_value_at(item, depth + 1)?;
            }
            Ok(())
        }
        Value::Object(map) => {
            for v in map.values() {
                validate_value_at(v, depth + 1)?;
            }
            Ok(())
        }
    }
}

/// Serializes `value` to deterministic UTF-8 canonical JSON with keys sorted by UTF-8 bytes and
/// strict safe-integer / finite-float enforcement.
pub fn to_canonical_json(value: &Value) -> Result<String, CanonicalError> {
    let mut out = String::new();
    write_canonical(value, 0, &mut out)?;
    Ok(out)
}

fn write_canonical(value: &Value, depth: usize, out: &mut String) -> Result<(), CanonicalError> {
    if depth > MAX_JSON_DEPTH {
        return Err(CanonicalError::MaxDepthExceeded {
            limit: MAX_JSON_DEPTH,
        });
    }
    match value {
        Value::Null => out.push_str("null"),
        Value::Bool(true) => out.push_str("true"),
        Value::Bool(false) => out.push_str("false"),
        Value::Number(n) => {
            let norm = validate_number(n)?;
            out.push_str(&norm.to_string());
        }
        Value::String(s) => {
            out.push_str(&serde_json::to_string(s).expect("string JSON serialization"));
        }
        Value::Array(items) => {
            out.push('[');
            for (i, item) in items.iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                write_canonical(item, depth + 1, out)?;
            }
            out.push(']');
        }
        Value::Object(map) => {
            // Sort explicitly by UTF-8 byte order; do not rely on serde_json's map ordering
            // because Cargo feature unification enables `preserve_order` across the workspace.
            let mut entries: Vec<(&String, &Value)> = map.iter().collect();
            entries.sort_unstable_by(|(a, _), (b, _)| a.as_bytes().cmp(b.as_bytes()));
            out.push('{');
            for (i, (k, v)) in entries.into_iter().enumerate() {
                if i > 0 {
                    out.push(',');
                }
                out.push_str(&serde_json::to_string(k).expect("key JSON serialization"));
                out.push(':');
                write_canonical(v, depth + 1, out)?;
            }
            out.push('}');
        }
    }
    Ok(())
}

/// Lowercase hex SHA-256 digest of `bytes`.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hex::encode(hasher.finalize())
}
