//! Lossy compaction of an archived session's event log, only where the workspace's `keepSess` asks for
//! it (audit #62; the default, 永久, never does). Screenshots and other inline images (base64, nearly
//! half of the events table) become a placeholder, and a `steve.v1` tool's big outputs keep their
//! head and tail. What the transcript shows of an event stays: the web already gets inline images as
//! `{type:"image"}` and strings cut to 4000 characters.

use serde_json::{Map, Value};
use steve_node_api::{Action, Part, ToolView};

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

/// Compacts `data` of a `steve.v1` event of `kind` in place; whether anything changed.
pub fn compact(kind: &str, data: &mut Value) -> bool {
    if kind == crate::TOOL_CALL_EVENT {
        if let Some(tool_val) = data.get_mut("tool")
            && let Ok(mut tool) = serde_json::from_value::<ToolView>(tool_val.clone())
        {
            if compact_tool(&mut tool) {
                if let Ok(encoded) = serde_json::to_value(&tool) {
                    *tool_val = encoded;
                    return true;
                }
            }
            return false;
        }
    }
    walk(data, false)
}

fn compact_str(s: &mut String) -> bool {
    match head_tail(s) {
        Some(cut) => {
            *s = cut;
            true
        }
        None => false,
    }
}

fn compact_tool(tool: &mut ToolView) -> bool {
    let mut changed = false;
    for part in &mut tool.result {
        match part {
            Part::Text { text } => changed |= compact_str(text),
            Part::Diff { old, new, .. } => {
                if let Some(o) = old {
                    changed |= compact_str(o);
                }
                changed |= compact_str(new);
            }
            Part::Image { .. } | Part::File { .. } => {}
        }
    }
    if let Some(err) = &mut tool.error {
        changed |= compact_str(err);
    }
    if let Some(action) = &mut tool.action {
        match action {
            Action::Exec { command, .. } => changed |= compact_str(command),
            Action::Write { change_summary, .. } => changed |= compact_str(change_summary),
            Action::Search { query } => changed |= compact_str(query),
            Action::Fetch { url } => changed |= compact_str(url),
            Action::Unknown { summary } => changed |= compact_str(summary),
            Action::Tool { args, .. } => changed |= walk(args, true),
            Action::Read { .. } | Action::Delete { .. } | Action::Move { .. } => {}
        }
    }
    changed
}

fn is_inline_image(o: &Map<String, Value>) -> bool {
    o.get("type").and_then(Value::as_str) == Some("image")
        && !o.contains_key("omitted")
        && (o.contains_key("data") || o.contains_key("source"))
}

fn walk(v: &mut Value, cut_strings: bool) -> bool {
    match v {
        Value::Object(o) if is_inline_image(o) => {
            *v = image_placeholder();
            true
        }
        Value::Object(o) => o
            .values_mut()
            .fold(false, |changed, x| walk(x, cut_strings) | changed),
        Value::Array(a) => a
            .iter_mut()
            .fold(false, |changed, x| walk(x, cut_strings) | changed),
        Value::String(s) if cut_strings => compact_str(s),
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
    use steve_node_api::{CallId, ToolCategory, ToolOrigin, ToolPhase};

    fn sample_tool(result: Vec<Part>, action: Option<Action>, error: Option<String>) -> Value {
        let tool = ToolView {
            id: CallId::new("c1").unwrap(),
            parent: None,
            origin: ToolOrigin::Harness,
            name: "Bash".into(),
            category: ToolCategory::Exec,
            title: "cargo test".into(),
            description: None,
            target: None,
            phase: ToolPhase::Succeeded,
            action,
            evidence: None,
            result,
            error,
            duration_ms: None,
        };
        json!({ "tool": tool })
    }

    #[test]
    fn images_become_placeholders_in_user_and_tool_args_and_twice_is_a_no_op() {
        let mut tool = sample_tool(
            vec![],
            Some(Action::Tool {
                name: "steve_deliver".into(),
                args: json!({
                    "inline": {"type": "image", "data": "iVBORw0KGgo=", "mimeType": "image/png"},
                    "source": {"type": "image", "source": {"type": "base64", "data": "aW1hZ2U="}}
                }),
            }),
            None,
        );
        assert!(compact("tool", &mut tool));
        assert_eq!(
            tool["tool"]["action"]["args"]["inline"],
            image_placeholder()
        );
        assert_eq!(
            tool["tool"]["action"]["args"]["source"],
            image_placeholder()
        );
        assert_eq!(tool["tool"]["phase"], "succeeded");
        assert!(!compact("tool", &mut tool), "already compacted");

        let mut user =
            json!({"text":"看图","images":[{"type":"image","data":"QUJD","mimeType":"image/png"}]});
        assert!(compact("user", &mut user));
        assert_eq!(user["images"][0], image_placeholder());
        assert_eq!(user["text"], "看图");
        assert!(!compact("user", &mut user), "already compacted");
    }

    #[test]
    fn a_big_tool_output_keeps_its_head_and_tail_and_other_text_stays() {
        let out = format!(
            "{}{}{}",
            "头".repeat(HEAD_CHARS),
            "中".repeat(50_000),
            "尾".repeat(TAIL_CHARS)
        );
        let mut v = sample_tool(vec![Part::Text { text: out }], None, None);
        assert!(compact("tool", &mut v));
        let s = v["tool"]["result"][0]["text"].as_str().unwrap();
        assert!(s.starts_with(&"头".repeat(HEAD_CHARS)) && s.ends_with(&"尾".repeat(TAIL_CHARS)));
        assert!(s.contains("省略了中间 50000 个字符"), "{}", &s[..100]);
        assert_eq!(s.matches('中').count(), 1, "only the note's 中间");
        assert!(!compact("tool", &mut v), "already compacted");
        // a message's text is never cut, only a tool's output
        let mut msg =
            json!({"role": "assistant", "message": "m1", "text": "长".repeat(BIG_CHARS + 1)});
        assert!(!compact("text", &mut msg));
        let mut small = sample_tool(
            vec![Part::Text {
                text: "x".repeat(BIG_CHARS),
            }],
            None,
            None,
        );
        assert!(!compact("tool", &mut small));
    }
}
