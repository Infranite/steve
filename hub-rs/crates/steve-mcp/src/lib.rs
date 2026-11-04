//! Steve tools for agents, served over MCP streamable HTTP (replaces `hub/src/mcp.ts`).
//!
//! Each agent session gets a bearer token scoped to its own item. Asking, requesting approval
//! and delivering are "send and end the turn": the answer arrives later as a new message, so
//! nothing waits in memory and a Hub restart loses nothing. The server is stateless (no
//! `Mcp-Session-Id`), like the TS server with `sessionIdGenerator: undefined`.
//!
//! The Hub supplies a [`Backend`] (token lookup + the five actions) and mounts [`router`] for
//! every path that starts with `/mcp`. Serve the app with
//! `into_make_service_with_connect_info::<SocketAddr>()`: requests from a non-loopback peer
//! (or without `ConnectInfo`) are refused with `403`.

use std::{future::Future, net::SocketAddr, sync::Arc};

use axum::{
    Router,
    body::Body,
    extract::{ConnectInfo, FromRequestParts, Request, State},
    http::{StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
};
use rmcp::{
    ErrorData as McpError, RoleServer, ServerHandler,
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ContentBlock, Implementation,
        JsonObject, ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig,
        ToolsCapability,
    },
    service::RequestContext,
    transport::{
        StreamableHttpServerConfig, StreamableHttpService,
        streamable_http_server::session::never::NeverSessionManager,
    },
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};

#[doc(hidden)]
pub mod testing;
pub mod tools;

pub use tools::END;

/// Error from a tool action; its `Display` text is returned to the agent as an `isError` result,
/// as the TS SDK does with a thrown `Error.message`.
pub type ToolError = Box<dyn std::error::Error + Send + Sync>;

/// One step of the agent's plan (`steve_update`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PlanStep {
    pub step: String,
    pub done: bool,
}

/// Something the agent produced (`steve_deliver`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Output {
    pub kind: String,
    pub title: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub location: Option<String>,
    /// `record`: documents the work (a self-check screenshot, a log, an intermediate file) rather than
    /// being its result; shown with the item's work record instead of its outputs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
}

/// What the agent leans towards on a `steve_ask` (both optional; YOLO may pick `recommended`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AskHint {
    pub recommended: Option<String>,
    pub why: Option<String>,
}

/// A check the agent ran (`steve_deliver`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Check {
    pub name: String,
    pub passed: bool,
}

/// What the MCP server needs from the Hub. `Session` is the session row as of the request.
pub trait Backend: Send + Sync + 'static {
    type Session: Clone + Send + Sync + 'static;

    /// The session whose `token_hash` is `sha256hex(token)`, if any.
    fn session_by_token(&self, token: &str) -> impl Future<Output = Option<Self::Session>> + Send;

    fn update(
        &self,
        s: &Self::Session,
        now: String,
        plan: Vec<PlanStep>,
    ) -> impl Future<Output = Result<(), ToolError>> + Send;

    fn ask(
        &self,
        s: &Self::Session,
        question: String,
        options: Vec<String>,
        hint: AskHint,
    ) -> impl Future<Output = Result<(), ToolError>> + Send;

    fn request_approval(
        &self,
        s: &Self::Session,
        action: String,
        detail: String,
    ) -> impl Future<Output = Result<(), ToolError>> + Send;

    /// `unfinished`: plan steps (exact text) the agent deliberately left open; absent when not sent.
    fn deliver(
        &self,
        s: &Self::Session,
        summary: String,
        outputs: Vec<Output>,
        checks: Vec<Check>,
        unfinished: Option<Vec<String>>,
    ) -> impl Future<Output = Result<(), ToolError>> + Send;

    fn publish(
        &self,
        s: &Self::Session,
        branch: String,
        summary: String,
    ) -> impl Future<Output = Result<(), ToolError>> + Send;
}

/// The authenticated session, carried from the auth middleware to the tool handler through
/// the request extensions (rmcp exposes the request `Parts` to handlers).
#[derive(Clone)]
struct Authed<S>(S);

/// The MCP endpoint, path-agnostic: route every request whose path starts with `/mcp` to it.
pub fn router<B: Backend>(backend: Arc<B>) -> Router {
    let factory = {
        let backend = backend.clone();
        move || {
            Ok(Handler {
                backend: backend.clone(),
            })
        }
    };
    let config = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        .with_json_response(true);
    let svc = StreamableHttpService::new(factory, Arc::new(NeverSessionManager::default()), config);
    Router::new()
        .fallback_service(svc)
        .layer(middleware::from_fn_with_state(backend, auth::<B>))
}

/// Same peer test as the TS regex `^(127\.|::1|::ffff:127\.)`.
fn is_loopback(addr: &SocketAddr) -> bool {
    match addr {
        SocketAddr::V4(a) => a.ip().is_loopback(),
        SocketAddr::V6(a) => {
            a.ip().is_loopback() || a.ip().to_ipv4_mapped().is_some_and(|v4| v4.is_loopback())
        }
    }
}

fn empty(status: StatusCode) -> Response {
    (status, Body::empty()).into_response()
}

async fn auth<B: Backend>(State(backend): State<Arc<B>>, req: Request, next: Next) -> Response {
    // Through the extractor so `MockConnectInfo` works in tests.
    let (mut parts, body) = req.into_parts();
    let peer = ConnectInfo::<SocketAddr>::from_request_parts(&mut parts, &())
        .await
        .ok()
        .map(|c| c.0);
    let mut req = Request::from_parts(parts, body);
    if !peer.is_some_and(|p| is_loopback(&p)) {
        return empty(StatusCode::FORBIDDEN);
    }
    let token = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    let session = if token.is_empty() {
        None
    } else {
        backend.session_by_token(token).await
    };
    let Some(session) = session else {
        return empty(StatusCode::UNAUTHORIZED);
    };
    req.extensions_mut().insert(Authed(session));
    next.run(req).await
}

struct Handler<B> {
    backend: Arc<B>,
}

fn text(t: impl Into<String>) -> Vec<ContentBlock> {
    vec![ContentBlock::text(t)]
}

fn parse<T: DeserializeOwned>(args: &JsonObject) -> Result<T, String> {
    serde_json::from_value(serde_json::Value::Object(args.clone())).map_err(|e| e.to_string())
}

#[derive(Deserialize)]
struct UpdateArgs {
    now: String,
    plan: Vec<PlanStep>,
}
#[derive(Deserialize)]
struct AskArgs {
    question: String,
    options: Vec<String>,
    #[serde(default)]
    recommended: Option<String>,
    #[serde(default)]
    why: Option<String>,
}
#[derive(Deserialize)]
struct ApprovalArgs {
    action: String,
    detail: String,
}
#[derive(Deserialize)]
struct DeliverArgs {
    summary: String,
    outputs: Vec<Output>,
    checks: Vec<Check>,
    #[serde(default)]
    unfinished: Option<Vec<String>>,
}
#[derive(Deserialize)]
struct PublishArgs {
    branch: String,
    summary: String,
}

impl<B: Backend> Handler<B> {
    /// Runs one tool; `Ok` is the success text, `Err` the `isError` text.
    async fn run(&self, s: &B::Session, name: &str, args: &JsonObject) -> Result<String, String> {
        tools::validate(name, args)?;
        let b = &*self.backend;
        let done = match name {
            tools::UPDATE => {
                let a: UpdateArgs = parse(args)?;
                b.update(s, a.now, a.plan)
                    .await
                    .map(|_| "Steve updated the item.".to_owned())
            }
            tools::ASK => {
                let a: AskArgs = parse(args)?;
                let hint = AskHint {
                    recommended: a.recommended,
                    why: a.why,
                };
                b.ask(s, a.question, a.options, hint)
                    .await
                    .map(|_| format!("Question sent to the user. {END}"))
            }
            tools::REQUEST_APPROVAL => {
                let a: ApprovalArgs = parse(args)?;
                b.request_approval(s, a.action, a.detail)
                    .await
                    .map(|_| format!("Approval requested. {END}"))
            }
            tools::DELIVER => {
                let a: DeliverArgs = parse(args)?;
                b.deliver(s, a.summary, a.outputs, a.checks, a.unfinished)
                    .await
                    .map(|_| format!("Delivered for acceptance. {END}"))
            }
            tools::PUBLISH => {
                let a: PublishArgs = parse(args)?;
                b.publish(s, a.branch, a.summary)
                    .await
                    .map(|_| format!("Release requested. {END}"))
            }
            _ => return Err(format!("MCP error -32602: Tool {name} not found")),
        };
        done.map_err(|e| {
            tracing::warn!(tool = name, error = %e, "steve tool failed");
            e.to_string()
        })
    }
}

impl<B: Backend> ServerHandler for Handler<B> {
    fn get_info(&self) -> ServerConfig {
        let mut tools = ToolsCapability::default();
        tools.list_changed = Some(true);
        let mut caps = ServerCapabilities::default();
        caps.tools = Some(tools);
        ServerConfig::new(caps).with_server_info(Implementation::new("steve", "1.0.0"))
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        _ctx: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        Ok(ListToolsResult::with_all_items(tools::list()))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let session = ctx
            .extensions
            .get::<axum::http::request::Parts>()
            .and_then(|p| p.extensions.get::<Authed<B::Session>>())
            .map(|a| a.0.clone())
            // Unreachable behind the auth middleware; fail closed anyway.
            .ok_or_else(|| McpError::internal_error("no Steve session for this request", None))?;
        let args = request.arguments.unwrap_or_default();
        let result = match self.run(&session, &request.name, &args).await {
            Ok(t) => CallToolResult::success(text(t)),
            Err(t) => CallToolResult::error(text(t)),
        };
        Ok(CallToolResponse::Complete(result))
    }
}
