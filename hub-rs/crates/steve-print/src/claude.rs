//! `claude -p`: `--output-format json` for a plain call, `stream-json` with partial messages when the
//! text is wanted as it is written or images go in on stdin.

use crate::usage::{claude_usage, money};
use crate::{
    GATEWAY_SYSTEM, Image, OnText, PrintError, PrintRun, Ran, Reply, ToolMode, command, jstr,
    truthy,
};
use serde_json::{Value, json};
use std::process::Stdio;

/// The arguments of a run that reads its attachments: Read is the only tool there is (not merely
/// allowed: no shell, no writes, whatever the user's own allow rules say), and only user settings load
/// (the gateway login), not a project's or local ones.
pub const ATTACHMENT_TOOLS: [&str; 6] = [
    "--tools",
    "Read",
    "--allowedTools",
    "Read",
    "--setting-sources",
    "user",
];

/// The tool arguments of `tools`: none, or [`ATTACHMENT_TOOLS`].
pub fn tool_args(tools: ToolMode) -> &'static [&'static str] {
    match tools {
        ToolMode::None => &[],
        ToolMode::ReadOnly => &ATTACHMENT_TOOLS,
    }
}

/// The arguments that lay a `claude -p` call out for the prompt cache.
///
/// Claude Code's own system prompt, tool list and per-machine context came to ~16k tokens per call and
/// the call's instructions sat in the user message, where the cache cannot reuse them. Now:
/// - no tools: the stable head (or [`GATEWAY_SYSTEM`]) replaces the system prompt, with no tools, MCP
///   servers or skills, so the call is the head (read from the cache) plus the message;
/// - with tools: Claude Code's prompt and tools stay, the head is appended to the system prompt.
pub fn claude_layout(head: Option<&str>, tools: ToolMode) -> Vec<&str> {
    let mut a: Vec<&str> = Vec::with_capacity(6);
    match tools {
        ToolMode::None => a.extend([
            "--system-prompt",
            head.unwrap_or(GATEWAY_SYSTEM),
            "--tools",
            "",
        ]),
        ToolMode::ReadOnly => {
            if let Some(h) = head {
                a.extend(["--append-system-prompt", h]);
            }
        }
    }
    a.extend(["--strict-mcp-config", "--disable-slash-commands"]);
    a
}

/// The whole argv of a run: `stream` picks stream-json (with images the message goes to stdin).
pub fn claude_args(run: &PrintRun<'_>, stream: bool) -> Vec<String> {
    let mut a: Vec<&str> = vec!["-p"];
    if stream {
        if run.images.is_empty() {
            a.push(run.message);
        } else {
            a.extend(["--input-format", "stream-json"]);
        }
        a.extend([
            "--output-format",
            "stream-json",
            "--verbose",
            "--include-partial-messages",
        ]);
    } else {
        a.extend([run.message, "--output-format", "json"]);
    }
    a.extend(["--model", run.model]);
    a.extend(claude_layout(run.head, run.tools));
    a.extend(tool_args(run.tools));
    if let Some(e) = run.effort {
        a.extend(["--effort", e]);
    }
    a.into_iter().map(String::from).collect()
}

/// The stream-json user message of a call with images: the text, then the images as base64 blocks.
pub fn user_message(text: &str, images: &[Image]) -> String {
    use base64::Engine;
    let mut content = vec![json!({"type": "text", "text": text})];
    content.extend(images.iter().map(|i| {
        json!({"type": "image", "source": {
            "type": "base64",
            "media_type": i.mime,
            "data": base64::engine::general_purpose::STANDARD.encode(&i.bytes),
        }})
    }));
    json!({"type": "user", "message": {"role": "user", "content": content}}).to_string()
}

/// The reply of a harness `result` object: its text and cost, or its error.
pub fn result_of(d: &Value) -> Result<Reply, PrintError> {
    if d.get("is_error").is_some_and(truthy) {
        return Err(PrintError::harness(jstr(
            d.get("result").unwrap_or(&Value::Null),
        )));
    }
    let text = match d.get("result") {
        None | Some(Value::Null) => String::new(),
        Some(v) => jstr(v),
    };
    Ok(Reply {
        text,
        cost: d.get("total_cost_usd").and_then(Value::as_f64).map(money),
    })
}

fn of(d: &Value, model: &str) -> Ran {
    Ran {
        reply: result_of(d),
        usage: claude_usage(d, model),
    }
}

/// What a `stream-json` run has said so far: the text of the model's current message and, at the end,
/// the `result` object. A harness that prints only the `result` line (as `--output-format json`) works
/// too.
#[derive(Debug, Default)]
pub struct Streamed {
    pub text: String,
    pub result: Option<Value>,
}

impl Streamed {
    /// Take one output line; true when the text grew.
    pub fn line(&mut self, l: &str) -> bool {
        let Ok(v) = serde_json::from_str::<Value>(l.trim()) else {
            return false;
        };
        match v.get("type").and_then(Value::as_str) {
            Some("result") => {
                self.result = Some(v);
                false
            }
            Some("stream_event") => {
                let e = &v["event"];
                match e.get("type").and_then(Value::as_str) {
                    /* a new message (after a tool call): its text replaces the last one's */
                    Some("message_start") => {
                        self.text.clear();
                        false
                    }
                    Some("content_block_delta") if e["delta"]["type"] == "text_delta" => {
                        match e["delta"]["text"].as_str() {
                            Some(t) if !t.is_empty() => {
                                self.text.push_str(t);
                                true
                            }
                            _ => false,
                        }
                    }
                    _ => false,
                }
            }
            _ => false,
        }
    }
}

pub(crate) async fn run(run: &PrintRun<'_>, on_text: Option<OnText<'_>>) -> Ran {
    /* images go in on stdin, which only the stream-json layout takes */
    match on_text {
        Some(f) => streaming(run, f).await,
        None if !run.images.is_empty() => streaming(run, &|_| {}).await,
        None => once(run).await,
    }
}

async fn once(run: &PrintRun<'_>) -> Ran {
    let mut cmd = command(run.executable, run.cwd);
    cmd.args(claude_args(run, false)).stdin(Stdio::null());
    let out = match tokio::time::timeout(run.timeout, cmd.output()).await {
        Err(_) => return Ran::failed(PrintError::Timeout(run.timeout)),
        Ok(Err(e)) => return Ran::failed(PrintError::Io(e)),
        Ok(Ok(o)) => o,
    };
    let stdout = String::from_utf8_lossy(&out.stdout);
    match serde_json::from_str::<Value>(&stdout) {
        Ok(d) => of(&d, run.model),
        Err(_) if !out.status.success() => {
            let err = String::from_utf8_lossy(&out.stderr);
            Ran::failed(PrintError::Failed(err.trim().to_string()))
        }
        Err(_) => Ran::failed(PrintError::NoResult),
    }
}

/// With `--output-format stream-json --include-partial-messages`: text deltas go to `on_text` as they
/// arrive, the closing `result` line is the reply.
async fn streaming(run: &PrintRun<'_>, on_text: OnText<'_>) -> Ran {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt};
    let images = !run.images.is_empty();
    let mut cmd = command(run.executable, run.cwd);
    cmd.args(claude_args(run, true))
        .stdin(if images {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return Ran::failed(PrintError::Io(e)),
    };
    if images && let Some(mut w) = child.stdin.take() {
        let line = format!("{}\n", user_message(run.message, run.images));
        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            if let Err(e) = w.write_all(line.as_bytes()).await {
                tracing::warn!("writing the message to the harness: {e}");
            }
            let _ = w.shutdown().await;
        });
    }
    let (Some(stdout), Some(mut stderr)) = (child.stdout.take(), child.stderr.take()) else {
        return Ran::failed(PrintError::NoOutput);
    };
    let err = tokio::spawn(async move {
        let mut s = String::new();
        let _ = stderr.read_to_string(&mut s).await;
        s
    });
    let read = async {
        let mut lines = tokio::io::BufReader::new(stdout).lines();
        let mut s = Streamed::default();
        while let Some(l) = lines.next_line().await? {
            if s.line(&l) {
                on_text(&s.text);
            }
        }
        let status = child.wait().await?;
        Ok::<_, std::io::Error>((s, status))
    };
    let (s, status) = match tokio::time::timeout(run.timeout, read).await {
        Err(_) => return Ran::failed(PrintError::Timeout(run.timeout)),
        Ok(Err(e)) => return Ran::failed(PrintError::Io(e)),
        Ok(Ok(x)) => x,
    };
    match s.result {
        Some(d) => of(&d, run.model),
        None if !status.success() => {
            let e = err.await.unwrap_or_default();
            Ran::failed(PrintError::Failed(e.trim().to_string()))
        }
        None => Ran::failed(PrintError::NoResult),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;
    use std::time::Duration;

    fn run<'a>(
        message: &'a str,
        head: Option<&'a str>,
        tools: ToolMode,
        images: &'a [Image],
    ) -> PrintRun<'a> {
        PrintRun {
            harness: crate::Harness::Claude,
            executable: Path::new("claude"),
            cwd: Path::new("/s"),
            model: "m1",
            effort: Some("high"),
            head,
            message,
            tools,
            images,
            timeout: Duration::from_secs(1),
        }
    }

    #[test]
    fn the_argv_of_each_layout() {
        assert_eq!(
            claude_args(&run("message", Some("rules"), ToolMode::None, &[]), false),
            [
                "-p",
                "message",
                "--output-format",
                "json",
                "--model",
                "m1",
                "--system-prompt",
                "rules",
                "--tools",
                "",
                "--strict-mcp-config",
                "--disable-slash-commands",
                "--effort",
                "high"
            ]
        );
        assert_eq!(
            claude_args(&run("message", None, ToolMode::ReadOnly, &[]), true),
            [
                "-p",
                "message",
                "--output-format",
                "stream-json",
                "--verbose",
                "--include-partial-messages",
                "--model",
                "m1",
                "--strict-mcp-config",
                "--disable-slash-commands",
                "--tools",
                "Read",
                "--allowedTools",
                "Read",
                "--setting-sources",
                "user",
                "--effort",
                "high"
            ]
        );
        // with images the message is not an argument: it goes to stdin
        let img = [Image {
            mime: "image/png".into(),
            bytes: b"x".to_vec(),
        }];
        let a = claude_args(&run("message", None, ToolMode::None, &img), true);
        assert_eq!(a[..3], ["-p", "--input-format", "stream-json"]);
        assert!(!a.contains(&"message".to_string()));
        assert!(a.contains(&GATEWAY_SYSTEM.to_string()));
    }

    #[test]
    fn the_user_message_carries_the_images_as_base64() {
        let img = [Image {
            mime: "image/png".into(),
            bytes: b"image".to_vec(),
        }];
        assert_eq!(
            user_message("看图", &img),
            r#"{"type":"user","message":{"role":"user","content":[{"type":"text","text":"看图"},{"type":"image","source":{"type":"base64","media_type":"image/png","data":"aW1hZ2U="}}]}}"#
        );
    }

    #[test]
    fn stream_json_lines() {
        let mut s = Streamed::default();
        let d = |t: &str| {
            json!({"type":"stream_event","event":{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":t}}}).to_string()
        };
        let start = json!({"type":"stream_event","event":{"type":"message_start","message":{}}})
            .to_string();
        assert!(!s.line(r#"{"type":"system","subtype":"init"}"#));
        assert!(!s.line(&start));
        assert!(s.line(&d("我先读")));
        assert!(!s.line(&json!({"type":"stream_event","event":{"type":"content_block_delta","delta":{"type":"input_json_delta","partial_json":"{"}}}).to_string()));
        assert!(!s.line(&start));
        assert_eq!(s.text, "", "a new message replaces the last one's text");
        assert!(s.line(&d("{\"answer\":")) && s.line(&d("\"好\"}")));
        assert_eq!(s.text, "{\"answer\":\"好\"}");
        assert!(!s.line("not json"));
        assert!(!s.line(r#"{"type":"result","is_error":false,"result":"{\"answer\":\"好\"}","total_cost_usd":0.01}"#));
        assert_eq!(
            result_of(s.result.as_ref().unwrap()).unwrap(),
            Reply {
                text: "{\"answer\":\"好\"}".into(),
                cost: Some(money(0.01))
            }
        );
    }

    #[test]
    fn an_error_result_says_what_it_was() {
        let e =
            result_of(&json!({"is_error": true, "result": "API Error: 503 upstream unavailable"}))
                .unwrap_err();
        assert!(e.transient(), "{e:?}");
        let PrintError::Harness { text, .. } = e else {
            panic!()
        };
        assert_eq!(text, "API Error: 503 upstream unavailable");
        assert_eq!(result_of(&json!({"result": null})).unwrap().text, "");
    }
}
