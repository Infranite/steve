//! Steve's `/mcp` for a client outside Steve (Claude Desktop, Cursor, another agent): it sends one of
//! its user's access tokens (`Authorization: Bearer stt_…`) and acts as that user, in the user's
//! workspace (`?ws=<id>` or `x-steve-ws` names one of several). Unlike a Steve session's token it
//! is accepted from any address the Hub answers, like the rest of the API.
//!
//! What such a client gets is what makes sense without a Steve session: finding and reading the
//! workspace's items, handing Steve new work, adding to an item, the knowledge base and the user's
//! memories (to read; tidying them stays with Steve's own runs), and the list of agent sessions. The tools that belong to a session's own turn (`steve_update`, `steve_ask`,
//! `steve_deliver`, the vault, mail drafts, …) are not listed and are refused. An outside agent
//! that needs the owner's input creates an item (the owner sees it like any other) or adds to one.

use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use steve_store::{Store, User};

use steve_modules::caps::tools::{CALL, SEARCH};

use super::mcp_extra::Extra;
use super::research_mcp::{self, ItemScope};
use super::tool_registry;

pub const ITEM_CREATE: &str = "steve_item_create";
pub const ITEM_SUPPLEMENT: &str = "steve_item_supplement";

/// Who is calling: the token's user, in one of their workspaces.
#[derive(Clone, Debug)]
pub struct Outside {
    pub user: User,
    pub ws: String,
    /// The token's name, for the item's source.
    pub token: String,
}

/// The user and workspace an access token `token` stands for; `ws` names one of the user's
/// workspaces (else their first). `None`: not a live access token, or not that user's workspace.
pub fn identify(store: &Store, token: &str, ws: Option<&str>) -> Option<Outside> {
    let (user, tok) = store.access().token_user(token).ok().flatten()?;
    let mine: Vec<String> = store
        .reader(|c| {
            let mut st = c.prepare(
                "SELECT workspace_id FROM workspace_members WHERE user_id = ? ORDER BY created, workspace_id",
            )?;
            let v = st
                .query_map([&user.id], |r| r.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            Ok::<_, steve_store::StoreError>(v)
        })
        .ok()?;
    let ws = match ws.filter(|w| !w.is_empty()) {
        Some(w) => mine.iter().find(|m| *m == w)?.clone(),
        None => mine
            .first()
            .cloned()
            .unwrap_or_else(|| user.workspace_id.clone()),
    };
    Some(Outside {
        user,
        ws,
        token: tok.name,
    })
}

fn schema(props: Value, required: &[&str]) -> Value {
    json!({"type": "object", "properties": props, "required": required, "additionalProperties": false})
}

/// The tools an outside client sees, in this order.
pub fn tools() -> Vec<Value> {
    let read_only = json!({"readOnlyHint": true, "destructiveHint": false, "openWorldHint": false});
    let mut v: Vec<Value> = research_mcp::tools()
        .into_iter()
        .filter(|t| {
            matches!(
                t["name"].as_str(),
                Some(
                    research_mcp::ITEMS_SEARCH
                        | research_mcp::ITEM_READ
                        | research_mcp::SESSIONS_LIST
                        | steve_knowledge::tools::SEARCH
                        | steve_knowledge::tools::READ
                )
            )
        })
        .chain(
            steve_knowledge::steward::tools()
                .into_iter()
                .filter(|t| t["name"] == steve_knowledge::steward::LIST),
        )
        .map(|mut t| {
            t["annotations"] = read_only.clone();
            t
        })
        .collect();
    v.push(json!({
        "name": ITEM_CREATE,
        "description": "Hand Steve a new piece of work as the signed-in user: it becomes an item (ST-…) that one of Steve's Bots works on, and the owner follows it in Steve. Write `goal` as a complete, self-contained request. Leave out `bot` and `title` and Steve chooses them the way it does for a message. Returns the item's id; read it later with steve_item_read for its progress and result.",
        "inputSchema": schema(json!({
            "goal": {"type": "string", "description": "What should be done, self-contained."},
            "title": {"type": "string", "description": "A short title; Steve writes one when left out."},
            "bot": {"type": "string", "description": "The id or name of the Bot to do it; Steve chooses when left out."}
        }), &["goal"]),
        "annotations": {"readOnlyHint": false, "destructiveHint": false, "openWorldHint": false}
    }));
    v.push(json!({
        "name": ITEM_SUPPLEMENT,
        "description": "Add to an existing item as the signed-in user: more detail, a correction, or an answer to what its Bot asked. The Bot working on it gets the text. Use steve_items_search or steve_item_read first to find the item.",
        "inputSchema": schema(json!({
            "id": {"type": "string", "description": "e.g. ST-20"},
            "text": {"type": "string"}
        }), &["id", "text"]),
        "annotations": {"readOnlyHint": false, "destructiveHint": false, "openWorldHint": false}
    }));
    v
}

/// Whether an outside client may call `name`.
pub fn allows(name: &str) -> bool {
    tools().iter().any(|t| t["name"] == name)
}

/// What an outside client lists: finding items and knowledge, handing Steve work, and finding and
/// running the rest of its tools (`steve_tools_search`, `steve_tool_call`), as a session does.
pub const CORE: &[&str] = &[
    research_mcp::ITEMS_SEARCH,
    steve_knowledge::tools::SEARCH,
    ITEM_CREATE,
];

/// The outside client's tools as registry entries.
fn entries() -> Vec<tool_registry::Entry> {
    tool_registry::steve_entries(
        tools()
            .into_iter()
            .map(|t| steve_node_api::ToolDescriptor {
                name: t["name"].as_str().unwrap_or_default().to_string(),
                description: t["description"].as_str().unwrap_or_default().to_string(),
                input_schema: t["inputSchema"].clone(),
                read_only: t["annotations"]["readOnlyHint"].as_bool().unwrap_or(false),
                annotations: serde_json::Value::Null,
            })
            .collect(),
    )
}

/// The `tools/list` an outside client gets.
pub fn listed() -> Vec<Value> {
    let mut v: Vec<Value> = tools()
        .into_iter()
        .filter(|t| t["name"].as_str().is_some_and(|n| CORE.contains(&n)))
        .collect();
    for d in [tool_registry::search_tool(), tool_registry::call_tool()] {
        v.push(json!({
            "name": d.name,
            "description": d.description.replace(
                "Steve's own (items, work graph, knowledge, memories, contacts, secrets, GitHub, mail, chats, devices, diagnostics), the MCP servers and remote agents your Bot uses",
                "Steve's tools for a client outside Steve (items, sessions, knowledge, memories)",
            ),
            "inputSchema": d.input_schema,
            "annotations": {"readOnlyHint": d.read_only, "destructiveHint": false, "openWorldHint": false},
        }));
    }
    v
}

/// `steve_tools_search` and `steve_tool_call` for an outside client: over its own tools only.
async fn search_or_call(
    x: &Extra,
    o: &Outside,
    name: &str,
    args: &Value,
) -> Result<String, String> {
    let schema = if name == SEARCH {
        tool_registry::search_tool()
    } else {
        tool_registry::call_tool()
    };
    tool_registry::validate(&schema.input_schema, args).map_err(|e| e.join("; "))?;
    let all = entries();
    if name == SEARCH {
        let q = tool_registry::Query::from_args(args);
        let skip: Vec<String> = CORE.iter().map(|s| s.to_string()).collect();
        let (found, total) = tool_registry::search(&all, &skip, &q);
        return Ok(tool_registry::search_answer(&found, total, &[]));
    }
    let inner = args["name"].as_str().unwrap_or_default().trim();
    if inner == SEARCH || inner == CALL {
        return Err(format!("Call {inner} directly, not through {CALL}."));
    }
    let Some(e) = all.iter().find(|e| e.name == inner) else {
        return Err(format!(
            "{inner} is not available to a client outside Steve. Find the tools it has with {SEARCH}."
        ));
    };
    let inner_args = match &args["arguments"] {
        Value::Null => json!({}),
        v => v.clone(),
    };
    tool_registry::validate(&e.input_schema, &inner_args).map_err(|issues| {
        format!(
            "Invalid arguments for tool {inner}: {}. Its inputSchema: {}",
            issues.join("; "),
            e.input_schema
        )
    })?;
    Box::pin(call(x, o, inner, &inner_args)).await
}

fn text_arg<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args[key].as_str().map(str::trim).filter(|s| !s.is_empty())
}

/// Runs `name` for `o`: the success text, or the error text the client reads.
pub async fn call(x: &Extra, o: &Outside, name: &str, args: &Value) -> Result<String, String> {
    if name == SEARCH || name == CALL {
        return search_or_call(x, o, name, args).await;
    }
    if !allows(name) {
        return Err(format!(
            "{name} is not available to a client outside Steve: it belongs to a Steve session's own turn."
        ));
    }
    let need = |k: &str| {
        text_arg(args, k)
            .ok_or_else(|| format!("Invalid arguments: {k} must be a non-empty string."))
    };
    let (store, ws) = (x.store.clone(), o.ws.clone());
    match name {
        research_mcp::ITEMS_SEARCH => {
            let q = need("query")?.to_string();
            tokio::task::spawn_blocking(move || {
                research_mcp::items_search(&store, &ws, &ItemScope::All, &q)
            })
            .await
            .map_err(|e| e.to_string())?
        }
        research_mcp::ITEM_READ => {
            let id = need("id")?.to_string();
            tokio::task::spawn_blocking(move || {
                research_mcp::item_read(&store, &ws, &ItemScope::All, &id)
            })
            .await
            .map_err(|e| e.to_string())?
        }
        research_mcp::SESSIONS_LIST => {
            let all = args["all"].as_bool().unwrap_or(false);
            tokio::task::spawn_blocking(move || research_mcp::sessions_list(&store, &ws, all))
                .await
                .map_err(|e| e.to_string())?
        }
        steve_knowledge::tools::SEARCH
        | steve_knowledge::tools::READ
        | steve_knowledge::steward::LIST => {
            let cx = steve_modules::contacts::mcp::McpCx {
                ws,
                user: o.user.id.clone(),
                bot_id: "steve".into(),
                item_id: None,
                session_id: None,
            };
            steve_knowledge::tools::call(&x.modules, &x.kb, &cx, name, args).await
        }
        ITEM_CREATE => create(x, o, args).await,
        ITEM_SUPPLEMENT => {
            let (id, text) = (need("id")?.to_string(), need("text")?.to_string());
            let core = x.tools.core.clone();
            tokio::task::spawn_blocking(move || {
                let item = store
                    .items()
                    .get(&id)
                    .map_err(|e| e.to_string())?
                    .filter(|i| i.workspace_id == ws)
                    .ok_or_else(|| format!("No item {id} in this workspace."))?;
                core.supplement(&item.id, &text, steve_core::SupplementMode::Auto, &[])
                    .map_err(|e| e.to_string())?;
                Ok(format!("Added to {} «{}».", item.id, item.title))
            })
            .await
            .map_err(|e| e.to_string())?
        }
        _ => Err(format!("MCP error -32602: Tool {name} not found")),
    }
}

async fn create(x: &Extra, o: &Outside, args: &Value) -> Result<String, String> {
    let obj = args.as_object().cloned().unwrap_or_default();
    if obj
        .keys()
        .any(|k| !["goal", "title", "bot"].contains(&k.as_str()))
    {
        return Err("Invalid arguments: unexpected property.".into());
    }
    let goal = text_arg(args, "goal")
        .ok_or("Invalid arguments: goal must be a non-empty string.")?
        .to_string();
    let title = text_arg(args, "title").map(str::to_string);
    let core = x.tools.core.clone();
    // a Bot named by id or name, of this workspace
    let bot = match text_arg(args, "bot") {
        None => None,
        Some(b) => {
            let (store, ws, b) = (x.store.clone(), o.ws.clone(), b.to_string());
            let found = tokio::task::spawn_blocking(move || {
                store.reader(|c| {
                    let mut st = c.prepare(
                        "SELECT b.id, b.name FROM bots b WHERE b.deleted = 0 AND b.enabled = 1 AND (b.workspace_id = ?1
                           OR EXISTS (SELECT 1 FROM bot_workspaces w WHERE w.bot_id = b.id AND w.workspace_id = ?1)) ORDER BY b.name",
                    )?;
                    let v = st
                        .query_map([&ws], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?
                        .collect::<Result<Vec<_>, _>>()?;
                    Ok::<_, steve_store::StoreError>(v)
                })
            })
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?;
            match found
                .iter()
                .find(|(id, name)| id == &b || name.eq_ignore_ascii_case(&b))
            {
                Some((id, _)) => Some(id.clone()),
                None => {
                    let names: Vec<String> =
                        found.iter().map(|(id, n)| format!("{n} ({id})")).collect();
                    return Err(format!("No Bot {b} here. The Bots: {}.", names.join(", ")));
                }
            }
        }
    };
    let routed = if bot.is_none() || title.is_none() {
        Some(
            core.route(&o.ws, &goal, &[], false)
                .await
                .map_err(|e| e.to_string())?,
        )
    } else {
        None
    };
    let title = title
        .or_else(|| {
            routed
                .as_ref()
                .map(|r| r.title.clone())
                .filter(|t| !t.is_empty())
        })
        .unwrap_or_else(|| goal.chars().take(24).collect());
    let bot = bot.or_else(|| routed.as_ref().map(|r| r.bot.clone()));
    let project = routed.as_ref().map(|r| r.project.clone());
    let tag_ids = routed
        .as_ref()
        .map(|r| r.tag_ids.clone())
        .unwrap_or_default();
    let (ws, me, source) = (o.ws.clone(), o.user.id.clone(), o.token.clone());
    let item = tokio::task::spawn_blocking(move || {
        core.create_item(steve_core::CreateItem {
            ws,
            goal,
            title,
            bot,
            project,
            source: Some((format!("MCP · {source}"), "mcp".into())),
            tag_ids,
            actor: Some(me),
            ..Default::default()
        })
    })
    .await
    .map_err(|e| e.to_string())?
    .map_err(|e| e.to_string())?;
    Ok(format!(
        "Created {} «{}» for {}. Read it with steve_item_read to follow its progress and result.",
        item.id,
        item.title,
        item.bot_id.as_deref().unwrap_or("Steve")
    ))
}

fn rpc(id: &Value, result: Value) -> Response {
    (
        StatusCode::OK,
        [(axum::http::header::CONTENT_TYPE, "application/json")],
        json!({"jsonrpc": "2.0", "id": id, "result": result}).to_string(),
    )
        .into_response()
}

/// One JSON-RPC message from an outside client.
pub async fn handle(x: &Extra, o: Outside, msg: Value) -> Response {
    let id = msg.get("id").cloned().unwrap_or(Value::Null);
    let method = msg["method"].as_str().unwrap_or_default();
    match method {
        "initialize" => rpc(
            &id,
            json!({
                "protocolVersion": msg.pointer("/params/protocolVersion").cloned().unwrap_or(json!("2025-06-18")),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "steve", "version": env!("CARGO_PKG_VERSION")},
                "instructions": "Steve is the user's AI work hub. Find and read their items (ST-…), hand Steve new work, add to an item, and search their knowledge base. The tool list holds the common tools; find the others (reading an item, adding to one, the agent sessions, the user's memories) with steve_tools_search and run them with steve_tool_call.",
            }),
        ),
        m if m.starts_with("notifications/") || msg.get("id").is_none() => {
            StatusCode::ACCEPTED.into_response()
        }
        "ping" => rpc(&id, json!({})),
        "tools/list" => rpc(&id, json!({"tools": listed()})),
        "tools/call" => {
            let name = msg
                .pointer("/params/name")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let args = msg
                .pointer("/params/arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let out = call(x, &o, &name, &args).await;
            match out {
                Ok(t) => rpc(&id, json!({"content": [{"type": "text", "text": t}]})),
                Err(t) => rpc(&id, json!({"content": [{"type": "text", "text": t}], "isError": true})),
            }
        }
        _ => (
            StatusCode::OK,
            [(axum::http::header::CONTENT_TYPE, "application/json")],
            json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": format!("Method not found: {method}")}}).to_string(),
        )
            .into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_outside_client_gets_only_what_needs_no_session() {
        let names: Vec<String> = tools()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            names,
            [
                "steve_knowledge_search",
                "steve_knowledge_read",
                "steve_items_search",
                "steve_item_read",
                "steve_sessions_list",
                "steve_memory_list",
                ITEM_CREATE,
                ITEM_SUPPLEMENT
            ]
        );
        for n in [
            "steve_update",
            "steve_deliver",
            "steve_ask",
            "steve_secret_use",
            "steve_workflow",
            "steve_memory_tidy",
        ] {
            assert!(!allows(n), "{n}");
        }
    }

    #[test]
    fn an_outside_client_lists_a_core_and_finds_the_rest() {
        let names: Vec<String> = listed()
            .iter()
            .map(|t| t["name"].as_str().unwrap().to_string())
            .collect();
        assert_eq!(
            names,
            [
                "steve_knowledge_search",
                "steve_items_search",
                ITEM_CREATE,
                SEARCH,
                CALL
            ]
        );
        let all = entries();
        let skip: Vec<String> = CORE.iter().map(|s| s.to_string()).collect();
        let (found, _) = tool_registry::search(
            &all,
            &skip,
            &tool_registry::Query {
                text: "read one item".into(),
                limit: 3,
                ..Default::default()
            },
        );
        assert_eq!(found[0].name, "steve_item_read");
        // only its own tools: nothing of a session's turn
        assert!(all.iter().all(|e| allows(&e.name)));
    }
}
