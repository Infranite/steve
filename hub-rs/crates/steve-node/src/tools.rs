//! Steve's tools as harnesses reach them: an MCP endpoint on this machine's loopback that turns each
//! call into a [`ToolCall`] to the Hub ([`HubTools`]). The Hub defines, authorizes, runs and audits
//! the tools; this only speaks MCP to the agent. A token stands for one session of this Node, and a
//! call counts for the attempt that session runs at that moment (none running: refused); or for one
//! job attempt, valid until that job ends. Listing the tools is not a call: a harness lists them
//! when it connects, which can be before its session's first turn (Claude Code keeps what it got
//! then), so a session's list waits for its attempt a while, or answers from the catalog this Node
//! last had from the Hub.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

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
        ListToolsResult, PaginatedRequestParams, ServerCapabilities, ServerConfig, Tool,
        ToolAnnotations, ToolsCapability,
    },
    service::RequestContext,
    transport::{
        StreamableHttpServerConfig, StreamableHttpService,
        streamable_http_server::session::never::NeverSessionManager,
    },
};
use serde_json::Value;
use steve_node_api::*;

/// Who is running what: the Node answers which attempt a session runs now.
pub trait Current: Send + Sync + 'static {
    fn execution(&self, session: &SessionId) -> Option<ExecutionRef>;
}

/// Whom a token was given to.
#[derive(Clone, PartialEq, Eq, Hash)]
enum Holder {
    /// A session's harness: its calls count for the attempt it runs at the time.
    Session(SessionId),
    /// One job attempt's harness.
    Job(ExecutionRef),
}

pub(crate) struct Endpoint {
    hub: Mutex<Option<Arc<dyn HubTools>>>,
    tokens: Mutex<HashMap<String, Holder>>,
    current: Arc<dyn Current>,
    calls: AtomicU64,
    /// The last catalog per token holder, never another Bot's pins or a research grant. Idle
    /// sessions may reuse their own list; calls always fetch and authorize it anew.
    catalog: Mutex<HashMap<Holder, Catalog>>,
    /// Random per process. The Hub keys its durable tool journal by (execution, CallId), and an
    /// execution can outlive this process (it is in the Node journal), so a bare counter would
    /// repeat `mcp0` after a restart and get the earlier call's result back for a call that never
    /// ran. With the boot id in it, a CallId is never issued twice.
    boot: String,
}

impl Endpoint {
    pub fn new(current: Arc<dyn Current>) -> Arc<Endpoint> {
        Arc::new(Endpoint {
            hub: Mutex::new(None),
            tokens: Mutex::new(HashMap::new()),
            current,
            calls: AtomicU64::new(0),
            catalog: Mutex::new(HashMap::new()),
            boot: boot_id(),
        })
    }

    /// A CallId unique across restarts of this Node: `mcp-<boot>-<n>`.
    fn next_call(&self) -> Result<CallId, BadId> {
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        CallId::new(format!("mcp-{}-{n}", self.boot))
    }

    pub fn connect(&self, hub: Arc<dyn HubTools>) {
        *self.hub.lock().unwrap() = Some(hub);
    }

    /// A new token for `session` (the previous ones of the session stay valid until revoked).
    pub fn token(&self, session: &SessionId) -> String {
        self.issue(Holder::Session(session.clone()))
    }

    /// A token for one job attempt (`ExecutionRef::Job`), valid until [`Self::revoke_job`].
    pub fn job_token(&self, exec: &ExecutionRef) -> String {
        self.issue(Holder::Job(exec.clone()))
    }

    fn issue(&self, holder: Holder) -> String {
        use sha2::Digest as _;
        let n = self.calls.fetch_add(1, Ordering::SeqCst);
        let who = match &holder {
            Holder::Session(s) => s.to_string(),
            Holder::Job(e) => e.attempt().to_string(),
        };
        let seed = format!(
            "{who}:{n}:{}:{:?}:{:?}",
            self.boot,
            std::time::SystemTime::now(),
            std::thread::current().id()
        );
        let t = hex::encode(sha2::Sha256::digest(seed.as_bytes()));
        self.tokens.lock().unwrap().insert(t.clone(), holder);
        t
    }

    pub fn revoke_session(&self, session: &SessionId) {
        let h = Holder::Session(session.clone());
        self.tokens.lock().unwrap().retain(|_, s| *s != h);
        self.catalog.lock().unwrap().remove(&h);
    }

    pub fn revoke_job(&self, exec: &ExecutionRef) {
        let h = Holder::Job(exec.clone());
        self.tokens.lock().unwrap().retain(|_, s| *s != h);
        self.catalog.lock().unwrap().remove(&h);
    }

    fn holder_of(&self, token: &str) -> Option<Holder> {
        self.tokens.lock().unwrap().get(token).cloned()
    }

    /// Serves on 127.0.0.1 (a free port); returns the URL to give harnesses.
    pub async fn serve(self: &Arc<Self>) -> std::io::Result<String> {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;
        let app = router(self.clone());
        tokio::spawn(async move {
            if let Err(e) = axum::serve(
                listener,
                app.into_make_service_with_connect_info::<SocketAddr>(),
            )
            .await
            {
                tracing::error!("node tool endpoint: {e}");
            }
        });
        Ok(format!("http://{addr}/mcp"))
    }
}

/// 128 random bits from the OS; if that is unavailable, a digest of the clock, pid and an address,
/// which still differs between two processes.
fn boot_id() -> String {
    use sha2::Digest as _;
    use std::io::Read as _;
    let mut b = [0u8; 16];
    if std::fs::File::open("/dev/urandom")
        .and_then(|mut f| f.read_exact(&mut b))
        .is_ok()
    {
        return hex::encode(b);
    }
    let local = 0u8;
    let seed = format!(
        "{:?}:{}:{:p}",
        std::time::SystemTime::now(),
        std::process::id(),
        &local
    );
    hex::encode(&sha2::Sha256::digest(seed.as_bytes())[..16])
}

/// How long a session's tool list waits for the session's first attempt when the Node has no
/// catalog yet: the Hub starts the turn right after the open, so this is a race of a second or so.
const LIST_WAIT: std::time::Duration = std::time::Duration::from_secs(10);

#[derive(Clone)]
struct Authed(Holder);

pub(crate) fn router(ep: Arc<Endpoint>) -> Router {
    let factory = {
        let ep = ep.clone();
        move || Ok(Handler { ep: ep.clone() })
    };
    let config = StreamableHttpServerConfig::default()
        .with_legacy_session_mode(false)
        .with_json_response(true);
    let svc = StreamableHttpService::new(factory, Arc::new(NeverSessionManager::default()), config);
    Router::new()
        .fallback_service(svc)
        .layer(middleware::from_fn_with_state(ep, auth))
}

fn empty(status: StatusCode) -> Response {
    (status, Body::empty()).into_response()
}

fn loopback(addr: &SocketAddr) -> bool {
    match addr {
        SocketAddr::V4(a) => a.ip().is_loopback(),
        SocketAddr::V6(a) => {
            a.ip().is_loopback() || a.ip().to_ipv4_mapped().is_some_and(|v4| v4.is_loopback())
        }
    }
}

async fn auth(State(ep): State<Arc<Endpoint>>, req: Request, next: Next) -> Response {
    let (mut parts, body) = req.into_parts();
    let peer = ConnectInfo::<SocketAddr>::from_request_parts(&mut parts, &())
        .await
        .ok()
        .map(|c| c.0);
    let mut req = Request::from_parts(parts, body);
    if !peer.is_some_and(|p| loopback(&p)) {
        return empty(StatusCode::FORBIDDEN);
    }
    let token = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .unwrap_or("");
    let Some(holder) = (!token.is_empty()).then(|| ep.holder_of(token)).flatten() else {
        return empty(StatusCode::UNAUTHORIZED);
    };
    // rmcp's typed ToolAnnotations drops extension fields. Serve the Hub's list as raw MCP JSON
    // so standard hints and annotations.steve survive this protocol bridge unchanged.
    if req.method() == axum::http::Method::POST {
        let (parts, body) = req.into_parts();
        let bytes = match axum::body::to_bytes(body, 4 * 1024 * 1024).await {
            Ok(bytes) => bytes,
            Err(_) => return empty(StatusCode::PAYLOAD_TOO_LARGE),
        };
        if let Ok(msg) = serde_json::from_slice::<Value>(&bytes)
            && msg["method"] == "tools/list"
        {
            let id = msg.get("id").cloned().unwrap_or(Value::Null);
            let handler = Handler { ep: ep.clone() };
            let result = match handler.listing(&holder).await {
                Ok(catalog) => serde_json::json!({"jsonrpc":"2.0", "id":id, "result":{
                    "tools":catalog.tools.iter().map(descriptor_mcp).collect::<Vec<_>>()
                }}),
                Err(error) => serde_json::json!({"jsonrpc":"2.0", "id":id, "error":error}),
            };
            return axum::Json(result).into_response();
        }
        req = Request::from_parts(parts, Body::from(bytes));
    }
    req.extensions_mut().insert(Authed(holder));
    next.run(req).await
}

struct Handler {
    ep: Arc<Endpoint>,
}

impl Handler {
    fn authed(ctx: &RequestContext<RoleServer>) -> Result<Holder, McpError> {
        ctx.extensions
            .get::<axum::http::request::Parts>()
            .and_then(|p| p.extensions.get::<Authed>())
            .map(|a| a.0.clone())
            .ok_or_else(|| McpError::internal_error("no session for this request", None))
    }

    fn hub(&self) -> Result<Arc<dyn HubTools>, McpError> {
        self.ep
            .hub
            .lock()
            .unwrap()
            .clone()
            .ok_or_else(|| McpError::internal_error("the hub is not connected", None))
    }

    /// The Hub's catalog for `exec`, kept for listing at idle.
    async fn catalog(
        &self,
        hub: &dyn HubTools,
        exec: ExecutionRef,
        holder: &Holder,
    ) -> Result<Catalog, McpError> {
        let c = hub
            .catalog(exec)
            .await
            .map_err(|e| McpError::internal_error(e.safe_detail, None))?;
        self.ep
            .catalog
            .lock()
            .unwrap()
            .insert(holder.clone(), c.clone());
        Ok(c)
    }

    /// What a holder lists: the catalog for its running attempt; for a session running none, the
    /// last catalog this session had, else (none yet) the catalog once its attempt starts, waiting
    /// up to [`LIST_WAIT`]. A job's token lists only while the job lives.
    async fn listing(&self, holder: &Holder) -> Result<Catalog, McpError> {
        let hub = self.hub()?;
        let session = match (self.execution(holder), holder) {
            (Ok(exec), _) => return self.catalog(&*hub, exec, holder).await,
            (Err(e), Holder::Job(_)) => return Err(e),
            (Err(e), Holder::Session(s)) => {
                if let Some(c) = self.ep.catalog.lock().unwrap().get(holder).cloned() {
                    return Ok(c);
                }
                (s.clone(), e)
            }
        };
        let end = tokio::time::Instant::now() + LIST_WAIT;
        while tokio::time::Instant::now() < end {
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
            if let Some(exec) = self.ep.current.execution(&session.0) {
                return self.catalog(&*hub, exec, holder).await;
            }
        }
        Err(session.1)
    }

    /// The attempt a call counts for: the session's running one, or the job's own (checked again
    /// against the token, which goes when the job ends).
    fn execution(&self, holder: &Holder) -> Result<ExecutionRef, McpError> {
        match holder {
            Holder::Session(session) => self.ep.current.execution(session).ok_or_else(|| {
                McpError::invalid_request("no attempt of this session is running", None)
            }),
            Holder::Job(exec) => {
                let live = self.ep.tokens.lock().unwrap().values().any(|h| h == holder);
                live.then(|| exec.clone())
                    .ok_or_else(|| McpError::invalid_request("this job has ended", None))
            }
        }
    }
}

fn descriptor_mcp(t: &ToolDescriptor) -> Value {
    let mut annotations = t.annotations.as_object().cloned().unwrap_or_default();
    annotations.insert("readOnlyHint".into(), serde_json::json!(t.read_only));
    serde_json::json!({"name":t.name, "description":t.description,
        "inputSchema":t.input_schema, "annotations":annotations})
}

fn text(t: impl Into<String>) -> Vec<ContentBlock> {
    vec![ContentBlock::text(t)]
}

/// Steve's result in MCP's words (what the agent reads).
fn to_mcp(r: ToolResult) -> CallToolResult {
    match r {
        ToolResult::Completed { parts, .. } => {
            let t: Vec<String> = parts
                .into_iter()
                .filter_map(|p| match p {
                    Part::Text { text } => Some(text),
                    _ => None,
                })
                .collect();
            CallToolResult::success(text(t.join("\n")))
        }
        ToolResult::PendingApproval { decision, .. } => CallToolResult::success(text(decision)),
        ToolResult::Running { operation } => {
            CallToolResult::success(text(format!("Started ({operation}).")))
        }
        ToolResult::Failed { error } => CallToolResult::error(text(error.safe_detail)),
        ToolResult::Unknown { operation } => CallToolResult::error(text(format!(
            "The outcome is unknown ({operation}); check before trying again."
        ))),
    }
}

impl ServerHandler for Handler {
    fn get_info(&self) -> ServerConfig {
        // tools, and no `listChanged`: every request is its own exchange, so no notification can
        // follow one, and a client told otherwise asks to listen for them and is refused
        let mut caps = ServerCapabilities::default();
        caps.tools = Some(ToolsCapability::default());
        ServerConfig::new(caps).with_server_info(Implementation::new("steve", "1.0.0"))
    }

    async fn list_tools(
        &self,
        _request: Option<PaginatedRequestParams>,
        ctx: RequestContext<RoleServer>,
    ) -> Result<ListToolsResult, McpError> {
        let holder = Self::authed(&ctx)?;
        let catalog = self.listing(&holder).await?;
        let tools = catalog
            .tools
            .into_iter()
            .map(|t| {
                let schema = match t.input_schema {
                    serde_json::Value::Object(m) => m,
                    _ => serde_json::Map::new(),
                };
                // Preserve the Hub's description for clients; calls still require the live
                // execution and current catalog authorization in the Hub.
                Tool::new(t.name, t.description, Arc::new(schema))
                    .with_annotations(ToolAnnotations::new().read_only(t.read_only))
            })
            .collect();
        Ok(ListToolsResult::with_all_items(tools))
    }

    async fn call_tool(
        &self,
        request: CallToolRequestParams,
        ctx: RequestContext<RoleServer>,
    ) -> Result<CallToolResponse, McpError> {
        let holder = Self::authed(&ctx)?;
        let exec = self.execution(&holder)?;
        let hub = self.hub()?;
        let id = self
            .ep
            .next_call()
            .map_err(|e| McpError::internal_error(e.to_string(), None))?;
        let catalog = self.catalog(&*hub, exec.clone(), &holder).await?;
        let call = ToolCall {
            id,
            execution: exec,
            catalog: catalog.revision,
            name: request.name.to_string(),
            args: serde_json::Value::Object(request.arguments.unwrap_or_default()),
            deadline: Timestamp(
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_millis() as i64)
                    .unwrap_or(0)
                    + 120_000,
            ),
        };
        let result = match hub.call(call).await {
            Ok(r) => to_mcp(r),
            Err(e) => CallToolResult::error(text(e.safe_detail)),
        };
        Ok(CallToolResponse::Complete(result))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Nobody;
    impl Current for Nobody {
        fn execution(&self, _: &SessionId) -> Option<ExecutionRef> {
            None
        }
    }

    #[test]
    fn descriptors_preserve_standard_and_steve_annotations() {
        let annotations = serde_json::json!({"title":"Read", "idempotentHint":true,
            "steve":{"source":"mcp:files", "resources":["document"], "asYou":false}});
        let descriptor = ToolDescriptor {
            name: "files__read".into(),
            description: "Read.".into(),
            input_schema: serde_json::json!({"type":"object"}),
            read_only: true,
            annotations: annotations.clone(),
        };
        let value = descriptor_mcp(&descriptor);
        assert_eq!(value["annotations"]["steve"], annotations["steve"]);
        assert_eq!(value["annotations"]["idempotentHint"], true);
        assert_eq!(value["annotations"]["readOnlyHint"], true);
    }

    #[tokio::test]
    async fn idle_catalogs_never_cross_session_or_job_holders() {
        let ep = Endpoint::new(Arc::new(Nobody));
        let one = Holder::Session(SessionId::new("one").unwrap());
        let two = Holder::Session(SessionId::new("two").unwrap());
        let catalog = Catalog {
            revision: Revision::new("one").unwrap(),
            tools: vec![],
        };
        ep.catalog.lock().unwrap().insert(one.clone(), catalog);
        assert!(ep.catalog.lock().unwrap().get(&two).is_none());
        ep.revoke_session(&SessionId::new("one").unwrap());
        assert!(ep.catalog.lock().unwrap().is_empty());
    }

    struct CatalogHub(Catalog);
    #[async_trait::async_trait]
    impl HubTools for CatalogHub {
        async fn catalog(&self, _: ExecutionRef) -> ApiResult<Catalog> {
            // The actual link serializes this same contract.
            Ok(serde_json::from_value(serde_json::to_value(&self.0).unwrap()).unwrap())
        }
        async fn call(&self, _: ToolCall) -> ApiResult<ToolResult> {
            unreachable!()
        }
        async fn call_status(&self, _: ToolKey) -> ApiResult<ToolResult> {
            unreachable!()
        }
        async fn cancel_call(&self, _: ToolKey) -> ApiResult<ToolResult> {
            unreachable!()
        }
    }

    #[tokio::test]
    async fn annotations_survive_catalog_wire_and_node_mcp_http() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        struct Active;
        impl Current for Active {
            fn execution(&self, s: &SessionId) -> Option<ExecutionRef> {
                Some(ExecutionRef::Turn {
                    session: s.clone(),
                    turn: TurnId::new("turn").unwrap(),
                    attempt: AttemptId::new("attempt").unwrap(),
                })
            }
        }
        let ep = Endpoint::new(Arc::new(Active));
        let annotations = serde_json::json!({"readOnlyHint":true,"idempotentHint":true,
            "steve":{"source":"mcp:files","kind":"read","resources":["document"],"asYou":false}});
        ep.connect(Arc::new(CatalogHub(Catalog {
            revision: Revision::new("catalog").unwrap(),
            tools: vec![ToolDescriptor {
                name: "files__read".into(),
                description: "Read.".into(),
                input_schema: serde_json::json!({"type":"object"}),
                read_only: true,
                annotations: annotations.clone(),
            }],
        })));
        let url = ep.serve().await.unwrap();
        let token = ep.token(&SessionId::new("one").unwrap());
        let payload = r#"{"jsonrpc":"2.0","id":1,"method":"tools/list"}"#;
        let mut stream = tokio::net::TcpStream::connect(
            url.strip_prefix("http://")
                .unwrap()
                .trim_end_matches("/mcp"),
        )
        .await
        .unwrap();
        stream.write_all(format!("POST /mcp HTTP/1.1\r\nHost: localhost\r\nAuthorization: Bearer {token}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{payload}",payload.len()).as_bytes()).await.unwrap();
        let mut response = String::new();
        stream.read_to_string(&mut response).await.unwrap();
        let (_, body) = response.split_once("\r\n\r\n").unwrap();
        let result: Value = serde_json::from_str(body).unwrap();
        assert_eq!(
            result["result"]["tools"][0]["annotations"], annotations,
            "{response}"
        );

        // Idle after a running turn: only that session can reuse its own pinned catalog.
        let idle = Endpoint::new(Arc::new(Nobody));
        idle.connect(Arc::new(CatalogHub(Catalog {
            revision: Revision::new("unused").unwrap(),
            tools: vec![],
        })));
        let one = Holder::Session(SessionId::new("one").unwrap());
        let two = Holder::Session(SessionId::new("two").unwrap());
        let own = ep.catalog.lock().unwrap().get(&one).unwrap().clone();
        idle.catalog.lock().unwrap().insert(one.clone(), own);
        let handler = Handler { ep: idle };
        assert_eq!(
            handler.listing(&one).await.unwrap().tools[0].name,
            "files__read"
        );
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(20), handler.listing(&two))
                .await
                .is_err(),
            "another holder must wait for its own execution, never receive the first one's catalog"
        );
    }

    /// A restarted Node (a new Endpoint) must not hand out a CallId the previous process already
    /// used: the Hub would return that call's saved result for a call that never ran.
    #[test]
    fn call_ids_do_not_repeat_across_restarts() {
        let a = Endpoint::new(Arc::new(Nobody));
        let first: Vec<CallId> = (0..3).map(|_| a.next_call().unwrap()).collect();
        drop(a);
        let b = Endpoint::new(Arc::new(Nobody));
        let again: Vec<CallId> = (0..3).map(|_| b.next_call().unwrap()).collect();
        for id in &again {
            assert!(!first.contains(id), "{id} reused after restart");
        }
        assert_ne!(first[0], first[1]);
    }
}
