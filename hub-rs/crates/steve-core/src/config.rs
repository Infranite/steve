//! The part of config.ts the domain layer reads. Built by the binary from the environment
//! ([`CoreConfig::from_env`]) or directly in tests.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct CoreConfig {
    /// `STEVE_HOME`
    pub home: PathBuf,
    /// `STEVE_HOME/data` (database, attachments)
    pub data_dir: PathBuf,
    /// `STEVE_HOME/worktrees`
    pub worktrees: PathBuf,
    /// `STEVE_HOME/scratch`
    pub scratch: PathBuf,
    /// `STEVE_PORT`, for the MCP URL handed to adapters (always 127.0.0.1).
    pub port: u16,
    /// `CLAUDE_CODE_EXECUTABLE`, used by the model gateway.
    pub claude_executable: PathBuf,
    /// `CODEX_EXECUTABLE`: `codex exec` for Steve's judgments when Steve's harness is Codex.
    pub codex_executable: PathBuf,
    /// The top-level `model` of `$CODEX_HOME/config.toml` (what `codex exec` uses without `-m`).
    pub codex_model: Option<String>,
    /// `STEVE_MODELS_FAST`, tried in order.
    pub models_fast: Vec<String>,
    /// `STEVE_MODELS_SMART`, tried in order.
    pub models_smart: Vec<String>,
    /// `STEVE_MODELS_RESEARCH`, tried in order by inline research.
    pub models_research: Vec<String>,
    /// `STEVE_MODELS_CHECK`: the disclosure check's default; empty = the fast list.
    pub models_check: Vec<String>,
    /// Model for bots that don't pick one, per harness (`STEVE_DEFAULT_MODEL_CLAUDE`).
    pub default_models: HashMap<String, String>,
    /// `STEVE_REPO`: Steve's own repository (seed project, router hint).
    pub steve_repo: PathBuf,
    /// `STEVE_DEBUG_ROUTE`: log the router's raw output.
    pub debug_route: bool,
    /// Delay before resuming interrupted turns after boot (3 s: after the listener is up).
    pub resume_delay: Duration,
    /// `STEVE_NO_RESUME=1`: never run agents at boot (no automatic resume, no flushing of held prompts).
    /// Always set when a Hub opens a copy of the production database.
    pub no_resume: bool,
}

fn env(k: &str, d: &str) -> String {
    std::env::var(k).unwrap_or_else(|_| d.to_string())
}
fn list(s: &str) -> Vec<String> {
    s.split(',').map(|x| x.to_string()).collect()
}

/// A comma list with blanks dropped (research and check lists, as steve-research and the channels read them).
fn trimmed(s: &str) -> Vec<String> {
    s.split(',')
        .map(str::trim)
        .filter(|x| !x.is_empty())
        .map(String::from)
        .collect()
}

impl CoreConfig {
    /// Same variables and defaults as config.ts.
    pub fn from_env() -> CoreConfig {
        let home_dir = std::env::var("HOME").unwrap_or_else(|_| "/".into());
        let home = std::env::var("STEVE_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| Path::new(&home_dir).join(".steve"));
        let home = std::path::absolute(&home).unwrap_or(home);
        let mut c = CoreConfig::for_home(&home);
        c.port = env("STEVE_PORT", "8440").parse().unwrap_or(8440);
        c.claude_executable = env("CLAUDE_CODE_EXECUTABLE", "/opt/homebrew/bin/claude").into();
        c.codex_executable = env("CODEX_EXECUTABLE", "/opt/homebrew/bin/codex").into();
        let codex_home = std::env::var("CODEX_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| Path::new(&home_dir).join(".codex"));
        c.codex_model = crate::models::codex_config_model(&codex_home);
        c.models_fast = list(&env(
            "STEVE_MODELS_FAST",
            "claude-haiku-5-5,claude-sonnet-5-5",
        ));
        c.models_smart = list(&env(
            "STEVE_MODELS_SMART",
            "claude-sonnet-5-5,claude-opus-5-5",
        ));
        let research = trimmed(&env("STEVE_MODELS_RESEARCH", ""));
        if !research.is_empty() {
            c.models_research = research;
        }
        c.models_check = trimmed(&env("STEVE_MODELS_CHECK", ""));
        c.default_models.insert(
            "claude-code".into(),
            env("STEVE_DEFAULT_MODEL_CLAUDE", "claude-opus-5-5"),
        );
        c.steve_repo = std::env::var("STEVE_REPO")
            .map(PathBuf::from)
            .unwrap_or_else(|_| Path::new(&home_dir).join("Agents/steve"));
        c.no_resume = std::env::var("STEVE_NO_RESUME").is_ok_and(|v| v == "1");
        c.debug_route = std::env::var("STEVE_DEBUG_ROUTE").is_ok_and(|v| !v.is_empty());
        c
    }

    /// Defaults rooted at `home` (tests): no repo, port 8440, default model lists.
    pub fn for_home(home: &Path) -> CoreConfig {
        CoreConfig {
            home: home.to_path_buf(),
            data_dir: home.join("data"),
            worktrees: home.join("worktrees"),
            scratch: home.join("scratch"),
            port: 8440,
            claude_executable: "/opt/homebrew/bin/claude".into(),
            codex_executable: "/opt/homebrew/bin/codex".into(),
            codex_model: None,
            models_fast: list("claude-haiku-5-5,claude-sonnet-5-5"),
            models_smart: list("claude-sonnet-5-5,claude-opus-5-5"),
            models_research: list("claude-sonnet-5-5,claude-opus-5-5"),
            models_check: vec![],
            default_models: HashMap::from([(
                "claude-code".to_string(),
                "claude-opus-5-5".to_string(),
            )]),
            steve_repo: home.join("no-repo"),
            debug_route: false,
            resume_delay: Duration::from_secs(3),
            no_resume: false,
        }
    }

    pub fn releases_last(&self) -> PathBuf {
        self.home.join("releases/last.json")
    }
}
