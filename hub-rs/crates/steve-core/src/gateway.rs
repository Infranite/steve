//! Model gateway (gateway.ts): Steve's own short model calls through the harness CLI in print mode, with a
//! timeout per call and a fallback down each tier's model list. The router, chat answers and config drafts
//! are built here from data the caller collected under the core lock, so no lock is held while a model runs.

use crate::error::{CoreError, Result, msg};
use crate::repo::{Bot, Project, Tag};
use crate::util::{js_head, js_len};
use async_trait::async_trait;
use regex::Regex;
use serde_json::{Value, json};
use std::path::PathBuf;
use std::sync::LazyLock;
use std::time::{Duration, Instant};
pub use steve_store::ModelUsage;

/// "fast" is for routing and other short calls, "smart" for answers and drafts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    Fast,
    Smart,
}

/// A model's reply and which model of the tier gave it.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Printed {
    pub text: String,
    pub model: String,
    /// The harness's `total_cost_usd` of the answering run, when it reports one (module budgets add it up).
    pub cost_usd: Option<f64>,
    /// Tokens per model over every model tried, failed tries included (stats.md §2.4).
    pub usage: Vec<ModelUsage>,
    /// Models of the tier tried, the one that answered included.
    pub attempts: u32,
    /// Wall time of the whole call, fallbacks included.
    pub duration_ms: u64,
}

/// One call as the statistics see it: the reply or the error, and what it took either way.
#[derive(Debug)]
pub struct Attempt {
    pub result: Result<Printed>,
    /// Models tried.
    pub attempts: u32,
    pub duration_ms: u64,
    /// Tokens per model, failed tries included.
    pub usage: Vec<ModelUsage>,
    /// The model that answered, or the last one tried.
    pub model: String,
    /// `<model>: <error>` of each try that failed (503, timeout…).
    pub failures: Vec<String>,
    /// The harness the call ran on as the statistics name it (`claude-cli`, `codex-cli`); empty: `claude-cli`.
    pub harness: &'static str,
}

/// Told what the model has written so far in its current message, each time it grows.
pub type OnText<'a> = &'a (dyn Fn(&str) + Send + Sync);

/// One model call with fallback; implemented by [`ClaudeCli`], faked in tests.
#[async_trait]
pub trait ModelGateway: Send + Sync {
    /// `claudePrintWith`: `extra` are CLI arguments (e.g. `--allowedTools Read` for a chat answer that reads attachments).
    async fn call(
        &self,
        prompt: &str,
        tier: Tier,
        timeout: Duration,
        extra: &[String],
    ) -> Result<Printed>;
    /// [`ModelGateway::call`] that reports the text while it is generated (a chat answer shown as it is
    /// written). Without streaming, `on_text` hears the whole text once.
    async fn call_streaming(
        &self,
        prompt: &str,
        tier: Tier,
        timeout: Duration,
        extra: &[String],
        on_text: OnText<'_>,
    ) -> Result<Printed> {
        let p = self.call(prompt, tier, timeout, extra).await?;
        on_text(&p.text);
        Ok(p)
    }
    /// `claudePrint`: the text only.
    async fn print(&self, prompt: &str, tier: Tier, timeout: Duration) -> Result<String> {
        Ok(self.call(prompt, tier, timeout, &[]).await?.text)
    }
    /// [`ModelGateway::call`] (or [`ModelGateway::call_streaming`] with `on_text`) that also tells, when
    /// the call fails, how many models were tried and what they used; for [`crate::stats::RecordingGateway`].
    async fn attempt(
        &self,
        prompt: &str,
        tier: Tier,
        timeout: Duration,
        extra: &[String],
        on_text: Option<OnText<'_>>,
    ) -> Attempt {
        let t0 = Instant::now();
        let result = match on_text {
            Some(f) => self.call_streaming(prompt, tier, timeout, extra, f).await,
            None => self.call(prompt, tier, timeout, extra).await,
        };
        let ms = elapsed_ms(t0);
        match result {
            Ok(p) => Attempt {
                attempts: p.attempts.max(1),
                duration_ms: if p.duration_ms > 0 { p.duration_ms } else { ms },
                usage: p.usage.clone(),
                model: p.model.clone(),
                failures: vec![],
                harness: "",
                result: Ok(p),
            },
            Err(e) => Attempt {
                attempts: 1,
                duration_ms: ms,
                usage: vec![],
                model: String::new(),
                failures: vec![js_head(&e.to_string(), 200).to_string()],
                harness: "",
                result: Err(e),
            },
        }
    }
}

fn elapsed_ms(t0: Instant) -> u64 {
    u64::try_from(t0.elapsed().as_millis()).unwrap_or(u64::MAX)
}

/// One run of the CLI: its reply (text and `total_cost_usd`) or error, and the tokens it reported either way.
struct Tried {
    reply: Result<(String, Option<f64>)>,
    usage: Vec<ModelUsage>,
}

impl Tried {
    fn failed(e: CoreError) -> Tried {
        Tried {
            reply: Err(e),
            usage: vec![],
        }
    }
    fn of(d: &Value, model: &str) -> Tried {
        Tried {
            reply: result_of(d),
            usage: usage_of(d, model),
        }
    }
}

/// Steve's judgments through a harness CLI in `STEVE_HOME/scratch`, each model of the list in turn: the
/// purpose's harness and list from 设置 · 模型 ([`crate::models::Models::for_call`], Steve's bot unless
/// overridden) when `models` is set, else the tier's on Claude Code.
///
/// - Claude Code: `claude -p <prompt> --output-format json --model <m>`.
/// - Codex: `codex exec --json --ephemeral -s read-only [-m <m>] -` with the prompt on stdin (see
///   [`codex_args`]).
pub struct ClaudeCli {
    pub executable: PathBuf,
    /// `codex` for purposes that run on Codex; `None`: such calls fail with a clear error.
    pub codex: Option<PathBuf>,
    pub fast: Vec<String>,
    pub smart: Vec<String>,
    pub cwd: PathBuf,
    /// The resolver of 设置 · 模型; `None` (tests) uses `fast` / `smart` as they are.
    pub models: Option<crate::models::Models>,
}

impl ClaudeCli {
    /// `extra` is the caller's (tools it wants); `effort` comes from 设置 · 模型 and is kept apart so a
    /// call without tools keeps its cache-friendly layout ([`print_args`]) when an effort is set.
    async fn once(
        &self,
        prompt: &str,
        model: &str,
        timeout: Duration,
        extra: &[String],
        effort: &[String],
    ) -> Tried {
        let mut cmd = tokio::process::Command::new(&self.executable);
        for key in [
            "STEVE_UPGRADE_CONTROL_TOKEN",
            "STEVE_SUPERVISOR_PID",
            "STEVE_UPGRADE_ID",
        ] {
            cmd.env_remove(key);
        }
        let (message, layout) = print_args(prompt, extra);
        cmd.args(["-p", message, "--output-format", "json", "--model", model])
            .args(layout)
            .args(extra)
            .args(effort)
            .current_dir(&self.cwd)
            .stdin(std::process::Stdio::null())
            .kill_on_drop(true);
        let out = match tokio::time::timeout(timeout, cmd.output()).await {
            Err(_) => {
                return Tried::failed(msg(format!(
                    "Command timed out after {} ms",
                    timeout.as_millis()
                )));
            }
            Ok(Err(e)) => return Tried::failed(CoreError::Io(e)),
            Ok(Ok(o)) => o,
        };
        let stdout = String::from_utf8_lossy(&out.stdout);
        let d: Value = match serde_json::from_str(&stdout) {
            Ok(d) => d,
            Err(_) if !out.status.success() => {
                let err = String::from_utf8_lossy(&out.stderr);
                return Tried::failed(msg(format!("Command failed: {}", err.trim())));
            }
            Err(_) => return Tried::failed(msg("harness 没有返回结果")),
        };
        Tried::of(&d, model)
    }

    /// [`ClaudeCli::once`] with `--output-format stream-json --include-partial-messages`: text deltas go to
    /// `on_text` as they arrive, the closing `result` line is the reply.
    async fn once_streaming(
        &self,
        prompt: &str,
        model: &str,
        timeout: Duration,
        extra: &[String],
        effort: &[String],
        on_text: OnText<'_>,
    ) -> Tried {
        use tokio::io::{AsyncBufReadExt, AsyncReadExt};
        let mut cmd = tokio::process::Command::new(&self.executable);
        for key in [
            "STEVE_UPGRADE_CONTROL_TOKEN",
            "STEVE_SUPERVISOR_PID",
            "STEVE_UPGRADE_ID",
        ] {
            cmd.env_remove(key);
        }
        let (message, layout) = print_args(prompt, extra);
        let images = current_images();
        /* with images the message goes to stdin as one stream-json user message (text, then images) */
        let head: Vec<&str> = match &images {
            Some(_) => vec!["-p", "--input-format", "stream-json"],
            None => vec!["-p", message],
        };
        cmd.args(head)
            .args([
                "--output-format",
                "stream-json",
                "--verbose",
                "--include-partial-messages",
                "--model",
                model,
            ])
            .args(layout)
            .args(extra)
            .args(effort)
            .current_dir(&self.cwd)
            .stdin(if images.is_some() {
                std::process::Stdio::piped()
            } else {
                std::process::Stdio::null()
            })
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .kill_on_drop(true);
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => return Tried::failed(CoreError::Io(e)),
        };
        if let (Some(imgs), Some(mut w)) = (&images, child.stdin.take()) {
            let line = format!("{}\n", user_message(message, imgs));
            tokio::spawn(async move {
                use tokio::io::AsyncWriteExt;
                if let Err(e) = w.write_all(line.as_bytes()).await {
                    tracing::warn!("writing the message to the harness: {e}");
                }
                let _ = w.shutdown().await;
            });
        }
        let (Some(stdout), Some(mut stderr)) = (child.stdout.take(), child.stderr.take()) else {
            return Tried::failed(msg("harness 没有输出"));
        };
        let err = tokio::spawn(async move {
            let mut s = String::new();
            let _ = stderr.read_to_string(&mut s).await;
            s
        });
        let run = async {
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
        let (s, status) = match tokio::time::timeout(timeout, run).await {
            Err(_) => {
                return Tried::failed(msg(format!(
                    "Command timed out after {} ms",
                    timeout.as_millis()
                )));
            }
            Ok(Err(e)) => return Tried::failed(CoreError::Io(e)),
            Ok(Ok(x)) => x,
        };
        match s.result {
            Some(d) => Tried::of(&d, model),
            None if !status.success() => {
                let e = err.await.unwrap_or_default();
                Tried::failed(msg(format!("Command failed: {}", e.trim())))
            }
            None => Tried::failed(msg("harness 没有返回结果")),
        }
    }
}

/// The statistics' harness name of `codex exec` calls (`usage_records.harness`).
pub const CODEX_HARNESS: &str = "codex-cli";

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
    cwd: &std::path::Path,
    model: &str,
    effort: Option<&str>,
    tools: bool,
    instructions: Option<&std::path::Path>,
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

/// What a `codex exec --json` run has said so far: the text of its latest message, the closing usage,
/// and the error it ended with.
#[derive(Debug, Default)]
pub struct CodexStreamed {
    pub text: String,
    /// `turn.completed`'s `usage`; set when the turn ended well.
    pub usage: Option<Value>,
    pub error: Option<String>,
}

impl CodexStreamed {
    /// Take one output line; true when the text changed.
    pub fn line(&mut self, l: &str) -> bool {
        let Ok(v) = serde_json::from_str::<Value>(l.trim()) else {
            return false;
        };
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
                self.error = v["error"]["message"]
                    .as_str()
                    .map(String::from)
                    .or_else(|| self.error.take())
                    .or_else(|| Some("Codex 失败".into()));
                false
            }
            Some("error") => {
                self.error = v["message"].as_str().map(String::from);
                false
            }
            _ => false,
        }
    }

    /// The reply and the tokens (booked to `model`, Codex reports no per-model split).
    fn tried(self, model: &str) -> Tried {
        let usage = self
            .usage
            .as_ref()
            .map(|u| {
                vec![ModelUsage {
                    model: model.to_string(),
                    tokens: steve_stats::normalize(
                        "codex",
                        steve_stats::RawUsage::from_json(u),
                        true,
                    ),
                    cost_reported: None,
                    detail: json!({"scope": "main"}),
                }]
            })
            .unwrap_or_default();
        let reply = match (self.usage.is_some(), self.error) {
            (true, _) => Ok((self.text, None)),
            (false, Some(e)) => Err(msg(js_head(&e, 300).to_string())),
            (false, None) => Err(msg("harness 没有返回结果")),
        };
        Tried { reply, usage }
    }
}

/// Image blocks written out as files for `codex exec -i`; deleted on drop.
#[derive(Default)]
struct CodexImages(Vec<PathBuf>);

impl CodexImages {
    async fn write(dir: &std::path::Path, blocks: &[Value]) -> std::io::Result<CodexImages> {
        use base64::Engine;
        let dir = dir.join("codex-images");
        tokio::fs::create_dir_all(&dir).await?;
        let mut out = CodexImages::default();
        for b in blocks {
            let src = &b["source"];
            let Some(data) = src["data"].as_str() else {
                continue;
            };
            let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(data) else {
                continue;
            };
            let ext = match src["media_type"].as_str().unwrap_or("") {
                "image/jpeg" => "jpg",
                "image/gif" => "gif",
                "image/webp" => "webp",
                _ => "png",
            };
            let p = dir.join(format!("{}.{ext}", uuid::Uuid::new_v4().simple()));
            tokio::fs::write(&p, bytes).await?;
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

impl ClaudeCli {
    /// The file holding `head` as Codex's instructions (`STEVE_HOME/scratch/codex-instructions/<hash>.md`),
    /// written once per distinct head.
    async fn instructions_file(&self, head: &str) -> std::io::Result<PathBuf> {
        use sha2::Digest;
        let hash = sha2::Sha256::digest(head.as_bytes());
        let name: String = hash.iter().take(12).map(|b| format!("{b:02x}")).collect();
        let dir = self.cwd.join("codex-instructions");
        let p = dir.join(format!("{name}.md"));
        if tokio::fs::metadata(&p).await.is_err() {
            tokio::fs::create_dir_all(&dir).await?;
            let tmp = dir.join(format!("{name}.{}.tmp", std::process::id()));
            tokio::fs::write(&tmp, head).await?;
            tokio::fs::rename(&tmp, &p).await?;
        }
        Ok(p)
    }

    /// One `codex exec` run ([`codex_args`]) with the prompt on stdin. Codex reports whole messages, not
    /// deltas: `on_text` hears each message when it is complete.
    async fn once_codex(
        &self,
        prompt: &str,
        model: &str,
        timeout: Duration,
        extra: &[String],
        effort: Option<&str>,
        on_text: Option<OnText<'_>>,
    ) -> Tried {
        use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt};
        let Some(exe) = &self.codex else {
            return Tried::failed(msg(
                "没有配置 codex（CODEX_EXECUTABLE），不能用 Codex 做判断",
            ));
        };
        let tools = !extra.is_empty();
        let (head, message) = split_static(prompt);
        let (instructions, input) = if tools {
            let input = match head {
                Some(h) => format!("{h}\n\n{message}"),
                None => message.to_string(),
            };
            (None, input)
        } else {
            match self.instructions_file(head.unwrap_or(GATEWAY_SYSTEM)).await {
                Ok(p) => (Some(p), message.to_string()),
                Err(e) => return Tried::failed(CoreError::Io(e)),
            }
        };
        // the images of the call ([`with_images`]) as files for `-i`, removed when the call ends
        let images = match current_images() {
            Some(imgs) => match CodexImages::write(&self.cwd, &imgs).await {
                Ok(i) => i,
                Err(e) => return Tried::failed(CoreError::Io(e)),
            },
            None => CodexImages::default(),
        };
        let mut cmd = tokio::process::Command::new(exe);
        for key in [
            "STEVE_UPGRADE_CONTROL_TOKEN",
            "STEVE_SUPERVISOR_PID",
            "STEVE_UPGRADE_ID",
        ] {
            cmd.env_remove(key);
        }
        cmd.args(codex_args(
            &self.cwd,
            model,
            effort,
            tools,
            instructions.as_deref(),
            &images.0,
        ))
        .current_dir(&self.cwd)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);
        let mut child = match cmd.spawn() {
            Ok(c) => c,
            Err(e) => return Tried::failed(CoreError::Io(e)),
        };
        let (Some(mut stdin), Some(stdout), Some(mut stderr)) =
            (child.stdin.take(), child.stdout.take(), child.stderr.take())
        else {
            return Tried::failed(msg("harness 没有输出"));
        };
        let err = tokio::spawn(async move {
            let mut s = String::new();
            let _ = stderr.read_to_string(&mut s).await;
            s
        });
        let run = async {
            stdin.write_all(input.as_bytes()).await?;
            drop(stdin);
            let mut lines = tokio::io::BufReader::new(stdout).lines();
            let mut s = CodexStreamed::default();
            while let Some(l) = lines.next_line().await? {
                if s.line(&l)
                    && let Some(f) = on_text
                {
                    f(&s.text);
                }
            }
            let status = child.wait().await?;
            Ok::<_, std::io::Error>((s, status))
        };
        let (s, status) = match tokio::time::timeout(timeout, run).await {
            Err(_) => {
                return Tried::failed(msg(format!(
                    "Command timed out after {} ms",
                    timeout.as_millis()
                )));
            }
            Ok(Err(e)) => return Tried::failed(CoreError::Io(e)),
            Ok(Ok(x)) => x,
        };
        if s.usage.is_none() && s.error.is_none() && !status.success() {
            let e = err.await.unwrap_or_default();
            return Tried::failed(msg(format!("Command failed: {}", js_head(e.trim(), 300))));
        }
        s.tried(model)
    }
}

pub use steve_modules::gateway::{STATIC_END, split_static, with_static};

tokio::task_local! {
    /// Image blocks for the user message of the calls made inside [`with_images`].
    static IMAGES: std::sync::Arc<Vec<Value>>;
}

/// Runs `f` with `images` (`{"type":"image","source":{…}}` blocks) attached to the user message of every
/// model call it makes, like [`crate::stats::tagged`] carries the call's purpose: a chat answer about a
/// screenshot sees the screenshot without a tool call.
pub async fn with_images<F: std::future::Future>(images: Vec<Value>, f: F) -> F::Output {
    if images.is_empty() {
        return f.await;
    }
    IMAGES.scope(std::sync::Arc::new(images), f).await
}

/// The images of the current call, if any (see [`with_images`]).
pub fn current_images() -> Option<std::sync::Arc<Vec<Value>>> {
    IMAGES
        .try_with(|i| i.clone())
        .ok()
        .filter(|i| !i.is_empty())
}

/// The stream-json user message of a call with images: the text, then the images.
pub fn user_message(text: &str, images: &[Value]) -> String {
    let mut content = vec![json!({"type": "text", "text": text})];
    content.extend(images.iter().cloned());
    json!({"type": "user", "message": {"role": "user", "content": content}}).to_string()
}

/// The system prompt of a gateway call whose prompt has no stable head of its own.
pub const GATEWAY_SYSTEM: &str = "You are a component of Steve, a personal assistant app. Follow the instructions in the user message exactly and reply with only what they ask for.";

/// The CLI arguments of a chat answer: none without readable attachments (no tools at all), else
/// [`ATTACHMENT_TOOLS`].
pub fn answer_tools(files: &ChatFiles) -> Vec<String> {
    if files.readable {
        ATTACHMENT_TOOLS.iter().map(|a| a.to_string()).collect()
    } else {
        vec![]
    }
}

/// The CLI arguments of a chat answer that reads its attachments: Read is the only tool there is (not
/// merely allowed: no shell, no writes, whatever the user's own allow rules say), and only user settings
/// load (the gateway login), not a project's or local ones.
pub const ATTACHMENT_TOOLS: [&str; 6] = [
    "--tools",
    "Read",
    "--allowedTools",
    "Read",
    "--setting-sources",
    "user",
];

/// The message for `-p` and the arguments that lay a `claude -p` call out for the prompt cache.
///
/// Claude Code's own system prompt, tool list and per-machine context came to ~16k tokens per call and
/// the call's instructions sat in the user message, where the cache cannot reuse them; one route call
/// cost ~20k tokens, about half of them cache writes. Now:
/// - no `extra` (no tools): the prompt's stable head (or [`GATEWAY_SYSTEM`]) replaces the system prompt,
///   with no tools, MCP servers or skills, so the call is the head (read from the cache) plus the
///   message;
/// - with `extra` (e.g. a chat answer that reads attachments): Claude Code's prompt and tools stay, the
///   head is appended to the system prompt.
pub fn print_args<'a>(prompt: &'a str, extra: &[String]) -> (&'a str, Vec<&'a str>) {
    let (head, message) = split_static(prompt);
    let mut a: Vec<&str> = Vec::with_capacity(6);
    if extra.is_empty() {
        a.extend([
            "--system-prompt",
            head.unwrap_or(GATEWAY_SYSTEM),
            "--tools",
            "",
        ]);
    } else if let Some(h) = head {
        a.extend(["--append-system-prompt", h]);
    }
    a.extend(["--strict-mcp-config", "--disable-slash-commands"]);
    (message, a)
}

/// The reply of a harness `result` object: its text and cost, or its error.
fn result_of(d: &Value) -> Result<(String, Option<f64>)> {
    if d.get("is_error").is_some_and(truthy) {
        return Err(msg(js_head(
            &jstr(d.get("result").unwrap_or(&Value::Null)),
            300,
        )
        .to_string()));
    }
    let text = match d.get("result") {
        None | Some(Value::Null) => String::new(),
        Some(v) => jstr(v),
    };
    Ok((text, d.get("total_cost_usd").and_then(Value::as_f64)))
}

/// The tokens a harness `result` object reports (stats.md §2.4): one row per key of `modelUsage` (the
/// models that actually answered, sub-calls included), else one row from `usage` booked to `model`, the
/// model asked for. Missing figures stay `None`. Dollars: each model's `costUSD`, else `total_cost_usd`
/// on the first row.
pub fn usage_of(d: &Value, model: &str) -> Vec<ModelUsage> {
    let total = d.get("total_cost_usd").and_then(Value::as_f64);
    let tokens =
        |u: &Value| steve_stats::normalize("claude-cli", steve_stats::RawUsage::from_json(u), true);
    let mut rows: Vec<ModelUsage> = match (d.get("modelUsage"), d.get("usage")) {
        (Some(Value::Object(m)), _) if !m.is_empty() => m
            .iter()
            .map(|(k, u)| ModelUsage {
                model: k.clone(),
                tokens: tokens(u),
                cost_reported: u.get("costUSD").and_then(Value::as_f64),
                detail: json!({"scope": "all"}),
            })
            .collect(),
        (_, Some(u @ Value::Object(_))) => vec![ModelUsage {
            model: model.to_string(),
            tokens: tokens(u),
            cost_reported: None,
            detail: json!({"scope": "main"}),
        }],
        _ if total.is_some() => vec![ModelUsage {
            model: model.to_string(),
            detail: json!({"scope": "main"}),
            ..ModelUsage::default()
        }],
        _ => vec![],
    };
    if rows.iter().all(|r| r.cost_reported.is_none())
        && let Some(r) = rows.first_mut()
    {
        r.cost_reported = total;
    }
    rows
}

/// Adds `more` to `into`, summing rows of the same model and scope (a model tried twice).
fn merge_usage(into: &mut Vec<ModelUsage>, more: Vec<ModelUsage>) {
    fn add<T: std::ops::Add<Output = T>>(a: Option<T>, b: Option<T>) -> Option<T> {
        match (a, b) {
            (Some(a), Some(b)) => Some(a + b),
            (a, b) => a.or(b),
        }
    }
    for m in more {
        match into
            .iter_mut()
            .find(|r| r.model == m.model && r.detail == m.detail)
        {
            Some(r) => {
                let (a, b) = (&mut r.tokens, m.tokens);
                a.input = add(a.input, b.input);
                a.output = add(a.output, b.output);
                a.cache_read = add(a.cache_read, b.cache_read);
                a.cache_write = add(a.cache_write, b.cache_write);
                a.reasoning = add(a.reasoning, b.reasoning);
                r.cost_reported = add(r.cost_reported, m.cost_reported);
            }
            None => into.push(m),
        }
    }
}

/// What a `stream-json` run has said so far: the text of the model's current message and, at the end, the
/// `result` object. A harness that prints only the `result` line (as `--output-format json`) works too.
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

impl ClaudeCli {
    /// The models (and effort) of this call: its purpose and workspace come from the caller's
    /// [`crate::stats::CallTag`].
    pub fn choice(&self, tier: Tier) -> crate::models::Choice {
        match &self.models {
            Some(m) => {
                let tag = crate::stats::current_tag();
                m.for_call(&tag.workspace_id, tag.purpose, tier)
            }
            None => crate::models::Choice::claude(match tier {
                Tier::Fast => self.fast.clone(),
                Tier::Smart => self.smart.clone(),
            }),
        }
    }

    /// Each model of the tier in turn until one answers; streaming when `on_text` is given. Counts the
    /// models tried and keeps the tokens of failed tries too (they were spent).
    async fn fall_back(
        &self,
        prompt: &str,
        tier: Tier,
        timeout: Duration,
        extra: &[String],
        on_text: Option<OnText<'_>>,
    ) -> Attempt {
        let t0 = Instant::now();
        let choice = self.choice(tier);
        let models = &choice.models;
        // apart from `extra`: an effort does not make the call one that wants Claude Code's tools
        let effort: Vec<String> = match &choice.effort {
            Some(e) => vec!["--effort".to_string(), e.clone()],
            None => vec![],
        };
        let codex = choice.harness == crate::models::CODEX;
        let mut a = Attempt {
            result: Err(msg("没有可用的模型")),
            attempts: 0,
            duration_ms: 0,
            usage: vec![],
            model: String::new(),
            failures: vec![],
            harness: if codex { CODEX_HARNESS } else { "" },
        };
        // Codex with no model named runs its configured default (`-m` left out)
        let default_only = [String::new()];
        let models: &[String] = if codex && models.is_empty() {
            &default_only
        } else {
            models
        };
        /* images go in on stdin, which only the stream-json layout takes */
        let quiet = |_: &str| {};
        let on_text = on_text.or_else(|| current_images().map(|_| &quiet as OnText<'_>));
        for m in models {
            let r = match on_text {
                _ if codex => {
                    self.once_codex(prompt, m, timeout, extra, choice.effort.as_deref(), on_text)
                        .await
                }
                Some(f) => {
                    self.once_streaming(prompt, m, timeout, extra, &effort, f)
                        .await
                }
                None => self.once(prompt, m, timeout, extra, &effort).await,
            };
            a.attempts += 1;
            a.model = m.clone();
            merge_usage(&mut a.usage, r.usage);
            match r.reply {
                Ok((text, cost_usd)) => {
                    a.duration_ms = elapsed_ms(t0);
                    a.result = Ok(Printed {
                        text,
                        model: m.clone(),
                        cost_usd,
                        usage: a.usage.clone(),
                        attempts: a.attempts,
                        duration_ms: a.duration_ms,
                    });
                    return a;
                }
                Err(e) => {
                    let e1 = js_head(&e.to_string(), 200).to_string();
                    tracing::error!("model {m} failed: {e1}");
                    a.failures.push(format!("{m}: {e1}"));
                    a.result = Err(e);
                }
            }
        }
        a.duration_ms = elapsed_ms(t0);
        a
    }
}

#[async_trait]
impl ModelGateway for ClaudeCli {
    async fn call(
        &self,
        prompt: &str,
        tier: Tier,
        timeout: Duration,
        extra: &[String],
    ) -> Result<Printed> {
        self.fall_back(prompt, tier, timeout, extra, None)
            .await
            .result
    }
    async fn call_streaming(
        &self,
        prompt: &str,
        tier: Tier,
        timeout: Duration,
        extra: &[String],
        on_text: OnText<'_>,
    ) -> Result<Printed> {
        self.fall_back(prompt, tier, timeout, extra, Some(on_text))
            .await
            .result
    }
    async fn attempt(
        &self,
        prompt: &str,
        tier: Tier,
        timeout: Duration,
        extra: &[String],
        on_text: Option<OnText<'_>>,
    ) -> Attempt {
        self.fall_back(prompt, tier, timeout, extra, on_text).await
    }
}

/// Steve's gateway as the modules' [`steve_modules::Gateway`] (B1), so core and modules share one model
/// gateway: same tier lists, same fallback, same scratch directory.
pub struct SharedGateway(pub std::sync::Arc<dyn ModelGateway>);

#[async_trait]
impl steve_modules::Gateway for SharedGateway {
    async fn run_with_images(
        &self,
        tier: steve_modules::Tier,
        prompt: &str,
        images: &[Value],
        timeout: Duration,
    ) -> std::result::Result<steve_modules::GatewayOut, String> {
        with_images(images.to_vec(), self.run(tier, prompt, timeout)).await
    }

    async fn run(
        &self,
        tier: steve_modules::Tier,
        prompt: &str,
        timeout: Duration,
    ) -> std::result::Result<steve_modules::GatewayOut, String> {
        let tier = match tier {
            steve_modules::Tier::Fast => Tier::Fast,
            steve_modules::Tier::Smart => Tier::Smart,
        };
        self.0
            .call(prompt, tier, timeout, &[])
            .await
            .map(|p| steve_modules::GatewayOut {
                text: p.text,
                cost_usd: p.cost_usd,
                model: p.model,
                usage: p.usage,
                attempts: p.attempts,
                duration_ms: p.duration_ms,
            })
            .map_err(|e| e.to_string())
    }
}

/// JS `String(v)` for a JSON value.
pub fn jstr(v: &Value) -> String {
    match v {
        Value::String(s) => s.clone(),
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::Array(a) => a
            .iter()
            .map(|x| if x.is_null() { String::new() } else { jstr(x) })
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}

/// JS truthiness of a JSON value.
pub fn truthy(v: &Value) -> bool {
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0 && !f.is_nan()),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

static BRACES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"\{[\s\S]*\}").expect("valid regex"));

/// The greedy first `{…}` of a model reply, parsed; `None` without braces, an error for bad JSON.
pub fn first_json(t: &str) -> Result<Option<Value>> {
    match BRACES.find(t) {
        None => Ok(None),
        Some(m) => Ok(Some(serde_json::from_str(m.as_str())?)),
    }
}

/// The string value of `"key": "…` in a reply still being written, as far as it has got (escapes decoded,
/// an escape cut off at the end left out); `None` until the value has begun.
pub fn partial_str(t: &str, key: &str) -> Option<String> {
    let k = format!("\"{key}\"");
    let b = t.as_bytes();
    let mut from = 0;
    let start = loop {
        let at = from + t[from..].find(&k)?;
        let rest = t[at + k.len()..].trim_start();
        if let Some(v) = rest.strip_prefix(':').map(str::trim_start)
            && let Some(v) = v.strip_prefix('"')
        {
            break t.len() - v.len();
        }
        from = at + k.len();
    };
    let mut i = start;
    while i < b.len() {
        match b[i] {
            b'"' => break,
            b'\\' if b.get(i + 1) == Some(&b'u') => {
                if i + 6 > b.len() {
                    break;
                }
                i += 6
            }
            b'\\' if i + 1 >= b.len() => break,
            b'\\' => i += 2,
            _ => i += 1,
        }
    }
    let body = &t[start..i];
    let decode = |s: &str| serde_json::from_str::<String>(&format!("\"{s}\"")).ok();
    /* a high surrogate whose pair is still to come */
    decode(body).or_else(|| decode(&body[..body.rfind("\\u")?]))
}

pub const TAG_CATS: [&str; 5] = ["项目", "类型", "领域", "客户", "主题"];

/// The routing results (thread-routing §3.1). `chat` from older routers reads as `answer`.
pub const KINDS: [&str; 9] = [
    "answer",
    "research",
    "item",
    "suggest",
    "ask",
    "supplement",
    "status",
    "config",
    "reroute",
];

/// Why the router chose what it chose (§3.2); the guards in `thread::decide` read them.
pub const SIGNALS: [&str; 7] = [
    "explicit",
    "change",
    "deliverable",
    "long",
    "question",
    "lookup",
    "vague",
];

#[derive(Debug, Clone, PartialEq)]
pub struct NewTag {
    pub name: String,
    pub cat: String,
    pub description: String,
}

/// Work an answer or research run leads to; the user decides whether it becomes an item.
#[derive(Debug, Clone, PartialEq, Default, serde::Serialize, serde::Deserialize)]
pub struct Offer {
    pub title: String,
    pub goal: String,
}

/// The router's decision for a message.
#[derive(Debug, Clone, PartialEq)]
pub struct Route {
    /// one of [`KINDS`]
    pub kind: String,
    /// 0–1, 0.5 when the model gave none
    pub confidence: f64,
    /// a subset of [`SIGNALS`]
    pub signals: Vec<String>,
    /// work to offer after an answer or research
    pub offer: Option<Offer>,
    /// the item a supplement, status or reroute is about (checked to be of this workspace)
    pub item: Option<String>,
    /// choices of an `ask`
    pub options: Vec<String>,
    pub title: String,
    /// The task for the bot, self-contained (a follow-up like “按建议” restated from Steve's reply).
    pub goal: String,
    pub bot: String,
    /// The bot is the project's default bot in place of the router's “steve”: the router's `reply`
    /// may name the wrong bot then, so the code writes the hand-over sentence itself.
    pub bot_from_project: bool,
    pub project: Option<String>,
    pub tag_ids: Vec<String>,
    pub new_tags: Vec<NewTag>,
    /// one sentence for the user, without the tags (the code adds them once)
    pub reply: String,
    /// the router's one-sentence reason (≤200)
    pub why: String,
    /// which model routed
    pub model: Option<String>,
    /// the router failed and this is the default
    pub fallback: bool,
}

impl Default for Route {
    fn default() -> Route {
        Route {
            kind: "answer".into(),
            confidence: 0.5,
            signals: vec![],
            offer: None,
            item: None,
            options: vec![],
            title: String::new(),
            goal: String::new(),
            bot: "steve".into(),
            bot_from_project: false,
            project: None,
            tag_ids: vec![],
            new_tags: vec![],
            reply: String::new(),
            why: String::new(),
            model: None,
            fallback: false,
        }
    }
}

impl Route {
    /// What the router falls back to when the model fails (§3.3 rule 6): never an item. Research when
    /// there is a project to read (`project`), else a direct answer.
    pub fn fallback(text: &str, project: Option<String>) -> Route {
        Route {
            kind: if project.is_some() {
                "research".into()
            } else {
                "answer".into()
            },
            title: js_head(text, 24).into(),
            goal: text.into(),
            project,
            why: "没能判断，先在这里回答".into(),
            fallback: true,
            ..Route::default()
        }
    }
}

/// A thread message as context for the router and chat answers.
#[derive(Debug, Clone, PartialEq)]
pub struct Said {
    pub author: String,
    pub text: String,
}

/// An open item of the conversation the router may send a message to (§5.3).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct FocusItem {
    pub id: String,
    pub title: String,
    pub status: String,
    /// the start of the user message it came from
    pub origin: String,
}

/// What the router may pick from (collected under the core lock).
#[derive(Debug, Clone, Default)]
pub struct RouteCtx {
    /// enabled bots of the workspace
    pub bots: Vec<Bot>,
    pub projects: Vec<Project>,
    pub tags: Vec<Tag>,
    /// the project whose path is `STEVE_REPO`, if any
    pub steve_project: Option<String>,
    /// the conversation's open items, most recent first (≤3)
    pub focus: Vec<FocusItem>,
    /// ids of the workspace's items: an `item` the router names must be one of them
    pub items: Vec<String>,
    /// an inline research run is reading right now
    pub researching: bool,
    pub debug: bool,
}

fn js_json(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_default()
}

/// The fixed part of the router prompt: principles, kinds and examples (thread-routing §3.2).
pub const ROUTE_RULES: &str = include_str!("thread/route_prompt.txt");

/// The router prompt: [`ROUTE_RULES`] plus what exists in the workspace and the recent conversation.
///
/// Laid out for the prompt cache: the rules, the workspace (bots, projects, tags) and the reply format
/// come first and are the same for every message of the workspace; after [`STATIC_END`] come the
/// conversation's open items, the recent conversation and the message.
pub fn route_prompt(c: &RouteCtx, text: &str, history: &[Said], promote: bool) -> String {
    let skip = history.len().saturating_sub(6);
    let recent: Vec<String> = history[skip..]
        .iter()
        .map(|m| {
            let t = if js_len(&m.text) > 800 {
                format!("{}…", js_head(&m.text, 800))
            } else {
                m.text.clone()
            };
            format!(
                "  {}: {}",
                if m.author == "me" { "User" } else { "Steve" },
                js_json(&t)
            )
        })
        .collect();
    let mut head: Vec<String> = vec![
        ROUTE_RULES.trim_end().to_string(),
        "Bots (id: role):".into(),
    ];
    head.extend(c.bots.iter().map(|b| format!("  {}: {}", b.id, b.role)));
    head.push("For an item, pick \"steve\" unless the task clearly needs a specialised bot; work in a project with a default bot goes to that bot. For other kinds the bot does not matter.".into());
    head.push("Projects (id: name — description · default bot). For an item: the project whose directory the work happens in; for research: the project to read; null if none fits:".into());
    head.extend(c.projects.iter().map(|p| {
        format!(
            "  {}: {} — {}{}",
            p.id,
            p.name,
            p.description,
            p.default_bot
                .as_deref()
                .map(|b| format!(" · {b}"))
                .unwrap_or_default()
        )
    }));
    if let Some(s) = &c.steve_project {
        head.push(format!("Anything about Steve itself — its chat page, web UI, cards, Hub, bots, knowledge system, releases, design — is in project {s}."));
    }
    head.push(format!(
        "Tags (id: name · category · description), for items only. Choose existing tags that fit; only if none of the non-project tags fits, propose ONE new tag with category in {} and a concrete description:",
        TAG_CATS.join("/")
    ));
    head.extend(
        c.tags
            .iter()
            .map(|t| format!("  {}: {} · {} · {}", t.id, t.name, t.cat, t.description)),
    );
    head.push("goal: the task, self-contained, in the user language (for an ordinary message, the message itself). title: names the work concretely, never just “按建议” or “好的”.".into());
    head.push("Reply with ONLY one JSON object: {\"kind\":\"answer\"|\"research\"|\"item\"|\"suggest\"|\"ask\"|\"supplement\"|\"status\"|\"config\"|\"reroute\",\"confidence\":0.0-1.0,\"signals\":[\"explicit\"|\"change\"|\"deliverable\"|\"long\"|\"question\"|\"lookup\"|\"vague\"],\"offer\":{\"title\":\"\",\"goal\":\"\"} or null,\"item\":\"<item id or null>\",\"options\":[\"\"],\"title\":\"<= 24 chars, user language\",\"goal\":\"\",\"bot\":\"<bot id>\",\"project\":\"<project id or null>\",\"tags\":[\"<tag id>\"],\"newTags\":[{\"name\":\"\",\"cat\":\"\",\"description\":\"\"}],\"reply\":\"one short sentence in the user language, without tags\",\"why\":\"one short sentence in the user language: why this kind\"}".into());
    let mut lines: Vec<String> = vec![];
    if !c.focus.is_empty() {
        lines.push(
            "Open items of this conversation (id: title · status · from the user's message):"
                .into(),
        );
        lines.extend(c.focus.iter().map(|f| {
            format!(
                "  {}: {} · {} · {}",
                f.id,
                f.title,
                f.status,
                js_json(js_head(&f.origin, 80))
            )
        }));
    }
    if c.researching {
        lines.push("Steve is reading for an earlier question right now. What the user says meanwhile does not become an item unless they explicitly ask for one.".into());
    }
    if !recent.is_empty() {
        lines.push("Recent conversation, oldest first:".into());
        lines.extend(recent);
    }
    lines.push(format!("User message: {}", js_json(text)));
    lines.push(if promote {
        "The user pressed “做成事项” on Steve's last answer: kind is \"item\"; goal is the work this conversation is about, drawn from the question and Steve's answer.".into()
    } else {
        "If the message only confirms or picks up something Steve proposed in its last reply (e.g. “按建议”, “就这么办”, “好，做吧”, “改吧”, “可以”, “第二个”), it is about that proposal: when the proposal is work, kind is \"item\" with signal \"explicit\" and goal restates the proposed work completely from Steve's reply.".into()
    });
    lines.push("Reply with ONLY the JSON object described in the instructions.".into());
    head.retain(|l| !l.is_empty());
    lines.retain(|l| !l.is_empty());
    with_static(&head.join("\n"), &lines.join("\n"))
}

static TAG_NOTE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\s*标签[：:][^。]*(。|$)").expect("valid regex"));

/// The router's reply without the tag note the code appends itself (“标签：前端。” once, not twice).
pub fn strip_tag_note(reply: &str) -> String {
    TAG_NOTE.replace_all(reply, "").trim().to_string()
}

/// An `{title, goal}` object as an [`Offer`]; `None` without a title.
pub fn offer_of(o: &Value) -> Option<Offer> {
    if !o.is_object() {
        return None;
    }
    let title = o.get("title").map(jstr).unwrap_or_default();
    let title = title.trim();
    (!title.is_empty()).then(|| Offer {
        title: js_head(title, 40).to_string(),
        goal: match o.get("goal").map(jstr) {
            Some(g) if !g.trim().is_empty() => g.trim().to_string(),
            _ => title.to_string(),
        },
    })
}

/// The answer without its closing `OFFER: {…}` line (research prompt §4.1), and the offer it names.
pub fn split_offer(text: &str) -> (String, Option<Offer>) {
    let t = text.trim_end();
    let Some(at) = t.rfind("OFFER:") else {
        return (t.to_string(), None);
    };
    let line_start = t[..at].rfind('\n').map(|i| i + 1).unwrap_or(0);
    if !t[line_start..at].trim().is_empty() {
        return (t.to_string(), None);
    }
    let offer = serde_json::from_str::<Value>(t[at + "OFFER:".len()..].trim())
        .ok()
        .as_ref()
        .and_then(offer_of);
    match offer {
        Some(o) => (t[..line_start].trim_end().to_string(), Some(o)),
        None => (t.to_string(), None),
    }
}

/// Validate a router reply against what exists; `None` when it is unusable (the caller falls back).
pub fn parse_route(c: &RouteCtx, text: &str, raw: &str, promote: bool) -> Option<Route> {
    let r = first_json(raw).ok()??;
    let kind = match r.get("kind").and_then(Value::as_str)? {
        "chat" => "answer",
        k if KINDS.contains(&k) => k,
        _ => return None,
    };
    let kind = if promote { "item" } else { kind };
    let str_of = |k: &str| match r.get(k) {
        None | Some(Value::Null) => None,
        Some(v) => Some(jstr(v)).filter(|s| !s.trim().is_empty()),
    };
    /* the item a supplement, status or reroute is about must be one of this workspace's */
    let item = str_of("item").map(|s| s.trim().trim_start_matches('#').to_uppercase());
    let item = match item {
        Some(i) if c.items.contains(&i) => Some(i),
        Some(_) if matches!(kind, "supplement" | "status") => return None,
        None if matches!(kind, "supplement" | "status") => return None,
        _ => None,
    };
    let has_bot = |id: &str| c.bots.iter().any(|b| b.id == id);
    let project = r
        .get("project")
        .and_then(Value::as_str)
        .filter(|p| c.projects.iter().any(|x| x.id == *p))
        .map(String::from);
    /* work in a project goes to the project's own bot unless a specialised one was picked; only items have a bot */
    let own = project
        .as_deref()
        .and_then(|p| c.projects.iter().find(|x| x.id == p))
        .and_then(|p| p.default_bot.clone())
        .filter(|b| has_bot(b));
    let picked = r.get("bot").and_then(Value::as_str).filter(|b| has_bot(b));
    let (bot, bot_from_project) = match (kind, picked, &own) {
        ("item", Some("steve"), Some(o)) => (o.clone(), o != "steve"),
        ("item", None, Some(o)) => (o.clone(), true),
        (_, Some(b), _) => (b.to_string(), false),
        _ => ("steve".to_string(), false),
    };
    let s_or = |k: &str| match r.get(k) {
        Some(v) if truthy(v) => jstr(v),
        _ => text.to_string(),
    };
    let tag_ids = match r.get("tags") {
        None | Some(Value::Null) => vec![],
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(Value::as_str)
            .filter(|x| c.tags.iter().any(|t| t.id == *x))
            .map(String::from)
            .collect(),
        Some(_) => return None,
    };
    let new_tags = match r.get("newTags") {
        None | Some(Value::Null) => vec![],
        Some(Value::Array(a)) => a
            .iter()
            .filter(|t| {
                t.get("name").is_some_and(truthy)
                    && t.get("cat")
                        .and_then(Value::as_str)
                        .is_some_and(|x| TAG_CATS.contains(&x))
                    && t.get("description").is_some_and(truthy)
            })
            .take(1)
            .map(|t| NewTag {
                name: jstr(&t["name"]),
                cat: jstr(&t["cat"]),
                description: jstr(&t["description"]),
            })
            .collect(),
        Some(_) => return None,
    };
    let confidence = r
        .get("confidence")
        .and_then(Value::as_f64)
        .filter(|x| x.is_finite())
        .map(|x| x.clamp(0.0, 1.0))
        .unwrap_or(0.5);
    let signals = match r.get("signals") {
        Some(Value::Array(a)) => {
            let mut v: Vec<String> = vec![];
            for s in a.iter().filter_map(Value::as_str) {
                if SIGNALS.contains(&s) && !v.iter().any(|x| x == s) {
                    v.push(s.into());
                }
            }
            v
        }
        _ => vec![],
    };
    let offer = r.get("offer").and_then(offer_of);
    let options = match r.get("options") {
        Some(Value::Array(a)) => a
            .iter()
            .filter_map(Value::as_str)
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .take(3)
            .map(String::from)
            .collect(),
        _ => vec![],
    };
    Some(Route {
        kind: kind.into(),
        confidence,
        signals,
        offer,
        item,
        options,
        title: js_head(&s_or("title"), 40).into(),
        goal: s_or("goal"),
        bot,
        bot_from_project,
        project,
        tag_ids,
        new_tags,
        reply: match r.get("reply") {
            None | Some(Value::Null) => String::new(),
            Some(v) => strip_tag_note(&jstr(v)),
        },
        why: match r.get("why") {
            None | Some(Value::Null) => String::new(),
            Some(v) => js_head(&jstr(v), 200).to_string(),
        },
        model: None,
        fallback: false,
    })
}

/// Route a message; any failure gives [`Route::fallback`], which never creates an item.
pub async fn route(
    gw: &dyn ModelGateway,
    c: &RouteCtx,
    text: &str,
    history: &[Said],
    promote: bool,
) -> Route {
    let prompt = route_prompt(c, text, history, promote);
    match gw
        .call(&prompt, Tier::Fast, Duration::from_secs(60), &[])
        .await
    {
        Ok(Printed {
            text: raw, model, ..
        }) => {
            if c.debug {
                tracing::info!("route raw: {raw}");
            }
            if let Some(r) = parse_route(c, text, &raw, promote) {
                return Route {
                    model: Some(model),
                    ..r
                };
            }
            tracing::warn!("route unusable: {}", js_head(&raw, 200));
        }
        Err(e) => tracing::error!("route failed {e}"),
    }
    let project = c.steve_project.clone().or_else(|| {
        c.projects
            .iter()
            .find(|p| !p.archived)
            .map(|p| p.id.clone())
    });
    let mut r = Route::fallback(text, project);
    if promote {
        /* “做成事项” was pressed: the user asked for an item, the router only names it */
        r.kind = "item".into();
        r.project = None;
    }
    r
}

/// Attachments of a chat message, as copied next to the print-mode run (its cwd is the scratch dir).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ChatFiles {
    /// how many the user attached
    pub count: usize,
    /// the path note, or `\n（附件没能读取：…）` when copying failed
    pub note: String,
    /// copied: let the model read them with the Read tool only (files that are not inline images)
    pub readable: bool,
    /// images that go into the message itself (`{"type":"image","source":{…}}`)
    pub images: Vec<Value>,
}

/// The chat-answer prompt (gateway.ts, with the “做成事项” hint replaced by an offer, thread-routing §6.1). `extra` is module context (B1 `PromptContext::chat`),
/// inserted after the main bot's part and before the history; `""` leaves the prompt as in TS.
pub fn answer_prompt(
    main: &Bot,
    text: &str,
    history: &[Said],
    files: &ChatFiles,
    extra: &str,
) -> String {
    let skip = history.len().saturating_sub(10);
    let ctx = history[skip..]
        .iter()
        .map(|m| {
            format!(
                "{}: {}",
                if m.author == "me" { "User" } else { "Steve" },
                m.text
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let read = match (files.count > 0, files.readable, files.images.is_empty()) {
        (false, ..) => "",
        (true, true, true) => "Read the attached files with the Read tool before answering.\n\n",
        (true, true, false) => {
            "The attached images are in this message; read the other attached files with the Read tool before answering.\n\n"
        }
        (true, false, _) => {
            "The attached images are in this message: look at them before answering.\n\n"
        }
    };
    let extra = if extra.is_empty() {
        String::new()
    } else {
        format!("{extra}\n\n")
    };
    // the bot and the reply format are the cached head; module context and the conversation change
    with_static(
        &format!(
            "You are {}, the user's assistant. {} {}\nAnswer in the user's language, briefly. You answer in the conversation: you don't change anything, change settings or start work from this reply, so never claim you did. Steve can change its own settings when the user says the change itself in the conversation (e.g. “把免打扰改成 22:00 – 09:00”, “停用写作 Bot”, “开 YOLO”): it drafts the change as a card the user applies with one click and can undo. Asked whether or how settings can be changed, say so and give an example sentence; never claim it is impossible. When the user asks for a change or reports a problem, give a concrete plan right away; never ask whether to plan here or make it an item. If the answer leads to a change worth making, put it in \"offer\" (the user can make it a tracked item with one click); otherwise leave offer null.\nReply with ONLY one JSON object: {{\"thought\":\"2-4 short sentences in the user's language: what the user is asking and how you approach the answer\",\"answer\":\"the answer itself (markdown allowed)\",\"offer\":{{\"title\":\"<= 24 chars\",\"goal\":\"the work, self-contained\"}} or null}}",
            main.name, main.persona, main.rules
        ),
        &format!(
            "{read}{extra}Recent conversation:\n{ctx}\n\nUser: {text}{}",
            files.note
        ),
    )
}

/// A direct answer, with a short account of how Steve got there (shown folded above the answer).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Answer {
    pub text: String,
    pub thought: String,
    /// work the answer leads to (§3.4)
    pub offer: Option<Offer>,
    pub model: Option<String>,
    pub failed: bool,
}

/// Told the reasoning and the answer so far while a chat answer is written.
pub type OnDraft<'a> = &'a (dyn Fn(&str, &str) + Send + Sync);

/// The reasoning and the answer so far in a chat answer still being written (see [`answer_prompt`]). A reply
/// that is not the JSON object asked for is the answer itself, as when it is done.
pub fn draft_of(raw: &str) -> (String, String) {
    let thought = partial_str(raw, "thought").unwrap_or_default();
    let answer = match partial_str(raw, "answer") {
        Some(a) => a,
        None if raw.trim_start().starts_with(['{', '`']) => String::new(),
        None => raw.to_string(),
    };
    (thought.trim().to_string(), answer.trim_start().to_string())
}

/// Answer a chat message; a failure becomes the reply text. `on_draft` hears the answer as it is written.
pub async fn answer(
    gw: &dyn ModelGateway,
    main: &Bot,
    text: &str,
    history: &[Said],
    files: &ChatFiles,
    context: &str,
    on_draft: OnDraft<'_>,
) -> Answer {
    let extra = answer_tools(files);
    let timeout = Duration::from_secs(if files.count > 0 { 150 } else { 90 });
    let on_text = |raw: &str| {
        let (t, a) = draft_of(raw);
        on_draft(&t, &a)
    };
    match with_images(
        files.images.clone(),
        gw.call_streaming(
            &answer_prompt(main, text, history, files, context),
            Tier::Smart,
            timeout,
            &extra,
            &on_text,
        ),
    )
    .await
    {
        Ok(Printed {
            text: raw, model, ..
        }) => {
            let r = first_json(&raw).ok().flatten();
            match r
                .as_ref()
                .and_then(|r| r.get("answer"))
                .and_then(Value::as_str)
                .filter(|a| !a.trim().is_empty())
            {
                Some(a) => {
                    let thought = match r.as_ref().and_then(|r| r.get("thought")) {
                        None | Some(Value::Null) => String::new(),
                        Some(v) => js_head(jstr(v).trim(), 1200).to_string(),
                    };
                    let (text, line) = split_offer(a.trim());
                    let offer = r
                        .as_ref()
                        .and_then(|r| r.get("offer"))
                        .and_then(offer_of)
                        .or(line);
                    Answer {
                        text,
                        thought,
                        offer,
                        model: Some(model),
                        failed: false,
                    }
                }
                None => {
                    let (text, offer) = split_offer(raw.trim());
                    Answer {
                        text,
                        thought: String::new(),
                        offer,
                        model: Some(model),
                        failed: false,
                    }
                }
            }
        }
        Err(e) => Answer {
            text: format!("（回答失败：{}）", js_head(&e.to_string(), 80)),
            thought: String::new(),
            offer: None,
            model: None,
            failed: true,
        },
    }
}

/// A field the config draft may change.
#[derive(Debug, Clone)]
pub struct FieldSpec {
    pub key: String,
    pub label: String,
    /// text | enum | number
    pub ty: &'static str,
    pub values: Option<Vec<String>>,
    pub current: Value,
}

/// Map a request onto fields; returns `(key, value)` for known keys only.
pub async fn draft_fields(
    gw: &dyn ModelGateway,
    target: &str,
    fields: &[FieldSpec],
    request: &str,
) -> Result<Vec<(String, Value)>> {
    let mut lines = vec![
        format!(
            "You configure \"{target}\" in Steve, a personal assistant app. Map the user's request onto these fields. Change only what the request asks for."
        ),
        "Fields (key · label · type · allowed values · current value):".to_string(),
    ];
    for f in fields {
        let vals = f
            .values
            .as_ref()
            .map(|v| format!(" · {}", v.join("|")))
            .unwrap_or_default();
        lines.push(format!(
            "  {} · {} · {}{} · {}",
            f.key, f.label, f.ty, vals, f.current
        ));
    }
    lines.push("For free-text fields, write the full new value in the user's language (append to the current text if the request adds a rule).".into());
    lines.push("Reply with ONLY JSON: {\"changes\":[{\"key\":\"\",\"value\":...}]}".into());
    let prompt = with_static(&lines.join("\n"), &format!("Request: {}", js_json(request)));
    let raw = gw
        .print(&prompt, Tier::Smart, Duration::from_secs(90))
        .await?;
    let r = first_json(&raw)?;
    let Some(Value::Array(cs)) = r.as_ref().and_then(|r| r.get("changes")) else {
        return Ok(vec![]);
    };
    Ok(cs
        .iter()
        .filter_map(|c| {
            let k = c.get("key")?.as_str()?;
            fields.iter().any(|f| f.key == k).then(|| {
                (
                    k.to_string(),
                    c.get("value").cloned().unwrap_or(Value::Null),
                )
            })
        })
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn bot(id: &str) -> Bot {
        Bot {
            id: id.into(),
            workspace_id: "default".into(),
            name: id.into(),
            role: "r".into(),
            main: id == "steve",
            harness: "claude-code".into(),
            model: None,
            reasoning_effort: None,
            rules: String::new(),
            accept_mode: "inherit".into(),
            perm_mode: "default".into(),
            enabled: true,
            revision: 1,
            updated: 0,
            persona: String::new(),
            proactive: 1,
            avatar: Default::default(),
            default_project: None,
            prompt: None,
            device: None,
        }
    }
    fn project(id: &str, default_bot: Option<&str>) -> Project {
        Project {
            id: id.into(),
            workspace_id: "default".into(),
            name: id.into(),
            description: String::new(),
            device_id: "hub".into(),
            path: "/x".into(),
            kind: "dir".into(),
            worktree: false,
            default_bot: default_bot.map(String::from),
            release: None,
            archived: false,
            created: 0,
            updated: 0,
        }
    }
    fn ctx() -> RouteCtx {
        RouteCtx {
            bots: vec![bot("steve"), bot("dev"), bot("writer")],
            projects: vec![project("prj_a", Some("dev")), project("prj_b", None)],
            tags: vec![Tag {
                id: "tag_1".into(),
                workspace_id: "default".into(),
                name: "t".into(),
                cat: "类型".into(),
                description: "d".into(),
                created_by: "你".into(),
                created: 0,
            }],
            steve_project: Some("prj_a".into()),
            ..Default::default()
        }
    }

    #[test]
    fn first_json_is_greedy() {
        assert_eq!(
            first_json("x {\"a\":{\"b\":1}} y").unwrap(),
            Some(json!({"a":{"b":1}}))
        );
        assert_eq!(first_json("none").unwrap(), None);
        assert!(first_json("{bad}").is_err());
    }

    #[test]
    fn route_validates_against_what_exists() {
        let c = ctx();
        let r = parse_route(&c, "做个东西", r#"{"kind":"item","title":"","bot":"ghost","project":"prj_x","tags":["tag_1","tag_9"],"newTags":[{"name":"n","cat":"类型","description":"d"},{"name":"m","cat":"类型","description":"d"}],"reply":"好"}"#, false).unwrap();
        assert_eq!(r.bot, "steve");
        assert_eq!(r.project, None);
        assert_eq!(r.title, "做个东西");
        assert_eq!(r.goal, "做个东西");
        assert_eq!(r.tag_ids, vec!["tag_1"]);
        assert_eq!(r.new_tags.len(), 1);
        assert!(parse_route(&c, "x", r#"{"kind":"nope"}"#, false).is_none());
        assert!(parse_route(&c, "x", "no json", false).is_none());
    }

    #[test]
    fn project_work_goes_to_the_project_bot() {
        let c = ctx();
        let r = parse_route(
            &c,
            "x",
            r#"{"kind":"item","bot":"steve","project":"prj_a"}"#,
            false,
        )
        .unwrap();
        assert_eq!(r.bot, "dev");
        let r = parse_route(
            &c,
            "x",
            r#"{"kind":"item","bot":"writer","project":"prj_a"}"#,
            false,
        )
        .unwrap();
        assert_eq!(r.bot, "writer");
        let r = parse_route(&c, "x", r#"{"kind":"item","project":"prj_b"}"#, false).unwrap();
        assert_eq!(r.bot, "steve");
        let r = parse_route(&c, "x", r#"{"kind":"chat","project":"prj_a"}"#, true).unwrap();
        assert_eq!(r.kind, "item");
        assert_eq!((r.bot.as_str(), r.bot_from_project), ("dev", true));
    }

    #[test]
    fn route_prompt_has_context() {
        let c = ctx();
        let h = vec![
            Said {
                author: "me".into(),
                text: "问".into(),
            },
            Said {
                author: "steve".into(),
                text: "答".into(),
            },
        ];
        let p = route_prompt(&c, "按建议", &h, false);
        assert!(p.contains("Recent conversation, oldest first:\n  User: \"问\"\n  Steve: \"答\""));
        assert!(p.contains("is in project prj_a."));
        assert!(p.contains("User message: \"按建议\""));
        // the workspace part is the cached head, the conversation comes after it
        let (head, tail) = split_static(&p);
        let head = head.unwrap();
        assert!(head.starts_with(ROUTE_RULES.trim_end()));
        assert!(
            head.contains("is in project prj_a.")
                && head.contains("Reply with ONLY one JSON object")
        );
        assert!(!head.contains("User message:") && tail.contains("User message: \"按建议\""));
        assert!(!head.contains("\n\n") && !tail.contains("\n\n"));
        // the same workspace gives the same head whatever the message
        assert_eq!(
            split_static(&route_prompt(&c, "别的", &[], true)).0,
            Some(head)
        );
    }

    #[test]
    fn an_answer_reading_attachments_has_read_and_nothing_else() {
        let none = ChatFiles::default();
        assert!(answer_tools(&none).is_empty(), "no attachments: no tools");
        let files = ChatFiles {
            readable: true,
            count: 1,
            ..ChatFiles::default()
        };
        let a = answer_tools(&files);
        let after = |f: &str| a.iter().position(|x| x == f).map(|i| a[i + 1].as_str());
        assert_eq!(after("--tools"), Some("Read"), "the only tool there is");
        assert_eq!(after("--setting-sources"), Some("user"));
    }

    #[test]
    fn print_args_put_the_stable_head_in_the_system_prompt() {
        let p = with_static("rules", "message");
        assert_eq!(
            print_args(&p, &[]),
            (
                "message",
                vec![
                    "--system-prompt",
                    "rules",
                    "--tools",
                    "",
                    "--strict-mcp-config",
                    "--disable-slash-commands"
                ]
            )
        );
        // no head: a short fixed system prompt, still no tools
        let (m, a) = print_args("just this", &[]);
        assert_eq!(
            (m, &a[..2]),
            ("just this", &["--system-prompt", GATEWAY_SYSTEM][..])
        );
        // tools wanted: Claude Code's own prompt stays, the head is appended
        let extra = vec!["--allowedTools".to_string(), "Read".to_string()];
        assert_eq!(
            print_args(&p, &extra),
            (
                "message",
                vec![
                    "--append-system-prompt",
                    "rules",
                    "--strict-mcp-config",
                    "--disable-slash-commands"
                ]
            )
        );
        assert_eq!(
            print_args("m", &extra),
            ("m", vec!["--strict-mcp-config", "--disable-slash-commands"])
        );
    }

    #[test]
    fn a_reply_being_written_is_read_as_far_as_it_goes() {
        let full = "```json\n{\"thought\":\"用户问\\\"你好\\\"\",\"answer\":\"你好\\n**呀** \\ud83d\\ude00\"}\n```";
        let mut seen = vec![];
        for n in (0..=full.len()).filter(|n| full.is_char_boundary(*n)) {
            seen.push(draft_of(&full[..n]));
        }
        assert_eq!(seen[0], (String::new(), String::new()));
        assert!(seen.contains(&("用户".into(), String::new())));
        assert!(
            seen.contains(&("用户问\"".into(), String::new())),
            "an escape is decoded once whole"
        );
        assert!(
            seen.contains(&("用户问\"你好\"".into(), "你好\n**呀** ".into())),
            "a cut-off \\u escape or surrogate pair is left out"
        );
        assert_eq!(
            seen.last().unwrap(),
            &("用户问\"你好\"".into(), "你好\n**呀** 😀".into())
        );
        /* it only ever grows */
        for w in seen.windows(2) {
            assert!(w[1].1.starts_with(&w[0].1), "{:?} → {:?}", w[0], w[1]);
        }
        assert_eq!(
            partial_str(
                r#"{"thought":"说 \"answer\": \"x\"","answer": "y"#,
                "answer"
            )
            .as_deref(),
            Some("y")
        );
        assert_eq!(draft_of("直接回答"), (String::new(), "直接回答".into()));
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
        assert_eq!(s.text, "");
        assert!(s.line(&d("{\"answer\":")) && s.line(&d("\"好\"}")));
        assert_eq!(s.text, "{\"answer\":\"好\"}");
        assert!(!s.line("not json"));
        assert!(!s.line(r#"{"type":"result","is_error":false,"result":"{\"answer\":\"好\"}","total_cost_usd":0.01}"#));
        assert_eq!(
            result_of(s.result.as_ref().unwrap()).unwrap(),
            ("{\"answer\":\"好\"}".into(), Some(0.01))
        );
    }

    /// A stand-in `claude` that writes its answer in pieces, a moment apart.
    #[cfg(unix)]
    fn slow_claude(dir: &std::path::Path, body: &str) -> ClaudeCli {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join("claude");
        std::fs::write(&p, format!("#!/bin/sh\n{body}\n")).unwrap();
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755)).unwrap();
        ClaudeCli {
            codex: None,
            executable: p,
            fast: vec!["m1".into()],
            smart: vec!["m1".into(), "m2".into()],
            cwd: dir.to_path_buf(),
            models: None,
        }
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn the_cli_streams_text_before_it_exits() {
        let dir = tempfile::tempdir().unwrap();
        let ev = |t: &str| {
            format!(
                "echo '{{\"type\":\"stream_event\",\"event\":{{\"type\":\"content_block_delta\",\"delta\":{{\"type\":\"text_delta\",\"text\":{}}}}}}}'",
                serde_json::to_string(t).unwrap()
            )
        };
        let body = [
            r#"case "$*" in *stream-json*--include-partial-messages*) ;; *) echo "not streaming: $*" >&2; exit 1;; esac"#.to_string(),
            r#"case "$*" in *m1*) echo boom >&2; exit 3;; esac"#.to_string(),
            ev("{\"thought\":\"想\",\"answer\":\"一"),
            "sleep 0.3".into(),
            ev("二\"}"),
            "sleep 0.3".into(),
            r#"echo '{"type":"result","is_error":false,"result":"{\"thought\":\"想\",\"answer\":\"一二\"}"}'"#.into(),
        ]
        .join("\n");
        let gw = slow_claude(dir.path(), &body);
        let t0 = std::time::Instant::now();
        let seen = std::sync::Mutex::new(vec![]);
        let a = answer(
            &gw,
            &bot("steve"),
            "数数",
            &[],
            &ChatFiles::default(),
            "",
            &|t, x| {
                seen.lock()
                    .unwrap()
                    .push((t.to_string(), x.to_string(), t0.elapsed()))
            },
        )
        .await;
        assert_eq!(
            (a.text.as_str(), a.thought.as_str(), a.model.as_deref()),
            ("一二", "想", Some("m2")),
            "{a:?}"
        );
        let seen = seen.into_inner().unwrap();
        assert_eq!(
            seen.iter().map(|s| s.1.as_str()).collect::<Vec<_>>(),
            ["一", "一二"]
        );
        assert!(
            t0.elapsed() - seen[0].2 >= Duration::from_millis(500),
            "the first part came before the run ended"
        );
        /* a run that fails says why */
        let gw = slow_claude(dir.path(), "echo nope >&2; exit 2");
        let e = gw
            .call_streaming("x", Tier::Fast, Duration::from_secs(5), &[], &|_| {})
            .await
            .unwrap_err();
        assert_eq!(e.to_string(), "Command failed: nope");
        let gw = slow_claude(dir.path(), "sleep 5");
        let e = gw
            .call_streaming("x", Tier::Fast, Duration::from_millis(200), &[], &|_| {})
            .await
            .unwrap_err();
        assert_eq!(e.to_string(), "Command timed out after 200 ms");
    }

    #[test]
    fn fallback_route_never_makes_an_item() {
        let r = Route::fallback(
            "一二三四五六七八九十一二三四五六七八九十一二三四五六七八",
            Some("prj_a".into()),
        );
        assert_eq!(r.title.chars().count(), 24);
        assert_eq!((r.kind.as_str(), r.fallback), ("research", true));
        assert_eq!(r.why, "没能判断，先在这里回答");
        assert_eq!(Route::fallback("x", None).kind, "answer");
    }

    #[tokio::test]
    async fn a_failing_or_unusable_router_answers_in_place() {
        struct Gw(Option<&'static str>);
        #[async_trait]
        impl ModelGateway for Gw {
            async fn call(&self, _: &str, _: Tier, _: Duration, _: &[String]) -> Result<Printed> {
                match self.0 {
                    Some(t) => Ok(Printed {
                        text: t.into(),
                        model: "m".into(),
                        cost_usd: None,
                        ..Default::default()
                    }),
                    None => Err(msg("Command timed out after 60000 ms")),
                }
            }
        }
        let c = ctx();
        for gw in [
            Gw(None),
            Gw(Some("不是 JSON")),
            Gw(Some(r#"{"kind":"build"}"#)),
        ] {
            let r = route(&gw, &c, "知识系统怎么运作", &[], false).await;
            assert_eq!((r.kind.as_str(), r.fallback), ("research", true));
            assert_eq!(r.project.as_deref(), Some("prj_a"));
        }
        let none = RouteCtx {
            steve_project: None,
            projects: vec![],
            ..ctx()
        };
        assert_eq!(
            route(&Gw(None), &none, "hey", &[], false).await.kind,
            "answer"
        );
        /* “做成事项” was pressed: still an item */
        assert_eq!(route(&Gw(None), &c, "x", &[], true).await.kind, "item");
    }

    #[test]
    fn route_reads_the_new_fields() {
        let mut c = ctx();
        c.items = vec!["ST-3".into()];
        let r = parse_route(&c, "x", r#"{"kind":"research","confidence":0.82,"signals":["question","lookup","nope","question"],"offer":{"title":"修评论渲染","goal":""},"project":"prj_a","bot":"steve"}"#, false).unwrap();
        assert_eq!(r.kind, "research");
        assert_eq!(r.confidence, 0.82);
        assert_eq!(r.signals, vec!["question", "lookup"]);
        assert_eq!(
            r.offer,
            Some(Offer {
                title: "修评论渲染".into(),
                goal: "修评论渲染".into()
            })
        );
        /* research does not pick a bot: steve stays steve (the ST-20 swap) */
        assert_eq!((r.bot.as_str(), r.bot_from_project), ("steve", false));
        let r = parse_route(&c, "x", r#"{"kind":"chat"}"#, false).unwrap();
        assert_eq!((r.kind.as_str(), r.confidence), ("answer", 0.5));
        let r = parse_route(
            &c,
            "x",
            r##"{"kind":"supplement","item":"#st-3","confidence":2}"##,
            false,
        )
        .unwrap();
        assert_eq!((r.item.as_deref(), r.confidence), (Some("ST-3"), 1.0));
        assert!(
            parse_route(&c, "x", r#"{"kind":"supplement","item":"ST-9"}"#, false).is_none(),
            "another workspace's item"
        );
        assert!(parse_route(&c, "x", r#"{"kind":"status"}"#, false).is_none());
        let r = parse_route(&c, "x", r#"{"kind":"reroute","item":"ST-9"}"#, false).unwrap();
        assert_eq!(r.item, None);
        let r = parse_route(
            &c,
            "x",
            r#"{"kind":"ask","options":["对话页"," ","事项页","设置页","更多"]}"#,
            false,
        )
        .unwrap();
        assert_eq!(r.options, vec!["对话页", "事项页", "设置页"]);
        for k in KINDS {
            assert!(
                parse_route(
                    &c,
                    "x",
                    &format!(r#"{{"kind":"{k}","item":"ST-3"}}"#),
                    false
                )
                .is_some()
            );
        }
    }

    #[test]
    fn the_tag_note_is_left_to_the_code() {
        assert_eq!(
            strip_tag_note("好，交给开发 Bot 了。 标签：前端。"),
            "好，交给开发 Bot 了。"
        );
        assert_eq!(
            strip_tag_note("好。标签：前端、发布交付（新建）。 标签：前端。"),
            "好。"
        );
        assert_eq!(strip_tag_note("标签：前端"), "");
        let r = parse_route(
            &ctx(),
            "x",
            r#"{"kind":"item","reply":"交给它了。 标签：前端。"}"#,
            false,
        )
        .unwrap();
        assert_eq!(r.reply, "交给它了。");
    }

    #[test]
    fn an_offer_line_is_taken_off_the_answer() {
        let (t, o) = split_offer(
            "结论在前。\n\n细节。\nOFFER: {\"title\":\"修评论\",\"goal\":\"让评论支持 markdown\"}\n",
        );
        assert_eq!(t, "结论在前。\n\n细节。");
        assert_eq!(o.unwrap().goal, "让评论支持 markdown");
        let (t, o) = split_offer("说到 OFFER: 这个词");
        assert_eq!((t.as_str(), o), ("说到 OFFER: 这个词", None));
        let (t, o) = split_offer("答\nOFFER: 不是 JSON");
        assert_eq!((t.as_str(), o), ("答\nOFFER: 不是 JSON", None));
    }

    #[test]
    fn route_prompt_lists_focus_items_and_research() {
        let mut c = ctx();
        c.focus = vec![FocusItem {
            id: "ST-21".into(),
            title: "改按钮".into(),
            status: "run".into(),
            origin: "帮我把按钮改了".into(),
        }];
        c.researching = true;
        let p = route_prompt(&c, "x", &[], false);
        assert!(p.starts_with("You are the router of Steve"));
        assert!(p.contains("Reading code, docs or the knowledge base to answer a question is research, not an item."));
        assert!(p.contains("  ST-21: 改按钮 · run · \"帮我把按钮改了\""));
        assert!(p.contains("Steve is reading for an earlier question right now."));
        assert!(p.contains("  prj_a: prj_a —  · dev"));
    }
    #[tokio::test]
    async fn shared_gateway_passes_images_and_clears_the_scope() {
        struct Capture;
        #[async_trait]
        impl ModelGateway for Capture {
            async fn call(&self, _: &str, _: Tier, _: Duration, _: &[String]) -> Result<Printed> {
                let images = current_images().expect("image scope reaches actual model gateway");
                assert_eq!(images[0]["source"]["data"], "aW1hZ2U=");
                Ok(Printed::default())
            }
        }
        use steve_modules::Gateway;
        let g = SharedGateway(std::sync::Arc::new(Capture));
        let image = json!({"type":"image","source":{"type":"base64","media_type":"image/png","data":"aW1hZ2U="}});
        g.run_with_images(
            steve_modules::Tier::Smart,
            "read",
            &[image],
            Duration::from_secs(1),
        )
        .await
        .unwrap();
        assert!(current_images().is_none());
    }
}
