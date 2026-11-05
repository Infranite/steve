//! A Steve Node: the host of harnesses on one machine (docs/hub-node-client.md).
//!
//! It runs exactly what the Hub asks, one attempt at a time per session, and reports facts. It never
//! retries, never picks the next message, never decides about permissions: the Hub does. Agent
//! protocols end here: the ACP driver (`steve_acp::session`, `normalize`, `permission`) speaks to the
//! adapter, and only Steve's events leave.
//!
//! Everything accepted is in the [`journal`] first, so a restart turns what was running into
//! `unknown` instead of forgetting it.

mod journal;
mod tools;
pub mod workspace;

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde_json::{Value, json};
use steve_acp::normalize::{Fact, Normalizer};
use steve_acp::permission::{self, Asked};
use steve_acp::rpc::{Conn, Incoming, Spawn};
use steve_acp::session::{self as acp_session, OpenConfig, Reports, Strictness};
use steve_acp::{AcpConfig, FileRef, StoredFile};
use steve_node_api::*;

use journal::{Journal, Op, SessionRow, WorkspaceRow};
pub use workspace::Root;

/// What this Node is and where it keeps its things.
pub struct NodeConfig {
    pub node: NodeId,
    /// The journal, scratch directories, worktrees and the attachment cache live here.
    pub home: PathBuf,
    pub acp: AcpConfig,
    pub roots: Vec<Root>,
    pub capacity: Capacity,
}

fn now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

fn id<T: TryFrom<String>>(s: impl Into<String>) -> ApiResult<T>
where
    T::Error: std::fmt::Display,
{
    T::try_from(s.into()).map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))
}

struct Live {
    conn: Arc<Conn>,
    generation: u64,
    acp_id: String,
    image: bool,
}

/// The attempt a session runs now.
struct Run {
    exec: ExecutionRef,
    op: OperationId,
    norm: Normalizer,
    generation: Option<u64>,
    deadline: Timestamp,
    acted: bool,
    cancel: bool,
}

struct Wait {
    req: PermissionRequest,
    asked: Asked,
    rpc_id: Value,
    conn: Arc<Conn>,
    session: SessionId,
}

#[derive(Default)]
struct State {
    live: HashMap<SessionId, Live>,
    runs: HashMap<SessionId, Run>,
    waits: HashMap<PermissionId, Wait>,
    snapshots: HashMap<u64, (Vec<SnapshotEntry>, Vec<StreamHead>)>,
    generation: u64,
}

struct Inner {
    cfg: NodeConfig,
    j: Journal,
    incarnation: u64,
    st: Mutex<State>,
    sink: Mutex<Option<Arc<dyn EventSink>>>,
    files: Mutex<Option<Arc<dyn HubFiles>>>,
    wake: tokio::sync::Notify,
    delivering: AtomicBool,
    /// Steve's tools for this Node's harnesses, and where they reach them.
    tools: Arc<tools::Endpoint>,
    tools_url: Mutex<Option<String>>,
}

/// Which attempt a session runs now, for the tool endpoint.
struct CurrentOf(std::sync::Weak<Inner>);

impl tools::Current for CurrentOf {
    fn execution(&self, session: &SessionId) -> Option<ExecutionRef> {
        let inner = self.0.upgrade()?;
        let st = inner.st.lock().unwrap_or_else(|e| e.into_inner());
        st.runs.get(session).map(|r| r.exec.clone())
    }
}

/// What a new command's handler decided.
struct Decided {
    state: Observed,
    execution: Option<ExecutionRef>,
    workspace: Option<WorkspaceRef>,
    events: Vec<(Subject, EventBody)>,
}

impl Decided {
    fn done(result: OperationResult) -> Self {
        Decided {
            state: Observed::Finished {
                outcome: Outcome::Succeeded,
                result: Some(result),
                error: None,
            },
            execution: None,
            workspace: None,
            events: vec![],
        }
    }
}

#[derive(Clone)]
pub struct LocalNode(Arc<Inner>);

impl LocalNode {
    /// Opens the journal (a new incarnation: what was running is `unknown` now).
    pub fn open(cfg: NodeConfig) -> ApiResult<LocalNode> {
        std::fs::create_dir_all(&cfg.home)
            .map_err(|e| ApiError::new(ErrorCode::Internal, format!("node home: {e}")))?;
        let (j, incarnation) = Journal::open(&cfg.home.join("node.sqlite3"))?;
        Ok(LocalNode(Arc::new_cyclic(|weak| Inner {
            cfg,
            j,
            incarnation,
            st: Mutex::new(State::default()),
            sink: Mutex::new(None),
            files: Mutex::new(None),
            wake: tokio::sync::Notify::new(),
            delivering: AtomicBool::new(false),
            tools: tools::Endpoint::new(Arc::new(CurrentOf(weak.clone()))),
            tools_url: Mutex::new(None),
        })))
    }

    /// Serve Steve's tools to this Node's harnesses, answered by `hub`. Sessions started after this
    /// get them; until then harnesses run without Steve's tools.
    pub async fn connect_tools(&self, hub: Arc<dyn HubTools>) -> std::io::Result<()> {
        self.0.tools.connect(hub);
        if self.0.tools_url.lock().unwrap().is_none() {
            let url = self.0.tools.serve().await?;
            *self.0.tools_url.lock().unwrap() = Some(url);
        }
        Ok(())
    }

    pub fn incarnation(&self) -> u64 {
        self.0.incarnation
    }

    /// Where events go and where attachments come from (the Hub). Starts delivering what is waiting.
    pub fn connect(&self, sink: Arc<dyn EventSink>, files: Option<Arc<dyn HubFiles>>) {
        *self.0.sink.lock().unwrap() = Some(sink);
        *self.0.files.lock().unwrap() = files;
        if !self.0.delivering.swap(true, Ordering::SeqCst) {
            let me = self.clone();
            tokio::spawn(async move { me.deliver_loop().await });
        }
        self.0.wake.notify_one();
    }

    fn st(&self) -> MutexGuard<'_, State> {
        self.0.st.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn mine(&self, w: &WorkspaceRef) -> ApiResult<()> {
        if w.node != self.0.cfg.node {
            return Err(ApiError::new(ErrorCode::Denied, "another node's workspace"));
        }
        Ok(())
    }

    // ── events ──

    fn stream_of(subject: &Subject) -> ApiResult<StreamId> {
        id(match subject {
            Subject::Session { session } => derived_id("session", session.as_str()),
            Subject::Execution(ExecutionRef::Turn { session, .. }) => {
                derived_id("session", session.as_str())
            }
            Subject::Execution(ExecutionRef::Job { job, .. }) => derived_id("job", job.as_str()),
            Subject::Operation { operation } => derived_id("op", operation.as_str()),
        })
    }

    /// Journal the event, then let delivery pick it up.
    fn emit(&self, subject: Subject, body: EventBody) {
        let r = (|| {
            let stream = Self::stream_of(&subject)?;
            let n = self.0.j.bump("event")?;
            let event_id: EventId = id(format!("ev_{}_{n}", self.0.incarnation))?;
            self.0.j.record(&stream, |cursor| NodeEvent {
                schema: EVENT_SCHEMA,
                id: event_id,
                at: Timestamp(now()),
                cursor,
                subject,
                body,
            })
        })();
        match r {
            Ok(_) => self.0.wake.notify_one(),
            Err(e) => tracing::error!("node event: {e}"),
        }
    }

    async fn deliver_loop(self) {
        let mut backoff = Duration::from_millis(100);
        loop {
            let batch = match self.0.j.unacked(200) {
                Ok(b) => b,
                Err(e) => {
                    tracing::error!("node delivery: {e}");
                    vec![]
                }
            };
            if batch.is_empty() {
                let _ = tokio::time::timeout(Duration::from_secs(5), self.0.wake.notified()).await;
                continue;
            }
            let sink = self.0.sink.lock().unwrap().clone();
            let Some(sink) = sink else {
                self.0.wake.notified().await;
                continue;
            };
            match sink.commit(EventBatch { events: batch }).await {
                Ok(ack) => {
                    backoff = Duration::from_millis(100);
                    if let Err(e) = self.0.j.ack(&ack) {
                        tracing::error!("node ack: {e}");
                    }
                    if ack.durable.is_empty() {
                        tokio::time::sleep(backoff).await;
                    }
                }
                Err(e) => {
                    tracing::warn!("hub did not take events: {e}");
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_secs(10));
                }
            }
        }
    }

    // ── the command inbox ──

    /// Known id: same method and content → its receipt (even past the window), otherwise conflict.
    /// New: past the window → stale; `decide` runs under the state lock, and the command, its state
    /// and its events are journaled together before the answer. Returns whether it was new.
    fn accept<T: serde::Serialize>(
        &self,
        method: &'static str,
        c: &Command<T>,
        decide: impl FnOnce(&LocalNode, &mut State) -> ApiResult<Decided>,
    ) -> ApiResult<(Receipt, bool)> {
        c.check_window()?;
        let digest = c.digest(method)?;
        let mut st = self.st();
        if let Some(op) = self.0.j.op(&c.id)? {
            return if op.digest == digest && op.method == method {
                Ok((op.receipt, false))
            } else {
                Err(ApiError::new(ErrorCode::Conflict, "same operation, other content").on(&c.id))
            };
        }
        if now() > c.accept_before.0 {
            return Err(ApiError::new(ErrorCode::Stale, "past accept_before").on(&c.id));
        }
        let d = decide(self, &mut st)?;
        let receipt = Receipt {
            operation: c.id.clone(),
            state: d.state,
        };
        self.0.j.accept(
            &c.id,
            &Op {
                method: method.into(),
                digest,
                receipt: receipt.clone(),
                execution: d.execution,
                workspace: d.workspace,
                cancel: false,
            },
            now(),
        )?;
        drop(st);
        for (s, b) in d.events {
            self.emit(s, b);
        }
        Ok((receipt, true))
    }

    fn check_policy(p: &ExecutionPolicy) -> ApiResult<()> {
        if let SandboxRequirement::Required { guarantees } = &p.sandbox
            && !guarantees.is_empty()
        {
            return Err(ApiError::new(
                ErrorCode::Unsupported,
                "this node enforces no sandbox for sessions yet",
            ));
        }
        Ok(())
    }

    fn check_harness(&self, h: &str) -> ApiResult<()> {
        if !self.0.cfg.acp.adapters.contains_key(h) {
            return Err(ApiError::new(
                ErrorCode::Unsupported,
                format!("no harness {h} on this node"),
            ));
        }
        Ok(())
    }

    fn revision(&self) -> ApiResult<Revision> {
        id(format!("r{}", self.0.j.bump("revision")?))
    }

    fn workspace_path(&self, w: &WorkspaceRef) -> ApiResult<(PathBuf, u64)> {
        self.mine(w)?;
        let row = self
            .0
            .j
            .workspace(&w.id)?
            .ok_or_else(|| ApiError::new(ErrorCode::Invalid, "no such workspace"))?;
        Ok((PathBuf::from(row.path), row.incarnation))
    }

    // ── running an attempt ──

    /// The ACP mode for a permission mode (Steve's names; Codex maps them to its sandbox modes).
    fn mode(harness: &str, p: PermissionMode) -> Option<String> {
        let legacy = match p {
            PermissionMode::Plan => "plan",
            PermissionMode::Ask => "default",
            PermissionMode::AcceptEdits => "acceptEdits",
            PermissionMode::FullAccess => "bypassPermissions",
        };
        if harness == "codex" {
            Some(steve_acp::codex_mode(Some(legacy)).to_string())
        } else {
            (legacy != "default").then(|| legacy.to_string())
        }
    }

    async fn run_attempt(
        self,
        session: SessionId,
        spec: TurnSpec,
        exec: ExecutionRef,
        op: OperationId,
    ) {
        let completion = match self.attempt(&session, &spec).await {
            Ok(c) => c,
            Err(e) => Completion {
                outcome: if e.code == ErrorCode::Offline {
                    Outcome::Interrupted
                } else {
                    Outcome::Failed
                },
                stop_reason: "error".into(),
                transient: steve_acp::is_transient(&e.safe_detail),
                acted: self.st().runs.get(&session).is_some_and(|r| r.acted),
                error: Some(e),
                result: vec![],
                continuation: None,
            },
        };
        self.end_attempt(&session, &exec, &op, completion);
    }

    /// The process for the session, started and opened if needed (strictly: a mode, model or effort
    /// that does not take is an error, not another configuration).
    async fn ensure(
        &self,
        row: &SessionRow,
        cwd: &str,
    ) -> ApiResult<(Arc<Conn>, u64, String, bool)> {
        if let Some(l) = self.st().live.get(&row.view.id) {
            return Ok((l.conn.clone(), l.generation, l.acp_id.clone(), l.image));
        }
        let harness = &row.open.choice.harness;
        let (program, args) = self
            .0
            .cfg
            .acp
            .adapter_command(harness)
            .map_err(|e| ApiError::new(ErrorCode::Unsupported, e))?;
        let generation = {
            let mut st = self.st();
            st.generation += 1;
            st.generation
        };
        let sid = row.view.id.clone();
        let (me1, me2, sid1, sid2) = (self.clone(), self.clone(), sid.clone(), sid.clone());
        let conn = Conn::spawn(
            Spawn {
                program,
                args,
                cwd: cwd.into(),
                env: vec![(
                    "CLAUDE_CODE_EXECUTABLE".into(),
                    self.0.cfg.acp.claude_executable.clone().into_os_string(),
                )],
                log_tag: format!("[node {sid}]"),
            },
            move |c, inc| me1.on_incoming(&sid1, generation, c, inc),
            move || me2.on_exit(&sid2, generation),
        )
        .map_err(|e| ApiError::new(ErrorCode::Unsupported, e.message))?;
        struct Seen(Mutex<(Option<String>, Option<String>, Option<String>)>);
        impl Reports for Seen {
            fn new_session(&self, acp_id: &str) {
                self.0.lock().unwrap().0 = Some(acp_id.into());
            }
            fn model(&self, m: &str) {
                self.0.lock().unwrap().1 = Some(m.into());
            }
            fn effort(&self, e: &str) {
                self.0.lock().unwrap().2 = Some(e.into());
            }
        }
        let seen = Seen(Mutex::new((None, None, None)));
        let opened = acp_session::open(
            &conn,
            OpenConfig {
                client_version: &self.0.cfg.acp.version,
                cwd,
                resume: row.acp_id.as_deref(),
                mcp_servers: self.mcp_servers(&sid),
                mode: Self::mode(harness, row.open.policy.permissions),
                model: row.open.choice.model.as_deref(),
                effort: row.open.choice.effort.as_deref(),
            },
            &seen,
            Strictness::Strict,
        )
        .await;
        let opened = match opened {
            Ok(o) => o,
            Err(e) => {
                conn.kill();
                return Err(ApiError::new(ErrorCode::Unsupported, e.message));
            }
        };
        let (new_id, model, effort) = seen.0.into_inner().unwrap();
        // what the harness runs, as it says
        if let Some(mut r) = self.0.j.session(&sid)? {
            if let Some(a) = new_id {
                r.acp_id = Some(a);
            }
            if model.is_some() {
                r.view.effective.model = model;
            }
            if effort.is_some() {
                r.view.effective.effort = effort;
            }
            r.view.resume = r
                .acp_id
                .as_ref()
                .map(|a| id(a.clone()))
                .transpose()
                .ok()
                .flatten();
            self.0.j.put_session(&r)?;
            self.emit(
                Subject::Session {
                    session: sid.clone(),
                },
                EventBody::SessionChanged { view: r.view },
            );
        }
        if conn.has_exited() {
            return Err(ApiError::new(ErrorCode::Offline, "adapter exited"));
        }
        self.st().live.insert(
            sid,
            Live {
                conn: conn.clone(),
                generation,
                acp_id: opened.acp_id.clone(),
                image: opened.image,
            },
        );
        Ok((conn, generation, opened.acp_id, opened.image))
    }

    /// Steve's tool server for a new adapter process of `session` (a fresh token; the old ones go).
    fn mcp_servers(&self, session: &SessionId) -> Value {
        let url = self.0.tools_url.lock().unwrap().clone();
        match url {
            Some(url) => {
                self.0.tools.revoke_session(session);
                let token = self.0.tools.token(session);
                json!([{ "type": "http", "name": "steve", "url": url,
                    "headers": [{ "name": "Authorization", "value": format!("Bearer {token}") }] }])
            }
            None => json!([]),
        }
    }

    /// Prompt blocks: the text, attachments put into the workspace (from the Hub), images inline.
    async fn prompt(&self, cwd: &str, parts: &[Part], image: bool) -> ApiResult<Value> {
        let mut text = vec![];
        let mut refs = vec![];
        let mut stored: HashMap<String, StoredFile> = HashMap::new();
        for p in parts {
            match p {
                Part::Text { text: t } => text.push(t.clone()),
                Part::Image { artifact } | Part::File { artifact, .. } => {
                    let name = match p {
                        Part::File { name, .. } => name.clone(),
                        _ => format!("image-{}", &artifact.sha256.as_str()[7..15]),
                    };
                    let path = self.fetch(artifact).await?;
                    refs.push(FileRef {
                        id: artifact.id.to_string(),
                        name: name.clone(),
                        mime: artifact.mime.clone(),
                        size: artifact.size,
                    });
                    stored.insert(
                        artifact.id.to_string(),
                        StoredFile {
                            id: artifact.id.to_string(),
                            name,
                            mime: artifact.mime.clone(),
                            size: artifact.size,
                            path,
                        },
                    );
                }
                Part::Diff { .. } => {}
            }
        }
        let a = steve_acp::for_agent(cwd, &refs, image, |i| stored.get(i).cloned())
            .map_err(|e| ApiError::new(ErrorCode::Internal, e))?;
        let body = text.join("\n\n") + &a.note;
        Ok(Value::Array(
            std::iter::once(json!({"type": "text", "text": body}))
                .chain(a.images)
                .collect(),
        ))
    }

    /// An attachment's bytes, from the cache or the Hub (checked against its hash).
    async fn fetch(&self, a: &ArtifactRef) -> ApiResult<PathBuf> {
        let dir = self.0.cfg.home.join("blobs");
        let path = dir.join(a.sha256.as_str().trim_start_matches("sha256:"));
        if path.exists() {
            return Ok(path);
        }
        let files = self.0.files.lock().unwrap().clone();
        let Some(files) = files else {
            return Err(ApiError::new(
                ErrorCode::Unsupported,
                "no hub to fetch attachments from",
            ));
        };
        let mut bytes = vec![];
        loop {
            let c = files
                .fetch(
                    FileAccess::Operation {
                        operation: id("op_fetch")?,
                    },
                    a.id.clone(),
                    bytes.len() as u64,
                    1 << 20,
                )
                .await?;
            bytes.extend(c.bytes);
            if c.eof {
                break;
            }
        }
        if Digest::sha256(&bytes) != a.sha256 {
            return Err(ApiError::new(
                ErrorCode::Invalid,
                "attachment hash mismatch",
            ));
        }
        std::fs::create_dir_all(&dir)
            .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?;
        std::fs::write(&path, bytes)
            .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?;
        Ok(path)
    }

    async fn attempt(&self, session: &SessionId, spec: &TurnSpec) -> ApiResult<Completion> {
        let row = self
            .0
            .j
            .session(session)?
            .ok_or_else(|| ApiError::new(ErrorCode::Invalid, "no such session"))?;
        let (cwd, _) = self.workspace_path(&row.open.workspace)?;
        let cwd = cwd.to_string_lossy().to_string();
        if self.st().runs.get(session).is_some_and(|r| r.cancel) {
            return Ok(cancelled());
        }
        let (conn, generation, acp_id, image) = self.ensure(&row, &cwd).await?;
        let prompt = self.prompt(&cwd, &spec.run.prompt, image).await?;
        {
            let mut st = self.st();
            if let Some(r) = st.runs.get_mut(session) {
                r.generation = Some(generation);
            }
        }
        let req = conn.request(
            "session/prompt",
            json!({"sessionId": acp_id, "prompt": prompt}),
            None,
        );
        let mut req = std::pin::pin!(req);
        // the first poll writes the prompt; a cancel asked meanwhile goes right after it
        let mut early = None;
        std::future::poll_fn(|cx| {
            if let std::task::Poll::Ready(v) = req.as_mut().poll(cx) {
                early = Some(v);
            }
            std::task::Poll::Ready(())
        })
        .await;
        if self.st().runs.get(session).is_some_and(|r| r.cancel) {
            conn.notify("session/cancel", json!({"sessionId": acp_id}));
        }
        let r = match early {
            Some(r) => r,
            None => req.await,
        };
        let acted = self.st().runs.get(session).is_some_and(|r| r.acted);
        Ok(match r {
            Ok(resp) => {
                let stop = resp
                    .get("stopReason")
                    .and_then(Value::as_str)
                    .unwrap_or("end_turn")
                    .to_string();
                self.final_usage(session, &resp);
                Completion {
                    outcome: if stop == "cancelled" {
                        Outcome::Cancelled
                    } else {
                        Outcome::Succeeded
                    },
                    stop_reason: stop,
                    error: None,
                    result: vec![],
                    continuation: None,
                    transient: false,
                    acted,
                }
            }
            Err(e) => {
                let gone = conn.has_exited() || steve_acp::is_dead_agent(&e.message);
                if gone {
                    // the next attempt starts a fresh adapter (it loads the session from history)
                    let dead = {
                        let mut st = self.st();
                        match st.live.get(session) {
                            Some(l) if l.generation == generation => st.live.remove(session),
                            _ => None,
                        }
                    };
                    if let Some(l) = dead {
                        l.conn.kill();
                    }
                }
                Completion {
                    outcome: if gone {
                        Outcome::Interrupted
                    } else {
                        Outcome::Failed
                    },
                    stop_reason: "error".into(),
                    transient: steve_acp::is_transient(&e.message),
                    acted,
                    error: Some(ApiError::new(ErrorCode::Internal, e.message)),
                    result: vec![],
                    continuation: None,
                }
            }
        })
    }

    /// The token figures of the prompt's answer: the main loop's, and per model everything
    /// (sub-agents, compaction) when the adapter reports it.
    fn final_usage(&self, session: &SessionId, resp: &Value) {
        let exec = match self.st().runs.get(session) {
            Some(r) => r.exec.clone(),
            None => return,
        };
        let tok = |u: &Value| Tokens {
            input: u.get("inputTokens").and_then(Value::as_u64),
            output: u.get("outputTokens").and_then(Value::as_u64),
            cache_read: u
                .get("cachedReadTokens")
                .or_else(|| u.get("cachedInputTokens"))
                .and_then(Value::as_u64),
            cache_write: u.get("cachedWriteTokens").and_then(Value::as_u64),
        };
        let attempt = exec.attempt().clone();
        let mut reports = vec![];
        if let Some(u) = resp.get("usage").filter(|u| u.is_object()) {
            reports.push((None, UsageScope::Main, tok(u)));
        }
        if let Some(m) = resp
            .pointer("/_meta/quota/model_usage")
            .and_then(Value::as_array)
        {
            for x in m {
                if let Some(tc) = x.get("token_count") {
                    reports.push((
                        x.get("model").and_then(Value::as_str).map(String::from),
                        UsageScope::All,
                        tok(tc),
                    ));
                }
            }
        }
        for (i, (model, scope, tokens)) in reports.into_iter().enumerate() {
            let Ok(meter) = id(format!("{attempt}.final")) else {
                continue;
            };
            self.emit(
                Subject::Execution(exec.clone()),
                EventBody::Usage {
                    report: UsageReport {
                        report_id: format!("{attempt}.f{i}"),
                        model,
                        meter_epoch: meter,
                        scope,
                        measurement: Measurement::Final,
                        tokens,
                        context_used: None,
                        context_limit: None,
                        cost: None,
                    },
                },
            );
        }
    }

    fn end_attempt(
        &self,
        session: &SessionId,
        exec: &ExecutionRef,
        op: &OperationId,
        completion: Completion,
    ) {
        let (run, waits) = {
            let mut st = self.st();
            let run = match st.runs.get(session) {
                Some(r) if r.exec == *exec => st.runs.remove(session),
                _ => None,
            };
            let ids: Vec<PermissionId> = st
                .waits
                .iter()
                .filter(|(_, w)| w.req.execution == *exec)
                .map(|(k, _)| k.clone())
                .collect();
            let waits: Vec<Wait> = ids.iter().filter_map(|k| st.waits.remove(k)).collect();
            (run, waits)
        };
        for w in waits {
            w.conn
                .respond(w.rpc_id, json!({"outcome": {"outcome": "cancelled"}}));
            self.emit(
                Subject::Execution(exec.clone()),
                EventBody::PermissionExpired { id: w.req.id },
            );
        }
        if let Some(mut r) = run {
            for f in r.norm.finish() {
                if let Fact::Event(e) = f {
                    self.emit(Subject::Execution(exec.clone()), e);
                }
            }
        }
        let _ = self.0.j.set_state(
            op,
            Observed::Finished {
                outcome: completion.outcome,
                result: Some(OperationResult::Execution(completion.clone())),
                error: completion.error.clone(),
            },
        );
        self.emit(
            Subject::Execution(exec.clone()),
            EventBody::AttemptEnded { completion },
        );
        if let Ok(Some(mut row)) = self.0.j.session(session)
            && row.view.phase == SessionPhase::Active
        {
            row.view.phase = SessionPhase::Ready;
            row.view.active = None;
            let _ = self.0.j.put_session(&row);
        }
    }

    // ── what the adapter sends ──

    fn on_incoming(&self, session: &SessionId, generation: u64, conn: &Conn, inc: Incoming) {
        match inc {
            Incoming::Notification { method, params } if method == "session/update" => {
                let Some(u) = params.get("update") else {
                    return;
                };
                self.on_update(session, generation, u);
            }
            Incoming::Notification { .. } => {}
            Incoming::Request { id, method, params } if method == "session/request_permission" => {
                self.on_permission(session, generation, conn, id, &params);
            }
            Incoming::Request { id, method, .. } => {
                conn.respond_error(id, -32601, &format!("{method} not supported by Steve"));
            }
        }
    }

    fn on_update(&self, session: &SessionId, generation: u64, u: &Value) {
        let mut out: Vec<(Subject, EventBody)> = vec![];
        let mut config: Vec<Fact> = vec![];
        {
            let mut st = self.st();
            match st.runs.get_mut(session) {
                Some(r) if r.generation == Some(generation) => {
                    for f in r.norm.update(u) {
                        match f {
                            Fact::Event(EventBody::Usage { report })
                                if report.scope == UsageScope::Background =>
                            {
                                out.push((
                                    Subject::Session {
                                        session: session.clone(),
                                    },
                                    EventBody::Usage { report },
                                ));
                            }
                            Fact::Event(e) => {
                                if matches!(e, EventBody::ToolChanged { .. }) {
                                    r.acted = true;
                                }
                                out.push((Subject::Execution(r.exec.clone()), e));
                            }
                            f @ (Fact::Model(_) | Fact::Effort(_)) => config.push(f),
                            Fact::Nothing => {}
                        }
                    }
                }
                _ => {
                    // outside our attempts only the harness's own background cost counts
                    let Ok(meter) = id::<Epoch>(format!("g{generation}")) else {
                        return;
                    };
                    let mut n = Normalizer::new("/", &format!("bg{generation}"), meter);
                    for f in n.update(u) {
                        if let Fact::Event(EventBody::Usage { report }) = f
                            && report.scope == UsageScope::Background
                        {
                            out.push((
                                Subject::Session {
                                    session: session.clone(),
                                },
                                EventBody::Usage { report },
                            ));
                        }
                    }
                }
            }
        }
        for (s, b) in out {
            self.emit(s, b);
        }
        if !config.is_empty()
            && let Ok(Some(mut row)) = self.0.j.session(session)
        {
            for f in config {
                match f {
                    Fact::Model(m) => row.view.effective.model = Some(m),
                    Fact::Effort(e) => row.view.effective.effort = Some(e),
                    _ => {}
                }
            }
            let _ = self.0.j.put_session(&row);
            self.emit(
                Subject::Session {
                    session: session.clone(),
                },
                EventBody::SessionChanged { view: row.view },
            );
        }
    }

    fn on_permission(
        &self,
        session: &SessionId,
        generation: u64,
        conn: &Conn,
        rpc_id: Value,
        params: &Value,
    ) {
        let cancelled = json!({"outcome": {"outcome": "cancelled"}});
        let root = self
            .0
            .j
            .session(session)
            .ok()
            .flatten()
            .and_then(|r| self.workspace_path(&r.open.workspace).ok())
            .map(|(p, _)| p)
            .unwrap_or_default();
        let asked = permission::asked(&rpc_id, params, &root);
        let (exec, op, deadline) = {
            let st = self.st();
            match st.runs.get(session) {
                Some(r) if r.generation == Some(generation) && !r.cancel => {
                    (r.exec.clone(), r.op.clone(), r.deadline)
                }
                _ => {
                    drop(st);
                    conn.respond(rpc_id, cancelled);
                    return;
                }
            }
        };
        // the entry to Steve's own tools: the Hub authorizes each call itself (HubTools)
        if asked.steve_tool.is_some() {
            conn.respond(
                rpc_id,
                permission::answer(&asked, Some(PermissionDecision::AllowOnce)),
            );
            return;
        }
        let Ok(pid) = id::<PermissionId>(derived_id(op.as_str(), &asked.tool_call)) else {
            conn.respond(rpc_id, cancelled);
            return;
        };
        let Ok(invocation) = id::<CallId>(derived_id("call", &asked.tool_call)) else {
            conn.respond(rpc_id, cancelled);
            return;
        };
        let Ok(digest) = permission_digest(&asked.action, &[]) else {
            conn.respond(rpc_id, cancelled);
            return;
        };
        let req = PermissionRequest {
            id: pid.clone(),
            execution: exec.clone(),
            invocation,
            title: asked.title.clone(),
            action: asked.action.clone(),
            preconditions: vec![],
            digest,
            choices: asked.choices.iter().map(|(d, _)| *d).collect(),
            expires_at: deadline,
        };
        let Some(conn) = self.st().live.get(session).map(|l| l.conn.clone()) else {
            conn.respond(rpc_id, cancelled);
            return;
        };
        {
            let mut st = self.st();
            st.waits.insert(
                pid.clone(),
                Wait {
                    req: req.clone(),
                    asked,
                    rpc_id,
                    conn,
                    session: session.clone(),
                },
            );
            let on: Vec<WaitRef> = st
                .waits
                .values()
                .filter(|w| w.req.execution == exec)
                .map(|w| WaitRef::Permission(w.req.id.clone()))
                .collect();
            let _ = self.0.j.set_state(&op, Observed::Waiting { on });
        }
        self.emit(
            Subject::Execution(exec),
            EventBody::PermissionRequested { request: req },
        );
    }

    fn on_exit(&self, session: &SessionId, generation: u64) {
        let mut st = self.st();
        if st
            .live
            .get(session)
            .is_some_and(|l| l.generation == generation)
        {
            st.live.remove(session);
        }
    }

    fn snapshot_entries(&self) -> ApiResult<Vec<SnapshotEntry>> {
        let st = self.st();
        Ok(self
            .0
            .j
            .ops()?
            .into_iter()
            .map(|(_, o)| SnapshotEntry {
                pending: o
                    .execution
                    .as_ref()
                    .map(|e| {
                        st.waits
                            .values()
                            .filter(|w| w.req.execution == *e)
                            .map(|w| w.req.clone())
                            .collect()
                    })
                    .unwrap_or_default(),
                receipt: o.receipt,
                method: o.method,
                digest: o.digest,
                execution: o.execution,
                workspace: o.workspace,
                cancel_requested: o.cancel,
            })
            .collect())
    }
}

fn cancelled() -> Completion {
    Completion {
        outcome: Outcome::Cancelled,
        stop_reason: "cancelled".into(),
        error: None,
        result: vec![],
        continuation: None,
        transient: false,
        acted: false,
    }
}

fn unsupported<T>(what: &str) -> ApiResult<T> {
    Err(ApiError::new(
        ErrorCode::Unsupported,
        format!("not on this node yet: {what}"),
    ))
}

#[async_trait]
impl SessionExecutor for LocalNode {
    async fn open_session(&self, c: Command<OpenSession>) -> ApiResult<Receipt> {
        self.mine(&c.body.workspace)?;
        let (r, _) = self.accept("open_session", &c, |me, st| {
            let o = &c.body;
            me.check_harness(&o.choice.harness)?;
            Self::check_policy(&o.policy)?;
            let (_, incarnation) = me.workspace_path(&o.workspace)?;
            let old = me.0.j.session(&o.session)?;
            if let Some(s) = &old {
                if st.runs.contains_key(&o.session) {
                    return Err(ApiError::new(ErrorCode::Busy, "an attempt is running"));
                }
                if s.view.phase != SessionPhase::Released {
                    return Err(ApiError::new(ErrorCode::Conflict, "already open"));
                }
                if s.open.workspace != o.workspace || s.open.choice.harness != o.choice.harness {
                    return Err(ApiError::new(
                        ErrorCode::Conflict,
                        "reopened in another workspace or harness",
                    ));
                }
            }
            // a resume handle the Hub kept, else the one this Node remembers
            let acp_id = o
                .resume
                .as_ref()
                .map(|r| r.to_string())
                .or_else(|| old.as_ref().and_then(|s| s.acp_id.clone()));
            let view = SessionView {
                id: o.session.clone(),
                revision: me.revision()?,
                phase: SessionPhase::Ready,
                active: None,
                effective: o.choice.clone(),
                effective_permissions: o.policy.permissions,
                guarantees: vec![],
                resume: acp_id.clone().map(id).transpose()?,
            };
            me.0.j.put_session(&SessionRow {
                open: o.clone(),
                view: view.clone(),
                incarnation,
                acp_id,
            })?;
            st.live.remove(&o.session);
            let mut d = Decided::done(OperationResult::Session(view.clone()));
            d.workspace = Some(o.workspace.clone());
            d.events = vec![(
                Subject::Session {
                    session: o.session.clone(),
                },
                EventBody::SessionChanged { view },
            )];
            Ok(d)
        })?;
        Ok(r)
    }

    async fn configure_session(&self, c: Command<ConfigureSession>) -> ApiResult<Receipt> {
        let (r, fresh) = self.accept("configure_session", &c, |me, st| {
            let b = &c.body;
            if st.runs.contains_key(&b.session) {
                return Err(ApiError::new(ErrorCode::Busy, "an attempt is running"));
            }
            let mut row =
                me.0.j
                    .session(&b.session)?
                    .ok_or_else(|| ApiError::new(ErrorCode::Invalid, "session not open"))?;
            if row.view.revision != b.expected {
                return Err(ApiError::new(
                    ErrorCode::Conflict,
                    "the session changed since",
                ));
            }
            if b.choice.harness != row.open.choice.harness {
                return Err(ApiError::new(
                    ErrorCode::Invalid,
                    "another harness needs another session",
                ));
            }
            Self::check_policy(&b.policy)?;
            row.open.choice = b.choice.clone();
            row.open.policy = b.policy.clone();
            row.view.effective = b.choice.clone();
            row.view.effective_permissions = b.policy.permissions;
            row.view.revision = me.revision()?;
            me.0.j.put_session(&row)?;
            let view = row.view.clone();
            let mut d = Decided::done(OperationResult::Session(view.clone()));
            d.events = vec![(
                Subject::Session {
                    session: b.session.clone(),
                },
                EventBody::SessionChanged { view },
            )];
            Ok(d)
        })?;
        // the next attempt starts the adapter again with the new configuration
        if fresh && let Some(l) = self.st().live.remove(&c.body.session) {
            l.conn.kill();
        }
        Ok(r)
    }

    async fn start_turn(&self, c: Command<TurnSpec>) -> ApiResult<Receipt> {
        let exec = ExecutionRef::Turn {
            session: c.body.session.clone(),
            turn: c.body.turn.clone(),
            attempt: c.body.run.attempt.clone(),
        };
        let (r, fresh) = self.accept("start_turn", &c, |me, st| {
            let t = &c.body;
            let mut row =
                me.0.j
                    .session(&t.session)?
                    .ok_or_else(|| ApiError::new(ErrorCode::Invalid, "session not open"))?;
            if row.view.phase == SessionPhase::Released {
                return Err(ApiError::new(
                    ErrorCode::Invalid,
                    "session released: open it again",
                ));
            }
            t.check_against(&row.open)?;
            let (_, incarnation) = me.workspace_path(&row.open.workspace)?;
            if incarnation != row.incarnation {
                return Err(ApiError::new(
                    ErrorCode::Invalid,
                    "the session's workspace is gone: open it again",
                ));
            }
            if t.run.limits.deadline.0 <= now() {
                return Err(ApiError::new(ErrorCode::Expired, "deadline already passed"));
            }
            if st.runs.contains_key(&t.session) {
                return Err(ApiError::new(ErrorCode::Busy, "an attempt is running"));
            }
            if st.runs.len() as u32 >= me.0.cfg.capacity.sessions {
                return Err(ApiError::new(ErrorCode::Exhausted, "no session capacity"));
            }
            if let Some((_, other)) = me.0.j.attempt(&t.run.attempt)?
                && other != c.id
            {
                return Err(ApiError::new(
                    ErrorCode::Conflict,
                    "this attempt already ran under another operation",
                ));
            }
            me.0.j.take_attempt(&exec, &c.id)?;
            let meter: Epoch = id(format!("{}.{}", t.run.attempt, me.0.incarnation))?;
            let root = me.workspace_path(&row.open.workspace)?.0;
            st.runs.insert(
                t.session.clone(),
                Run {
                    exec: exec.clone(),
                    op: c.id.clone(),
                    norm: Normalizer::new(root, t.run.attempt.as_str(), meter),
                    generation: None,
                    deadline: t.run.limits.deadline,
                    acted: false,
                    cancel: false,
                },
            );
            row.view.phase = SessionPhase::Active;
            row.view.active = Some(exec.clone());
            me.0.j.put_session(&row)?;
            Ok(Decided {
                state: Observed::Running,
                execution: Some(exec.clone()),
                workspace: Some(row.open.workspace.clone()),
                events: vec![(Subject::Execution(exec.clone()), EventBody::AttemptStarted)],
            })
        })?;
        if fresh {
            let me = self.clone();
            let spec = c.body.clone();
            let (session, op) = (c.body.session.clone(), c.id.clone());
            tokio::spawn(me.run_attempt(session, spec, exec, op));
        }
        Ok(r)
    }

    async fn session(&self, s: SessionId) -> ApiResult<SessionView> {
        self.0
            .j
            .session(&s)?
            .map(|r| r.view)
            .ok_or_else(|| ApiError::new(ErrorCode::Invalid, "no such session"))
    }

    async fn release_session(&self, c: Command<SessionId>) -> ApiResult<Receipt> {
        let (r, _) = self.accept("release_session", &c, |me, st| {
            if st.runs.contains_key(&c.body) {
                return Err(ApiError::new(ErrorCode::Busy, "an attempt is running"));
            }
            let mut row =
                me.0.j
                    .session(&c.body)?
                    .ok_or_else(|| ApiError::new(ErrorCode::Invalid, "no such session"))?;
            if let Some(l) = st.live.remove(&c.body) {
                l.conn.kill();
            }
            me.0.tools.revoke_session(&c.body);
            row.view.phase = SessionPhase::Released;
            me.0.j.put_session(&row)?;
            let mut d = Decided::done(OperationResult::Unit);
            d.events = vec![(
                Subject::Session {
                    session: c.body.clone(),
                },
                EventBody::SessionChanged { view: row.view },
            )];
            Ok(d)
        })?;
        Ok(r)
    }
}

#[async_trait]
impl JobExecutor for LocalNode {
    async fn start_job(&self, _c: Command<JobSpec>) -> ApiResult<Receipt> {
        unsupported("jobs")
    }
}

#[async_trait]
impl ExecutionControl for LocalNode {
    async fn operation(&self, op: OperationId) -> ApiResult<Receipt> {
        self.0
            .j
            .op(&op)?
            .map(|o| o.receipt)
            .ok_or_else(|| ApiError::new(ErrorCode::Invalid, "no such operation").on(&op))
    }

    async fn cancel(&self, c: Command<Cancel>) -> ApiResult<Receipt> {
        let target = c.body.target.clone();
        let mut cancels: Vec<(Arc<Conn>, String)> = vec![];
        let mut expired: Vec<(Wait, ExecutionRef)> = vec![];
        let (r, _) = self.accept("cancel", &c, |me, st| {
            let op =
                me.0.j
                    .op(&target)?
                    .ok_or_else(|| ApiError::new(ErrorCode::Invalid, "no such operation"))?;
            if op.receipt.state.is_finished() {
                return Ok(Decided::done(OperationResult::Unit));
            }
            me.0.j.set_cancel(&target)?;
            let session = st
                .runs
                .iter()
                .find(|(_, r)| r.op == target)
                .map(|(s, _)| s.clone());
            if let Some(s) = session {
                let gen_ = st.runs.get_mut(&s).map(|r| {
                    r.cancel = true;
                    (r.generation, r.exec.clone())
                });
                if let Some((Some(g), exec)) = gen_ {
                    if let Some(l) = st.live.get(&s).filter(|l| l.generation == g) {
                        cancels.push((l.conn.clone(), l.acp_id.clone()));
                    }
                    let ids: Vec<PermissionId> = st
                        .waits
                        .iter()
                        .filter(|(_, w)| w.req.execution == exec)
                        .map(|(k, _)| k.clone())
                        .collect();
                    for k in ids {
                        if let Some(w) = st.waits.remove(&k) {
                            expired.push((w, exec.clone()));
                        }
                    }
                }
            }
            Ok(Decided::done(OperationResult::Unit))
        })?;
        for (w, exec) in expired {
            w.conn
                .respond(w.rpc_id, json!({"outcome": {"outcome": "cancelled"}}));
            self.emit(
                Subject::Execution(exec),
                EventBody::PermissionExpired { id: w.req.id },
            );
        }
        for (conn, acp_id) in cancels {
            conn.notify("session/cancel", json!({"sessionId": acp_id}));
        }
        Ok(r)
    }

    async fn resolve_permission(&self, c: Command<PermissionResolution>) -> ApiResult<Receipt> {
        let res = c.body.clone();
        let mut answered: Option<(Arc<Conn>, Value, Value)> = None;
        let (r, _) = self.accept("resolve_permission", &c, |me, st| {
            let Some(w) = st.waits.get(&res.id) else {
                return Err(ApiError::new(
                    ErrorCode::Stale,
                    "nobody waits for this answer",
                ));
            };
            if w.req.execution != res.execution || w.req.digest != res.action_digest {
                return Err(ApiError::new(
                    ErrorCode::Stale,
                    "the answer is for another attempt or action",
                ));
            }
            if w.req.expires_at.0 <= now() {
                return Err(ApiError::new(ErrorCode::Expired, "the request expired"));
            }
            if !w.req.choices.contains(&res.decision) {
                return Err(ApiError::new(
                    ErrorCode::Invalid,
                    "not one of the offered choices",
                ));
            }
            let w = st.waits.remove(&res.id).expect("checked above");
            let result = permission::answer(&w.asked, Some(res.decision));
            let exec = w.req.execution.clone();
            if let Some(run) = st.runs.get(&w.session).filter(|r| r.exec == exec) {
                let on: Vec<WaitRef> = st
                    .waits
                    .values()
                    .filter(|x| x.req.execution == exec)
                    .map(|x| WaitRef::Permission(x.req.id.clone()))
                    .collect();
                let _ = me.0.j.set_state(
                    &run.op,
                    if on.is_empty() {
                        Observed::Running
                    } else {
                        Observed::Waiting { on }
                    },
                );
            }
            answered = Some((w.conn, w.rpc_id, result));
            let mut d = Decided::done(OperationResult::Unit);
            d.events = vec![(
                Subject::Execution(exec),
                EventBody::PermissionResolved {
                    resolution: res.clone(),
                },
            )];
            Ok(d)
        })?;
        if let Some((conn, rpc_id, result)) = answered {
            conn.respond(rpc_id, result);
        }
        Ok(r)
    }

    async fn snapshot(&self, page: PageRequest) -> ApiResult<Snapshot> {
        let limit = page.limit.max(1) as usize;
        let (token, offset) = match &page.cursor {
            None => {
                let entries = self.snapshot_entries()?;
                let heads = self.0.j.heads()?;
                let t = self.0.j.bump("snapshot")?;
                self.st().snapshots.insert(t, (entries, heads));
                (t, 0usize)
            }
            Some(c) => {
                let bad = || ApiError::new(ErrorCode::Invalid, "not a snapshot cursor");
                let (t, o) = c
                    .as_str()
                    .strip_prefix("snap")
                    .and_then(|r| r.split_once('.'))
                    .ok_or_else(bad)?;
                (t.parse().map_err(|_| bad())?, o.parse().map_err(|_| bad())?)
            }
        };
        let mut st = self.st();
        let Some((entries, heads)) = st.snapshots.get(&token) else {
            return Err(ApiError::new(ErrorCode::Expired, "this snapshot is gone"));
        };
        let page: Vec<SnapshotEntry> = entries.iter().skip(offset).take(limit).cloned().collect();
        let heads = heads.clone();
        let more = offset + page.len() < entries.len();
        let next = if more {
            Some(id(format!("snap{token}.{}", offset + page.len()))?)
        } else {
            st.snapshots.remove(&token);
            None
        };
        Ok(Snapshot {
            entries: page,
            heads,
            next,
        })
    }

    async fn streams(&self) -> ApiResult<Vec<StreamHead>> {
        self.0.j.heads()
    }

    async fn replay(&self, from: Watermark, max_bytes: u32) -> ApiResult<EventBatch> {
        self.0.j.replay(&from, max_bytes)
    }

    async fn acknowledge(&self, ack: EventAck) -> ApiResult<()> {
        self.0.j.ack(&ack)
    }
}

#[async_trait]
impl WorkspaceExecutor for LocalNode {
    async fn prepare_workspace(&self, c: Command<Prepare>) -> ApiResult<Receipt> {
        let (r, _) = self.accept("prepare_workspace", &c, |me, _st| {
            let spec = digest_of(&c.body.kind)?;
            let wid: WorkspaceId = id(derived_id("w", &c.body.resource))?;
            if let Some(w) = me.0.j.workspace(&wid)? {
                if w.resource == c.body.resource && w.spec == spec {
                    let mut d = Decided::done(OperationResult::Workspace(w.view.clone()));
                    d.workspace = Some(w.view.workspace);
                    return Ok(d);
                }
                return Err(ApiError::new(
                    ErrorCode::Conflict,
                    "this resource's workspace was prepared otherwise",
                ));
            }
            let name = workspace::dir_name(&c.body.resource);
            let home = &me.0.cfg.home;
            let (path, repo, branch) = match &c.body.kind {
                PrepareKind::Scratch => {
                    let p = home.join("scratch").join(&name);
                    std::fs::create_dir_all(&p)
                        .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?;
                    (p, None, None)
                }
                PrepareKind::Existing { dir } => {
                    (workspace::resolve(&me.0.cfg.roots, dir)?, None, None)
                }
                PrepareKind::Worktree {
                    repository,
                    branch,
                    base,
                } => {
                    let repo = workspace::resolve(&me.0.cfg.roots, repository)?;
                    let p = home.join("worktrees").join(&name);
                    workspace::add_worktree(&repo, &p, branch, base.as_deref()).map_err(|e| {
                        ApiError::new(ErrorCode::Invalid, format!("git worktree add: {e}"))
                    })?;
                    (p, Some(repo), Some(branch.clone()))
                }
            };
            let view = WorkspaceView {
                workspace: WorkspaceRef {
                    node: me.0.cfg.node.clone(),
                    id: wid,
                },
                revision: me.revision()?,
                usable: workspace::usable(&path),
                branch,
                display: workspace::display(&path),
            };
            me.0.j.put_workspace(&WorkspaceRow {
                resource: c.body.resource.clone(),
                spec,
                kind: c.body.kind.clone(),
                repo: repo.map(|r| r.to_string_lossy().to_string()),
                view: view.clone(),
                path: path.to_string_lossy().to_string(),
                incarnation: me.0.j.bump("workspace")?,
            })?;
            let mut d = Decided::done(OperationResult::Workspace(view.clone()));
            d.workspace = Some(view.workspace);
            Ok(d)
        })?;
        Ok(r)
    }

    async fn workspace(&self, w: WorkspaceRef) -> ApiResult<WorkspaceView> {
        self.mine(&w)?;
        let row = self
            .0
            .j
            .workspace(&w.id)?
            .ok_or_else(|| ApiError::new(ErrorCode::Invalid, "no such workspace"))?;
        let mut v = row.view;
        v.usable = workspace::usable(std::path::Path::new(&row.path));
        Ok(v)
    }

    async fn tidy_workspace(&self, c: Command<Tidy>) -> ApiResult<Receipt> {
        self.mine(&c.body.workspace)?;
        let (r, _) = self.accept("tidy_workspace", &c, |me, st| {
            let w = &c.body.workspace;
            let busy = st.runs.values().any(|_| true)
                && me
                    .0
                    .j
                    .sessions_in(w)?
                    .iter()
                    .any(|s| st.runs.contains_key(s));
            let unknown = me.0.j.ops()?.into_iter().any(|(_, o)| {
                o.workspace.as_ref() == Some(w)
                    && o.execution.is_some()
                    && !o.receipt.state.is_finished()
            });
            if busy || unknown {
                return Err(ApiError::new(ErrorCode::Busy, "the workspace is in use"));
            }
            let row =
                me.0.j
                    .workspace(&w.id)?
                    .ok_or_else(|| ApiError::new(ErrorCode::Invalid, "no such workspace"))?;
            if row.view.revision != c.body.expected {
                return Err(ApiError::new(ErrorCode::Conflict, "changed since"));
            }
            workspace::remove(
                &row.kind,
                std::path::Path::new(&row.path),
                row.repo.as_deref().map(std::path::Path::new),
                row.view.branch.as_deref(),
                c.body.keep_branch,
            )
            .map_err(|e| ApiError::new(ErrorCode::Internal, e))?;
            me.0.j.drop_workspace(&w.id)?;
            for s in me.0.j.sessions_in(w)? {
                if let Some(mut srow) = me.0.j.session(&s)? {
                    srow.view.phase = SessionPhase::Released;
                    me.0.j.put_session(&srow)?;
                }
                if let Some(l) = st.live.remove(&s) {
                    l.conn.kill();
                }
            }
            Ok(Decided::done(OperationResult::Unit))
        })?;
        Ok(r)
    }

    async fn git_facts(&self, _q: GitQuery) -> ApiResult<GitFacts> {
        unsupported("git")
    }
    async fn git_apply(&self, _c: Command<GitApply>) -> ApiResult<Receipt> {
        unsupported("git")
    }
    async fn exec(&self, _c: Command<ExecSpec>) -> ApiResult<Receipt> {
        unsupported("exec")
    }
    async fn list_dir(&self, _p: PathRef, _page: PageRequest) -> ApiResult<Page<FileInfo>> {
        unsupported("files")
    }
    async fn list_root(&self, _at: RootPath, _page: PageRequest) -> ApiResult<Page<FileInfo>> {
        unsupported("files")
    }
    async fn read_file(&self, _q: FileRead) -> ApiResult<ReadChunk> {
        unsupported("files")
    }
    async fn scan(&self, _c: Command<ScanSpec>) -> ApiResult<Receipt> {
        unsupported("scan")
    }
    async fn scan_page(&self, _s: OperationId, _p: PageRequest) -> ApiResult<ScanPage> {
        unsupported("scan")
    }
}

#[async_trait]
impl Discovery for LocalNode {
    async fn probe(&self, _fresh: bool) -> ApiResult<Capabilities> {
        let mut harnesses: Vec<HarnessCapability> = self
            .0
            .cfg
            .acp
            .adapters
            .keys()
            .map(|h| HarnessCapability {
                name: h.clone(),
                version: String::new(),
                sessions: true,
                jobs: false,
                models: vec![],
                resume: true,
                cancel: true,
                permissions: true,
                images: true,
                enforcement: vec![],
            })
            .collect();
        harnesses.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(Capabilities {
            node: self.0.cfg.node.clone(),
            os: std::env::consts::OS.into(),
            version: self.0.cfg.acp.version.clone(),
            harnesses,
            roots: self
                .0
                .cfg
                .roots
                .iter()
                .map(|r| RootInfo {
                    id: r.id.clone(),
                    display: r.display.clone(),
                })
                .collect(),
            capacity: self.0.cfg.capacity.clone(),
        })
    }
    async fn history(&self, _p: PageRequest) -> ApiResult<Page<HistoryEntry>> {
        unsupported("history")
    }
    async fn import_history(&self, _c: Command<HistoryImport>) -> ApiResult<Receipt> {
        unsupported("history")
    }
    async fn import_page(&self, _i: OperationId, _p: PageRequest) -> ApiResult<ImportedPage> {
        unsupported("history")
    }
}

impl Node for LocalNode {
    fn id(&self) -> &NodeId {
        &self.0.cfg.node
    }
}
