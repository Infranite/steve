//! Harness declarations and the trusted built-in catalog. This crate reads no environment or
//! filesystem and starts no process: callers supply commands and runtime context separately.
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashMap};
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use steve_node_api::PermissionMode;

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
    pub fn choice(&self, mode: PermissionMode) -> ModeChoice {
        self.choice_with(mode, false, None)
    }

    /// [`Self::choice`] with adapter-level mode properties (`blind_set_mode`, `mode_meta`).
    pub fn choice_with(
        &self,
        mode: PermissionMode,
        blind: bool,
        mode_meta: Option<&BTreeMap<String, Value>>,
    ) -> ModeChoice {
        let narrow = matches!(mode, PermissionMode::Plan | PermissionMode::Ask);
        let key = match mode {
            PermissionMode::Plan => "plan",
            PermissionMode::Ask => "ask",
            PermissionMode::AcceptEdits => "accept_edits",
            PermissionMode::Auto => "auto",
            PermissionMode::FullAccess => "full_access",
        };
        let meta = mode_meta.and_then(|m| m.get(key)).cloned();
        ModeChoice {
            requested: key,
            id: self.id(mode).to_owned(),
            fallback: (mode == PermissionMode::Plan).then(|| self.ask.clone()),
            narrow,
            blind,
            meta,
            native_execution: false,
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
    /// The owner's own instructions and extensions the harness reads from its directory (its
    /// user-level `AGENTS.md` / `CLAUDE.md`, skills, subagents, commands): each time a session of
    /// the owner opens, these are linked into its directory, beside `import`, so the harness finds
    /// them where it would without Steve and an edit the owner makes is seen by the next session.
    /// An entry ending in `/*` links each child of that directory on its own (one Steve also
    /// writes into, like the skills directory); names starting with `.` are the harness's own and
    /// are skipped. What the harness or a person put there is left alone; a Bot skill of the same
    /// name gives way to the owner's. A session of another principal gets none of it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub context: Vec<String>,
    /// The harness's sign-in on the Node, apart from the owner's settings and personal context
    /// above: every principal's session on the Node uses the Node's one sign-in ([`Login`]).
    #[serde(default, skip_serializing_if = "Login::is_empty")]
    pub login: Login,
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
    /// Optional auxiliary HOME for the adapter process: a plain directory under the state one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub home: Option<String>,
}

/// How a session uses the harness's sign-in on its Node. The harness signs in by itself there (its
/// own login command, its own settings, set up by whoever installed it on that machine); Steve
/// never configures a model provider and never handles a provider key. Every principal's session on
/// the Node, the Node owner's and anyone else's (a schedule's service principal too), uses that one
/// sign-in, and never a copy of it: a copy that renewed its tokens on its own would sign the
/// original out. What stays per principal is the rest of the harness's directory (history,
/// sessions) and, for the Node's owner only, their settings and personal context
/// ([`StateDir::import`], [`StateDir::context`]).
///
/// Two ways to reach the one sign-in, chosen by what the harness supports:
///
/// - [`Self::store_env`]: a variable that moves only the harness's credential store. The session
///   gets the Node's own value of it and the harness uses the Node's store in place; nothing is
///   linked or written (Claude Code).
/// - Without it, each of [`Self::files`] is a link in the session's directory to the Node's own
///   file. Only for a harness that, when it renews its sign-in, writes through a link or replaces
///   the file the link leads to (never the link itself), so the link and the Node's file stay one
///   (Codex resolves the link and renames over its target, under a lock beside the target; Pi
///   writes through it, locked at the target).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Login {
    /// The harness's credential files, relative to its credential store (its directory, or the
    /// one [`Self::store_env`] names): linked into the session's directory without `store_env`
    /// (even before the Node signs in, so a later sign-in is seen); with it, what a confined
    /// session may read and write there.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub files: Vec<String>,
    /// What else the harness makes in its credential store while it renews its sign-in, beside
    /// [`Self::files`]: its lock and its temporary file, by name (a trailing `*` matches any rest
    /// of it). A session that may not see the Node's harness directory may still write these
    /// there, so a renewal is never made and then lost (the old sign-in would be spent).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub renew: Vec<String>,
    /// A variable that moves only the harness's credential store, apart from [`StateDir::env`]
    /// (Claude Code's `CLAUDE_SECURESTORAGE_CONFIG_DIR`: its keychain entry and its
    /// `.credentials.json`). A session gets the Node's own (`StateDirRuntime::node_store`).
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub store_env: String,
    /// The harness's settings file (relative to its directory, `.toml` or `.json`) that may hold
    /// its sign-in as well: which provider or gateway it uses and how it gets its key.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub settings: String,
    /// The sign-in entries of [`Self::settings`]: a top-level `key`, or `key.SUB` for one entry of
    /// a table; a trailing `*` matches any rest of the last name. In every session's file these
    /// entries are the Node's own, as they are there when the harness opens; everything else in
    /// it stays the session's (the owner's starts from their whole file, [`StateDir::import`]).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub entries: Vec<String>,
}

impl Login {
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
            && self.renew.is_empty()
            && self.store_env.is_empty()
            && self.settings.is_empty()
            && self.entries.is_empty()
    }

    /// Whether `rel` is a plain relative path of names (no `.`, `..`, empty or absolute parts).
    pub fn plain(rel: &str) -> bool {
        !rel.is_empty()
            && rel.len() <= 256
            && !Path::new(rel).is_absolute()
            && rel.split('/').all(|p| {
                !p.is_empty()
                    && p != "."
                    && p != ".."
                    && p.chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
            })
    }

    /// Whether `name` of [`Self::renew`] is usable: one plain name, `*` only at its end.
    pub fn renew_ok(name: &str) -> bool {
        let base = name.strip_suffix('*').unwrap_or(name);
        !base.is_empty() && !base.contains(['/', '*']) && Self::plain(base) && base != "."
    }

    /// Whether `entry` of [`Self::entries`] is usable: one or two names, `*` only at the end.
    pub fn entry_ok(entry: &str) -> bool {
        let parts: Vec<&str> = entry.split('.').collect();
        (1..=2).contains(&parts.len())
            && parts.iter().enumerate().all(|(i, p)| {
                let name = p.strip_suffix('*').unwrap_or(p);
                (i + 1 == parts.len() || !p.ends_with('*'))
                    && (!name.is_empty() || p.ends_with('*'))
                    && name
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-'))
            })
    }

    /// Whether `name` is a usable variable name.
    pub fn var_ok(name: &str) -> bool {
        !name.is_empty()
            && name.len() <= 128
            && name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
            && !name.as_bytes()[0].is_ascii_digit()
    }

    /// The names in the credential store a confined session may read and write: the credential
    /// files that are one plain name, and [`Self::renew`]'s.
    pub fn store_names(&self) -> Vec<String> {
        self.files
            .iter()
            .filter(|f| Self::plain(f) && !f.contains('/'))
            .chain(self.renew.iter().filter(|n| Self::renew_ok(n)))
            .cloned()
            .collect()
    }
}

impl StateDir {
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

    /// Environment assignments computed from this declaration alone. Importing ambient git
    /// configuration is a runtime concern of the ACP driver.
    pub fn isolation_env(&self, dir: &Path) -> Vec<(OsString, OsString)> {
        let mut env = Vec::new();
        if !self.env.is_empty() {
            env.push((self.env.clone().into(), dir.as_os_str().to_owned()));
        }
        if self.xdg {
            for (var, sub) in [
                ("XDG_DATA_HOME", "xdg/data"),
                ("XDG_CONFIG_HOME", "xdg/config"),
                ("XDG_STATE_HOME", "xdg/state"),
                ("XDG_CACHE_HOME", "xdg/cache"),
            ] {
                env.push((var.into(), dir.join(sub).into_os_string()));
            }
        }
        env
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

/// Some ACP adapters use `modes` for reasoning, and do not enforce tool permissions at all.
/// Such an adapter must never silently stand in for a narrower Steve permission mode.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionPolicy {
    #[default]
    Acp,
    FullAccessOnly,
}

impl PermissionPolicy {
    pub fn check(self, mode: PermissionMode) -> Result<(), String> {
        if self == Self::FullAccessOnly && mode != PermissionMode::FullAccess {
            Err("unsupported: this adapter only supports full_access; its ACP modes are thinking levels, not tool permissions".into())
        } else {
            Ok(())
        }
    }
}

/// Where a call's tokens are read when the adapter does not report them.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageFallback {
    #[default]
    None,
    /// Codex's rollout file of the session (`$CODEX_HOME/sessions`).
    CodexRollout,
    /// Claude assistant message usage in `$CLAUDE_CONFIG_DIR/projects`.
    ClaudeTranscript,
}

// One field schema generates resolved properties, manifest patches and accepted keys.
macro_rules! apply_property {
    ($s:ident, $p:ident, $f:ident, copied) => {
        if let Some(v) = $s.$f {
            $p.$f = v;
        }
    };
    ($s:ident, $p:ident, $f:ident, cloned) => {
        if let Some(v) = &$s.$f {
            $p.$f = v.clone();
        }
    };
    ($s:ident, $p:ident, $f:ident, state) => {
        if let Some(v) = &$s.$f {
            $p.$f = Some(v.clone());
        }
    };
    ($s:ident, $p:ident, $f:ident, optional_string) => {
        if let Some(v) = &$s.$f {
            $p.$f = Some(v.clone()).filter(|v| !v.is_empty());
        }
    };
    ($s:ident, $p:ident, $f:ident, trimmed_string) => {
        if let Some(v) = &$s.$f {
            $p.$f = Some(v.clone()).filter(|v| !v.trim().is_empty());
        }
    };
    ($s:ident, $p:ident, $f:ident, json) => {
        if let Some(v) = &$s.$f {
            $p.$f = Some(v.clone()).filter(|v| !v.is_null());
        }
    };
    ($s:ident, $p:ident, modes, modes) => {
        for (k, v) in &$s.modes {
            let slot = match k.as_str() {
                "plan" => &mut $p.modes.plan,
                "ask" => &mut $p.modes.ask,
                "accept_edits" => &mut $p.modes.accept_edits,
                "auto" => &mut $p.modes.auto,
                "full_access" => &mut $p.modes.full_access,
                _ => continue,
            };
            *slot = v.clone();
        }
    };
    ($s:ident, $p:ident, mode_meta, mode_meta) => {
        for (k, v) in &$s.mode_meta {
            if v.is_null() {
                $p.mode_meta.remove(k);
            } else {
                $p.mode_meta.insert(k.clone(), v.clone());
            }
        }
    };
}
macro_rules! adapter_properties {
    ($($(#[$doc:meta])* $field:ident: $ty:ty = $default:expr => $patch:ty, $skip:literal, $apply:ident;)*) => {
        /// Resolved adapter capabilities. Runtime manifests use PropertiesPatch instead.
        #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(default)]
        pub struct AdapterProperties {
            $($(#[$doc])* pub $field: $ty,)*
            /// Trusted built-in history support; deliberately not a manifest override.
            pub local_transcript: bool,
        }
        impl Default for AdapterProperties {
            fn default() -> Self { Self { $($field: $default,)* local_transcript: false } }
        }
        /// Each supplied property overlays the built-in or conservative default value.
        #[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
        pub struct PropertiesPatch {
            $(#[serde(default, skip_serializing_if = $skip)] pub $field: $patch,)*
        }
        /// Manifest property keys, generated from exactly the same field schema.
        pub const PROPERTY_KEYS: [&str; [$(stringify!($field),)*].len()] = [$(stringify!($field),)*];
        impl PropertiesPatch {
            pub fn apply(&self, properties: &mut AdapterProperties) {
                $(apply_property!(self, properties, $field, $apply);)*
            }
        }
    }
}
adapter_properties! {
    /// Supports the gateway's read-only tools for explicitly attached files.
    read_attached: bool = false => Option<bool>, "Option::is_none", copied;
    /// Its read-only mode is enforced by the harness's own sandbox.
    self_read_only: bool = false => Option<bool>, "Option::is_none", copied;
    /// It runs inside the Node's OS sandbox (macOS cannot nest a harness's own one in it).
    nested_sandbox: bool = true => Option<bool>, "Option::is_none", copied;
    /// Where the harness keeps its configuration, logins and history, when it can be moved: what
    /// gives each principal its own (`None`: it cannot, and every run uses the Node's).
    state: Option<StateDir> = None => Option<StateDir>, "Option::is_none", state;

    modes: ModeMap = ModeMap::default() => BTreeMap<String, String>, "BTreeMap::is_empty", modes;
    /// Send `session/set_mode` even when `session/new|load` reports no `availableModes`, and offer
    /// modes with non-empty ids (or `mode_meta` entries) without an `availableModes` list.
    blind_set_mode: bool = false => Option<bool>, "Option::is_none", copied;
    /// The adapter runs native tools without approval modes. Only an explicitly selected
    /// FullAccess is supported; its model/thinking options are never permission modes.
    native_execution: bool = false => Option<bool>, "Option::is_none", copied;

    permission_policy: PermissionPolicy = PermissionPolicy::Acp => Option<PermissionPolicy>, "Option::is_none", copied;

    require_mcp_http: bool = false => Option<bool>, "Option::is_none", copied;
    /// Per-Steve-mode (`plan`, `ask`, `accept_edits`, `auto`, `full_access`) `_meta` sent on
    /// `session/new|load` (for adapters whose higher autonomy modes are session `_meta` flags).
    mode_meta: BTreeMap<String, Value> = BTreeMap::new() => BTreeMap<String, Value>, "BTreeMap::is_empty", mode_meta;
    /// The variable that receives the configured harness executable (`claude_executable`).
    executable_env: Option<String> = None => Option<String>, "Option::is_none", optional_string;

    session_meta: SessionMeta = SessionMeta::None => Option<SessionMeta>, "Option::is_none", copied;

    launch_extra: LaunchExtra = LaunchExtra::None => Option<LaunchExtra>, "Option::is_none", copied;
    /// Discovery asks it `initialize` and opens a scratch session (`ACP initialize probe`), at most every half
    /// hour.
    probe_initialize: bool = true => Option<bool>, "Option::is_none", copied;
    /// What `session/new|load` adds to its `_meta` to switch the harness's built-in tools off for
    /// a call that uses none (a model call of Steve's own: cheaper, and nothing to guard).
    tools_off_meta: Option<Value> = None => Option<Value>, "Option::is_none", json;
    /// Its `inputTokens` include the cache reads (OpenAI's convention).
    usage_input_includes_cache: bool = false => Option<bool>, "Option::is_none", copied;
    /// Its `PromptResponse.usage` reports cumulative session token totals rather than per-turn
    /// deltas (`mistral-vibe`), so multi-turn sessions subtract the prior session high-water mark.
    usage_prompt_cumulative: bool = false => Option<bool>, "Option::is_none", copied;
    /// Where a call's tokens come from when the adapter reports none.
    usage_fallback: UsageFallback = UsageFallback::None => Option<UsageFallback>, "Option::is_none", copied;
    /// The models to offer before (or without) a probe of the adapter.
    fallback_models: Vec<String> = vec![] => Option<Vec<String>>, "Option::is_none", cloned;
    /// The reasoning efforts to offer before (or without) a probe of the adapter.
    fallback_efforts: Vec<String> = vec![] => Option<Vec<String>>, "Option::is_none", cloned;
    /// Steve's judgments fall back down Steve's model lists (`STEVE_MODELS_SMART`) after the bot's
    /// model; without it they try the bot's model, else the adapter's own default.
    model_fallback: bool = false => Option<bool>, "Option::is_none", copied;
    /// Words that name its quick, cheap models, best first (`haiku`): of the models the adapter
    /// itself offers, the newest whose name has one as a whole word is what Steve's cheap, frequent
    /// judgments (routing, titles) are put on once. None: they follow Steve's model like everything
    /// else.
    fast_models: Vec<String> = vec![] => Option<Vec<String>>, "Option::is_none", cloned;
    /// Where it reads skills (`<name>/SKILL.md`, a Markdown file with `name` and `description` in
    /// its front matter): a directory under its own state directory ([`Self::state`]). A Bot's
    /// skills are put there before its session opens. `None`: it reads none Steve can give it.
    skills: Option<String> = None => Option<String>, "Option::is_none", trimmed_string;
    /// Command template to resume a session in a terminal (`{id}` is replaced by the quoted ACP
    /// session id, e.g. `"claude --resume {id}"`).
    resume_cli: Option<String> = None => Option<String>, "Option::is_none", trimmed_string;
    /// Command template to fork a session in a terminal (`{id}` is replaced by the quoted ACP
    /// session id, e.g. `"claude --resume {id} --fork-session"`).
    fork_cli: Option<String> = None => Option<String>, "Option::is_none", trimmed_string;
    /// Extra CLI arguments appended when the adapter runs inside Steve's outer OS sandbox
    /// (for example `--no-sandbox` so Copilot CLI disables its nested inner sandbox rather than
    /// aborting inside Seatbelt or Bubblewrap).
    outer_sandbox_args: Vec<String> = vec![] => Option<Vec<String>>, "Option::is_none", cloned;
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

impl Declaration {
    /// Mode keys ignored by resolution, for diagnostics owned by the runtime or its caller.
    pub fn ignored_mode_keys(&self) -> impl Iterator<Item = &str> {
        self.properties
            .modes
            .keys()
            .map(String::as_str)
            .filter(|key| {
                !matches!(
                    *key,
                    "plan" | "ask" | "accept_edits" | "auto" | "full_access"
                )
            })
    }
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

/// The adapter's mode id for one of Steve's permission modes, and what may stand in for it when
/// the adapter does not offer it ([`ModeMap::choice`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModeChoice {
    /// Steve's canonical mode key (`plan`, `ask`, `accept_edits`, `auto`, `full_access`) for
    /// error diagnostics when `id` is empty.
    pub requested: &'static str,
    pub id: String,
    /// A narrower mode that may be set instead (Plan asks before every change when the adapter
    /// has no plan mode).
    pub fallback: Option<String>,
    /// The mode only narrows what the adapter does by itself (Plan, Ask): an adapter that reports
    /// no modes at all asks for every permission, so nothing needs to be set. A wider mode is
    /// refused there.
    pub narrow: bool,
    /// Send `session/set_mode` even when `available` is `None` (`blind_set_mode`).
    pub blind: bool,
    /// `_meta` sent on `session/new|load` for this mode (`mode_meta`).
    pub meta: Option<Value>,
    /// Native tool execution has no harness approval modes. It must be explicitly selected.
    pub native_execution: bool,
}

impl ModeChoice {
    fn label(&self) -> &str {
        if self.id.is_empty() {
            self.requested
        } else {
            &self.id
        }
    }

    /// The id to set given the modes the adapter offers (`None`: it reports none): `Ok(None)` sets
    /// nothing; an error when the mode cannot be had.
    pub fn resolve(&self, available: Option<&[String]>) -> Result<Option<String>, String> {
        if self.native_execution {
            return if self.requested == "full_access" {
                Ok(None)
            } else {
                Err(format!(
                    "unsupported: permission mode {} (the adapter executes native tools without approval modes)",
                    self.requested
                ))
            };
        }
        let non_empty = |s: &str| (!s.is_empty()).then(|| s.to_owned());
        match available {
            None => {
                if self.blind {
                    if let Some(id) = non_empty(&self.id) {
                        return Ok(Some(id));
                    }
                    if let Some(f) = self.fallback.as_deref().and_then(non_empty) {
                        return Ok(Some(f));
                    }
                }
                let has_mapping =
                    !self.id.is_empty() || self.fallback.as_deref().is_some_and(|f| !f.is_empty());
                if self.meta.is_some() || (self.narrow && has_mapping) {
                    Ok(None)
                } else {
                    Err(format!("unsupported: permission mode {}", self.label()))
                }
            }
            Some(ids) if !self.id.is_empty() && ids.contains(&self.id) => Ok(Some(self.id.clone())),
            Some(ids) => match &self.fallback {
                Some(f) if !f.is_empty() && ids.contains(f) => Ok(Some(f.clone())),
                _ if self.meta.is_some() => Ok(None),
                _ => Err(format!(
                    "unsupported: permission mode {} (the adapter offers {})",
                    self.label(),
                    ids.join(", ")
                )),
            },
        }
    }
}

mod catalog;
pub use catalog::*;
