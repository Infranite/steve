//! `codex exec --json`: the message on stdin, the stable head as Codex's instructions (a cached file),
//! images as `-i` files.

use crate::usage::codex_usage;
use crate::{
    GATEWAY_SYSTEM, Hooks, Image, PrintError, PrintRun, Ran, Reply, Stderr, ToolMode, command,
    read_to_exit, untracked,
};
use serde_json::{Value, json};
use std::path::{Path, PathBuf};
use std::process::Stdio;

/// Codex features a judgment never needs: user hooks, plugins, sub-agents, the browser and computer
/// use, image generation, goals and apps. `unified_exec` is off so a shell command shows up as a
/// `command_execution` event.
pub const CODEX_OFF: [&str; 9] = [
    "hooks",
    "plugins",
    "multi_agent",
    "computer_use",
    "browser_use",
    "image_generation",
    "goals",
    "apps",
    "unified_exec",
];

/// The arguments of one `codex exec` judgment (the prompt goes on stdin, `-`): JSON events, nothing
/// persisted, the read-only sandbox (no writes, no network for commands), no MCP servers and no web
/// search. `instructions` (a file holding the prompt's stable head) replaces Codex's own instructions
/// for a call without tools, as `--system-prompt` does for `claude -p`; a call with tools (`tools`,
/// e.g. a chat answer that reads attachments) keeps Codex's instructions and its shell.
pub fn codex_args(
    cwd: &Path,
    model: &str,
    effort: Option<&str>,
    tools: bool,
    instructions: Option<&Path>,
    images: &[PathBuf],
) -> Vec<String> {
    let mut a: Vec<String> = [
        "exec",
        "--json",
        "--ephemeral",
        "--skip-git-repo-check",
        "-s",
        "read-only",
        "-C",
    ]
    .map(String::from)
    .into();
    a.push(cwd.to_string_lossy().into_owned());
    if !model.is_empty() {
        a.extend(["-m".into(), model.into()]);
    }
    a.extend([
        "-c".into(),
        "mcp_servers={}".into(),
        "-c".into(),
        "web_search=\"disabled\"".into(),
    ]);
    for f in CODEX_OFF {
        a.extend(["--disable".into(), f.into()]);
    }
    if !tools {
        a.extend(["--disable".into(), "shell_tool".into()]);
    }
    if let Some(p) = instructions {
        a.extend([
            "-c".into(),
            format!(
                "model_instructions_file={}",
                serde_json::to_string(&p.to_string_lossy()).unwrap_or_default()
            ),
        ]);
    }
    if let Some(e) = effort {
        a.extend(["-c".into(), format!("model_reasoning_effort=\"{e}\"")]);
    }
    for i in images {
        a.extend(["-i".into(), i.to_string_lossy().into_owned()]);
    }
    a.push("-".into());
    a
}

/// How a Codex run said it failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodexEnd {
    /// An `error` event or `turn.failed` with a message.
    Message(String),
    /// `turn.failed` without one.
    Failed,
}

/// What a `codex exec --json` run has said so far: the text of its latest message, the closing usage,
/// and the error it ended with.
#[derive(Debug, Default)]
pub struct CodexStreamed {
    pub text: String,
    /// `turn.completed`'s `usage`; set when the turn ended well.
    pub usage: Option<Value>,
    pub error: Option<CodexEnd>,
}

impl CodexStreamed {
    /// Take one output line; true when the text changed.
    pub fn line(&mut self, l: &str) -> bool {
        let Ok(v) = serde_json::from_str::<Value>(l.trim()) else {
            return false;
        };
        let message = |v: &Value| v.as_str().map(|s| CodexEnd::Message(s.to_string()));
        match v.get("type").and_then(Value::as_str) {
            Some("item.completed") if v["item"]["type"] == "agent_message" => {
                match v["item"]["text"].as_str() {
                    Some(t) if !t.trim().is_empty() => {
                        self.text = t.trim_start().to_string();
                        true
                    }
                    _ => false,
                }
            }
            Some("turn.completed") => {
                self.usage = Some(v.get("usage").cloned().unwrap_or(json!({})));
                false
            }
            Some("turn.failed") => {
                self.error = message(&v["error"]["message"])
                    .or_else(|| self.error.take())
                    .or(Some(CodexEnd::Failed));
                false
            }
            Some("error") => {
                self.error = message(&v["message"]);
                false
            }
            _ => false,
        }
    }

    /// The reply and the tokens (booked to `model`).
    fn ran(self, model: &str) -> Ran {
        let usage = self
            .usage
            .as_ref()
            .map(|u| vec![codex_usage(u, model)])
            .unwrap_or_default();
        let reply = match (self.usage.is_some(), self.error) {
            (true, _) => Ok(Reply {
                text: self.text,
                cost: None,
            }),
            (false, Some(CodexEnd::Message(e))) => Err(PrintError::harness(e)),
            (false, Some(CodexEnd::Failed)) => Err(PrintError::CodexFailed),
            (false, None) => Err(PrintError::NoResult),
        };
        Ran { reply, usage }
    }
}

/// Images written out as files for `codex exec -i` (`<cwd>/codex-images`); deleted on drop.
#[derive(Default)]
struct CodexImages(Vec<PathBuf>);

impl CodexImages {
    async fn write(dir: &Path, images: &[Image]) -> std::io::Result<CodexImages> {
        let dir = dir.join("codex-images");
        tokio::fs::create_dir_all(&dir).await?;
        let mut out = CodexImages::default();
        for i in images {
            let ext = match i.mime.as_str() {
                "image/jpeg" => "jpg",
                "image/gif" => "gif",
                "image/webp" => "webp",
                _ => "png",
            };
            let p = dir.join(format!("{}.{ext}", uuid::Uuid::new_v4().simple()));
            tokio::fs::write(&p, &i.bytes).await?;
            out.0.push(p);
        }
        Ok(out)
    }
}

impl Drop for CodexImages {
    fn drop(&mut self) {
        for p in &self.0 {
            let _ = std::fs::remove_file(p);
        }
    }
}

/// The file holding `head` as Codex's instructions (`<cwd>/codex-instructions/<hash>.md`), written
/// once per distinct head.
pub async fn instructions_file(cwd: &Path, head: &str) -> std::io::Result<PathBuf> {
    use sha2::Digest;
    let hash = sha2::Sha256::digest(head.as_bytes());
    let name: String = hash.iter().take(12).map(|b| format!("{b:02x}")).collect();
    let dir = cwd.join("codex-instructions");
    let p = dir.join(format!("{name}.md"));
    if tokio::fs::metadata(&p).await.is_err() {
        tokio::fs::create_dir_all(&dir).await?;
        let tmp = dir.join(format!("{name}.{}.tmp", std::process::id()));
        tokio::fs::write(&tmp, head).await?;
        tokio::fs::rename(&tmp, &p).await?;
    }
    Ok(p)
}

/// One `codex exec` run ([`codex_args`]) with the message on stdin. Codex reports whole messages, not
/// deltas: `on_text` hears each message when it is complete.
pub(crate) async fn run(run: &PrintRun<'_>, hooks: Hooks<'_>) -> Ran {
    use tokio::io::AsyncWriteExt;
    let tools = run.tools == ToolMode::ReadOnly;
    let (instructions, input) = if tools {
        let input = match run.head {
            Some(h) => format!("{h}\n\n{}", run.message),
            None => run.message.to_string(),
        };
        (None, input)
    } else {
        match instructions_file(run.cwd, run.head.unwrap_or(GATEWAY_SYSTEM)).await {
            Ok(p) => (Some(p), run.message.to_string()),
            Err(e) => return Ran::failed(PrintError::Io(e)),
        }
    };
    // the images as files for `-i`, removed when the run ends
    let images = if run.images.is_empty() {
        CodexImages::default()
    } else {
        match CodexImages::write(run.cwd, run.images).await {
            Ok(i) => i,
            Err(e) => return Ran::failed(PrintError::Io(e)),
        }
    };
    let mut cmd = command(run.executable, run.cwd, hooks.spawned.is_some());
    cmd.args(codex_args(
        run.cwd,
        run.model,
        run.effort,
        tools,
        instructions.as_deref(),
        &images.0,
    ))
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    let mut child = match cmd.spawn() {
        Ok(c) => c,
        Err(e) => return Ran::failed(PrintError::Io(e)),
    };
    if !hooks.spawned(&child) {
        let _ = child.kill().await;
        return Ran::failed(untracked());
    }
    let (Some(mut stdin), Some(stdout), Some(stderr)) =
        (child.stdin.take(), child.stdout.take(), child.stderr.take())
    else {
        return Ran::failed(PrintError::NoOutput);
    };
    let err = Stderr::read(stderr);
    let read = async {
        stdin.write_all(input.as_bytes()).await?;
        drop(stdin);
        let mut s = CodexStreamed::default();
        // every message comes whole: each one is a new message
        let mut message = 0;
        let status = read_to_exit(stdout, &mut child, hooks.spawned.is_some(), |l| {
            if s.line(l)
                && let Some(f) = hooks.text
            {
                message += 1;
                f(message, &s.text);
            }
        })
        .await?;
        Ok::<_, std::io::Error>((s, status))
    };
    let (s, status) = match tokio::time::timeout(run.timeout, read).await {
        Err(_) => return Ran::failed(PrintError::Timeout(run.timeout)),
        Ok(Err(e)) => return Ran::failed(PrintError::Io(e)),
        Ok(Ok(x)) => x,
    };
    if s.usage.is_none() && s.error.is_none() && !status.success() {
        let e = err.after_exit(hooks.spawned.is_some()).await;
        return Ran::failed(PrintError::Failed(e.trim().to_string()));
    }
    s.ran(run.model)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_args_turn_the_shell_off_without_tools() {
        let cwd = Path::new("/s");
        let head = [
            "exec",
            "--json",
            "--ephemeral",
            "--skip-git-repo-check",
            "-s",
            "read-only",
            "-C",
            "/s",
            "-m",
            "gpt-x",
            "-c",
            "mcp_servers={}",
            "-c",
            "web_search=\"disabled\"",
            "--disable",
            "hooks",
            "--disable",
            "plugins",
            "--disable",
            "multi_agent",
            "--disable",
            "computer_use",
            "--disable",
            "browser_use",
            "--disable",
            "image_generation",
            "--disable",
            "goals",
            "--disable",
            "apps",
            "--disable",
            "unified_exec",
        ];
        let instr = Path::new("/s/instructions.md");
        let mut want: Vec<&str> = head.to_vec();
        want.extend([
            "--disable",
            "shell_tool",
            "-c",
            "model_instructions_file=\"/s/instructions.md\"",
            "-c",
            "model_reasoning_effort=\"high\"",
            "-i",
            "/s/a.png",
            "-",
        ]);
        assert_eq!(
            codex_args(
                cwd,
                "gpt-x",
                Some("high"),
                false,
                Some(instr),
                &["/s/a.png".into()]
            ),
            want
        );
        // with tools: the shell and Codex's own instructions stay
        let mut want: Vec<&str> = head.to_vec();
        want.push("-");
        assert_eq!(codex_args(cwd, "gpt-x", None, true, None, &[]), want);
        // no model named: Codex's configured default
        assert!(!codex_args(cwd, "", None, true, None, &[]).contains(&"-m".to_string()));
    }

    #[test]
    fn codex_lines() {
        let mut s = CodexStreamed::default();
        let msg = |t: &str| {
            json!({"type": "item.completed", "item": {"type": "agent_message", "text": t}})
                .to_string()
        };
        assert!(!s.line(&msg("  ")));
        assert!(s.line(&msg("  一")));
        assert_eq!(s.text, "一");
        assert!(s.line(&msg("二")), "a later message replaces the text");
        assert_eq!(s.text, "二");
        // a failure that says nothing keeps the earlier error's words
        s.line(
            &json!({"type": "error", "message": "requested model is not available"}).to_string(),
        );
        s.line(&json!({"type": "turn.failed", "error": {}}).to_string());
        assert_eq!(
            s.error,
            Some(CodexEnd::Message("requested model is not available".into()))
        );
        let mut s = CodexStreamed::default();
        s.line(&json!({"type": "turn.failed", "error": {}}).to_string());
        assert_eq!(s.error, Some(CodexEnd::Failed));
        assert!(matches!(s.ran("m").reply, Err(PrintError::CodexFailed)));
        assert!(matches!(
            CodexStreamed::default().ran("m").reply,
            Err(PrintError::NoResult)
        ));
    }
}
