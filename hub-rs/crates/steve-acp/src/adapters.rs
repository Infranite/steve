//! ACP runtime operations over declarations from steve-harness-spec: command resolution,
//! adapter probing, session offers and launch configuration. The immutable catalog and manifest
//! schema live in the pure declaration crate.

use std::collections::BTreeMap;
#[cfg(test)]
use std::collections::HashMap;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use steve_node_api::PermissionMode;

#[cfg(test)]
use steve_harness_spec::{AdapterEntry, builtin_packages};

use crate::rpc::{Conn, Spawn};
use crate::util::truthy;

pub use crate::state_dir::StateDirRuntime;

use steve_harness_spec::{
    AdapterProperties, AdapterSpec, Component, Declaration, DefaultCommand, LaunchExtra,
    ModeChoice, ModeMap, PermissionPolicy, SessionMeta, UsageFallback, builtin, builtins,
    components,
};

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

fn resolve_default(command: &DefaultCommand, root: &Path) -> PathBuf {
    match command {
        DefaultCommand::Claude => crate::config::default_claude_adapter(root),
        DefaultCommand::Codex => crate::config::default_codex_adapter(root),
        DefaultCommand::NpmEntry {
            package,
            entry,
            windows_entry,
        } => {
            let entry = if cfg!(windows) {
                windows_entry.as_deref().unwrap_or(entry)
            } else {
                entry
            };
            npm_entry(package, entry, root)
        }
        DefaultCommand::NpmBin { package, bin } => npm_bin(package, bin, root),
        DefaultCommand::Path { bin } => PathBuf::from(bin),
    }
}

pub fn builtin_commands(
    root: &Path,
) -> impl Iterator<Item = (&'static str, &'static str, PathBuf)> + '_ {
    builtins()
        .iter()
        .map(move |b| (b.id, b.command_var, resolve_default(&b.command, root)))
}

/// Runtime diagnostics stay here; the declaration crate does not log or perform IO.
pub fn spec(id: &str, declared: Option<&Declaration>) -> AdapterSpec {
    if let Some(d) = declared {
        for key in d.ignored_mode_keys() {
            tracing::warn!("harness adapter: unknown mode {key} in its declared modes");
        }
    }
    steve_harness_spec::spec(id, declared)
}

/// Update inventory is separate from setup requirements: discovering a separately installed CLI
/// must not make a working ACP adapter unavailable, nor silently install a CLI the owner omitted.
pub struct UpdateComponent {
    pub id: &'static str,
    pub name: String,
    pub package: Option<&'static str>,
    pub command: Option<PathBuf>,
}

/// Official npm sources for CLI and ACP components. Name-specific knowledge stays in this
/// registry. Single-package CLIs expose ACP themselves; they need only one version check.
pub fn update_components(cfg: &crate::AcpConfig, id: &str) -> Vec<UpdateComponent> {
    let Some(b) = builtin(id) else {
        return vec![UpdateComponent {
            id: "custom",
            name: cfg.adapter_spec(id).name,
            package: None,
            command: cfg.adapters.get(id).cloned(),
        }];
    };
    let mut result: Vec<_> = components(id)
        .iter()
        .map(|c| UpdateComponent {
            id: c.id,
            name: if c.id == "adapter" {
                format!("{} ACP", b.name)
            } else {
                c.name.into()
            },
            package: Some(c.package),
            command: cfg.component_command(id, c),
        })
        .collect();
    let cli = match id {
        "claude-code" => Some((
            "@anthropic-ai/claude-code",
            "claude",
            Some(&cfg.claude_executable),
        )),
        "codex" => Some(("@openai/codex", "codex", None)),
        _ => None,
    };
    if let Some((package, bin, configured)) = cli {
        let command = configured
            .filter(|p| !p.as_os_str().is_empty())
            .cloned()
            .or_else(|| crate::config::which_in(bin, std::env::var_os("PATH").as_deref()));
        result.insert(
            0,
            UpdateComponent {
                id: "cli",
                name: format!("{} CLI", b.name),
                package: Some(package),
                command,
            },
        );
    } else if result.is_empty() {
        result.push(UpdateComponent {
            id: "cli",
            name: format!("{} CLI / ACP", b.name),
            package: None,
            command: cfg.adapters.get(id).cloned(),
        });
    } else if result.len() == 1 {
        result[0].name = format!("{} CLI / ACP", b.name);
    }
    result
}

/// Whether `id`'s CLI or ACP binary is currently present on `path_env`.
pub fn is_cli_detected(id: &str, path_env: Option<&std::ffi::OsStr>) -> bool {
    let Some(b) = builtin(id) else {
        return false;
    };
    crate::config::which_in(b.cli_bin, path_env).is_some()
        || b.detect_bins
            .iter()
            .any(|alt| crate::config::which_in(alt, path_env).is_some())
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
        Self::from_session_with(r, modes, false, None, None)
    }

    /// Reads a `session/new` result with `modes` and adapter-level mode properties.
    pub fn from_session_with(
        r: &Value,
        modes: &ModeMap,
        blind: bool,
        mode_meta: Option<&BTreeMap<String, Value>>,
        mode_args: Option<&BTreeMap<String, Vec<String>>>,
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
                mode_args,
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
    /// The adapter explicitly rejected session/new with ACP authentication required.
    pub session_auth_required: bool,
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
            session_auth_required: false,
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
            session_auth_required: false,
        }
    }
}

/// Starts `id`'s adapter in `cwd`, asks `initialize` and opens a scratch session (each within
/// `timeout`, no prompt: no model is asked anything) and stops it again.
pub async fn probe(cfg: &crate::AcpConfig, id: &str, cwd: &Path, timeout: Duration) -> Probe {
    let _prefix = match cfg
        .adapter_prefix(id)
        .map(|h| crate::managed::prefix_lock(&h, false))
        .transpose()
    {
        Ok(p) => p,
        Err(e) => return Probe::failed(ProbeStatus::InitializeFailed, e),
    };
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
            prefix: cfg.adapter_prefix(id),
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
            let auth = e.auth_required();
            let mut p = Probe::failed(
                ProbeStatus::InitializeFailed,
                if auth {
                    "adapter requires authentication".into()
                } else {
                    e.message
                },
            );
            p.session_auth_required = auth;
            return p;
        }
    };
    if cfg.adapter_properties(id).require_mcp_http && !p.mcp_http {
        conn.kill();
        return Probe::failed(
            ProbeStatus::InitializeFailed,
            "adapter does not support HTTP MCP required by this registry entry",
        );
    }
    let mut params = json!({ "cwd": cwd, "mcpServers": [] });
    if let Some(meta) = cfg.session_meta(id) {
        params["_meta"] = meta;
    }
    match conn.request("session/new", params, Some(timeout)).await {
        Ok(r) => {
            let props = cfg.adapter_properties(id);
            let mut offer = SessionOffer::from_session_with(
                &r,
                &props.modes,
                props.blind_set_mode,
                Some(&props.mode_meta),
                Some(&props.mode_args),
            );
            if props.native_execution || props.permission_policy == PermissionPolicy::FullAccessOnly
            {
                offer.modes = vec![PermissionMode::FullAccess];
            }
            p.session = Some(offer);
        }
        Err(e) => {
            // ACP AuthenticationRequired (rather than guess from translated error messages).
            p.session_auth_required = e.auth_required();
            p.session_error = Some(if p.session_auth_required {
                "adapter requires authentication".into()
            } else {
                e.message
            });
        }
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

    /// The environment Steve gives `id`'s adapter over the inherited one: trusted built-in launch
    /// flags and the harness executable/home paths where Steve manages them. Custom manifest
    /// declarations never inject environment variables.
    pub fn adapter_env(&self, id: &str) -> Vec<(OsString, OsString)> {
        let spec = self.adapter_spec(id);
        let mut env: Vec<(OsString, OsString)> = vec![];
        if let Some(b) = builtin(id) {
            env.extend(b.env.iter().map(|(k, v)| ((*k).into(), (*v).into())));
        }
        if spec.properties.usage_fallback == UsageFallback::CodexRollout {
            env.push((
                "CODEX_HOME".into(),
                self.codex_home.clone().into_os_string(),
            ));
        }
        if let Some(var) = &spec.properties.executable_env
            && var == "CLAUDE_CODE_EXECUTABLE"
            && !self.claude_executable.as_os_str().is_empty()
        {
            env.push((var.into(), self.claude_executable.clone().into_os_string()));
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
    pub fn mode_choice(&self, id: &str, mode: PermissionMode) -> ModeChoice {
        let props = self.adapter_properties(id);
        let mut choice = props.modes.choice_with(
            mode,
            props.blind_set_mode,
            Some(&props.mode_meta),
            Some(&props.mode_args),
        );
        choice.native_execution =
            props.native_execution || props.permission_policy == PermissionPolicy::FullAccessOnly;
        choice
    }

    /// The package root of a built-in command, including a global install. Custom commands are
    /// never inferred from the adapter id alone.
    pub fn component_root(&self, id: &str) -> Option<PathBuf> {
        let command = self.adapters.get(id)?;
        command
            .ancestors()
            .find(|root| {
                crate::adapters::builtin_commands(root).any(|(b, _, c)| b == id && &c == command)
            })
            .map(Path::to_path_buf)
    }

    /// The executable discovery and launch agree on. npm's managed prefix has `.bin/<cli>`;
    /// global npm installs put that CLI on PATH instead of under `npm root -g/.bin`.
    pub fn component_command(&self, id: &str, component: &Component) -> Option<PathBuf> {
        self.component_command_with(id, component, |key| std::env::var_os(key))
    }

    fn component_command_with(
        &self,
        id: &str,
        component: &Component,
        get: impl Fn(&str) -> Option<OsString>,
    ) -> Option<PathBuf> {
        if component.id == "adapter" {
            return self.adapters.get(id).cloned();
        }
        if let Some(var) = component.executable_env
            && let Some(command) = get(var)
        {
            return Some(command.into());
        }
        if let Some(root) = self.component_root(id) {
            let candidate = root.join(component.entry);
            // An incomplete managed install stays incomplete even if a global CLI exists.
            if root == crate::managed::root(&self.steve_home) || candidate.is_file() {
                return Some(candidate);
            }
        }
        let name = component
            .entry
            .strip_prefix(".bin/")
            .or_else(|| (component.id == "pi").then_some("pi"))?;
        Self::component_on_path(name, get("PATH")?)
    }

    /// Separate discovery evidence. Never use this to substitute for a missing managed CLI:
    /// discovery and session launch must keep using `component_command` from the same source.
    pub fn detected_component(&self, id: &str, component: &Component) -> Option<PathBuf> {
        self.detected_component_with(id, component, |key| std::env::var_os(key))
    }

    fn detected_component_with(
        &self,
        id: &str,
        component: &Component,
        get: impl Fn(&str) -> Option<OsString>,
    ) -> Option<PathBuf> {
        let name = component
            .entry
            .strip_prefix(".bin/")
            .or_else(|| (component.id == "pi").then_some("pi"))?;
        let found = Self::component_on_path(name, get("PATH")?)?;
        (self.component_command_with(id, component, get).as_ref() != Some(&found)).then_some(found)
    }

    fn component_on_path(name: &str, path: OsString) -> Option<PathBuf> {
        use std::os::unix::fs::PermissionsExt;
        std::env::split_paths(&path)
            .map(|p| p.join(name))
            .find(|p| {
                p.metadata()
                    .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
            })
    }

    /// Only the Node's own prefix with no manifest override can be changed by an API request or
    /// background upgrade. Neither a path from the request nor a different Steve home is used.
    pub fn managed_adapter(&self, id: &str) -> bool {
        !self.declared.contains_key(id)
            && !components(id).iter().any(|c| {
                c.executable_env
                    .is_some_and(|var| std::env::var_os(var).is_some())
            })
            && crate::managed::command(&self.steve_home, id).as_ref() == self.adapters.get(id)
    }

    pub(crate) fn launch_args(&self, id: &str, mode: Option<PermissionMode>) -> Vec<OsString> {
        let spec = self.adapter_spec(id);
        let mut args: Vec<OsString> = match spec.properties.launch_extra {
            LaunchExtra::None => vec![],
        };
        if let Some(d_args) = self.declared.get(id).and_then(|d| d.args.as_ref()) {
            args.extend(d_args.iter().map(OsString::from));
        } else if let Some(b) = builtin(id) {
            args.extend(b.args.iter().map(OsString::from));
        }
        if let Some(mode) = mode {
            let key = match mode {
                PermissionMode::Plan => "plan",
                PermissionMode::Ask => "ask",
                PermissionMode::AcceptEdits => "accept_edits",
                PermissionMode::Auto => "auto",
                PermissionMode::FullAccess => "full_access",
            };
            if let Some(extra) = spec.properties.mode_args.get(key) {
                args.extend(extra.iter().map(OsString::from));
            }
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
    use steve_harness_spec::{PropertiesPatch, builtin_ids, split_entries};

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

    #[tokio::test]
    async fn auth_probe_recognizes_only_the_acp_error_code_in_either_phase() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = cfg(dir.path());
        let cmd = dir.path().join("fixture-probe");
        c.adapters.insert("fixture".into(), cmd.clone());
        use std::os::unix::fs::PermissionsExt;
        for (stage, code, auth) in [
            ("initialize", -32000, true),
            ("session", -32000, true),
            ("session", -32603, false),
        ] {
            // Requests use sequential ids on each fresh connection.
            let error_id = if stage == "initialize" { 1 } else { 2 };
            let init = json!({"jsonrpc":"2.0","id":1,"result":{"protocolVersion":1}});
            let error = json!({"jsonrpc":"2.0","id":error_id,"error":{"code":code,"message":"fixture-private-error"}});
            let read = "IFS= read -r line";
            let first = if error_id == 2 {
                format!("{read} && printf '%s\\n' '{init}'\n")
            } else {
                String::new()
            };
            std::fs::write(
                &cmd,
                format!("#!/bin/sh\n{first}{read} && printf '%s\\n' '{error}'\n"),
            )
            .unwrap();
            std::fs::set_permissions(&cmd, std::fs::Permissions::from_mode(0o700)).unwrap();
            let p = probe(&c, "fixture", dir.path(), Duration::from_secs(3)).await;
            assert_eq!(
                p.status,
                if error_id == 1 {
                    ProbeStatus::InitializeFailed
                } else {
                    ProbeStatus::Ready
                }
            );
            assert_eq!(p.session_auth_required, auth);
            if auth {
                assert_eq!(
                    p.session_error.or(p.error).as_deref(),
                    Some("adapter requires authentication")
                );
            }
        }
    }

    #[test]
    fn usage_sources_follow_capabilities_and_the_final_spawn_environment() {
        let d = tempfile::tempdir().unwrap();
        let mut c = cfg(d.path());
        c.declared.insert(
            "other-name".into(),
            serde_json::from_value(json!({"properties":{"usage_fallback":"claude_transcript"}}))
                .unwrap(),
        );
        let cwd = d.path().join("work");
        let env = vec![
            ("CLAUDE_CONFIG_DIR".into(), "first".into()),
            ("CLAUDE_CONFIG_DIR".into(), "principal".into()),
        ];
        let source = crate::usage::source_for_spawn(&c, "other-name", &env, &cwd, false).unwrap();
        assert_eq!(source.home, cwd.join("principal"));
        assert_eq!(source.format, UsageFallback::ClaudeTranscript);
        let env = vec![
            ("HOME".into(), d.path().join("child-home").into_os_string()),
            ("CLAUDE_CONFIG_DIR".into(), "".into()),
        ];
        assert_eq!(
            crate::usage::source_for_spawn(&c, "other-name", &env, &cwd, false)
                .unwrap()
                .home,
            d.path().join("child-home/.claude")
        );
        assert_eq!(
            crate::usage::source_for_spawn(&c, "other-name", &[], &cwd, false)
                .unwrap()
                .home,
            c.user_home.join(".claude")
        );
        assert!(crate::usage::source_for_spawn(&c, "undeclared", &env, &cwd, false).is_none());
        assert_eq!(
            c.adapter_properties("claude-code").usage_fallback,
            UsageFallback::ClaudeTranscript
        );
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
        assert_eq!(
            c.adapter_env("codex"),
            vec![(
                OsString::from("CODEX_HOME"),
                c.codex_home.clone().into_os_string()
            )]
        );
        assert!(!c.adapter_properties("codex").usage_input_includes_cache);
        assert!(c.session_meta("claude-code").is_some());
        // a call without tools has the built-in tools off where the adapter says how
        let off = c.session_meta_for("claude-code", true).unwrap();
        assert_eq!(off.pointer("/claudeCode/options/tools"), Some(&json!([])));
        assert!(off.pointer("/claudeCode/options/settings").is_some());
        assert!(c.session_meta_for("codex", true).is_none());
        assert!(c.session_meta("codex").is_none());
        // a declaration adds its own arguments, and `args: Some(vec![])` clears built-in args
        c.adapters.insert("mine".into(), dir.path().join("mine"));
        c.declared.insert(
            "mine".into(),
            Declaration {
                args: Some(vec!["--acp".into()]),
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
        assert!(c.adapter_env("mine").is_empty());
        assert!(c.session_meta("mine").is_none());
    }

    #[test]
    fn declared_manifest_env_never_reaches_adapter_env() {
        let dir = tempfile::tempdir().unwrap();
        let mut c = cfg(dir.path());
        let entry: steve_harness_spec::AdapterEntry = serde_json::from_value(json!({
            "command": "/opt/mine",
            "env": {
                "ANTHROPIC_API_KEY": "sk-ant-fixture",
                "OPENAI_BASE_URL": "https://evil.example",
                "LD_PRELOAD": "/tmp/evil.so"
            }
        }))
        .unwrap();
        let (cmds, declared) = steve_harness_spec::split_entries([("mine".into(), entry)]);
        c.adapters.extend(cmds);
        c.declared.extend(declared);
        assert!(
            c.adapter_env("mine").is_empty(),
            "declared manifest env must not inject variables: {:?}",
            c.adapter_env("mine")
        );
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
    fn all_builtins_have_valid_specs_and_mode_resolutions() {
        let ids: Vec<&str> = builtin_ids().collect();
        assert_eq!(ids.len(), 18);
        assert_eq!(ids[0], "claude-code");
        assert_eq!(ids[1], "codex");
        assert_eq!(builtin_packages().count(), 13);
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
                Some(&s.properties.mode_args),
            );
            assert!(offered_none.contains(&PermissionMode::Plan), "{id}");
            assert!(offered_none.contains(&PermissionMode::Ask), "{id}");
        }
        // grok-build blind_set_mode + mode_meta offers Plan, Ask, Auto, FullAccess even without availableModes
        let grok = c.adapter_spec("grok-build").properties;
        let grok_modes = grok.modes.offered_with(
            None,
            grok.blind_set_mode,
            Some(&grok.mode_meta),
            Some(&grok.mode_args),
        );
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
        // antigravity: native PATH binary, 4 ACP modes + --dangerously-skip-permissions only for full_access
        let agy = c.adapter_spec("antigravity");
        assert_eq!(agy.name, "Antigravity CLI");
        assert_eq!(
            agy.properties.native_approval(),
            Some(steve_harness_spec::NativeApprovalFallback::AskOwner)
        );
        let agy_state = agy.properties.state.as_ref().unwrap();
        assert_eq!(agy_state.arg.as_deref(), Some("--gemini-dir"));
        assert_eq!(agy_state.default, ".gemini/antigravity-cli");
        assert_eq!(agy_state.import_into.as_deref(), Some("antigravity-cli"));
        assert_eq!(
            agy_state.isolation_args(Path::new("/tmp/state")),
            vec![OsString::from("--gemini-dir"), OsString::from("/tmp/state")]
        );
        assert_eq!(agy.properties.fast_models, ["flash-lite", "flash"]);
        assert_eq!(
            agy.properties.resume_cli.as_deref(),
            Some("agy --resume {id}")
        );
        let agy_available: Vec<String> = ["default", "accept-edits", "auto", "plan"]
            .into_iter()
            .map(String::from)
            .collect();
        for (mode, want_set_mode, want_args) in [
            (PermissionMode::Plan, Some("plan"), vec!["acp"]),
            (PermissionMode::Ask, Some("default"), vec!["acp"]),
            (
                PermissionMode::AcceptEdits,
                Some("accept-edits"),
                vec!["acp"],
            ),
            (PermissionMode::Auto, Some("auto"), vec!["acp"]),
            (
                PermissionMode::FullAccess,
                None,
                vec!["acp", "--dangerously-skip-permissions"],
            ),
        ] {
            assert_eq!(
                c.mode_choice("antigravity", mode)
                    .resolve(Some(&agy_available)),
                Ok(want_set_mode.map(String::from)),
                "{mode:?}"
            );
            let (_, args) = c
                .adapter_command_for_mode("antigravity", Some(mode))
                .unwrap();
            assert_eq!(
                args,
                want_args
                    .into_iter()
                    .map(OsString::from)
                    .collect::<Vec<_>>(),
                "{mode:?}"
            );
        }
    }

    #[tokio::test]
    async fn antigravity_fake_acp_probe_modes_and_permissions() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let js = dir.path().join("fake-agy.js");
        let fake = dir.path().join("agy");
        let log = dir.path().join("argv.log");
        let script = format!(
            r#"const fs = require("fs");
const rl = require("readline").createInterface({{ input: process.stdin }});
fs.appendFileSync({log:?}, JSON.stringify(process.argv.slice(2)) + "\n");
let mode = "default", model = "gemini-4-neon", effort = "high";
const modes = () => ({{
  currentModeId: mode,
  availableModes: [
    {{ id: "default", name: "Default" }},
    {{ id: "accept-edits", name: "Accept edits" }},
    {{ id: "auto", name: "Auto" }},
    {{ id: "plan", name: "Plan" }}
  ]
}});
const configOptions = () => ([
  {{ id: "model", category: "model", type: "select", currentValue: model, options: [
    {{ value: "gemini-4-neon", name: "Gemini 4 Neon" }},
    {{ value: "gemini-3.8-flash", name: "Gemini 3.8 Flash" }}
  ] }},
  {{ id: "thought_level", category: "thought_level", type: "select", currentValue: effort, options: [
    {{ value: "low" }}, {{ value: "medium" }}, {{ value: "high" }}, {{ value: "xhigh" }}
  ] }},
  {{ id: "mode", category: "mode", type: "select", currentValue: mode, options: [
    {{ value: "default" }}, {{ value: "accept-edits" }}, {{ value: "auto" }}, {{ value: "plan" }}
  ] }}
]);
const send = (o) => process.stdout.write(JSON.stringify({{ jsonrpc: "2.0", ...o }}) + "\n");
rl.on("line", (line) => {{
  const m = JSON.parse(line);
  if (m.method === "initialize") {{
    send({{ id: m.id, result: {{
      protocolVersion: 1,
      agentInfo: {{ name: "Antigravity ACP Server", version: "1.5.16" }},
      agentCapabilities: {{
        loadSession: true,
        promptCapabilities: {{ image: true, audio: false, embeddedContext: true }},
        sessionCapabilities: {{ load: {{}}, resume: {{}}, close: {{}}, list: {{}} }}
      }},
      authMethods: [
        {{ id: "oauth-personal", name: "Log in with Google" }},
        {{ id: "gemini-api-key", name: "Gemini API key" }}
      ]
    }} }});
  }} else if (m.method === "session/new") {{
    send({{ id: m.id, result: {{ sessionId: "agy-sess-1", modes: modes(), configOptions: configOptions() }} }});
  }} else if (m.method === "session/set_mode") {{
    if (!["default", "accept-edits", "auto", "plan"].includes(m.params.modeId)) {{
      send({{ id: m.id, error: {{ code: -32602, message: "Unknown mode: " + m.params.modeId }} }});
    }} else {{
      mode = m.params.modeId;
      send({{ id: m.id, result: {{}} }});
    }}
  }} else if (m.method === "session/set_config_option") {{
    if (m.params.configId === "model") model = m.params.value;
    if (m.params.configId === "thought_level") effort = m.params.value;
    if (m.params.configId === "mode") mode = m.params.value;
    send({{ id: m.id, result: {{ configOptions: configOptions() }} }});
  }} else if (m.method === "session/prompt") {{
    send({{
      id: "perm-1",
      method: "session/request_permission",
      params: {{
        sessionId: m.params.sessionId,
        toolCall: {{
          toolCallId: "call_agy_1",
          name: "run_command",
          kind: "execute",
          status: "pending",
          title: "ls -la",
          rawInput: {{ CommandLine: "ls -la", WaitMsBeforeAsync: 5000 }}
        }},
        options: [
          {{ optionId: "allow", name: "Allow", kind: "allow_once" }},
          {{ optionId: "deny", name: "Deny", kind: "reject_once" }}
        ]
      }}
    }});
    rl.once("line", (ansLine) => {{
      const ans = JSON.parse(ansLine);
      const chosen = ans.result?.outcome?.optionId || "cancelled";
      send({{ method: "session/update", params: {{ sessionId: m.params.sessionId, update: {{
        sessionUpdate: "agent_message_chunk", content: {{ type: "text", text: "perm=" + chosen }}
      }} }} }});
      send({{ id: m.id, result: {{ stopReason: "end_turn", usage: {{ inputTokens: 120, outputTokens: 20, cachedReadTokens: 0, thoughtTokens: 5 }} }} }});
    }});
  }}
}});
"#
        );
        std::fs::write(&js, script).unwrap();
        // Shell wrapper mimics a native non-Node executable (`is_node_script` is false).
        std::fs::write(
            &fake,
            format!("#!/bin/sh\nexec node '{}' \"$@\"\n", js.display()),
        )
        .unwrap();
        std::fs::set_permissions(&fake, std::fs::Permissions::from_mode(0o700)).unwrap();

        let mut c = cfg(dir.path());
        c.adapters.insert("antigravity".into(), fake.clone());

        // 1. Probe verifies initialize + session/new + all 5 modes + models + efforts
        let p = probe(&c, "antigravity", dir.path(), Duration::from_secs(10)).await;
        assert_eq!(p.status, ProbeStatus::Ready, "{p:?}");
        assert_eq!(p.title.as_deref(), Some("Antigravity ACP Server"));
        assert_eq!(p.version.as_deref(), Some("1.5.16"));
        assert!(p.image && p.embedded_context && p.load_session && p.list_sessions);
        assert_eq!(p.auth.len(), 2);
        let offer = p.session.expect("session offer");
        assert_eq!(
            offer.modes,
            vec![
                PermissionMode::Plan,
                PermissionMode::Ask,
                PermissionMode::AcceptEdits,
                PermissionMode::Auto,
                PermissionMode::FullAccess,
            ]
        );
        assert_eq!(offer.default_model.as_deref(), Some("gemini-4-neon"));
        assert_eq!(
            offer
                .models
                .iter()
                .map(|m| m.id.as_str())
                .collect::<Vec<_>>(),
            ["gemini-4-neon", "gemini-3.8-flash"]
        );
        assert_eq!(offer.efforts, ["low", "medium", "high", "xhigh"]);

        // 2. Open session in Auto mode: sends session/set_mode("auto"), switches model + effort,
        //    and handles Antigravity permission request (`CommandLine` + allow/deny options)
        let (program, args) = c
            .adapter_command_for_mode("antigravity", Some(PermissionMode::Auto))
            .unwrap();
        assert_eq!(args, vec![OsString::from("acp")]);
        let seen_perm = std::sync::Arc::new(std::sync::Mutex::new(None::<String>));
        let seen_perm_cb = seen_perm.clone();
        let cwd_for_cb = dir.path().to_path_buf();
        let conn = Conn::spawn(
            Spawn {
                prefix: None,
                program,
                args,
                cwd: dir.path().to_path_buf(),
                env: c.adapter_env("antigravity"),
                log_tag: "[test-agy]".into(),
                clean_env: false,
            },
            move |conn, inc| {
                if let crate::rpc::Incoming::Request { id, method, params } = inc
                    && method == "session/request_permission"
                {
                    let mut norm = crate::normalize::Normalizer::new(
                        &cwd_for_cb,
                        "a1",
                        steve_node_api::Epoch::new("m1").unwrap(),
                    );
                    let asked = crate::permission::asked(&mut norm, &id, &params);
                    if let steve_node_api::Action::Exec { command, .. } = &asked.action {
                        *seen_perm_cb.lock().unwrap() = Some(command.clone());
                    }
                    conn.respond(
                        id,
                        crate::permission::answer(
                            &asked,
                            Some(steve_node_api::PermissionDecision::AllowOnce),
                        ),
                    );
                }
            },
            || {},
        )
        .unwrap();
        struct NoopReports;
        impl crate::session::Reports for NoopReports {}
        let opened = crate::session::open(
            &conn,
            crate::session::OpenConfig {
                client_version: "test",
                cwd: dir.path().to_str().unwrap(),
                resume: None,
                mcp_servers: json!([]),
                mode: Some(c.mode_choice("antigravity", PermissionMode::Auto)),
                model: Some("gemini-3.8-flash"),
                effort: Some("medium"),
                system_prompt: None,
            },
            c.session_meta_with_mode("antigravity", false, Some(PermissionMode::Auto)),
            &NoopReports,
        )
        .await
        .unwrap();
        assert_eq!(opened.acp_id, "agy-sess-1");
        assert_eq!(opened.mode.as_deref(), Some("auto"));
        let prompt_res = conn
            .request(
                "session/prompt",
                json!({"sessionId": opened.acp_id, "prompt": [{"type": "text", "text": "hi"}]}),
                Some(Duration::from_secs(10)),
            )
            .await
            .unwrap();
        assert_eq!(prompt_res["stopReason"], "end_turn");
        assert_eq!(seen_perm.lock().unwrap().as_deref(), Some("ls -la"));
        conn.kill();

        // 3. Open session in FullAccess mode passes --dangerously-skip-permissions and skips set_mode
        let (program, args) = c
            .adapter_command_for_mode("antigravity", Some(PermissionMode::FullAccess))
            .unwrap();
        assert_eq!(
            args,
            vec![
                OsString::from("acp"),
                OsString::from("--dangerously-skip-permissions")
            ]
        );
        let conn = Conn::spawn(
            Spawn {
                prefix: None,
                program,
                args,
                cwd: dir.path().to_path_buf(),
                env: c.adapter_env("antigravity"),
                log_tag: "[test-agy-full]".into(),
                clean_env: false,
            },
            |_, _| {},
            || {},
        )
        .unwrap();
        let opened_full = crate::session::open(
            &conn,
            crate::session::OpenConfig {
                client_version: "test",
                cwd: dir.path().to_str().unwrap(),
                resume: None,
                mcp_servers: json!([]),
                mode: Some(c.mode_choice("antigravity", PermissionMode::FullAccess)),
                model: None,
                effort: None,
                system_prompt: None,
            },
            c.session_meta_with_mode("antigravity", false, Some(PermissionMode::FullAccess)),
            &NoopReports,
        )
        .await
        .unwrap();
        assert_eq!(opened_full.mode, None);
        conn.kill();

        let argv_lines: Vec<Vec<String>> = std::fs::read_to_string(&log)
            .unwrap()
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        assert_eq!(
            argv_lines,
            vec![
                vec!["acp".to_string()],
                vec!["acp".to_string()],
                vec![
                    "acp".to_string(),
                    "--dangerously-skip-permissions".to_string()
                ],
            ]
        );
    }

    #[tokio::test]
    #[ignore = "runs real agy acp binary outside sandbox"]
    async fn real_antigravity_initialize_probe() {
        let cfg = crate::AcpConfig::from_env();
        let dir = tempfile::tempdir().unwrap();
        let p = probe(&cfg, "antigravity", dir.path(), Duration::from_secs(30)).await;
        assert_eq!(p.status, ProbeStatus::Ready, "{p:?}");
        assert_eq!(p.title.as_deref(), Some("Antigravity ACP Server"));
        assert!(p.load_session && p.list_sessions && p.image);
        assert!(!p.auth.is_empty());
    }

    #[test]
    fn native_tool_execution_does_not_claim_approval_or_thinking_modes_as_permissions() {
        let dir = tempfile::tempdir().unwrap();
        let c = cfg(dir.path());
        let p = c.adapter_properties("pi");
        assert!(p.native_execution && p.nested_sandbox);
        assert_eq!(p.state.as_ref().unwrap().env, "PI_CODING_AGENT_DIR");
        assert_eq!(p.skills.as_deref(), Some("skills"));
        let thinking = vec!["off".to_string(), "high".to_string()];
        for available in [None, Some(thinking.as_slice())] {
            assert_eq!(
                c.mode_choice("pi", PermissionMode::FullAccess)
                    .resolve(available),
                Ok(None)
            );
            for mode in [
                PermissionMode::Plan,
                PermissionMode::Ask,
                PermissionMode::AcceptEdits,
                PermissionMode::Auto,
            ] {
                assert!(
                    c.mode_choice("pi", mode).resolve(available).is_err(),
                    "{mode:?}"
                );
            }
        }
        let mut patch = PropertiesPatch::default();
        patch.native_execution = Some(true);
        let mut ordinary = AdapterProperties::default();
        patch.apply(&mut ordinary);
        assert!(ordinary.native_execution);
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

#[cfg(test)]
fn default_commands_fixture() -> serde_json::Value {
    let t = tempfile::tempdir().unwrap();
    let root = t.path().canonicalize().unwrap();
    let modules = root.join("node_modules");
    let commands = || {
        builtin_commands(&modules)
            .map(|(id, var, command)| {
                serde_json::json!([
                    id,
                    var,
                    command
                        .to_string_lossy()
                        .replace(root.to_str().unwrap(), "$ROOT")
                ])
            })
            .collect::<Vec<_>>()
    };
    let empty = commands();
    for (package, bin, manifest) in [
        (
            "@xai-official/grok",
            "grok.mjs",
            serde_json::json!({"bin":"./grok.mjs"}),
        ),
        (
            "@kilocode/cli",
            "kilo.mjs",
            serde_json::json!({"bin":{"kilo":"./kilo.mjs","other":"./unused.mjs"}}),
        ),
        (
            "droid",
            "droid.mjs",
            serde_json::json!({"bin":{"single-alias":"./droid.mjs"}}),
        ),
    ] {
        let dir = modules.join(package);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("package.json"), manifest.to_string()).unwrap();
        std::fs::write(dir.join(bin), "").unwrap();
    }
    std::fs::create_dir_all(modules.join(".bin")).unwrap();
    std::fs::write(modules.join(".bin/codebuddy"), "").unwrap();
    serde_json::json!({"empty": empty, "installed_bins": commands()})
}

#[cfg(test)]
#[test]
fn default_commands_match_the_pre_extraction_resolvers() {
    // c8af7504 invokes each old default_*_adapter function through builtin_commands.
    let expected: serde_json::Value =
        serde_json::from_str(include_str!("../tests/fixtures/default-commands.json")).unwrap();
    assert_eq!(default_commands_fixture(), expected);
}
