//! The modules' MCP tools served next to steve-mcp's five Steve tools without changing steve-mcp: contacts
//! (P3), knowledge (K3/K4), the vault's secret tools (V4), GitHub (G4), mail (E5) and the flow's
//! `steve_event_get` (S10). A tool that opened an approval marks the session as waiting, like
//! `steve_request_approval`: `tools/list` answers get the module's
//! definitions appended, `tools/call` for those names is answered here with the session's [`McpCx`];
//! everything else passes through untouched.

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
    pub store: Store,
    pub modules: Modules,
    pub ports: Ports,
    pub kb: steve_knowledge::Knowledge,
    pub tools: super::modules::Tools,
}

/// Which module answers a tool name.
#[derive(Clone, Copy)]
enum Owner {
    Upgrade,
    Contacts,
    Knowledge,
    Remember,
    Vault,
    Github,
    Mail,
    Flow,
    ReplySource,
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

fn owner_of(name: &str) -> Option<Owner> {
    if name == "steve_upgrade" {
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
    } else {
        None
    }
}

/// The module tool definitions appended to `tools/list`, in this order.
fn extra_tools() -> Vec<Value> {
    let mut v: Vec<Value> = mcp::tools().to_vec();
    v.extend(steve_knowledge::tools::tools().iter().cloned());
    v.push(steve_knowledge::candidates::remember_tool());
    v.extend(steve_modules::vault::mcp::tools().iter().cloned());
    v.extend(steve_github::mcp::tools().iter().cloned());
    v.extend(steve_mail::mcp::tools().iter().cloned());
    v.extend(steve_flow::mcp::tools().iter().cloned());
    v.push(reply_source_tool());
    v.push(json!({"name":"steve_upgrade","description":"Check Steve GitHub releases, inspect an upgrade, or request an instance upgrade. A request requires instance owner approval in Settings / Versions before execution. Never grants approval through an agent token.","inputSchema":{"type":"object","properties":{"action":{"type":"string","enum":["check","status","request"]},"version":{"type":"string"},"idempotency_key":{"type":"string"}},"required":["action"]}}));
    v
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

pub async fn layer(State(x): State<Extra>, req: Request, next: Next) -> Response {
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

    if method == "tools/call" {
        let m = msg.clone().unwrap_or(Value::Null);
        let name = m
            .pointer("/params/name")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if let Some(tool_owner) = owner_of(&name) {
            let token = parts
                .headers
                .get(header::AUTHORIZATION)
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.strip_prefix("Bearer "))
                .map(str::to_string);
            let store = x.store.clone();
            let session = match token {
                Some(t) => tokio::task::spawn_blocking(move || {
                    store.sessions().by_token(&t).ok().flatten()
                })
                .await
                .ok()
                .flatten(),
                None => None,
            };
            let Some(s) = session else {
                return StatusCode::UNAUTHORIZED.into_response();
            };
            let args = m
                .pointer("/params/arguments")
                .cloned()
                .unwrap_or_else(|| json!({}));
            let id = m.get("id").cloned().unwrap_or(Value::Null);
            let out = call_owned(&x, s, tool_owner, &name, args).await;
            return match out {
                Ok(text) => json_rpc(
                    &id,
                    json!({ "content": [{ "type": "text", "text": text }] }),
                ),
                Err(text) => json_rpc(
                    &id,
                    json!({ "content": [{ "type": "text", "text": text }], "isError": true }),
                ),
            };
        }
    }

    let res = next.run(rebuild(bytes)).await;
    if method != "tools/list" || res.status() != StatusCode::OK {
        return res;
    }
    let is_json = res
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .is_some_and(|t| t.starts_with("application/json"));
    if !is_json {
        return res;
    }
    let (mut rp, rb) = res.into_parts();
    let Ok(out) = axum::body::to_bytes(rb, LIMIT).await else {
        return StatusCode::INTERNAL_SERVER_ERROR.into_response();
    };
    let Ok(mut v) = serde_json::from_slice::<Value>(&out) else {
        return Response::from_parts(rp, Body::from(out));
    };
    if let Some(tools) = v.pointer_mut("/result/tools").and_then(Value::as_array_mut) {
        tools.extend(extra_tools());
    }
    rp.headers.remove(header::CONTENT_LENGTH);
    Response::from_parts(rp, Body::from(v.to_string()))
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
            let (m, kb) = (x.modules.clone(), x.kb.clone());
            tokio::task::spawn_blocking(move || {
                steve_knowledge::candidates::remember(&m, &kb, &cx, &args)
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

/// The modules' tool definitions (what `tools/list` adds).
pub(crate) fn module_tools() -> Vec<Value> {
    extra_tools()
}
