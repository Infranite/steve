//! The Hub as the MCP client of the library's servers (`steve_modules::caps::mcp`): a Bot's sessions
//! reach a server's tools through Steve's one tool endpoint ([`super::tool_registry`]), never through a
//! server of their own. The Hub lists each server's tools (kept until the server changes, refreshed
//! every [`FRESH`]) and runs a call on the session's behalf.
//!
//! - `stdio`: the Hub starts the server on its own machine, in the same OS sandbox a Node's runs get
//!   ([`steve_node::Sandbox`]), one process per server shared by every session that uses it, stopped
//!   after [`IDLE`] without a call (and when the server is changed or deleted).
//! - `http`: MCP streamable HTTP (JSON or an event stream per answer, `Mcp-Session-Id` kept).
//! - `sse`: the older HTTP+SSE transport (an event stream names the endpoint messages are posted to).
//!
//! Secret values (env, headers) are read from the vault when a connection opens, used for it only and
//! never sent to a Node or a harness.

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures::StreamExt;
use serde_json::{Value, json};
use steve_modules::caps::Capabilities;
use steve_store::caps::mcp::Server;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::oneshot;

/// A server's list of tools is asked again after this.
pub const FRESH: Duration = Duration::from_secs(10 * 60);
/// After a failed listing, the next try waits this long.
const RETRY: Duration = Duration::from_secs(60);
/// A connection without a call for this long is closed (a stdio server stops).
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

struct Listed {
    updated: i64,
    at: Instant,
    tools: Result<Vec<RemoteTool>, String>,
}

/// The Hub's connections to the library's servers. One per Hub.
pub struct LibraryMcp {
    caps: Capabilities,
    sandbox: Option<steve_node::Sandbox>,
    http: reqwest::Client,
    conns: Mutex<HashMap<String, Arc<tokio::sync::Mutex<Option<Arc<Conn>>>>>>,
    lists: Mutex<HashMap<String, Listed>>,
    /// Servers being listed now.
    refreshing: Mutex<std::collections::HashSet<String>>,
}

impl LibraryMcp {
    /// `sandbox`: what stdio servers start in (`None`: as they are, for tests).
    pub fn new(caps: Capabilities, sandbox: Option<steve_node::Sandbox>) -> Arc<LibraryMcp> {
        let http = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(10))
            // Custom credential headers must not follow a redirect to a different origin.
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("steve-hub/", env!("CARGO_PKG_VERSION")))
            .build()
            .unwrap_or_default();
        Arc::new(LibraryMcp {
            caps,
            sandbox,
            http,
            conns: Mutex::default(),
            lists: Mutex::default(),
            refreshing: Mutex::default(),
        })
    }

    /// The tools `server` has, as last listed; `None`: not listed yet (or the server changed since).
    pub fn cached(&self, server: &Server) -> Option<Result<Vec<RemoteTool>, String>> {
        let lists = self.lists.lock().unwrap();
        lists
            .get(&server.id)
            .filter(|l| l.updated == server.updated)
            .map(|l| l.tools.clone())
    }

    /// What is known of `server`'s tools for its present version, and whether that is older than
    /// [`FRESH`] (or, after a failure, [`RETRY`]).
    fn lookup(&self, server: &Server) -> Option<(Result<Vec<RemoteTool>, String>, bool)> {
        let lists = self.lists.lock().unwrap();
        let l = lists
            .get(&server.id)
            .filter(|l| l.updated == server.updated)?;
        let wait = if l.tools.is_ok() { FRESH } else { RETRY };
        Some((l.tools.clone(), l.at.elapsed() >= wait))
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
        if let Some((tools, stale)) = self.lookup(server) {
            if stale {
                self.warm(server, who);
            }
            return tools;
        }
        self.warm(server, who);
        let until = Instant::now() + wait;
        loop {
            tokio::time::sleep(Duration::from_millis(50)).await;
            if let Some((tools, _)) = self.lookup(server) {
                return tools;
            }
            if Instant::now() >= until {
                return Err(format!(
                    "MCP server {} has not listed its tools yet (still trying)",
                    server.name
                ));
            }
        }
    }

    /// Lists `server`'s tools in the background unless that is under way; a failed listing keeps the
    /// list an earlier one got for the same version of the server.
    pub fn warm(self: &Arc<Self>, server: &Server, who: &str) {
        let Ok(rt) = tokio::runtime::Handle::try_current() else {
            return;
        };
        if !self.refreshing.lock().unwrap().insert(server.id.clone()) {
            return;
        }
        let (me, server, who) = (self.clone(), server.clone(), who.to_string());
        rt.spawn(async move {
            let got = match tokio::time::timeout(OPEN_WAIT, me.list_now(&server, &who)).await {
                Ok(r) => r,
                Err(_) => Err(format!(
                    "MCP server {} did not list its tools within {} s",
                    server.name,
                    OPEN_WAIT.as_secs()
                )),
            };
            let mut lists = me.lists.lock().unwrap();
            let tools = match (got, lists.get(&server.id)) {
                (Err(e), Some(old)) if old.updated == server.updated && old.tools.is_ok() => {
                    tracing::warn!(server = %server.name, "MCP server listing failed, keeping the last list: {e}");
                    old.tools.clone()
                }
                (Err(e), _) => {
                    tracing::warn!(server = %server.name, "MCP server listing failed: {e}");
                    Err(e)
                }
                (ok, _) => ok,
            };
            lists.insert(
                server.id.clone(),
                Listed {
                    updated: server.updated,
                    at: Instant::now(),
                    tools,
                },
            );
            drop(lists);
            me.refreshing.lock().unwrap().remove(&server.id);
        });
    }

    async fn list_now(&self, server: &Server, who: &str) -> Result<Vec<RemoteTool>, String> {
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
    pub fn sweep(&self) {
        let ids: Vec<String> = self.conns.lock().unwrap().keys().cloned().collect();
        let current = self.caps.vault().store().reader(|c| {
            ids.iter()
                .map(|id| {
                    Ok((
                        id.clone(),
                        steve_store::caps::mcp::get(c, id)?.map(|s| s.updated),
                    ))
                })
                .collect::<Result<HashMap<_, _>, steve_store::StoreError>>()
        });
        let Ok(current) = current else {
            return;
        };
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
        self.lists
            .lock()
            .unwrap()
            .retain(|id, l| current.get(id).copied().flatten() == Some(l.updated));
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
        let link = match server.transport.as_str() {
            "stdio" => {
                let env: Vec<(String, String)> = server
                    .env
                    .iter()
                    .map(|v| {
                        let value = if v.secret {
                            secret(steve_store::caps::mcp::secret_label("stdio", &v.name))?
                        } else {
                            v.value.clone().unwrap_or_default()
                        };
                        Ok::<_, String>((v.name.clone(), value))
                    })
                    .collect::<Result<_, _>>()?;
                Link::Stdio(Stdio::start(self.sandbox.as_ref(), server, env)?)
            }
            t => {
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
                if t == "sse" {
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
                }
            }
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
    Stdio(Stdio),
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
            Link::Stdio(s) => s.alive.load(Ordering::Relaxed),
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
            Link::Stdio(s) => s.request(id, &msg).await,
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
            Link::Stdio(s) => s.send(&msg).await,
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

struct Stdio {
    stdin: Arc<tokio::sync::Mutex<tokio::process::ChildStdin>>,
    pending: Pending,
    alive: Arc<AtomicBool>,
    stderr: Arc<Mutex<VecDeque<String>>>,
    _child: steve_node::CapabilityProcess,
}

impl Stdio {
    fn start(
        sandbox: Option<&steve_node::Sandbox>,
        server: &Server,
        env: Vec<(String, String)>,
    ) -> Result<Stdio, String> {
        let program = server.command.clone().unwrap_or_default();
        let args: Vec<std::ffi::OsString> = server.args.iter().map(Into::into).collect();
        let mut process = steve_node::Sandbox::spawn_capability(sandbox, program.into(), args, env)
            .map_err(|e| {
                format!(
                    "could not start {}: {e}",
                    server.command.as_deref().unwrap_or("")
                )
            })?;
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
                // every request still waiting fails now
                pending.lock().unwrap().clear();
            });
        }
        {
            let stderr = stderr.clone();
            tokio::spawn(async move {
                let mut lines = BufReader::new(stderr_pipe).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let mut s = stderr.lock().unwrap();
                    if s.len() >= 20 {
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
            let mut pending = self.pending.lock().unwrap();
            pending.retain(|_, tx| !tx.is_closed());
            pending.insert(id, tx);
        }
        if let Err(e) = self.send(msg).await {
            self.pending.lock().unwrap().remove(&id);
            return Err(self.exited(&e));
        }
        rx.await.map_err(|_| self.exited("it stopped"))
    }

    fn exited(&self, why: &str) -> String {
        let tail: Vec<String> = self.stderr.lock().unwrap().iter().cloned().collect();
        if tail.is_empty() {
            why.to_string()
        } else {
            format!("{why}; its last output: {}", tail.join(" | "))
        }
    }
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
