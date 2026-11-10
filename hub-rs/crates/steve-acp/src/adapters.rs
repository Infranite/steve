//! The harness adapter registry (harness parity S1): what Steve knows about each ACP adapter, keyed
//! by its id (any string). Everything Steve needs to run sessions, jobs and judgments over plain
//! ACP is an id, a name and a launch command; what an adapter needs beyond that is a **declared
//! property** here, never a comparison with its name elsewhere.
//!
//! Sources, later over earlier:
//! 1. the built-in entries below (Claude Code via claude-agent-acp, Codex via codex-acp), as data;
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
        let narrow = matches!(mode, PermissionMode::Plan | PermissionMode::Ask);
        crate::session::ModeChoice {
            id: self.id(mode).to_owned(),
            fallback: (mode == PermissionMode::Plan).then(|| self.ask.clone()),
            narrow,
        }
    }

    /// Steve's modes an adapter offering `ids` can be set to (as [`Self::choice`] resolves them);
    /// `None` (it reports no modes): Plan and Ask only.
    pub fn offered(&self, ids: Option<&[String]>) -> Vec<PermissionMode> {
        [
            PermissionMode::Plan,
            PermissionMode::Ask,
            PermissionMode::AcceptEdits,
            PermissionMode::Auto,
            PermissionMode::FullAccess,
        ]
        .into_iter()
        .filter(|m| self.choice(*m).resolve(ids).is_ok())
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
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StateDir {
    /// The variable that moves it.
    pub env: String,
    /// Where it is under the user's home when the variable is not set.
    pub default: String,
    /// What of the Node owner's own directory the owner's state starts with (their own settings,
    /// never anyone else's; a login only where a copy cannot sign the original out).
    #[serde(default)]
    pub import: Vec<String>,
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
    /// Where a call's tokens come from when the adapter reports none.
    pub usage_fallback: UsageFallback,
    /// The models to offer before (or without) a probe of the adapter.
    pub fallback_models: Vec<String>,
    /// The reasoning efforts to offer before (or without) a probe of the adapter.
    pub fallback_efforts: Vec<String>,
    /// Steve's judgments fall back down Steve's model lists (`STEVE_MODELS_SMART`) after the bot's
    /// model; without it they try the bot's model, else the adapter's own default.
    pub model_fallback: bool,
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
            executable_env: None,
            session_meta: SessionMeta::None,
            launch_extra: LaunchExtra::None,
            probe_initialize: true,
            tools_off_meta: None,
            usage_input_includes_cache: false,
            usage_fallback: UsageFallback::None,
            fallback_models: vec![],
            fallback_efforts: vec![],
            model_fallback: false,
        }
    }
}

/// The keys of [`PropertiesPatch`] (what a manifest's `properties` may hold).
pub const PROPERTY_KEYS: [&str; 14] = [
    "self_read_only",
    "nested_sandbox",
    "state",
    "modes",
    "executable_env",
    "session_meta",
    "launch_extra",
    "probe_initialize",
    "tools_off_meta",
    "usage_input_includes_cache",
    "usage_fallback",
    "fallback_models",
    "fallback_efforts",
    "model_fallback",
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
    pub usage_fallback: Option<UsageFallback>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_models: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fallback_efforts: Option<Vec<String>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_fallback: Option<bool>,
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
    }
}

/// What a manifest declares about an adapter besides its command.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Declaration {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
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
    /// The variable that names its command (`STEVE_ADAPTER_*`), and where it is installed by
    /// default under `npm root -g`.
    command_var: &'static str,
    default_command: fn(&Path) -> PathBuf,
    properties: fn() -> AdapterProperties,
}

const BUILTINS: &[Builtin] = &[
    Builtin {
        id: "claude-code",
        name: "Claude Code",
        install_hint: "@agentclientprotocol/claude-agent-acp",
        command_var: "STEVE_ADAPTER_CLAUDE",
        default_command: crate::config::default_claude_adapter,
        properties: || AdapterProperties {
            self_read_only: false,
            nested_sandbox: true,
            state: Some(StateDir {
                env: "CLAUDE_CONFIG_DIR".into(),
                default: ".claude".into(),
                import: vec!["settings.json".into()],
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
        },
    },
    Builtin {
        id: "codex",
        name: "Codex",
        install_hint: "@agentclientprotocol/codex-acp",
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
            fallback_models: vec![],
            fallback_efforts: strings(&["low", "medium", "high", "xhigh", "max"]),
            model_fallback: false,
        },
    },
];

fn strings(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| s.to_string()).collect()
}

/// The id of the adapter Steve uses where nothing chose one, and the command a Node looks it up
/// by on its `PATH` when its manifest does not name one.
pub const DEFAULT_ADAPTER: (&str, &str) = ("claude-code", "claude-code-acp");

/// The ids of the built-in entries.
pub fn builtin_ids() -> impl Iterator<Item = &'static str> {
    BUILTINS.iter().map(|b| b.id)
}

/// The built-in entries' commands: per id, the variable that names it and the command the global
/// npm install under `npm_root` gives.
pub fn builtin_commands(
    npm_root: &Path,
) -> impl Iterator<Item = (&'static str, &'static str, PathBuf)> + '_ {
    BUILTINS
        .iter()
        .map(move |b| (b.id, b.command_var, (b.default_command)(npm_root)))
}

/// The built-in entries' npm packages: per id, the package Steve installs and keeps up to date
/// ([`crate::managed`]).
pub fn builtin_packages() -> impl Iterator<Item = (&'static str, &'static str)> {
    BUILTINS.iter().map(|b| (b.id, b.install_hint))
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
            modes: modes.offered(crate::session::offered_modes(r).as_deref()),
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
    // a bare name is looked up on PATH by the spawn; a path must exist
    if program.components().count() > 1 && !program.exists() {
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
        "clientCapabilities": { "fs": { "readTextFile": false, "writeTextFile": false }, "terminal": false },
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
            let modes = cfg.adapter_properties(id).modes;
            p.session = Some(SessionOffer::from_session(&r, &modes));
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

    /// The environment Steve gives `id`'s adapter over the inherited one: the harness executable
    /// where it takes one, then the declared variables.
    pub fn adapter_env(&self, id: &str) -> Vec<(OsString, OsString)> {
        let spec = self.adapter_spec(id);
        let mut env: Vec<(OsString, OsString)> = vec![];
        if let Some(var) = &spec.properties.executable_env {
            env.push((var.into(), self.claude_executable.clone().into_os_string()));
        }
        if let Some(d) = self.declared.get(id) {
            env.extend(d.env.iter().map(|(k, v)| (k.into(), v.into())));
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
        let base = self.session_meta(id);
        let extra = tools_off
            .then(|| self.adapter_properties(id).tools_off_meta)
            .flatten();
        match (base, extra) {
            (Some(mut b), Some(e)) => {
                merge(&mut b, e);
                Some(b)
            }
            (b, e) => b.or(e),
        }
    }

    /// The ACP mode id `mode` is set as on `id`'s adapter.
    pub fn adapter_mode(&self, id: &str, mode: PermissionMode) -> String {
        self.adapter_properties(id).modes.id(mode).to_owned()
    }

    /// What `session/set_mode` asks of `id`'s adapter for `mode` ([`ModeMap::choice`]).
    pub fn mode_choice(&self, id: &str, mode: PermissionMode) -> crate::session::ModeChoice {
        self.adapter_properties(id).modes.choice(mode)
    }

    pub(crate) fn launch_args(&self, id: &str) -> Vec<OsString> {
        let spec = self.adapter_spec(id);
        let mut args: Vec<OsString> = match spec.properties.launch_extra {
            LaunchExtra::None => vec![],
        };
        if let Some(d) = self.declared.get(id) {
            args.extend(d.args.iter().map(OsString::from));
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
            idle_release: Duration::from_secs(1),
            retry_delays: vec![],
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
        assert_eq!(d.args, ["--acp"]);
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
        // a declaration adds its own arguments and variables
        c.adapters.insert("mine".into(), dir.path().join("mine"));
        c.declared.insert(
            "mine".into(),
            Declaration {
                args: vec!["--acp".into()],
                env: BTreeMap::from([("K".into(), "v".into())]),
                ..Default::default()
            },
        );
        let (_, args) = c.adapter_command("mine").unwrap();
        assert_eq!(args, vec![OsString::from("--acp")]);
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
            assert_eq!(
                o.modes.len(),
                5,
                "{id}: every Steve mode maps to one it offers"
            );
            assert!(!o.models.is_empty() && !o.efforts.is_empty(), "{id}");
        }
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
    }
}
