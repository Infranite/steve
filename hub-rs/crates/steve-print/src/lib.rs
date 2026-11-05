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

/// Environment a model run must not inherit from the Hub (the upgrade control channel).
const SCRUBBED_ENV: [&str; 3] = [
    "STEVE_UPGRADE_CONTROL_TOKEN",
    "STEVE_SUPERVISOR_PID",
    "STEVE_UPGRADE_ID",
];

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
    match run.harness {
        Harness::Claude => claude::run(run, on_text).await,
        Harness::Codex => codex::run(run, on_text).await,
    }
}

fn command(exe: &Path, cwd: &Path) -> tokio::process::Command {
    let mut cmd = tokio::process::Command::new(exe);
    for key in SCRUBBED_ENV {
        cmd.env_remove(key);
    }
    cmd.current_dir(cwd).kill_on_drop(true);
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

    /// A leaf: a Node can run it without the Hub's store, core or statistics.
    #[test]
    fn the_only_steve_dependency_is_the_node_contract() {
        let toml = include_str!("../Cargo.toml");
        let steve: Vec<&str> = toml
            .lines()
            .filter_map(|l| l.split_once(" = ").map(|(k, _)| k.trim()))
            .filter(|k| k.starts_with("steve-") || k.starts_with("steve_"))
            .collect();
        assert_eq!(steve, ["steve-node-api"]);
    }
}
