//! MCP tools of the vault: `steve_secret_list` and `steve_secret_use`.
//! Definitions in the `{name, description, inputSchema}` shape of steve-mcp's `tools.json`; serving
//! them is the integrator's (list after the Steve tools, route the names to [`call`] with the
//! session's [`UseCx`]). Results hold metadata, or the command output.

use std::sync::LazyLock;

use serde_json::{Map, Value, json};

use super::items::repo as irepo;
use super::r#use::{END, NO_SUCH, SecretUses, UseCx, UseOutcome, UseRequest};
use super::{VaultError, VaultResult};
use crate::ApiError;

pub const LIST: &str = "steve_secret_list";
pub const USE: &str = "steve_secret_use";

static TOOLS: LazyLock<Vec<Value>> = LazyLock::new(|| {
    let map = |d: &str| json!({ "type": "object", "additionalProperties": { "type": "string" }, "description": d });
    vec![
        json!({
            "name": LIST,
            "description": "List the credentials in the user's vault that you may ask to use. You never see the values.",
            "inputSchema": {
                "$schema": "http://json-schema.org/draft-07/schema#",
                "type": "object",
                "properties": { "query": { "type": "string" } }
            }
        }),
        json!({
            "name": USE,
            "description": "Run a command that needs a credential from the user's vault. Steve injects the values and runs the command for you; you get the exit code and the output as the command printed it. Usually the user must approve first.",
            "inputSchema": {
                "$schema": "http://json-schema.org/draft-07/schema#",
                "type": "object",
                "properties": {
                    "item": { "type": "string", "description": "id or exact title" },
                    "purpose": { "type": "string", "description": "why, in the user's language" },
                    "command": { "type": "string" },
                    "cwd": { "type": "string", "description": "relative to your working directory" },
                    "env": map("{VAR: field label}"),
                    "files": map("{VAR: field label}; VAR receives a path to a temporary file holding the value"),
                    "stdin": { "type": "string", "description": "field label" },
                    "timeout_s": { "type": "number" }
                },
                "required": ["item", "purpose", "command"]
            }
        }),
    ]
});

/// The tool definitions, in listing order.
pub fn tools() -> &'static [Value] {
    &TOOLS
}

pub fn handles(name: &str) -> bool {
    name == LIST || name == USE
}

/// A tool result. `awaiting_approval`: a decision was opened; mark the session pending=approval and
/// set the item's `now` exactly as for `steve_request_approval` (the text already ends with END).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolOut {
    pub text: String,
    pub awaiting_approval: bool,
}

fn received(v: Option<&Value>) -> &'static str {
    match v {
        None => "undefined",
        Some(Value::Null) => "null",
        Some(Value::Bool(_)) => "boolean",
        Some(Value::Number(_)) => "number",
        Some(Value::String(_)) => "string",
        Some(Value::Array(_)) => "array",
        Some(Value::Object(_)) => "object",
    }
}

/// zod-style argument errors, like the other tools.
fn validate(name: &str, args: &Value) -> Result<(), String> {
    let mut issues = Vec::new();
    let mut want = |k: &str, ty: &str, required: bool| {
        let v = args.get(k);
        if v.is_none() && !required {
            return;
        }
        let ok = match ty {
            "string" => v.is_some_and(Value::is_string),
            "number" => v.is_some_and(Value::is_number),
            "object" => v.is_some_and(Value::is_object),
            _ => true,
        };
        if !ok {
            issues.push(format!(
                "Invalid input: expected {ty}, received {} at {k}",
                received(v)
            ));
        }
    };
    if name == LIST {
        want("query", "string", false);
    } else {
        want("item", "string", true);
        want("purpose", "string", true);
        want("command", "string", true);
        want("cwd", "string", false);
        want("env", "object", false);
        want("files", "object", false);
        want("stdin", "string", false);
        want("timeout_s", "number", false);
        for k in ["env", "files"] {
            if let Some(Value::Object(m)) = args.get(k) {
                for (var, v) in m {
                    if !v.is_string() {
                        issues.push(format!(
                            "Invalid input: expected string, received {} at {k}.{var}",
                            received(Some(v))
                        ));
                    }
                }
            }
        }
    }
    if issues.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "MCP error -32602: Input validation error: Invalid arguments for tool {name}: {}",
            issues.join("\n")
        ))
    }
}

fn s(args: &Value, k: &str) -> Option<String> {
    args.get(k).and_then(Value::as_str).map(str::to_string)
}

fn pairs(args: &Value, k: &str) -> Vec<(String, String)> {
    args.get(k)
        .and_then(Value::as_object)
        .map(|m: &Map<String, Value>| {
            m.iter()
                .filter_map(|(var, v)| v.as_str().map(|l| (var.clone(), l.to_string())))
                .collect()
        })
        .unwrap_or_default()
}

/// Runs one tool. `Err` is the `isError` text.
pub async fn call(
    uses: &SecretUses,
    cx: &UseCx,
    name: &str,
    args: &Value,
) -> Result<ToolOut, String> {
    if !handles(name) {
        return Err(format!("MCP error -32602: Tool {name} not found"));
    }
    validate(name, args)?;
    let r = if name == LIST {
        let (u, cx, q) = (uses.clone(), cx.clone(), s(args, "query"));
        uses.vault()
            .blocking(move |_| list(&u, &cx, q.as_deref()))
            .await
            .map(|text| ToolOut {
                text,
                awaiting_approval: false,
            })
    } else {
        let req = UseRequest {
            item: s(args, "item").unwrap_or_default(),
            purpose: s(args, "purpose").unwrap_or_default(),
            command: s(args, "command").unwrap_or_default(),
            cwd: s(args, "cwd"),
            env: pairs(args, "env"),
            files: pairs(args, "files"),
            stdin: s(args, "stdin"),
            timeout_s: args.get("timeout_s").and_then(Value::as_f64),
        };
        uses.request(cx, req).await.map(|o| match o {
            UseOutcome::Ran { out, .. } => ToolOut {
                text: format!("Exit code {}.\n{}", out.code(), out.combined()),
                awaiting_approval: false,
            },
            UseOutcome::Asked { .. } => ToolOut {
                text: format!("Approval requested. {END}"),
                awaiting_approval: true,
            },
        })
    };
    r.map_err(|e| match e {
        VaultError::Api(ApiError::NotFound(n)) if n == "条目" => NO_SUCH.to_string(),
        VaultError::Api(ApiError::Internal(m)) => {
            tracing::error!(tool = name, error = %m, "vault tool failed");
            "Steve could not do this right now. Try again later.".to_string()
        }
        e => e.to_string(),
    })
}

/// `steve_secret_list`: the items the session's user sees, metadata only.
fn list(uses: &SecretUses, cx: &UseCx, query: Option<&str>) -> VaultResult<String> {
    let v = uses.vault();
    v.ensure_vaults(&cx.ws, &cx.user)?;
    let f = irepo::ItemFilter {
        q: query
            .map(str::trim)
            .filter(|q| !q.is_empty())
            .map(String::from),
        ..Default::default()
    };
    let lines = v.store().reader(|c| {
        let mut lines = Vec::new();
        for i in irepo::list_items(c, &cx.ws, &cx.user, &f)? {
            let labels: Vec<String> = irepo::fields(c, &i.id)?
                .into_iter()
                .map(|f| f.label)
                .collect();
            let site = if i.site.is_empty() { "-" } else { &i.site };
            lines.push(format!(
                "{} · {} · {} · {site} · {} · fields: {}",
                i.id,
                i.title,
                i.template,
                i.sensitivity,
                labels.join(", ")
            ));
        }
        Ok::<_, VaultError>(lines)
    })?;
    if lines.is_empty() {
        return Ok(match query {
            Some(q) if !q.trim().is_empty() => format!("No credentials match \"{}\".", q.trim()),
            _ => "The vault has no credentials you may ask to use.".to_string(),
        });
    }
    Ok(format!(
        "{}\nTo use one, call steve_secret_use.",
        lines.join("\n")
    ))
}
