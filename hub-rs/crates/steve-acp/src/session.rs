//! Opening an ACP session on a started adapter: `initialize`, `session/new` or `session/load`, the
//! permission mode, the model and the reasoning effort, for the Node's driver.
//!
//! What the adapter reports comes back through [`Reports`] at the moment it is known. A mode or
//! model that could not be set is an error: the Hub asked for a configuration and must not get
//! another one silently.

use std::time::Duration;

use serde_json::{Value, json};

use crate::rpc::{Conn, RpcError};
use crate::util::{coalesce, truthy};

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

fn option_supports(options: &Value, effort: &str) -> bool {
    options.as_array().is_some_and(|os| {
        os.iter().any(|o| {
            o.get("value").and_then(Value::as_str) == Some(effort)
                || o.get("options").is_some_and(|v| option_supports(v, effort))
        })
    })
}

/// What to open.
pub struct OpenConfig<'a> {
    pub client_version: &'a str,
    pub cwd: &'a str,
    /// `session/load` this one; `None` starts a new session.
    pub resume: Option<&'a str>,
    /// The `mcpServers` of `session/new|load`.
    pub mcp_servers: Value,
    /// The permission mode to set (`None`: leave the adapter's).
    pub mode: Option<ModeChoice>,
    pub model: Option<&'a str>,
    pub effort: Option<&'a str>,
    /// Steve's instructions as the session's system prompt, sent only to an adapter that
    /// advertises `sessionCapabilities.systemPrompt` ([`Opened::system_prompt`] says whether).
    pub system_prompt: Option<SystemPrompt<'a>>,
}

/// The adapter's mode id for one of Steve's permission modes, and what may stand in for it when
/// the adapter does not offer it ([`crate::adapters::ModeMap::choice`]).
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

/// A system prompt for the session (ACP `systemPrompt` on `session/new`).
#[derive(Debug, Clone, Copy)]
pub struct SystemPrompt<'a> {
    pub text: &'a str,
    /// In place of the harness's own (`override`), else after it (`append`).
    pub replace: bool,
}

/// Facts as they become known.
pub trait Reports: Sync {
    /// A new session was created (before anything else can fail).
    fn new_session(&self, _acp_id: &str) {}
    /// The model the adapter runs (reported on open, then after setting one).
    fn model(&self, _model: &str) {}
    /// The reasoning effort in force.
    fn effort(&self, _effort: &str) {}
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
}

fn err(message: String) -> RpcError {
    RpcError {
        message,
        data: None,
    }
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
    let init = json!({
        "protocolVersion": 1,
        "clientCapabilities": {
            "fs": { "readTextFile": false, "writeTextFile": false },
            "terminal": false,
            "_meta": { "parameterizedModelPicker": true },
        },
        "clientInfo": { "name": "steve", "version": c.client_version },
    });
    let init = l
        .request("initialize", init, Some(Duration::from_secs(60)))
        .await?;
    // image content blocks only go to adapters that say they take them; attachments always also go by path
    let image = init
        .pointer("/agentCapabilities/promptCapabilities/image")
        .is_some_and(truthy);
    // a system prompt of the client's own, in the mode the adapter advertises
    let system_prompt = c.system_prompt.filter(|sp| {
        let mode = if sp.replace { "override" } else { "append" };
        init.pointer("/agentCapabilities/sessionCapabilities/systemPrompt/mode")
            .and_then(|m| m.get(mode))
            .is_some_and(|v| !v.is_null() && v != &Value::Bool(false))
    });
    let system_prompt_json = system_prompt.map(|sp| {
        json!({"mode": if sp.replace { "override" } else { "append" },
            "content": [{"type": "text", "text": sp.text}]})
    });
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
                .request("session/load", p, Some(Duration::from_secs(120)))
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
                .request("session/new", p, Some(Duration::from_secs(120)))
                .await?;
            let id = r
                .get("sessionId")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .ok_or_else(|| err("adapter returned no sessionId on session/new".into()))?
                .to_owned();
            reports.new_session(&id);
            (r, id)
        }
    };
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
            Err(e) => return Err(err(e)),
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
            Err(e) => return Err(err(format!("权限模式 {mode} 没能生效：{}", e.message))),
        }
    }
    if let Some(model) = model {
        let opt_id = model_option(&config)
            .and_then(|o| o.get("id"))
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .or(model_opt_id.as_deref());
        match apply_model(l, &acp_id, model, opt_id).await {
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
                    // Nothing in the answer relates the two ids, so this is another model as far
                    // as anyone can tell, and the open fails rather than run on it. No table of
                    // known aliases here: which ids mean the same model is the adapter's to say.
                    Some(actual) => {
                        reports.model(actual);
                        return Err(err(format!("模型没能生效：请求 {model}，实际 {actual}")));
                    }
                    // the adapter took it without saying what it runs: what was asked and accepted
                    // is in force
                    None => reports.model(model),
                }
                if updated.get("configOptions").is_some() {
                    config = updated;
                }
            }
            Err(e) => return Err(err(format!("模型 {model} 没能生效：{}", e.message))),
        }
    }
    let effort_opt = effort_option(&config);
    if let Some(effort) = c.effort.filter(|e| !e.is_empty()) {
        let option = effort_opt.ok_or_else(|| {
            err(format!(
                "当前 harness 未报告可设置的思考强度，无法应用 {effort}"
            ))
        })?;
        if !option
            .get("options")
            .is_some_and(|v| option_supports(v, effort))
        {
            return Err(err(format!("当前模型不支持思考强度 {effort}")));
        }
        let updated = l
            .request(
                "session/set_config_option",
                json!({"sessionId": acp_id, "configId": option["id"], "value": effort}),
                Some(Duration::from_secs(30)),
            )
            .await?;
        let actual = effort_option(&updated).and_then(|o| o["currentValue"].as_str());
        if let Some(actual) = actual {
            reports.effort(actual);
        }
        if actual != Some(effort) {
            return Err(err(format!(
                "思考强度未生效：请求 {effort}，实际 {}",
                actual.unwrap_or("未知")
            )));
        }
    } else if let Some(actual) = effort_opt.and_then(|o| o["currentValue"].as_str()) {
        reports.effort(actual);
    }
    Ok(Opened {
        acp_id,
        mode,
        image,
        system_prompt: system_prompt.is_some(),
    })
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
    let t = Some(Duration::from_secs(30));
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
    let t = Some(Duration::from_secs(30));
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
    if let Some(cur) = current.filter(|c| *c != model && !named(&r, c, model)) {
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
}
