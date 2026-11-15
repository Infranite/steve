//! Small helpers that reproduce JavaScript string semantics the TS code relies on.

use serde_json::Value;

/// `s.slice(0, n)` in JS counts UTF-16 code units, not bytes or chars. A surrogate pair that would
/// be cut in half is dropped instead of kept as a lone surrogate.
pub fn js_slice(s: &str, n: usize) -> &str {
    let mut units = 0;
    for (i, c) in s.char_indices() {
        units += c.len_utf16();
        if units > n {
            return &s[..i];
        }
    }
    s
}

/// `${v}` in a JS template: missing → `undefined`, strings unquoted, other JSON as text.
#[cfg_attr(not(test), allow(dead_code))]
pub fn js_str(v: Option<&Value>) -> String {
    match v {
        None => "undefined".into(),
        Some(Value::String(s)) => s.clone(),
        Some(Value::Object(_)) => "[object Object]".into(),
        Some(Value::Array(a)) => a
            .iter()
            .map(|x| {
                if x.is_null() {
                    String::new()
                } else {
                    js_str(Some(x))
                }
            })
            .collect::<Vec<_>>()
            .join(","),
        Some(other) => other.to_string(),
    }
}

/// `!!v` in JS.
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0 && !f.is_nan()),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

/// `a ?? b` on JSON fields: the first value that is present and not null.
pub fn coalesce<'a>(vals: impl IntoIterator<Item = Option<&'a Value>>) -> Option<&'a Value> {
    vals.into_iter().flatten().find(|v| !v.is_null())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn slice_counts_utf16() {
        assert_eq!(js_slice("abc", 2), "ab");
        assert_eq!(js_slice("允许执行", 3), "允许执");
        assert_eq!(js_slice("a😀b", 2), "a");
        assert_eq!(js_slice("a😀b", 3), "a😀");
        assert_eq!(js_slice("短", 500), "短");
    }

    #[test]
    fn template_strings() {
        assert_eq!(js_str(None), "undefined");
        assert_eq!(js_str(Some(&json!("x"))), "x");
        assert_eq!(js_str(Some(&json!(null))), "null");
        assert_eq!(js_str(Some(&json!(3))), "3");
    }
}
