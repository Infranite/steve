//! What clients and plugin taps receive of a `steve.v1` session event. Inline base64 screenshots
//! and huge tool outputs stay in the database (fork and handoff read them there), while structured
//! `Part::Diff` previews and `ArtifactRef` images/files ship intact.

use serde_json::{Map, Value};
use steve_node_api::{Action, Part, ToolView};

use crate::js::{slice, utf16_len};

const MAX: usize = 4000;

fn cut(s: &mut String) {
    if utf16_len(s) > MAX {
        *s = format!("{}…", slice(s, MAX));
    }
}

fn slim_tool(tool: &mut ToolView) {
    cut(&mut tool.title);
    if let Some(d) = &mut tool.description {
        cut(d);
    }
    if let Some(t) = &mut tool.target {
        cut(t);
    }
    if let Some(e) = &mut tool.error {
        cut(e);
    }
    if let Some(action) = &mut tool.action {
        match action {
            Action::Exec { command, .. } => cut(command),
            Action::Write { change_summary, .. } => cut(change_summary),
            Action::Search { query } => cut(query),
            Action::Fetch { url } => cut(url),
            Action::Unknown { summary } => cut(summary),
            Action::Tool { args, .. } => {
                *args = slim_value(std::mem::take(args));
            }
            Action::Read { .. } | Action::Delete { .. } | Action::Move { .. } => {}
        }
    }
    for part in &mut tool.result {
        match part {
            Part::Text { text } => cut(text),
            // Diffs and artifact references stay whole so the web renders diffs and fetches blobs.
            Part::Diff { .. } | Part::Image { .. } | Part::File { .. } => {}
        }
    }
}

fn is_inline_image(o: &Map<String, Value>) -> bool {
    o.get("type").and_then(Value::as_str) == Some("image")
        && (o.contains_key("data") || o.contains_key("source"))
}

fn slim_value(v: Value) -> Value {
    match v {
        Value::String(mut s) => {
            cut(&mut s);
            Value::String(s)
        }
        Value::Array(a) => Value::Array(a.into_iter().map(slim_value).collect()),
        Value::Object(o) => {
            if is_inline_image(&o) {
                let mut m = Map::new();
                m.insert("type".into(), Value::from("image"));
                return Value::Object(m);
            }
            let is_diff = o.get("type").and_then(Value::as_str) == Some("diff");
            Value::Object(
                o.into_iter()
                    .map(|(k, x)| {
                        let y = if is_diff && (k == "old" || k == "new") {
                            x
                        } else {
                            slim_value(x)
                        };
                        (k, y)
                    })
                    .collect(),
            )
        }
        other => other,
    }
}

/// Slims a `steve.v1` event payload for SSE / transcript API / plugin egress: replaces inline
/// base64 images with `{type:"image"}` and cuts strings over 4000 UTF-16 units (except
/// `Part::Diff` `old`/`new` fields) to 4000 + `…`.
pub fn ev_out(kind: &str, mut d: Value) -> Value {
    if kind == steve_transcript::TOOL {
        if let Some(tool_val) = d.get_mut("tool")
            && let Ok(mut tool) = serde_json::from_value::<ToolView>(tool_val.clone())
        {
            slim_tool(&mut tool);
            if let Ok(encoded) = serde_json::to_value(tool) {
                *tool_val = encoded;
                return d;
            }
        }
    }
    slim_value(d)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use steve_node_api::{
        ArtifactId, ArtifactRef, CallId, Digest, ToolCategory, ToolOrigin, ToolPhase,
    };

    fn sample_tool(result: Vec<Part>, action: Option<Action>) -> Value {
        let tool = ToolView {
            id: CallId::new("t1").unwrap(),
            parent: None,
            origin: ToolOrigin::Harness,
            name: "Bash".into(),
            category: ToolCategory::Exec,
            title: "Bash ls".into(),
            description: None,
            target: None,
            phase: ToolPhase::Succeeded,
            action,
            evidence: None,
            result,
            error: None,
            duration_ms: None,
        };
        json!({ "tool": tool })
    }

    #[test]
    fn inline_screenshots_strip_base64_while_artifact_refs_stay() {
        let png = "iVBOR".repeat(60_000);
        let user = json!({
            "text": "look",
            "images": [
                { "type": "image", "data": png, "mimeType": "image/png" },
                { "type": "image", "source": { "type": "base64", "data": png } }
            ]
        });
        let out = ev_out("user", user);
        assert_eq!(
            out["images"],
            json!([{ "type": "image" }, { "type": "image" }])
        );
        assert!(out.to_string().len() < 120);

        let artifact = ArtifactRef {
            id: ArtifactId::new("art_1").unwrap(),
            sha256: Digest::sha256(b"png"),
            size: 42,
            mime: "image/png".into(),
        };
        let tool = sample_tool(
            vec![Part::Image {
                artifact: artifact.clone(),
            }],
            None,
        );
        let tool_out = ev_out("tool", tool);
        assert_eq!(tool_out["tool"]["result"][0]["type"], "image");
        assert_eq!(tool_out["tool"]["result"][0]["artifact"]["id"], "art_1");
    }

    #[test]
    fn long_text_cut_diffs_whole() {
        let long = "x".repeat(10_000);
        let file = "line\n".repeat(5000);
        let tool = sample_tool(
            vec![
                Part::Text { text: long.clone() },
                Part::Diff {
                    path: "a.rs".into(),
                    old: Some(file.clone()),
                    new: file.clone(),
                    added: 5000,
                    removed: 5000,
                },
            ],
            Some(Action::Tool {
                name: "steve_update".into(),
                args: json!({
                    "now": long,
                    "img": { "type": "image", "data": "base64", "mimeType": "image/png" }
                }),
            }),
        );
        let out = ev_out("tool", tool);
        assert_eq!(out["tool"]["title"], json!("Bash ls"));
        assert_eq!(
            utf16_len(
                out["tool"]["result"][0]["text"]
                    .as_str()
                    .unwrap_or_default()
            ),
            4001
        );
        assert_eq!(out["tool"]["result"][1]["old"], json!(file));
        assert_eq!(out["tool"]["result"][1]["new"], json!(file));
        assert_eq!(
            utf16_len(
                out["tool"]["action"]["args"]["now"]
                    .as_str()
                    .unwrap_or_default()
            ),
            4001
        );
        assert_eq!(
            out["tool"]["action"]["args"]["img"],
            json!({ "type": "image" })
        );
        assert_eq!(ev_out("system", Value::Null), Value::Null);
        let text = json!({ "role": "assistant", "message": "m1", "text": "hi" });
        assert_eq!(ev_out("text", text.clone()), text);
    }
}
