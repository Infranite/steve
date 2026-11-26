//! One-shot ACP calls (harness parity S2): a single prompt on a fresh (or loaded) session of any
//! adapter, for think jobs, research and Steve's own model calls. Every adapter goes the same way:
//! `initialize`, `session/new|load` in a read-only mode the adapter offers, the model and effort as
//! ACP config options, `session/prompt`, its chunks streamed as Steve's events, and the usage it
//! reported. One-shot calls rely on the OS read-only sandbox, `PermissionMode::Plan`, and
//! `tools_off`; any `session/request_permission` is unconditionally rejected so interactive
//! session approvals remain on the single Hub `PermissionResolver` port.
//!
//! The caller owns the process (its journal, its sandbox through [`Request::wrap`]) and the job's
//! deadline and cancellation policy. Use [`run_cancellable`] and await its result to settle usage
//! after stopping; dropping either future is emergency process cleanup without a usage receipt.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::AcpConfig;
use crate::normalize::{Fact, Normalizer};
use crate::permission;
use crate::rpc::{Conn, Incoming, RpcError, Spawn};
use crate::session::{self, OpenConfig, Reports};
use crate::usage::{POLL_INTERVAL, SETTLE_WAIT, Sampler};
use serde_json::{Value, json};

use steve_node_api::{
    Epoch, EventBody, GatewayPhase, PermissionDecision, PermissionMode, Role, TimeoutPhase,
    ToolCategory, UsageReport,
};
use tokio::sync::mpsc;

/// Why a one-shot call failed. The words a user sees are the caller's.
#[derive(Debug, thiserror::Error)]
pub enum OneShotError {
    /// The accepted run's output/ingress budget was exhausted.
    #[error("output limit")]
    OutputLimit,
    /// Still running after the timeout; the adapter is stopped.
    #[error("timed out in {0}")]
    Timeout(TimeoutPhase),
    /// The adapter could not be started or opened.
    #[error("{0}")]
    Failed(String),
    /// The model reached for a tool the call's policy does not allow (a call without tools, or
    /// with a narrower set than it wanted): the request needs what this call cannot do, and the
    /// caller decides where it goes instead.
    #[error("read-only policy refused a tool")]
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
    pub effort: steve_harness_spec::ReasoningEffort,
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
            self.0.signal(libc::SIGKILL);
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

fn tool_category_label(category: ToolCategory) -> &'static str {
    match category {
        ToolCategory::Read => "read",
        ToolCategory::Edit => "edit",
        ToolCategory::Delete => "delete",
        ToolCategory::Move => "move",
        ToolCategory::Search => "search",
        ToolCategory::Exec => "exec",
        ToolCategory::Think => "think",
        ToolCategory::Fetch => "fetch",
        ToolCategory::Steve => "steve",
        ToolCategory::Other => "other",
    }
}

/// Monotonic evidence; a meaningful reply also proves submission if stdout reached us
/// before the writer's acknowledgement was polled.
#[derive(Default)]
struct Progress(Mutex<Option<GatewayPhase>>);

impl Progress {
    fn advance(&self, next: GatewayPhase, emit: &(dyn Fn(EventBody) + Send + Sync)) {
        let mut phase = self.0.lock().unwrap();
        for step in [
            GatewayPhase::Submitting,
            GatewayPhase::Submitted,
            GatewayPhase::Responding,
        ] {
            if step <= next && phase.is_none_or(|last| last < step) {
                *phase = Some(step);
                emit(EventBody::GatewayProgress { phase: step });
            }
        }
    }

    fn started(&self) -> bool {
        self.0.lock().unwrap().is_some()
    }

    fn timeout(&self) -> OneShotError {
        OneShotError::Timeout(GatewayPhase::timeout(
            *self.0.lock().unwrap(),
            TimeoutPhase::Queue,
        ))
    }
}

fn is_response(fact: &Fact) -> bool {
    match fact {
        Fact::Event(EventBody::TextDelta { text, .. }) => !text.is_empty(),
        Fact::Event(EventBody::ToolChanged { .. })
        | Fact::Event(EventBody::SubtaskChanged { .. }) => true,
        Fact::Event(EventBody::Plan { entries }) => !entries.is_empty(),
        // Usage/diagnostics can come from an independent sampler, and some adapters issue
        // empty/session-status notifications without returning any model work.
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
    progress: &Progress,
) -> Result<(), OneShotError> {
    // A trailing setup/history event cannot establish that a prompt was submitted or
    // authorize a tool before that prompt. The caller drives this inbox before the writer.
    if !progress.started() {
        if let Incoming::Request { id, method, .. } = inc {
            if method == "session/request_permission" {
                conn.respond(id, json!({"outcome":{"outcome":"cancelled"}}));
            } else {
                conn.respond_error(id, -32601, "client capability is unavailable");
            }
        }
        return Ok(());
    }
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
                if facts.iter().any(is_response) {
                    progress.advance(GatewayPhase::Responding, on_event);
                }
                out.events(facts, on_event);
            }
        }
        Incoming::Request { id, method, params } if method == "session/request_permission" => {
            if let Some(sid) = params.get("sessionId").and_then(Value::as_str)
                && out.session.as_deref() != Some(sid)
            {
                conn.respond(id, json!({"outcome":{"outcome":"cancelled"}}));
                return Ok(());
            }
            progress.advance(GatewayPhase::Responding, on_event);
            let asked = permission::asked(norm, &id, &params);
            let category = tool_category_label(asked.fact.category);
            metrics::counter!(
                "steve_permission_refused_total",
                "surface" => "oneshot",
                "harness" => req.harness.to_string(),
                "category" => category,
            )
            .increment(1);
            tracing::warn!(
                harness = req.harness,
                prefix = req.prefix,
                tool = %asked.fact.name,
                category,
                "oneshot refused session/request_permission"
            );
            conn.respond(
                id,
                permission::answer(&asked, Some(PermissionDecision::Reject)),
            );
            return Err(OneShotError::ToolRefused);
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
    progress: &Progress,
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
            Some(inc) = rx.recv() => incoming(conn, inc, norm, out, req, on_event, progress)?,
            result = &mut prompting => break result,
            _ = tick.tick() => out.events(sampler.poll().await, on_event),
        }
    };
    while let Ok(inc) = rx.try_recv() {
        tokio::select! {
            biased;
            _ = &mut cancel => return Err(failed("cancelled")),
            _ = std::future::ready(()) => incoming(conn, inc, norm, out, req, on_event, progress)?,
        }
    }
    if result.is_ok() || result.as_ref().is_err_and(|e| e.data.is_some()) {
        // A non-streaming adapter can answer with a terminal result as its first valid byte.
        // A local read/write error has no JSON-RPC response and is not evidence of one.
        progress.advance(GatewayPhase::Responding, on_event);
    }
    out.events(norm.finish(), on_event);
    result.map_err(|e| {
        if e.data.is_some() {
            OneShotError::harness(e)
        } else {
            failed(e)
        }
    })
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
    let deadline = tokio::time::Instant::now() + req.timeout;
    let progress = Progress::default();
    let properties = req.cfg.adapter_properties(req.harness);
    let meter = Epoch::new(format!("{}.acp", req.prefix)).map_err(failed)?;
    let mut norm = Normalizer::new(req.cwd, req.prefix, meter.clone())
        .for_harness(req.harness)
        .input_includes_cache(properties.usage_input_includes_cache);
    let mut out = Output::default();
    let mut sampler = None;
    let mut owned = None;
    let mut inbox = None;
    let limit_hit = Arc::new(AtomicBool::new(false));
    let admitted_limit = limit_hit.clone();
    // Keep the sampler and process owner outside the timed future: a timeout must still stop
    // the producer, settle usage and call on_event before returning its error.
    let result = {
        let work = async {
            // Ready cancellation must not even spawn. Repeat the checkpoint before prompting,
            // because on_session can request a stop while the opening future is resolving.
            tokio::select! { biased; _ = &mut cancel => return Err(failed("cancelled")), _ = std::future::ready(()) => {} }
            if tokio::time::Instant::now() >= deadline {
                return Err(progress.timeout());
            }
            let SpawnedJob { conn, rx, home } = spawn_job(&req, on_spawn, admitted_limit)?;
            let rx = inbox.insert(rx);
            owned = Some(Owned(conn.clone(), false));
            let mut prior_tokens = None;
            let opened = tokio::select! {
                biased;
                _ = &mut cancel => return Err(failed("cancelled")),
                result = open_job_session(&conn, &req, rx, &mut prior_tokens) => result?,
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
            norm.set_root_session(&opened.acp_id);
            sampler = Some(tokio::select! {
                biased;
                _ = &mut cancel => return Err(failed("cancelled")),
                source = Sampler::start_source(home, &opened.acp_id, req.prefix, meter) => source,
            });
            tokio::select! { biased; _ = &mut cancel => return Err(failed("cancelled")), _ = std::future::ready(()) => {} }
            if tokio::time::Instant::now() >= deadline {
                return Err(progress.timeout());
            }
            let prompting = async {
                progress.advance(GatewayPhase::Submitting, on_event);
                let submitted = || progress.advance(GatewayPhase::Submitted, on_event);
                conn.request_submitted(
                    "session/prompt",
                    json!({"sessionId": opened.acp_id, "prompt": prompt}),
                    &submitted,
                )
                .await
            };
            let end = drive_prompt(
                &conn,
                rx,
                prompting,
                &mut norm,
                &mut out,
                sampler.as_mut().expect("started"),
                &req,
                on_event,
                &progress,
                cancel.as_mut(),
            )
            .await?;
            Ok((opened.acp_id, end))
        };
        tokio::time::timeout_at(deadline, work)
            .await
            .unwrap_or_else(|_| Err(progress.timeout()))
    };
    let over_limit = limit_hit.load(Ordering::Relaxed);
    let keep_billing_tail = progress.started() && (result.is_err() || over_limit);
    let keep_draft_tail =
        keep_billing_tail && !over_limit && !matches!(result, Err(OneShotError::ToolRefused));
    let stop_before_settlement =
        over_limit || !matches!(result, Ok(_) | Err(OneShotError::Harness { .. }));
    if stop_before_settlement && let Some(owned) = &mut owned {
        // Do not leave execution running during the sampling window, even when the adapter
        // ignores TERM. A genuine terminal RPC error has already ended the prompt; allow its
        // bounded accounting tail, as for a successful terminal response.
        stop_adapter(owned).await;
    }
    if let Some(sampler) = &mut sampler {
        if keep_billing_tail && let Some(rx) = &mut inbox {
            drain_tail(rx, &mut norm, &mut out, on_event, keep_draft_tail);
        }
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
    if keep_billing_tail && let Some(rx) = &mut inbox {
        drain_tail(rx, &mut norm, &mut out, on_event, false);
    }
    job_outcome(result, out, &limit_hit)
}

/// Keep buffered accounting after failure, and already-emitted assistant draft text after an
/// external stop or timeout, without advancing the phase, granting tools, or emitting text that
/// follows an unchecked tool or permission event.
fn drain_tail(
    rx: &mut mpsc::UnboundedReceiver<Incoming>,
    norm: &mut Normalizer,
    out: &mut Output,
    on_event: &(dyn Fn(EventBody) + Send + Sync),
    mut keep_draft: bool,
) {
    while let Ok(inc) = rx.try_recv() {
        match inc {
            Incoming::Request { .. } => {
                keep_draft = false;
            }
            Incoming::Notification { method, params }
                if method == "session/update"
                    && out.session.as_deref()
                        == params.get("sessionId").and_then(Value::as_str) =>
            {
                let Some(update) = params.get("update") else {
                    continue;
                };
                let is_tool = matches!(
                    update.get("sessionUpdate").and_then(Value::as_str),
                    Some("tool_call" | "tool_call_update")
                );
                let allow_draft = keep_draft && !is_tool;
                let close_draft = keep_draft;
                if is_tool {
                    keep_draft = false;
                }
                let facts = norm
                    .update(update)
                    .into_iter()
                    .filter(|fact| match fact {
                        Fact::Event(EventBody::Usage { .. }) => true,
                        Fact::Event(
                            EventBody::MessageStarted { .. } | EventBody::TextDelta { .. },
                        ) => allow_draft,
                        Fact::Event(EventBody::MessageCompleted { .. }) => close_draft,
                        _ => false,
                    })
                    .collect();
                out.events(facts, on_event);
            }
            _ => {}
        }
    }
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
    let (mut program, mut args) = req
        .cfg
        .adapter_command_for_mode(req.harness, Some(req.mode))
        .map_err(failed)?;
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
            harness: req.harness,
            client_version: &req.cfg.version,
            cwd: &cwd,
            resume: req.resume,
            mcp_servers: req.mcp_servers.clone(),
            mode: Some(req.cfg.mode_choice(req.harness, req.mode)),
            model: req.model,
            effort: req.effort,
            effort_mode: req.cfg.adapter_properties(req.harness).effort_mode,
            system_prompt: req.head.map(|text| session::SystemPrompt {
                text,
                mode: if req.tools_off {
                    session::SystemPromptMode::AcpOverride
                } else {
                    session::SystemPromptMode::AcpAppend
                },
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
            claude_config_dir: None,
            steve_home: root.into(),
            codex_home: root.into(),
            user_home: root.into(),
            version: "test".into(),
        }
    }

    fn request<'a>(cfg: &'a AcpConfig, root: &'a Path) -> Request<'a> {
        Request {
            cfg,
            harness: "neutral-test",
            cwd: root,
            model: None,
            effort: steve_harness_spec::ReasoningEffort::Default,
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
        let out = run(request(&cfg, temp.path()), &|_| true, &event)
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
            let mut req = request(&cfg, temp.path());
            req.harness = "claude-code";
            req.resume = resume;
            let out = run(req, &|_| true, &|_| {}).await.unwrap();
            let meta: Value = serde_json::from_str(&out.text).unwrap();
            assert_eq!(Some(meta), cfg.session_meta_for("claude-code", true));
        }
    }

    #[tokio::test]
    async fn a_permission_request_is_always_refused_without_self_approving() {
        let temp = tempfile::tempdir().unwrap();
        let answer_path = temp.path().join("permission-answer.json");
        let cfg = adapter(
            temp.path(),
            &format!(
                r#"        send({{'jsonrpc':'2.0','id':'permission','method':'session/request_permission','params':{{'sessionId':'acp-session','toolCall':{{'toolCallId':'read','kind':'read','status':'pending','rawInput':{{'file_path':'allowed.txt'}}}},'options':[{{'optionId':'yes','kind':'allow_once'}},{{'optionId':'no','kind':'reject_once'}}]}}}})
        reply_line = sys.stdin.readline()
        if reply_line:
            with open({answer_path:?}, 'w') as f:
                f.write(reply_line)
        time.sleep(0.2)
        chunk('must not appear')"#,
                answer_path = answer_path.to_str().unwrap(),
            ),
            true,
        );
        let seen = Mutex::new(vec![]);
        let out = run(request(&cfg, temp.path()), &|_| true, &|ev| {
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
        if let Ok(raw) = std::fs::read_to_string(&answer_path) {
            let msg: Value = serde_json::from_str(&raw).unwrap();
            assert_eq!(msg["result"]["outcome"]["optionId"], "no");
        }
    }

    #[tokio::test]
    async fn an_adapter_cannot_inherit_a_permissive_mode() {
        let temp = tempfile::tempdir().unwrap();
        let cfg = adapter(temp.path(), "        chunk('must not appear')", false);
        let out = run(request(&cfg, temp.path()), &|_| true, &|_| {}).await;
        assert!(
            matches!(out, Err(OneShotError::Failed(e)) if e.contains("unsupported: permission mode plan"))
        );
    }

    #[tokio::test]
    async fn a_resumed_session_does_not_replay_history_as_a_new_draft() {
        let temp = tempfile::tempdir().unwrap();
        let cfg = adapter(temp.path(), "        chunk('new answer')", true);
        let mut req = request(&cfg, temp.path());
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
        let out = run(request(&cfg, temp.path()), &|_| true, &|event| {
            if matches!(event, EventBody::TextDelta { .. }) {
                chunks.fetch_add(1, Ordering::Relaxed);
            }
        })
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
        let mut req = request(&cfg, temp.path());
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
        let mut req = request(&cfg, temp.path());
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
            let out = run(request(&cfg, temp.path()), &|_| true, &|_| {}).await;
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
        let mut req = request(&cfg, temp.path());
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
        let out = run(request(&cfg, temp.path()), &|_| true, &|_| {})
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
            let mut req = request(&cfg, root);
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
    async fn system_prompt_append_accepts_object_cap_and_meta_append() {
        let temp = tempfile::tempdir().unwrap();
        let script_path = temp.path().join("cap-adapter.py");
        let script = r#"#!/usr/bin/env python3
import json, os, sys
mode = os.environ["CAP_MODE"]
opened = None
for line in sys.stdin:
    msg = json.loads(line)
    m = msg.get("method")
    if m == "initialize":
        if mode == "claude_shape":
            caps = {"sessionCapabilities": {"systemPrompt": {"mode": {"override": {}}}}}
        elif mode == "codex_shape":
            caps = {"sessionCapabilities": {"systemPrompt": {}}}
        elif mode == "agy_meta":
            caps = {"_meta": {"antigravity": {"systemPrompt": {"append": True}}}}
        else:
            caps = {}
        res = {"protocolVersion": 1, "agentCapabilities": caps}
    elif m == "session/new":
        opened = msg["params"]
        res = {"sessionId": "s1"}
    elif m == "session/prompt":
        print(json.dumps({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": "s1",
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": {"type": "text", "text": json.dumps(opened)}
                }
            }
        }), flush=True)
        res = {"stopReason": "end_turn"}
    else:
        res = {}
    if "id" in msg:
        print(json.dumps({"jsonrpc": "2.0", "id": msg["id"], "result": res}), flush=True)
"#;
        std::fs::write(&script_path, script).unwrap();
        std::fs::set_permissions(&script_path, std::fs::Permissions::from_mode(0o700)).unwrap();

        struct NoopReports;
        impl session::Reports for NoopReports {}

        for (cap_mode, sp_mode, expect_delivered) in [
            ("claude_shape", session::SystemPromptMode::AcpAppend, true),
            ("codex_shape", session::SystemPromptMode::AcpAppend, true),
            (
                "agy_meta",
                session::SystemPromptMode::MetaAppend("antigravity"),
                true,
            ),
            ("none", session::SystemPromptMode::AcpAppend, false),
            (
                "none",
                session::SystemPromptMode::MetaAppend("antigravity"),
                false,
            ),
        ] {
            let conn = Conn::spawn_registered(
                Spawn {
                    program: script_path.clone(),
                    args: vec![],
                    cwd: temp.path().into(),
                    env: vec![("CAP_MODE".into(), cap_mode.into())],
                    log_tag: format!("[test {cap_mode}]"),
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
            let opened = session::open(
                &conn,
                session::OpenConfig {
                    harness: "neutral-test",
                    client_version: "test",
                    cwd: temp.path().to_str().unwrap(),
                    resume: None,
                    mcp_servers: json!([]),
                    mode: None,
                    model: None,
                    effort: steve_node_api::ReasoningEffort::Default,
                    effort_mode: steve_node_api::EffortMode::AcpOption,
                    system_prompt: Some(session::SystemPrompt {
                        text: "Personal rule",
                        mode: sp_mode,
                    }),
                },
                Some(json!({"existing": 1})),
                &NoopReports,
            )
            .await
            .unwrap();
            assert_eq!(opened.system_prompt, expect_delivered, "{cap_mode}");
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_keepalive_never_starts_execution_but_model_content_does() {
        let temp = tempfile::tempdir().unwrap();
        let cfg = adapter(temp.path(), "", true);
        let req = request(&cfg, temp.path());
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
        for expected in [TimeoutPhase::FirstByte, TimeoutPhase::Execution] {
            for output in ["hello", "thought"] {
                let (tx, mut rx) = mpsc::unbounded_channel();
                let meter = Epoch::new("clock").unwrap();
                let mut norm = Normalizer::new(temp.path(), "test", meter.clone());
                let mut out = Output {
                    session: Some("s1".into()),
                    ..Default::default()
                };
                let mut sampler = Sampler::start_source(None, "s1", "test", meter).await;
                let progress = Progress::default();
                let on_event = |_| {};
                let send = |update| {
                    tx.send(Incoming::Notification {
                        method: "session/update".into(),
                        params: json!({"sessionId":"s1","update": update}),
                    })
                    .unwrap();
                };
                send(json!({"sessionUpdate": "agent_message_chunk",
                    "content":{"type":"text","text":"trailing setup history"}}));
                let mut submitted = false;
                let prompting = std::future::poll_fn(|_| {
                    if !submitted {
                        submitted = true;
                        progress.advance(GatewayPhase::Submitted, &on_event);
                        send(json!({"sessionUpdate": "keepalive"}));
                        send(json!({"sessionUpdate": "agent_message_chunk",
                            "content":{"type":"text","text":""}}));
                        if expected == TimeoutPhase::Execution {
                            let kind = if output == "thought" {
                                "agent_thought_chunk"
                            } else {
                                "agent_message_chunk"
                            };
                            send(
                                json!({"sessionUpdate": kind,"content":{"type":"text","text":output}}),
                            );
                        }
                    }
                    std::task::Poll::Pending
                });
                let deadline = tokio::time::Instant::now() + Duration::from_secs(3);
                let mut cancel = std::pin::pin!(std::future::pending());
                let work = drive_prompt(
                    &conn,
                    &mut rx,
                    prompting,
                    &mut norm,
                    &mut out,
                    &mut sampler,
                    &req,
                    &on_event,
                    &progress,
                    cancel.as_mut(),
                );
                let (result, ()) = tokio::join!(tokio::time::timeout_at(deadline, work), async {
                    tokio::task::yield_now().await;
                    tokio::time::advance(Duration::from_secs(3)).await;
                });
                assert!(result.is_err());
                assert!(
                    matches!(progress.timeout(), OneShotError::Timeout(phase) if phase == expected)
                );
            }
        }
    }

    #[tokio::test]
    async fn usage_preceding_a_prompt_error_is_kept_even_when_response_wins_the_poll() {
        let temp = tempfile::tempdir().unwrap();
        let cfg = adapter(temp.path(), "", true);
        let req = request(&cfg, temp.path());
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
                data: Some(json!({"code": -32603, "message": "API Error: 503 overloaded"})),
            }))
        });
        let mut sampler =
            Sampler::start_source(None, "s1", "test", Epoch::new("test").unwrap()).await;
        let progress = Progress::default();
        progress.advance(GatewayPhase::Submitted, &on_event);
        let err = drive_prompt(
            &conn,
            &mut rx,
            prompting,
            &mut norm,
            &mut out,
            &mut sampler,
            &req,
            &on_event,
            &progress,
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
        let err = run(request(&cfg, temp.path()), &|_| true, &|ev| {
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
        let pending = run(request(&cfg, temp.path()), &spawned, &|_| {});
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
        let event = |_: EventBody| {};
        let result = run_cancellable(
            request(&cfg, temp.path()),
            &spawned,
            &event,
            std::future::ready(()),
        )
        .await;
        assert!(matches!(result, Err(OneShotError::Failed(_))));
        assert_eq!(spawn_count.load(Ordering::SeqCst), 0);

        let stop = tokio::sync::Notify::new();
        let session = |_: &str| stop.notify_one();
        let mut req = request(&cfg, temp.path());
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
        let mut req = request(&cfg, temp.path());
        req.max_output_bytes = 4000;
        assert!(matches!(
            run(req, &spawned, &event).await,
            Err(OneShotError::OutputLimit)
        ));
        assert!(final_seen.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn cancellation_delivers_buffered_text_before_any_unchecked_tool_or_permission() {
        let temp = tempfile::tempdir().unwrap();
        let cfg = adapter(
            temp.path(),
            r#"        lines = [
            {"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"acp-session","update":{"sessionUpdate":"agent_message_chunk","messageId":"m1","content":{"type":"text","text":"先"}}}},
            {"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"acp-session","update":{"sessionUpdate":"agent_message_chunk","messageId":"m2","content":{"type":"text","text":"写到一半"}}}},
            {"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"acp-session","update":{"sessionUpdate":"tool_call","toolCallId":"t1","kind":"edit","title":"write","rawInput":{"path":"file"}}}},
            {"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"acp-session","update":{"sessionUpdate":"agent_message_chunk","messageId":"m3","content":{"type":"text","text":"must-not-leak-after-tool"}}}},
            {"jsonrpc":"2.0","method":"session/update","params":{"sessionId":"acp-session","update":{"sessionUpdate":"usage_update","used":4,"size":100,"cost":{"amount":0.02,"currency":"USD"}}}},
        ]
        sys.stdout.write("".join(json.dumps(x, ensure_ascii=False) + "\n" for x in lines))
        sys.stdout.flush()
        time.sleep(60)"#,
            true,
        );
        let stop = tokio::sync::Notify::new();
        let started = Mutex::new(Vec::<String>::new());
        let deltas = Mutex::new(Vec::<(String, String)>::new());
        let completed = Mutex::new(Vec::<String>::new());
        let costs = Mutex::new(Vec::<String>::new());
        let event = |ev: EventBody| match ev {
            EventBody::MessageStarted { message, .. } => {
                started.lock().unwrap().push(message);
            }
            EventBody::TextDelta {
                message,
                text: delta,
                ..
            } => {
                if delta == "先" {
                    stop.notify_one();
                }
                deltas.lock().unwrap().push((message, delta));
            }
            EventBody::MessageCompleted { message } => {
                completed.lock().unwrap().push(message);
            }
            EventBody::Usage { report } => {
                if let Some(cost) = report.cost {
                    costs.lock().unwrap().push(cost.decimal);
                }
            }
            _ => {}
        };
        let result = run_cancellable(
            request(&cfg, temp.path()),
            &|_| true,
            &event,
            stop.notified(),
        )
        .await;
        assert!(matches!(result, Err(OneShotError::Failed(_))));
        let started = started.lock().unwrap();
        assert_eq!(started.len(), 2, "only m1 and m2 should start: {started:?}");
        assert_eq!(
            *deltas.lock().unwrap(),
            vec![
                (started[0].clone(), "先".to_string()),
                (started[1].clone(), "写到一半".to_string()),
            ]
        );
        assert_eq!(*completed.lock().unwrap(), *started);
        assert_eq!(*costs.lock().unwrap(), vec!["0.02".to_string()]);
    }
}
