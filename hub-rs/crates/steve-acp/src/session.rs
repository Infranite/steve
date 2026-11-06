//! Opening an ACP session on a started adapter: `initialize`, `session/new` or `session/load`, the
//! permission mode, the model and the reasoning effort, for the Node's driver.
//!
//! What the adapter reports comes back through [`Reports`] at the moment it is known. How strict
//! to be is the caller's [`Strictness`]: lenient logs a mode or model it could not set and goes on
//! (how the Hub's old runner worked); a Node fails closed, since the Hub asked for a configuration
//! and must not get another one silently.

use std::time::Duration;

use serde_json::{Value, json};

use crate::rpc::{Conn, RpcError};
use crate::util::{coalesce, js_str, truthy};

/// The configuration option of the reasoning effort, if the adapter reports one.
pub(crate) fn effort_option(config: &Value) -> Option<&Value> {
    config.get("configOptions")?.as_array()?.iter().find(|o| {
        o.get("id")
            .and_then(Value::as_str)
            .is_some_and(|id| ["thought_level", "reasoning_effort", "effort"].contains(&id))
            || o.get("category").and_then(Value::as_str) == Some("thought_level")
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
    /// The mode to set (`None`: leave the adapter's).
    pub mode: Option<String>,
    pub model: Option<&'a str>,
    pub effort: Option<&'a str>,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strictness {
    /// A mode or model that could not be set is logged; the session opens anyway.
    Lenient,
    /// It is an error.
    Strict,
}

#[derive(Debug, Clone)]
pub struct Opened {
    pub acp_id: String,
    /// The adapter takes image blocks.
    pub image: bool,
}

fn err(message: String) -> RpcError {
    RpcError {
        message,
        data: None,
    }
}

/// initialize + session/new|load + mode + model + effort.
pub async fn open(
    l: &Conn,
    c: OpenConfig<'_>,
    reports: &dyn Reports,
    strictness: Strictness,
) -> Result<Opened, RpcError> {
    open_with_meta(l, c, None, reports, strictness).await
}

/// [`open`], with `meta` as the `_meta` of `session/new|load` (Claude Code's adapter reads its
/// options there, `claudeCode.options.settings` among them).
pub async fn open_with_meta(
    l: &Conn,
    c: OpenConfig<'_>,
    meta: Option<Value>,
    reports: &dyn Reports,
    strictness: Strictness,
) -> Result<Opened, RpcError> {
    let strict = strictness == Strictness::Strict;
    let init = json!({
        "protocolVersion": 1,
        "clientCapabilities": { "fs": { "readTextFile": false, "writeTextFile": false }, "terminal": false },
        "clientInfo": { "name": "steve", "version": c.client_version },
    });
    let init = l
        .request("initialize", init, Some(Duration::from_secs(60)))
        .await?;
    // image content blocks only go to adapters that say they take them; attachments always also go by path
    let image = init
        .pointer("/agentCapabilities/promptCapabilities/image")
        .is_some_and(truthy);
    let (r, acp_id) = match c.resume {
        Some(id) => {
            let mut p = json!({ "sessionId": id, "cwd": c.cwd, "mcpServers": c.mcp_servers });
            if let Some(m) = &meta {
                p["_meta"] = m.clone();
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
            let r = l
                .request("session/new", p, Some(Duration::from_secs(120)))
                .await?;
            let id = js_str(r.get("sessionId"));
            reports.new_session(&id);
            (r, id)
        }
    };
    // Claude Code reports models in `models`; Codex only as the "model" config option
    let model_opt = r
        .get("configOptions")
        .and_then(Value::as_array)
        .and_then(|os| {
            os.iter()
                .find(|o| o.get("id").and_then(Value::as_str) == Some("model"))
        });
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
    // the mode does not survive session/load, so it is set every time the process starts
    if let Some(mode) = c.mode
        && let Err(e) = l
            .request(
                "session/set_mode",
                json!({ "sessionId": acp_id, "modeId": mode }),
                Some(Duration::from_secs(30)),
            )
            .await
    {
        if strict {
            return Err(err(format!("权限模式 {mode} 没能生效：{}", e.message)));
        }
        tracing::error!("set_mode {}", e.message);
    }
    let mut config = r.clone();
    if let Some(model) = model {
        match apply_model(l, &acp_id, model, model_opt.is_some()).await {
            Ok(updated) => {
                let actual = updated
                    .pointer("/models/currentModelId")
                    .and_then(Value::as_str)
                    .or_else(|| {
                        updated
                            .get("configOptions")
                            .and_then(Value::as_array)
                            .and_then(|os| os.iter().find(|o| o["id"] == "model"))
                            .and_then(|o| o["currentValue"].as_str())
                    });
                match actual {
                    Some(actual) => {
                        reports.model(actual);
                        if strict && actual != model {
                            return Err(err(format!("模型没能生效：请求 {model}，实际 {actual}")));
                        }
                    }
                    // the adapter took it without saying what it runs: what was asked and accepted
                    // is in force (strictly; leniently, what it last saw is kept)
                    None if strict => reports.model(model),
                    None => {}
                }
                if updated.get("configOptions").is_some() {
                    config = updated;
                }
            }
            Err(e) => {
                if strict {
                    return Err(err(format!("模型 {model} 没能生效：{}", e.message)));
                }
                tracing::warn!("set model {model} {}", e.message);
            }
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
    Ok(Opened { acp_id, image })
}

/// Switch the session's model. claude-agent-acp 0.99.1 and codex-acp only take the "model" config
/// option (`session/set_model` is -32601 there) and the model does not survive `session/load`, so
/// this runs on every start. `session/set_model` is the fallback for adapters without the option.
/// The adapter may map an id it does not list to a near one; that is logged here and judged by the
/// caller (strict callers refuse it).
async fn apply_model(
    l: &Conn,
    acp_id: &str,
    model: &str,
    has_option: bool,
) -> Result<Value, RpcError> {
    let t = Some(Duration::from_secs(30));
    let by_option = || {
        l.request(
            "session/set_config_option",
            json!({ "sessionId": acp_id, "configId": "model", "value": model }),
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
    let not_found = |e: &RpcError| {
        e.data
            .as_ref()
            .and_then(|d| d.get("code"))
            .and_then(Value::as_i64)
            == Some(-32601)
    };
    let r = if has_option {
        by_option().await
    } else {
        match by_set_model().await {
            Err(e) if not_found(&e) => by_option().await,
            r => r,
        }
    }?;
    let current = r
        .get("configOptions")
        .and_then(Value::as_array)
        .and_then(|os| {
            os.iter()
                .find(|o| o.get("id").and_then(Value::as_str) == Some("model"))
        })
        .and_then(|o| o.get("currentValue"))
        .and_then(Value::as_str);
    if let Some(cur) = current.filter(|c| *c != model) {
        tracing::warn!("set model {model}: the adapter runs {cur}");
    }
    Ok(r)
}
