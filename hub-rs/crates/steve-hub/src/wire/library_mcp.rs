//! The Hub as the MCP client of the library's servers (`steve_modules::caps::mcp`): a Bot's sessions
//! reach a server's tools through Steve's one tool endpoint ([`super::tool_registry`]), never through a
//! server of their own. The Hub lists each server's tools (kept until the server changes, refreshed
//! every [`FRESH`]) and runs a call on the session's behalf.
//!
//! - `stdio`: delegated to the colocated Node via [`steve_node_api::WorkspaceExecutor`], where the
//!   Node starts and isolates the server inside `steve_confine::Sandbox::spawn_capability`, one
//!   process per server shared by every session that uses it, stopped after [`IDLE`] without a call
//!   (and when the server is changed or deleted). Secret env vars are delivered via one-shot
//!   [`super::node_secrets::NodeSecrets`] grants only when the Node spawns a new process.
//! - `http`: MCP streamable HTTP (JSON or an event stream per answer, `Mcp-Session-Id` kept).
//! - `sse`: the older HTTP+SSE transport (an event stream names the endpoint messages are posted to).
//!
//! Secret values (env, headers) are read from the vault only when a connection opens.

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::StreamExt;
use serde_json::{Value, json};
use steve_modules::caps::Capabilities;
use steve_store::caps::mcp::Server;
use tokio::sync::oneshot;
use zeroize::Zeroizing;

use super::node_secrets::NodeSecrets;
use super::workspaces::Workspaces;

/// A server's list of tools is asked again after this.
pub const FRESH: Duration = Duration::from_secs(10 * 60);
/// After a failed listing, the next try waits this long.
const RETRY: Duration = Duration::from_secs(60);
/// A connection without a call for this long is closed.
pub const IDLE: Duration = Duration::from_secs(10 * 60);
/// Opening a connection and listing: a stdio server may first be fetched (`npx -y …`).
const OPEN_WAIT: Duration = Duration::from_secs(60);
/// One call; a Node gives the whole tool call 120 s.
const CALL_WAIT: Duration = Duration::from_secs(110);
const PROTOCOL: &str = "2025-06-18";

/// One tool of a library server, as the server describes it.
#[derive(Debug, Clone, PartialEq)]
pub struct RemoteTool {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
    pub annotations: Value,
}

#[derive(Clone)]
struct ToolList {
    tools: Vec<RemoteTool>,
}
#[derive(Clone, Hash, PartialEq, Eq)]
struct ListingKey {
    id: String,
    updated: i64,
}
impl ListingKey {
    fn of(server: &Server) -> Self {
        Self {
            id: server.id.clone(),
            updated: server.updated,
        }
    }
}
static LIST_SPACE: steve_runtime::cache::CacheSpace<ListingKey, ToolList> =
    steve_runtime::cache::CacheSpace {
        name: "library-mcp",
        ttl: FRESH,
        // A temporary listing failure must not erase the last tool directory.
        stale_for: Duration::from_secs(600),
        share_pct: 9,
        tags: |key| vec![steve_runtime::cache::Tag::of("mcp-server", &key.id)],
        weigh: |list| {
            list.tools
                .iter()
                .map(|t| {
                    t.name.len() + t.description.len() + t.input_schema.to_string().len() + 128
                })
                .sum::<usize>()
                .min(u32::MAX as usize) as u32
        },
    };
static FAILURE_SPACE: steve_runtime::cache::CacheSpace<ListingKey, String> =
    steve_runtime::cache::CacheSpace {
        name: "library-mcp-failures",
        ttl: RETRY,
        stale_for: Duration::ZERO,
        share_pct: 1,
        tags: |key| vec![steve_runtime::cache::Tag::of("mcp-server", &key.id)],
        weigh: |error| error.len().min(u32::MAX as usize) as u32,
    };

/// Lazy vault secret reader for one Node stdio MCP spawn operation.
/// Secrets are decrypted and logged at most once, only if the Node actually spawns a new process
/// and requests them over `HubSecrets::take`.
struct McpServerSecrets {
    caps: Capabilities,
    server_id: String,
    who: String,
    loaded: Mutex<Option<Vec<(String, Zeroizing<String>)>>>,
}

impl McpServerSecrets {
    fn new(caps: Capabilities, server_id: String, who: String) -> Self {
        Self {
            caps,
            server_id,
            who,
            loaded: Mutex::new(None),
        }
    }
}

impl steve_modules::vault::UseValues for McpServerSecrets {
    fn value(
        &self,
        label: &str,
    ) -> steve_modules::vault::VaultResult<steve_modules::vault::secret::SecretBytes> {
        let mut guard = self.loaded.lock().unwrap_or_else(|e| e.into_inner());
        if guard.is_none() {
            let loaded = self.caps.mcp_server_secrets(&self.server_id, &self.who)?;
            *guard = Some(loaded);
        }
        let list = guard.as_ref().unwrap();
        let (_, val) = list.iter().find(|(l, _)| l == label).ok_or_else(|| {
            steve_modules::vault::VaultError::from(steve_modules::ApiError::not_found(format!(
                "MCP secret {label}"
            )))
        })?;
        Ok(steve_modules::vault::secret::SecretBytes::new(
            val.as_bytes().to_vec(),
        ))
    }
}

/// Revokes a one-shot `NodeSecrets` grant as soon as the stdio MCP RPC returns.
struct SecretGrantGuard {
    secrets: Arc<NodeSecrets>,
    op: String,
}

impl Drop for SecretGrantGuard {
    fn drop(&mut self) {
        self.secrets.end(&self.op);
    }
}

/// The Hub's connections to the library's servers. One per Hub.
pub struct LibraryMcp {
    caps: Capabilities,
    workspaces: Option<Arc<Workspaces>>,
    secrets: Option<Arc<NodeSecrets>>,
    http: reqwest::Client,
    conns: Mutex<HashMap<String, Arc<tokio::sync::Mutex<Option<Arc<Conn>>>>>>,
    stdio_ids: Mutex<HashSet<String>>,
    lists: steve_runtime::cache::Cached<ListingKey, ToolList>,
    failures: steve_runtime::cache::Cached<ListingKey, String>,
    caches: Arc<steve_runtime::cache::Caches>,
}

impl LibraryMcp {
    pub fn new(
        caps: Capabilities,
        workspaces: Option<Arc<Workspaces>>,
        secrets: Option<Arc<NodeSecrets>>,
        caches: Arc<steve_runtime::cache::Caches>,
    ) -> Arc<LibraryMcp> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            // Custom credential headers must not follow a redirect to a different origin.
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("steve-hub/", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_default();
        Arc::new(LibraryMcp {
            caps,
            workspaces,
            secrets,
            http,
            conns: Mutex::default(),
            stdio_ids: Mutex::default(),
            lists: caches.space(&LIST_SPACE),
            failures: caches.space(&FAILURE_SPACE),
            caches,
        })
    }

    fn stdio_target(
        &self,
        server: &Server,
        who: &str,
    ) -> Result<
        (
            Arc<dyn steve_node_api::Node>,
            steve_node_api::McpStdioServer,
            Option<SecretGrantGuard>,
        ),
        String,
    > {
        let node = self
            .workspaces
            .as_ref()
            .and_then(|w| w.local_node())
            .ok_or_else(|| {
                format!(
                    "MCP server {} requires the local Node (offline)",
                    server.name
                )
            })?;
        self.stdio_ids.lock().unwrap().insert(server.id.clone());

        let mut env = Vec::new();
        let mut secret_env = Vec::new();
        for v in &server.env {
            if v.secret {
                secret_env.push((
                    v.name.clone(),
                    steve_store::caps::mcp::secret_label("stdio", &v.name),
                ));
            } else {
                env.push((v.name.clone(), v.value.clone().unwrap_or_default()));
            }
        }
        let op = format!("op_mcp_{}", uuid::Uuid::new_v4().simple());
        let operation = steve_node_api::OperationId::new(op.clone()).map_err(|e| e.to_string())?;
        let grant = if !secret_env.is_empty() {
            let secrets = self
                .secrets
                .as_ref()
                .ok_or_else(|| format!("MCP server {} secrets channel unavailable", server.name))?;
            let ids: Vec<String> = secret_env.iter().map(|(_, id)| id.clone()).collect();
            secrets.give(
                &op,
                node.id().clone(),
                ids,
                Arc::new(McpServerSecrets::new(
                    self.caps.clone(),
                    server.id.clone(),
                    who.to_string(),
                )),
            );
            Some(SecretGrantGuard {
                secrets: secrets.clone(),
                op,
            })
        } else {
            None
        };

        let spec = steve_node_api::McpStdioServer {
            id: server.id.clone(),
            updated: server.updated,
            name: server.name.clone(),
            command: server.command.clone().unwrap_or_default(),
            args: server.args.clone(),
            env,
            secret_env,
            operation,
        };
        Ok((node, spec, grant))
    }

    /// The tools `server` has, as last listed; `None`: not listed yet (or the server changed since).
    pub async fn cached(&self, server: &Server) -> Option<Result<Vec<RemoteTool>, String>> {
        let key = ListingKey::of(server);
        if let Some((list, _)) = self.lists.peek(&key).await {
            return Some(Ok(list.tools.clone()));
        }
        self.retry(server).await.map(Err)
    }
    async fn retry(&self, server: &Server) -> Option<String> {
        self.failures
            .peek(&ListingKey::of(server))
            .await
            .filter(|(_, fresh)| *fresh)
            .map(|(error, _)| error.as_ref().clone())
    }
    async fn lookup(&self, server: &Server) -> Option<(Result<Vec<RemoteTool>, String>, bool)> {
        let key = ListingKey::of(server);
        if let Some((list, fresh)) = self.lists.peek(&key).await {
            return Some((
                Ok(list.tools.clone()),
                !fresh && self.retry(server).await.is_none(),
            ));
        }
        self.retry(server).await.map(|error| (Err(error), false))
    }

    /// The tools `server` has: as listed before (listed again in the background once that is old),
    /// else listed now, waiting at most `wait` (the listing goes on after that and is kept). `who`:
    /// the Bot the connection is opened for, in the vault's use log.
    pub async fn tools_within(
        self: &Arc<Self>,
        server: &Server,
        who: &str,
        wait: Duration,
    ) -> Result<Vec<RemoteTool>, String> {
        if let Some((tools, stale)) = self.lookup(server).await {
            if stale {
                self.warm(server, who);
            }
            return tools;
        }
        let name = server.name.clone();
        let (me, server, who) = (self.clone(), server.clone(), who.to_string());
        // The initializer continues opening/listing after this caller's shorter wait.
        // Off mode uses the same lifecycle but never retains the successful list.
        let mut task = tokio::spawn(async move { me.cached_list(&server, &who).await });
        match tokio::time::timeout(wait, &mut task).await {
            Ok(Ok(result)) => result,
            Ok(Err(error)) => Err(format!("MCP listing task failed: {error}")),
            Err(_) => Err(format!(
                "MCP server {name} has not listed its tools yet (still trying)"
            )),
        }
    }

    /// Lists `server`'s tools in the background unless that is under way; a failed listing keeps the
    /// list an earlier one got for the same version of the server.
    pub fn warm(self: &Arc<Self>, server: &Server, who: &str) {
        let Ok(rt) = tokio::runtime::Handle::try_current() else {
            return;
        };
        let (me, server, who) = (self.clone(), server.clone(), who.to_string());
        rt.spawn(async move {
            let _ = me.cached_list(&server, &who).await;
        });
    }
    async fn cached_list(&self, server: &Server, who: &str) -> Result<Vec<RemoteTool>, String> {
        let key = ListingKey::of(server);
        let old = self.lists.peek(&key).await.map(|(list, _)| list);
        let result = self
            .lists
            .get_or_load(key.clone(), || async {
                if let Some(error) = self.retry(server).await {
                    return Err(error);
                }
                match tokio::time::timeout(OPEN_WAIT, self.list_now(server, who)).await {
                    Ok(Ok(tools)) => Ok(ToolList { tools }),
                    result => {
                        let error = match result {
                            Ok(Err(error)) => error,
                            _ => format!(
                                "MCP server {} did not list its tools within {} s",
                                server.name,
                                OPEN_WAIT.as_secs()
                            ),
                        };
                        let _ = self
                            .failures
                            .get_or_load(key, || async { Ok::<_, ()>(error.clone()) })
                            .await;
                        Err(error)
                    }
                }
            })
            .await;
        match result {
            Ok(list) => Ok(list.tools.clone()),
            Err(_) if old.is_some() => Ok(old.unwrap().tools.clone()),
            Err(error) => Err(error),
        }
    }

    async fn list_now(&self, server: &Server, who: &str) -> Result<Vec<RemoteTool>, String> {
        if server.transport == "stdio" {
            let (node, spec, _grant) = self.stdio_target(server, who)?;
            let tools = node.mcp_stdio_list(spec).await.map_err(|e| e.safe_detail)?;
            return Ok(tools
                .into_iter()
                .map(|t| RemoteTool {
                    name: t.name,
                    description: t.description,
                    input_schema: t.input_schema,
                    annotations: t.annotations,
                })
                .collect());
        }
        let conn = self.conn(server, who).await?;
        let mut out = vec![];
        let mut cursor: Option<String> = None;
        for _ in 0..20 {
            let params = match &cursor {
                Some(c) => json!({ "cursor": c }),
                None => json!({}),
            };
            let page = conn.request("tools/list", params).await?;
            let tools = page["tools"]
                .as_array()
                .ok_or_else(|| format!("MCP server {} returned no tools array", server.name))?;
            for t in tools {
                let name = t["name"]
                    .as_str()
                    .filter(|n| !n.is_empty())
                    .ok_or_else(|| {
                        format!("MCP server {} returned a tool without a name", server.name)
                    })?;
                if !t["inputSchema"].is_object() {
                    return Err(format!(
                        "MCP server {} tool {name} has no object inputSchema",
                        server.name
                    ));
                }
                if out.iter().any(|old: &RemoteTool| old.name == name) {
                    return Err(format!("MCP server {} repeated tool {name}", server.name));
                }
                out.push(RemoteTool {
                    name: name.to_string(),
                    description: t["description"].as_str().unwrap_or_default().to_string(),
                    input_schema: t["inputSchema"].clone(),
                    annotations: t["annotations"].clone(),
                });
            }
            cursor = page["nextCursor"]
                .as_str()
                .filter(|c| !c.is_empty())
                .map(str::to_string);
            if cursor.is_none() {
                break;
            }
        }
        if cursor.is_some() {
            return Err(format!(
                "MCP server {} exceeded the 20-page tool catalog limit",
                server.name
            ));
        }
        Ok(out)
    }

    /// Runs `tool` (the server's own name for it) with `args`: the text the agent reads, `Err` when the
    /// server answered with an error or could not be reached.
    pub async fn call(
        &self,
        server: &Server,
        who: &str,
        tool: &str,
        args: Value,
    ) -> Result<String, String> {
        if server.transport == "stdio" {
            let (node, spec, _grant) = self.stdio_target(server, who)?;
            let resp = tokio::time::timeout(
                CALL_WAIT,
                node.mcp_stdio_call(steve_node_api::McpStdioCallRequest {
                    server: spec,
                    tool: tool.to_string(),
                    arguments: args,
                }),
            )
            .await
            .map_err(|_| {
                format!(
                    "MCP server {} did not answer {tool} within {} s",
                    server.name,
                    CALL_WAIT.as_secs()
                )
            })?
            .map_err(|e| e.safe_detail)?;
            return result_text(&resp.result);
        }
        let run = async {
            let conn = self.conn(server, who).await?;
            conn.request("tools/call", json!({ "name": tool, "arguments": args }))
                .await
        };
        let out = tokio::time::timeout(CALL_WAIT, run).await.map_err(|_| {
            format!(
                "MCP server {} did not answer {tool} within {} s",
                server.name,
                CALL_WAIT.as_secs()
            )
        })??;
        result_text(&out)
    }

    /// Closes the connections no call used for [`IDLE`], and those of servers changed since.
    pub async fn sweep(&self) {
        let http_ids: Vec<String> = self.conns.lock().unwrap().keys().cloned().collect();
        let stdio_ids: Vec<String> = self.stdio_ids.lock().unwrap().iter().cloned().collect();
        let all_ids: Vec<String> = http_ids
            .iter()
            .chain(stdio_ids.iter())
            .cloned()
            .collect::<HashSet<_>>()
            .into_iter()
            .collect();
        let current = self.caps.vault().store().read(|c| {
            all_ids
                .iter()
                .map(|id| {
                    Ok((
                        id.clone(),
                        steve_store::caps::mcp::Servers(c)
                            .get(id)?
                            .map(|s| s.updated),
                    ))
                })
                .collect::<Result<HashMap<_, _>, steve_store::StoreError>>()
        });
        let Ok(current) = current else {
            return;
        };
        {
            let mut conns = self.conns.lock().unwrap();
            conns.retain(|id, slot| {
                if let Ok(mut g) = slot.try_lock() {
                    if g.as_ref().is_some_and(|c| {
                        current.get(id).copied().flatten() != Some(c.updated)
                            || (c.active.load(Ordering::Relaxed) == 0 && c.idle() > IDLE)
                            || !c.alive()
                    }) {
                        *g = None;
                    }
                    return current.get(id).is_some_and(Option::is_some);
                }
                true
            });
        }
        let live_stdio: Vec<(String, i64)> = {
            let mut tracked = self.stdio_ids.lock().unwrap();
            tracked.retain(|id| current.get(id).is_some_and(Option::is_some));
            tracked
                .iter()
                .filter_map(|id| current.get(id).copied().flatten().map(|u| (id.clone(), u)))
                .collect()
        };
        for (id, updated) in &current {
            if updated.is_none() {
                self.caches
                    .invalidate(&[steve_runtime::cache::Tag::of("mcp-server", id)]);
            }
        }
        if let Some(node) = self.workspaces.as_ref().and_then(|w| w.local_node()) {
            let _ = node
                .mcp_stdio_sweep(steve_node_api::McpStdioSweepRequest { live: live_stdio })
                .await;
        }
    }

    /// The open connection to `server`, opened (and initialized) when there is none or it is stale.
    async fn conn(&self, server: &Server, who: &str) -> Result<Arc<Conn>, String> {
        let slot = self
            .conns
            .lock()
            .unwrap()
            .entry(server.id.clone())
            .or_default()
            .clone();
        let mut g = slot.lock().await;
        if let Some(c) = g.as_ref()
            && c.updated == server.updated
            && c.alive()
        {
            c.touch();
            return Ok(c.clone());
        }
        *g = None;
        let c = Arc::new(self.open(server, who).await?);
        c.initialize().await?;
        *g = Some(c.clone());
        Ok(c)
    }

    async fn open(&self, server: &Server, who: &str) -> Result<Conn, String> {
        let (caps, sid, who_s) = (self.caps.clone(), server.id.clone(), who.to_string());
        let secrets = tokio::task::spawn_blocking(move || caps.mcp_server_secrets(&sid, &who_s))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| format!("the secrets of MCP server {}: {e}", server.name))?;
        let secret = |label: String| {
            secrets
                .iter()
                .find(|(l, _)| *l == label)
                .map(|(_, v)| v.to_string())
                .ok_or_else(|| format!("MCP server {} is missing secret {label}", server.name))
        };
        let t = server.transport.as_str();
        if t == "stdio" {
            return Err(format!(
                "MCP server {} uses stdio transport and runs on the local Node",
                server.name
            ));
        }
        let mut headers = reqwest::header::HeaderMap::new();
        for v in &server.headers {
            let value = if v.secret {
                secret(steve_store::caps::mcp::secret_label(t, &v.name))?
            } else {
                v.value.clone().unwrap_or_default()
            };
            let (Ok(k), Ok(val)) = (
                reqwest::header::HeaderName::from_bytes(v.name.as_bytes()),
                reqwest::header::HeaderValue::from_str(&value),
            ) else {
                return Err(format!("header {} of MCP server {}", v.name, server.name));
            };
            headers.insert(k, val);
        }
        let url = server.url.clone().unwrap_or_default();
        let link = if t == "sse" {
            Link::Sse(Sse::open(self.http.clone(), &url, headers).await?)
        } else {
            Link::Http(Http {
                client: self.http.clone(),
                url,
                headers,
                session: Mutex::new(None),
                protocol: Mutex::new(None),
                alive: AtomicBool::new(true),
            })
        };
        Ok(Conn {
            name: server.name.clone(),
            updated: server.updated,
            last: Mutex::new(Instant::now()),
            next: AtomicU64::new(1),
            active: AtomicUsize::new(0),
            link,
        })
    }
}

/// A tool result as the agent reads it: its text, other content described, the structured content
/// when there is no text. `Err` for an `isError` result.
pub fn result_text(r: &Value) -> Result<String, String> {
    let mut parts: Vec<String> = vec![];
    for c in r["content"].as_array().into_iter().flatten() {
        match c["type"].as_str() {
            Some("text") => parts.push(c["text"].as_str().unwrap_or_default().to_string()),
            Some("resource") => {
                let res = &c["resource"];
                match res["text"].as_str() {
                    Some(t) => parts.push(t.to_string()),
                    None => parts.push(format!(
                        "[resource {} ({})]",
                        res["uri"].as_str().unwrap_or("?"),
                        res["mimeType"].as_str().unwrap_or("binary")
                    )),
                }
            }
            Some("resource_link") => parts.push(format!(
                "[link {} {}]",
                c["name"].as_str().unwrap_or_default(),
                c["uri"].as_str().unwrap_or_default()
            )),
            Some(kind) => parts.push(format!(
                "[{kind} ({}, {} bytes base64)]",
                c["mimeType"].as_str().unwrap_or("?"),
                c["data"].as_str().map_or(0, str::len)
            )),
            None => {}
        }
    }
    if parts.is_empty() && !r["structuredContent"].is_null() {
        parts.push(r["structuredContent"].to_string());
    }
    let text = parts.join("\n");
    if r["isError"].as_bool() == Some(true) {
        Err(if text.is_empty() {
            "the tool failed".into()
        } else {
            text
        })
    } else {
        Ok(text)
    }
}

struct Conn {
    name: String,
    updated: i64,
    last: Mutex<Instant>,
    next: AtomicU64,
    active: AtomicUsize,
    link: Link,
}

enum Link {
    Http(Http),
    Sse(Sse),
}

impl Conn {
    fn touch(&self) {
        *self.last.lock().unwrap() = Instant::now();
    }

    fn idle(&self) -> Duration {
        self.last.lock().unwrap().elapsed()
    }

    fn alive(&self) -> bool {
        match &self.link {
            Link::Sse(s) => s.alive.load(Ordering::Relaxed),
            Link::Http(h) => h.alive.load(Ordering::Relaxed),
        }
    }

    async fn initialize(&self) -> Result<(), String> {
        let r = self
            .request(
                "initialize",
                json!({
                    "protocolVersion": PROTOCOL,
                    "capabilities": {},
                    "clientInfo": {"name": "steve", "version": env!("CARGO_PKG_VERSION")},
                }),
            )
            .await?;
        if let Link::Http(h) = &self.link {
            *h.protocol.lock().unwrap() = r["protocolVersion"].as_str().map(str::to_string);
        }
        self.notify("notifications/initialized").await
    }

    /// One request; the `result`, or the error the server or transport gave.
    async fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        self.active.fetch_add(1, Ordering::Relaxed);
        struct Activity<'a>(&'a Conn);
        impl Drop for Activity<'_> {
            fn drop(&mut self) {
                self.0.touch();
                self.0.active.fetch_sub(1, Ordering::Relaxed);
            }
        }
        let _activity = Activity(self);
        self.touch();
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let msg = json!({"jsonrpc": "2.0", "id": id, "method": method, "params": params});
        let answer = match &self.link {
            Link::Http(h) => h.request(id, &msg).await,
            Link::Sse(s) => s.request(id, &msg).await,
        }
        .map_err(|e| format!("MCP server {}: {e}", self.name))?;
        if let Some(e) = answer.get("error") {
            return Err(format!(
                "MCP server {}: {} ({})",
                self.name,
                e["message"].as_str().unwrap_or("error"),
                e["code"]
            ));
        }
        Ok(answer["result"].clone())
    }

    async fn notify(&self, method: &str) -> Result<(), String> {
        let msg = json!({"jsonrpc": "2.0", "method": method});
        match &self.link {
            Link::Http(h) => h.post(&msg).await.map(|_| ()),
            Link::Sse(s) => s.post(&msg).await,
        }
        .map_err(|e| format!("MCP server {}: {e}", self.name))
    }
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>;

/// Hands a message from the server to the request waiting for it (by id).
fn deliver(pending: &Pending, msg: Value) {
    let id = msg.get("id").and_then(Value::as_u64);
    if msg.get("method").is_none()
        && let Some(id) = id
        && let Some(tx) = pending.lock().unwrap().remove(&id)
    {
        let _ = tx.send(msg);
    }
}

/// What a server's own request gets: `ping` an empty result, anything else "not supported".
fn answer_to(msg: &Value) -> Option<Value> {
    let id = msg.get("id")?;
    let method = msg.get("method")?.as_str()?;
    Some(if method == "ping" {
        json!({"jsonrpc": "2.0", "id": id, "result": {}})
    } else {
        json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "not supported by Steve"}})
    })
}

struct Http {
    client: reqwest::Client,
    url: String,
    headers: reqwest::header::HeaderMap,
    session: Mutex<Option<String>>,
    protocol: Mutex<Option<String>>,
    alive: AtomicBool,
}

impl Http {
    async fn post(&self, msg: &Value) -> Result<reqwest::Response, String> {
        let mut req = self
            .client
            .post(&self.url)
            .headers(self.headers.clone())
            .header("Accept", "application/json, text/event-stream")
            .json(msg);
        if let Some(s) = self.session.lock().unwrap().clone() {
            req = req.header("Mcp-Session-Id", s);
        }
        if let Some(p) = self.protocol.lock().unwrap().clone() {
            req = req.header("MCP-Protocol-Version", p);
        }
        let res = req.send().await.map_err(|e| e.to_string())?;
        if let Some(s) = res
            .headers()
            .get("mcp-session-id")
            .and_then(|v| v.to_str().ok())
        {
            *self.session.lock().unwrap() = Some(s.to_string());
        }
        if !res.status().is_success() {
            if res.status() == reqwest::StatusCode::NOT_FOUND {
                self.alive.store(false, Ordering::Relaxed);
            }
            let status = res.status();
            let body = res.text().await.unwrap_or_default();
            return Err(format!(
                "HTTP {status}: {}",
                body.chars().take(500).collect::<String>()
            ));
        }
        Ok(res)
    }

    async fn request(&self, id: u64, msg: &Value) -> Result<Value, String> {
        let res = self.post(msg).await?;
        let sse = res
            .headers()
            .get(reqwest::header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .is_some_and(|t| t.starts_with("text/event-stream"));
        if !sse {
            let v: Value = res.json().await.map_err(|e| e.to_string())?;
            let found = match v {
                Value::Array(xs) => xs.into_iter().find(|x| x["id"].as_u64() == Some(id)),
                x if x["id"].as_u64() == Some(id) && x.get("method").is_none() => Some(x),
                _ => None,
            };
            return found.ok_or_else(|| "no answer to the request".to_string());
        }
        let mut events = SseEvents::default();
        let mut body = res.bytes_stream();
        while let Some(chunk) = body.next().await {
            let chunk = chunk.map_err(|e| e.to_string())?;
            for (_, data) in events.push(&chunk) {
                if let Ok(m) = serde_json::from_str::<Value>(&data)
                    && m.get("method").is_none()
                    && m["id"].as_u64() == Some(id)
                {
                    return Ok(m);
                }
            }
        }
        Err("the event stream ended without an answer".into())
    }
}

struct Sse {
    client: reqwest::Client,
    headers: reqwest::header::HeaderMap,
    endpoint: String,
    pending: Pending,
    alive: Arc<AtomicBool>,
    reader: tokio::task::JoinHandle<()>,
}

impl Drop for Sse {
    fn drop(&mut self) {
        self.reader.abort();
    }
}

impl Sse {
    async fn open(
        client: reqwest::Client,
        url: &str,
        headers: reqwest::header::HeaderMap,
    ) -> Result<Sse, String> {
        let res = client
            .get(url)
            .headers(headers.clone())
            .header("Accept", "text/event-stream")
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if !res.status().is_success() {
            return Err(format!("HTTP {}", res.status()));
        }
        let base = res.url().clone();
        let mut body = res.bytes_stream();
        let mut events = SseEvents::default();
        let mut endpoint = None;
        let mut early = vec![];
        while endpoint.is_none() {
            let Some(chunk) = body.next().await else {
                return Err("the event stream ended before it named an endpoint".into());
            };
            for (event, data) in events.push(&chunk.map_err(|e| e.to_string())?) {
                if event == "endpoint" && endpoint.is_none() {
                    let target = base
                        .join(data.trim())
                        .map_err(|e| format!("endpoint {data}: {e}"))?;
                    if target.origin() != base.origin() {
                        return Err(
                            "SSE message endpoint must have the same origin as the server".into(),
                        );
                    }
                    endpoint = Some(target.to_string());
                } else {
                    early.push(data);
                }
            }
        }
        let pending: Pending = Arc::default();
        let alive = Arc::new(AtomicBool::new(true));
        let reader = {
            let (pending, alive) = (pending.clone(), alive.clone());
            let (client, headers, endpoint) = (client.clone(), headers.clone(), endpoint.clone());
            tokio::spawn(async move {
                let handle = |data: String| {
                    let (client, headers, endpoint, pending) = (
                        client.clone(),
                        headers.clone(),
                        endpoint.clone(),
                        pending.clone(),
                    );
                    async move {
                        let Ok(msg) = serde_json::from_str::<Value>(&data) else {
                            return;
                        };
                        if let Some(reply) = answer_to(&msg) {
                            if let Some(e) = &endpoint {
                                let _ = client.post(e).headers(headers).json(&reply).send().await;
                            }
                            return;
                        }
                        deliver(&pending, msg);
                    }
                };
                for d in early {
                    handle(d).await;
                }
                while let Some(Ok(chunk)) = body.next().await {
                    for (_, data) in events.push(&chunk) {
                        handle(data).await;
                    }
                }
                alive.store(false, Ordering::Relaxed);
                pending.lock().unwrap().clear();
            })
        };
        Ok(Sse {
            client,
            headers,
            endpoint: endpoint.unwrap_or_default(),
            pending,
            alive,
            reader,
        })
    }

    async fn post(&self, msg: &Value) -> Result<(), String> {
        let res = self
            .client
            .post(&self.endpoint)
            .headers(self.headers.clone())
            .json(msg)
            .send()
            .await
            .map_err(|e| e.to_string())?;
        if res.status().is_success() {
            Ok(())
        } else {
            Err(format!("HTTP {}", res.status()))
        }
    }

    async fn request(&self, id: u64, msg: &Value) -> Result<Value, String> {
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().unwrap();
            pending.retain(|_, tx| !tx.is_closed());
            pending.insert(id, tx);
        }
        if let Err(e) = self.post(msg).await {
            self.pending.lock().unwrap().remove(&id);
            return Err(e);
        }
        rx.await.map_err(|_| "the event stream ended".to_string())
    }
}

/// Server-sent events from chunks: `(event, data)` for each complete event.
#[derive(Default)]
struct SseEvents {
    buf: Vec<u8>,
    event: String,
    data: Vec<String>,
}

impl SseEvents {
    fn push(&mut self, chunk: &[u8]) -> Vec<(String, String)> {
        self.buf.extend_from_slice(chunk);
        let mut out = vec![];
        while let Some(i) = self.buf.iter().position(|&b| b == b'\n') {
            let line: Vec<u8> = self.buf.drain(..=i).collect();
            let line = String::from_utf8_lossy(&line);
            let line = line.trim_end_matches(['\n', '\r']);
            if line.is_empty() {
                if !self.data.is_empty() {
                    let event = std::mem::take(&mut self.event);
                    out.push((
                        if event.is_empty() {
                            "message".into()
                        } else {
                            event
                        },
                        self.data.join("\n"),
                    ));
                    self.data.clear();
                }
                self.event.clear();
            } else if let Some(v) = line.strip_prefix("data:") {
                self.data.push(v.strip_prefix(' ').unwrap_or(v).to_string());
            } else if let Some(v) = line.strip_prefix("event:") {
                self.event = v.trim().to_string();
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    #[tokio::test]
    async fn changed_library_metadata_invalidates_its_list_immediately() {
        let caches = steve_runtime::cache::Caches::new(Default::default());
        let cache = caches.space(&LIST_SPACE);
        let key = ListingKey {
            id: "server".into(),
            updated: 1,
        };
        cache
            .get_or_load(key.clone(), || async {
                Ok::<_, ()>(ToolList { tools: vec![] })
            })
            .await
            .unwrap();
        caches.invalidate(&[steve_runtime::cache::Tag::global("hub")]);
        assert!(
            cache.peek(&key).await.is_some(),
            "unrelated configuration is not a dependency"
        );
        caches.invalidate(&[steve_runtime::cache::Tag::of("mcp-server", "server")]);
        let tool = RemoteTool {
            name: "new".into(),
            description: String::new(),
            input_schema: json!({}),
            annotations: json!({}),
        };
        let tools = cache
            .get_or_load(key, || async {
                Ok::<_, ()>(ToolList { tools: vec![tool] })
            })
            .await
            .unwrap();
        assert_eq!(tools.tools[0].name, "new");
    }

    use super::*;

    #[test]
    fn events_are_read_across_chunks() {
        let mut e = SseEvents::default();
        assert!(e.push(b"event: endpoint\ndata: /messages?s=1").is_empty());
        assert_eq!(
            e.push(b"\n\ndata: {\"a\":1}\r\n\r\n"),
            [
                ("endpoint".to_string(), "/messages?s=1".to_string()),
                ("message".to_string(), "{\"a\":1}".to_string())
            ]
        );
    }

    #[test]
    fn a_result_reads_as_text() {
        assert_eq!(
            result_text(
                &json!({"content": [{"type": "text", "text": "a"}, {"type": "image", "mimeType": "image/png", "data": "AAAA"}]})
            ),
            Ok("a\n[image (image/png, 4 bytes base64)]".into())
        );
        assert_eq!(
            result_text(&json!({"content": [], "structuredContent": {"n": 1}})),
            Ok("{\"n\":1}".into())
        );
        assert_eq!(
            result_text(&json!({"content": [{"type": "text", "text": "nope"}], "isError": true})),
            Err("nope".into())
        );
    }
}

pub(crate) const CACHE_SPECS: [(&str, u8); 2] = [
    (LIST_SPACE.name, LIST_SPACE.share_pct),
    (FAILURE_SPACE.name, FAILURE_SPACE.share_pct),
];

#[cfg(test)]
pub(crate) fn register_cache_spaces(caches: &steve_runtime::cache::Caches) {
    let _ = caches.space(&LIST_SPACE);
    let _ = caches.space(&FAILURE_SPACE);
}
