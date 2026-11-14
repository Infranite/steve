//! Session MCP routing and dispatch for the modules' Steve tools. Authenticated session list/call
//! requests go through [`super::tool_service::ToolService`], as does the Node tool bridge: one
//! registry, core plus Bot pins, and search/call for the rest. User access tokens retain the scoped
//! [`super::outside_mcp`] catalog; research tokens retain their separate research catalog. Session
//! tokens stay loopback-only. Module calls keep their existing approval and session context.

use axum::body::{Body, Bytes};
use axum::extract::{Request, State};
use axum::http::{StatusCode, header};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};
use steve_modules::Modules;
use steve_modules::contacts::{Ports, mcp};
use steve_store::Store;

#[derive(Clone)]
pub struct Extra {
    /// 能力: the remote agents a session's Bot uses, as tools.
    pub caps: steve_modules::caps::Capabilities,
    pub store: Store,
    pub modules: Modules,
    pub ports: Ports,
    pub kb: steve_knowledge::Knowledge,
    pub tools: super::modules::Tools,
    /// The Hub's connections to the library's MCP servers.
    pub library: std::sync::Arc<super::library_mcp::LibraryMcp>,
}

/// Which module answers a tool name.
#[derive(Clone, Copy)]
pub(crate) enum Owner {
    Devices,
    Diagnostics,
    Items,
    Workflow,
    Upgrade,
    Contacts,
    Knowledge,
    Remember,
    Vault,
    Github,
    Mail,
    Flow,
    ReplySource,
    Im,
    A2a,
}

/// `steve_devices` (B7): the Nodes work can run on, with their tags; `require` sends this item's
/// next sessions to a Node with a tag.
pub const DEVICES: &str = "steve_devices";

fn devices_tool() -> Value {
    json!({
        "name": DEVICES,
        "description": "List the devices (Nodes) Steve can run work on, with their tags and whether they are online; the Hub's own machine is `local`. With action `require` and a tag, this item's next sessions go to a device with that tag (a continuation in a new session, a handed-over item); `require` with no tag clears it.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["list", "require"]},
                "tag": {"type": "string"}
            },
            "required": ["action"],
            "additionalProperties": false
        }
    })
}

/// `steve_devices`: list, or require a tag for the item of `cx`.
fn devices(store: &Store, cx: &mcp::McpCx, args: &Value) -> Result<String, String> {
    match args["action"].as_str() {
        Some("require") => {
            let item = cx.item_id.as_deref().ok_or("这个会话不属于任何事项")?;
            let tag = args["tag"].as_str().filter(|t| !t.trim().is_empty());
            let ok = store
                .transaction(|tx| steve_store::nodes::set_item_tag(tx, item, tag))
                .map_err(|e| e.to_string())?;
            if !ok {
                return Err(steve_i18n::t!("hub.node.tag_bad"));
            }
            Ok(match tag {
                Some(t) => format!("{item} 之后的会话会安排到带标签「{t}」的设备上"),
                None => format!("{item} 不再要求设备标签"),
            })
        }
        _ => {
            let since = crate::nodes::now() - 3 * 60 * 1000;
            let v = store
                .reader(|c| {
                    let tags = steve_store::nodes::all_tags(c)?;
                    let online = |id: &str| -> Result<bool, steve_store::StoreError> {
                        Ok(c.query_row(
                            "SELECT EXISTS(SELECT 1 FROM node_vitals WHERE node_id=? AND at>=?)",
                            rusqlite::params![id, since],
                            |r| r.get(0),
                        )?)
                    };
                    let mut out = vec![json!({
                        "node": "local", "name": "Hub", "tags": tags.get("local").cloned().unwrap_or_default(),
                        "online": true
                    })];
                    let mut st = c.prepare(
                        "SELECT node_id, name, state FROM nodes WHERE local = 0 AND state IN ('active','paused') ORDER BY name",
                    )?;
                    let rows = st
                        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?)))?
                        .collect::<Result<Vec<_>, _>>()?;
                    for (id, name, state) in rows {
                        out.push(json!({
                            "node": id, "name": name, "tags": tags.get(&id).cloned().unwrap_or_default(),
                            "online": online(&id)?, "paused": state == "paused"
                        }));
                    }
                    Ok::<_, steve_store::StoreError>(out)
                })
                .map_err(|e| e.to_string())?;
            Ok(serde_json::to_string(&json!({ "devices": v })).unwrap_or_default())
        }
    }
}

/// `steve_reply_source` (channels-personas.md §6): an update back to the chat an item came from.
/// Not auto-allowed: on a bot account it can post outward.
pub const REPLY_SOURCE: &str = "steve_reply_source";

fn reply_source_tool() -> Value {
    json!({
        "name": REPLY_SOURCE,
        "description": "Post a short progress update or result back to the chat thread this item came from. Steve speaks as the persona configured there and checks it does not reveal anything the audience should not see.",
        "inputSchema": {
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "properties": { "text": { "type": "string", "description": "in the language of that chat" } },
            "required": ["text"]
        }
    })
}

pub(crate) fn owner_of(name: &str) -> Option<Owner> {
    if name == DEVICES {
        Some(Owner::Devices)
    } else if name == super::diagnostics::TOOL {
        Some(Owner::Diagnostics)
    } else if super::item_mcp::handles(name) {
        Some(Owner::Items)
    } else if name == "steve_workflow" {
        Some(Owner::Workflow)
    } else if name == "steve_upgrade" {
        Some(Owner::Upgrade)
    } else if mcp::handles(name) {
        Some(Owner::Contacts)
    } else if steve_knowledge::tools::handles(name) {
        Some(Owner::Knowledge)
    } else if name == steve_knowledge::candidates::REMEMBER {
        Some(Owner::Remember)
    } else if steve_modules::vault::mcp::handles(name) {
        Some(Owner::Vault)
    } else if steve_github::mcp::handles(name) {
        Some(Owner::Github)
    } else if steve_mail::mcp::handles(name) {
        Some(Owner::Mail)
    } else if steve_flow::mcp::handles(name) {
        Some(Owner::Flow)
    } else if name == REPLY_SOURCE {
        Some(Owner::ReplySource)
    } else if steve_channels::im_mcp::handles(name) {
        Some(Owner::Im)
    } else if name.starts_with(steve_modules::caps::a2a::TOOL_PREFIX) {
        Some(Owner::A2a)
    } else {
        None
    }
}

/// All built-in module definitions, before registry selection.
fn extra_tools() -> Vec<Value> {
    let mut v: Vec<Value> = mcp::tools().to_vec();
    v.push(super::diagnostics::tool());
    v.extend(super::item_mcp::tools());
    v.push(workflow_tool());
    v.extend(steve_knowledge::tools::all_tools().iter().cloned());
    v.push(steve_knowledge::candidates::remember_tool());
    v.extend(steve_modules::vault::mcp::tools().iter().cloned());
    v.extend(steve_github::mcp::tools().iter().cloned());
    v.extend(steve_mail::mcp::tools().iter().cloned());
    v.extend(steve_flow::mcp::tools().iter().cloned());
    v.push(reply_source_tool());
    v.push(devices_tool());
    v.extend(steve_channels::im_mcp::tools());
    v.push(json!({"name":"steve_upgrade","description":"Check Steve GitHub releases, inspect an upgrade, or request an instance upgrade. A request requires instance owner approval in Settings / Versions before execution. Never grants approval through an agent token.","inputSchema":{"type":"object","properties":{"action":{"type":"string","enum":["check","status","request"]},"version":{"type":"string"},"idempotency_key":{"type":"string"}},"required":["action"]}}));
    v
}

fn workflow_tool() -> Value {
    json!({
        "name": "steve_workflow",
        "description": "Read or replan this item's shared work graph. Only the parent item's responsible Bot may change the graph; child Bots can read their own work and upstream deliveries, deliver success through steve_deliver, or report a failure / rejected review with action fail and reason, then end their turn. A work graph exists to finish sooner: plan units that can run at the same time (split by independent scope, not by phase), plus an independent review and an integration unit. Dependencies must pass before successors run, and units sharing a resource key never run together, so keep keys fine-grained. Every ready unit runs unless max_parallel limits it; set it only for a real constraint. A plan reply may carry notes on what keeps its units from running at once. Graph changes require the latest expected_revision and a reason. A new graph starts at revision 0. A plan replaces the active specification; preserve existing unit IDs for unchanged work. Cancelling units does not waive required acceptance. Only the user can explicitly switch to single-session acceptance on the item page after pausing; Bot tokens cannot approve it. Do not transfer credentials or permissions between Bots.",
        "inputSchema": {
            "type": "object",
            "properties": {
                "action": {"type": "string", "enum": ["get", "plan", "pause", "resume", "retry", "cancel", "fail"]},
                "expected_revision": {"type": "integer", "minimum": 0},
                "reason": {"type": "string", "minLength": 1},
                "unit_id": {"type": "string"},
                "max_parallel": {"type": "integer", "minimum": 1, "description": "Leave out: every ready unit whose resources are free runs. Set only for a real limit such as a shared device or a rate limit."},
                "max_attempts": {"type": "integer", "minimum": 1},
                "units": {
                    "type": "array",
                    "items": {
                        "type": "object",
                        "properties": {
                            "id": {"type": "string", "minLength": 1},
                            "title": {"type": "string", "minLength": 1},
                            "goal": {"type": "string", "minLength": 1, "description": "The unit's brief as short Markdown people read on the item page: one sentence on what this unit delivers, then bullet sections **要做**, **验收**, and only if needed **上游** (which upstream results to use) and **约束**. Each bullet one line; no walls of text, logs or long file lists (put details in the conversation or upstream outputs)."},
                            "bot_id": {"type": "string", "minLength": 1},
                            "kind": {"type": "string", "enum": ["work", "review", "integrate"]},
                            "depends_on": {"type": "array", "items": {"type": "string"}},
                            "resources": {"type": "array", "items": {"type": "string"}},
                            "required": {"type": "boolean"}
                        },
                        "required": ["id", "title", "goal", "bot_id", "kind", "depends_on", "resources", "required"],
                        "additionalProperties": false
                    }
                }
            },
            "required": ["action"],
            "additionalProperties": false
        }
    })
}

/// A tool result that opened an approval: the session waits for the user.
fn approval(
    x: &Extra,
    cx: &mcp::McpCx,
    out: Result<steve_modules::vault::mcp::ToolOut, String>,
) -> Result<String, String> {
    out.map(|o| {
        if o.awaiting_approval
            && let Some(sid) = &cx.session_id
        {
            super::ports::awaiting_approval(&x.tools.core, sid);
        }
        o.text
    })
}

const LIMIT: usize = 4 * 1024 * 1024;

fn json_rpc(id: &Value, result: Value) -> Response {
    let body = json!({ "jsonrpc": "2.0", "id": id, "result": result });
    (
        StatusCode::OK,
        [(header::CONTENT_TYPE, "application/json")],
        body.to_string(),
    )
        .into_response()
}

/// The user an agent works for. The Hub is single-user for now, so that is the workspace's owner (the
/// first owner membership); with several users this becomes the item's requester.
pub(crate) fn owner(store: &Store, ws: &str) -> Option<String> {
    store
        .with_conn(|c| {
            use rusqlite::OptionalExtension;
            Ok(c.query_row(
                "SELECT user_id FROM workspace_members WHERE workspace_id = ? AND role = 'owner' ORDER BY created LIMIT 1",
                [ws],
                |r| r.get::<_, String>(0),
            )
            .optional()?)
        })
        .ok()
        .flatten()
}

pub async fn layer(
    State(service): State<std::sync::Arc<super::tool_service::ToolService>>,
    req: Request,
    next: Next,
) -> Response {
    let x = &service.extra;
    let (parts, body) = req.into_parts();
    let Ok(bytes) = axum::body::to_bytes(body, LIMIT).await else {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    };
    let msg: Option<Value> = serde_json::from_slice(&bytes).ok();
    let method = msg
        .as_ref()
        .and_then(|m| m.get("method"))
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let rebuild = |b: Bytes| Request::from_parts(parts.clone(), Body::from(b));

    /* a research run's token: only the read-only research tools, answered here (thread-routing §7.4) */
    let bearer = parts
        .headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .map(str::to_string);
    /* a user's access token: a client outside Steve, only the tools that need no session */
    if let Some(t) = bearer
        .clone()
        .filter(|t| t.starts_with(steve_store::access::TOKEN_PREFIX))
    {
        let ws = parts
            .headers
            .get("x-steve-ws")
            .and_then(|v| v.to_str().ok())
            .map(str::to_string)
            .or_else(|| {
                parts.uri.query().and_then(|q| {
                    q.split('&')
                        .find_map(|kv| kv.strip_prefix("ws="))
                        .map(str::to_string)
                })
            });
        let store = x.store.clone();
        let who = tokio::task::spawn_blocking(move || {
            super::outside_mcp::identify(&store, &t, ws.as_deref())
        })
        .await
        .ok()
        .flatten();
        let Some(who) = who else {
            return StatusCode::UNAUTHORIZED.into_response();
        };
        if parts.method != axum::http::Method::POST {
            return StatusCode::METHOD_NOT_ALLOWED.into_response();
        }
        let Some(m) = msg.clone() else {
            return StatusCode::BAD_REQUEST.into_response();
        };
        return super::outside_mcp::handle(&x, who, m).await;
    }
    if let Some(t) = bearer.clone() {
        let store = x.store.clone();
        let run =
            tokio::task::spawn_blocking(move || store.thread().run_by_token(&t).ok().flatten())
                .await
                .ok()
                .flatten();
        if let Some(run) = run {
            if parts.method != axum::http::Method::POST {
                return StatusCode::METHOD_NOT_ALLOWED.into_response();
            }
            let Some(m) = msg.clone() else {
                return StatusCode::BAD_REQUEST.into_response();
            };
            let (store, ws) = (x.store.clone(), run.workspace_id.clone());
            let owner = tokio::task::spawn_blocking(move || owner(&store, &ws))
                .await
                .ok()
                .flatten()
                .unwrap_or_default();
            return super::research_mcp::handle(&x, run, m, owner).await;
        }
    }

    // Session tokens remain loopback-only, exactly as steve-mcp's authentication layer.
    // Do not let intercepting tools/list or tools/call bypass that boundary.
    if method == "tools/list" || method == "tools/call" || method == "initialize" {
        use axum::extract::{ConnectInfo, FromRequestParts};
        let mut auth_parts = parts.clone();
        let peer = ConnectInfo::<std::net::SocketAddr>::from_request_parts(&mut auth_parts, &())
            .await
            .ok()
            .map(|c| c.0);
        if !peer.is_some_and(|p| p.ip().is_loopback()
            || matches!(p.ip(), std::net::IpAddr::V6(v) if v.to_ipv4_mapped().is_some_and(|v| v.is_loopback()))) {
            return StatusCode::FORBIDDEN.into_response();
        }
        let store = x.store.clone();
        let session = match bearer {
            Some(t) => {
                tokio::task::spawn_blocking(move || store.sessions().by_token(&t).ok().flatten())
                    .await
                    .ok()
                    .flatten()
            }
            None => None,
        };
        let Some(s) = session else {
            return StatusCode::UNAUTHORIZED.into_response();
        };
        if parts.method != axum::http::Method::POST {
            return StatusCode::METHOD_NOT_ALLOWED.into_response();
        }
        let m = msg.as_ref().unwrap();
        let id = m.get("id").cloned().unwrap_or(Value::Null);
        if method == "tools/list" {
            let r = service
                .registry(&s, super::tool_service::Reach::Listed)
                .await;
            let tools: Vec<Value> = r.listed_entries().iter().map(|e| e.mcp()).collect();
            return json_rpc(&id, json!({"tools": tools}));
        }
        if method == "initialize" {
            return json_rpc(
                &id,
                json!({
                    "protocolVersion": m.pointer("/params/protocolVersion").cloned().unwrap_or(json!("2025-06-18")),
                    "capabilities": {"tools": {}},
                    "serverInfo": {"name": "steve", "version": env!("CARGO_PKG_VERSION")},
                    "instructions": "Your list contains Steve's core tools and your Bot's pins. Find other enabled tools with steve_tools_search, then run them with steve_tool_call {name, arguments}. The tool catalog in your brief describes the available areas."
                }),
            );
        }
        let name = m
            .pointer("/params/name")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let args = m
            .pointer("/params/arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let (text, failed) = match service.call(s, name, args).await {
            Ok(t) => (t, false),
            Err(t) => (t, true),
        };
        return json_rpc(
            &id,
            json!({
                "content": [{"type": "text", "text": super::tool_service::bound_result(name, text)}],
                "isError": failed,
            }),
        );
    }
    next.run(rebuild(bytes)).await
}

/// A module's tool (`owner`), run for session `s`: the success text or the error text. Independent of
/// any tool protocol (MCP wraps it below; the Node path's tool service calls it too).
async fn call_owned(
    x: &Extra,
    s: steve_store::Session,
    tool_owner: Owner,
    name: &str,
    args: Value,
) -> Result<String, String> {
    let observation = super::diagnostics::ToolObservation::new(&x.store, &s, name);
    observation
        .run(call_owned_inner(x, s, tool_owner, name, args))
        .await
}

async fn call_owned_inner(
    x: &Extra,
    s: steve_store::Session,
    tool_owner: Owner,
    name: &str,
    args: Value,
) -> Result<String, String> {
    if matches!(tool_owner, Owner::Diagnostics) {
        let (store, core) = (x.store.clone(), x.tools.core.clone());
        return tokio::task::spawn_blocking(steve_i18n::carry(move || {
            super::diagnostics::for_session(&store, &core, &s, args)
        }))
        .await
        .map_err(|_| "Diagnostic query unavailable.".to_string())?;
    }
    if matches!(tool_owner, Owner::Items) {
        let (store, core, name) = (x.store.clone(), x.tools.core.clone(), name.to_owned());
        return tokio::task::spawn_blocking(steve_i18n::carry(move || {
            super::item_mcp::call(&store, &core, &s, &name, args)
        }))
        .await
        .map_err(|e| e.to_string())?;
    }
    if matches!(tool_owner, Owner::Workflow) {
        // Preserve the authenticated session itself. No caller-supplied item, workspace or
        // principal can replace the scope established by its bearer token / Node run.
        let core = x.tools.core.clone();
        let ends_turn = args["action"] == "fail";
        return tokio::task::spawn_blocking(steve_i18n::carry(move || {
            core.tool_workflow(&s, args)
                .map(|v| {
                    if ends_turn {
                        format!("{v}\n\n{}", steve_mcp::END)
                    } else {
                        v.to_string()
                    }
                })
                .map_err(|e| e.to_string())
        }))
        .await
        .map_err(|e| e.to_string())?;
    }
    if matches!(tool_owner, Owner::A2a) {
        return x.caps.a2a_call(&s.id, name, &args).await;
    }
    let store = x.store.clone();
    let ws = s.workspace_id.clone();
    let user = tokio::task::spawn_blocking(move || owner(&store, &ws))
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
    let cx = mcp::McpCx {
        ws: s.workspace_id,
        user,
        bot_id: s.bot_id.unwrap_or_else(|| "agent".into()),
        item_id: s.item_id,
        session_id: Some(s.id),
    };
    let out: Result<String, String> = match tool_owner {
        Owner::Diagnostics => unreachable!("diagnostics preserve the session above"),
        Owner::Items => unreachable!("item management preserves the session above"),
        Owner::Workflow => unreachable!("workflow preserves the session above"),
        Owner::A2a => unreachable!("remote agents are answered above"),
        Owner::Upgrade => {
            let ws = cx.ws.clone();
            let store = x.store.clone();
            let may_request=tokio::task::spawn_blocking(move||store.reader(|c|{let (members,owners):(i64,i64)=c.query_row("SELECT COUNT(*),COALESCE(SUM(CASE WHEN u.role='owner' THEN 1 ELSE 0 END),0) FROM workspace_members wm JOIN users u ON u.id=wm.user_id WHERE wm.workspace_id=?",[ws],|r|Ok((r.get(0)?,r.get(1)?)))?;Ok::<_,steve_store::StoreError>(members==1&&owners==1)}).unwrap_or(false)).await.unwrap_or(false);
            if args["action"] == "request" && !may_request {
                Err("共享工作空间或非实例管理员的工作空间目前只能检查/查看升级；请实例管理员从设置发起升级".into())
            } else {
                match crate::config::Config::from_env()
                    .map_err(|e| e.to_string())
                    .and_then(|cfg| crate::upgrades::Updates::new(&cfg).map_err(|e| e.to_string()))
                {
                    Ok(u) => u
                        .agent(cx.session_id.as_deref().unwrap_or_default(), &args)
                        .await
                        .map_err(|e| e.to_string()),
                    Err(e) => Err(e),
                }
            }
        }
        Owner::Contacts => mcp::call(&x.modules, &x.ports, &cx, name, &args).await,
        Owner::Knowledge => steve_knowledge::tools::call(&x.modules, &x.kb, &cx, name, &args).await,
        // synchronous (one write transaction): off the runtime
        Owner::Vault => {
            let ucx = steve_modules::vault::UseCx::from(&cx);
            let out = steve_modules::vault::mcp::call(&x.tools.uses, &ucx, name, &args).await;
            approval(x, &cx, out)
        }
        Owner::Github => {
            let out = steve_github::mcp::call(&x.tools.gh, &cx, name, &args).await;
            approval(x, &cx, out)
        }
        // `steve_mail_draft` marks the session itself (`DraftHooks::awaiting_approval`)
        Owner::Mail => steve_mail::mcp::call(&x.tools.mail, &cx, name, &args).await,
        Owner::Flow => steve_flow::mcp::call(&x.tools.flow, &cx, name, &args).await,
        Owner::Im => steve_channels::im_mcp::call(&x.tools.channels, &cx, name, &args).await,
        Owner::Devices => {
            let (store, cx2, args2) = (x.store.clone(), cx.clone(), args.clone());
            tokio::task::spawn_blocking(move || devices(&store, &cx2, &args2))
                .await
                .map_err(|e| e.to_string())?
        }
        Owner::ReplySource => match (
            args.get("text").and_then(Value::as_str),
            cx.item_id.as_deref(),
        ) {
            (None, _) => Err(format!(
                "MCP error -32602: Input validation error: Invalid arguments for tool {REPLY_SOURCE}: Invalid input: expected string, received {} at text",
                if args.get("text").is_none() {
                    "undefined"
                } else {
                    "other"
                }
            )),
            (Some(_), None) => Ok(steve_channels::outbound::SRC_NO_SOURCE.to_string()),
            (Some(text), Some(item)) => x
                .tools
                .outbox
                .reply_source(&x.tools.channels, &cx.ws, item, text)
                .await
                .map_err(|e| e.to_string()),
        },
        Owner::Remember => {
            let m = x.modules.clone();
            tokio::task::spawn_blocking(move || {
                steve_knowledge::candidates::remember(&m, &cx, &args)
            })
            .await
            .unwrap_or_else(|e| Err(e.to_string()))
        }
    };
    out
}

/// A module's tool by name; `None`: no module serves it.
pub(crate) async fn call_module(
    x: &Extra,
    s: steve_store::Session,
    name: &str,
    args: Value,
) -> Option<Result<String, String>> {
    let owner = owner_of(name)?;
    Some(call_owned(x, s, owner, name, args).await)
}

/// The modules' complete definitions for the shared registry.
pub(crate) fn module_tools() -> Vec<Value> {
    extra_tools()
}
