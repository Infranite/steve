//! Item-scoped management for agent sessions. The authenticated session selects the item;
//! conversation disclosure is recalculated for each call, just as it is for inline research.

use serde_json::{Value, json};
use steve_store::{Session, Store};

use super::research_mcp::{self, ItemScope};

pub const DEPENDENCIES: &str = "steve_item_dependencies";

pub fn handles(name: &str) -> bool {
    matches!(
        name,
        research_mcp::ITEMS_SEARCH | research_mcp::ITEM_READ | DEPENDENCIES
    )
}

pub fn tools() -> Vec<Value> {
    let mut tools: Vec<_> = research_mcp::tools()
        .into_iter()
        .filter(|t| {
            matches!(
                t["name"].as_str(),
                Some(research_mcp::ITEMS_SEARCH | research_mcp::ITEM_READ)
            )
        })
        .collect();
    for t in &mut tools {
        t["inputSchema"]["additionalProperties"] = json!(false);
        if let Some(description) = t["description"].as_str() {
            t["description"] = json!(format!(
                "{description} Only items visible to this session's originating conversation are returned; an unknown origin sees only this item."
            ));
        }
    }
    tools.push(json!({
        "name": DEPENDENCIES,
        "description": "Read this item's persisted prerequisites, waiting status and effective resume policy. Use action wait to add verified prerequisite item IDs, preserve existing dependencies, and wait without delivering or closing this item. A reason is required. resume automatic or notify applies only to this item and cycle; workspace follows the workspace preference. Only this item's current responsible session may write; child work units must report blockers to their parent. Inspect the returned persisted snapshot and end your turn when end_turn is true. Use get to inspect the saved state later; never claim a dependency is registered from a plan update alone.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["get", "wait"]},
                "deps": {"type": "array", "items": {"type": "string", "minLength": 1}, "uniqueItems": true},
                "reason": {"type": "string", "minLength": 1},
                "resume": {"type": "string", "enum": ["automatic", "notify", "workspace"]}
            },
            "required": ["action"],
            "additionalProperties": false
        }
    }));
    tools
}

fn string<'a>(args: &'a Value, key: &str) -> Result<&'a str, String> {
    args[key]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| format!("Invalid arguments: {key} must be a non-empty string."))
}

fn validate_args(name: &str, args: &Value) -> Result<(), String> {
    let allowed: &[&str] = match name {
        research_mcp::ITEMS_SEARCH => &["query"],
        research_mcp::ITEM_READ => &["id"],
        DEPENDENCIES => &["action", "deps", "reason", "resume"],
        _ => return Err("Unknown item tool.".into()),
    };
    let obj = args
        .as_object()
        .ok_or("Invalid arguments: expected an object.")?;
    if obj.keys().any(|k| !allowed.contains(&k.as_str())) {
        return Err("Invalid arguments: unexpected property.".into());
    }
    match name {
        research_mcp::ITEMS_SEARCH => {
            string(args, "query")?;
        }
        research_mcp::ITEM_READ => {
            string(args, "id")?;
        }
        DEPENDENCIES => {
            if !matches!(args["action"].as_str(), Some("get" | "wait")) {
                return Err("Invalid arguments: action must be get or wait.".into());
            }
            if args["action"] == "get" && obj.len() != 1 {
                return Err("Invalid arguments: get accepts only action.".into());
            }
            if args["action"] == "wait" {
                string(args, "reason")?;
                if !matches!(
                    args.get("resume").and_then(Value::as_str),
                    None | Some("automatic" | "notify" | "workspace")
                ) || args.get("resume").is_some_and(|v| !v.is_string())
                {
                    return Err("Invalid arguments: unknown resume policy.".into());
                }
                if args["deps"].as_array().is_none_or(|v| {
                    v.iter()
                        .any(|d| d.as_str().is_none_or(|s| s.trim().is_empty()))
                }) {
                    return Err("Invalid arguments: deps must be an array of item IDs.".into());
                }
            }
        }
        _ => unreachable!(),
    }
    Ok(())
}

/// Scope derives from persisted identity, never from a tool argument or a workspace-owner fallback.
fn scope(store: &Store, session: &Session) -> Result<(Session, ItemScope), String> {
    let denied = || "This session is not the current responsible item session.".to_string();
    let s = store
        .sessions()
        .get(&session.id)
        .map_err(|e| e.to_string())?
        .filter(|s| {
            s.status != "ended" && s.readonly == 0 && s.device_id == "hub" && s.source == "Steve"
        })
        .ok_or_else(denied)?;
    let id = s.item_id.as_deref().ok_or_else(denied)?;
    let item = store
        .items()
        .get(id)
        .map_err(|e| e.to_string())?
        .filter(|i| i.workspace_id == s.workspace_id && i.bot_id.is_some() && i.bot_id == s.bot_id)
        .ok_or_else(denied)?;
    store
        .bots()
        .get(s.bot_id.as_deref().ok_or_else(denied)?)
        .map_err(|e| e.to_string())?
        .filter(|b| b.enabled)
        .ok_or_else(denied)?;
    if !store
        .bots()
        .in_workspace(s.bot_id.as_deref().ok_or_else(denied)?, &s.workspace_id)
        .map_err(|e| e.to_string())?
    {
        return Err(denied());
    }
    let current = store
        .sessions()
        .of_item(id)
        .map_err(|e| e.to_string())?
        .into_iter()
        .rev()
        .find(|s| {
            s.status != "ended" && s.readonly == 0 && s.device_id == "hub" && s.source == "Steve"
        });
    if current.is_none_or(|c| c.id != s.id) {
        return Err(denied());
    }
    let own = ItemScope::Conversation([id.to_string()].into_iter().collect());
    let work_unit = item.source.1 == "workflow"
        || store
            .workflows()
            .by_child(id)
            .map_err(|e| e.to_string())?
            .is_some();
    // Work units have a separate upstream projection in steve_workflow. Do not inherit the
    // parent's main-conversation privileges, including when an old attempt was retired.
    if work_unit {
        return Ok((s, own));
    }
    let origin = store
        .thread()
        .item(id)
        .map_err(|e| e.to_string())?
        .filter(|t| t.workspace_id == s.workspace_id);
    let Some(origin) = origin else {
        return Ok((s, own));
    };
    let message = store
        .messages()
        .get(&origin.origin_msg)
        .map_err(|e| e.to_string())?
        .filter(|m| m.workspace_id == s.workspace_id);
    if message.is_none() {
        return Ok((s, own));
    }
    let scope = ItemScope::of(store, &origin.origin_msg)?;
    Ok((s, scope))
}

#[cfg(test)]
#[path = "item_mcp_tests.rs"]
mod tests;

fn redact_dependencies(value: &mut Value, scope: &ItemScope) {
    if !matches!(scope, ItemScope::All)
        && let Some(object) = value.as_object_mut()
    {
        // A wait can predate a conversation membership change. Its free-text explanation may
        // mention prerequisites that are no longer visible to the current audience.
        object.remove("reason");
    }
    if let Some(deps) = value["dependencies"].as_array_mut() {
        let before = deps.len();
        deps.retain(|d| d["id"].as_str().is_some_and(|id| scope.sees(id)));
        let hidden = before - deps.len();
        value["hidden_dependencies"] = json!(hidden);
    }
    if let Some(deps) = value["open_dependencies"].as_array_mut() {
        deps.retain(|d| d.as_str().is_some_and(|id| scope.sees(id)));
    }
}

/// Shared by the HTTP MCP endpoint and the authenticated Node tool service.
pub fn call(
    store: &Store,
    core: &steve_core::Core,
    session: &Session,
    name: &str,
    args: Value,
) -> Result<String, String> {
    validate_args(name, &args)?;
    let (current, scope) = scope(store, session)?;
    match name {
        research_mcp::ITEMS_SEARCH => research_mcp::items_search(
            store,
            &current.workspace_id,
            &scope,
            string(&args, "query")?,
        ),
        research_mcp::ITEM_READ => {
            research_mcp::item_read(store, &current.workspace_id, &scope, string(&args, "id")?)
        }
        DEPENDENCIES => {
            let wait = args["action"] == "wait";
            if wait {
                for id in args["deps"].as_array().into_iter().flatten() {
                    let id = id.as_str().unwrap_or_default();
                    if !scope.sees(id)
                        || store
                            .items()
                            .get(id)
                            .map_err(|e| e.to_string())?
                            .is_none_or(|i| i.workspace_id != current.workspace_id)
                    {
                        return Err("Prerequisite item is not available to this session.".into());
                    }
                }
            }
            let mut snapshot = core
                .tool_item_dependencies(&current, args)
                .map_err(|e| e.to_string())?;
            redact_dependencies(&mut snapshot, &scope);
            Ok(if wait && snapshot["end_turn"] == true {
                format!("{snapshot}\n\n{}", steve_mcp::END)
            } else {
                snapshot.to_string()
            })
        }
        _ => Err("Unknown item tool.".into()),
    }
}
