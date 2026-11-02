//! api.ts `evOut`: what the web gets of a session event. Screenshots (base64) and huge tool output stay in the
//! database (fork and handoff read them there). Of the harness's `_meta` (a second copy of the tool output) only the
//! tool's name ships, as `tool`; of `rawInput` only what the transcript shows (the command and its description, the
//! file, the pattern, an edit's old and new text, Steve's plan).

use serde_json::{Map, Value};

use crate::js::{slice, utf16_len};

const MAX: usize = 4000;
/// the `rawInput` fields the transcript reads
const INPUT: &[&str] = &[
    "command",
    "description",
    "file_path",
    "pattern",
    "query",
    "url",
    "now",
    "plan",
    "summary",
    "question",
    "old_string",
    "new_string",
    "content",
    "edits",
];

fn slim(v: Value, key: &str) -> Value {
    match v {
        Value::String(s) if utf16_len(&s) > MAX && key != "newText" && key != "oldText" => {
            Value::String(format!("{}…", slice(&s, MAX)))
        }
        Value::Array(a) => Value::Array(a.into_iter().map(|x| slim(x, "")).collect()),
        Value::Object(o) => {
            if o.get("type").and_then(Value::as_str) == Some("image") {
                let mut m = Map::new();
                m.insert("type".into(), Value::from("image"));
                return Value::Object(m);
            }
            Value::Object(
                o.into_iter()
                    .map(|(k, x)| {
                        let y = slim(x, &k);
                        (k, y)
                    })
                    .collect(),
            )
        }
        other => other,
    }
}

/// Keeps of `_meta` only `claudeCode.toolName` (as `tool`) and of `rawInput` only [`INPUT`], replaces images by `{type:"image"}`, cuts strings over 4000 UTF-16 units
/// (except diff `oldText`/`newText`) to 4000 + `…`.
pub fn ev_out(d: Value) -> Value {
    let mut o = match d {
        Value::Object(o) => o,
        // `const {_meta, rawInput, ...x} = array` spreads indices into an object
        Value::Array(a) => a
            .into_iter()
            .enumerate()
            .map(|(i, x)| (i.to_string(), x))
            .collect(),
        other => return other,
    };
    let tool = o
        .shift_remove("_meta")
        .and_then(|m| m.pointer("/claudeCode/toolName").cloned())
        .filter(Value::is_string);
    let input = match o.shift_remove("rawInput") {
        Some(Value::Object(m)) => Some(
            m.into_iter()
                .filter(|(k, _)| INPUT.contains(&k.as_str()))
                .collect::<Map<_, _>>(),
        )
        .filter(|m| !m.is_empty()),
        _ => None,
    };
    if let Some(i) = input {
        o.insert("rawInput".into(), Value::Object(i));
    }
    if let Some(t) = tool {
        o.insert("tool".into(), t);
    }
    slim(Value::Object(o), "")
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn screenshot_ships_without_base64() {
        let png = "iVBOR".repeat(60_000);
        let d = json!({ "toolCallId": "t1", "status": "completed", "_meta": { "claudeCode": { "toolName": "Read", "toolResponse": { "stdout": png } } }, "rawInput": { "file_path": "/x.png" },
            "content": [{ "type": "content", "content": { "type": "image", "data": png, "mimeType": "image/png" } }], "rawOutput": [{ "type": "image", "source": { "type": "base64", "data": png } }] });
        let out = ev_out(d);
        assert!(out.get("_meta").is_none());
        assert_eq!(out["tool"], json!("Read"));
        assert_eq!(out["rawInput"], json!({ "file_path": "/x.png" }));
        assert_eq!(
            out["content"],
            json!([{ "type": "content", "content": { "type": "image" } }])
        );
        assert_eq!(out["rawOutput"], json!([{ "type": "image" }]));
        assert!(out.to_string().len() < 300);
    }

    #[test]
    fn long_text_cut_diffs_whole() {
        let long = "x".repeat(10_000);
        let file = "line\n".repeat(5000);
        let out = ev_out(
            json!({ "title": "Bash ls", "status": "completed", "rawOutput": long, "content": [{ "type": "content", "content": { "type": "text", "text": long } }, { "type": "diff", "path": "/a", "oldText": file, "newText": file }] }),
        );
        assert_eq!(out["title"], json!("Bash ls"));
        assert_eq!(
            utf16_len(out["rawOutput"].as_str().unwrap_or_default()),
            4001
        );
        assert_eq!(
            utf16_len(
                out["content"][0]["content"]["text"]
                    .as_str()
                    .unwrap_or_default()
            ),
            4001
        );
        assert_eq!(out["content"][1]["newText"], json!(file));
        assert_eq!(ev_out(Value::Null), Value::Null);
        let cmd = ev_out(
            json!({ "rawInput": { "command": "ls", "description": "List", "timeout": 5, "dangerouslyDisableSandbox": true }, "_meta": { "claudeCode": {} } }),
        );
        assert_eq!(
            cmd,
            json!({ "rawInput": { "command": "ls", "description": "List" } })
        );
        assert_eq!(ev_out(json!({ "rawInput": { "timeout": 5 } })), json!({}));
        let chunk = json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": "hi" } });
        assert_eq!(ev_out(chunk.clone()), chunk);
    }
}
