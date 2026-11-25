//! Shared JSON utility helpers used across Hub, Node, and domain crates.

use serde_json::Value;

/// JavaScript-style truthiness of a JSON value (`null`, `false`, `0`, `NaN`, and `""` are falsy;
/// non-empty strings, non-zero numbers, arrays, and objects are truthy).
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0 && !f.is_nan()),
        Value::String(s) => !s.is_empty(),
        Value::Array(_) | Value::Object(_) => true,
    }
}

/// [`truthy`] on an optional JSON value (`None` is falsy).
pub fn truthy_opt(v: Option<&Value>) -> bool {
    v.is_some_and(truthy)
}

/// Greedy `{…}` substring spanning from the first `{` to the last `}` in `raw`, if any.
pub fn first_json_span(raw: &str) -> Option<&str> {
    let start = raw.find('{')?;
    let end = raw.rfind('}')?;
    raw.get(start..=end)
}

/// Parses the greedy `{…}` span of a model reply. Returns `Ok(None)` when no `{…}` span is
/// present, or `Err` if the span is malformed JSON.
pub fn first_json(raw: &str) -> Result<Option<Value>, serde_json::Error> {
    let Some(span) = first_json_span(raw) else {
        return Ok(None);
    };
    serde_json::from_str(span).map(Some)
}

/// Parses the greedy `{…}` span of a model reply, returning `None` when absent or malformed.
pub fn first_json_opt(raw: &str) -> Option<Value> {
    first_json(raw).ok().flatten()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn truthy_follows_js_semantics() {
        assert!(!truthy(&Value::Null));
        assert!(!truthy(&json!(false)));
        assert!(!truthy(&json!(0)));
        assert!(!truthy(&json!(0.0)));
        assert!(!truthy(&json!("")));
        assert!(!truthy_opt(None));

        assert!(truthy(&json!(true)));
        assert!(truthy(&json!(1)));
        assert!(truthy(&json!(-0.5)));
        assert!(truthy(&json!("x")));
        assert!(truthy(&json!([])));
        assert!(truthy(&json!({})));
        assert!(truthy_opt(Some(&json!("ok"))));
    }

    #[test]
    fn first_json_is_greedy_and_reports_invalid_spans() {
        assert_eq!(
            first_json("prefix {\"a\":{\"b\":1}} suffix").unwrap(),
            Some(json!({"a": {"b": 1}}))
        );
        assert_eq!(
            first_json_opt("prefix {\"a\":1} suffix"),
            Some(json!({"a": 1}))
        );
        assert_eq!(first_json("no braces").unwrap(), None);
        assert_eq!(first_json("} reversed {").unwrap(), None);
        assert!(first_json("{bad}").is_err());
        assert_eq!(first_json_opt("{bad}"), None);
    }
}
