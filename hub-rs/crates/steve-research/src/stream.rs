//! Parsing of `claude -p --output-format stream-json --verbose --include-partial-messages` lines into what
//! a research run cares about: text deltas, tool calls with their input, tool results, the session id and
//! the closing `result`.

use serde_json::Value;

/// Safe diagnostic category only: tool outputs may contain private data and are never retained.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolError {
    PermissionDenied,
    InvalidArguments,
    Transport,
    GuardViolation,
    Unknown,
}

impl ToolError {
    pub fn code(self) -> &'static str {
        match self {
            Self::PermissionDenied => "permission_denied",
            Self::InvalidArguments => "invalid_arguments",
            Self::Transport => "transport",
            Self::GuardViolation => "guard_violation",
            Self::Unknown => "unknown",
        }
    }
}

fn classify_error(v: &Value) -> ToolError {
    // Examine only a failing tool's error payload, not successful search results or arguments.
    let text = v.to_string().to_ascii_lowercase();
    if [
        "permission denied",
        "access denied",
        "not authorized",
        "unauthorized",
        "approval required",
        "requires approval",
        "rejected by policy",
        "denied by policy",
    ]
    .iter()
    .any(|s| text.contains(s))
    {
        ToolError::PermissionDenied
    } else if [
        "invalid arguments",
        "input validation error",
        "invalid params",
        "-32602",
    ]
    .iter()
    .any(|s| text.contains(s))
    {
        ToolError::InvalidArguments
    } else if [
        "connection refused",
        "connection reset",
        "connection closed",
        "timed out",
        "timeout",
        "error sending request",
        "transport error",
    ]
    .iter()
    .any(|s| text.contains(s))
    {
        ToolError::Transport
    } else {
        ToolError::Unknown
    }
}

/// One fact from one output line.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamItem {
    /// `system/init`: the harness session (for `--resume`) and the model it uses.
    Init { session_id: String, model: String },
    /// A new model message begins (one turn); its text replaces the previous message's.
    MessageStart { id: String },
    /// Text the model just wrote.
    TextDelta(String),
    /// A complete tool call (from the `assistant` message, so the input is whole).
    ToolUse {
        id: String,
        name: String,
        input: Value,
        message_id: String,
    },
    /// The harness's answer to a tool call: whether it failed and how long it was.
    ToolResult {
        id: String,
        is_error: bool,
        len: usize,
        error: Option<ToolError>,
    },
    /// The closing `result` object.
    Result(Value),
}

/// Every [`StreamItem`] in one line; nothing for lines that aren't JSON or don't matter.
pub fn parse_line(l: &str) -> Vec<StreamItem> {
    let Ok(v) = serde_json::from_str::<Value>(l.trim()) else {
        return Vec::new();
    };
    let s = |v: &Value| v.as_str().unwrap_or_default().to_string();
    match v.get("type").and_then(Value::as_str) {
        Some("system") if v["subtype"] == "init" => vec![StreamItem::Init {
            session_id: s(&v["session_id"]),
            model: s(&v["model"]),
        }],
        Some("result") => vec![StreamItem::Result(v)],
        Some("stream_event") => {
            let e = &v["event"];
            match e.get("type").and_then(Value::as_str) {
                Some("message_start") => vec![StreamItem::MessageStart {
                    id: s(&e["message"]["id"]),
                }],
                Some("content_block_delta") if e["delta"]["type"] == "text_delta" => {
                    match e["delta"]["text"].as_str() {
                        Some(t) if !t.is_empty() => vec![StreamItem::TextDelta(t.to_string())],
                        _ => Vec::new(),
                    }
                }
                _ => Vec::new(),
            }
        }
        Some("assistant") => {
            let message_id = s(&v["message"]["id"]);
            blocks(&v)
                .filter(|b| b["type"] == "tool_use")
                .map(|b| StreamItem::ToolUse {
                    id: s(&b["id"]),
                    name: s(&b["name"]),
                    input: b.get("input").cloned().unwrap_or(Value::Null),
                    message_id: message_id.clone(),
                })
                .collect()
        }
        Some("user") => blocks(&v)
            .filter(|b| b["type"] == "tool_result")
            .map(|b| StreamItem::ToolResult {
                id: s(&b["tool_use_id"]),
                is_error: b["is_error"].as_bool().unwrap_or(false),
                len: content_len(&b["content"]),
                error: b["is_error"]
                    .as_bool()
                    .unwrap_or(false)
                    .then(|| classify_error(&b["content"])),
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Every [`StreamItem`] in one line of `codex exec --json`: a shell command is a `Shell` tool call, an
/// MCP call `mcp__<server>__<tool>`, a web search `WebSearch` and a file change `Edit` (both refused by
/// the guard). Each agent message is a new message with its whole text. The end of the turn is a
/// `result`-like object: `{"harness": "codex", "is_error", "result", "usage"}`; its `result` is null on
/// success (the answer is the last message).
pub fn parse_codex_line(l: &str) -> Vec<StreamItem> {
    let Ok(v) = serde_json::from_str::<Value>(l.trim()) else {
        return Vec::new();
    };
    let s = |v: &Value| v.as_str().unwrap_or_default().to_string();
    let item = &v["item"];
    let id = s(&item["id"]);
    let tool = |name: String, input: Value| StreamItem::ToolUse {
        id: id.clone(),
        name,
        input,
        message_id: String::new(),
    };
    let call = || -> Option<StreamItem> {
        Some(match item["type"].as_str()? {
            "command_execution" => tool(
                "Shell".into(),
                serde_json::json!({"command": item["command"]}),
            ),
            "mcp_tool_call" => tool(
                format!("mcp__{}__{}", s(&item["server"]), s(&item["tool"])),
                item.get("arguments").cloned().unwrap_or(Value::Null),
            ),
            "web_search" => tool(
                "WebSearch".into(),
                serde_json::json!({"query": item["query"]}),
            ),
            "file_change" => tool(
                "Edit".into(),
                serde_json::json!({"changes": item["changes"]}),
            ),
            _ => return None,
        })
    };
    match v.get("type").and_then(Value::as_str) {
        Some("item.started") => call().into_iter().collect(),
        Some("item.completed") if item["type"] == "agent_message" => {
            let t = s(&item["text"]);
            let mut out = vec![StreamItem::MessageStart { id: id.clone() }];
            if !t.trim().is_empty() {
                out.push(StreamItem::TextDelta(t.trim_start().to_string()));
            }
            out
        }
        Some("item.completed") => {
            let Some(c) = call() else { return Vec::new() };
            let failed = item["status"] == "failed"
                || item["exit_code"].as_i64().is_some_and(|c| c != 0)
                || item.get("error").is_some_and(|e| !e.is_null())
                || item.pointer("/result/isError").and_then(Value::as_bool) == Some(true);
            let len = item["aggregated_output"]
                .as_str()
                .map_or(0, |o| o.chars().count());
            let error = failed.then(|| {
                let payload = item
                    .get("error")
                    .filter(|e| !e.is_null())
                    .or_else(|| item.pointer("/result/content"))
                    .unwrap_or(&item["aggregated_output"]);
                classify_error(payload)
            });
            vec![
                c,
                StreamItem::ToolResult {
                    id,
                    is_error: failed,
                    len,
                    error,
                },
            ]
        }
        Some("turn.completed") => vec![StreamItem::Result(serde_json::json!({
            "type": "result", "harness": "codex", "is_error": false, "result": null,
            "usage": v.get("usage").cloned().unwrap_or(Value::Null),
        }))],
        Some("turn.failed") => vec![StreamItem::Result(serde_json::json!({
            "type": "result", "harness": "codex", "is_error": true,
            "result": v["error"]["message"].as_str().unwrap_or("Codex 失败"),
        }))],
        _ => Vec::new(),
    }
}

fn blocks(v: &Value) -> impl Iterator<Item = &Value> {
    v["message"]["content"]
        .as_array()
        .map(|a| a.iter())
        .into_iter()
        .flatten()
}

fn content_len(c: &Value) -> usize {
    match c {
        Value::String(s) => s.chars().count(),
        Value::Array(a) => a
            .iter()
            .map(|b| b["text"].as_str().map_or(0, |t| t.chars().count()))
            .sum(),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn failures_keep_categories_not_private_error_text() {
        for (message, expected) in [
            (
                "Permission denied: secret-token-123",
                ToolError::PermissionDenied,
            ),
            (
                "MCP tool call requires approval, but approval policy is never",
                ToolError::PermissionDenied,
            ),
            (
                "Input validation error: secret-token-123",
                ToolError::InvalidArguments,
            ),
            ("connection refused: secret-token-123", ToolError::Transport),
            ("could not search secret-token-123", ToolError::Unknown),
        ] {
            let claude = serde_json::json!({"type":"user","message":{"content":[{
                "type":"tool_result","tool_use_id":"t1","is_error":true,"content":message
            }]}});
            let codex = serde_json::json!({"type":"item.completed","item":{
                "type":"mcp_tool_call","id":"t1","server":"steve","tool":"steve_item_read",
                "arguments":{"id":"ST-23"},"status":"failed","error":{"message":message}
            }});
            for parsed in [
                parse_line(&claude.to_string()),
                parse_codex_line(&codex.to_string()),
            ] {
                assert!(parsed.iter().any(|p| matches!(p, StreamItem::ToolResult { is_error:true, error:Some(e), .. } if *e == expected)));
                assert!(!format!("{parsed:?}").contains("secret-token-123"));
            }
        }
    }

    #[test]
    fn mcp_business_failure_is_not_a_successful_transport() {
        let event = serde_json::json!({"type":"item.completed","item":{
            "type":"mcp_tool_call","id":"t1","server":"steve","tool":"steve_item_read",
            "arguments":{"id":"ST-23"},"status":"completed",
            "result":{"isError":true,"content":[{"type":"text","text":"Invalid arguments: private"}]}
        }});
        assert!(matches!(
            &parse_codex_line(&event.to_string())[..],
            [
                _,
                StreamItem::ToolResult {
                    is_error: true,
                    error: Some(ToolError::InvalidArguments),
                    ..
                }
            ]
        ));
        let mut successful = event;
        successful["item"]["result"]["isError"] = serde_json::json!(false);
        successful["item"]["result"]["content"][0]["text"] = serde_json::json!("permission denied");
        assert!(matches!(
            &parse_codex_line(&successful.to_string())[..],
            [
                _,
                StreamItem::ToolResult {
                    is_error: false,
                    error: None,
                    ..
                }
            ]
        ));
    }

    #[test]
    fn parses_the_shapes_the_harness_prints() {
        let init = r#"{"type":"system","subtype":"init","cwd":"/p","session_id":"s1","tools":["Read"],"model":"claude-sonnet-5-5"}"#;
        assert_eq!(
            parse_line(init),
            vec![StreamItem::Init {
                session_id: "s1".into(),
                model: "claude-sonnet-5-5".into()
            }]
        );
        let start =
            r#"{"type":"stream_event","event":{"type":"message_start","message":{"id":"m1"}}}"#;
        assert_eq!(
            parse_line(start),
            vec![StreamItem::MessageStart { id: "m1".into() }]
        );
        let delta = r#"{"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"text_delta","text":"hi"}}}"#;
        assert_eq!(parse_line(delta), vec![StreamItem::TextDelta("hi".into())]);
        let tool = r#"{"type":"assistant","message":{"id":"m1","content":[{"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"/p/a.rs"}}]}}"#;
        assert!(
            matches!(&parse_line(tool)[..], [StreamItem::ToolUse { name, .. }] if name == "Read")
        );
        let res = r#"{"type":"user","message":{"role":"user","content":[{"tool_use_id":"t1","type":"tool_result","content":"abc","is_error":true}]}}"#;
        assert_eq!(
            parse_line(res),
            vec![StreamItem::ToolResult {
                id: "t1".into(),
                is_error: true,
                len: 3,
                error: Some(ToolError::Unknown),
            }]
        );
        assert!(matches!(
            &parse_line(r#"{"type":"result","result":"x"}"#)[..],
            [StreamItem::Result(_)]
        ));
        assert!(parse_line("not json").is_empty());
    }

    #[test]
    fn parses_codex_events() {
        let cmd = r#"{"type":"item.started","item":{"id":"item_1","type":"command_execution","command":"/bin/zsh -lc 'cat a.txt'","aggregated_output":"","exit_code":null,"status":"in_progress"}}"#;
        assert!(
            matches!(&parse_codex_line(cmd)[..], [StreamItem::ToolUse { name, input, .. }] if name == "Shell" && input["command"] == "/bin/zsh -lc 'cat a.txt'")
        );
        let done = r#"{"type":"item.completed","item":{"id":"item_1","type":"command_execution","command":"/bin/zsh -lc 'cat a.txt'","aggregated_output":"hello\n","exit_code":0,"status":"completed"}}"#;
        assert!(matches!(
            &parse_codex_line(done)[..],
            [
                StreamItem::ToolUse { .. },
                StreamItem::ToolResult {
                    is_error: false,
                    len: 6,
                    ..
                }
            ]
        ));
        let mcp = r#"{"type":"item.started","item":{"id":"i2","type":"mcp_tool_call","server":"steve","tool":"steve_knowledge_search","arguments":{"query":"x"},"status":"in_progress"}}"#;
        assert!(
            matches!(&parse_codex_line(mcp)[..], [StreamItem::ToolUse { name, .. }] if name == "mcp__steve__steve_knowledge_search")
        );
        let msg =
            r#"{"type":"item.completed","item":{"id":"i3","type":"agent_message","text":"答"}}"#;
        assert_eq!(
            parse_codex_line(msg),
            vec![
                StreamItem::MessageStart { id: "i3".into() },
                StreamItem::TextDelta("答".into())
            ]
        );
        let end = r#"{"type":"turn.completed","usage":{"input_tokens":10,"cached_input_tokens":4,"output_tokens":2}}"#;
        assert!(
            matches!(&parse_codex_line(end)[..], [StreamItem::Result(r)] if r["is_error"] == false && r["usage"]["input_tokens"] == 10)
        );
        let failed =
            r#"{"type":"turn.failed","error":{"message":"requested model is not available"}}"#;
        assert!(
            matches!(&parse_codex_line(failed)[..], [StreamItem::Result(r)] if r["is_error"] == true)
        );
        assert!(
            parse_codex_line(
                r#"{"type":"item.completed","item":{"id":"x","type":"reasoning","text":"…"}}"#
            )
            .is_empty()
        );
        assert!(
            matches!(&parse_codex_line(r#"{"type":"item.started","item":{"id":"w","type":"web_search","query":"q"}}"#)[..], [StreamItem::ToolUse { name, .. }] if name == "WebSearch")
        );
    }
}
