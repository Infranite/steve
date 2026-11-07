//! One model run of a harness CLI in print mode: `claude -p` (json or stream-json) or `codex exec
//! --json`. The caller has already chosen the harness and the model; there are no tiers, no model
//! lists and no fallback here, so the Hub's gateway and a Node can both drive it.
//!
//! The run lays the call out for the prompt cache (the stable head as the system prompt or Codex's
//! instructions), passes images (stdin for Claude Code, `-i` files for Codex), streams the model's
//! text to a callback and reads the usage the harness reported as node-api [`UsageReport`]s.
//!
//! Errors are typed ([`PrintError`]); the words a user sees are the caller's.

mod claude;
mod codex;
mod usage;

pub use claude::{
    ATTACHMENT_TOOLS, Streamed, claude_args, claude_layout, result_of, tool_args, user_message,
};
pub use codex::{CODEX_OFF, CodexEnd, CodexStreamed, codex_args, instructions_file};
pub use usage::{claude_usage, codex_usage, cost_f64, money, tokens};

use std::path::Path;
use std::time::Duration;
pub use steve_node_api::{Epoch, Measurement, Money, Tokens, UsageReport, UsageScope};

/// The system prompt of a call whose prompt has no stable head of its own.
pub const GATEWAY_SYSTEM: &str = "You are a component of Steve, a personal assistant app. Follow the instructions in the user message exactly and reply with only what they ask for.";

/// Which CLI runs the call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Harness {
    /// `claude -p`.
    Claude,
    /// `codex exec`.
    Codex,
}

/// What the run may use besides answering.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ToolMode {
    /// No tools at all: the cache-friendly layout.
    #[default]
    None,
    /// Read only (files already placed under `cwd`); Codex keeps its read-only shell.
    ReadOnly,
}

/// An image for the user message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    /// `image/png`, `image/jpeg`…
    pub mime: String,
    pub bytes: Vec<u8>,
}

/// Told what the model has written so far in its current message, each time it changes.
pub type OnText<'a> = &'a (dyn Fn(&str) + Send + Sync);

/// Told the model's current message each time it changes: its number in the run (a new number is a
/// new message, whose text replaces the last one's) and its text so far.
pub type OnMessage<'a> = &'a (dyn Fn(u32, &str) + Send + Sync);

/// Told the harness's process id as soon as it runs, before any of its output is read. The process
/// leads a process group of its own (the id is the group's). False: the caller could not keep track
/// of it; the process is killed and the run fails.
pub type OnSpawn<'a> = &'a (dyn Fn(u32) -> bool + Send + Sync);

/// How a run reports back (crate-internal): its text, and its process when a host supervises it.
#[derive(Clone, Copy, Default)]
pub(crate) struct Hooks<'a> {
    pub text: Option<OnMessage<'a>>,
    pub spawned: Option<OnSpawn<'a>>,
}

impl Hooks<'_> {
    /// The new process is known to the host (or there is none to tell); false: kill it.
    pub(crate) fn spawned(&self, child: &tokio::process::Child) -> bool {
        match self.spawned {
            None => true,
            Some(f) => child.id().is_some_and(f),
        }
    }
}

/// A process the host would not keep (see [`OnSpawn`]).
pub(crate) fn untracked() -> PrintError {
    PrintError::Io(std::io::Error::other(
        "the harness process could not be recorded",
    ))
}

/// One run with one model.
#[derive(Debug, Clone)]
pub struct PrintRun<'a> {
    pub harness: Harness,
    pub executable: &'a Path,
    /// The working directory; Codex's cached instructions and image files are written below it.
    pub cwd: &'a Path,
    /// Empty: the harness's default (Codex only; `-m` is left out).
    pub model: &'a str,
    /// Reasoning effort (`--effort` / `model_reasoning_effort`).
    pub effort: Option<&'a str>,
    /// The prompt's stable head; `None`: [`GATEWAY_SYSTEM`] for a call without tools.
    pub head: Option<&'a str>,
    /// The rest of the prompt.
    pub message: &'a str,
    pub tools: ToolMode,
    pub images: &'a [Image],
    pub timeout: Duration,
}

/// The model's answer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Reply {
    pub text: String,
    /// The harness's total for the run (`total_cost_usd`); Codex reports none.
    pub cost: Option<Money>,
}

/// Why a run gave no answer.
#[derive(Debug)]
pub enum PrintError {
    /// The accepted run's output/ingress budget was exhausted.
    OutputLimit,
    /// Still running after the timeout; the process is killed.
    Timeout(Duration),
    /// Exited unsuccessfully without a result; its stderr, trimmed.
    Failed(String),
    /// The harness reported an error (an `is_error` result, Codex's error message). `transient`: the
    /// provider was unavailable (a 503 or overload), so another try may succeed.
    Harness {
        text: String,
        transient: bool,
    },
    /// Codex's turn failed without saying why.
    CodexFailed,
    /// Exited well but gave no result.
    NoResult,
    /// The process's pipes were not there.
    NoOutput,
    Io(std::io::Error),
}

impl PrintError {
    /// Whether another try of the same call may succeed.
    pub fn transient(&self) -> bool {
        matches!(
            self,
            PrintError::Harness {
                transient: true,
                ..
            }
        )
    }

    pub(crate) fn harness(text: String) -> PrintError {
        let transient = is_transient(&text);
        PrintError::Harness { text, transient }
    }
}

/// A provider error that says the service was unavailable or overloaded.
fn is_transient(text: &str) -> bool {
    let t = text.to_ascii_lowercase();
    ["503", "529", "overloaded"].iter().any(|k| t.contains(k))
}

/// The answer or the error, and the tokens the run reported either way (a failed try was spent too).
#[derive(Debug)]
pub struct Ran {
    pub reply: Result<Reply, PrintError>,
    /// One [`steve_node_api::Measurement::Final`] report per model (Claude's `modelUsage`) or for the
    /// run (`usage`, Codex).
    pub usage: Vec<UsageReport>,
}

impl Ran {
    pub(crate) fn failed(e: PrintError) -> Ran {
        Ran {
            reply: Err(e),
            usage: vec![],
        }
    }
}

/// Runs `run` once. Text goes to `on_text` as it is written (Claude Code: deltas through stream-json,
/// used when `on_text` is given or there are images; Codex: each whole message).
pub async fn run(run: &PrintRun<'_>, on_text: Option<OnText<'_>>) -> Ran {
    let text = on_text.map(|f| move |_: u32, t: &str| f(t));
    let hooks = Hooks {
        text: text.as_ref().map(|f| f as OnMessage<'_>),
        spawned: None,
    };
    run_with(run, hooks).await
}

/// Runs `run` once for a host that supervises the process (a Node): the harness leads its own
/// process group and `spawned` hears its id before anything else happens, so the host can stop the
/// whole group and prove it gone. The text always streams, message by message, to `on_message`.
pub async fn run_supervised(
    run: &PrintRun<'_>,
    spawned: OnSpawn<'_>,
    on_message: OnMessage<'_>,
) -> Ran {
    run_with(
        run,
        Hooks {
            text: Some(on_message),
            spawned: Some(spawned),
        },
    )
    .await
}

async fn run_with(run: &PrintRun<'_>, hooks: Hooks<'_>) -> Ran {
    match run.harness {
        Harness::Claude => claude::run(run, hooks).await,
        Harness::Codex => codex::run(run, hooks).await,
    }
}

/// How long a supervised run's output is still read after its process exited.
const AFTER_EXIT: Duration = Duration::from_secs(2);

/// Reads the child's output line by line (each to `line`) and waits for it. Supervised, the child's
/// exit ends the reading after [`AFTER_EXIT`] even when the pipe is still open: a process forked
/// elsewhere in the host meanwhile may have inherited its write end, and a host stopping a run
/// must not wait on a process it did not start.
pub(crate) async fn read_to_exit<R: tokio::io::AsyncRead + Unpin>(
    out: R,
    child: &mut tokio::process::Child,
    supervised: bool,
    mut line: impl FnMut(&str),
) -> std::io::Result<std::process::ExitStatus> {
    use tokio::io::AsyncBufReadExt;
    let mut lines = tokio::io::BufReader::new(out).lines();
    if !supervised {
        while let Some(l) = lines.next_line().await? {
            line(&l);
        }
        return child.wait().await;
    }
    let mut exited = None;
    loop {
        let next = match exited {
            Some(_) => match tokio::time::timeout(AFTER_EXIT, lines.next_line()).await {
                Ok(r) => r?,
                Err(_) => break,
            },
            None => tokio::select! {
                r = lines.next_line() => r?,
                s = child.wait() => {
                    exited = Some(s?);
                    continue;
                }
            },
        };
        match next {
            Some(l) => line(&l),
            None => break,
        }
    }
    match exited {
        Some(s) => Ok(s),
        None => child.wait().await,
    }
}

/// The child's stderr, read in the background as it comes.
pub(crate) struct Stderr {
    seen: std::sync::Arc<std::sync::Mutex<Vec<u8>>>,
    task: tokio::task::JoinHandle<()>,
}

impl Stderr {
    pub(crate) fn read<R: tokio::io::AsyncRead + Unpin + Send + 'static>(mut err: R) -> Self {
        use tokio::io::AsyncReadExt;
        let seen = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
        let into = seen.clone();
        let task = tokio::spawn(async move {
            let mut buf = [0u8; 4096];
            while let Ok(n) = err.read(&mut buf).await
                && n > 0
            {
                into.lock()
                    .unwrap_or_else(|p| p.into_inner())
                    .extend_from_slice(&buf[..n]);
            }
        });
        Self { seen, task }
    }

    /// What the child wrote, once it exited. Supervised, like [`read_to_exit`], the read ends after
    /// [`AFTER_EXIT`] with what came so far even when another process still holds the pipe open.
    pub(crate) async fn after_exit(mut self, supervised: bool) -> String {
        if supervised {
            if tokio::time::timeout(AFTER_EXIT, &mut self.task)
                .await
                .is_err()
            {
                self.task.abort();
            }
        } else {
            let _ = (&mut self.task).await;
        }
        let seen = std::mem::take(&mut *self.seen.lock().unwrap_or_else(|p| p.into_inner()));
        String::from_utf8_lossy(&seen).into_owned()
    }
}

/// `group`: the process leads a process group of its own.
fn command(exe: &Path, cwd: &Path, group: bool) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(exe);
    // what a model run must not inherit from the Hub: the upgrade channel, the key store, secret files
    for key in steve_vault::harness::hub_private_vars() {
        cmd.env_remove(key);
    }
    cmd.current_dir(cwd).kill_on_drop(true);
    if group {
        cmd.process_group(0);
    }
    cmd
}

/// JS `String(v)` for a JSON value.
pub fn jstr(v: &serde_json::Value) -> String {
    use serde_json::Value;
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
pub fn truthy(v: &serde_json::Value) -> bool {
    use serde_json::Value;
    match v {
        Value::Null => false,
        Value::Bool(b) => *b,
        Value::Number(n) => n.as_f64().is_some_and(|f| f != 0.0 && !f.is_nan()),
        Value::String(s) => !s.is_empty(),
        _ => true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_busy_provider_is_transient() {
        assert!(PrintError::harness("API Error: 503 upstream unavailable".into()).transient());
        assert!(PrintError::harness("Overloaded".into()).transient());
        assert!(!PrintError::harness("prompt is too long".into()).transient());
        assert!(!PrintError::NoResult.transient());
    }

    /// A leaf: a Node can run it without the Hub's store, core or statistics (steve-vault is the
    /// vault's store-free part: what a harness must not see).
    #[test]
    fn the_only_steve_dependencies_are_the_node_contract_and_the_store_free_vault() {
        let toml = include_str!("../Cargo.toml");
        let steve: Vec<&str> = toml
            .lines()
            .filter_map(|l| l.split_once(" = ").map(|(k, _)| k.trim()))
            .filter(|k| k.starts_with("steve-") || k.starts_with("steve_"))
            .collect();
        assert_eq!(steve, ["steve-node-api", "steve-vault"]);
    }
}
