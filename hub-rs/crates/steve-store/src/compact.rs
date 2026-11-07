//! Lossy compaction of an archived session's event log, only where the workspace's `keepSess` asks for
//! it (audit #62; the default, 永久, never does). Screenshots and other inline images (base64, nearly
//! half of the events table) become a placeholder, and a tool's big outputs keep their head and tail.
//! What the transcript shows of an event stays: the web already gets images as `{type:"image"}` and
//! strings cut to 4000 characters.

use serde_json::{Map, Value};

/// A tool's string longer than this (characters) keeps only its head and tail.
pub const BIG_CHARS: usize = 16_000;
/// What stays of a big string: the first and the last characters.
pub const HEAD_CHARS: usize = 4_000;
pub const TAIL_CHARS: usize = 2_000;

/// The placeholder an inline image becomes (the web shows it as an image that is not there).
pub fn image_placeholder() -> Value {
    let mut m = Map::new();
    m.insert("type".into(), Value::from("image"));
    m.insert(
        "omitted".into(),
        Value::from("归档后按会话记录保留设置省略了图片"),
    );
    Value::Object(m)
}

/// Compacts `data` of an event of `kind` in place; whether anything changed.
pub fn compact(kind: &str, data: &mut Value) -> bool {
    let tool = matches!(kind, "tool_call" | "tool_call_update");
    walk(data, tool)
}

fn is_inline_image(o: &Map<String, Value>) -> bool {
    o.get("type").and_then(Value::as_str) == Some("image")
        && !o.contains_key("omitted")
        && (o.contains_key("data") || o.contains_key("source"))
}

fn walk(v: &mut Value, tool: bool) -> bool {
    match v {
        Value::Object(o) if is_inline_image(o) => {
            *v = image_placeholder();
            true
        }
        Value::Object(o) => o
            .values_mut()
            .fold(false, |changed, x| walk(x, tool) | changed),
        Value::Array(a) => a
            .iter_mut()
            .fold(false, |changed, x| walk(x, tool) | changed),
        Value::String(s) if tool => match head_tail(s) {
            Some(cut) => {
                *s = cut;
                true
            }
            None => false,
        },
        _ => false,
    }
}

/// `s` cut to its head and tail with a note of what was left out, if it is big.
fn head_tail(s: &str) -> Option<String> {
    let n = s.chars().count();
    if n <= BIG_CHARS {
        return None;
    }
    let head: String = s.chars().take(HEAD_CHARS).collect();
    let tail: String = s.chars().skip(n - TAIL_CHARS).collect();
    Some(format!(
        "{head}\n…（归档后按会话记录保留设置省略了中间 {} 个字符）…\n{tail}",
        n - HEAD_CHARS - TAIL_CHARS
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn images_become_placeholders_in_every_kind_and_twice_is_a_no_op() {
        let mut v = json!({"content":[{"type":"content","content":{"type":"image","data":"iVBORw0KGgo=","mimeType":"image/png"}}],
            "rawOutput":[{"type":"image","source":{"type":"base64","data":"aW1hZ2U="}}], "status":"completed"});
        assert!(compact("tool_call_update", &mut v));
        assert_eq!(v["content"][0]["content"], image_placeholder());
        assert_eq!(v["rawOutput"][0], image_placeholder());
        assert_eq!(v["status"], "completed");
        assert!(!compact("tool_call_update", &mut v), "already compacted");
        let mut user =
            json!({"text":"看图","images":[{"type":"image","data":"QUJD","mimeType":"image/png"}]});
        assert!(compact("user", &mut user));
        assert_eq!(user["images"][0], image_placeholder());
        assert_eq!(user["text"], "看图");
    }

    #[test]
    fn a_big_tool_output_keeps_its_head_and_tail_and_other_text_stays() {
        let out = format!(
            "{}{}{}",
            "头".repeat(HEAD_CHARS),
            "中".repeat(50_000),
            "尾".repeat(TAIL_CHARS)
        );
        let mut v = json!({"rawOutput": out, "title": "cargo test"});
        assert!(compact("tool_call", &mut v));
        let s = v["rawOutput"].as_str().unwrap();
        assert!(s.starts_with(&"头".repeat(HEAD_CHARS)) && s.ends_with(&"尾".repeat(TAIL_CHARS)));
        assert!(s.contains("省略了中间 50000 个字符"), "{}", &s[..100]);
        assert_eq!(s.matches('中').count(), 1, "only the note's 中间");
        assert!(!compact("tool_call", &mut v), "already compacted");
        // a message's text is never cut, only a tool's output
        let mut msg = json!({"text": "长".repeat(BIG_CHARS + 1)});
        assert!(!compact("agent_message_chunk", &mut msg));
        let mut small = json!({"rawOutput": "x".repeat(BIG_CHARS)});
        assert!(!compact("tool_call_update", &mut small));
    }
}
