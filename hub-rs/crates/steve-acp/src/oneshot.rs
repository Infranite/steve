//! One-shot ACP calls (harness parity S2): a single prompt on a fresh (or loaded) session of any
//! adapter, for think jobs, research and Steve's own model calls. Every adapter goes the same way:
//! `initialize`, `session/new|load` in a read-only mode the adapter offers, the model and effort as
//! ACP config options, `session/prompt`, its chunks streamed as Steve's events, permission requests
//! answered by the caller's tool policy, and the usage it reported.
//!
//! The caller owns the process (its journal, its sandbox through [`Request::wrap`]) and the job's
//! deadline and cancellation policy. Use [`run_cancellable`] and await its result to settle usage
//! after stopping; dropping either future is emergency process cleanup without a usage receipt.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use crate::AcpConfig;
use crate::normalize::{Fact, Normalizer, ToolFact};
use crate::permission;
use crate::rpc::{Conn, Incoming, RpcError, Spawn};
use crate::session::{self, OpenConfig, Reports};
use crate::usage::{POLL_INTERVAL, SETTLE_WAIT, Sampler};
use serde_json::{Value, json};

use steve_node_api::{
    Epoch, EventBody, PermissionDecision, PermissionMode, Role, ToolCategory, ToolOrigin,
    ToolPhase, UsageReport,
};
use tokio::sync::mpsc;

/// What [`OneShotError::ToolRefused`] says, also where a Node reports it back as text.
pub const TOOL_REFUSED: &str = "read-only policy refused a tool";

/// Why a one-shot call failed. The words a user sees are the caller's.
#[derive(Debug, thiserror::Error)]
pub enum OneShotError {
    /// The accepted run's output/ingress budget was exhausted.
    #[error("output limit")]
    OutputLimit,
    /// Still running after the timeout; the adapter is stopped.
    #[error("timed out after {0:?}")]
    Timeout(Duration),
    /// The adapter could not be started or opened.
    #[error("{0}")]
    Failed(String),
    /// The model reached for a tool the call's policy does not allow (a call without tools, or
    /// with a narrower set than it wanted): the request needs what this call cannot do, and the
    /// caller decides where it goes instead.
    #[error("{TOOL_REFUSED}")]
    ToolRefused,
    /// The adapter answered the prompt with an error (a JSON-RPC error: the provider, the model).
    /// `transient`: it said the service was unavailable or overloaded, so another try may succeed.
    #[error("{text}")]
    Harness {
        text: String,
        code: Option<i64>,
        transient: bool,
    },
    /// The adapter lacks what the call needs (`unsupported: images`).
    #[error("unsupported: {0}")]
    Unsupported(&'static str),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

impl OneShotError {
    /// Whether another try of the same call may succeed.
    pub fn transient(&self) -> bool {
        matches!(
            self,
            OneShotError::Harness {
                transient: true,
                ..
            }
        )
    }

    /// The adapter's error answer as a harness error: transient when its words say so.
    pub fn harness(e: RpcError) -> Self {
        let code = e
            .data
            .as_ref()
            .and_then(|d| d.get("code"))
            .and_then(Value::as_i64);
        let detail = e
            .data
            .as_ref()
            .and_then(|d| d.get("data"))
            .map(|d| match d {
                Value::String(s) => s.clone(),
                d => d.to_string(),
            })
            .filter(|d| !d.is_empty() && !e.message.contains(d.as_str()));
        let text = match detail {
            Some(d) => format!("{}: {d}", e.message),
            None => e.message,
        };
        let transient = crate::is_transient(&text);
        OneShotError::Harness {
            text,
            code,
            transient,
        }
    }
}

/// An image for the prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    /// `image/png`, `image/jpeg`…
    pub mime: String,
    pub bytes: Vec<u8>,
}

impl Image {
    /// The ACP `image` content block.
    pub fn block(&self) -> Value {
        use base64::Engine;
        json!({"type": "image", "mimeType": self.mime,
            "data": base64::engine::general_purpose::STANDARD.encode(&self.bytes)})
    }
}

/// An adapter's program and arguments.
pub type Command = (PathBuf, Vec<OsString>);

/// The variables an adapter is started with over the inherited ones.
pub type Env = Vec<(OsString, OsString)>;

/// The adapter's command as started: the caller may wrap it (an OS sandbox) and add variables.
pub type Wrap<'a> = &'a (dyn Fn(Command, &mut Env) -> Command + Send + Sync);

pub struct Request<'a> {
    pub cfg: &'a AcpConfig,
    pub harness: &'a str,
    pub cwd: &'a Path,
    pub model: Option<&'a str>,
    pub effort: Option<&'a str>,
    pub resume: Option<&'a str>,
    /// The call's stable instructions: the session's system prompt where the adapter takes one
    /// (`sessionCapabilities.systemPrompt`), else the start of the prompt.
    pub head: Option<&'a str>,
    /// The call uses no tool of the harness's: the head replaces the harness's own system prompt,
    /// and an adapter that declares how its built-in tools are switched off has them off.
    pub tools_off: bool,
    /// Steve's permission mode the session is set to: Plan for a call that only reads.
    pub mode: PermissionMode,
    pub prompt: Vec<Value>,
    pub mcp_servers: Value,
    /// Wraps the adapter's command (the caller's sandbox); `None`: it runs as it is.
    pub wrap: Option<Wrap<'a>>,
    pub timeout: Duration,
    pub prefix: &'a str,
    pub on_session: Option<&'a (dyn Fn(&str) + Send + Sync)>,
    /// The accepted run's ingress budget, not a fixed count of chunks.
    pub max_output_bytes: u64,
    /// Activity without exposing private thought text to the caller's draft.
    pub on_activity: Option<&'a (dyn Fn() + Send + Sync)>,
    /// Complete, merged facts; unknown tools and incomplete inputs must fail closed.
    pub allow_tool: &'a (dyn Fn(&ToolFact) -> bool + Send + Sync),
}

pub struct Outcome {
    pub text: String,
    pub usage: Vec<UsageReport>,
    pub session: String,
    pub stop_reason: String,
}

/// A dropped job must stop its adapter even when dropped during initialize/session/new.
struct Owned(Arc<Conn>, bool);

impl Drop for Owned {
    fn drop(&mut self) {
        if !self.1 {
            self.0.kill();
        }
    }
}

struct SessionReports<'a>(Option<&'a (dyn Fn(&str) + Send + Sync)>);
impl Reports for SessionReports<'_> {
    fn new_session(&self, id: &str) {
        if let Some(report) = self.0 {
            report(id);
        }
    }
}

#[derive(Default)]
struct Output {
    session: Option<String>,
    message: Option<String>,
    assistant: std::collections::HashSet<String>,
    text: String,
    usage: Vec<UsageReport>,
}

impl Output {
    fn events(&mut self, facts: Vec<Fact>, on_event: &(dyn Fn(EventBody) + Send + Sync)) {
        for fact in facts {
            let Fact::Event(event) = fact else { continue };
            match &event {
                EventBody::MessageStarted {
                    message,
                    role: Role::Assistant,
                    ..
                } => {
                    self.message = Some(message.clone());
                    self.assistant.insert(message.clone());
                    self.text.clear();
                }
                EventBody::MessageStarted { .. } => continue,
                EventBody::TextDelta {
                    message,
                    utf8_offset,
                    text,
                    ..
                } if self.message.as_deref() == Some(message.as_str()) => {
                    let at = *utf8_offset as usize;
                    if at <= self.text.len() && self.text.is_char_boundary(at) {
                        self.text.truncate(at);
                        self.text.push_str(text);
                    }
                }
                EventBody::TextDelta { .. } => continue,
                EventBody::MessageCompleted { message } if !self.assistant.contains(message) => {
                    continue;
                }
                EventBody::Usage { report } => self.usage.push(report.clone()),
                _ => {}
            }
            on_event(event);
        }
    }
}

fn failed(e: impl std::fmt::Display) -> OneShotError {
    OneShotError::Failed(e.to_string())
}

/// ACP sends a pending tool before its arguments finish streaming. There is nothing to
/// authorize yet when its required target/command is missing. Inspect every refinement as
/// soon as that field arrives; requests for permission and non-pending facts never defer.
fn awaiting_input(fact: &ToolFact) -> bool {
    fact.phase == ToolPhase::Pending
        && fact.origin == ToolOrigin::Harness
        && fact.url.is_none()
        && match fact.category {
            ToolCategory::Read => fact.paths.is_empty() && fact.command.is_none(),
            ToolCategory::Search => {
                fact.paths.is_empty()
                    && fact.command.is_none()
                    && fact.query.is_none()
                    && ["glob", "include", "file_pattern", "files"]
                        .iter()
                        .all(|k| fact.raw["rawInput"].get(*k).is_none())
            }
            ToolCategory::Exec => fact.command.is_none(),
            _ => false,
        }
}

fn incoming(
    conn: &Conn,
    inc: Incoming,
    norm: &mut Normalizer,
    out: &mut Output,
    req: &Request<'_>,
    on_event: &(dyn Fn(EventBody) + Send + Sync),
) -> Result<(), OneShotError> {
    match inc {
        Incoming::Notification { method, params } if method == "session/update" => {
            if out
                .session
                .as_deref()
                .is_some_and(|sid| params.get("sessionId").and_then(Value::as_str) != Some(sid))
            {
                return Ok(());
            }
            if let Some(activity) = req.on_activity {
                activity();
            }
            if let Some(update) = params.get("update") {
                let facts = norm.update(update);
                if let Some(id) = update.get("toolCallId").and_then(Value::as_str)
                    && norm
                        .fact(id)
                        .is_some_and(|f| !awaiting_input(f) && !(req.allow_tool)(f))
                {
                    return Err(OneShotError::ToolRefused);
                }
                out.events(facts, on_event);
            }
        }
        Incoming::Request { id, method, params } if method == "session/request_permission" => {
            let asked = permission::asked(norm, &id, &params);
            let allowed = (req.allow_tool)(&asked.fact);
            let decision = if allowed {
                PermissionDecision::AllowOnce
            } else {
                PermissionDecision::Reject
            };
            conn.respond(id, permission::answer(&asked, Some(decision)));
            if !allowed {
                return Err(OneShotError::ToolRefused);
            }
            out.events(asked.updates, on_event);
        }
        // initialize advertises no client-side filesystem or terminal access.
        Incoming::Request { id, .. } => {
            conn.respond_error(id, -32601, "client capability is unavailable");
        }
        _ => {}
    }
    Ok(())
}

/// Stream the adapter's updates while both the handshake and prompt are pending. The RPC reader
/// delivers notifications in stdout order, including every update before the prompt response.
fn incoming_during_open(
    conn: &Conn,
    inc: Incoming,
    on_activity: Option<&(dyn Fn() + Send + Sync)>,
    prior_tokens: &mut Option<(u64, u64, u64, u64, u64)>,
) {
    match inc {
        Incoming::Request { id, method, .. } if method == "session/request_permission" => {
            conn.respond(id, json!({"outcome":{"outcome":"cancelled"}}));
        }
        Incoming::Request { id, .. } => {
            conn.respond_error(id, -32601, "client capability is unavailable");
        }
        Incoming::Notification { method, params } if method == "session/update" => {
            if let Some(activity) = on_activity {
                activity();
            }
            if let Some(update) = params.get("update")
                && update.get("sessionUpdate").and_then(Value::as_str) == Some("usage_update")
                && let Some(t) = Normalizer::cumulative_tokens_from_usage_update(update)
            {
                *prior_tokens = Some(t);
            }
        }
        _ => {}
    }
}

/// Drains notifications emitted during `session/new` or `session/load` (including any queued
/// right before `opening` resolves) so replayed history never leaks into `session/prompt`.
async fn drive_open<T, E, F>(
    conn: &Conn,
    rx: &mut mpsc::UnboundedReceiver<Incoming>,
    opening: F,
    on_activity: Option<&(dyn Fn() + Send + Sync)>,
    prior_tokens: &mut Option<(u64, u64, u64, u64, u64)>,
) -> Result<T, OneShotError>
where
    E: std::fmt::Display,
    F: std::future::Future<Output = Result<T, E>>,
{
    tokio::pin!(opening);
    let result = loop {
        tokio::select! {
            biased;
            // session/load replays history before its response. That history is neither
            // a new draft nor a tool the current attempt is executing.
            Some(inc) = rx.recv() => incoming_during_open(conn, inc, on_activity, prior_tokens),
            result = &mut opening => break result,
        }
    };
    while let Ok(inc) = rx.try_recv() {
        incoming_during_open(conn, inc, on_activity, prior_tokens);
    }
    result.map_err(failed)
}

/// Streams updates until `prompting` resolves, then drains any updates already queued on `rx`
/// and seals `norm` before inspecting `Ok` vs `Err`—so usage/cost updates emitted right before
/// a JSON-RPC error response are never dropped when `prompting` wins a `select!` poll.
async fn drive_prompt<F, C>(
    conn: &Conn,
    rx: &mut mpsc::UnboundedReceiver<Incoming>,
    prompting: F,
    norm: &mut Normalizer,
    out: &mut Output,
    sampler: &mut Sampler,
    req: &Request<'_>,
    on_event: &(dyn Fn(EventBody) + Send + Sync),
    mut cancel: std::pin::Pin<&mut C>,
) -> Result<Value, OneShotError>
where
    F: std::future::Future<Output = Result<Value, RpcError>>,
    C: std::future::Future<Output = ()>,
{
    tokio::pin!(prompting);
    let mut tick = tokio::time::interval(POLL_INTERVAL);
    let result = loop {
        tokio::select! {
            biased;
            _ = &mut cancel => return Err(failed("cancelled")),
            Some(inc) = rx.recv() => incoming(conn, inc, norm, out, req, on_event)?,
            result = &mut prompting => break result,
            _ = tick.tick() => out.events(sampler.poll().await, on_event),
        }
    };
    while let Ok(inc) = rx.try_recv() {
        tokio::select! {
            biased;
            _ = &mut cancel => return Err(failed("cancelled")),
            _ = std::future::ready(()) => incoming(conn, inc, norm, out, req, on_event)?,
        }
    }
    out.events(norm.finish(), on_event);
    let end = result.map_err(OneShotError::harness)?;
    Ok(end)
}

#[cfg(test)]
async fn run(
    req: Request<'_>,
    on_spawn: &(dyn Fn(u32) -> bool + Send + Sync),
    on_event: &(dyn Fn(EventBody) + Send + Sync),
) -> Result<Outcome, OneShotError> {
    run_cancellable(req, on_spawn, on_event, std::future::pending()).await
}

/// Stop execution when `cancel` resolves, then settle this attempt's source usage before returning.
/// Cancellation must be awaited: no further handshake, prompt or tool permission is driven after
/// the signal. Only process shutdown and bounded source sampling continue. The caller retains its
/// cancellation/budget reason; this driver returns `Failed("cancelled")` for an external stop.
pub async fn run_cancellable(
    req: Request<'_>,
    on_spawn: &(dyn Fn(u32) -> bool + Send + Sync),
    on_event: &(dyn Fn(EventBody) + Send + Sync),
    cancel: impl std::future::Future<Output = ()>,
) -> Result<Outcome, OneShotError> {
    tokio::pin!(cancel);
    let timeout = req.timeout;
    let properties = req.cfg.adapter_properties(req.harness);
    let meter = Epoch::new(format!("{}.acp", req.prefix)).map_err(failed)?;
    let mut norm = Normalizer::new(req.cwd, req.prefix, meter.clone())
        .input_includes_cache(properties.usage_input_includes_cache);
    let mut out = Output::default();
    let mut sampler = None;
    let mut owned = None;
    let limit_hit = Arc::new(AtomicBool::new(false));
    let admitted_limit = limit_hit.clone();
    // Keep the sampler and process owner outside the timed future: a timeout must still stop
    // the producer, settle usage and call on_event before returning its error.
    let result = {
        let work = async {
            // Ready cancellation must not even spawn. Repeat the checkpoint before prompting,
            // because on_session can request a stop while the opening future is resolving.
            tokio::select! { biased; _ = &mut cancel => return Err(failed("cancelled")), _ = std::future::ready(()) => {} }
            let SpawnedJob { conn, mut rx, home } = spawn_job(&req, on_spawn, admitted_limit)?;
            owned = Some(Owned(conn.clone(), false));
            let mut prior_tokens = None;
            let opened = tokio::select! {
                biased;
                _ = &mut cancel => return Err(failed("cancelled")),
                result = open_job_session(&conn, &req, &mut rx, &mut prior_tokens) => result?,
            };
            if properties.usage_prompt_cumulative {
                norm.set_prompt_cumulative_baseline(prior_tokens.unwrap_or((0, 0, 0, 0, 0)));
            }
            if !opened.image && req.prompt.iter().any(|p| p["type"] == "image") {
                return Err(OneShotError::Unsupported("images"));
            }
            let mut prompt = req.prompt.clone();
            if let Some(head) = req.head.filter(|_| !opened.system_prompt) {
                prompt.insert(0, json!({"type": "text", "text": head}));
            }
            out.session = Some(opened.acp_id.clone());
            sampler = Some(tokio::select! {
                biased;
                _ = &mut cancel => return Err(failed("cancelled")),
                source = Sampler::start_source(home, &opened.acp_id, req.prefix, meter) => source,
            });
            tokio::select! { biased; _ = &mut cancel => return Err(failed("cancelled")), _ = std::future::ready(()) => {} }
            let prompting = conn.request(
                "session/prompt",
                json!({"sessionId": opened.acp_id, "prompt": prompt}),
                None,
            );
            let end = drive_prompt(
                &conn,
                &mut rx,
                prompting,
                &mut norm,
                &mut out,
                sampler.as_mut().expect("started"),
                &req,
                on_event,
                cancel.as_mut(),
            )
            .await?;
            Ok((opened.acp_id, end))
        };
        tokio::time::timeout(timeout, work)
            .await
            .unwrap_or_else(|_| Err(OneShotError::Timeout(timeout)))
    };
    if (matches!(
        result,
        Err(OneShotError::Timeout(_) | OneShotError::OutputLimit | OneShotError::Failed(_))
    ) || limit_hit.load(Ordering::Relaxed))
        && let Some(owned) = &mut owned
    {
        // Do not leave execution running during the sampling window, even when the adapter
        // ignores TERM. This handle addresses only the process group created for this call.
        stop_adapter(owned).await;
    }
    if let Some(sampler) = &mut sampler {
        let native = result
            .as_ref()
            .ok()
            .map(|(_, end)| norm.prompt_response(end))
            .unwrap_or_default();
        out.events(norm.finish(), on_event);
        out.events(sampler.finish(native, SETTLE_WAIT).await, on_event);
    }
    if let Some(owned) = &mut owned {
        stop_adapter(owned).await;
    }
    job_outcome(result, out, &limit_hit)
}

struct SpawnedJob {
    conn: Arc<Conn>,
    rx: mpsc::UnboundedReceiver<Incoming>,
    home: Option<crate::usage::Source>,
}

fn spawn_job(
    req: &Request<'_>,
    on_spawn: &(dyn Fn(u32) -> bool + Send + Sync),
    admitted_limit: Arc<AtomicBool>,
) -> Result<SpawnedJob, OneShotError> {
    let (mut program, mut args) = req.cfg.adapter_command(req.harness).map_err(failed)?;
    let mut env = req.cfg.adapter_env(req.harness);
    if let Some(wrap) = req.wrap {
        (program, args) = wrap((program, args), &mut env);
    }
    let home = crate::usage::source(req.cfg, req.harness, &env, req.cwd);
    let (tx, rx) = mpsc::unbounded_channel();
    let bytes = AtomicU64::new(0);
    let max_output_bytes = req.max_output_bytes;
    let conn = Conn::spawn_registered(
        Spawn {
            program,
            args,
            cwd: req.cwd.into(),
            env,
            log_tag: format!("[job {}]", req.prefix),
            clean_env: false,
            prefix: req.cfg.adapter_prefix(req.harness),
        },
        1 << 20,
        move |n| {
            let allowed = bytes
                .fetch_add(n as u64, Ordering::Relaxed)
                .saturating_add(n as u64)
                <= max_output_bytes;
            if !allowed {
                admitted_limit.store(true, Ordering::Relaxed);
            }
            allowed
        },
        |pid| {
            if on_spawn(pid) {
                Ok(())
            } else {
                Err(RpcError {
                    message: "job process registration failed".into(),
                    data: None,
                })
            }
        },
        move |_, inc| {
            let _ = tx.send(inc);
            None
        },
        || {},
    )
    .map_err(failed)?;
    Ok(SpawnedJob { conn, rx, home })
}

async fn open_job_session(
    conn: &Conn,
    req: &Request<'_>,
    rx: &mut mpsc::UnboundedReceiver<Incoming>,
    prior_tokens: &mut Option<(u64, u64, u64, u64, u64)>,
) -> Result<session::Opened, OneShotError> {
    let cwd = req.cwd.to_string_lossy();
    let reports = SessionReports(req.on_session);
    let opening = session::open(
        conn,
        OpenConfig {
            client_version: &req.cfg.version,
            cwd: &cwd,
            resume: req.resume,
            mcp_servers: req.mcp_servers.clone(),
            mode: Some(req.cfg.mode_choice(req.harness, req.mode)),
            model: req.model,
            effort: req.effort,
            system_prompt: req.head.map(|text| session::SystemPrompt {
                text,
                replace: req.tools_off,
            }),
        },
        req.cfg
            .session_meta_with_mode(req.harness, req.tools_off, Some(req.mode)),
        &reports,
    );
    drive_open(conn, rx, opening, req.on_activity, prior_tokens).await
}

fn job_outcome(
    result: Result<(String, Value), OneShotError>,
    out: Output,
    limit_hit: &AtomicBool,
) -> Result<Outcome, OneShotError> {
    if limit_hit.load(Ordering::Relaxed) {
        return Err(OneShotError::OutputLimit);
    }
    let (session, end) = result?;
    Ok(Outcome {
        text: out.text,
        usage: out.usage,
        session,
        stop_reason: end["stopReason"].as_str().unwrap_or("end_turn").into(),
    })
}

/// Bound process shutdown separately from the sampler's three-second tail window. KILL the
/// remaining group even if its leader exited on TERM: a descendant must not keep doing work.
async fn stop_adapter(owned: &mut Owned) {
    if owned.1 {
        return;
    }
    owned.0.kill();
    let _ = tokio::time::timeout(Duration::from_millis(500), owned.0.wait_exit()).await;
    owned.0.signal(libc::SIGKILL);
    owned.1 = true;
    let _ = tokio::time::timeout(Duration::from_secs(2), owned.0.wait_exit()).await;
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::Mutex;
    use std::time::Instant;

    fn adapter(root: &Path, body: &str, modes: bool) -> AcpConfig {
        let path = root.join("adapter.py");
        let mode = if modes {
            r#"{"availableModes":[{"id":"plan","name":"Plan"},{"id":"full-access","name":"Full"}],"currentModeId":"full-access"}"#
        } else {
            r#"{"availableModes":[{"id":"full-access","name":"Full"}],"currentModeId":"full-access"}"#
        };
        let script = format!(
            r#"#!/usr/bin/env python3
import json, sys, time
def send(v):
    print(json.dumps(v, ensure_ascii=False), flush=True)
def update(v):
    send({{"jsonrpc":"2.0","method":"session/update","params":{{"sessionId":"acp-session","update":v}}}})
def chunk(text):
    update({{"sessionUpdate":"agent_message_chunk","content":{{"type":"text","text":text}}}})
for line in sys.stdin:
    msg=json.loads(line)
    method=msg.get("method")
    if method=="initialize":
        import os
        caps={{"sessionCapabilities":{{"systemPrompt":{{"mode":{{"override":{{}},"append":{{}}}}}}}}}} if os.path.exists(os.path.join(os.path.dirname(__file__),"system-prompt")) else {{}}
        result={{"protocolVersion":1,"agentCapabilities":caps}}
    elif method in ("session/new","session/load"):
        opened_params=msg["params"]
        if method=="session/load":
            for _ in range(4000):
                chunk("old-history-marker")
        result={{"sessionId":"acp-session","modes":{mode}}}
    elif method=="session/set_mode":
        assert msg["params"]["modeId"]=="plan"
        result={{}}
    elif method=="session/prompt":
{body}
        result={{"stopReason":"end_turn","usage":{{"inputTokens":11,"outputTokens":22}}}}
    else:
        result={{}}
    if "id" in msg:
        send({{"jsonrpc":"2.0","id":msg["id"],"result":result}})
"#
        );
        std::fs::write(&path, script).unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o700)).unwrap();
        AcpConfig {
            declared: Default::default(),
            adapters: HashMap::from([("neutral-test".into(), path)]),
            node: "node".into(),
            claude_executable: "unused".into(),
            steve_home: root.into(),
            codex_home: root.into(),
            user_home: root.into(),
            version: "test".into(),
        }
    }

    fn request<'a>(
        cfg: &'a AcpConfig,
        root: &'a Path,
        allow: &'a (dyn Fn(&ToolFact) -> bool + Send + Sync),
    ) -> Request<'a> {
        Request {
            cfg,
            harness: "neutral-test",
            cwd: root,
            model: None,
            effort: None,
            resume: None,
            head: None,
            tools_off: true,
            mode: PermissionMode::Plan,
            prompt: vec![json!({"type":"text","text":"answer"})],
            mcp_servers: json!([]),
            wrap: None,
            timeout: Duration::from_secs(5),
            prefix: "attempt_test",
            on_session: None,
            max_output_bytes: 16 << 20,
            on_activity: None,
            allow_tool: allow,
        }
    }

    #[tokio::test]
    async fn neutral_adapter_streams_before_prompt_finishes() {
        let temp = tempfile::tempdir().unwrap();
        let cfg = adapter(
            temp.path(),
            "        update({'sessionUpdate':'agent_thought_chunk','content':{'type':'text','text':'private-thought-marker'}})\n        chunk('第一段')\n        time.sleep(0.2)\n        chunk('，第二段')",
            true,
        );
        let seen = Mutex::new(vec![]);
        let event = |ev| {
            if let EventBody::TextDelta { text, .. } = ev {
                seen.lock().unwrap().push((text, Instant::now()));
            }
        };
        let out = run(request(&cfg, temp.path(), &|_| false), &|_| true, &event)
            .await
            .unwrap();
        assert_eq!(out.text, "第一段，第二段");
        assert_eq!(out.session, "acp-session");
        assert_eq!(out.usage.len(), 1);
        assert_eq!(out.usage[0].tokens.input, Some(11));
        assert_eq!(out.usage[0].tokens.output, Some(22));
        let seen = seen.lock().unwrap();
        assert_eq!(seen.len(), 2);
        assert_eq!(seen[0].0, "第一段");
        assert!(seen[1].1.duration_since(seen[0].1) >= Duration::from_millis(150));
    }

    #[tokio::test]
    async fn jobs_keep_the_nodes_secret_file_denials_on_new_and_resumed_sessions() {
        let temp = tempfile::tempdir().unwrap();
        let mut cfg = adapter(
            temp.path(),
            "        chunk(json.dumps(opened_params.get('_meta')))",
            true,
        );
        let peer = cfg.adapters["neutral-test"].clone();
        cfg.adapters.insert("claude-code".into(), peer);
        for resume in [None, Some("acp-session")] {
            let mut req = request(&cfg, temp.path(), &|_| false);
            req.harness = "claude-code";
            req.resume = resume;
            let out = run(req, &|_| true, &|_| {}).await.unwrap();
            let meta: Value = serde_json::from_str(&out.text).unwrap();
            assert_eq!(Some(meta), cfg.session_meta_for("claude-code", true));
        }
    }

    #[tokio::test]
    async fn a_tool_without_permission_is_refused_before_the_answer() {
        let temp = tempfile::tempdir().unwrap();
        let cfg = adapter(
            temp.path(),
            "        update({'sessionUpdate':'tool_call','toolCallId':'t','kind':'edit','title':'write','rawInput':{'path':'file'}})\n        time.sleep(1)\n        chunk('must not appear')",
            true,
        );
        let seen = Mutex::new(vec![]);
        let out = run(request(&cfg, temp.path(), &|_| false), &|_| true, &|ev| {
            seen.lock().unwrap().push(ev)
        })
        .await;
        assert!(matches!(out, Err(OneShotError::ToolRefused)));
        assert!(
            !seen
                .lock()
                .unwrap()
                .iter()
                .any(|e| matches!(e, EventBody::TextDelta { .. }))
        );
    }

    #[tokio::test]
    async fn a_read_waits_for_streamed_arguments_then_checks_the_complete_pending_input() {
        let temp = tempfile::tempdir().unwrap();
        // Shapes from the bundled adapter: toolCall(inputComplete:false),
        // partialRefinement(offset), refinement(file_path), then its result.
        let cfg = adapter(
            temp.path(),
            r#"        update({'sessionUpdate':'tool_call','toolCallId':'read','name':'Read','kind':'read','status':'pending','rawInput':{},'locations':[]})
        update({'sessionUpdate':'tool_call_update','toolCallId':'read','rawInput':{'offset':1},'locations':[]})
        update({'sessionUpdate':'tool_call_update','toolCallId':'read','rawInput':{'file_path':'allowed.txt'},'locations':[{'path':'allowed.txt'}]})
        update({'sessionUpdate':'tool_call_update','toolCallId':'read','status':'completed'})
        chunk('read answer')"#,
            true,
        );
        let checked = Mutex::new(vec![]);
        let allow = |fact: &ToolFact| {
            checked.lock().unwrap().push(fact.phase);
            fact.category == ToolCategory::Read && fact.paths == ["allowed.txt"]
        };
        let out = run(request(&cfg, temp.path(), &allow), &|_| true, &|_| {})
            .await
            .unwrap();
        assert_eq!(out.text, "read answer");
        assert_eq!(
            *checked.lock().unwrap(),
            [ToolPhase::Pending, ToolPhase::Succeeded],
            "check the complete input before execution, not merely the final result"
        );
    }

    #[tokio::test]
    async fn a_pending_read_is_refused_as_soon_as_its_forbidden_path_arrives() {
        let temp = tempfile::tempdir().unwrap();
        let cfg = adapter(
            temp.path(),
            r#"        update({'sessionUpdate':'tool_call','toolCallId':'read','kind':'read','status':'pending','rawInput':{}})
        update({'sessionUpdate':'tool_call_update','toolCallId':'read','rawInput':{'file_path':'outside.txt'}})
        time.sleep(0.2)
        chunk('must not appear')"#,
            true,
        );
        let checked = Mutex::new(vec![]);
        let allow = |fact: &ToolFact| {
            checked.lock().unwrap().push(fact.paths.clone());
            fact.paths == ["allowed.txt"]
        };
        let out = run(request(&cfg, temp.path(), &allow), &|_| true, &|_| {}).await;
        assert!(matches!(out, Err(OneShotError::ToolRefused)));
        assert_eq!(*checked.lock().unwrap(), [vec!["outside.txt".to_string()]]);
    }

    #[tokio::test]
    async fn missing_arguments_never_authorize_a_running_tool_or_a_permission_request() {
        for body in [
            r#"        update({'sessionUpdate':'tool_call','toolCallId':'read','kind':'read','status':'in_progress','rawInput':{}})
        time.sleep(0.2)
        chunk('must not appear')"#,
            r#"        send({'jsonrpc':'2.0','id':'permission','method':'session/request_permission','params':{'sessionId':'acp-session','toolCall':{'toolCallId':'read','kind':'read','status':'pending','rawInput':{}},'options':[{'optionId':'yes','kind':'allow_once'},{'optionId':'no','kind':'reject_once'}]}})
        time.sleep(0.2)
        chunk('must not appear')"#,
        ] {
            let temp = tempfile::tempdir().unwrap();
            let cfg = adapter(temp.path(), body, true);
            let checked = AtomicU64::new(0);
            let allow = |_: &ToolFact| {
                checked.fetch_add(1, Ordering::Relaxed);
                false
            };
            let out = run(request(&cfg, temp.path(), &allow), &|_| true, &|_| {}).await;
            assert!(matches!(out, Err(OneShotError::ToolRefused)));
            assert_eq!(checked.load(Ordering::Relaxed), 1);
        }
    }

    #[tokio::test]
    async fn an_adapter_cannot_inherit_a_permissive_mode() {
        let temp = tempfile::tempdir().unwrap();
        let cfg = adapter(temp.path(), "        chunk('must not appear')", false);
        let out = run(request(&cfg, temp.path(), &|_| false), &|_| true, &|_| {}).await;
        assert!(
            matches!(out, Err(OneShotError::Failed(e)) if e.contains("unsupported: permission mode plan"))
        );
    }

    #[tokio::test]
    async fn a_resumed_session_does_not_replay_history_as_a_new_draft() {
        let temp = tempfile::tempdir().unwrap();
        let cfg = adapter(temp.path(), "        chunk('new answer')", true);
        let mut req = request(&cfg, temp.path(), &|_| false);
        req.resume = Some("acp-session");
        let seen = Mutex::new(vec![]);
        let out = run(req, &|_| true, &|event| {
            if let EventBody::TextDelta { text, .. } = event {
                seen.lock().unwrap().push(text);
            }
        })
        .await
        .unwrap();
        assert_eq!(out.text, "new answer");
        assert_eq!(*seen.lock().unwrap(), vec!["new answer"]);
    }

    #[tokio::test]
    async fn a_large_burst_is_streamed_without_a_chunk_count_limit() {
        let temp = tempfile::tempdir().unwrap();
        let cfg = adapter(
            temp.path(),
            "        for _ in range(4000): chunk('x')",
            true,
        );
        let chunks = AtomicU64::new(0);
        let out = run(
            request(&cfg, temp.path(), &|_| false),
            &|_| true,
            &|event| {
                if matches!(event, EventBody::TextDelta { .. }) {
                    chunks.fetch_add(1, Ordering::Relaxed);
                }
            },
        )
        .await
        .unwrap();
        assert_eq!(chunks.load(Ordering::Relaxed), 4000);
        assert_eq!(out.text, "x".repeat(4000));
    }

    #[tokio::test]
    async fn private_thoughts_refresh_activity_without_becoming_drafts() {
        let temp = tempfile::tempdir().unwrap();
        let cfg = adapter(
            temp.path(),
            "        update({'sessionUpdate':'agent_thought_chunk','content':{'type':'text','text':'private'}})\n        chunk('answer')",
            true,
        );
        let activity = AtomicU64::new(0);
        let on_activity = || {
            activity.fetch_add(1, Ordering::Relaxed);
        };
        let mut req = request(&cfg, temp.path(), &|_| false);
        req.on_activity = Some(&on_activity);
        let out = run(req, &|_| true, &|_| {}).await.unwrap();
        assert_eq!(out.text, "answer");
        assert_eq!(activity.load(Ordering::Relaxed), 2);
    }

    #[tokio::test]
    async fn an_ingress_budget_hit_keeps_its_typed_stop_reason() {
        let temp = tempfile::tempdir().unwrap();
        let cfg = adapter(
            temp.path(),
            "        for _ in range(4000): chunk('x')",
            true,
        );
        let mut req = request(&cfg, temp.path(), &|_| false);
        req.max_output_bytes = 4096;
        let out = run(req, &|_| true, &|_| {}).await;
        assert!(matches!(out, Err(OneShotError::OutputLimit)));
    }

    #[tokio::test]
    async fn an_error_answer_is_the_harness_error_and_says_whether_it_may_pass() {
        let temp = tempfile::tempdir().unwrap();
        for (message, transient) in [
            ("API Error: 529 overloaded", true),
            ("invalid model", false),
        ] {
            let cfg = adapter(
                temp.path(),
                &format!(
                    "        send({{'jsonrpc':'2.0','id':msg['id'],'error':{{'code':-32603,'message':'{message}'}}}})\n        continue"
                ),
                true,
            );
            let out = run(request(&cfg, temp.path(), &|_| false), &|_| true, &|_| {}).await;
            match out {
                Err(OneShotError::Harness {
                    text,
                    code,
                    transient: t,
                }) => {
                    assert!(text.contains(message), "{text}");
                    assert_eq!(code, Some(-32603));
                    assert_eq!(t, transient, "{message}");
                }
                other => panic!("{:?}", other.err()),
            }
        }
    }

    #[tokio::test]
    async fn images_go_only_to_an_adapter_that_takes_them() {
        let temp = tempfile::tempdir().unwrap();
        let cfg = adapter(temp.path(), "        chunk('saw it')", true);
        let image = Image {
            mime: "image/png".into(),
            bytes: vec![1, 2, 3],
        };
        let mut req = request(&cfg, temp.path(), &|_| false);
        req.prompt.push(image.block());
        let out = run(req, &|_| true, &|_| {}).await;
        assert!(matches!(out, Err(OneShotError::Unsupported("images"))));
    }

    #[tokio::test]
    async fn the_stop_reason_is_the_adapters() {
        let temp = tempfile::tempdir().unwrap();
        let cfg = adapter(
            temp.path(),
            "        chunk('cut')\n        send({'jsonrpc':'2.0','id':msg['id'],'result':{'stopReason':'max_tokens'}})\n        continue",
            true,
        );
        let out = run(request(&cfg, temp.path(), &|_| false), &|_| true, &|_| {})
            .await
            .unwrap();
        assert_eq!(out.stop_reason, "max_tokens");
        assert_eq!(out.text, "cut");
    }

    #[tokio::test]
    async fn the_head_is_the_system_prompt_where_the_adapter_takes_one() {
        let temp = tempfile::tempdir().unwrap();
        let cfg = adapter(
            temp.path(),
            "        chunk(json.dumps([opened_params.get('systemPrompt'), msg['params']['prompt'][0]['text']]))",
            true,
        );
        let root = temp.path();
        let said = async |tools_off| {
            let mut req = request(&cfg, root, &|_| false);
            req.head = Some("rules");
            req.tools_off = tools_off;
            let out = run(req, &|_| true, &|_| {}).await.unwrap();
            serde_json::from_str::<Value>(&out.text).unwrap()
        };
        // plain ACP: the head is the prompt's first block
        assert_eq!(said(true).await, json!([null, "rules"]));
        std::fs::write(root.join("system-prompt"), "").unwrap();
        // advertised: in place of the harness's own without tools, after it with them
        assert_eq!(
            said(true).await,
            json!([{"mode": "override", "content": [{"type": "text", "text": "rules"}]}, "answer"])
        );
        assert_eq!(said(false).await[0]["mode"], "append");
    }

    #[tokio::test]
    async fn usage_preceding_a_prompt_error_is_kept_even_when_response_wins_the_poll() {
        let temp = tempfile::tempdir().unwrap();
        let cfg = adapter(temp.path(), "", true);
        let req = request(&cfg, temp.path(), &|_| false);
        let conn = Conn::spawn_registered(
            Spawn {
                program: "/bin/cat".into(),
                args: vec![],
                cwd: temp.path().into(),
                env: vec![],
                log_tag: "[test]".into(),
                clean_env: false,
                prefix: None,
            },
            1 << 20,
            |_| true,
            |_| Ok(()),
            |_, _| None,
            || {},
        )
        .unwrap();
        let _owned = Owned(conn.clone(), false);
        let (tx, mut rx) = mpsc::unbounded_channel();
        let meter = Epoch::new("attempt_test.acp").unwrap();
        let mut norm = Normalizer::new(temp.path(), "attempt_test", meter);
        let mut out = Output::default();
        let seen = Mutex::new(vec![]);
        let on_event = |ev: EventBody| {
            if let EventBody::Usage { report } = ev {
                seen.lock().unwrap().push(report);
            }
        };
        let mut sent = false;
        let prompting = std::future::poll_fn(|_| {
            if !sent {
                sent = true;
                // Reproduces the exact reader-task interleaving on 2-vCPU Linux CI:
                // `biased` already polled `rx.recv()` (`Pending`), and before `prompting`
                // finishes polling `oneshot_rx`, stdout delivers `usage_update` ($0.01)
                // followed immediately by the JSON-RPC `error` (`503 overloaded`).
                let _ = tx.send(Incoming::Notification {
                    method: "session/update".into(),
                    params: json!({
                        "sessionId": "s1",
                        "update": {
                            "sessionUpdate": "usage_update",
                            "used": 4,
                            "size": 100,
                            "cost": { "amount": 0.01, "currency": "USD" }
                        }
                    }),
                });
            }
            std::task::Poll::Ready(Err(RpcError {
                message: "API Error: 503 overloaded".into(),
                data: None,
            }))
        });
        let mut sampler =
            Sampler::start_source(None, "s1", "test", Epoch::new("test").unwrap()).await;
        let err = drive_prompt(
            &conn,
            &mut rx,
            prompting,
            &mut norm,
            &mut out,
            &mut sampler,
            &req,
            &on_event,
            std::pin::pin!(std::future::pending()).as_mut(),
        )
        .await
        .expect_err("prompt failed with 503");
        assert!(err.transient(), "{err:?}");
        let usage = seen.lock().unwrap();
        assert_eq!(
            usage.len(),
            1,
            "failed try's usage_update must be drained before returning the harness error"
        );
    }

    #[tokio::test]
    async fn replayed_history_at_the_tail_of_open_never_leaks_into_the_prompt() {
        let temp = tempfile::tempdir().unwrap();
        let conn = Conn::spawn_registered(
            Spawn {
                program: "/bin/cat".into(),
                args: vec![],
                cwd: temp.path().into(),
                env: vec![],
                log_tag: "[test]".into(),
                clean_env: false,
                prefix: None,
            },
            1 << 20,
            |_| true,
            |_| Ok(()),
            |_, _| None,
            || {},
        )
        .unwrap();
        let _owned = Owned(conn.clone(), false);
        let (tx, mut rx) = mpsc::unbounded_channel();
        let activity = AtomicU64::new(0);
        let on_activity = || {
            activity.fetch_add(1, Ordering::Relaxed);
        };
        let mut sent = false;
        let opening = std::future::poll_fn(|_| {
            if !sent {
                sent = true;
                let _ = tx.send(Incoming::Notification {
                    method: "session/update".into(),
                    params: json!({
                        "sessionId": "s1",
                        "update": {
                            "sessionUpdate": "agent_message_chunk",
                            "content": { "type": "text", "text": "old-history-marker" }
                        }
                    }),
                });
            }
            std::task::Poll::Ready(Ok::<_, &'static str>("s1"))
        });
        let mut prior_tokens = None;
        let sid = drive_open(
            &conn,
            &mut rx,
            opening,
            Some(&on_activity),
            &mut prior_tokens,
        )
        .await
        .unwrap();
        assert_eq!(sid, "s1");
        assert_eq!(activity.load(Ordering::Relaxed), 1);
        assert!(
            rx.try_recv().is_err(),
            "open must drain queued replay updates"
        );
    }

    #[tokio::test]
    async fn an_error_answer_after_a_usage_update_still_delivers_that_usage() {
        let temp = tempfile::tempdir().unwrap();
        let cfg = adapter(
            temp.path(),
            r#"        update({'sessionUpdate':'usage_update','used':4,'size':100,'cost':{'amount':0.01,'currency':'USD'}})
        send({'jsonrpc':'2.0','id':msg['id'],'error':{'code':-32603,'message':'API Error: 503 overloaded'}})
        continue"#,
            true,
        );
        let usage = Mutex::new(vec![]);
        let err = run(request(&cfg, temp.path(), &|_| false), &|_| true, &|ev| {
            if let EventBody::Usage { report } = ev {
                usage.lock().unwrap().push(report);
            }
        })
        .await
        .err()
        .expect("503 error");
        assert!(err.transient(), "{err:?}");
        let usage = usage.lock().unwrap();
        assert_eq!(usage.len(), 2);
        assert_eq!(usage[0].cost.as_ref().unwrap().decimal, "0.01");
        assert_eq!(usage[1].measurement, steve_node_api::Measurement::Final);
        assert_eq!(usage[1].tokens, steve_node_api::Tokens::default());
    }

    #[tokio::test]
    async fn dropping_a_pending_job_stops_its_registered_process() {
        let temp = tempfile::tempdir().unwrap();
        let cfg = adapter(temp.path(), "        time.sleep(10)", true);
        let pid = Mutex::new(None);
        let spawned = |p| {
            *pid.lock().unwrap() = Some(p);
            true
        };
        let pending = run(request(&cfg, temp.path(), &|_| false), &spawned, &|_| {});
        assert!(
            tokio::time::timeout(Duration::from_millis(150), pending)
                .await
                .is_err()
        );
        let pid = pid.lock().unwrap().unwrap();
        for _ in 0..50 {
            if unsafe { libc::kill(pid as i32, 0) } != 0 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!("owned adapter did not stop after its job was dropped");
    }

    #[tokio::test]
    async fn ready_cancel_never_spawns_and_session_stop_never_prompts() {
        let temp = tempfile::tempdir().unwrap();
        let cfg = adapter(temp.path(), "        chunk(\"must not run\")", true);
        let spawn_count = AtomicU64::new(0);
        let spawned = |_: u32| {
            spawn_count.fetch_add(1, Ordering::SeqCst);
            true
        };
        let allow = |_: &ToolFact| -> bool { panic!("stopped work must not authorize tools") };
        let event = |_: EventBody| {};
        let result = run_cancellable(
            request(&cfg, temp.path(), &allow),
            &spawned,
            &event,
            std::future::ready(()),
        )
        .await;
        assert!(matches!(result, Err(OneShotError::Failed(_))));
        assert_eq!(spawn_count.load(Ordering::SeqCst), 0);

        let stop = tokio::sync::Notify::new();
        let session = |_: &str| stop.notify_one();
        let mut req = request(&cfg, temp.path(), &allow);
        req.on_session = Some(&session);
        let text = Mutex::new(String::new());
        let on = |body: EventBody| {
            if let EventBody::TextDelta { text: delta, .. } = body {
                text.lock().unwrap().push_str(&delta);
            }
        };
        let result = run_cancellable(req, &spawned, &on, stop.notified()).await;
        assert!(matches!(result, Err(OneShotError::Failed(_))));
        assert_eq!(spawn_count.load(Ordering::SeqCst), 1);
        assert!(
            text.lock().unwrap().is_empty(),
            "cancellation during open must prevent session/prompt"
        );
    }

    #[tokio::test]
    async fn cancellation_is_checked_before_each_queued_permission() {
        let temp = tempfile::tempdir().unwrap();
        let cfg = adapter(
            temp.path(),
            r#"        for n in range(100):
            send({"jsonrpc":"2.0","id":n+1000,"method":"session/request_permission","params":{
                "toolCall":{"toolCallId":str(n),"kind":"read","status":"pending","rawInput":{"file_path":"file.txt"}},
                "options":[{"optionId":"yes","kind":"allow_once","name":"Allow"}]}})
        time.sleep(60)"#,
            true,
        );
        let stop = tokio::sync::Notify::new();
        let permissions = AtomicU64::new(0);
        let allow = |_: &ToolFact| {
            permissions.fetch_add(1, Ordering::SeqCst);
            stop.notify_one();
            true
        };
        let result = run_cancellable(
            request(&cfg, temp.path(), &allow),
            &|_| true,
            &|_| {},
            stop.notified(),
        )
        .await;
        assert!(matches!(result, Err(OneShotError::Failed(_))));
        assert_eq!(
            permissions.load(Ordering::SeqCst),
            1,
            "only the permission preceding the stop can run"
        );
    }

    #[tokio::test]
    async fn output_admission_stop_reaps_producer_before_final_callback() {
        let temp = tempfile::tempdir().unwrap();
        let cfg = adapter(
            temp.path(),
            r#"        import signal
        signal.signal(signal.SIGTERM, signal.SIG_IGN)
        chunk("x" * 8000)
        time.sleep(60)"#,
            true,
        );
        let pid = AtomicU64::new(0);
        let spawned = |p: u32| {
            pid.store(p as u64, Ordering::SeqCst);
            true
        };
        let final_seen = AtomicBool::new(false);
        let event = |event: EventBody| {
            if matches!(event, EventBody::Usage { report } if report.measurement == steve_node_api::Measurement::Final)
            {
                let p = pid.load(Ordering::SeqCst) as i32;
                assert!(p > 0);
                assert_eq!(
                    unsafe { libc::kill(p, 0) },
                    -1,
                    "a budget stop must reap execution before emitting Final"
                );
                final_seen.store(true, Ordering::SeqCst);
            }
        };
        let allow = |_: &ToolFact| true;
        let mut req = request(&cfg, temp.path(), &allow);
        req.max_output_bytes = 4000;
        assert!(matches!(
            run(req, &spawned, &event).await,
            Err(OneShotError::OutputLimit)
        ));
        assert!(final_seen.load(Ordering::SeqCst));
    }
}
