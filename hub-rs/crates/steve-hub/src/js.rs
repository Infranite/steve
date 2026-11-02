//! Small JavaScript semantics the API copies on purpose: UTF-16 lengths and slices, `String(v)`,
//! `JSON.parse` error messages and `firstJson`.

use serde_json::Value;

use crate::error::ApiError;

/// `s.length` in JS (UTF-16 code units); zod's `min`/`max` count these.
pub fn utf16_len(s: &str) -> usize {
    s.encode_utf16().count()
}

/// `s.slice(0, n)`. Cutting a surrogate pair in half leaves a lone surrogate in JS, which becomes U+FFFD
/// once stored through better-sqlite3, so that is what we produce.
pub fn slice(s: &str, n: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let w = c.len_utf16();
        if used + w > n {
            if used < n {
                out.push('\u{FFFD}');
            }
            break;
        }
        out.push(c);
        used += w;
    }
    out
}

/// JS number → string for the values JSON can carry.
pub fn number_string(n: &serde_json::Number) -> String {
    if let Some(i) = n.as_i64() {
        return i.to_string();
    }
    if let Some(u) = n.as_u64() {
        return u.to_string();
    }
    let f = n.as_f64().unwrap_or(f64::NAN);
    if f.fract() == 0.0 && f.abs() < 1e21 {
        format!("{f:.0}")
    } else if f.abs() >= 1e21 {
        // JS switches to exponent form at 1e21: 1e+21
        let s = format!("{f:e}");
        match s.split_once('e') {
            Some((m, e)) if !e.starts_with('-') => format!("{m}e+{e}"),
            _ => s,
        }
    } else {
        f.to_string()
    }
}

/// A JS number as JSON: integral values without a fraction (`70`, not `70.0`); NaN/±∞ are `null`
/// like `JSON.stringify`.
pub fn num_value(f: f64) -> Value {
    if !f.is_finite() {
        Value::Null
    } else if f.fract() == 0.0 && f.abs() < 9e15 {
        Value::from(f as i64)
    } else {
        Value::from(f)
    }
}

/// `String(v)`.
pub fn string(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => number_string(n),
        Value::String(s) => s.clone(),
        // Array.prototype.join: null/undefined elements become ""
        Value::Array(a) => a
            .iter()
            .map(|x| {
                if x.is_null() {
                    String::new()
                } else {
                    string(x)
                }
            })
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}

/// JS truthiness of a JSON value.
pub fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}

/// `JSON.parse(text)` with V8's SyntaxError message for the common cases. Hono's `c.req.json()` throws
/// this and api.ts serves it as `500 {"error": message}`.
pub fn parse_json(text: &str) -> Result<Value, ApiError> {
    serde_json::from_str(text).map_err(|e| ApiError::internal(syntax_message(text, &e)))
}

fn syntax_message(text: &str, e: &serde_json::Error) -> String {
    if text.trim().is_empty() || e.is_eof() {
        return "Unexpected end of JSON input".into();
    }
    // position of the offending character (serde reports 1-based line/column)
    let mut pos = 0usize;
    for (i, line) in text.split('\n').enumerate() {
        if i + 1 == e.line() {
            pos += line.chars().take(e.column().saturating_sub(1)).count();
            break;
        }
        pos += line.chars().count() + 1;
    }
    let ch = text
        .chars()
        .nth(pos)
        .or_else(|| text.trim().chars().next())
        .unwrap_or(' ');
    let shown = if text.chars().count() <= 10 {
        format!("\"{text}\"")
    } else {
        let start = pos.saturating_sub(10);
        let s: String = text.chars().skip(start).take(20).collect();
        format!("{}\"{s}\"...", if start > 0 { "...\"" } else { "" })
            .replacen("...\"\"", "...\"", 1)
    };
    format!("Unexpected token '{ch}', {shown} is not valid JSON")
}

/// gateway.ts `firstJson`: the greedy `{…}` span of a model reply, parsed. No braces → `None`; a span that
/// is not valid JSON throws (500) like `JSON.parse`.
pub fn first_json(text: &str) -> Result<Option<Value>, ApiError> {
    let (Some(a), Some(b)) = (text.find('{'), text.rfind('}')) else {
        return Ok(None);
    };
    if b < a {
        return Ok(None);
    }
    parse_json(&text[a..=b]).map(Some)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn utf16() {
        assert_eq!(utf16_len("你好"), 2);
        assert_eq!(utf16_len("😀"), 2);
        assert_eq!(slice("写 hello.txt 文件", 5), "写 hel");
        assert_eq!(slice("a😀b", 2), "a\u{FFFD}");
        assert_eq!(slice("ab", 10), "ab");
    }

    #[test]
    fn string_like_js() {
        assert_eq!(string(&json!(4)), "4");
        assert_eq!(string(&json!(4.0)), "4");
        assert_eq!(string(&json!(0.5)), "0.5");
        assert_eq!(string(&json!(true)), "true");
        assert_eq!(string(&json!(null)), "null");
        assert_eq!(string(&json!([1, null, "a"])), "1,,a");
        assert_eq!(string(&json!({"a": 1})), "[object Object]");
    }

    #[test]
    fn json_errors() {
        assert_eq!(
            parse_json(""),
            Err(ApiError::internal("Unexpected end of JSON input"))
        );
        assert_eq!(
            parse_json("{\"a\":"),
            Err(ApiError::internal("Unexpected end of JSON input"))
        );
        assert_eq!(
            parse_json("abc"),
            Err(ApiError::internal(
                "Unexpected token 'a', \"abc\" is not valid JSON"
            ))
        );
    }

    #[test]
    fn first_json_greedy() {
        assert_eq!(
            first_json("x {\"a\":{\"b\":1}} y").ok().flatten(),
            Some(json!({"a":{"b":1}}))
        );
        assert_eq!(first_json("no json").ok().flatten(), None);
        assert!(first_json("{oops}").is_err());
    }
}
