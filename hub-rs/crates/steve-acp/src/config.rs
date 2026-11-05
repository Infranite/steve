//! Adapter resolution and runner settings (the ACP part of config.ts).

use std::collections::HashMap;
use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// Everything the runner needs from the Hub's configuration.
#[derive(Debug, Clone)]
pub struct AcpConfig {
    /// Adapter entry point per harness (`claude-code`, `codex`). A `.js` path runs with [`Self::node`].
    pub adapters: HashMap<String, PathBuf>,
    /// Node used for `.js` adapters (TS used its own `process.execPath`). `STEVE_NODE`, else `node` on PATH.
    pub node: PathBuf,
    /// Passed to adapters as `CLAUDE_CODE_EXECUTABLE`.
    pub claude_executable: PathBuf,
    /// `STEVE_HOME`; the sanitized Codex catalog goes to `<home>/codex/models.json`.
    pub steve_home: PathBuf,
    /// `$CODEX_HOME` (default `~/.codex`).
    pub codex_home: PathBuf,
    /// The user's home, for `~/` in Codex config.
    pub user_home: PathBuf,
    /// `STEVE_VERSION`, sent as `clientInfo.version`.
    pub version: String,
    /// `STEVE_IDLE_RELEASE_MS` (default 10 min).
    pub idle_release: Duration,
    /// Gateway retry waits; the attempt count is their number (TS: 20 s, 60 s).
    pub retry_delays: Vec<Duration>,
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

/// Node's `process.platform` / `process.arch` names, used in codex-acp's per-platform package name.
fn node_platform() -> (&'static str, &'static str) {
    let os = match env::consts::OS {
        "macos" => "darwin",
        "windows" => "win32",
        o => o,
    };
    let arch = match env::consts::ARCH {
        "aarch64" => "arm64",
        "x86_64" => "x64",
        a => a,
    };
    (os, arch)
}

pub fn default_claude_adapter(npm_root: &Path) -> PathBuf {
    npm_root.join("@agentclientprotocol/claude-agent-acp/dist/index.js")
}

/// codex-acp's bin is a Node wrapper that spawnSync's a native binary; killing the wrapper would orphan
/// the binary, so Steve runs the platform binary directly when it is installed.
pub fn default_codex_adapter(npm_root: &Path) -> PathBuf {
    let pkg = npm_root.join("@zed-industries/codex-acp");
    let (os, arch) = node_platform();
    let native = pkg
        .join("node_modules/@zed-industries")
        .join(format!("codex-acp-{os}-{arch}"))
        .join("bin/codex-acp");
    if native.exists() {
        native
    } else {
        pkg.join("bin/codex-acp.js")
    }
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
        let mut root: Option<PathBuf> = None;
        let mut npm = || root.get_or_insert_with(npm_root).clone();
        let claude = var("STEVE_ADAPTER_CLAUDE").unwrap_or_else(|| default_claude_adapter(&npm()));
        let codex = var("STEVE_ADAPTER_CODEX").unwrap_or_else(|| default_codex_adapter(&npm()));
        let steve_home = var("STEVE_HOME").unwrap_or_else(|| home.join(".steve"));
        let steve_home = std::path::absolute(&steve_home).unwrap_or(steve_home);
        Self {
            adapters: HashMap::from([
                ("claude-code".to_owned(), claude),
                ("codex".to_owned(), codex),
            ]),
            node: var("STEVE_NODE")
                .or_else(|| which("node"))
                .unwrap_or_else(|| PathBuf::from("node")),
            claude_executable: var("CLAUDE_CODE_EXECUTABLE")
                .unwrap_or_else(|| PathBuf::from("/opt/homebrew/bin/claude")),
            steve_home,
            codex_home: var("CODEX_HOME").unwrap_or_else(|| home.join(".codex")),
            user_home: home,
            version: env::var("STEVE_VERSION").unwrap_or_else(|_| "dev".into()),
            // Number("") is 0 in JS; garbage is NaN, which setTimeout treats as 0 — both release at once
            idle_release: Duration::from_millis(
                env::var("STEVE_IDLE_RELEASE_MS")
                    .map(|v| v.trim().parse::<f64>().unwrap_or(0.0).max(0.0) as u64)
                    .unwrap_or(600_000),
            ),
            retry_delays: vec![Duration::from_secs(20), Duration::from_secs(60)],
        }
    }
}

impl AcpConfig {
    /// The program and arguments that start `harness`'s adapter (a `.js` adapter runs through node;
    /// Codex gets its catalog and home).
    pub fn adapter_command(
        &self,
        harness: &str,
    ) -> Result<(std::path::PathBuf, Vec<std::ffi::OsString>), String> {
        let cmd = self
            .adapters
            .get(harness)
            .ok_or_else(|| format!("no adapter for harness {harness}"))?;
        let extra: Vec<std::ffi::OsString> = if harness == "codex" {
            crate::codex::codex_args(&self.codex_home, &self.steve_home, &self.user_home)
                .into_iter()
                .map(std::ffi::OsString::from)
                .collect()
        } else {
            vec![]
        };
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
