//! A Steve Node: the host of harnesses on one machine (docs/hub-node-client.md).
//!
//! It runs exactly what the Hub asks, one attempt at a time per session, and reports facts. It never
//! retries, never picks the next message, never decides about permissions: the Hub does. Agent
//! protocols end here: the ACP driver (`steve_acp::session`, `normalize`, `permission`) speaks to the
//! adapter, and only Steve's events leave.
//!
//! A command is accepted in one journal transaction (the command, its state and its events), then
//! handed to its session's [`actor`], which does the process work in order and journals what came
//! of it. A restart turns what was running into `unknown`; the Hub decides what happens to it
//! ([`ExecutionControl::abandon`]), and nothing is freed before its processes are confirmed gone.

mod actor;
mod journal;
mod tools;
pub mod workspace;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde_json::{Value, json};
use steve_acp::permission::Asked;
use steve_acp::{AcpConfig, FileRef, StoredFile};
use steve_node_api::*;
use tokio::sync::mpsc;

use actor::{Actor, Msg};
use journal::{Journal, Op, SessionRow, Tx, WorkspaceRow};
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

pub(crate) fn now() -> i64 {
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

/// The process group is gone (or now stopped: TERM, then KILL). False: something in it still runs.
pub(crate) async fn stop_group(pgid: i64) -> bool {
    let alive = |p: i64| unsafe { libc::kill(-(p as libc::pid_t), 0) } == 0;
    if pgid <= 1 || !alive(pgid) {
        return true;
    }
    unsafe { libc::kill(-(pgid as libc::pid_t), libc::SIGTERM) };
    for _ in 0..30 {
        if !alive(pgid) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    unsafe { libc::kill(-(pgid as libc::pid_t), libc::SIGKILL) };
    for _ in 0..20 {
        if !alive(pgid) {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    !alive(pgid)
}

/// The exclusive hold on a Node's home: one Node per journal.
struct HomeLock(#[allow(dead_code)] std::fs::File);

impl HomeLock {
    fn take(home: &Path) -> ApiResult<HomeLock> {
        use std::os::fd::AsRawFd;
        let f = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(home.join("node.lock"))
            .map_err(|e| ApiError::new(ErrorCode::Internal, format!("node lock: {e}")))?;
        if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
            return Err(ApiError::new(
                ErrorCode::Conflict,
                "another node runs on this home",
            ));
        }
        Ok(HomeLock(f))
    }
}

pub(crate) struct Inner {
    cfg: NodeConfig,
    j: Journal,
    incarnation: u64,
    _lock: HomeLock,
    generation: AtomicU64,
    actors: Mutex<HashMap<SessionId, mpsc::UnboundedSender<Msg>>>,
    sink: Mutex<Option<Arc<dyn EventSink>>>,
    files: Mutex<Option<Arc<dyn HubFiles>>>,
    wake: tokio::sync::Notify,
    delivering: AtomicBool,
    /// Since when the Hub does not take events (for the offline lease).
    hub_down_since: Mutex<Option<i64>>,
    tools: Arc<tools::Endpoint>,
    tools_url: Mutex<Option<String>>,
    /// Workspace work on one repository goes one at a time.
    repos: Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>,
    snapshots: Mutex<HashMap<u64, Snapshot>>,
}

/// Which attempt a session runs now, for the tool endpoint (from the journal).
struct CurrentOf(std::sync::Weak<Inner>);

impl tools::Current for CurrentOf {
    fn execution(&self, session: &SessionId) -> Option<ExecutionRef> {
        let inner = self.0.upgrade()?;
        inner
            .j
            .read(|t| t.session(session))
            .ok()
            .flatten()
            .and_then(|r| r.view.active)
    }
}

/// Journal events in `t` (numbered in their streams as part of the transaction).
pub(crate) fn emit_all(inner: &Inner, t: &Tx, evs: Vec<(Subject, EventBody)>) -> ApiResult<()> {
    for (subject, body) in evs {
        let stream: StreamId = id(match &subject {
            Subject::Session { session } => derived_id("session", session.as_str()),
            Subject::Execution(ExecutionRef::Turn { session, .. }) => {
                derived_id("session", session.as_str())
            }
            Subject::Execution(ExecutionRef::Job { job, .. }) => derived_id("job", job.as_str()),
            Subject::Operation { operation } => derived_id("op", operation.as_str()),
        })?;
        let n = t.bump("event")?;
        let event_id: EventId = id(format!("ev_{}_{n}", inner.incarnation))?;
        t.record(&stream, |cursor| NodeEvent {
            schema: EVENT_SCHEMA,
            id: event_id,
            at: Timestamp(now()),
            cursor,
            subject,
            body,
        })?;
    }
    Ok(())
}

/// The directory of a workspace, checked again now (still there, still inside its root).
pub(crate) fn workspace_dir(inner: &Inner, w: &WorkspaceRef) -> ApiResult<PathBuf> {
    if w.node != inner.cfg.node {
        return Err(ApiError::new(ErrorCode::Denied, "another node's workspace"));
    }
    let row = inner
        .j
        .read(|t| t.workspace(&w.id))?
        .ok_or_else(|| ApiError::new(ErrorCode::Invalid, "no such workspace"))?;
    workspace::still_valid(
        Path::new(&row.path),
        &inner.cfg.roots,
        matches!(row.kind, PrepareKind::Existing { .. }),
    )
}

/// The ACP mode for a permission mode (Steve's names; Codex maps them to its sandbox modes). Ask is
/// set explicitly, so what the adapter remembered never stays.
pub(crate) fn mode(harness: &str, p: PermissionMode) -> Option<String> {
    let legacy = match p {
        PermissionMode::Plan => "plan",
        PermissionMode::Ask => "default",
        PermissionMode::AcceptEdits => "acceptEdits",
        PermissionMode::FullAccess => "bypassPermissions",
    };
    if harness == "codex" {
        Some(steve_acp::codex_mode(Some(legacy)).to_string())
    } else {
        Some(legacy.to_string())
    }
}

/// Steve's tool server for a new adapter process of `session` (a fresh token; the old ones go).
pub(crate) fn mcp_servers(inner: &Inner, session: &SessionId) -> Value {
    let url = inner.tools_url.lock().unwrap().clone();
    match url {
        Some(url) => {
            inner.tools.revoke_session(session);
            let token = inner.tools.token(session);
            json!([{ "type": "http", "name": "steve", "url": url,
                "headers": [{ "name": "Authorization", "value": format!("Bearer {token}") }] }])
        }
        None => json!([]),
    }
}

pub(crate) fn revoke_tools(inner: &Inner, session: &SessionId) {
    inner.tools.revoke_session(session);
}

/// Prompt blocks: the text, attachments put into the workspace (from the Hub, for this execution),
/// images inline.
pub(crate) async fn prompt(
    inner: &Arc<Inner>,
    exec: &ExecutionRef,
    cwd: &Path,
    parts: &[Part],
    image: bool,
) -> ApiResult<Value> {
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
                let path = fetch(inner, exec, artifact).await?;
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
    let a = steve_acp::for_agent(&cwd.to_string_lossy(), &refs, image, |i| {
        stored.get(i).cloned()
    })
    .map_err(|e| ApiError::new(ErrorCode::Internal, e))?;
    let body = text.join("\n\n") + &a.note;
    Ok(Value::Array(
        std::iter::once(json!({"type": "text", "text": body}))
            .chain(a.images)
            .collect(),
    ))
}

/// An attachment's bytes, from the cache (checked) or the Hub (for this execution; size, progress
/// and hash checked; written whole or not at all).
async fn fetch(inner: &Arc<Inner>, exec: &ExecutionRef, a: &ArtifactRef) -> ApiResult<PathBuf> {
    use sha2::Digest as _;
    let dir = inner.cfg.home.join("blobs");
    let path = dir.join(a.sha256.as_str().trim_start_matches("sha256:"));
    if let Ok(bytes) = std::fs::read(&path)
        && bytes.len() as u64 == a.size
        && Digest::sha256(&bytes) == a.sha256
    {
        return Ok(path);
    }
    let files = inner.files.lock().unwrap().clone();
    let Some(files) = files else {
        return Err(ApiError::new(
            ErrorCode::Unsupported,
            "no hub to fetch attachments from",
        ));
    };
    let mut bytes: Vec<u8> = Vec::with_capacity(a.size.min(1 << 26) as usize);
    let mut hasher = sha2::Sha256::new();
    loop {
        let c = files
            .fetch(
                FileAccess::Execution(exec.clone()),
                a.id.clone(),
                bytes.len() as u64,
                1 << 20,
            )
            .await?;
        if c.offset != bytes.len() as u64 {
            return Err(ApiError::new(
                ErrorCode::Invalid,
                "attachment chunk out of order",
            ));
        }
        if c.bytes.is_empty() && !c.eof {
            return Err(ApiError::new(
                ErrorCode::Invalid,
                "attachment made no progress",
            ));
        }
        hasher.update(&c.bytes);
        bytes.extend(c.bytes);
        if bytes.len() as u64 > a.size {
            return Err(ApiError::new(
                ErrorCode::Invalid,
                "attachment larger than said",
            ));
        }
        if c.eof {
            break;
        }
    }
    let got = Digest::new(format!("sha256:{}", hex::encode(hasher.finalize())))
        .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?;
    if bytes.len() as u64 != a.size || got != a.sha256 {
        return Err(ApiError::new(
            ErrorCode::Invalid,
            "attachment hash or size mismatch",
        ));
    }
    std::fs::create_dir_all(&dir).map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?;
    let tmp = dir.join(format!(
        ".{}.{}",
        path.file_name().unwrap_or_default().to_string_lossy(),
        now()
    ));
    std::fs::write(&tmp, &bytes).map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?;
    std::fs::rename(&tmp, &path).map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))?;
    Ok(path)
}

/// HEAD of the workspace's repository, if it is one.
fn head_of(dir: &Path) -> Option<String> {
    let o = std::process::Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(dir)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output()
        .ok()?;
    o.status
        .success()
        .then(|| String::from_utf8_lossy(&o.stdout).trim().to_string())
}

/// A file's version as the Node sees it (size and mtime), `None` when it does not exist.
fn file_version(p: &Path) -> Option<Revision> {
    let m = std::fs::symlink_metadata(p).ok()?;
    let mt = m
        .modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    Revision::new(format!("v{}.{mt}", m.len())).ok()
}

/// A word for a shell, quoted so that a list of arguments reads back as exactly that list.
fn shell_quote(a: &str) -> String {
    let safe = !a.is_empty()
        && a.bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_./=:,+@%".contains(&b));
    if safe {
        a.to_string()
    } else {
        format!("'{}'", a.replace('\'', "'\\''"))
    }
}

/// A harness's permission request as Steve's: its facts bound to this execution and workspace
/// (paths inside it are typed, anything outside it is said so: an explicit unknown), with what must
/// still hold when the answer is used (HEAD of the repository, each target file's version).
pub(crate) fn bind_permission(
    inner: &Inner,
    session: &SessionId,
    exec: &ExecutionRef,
    op: &OperationId,
    rpc_id: &Value,
    asked: &Asked,
    deadline: Timestamp,
) -> ApiResult<PermissionRequest> {
    use steve_acp::normalize::CommandFact;
    let row = inner
        .j
        .read(|t| t.session(session))?
        .ok_or_else(|| ApiError::new(ErrorCode::Invalid, "no such session"))?;
    let w = row.open.workspace.clone();
    let dir = workspace_dir(inner, &w)?;
    let fact = &asked.fact;
    // a path inside the workspace, typed; None: outside it (or not a plain path)
    let inside = |p: &str| -> Option<(PathRef, PathBuf)> {
        let path = Path::new(p);
        let rel = if path.is_absolute() {
            path.strip_prefix(&dir).ok()?.to_path_buf()
        } else {
            path.to_path_buf()
        };
        if rel
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
        {
            return None;
        }
        let rel_s = rel.to_string_lossy().to_string();
        Some((
            PathRef {
                workspace: w.clone(),
                rel: RelPath::new(rel_s).ok()?,
            },
            dir.join(&rel),
        ))
    };
    let bound: Vec<Option<(PathRef, PathBuf)>> = fact.paths.iter().map(|p| inside(p)).collect();
    let all_inside = bound.iter().all(Option::is_some);
    let refs: Vec<PathRef> = bound.iter().flatten().map(|(r, _)| r.clone()).collect();
    let outside = |what: &str| Action::Unknown {
        summary: format!("{what}（工作区外）：{}", fact.paths.join(", ")),
    };
    let mut pre = vec![];
    let action = match fact.category {
        ToolCategory::Steve => Action::Tool {
            name: fact
                .name
                .strip_prefix("mcp__steve__")
                .unwrap_or(&fact.name)
                .to_string(),
            args: fact.raw.get("rawInput").cloned().unwrap_or(Value::Null),
        },
        ToolCategory::Exec => {
            let command = match &fact.command {
                Some(CommandFact::Shell(s)) => Some(s.clone()),
                Some(CommandFact::Argv(a)) => Some(
                    a.iter()
                        .map(|x| shell_quote(x))
                        .collect::<Vec<_>>()
                        .join(" "),
                ),
                None => None,
            };
            match (command, fact.cwd.as_deref().map(inside)) {
                (Some(command), None) => Action::Exec {
                    command,
                    cwd: None,
                    network: NetworkScope::Unrestricted,
                },
                (Some(command), Some(Some((cwd, _)))) => Action::Exec {
                    command,
                    cwd: Some(cwd),
                    network: NetworkScope::Unrestricted,
                },
                (Some(command), Some(None)) => Action::Unknown {
                    summary: format!(
                        "在工作区外执行：{command}（{}）",
                        fact.cwd.clone().unwrap_or_default()
                    ),
                },
                (None, _) => Action::Unknown {
                    summary: asked.title.clone(),
                },
            }
        }
        ToolCategory::Edit if all_inside && !refs.is_empty() => Action::Write {
            paths: refs.clone(),
            change_summary: asked.title.clone(),
            change: fact.change.clone(),
        },
        ToolCategory::Edit => outside("写入"),
        ToolCategory::Delete if all_inside && !refs.is_empty() => Action::Delete {
            paths: refs.clone(),
        },
        ToolCategory::Delete => outside("删除"),
        ToolCategory::Move if all_inside && refs.len() == 2 => Action::Move {
            from: refs[0].clone(),
            to: refs[1].clone(),
        },
        ToolCategory::Move => outside("移动"),
        ToolCategory::Read if all_inside => Action::Read {
            paths: refs.clone(),
        },
        ToolCategory::Read => outside("读取"),
        ToolCategory::Search => Action::Search {
            query: fact.query.clone().unwrap_or_default(),
        },
        ToolCategory::Fetch => Action::Fetch {
            url: fact.url.clone().unwrap_or_default(),
        },
        _ => Action::Unknown {
            summary: asked.title.clone(),
        },
    };
    if matches!(
        action,
        Action::Write { .. } | Action::Delete { .. } | Action::Move { .. }
    ) {
        for (r, abs) in bound.iter().flatten() {
            pre.push(Precondition::File {
                path: r.clone(),
                version: file_version(abs),
            });
        }
    }
    if matches!(
        action,
        Action::Exec { .. } | Action::Write { .. } | Action::Delete { .. } | Action::Move { .. }
    ) && let Some(head) = head_of(&dir)
    {
        pre.push(Precondition::Head {
            workspace: w.clone(),
            commit: head,
        });
    }
    let digest = permission_digest(&action, &pre)?;
    Ok(PermissionRequest {
        // one request instance: the operation and the adapter's request id
        id: id(derived_id(op.as_str(), &rpc_id.to_string()))?,
        execution: exec.clone(),
        // the same id the tool's view has
        invocation: fact.call.clone(),
        title: asked.title.clone(),
        action,
        preconditions: pre,
        digest,
        choices: asked.choices.iter().map(|(d, _)| *d).collect(),
        expires_at: deadline,
    })
}

/// The preconditions of a request still hold (checked right before an approval is used).
fn preconditions_hold(inner: &Inner, t: &Tx, pre: &[Precondition]) -> ApiResult<()> {
    for p in pre {
        let ok = match p {
            Precondition::Workspace {
                workspace,
                revision,
            } => t
                .workspace(&workspace.id)?
                .is_some_and(|w| w.view.revision == *revision),
            Precondition::Head { workspace, commit } => {
                let row = t
                    .workspace(&workspace.id)?
                    .ok_or_else(|| ApiError::new(ErrorCode::Stale, "the workspace is gone"))?;
                head_of(Path::new(&row.path)).as_deref() == Some(commit.as_str())
            }
            Precondition::File { path, version } => {
                let row = t
                    .workspace(&path.workspace.id)?
                    .ok_or_else(|| ApiError::new(ErrorCode::Stale, "the workspace is gone"))?;
                file_version(&Path::new(&row.path).join(path.rel.as_str())) == *version
            }
        };
        let _ = inner;
        if !ok {
            return Err(ApiError::new(
                ErrorCode::Stale,
                "what the request was about changed since",
            ));
        }
    }
    Ok(())
}

/// What a new command's handler decided.
struct Decided {
    state: Observed,
    execution: Option<ExecutionRef>,
    workspace: Option<WorkspaceRef>,
    session: Option<SessionId>,
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
            session: None,
            events: vec![],
        }
    }
    fn accepted() -> Self {
        Decided {
            state: Observed::Accepted,
            execution: None,
            workspace: None,
            session: None,
            events: vec![],
        }
    }
}

fn failed(e: ApiError) -> Observed {
    Observed::Finished {
        outcome: Outcome::Failed,
        result: None,
        error: Some(e),
    }
}

#[derive(Clone)]
pub struct LocalNode(Arc<Inner>);

impl LocalNode {
    /// Takes the home (one Node per journal), opens the journal (a new incarnation: what was running
    /// is `unknown` now) and stops nothing: groups left by the old incarnation keep their sessions
    /// held until the Hub abandons them.
    pub fn open(cfg: NodeConfig) -> ApiResult<LocalNode> {
        std::fs::create_dir_all(&cfg.home)
            .map_err(|e| ApiError::new(ErrorCode::Internal, format!("node home: {e}")))?;
        let lock = HomeLock::take(&cfg.home)?;
        let (j, incarnation) = Journal::open(&cfg.home.join("node.sqlite3"))?;
        // a group of the old incarnation that is gone already holds nothing
        j.tx(|t| {
            for p in t.procs()? {
                let alive = unsafe { libc::kill(-(p.pgid as libc::pid_t), 0) } == 0;
                if p.incarnation < incarnation && !alive {
                    t.drop_proc(&p.session, p.pgid)?;
                }
            }
            Ok(())
        })?;
        Ok(LocalNode(Arc::new_cyclic(|weak| Inner {
            cfg,
            j,
            incarnation,
            _lock: lock,
            generation: AtomicU64::new(0),
            actors: Mutex::new(HashMap::new()),
            sink: Mutex::new(None),
            files: Mutex::new(None),
            wake: tokio::sync::Notify::new(),
            delivering: AtomicBool::new(false),
            hub_down_since: Mutex::new(None),
            tools: tools::Endpoint::new(Arc::new(CurrentOf(weak.clone()))),
            tools_url: Mutex::new(None),
            repos: Mutex::new(HashMap::new()),
            snapshots: Mutex::new(HashMap::new()),
        })))
    }

    pub fn incarnation(&self) -> u64 {
        self.0.incarnation
    }

    /// Where events go and where attachments come from (the Hub). Starts delivering what is waiting.
    pub fn connect(&self, sink: Arc<dyn EventSink>, files: Option<Arc<dyn HubFiles>>) {
        *self.0.sink.lock().unwrap() = Some(sink);
        *self.0.files.lock().unwrap() = files;
        if !self.0.delivering.swap(true, Ordering::SeqCst) {
            tokio::spawn(deliver_loop(Arc::downgrade(&self.0)));
        }
        self.0.wake.notify_one();
    }

    /// Serve Steve's tools to this Node's harnesses, answered by `hub`. Sessions started after this
    /// get them.
    pub async fn connect_tools(&self, hub: Arc<dyn HubTools>) -> std::io::Result<()> {
        self.0.tools.connect(hub);
        if self.0.tools_url.lock().unwrap().is_none() {
            let url = self.0.tools.serve().await?;
            *self.0.tools_url.lock().unwrap() = Some(url);
        }
        Ok(())
    }

    fn actor(&self, session: &SessionId) -> mpsc::UnboundedSender<Msg> {
        let mut a = self.0.actors.lock().unwrap();
        if let Some(tx) = a.get(session).filter(|t| !t.is_closed()) {
            return tx.clone();
        }
        let tx = Actor::spawn(self.0.clone(), session.clone());
        a.insert(session.clone(), tx.clone());
        tx
    }

    fn mine(&self, w: &WorkspaceRef) -> ApiResult<()> {
        if w.node != self.0.cfg.node {
            return Err(ApiError::new(ErrorCode::Denied, "another node's workspace"));
        }
        Ok(())
    }

    /// The command inbox. Known id: same method and content → its receipt (even past the window),
    /// otherwise conflict. New: past the window → stale; `decide` checks against the journal and
    /// changes it, in the same transaction as the command and its events. Returns whether it was new.
    fn accept<T: serde::Serialize>(
        &self,
        method: &'static str,
        c: &Command<T>,
        decide: impl FnOnce(&Tx) -> ApiResult<Decided>,
    ) -> ApiResult<(Receipt, bool)> {
        c.check_window()?;
        let digest = c.digest(method)?;
        let r = self.0.j.tx(|t| {
            if let Some(op) = t.op(&c.id)? {
                return if op.digest == digest && op.method == method {
                    Ok((op.receipt, false))
                } else {
                    Err(
                        ApiError::new(ErrorCode::Conflict, "same operation, other content")
                            .on(&c.id),
                    )
                };
            }
            if now() > c.accept_before.0 {
                return Err(ApiError::new(ErrorCode::Stale, "past accept_before").on(&c.id));
            }
            let d = decide(t)?;
            let receipt = Receipt {
                operation: c.id.clone(),
                state: d.state,
            };
            t.accept(
                &c.id,
                &Op {
                    method: method.into(),
                    digest,
                    receipt: receipt.clone(),
                    execution: d.execution,
                    workspace: d.workspace,
                    cancel: false,
                },
                d.session.as_ref(),
                now(),
            )?;
            emit_all(&self.0, t, d.events)?;
            Ok((receipt, true))
        })?;
        self.0.wake.notify_one();
        Ok(r)
    }

    /// What this Node can enforce; anything else is refused before it is accepted.
    fn check_policy(p: &ExecutionPolicy) -> ApiResult<()> {
        if let SandboxRequirement::Required { guarantees } = &p.sandbox
            && !guarantees.is_empty()
        {
            return Err(ApiError::new(
                ErrorCode::Unsupported,
                "this node enforces no sandbox for sessions yet",
            ));
        }
        if p.network != NetworkScope::Unrestricted {
            return Err(ApiError::new(
                ErrorCode::Unsupported,
                "this node cannot restrict a harness's network",
            ));
        }
        Ok(())
    }

    fn check_limits(l: &Limits) -> ApiResult<()> {
        if l.max_tokens.is_some() || l.max_cost.is_some() {
            return Err(ApiError::new(
                ErrorCode::Unsupported,
                "this node cannot stop a harness at a token or cost limit",
            ));
        }
        if l.deadline.0 <= now() {
            return Err(ApiError::new(ErrorCode::Expired, "deadline already passed"));
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

    /// The session has nothing running, waiting or unknown.
    fn idle(t: &Tx, session: &SessionId) -> ApiResult<()> {
        let open = t.open_ops_of(session)?;
        if open
            .iter()
            .any(|(_, o)| matches!(o.receipt.state, Observed::Unknown { .. }))
        {
            return Err(ApiError::new(
                ErrorCode::Busy,
                "an attempt of this session is unknown: abandon it first",
            ));
        }
        if !open.is_empty() {
            return Err(ApiError::new(ErrorCode::Busy, "an attempt is running"));
        }
        Ok(())
    }

    fn repo_lock(&self, repo: &Path) -> Arc<tokio::sync::Mutex<()>> {
        self.0
            .repos
            .lock()
            .unwrap()
            .entry(repo.to_path_buf())
            .or_default()
            .clone()
    }

    /// Finish an operation that ran outside the inbox transaction, with its event.
    fn complete(&self, op: &OperationId, state: Observed, more: impl FnOnce(&Tx) -> ApiResult<()>) {
        let r = self.0.j.tx(|t| {
            more(t)?;
            t.set_state(op, &state)?;
            emit_all(
                &self.0,
                t,
                vec![(
                    Subject::Operation {
                        operation: op.clone(),
                    },
                    EventBody::OperationChanged {
                        receipt: Receipt {
                            operation: op.clone(),
                            state: state.clone(),
                        },
                    },
                )],
            )
        });
        self.0.wake.notify_one();
        if let Err(e) = r {
            tracing::error!("complete {op}: {e}");
        }
    }
}

async fn deliver_loop(inner: std::sync::Weak<Inner>) {
    let mut backoff = Duration::from_millis(100);
    loop {
        let Some(me) = inner.upgrade() else { return };
        let batch = me.j.read(|t| t.unacked(200)).unwrap_or_else(|e| {
            tracing::error!("node delivery: {e}");
            vec![]
        });
        if batch.is_empty() {
            // wait for new events (held at most two seconds: a dropped Node ends this loop)
            let _ = tokio::time::timeout(Duration::from_secs(2), me.wake.notified()).await;
            continue;
        }
        let sink = me.sink.lock().unwrap().clone();
        let Some(sink) = sink else {
            drop(me);
            tokio::time::sleep(Duration::from_millis(200)).await;
            continue;
        };
        match sink.commit(EventBatch { events: batch }).await {
            Ok(ack) => {
                backoff = Duration::from_millis(100);
                *me.hub_down_since.lock().unwrap() = None;
                if let Err(e) = me.j.tx(|t| t.ack(&ack)) {
                    tracing::error!("node ack: {e}");
                }
                if ack.durable.is_empty() {
                    drop(me);
                    tokio::time::sleep(backoff).await;
                }
            }
            Err(e) => {
                tracing::warn!("hub did not take events: {e}");
                me.hub_down_since.lock().unwrap().get_or_insert_with(now);
                drop(me);
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(10));
            }
        }
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
    /// Accepted now; finished once the harness started with this configuration and confirmed it.
    async fn open_session(&self, c: Command<OpenSession>) -> ApiResult<Receipt> {
        self.mine(&c.body.workspace)?;
        let (r, fresh) = self.accept("open_session", &c, |t| {
            let o = &c.body;
            self.check_harness(&o.choice.harness)?;
            Self::check_policy(&o.policy)?;
            let w = t
                .workspace(&o.workspace.id)?
                .ok_or_else(|| ApiError::new(ErrorCode::Invalid, "no such workspace"))?;
            let old = t.session(&o.session)?;
            if let Some(s) = &old {
                Self::idle(t, &o.session)?;
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
            if t.proc_of(&o.session)?
                .is_some_and(|p| p.incarnation < self.0.incarnation)
            {
                return Err(ApiError::new(
                    ErrorCode::Busy,
                    "a harness of this session from before the restart still runs: abandon it",
                ));
            }
            // a resume handle the Hub kept, else the one this Node remembers
            let acp_id = o
                .resume
                .as_ref()
                .map(|r| r.to_string())
                .or_else(|| old.as_ref().and_then(|s| s.acp_id.clone()));
            let view = SessionView {
                id: o.session.clone(),
                revision: id(format!("r{}", t.bump("revision")?))?,
                phase: SessionPhase::Opening,
                active: None,
                effective: o.choice.clone(),
                effective_permissions: o.policy.permissions,
                guarantees: vec![],
                resume: acp_id.clone().map(id).transpose()?,
            };
            t.put_session(&SessionRow {
                open: o.clone(),
                view,
                incarnation: w.incarnation,
                acp_id,
            })?;
            let mut d = Decided::accepted();
            d.workspace = Some(o.workspace.clone());
            d.session = Some(o.session.clone());
            Ok(d)
        })?;
        if fresh {
            let _ = self
                .actor(&c.body.session)
                .send(Msg::Open { op: c.id.clone() });
        }
        Ok(r)
    }

    /// Accepted now; finished once the harness restarted with the new configuration and confirmed it.
    async fn configure_session(&self, c: Command<ConfigureSession>) -> ApiResult<Receipt> {
        let (r, fresh) = self.accept("configure_session", &c, |t| {
            let b = &c.body;
            Self::idle(t, &b.session)?;
            let mut row = t
                .session(&b.session)?
                .ok_or_else(|| ApiError::new(ErrorCode::Invalid, "session not open"))?;
            if row.view.phase != SessionPhase::Ready {
                return Err(ApiError::new(ErrorCode::Busy, "the session is not ready"));
            }
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
            row.view.phase = SessionPhase::Opening;
            t.put_session(&row)?;
            let mut d = Decided::accepted();
            d.session = Some(b.session.clone());
            Ok(d)
        })?;
        if fresh {
            let _ = self
                .actor(&c.body.session)
                .send(Msg::Open { op: c.id.clone() });
        }
        Ok(r)
    }

    async fn start_turn(&self, c: Command<TurnSpec>) -> ApiResult<Receipt> {
        let exec = ExecutionRef::Turn {
            session: c.body.session.clone(),
            turn: c.body.turn.clone(),
            attempt: c.body.run.attempt.clone(),
        };
        let (r, fresh) = self.accept("start_turn", &c, |t| {
            let tu = &c.body;
            let mut row = t
                .session(&tu.session)?
                .ok_or_else(|| ApiError::new(ErrorCode::Invalid, "session not open"))?;
            match row.view.phase {
                SessionPhase::Ready => {}
                SessionPhase::Released => {
                    return Err(ApiError::new(
                        ErrorCode::Invalid,
                        "session released: open it again",
                    ));
                }
                _ => return Err(ApiError::new(ErrorCode::Busy, "the session is not ready")),
            }
            tu.check_against(&row.open)?;
            let w = t.workspace(&row.open.workspace.id)?.ok_or_else(|| {
                ApiError::new(ErrorCode::Invalid, "the session's workspace is gone")
            })?;
            if w.incarnation != row.incarnation {
                return Err(ApiError::new(
                    ErrorCode::Invalid,
                    "the session's workspace is gone: open it again",
                ));
            }
            Self::check_policy(&tu.run.policy)?;
            Self::check_limits(&tu.run.limits)?;
            Self::idle(t, &tu.session)?;
            let running = t
                .ops()?
                .into_iter()
                .filter(|(_, o)| o.method == "start_turn" && !o.receipt.state.is_finished())
                .count() as u32;
            if running >= self.0.cfg.capacity.sessions {
                return Err(ApiError::new(ErrorCode::Exhausted, "no session capacity"));
            }
            if let Some((_, other)) = t.attempt(&tu.run.attempt)?
                && other != c.id
            {
                return Err(ApiError::new(
                    ErrorCode::Conflict,
                    "this attempt already ran under another operation",
                ));
            }
            t.take_attempt(&exec, &c.id)?;
            row.view.phase = SessionPhase::Active;
            row.view.active = Some(exec.clone());
            t.put_session(&row)?;
            Ok(Decided {
                state: Observed::Running,
                execution: Some(exec.clone()),
                workspace: Some(row.open.workspace.clone()),
                session: Some(tu.session.clone()),
                events: vec![(Subject::Execution(exec.clone()), EventBody::AttemptStarted)],
            })
        })?;
        if fresh {
            let _ = self.actor(&c.body.session).send(Msg::Start {
                op: c.id.clone(),
                spec: Box::new(c.body.clone()),
                exec,
            });
        }
        Ok(r)
    }

    async fn session(&self, s: SessionId) -> ApiResult<SessionView> {
        self.0
            .j
            .read(|t| t.session(&s))?
            .map(|r| r.view)
            .ok_or_else(|| ApiError::new(ErrorCode::Invalid, "no such session"))
    }

    /// Accepted now; finished once the harness's process group is confirmed gone.
    async fn release_session(&self, c: Command<SessionId>) -> ApiResult<Receipt> {
        let (r, fresh) = self.accept("release_session", &c, |t| {
            Self::idle(t, &c.body)?;
            let mut row = t
                .session(&c.body)?
                .ok_or_else(|| ApiError::new(ErrorCode::Invalid, "no such session"))?;
            row.view.phase = SessionPhase::Releasing;
            t.put_session(&row)?;
            let mut d = Decided::accepted();
            d.session = Some(c.body.clone());
            Ok(d)
        })?;
        if fresh {
            let _ = self.actor(&c.body).send(Msg::Stop {
                op: Some(c.id.clone()),
                done: None,
            });
        }
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
            .read(|t| t.op(&op))?
            .map(|o| o.receipt)
            .ok_or_else(|| ApiError::new(ErrorCode::Invalid, "no such operation").on(&op))
    }

    async fn cancel(&self, c: Command<Cancel>) -> ApiResult<Receipt> {
        let target = c.body.target.clone();
        let mut session = None;
        let (r, fresh) = self.accept("cancel", &c, |t| {
            let op = t
                .op(&target)?
                .ok_or_else(|| ApiError::new(ErrorCode::Invalid, "no such operation"))?;
            if !op.receipt.state.is_finished() {
                t.set_cancel(&target)?;
                if let Some(ExecutionRef::Turn { session: s, .. }) = &op.execution {
                    session = Some(s.clone());
                }
            }
            Ok(Decided::done(OperationResult::Unit))
        })?;
        if fresh && let Some(s) = session {
            let _ = self.actor(&s).send(Msg::Cancel { target });
        }
        Ok(r)
    }

    /// Accepted now; finished once the execution's processes are confirmed gone: it then ends as
    /// interrupted and its session is free to be opened again.
    async fn abandon(&self, c: Command<Abandon>) -> ApiResult<Receipt> {
        let target = c.body.target.clone();
        let mut session = None;
        let (r, fresh) = self.accept("abandon", &c, |t| {
            let op = t
                .op(&target)?
                .ok_or_else(|| ApiError::new(ErrorCode::Invalid, "no such operation"))?;
            if !matches!(op.receipt.state, Observed::Unknown { .. }) {
                return Err(ApiError::new(
                    ErrorCode::Conflict,
                    "only an unknown execution is abandoned (cancel a live one)",
                ));
            }
            if let Some(ExecutionRef::Turn { session: s, .. }) = &op.execution {
                session = Some(s.clone());
            }
            Ok(Decided::accepted())
        })?;
        if fresh {
            let me = self.clone();
            let op = c.id.clone();
            tokio::spawn(async move {
                let gone = match &session {
                    Some(s) => {
                        let (tx, rx) = tokio::sync::oneshot::channel();
                        let _ = me.actor(s).send(Msg::Stop {
                            op: None,
                            done: Some(tx),
                        });
                        rx.await.unwrap_or(false)
                    }
                    None => true,
                };
                if !gone {
                    return me.complete(
                        &op,
                        failed(ApiError::new(
                            ErrorCode::Busy,
                            "its processes would not stop",
                        )),
                        |_| Ok(()),
                    );
                }
                let inner = me.0.clone();
                me.complete(&op, Decided::done(OperationResult::Unit).state, |t| {
                    let Some(o) = t.op(&target)? else {
                        return Ok(());
                    };
                    let completion = Completion {
                        outcome: Outcome::Interrupted,
                        stop_reason: "abandoned".into(),
                        error: Some(ApiError::new(ErrorCode::OutcomeUnknown, "abandoned")),
                        result: vec![],
                        continuation: None,
                        transient: false,
                        acted: true,
                    };
                    t.set_state(
                        &target,
                        &Observed::Finished {
                            outcome: Outcome::Interrupted,
                            result: Some(OperationResult::Execution(completion.clone())),
                            error: completion.error.clone(),
                        },
                    )?;
                    if let Some(exec) = &o.execution {
                        emit_all(
                            &inner,
                            t,
                            vec![(
                                Subject::Execution(exec.clone()),
                                EventBody::AttemptEnded { completion },
                            )],
                        )?;
                    }
                    if let Some(s) = &session
                        && let Some(mut row) = t.session(s)?
                    {
                        row.view.phase = SessionPhase::Released;
                        row.view.active = None;
                        t.put_session(&row)?;
                    }
                    Ok(())
                });
            });
        }
        Ok(r)
    }

    async fn resolve_permission(&self, c: Command<PermissionResolution>) -> ApiResult<Receipt> {
        let res = c.body.clone();
        let mut session = None;
        let (r, fresh) = self.accept("resolve_permission", &c, |t| {
            let Some((op, s, req)) = t.wait(&res.id)? else {
                return Err(ApiError::new(
                    ErrorCode::Stale,
                    "nobody waits for this answer",
                ));
            };
            if req.execution != res.execution || req.digest != res.action_digest {
                return Err(ApiError::new(
                    ErrorCode::Stale,
                    "the answer is for another attempt or action",
                ));
            }
            if req.expires_at.0 <= now() {
                return Err(ApiError::new(ErrorCode::Expired, "the request expired"));
            }
            if !req.choices.contains(&res.decision) {
                return Err(ApiError::new(
                    ErrorCode::Invalid,
                    "not one of the offered choices",
                ));
            }
            let o = t
                .op(&op)?
                .ok_or_else(|| ApiError::new(ErrorCode::Internal, "a wait without its attempt"))?;
            if o.receipt.state.is_finished() || o.cancel {
                return Err(ApiError::new(ErrorCode::Stale, "that attempt is over"));
            }
            if res.decision == PermissionDecision::AllowOnce {
                preconditions_hold(&self.0, t, &req.preconditions)?;
            }
            t.drop_wait(&res.id)?;
            let on: Vec<WaitRef> = t
                .waits_of(&op)?
                .into_iter()
                .map(|w| WaitRef::Permission(w.id))
                .collect();
            t.set_state(
                &op,
                &if on.is_empty() {
                    Observed::Running
                } else {
                    Observed::Waiting { on }
                },
            )?;
            session = Some(s);
            let mut d = Decided::done(OperationResult::Unit);
            d.events = vec![(
                Subject::Execution(req.execution.clone()),
                EventBody::PermissionResolved {
                    resolution: res.clone(),
                },
            )];
            Ok(d)
        })?;
        if fresh && let Some(s) = session {
            let _ = self.actor(&s).send(Msg::Answer {
                id: res.id.clone(),
                decision: res.decision,
            });
        }
        Ok(r)
    }

    async fn snapshot(&self, page: PageRequest) -> ApiResult<Snapshot> {
        let limit = page.limit.max(1) as usize;
        let (token, offset) = match &page.cursor {
            None => {
                // the operations, their waits and the stream heads, as of one moment
                let (entries, heads) = self.0.j.read(|t| {
                    let mut entries = vec![];
                    for (id, o) in t.ops()? {
                        entries.push(SnapshotEntry {
                            pending: t.waits_of(&id)?,
                            receipt: o.receipt,
                            method: o.method,
                            digest: o.digest,
                            execution: o.execution,
                            workspace: o.workspace,
                            cancel_requested: o.cancel,
                        });
                    }
                    Ok((entries, t.heads()?))
                })?;
                let token = self.0.j.tx(|t| t.bump("snapshot"))?;
                let mut snaps = self.0.snapshots.lock().unwrap();
                // pages of the last snapshots stay readable (a lost answer can be asked again)
                snaps.retain(|k, _| *k + 8 > token);
                snaps.insert(
                    token,
                    Snapshot {
                        entries,
                        heads,
                        next: None,
                    },
                );
                (token, 0usize)
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
        let snaps = self.0.snapshots.lock().unwrap();
        let Some(s) = snaps.get(&token) else {
            return Err(ApiError::new(
                ErrorCode::Expired,
                "this snapshot is gone: take a new one",
            ));
        };
        let entries: Vec<SnapshotEntry> =
            s.entries.iter().skip(offset).take(limit).cloned().collect();
        let more = offset + entries.len() < s.entries.len();
        Ok(Snapshot {
            heads: s.heads.clone(),
            next: if more {
                Some(id(format!("snap{token}.{}", offset + entries.len()))?)
            } else {
                None
            },
            entries,
        })
    }

    async fn streams(&self) -> ApiResult<Vec<StreamHead>> {
        self.0.j.read(|t| t.heads())
    }

    async fn replay(&self, from: Watermark, max_bytes: u32) -> ApiResult<EventBatch> {
        self.0.j.read(|t| t.replay(&from, max_bytes))
    }

    async fn acknowledge(&self, ack: EventAck) -> ApiResult<()> {
        self.0.j.tx(|t| t.ack(&ack))
    }
}

#[async_trait]
impl WorkspaceExecutor for LocalNode {
    /// The same resource and spec: the workspace there is. A new one: accepted now, finished once it
    /// is made (or adopted after checking it is what was asked).
    async fn prepare_workspace(&self, c: Command<Prepare>) -> ApiResult<Receipt> {
        let spec = digest_of(&c.body.kind)?;
        let wid: WorkspaceId = id(derived_id("w", &c.body.resource))?;
        let (r, fresh) = self.accept("prepare_workspace", &c, |t| {
            if let Some(w) = t.workspace(&wid)? {
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
            // the roots are checked before anything is made
            match &c.body.kind {
                PrepareKind::Existing { dir } => {
                    workspace::resolve(&self.0.cfg.roots, dir)?;
                }
                PrepareKind::Worktree { repository, .. } => {
                    workspace::resolve(&self.0.cfg.roots, repository)?;
                }
                PrepareKind::Scratch => {}
            }
            Ok(Decided::accepted())
        })?;
        if !fresh || r.state.is_finished() {
            return Ok(r);
        }
        let me = self.clone();
        let op = c.id.clone();
        let body = c.body.clone();
        tokio::spawn(async move {
            let name = workspace::dir_name(&body.resource);
            let home = me.0.cfg.home.clone();
            let made: ApiResult<(PathBuf, Option<PathBuf>, Option<String>)> = match &body.kind {
                PrepareKind::Scratch => {
                    let p = home.join("scratch").join(&name);
                    std::fs::create_dir_all(&p)
                        .map(|_| (p, None, None))
                        .map_err(|e| ApiError::new(ErrorCode::Internal, e.to_string()))
                }
                PrepareKind::Existing { dir } => {
                    workspace::resolve(&me.0.cfg.roots, dir).map(|p| (p, None, None))
                }
                PrepareKind::Worktree {
                    repository,
                    branch,
                    base,
                } => match workspace::resolve(&me.0.cfg.roots, repository) {
                    Ok(repo) => {
                        let lock = me.repo_lock(&repo);
                        let _g = lock.lock().await;
                        let p = home.join("worktrees").join(&name);
                        let (r2, p2, b2, base2) =
                            (repo.clone(), p.clone(), branch.clone(), base.clone());
                        tokio::task::spawn_blocking(move || {
                            workspace::add_worktree(&r2, &p2, &b2, base2.as_deref())
                        })
                        .await
                        .map_err(|e| e.to_string())
                        .and_then(|r| r)
                        .map(|_| (p, Some(repo), Some(branch.clone())))
                        .map_err(|e| {
                            ApiError::new(ErrorCode::Invalid, format!("git worktree: {e}"))
                        })
                    }
                    Err(e) => Err(e),
                },
            };
            let (path, repo, branch) = match made {
                Ok(x) => x,
                Err(e) => return me.complete(&op, failed(e), |_| Ok(())),
            };
            let inner = me.0.clone();
            let mut view_out = None;
            let saved = inner.j.tx(|t| {
                let view = WorkspaceView {
                    workspace: WorkspaceRef {
                        node: inner.cfg.node.clone(),
                        id: wid.clone(),
                    },
                    revision: id(format!("r{}", t.bump("revision")?))?,
                    usable: workspace::usable(&path),
                    branch,
                    display: workspace::display(&path),
                };
                t.put_workspace(&WorkspaceRow {
                    resource: body.resource.clone(),
                    spec: spec.clone(),
                    kind: body.kind.clone(),
                    repo: repo.map(|r| r.to_string_lossy().to_string()),
                    view: view.clone(),
                    path: path.to_string_lossy().to_string(),
                    incarnation: t.bump("workspace")?,
                })?;
                view_out = Some(view);
                Ok(())
            });
            match (saved, view_out) {
                (Ok(()), Some(view)) => me.complete(
                    &op,
                    Decided::done(OperationResult::Workspace(view)).state,
                    |_| Ok(()),
                ),
                (Err(e), _) => me.complete(&op, failed(e), |_| Ok(())),
                (Ok(()), None) => me.complete(
                    &op,
                    failed(ApiError::new(ErrorCode::Internal, "no view")),
                    |_| Ok(()),
                ),
            }
        });
        Ok(r)
    }

    async fn workspace(&self, w: WorkspaceRef) -> ApiResult<WorkspaceView> {
        self.mine(&w)?;
        let row = self
            .0
            .j
            .read(|t| t.workspace(&w.id))?
            .ok_or_else(|| ApiError::new(ErrorCode::Invalid, "no such workspace"))?;
        let mut v = row.view;
        v.usable = workspace::usable(Path::new(&row.path));
        Ok(v)
    }

    /// Refused while something of it runs, waits or is unknown. Accepted now; finished once its
    /// harnesses are confirmed gone and its directory is removed (what could not be done is said).
    async fn tidy_workspace(&self, c: Command<Tidy>) -> ApiResult<Receipt> {
        self.mine(&c.body.workspace)?;
        let w = c.body.workspace.clone();
        let mut sessions = vec![];
        let (r, fresh) = self.accept("tidy_workspace", &c, |t| {
            for s in t.sessions_in(&w)? {
                Self::idle(t, &s)?;
                sessions.push(s);
            }
            let row = t
                .workspace(&w.id)?
                .ok_or_else(|| ApiError::new(ErrorCode::Invalid, "no such workspace"))?;
            if row.view.revision != c.body.expected {
                return Err(ApiError::new(ErrorCode::Conflict, "changed since"));
            }
            for s in &sessions {
                if let Some(mut srow) = t.session(s)? {
                    srow.view.phase = SessionPhase::Releasing;
                    t.put_session(&srow)?;
                }
            }
            let mut d = Decided::accepted();
            d.workspace = Some(w.clone());
            Ok(d)
        })?;
        if !fresh {
            return Ok(r);
        }
        let me = self.clone();
        let op = c.id.clone();
        let keep = c.body.keep_branch;
        tokio::spawn(async move {
            // the harnesses first, confirmed
            for s in &sessions {
                let (tx, rx) = tokio::sync::oneshot::channel();
                let _ = me.actor(s).send(Msg::Stop {
                    op: None,
                    done: Some(tx),
                });
                if !rx.await.unwrap_or(false) {
                    return me.complete(
                        &op,
                        failed(ApiError::new(
                            ErrorCode::Busy,
                            "a harness in it would not stop",
                        )),
                        |_| Ok(()),
                    );
                }
            }
            let row = match me.0.j.read(|t| t.workspace(&w.id)) {
                Ok(Some(r)) => r,
                _ => {
                    return me.complete(
                        &op,
                        Decided::done(OperationResult::Unit).state,
                        |_| Ok(()),
                    );
                }
            };
            let lock = row.repo.as_ref().map(|r| me.repo_lock(Path::new(r)));
            let _g = match &lock {
                Some(l) => Some(l.lock().await),
                None => None,
            };
            let (kind, path, repo, branch) = (
                row.kind.clone(),
                PathBuf::from(&row.path),
                row.repo.clone().map(PathBuf::from),
                row.view.branch.clone(),
            );
            let removed = tokio::task::spawn_blocking(move || {
                workspace::remove(&kind, &path, repo.as_deref(), branch.as_deref(), keep)
            })
            .await
            .map_err(|e| e.to_string())
            .and_then(|r| r);
            match removed {
                Ok(note) => me.complete(
                    &op,
                    Observed::Finished {
                        outcome: Outcome::Succeeded,
                        result: Some(OperationResult::Unit),
                        error: note.map(|n| ApiError::new(ErrorCode::Conflict, n)),
                    },
                    |t| {
                        t.drop_workspace(&w.id)?;
                        for s in &sessions {
                            if let Some(mut srow) = t.session(s)? {
                                srow.view.phase = SessionPhase::Released;
                                t.put_session(&srow)?;
                            }
                        }
                        Ok(())
                    },
                ),
                Err(e) => me.complete(&op, failed(ApiError::new(ErrorCode::Internal, e)), |_| {
                    Ok(())
                }),
            }
        });
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
