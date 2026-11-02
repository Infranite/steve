//! Lossless normalization of large strings in tool events. References live in a
//! separate, trusted table: a string/object supplied by an agent is never a reference.
//! Payloads belong to a session, and are reachable only through that session's events.

use std::borrow::Cow;
use std::collections::HashMap;

use crate::{Result, StoreError, sha};
use rusqlite::{Connection, params};
use serde_json::Value;

const THRESHOLD: usize = 64 * 1024;
const PREVIEW_CHARS: usize = 4000;
const CONTENT_FIELDS: [&str; 4] = ["content", "rawOutput", "rawInput", "_meta"];

struct PayloadRef {
    path: String,
    hash: String,
}

#[derive(Default)]
pub(crate) struct Payloads {
    // Own exactly one serialized copy per unique value in this event. Repeated
    // fields only own a small path/hash reference, never another full string.
    unique: HashMap<String, String>,
    refs: Vec<PayloadRef>,
}

/// Keep protocol identifiers/status and small text inline; only the tool's content
/// fields can be large. Statistics can still read tool names and statuses directly.
/// Ordinary events borrow the input. Large tool events construct only the preview
/// tree rather than cloning all full strings and trimming them afterwards.
pub(crate) fn split<'a>(kind: &str, data: &'a Value) -> Result<(Cow<'a, Value>, Payloads)> {
    let mut payloads = Payloads::default();
    let Some(object) = data
        .as_object()
        .filter(|_| matches!(kind, "tool_call" | "tool_call_update"))
    else {
        return Ok((Cow::Borrowed(data), payloads));
    };
    if !CONTENT_FIELDS
        .iter()
        .any(|key| object.get(*key).is_some_and(has_large_string))
    {
        return Ok((Cow::Borrowed(data), payloads));
    }
    // Borrow keys from the original strings. HashMap checks exact string equality,
    // so duplicate text is serialized and SHA-hashed only on its first occurrence.
    let mut seen: HashMap<&str, String> = HashMap::new();
    let mut stored = serde_json::Map::with_capacity(object.len());
    for (key, value) in object {
        let value = if CONTENT_FIELDS.contains(&key.as_str()) {
            preview(value, &format!("/{key}"), &mut payloads, &mut seen)?
        } else {
            value.clone()
        };
        stored.insert(key.clone(), value);
    }
    Ok((Cow::Owned(Value::Object(stored)), payloads))
}

fn has_large_string(value: &Value) -> bool {
    match value {
        Value::String(s) => s.len() > THRESHOLD,
        Value::Array(a) => a.iter().any(has_large_string),
        Value::Object(o) => o.values().any(has_large_string),
        _ => false,
    }
}

fn preview<'a>(
    value: &'a Value,
    path: &str,
    payloads: &mut Payloads,
    seen: &mut HashMap<&'a str, String>,
) -> Result<Value> {
    match value {
        Value::String(s) if s.len() > THRESHOLD => {
            let hash = if let Some(hash) = seen.get(s.as_str()) {
                hash.clone()
            } else {
                let data = serde_json::to_string(s)?;
                let hash = sha(&data);
                payloads.unique.insert(hash.clone(), data);
                seen.insert(s.as_str(), hash.clone());
                hash
            };
            payloads.refs.push(PayloadRef {
                path: path.into(),
                hash,
            });
            // Enough Unicode scalar values to preserve the web's 4000 UTF-16-unit preview.
            Ok(Value::String(s.chars().take(PREVIEW_CHARS).collect()))
        }
        Value::Array(a) => a
            .iter()
            .enumerate()
            .map(|(i, v)| preview(v, &format!("{path}/{i}"), payloads, seen))
            .collect::<Result<Vec<_>>>()
            .map(Value::Array),
        Value::Object(o) => {
            let mut out = serde_json::Map::with_capacity(o.len());
            for (key, value) in o {
                let escaped = key.replace('~', "~0").replace('/', "~1");
                out.insert(
                    key.clone(),
                    preview(value, &format!("{path}/{escaped}"), payloads, seen)?,
                );
            }
            Ok(Value::Object(out))
        }
        _ => Ok(value.clone()),
    }
}

pub(crate) fn save(c: &Connection, session: &str, seq: i64, payloads: &Payloads) -> Result<()> {
    for (hash, data) in &payloads.unique {
        c.prepare_cached(
            "INSERT OR IGNORE INTO event_payloads(session_id, hash, data) VALUES (?, ?, ?)",
        )?
        .execute(params![session, hash, data])?;
    }
    for r in &payloads.refs {
        c.prepare_cached(
            "INSERT INTO event_payload_refs(event_seq, path, session_id, hash) VALUES (?, ?, ?, ?)",
        )?
        .execute(params![seq, r.path, session, r.hash])?;
    }
    Ok(())
}

pub(crate) fn hydrate(c: &Connection, session: &str, seq: i64, data: &mut Value) -> Result<()> {
    let mut st = c.prepare_cached("SELECT r.path, p.data, r.session_id FROM event_payload_refs r LEFT JOIN event_payloads p ON p.session_id = r.session_id AND p.hash = r.hash WHERE r.event_seq = ?")?;
    let mut rows = st.query(params![seq])?;
    while let Some(r) = rows.next()? {
        let path: String = r.get(0)?;
        let owner: String = r.get(2)?;
        if owner != session {
            return Err(StoreError::Hook("event payload session mismatch".into()));
        }
        let raw: String = r
            .get::<_, Option<String>>(1)?
            .ok_or_else(|| StoreError::Hook("missing event payload".into()))?;
        let value: Value = serde_json::from_str(&raw)?;
        let target = data
            .pointer_mut(&path)
            .ok_or_else(|| StoreError::Hook("invalid event payload reference".into()))?;
        *target = value;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    // Explicit representation-size proxy, not an allocator/RSS measurement. Count
    // retained string capacities and JSON key bytes; container bookkeeping excluded.
    fn tree_string_capacity(v: &Value) -> usize {
        match v {
            Value::String(s) => s.capacity(),
            Value::Array(a) => a.iter().map(tree_string_capacity).sum(),
            Value::Object(o) => o
                .iter()
                .map(|(k, v)| k.capacity() + tree_string_capacity(v))
                .sum(),
            _ => 0,
        }
    }

    #[test]
    fn ordinary_and_threshold_sized_events_borrow_input_without_copying() {
        for (kind, value) in [
            (
                "agent_message_chunk",
                json!({"text":"large non-tool event".repeat(20_000)}),
            ),
            (
                "tool_call_update",
                json!({"rawOutput":"x".repeat(THRESHOLD)}),
            ),
            ("tool_call_update", json!(["unusual-root-array"])),
        ] {
            let (preview, payloads) = split(kind, &value).unwrap();
            assert!(matches!(preview, Cow::Borrowed(_)));
            assert!(std::ptr::eq(preview.as_ref(), &value));
            assert!(payloads.unique.is_empty());
            assert!(payloads.refs.is_empty());
        }
    }

    #[test]
    fn repeated_large_fields_keep_one_serialized_payload_and_only_preview_strings() {
        // Escaping exercises serialized-vs-original size without changing the persisted hash.
        let text = "image\\\"\n🙂".repeat(100_000);
        let value = json!({"content":[{"data":text},{"data":text}],"rawOutput":text,"_meta":{"again/~":text},"status":"completed"});
        let original = tree_string_capacity(&value);
        let serialized = serde_json::to_string(&text).unwrap();
        let (preview, payloads) = split("tool_call_update", &value).unwrap();
        assert_eq!(payloads.unique.len(), 1);
        assert_eq!(payloads.refs.len(), 4);
        assert_eq!(payloads.unique.get(&sha(&serialized)), Some(&serialized));
        let preview_bytes = tree_string_capacity(&preview);
        let payload_bytes: usize = payloads
            .unique
            .iter()
            .map(|(hash, data)| hash.capacity() + data.capacity())
            .sum();
        let ref_bytes: usize = payloads
            .refs
            .iter()
            .map(|r| r.path.capacity() + r.hash.capacity())
            .sum();
        let resident_proxy = preview_bytes + payload_bytes + ref_bytes;
        // Previously refs held four independently serialized copies, plus a full input clone
        // before trimming. This lower bound deliberately excludes the latter transient clone.
        let old_refs_lower_bound = serialized.len() * payloads.refs.len();
        println!(
            "normalization retained_string_capacity_proxy original={original} preview={preview_bytes} unique_payload={payload_bytes} refs={ref_bytes} total={resident_proxy} prior_refs_lower_bound={old_refs_lower_bound}"
        );
        assert!(resident_proxy < old_refs_lower_bound / 2);
        assert!(preview_bytes < 100_000);
        assert_eq!(
            value["rawOutput"], text,
            "input remains lossless for publication"
        );
        for r in &payloads.refs {
            assert_eq!(
                preview.pointer(&r.path).unwrap().as_str().unwrap(),
                text.chars().take(PREVIEW_CHARS).collect::<String>()
            );
        }
    }
}
