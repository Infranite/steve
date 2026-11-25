//! Lossless normalization of large strings in `steve.v1` tool events (`{"tool": ToolView}`).
//! References live in a separate, trusted table: a string/object supplied by an agent is never a
//! reference. Payloads belong to a session, and are reachable only through that session's events.

use std::borrow::Cow;
use std::collections::HashMap;

use crate::{Result, StoreError, sha};
use rusqlite::{Connection, params};
use serde_json::Value;

const THRESHOLD: usize = 64 * 1024;
const PREVIEW_CHARS: usize = 4000;
/// `ToolView` fields whose nested strings can exceed [`THRESHOLD`] while keeping metadata
/// (`id`, `origin`, `name`, `category`, `phase`, `title`) inline for queries and statistics.
const TOOL_CONTENT_FIELDS: [&str; 3] = ["result", "action", "error"];

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

/// Keep protocol identifiers/status and small text inline; only a `steve.v1` tool event's content
/// fields (`tool.{result,action,error}`) can be large. Ordinary events borrow the input. Large
/// tool events construct only the preview tree rather than cloning all full strings and trimming
/// them afterwards.
pub(crate) fn split<'a>(kind: &str, data: &'a Value) -> Result<(Cow<'a, Value>, Payloads)> {
    let mut payloads = Payloads::default();
    if kind != crate::TOOL_CALL_EVENT {
        return Ok((Cow::Borrowed(data), payloads));
    }
    let Some(outer) = data.as_object() else {
        return Ok((Cow::Borrowed(data), payloads));
    };
    let Some(tool) = outer.get("tool").and_then(Value::as_object) else {
        return Ok((Cow::Borrowed(data), payloads));
    };
    if !TOOL_CONTENT_FIELDS
        .iter()
        .any(|key| tool.get(*key).is_some_and(has_large_string))
    {
        return Ok((Cow::Borrowed(data), payloads));
    }
    // Borrow keys from the original strings. HashMap checks exact string equality,
    // so duplicate text is serialized and SHA-hashed only on its first occurrence.
    let mut seen: HashMap<&str, String> = HashMap::new();
    let mut stored_tool = serde_json::Map::with_capacity(tool.len());
    for (key, value) in tool {
        let value = if TOOL_CONTENT_FIELDS.contains(&key.as_str()) {
            preview(value, &format!("/tool/{key}"), &mut payloads, &mut seen)?
        } else {
            value.clone()
        };
        stored_tool.insert(key.clone(), value);
    }
    let mut stored = serde_json::Map::with_capacity(outer.len());
    for (key, value) in outer {
        if key == "tool" {
            stored.insert(key.clone(), Value::Object(stored_tool.clone()));
        } else {
            stored.insert(key.clone(), value.clone());
        }
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

/// Keeps an event's large strings and its references to them, in the write `db` is in.
pub(crate) fn save(
    db: &mut dyn crate::contract::Backend,
    session: &str,
    seq: i64,
    payloads: &Payloads,
) -> Result<()> {
    use crate::ingest::schema::{EventPayloadRefs as Refs, EventPayloads as P};
    use sea_query::{OnConflict, Query};
    for (hash, data) in &payloads.unique {
        crate::contract::exec_on(
            db,
            Query::insert()
                .into_table(P::Table)
                .columns([P::SessionId, P::Hash, P::Data])
                .values_panic([session.into(), hash.as_str().into(), data.as_str().into()])
                .on_conflict(
                    OnConflict::columns([P::SessionId, P::Hash])
                        .do_nothing()
                        .to_owned(),
                ),
        )?;
    }
    for r in &payloads.refs {
        crate::contract::exec_on(
            db,
            Query::insert()
                .into_table(Refs::Table)
                .columns([Refs::EventSeq, Refs::Path, Refs::SessionId, Refs::Hash])
                .values_panic([
                    seq.into(),
                    r.path.as_str().into(),
                    session.into(),
                    r.hash.as_str().into(),
                ]),
        )?;
    }
    Ok(())
}

pub(crate) fn hydrate(c: &Connection, session: &str, seq: i64, data: &mut Value) -> Result<()> {
    let mut st = c.prepare_cached("SELECT r.path, p.data, r.session_id FROM event_payload_refs r LEFT JOIN event_payloads p ON p.session_id = r.session_id AND p.hash = r.hash WHERE r.event_seq = ?")?;
    let mut rows = st.query(params![seq])?;
    while let Some(r) = rows.next()? {
        let path: String = r.get(0)?;
        let owner: String = r.get(2)?;
        restore(session, data, &path, r.get(1)?, &owner)?;
    }
    Ok(())
}

pub(crate) fn restore(
    session: &str,
    data: &mut Value,
    path: &str,
    raw: Option<String>,
    owner: &str,
) -> Result<()> {
    if owner != session {
        return Err(StoreError::Hook("event payload session mismatch".into()));
    }
    let raw = raw.ok_or_else(|| StoreError::Hook("missing event payload".into()))?;
    let target = data
        .pointer_mut(path)
        .ok_or_else(|| StoreError::Hook("invalid event payload reference".into()))?;
    *target = serde_json::from_str(&raw)?;
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
                "text",
                json!({"role": "assistant", "message": "m1", "text": "large non-tool event".repeat(20_000)}),
            ),
            (
                "tool",
                json!({"tool": {"id": "t1", "result": [{"type": "text", "text": "x".repeat(THRESHOLD)}]}}),
            ),
            ("tool", json!(["unusual-root-array"])),
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
        let value = json!({
            "tool": {
                "id": "t1",
                "origin": "harness",
                "name": "Bash",
                "category": "exec",
                "title": "Run",
                "phase": "failed",
                "action": {"type": "tool", "name": "steve_deliver", "args": {"again/~": text}},
                "result": [
                    {"type": "text", "text": text},
                    {"type": "diff", "path": "a.rs", "new": text, "added": 1, "removed": 0}
                ],
                "error": text
            }
        });
        let original = tree_string_capacity(&value);
        let serialized = serde_json::to_string(&text).unwrap();
        let (preview, payloads) = split("tool", &value).unwrap();
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
        let old_refs_lower_bound = serialized.len() * payloads.refs.len();
        println!(
            "normalization retained_string_capacity_proxy original={original} preview={preview_bytes} unique_payload={payload_bytes} refs={ref_bytes} total={resident_proxy} prior_refs_lower_bound={old_refs_lower_bound}"
        );
        assert!(resident_proxy < old_refs_lower_bound / 2);
        assert!(preview_bytes < 100_000);
        assert_eq!(
            value["tool"]["result"][0]["text"], text,
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
