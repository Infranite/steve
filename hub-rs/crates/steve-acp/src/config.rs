//! Adapter resolution and runner settings (the ACP part of config.ts).

use std::collections::{BTreeMap, HashMap};
use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::Duration;

/// Upper bound for `npm root -g` discovery before falling back to the default global prefix.
/// Local npm prefix lookup normally completes in <300 ms; 5s bounds cold Node startup without
/// stalling Hub/Node boot.
const NPM_ROOT_TIMEOUT: Duration = Duration::from_secs(5);
/// Maximum stdout captured from `npm root -g` (a single directory path is <4 KiB; 64 KiB bounds
/// misbehaving shims).
const NPM_ROOT_MAX_STDOUT: usize = 64 * 1024;

/// Everything the ACP driver needs from the configuration (the Node's, built from the Hub's).
#[derive(Debug, Clone)]
pub struct AcpConfig {
    /// Adapter entry point per harness. A `.js`/`.mjs`/`.cjs` path runs with [`Self::node`].
    pub adapters: HashMap<String, PathBuf>,
    /// What the manifest declares about adapters beyond their command ([`crate::adapters`]).
    pub declared: HashMap<String, steve_harness_spec::Declaration>,
    /// Node used for JS adapters (TS used its own `process.execPath`). `STEVE_NODE`, else `node` on PATH.
    pub node: PathBuf,
    /// The harness executable, passed to an adapter that declares `CLAUDE_CODE_EXECUTABLE`
    /// (`executable_env`).
    pub claude_executable: PathBuf,
    /// Explicit `$CLAUDE_CONFIG_DIR` override when configured on the Node; `None` keeps Claude's
    /// default ambient location without injecting `CLAUDE_CONFIG_DIR`.
    pub claude_config_dir: Option<PathBuf>,
    /// `STEVE_HOME`; the sanitized Codex catalog goes to `<home>/codex/models.json`.
    pub steve_home: PathBuf,
    /// `$CODEX_HOME` (default `~/.codex`).
    pub codex_home: PathBuf,
    /// The user's home, for `~/` in Codex config.
    pub user_home: PathBuf,
    /// `STEVE_VERSION`, sent as `clientInfo.version`.
    pub version: String,
}

/// Global `node_modules` directory: checks `<node>/../../lib/node_modules` first without spawning
/// a subprocess, then `npm root -g`, falling back to Homebrew's global modules.
pub fn npm_root() -> PathBuf {
    if let Some(node) = env::var_os("STEVE_NODE")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| which("node"))
        && let Some(bin) = node.parent()
        && let Some(prefix) = bin.parent()
    {
        let candidate = prefix.join("lib/node_modules");
        if candidate.is_dir() {
            return candidate;
        }
        #[cfg(windows)]
        {
            let win_candidate = bin.join("node_modules");
            if win_candidate.is_dir() {
                return win_candidate;
            }
        }
    }
    let mut cmd = Command::new("npm");
    cmd.args(["root", "-g"]);
    steve_runtime::process::run_bounded(
        &mut cmd,
        "npm",
        steve_runtime::process::BoundedOptions {
            max_output_bytes: NPM_ROOT_MAX_STDOUT,
            ..steve_runtime::process::BoundedOptions::capture("npm_root", NPM_ROOT_TIMEOUT)
        },
    )
    .ok()
    .filter(|out| out.status.success())
    .map(|out| String::from_utf8_lossy(&out.stdout).trim().to_owned())
    .filter(|s| !s.is_empty())
    .map(PathBuf::from)
    .unwrap_or_else(|| PathBuf::from("/opt/homebrew/lib/node_modules"))
}

pub fn default_claude_adapter(npm_root: &Path) -> PathBuf {
    npm_root.join("@agentclientprotocol/claude-agent-acp/dist/index.js")
}

/// codex-acp (`@agentclientprotocol/codex-acp`): resolves the platform-specific native binary directly
/// when installed beside `@agentclientprotocol/codex-acp`, falling back to `dist/index.js` through
/// [`AcpConfig::node`].
pub fn default_codex_adapter(npm_root: &Path) -> PathBuf {
    if let Some(pkg) = codex_native_pkg() {
        #[cfg(windows)]
        let bin = "bin/codex-acp.exe";
        #[cfg(not(windows))]
        let bin = "bin/codex-acp";
        let native = npm_root.join(pkg).join(bin);
        if native.is_file() {
            return native;
        }
        let nested = npm_root
            .join("@agentclientprotocol/codex-acp/node_modules")
            .join(pkg)
            .join(bin);
        if nested.is_file() {
            return nested;
        }
    }
    npm_root.join("@agentclientprotocol/codex-acp/dist/index.js")
}

fn codex_native_pkg() -> Option<&'static str> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Some("@agentclientprotocol/codex-acp-darwin-arm64"),
        ("macos", "x86_64") => Some("@agentclientprotocol/codex-acp-darwin-x64"),
        ("linux", "aarch64") => Some("@agentclientprotocol/codex-acp-linux-arm64"),
        ("linux", "x86_64") => Some("@agentclientprotocol/codex-acp-linux-x64"),
        ("windows", "aarch64") => Some("@agentclientprotocol/codex-acp-win32-arm64"),
        ("windows", "x86_64") => Some("@agentclientprotocol/codex-acp-win32-x64"),
        _ => None,
    }
}

fn shebang_runs_node(line: &str) -> bool {
    let Some(rest) = line.strip_prefix("#!") else {
        return false;
    };
    let mut parts = rest.trim().split_whitespace();
    let Some(first) = parts.next() else {
        return false;
    };
    let Some(bin) = Path::new(first).file_name().and_then(|s| s.to_str()) else {
        return false;
    };
    let cmd = if bin == "env" {
        let Some(c) = parts.find(|tok| !tok.starts_with('-') && !tok.contains('=')) else {
            return false;
        };
        c
    } else {
        first
    };
    Path::new(cmd)
        .file_name()
        .and_then(|s| s.to_str())
        .is_some_and(|name| matches!(name, "node" | "nodejs"))
}

pub fn is_node_script(path: &Path) -> bool {
    let ext = path.extension().and_then(|e| e.to_str());
    if ext.is_some_and(|e| matches!(e, "js" | "mjs" | "cjs")) {
        return true;
    }
    if ext.is_some_and(|e| {
        matches!(
            e,
            "py" | "sh" | "bash" | "zsh" | "rb" | "pl" | "exe" | "cmd" | "bat" | "ps1"
        )
    }) {
        return false;
    }
    if path.is_file()
        && let Ok(f) = std::fs::File::open(path)
    {
        use std::io::Read;
        let mut head = [0u8; 128];
        if let Ok(n) = f.take(128).read(&mut head)
            && head[..n].starts_with(b"#!")
        {
            let first = head[..n]
                .split(|b| *b == b'\n' || *b == b'\r')
                .next()
                .unwrap_or_default();
            let line = String::from_utf8_lossy(first);
            return shebang_runs_node(&line);
        }
    }
    false
}

pub fn is_executable_file(p: &Path) -> bool {
    if !p.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::metadata(p)
            .map(|m| m.permissions().mode() & 0o111 != 0)
            .unwrap_or(false)
    }
    #[cfg(not(unix))]
    {
        true
    }
}

pub fn which_in(name: &str, path_env: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    env::split_paths(path_env?)
        .map(|d| d.join(name))
        .find(|p| is_executable_file(p))
}

pub fn default_claude_executable(path_env: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    which_in("claude", path_env)
}

fn which(name: &str) -> Option<PathBuf> {
    which_in(name, env::var_os("PATH").as_deref())
}

fn home_dir() -> PathBuf {
    env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// Resolves every built-in adapter command in priority order:
/// 1. `STEVE_ADAPTER_*` environment variable (`var`);
/// 2. `declared_commands` from `harness.json`;
/// 3. `<steve_home>/adapters` (when installed there);
/// 4. `cli_bin` on `PATH` (`path_env`);
/// 5. global npm (`npm_root`, for npm-managed entries, lazily queried only when needed);
/// 6. fallback: `<steve_home>/adapters/...` for npm-managed entries, or bare `cli_bin` for
///    PATH-only entries.
pub fn resolve_builtin_adapters(
    steve_home: &Path,
    declared_commands: &HashMap<String, PathBuf>,
    var: impl Fn(&str) -> Option<PathBuf>,
    path_env: Option<&std::ffi::OsStr>,
    mut global_npm: impl FnMut() -> PathBuf,
) -> HashMap<String, PathBuf> {
    let bins: HashMap<&str, &str> = steve_harness_spec::builtin_bins()
        .map(|(id, bin, _)| (id, bin))
        .collect();
    let vars: Vec<(&str, &str)> = crate::adapters::builtin_commands(Path::new(""))
        .map(|(id, v, _)| (id, v))
        .collect();
    let mut out = HashMap::with_capacity(vars.len());
    for (id, command_var) in vars {
        if let Some(cmd) = var(command_var) {
            out.insert(id.to_owned(), cmd);
            continue;
        }
        if let Some(cmd) = declared_commands.get(id) {
            out.insert(id.to_owned(), cmd.clone());
            continue;
        }
        if steve_harness_spec::is_npm_managed(id) {
            let own = crate::managed::command(steve_home, id).unwrap_or_default();
            if own.exists() {
                out.insert(id.to_owned(), own);
                continue;
            }
            if let Some(bin) = bins.get(id).and_then(|b| which_in(b, path_env)) {
                out.insert(id.to_owned(), bin);
                continue;
            }
            let root = global_npm();
            let global = crate::adapters::builtin_commands(&root)
                .find(|(b, ..)| *b == id)
                .map(|(.., c)| c)
                .filter(|c| c.exists());
            out.insert(id.to_owned(), global.unwrap_or(own));
        } else {
            let bin_name = bins.get(id).copied().unwrap_or(id);
            let cmd = which_in(bin_name, path_env).unwrap_or_else(|| PathBuf::from(bin_name));
            out.insert(id.to_owned(), cmd);
        }
    }
    for (id, cmd) in declared_commands {
        out.entry(id.clone()).or_insert_with(|| cmd.clone());
    }
    out
}

impl AcpConfig {
    /// Package reads share the same lease as the trusted installer, including the Hub's
    /// separate session Node and jobs Node. External/global commands remain operator-managed.
    pub fn adapter_prefix(&self, id: &str) -> Option<PathBuf> {
        self.adapters
            .get(id)
            .filter(|p| p.starts_with(crate::managed::root(&self.steve_home)))
            .map(|_| self.steve_home.clone())
    }
    /// Reads the same environment variables as config.ts. `npm root -g` only runs when an adapter
    /// path is not given explicitly.
    pub fn from_env() -> Self {
        let var = |k: &str| env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
        let home = home_dir();
        let steve_home = var("STEVE_HOME").unwrap_or_else(|| home.join(".steve"));
        let steve_home = std::path::absolute(&steve_home).unwrap_or(steve_home);
        let path_env = env::var_os("PATH");
        let mut root: Option<PathBuf> = None;
        let adapters = resolve_builtin_adapters(
            &steve_home,
            &HashMap::new(),
            var,
            path_env.as_deref(),
            || root.get_or_insert_with(npm_root).clone(),
        );
        Self {
            adapters,
            declared: HashMap::new(),
            node: var("STEVE_NODE")
                .or_else(|| which("node"))
                .unwrap_or_else(|| PathBuf::from("node")),
            claude_executable: var("CLAUDE_CODE_EXECUTABLE")
                .or_else(|| which("claude"))
                .unwrap_or_else(|| PathBuf::from("/opt/homebrew/bin/claude")),
            claude_config_dir: var("CLAUDE_CONFIG_DIR"),
            steve_home,
            codex_home: var("CODEX_HOME").unwrap_or_else(|| home.join(".codex")),
            user_home: home,
            version: env::var("STEVE_VERSION").unwrap_or_else(|_| "dev".into()),
        }
    }
}

impl AcpConfig {
    /// The program and arguments that start `harness`'s adapter (a `.js`/`.mjs`/`.cjs` adapter
    /// runs through node; then the built-in or declared arguments, [`crate::adapters`]).
    pub fn adapter_command(
        &self,
        harness: &str,
    ) -> Result<(std::path::PathBuf, Vec<std::ffi::OsString>), String> {
        self.adapter_command_for_mode(harness, None)
    }

    /// [`Self::adapter_command`] with any per-mode launch flags (`mode_args`) appended for `mode`.
    pub fn adapter_command_for_mode(
        &self,
        harness: &str,
        mode: Option<steve_node_api::PermissionMode>,
    ) -> Result<(std::path::PathBuf, Vec<std::ffi::OsString>), String> {
        let cmd = self
            .adapters
            .get(harness)
            .ok_or_else(|| format!("no adapter for harness {harness}"))?;
        let extra = self.launch_args(harness, mode);
        Ok(if is_node_script(cmd) {
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

    /// Harness state directories (`harness_id -> path`) derived from `builtins.toml` and this
    /// config, keeping harness-specific environment variable mapping inside `steve-acp`.
    pub fn discovery_state_dirs(&self) -> BTreeMap<String, PathBuf> {
        let mut state_dirs = BTreeMap::new();
        for b in steve_harness_spec::builtins() {
            if let Some(state) = &b.properties.state {
                let dir = match state.env.as_str() {
                    "CLAUDE_CONFIG_DIR" => Some(
                        self.claude_config_dir
                            .clone()
                            .unwrap_or_else(|| self.user_home.join(&state.default)),
                    ),
                    "CODEX_HOME" => Some(self.codex_home.clone()),
                    _ => None,
                };
                if let Some(dir) = dir {
                    state_dirs.insert(b.id.to_string(), dir);
                }
            }
        }
        state_dirs
    }

    /// Harness CLI binaries (`harness_id -> path`) derived from `builtins.toml`, this config, and
    /// `path_env`, keeping harness-specific executable discovery inside `steve-acp`.
    pub fn discovery_binaries(
        &self,
        env_var: impl Fn(&str) -> Option<std::ffi::OsString>,
        path_env: Option<&std::ffi::OsStr>,
    ) -> BTreeMap<String, PathBuf> {
        let mut bins = BTreeMap::new();
        for b in steve_harness_spec::builtins() {
            if b.properties.executable_env.as_deref() == Some("CLAUDE_CODE_EXECUTABLE") {
                if !self.claude_executable.as_os_str().is_empty() {
                    bins.insert(b.id.to_string(), self.claude_executable.clone());
                }
            } else if b.properties.usage_fallback == steve_harness_spec::UsageFallback::CodexRollout
            {
                let detected = env_var("CODEX_EXECUTABLE")
                    .filter(|v| !v.is_empty())
                    .map(PathBuf::from)
                    .or_else(|| {
                        b.detect_bins
                            .iter()
                            .find_map(|name| which_in(name, path_env))
                    });
                if let Some(bin) = detected {
                    bins.insert(b.id.to_string(), bin);
                }
            }
        }
        bins
    }
}

fn resolve_program_in(
    program: &str,
    executable: bool,
    path_env: Option<&std::ffi::OsStr>,
) -> Option<PathBuf> {
    let check = |p: &Path| {
        if executable {
            is_executable_file(p)
        } else {
            p.is_file()
        }
    };
    let p = Path::new(program);
    if p.components().count() > 1 {
        check(p).then(|| p.to_path_buf())
    } else {
        env::split_paths(path_env?)
            .map(|d| d.join(p))
            .find(|candidate| check(candidate))
    }
}

fn global_npm_root_from_node(node: &str, path_env: Option<&std::ffi::OsStr>) -> Option<PathBuf> {
    let node_path = resolve_program_in(node, true, path_env)?;
    let from_bin = |p: &Path| {
        let bin = p.parent()?;
        let prefix = bin.parent()?;
        let lib = prefix.join("lib/node_modules");
        if lib.is_dir() {
            return Some(lib);
        }
        #[cfg(windows)]
        {
            let win = bin.join("node_modules");
            if win.is_dir() {
                return Some(win);
            }
        }
        None
    };
    from_bin(&node_path).or_else(|| {
        std::fs::canonicalize(&node_path)
            .ok()
            .and_then(|c| from_bin(&c))
    })
}

/// Offline harness/adapter preflight owned by `steve-acp` (invoked via `steve-node harness-probe`
/// during `steve-hub --check-config` and boot warnings): resolves every built-in and manifest
/// adapter without spawning `npm` or any harness process.
pub fn probe_harnesses(
    steve_home: &Path,
    manifest_path: &Path,
    var: impl Fn(&str) -> Option<String>,
) -> steve_node_api::HarnessPreflight {
    #[derive(serde::Deserialize, Default)]
    struct ManifestDoc {
        #[serde(default)]
        adapters: HashMap<String, steve_harness_spec::AdapterEntry>,
        #[serde(default)]
        node: Option<PathBuf>,
    }

    let manifest = std::fs::read_to_string(manifest_path)
        .ok()
        .and_then(|raw| serde_json::from_str::<ManifestDoc>(&raw).ok())
        .unwrap_or_default();
    let manifest_adapter_ids: std::collections::HashSet<String> =
        manifest.adapters.keys().cloned().collect();
    let (manifest_commands, declared_adapters) =
        steve_harness_spec::split_entries(manifest.adapters);

    let path_str = var("PATH").unwrap_or_default();
    let path_os = std::ffi::OsStr::new(&path_str);
    let node = var("STEVE_NODE")
        .filter(|v| !v.trim().is_empty())
        .or_else(|| {
            manifest
                .node
                .as_ref()
                .map(|p| p.to_string_lossy().into_owned())
        })
        .unwrap_or_else(|| "node".into());

    let mut global_root: Option<PathBuf> = None;
    let resolved = resolve_builtin_adapters(
        steve_home,
        &manifest_commands,
        |k| var(k).filter(|v| !v.is_empty()).map(PathBuf::from),
        Some(path_os),
        || {
            global_root
                .get_or_insert_with(|| {
                    global_npm_root_from_node(&node, Some(path_os))
                        .unwrap_or_else(|| steve_home.join(".no-global-npm"))
                })
                .clone()
        },
    );
    let sorted: BTreeMap<String, PathBuf> = resolved.into_iter().collect();

    let path_only_bins: HashMap<&str, (&str, &str)> = steve_harness_spec::builtin_bins()
        .zip(steve_harness_spec::builtin_command_vars())
        .filter(|((id, ..), _)| !steve_harness_spec::is_npm_managed(id))
        .map(|((id, bin, _), (_, v))| (id, (bin, v)))
        .collect();

    let mut out = steve_node_api::HarnessPreflight::default();
    for (harness, adapter_path) in &sorted {
        let adapter = adapter_path.to_string_lossy();
        let resolved_for_script = resolve_program_in(&adapter, false, Some(path_os))
            .unwrap_or_else(|| adapter_path.clone());
        let js = is_node_script(&resolved_for_script);
        let available = resolve_program_in(&adapter, !js, Some(path_os)).is_some();
        if !available {
            if crate::managed::home_of(adapter_path)
                .is_some_and(|home| !crate::managed::wanted(&home, harness))
            {
                continue;
            }
            if let Some((bin, cmd_var)) = path_only_bins.get(harness.as_str())
                && adapter.as_ref() == *bin
                && var(cmd_var).is_none_or(|v| v.is_empty())
                && !manifest_adapter_ids.contains(harness)
                && !declared_adapters.contains_key(harness)
            {
                continue;
            }
            out.missing_adapters.push(steve_node_api::MissingAdapter {
                harness: harness.clone(),
                adapter: adapter.into_owned(),
            });
        } else if js && resolve_program_in(&node, true, Some(path_os)).is_none() {
            out.missing_node.push(steve_node_api::MissingNode {
                harness: harness.clone(),
                node: node.clone(),
            });
        } else {
            out.resolved_adapters.push(harness.clone());
        }
    }
    out
}
