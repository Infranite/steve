//! Adapter resolution and runner settings (the ACP part of config.ts).

use std::collections::HashMap;
use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Everything the ACP driver needs from the configuration (the Node's, built from the Hub's).
#[derive(Debug, Clone)]
pub struct AcpConfig {
    /// Adapter entry point per harness (`claude-code`, `codex`). A `.js` path runs with [`Self::node`].
    pub adapters: HashMap<String, PathBuf>,
    /// What the manifest declares about adapters beyond their command ([`crate::adapters`]).
    pub declared: HashMap<String, crate::adapters::Declaration>,
    /// Node used for `.js` adapters (TS used its own `process.execPath`). `STEVE_NODE`, else `node` on PATH.
    pub node: PathBuf,
    /// The harness executable, passed to an adapter that declares a variable for it
    /// (`executable_env`).
    pub claude_executable: PathBuf,
    /// `STEVE_HOME`; the sanitized Codex catalog goes to `<home>/codex/models.json`.
    pub steve_home: PathBuf,
    /// `$CODEX_HOME` (default `~/.codex`).
    pub codex_home: PathBuf,
    /// The user's home, for `~/` in Codex config.
    pub user_home: PathBuf,
    /// `STEVE_VERSION`, sent as `clientInfo.version`.
    pub version: String,
}

/// `npm root -g`, run once; failure falls back to Homebrew's global modules like the TS code.
pub fn npm_root() -> PathBuf {
    Command::new("npm")
        .args(["root", "-g"])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_owned())
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/opt/homebrew/lib/node_modules"))
}

pub fn default_claude_adapter(npm_root: &Path) -> PathBuf {
    npm_root.join("@agentclientprotocol/claude-agent-acp/dist/index.js")
}

/// codex-acp (`@agentclientprotocol/codex-acp`): a Node program, run with [`AcpConfig::node`].
pub fn default_codex_adapter(npm_root: &Path) -> PathBuf {
    npm_root.join("@agentclientprotocol/codex-acp/dist/index.js")
}

fn which(name: &str) -> Option<PathBuf> {
    env::split_paths(&env::var_os("PATH")?)
        .map(|d| d.join(name))
        .find(|p| p.is_file())
}

fn home_dir() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

impl AcpConfig {
    /// Reads the same environment variables as config.ts. `npm root -g` only runs when an adapter
    /// path is not given explicitly.
    pub fn from_env() -> Self {
        let var = |k: &str| env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
        let home = home_dir();
        let steve_home = var("STEVE_HOME").unwrap_or_else(|| home.join(".steve"));
        let steve_home = std::path::absolute(&steve_home).unwrap_or(steve_home);
        let mut root: Option<PathBuf> = None;
        let mut npm = || root.get_or_insert_with(npm_root).clone();
        // npm root -g only when an adapter is not named by its variable
        let named: Vec<(&str, Option<PathBuf>)> = crate::adapters::builtin_ids()
            .zip(crate::adapters::builtin_commands(Path::new("")))
            .map(|(id, (_, v, _))| (id, var(v)))
            .collect();
        let adapters: HashMap<String, PathBuf> = named
            .into_iter()
            .map(|(id, cmd)| {
                // Steve's own install, else a global npm one, else Steve's own path
                let cmd = cmd.unwrap_or_else(|| {
                    let own = crate::managed::command(&steve_home, id).unwrap_or_default();
                    if own.exists() {
                        return own;
                    }
                    let root = npm();
                    crate::adapters::builtin_commands(&root)
                        .find(|(b, ..)| *b == id)
                        .map(|(.., c)| c)
                        .filter(|c| c.exists())
                        .unwrap_or(own)
                });
                (id.to_owned(), cmd)
            })
            .collect();
        Self {
            adapters,
            declared: HashMap::new(),
            node: var("STEVE_NODE")
                .or_else(|| which("node"))
                .unwrap_or_else(|| PathBuf::from("node")),
            claude_executable: var("CLAUDE_CODE_EXECUTABLE")
                .unwrap_or_else(|| PathBuf::from("/opt/homebrew/bin/claude")),
            steve_home,
            codex_home: var("CODEX_HOME").unwrap_or_else(|| home.join(".codex")),
            user_home: home,
            version: env::var("STEVE_VERSION").unwrap_or_else(|_| "dev".into()),
        }
    }
}

impl AcpConfig {
    /// The program and arguments that start `harness`'s adapter (a `.js` adapter runs through node;
    /// then the declared extras and arguments, [`crate::adapters`]).
    pub fn adapter_command(
        &self,
        harness: &str,
    ) -> Result<(std::path::PathBuf, Vec<std::ffi::OsString>), String> {
        let cmd = self
            .adapters
            .get(harness)
            .ok_or_else(|| format!("no adapter for harness {harness}"))?;
        let extra = self.launch_args(harness);
        Ok(if cmd.extension().is_some_and(|e| e == "js") {
            (
                self.node.clone(),
                std::iter::once(cmd.clone().into_os_string())
                    .chain(extra)
                    .collect(),
            )
        } else {
            (cmd.clone(), extra)
        })
    }
}
