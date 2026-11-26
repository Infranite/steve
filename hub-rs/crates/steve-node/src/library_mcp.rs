//! Sandboxed `stdio` MCP server host on the colocated Node.
//!
//! External library `stdio` MCP servers are spawned and kept alive here inside
//! `steve_confine::Sandbox::spawn_capability`, never in the Hub process. Secrets named in
//! `McpStdioServer::secret_env` are fetched once per process spawn from the Hub via
//! `HubSecrets::take(operation, secret_id)` and never persisted or logged.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use steve_node_api::*;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::oneshot;
use zeroize::Zeroizing;

/// A stdio MCP server without a call for this long is stopped on sweep.
const IDLE: Duration = Duration::from_secs(10 * 60);
/// Opening a connection and listing tools (`npx -y ...` may fetch on first start).
const OPEN_WAIT: Duration = Duration::from_secs(60);
/// One tool call; the Hub/Node tool envelope allows 120 s.
const CALL_WAIT: Duration = Duration::from_secs(110);
const MCP_PROTOCOL: &str = "2025-06-18";
/// Maximum number of `tools/list` pagination pages accepted from one server.
const MAX_TOOL_PAGES: usize = 20;
/// Maximum stderr lines retained for exit diagnostics.
const STDERR_TAIL_LINES: usize = 20;

#[derive(Default)]
pub(crate) struct StdioMcpHost {
    conns: Mutex<HashMap<String, Arc<tokio::sync::Mutex<Option<Arc<StdioConn>>>>>>,
}

impl StdioMcpHost {
    pub(crate) async fn list(
        &self,
        sandbox: Option<Arc<steve_confine::Sandbox>>,
        hub_secrets: Option<Arc<dyn HubSecrets>>,
        server: McpStdioServer,
    ) -> ApiResult<Vec<McpStdioTool>> {
        let name = server.name.clone();
        let fut = async {
            let conn = self.conn(sandbox, hub_secrets, &server).await?;
            let mut out = Vec::new();
            let mut cursor: Option<String> = None;
            for _ in 0..MAX_TOOL_PAGES {
                let params = match &cursor {
                    Some(c) => json!({ "cursor": c }),
                    None => json!({}),
                };
                let page = conn
                    .request("tools/list", params)
                    .await
                    .map_err(|e| Error::new(Code::Internal, e))?;
                let tools = page["tools"].as_array().ok_or_else(|| {
                    Error::new(
                        Code::Internal,
                        format!("MCP server {} returned no tools array", server.name),
                    )
                })?;
                for t in tools {
                    let tname = t["name"]
                        .as_str()
                        .filter(|n| !n.is_empty())
                        .ok_or_else(|| {
                            Error::new(
                                Code::Internal,
                                format!(
                                    "MCP server {} returned a tool without a name",
                                    server.name
                                ),
                            )
                        })?;
                    if !t["inputSchema"].is_object() {
                        return Err(Error::new(
                            Code::Internal,
                            format!(
                                "MCP server {} tool {tname} has no object inputSchema",
                                server.name
                            ),
                        ));
                    }
                    if out.iter().any(|old: &McpStdioTool| old.name == tname) {
                        return Err(Error::new(
                            Code::Internal,
                            format!("MCP server {} repeated tool {tname}", server.name),
                        ));
                    }
                    out.push(McpStdioTool {
                        name: tname.to_string(),
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
                return Err(Error::new(
                    Code::Internal,
                    format!(
                        "MCP server {} exceeded the {MAX_TOOL_PAGES}-page tool catalog limit",
                        server.name
                    ),
                ));
            }
            Ok(out)
        };
        tokio::time::timeout(OPEN_WAIT, fut).await.map_err(|_| {
            Error::new(
                Code::Timeout,
                format!(
                    "MCP server {name} did not list its tools within {} s",
                    OPEN_WAIT.as_secs()
                ),
            )
        })?
    }

    pub(crate) async fn call(
        &self,
        sandbox: Option<Arc<steve_confine::Sandbox>>,
        hub_secrets: Option<Arc<dyn HubSecrets>>,
        req: McpStdioCallRequest,
    ) -> ApiResult<McpStdioCallResponse> {
        let name = req.server.name.clone();
        let tool = req.tool.clone();
        let fut = async {
            let conn = self.conn(sandbox, hub_secrets, &req.server).await?;
            let result = conn
                .request(
                    "tools/call",
                    json!({ "name": req.tool, "arguments": req.arguments }),
                )
                .await
                .map_err(|e| Error::new(Code::Internal, e))?;
            Ok(McpStdioCallResponse { result })
        };
        tokio::time::timeout(CALL_WAIT, fut).await.map_err(|_| {
            Error::new(
                Code::Timeout,
                format!(
                    "MCP server {name} did not answer {tool} within {} s",
                    CALL_WAIT.as_secs()
                ),
            )
        })?
    }

    pub(crate) fn sweep(&self, live: &[(String, i64)]) {
        let current: HashMap<&str, i64> = live.iter().map(|(id, u)| (id.as_str(), *u)).collect();
        let mut conns = self.conns.lock().unwrap_or_else(|e| e.into_inner());
        conns.retain(|id, slot| {
            if let Ok(mut g) = slot.try_lock() {
                let live_updated = current.get(id.as_str()).copied();
                if g.as_ref().is_some_and(|c| {
                    live_updated != Some(c.updated)
                        || (c.active.load(Ordering::Relaxed) == 0 && c.idle() > IDLE)
                        || !c.alive()
                }) {
                    *g = None;
                }
                return live_updated.is_some();
            }
            true
        });
    }

    pub(crate) fn shutdown(&self) {
        let slots: Vec<_> = self
            .conns
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain()
            .map(|(_, s)| s)
            .collect();
        for slot in slots {
            if let Ok(mut g) = slot.try_lock() {
                *g = None;
            }
        }
    }

    async fn conn(
        &self,
        sandbox: Option<Arc<steve_confine::Sandbox>>,
        hub_secrets: Option<Arc<dyn HubSecrets>>,
        server: &McpStdioServer,
    ) -> ApiResult<Arc<StdioConn>> {
        let slot = self
            .conns
            .lock()
            .unwrap_or_else(|e| e.into_inner())
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
        let mut env = server.env.clone();
        if !server.secret_env.is_empty() {
            let secrets = hub_secrets.ok_or_else(|| {
                Error::new(
                    Code::Unavailable,
                    format!(
                        "Hub secrets channel unavailable for MCP server {}",
                        server.name
                    ),
                )
            })?;
            for (var_name, secret_id) in &server.secret_env {
                let bytes = secrets
                    .take(server.operation.clone(), secret_id.clone())
                    .await
                    .map_err(|e| {
                        Error::new(
                            e.code,
                            format!(
                                "secret for MCP server {} ({var_name}): {}",
                                server.name, e.safe_detail
                            ),
                        )
                    })?;
                let val =
                    Zeroizing::new(String::from_utf8(bytes.expose().to_vec()).map_err(|_| {
                        Error::new(
                            Code::Invalid,
                            format!(
                                "secret for MCP server {} ({var_name}) is not valid UTF-8",
                                server.name
                            ),
                        )
                    })?);
                env.push((var_name.clone(), val.to_string()));
            }
        }
        let stdio = Stdio::start(sandbox.as_deref(), server, env)
            .map_err(|e| Error::new(Code::Internal, e))?;
        let c = Arc::new(StdioConn {
            name: server.name.clone(),
            updated: server.updated,
            last: Mutex::new(Instant::now()),
            next: AtomicU64::new(1),
            active: AtomicUsize::new(0),
            stdio,
        });
        c.initialize()
            .await
            .map_err(|e| Error::new(Code::Internal, e))?;
        *g = Some(c.clone());
        Ok(c)
    }
}

struct StdioConn {
    name: String,
    updated: i64,
    last: Mutex<Instant>,
    next: AtomicU64,
    active: AtomicUsize,
    stdio: Stdio,
}

impl StdioConn {
    fn touch(&self) {
        *self.last.lock().unwrap_or_else(|e| e.into_inner()) = Instant::now();
    }

    fn idle(&self) -> Duration {
        self.last
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .elapsed()
    }

    fn alive(&self) -> bool {
        self.stdio.alive.load(Ordering::Relaxed)
    }

    async fn initialize(&self) -> Result<(), String> {
        let _ = self
            .request(
                "initialize",
                json!({
                    "protocolVersion": MCP_PROTOCOL,
                    "capabilities": {},
                    "clientInfo": {"name": "steve", "version": env!("CARGO_PKG_VERSION")},
                }),
            )
            .await?;
        self.notify("notifications/initialized").await
    }

    async fn request(&self, method: &str, params: Value) -> Result<Value, String> {
        self.active.fetch_add(1, Ordering::Relaxed);
        struct Activity<'a>(&'a StdioConn);
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
        let answer = self
            .stdio
            .request(id, &msg)
            .await
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
        self.stdio
            .send(&msg)
            .await
            .map_err(|e| format!("MCP server {}: {e}", self.name))
    }
}

type Pending = Arc<Mutex<HashMap<u64, oneshot::Sender<Value>>>>;

fn deliver(pending: &Pending, msg: Value) {
    let id = msg.get("id").and_then(Value::as_u64);
    if msg.get("method").is_none()
        && let Some(id) = id
        && let Some(tx) = pending
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&id)
    {
        let _ = tx.send(msg);
    }
}

fn answer_to(msg: &Value) -> Option<Value> {
    let id = msg.get("id")?;
    let method = msg.get("method")?.as_str()?;
    Some(if method == "ping" {
        json!({"jsonrpc": "2.0", "id": id, "result": {}})
    } else {
        json!({"jsonrpc": "2.0", "id": id, "error": {"code": -32601, "message": "not supported by Steve"}})
    })
}

struct Stdio {
    stdin: Arc<tokio::sync::Mutex<tokio::process::ChildStdin>>,
    pending: Pending,
    alive: Arc<AtomicBool>,
    stderr: Arc<Mutex<VecDeque<String>>>,
    _child: steve_confine::CapabilityProcess,
}

impl Stdio {
    fn start(
        sandbox: Option<&steve_confine::Sandbox>,
        server: &McpStdioServer,
        env: Vec<(String, String)>,
    ) -> Result<Stdio, String> {
        let args: Vec<std::ffi::OsString> = server.args.iter().map(Into::into).collect();
        let mut process = steve_confine::Sandbox::spawn_capability(
            sandbox,
            server.command.clone().into(),
            args,
            env,
        )
        .map_err(|e| format!("could not start {}: {e}", server.command))?;
        let child = &mut process.child;
        let stdin = Arc::new(tokio::sync::Mutex::new(
            child.stdin.take().ok_or("no stdin")?,
        ));
        let stdout = child.stdout.take().ok_or("no stdout")?;
        let stderr_pipe = child.stderr.take().ok_or("no stderr")?;
        let pending: Pending = Arc::default();
        let alive = Arc::new(AtomicBool::new(true));
        let stderr: Arc<Mutex<VecDeque<String>>> = Arc::default();
        {
            let (pending, alive, stdin) = (pending.clone(), alive.clone(), stdin.clone());
            tokio::spawn(async move {
                let mut lines = BufReader::new(stdout).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let Ok(msg) = serde_json::from_str::<Value>(&line) else {
                        continue;
                    };
                    if let Some(reply) = answer_to(&msg) {
                        let mut w = stdin.lock().await;
                        let _ = w.write_all(format!("{reply}\n").as_bytes()).await;
                        let _ = w.flush().await;
                        continue;
                    }
                    deliver(&pending, msg);
                }
                alive.store(false, Ordering::Relaxed);
                pending.lock().unwrap_or_else(|e| e.into_inner()).clear();
            });
        }
        {
            let stderr = stderr.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr_pipe).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let mut s = stderr.lock().unwrap_or_else(|e| e.into_inner());
                    if s.len() >= STDERR_TAIL_LINES {
                        s.pop_front();
                    }
                    s.push_back(line);
                }
            });
        }
        Ok(Stdio {
            stdin,
            pending,
            alive,
            stderr,
            _child: process,
        })
    }

    async fn send(&self, msg: &Value) -> Result<(), String> {
        let mut w = self.stdin.lock().await;
        w.write_all(format!("{msg}\n").as_bytes())
            .await
            .map_err(|e| e.to_string())?;
        w.flush().await.map_err(|e| e.to_string())
    }

    async fn request(&self, id: u64, msg: &Value) -> Result<Value, String> {
        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
            pending.retain(|_, tx| !tx.is_closed());
            pending.insert(id, tx);
        }
        if let Err(e) = self.send(msg).await {
            self.pending
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&id);
            return Err(self.exited(&e));
        }
        rx.await.map_err(|_| self.exited("it stopped"))
    }

    fn exited(&self, why: &str) -> String {
        let tail: Vec<String> = self
            .stderr
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .cloned()
            .collect();
        if tail.is_empty() {
            why.to_string()
        } else {
            format!("{why}; its last output: {}", tail.join(" | "))
        }
    }
}
