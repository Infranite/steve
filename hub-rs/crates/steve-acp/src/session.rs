//! Opening an ACP session on a started adapter: `initialize`, `session/new` or `session/load`, the
//! permission mode, the model and the reasoning effort, for the Node's driver.
//!
//! What the adapter reports comes back through [`Reports`] at the moment it is known. A mode or
//! model that could not be set is an error: the Hub asked for a configuration and must not get
//! another one silently. Reasoning effort is mapped deterministically through the adapter layer
//! ([`steve_harness_spec::effort`]) using three strategies (`acp_option`, `model_variant`,
//! `unsupported`) so an unsupported model runs at its own default with an explicit record rather
//! than failing the turn.

use std::fmt;
use std::time::Duration;

use serde_json::{Value, json};
use steve_harness_spec::{
    EffortContext, EffortMapping, EffortMode, EffortStrategy, ModeChoice, ReasoningEffort,
    parse_model_variant, resolve_for_model,
};
use steve_i18n::t;

use crate::rpc::{Conn, RpcError};
use crate::util::{coalesce, truthy};

/// Timeout for ACP `initialize` handshake.
const INITIALIZE_TIMEOUT: Duration = Duration::from_secs(60);
/// Timeout for ACP `session/new` or `session/load`.
const SESSION_OPEN_TIMEOUT: Duration = Duration::from_secs(120);
/// Timeout for ACP `session/set_mode`, `session/set_model`, and `session/set_config_option`.
const CONFIG_OPTION_TIMEOUT: Duration = Duration::from_secs(30);

/// Strongly-typed errors produced while opening and configuring an ACP session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionOpenError {
    MissingSessionId,
    ModeUnresolvable {
        detail: String,
    },
    ModeApplyFailed {
        mode: String,
        detail: String,
    },
    ModelApplyFailed {
        model: String,
        detail: String,
    },
    ModelMismatch {
        requested: String,
        actual: String,
    },
    EffortApplyFailed {
        requested: ReasoningEffort,
        applied: String,
        detail: String,
    },
    EffortMismatch {
        requested: ReasoningEffort,
        expected: String,
        actual: Option<String>,
    },
}

impl SessionOpenError {
    /// Stable machine-readable error kind for telemetry and structured RPC data.
    pub const fn code(&self) -> &'static str {
        match self {
            Self::MissingSessionId => "missing_session_id",
            Self::ModeUnresolvable { .. } => "mode_unresolvable",
            Self::ModeApplyFailed { .. } => "mode_apply_failed",
            Self::ModelApplyFailed { .. } => "model_apply_failed",
            Self::ModelMismatch { .. } => "model_mismatch",
            Self::EffortApplyFailed { .. } => "effort_apply_failed",
            Self::EffortMismatch { .. } => "effort_mismatch",
        }
    }

    /// Convert into an [`RpcError`] carrying localized user text and typed metadata.
    pub fn into_rpc_error(self) -> RpcError {
        let message = self.to_string();
        let data = match &self {
            Self::MissingSessionId => json!({ "kind": self.code() }),
            Self::ModeUnresolvable { detail } => {
                json!({ "kind": self.code(), "detail": detail })
            }
            Self::ModeApplyFailed { mode, detail } => {
                json!({ "kind": self.code(), "mode": mode, "detail": detail })
            }
            Self::ModelApplyFailed { model, detail } => {
                json!({ "kind": self.code(), "model": model, "detail": detail })
            }
            Self::ModelMismatch { requested, actual } => {
                json!({ "kind": self.code(), "requested": requested, "actual": actual })
            }
            Self::EffortApplyFailed {
                requested,
                applied,
                detail,
            } => json!({
                "kind": self.code(),
                "requested": requested.as_str(),
                "applied": applied,
                "detail": detail,
            }),
            Self::EffortMismatch {
                requested,
                expected,
                actual,
            } => json!({
                "kind": self.code(),
                "requested": requested.as_str(),
                "expected": expected,
                "actual": actual,
            }),
        };
        RpcError {
            message,
            data: Some(data),
        }
    }
}

impl fmt::Display for SessionOpenError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingSessionId => f.write_str(&t!("acp.session.missing_session_id")),
            Self::ModeUnresolvable { detail } => f.write_str(detail),
            Self::ModeApplyFailed { mode, detail } => f.write_str(&t!(
                "acp.session.mode_apply_failed",
                mode = mode,
                detail = detail
            )),
            Self::ModelApplyFailed { model, detail } => f.write_str(&t!(
                "acp.session.model_apply_failed",
                model = model,
                detail = detail
            )),
            Self::ModelMismatch { requested, actual } => f.write_str(&t!(
                "acp.session.model_mismatch",
                requested = requested,
                actual = actual
            )),
            Self::EffortApplyFailed {
                requested,
                applied,
                detail,
            } => f.write_str(&t!(
                "acp.session.effort_apply_failed",
                requested = requested.as_str(),
                applied = applied,
                detail = detail
            )),
            Self::EffortMismatch {
                requested,
                expected,
                actual,
            } => {
                let unknown = t!("acp.session.unknown");
                let actual = actual.as_deref().unwrap_or(unknown.as_ref());
                f.write_str(&t!(
                    "acp.session.effort_mismatch",
                    requested = requested.as_str(),
                    expected = expected,
                    actual = actual
                ))
            }
        }
    }
}

impl std::error::Error for SessionOpenError {}

impl From<SessionOpenError> for RpcError {
    fn from(e: SessionOpenError) -> Self {
        e.into_rpc_error()
    }
}

/// Whether a `configOptions` entry represents the session model (`id == "model"` or
/// `category == "model"`).
pub fn is_model_option(o: &Value) -> bool {
    o.get("id").and_then(Value::as_str) == Some("model")
        || o.get("category").and_then(Value::as_str) == Some("model")
}

/// The configuration option of the model, if the adapter reports one.
pub fn model_option(config: &Value) -> Option<&Value> {
    config
        .get("configOptions")?
        .as_array()?
        .iter()
        .find(|o| is_model_option(o))
}

/// The configuration option of the permission mode, if the adapter reports one.
pub fn mode_option(config: &Value) -> Option<&Value> {
    config.get("configOptions")?.as_array()?.iter().find(|o| {
        o.get("category").and_then(Value::as_str) == Some("mode")
            || o.get("id").and_then(Value::as_str) == Some("mode")
    })
}

/// The configuration option of the reasoning effort, if the adapter reports one.
pub fn effort_option(config: &Value) -> Option<&Value> {
    config.get("configOptions")?.as_array()?.iter().find(|o| {
        o.get("id").and_then(Value::as_str).is_some_and(|id| {
            ["thought_level", "reasoning_effort", "effort", "thinking"].contains(&id)
        }) || o
            .get("category")
            .and_then(Value::as_str)
            .is_some_and(|cat| ["thought_level", "thinking"].contains(&cat))
    })
}

/// What to open.
pub struct OpenConfig<'a> {
    /// Harness adapter id (used to select per-harness client capabilities during `initialize`).
    pub harness: &'a str,
    pub client_version: &'a str,
    pub cwd: &'a str,
    /// `session/load` this one; `None` starts a new session.
    pub resume: Option<&'a str>,
    /// The `mcpServers` of `session/new|load`.
    pub mcp_servers: Value,
    /// The permission mode to set (`None`: leave the adapter's).
    pub mode: Option<ModeChoice>,
    pub model: Option<&'a str>,
    /// Steve's canonical reasoning effort (`default | low | medium | high | max`).
    pub effort: ReasoningEffort,
    /// Adapter-declared effort mechanism (`acp_option | model_variant | unsupported`).
    pub effort_mode: EffortMode,
    /// Steve's instructions as the session's system prompt, sent only to an adapter that
    /// advertises `sessionCapabilities.systemPrompt` ([`Opened::system_prompt`] says whether).
    pub system_prompt: Option<SystemPrompt<'a>>,
}

fn available_modes(r: &Value) -> Option<Vec<String>> {
    r.pointer("/modes/availableModes")?
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|m| m.get("id").and_then(Value::as_str).map(String::from))
                .collect::<Vec<_>>()
        })
        .filter(|v| !v.is_empty())
}

fn collect_option_values(v: &Value, out: &mut Vec<String>) {
    for m in v.as_array().into_iter().flatten() {
        if let Some(group) = m.get("options") {
            collect_option_values(group, out);
        } else if let Some(val) = m.get("value").and_then(Value::as_str) {
            out.push(val.to_owned());
        }
    }
}

/// The mode ids a `session/new|load` response offers (`modes.availableModes`, else the `mode`
/// config option); `None` when it reports neither.
pub fn offered_modes(r: &Value) -> Option<Vec<String>> {
    available_modes(r).or_else(|| {
        let mut out = vec![];
        collect_option_values(mode_option(r)?.get("options")?, &mut out);
        (!out.is_empty()).then_some(out)
    })
}

fn collect_available_model_ids(r: &Value) -> Vec<String> {
    let mut out = Vec::new();
    let mut push_unique = |raw: &str| {
        let s = raw.trim();
        if !s.is_empty() && !out.iter().any(|existing: &String| existing == s) {
            out.push(s.to_owned());
        }
    };
    if let Some(avail) = r
        .pointer("/models/availableModels")
        .and_then(Value::as_array)
    {
        for entry in avail {
            if let Some(id) = entry.get("modelId").and_then(Value::as_str) {
                push_unique(id);
            }
        }
    }
    if let Some(opt) = model_option(r)
        && let Some(options) = opt.get("options")
    {
        let mut vals = Vec::new();
        collect_option_values(options, &mut vals);
        for v in vals {
            push_unique(&v);
        }
    }
    out
}

fn effort_option_values(config: &Value) -> Vec<String> {
    let mut out = Vec::new();
    if let Some(opt) = effort_option(config)
        && let Some(options) = opt.get("options")
    {
        collect_option_values(options, &mut out);
    }
    out
}

fn strategy_label(s: EffortStrategy) -> &'static str {
    match s {
        EffortStrategy::AcpOption => "acp_option",
        EffortStrategy::ModelVariant => "model_variant",
        EffortStrategy::Unsupported => "unsupported",
    }
}

fn record_effort_metric(harness: &str, mapping: &EffortMapping) {
    metrics::counter!(
        "steve_acp_effort_mapped_total",
        "harness" => harness.to_owned(),
        "strategy" => strategy_label(mapping.strategy),
        "requested" => mapping.requested.as_str(),
        "supported" => if mapping.supported { "true" } else { "false" },
    )
    .increment(1);
}

/// How a system prompt is delivered on `session/new|load`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemPromptMode<'a> {
    /// Standard ACP `systemPrompt` with `mode = "append"`.
    AcpAppend,
    /// Standard ACP `systemPrompt` with `mode = "override"`.
    AcpOverride,
    /// Adapter `_meta.<namespace>.systemPrompt = { "append": text }` when
    /// `agentCapabilities._meta.<namespace>.systemPrompt.append` is truthy.
    MetaAppend(&'a str),
}

/// A system prompt for the session (ACP `systemPrompt` or `_meta.<ns>.systemPrompt` on
/// `session/new|load`).
#[derive(Debug, Clone, Copy)]
pub struct SystemPrompt<'a> {
    pub text: &'a str,
    pub mode: SystemPromptMode<'a>,
}

/// Facts as they become known.
pub trait Reports: Sync {
    /// A new session was created (before anything else can fail).
    fn new_session(&self, _acp_id: &str) {}
    /// The model the adapter runs (reported on open, then after setting one).
    fn model(&self, _model: &str) {}
    /// The effective canonical reasoning effort in force.
    fn effort(&self, _effort: ReasoningEffort) {}
    /// How Steve's requested effort mapped onto this harness and model.
    fn effort_mapping(&self, _mapping: &EffortMapping) {}
}

#[derive(Debug, Clone)]
pub struct Opened {
    pub acp_id: String,
    /// The mode set (`None`: none was).
    pub mode: Option<String>,
    /// The adapter takes image blocks.
    pub image: bool,
    /// The session has [`OpenConfig::system_prompt`] as its system prompt.
    pub system_prompt: bool,
    /// Deterministic record of how [`OpenConfig::effort`] was applied (or fell back to default).
    pub effort_mapping: Option<EffortMapping>,
}

/// initialize + session/new|load + mode + model + effort, with `meta` as the `_meta` of
/// `session/new|load` (Claude Code's adapter reads its options there,
/// `claudeCode.options.settings` among them).
pub async fn open(
    l: &Conn,
    c: OpenConfig<'_>,
    meta: Option<Value>,
    reports: &dyn Reports,
) -> Result<Opened, RpcError> {
    let Initialized {
        image,
        system_prompt,
        system_prompt_json,
        system_prompt_meta,
    } = initialize_session(l, &c).await?;
    let meta = match (meta, system_prompt_meta) {
        (Some(mut base), Some(extra)) => {
            merge_json(&mut base, extra);
            Some(base)
        }
        (None, Some(extra)) => Some(extra),
        (base, None) => base,
    };
    let (r, acp_id) = open_session_request(l, &c, meta, system_prompt_json, reports).await?;
    // Claude Code reports models in `models`; Codex and other ACP adapters as a model configOption
    let model_opt = model_option(&r);
    let model_opt_id = model_opt
        .and_then(|o| o.get("id"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(str::to_owned);
    let models = r.get("models").filter(|v| !v.is_null());
    let reported = coalesce([
        models.and_then(|m| m.get("currentModelId")),
        model_opt.and_then(|o| o.get("currentValue")),
    ])
    .and_then(Value::as_str)
    .filter(|m| !m.is_empty());
    let model = c.model.filter(|m| !m.is_empty());
    if let Some(rep) = reported {
        reports.model(rep);
    }
    // the mode does not survive session/load, so it is set every time the process starts; one the
    // adapter does not offer is refused (or stood in for by a narrower one), never left to its
    // own default
    let mode = match &c.mode {
        None => None,
        Some(choice) => match choice.resolve(offered_modes(&r).as_deref()) {
            Ok(id) => id,
            Err(detail) => return Err(SessionOpenError::ModeUnresolvable { detail }.into()),
        },
    };
    let mut config = r.clone();
    if let Some(mode) = &mode {
        let mode_opt_id = mode_option(&r)
            .and_then(|o| o.get("id"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty());
        match apply_mode(l, &acp_id, mode, mode_opt_id, available_modes(&r).is_some()).await {
            Ok(Some(updated)) => {
                if updated.get("configOptions").is_some() {
                    config = updated;
                }
            }
            Ok(None) => {}
            Err(e) => {
                return Err(SessionOpenError::ModeApplyFailed {
                    mode: mode.clone(),
                    detail: e.message,
                }
                .into());
            }
        }
    }

    let effort_mapping = if c.effort_mode == EffortMode::ModelVariant && !c.effort.is_default() {
        let available_models = collect_available_model_ids(&config);
        let base_model = model.or(reported);
        let mapping = resolve_for_model(
            c.effort,
            &EffortContext {
                mode: EffortMode::ModelVariant,
                model: base_model,
                acp_options: &[],
                available_models: &available_models,
            },
        );
        let target_model = mapping.target_model.as_deref().or(model);
        if let Some(target) = target_model {
            apply_requested_model(
                l,
                &acp_id,
                target,
                model_opt_id.as_deref(),
                &mut config,
                reports,
            )
            .await?;
        }
        if !mapping.supported {
            tracing::info!(
                harness = c.harness,
                model = ?base_model,
                requested = c.effort.as_str(),
                "model_variant effort unsupported for model; running with model default"
            );
        }
        record_effort_metric(c.harness, &mapping);
        reports.effort(mapping.effective);
        reports.effort_mapping(&mapping);
        mapping
    } else {
        if let Some(model) = model {
            apply_requested_model(
                l,
                &acp_id,
                model,
                model_opt_id.as_deref(),
                &mut config,
                reports,
            )
            .await?;
        }
        let raw_options = effort_option_values(&config);
        let mapping = resolve_for_model(
            c.effort,
            &EffortContext {
                mode: c.effort_mode,
                model: model.or(reported),
                acp_options: &raw_options,
                available_models: &[],
            },
        );
        if c.effort.is_default() {
            if c.resume.is_some()
                && let Some(opt) = effort_option(&config)
                && raw_options
                    .iter()
                    .any(|v| v.eq_ignore_ascii_case("default"))
                && opt["currentValue"].as_str() != Some("default")
            {
                let _ = l
                    .request(
                        "session/set_config_option",
                        json!({"sessionId": acp_id, "configId": opt["id"], "value": "default"}),
                        Some(CONFIG_OPTION_TIMEOUT),
                    )
                    .await;
            }
            reports.effort(ReasoningEffort::Default);
        } else if let Some(target) = mapping.native_effort.as_deref() {
            let config_id = effort_option(&config)
                .and_then(|o| o.get("id"))
                .cloned()
                .expect("acp_option mapping requires effort_option");
            let updated = l
                .request(
                    "session/set_config_option",
                    json!({"sessionId": acp_id, "configId": config_id, "value": target}),
                    Some(CONFIG_OPTION_TIMEOUT),
                )
                .await
                .map_err(|e| {
                    RpcError::from(SessionOpenError::EffortApplyFailed {
                        requested: c.effort,
                        applied: target.to_owned(),
                        detail: e.message,
                    })
                })?;
            let actual = effort_option(&updated).and_then(|o| o["currentValue"].as_str());
            if actual != Some(target) {
                return Err(SessionOpenError::EffortMismatch {
                    requested: c.effort,
                    expected: target.to_owned(),
                    actual: actual.map(str::to_owned),
                }
                .into());
            }
            reports.effort(mapping.effective);
        } else {
            tracing::info!(
                harness = c.harness,
                model = ?model.or(reported),
                requested = c.effort.as_str(),
                "reasoning effort unsupported on this model; running with model default"
            );
            reports.effort(mapping.effective);
        }
        record_effort_metric(c.harness, &mapping);
        reports.effort_mapping(&mapping);
        mapping
    };

    Ok(Opened {
        acp_id,
        mode,
        image,
        system_prompt,
        effort_mapping: Some(effort_mapping),
    })
}

fn merge_json(base: &mut Value, extra: Value) {
    if let (Value::Object(dst), Value::Object(src)) = (base, extra) {
        for (k, v) in src {
            match dst.get_mut(&k) {
                Some(existing) if existing.is_object() && v.is_object() => merge_json(existing, v),
                _ => {
                    dst.insert(k, v);
                }
            }
        }
    }
}

struct Initialized {
    image: bool,
    system_prompt: bool,
    system_prompt_json: Option<Value>,
    system_prompt_meta: Option<Value>,
}

async fn initialize_session(l: &Conn, c: &OpenConfig<'_>) -> Result<Initialized, RpcError> {
    let mut client_caps = json!({
        "fs": { "readTextFile": false, "writeTextFile": false },
        "terminal": false,
        "_meta": { "parameterizedModelPicker": true },
    });
    crate::adapter::for_harness(c.harness).configure_client_capabilities(&mut client_caps);
    let init = json!({
        "protocolVersion": 1,
        "clientCapabilities": client_caps,
        "clientInfo": { "name": "steve", "version": c.client_version },
    });
    let init = l
        .request("initialize", init, Some(INITIALIZE_TIMEOUT))
        .await?;
    // image content blocks only go to adapters that say they take them; attachments always also go by path
    let image = init
        .pointer("/agentCapabilities/promptCapabilities/image")
        .is_some_and(truthy);
    // System prompt channel advertised by the adapter:
    // - ACP `append` is the baseline mode whenever `sessionCapabilities.systemPrompt` is present
    //   (non-null, non-false) unless `mode.append` is explicitly `false`.
    // - ACP `override` requires `sessionCapabilities.systemPrompt.mode.override`.
    // - `_meta.<namespace>.systemPrompt.append` requires truthy capability under `_meta`.
    let (system_prompt_json, system_prompt_meta) = match c.system_prompt {
        Some(sp) => match sp.mode {
            SystemPromptMode::AcpAppend => {
                let cap = init.pointer("/agentCapabilities/sessionCapabilities/systemPrompt");
                let supported = cap.is_some_and(|v| {
                    !v.is_null()
                        && v != &Value::Bool(false)
                        && v.pointer("/mode/append") != Some(&Value::Bool(false))
                });
                let payload = supported.then(|| {
                    json!({
                        "mode": "append",
                        "content": [{"type": "text", "text": sp.text}],
                    })
                });
                (payload, None)
            }
            SystemPromptMode::AcpOverride => {
                let supported = init
                    .pointer("/agentCapabilities/sessionCapabilities/systemPrompt/mode/override")
                    .is_some_and(|v| !v.is_null() && v != &Value::Bool(false));
                let payload = supported.then(|| {
                    json!({
                        "mode": "override",
                        "content": [{"type": "text", "text": sp.text}],
                    })
                });
                (payload, None)
            }
            SystemPromptMode::MetaAppend(namespace) => {
                let supported = init
                    .pointer(&format!(
                        "/agentCapabilities/_meta/{namespace}/systemPrompt/append"
                    ))
                    .is_some_and(truthy);
                let payload = supported.then(|| {
                    json!({
                        namespace: {
                            "systemPrompt": {
                                "append": sp.text,
                            }
                        }
                    })
                });
                (None, payload)
            }
        },
        None => (None, None),
    };
    Ok(Initialized {
        image,
        system_prompt: system_prompt_json.is_some() || system_prompt_meta.is_some(),
        system_prompt_json,
        system_prompt_meta,
    })
}

async fn open_session_request(
    l: &Conn,
    c: &OpenConfig<'_>,
    meta: Option<Value>,
    system_prompt_json: Option<Value>,
    reports: &dyn Reports,
) -> Result<(Value, String), RpcError> {
    let (r, acp_id) = match c.resume {
        Some(id) => {
            let mut p = json!({ "sessionId": id, "cwd": c.cwd, "mcpServers": c.mcp_servers });
            if let Some(m) = &meta {
                p["_meta"] = m.clone();
            }
            if let Some(sp) = &system_prompt_json {
                p["systemPrompt"] = sp.clone();
            }
            let r = l
                .request("session/load", p, Some(SESSION_OPEN_TIMEOUT))
                .await?;
            (r, id.to_string())
        }
        None => {
            let mut p = json!({ "cwd": c.cwd, "mcpServers": c.mcp_servers });
            if let Some(m) = meta {
                p["_meta"] = m;
            }
            if let Some(sp) = system_prompt_json {
                p["systemPrompt"] = sp;
            }
            let r = l
                .request("session/new", p, Some(SESSION_OPEN_TIMEOUT))
                .await?;
            let id = r
                .get("sessionId")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| RpcError::from(SessionOpenError::MissingSessionId))?
                .to_owned();
            reports.new_session(&id);
            (r, id)
        }
    };
    Ok((r, acp_id))
}

async fn apply_requested_model(
    l: &Conn,
    acp_id: &str,
    model: &str,
    model_opt_id: Option<&str>,
    config: &mut Value,
    reports: &dyn Reports,
) -> Result<(), RpcError> {
    let opt_id = model_option(config)
        .and_then(|o| o.get("id"))
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .or(model_opt_id);
    match apply_model(l, acp_id, model, opt_id).await {
        Ok(updated) => {
            let actual = updated
                .pointer("/models/currentModelId")
                .and_then(Value::as_str)
                .or_else(|| model_option(&updated).and_then(|o| o["currentValue"].as_str()));
            match actual {
                Some(actual) if actual == model => reports.model(actual),
                // An adapter may answer with its own short id for the model that was asked
                // (on a loaded session, say). That is the asked model only when the adapter's
                // own list says so: the entry it reports as current is named what was asked.
                Some(actual) if named(&updated, actual, model) => reports.model(model),
                // Empirically verified on `agy acp`: sending a concrete variant id such as
                // `gemini-4-neon-xhigh` normalizes `model.currentValue` to the family base
                // `gemini-4-neon` while setting `thought_level.currentValue` to `xhigh`.
                Some(actual) if variant_normalized(&updated, actual, model) => {
                    reports.model(model);
                }
                // Nothing in the answer relates the two ids, so this is another model as far
                // as anyone can tell, and the open fails rather than run on it. No table of
                // known aliases here: which ids mean the same model is the adapter's to say.
                Some(actual) => {
                    reports.model(actual);
                    return Err(SessionOpenError::ModelMismatch {
                        requested: model.to_owned(),
                        actual: actual.to_owned(),
                    }
                    .into());
                }
                // the adapter took it without saying what it runs: what was asked and accepted
                // is in force
                None => reports.model(model),
            }
            if updated.get("configOptions").is_some() {
                *config = updated;
            }
        }
        Err(e) => {
            return Err(SessionOpenError::ModelApplyFailed {
                model: model.to_owned(),
                detail: e.message,
            }
            .into());
        }
    }
    Ok(())
}

/// Whether `asked` is an effort-suffixed variant of `actual` and the adapter confirmed that
/// exact effort token in its updated effort `configOption`.
fn variant_normalized(answer: &Value, actual: &str, asked: &str) -> bool {
    let Some(parsed) = parse_model_variant(asked) else {
        return false;
    };
    if parsed.family_base != actual {
        return false;
    }
    effort_option(answer)
        .and_then(|o| o.get("currentValue"))
        .and_then(Value::as_str)
        .is_some_and(|cur| cur.eq_ignore_ascii_case(parsed.raw_token))
}

/// Whether the adapter's own model list in `answer` calls the entry with id `actual` by the name
/// `asked`: the "model" config option's choices (`value`, `name`, possibly in groups) and
/// `models.availableModels` (`modelId`, `name`).
fn named(answer: &Value, actual: &str, asked: &str) -> bool {
    fn any(list: &Value, id: &str, actual: &str, asked: &str) -> bool {
        list.as_array().is_some_and(|es| {
            es.iter().any(|e| {
                (e.get(id).and_then(Value::as_str) == Some(actual)
                    && e.get("name").and_then(Value::as_str) == Some(asked))
                    || e.get("options").is_some_and(|g| any(g, id, actual, asked))
            })
        })
    }
    let option = model_option(answer).and_then(|o| o.get("options"));
    option.is_some_and(|l| any(l, "value", actual, asked))
        || answer
            .pointer("/models/availableModels")
            .is_some_and(|l| any(l, "modelId", actual, asked))
}

fn method_not_found(e: &RpcError) -> bool {
    e.data
        .as_ref()
        .and_then(|d| d.get("code"))
        .and_then(Value::as_i64)
        == Some(-32601)
}

async fn apply_mode(
    l: &Conn,
    acp_id: &str,
    mode: &str,
    config_id: Option<&str>,
    prefer_set_mode: bool,
) -> Result<Option<Value>, RpcError> {
    let t = Some(CONFIG_OPTION_TIMEOUT);
    let by_set_mode = || {
        l.request(
            "session/set_mode",
            json!({ "sessionId": acp_id, "modeId": mode }),
            t,
        )
    };
    let by_option = |cid: &str| {
        l.request(
            "session/set_config_option",
            json!({ "sessionId": acp_id, "configId": cid, "value": mode }),
            t,
        )
    };
    match (config_id, prefer_set_mode) {
        (Some(cid), false) => match by_option(cid).await {
            Ok(v) => Ok(Some(v)),
            Err(e) if method_not_found(&e) => by_set_mode().await.map(Some),
            Err(e) => Err(e),
        },
        (Some(cid), true) => match by_set_mode().await {
            Ok(v) => Ok(Some(v)),
            Err(e) if method_not_found(&e) => by_option(cid).await.map(Some),
            Err(e) => Err(e),
        },
        (None, _) => by_set_mode().await.map(Some),
    }
}

/// Switch the session's model. claude-agent-acp 0.99.1 and codex-acp only take the "model" config
/// option (`session/set_model` is -32601 there) and the model does not survive `session/load`, so
/// this runs on every start. `session/set_model` is the fallback for adapters without the option.
/// The adapter may answer with another id than the one asked; that is logged here and judged by
/// the caller ([`named`]).
async fn apply_model(
    l: &Conn,
    acp_id: &str,
    model: &str,
    option_id: Option<&str>,
) -> Result<Value, RpcError> {
    let t = Some(CONFIG_OPTION_TIMEOUT);
    let cid = option_id.unwrap_or("model");
    let by_option = || {
        l.request(
            "session/set_config_option",
            json!({ "sessionId": acp_id, "configId": cid, "value": model }),
            t,
        )
    };
    let by_set_model = || {
        l.request(
            "session/set_model",
            json!({ "sessionId": acp_id, "modelId": model }),
            t,
        )
    };
    let r = if option_id.is_some() {
        by_option().await
    } else {
        match by_set_model().await {
            Err(e) if method_not_found(&e) => by_option().await,
            r => r,
        }
    }?;
    let current = model_option(&r)
        .and_then(|o| o.get("currentValue"))
        .and_then(Value::as_str);
    // its own short id for the asked model is no news
    if let Some(cur) =
        current.filter(|c| *c != model && !named(&r, c, model) && !variant_normalized(&r, c, model))
    {
        tracing::warn!("set model {model}: the adapter runs {cur}");
    }
    Ok(r)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What claude-agent-acp 0.106 answers on a loaded session when asked for an id it knows under
    /// a short one: `currentValue` is the short id, and its entry is named what was asked.
    #[test]
    fn an_id_the_adapters_list_names_as_asked_is_the_asked_model() {
        let answer = json!({"configOptions": [{"id": "model", "currentValue": "quick", "options": [
            {"value": "route/big-2", "name": "route/big-2"},
            {"value": "quick", "name": "big-2"},
        ]}]});
        assert!(named(&answer, "quick", "big-2"));
        assert!(!named(&answer, "quick", "route/big-2"), "another entry");
        assert!(!named(&answer, "route/big-2", "big-2"), "not its name");
        let grouped = json!({"configOptions": [{"id": "model", "options": [
            {"group": "g", "options": [{"value": "quick", "name": "big-2"}]},
        ]}]});
        assert!(named(&grouped, "quick", "big-2"));
        let models = json!({"models": {"currentModelId": "quick", "availableModels": [
            {"modelId": "quick", "name": "big-2"},
        ]}});
        assert!(named(&models, "quick", "big-2"));
        assert!(!named(&json!({}), "quick", "big-2"));
        // a display name is not an id
        let shown = json!({"configOptions": [{"id": "model", "options": [
            {"value": "quick", "name": "Big 2"},
        ]}]});
        assert!(!named(&shown, "quick", "big-2"));
    }

    #[test]
    fn agy_variant_model_normalization_is_recognized() {
        let answer = json!({"configOptions": [
            {"id": "model", "category": "model", "currentValue": "gemini-4-neon"},
            {"id": "thought_level", "category": "thought_level", "currentValue": "xhigh"}
        ]});
        assert!(variant_normalized(
            &answer,
            "gemini-4-neon",
            "gemini-4-neon-xhigh"
        ));
        assert!(!variant_normalized(
            &answer,
            "gemini-4-neon",
            "gemini-4-neon-low"
        ));
        assert!(!variant_normalized(
            &answer,
            "gemini-4-carbon",
            "gemini-4-neon-xhigh"
        ));
    }

    #[tokio::test]
    async fn session_open_errors_localize_in_en_and_zh_and_carry_structured_code() {
        let e = SessionOpenError::EffortMismatch {
            requested: ReasoningEffort::High,
            expected: "xhigh".into(),
            actual: Some("low".into()),
        };
        let rpc = e.clone().into_rpc_error();
        assert_eq!(rpc.data.as_ref().unwrap()["kind"], "effort_mismatch");
        assert_eq!(rpc.data.as_ref().unwrap()["requested"], "high");
        assert_eq!(rpc.data.as_ref().unwrap()["expected"], "xhigh");
        assert_eq!(rpc.data.as_ref().unwrap()["actual"], "low");

        let en = steve_i18n::scope(steve_i18n::EN, async { e.to_string() }).await;
        assert!(en.contains("high") && en.contains("xhigh") && en.contains("low"));
        let zh = steve_i18n::scope(steve_i18n::ZH_CN, async { e.to_string() }).await;
        assert!(zh.contains("high") && zh.contains("xhigh") && zh.contains("low"));
        assert_ne!(en, zh);
    }
}
