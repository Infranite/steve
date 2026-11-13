//! The harness adapter registry (harness parity S1): what Steve knows about each ACP adapter, keyed
//! by its id (any string). Everything Steve needs to run sessions, jobs and judgments over plain
//! ACP is an id, a name and a launch command; what an adapter needs beyond that is a **declared
//! property** here, never a comparison with its name elsewhere.
//!
//! Sources, later over earlier:
//! 1. the built-in entries below, as data;
//! 2. the manifest a Node or the Hub reads (`harness.json`): `{"adapters": {"<id>": "/path"}}` as
//!    before, or `{"adapters": {"<id>": {"command": "/path", "name": "My Agent", "args": [],
//!    "env": {}, "properties": {…}}}}` for an adapter of one's own or to change a built-in one.
//!
//! [`probe`] asks an adapter what it is (`initialize`): its title and version, whether it takes
//! images, loads and lists sessions, and how one signs in. Nothing here refuses to start: an
//! adapter that cannot be started or initialized is reported as such.

use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use steve_node_api::PermissionMode;

use crate::rpc::{Conn, Spawn};
use crate::util::truthy;

/// The ACP mode ids Steve's permission modes are set as. Without a declaration an adapter gets the
/// ids most ACP adapters use (`default`, `plan`, `acceptEdits`, `auto`, `bypassPermissions`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModeMap {
    pub plan: String,
    pub ask: String,
    pub accept_edits: String,
    pub auto: String,
    pub full_access: String,
}

impl Default for ModeMap {
    fn default() -> Self {
        Self {
            plan: "plan".into(),
            ask: "default".into(),
            accept_edits: "acceptEdits".into(),
            auto: "auto".into(),
            full_access: "bypassPermissions".into(),
        }
    }
}

impl ModeMap {
    /// What is asked of the adapter for `mode`: its id, and for Plan the Ask id as a stand-in.
    pub fn choice(&self, mode: PermissionMode) -> crate::session::ModeChoice {
        self.choice_with(mode, false, None)
    }

    /// [`Self::choice`] with adapter-level mode properties (`blind_set_mode`, `mode_meta`).
    pub fn choice_with(
        &self,
        mode: PermissionMode,
        blind: bool,
        mode_meta: Option<&BTreeMap<String, Value>>,
    ) -> crate::session::ModeChoice {
        let narrow = matches!(mode, PermissionMode::Plan | PermissionMode::Ask);
        let key = match mode {
            PermissionMode::Plan => "plan",
            PermissionMode::Ask => "ask",
            PermissionMode::AcceptEdits => "accept_edits",
            PermissionMode::Auto => "auto",
            PermissionMode::FullAccess => "full_access",
        };
        let meta = mode_meta.and_then(|m| m.get(key)).cloned();
        crate::session::ModeChoice {
            requested: key,
            id: self.id(mode).to_owned(),
            fallback: (mode == PermissionMode::Plan).then(|| self.ask.clone()),
            narrow,
            blind,
            meta,
        }
    }

    /// Steve's modes an adapter offering `ids` can be set to (as [`Self::choice`] resolves them);
    /// `None` (it reports no modes): Plan and Ask only.
    pub fn offered(&self, ids: Option<&[String]>) -> Vec<PermissionMode> {
        self.offered_with(ids, false, None)
    }

    /// [`Self::offered`] with adapter-level mode properties (`blind_set_mode`, `mode_meta`).
    pub fn offered_with(
        &self,
        ids: Option<&[String]>,
        blind: bool,
        mode_meta: Option<&BTreeMap<String, Value>>,
    ) -> Vec<PermissionMode> {
        [
            PermissionMode::Plan,
            PermissionMode::Ask,
            PermissionMode::AcceptEdits,
            PermissionMode::Auto,
            PermissionMode::FullAccess,
        ]
        .into_iter()
        .filter(|m| self.choice_with(*m, blind, mode_meta).resolve(ids).is_ok())
        .collect()
    }

    /// The adapter's mode id for `mode`.
    pub fn id(&self, mode: PermissionMode) -> &str {
        match mode {
            PermissionMode::Plan => &self.plan,
            PermissionMode::Ask => &self.ask,
            PermissionMode::AcceptEdits => &self.accept_edits,
            PermissionMode::Auto => &self.auto,
            PermissionMode::FullAccess => &self.full_access,
        }
    }
}

/// A harness's own directory (path model §4.5.1).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateDir {
    /// The variable that moves it (empty when `xdg` alone moves the harness's state).
    #[serde(default)]
    pub env: String,
    /// Where it is under the user's home when the variable is not set.
    pub default: String,
    /// What of the Node owner's own directory the owner's state starts with (their own settings,
    /// never anyone else's; a login only where a copy cannot sign the original out).
    #[serde(default)]
    pub import: Vec<String>,
    /// True when `env` sets a virtual `$HOME` rather than the dot-directory itself: `env` receives
    /// the per-principal state directory, while files from `default` are imported into
    /// `<state>/<default>/` (e.g. `GEMINI_CLI_HOME` -> `<state>/.gemini/`, `FACTORY_HOME_OVERRIDE`
    /// -> `<state>/.factory/`).
    #[serde(default, skip_serializing_if = "is_false")]
    pub home_override: bool,
    /// Subdirectory under `<state>` into which `import` files from `~/<default>` are copied when
    /// `env` adds a subdirectory layer of its own (e.g. `GOOSE_PATH_ROOT` reads `<state>/config/`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub import_into: Option<String>,
    /// Set `XDG_DATA_HOME`, `XDG_CONFIG_HOME`, `XDG_STATE_HOME`, and `XDG_CACHE_HOME` under the
    /// per-principal state directory so XDG-based CLIs stay isolated per principal.
    #[serde(default, skip_serializing_if = "is_false")]
    pub xdg: bool,
    /// Extra home-relative paths the harness reads/writes in ambient mode or that sandbox policy
    /// must protect for non-owner principals.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_dirs: Vec<String>,
    /// Home-relative files (not directories) the harness keeps next to its directory, such as
    /// Claude Code's `.claude.json`: written in ambient mode, hidden from non-owner principals.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub home_files: Vec<String>,
}

fn absolutize_env_path(p: PathBuf, user_home: &Path) -> PathBuf {
    if p.is_absolute() {
        p
    } else {
        std::path::absolute(&p).unwrap_or_else(|_| user_home.join(p))
    }
}

impl StateDir {
    /// Where the owner's ambient configuration files live when importing into a per-principal
    /// state directory.
    pub fn owner_source(&self, user_home: &Path) -> Option<PathBuf> {
        if !self.env.is_empty()
            && let Some(val) = std::env::var_os(&self.env).filter(|v| !v.is_empty())
        {
            let p = absolutize_env_path(PathBuf::from(val), user_home);
            return Some(if self.home_override && !self.default.is_empty() {
                p.join(&self.default)
            } else if let Some(sub) = self.import_into.as_deref().filter(|s| !s.is_empty()) {
                p.join(sub)
            } else {
                p
            });
        }
        (!self.default.is_empty()).then(|| user_home.join(&self.default))
    }

    /// Directories the harness writes in ambient (non-isolated) mode—used by read-only sandboxes
    /// so the adapter can write its own session/config state without ever opening bare `$HOME`.
    pub fn ambient_dirs(&self, user_home: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        if !self.env.is_empty()
            && let Some(val) = std::env::var_os(&self.env).filter(|v| !v.is_empty())
        {
            let p = absolutize_env_path(PathBuf::from(val), user_home);
            if self.home_override {
                if !self.default.is_empty() {
                    out.push(p.join(&self.default));
                }
            } else {
                out.push(p);
            }
        } else if !self.default.is_empty() {
            out.push(user_home.join(&self.default));
        }
        if self.xdg {
            for var in [
                "XDG_CONFIG_HOME",
                "XDG_DATA_HOME",
                "XDG_STATE_HOME",
                "XDG_CACHE_HOME",
            ] {
                if let Some(val) = std::env::var_os(var).filter(|v| !v.is_empty()) {
                    let base = absolutize_env_path(PathBuf::from(val), user_home);
                    if let Some(leaf) = Path::new(&self.default)
                        .file_name()
                        .filter(|n| !n.is_empty())
                    {
                        out.push(base.join(leaf));
                    }
                }
            }
        }
        for extra in &self.extra_dirs {
            if !extra.is_empty() {
                out.push(user_home.join(extra));
            }
        }
        out
    }

    /// The harness's places under the user's home where nothing moves them: its `default`
    /// directory and `extra_dirs` (first), and its `home_files` (second), all home-relative.
    pub fn home_places(&self) -> (Vec<&str>, Vec<&str>) {
        let dirs = std::iter::once(self.default.as_str())
            .chain(self.extra_dirs.iter().map(String::as_str))
            .filter(|d| !d.is_empty() && !Path::new(d).is_absolute())
            .collect();
        let files = self
            .home_files
            .iter()
            .map(String::as_str)
            .filter(|f| !f.is_empty() && !Path::new(f).is_absolute())
            .collect();
        (dirs, files)
    }

    /// Environment variables that move this harness's state into `dir`.
    pub fn isolation_env(&self, dir: &Path) -> Vec<(OsString, OsString)> {
        self.isolation_env_for(dir, None, false)
    }

    /// Environment variables that move this harness's state into `dir`, preserving the owner's
    /// XDG git config when `XDG_CONFIG_HOME` is redirected for an owner session.
    pub fn isolation_env_for(
        &self,
        dir: &Path,
        user_home: Option<&Path>,
        import_owner: bool,
    ) -> Vec<(OsString, OsString)> {
        let mut out = Vec::new();
        if !self.env.is_empty() {
            out.push((self.env.clone().into(), dir.as_os_str().to_owned()));
        }
        if self.xdg {
            if import_owner
                && std::env::var_os("GIT_CONFIG_GLOBAL").is_none_or(|v| v.is_empty())
                && let Some(home) = user_home
                && !home.join(".gitconfig").exists()
            {
                let xdg_cfg = std::env::var_os("XDG_CONFIG_HOME")
                    .filter(|v| !v.is_empty())
                    .map(|v| absolutize_env_path(PathBuf::from(v), home))
                    .unwrap_or_else(|| home.join(".config"));
                let git_cfg = xdg_cfg.join("git/config");
                if git_cfg.is_file() {
                    out.push(("GIT_CONFIG_GLOBAL".into(), git_cfg.into_os_string()));
                }
            }
            for (var, sub) in [
                ("XDG_DATA_HOME", "xdg/data"),
                ("XDG_CONFIG_HOME", "xdg/config"),
                ("XDG_STATE_HOME", "xdg/state"),
                ("XDG_CACHE_HOME", "xdg/cache"),
            ] {
                out.push((var.into(), dir.join(sub).into_os_string()));
            }
        }
        out
    }
}

fn is_false(b: &bool) -> bool {
    !*b
}

/// What `session/new|load` carries in `_meta` for the adapter.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionMeta {
    #[default]
    None,
    /// claude-agent-acp's `claudeCode.options.settings`: its file tools denied Steve's secret
    /// files in every permission mode.
    ClaudeSettings,
}

/// Arguments the adapter is started with beyond its declared `args`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LaunchExtra {
    #[default]
    None,
}

/// Host integration properties of an adapter. Callers select policy by these, never by a harness
/// name. `self_read_only` applies only after selecting its ACP read-only mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterProperties {
    /// Its read-only mode is enforced by the harness's own sandbox.
    pub self_read_only: bool,
    /// It runs inside the Node's OS sandbox (macOS cannot nest a harness's own one in it).
    pub nested_sandbox: bool,
    /// Where the harness keeps its configuration, logins and history, when it can be moved: what
    /// gives each principal its own (`None`: it cannot, and every run uses the Node's).
    pub state: Option<StateDir>,
    pub modes: ModeMap,
    /// Send `session/set_mode` even when `session/new|load` reports no `availableModes`, and offer
    /// modes with non-empty ids (or `mode_meta` entries) without an `availableModes` list.
    pub blind_set_mode: bool,
    /// Per-Steve-mode (`plan`, `ask`, `accept_edits`, `auto`, `full_access`) `_meta` sent on
    /// `session/new|load` (for adapters whose higher autonomy modes are session `_meta` flags).
    pub mode_meta: BTreeMap<String, Value>,
    /// The variable that receives the configured harness executable (`claude_executable`).
    pub executable_env: Option<String>,
    pub session_meta: SessionMeta,
    pub launch_extra: LaunchExtra,
    /// Discovery asks it `initialize` and opens a scratch session ([`probe`]), at most every half
    /// hour.
    pub probe_initialize: bool,
    /// What `session/new|load` adds to its `_meta` to switch the harness's built-in tools off for
    /// a call that uses none (a model call of Steve's own: cheaper, and nothing to guard).
    pub tools_off_meta: Option<Value>,
    /// Its `inputTokens` include the cache reads (OpenAI's convention).
    pub usage_input_includes_cache: bool,
    /// Its `PromptResponse.usage` reports cumulative session token totals rather than per-turn
    /// deltas (`mistral-vibe`), so multi-turn sessions subtract the prior session high-water mark.
    pub usage_prompt_cumulative: bool,
    /// Where a call's tokens come from when the adapter reports none.
    pub usage_fallback: UsageFallback,
    /// The models to offer before (or without) a probe of the adapter.
    pub fallback_models: Vec<String>,
    /// The reasoning efforts to offer before (or without) a probe of the adapter.
    pub fallback_efforts: Vec<String>,
    /// Steve's judgments fall back down Steve's model lists (`STEVE_MODELS_SMART`) after the bot's
    /// model; without it they try the bot's model, else the adapter's own default.
    pub model_fallback: bool,
    /// Words that name its quick, cheap models, best first (`haiku`): of the models the adapter
    /// itself offers, the newest whose name has one as a whole word is what Steve's cheap, frequent
    /// judgments (routing, titles) are put on once. None: they follow Steve's model like everything
    /// else.
    pub fast_models: Vec<String>,
    /// Where it reads skills (`<name>/SKILL.md`, a Markdown file with `name` and `description` in
    /// its front matter): a directory under its own state directory ([`Self::state`]). A Bot's
    /// skills are put there before its session opens. `None`: it reads none Steve can give it.
    pub skills: Option<String>,
    /// Command template to resume a session in a terminal (`{id}` is replaced by the quoted ACP
    /// session id, e.g. `"claude --resume {id}"`).
    pub resume_cli: Option<String>,
    /// Command template to fork a session in a terminal (`{id}` is replaced by the quoted ACP
    /// session id, e.g. `"claude --resume {id} --fork-session"`).
    pub fork_cli: Option<String>,
    /// True when the Node knows how to locate and inspect this harness's on-disk session
    /// transcript files (`steve_node::history`).
    pub local_transcript: bool,
    /// Extra CLI arguments appended when the adapter runs inside Steve's outer OS sandbox
    /// (for example `--no-sandbox` so Copilot CLI disables its nested inner sandbox rather than
    /// aborting inside Seatbelt or Bubblewrap).
    pub outer_sandbox_args: Vec<String>,
}

/// Where a call's tokens are read when the adapter does not report them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageFallback {
    #[default]
    None,
    /// Codex's rollout file of the session (`$CODEX_HOME/sessions`).
    CodexRollout,
}

/// What an adapter nobody described gets: assume the worst (it reads the disk itself, so it runs
/// in the Node's sandbox), plain ACP for everything else.
impl Default for AdapterProperties {
    fn default() -> Self {
        Self {
            self_read_only: false,
            nested_sandbox: true,
            state: None,
            modes: ModeMap::default(),
            blind_set_mode: false,
            mode_meta: BTreeMap::new(),
            executable_env: None,
            session_meta: SessionMeta::None,
            launch_extra: LaunchExtra::None,
            probe_initialize: true,
            tools_off_meta: None,
            usage_input_includes_cache: false,
            usage_prompt_cumulative: false,
            usage_fallback: UsageFallback::None,
            fallback_models: vec![],
            fallback_efforts: vec![],
            model_fallback: false,
            fast_models: vec![],
            skills: None,
            resume_cli: None,
            fork_cli: None,
            local_transcript: false,
            outer_sandbox_args: vec![],
        }
    }
}

/// The keys of [`PropertiesPatch`] (what a manifest's `properties` may hold).
pub const PROPERTY_KEYS: [&str; 22] = [
    "self_read_only",
    "nested_sandbox",
    "state",
    "modes",
    "blind_set_mode",
    "mode_meta",
    "executable_env",
    "session_meta",
    "launch_extra",
    "probe_initialize",
    "tools_off_meta",
    "usage_input_includes_cache",
    "usage_prompt_cumulative",
    "usage_fallback",
    "fallback_models",
    "fallback_efforts",
    "model_fallback",
    "fast_models",
    "skills",
    "resume_cli",
    "fork_cli",
    "outer_sandbox_args",
];

/// Declared properties, each optional: what is given replaces the built-in (or default) value.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PropertiesPatch {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub self_read_only: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nested_sandbox: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub state: Option<StateDir>,
    /// Some of Steve's modes (`plan`, `ask`, `accept_edits`, `auto`, `full_access`) → mode ids.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub modes: BTreeMap<String, String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blind_set_mode: Option<bool>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub mode_meta: BTreeMap<String, Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub executable_env: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_meta: Option<SessionMeta>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_extra: Option<LaunchExtra>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub probe_initialize: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tools_off_meta: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_input_includes_cache: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_prompt_cumulative: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage_fallback: Option<UsageFallback>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_models: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_efforts: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_fallback: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fast_models: Option<Vec<String>>,
    /// `""`: it reads none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skills: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub resume_cli: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fork_cli: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub outer_sandbox_args: Option<Vec<String>>,
}

impl PropertiesPatch {
    fn apply(&self, p: &mut AdapterProperties) {
        if let Some(v) = self.self_read_only {
            p.self_read_only = v;
        }
        if let Some(v) = self.nested_sandbox {
            p.nested_sandbox = v;
        }
        if let Some(v) = &self.state {
            p.state = Some(v.clone());
        }
        for (k, v) in &self.modes {
            let slot = match k.as_str() {
                "plan" => &mut p.modes.plan,
                "ask" => &mut p.modes.ask,
                "accept_edits" => &mut p.modes.accept_edits,
                "auto" => &mut p.modes.auto,
                "full_access" => &mut p.modes.full_access,
                other => {
                    tracing::warn!("harness adapter: unknown mode {other} in its declared modes");
                    continue;
                }
            };
            *slot = v.clone();
        }
        if let Some(v) = self.blind_set_mode {
            p.blind_set_mode = v;
        }
        for (k, v) in &self.mode_meta {
            if v.is_null() {
                p.mode_meta.remove(k);
            } else {
                p.mode_meta.insert(k.clone(), v.clone());
            }
        }
        if let Some(v) = &self.executable_env {
            p.executable_env = Some(v.clone()).filter(|v| !v.is_empty());
        }
        if let Some(v) = self.session_meta {
            p.session_meta = v;
        }
        if let Some(v) = self.launch_extra {
            p.launch_extra = v;
        }
        if let Some(v) = self.probe_initialize {
            p.probe_initialize = v;
        }
        if let Some(v) = &self.tools_off_meta {
            p.tools_off_meta = Some(v.clone()).filter(|v| !v.is_null());
        }
        if let Some(v) = self.usage_input_includes_cache {
            p.usage_input_includes_cache = v;
        }
        if let Some(v) = self.usage_prompt_cumulative {
            p.usage_prompt_cumulative = v;
        }
        if let Some(v) = self.usage_fallback {
            p.usage_fallback = v;
        }
        if let Some(v) = &self.fallback_models {
            p.fallback_models = v.clone();
        }
        if let Some(v) = &self.fallback_efforts {
            p.fallback_efforts = v.clone();
        }
        if let Some(v) = self.model_fallback {
            p.model_fallback = v;
        }
        if let Some(v) = &self.fast_models {
            p.fast_models = v.clone();
        }
        if let Some(v) = &self.skills {
            p.skills = Some(v.clone()).filter(|v| !v.trim().is_empty());
        }
        if let Some(v) = &self.resume_cli {
            p.resume_cli = Some(v.clone()).filter(|v| !v.trim().is_empty());
        }
        if let Some(v) = &self.fork_cli {
            p.fork_cli = Some(v.clone()).filter(|v| !v.trim().is_empty());
        }
        if let Some(v) = &self.outer_sandbox_args {
            p.outer_sandbox_args = v.clone();
        }
    }
}

/// What a manifest declares about an adapter besides its command.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Declaration {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub args: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub properties: PropertiesPatch,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub install_hint: Option<String>,
}

/// One manifest entry: a command (as `harness.json` always had it) or a declaration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
#[allow(clippy::large_enum_variant)] // a configuration value, read once
pub enum AdapterEntry {
    Command(PathBuf),
    Declared {
        command: PathBuf,
        #[serde(flatten)]
        declaration: Declaration,
    },
}

/// A manifest's `adapters` as commands and declarations.
pub fn split_entries(
    entries: impl IntoIterator<Item = (String, AdapterEntry)>,
) -> (HashMap<String, PathBuf>, HashMap<String, Declaration>) {
    let mut commands = HashMap::new();
    let mut declared = HashMap::new();
    for (id, e) in entries {
        match e {
            AdapterEntry::Command(c) => {
                commands.insert(id, c);
            }
            AdapterEntry::Declared {
                command,
                declaration,
            } => {
                commands.insert(id.clone(), command);
                declared.insert(id, declaration);
            }
        }
    }
    (commands, declared)
}

/// Everything known about one adapter: its built-in data, then what was declared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdapterSpec {
    pub id: String,
    pub name: String,
    /// One of Steve's built-in entries.
    pub builtin: bool,
    pub properties: AdapterProperties,
    /// How to install it, for people.
    pub install_hint: Option<String>,
}

struct Builtin {
    id: &'static str,
    name: &'static str,
    install_hint: &'static str,
    /// The npm package Steve manages under `<steve-home>/adapters`, when distributed via npm.
    npm_package: Option<&'static str>,
    /// The binary name a Node or Hub looks up on `PATH`.
    cli_bin: &'static str,
    /// Additional CLI binaries the user may have installed on `PATH` (for setup auto-detection).
    detect_bins: &'static [&'static str],
    /// Arguments passed to launch ACP mode when not overridden by a manifest's `args`.
    args: &'static [&'static str],
    /// Environment variables passed when starting the adapter, before any declared `env`.
    env: &'static [(&'static str, &'static str)],
    /// The variable that names its command (`STEVE_ADAPTER_*`), and where it is installed by
    /// default under `npm root -g` (or `cli_bin` for PATH-only adapters).
    command_var: &'static str,
    default_command: fn(&Path) -> PathBuf,
    properties: fn() -> AdapterProperties,
}

fn npm_entry(pkg: &str, entry: &str, root: &Path) -> PathBuf {
    root.join(pkg).join(entry)
}

fn npm_bin(pkg: &str, bin: &str, root: &Path) -> PathBuf {
    let pkg_dir = root.join(pkg);
    if let Ok(raw) = std::fs::read(pkg_dir.join("package.json"))
        && let Ok(manifest) = serde_json::from_slice::<Value>(&raw)
    {
        let entry = match manifest.get("bin") {
            Some(Value::String(s)) => Some(s.as_str()),
            Some(Value::Object(map)) => map.get(bin).and_then(Value::as_str).or_else(|| {
                (map.len() == 1)
                    .then(|| map.values().next()?.as_str())
                    .flatten()
            }),
            _ => None,
        };
        if let Some(rel) = entry
            .map(|s| s.trim_start_matches("./"))
            .filter(|s| !s.is_empty())
        {
            let candidate = pkg_dir.join(rel);
            if candidate.is_file() {
                return candidate;
            }
        }
    }
    #[cfg(windows)]
    let name = format!("{bin}.cmd");
    #[cfg(not(windows))]
    let name = bin;
    let local_bin = root.join(".bin").join(&name);
    if local_bin.exists() {
        return local_bin;
    }
    if root.parent().and_then(|p| p.file_name()) != Some(std::ffi::OsStr::new("adapters"))
        && let Some(parent) = root.parent()
    {
        #[cfg(windows)]
        let global_bin = parent.join(&name);
        #[cfg(not(windows))]
        let global_bin = parent.parent().unwrap_or(parent).join("bin").join(&name);
        if global_bin.exists() {
            return global_bin;
        }
    }
    local_bin
}

fn default_gemini_adapter(npm_root: &Path) -> PathBuf {
    npm_entry("@google/gemini-cli", "bundle/gemini.js", npm_root)
}

fn default_copilot_adapter(npm_root: &Path) -> PathBuf {
    npm_entry("@github/copilot", "npm-loader.js", npm_root)
}

fn default_opencode_adapter(npm_root: &Path) -> PathBuf {
    #[cfg(windows)]
    let entry = "bin/opencode.exe";
    #[cfg(not(windows))]
    let entry = "bin/opencode";
    npm_entry("@opencode/cli", entry, npm_root)
}

fn default_qwen_adapter(npm_root: &Path) -> PathBuf {
    npm_entry("@qwen-code/qwen-code", "cli-entry.js", npm_root)
}

fn default_goose_adapter(npm_root: &Path) -> PathBuf {
    npm_entry("@aaif/goose-acp", "bin/goose.mjs", npm_root)
}

fn default_kimi_adapter(npm_root: &Path) -> PathBuf {
    npm_entry("@moonshot-ai/kimi-code", "dist/main.mjs", npm_root)
}

fn default_grok_adapter(npm_root: &Path) -> PathBuf {
    npm_bin("@xai-official/grok", "grok", npm_root)
}

fn default_kilo_adapter(npm_root: &Path) -> PathBuf {
    npm_bin("@kilocode/cli", "kilo", npm_root)
}

fn default_droid_adapter(npm_root: &Path) -> PathBuf {
    npm_bin("droid", "droid", npm_root)
}

fn default_codebuddy_adapter(npm_root: &Path) -> PathBuf {
    npm_bin("@tencent-ai/codebuddy-code", "codebuddy", npm_root)
}

fn path_cursor_adapter(_: &Path) -> PathBuf {
    PathBuf::from("cursor-agent")
}

fn path_kiro_adapter(_: &Path) -> PathBuf {
    PathBuf::from("kiro-cli")
}

fn path_vibe_adapter(_: &Path) -> PathBuf {
    PathBuf::from("vibe-acp")
}

fn path_qoder_adapter(_: &Path) -> PathBuf {
    PathBuf::from("qodercli")
}

const BUILTINS: &[Builtin] = &[
    Builtin {
        id: "claude-code",
        name: "Claude Code",
        install_hint: "@agentclientprotocol/claude-agent-acp",
        npm_package: Some("@agentclientprotocol/claude-agent-acp"),
        cli_bin: "claude-agent-acp",
        detect_bins: &["claude", "claude-code-acp"],
        args: &[],
        env: &[],
        command_var: "STEVE_ADAPTER_CLAUDE",
        default_command: crate::config::default_claude_adapter,
        properties: || AdapterProperties {
            self_read_only: false,
            nested_sandbox: true,
            state: Some(StateDir {
                env: "CLAUDE_CONFIG_DIR".into(),
                default: ".claude".into(),
                import: vec!["settings.json".into()],
                home_files: strings(&[".claude.json"]),
                ..Default::default()
            }),
            modes: ModeMap::default(),
            executable_env: Some("CLAUDE_CODE_EXECUTABLE".into()),
            session_meta: SessionMeta::ClaudeSettings,
            launch_extra: LaunchExtra::None,
            probe_initialize: true,
            // claude-agent-acp: `claudeCode.options.tools` is the list of built-in tools
            tools_off_meta: Some(json!({"claudeCode": {"options": {"tools": []}}})),
            usage_input_includes_cache: false,
            usage_fallback: UsageFallback::None,
            fallback_models: strings(&["claude-opus-5-5", "claude-sonnet-5-5", "claude-haiku-5-5"]),
            fallback_efforts: strings(&["low", "medium", "high", "xhigh", "max"]),
            model_fallback: true,
            fast_models: strings(&["haiku"]),
            // its user skills: `$CLAUDE_CONFIG_DIR/skills/<name>/SKILL.md`
            skills: Some("skills".into()),
            resume_cli: Some("claude --resume {id}".into()),
            fork_cli: Some("claude --resume {id} --fork-session".into()),
            local_transcript: true,
            ..Default::default()
        },
    },
    Builtin {
        id: "codex",
        name: "Codex",
        install_hint: "@agentclientprotocol/codex-acp",
        npm_package: Some("@agentclientprotocol/codex-acp"),
        cli_bin: "codex-acp",
        detect_bins: &["codex"],
        args: &[],
        env: &[],
        command_var: "STEVE_ADAPTER_CODEX",
        default_command: crate::config::default_codex_adapter,
        // Codex's own default may be full access, so the mode is always set: read-only asks before
        // every edit or network access; workspace-write edits the workspace and asks before
        // writing outside it or the network; agent asks only for what it judges unsafe (Steve's
        // auto); agent-full-access only for FullAccess.
        properties: || AdapterProperties {
            self_read_only: true,
            nested_sandbox: false,
            state: Some(StateDir {
                env: "CODEX_HOME".into(),
                default: ".codex".into(),
                // not its login: copies refreshing on their own would sign the owner's out
                import: vec!["config.toml".into()],
                ..Default::default()
            }),
            modes: ModeMap {
                plan: "read-only".into(),
                ask: "read-only".into(),
                accept_edits: "workspace-write".into(),
                auto: "agent".into(),
                full_access: "agent-full-access".into(),
            },
            executable_env: None,
            // none yet: codex-acp's `session/set_mode` replaces the permission profile with the
            // mode's built-in one, so deny entries do not survive it; and once a profile has
            // deny-read entries, codex-core keeps a command the user approved to run outside the
            // sandbox inside it, so Ask mode could no longer write anything
            session_meta: SessionMeta::None,
            launch_extra: LaunchExtra::None,
            probe_initialize: true,
            tools_off_meta: None,
            // codex-acp reports a turn's tokens (OpenAI's way: input includes the cached part);
            // its rollout file stands in when a turn's report is missing
            usage_input_includes_cache: true,
            usage_fallback: UsageFallback::CodexRollout,
            // what a Codex probe lists, so its bots can pick a model before (or without) a probe,
            // as Claude Code's can
            fallback_models: strings(&["gpt-6.1-sol", "gpt-6-sol", "gpt-6-luna", "gpt-5.6-luna"]),
            fallback_efforts: strings(&["low", "medium", "high", "xhigh", "max", "ultra"]),
            model_fallback: false,
            // what Codex calls its quick tier now, then what it called it before
            fast_models: strings(&["luna", "mini"]),
            // its user skills: `$CODEX_HOME/skills/<name>/SKILL.md`
            skills: Some("skills".into()),
            resume_cli: Some("codex resume {id}".into()),
            fork_cli: Some("codex fork {id}".into()),
            local_transcript: true,
            ..Default::default()
        },
    },
    Builtin {
        id: "gemini",
        name: "Gemini CLI",
        install_hint: "@google/gemini-cli",
        npm_package: Some("@google/gemini-cli"),
        cli_bin: "gemini",
        detect_bins: &[],
        args: &["--acp"],
        env: &[
            ("GEMINI_CLI_TRUST_WORKSPACE", "true"),
            ("GEMINI_CLI_NO_RELAUNCH", "true"),
        ],
        command_var: "STEVE_ADAPTER_GEMINI",
        default_command: default_gemini_adapter,
        properties: || AdapterProperties {
            state: Some(StateDir {
                env: "GEMINI_CLI_HOME".into(),
                default: ".gemini".into(),
                import: vec!["settings.json".into()],
                home_override: true,
                ..Default::default()
            }),
            modes: ModeMap {
                plan: "plan".into(),
                ask: "default".into(),
                accept_edits: "autoEdit".into(),
                auto: String::new(),
                full_access: "yolo".into(),
            },
            usage_input_includes_cache: true,
            fast_models: strings(&["flash-lite", "flash"]),
            resume_cli: Some("gemini --resume {id}".into()),
            ..Default::default()
        },
    },
    Builtin {
        id: "github-copilot-cli",
        name: "GitHub Copilot CLI",
        install_hint: "@github/copilot",
        npm_package: Some("@github/copilot"),
        cli_bin: "copilot",
        detect_bins: &[],
        args: &["--acp", "--no-auto-update"],
        env: &[("COPILOT_AUTO_UPDATE", "false")],
        command_var: "STEVE_ADAPTER_COPILOT",
        default_command: default_copilot_adapter,
        properties: || AdapterProperties {
            state: Some(StateDir {
                env: "COPILOT_HOME".into(),
                default: ".copilot".into(),
                import: vec!["settings.json".into()],
                extra_dirs: strings(&[
                    ".config/github-copilot",
                    "Library/Application Support/github-copilot",
                ]),
                ..Default::default()
            }),
            modes: ModeMap {
                plan: "https://agentclientprotocol.com/protocol/session-modes#plan".into(),
                ask: "https://agentclientprotocol.com/protocol/session-modes#agent".into(),
                accept_edits: String::new(),
                auto: String::new(),
                full_access: "https://agentclientprotocol.com/protocol/session-modes#autopilot"
                    .into(),
            },
            fast_models: strings(&["haiku", "flash", "mini"]),
            resume_cli: Some("copilot --resume={id}".into()),
            outer_sandbox_args: strings(&["--no-sandbox"]),
            ..Default::default()
        },
    },
    Builtin {
        id: "opencode",
        name: "OpenCode",
        install_hint: "@opencode/cli",
        npm_package: Some("@opencode/cli"),
        cli_bin: "opencode",
        detect_bins: &[],
        args: &["acp"],
        env: &[("OPENCODE_DISABLE_AUTOUPDATE", "1")],
        command_var: "STEVE_ADAPTER_OPENCODE",
        default_command: default_opencode_adapter,
        properties: || AdapterProperties {
            state: Some(StateDir {
                env: String::new(),
                default: ".local/share/opencode".into(),
                xdg: true,
                extra_dirs: strings(&[
                    ".config/opencode",
                    ".local/state/opencode",
                    ".cache/opencode",
                    ".opencode",
                ]),
                ..Default::default()
            }),
            // OpenCode's `build` agent mode delegates tool permission prompts over ACP
            // `session/request_permission`, where Steve's permission handler enforces Ask /
            // AcceptEdits / Auto / FullAccess; `plan` restricts OpenCode to read-only planning.
            modes: ModeMap {
                plan: "plan".into(),
                ask: "build".into(),
                accept_edits: "build".into(),
                auto: "build".into(),
                full_access: "build".into(),
            },
            fast_models: strings(&["haiku", "mini", "flash", "nano", "lite"]),
            resume_cli: Some("opencode -s {id}".into()),
            fork_cli: Some("opencode -s {id} --fork".into()),
            ..Default::default()
        },
    },
    Builtin {
        id: "qwen-code",
        name: "Qwen Code",
        install_hint: "@qwen-code/qwen-code",
        npm_package: Some("@qwen-code/qwen-code"),
        cli_bin: "qwen",
        detect_bins: &[],
        args: &["--acp"],
        env: &[("QWEN_CODE_NO_RELAUNCH", "true")],
        command_var: "STEVE_ADAPTER_QWEN",
        default_command: default_qwen_adapter,
        properties: || AdapterProperties {
            state: Some(StateDir {
                env: "QWEN_HOME".into(),
                default: ".qwen".into(),
                import: vec!["settings.json".into()],
                ..Default::default()
            }),
            modes: ModeMap {
                plan: "plan".into(),
                ask: "default".into(),
                accept_edits: "auto-edit".into(),
                auto: "auto".into(),
                full_access: "yolo".into(),
            },
            usage_input_includes_cache: true,
            fast_models: strings(&["flash", "turbo", "mini", "haiku", "lite"]),
            resume_cli: Some("qwen --resume {id}".into()),
            ..Default::default()
        },
    },
    Builtin {
        id: "goose",
        name: "goose",
        install_hint: "@aaif/goose-acp",
        npm_package: Some("@aaif/goose-acp"),
        cli_bin: "goose",
        detect_bins: &[],
        args: &["acp"],
        env: &[("GOOSE_DISABLE_KEYRING", "1")],
        command_var: "STEVE_ADAPTER_GOOSE",
        default_command: default_goose_adapter,
        properties: || AdapterProperties {
            state: Some(StateDir {
                env: "GOOSE_PATH_ROOT".into(),
                default: ".config/goose".into(),
                import: vec!["config.yaml".into()],
                import_into: Some("config".into()),
                extra_dirs: strings(&[
                    ".local/share/goose",
                    ".local/state/goose",
                    "Library/Application Support/goose",
                ]),
                ..Default::default()
            }),
            modes: ModeMap {
                plan: String::new(),
                ask: "approve".into(),
                accept_edits: String::new(),
                auto: "smart_approve".into(),
                full_access: "auto".into(),
            },
            usage_input_includes_cache: true,
            fast_models: strings(&["haiku", "mini", "flash", "nano", "lite"]),
            resume_cli: Some("goose session --resume --session-id {id}".into()),
            ..Default::default()
        },
    },
    Builtin {
        id: "kimi-code",
        name: "Kimi Code",
        install_hint: "@moonshot-ai/kimi-code",
        npm_package: Some("@moonshot-ai/kimi-code"),
        cli_bin: "kimi",
        detect_bins: &[],
        args: &["acp"],
        env: &[("KIMI_CODE_NO_AUTO_UPDATE", "1")],
        command_var: "STEVE_ADAPTER_KIMI",
        default_command: default_kimi_adapter,
        properties: || AdapterProperties {
            state: Some(StateDir {
                env: "KIMI_CODE_HOME".into(),
                default: ".kimi-code".into(),
                import: strings(&["config.toml", "tui.toml"]),
                extra_dirs: strings(&[".kimi"]),
                ..Default::default()
            }),
            modes: ModeMap {
                plan: "plan".into(),
                ask: "default".into(),
                accept_edits: String::new(),
                auto: "yolo".into(),
                full_access: "auto".into(),
            },
            resume_cli: Some("kimi --session {id}".into()),
            ..Default::default()
        },
    },
    Builtin {
        id: "grok-build",
        name: "Grok Build",
        install_hint: "@xai-official/grok",
        npm_package: Some("@xai-official/grok"),
        cli_bin: "grok",
        detect_bins: &[],
        args: &["agent", "--no-leader", "stdio"],
        env: &[("GROK_DISABLE_AUTOUPDATER", "1")],
        command_var: "STEVE_ADAPTER_GROK",
        default_command: default_grok_adapter,
        properties: || AdapterProperties {
            state: Some(StateDir {
                env: "GROK_HOME".into(),
                default: ".grok".into(),
                import: vec!["config.toml".into()],
                ..Default::default()
            }),
            modes: ModeMap {
                plan: "plan".into(),
                ask: "default".into(),
                accept_edits: String::new(),
                auto: String::new(),
                full_access: String::new(),
            },
            blind_set_mode: true,
            mode_meta: BTreeMap::from([
                ("auto".into(), json!({"autoMode": true})),
                ("full_access".into(), json!({"yoloMode": true})),
            ]),
            usage_input_includes_cache: true,
            resume_cli: Some("grok --resume {id}".into()),
            ..Default::default()
        },
    },
    Builtin {
        id: "kilo",
        name: "Kilo Code",
        install_hint: "@kilocode/cli",
        npm_package: Some("@kilocode/cli"),
        cli_bin: "kilo",
        detect_bins: &[],
        args: &["acp"],
        env: &[("KILO_DISABLE_AUTOUPDATE", "1")],
        command_var: "STEVE_ADAPTER_KILO",
        default_command: default_kilo_adapter,
        properties: || AdapterProperties {
            state: Some(StateDir {
                env: String::new(),
                default: ".local/share/kilo".into(),
                xdg: true,
                extra_dirs: strings(&[
                    ".config/kilo",
                    ".cache/kilo",
                    ".local/state/kilo",
                    ".kilo",
                    ".kilocode",
                ]),
                ..Default::default()
            }),
            modes: ModeMap {
                plan: "plan".into(),
                ask: "ask".into(),
                accept_edits: "code".into(),
                auto: String::new(),
                full_access: String::new(),
            },
            fast_models: strings(&["haiku", "mini", "flash", "nano", "lite", "small", "free"]),
            resume_cli: Some("kilo --session {id}".into()),
            ..Default::default()
        },
    },
    Builtin {
        id: "factory-droid",
        name: "Factory Droid",
        install_hint: "droid",
        npm_package: Some("droid"),
        cli_bin: "droid",
        detect_bins: &[],
        args: &["exec", "--output-format", "acp"],
        env: &[
            ("DROID_DISABLE_AUTO_UPDATE", "true"),
            ("FACTORY_DROID_AUTO_UPDATE_ENABLED", "false"),
        ],
        command_var: "STEVE_ADAPTER_DROID",
        default_command: default_droid_adapter,
        properties: || AdapterProperties {
            state: Some(StateDir {
                env: "FACTORY_HOME_OVERRIDE".into(),
                default: ".factory".into(),
                import: vec!["settings.json".into()],
                home_override: true,
                ..Default::default()
            }),
            modes: ModeMap {
                plan: "spec".into(),
                ask: "normal".into(),
                accept_edits: "auto-low".into(),
                auto: "auto-medium".into(),
                full_access: "auto-high".into(),
            },
            fast_models: strings(&["haiku", "luna", "flash", "mini"]),
            resume_cli: Some("droid --resume {id}".into()),
            ..Default::default()
        },
    },
    Builtin {
        id: "codebuddy-code",
        name: "CodeBuddy Code",
        install_hint: "@tencent-ai/codebuddy-code",
        npm_package: Some("@tencent-ai/codebuddy-code"),
        cli_bin: "codebuddy",
        detect_bins: &[],
        args: &["--acp"],
        env: &[("DISABLE_AUTOUPDATER", "1")],
        command_var: "STEVE_ADAPTER_CODEBUDDY",
        default_command: default_codebuddy_adapter,
        properties: || AdapterProperties {
            state: Some(StateDir {
                env: "CODEBUDDY_CONFIG_DIR".into(),
                default: ".codebuddy".into(),
                import: vec!["settings.json".into()],
                extra_dirs: strings(&[
                    "Library/Application Support/CodeBuddyExtension",
                    ".local/share/CodeBuddyExtension",
                ]),
                ..Default::default()
            }),
            modes: ModeMap {
                plan: "plan".into(),
                ask: "default".into(),
                accept_edits: "acceptEdits".into(),
                auto: "auto".into(),
                full_access: "fullAccess".into(),
            },
            fast_models: strings(&["flash", "fast"]),
            resume_cli: Some("codebuddy --resume {id}".into()),
            ..Default::default()
        },
    },
    Builtin {
        id: "cursor",
        name: "Cursor",
        install_hint: "curl https://cursor.com/install -fsS | bash",
        npm_package: None,
        cli_bin: "cursor-agent",
        detect_bins: &["cursor"],
        args: &["--disable-auto-update", "acp"],
        env: &[],
        command_var: "STEVE_ADAPTER_CURSOR",
        default_command: path_cursor_adapter,
        properties: || AdapterProperties {
            state: Some(StateDir {
                env: "CURSOR_CONFIG_DIR".into(),
                default: ".cursor".into(),
                import: vec!["cli-config.json".into()],
                extra_dirs: strings(&["Library/Application Support/Cursor", ".config/Cursor"]),
                ..Default::default()
            }),
            modes: ModeMap {
                plan: "plan".into(),
                ask: "ask".into(),
                accept_edits: "agent".into(),
                auto: String::new(),
                full_access: String::new(),
            },
            fast_models: strings(&["composer", "flash", "mini", "nano", "haiku", "luna"]),
            resume_cli: Some("cursor-agent --resume {id}".into()),
            ..Default::default()
        },
    },
    Builtin {
        id: "kiro",
        name: "Kiro CLI",
        install_hint: "curl -fsSL https://cli.kiro.dev/install | bash",
        npm_package: None,
        cli_bin: "kiro-cli",
        detect_bins: &[],
        args: &["acp", "--agent-engine=v3"],
        env: &[("KIRO_NO_AUTO_UPDATE", "1")],
        command_var: "STEVE_ADAPTER_KIRO",
        default_command: path_kiro_adapter,
        properties: || AdapterProperties {
            state: Some(StateDir {
                env: "KIRO_HOME".into(),
                default: ".kiro".into(),
                import: strings(&["settings/cli.json", "settings/permissions.yaml"]),
                extra_dirs: strings(&[
                    "Library/Application Support/kiro-cli",
                    ".local/share/kiro-cli",
                    ".config/Kiro",
                    ".config/kiro",
                    "Library/Application Support/Kiro",
                ]),
                ..Default::default()
            }),
            modes: ModeMap {
                plan: "planner".into(),
                ask: "vibe".into(),
                accept_edits: String::new(),
                auto: String::new(),
                full_access: String::new(),
            },
            fast_models: strings(&["haiku", "luna", "qwen"]),
            resume_cli: Some("kiro-cli chat --resume-id {id}".into()),
            ..Default::default()
        },
    },
    Builtin {
        id: "mistral-vibe",
        name: "Mistral Vibe",
        install_hint: "uv tool install mistral-vibe",
        npm_package: None,
        cli_bin: "vibe-acp",
        detect_bins: &["vibe"],
        args: &[],
        env: &[
            ("VIBE_ENABLE_UPDATE_CHECKS", "false"),
            ("VIBE_ENABLE_AUTO_UPDATE", "false"),
            ("VIBE_ENABLE_TELEMETRY", "false"),
            ("VIBE_EXPERIMENTS__ENABLE", "false"),
        ],
        command_var: "STEVE_ADAPTER_VIBE",
        default_command: path_vibe_adapter,
        properties: || AdapterProperties {
            state: Some(StateDir {
                env: "VIBE_HOME".into(),
                default: ".vibe".into(),
                import: vec!["config.toml".into()],
                extra_dirs: strings(&[".config/vibe"]),
                ..Default::default()
            }),
            modes: ModeMap {
                plan: String::new(),
                ask: "ask".into(),
                accept_edits: "accept-edits".into(),
                auto: "smart-approve".into(),
                full_access: "auto-approve".into(),
            },
            usage_input_includes_cache: true,
            usage_prompt_cumulative: true,
            fallback_models: strings(&["mistral-medium-3.5"]),
            fast_models: strings(&["small", "ministral", "codestral", "flash", "mini", "haiku"]),
            resume_cli: Some("vibe --resume {id}".into()),
            ..Default::default()
        },
    },
    Builtin {
        id: "qoder",
        name: "Qoder CLI",
        install_hint: "curl -fsSL https://qoder.com/install | bash",
        npm_package: None,
        cli_bin: "qodercli",
        detect_bins: &["qoder"],
        args: &[
            "--settings",
            r#"{"general":{"enableAutoUpdate":false}}"#,
            "--acp",
        ],
        env: &[],
        command_var: "STEVE_ADAPTER_QODER",
        default_command: path_qoder_adapter,
        properties: || AdapterProperties {
            state: Some(StateDir {
                env: "QODER_CONFIG_DIR".into(),
                default: ".qoder".into(),
                import: vec!["settings.json".into()],
                extra_dirs: strings(&[
                    ".qoder-cn",
                    "Library/Application Support/Qoder",
                    ".config/Qoder",
                ]),
                home_files: strings(&[".qoder.json"]),
                ..Default::default()
            }),
            modes: ModeMap {
                plan: String::new(),
                ask: "default".into(),
                accept_edits: "acceptEdits".into(),
                auto: "auto".into(),
                full_access: "yolo".into(),
            },
            fast_models: strings(&["flash", "turbo", "mini", "lite", "haiku", "small"]),
            resume_cli: Some("qodercli --resume {id}".into()),
            ..Default::default()
        },
    },
];

fn strings(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

/// The id of the adapter Steve uses where nothing chose one, and the command a Node looks it up
/// by on its `PATH` when its manifest does not name one.
pub const DEFAULT_ADAPTER: (&str, &str) = ("claude-code", "claude-agent-acp");

/// The npm-managed adapters Steve installs by default when no explicit selection exists yet.
pub const DEFAULT_MANAGED_ADAPTERS: &[&str] = &["claude-code", "codex"];

/// Whether `id`'s CLI or ACP binary is currently present on `path_env`.
pub fn is_cli_detected(id: &str, path_env: Option<&std::ffi::OsStr>) -> bool {
    let Some(b) = BUILTINS.iter().find(|b| b.id == id) else {
        return false;
    };
    crate::config::which_in(b.cli_bin, path_env).is_some()
        || b.detect_bins
            .iter()
            .any(|alt| crate::config::which_in(alt, path_env).is_some())
}

/// The ids of the built-in entries.
pub fn builtin_ids() -> impl Iterator<Item = &'static str> {
    BUILTINS.iter().map(|b| b.id)
}

/// The built-in entries' commands: per id, the variable that names it and the command the global
/// npm install under `npm_root` gives (or the bare CLI name for PATH-only entries).
pub fn builtin_commands(
    npm_root: &Path,
) -> impl Iterator<Item = (&'static str, &'static str, PathBuf)> + '_ {
    BUILTINS
        .iter()
        .map(move |b| (b.id, b.command_var, (b.default_command)(npm_root)))
}

/// The built-in entries' `PATH` binary names (`cli_bin` and optional secondary CLI binaries).
pub fn builtin_bins() -> impl Iterator<Item = (&'static str, &'static str, &'static [&'static str])>
{
    BUILTINS.iter().map(|b| (b.id, b.cli_bin, b.detect_bins))
}

/// Whether built-in `id` is managed via npm in `<steve-home>/adapters`.
pub fn is_npm_managed(id: &str) -> bool {
    BUILTINS
        .iter()
        .find(|b| b.id == id)
        .and_then(|b| b.npm_package)
        .is_some()
}

/// The built-in entries' npm packages: per id, the package Steve installs and keeps up to date
/// ([`crate::managed`]).
pub fn builtin_packages() -> impl Iterator<Item = (&'static str, &'static str)> {
    BUILTINS
        .iter()
        .filter_map(|b| b.npm_package.map(|pkg| (b.id, pkg)))
}

/// The variable that names the default model of new sessions on the default adapter.
pub const DEFAULT_MODEL_VAR: &str = "STEVE_DEFAULT_MODEL_CLAUDE";

/// The spec of `id`: its built-in data (if any) with `declared` over it.
pub fn spec(id: &str, declared: Option<&Declaration>) -> AdapterSpec {
    let builtin = BUILTINS.iter().find(|b| b.id == id);
    let mut properties = builtin.map(|b| (b.properties)()).unwrap_or_default();
    let mut name = builtin
        .map(|b| b.name.to_owned())
        .unwrap_or_else(|| id.to_owned());
    let mut install_hint = builtin.map(|b| b.install_hint.to_owned());
    if let Some(d) = declared {
        d.properties.apply(&mut properties);
        if let Some(n) = d.name.as_ref().filter(|n| !n.trim().is_empty()) {
            name = n.clone();
        }
        if d.install_hint.is_some() {
            install_hint = d.install_hint.clone();
        }
    }
    AdapterSpec {
        id: id.to_owned(),
        name,
        builtin: builtin.is_some(),
        properties,
        install_hint,
    }
}

// ── probe ──

/// How an adapter answered [`probe`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeStatus {
    /// `initialize` succeeded.
    Ready,
    /// Its command could not be started.
    AdapterMissing,
    /// It started, but `initialize` failed or did not answer in time.
    InitializeFailed,
}

/// A way to sign in the adapter offers (`authMethods`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuthMethod {
    pub id: String,
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// A model an adapter offers (`configOptions` category `model`, or `models.availableModels`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelOption {
    pub id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
}

/// What a scratch `session/new` said: the modes, models and efforts on offer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SessionOffer {
    /// Steve's permission modes it can be set to ([`ModeMap::offered`]).
    pub modes: Vec<PermissionMode>,
    pub models: Vec<ModelOption>,
    /// The model it runs when none is set.
    pub default_model: Option<String>,
    /// The reasoning efforts it takes (`thought_level`).
    pub efforts: Vec<String>,
}

impl SessionOffer {
    /// Reads a `session/new` result with `modes`, the adapter's mode map.
    pub fn from_session(r: &Value, modes: &ModeMap) -> Self {
        Self::from_session_with(r, modes, false, None)
    }

    /// Reads a `session/new` result with `modes` and adapter-level mode properties.
    pub fn from_session_with(
        r: &Value,
        modes: &ModeMap,
        blind: bool,
        mode_meta: Option<&BTreeMap<String, Value>>,
    ) -> Self {
        let options = r.get("configOptions").and_then(Value::as_array);
        let option = |category: &str| {
            options.and_then(|os| {
                os.iter().find(|o| {
                    o.get("category").and_then(Value::as_str) == Some(category)
                        || o.get("id").and_then(Value::as_str) == Some(category)
                })
            })
        };
        // a select's options, flattened out of their groups
        fn values(v: &Value, out: &mut Vec<ModelOption>) {
            for o in v.as_array().into_iter().flatten() {
                if let Some(group) = o.get("options") {
                    values(group, out);
                } else if let Some(id) = o.get("value").and_then(Value::as_str) {
                    let text = |k: &str| o.get(k).and_then(Value::as_str).map(String::from);
                    out.push(ModelOption {
                        id: id.into(),
                        name: text("name"),
                        description: text("description"),
                    });
                }
            }
        }
        let mut models = vec![];
        let mut default_model = None;
        if let Some(o) = option("model") {
            values(&o["options"], &mut models);
            default_model = o["currentValue"].as_str().map(String::from);
        } else if let Some(m) = r.get("models").filter(|m| m.is_object()) {
            for a in m["availableModels"].as_array().into_iter().flatten() {
                if let Some(id) = a.get("modelId").and_then(Value::as_str) {
                    let text = |k: &str| a.get(k).and_then(Value::as_str).map(String::from);
                    models.push(ModelOption {
                        id: id.into(),
                        name: text("name"),
                        description: text("description"),
                    });
                }
            }
            default_model = m["currentModelId"].as_str().map(String::from);
        }
        let mut efforts = vec![];
        if let Some(o) = crate::session::effort_option(r) {
            values(&o["options"], &mut efforts);
        }
        Self {
            modes: modes.offered_with(
                crate::session::offered_modes(r).as_deref(),
                blind,
                mode_meta,
            ),
            models,
            default_model: default_model.filter(|m| !m.is_empty()),
            // `default` is no effort of its own: what runs when none is set
            efforts: efforts
                .into_iter()
                .map(|e| e.id)
                .filter(|e| e != "default")
                .collect(),
        }
    }
}

/// What `initialize` (and a scratch `session/new`) said.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Probe {
    pub status: ProbeStatus,
    /// What went wrong, when not ready (no stderr, no arguments).
    pub error: Option<String>,
    /// `agentInfo.title` (else `name`).
    pub title: Option<String>,
    /// `agentInfo.version`.
    pub version: Option<String>,
    pub image: bool,
    pub embedded_context: bool,
    pub load_session: bool,
    pub list_sessions: bool,
    pub mcp_http: bool,
    /// It takes a system prompt of the client's own (`sessionCapabilities.systemPrompt`).
    pub system_prompt: bool,
    pub auth: Vec<AuthMethod>,
    /// What a scratch session offered; `None` when it could not be opened (`session_error`).
    pub session: Option<SessionOffer>,
    pub session_error: Option<String>,
}

impl Probe {
    fn failed(status: ProbeStatus, error: impl Into<String>) -> Self {
        Self {
            status,
            error: Some(error.into()),
            title: None,
            version: None,
            image: false,
            embedded_context: false,
            load_session: false,
            list_sessions: false,
            mcp_http: false,
            system_prompt: false,
            auth: vec![],
            session: None,
            session_error: None,
        }
    }

    /// Reads an `initialize` result.
    pub fn from_initialize(r: &Value) -> Self {
        let caps = r.get("agentCapabilities").cloned().unwrap_or(Value::Null);
        let flag = |p: &str| caps.pointer(p).is_some_and(truthy);
        let text = |p: &str| {
            r.pointer(p)
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(String::from)
        };
        let auth = r
            .get("authMethods")
            .and_then(Value::as_array)
            .map(|ms| {
                ms.iter()
                    .filter_map(|m| {
                        Some(AuthMethod {
                            id: m.get("id")?.as_str()?.to_owned(),
                            name: m
                                .get("name")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_owned(),
                            description: m
                                .get("description")
                                .and_then(Value::as_str)
                                .map(String::from),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();
        Self {
            status: ProbeStatus::Ready,
            error: None,
            title: text("/agentInfo/title").or_else(|| text("/agentInfo/name")),
            version: text("/agentInfo/version"),
            image: flag("/promptCapabilities/image"),
            embedded_context: flag("/promptCapabilities/embeddedContext"),
            load_session: flag("/loadSession"),
            // `sessionCapabilities.list` is an object when offered
            list_sessions: caps
                .pointer("/sessionCapabilities/list")
                .is_some_and(|v| !v.is_null() && v != &Value::Bool(false)),
            mcp_http: flag("/mcpCapabilities/http"),
            system_prompt: caps
                .pointer("/sessionCapabilities/systemPrompt")
                .is_some_and(|v| !v.is_null() && v != &Value::Bool(false)),
            auth,
            session: None,
            session_error: None,
        }
    }
}

/// Starts `id`'s adapter in `cwd`, asks `initialize` and opens a scratch session (each within
/// `timeout`, no prompt: no model is asked anything) and stops it again.
pub async fn probe(cfg: &crate::AcpConfig, id: &str, cwd: &Path, timeout: Duration) -> Probe {
    let (program, args) = match cfg.adapter_command(id) {
        Ok(c) => c,
        Err(e) => return Probe::failed(ProbeStatus::AdapterMissing, e),
    };
    // a bare name is looked up on PATH by the spawn; a path must exist: the program's, and the
    // adapter's own file when another program runs it (a `.js` one through node, which would
    // start and answer a missing file with a stack trace and an exit, read as a failed handshake)
    let gone = |p: &Path| p.components().count() > 1 && !p.exists();
    if gone(&program) || cfg.adapters.get(id).is_some_and(|own| gone(own)) {
        return Probe::failed(
            ProbeStatus::AdapterMissing,
            "the adapter's command is missing",
        );
    }
    let cwd = if cwd.is_dir() {
        cwd.to_path_buf()
    } else {
        std::env::temp_dir()
    };
    let conn: Arc<Conn> = match Conn::spawn(
        Spawn {
            program,
            args,
            cwd: cwd.clone(),
            env: cfg.adapter_env(id),
            log_tag: format!("[probe {id}]"),
            clean_env: false,
        },
        |c, i| {
            // nothing is asked of a client before a session; refuse whatever is
            if let crate::rpc::Incoming::Request { id, .. } = i {
                c.respond_error(id, -32601, "not supported while probing");
            }
        },
        || {},
    ) {
        Ok(c) => c,
        Err(e) => return Probe::failed(ProbeStatus::AdapterMissing, e.message),
    };
    let init = json!({
        "protocolVersion": 1,
        "clientCapabilities": {
            "fs": { "readTextFile": false, "writeTextFile": false },
            "terminal": false,
            "_meta": { "parameterizedModelPicker": true },
        },
        "clientInfo": { "name": "steve", "version": cfg.version },
    });
    let r = conn.request("initialize", init, Some(timeout)).await;
    let mut p = match r {
        Ok(r) => Probe::from_initialize(&r),
        Err(e) => {
            conn.kill();
            return Probe::failed(ProbeStatus::InitializeFailed, e.message);
        }
    };
    let mut params = json!({ "cwd": cwd, "mcpServers": [] });
    if let Some(meta) = cfg.session_meta(id) {
        params["_meta"] = meta;
    }
    match conn.request("session/new", params, Some(timeout)).await {
        Ok(r) => {
            let props = cfg.adapter_properties(id);
            p.session = Some(SessionOffer::from_session_with(
                &r,
                &props.modes,
                props.blind_set_mode,
                Some(&props.mode_meta),
            ));
        }
        Err(e) => p.session_error = Some(e.message),
    }
    conn.kill();
    p
}

impl crate::AcpConfig {
    /// The ids of the adapters this configuration can start, sorted.
    pub fn adapter_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self.adapters.keys().cloned().collect();
        ids.sort();
        ids
    }

    /// Everything known about `id` (built-in data and what the manifest declared).
    pub fn adapter_spec(&self, id: &str) -> AdapterSpec {
        spec(id, self.declared.get(id))
    }

    pub fn adapter_properties(&self, id: &str) -> AdapterProperties {
        self.adapter_spec(id).properties
    }

    /// The environment Steve gives `id`'s adapter over the inherited one: built-in launch
    /// variables, the harness executable where it takes one, then the declared variables.
    pub fn adapter_env(&self, id: &str) -> Vec<(OsString, OsString)> {
        let spec = self.adapter_spec(id);
        let mut env: Vec<(OsString, OsString)> = vec![];
        if let Some(b) = BUILTINS.iter().find(|b| b.id == id) {
            env.extend(b.env.iter().map(|(k, v)| ((*k).into(), (*v).into())));
        }
        if let Some(var) = &spec.properties.executable_env
            && var == "CLAUDE_CODE_EXECUTABLE"
            && !self.claude_executable.as_os_str().is_empty()
        {
            env.push((var.into(), self.claude_executable.clone().into_os_string()));
        }
        if let Some(d) = self.declared.get(id) {
            for (k, v) in &d.env {
                let key = OsString::from(k);
                if let Some(slot) = env.iter_mut().find(|(ek, _)| *ek == key) {
                    slot.1 = OsString::from(v);
                } else {
                    env.push((key, OsString::from(v)));
                }
            }
        }
        env
    }

    /// The `_meta` of `session/new|load` for `id`'s adapter.
    pub fn session_meta(&self, id: &str) -> Option<Value> {
        match self.adapter_properties(id).session_meta {
            SessionMeta::None => None,
            SessionMeta::ClaudeSettings => Some(json!({ "claudeCode": { "options": {
                "settings": steve_vault::harness::claude_settings(&self.steve_home),
            } } })),
        }
    }

    /// [`Self::session_meta`], with the declared `tools_off_meta` merged in when `tools_off`.
    pub fn session_meta_for(&self, id: &str, tools_off: bool) -> Option<Value> {
        self.session_meta_with_mode(id, tools_off, None)
    }

    /// [`Self::session_meta_for`], also merging any `mode_meta` declared for `mode`.
    pub fn session_meta_with_mode(
        &self,
        id: &str,
        tools_off: bool,
        mode: Option<PermissionMode>,
    ) -> Option<Value> {
        let props = self.adapter_properties(id);
        let mut out = self.session_meta(id);
        let mut add = |extra: Option<Value>| {
            if let Some(e) = extra {
                match &mut out {
                    Some(b) => merge(b, e),
                    None => out = Some(e),
                }
            }
        };
        if tools_off {
            add(props.tools_off_meta);
        }
        if let Some(m) = mode {
            let key = match m {
                PermissionMode::Plan => "plan",
                PermissionMode::Ask => "ask",
                PermissionMode::AcceptEdits => "accept_edits",
                PermissionMode::Auto => "auto",
                PermissionMode::FullAccess => "full_access",
            };
            add(props.mode_meta.get(key).cloned());
        }
        out
    }

    /// The ACP mode id `mode` is set as on `id`'s adapter.
    pub fn adapter_mode(&self, id: &str, mode: PermissionMode) -> String {
        self.adapter_properties(id).modes.id(mode).to_owned()
    }

    /// What `session/set_mode` asks of `id`'s adapter for `mode` ([`ModeMap::choice_with`]).
    pub fn mode_choice(&self, id: &str, mode: PermissionMode) -> crate::session::ModeChoice {
        let props = self.adapter_properties(id);
        props
            .modes
            .choice_with(mode, props.blind_set_mode, Some(&props.mode_meta))
    }

    pub(crate) fn launch_args(&self, id: &str) -> Vec<OsString> {
        let spec = self.adapter_spec(id);
        let mut args: Vec<OsString> = match spec.properties.launch_extra {
            LaunchExtra::None => vec![],
        };
        if let Some(d_args) = self.declared.get(id).and_then(|d| d.args.as_ref()) {
            args.extend(d_args.iter().map(OsString::from));
        } else if let Some(b) = BUILTINS.iter().find(|b| b.id == id) {
            args.extend(b.args.iter().map(OsString::from));
        }
        args
    }
}

/// `extra` merged into `into`, objects key by key.
fn merge(into: &mut Value, extra: Value) {
    match (into, extra) {
        (Value::Object(a), Value::Object(b)) => {
            for (k, v) in b {
                match a.get_mut(&k) {
                    Some(slot) => merge(slot, v),
                    None => {
                        a.insert(k, v);
                    }
                }
            }
        }
        (slot, v) => *slot = v,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(dir: &Path) -> crate::AcpConfig {
        crate::AcpConfig {
            adapters: HashMap::new(),
            declared: HashMap::new(),
            node: "node".into(),
            claude_executable: "/bin/claude-x".into(),
            steve_home: dir.join("steve"),
            codex_home: dir.join("codex"),
            user_home: dir.to_path_buf(),
            version: "t".into(),
        }
    }

    #[test]
    fn a_manifest_takes_commands_and_declarations() {
        let m: HashMap<String, AdapterEntry> = serde_json::from_value(json!({
            "claude-code": "/a/claude.js",
            "mine": {"command": "/b/mine", "name": "Mine", "args": ["--acp"], "env": {"K": "v"},
                     "properties": {"nested_sandbox": false, "modes": {"ask": "ask-me"}}}
        }))
        .unwrap();
        let (commands, declared) = split_entries(m);
        assert_eq!(commands["claude-code"], PathBuf::from("/a/claude.js"));
        assert_eq!(commands["mine"], PathBuf::from("/b/mine"));
        assert!(!declared.contains_key("claude-code"));
        let d = &declared["mine"];
        assert_eq!(d.args.as_deref(), Some(["--acp".to_string()].as_slice()));
        let s = spec("mine", Some(d));
        assert_eq!(s.name, "Mine");
        assert!(!s.builtin);
        assert!(!s.properties.nested_sandbox);
        assert_eq!(s.properties.modes.ask, "ask-me");
        assert_eq!(s.properties.modes.plan, "plan");
    }

    #[test]
    fn an_undeclared_adapter_gets_plain_acp() {
        let s = spec("someone-else", None);
        assert_eq!(s.name, "someone-else");
        assert_eq!(s.properties, AdapterProperties::default());
        assert!(s.properties.nested_sandbox && !s.properties.self_read_only);
        assert_eq!(
            s.properties.modes.id(PermissionMode::FullAccess),
            "bypassPermissions"
        );
    }

    #[test]
    fn built_in_modes_and_launch_are_data() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = cfg(dir.path());
        for id in builtin_ids() {
            c.adapters.insert(id.into(), dir.path().join(id));
        }
        let codex = c.adapter_spec("codex");
        assert!(codex.builtin && codex.properties.self_read_only);
        for (m, want) in [
            (PermissionMode::Plan, "read-only"),
            (PermissionMode::Ask, "read-only"),
            (PermissionMode::AcceptEdits, "workspace-write"),
            (PermissionMode::Auto, "agent"),
            (PermissionMode::FullAccess, "agent-full-access"),
        ] {
            assert_eq!(c.adapter_mode("codex", m), want);
        }
        assert_eq!(
            c.adapter_mode("claude-code", PermissionMode::Ask),
            "default"
        );
        // the harness executable goes only to the adapter that takes one
        assert_eq!(
            c.adapter_env("claude-code"),
            vec![(
                OsString::from("CLAUDE_CODE_EXECUTABLE"),
                OsString::from("/bin/claude-x")
            )]
        );
        assert!(c.adapter_env("codex").is_empty());
        assert!(c.session_meta("claude-code").is_some());
        // a call without tools has the built-in tools off where the adapter says how
        let off = c.session_meta_for("claude-code", true).unwrap();
        assert_eq!(off.pointer("/claudeCode/options/tools"), Some(&json!([])));
        assert!(off.pointer("/claudeCode/options/settings").is_some());
        assert!(c.session_meta_for("codex", true).is_none());
        assert!(c.session_meta("codex").is_none());
        // a declaration adds its own arguments and variables, and `args: Some(vec![])` clears built-in args
        c.adapters.insert("mine".into(), dir.path().join("mine"));
        c.declared.insert(
            "mine".into(),
            Declaration {
                args: Some(vec!["--acp".into()]),
                env: BTreeMap::from([("K".into(), "v".into())]),
                ..Default::default()
            },
        );
        let (_, args) = c.adapter_command("mine").unwrap();
        assert_eq!(args, vec![OsString::from("--acp")]);
        c.declared.insert(
            "gemini".into(),
            Declaration {
                args: Some(vec![]),
                ..Default::default()
            },
        );
        let (_, gemini_args) = c.adapter_command("gemini").unwrap();
        assert!(gemini_args.is_empty());
        assert_eq!(c.adapter_env("mine"), vec![("K".into(), "v".into())]);
        assert!(c.session_meta("mine").is_none());
    }

    #[test]
    fn a_mode_the_adapter_does_not_offer_is_stood_in_for_or_refused() {
        let ids = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let claude = ModeMap::default();
        let all = ids(&[
            "default",
            "acceptEdits",
            "plan",
            "auto",
            "bypassPermissions",
        ]);
        assert_eq!(claude.offered(Some(&all)).len(), 5);
        // Codex's four modes cover every Steve mode through its map
        let codex = spec("codex", None).properties.modes;
        assert_eq!(
            codex
                .offered(Some(&ids(&[
                    "read-only",
                    "workspace-write",
                    "agent",
                    "agent-full-access"
                ])))
                .len(),
            5
        );
        // no plan mode: Plan asks instead; no auto: refused
        let some = ids(&["default", "acceptEdits"]);
        assert_eq!(
            claude.choice(PermissionMode::Plan).resolve(Some(&some)),
            Ok(Some("default".into()))
        );
        assert!(
            claude
                .choice(PermissionMode::Auto)
                .resolve(Some(&some))
                .unwrap_err()
                .starts_with("unsupported: permission mode auto")
        );
        // no modes at all: the adapter asks for everything, so only Plan and Ask can be had
        assert_eq!(
            claude.offered(None),
            [PermissionMode::Plan, PermissionMode::Ask]
        );
        assert_eq!(claude.choice(PermissionMode::Ask).resolve(None), Ok(None));
    }

    #[test]
    fn initialize_is_read_as_capabilities() {
        let p = Probe::from_initialize(&json!({
            "protocolVersion": 1,
            "agentInfo": {"name": "my-agent", "title": "My Agent", "version": "1.2.3"},
            "agentCapabilities": {"loadSession": true,
                "promptCapabilities": {"image": true, "embeddedContext": false},
                "mcpCapabilities": {"http": true},
                "sessionCapabilities": {"list": {}}},
            "authMethods": [{"id": "login", "name": "Log in", "description": "run my-agent login"}]
        }));
        assert_eq!(p.status, ProbeStatus::Ready);
        assert_eq!(p.title.as_deref(), Some("My Agent"));
        assert_eq!(p.version.as_deref(), Some("1.2.3"));
        assert!(p.image && p.load_session && p.list_sessions && p.mcp_http);
        assert!(!p.embedded_context);
        assert_eq!(p.auth.len(), 1);
        let bare = Probe::from_initialize(&json!({"protocolVersion": 1}));
        assert!(!bare.image && !bare.load_session && !bare.list_sessions);
        assert_eq!(bare.title, None);
    }

    #[test]
    fn a_scratch_session_says_what_it_offers() {
        // claude-agent-acp 0.99.1's shape (models as a config option, efforts grouped or not)
        let r = json!({"sessionId": "s",
            "modes": {"currentModeId": "default", "availableModes": [
                {"id": "default"}, {"id": "acceptEdits"}, {"id": "plan"}, {"id": "auto"}, {"id": "bypassPermissions"}]},
            "configOptions": [
                {"id": "model", "category": "model", "currentValue": "m-2",
                 "options": [{"value": "m-1", "name": "One"}, {"group": "g", "options": [{"value": "m-2"}]}]},
                {"id": "effort", "category": "thought_level", "currentValue": "low",
                 "options": [{"value": "low"}, {"value": "high"}]}]});
        let o = SessionOffer::from_session(&r, &ModeMap::default());
        assert_eq!(o.modes.len(), 5);
        assert_eq!(
            o.models.iter().map(|m| m.id.as_str()).collect::<Vec<_>>(),
            ["m-1", "m-2"]
        );
        assert_eq!(o.models[0].name.as_deref(), Some("One"));
        assert_eq!(o.default_model.as_deref(), Some("m-2"));
        assert_eq!(o.efforts, ["low", "high"]);
        // the older `models` shape, no modes, no efforts
        let r = json!({"sessionId": "s", "models": {"currentModelId": "x",
            "availableModels": [{"modelId": "x", "name": "X"}]}});
        let o = SessionOffer::from_session(&r, &ModeMap::default());
        assert_eq!(o.modes, [PermissionMode::Plan, PermissionMode::Ask]);
        assert_eq!(o.default_model.as_deref(), Some("x"));
        assert!(o.efforts.is_empty());
    }

    /// The built-in adapters installed here, as discovery sees them (no prompt: no model is asked
    /// anything). Run with the harnesses' state somewhere of its own; `cargo test -- --ignored`.
    #[tokio::test]
    #[ignore = "starts the real adapters"]
    async fn real_adapters_offer_every_mode_and_their_models() {
        let cfg = crate::AcpConfig::from_env();
        let dir = tempfile::tempdir().unwrap();
        for id in builtin_ids() {
            let p = probe(&cfg, id, dir.path(), Duration::from_secs(60)).await;
            let o = p.session.clone().unwrap_or_else(|| panic!("{id}: {p:?}"));
            eprintln!(
                "{id}: {:?} {:?} modes {:?} default {:?} efforts {:?} models {}",
                p.title,
                p.version,
                o.modes,
                o.default_model,
                o.efforts,
                o.models.len()
            );
            assert_eq!(p.status, ProbeStatus::Ready);
            assert!(
                !o.modes.is_empty(),
                "{id}: at least Plan/Ask map to a mode it offers"
            );
            assert!(
                !o.models.is_empty() || !cfg.adapter_properties(id).fallback_models.is_empty(),
                "{id}"
            );
        }
    }

    #[test]
    fn all_sixteen_builtins_have_valid_specs_and_mode_resolutions() {
        let ids: Vec<&str> = builtin_ids().collect();
        assert_eq!(ids.len(), 16);
        assert_eq!(ids[0], "claude-code");
        assert_eq!(ids[1], "codex");
        assert_eq!(builtin_packages().count(), 12);
        let dir = tempfile::tempdir().unwrap();
        let mut c = cfg(dir.path());
        for id in &ids {
            c.adapters.insert((*id).into(), dir.path().join(id));
            let s = c.adapter_spec(id);
            assert!(s.builtin, "{id}");
            assert!(!s.name.is_empty(), "{id}");
            assert!(
                s.install_hint.as_deref().is_some_and(|h| !h.is_empty()),
                "{id}"
            );
            assert!(
                s.properties
                    .resume_cli
                    .as_deref()
                    .is_some_and(|r| r.contains("{id}")),
                "{id}"
            );
            // Plan and Ask must always resolve when an adapter reports no modes (or blind_set_mode)
            let offered_none = s.properties.modes.offered_with(
                None,
                s.properties.blind_set_mode,
                Some(&s.properties.mode_meta),
            );
            assert!(offered_none.contains(&PermissionMode::Plan), "{id}");
            assert!(offered_none.contains(&PermissionMode::Ask), "{id}");
        }
        // grok-build blind_set_mode + mode_meta offers Plan, Ask, Auto, FullAccess even without availableModes
        let grok = c.adapter_spec("grok-build").properties;
        let grok_modes = grok
            .modes
            .offered_with(None, grok.blind_set_mode, Some(&grok.mode_meta));
        assert_eq!(
            grok_modes,
            vec![
                PermissionMode::Plan,
                PermissionMode::Ask,
                PermissionMode::Auto,
                PermissionMode::FullAccess,
            ]
        );
        assert_eq!(
            c.session_meta_with_mode("grok-build", false, Some(PermissionMode::Auto)),
            Some(json!({"autoMode": true}))
        );
        assert_eq!(
            c.session_meta_with_mode("grok-build", false, Some(PermissionMode::FullAccess)),
            Some(json!({"yoloMode": true}))
        );
    }

    #[tokio::test]
    async fn probe_reports_a_missing_adapter() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = cfg(dir.path());
        c.adapters
            .insert("gone".into(), dir.path().join("nope/agent"));
        let p = probe(&c, "gone", dir.path(), Duration::from_secs(5)).await;
        assert_eq!(p.status, ProbeStatus::AdapterMissing);
        let p = probe(&c, "unknown", dir.path(), Duration::from_secs(5)).await;
        assert_eq!(p.status, ProbeStatus::AdapterMissing);
        // a script adapter that is not there: missing too, without starting its interpreter on it
        // (`node` here is a program that would answer anything it is started with)
        c.node = "/bin/cat".into();
        c.adapters
            .insert("unbuilt".into(), dir.path().join("nope/agent.js"));
        let p = probe(&c, "unbuilt", dir.path(), Duration::from_millis(300)).await;
        assert_eq!(p.status, ProbeStatus::AdapterMissing, "{p:?}");
    }
}
