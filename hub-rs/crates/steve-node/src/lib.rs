//! A Steve Node: the host of harnesses on one machine (docs/hub-node-client.md).
//!
//! It runs exactly what the Hub asks, one attempt at a time per session, and reports facts. It never
//! retries, never picks the next message, never decides about permissions: the Hub does. Agent
//! protocols end here: the ACP driver (`steve_acp::session`, `normalize`, `permission`) speaks to the
//! adapter, and only Steve's events leave.
//!
//! A command is accepted in one journal transaction (the command, its state, its reservations and
//! its events), then handed to its session's [`actor`], which does the process work in order and
//! journals what came of it. A restart turns what was running into `unknown`; the Hub decides what
//! happens to it ([`ExecutionControl::abandon`]), and nothing is freed before its processes are
//! confirmed gone ([`procs`]).

mod actor;
mod exec;
mod journal;
mod procs;
mod tools;
pub mod workspace;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use serde_json::{Value, json};
use steve_acp::permission::Asked;
use steve_acp::{AcpConfig, FileRef, StoredFile};
use steve_node_api::*;
use tokio::sync::mpsc;

use actor::{Actor, Msg};
use journal::{Journal, Op, SessionRow, Tx, WorkspaceRow, internal};
pub use workspace::Root;

/// What this Node is and where it keeps its things.
pub struct NodeConfig {
    pub node: NodeId,
    /// The journal, scratch directories, worktrees and the attachment cache live here.
    pub home: PathBuf,
    pub acp: AcpConfig,
    /// The roots at open; [`LocalNode::set_roots`] replaces them.
    pub roots: Vec<Root>,
    pub capacity: Capacity,
    pub storage: Storage,
}

/// How much disk the journal may take. The limit is the quota, or less when the filesystem of the
/// home has less room than that past the reserve. At the limit the Node takes no new work
/// (`exhausted`) until its journal is back under the low-water mark; stopping, releasing and
/// answering keep a small margin past it, and the end of an attempt is always written.
#[derive(Clone)]
pub struct Storage {
    /// Live journal bytes at most (events not taken yet, command history, sessions…).
    pub quota: u64,
    /// Free space left on the home's filesystem (for the WAL, checkpoints and everything else).
    pub reserve: u64,
    /// After the limit was reached, new work is taken again below this share of it (percent).
    pub low_water: u8,
    /// Tests: the filesystem's free bytes, instead of asking it.
    #[doc(hidden)]
    pub free_space: Option<Arc<dyn Fn() -> u64 + Send + Sync>>,
}

impl Default for Storage {
    fn default() -> Storage {
        Storage {
            quota: 256 << 20,
            reserve: 512 << 20,
            low_water: 80,
            free_space: None,
        }
    }
}

/// Past the limit, what stops or answers work may still be written (bytes).
const CONTROL_MARGIN: u64 = 8 << 20;

/// High-water with hysteresis. `used`: live journal bytes; `need`: what the command adds. Once full,
/// new work waits until usage is back under `low_water` percent of the limit. Returns whether it is
/// taken and whether the journal counts as full afterwards.
fn admit(full: bool, used: u64, need: u64, limit: u64, low_water: u8) -> (bool, bool) {
    let low = limit / 100 * u64::from(low_water.min(100));
    if full && used >= low {
        return (false, true);
    }
    if used.saturating_add(need) >= limit {
        return (false, true);
    }
    (true, false)
}

/// How long a delivery may take, and how often an idle Node checks the Hub is there.
const HUB_TIMEOUT: Duration = Duration::from_secs(15);
const HEARTBEAT: Duration = Duration::from_secs(10);

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
    /// The roots allowed now (set at open, replaced by the host).
    roots: std::sync::RwLock<Arc<Vec<Root>>>,
    j: Journal,
    incarnation: u64,
    _lock: HomeLock,
    generation: AtomicU64,
    actors: Mutex<HashMap<SessionId, mpsc::UnboundedSender<Msg>>>,
    sink: Mutex<Option<Arc<dyn EventSink>>>,
    files: Mutex<Option<Arc<dyn HubFiles>>>,
    wake: tokio::sync::Notify,
    delivering: AtomicBool,
    closed: AtomicBool,
    /// The journal failed: no new work until a restart.
    faulted: AtomicBool,
    /// The journal reached its limit: no new work until it is under the low-water mark.
    full: AtomicBool,
    /// Since when the Hub does not take events (for the offline lease).
    hub_down_since: Mutex<Option<i64>>,
    /// What each session's harness sent during its running attempt (bytes, as it arrived).
    output: Mutex<HashMap<SessionId, Arc<AtomicU64>>>,
    tools: Arc<tools::Endpoint>,
    tools_url: Mutex<Option<String>>,
    /// Workspace work on one repository goes one at a time.
    repos: Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>,
    snapshots: Mutex<HashMap<u64, Snapshot>>,
    cache_n: AtomicU64,
    /// Commands ([`exec`]) running now, against `capacity.execs`.
    execs: AtomicUsize,
    tasks: Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

impl Inner {
    pub(crate) fn roots(&self) -> Arc<Vec<Root>> {
        self.roots.read().unwrap().clone()
    }
    pub(crate) fn fault(&self) {
        self.faulted.store(true, Ordering::SeqCst);
    }
    pub(crate) fn faulted(&self) -> bool {
        self.faulted.load(Ordering::SeqCst)
    }
    pub(crate) fn hub_down_since(&self) -> Option<i64> {
        *self.hub_down_since.lock().unwrap()
    }
    pub(crate) fn output_counter(&self, session: &SessionId) -> Arc<AtomicU64> {
        self.output
            .lock()
            .unwrap()
            .entry(session.clone())
            .or_default()
            .clone()
    }
    pub(crate) fn output(&self, session: &SessionId) -> u64 {
        self.output_counter(session).load(Ordering::SeqCst)
    }
    pub(crate) fn reset_output(&self, session: &SessionId) {
        self.output_counter(session).store(0, Ordering::SeqCst);
    }

    /// The journal's limit now (bytes of live pages): the quota, or the room the filesystem has
    /// past the reserve. Pages SQLite freed inside the file count as room.
    pub(crate) fn journal_limit(&self, t: &Tx) -> ApiResult<u64> {
        let s = &self.cfg.storage;
        let free = match &s.free_space {
            Some(f) => Some(f()),
            None => journal::free_bytes(&self.cfg.home),
        };
        let Some(free) = free else {
            return Ok(s.quota);
        };
        let room = t
            .file_bytes()?
            .saturating_add(free)
            .saturating_sub(s.reserve);
        Ok(s.quota.min(room))
    }

    /// The journal is at its limit (and then counts as full): running work stops.
    pub(crate) fn over_limit(&self, t: &Tx) -> ApiResult<bool> {
        let over = t.payload_bytes()? >= self.journal_limit(t)?;
        if over {
            self.full.store(true, Ordering::SeqCst);
        }
        Ok(over)
    }
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

/// The stream a subject's events go to.
pub(crate) fn stream_of(subject: &Subject) -> String {
    match subject {
        Subject::Session { session } => derived_id("session", session.as_str()),
        Subject::Execution(ExecutionRef::Turn { session, .. }) => {
            derived_id("session", session.as_str())
        }
        Subject::Execution(ExecutionRef::Job { job, .. }) => derived_id("job", job.as_str()),
        Subject::Operation { operation } => derived_id("op", operation.as_str()),
    }
}

/// Journal events in `t` (numbered in their streams as part of the transaction).
pub(crate) fn emit_all(inner: &Inner, t: &Tx, evs: Vec<(Subject, EventBody)>) -> ApiResult<()> {
    let mut limit = None;
    for (subject, body) in evs {
        let data = match &body {
            EventBody::OperationChanged { .. }
            | EventBody::AttemptEnded { .. }
            | EventBody::PermissionExpired { .. }
            | EventBody::PermissionResolved { .. }
            | EventBody::SessionChanged { .. }
            | EventBody::MessageCompleted { .. } => false,
            EventBody::Usage { report } => report.measurement != Measurement::Final,
            _ => true,
        };
        if data {
            let limit = match limit {
                Some(l) => l,
                None => *limit.insert(inner.journal_limit(t)?),
            };
            if t.payload_bytes()?
                .saturating_add(serde_json::to_vec(&body).map_err(internal)?.len() as u64 + 16384)
                > limit
            {
                inner.full.store(true, Ordering::SeqCst);
                return Err(ApiError::new(
                    ErrorCode::Exhausted,
                    "journal data budget reached",
                ));
            }
        }
        let stream: StreamId = id(stream_of(&subject))?;
        let n = t.bump("event")?;
        let event_id: EventId = id(format!("ev_{}_{}_{n}", inner.j.id(), inner.incarnation))?;
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
        &inner.roots(),
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
        PermissionMode::Auto => "auto",
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
/// and hash checked; written to a file of its own, then published whole).
async fn fetch(inner: &Arc<Inner>, exec: &ExecutionRef, a: &ArtifactRef) -> ApiResult<PathBuf> {
    use sha2::Digest as _;
    let internal = |e: std::io::Error| ApiError::new(ErrorCode::Internal, e.to_string());
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
    std::fs::create_dir_all(&dir).map_err(internal)?;
    // a temporary file of this call only, then published in one rename
    let n = inner.cache_n.fetch_add(1, Ordering::SeqCst);
    let tmp = dir.join(format!(
        ".{}.{}.{n}.{}",
        path.file_name().unwrap_or_default().to_string_lossy(),
        inner.incarnation,
        std::process::id()
    ));
    {
        use std::io::Write;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&tmp)
            .map_err(internal)?;
        f.write_all(&bytes).map_err(internal)?;
        f.sync_all().map_err(internal)?;
    }
    std::fs::rename(&tmp, &path).map_err(internal)?;
    Ok(path)
}

/// HEAD of the workspace's repository: `Ok(None)` when it is no repository, an error when it is one
/// and git cannot tell.
fn head_of(dir: &Path) -> ApiResult<Option<String>> {
    head_with(dir, Path::new("git"), Duration::from_secs(3))
}

fn head_with(dir: &Path, program: &Path, timeout: Duration) -> ApiResult<Option<String>> {
    use std::io::Read;
    use std::os::{fd::AsRawFd, unix::process::CommandExt};
    let is_repo = dir.join(".git").exists();
    let mut child = std::process::Command::new(program)
        .args(["rev-parse", "HEAD"])
        .current_dir(dir)
        .process_group(0)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .spawn()
        .map_err(internal)?;
    let mut pipe = child.stdout.take().expect("pipe");
    let flags = unsafe { libc::fcntl(pipe.as_raw_fd(), libc::F_GETFL) };
    if flags < 0
        || unsafe { libc::fcntl(pipe.as_raw_fd(), libc::F_SETFL, flags | libc::O_NONBLOCK) } < 0
    {
        let _ = child.kill();
        let _ = child.wait();
        return Err(internal("cannot bound HEAD pipe"));
    }
    let until = std::time::Instant::now() + timeout;
    let mut output = Vec::new();
    loop {
        let mut bytes = [0; 4096];
        for _ in 0..16 {
            match pipe.read(&mut bytes) {
                Ok(0) | Err(_) => break,
                Ok(n) => output.extend_from_slice(&bytes[..n]),
            }
        }
        if std::time::Instant::now() >= until || output.len() > 65536 {
            // The direct child has not been reaped: its pid/group cannot be recycled yet.
            unsafe {
                libc::kill(-(child.id() as i32), libc::SIGKILL);
            }
            let _ = child.kill();
            let _ = child.wait();
            return Err(ApiError::new(
                ErrorCode::Stale,
                "HEAD probe timed out or exceeded output budget",
            ));
        }
        match child.try_wait().map_err(internal)? {
            Some(status) => {
                // Do not wait for descendants holding a pipe after the direct child exits.
                while let Ok(n) = pipe.read(&mut bytes) {
                    if n == 0 || output.len() + n > 65536 {
                        break;
                    }
                    output.extend_from_slice(&bytes[..n]);
                }
                return if status.success() {
                    Ok(Some(String::from_utf8_lossy(&output).trim().to_string()))
                } else if !is_repo {
                    Ok(None)
                } else {
                    Err(internal("cannot read HEAD"))
                };
            }
            None => std::thread::sleep(Duration::from_millis(10)),
        }
    }
}

/// A file's version as the Node sees it (size, modification and change time, inode): `Ok(None)`
/// when it does not exist, an error when it cannot be read.
fn file_version(p: &Path) -> ApiResult<Option<Revision>> {
    use std::os::unix::fs::MetadataExt;
    match std::fs::symlink_metadata(p) {
        Ok(m) => {
            let resolved = p.canonicalize().map_err(internal)?;
            let target = std::fs::metadata(&resolved).map_err(internal)?;
            let signature = format!(
                "{:?}:{:?}:{:?}",
                resolved,
                (
                    m.dev(),
                    m.ino(),
                    m.len(),
                    m.mtime(),
                    m.mtime_nsec(),
                    m.ctime(),
                    m.ctime_nsec()
                ),
                (
                    target.dev(),
                    target.ino(),
                    target.len(),
                    target.mtime(),
                    target.mtime_nsec(),
                    target.ctime(),
                    target.ctime_nsec()
                )
            );
            Ok(Some(
                Revision::new(format!(
                    "v{}",
                    Digest::sha256(signature.as_bytes())
                        .as_str()
                        .replace(':', "_")
                ))
                .map_err(internal)?,
            ))
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(ApiError::new(
            ErrorCode::Internal,
            format!("cannot stat: {e}"),
        )),
    }
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

/// `p` (absolute, or relative to `dir`) is inside the workspace `dir` once symlinks are followed:
/// the path itself if it exists, else its nearest existing parent. Returns its typed reference and
/// where it is on disk.
fn inside(dir: &Path, w: &WorkspaceRef, p: &str) -> Option<(PathRef, PathBuf)> {
    let path = Path::new(p);
    let rel = if path.is_absolute() {
        path.strip_prefix(dir).ok()?.to_path_buf()
    } else {
        path.to_path_buf()
    };
    if rel.components().any(|c| {
        !matches!(
            c,
            std::path::Component::Normal(_) | std::path::Component::CurDir
        )
    }) {
        return None;
    }
    let abs = dir.join(&rel);
    let real_dir = dir.canonicalize().ok()?;
    let mut probe = dir.to_path_buf();
    for component in rel.components() {
        probe.push(component);
        match std::fs::symlink_metadata(&probe) {
            Ok(_) => {
                // Existing links must resolve, including the final component. A dangling link
                // is not a nonexistent ordinary file and cannot be approved as one.
                if !probe.canonicalize().ok()?.starts_with(&real_dir) {
                    return None;
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return None,
        }
    }
    Some((
        PathRef {
            workspace: w.clone(),
            rel: RelPath::new(rel.to_string_lossy().to_string()).ok()?,
        },
        abs,
    ))
}

/// Owned read-only binding inputs. Blocking filesystem probes never retain the Node/home lock.
pub(crate) struct PermissionContext {
    workspace: WorkspaceRef,
    row: WorkspaceRow,
    roots: Arc<Vec<Root>>,
    /// The session's harness process group, to tell which processes it started.
    proc: Option<journal::Proc>,
}
impl PermissionContext {
    fn dir(&self) -> ApiResult<PathBuf> {
        workspace::still_valid(
            Path::new(&self.row.path),
            &self.roots,
            matches!(self.row.kind, PrepareKind::Existing { .. }),
        )
    }
}
pub(crate) fn permission_context(
    inner: &Inner,
    session: &SessionId,
) -> ApiResult<PermissionContext> {
    inner.j.read(|t| {
        let proc = t.proc_of(session)?;
        let session = t
            .session(session)?
            .ok_or_else(|| internal("session gone"))?;
        let row = t
            .workspace(&session.open.workspace.id)?
            .ok_or_else(|| internal("workspace gone"))?;
        Ok(PermissionContext {
            workspace: session.open.workspace,
            row,
            roots: inner.roots(),
            proc,
        })
    })
}

/// A harness's permission request as Steve's: its facts bound to this execution and workspace
/// (paths inside it are typed, anything outside it or not fully known is said so: an explicit
/// unknown), with what must still hold when the answer is used (the workspace's revision, HEAD of
/// its repository, each target file's version).
pub(crate) fn bind_permission(
    context: &PermissionContext,
    exec: &ExecutionRef,
    op: &OperationId,
    rpc_id: &Value,
    asked: &Asked,
    deadline: Timestamp,
) -> ApiResult<PermissionRequest> {
    use steve_acp::normalize::CommandFact;
    let w = context.workspace.clone();
    let dir = context.dir()?;
    let fact = &asked.fact;
    let bound: Vec<Option<(PathRef, PathBuf)>> =
        fact.paths.iter().map(|p| inside(&dir, &w, p)).collect();
    let all_inside = bound.iter().all(Option::is_some);
    let refs: Vec<PathRef> = bound.iter().flatten().map(|(r, _)| r.clone()).collect();
    let unknown = |what: &str| Action::Unknown {
        summary: format!("{what}：{}", fact.paths.join(", ")),
    };
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
            match (command, fact.cwd.as_deref().map(|c| inside(&dir, &w, c))) {
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
        ToolCategory::Edit => unknown("写入（工作区外或未知）"),
        ToolCategory::Delete if all_inside && !refs.is_empty() => Action::Delete {
            paths: refs.clone(),
        },
        ToolCategory::Delete => unknown("删除（工作区外或未知）"),
        // the facts do not say which path is the source: never guessed
        ToolCategory::Move => unknown("移动"),
        ToolCategory::Read if all_inside && !refs.is_empty() => Action::Read {
            paths: refs.clone(),
        },
        ToolCategory::Read => unknown("读取（工作区外或未知）"),
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
    let mut pre = vec![Precondition::Workspace {
        workspace: w.clone(),
        revision: context.row.view.revision.clone(),
    }];
    // Bind every path, its parent chain and the workspace root, including Read and Exec.cwd.
    // Absence is bound to its existing parents, so replacing a parent invalidates approval.
    let mut paths: Vec<PathRef> = refs;
    if let Action::Exec { cwd, .. } = &action {
        paths.push(cwd.clone().unwrap_or(PathRef {
            workspace: w.clone(),
            rel: RelPath::new("").map_err(internal)?,
        }));
    }
    let mut seen = std::collections::HashSet::new();
    for path in paths {
        let mut rel = PathBuf::from(path.rel.as_str());
        loop {
            let text = rel.to_string_lossy().to_string();
            if seen.insert(text.clone()) {
                pre.push(Precondition::File {
                    path: PathRef {
                        workspace: w.clone(),
                        rel: RelPath::new(text).map_err(internal)?,
                    },
                    version: file_version(&dir.join(&rel))?,
                });
            }
            if !rel.pop() {
                break;
            }
        }
    }
    if matches!(
        action,
        Action::Exec { .. } | Action::Write { .. } | Action::Delete { .. }
    ) && let Some(head) = head_of(&dir)?
    {
        pre.push(Precondition::Head {
            workspace: w.clone(),
            commit: head,
        });
    }
    let digest = permission_digest(&action, &pre)?;
    // Which processes are the harness's own, for judging `kill <pid>` (not part of the digest).
    let own_pids = match &action {
        Action::Exec { command, .. } if command.contains("kill") => context
            .proc
            .as_ref()
            .map(|p| procs::started_by(p.pgid, p.started))
            .unwrap_or_default(),
        _ => vec![],
    };
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
        own_pids,
    })
}

/// The preconditions of a request still hold (checked off the journal right before an approval is
/// given to the harness). Files can change between this check and the harness's own write: a
/// window no lock of the Node closes, as the protocol says.
pub(crate) fn preconditions_hold(
    context: &PermissionContext,
    pre: &[Precondition],
) -> ApiResult<()> {
    for p in pre {
        let ok = match p {
            Precondition::Workspace {
                workspace,
                revision,
            } => context.workspace == *workspace && context.row.view.revision == *revision,
            Precondition::Head { workspace, commit } => {
                context.workspace == *workspace
                    && head_of(&context.dir()?)?.as_deref() == Some(commit.as_str())
            }
            Precondition::File { path, version } => {
                let dir = context.dir()?;
                context.workspace == path.workspace
                    && inside(&dir, &path.workspace, path.rel.as_str()).is_some()
                    && file_version(&dir.join(path.rel.as_str()))? == *version
            }
        };
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

/// A bound workspace's directory on this machine, for a Hub running on the same machine as its
/// Node (see [`LocalNode::local_dir`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LocalDir {
    /// Canonical, checked against the Node's roots when asked.
    pub path: PathBuf,
    /// The Node made it (a worktree or scratch dir), rather than adopting an existing directory.
    pub owned: bool,
}

impl LocalNode {
    /// Takes the home (one Node per journal), opens the journal (a new incarnation: what was running
    /// is `unknown` now) and stops nothing: groups left by the old incarnation keep their sessions
    /// held until the Hub abandons them.
    pub fn open(cfg: NodeConfig) -> ApiResult<LocalNode> {
        std::fs::create_dir_all(&cfg.home)
            .map_err(|e| ApiError::new(ErrorCode::Internal, format!("node home: {e}")))?;
        let lock = HomeLock::take(&cfg.home)?;
        let (j, incarnation) = Journal::open(&cfg.home.join("node.sqlite3"))?;
        // a group of the old incarnation that is surely gone holds nothing
        j.tx(|t| {
            for p in t.procs()? {
                if p.incarnation < incarnation && procs::state(p.pgid, p.started).gone() {
                    t.drop_proc(&p.session, p.pgid)?;
                }
            }
            Ok(())
        })?;
        Ok(LocalNode(Arc::new_cyclic(|weak| Inner {
            roots: std::sync::RwLock::new(Arc::new(cfg.roots.clone())),
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
            closed: AtomicBool::new(false),
            faulted: AtomicBool::new(false),
            full: AtomicBool::new(false),
            hub_down_since: Mutex::new(Some(now())),
            output: Mutex::new(HashMap::new()),
            tools: tools::Endpoint::new(Arc::new(CurrentOf(weak.clone()))),
            tools_url: Mutex::new(None),
            repos: Mutex::new(HashMap::new()),
            snapshots: Mutex::new(HashMap::new()),
            cache_n: AtomicU64::new(0),
            execs: AtomicUsize::new(0),
            tasks: Mutex::new(Vec::new()),
        })))
    }

    pub fn incarnation(&self) -> u64 {
        self.0.incarnation
    }

    /// Tests: caps the journal file at `pages` (`PRAGMA max_page_count`), so the next write that
    /// needs a new page fails as on a full disk. Returns the cap SQLite took.
    #[doc(hidden)]
    pub fn cap_journal_pages(&self, pages: u64) -> ApiResult<u64> {
        self.0.j.cap_pages(pages)
    }

    /// Where events go and where attachments come from (the Hub). Starts delivering what is waiting.
    pub fn connect(&self, sink: Arc<dyn EventSink>, files: Option<Arc<dyn HubFiles>>) {
        *self.0.sink.lock().unwrap() = Some(sink);
        *self.0.files.lock().unwrap() = files;
        if !self.0.delivering.swap(true, Ordering::SeqCst) {
            self.spawn_owned(deliver_loop(Arc::downgrade(&self.0)));
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

    /// Stops every harness (confirmed or recorded as not stopped) and ends the Node's tasks; the
    /// home is free once the last handle is dropped.
    pub async fn shutdown(&self) {
        self.0.closed.store(true, Ordering::SeqCst);
        self.0.wake.notify_one();
        let tasks = std::mem::take(&mut *self.0.tasks.lock().unwrap());
        for task in tasks {
            let _ = task.await;
        }
        let actors: Vec<_> = self.0.actors.lock().unwrap().drain().collect();
        for (_, tx) in actors {
            let (done, wait) = tokio::sync::oneshot::channel();
            if tx.send(Msg::Shutdown { done }).is_ok() {
                let _ = wait.await;
            }
        }
        // All effects have quiesced. Preserve unresolved outcomes for explicit Hub recovery.
        let _ = self.0.j.tx(|t| t.mark_unknown());
        self.0.wake.notify_one();
    }

    /// False: the Node is shutting down and `work` was not started.
    fn spawn_owned(&self, work: impl std::future::Future<Output = ()> + Send + 'static) -> bool {
        let mut tasks = self.0.tasks.lock().unwrap();
        if self.0.closed.load(Ordering::SeqCst) {
            return false;
        }
        tasks.retain(|task| !task.is_finished());
        tasks.push(tokio::spawn(work));
        true
    }

    fn actor(&self, session: &SessionId) -> mpsc::UnboundedSender<Msg> {
        let mut a = self.0.actors.lock().unwrap();
        if self.0.closed.load(Ordering::SeqCst) {
            let (tx, rx) = mpsc::unbounded_channel();
            drop(rx);
            return tx;
        }
        if let Some(tx) = a.get(session).filter(|t| !t.is_closed()) {
            return tx.clone();
        }
        let tx = Actor::spawn(self.0.clone(), session.clone());
        a.insert(session.clone(), tx.clone());
        tx
    }

    /// Host bridge for a Node on the Hub's own machine: where `w` is, so the Hub can judge a
    /// permission request against the real directory. Not on the [`Node`] trait and never sent over
    /// the transport: a remote Node has none, and the Hub then asks. `None` when the workspace is
    /// another Node's, unknown, gone or no longer inside the roots.
    pub fn local_dir(&self, w: &WorkspaceRef) -> Option<LocalDir> {
        self.mine(w).ok()?;
        let row = self.0.j.read(|t| t.workspace(&w.id)).ok()??;
        let existing = matches!(row.kind, PrepareKind::Existing { .. });
        let path = workspace::still_valid(Path::new(&row.path), &self.0.roots(), existing).ok()?;
        Some(LocalDir {
            path,
            owned: !existing,
        })
    }

    /// Host bridge: whether a prepare of `at` would pass the roots check now (a read-write root
    /// this Node has now, the directory there and inside it), so the Hub plans no session it
    /// would refuse. Reads the filesystem, creates nothing.
    pub fn allows(&self, at: &RootPath) -> bool {
        workspace::resolve(&self.0.roots(), at, true).is_ok()
    }

    /// Host bridge, like [`Self::local_dir`]: replaces the roots the Hub may name, for a Node on the
    /// Hub's own machine whose roots follow the Hub's projects and knowledge folders. Never sent over
    /// the transport (a remote Node's owner configures its roots). Takes effect for the next
    /// command; a workspace prepared under a root that is gone now is refused when it is used.
    pub fn set_roots(&self, roots: Vec<Root>) {
        *self.0.roots.write().unwrap() = Arc::new(roots);
    }

    fn mine(&self, w: &WorkspaceRef) -> ApiResult<()> {
        if w.node != self.0.cfg.node {
            return Err(ApiError::new(ErrorCode::Denied, "another node's workspace"));
        }
        Ok(())
    }

    /// The command inbox. Known id: same method and content → its receipt (even past the window),
    /// otherwise conflict. New: past the window → stale; a closed or faulted Node takes nothing new;
    /// `decide` checks against the journal and changes it, in the same transaction as the command
    /// and its events. Returns whether it was new.
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
            if self.0.closed.load(Ordering::SeqCst) {
                return Err(ApiError::new(
                    ErrorCode::Offline,
                    "the node is shutting down",
                ));
            }
            if self.0.faulted()
                && !matches!(method, "cancel" | "abandon" | "recover" | "release_session")
            {
                return Err(ApiError::new(
                    ErrorCode::Internal,
                    "the node's journal failed: restart it",
                ));
            }
            let control = matches!(
                method,
                "cancel" | "abandon" | "recover" | "release_session" | "resolve_permission"
            );
            let bytes = serde_json::to_vec(&c.body).map_err(internal)?.len() as u64;
            let (used, limit) = (t.payload_bytes()?, self.0.journal_limit(t)?);
            let taken = if control {
                used.saturating_add(bytes) < limit.saturating_add(CONTROL_MARGIN)
            } else {
                let full = self.0.full.load(Ordering::SeqCst);
                let (taken, full) = admit(full, used, bytes, limit, self.0.cfg.storage.low_water);
                self.0.full.store(full, Ordering::SeqCst);
                taken
            };
            if bytes > 1 << 20 || !taken {
                return Err(ApiError::new(
                    ErrorCode::Exhausted,
                    "journal admission budget reached",
                ));
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
            t.save_spec(&c.id, &c.body)?;
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
        // reads are not confined at all without a sandbox: a list of what may be read cannot be kept
        if !p.readable.is_empty() {
            return Err(ApiError::new(
                ErrorCode::Unsupported,
                "this node cannot confine what a harness reads",
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

    /// The workspace is not being prepared or tidied by another operation.
    fn workspace_free(t: &Tx, resource: &str) -> ApiResult<()> {
        if t.reservation(resource)?.is_some() {
            return Err(ApiError::new(
                ErrorCode::Busy,
                "the workspace is being prepared or tidied",
            ));
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

    /// Finish an operation that ran outside the inbox transaction, with its event and `more` in the
    /// same transaction.
    async fn complete(
        &self,
        op: &OperationId,
        state: Observed,
        more: impl Fn(&Tx) -> ApiResult<()>,
    ) {
        loop {
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
                self.0.fault();
                if self.0.closed.load(Ordering::SeqCst) {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            } else {
                return;
            }
        }
    }
}

/// Delivers journaled events to the Hub. Every call is bounded; while there is nothing to send the
/// Node still checks the Hub is there (an empty batch), so a Hub that went silent is noticed for the
/// offline lease.
async fn deliver_loop(inner: std::sync::Weak<Inner>) {
    let mut backoff = Duration::from_millis(100);
    let mut last_ok = std::time::Instant::now();
    loop {
        let Some(me) = inner.upgrade() else { return };
        if me.closed.load(Ordering::SeqCst) {
            return;
        }
        let batch = me.j.read(|t| t.unacked(200)).unwrap_or_else(|e| {
            tracing::error!("node delivery: {e}");
            vec![]
        });
        if batch.is_empty() && last_ok.elapsed() < HEARTBEAT {
            let _ = tokio::time::timeout(Duration::from_secs(2), me.wake.notified()).await;
            continue;
        }
        let sink = me.sink.lock().unwrap().clone();
        let Some(sink) = sink else {
            me.hub_down_since.lock().unwrap().get_or_insert_with(now);
            drop(me);
            tokio::time::sleep(Duration::from_millis(200)).await;
            continue;
        };
        let sent =
            tokio::time::timeout(HUB_TIMEOUT, sink.commit(EventBatch { events: batch })).await;
        match sent {
            Ok(Ok(ack)) => {
                last_ok = std::time::Instant::now();
                *me.hub_down_since.lock().unwrap() = None;
                let progress = me.j.tx(|t| {
                    let before: u64 = t.heads()?.iter().map(|h| h.acknowledged).sum();
                    t.ack(&ack)?;
                    let after: u64 = t.heads()?.iter().map(|h| h.acknowledged).sum();
                    Ok(after > before)
                });
                if matches!(progress, Ok(true)) {
                    backoff = Duration::from_millis(100);
                } else {
                    if let Err(e) = progress {
                        tracing::error!("node ack: {e}");
                    }
                    drop(me);
                    tokio::time::sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_secs(10));
                }
            }
            Ok(Err(e)) => {
                tracing::warn!("hub did not take events: {e}");
                me.hub_down_since.lock().unwrap().get_or_insert_with(now);
                drop(me);
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(10));
            }
            Err(_) => {
                tracing::warn!("hub did not answer");
                me.hub_down_since.lock().unwrap().get_or_insert_with(now);
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
        // a Node opened for its workspace faces only runs no harness
        if self.0.cfg.capacity.sessions == 0 {
            return Err(ApiError::new(
                ErrorCode::Unsupported,
                "this node runs no sessions",
            ));
        }
        let (r, fresh) = self.accept("open_session", &c, |t| {
            let o = &c.body;
            self.check_harness(&o.choice.harness)?;
            Self::check_policy(&o.policy)?;
            let w = t
                .workspace(&o.workspace.id)?
                .ok_or_else(|| ApiError::new(ErrorCode::Invalid, "no such workspace"))?;
            Self::workspace_free(t, &w.resource)?;
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
                revision: id(t.revision()?)?,
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
            Self::workspace_free(t, &w.resource)?;
            Self::check_policy(&tu.run.policy)?;
            Self::check_limits(&tu.run.limits)?;
            Self::idle(t, &tu.session)?;
            if t.unacked_bytes()? > self.0.cfg.storage.quota {
                return Err(ApiError::new(
                    ErrorCode::Exhausted,
                    "the hub has not taken this node's events: no new work until it does",
                ));
            }
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
                abandon: false,
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
    /// interrupted and its session is free to be opened again. One abandon per target; only
    /// executions are abandoned (other unknown operations are found out otherwise).
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
            let Some(ExecutionRef::Turn { session: s, .. }) = &op.execution else {
                return Err(ApiError::new(
                    ErrorCode::Unsupported,
                    "only an execution is abandoned",
                ));
            };
            if t.reservation(&format!("abandon:{target}"))?.is_some() {
                return Err(ApiError::new(
                    ErrorCode::Conflict,
                    "this execution is being abandoned already",
                ));
            }
            t.reserve(&format!("abandon:{target}"), &c.id)?;
            session = Some(s.clone());
            Ok(Decided::accepted())
        })?;
        if fresh && let Some(s) = session {
            let me = self.clone();
            let op = c.id.clone();
            self.spawn_owned(async move {
                let (tx, rx) = tokio::sync::oneshot::channel();
                let _ = me.actor(&s).send(Msg::Stop {
                    op: None,
                    abandon: true,
                    done: Some(tx),
                });
                let stopped = rx.await.unwrap_or(procs::Stopped::Uncertain);
                let key = format!("abandon:{target}");
                if !stopped.gone() {
                    return me
                        .complete(
                            &op,
                            failed(ApiError::new(
                                ErrorCode::Busy,
                                "its processes would not stop (or are not this node's to stop)",
                            )),
                            |t| t.unreserve(&key),
                        )
                        .await;
                }
                let inner = me.0.clone();
                me.complete(&op, Decided::done(OperationResult::Unit).state, |t| {
                    t.unreserve(&key)?;
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
                        job: None,
                    };
                    // only if it still is the unknown one this was for
                    if !matches!(o.receipt.state, Observed::Unknown { .. }) {
                        return Ok(());
                    }
                    t.set_state(
                        &target,
                        &Observed::Finished {
                            outcome: Outcome::Interrupted,
                            result: Some(OperationResult::Execution(completion.clone())),
                            error: completion.error.clone(),
                        },
                    )?;
                    if let Some(exec) = &o.execution {
                        for wait in t.waits_of(&target)? {
                            t.drop_wait(&wait.id)?;
                            emit_all(
                                &inner,
                                t,
                                vec![(
                                    Subject::Execution(exec.clone()),
                                    EventBody::PermissionExpired { id: wait.id },
                                )],
                            )?;
                        }
                        emit_all(
                            &inner,
                            t,
                            vec![(
                                Subject::Execution(exec.clone()),
                                EventBody::AttemptEnded { completion },
                            )],
                        )?;
                    }
                    if let Some(mut row) = t.session(&s)?
                        && row.view.active == o.execution
                    {
                        row.view.phase = SessionPhase::Released;
                        row.view.active = None;
                        t.put_session(&row)?;
                    }
                    Ok(())
                })
                .await;
            });
        }
        Ok(r)
    }

    async fn recover(&self, c: Command<Recover>) -> ApiResult<Receipt> {
        let target = c.body.target.clone();
        let key = format!("recover:{target}");
        let mut session = None;
        let (receipt, fresh) = self.accept("recover", &c, |t| {
            let op = t.op(&target)?.ok_or_else(|| internal("unknown target"))?;
            if op.digest != c.body.expected || !matches!(op.receipt.state, Observed::Unknown { .. })
            {
                return Err(ApiError::new(ErrorCode::Stale, "recovery target changed"));
            }
            if op.execution.is_some() {
                return unsupported("recover execution: use abandon");
            }
            if t.spec(&target)?.is_none()
                && matches!(op.method.as_str(), "prepare_workspace" | "tidy_workspace")
            {
                return unsupported("legacy workspace control has no process evidence");
            }
            session = t.session_of(&target)?;
            if session.is_none()
                && op.method == "abandon"
                && let Some(spec) = t.spec(&target)?
                && let Some(target) = spec.get("target").and_then(Value::as_str)
            {
                session = t.session_of(&OperationId::new(target).map_err(internal)?)?;
            }
            if let Some(session) = &session {
                for (_, other) in t.open_ops_of(session)? {
                    if !matches!(other.receipt.state, Observed::Unknown { .. }) {
                        return Err(ApiError::new(ErrorCode::Busy, "session has live work"));
                    }
                }
            }
            t.reserve(&key, &c.id)?;
            Ok(Decided::accepted())
        })?;
        if !fresh {
            return Ok(receipt);
        }
        let me = self.clone();
        self.spawn_owned(async move {
            let mut stopped = true;
            if let Some(session) = &session {
                let (tx, rx) = tokio::sync::oneshot::channel();
                let _ = me.actor(session).send(Msg::Stop {
                    op: None,
                    abandon: false,
                    done: Some(tx),
                });
                stopped = rx.await.is_ok_and(procs::Stopped::gone);
            }
            let process = workspace::control_session(&target);
            match me.0.j.read(|t| t.proc_of(&process)) {
                Ok(Some(p)) => {
                    if procs::stop(p.pgid, p.started).await.gone() {
                        stopped &= me.0.j.tx(|t| t.drop_proc(&process, p.pgid)).is_ok();
                    } else {
                        stopped = false;
                    }
                }
                Ok(None) => {}
                Err(_) => stopped = false,
            }
            if !stopped {
                me.complete(
                    &c.id,
                    failed(ApiError::new(
                        ErrorCode::OutcomeUnknown,
                        "recovery could not prove stopped",
                    )),
                    |t| t.unreserve(&key),
                )
                .await;
                return;
            }
            me.complete(&c.id, Decided::done(OperationResult::Unit).state, |t| {
                let op = t.op(&target)?.ok_or_else(|| internal("target gone"))?;
                if op.digest != c.body.expected
                    || !matches!(op.receipt.state, Observed::Unknown { .. })
                {
                    return Err(ApiError::new(ErrorCode::Stale, "recovery target changed"));
                }
                let state = Observed::Finished {
                    outcome: Outcome::Interrupted,
                    result: None,
                    error: Some(ApiError::new(
                        ErrorCode::OutcomeUnknown,
                        "control retired; effects preserved, not replayed",
                    )),
                };
                t.set_state(&target, &state)?;
                t.unreserve_owned(&target)?;
                t.unreserve(&key)?;
                t.drop_answer(&target)?;
                let mut events = vec![(
                    Subject::Operation {
                        operation: target.clone(),
                    },
                    EventBody::OperationChanged {
                        receipt: Receipt {
                            operation: target.clone(),
                            state,
                        },
                    },
                )];
                if matches!(
                    op.method.as_str(),
                    "open_session" | "configure_session" | "release_session"
                ) && let Some(session) = &session
                    && let Some(mut row) = t.session(session)?
                {
                    row.view.phase = SessionPhase::Released;
                    t.put_session(&row)?;
                    events.push((
                        Subject::Session {
                            session: session.clone(),
                        },
                        EventBody::SessionChanged { view: row.view },
                    ));
                }
                emit_all(&me.0, t, events)
            })
            .await;
        });
        Ok(receipt)
    }

    /// Accepted now (checked against the waiting request); finished once the answer is given to the
    /// harness, after its preconditions were checked again right before.
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
            if t.answer_pending_for(&res.id)? {
                return Err(ApiError::new(
                    ErrorCode::Conflict,
                    "another answer to this request is being given",
                ));
            }
            t.put_answer(&c.id, &res)?;
            session = Some(s);
            Ok(Decided::accepted())
        })?;
        if fresh && let Some(s) = session {
            let _ = self.actor(&s).send(Msg::Answer { op: c.id.clone() });
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
    /// The same resource and spec: the workspace there is. A new one: reserved and accepted now,
    /// finished once it is made (or adopted after checking it is what was asked), the workspace and
    /// the receipt in one transaction.
    async fn prepare_workspace(&self, c: Command<Prepare>) -> ApiResult<Receipt> {
        let spec = digest_of(&c.body.kind)?;
        let wid: WorkspaceId = id(derived_id("w", &c.body.resource))?;
        let (r, fresh) = self.accept("prepare_workspace", &c, |t| {
            Self::workspace_free(t, &c.body.resource)?;
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
                    workspace::resolve(&self.0.roots(), dir, true)?;
                }
                PrepareKind::Worktree { repository, .. } => {
                    workspace::resolve(&self.0.roots(), repository, true)?;
                }
                PrepareKind::Scratch => {}
            }
            t.reserve(&c.body.resource, &c.id)?;
            Ok(Decided::accepted())
        })?;
        if !fresh || r.state.is_finished() {
            return Ok(r);
        }
        let me = self.clone();
        let op = c.id.clone();
        let body = c.body.clone();
        self.spawn_owned(async move {
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
                    workspace::resolve(&me.0.roots(), dir, true).map(|p| (p, None, None))
                }
                PrepareKind::Worktree {
                    repository,
                    branch,
                    base,
                } => match workspace::resolve(&me.0.roots(), repository, true) {
                    Ok(repo) => {
                        let lock = me.repo_lock(&repo);
                        let _g = lock.lock().await;
                        let p = home.join("worktrees").join(&name);
                        let (r2, p2, b2, base2) =
                            (repo.clone(), p.clone(), branch.clone(), base.clone());
                        let commands = workspace::Commands {
                            inner: me.0.clone(),
                            op: op.clone(),
                        };
                        tokio::task::spawn_blocking(move || {
                            workspace::add_worktree(&commands, &r2, &p2, &b2, base2.as_deref())
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
            let resource = body.resource.clone();
            if me
                .0
                .j
                .read(|t| t.proc_of(&workspace::control_session(&op)))
                .map_or(true, |p| p.is_some())
            {
                me.complete(
                    &op,
                    Observed::Unknown {
                        reason: "workspace process not confirmed stopped".into(),
                    },
                    |_| Ok(()),
                )
                .await;
                return;
            }
            let (path, repo, branch) = match made {
                Ok(x) => x,
                Err(e) => {
                    return me
                        .complete(&op, failed(e), |t| t.unreserve(&resource))
                        .await;
                }
            };
            let inner = me.0.clone();
            let usable = workspace::usable(&path);
            loop {
                let r = inner.j.tx(|t| {
                    t.unreserve(&resource)?;
                    let view = WorkspaceView {
                        workspace: WorkspaceRef {
                            node: inner.cfg.node.clone(),
                            id: wid.clone(),
                        },
                        revision: id(t.revision()?)?,
                        usable,
                        branch: branch.clone(),
                        display: workspace::display(&path),
                    };
                    t.put_workspace(&WorkspaceRow {
                        resource: body.resource.clone(),
                        spec: spec.clone(),
                        kind: body.kind.clone(),
                        repo: repo.as_ref().map(|r| r.to_string_lossy().to_string()),
                        view: view.clone(),
                        path: path.to_string_lossy().to_string(),
                        incarnation: t.bump("workspace")?,
                    })?;
                    let state = Decided::done(OperationResult::Workspace(view.clone())).state;
                    t.set_state(&op, &state)?;
                    emit_all(
                        &inner,
                        t,
                        vec![(
                            Subject::Operation {
                                operation: op.clone(),
                            },
                            EventBody::OperationChanged {
                                receipt: Receipt {
                                    operation: op.clone(),
                                    state,
                                },
                            },
                        )],
                    )?;
                    Ok(())
                });
                inner.wake.notify_one();
                if let Err(e) = r {
                    tracing::error!("prepare {op}: {e}");
                    inner.fault();
                    if inner.closed.load(Ordering::SeqCst) {
                        break;
                    }
                    tokio::time::sleep(Duration::from_millis(100)).await;
                } else {
                    break;
                }
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

    /// Refused while something of it runs, waits or is unknown, or another prepare or tidy of it is
    /// under way. Reserved and accepted now: nothing opens or starts in it meanwhile. Finished once
    /// its harnesses are confirmed gone and its directory is removed (what could not be done is said).
    async fn tidy_workspace(&self, c: Command<Tidy>) -> ApiResult<Receipt> {
        self.mine(&c.body.workspace)?;
        let w = c.body.workspace.clone();
        let mut sessions = vec![];
        let mut resource = String::new();
        let (r, fresh) = self.accept("tidy_workspace", &c, |t| {
            let row = t
                .workspace(&w.id)?
                .ok_or_else(|| ApiError::new(ErrorCode::Invalid, "no such workspace"))?;
            Self::workspace_free(t, &row.resource)?;
            if row.view.revision != c.body.expected {
                return Err(ApiError::new(ErrorCode::Conflict, "changed since"));
            }
            if t.open_execs_in(&w)? > 0 {
                return Err(ApiError::new(
                    ErrorCode::Busy,
                    "a command runs in it or is unknown: recover it first",
                ));
            }
            for s in t.sessions_in(&w)? {
                Self::idle(t, &s)?;
                sessions.push(s);
            }
            for s in &sessions {
                if let Some(mut srow) = t.session(s)? {
                    srow.view.phase = SessionPhase::Releasing;
                    t.put_session(&srow)?;
                }
            }
            t.reserve(&row.resource, &c.id)?;
            resource = row.resource;
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
        let expected = c.body.expected.clone();
        self.spawn_owned(async move {
            // the harnesses first, confirmed
            for s in &sessions {
                let (tx, rx) = tokio::sync::oneshot::channel();
                let _ = me.actor(s).send(Msg::Stop {
                    op: None,
                    abandon: false,
                    done: Some(tx),
                });
                if !rx.await.unwrap_or(procs::Stopped::Uncertain).gone() {
                    let res = resource.clone();
                    return me
                        .complete(
                            &op,
                            failed(ApiError::new(
                                ErrorCode::Busy,
                                "a harness in it would not stop",
                            )),
                            |t| t.unreserve(&res),
                        )
                        .await;
                }
            }
            let row = match me.0.j.read(|t| t.workspace(&w.id)) {
                Ok(Some(r)) if r.view.revision == expected => r,
                _ => {
                    let res = resource.clone();
                    return me
                        .complete(
                            &op,
                            failed(ApiError::new(ErrorCode::Conflict, "changed since")),
                            |t| t.unreserve(&res),
                        )
                        .await;
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
            let commands = workspace::Commands {
                inner: me.0.clone(),
                op: op.clone(),
            };
            let removed = tokio::task::spawn_blocking(move || {
                workspace::remove(
                    &commands,
                    &kind,
                    &path,
                    repo.as_deref(),
                    branch.as_deref(),
                    keep,
                )
            })
            .await
            .map_err(|e| e.to_string())
            .and_then(|r| r);
            let res = resource.clone();
            if me
                .0
                .j
                .read(|t| t.proc_of(&workspace::control_session(&op)))
                .map_or(true, |p| p.is_some())
            {
                me.complete(
                    &op,
                    Observed::Unknown {
                        reason: "workspace process not confirmed stopped".into(),
                    },
                    |_| Ok(()),
                )
                .await;
                return;
            }
            match removed {
                Ok(note) => {
                    me.complete(
                        &op,
                        Observed::Finished {
                            outcome: Outcome::Succeeded,
                            result: Some(OperationResult::Unit),
                            error: note.map(|n| ApiError::new(ErrorCode::Conflict, n)),
                        },
                        |t| {
                            t.unreserve(&res)?;
                            t.drop_workspace(&w.id)?;
                            for s in &sessions {
                                if let Some(mut srow) = t.session(s)? {
                                    srow.view.phase = SessionPhase::Released;
                                    t.put_session(&srow)?;
                                }
                            }
                            Ok(())
                        },
                    )
                    .await
                }
                Err(e) => {
                    me.complete(&op, failed(ApiError::new(ErrorCode::Internal, e)), |t| {
                        t.unreserve(&res)
                    })
                    .await
                }
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
    /// Accepted now (the workspace still valid, a command slot free); finished once the command
    /// ended and its whole process group is confirmed gone, with its exit code and bounded output
    /// ([`exec`]). Any exit code is `succeeded` (the command ran to its end); the deadline is
    /// `timed_out`. A group that could not be confirmed gone leaves it `unknown`, and so does a
    /// restart while it ran: it is never run again (recover stops what is left).
    async fn exec(&self, c: Command<ExecSpec>) -> ApiResult<Receipt> {
        self.mine(&c.body.workspace)?;
        if self.0.cfg.capacity.execs == 0 {
            return unsupported("commands");
        }
        if !c.body.secrets.is_empty() {
            return unsupported("secrets for a command");
        }
        Self::check_policy(&c.body.policy)?;
        Self::check_limits(&c.body.limits)?;
        if c.body.command.trim().is_empty() || c.body.command.contains('\0') {
            return Err(ApiError::new(ErrorCode::Invalid, "no command").on(&c.id));
        }
        if c.body
            .env
            .iter()
            .any(|(k, v)| k.is_empty() || k.contains('=') || k.contains('\0') || v.contains('\0'))
        {
            return Err(ApiError::new(ErrorCode::Invalid, "a bad environment variable").on(&c.id));
        }
        let w = c.body.workspace.clone();
        let mut dir = PathBuf::new();
        let mut counted = false;
        let accepted = self.accept("exec", &c, |t| {
            let row = t
                .workspace(&w.id)?
                .ok_or_else(|| ApiError::new(ErrorCode::Invalid, "no such workspace"))?;
            Self::workspace_free(t, &row.resource)?;
            let existing = matches!(row.kind, PrepareKind::Existing { .. });
            let base = workspace::still_valid(Path::new(&row.path), &self.0.roots(), existing)?;
            dir = match &c.body.cwd {
                None => base,
                Some(rel) => workspace::below(&base, rel)?,
            };
            if self.0.execs.fetch_add(1, Ordering::SeqCst) >= self.0.cfg.capacity.execs as usize {
                self.0.execs.fetch_sub(1, Ordering::SeqCst);
                return Err(ApiError::new(ErrorCode::Exhausted, "no command capacity"));
            }
            counted = true;
            let mut d = Decided::accepted();
            d.workspace = Some(w.clone());
            Ok(d)
        });
        let r = match accepted {
            Ok((r, true)) => r,
            other => {
                if counted {
                    self.0.execs.fetch_sub(1, Ordering::SeqCst);
                }
                return other.map(|(r, _)| r);
            }
        };
        let me = self.clone();
        let op = c.id.clone();
        let run = exec::Run {
            dir,
            command: c.body.command.clone(),
            env: c.body.env.clone(),
            deadline: c.body.limits.deadline.0,
            cap: (c.body.limits.max_output_bytes as usize).min(1 << 20),
        };
        let spawned = self.spawn_owned(async move {
            let inner = me.0.clone();
            let op2 = op.clone();
            let ran = tokio::task::spawn_blocking(move || exec::run(&inner, &op2, &run))
                .await
                .map_err(|e| e.to_string())
                .and_then(|r| r);
            let state = match ran {
                Err(e) => failed(ApiError::new(
                    ErrorCode::Invalid,
                    format!("the command did not start: {e}"),
                )),
                Ok(ran) if !ran.gone => Observed::Unknown {
                    reason: "the command's processes were not confirmed gone".into(),
                },
                Ok(ran) => {
                    let (outcome, exit_code) = match ran.ended {
                        exec::Ended::Exited(code) => (Outcome::Succeeded, code),
                        exec::Ended::TimedOut => (Outcome::TimedOut, None),
                        exec::Ended::Stopped => (Outcome::Interrupted, None),
                    };
                    Observed::Finished {
                        outcome,
                        result: Some(OperationResult::Exec(ExecResult {
                            exit_code,
                            stdout: ran.stdout,
                            stderr: ran.stderr,
                            truncated: ran.truncated,
                        })),
                        error: None,
                    }
                }
            };
            me.complete(&op, state, |_| Ok(())).await;
            me.0.execs.fetch_sub(1, Ordering::SeqCst);
        });
        if !spawned {
            // shutting down: it never starts, and the restart reports it unknown
            self.0.execs.fetch_sub(1, Ordering::SeqCst);
        }
        Ok(r)
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
                .roots()
                .iter()
                .map(|r| RootInfo {
                    id: r.id.clone(),
                    display: r.display.clone(),
                    access: r.access,
                })
                .collect(),
            capacity: self.0.cfg.capacity.clone(),
            features: vec![],
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
    async fn locate(&self, _source: HistoryRef) -> ApiResult<HistoryLocation> {
        unsupported("history")
    }
}

impl Node for LocalNode {
    fn id(&self) -> &NodeId {
        &self.0.cfg.node
    }
}

#[cfg(test)]
mod binding_tests {
    use super::*;
    use std::os::unix::fs::symlink;

    #[test]
    fn hanging_git_probe_is_bounded_and_reaped() {
        use std::os::unix::fs::PermissionsExt;
        let d = tempfile::tempdir().unwrap();
        let program = d.path().join("git");
        std::fs::write(&program, "#!/bin/sh\necho $$ > pid\nexec sleep 30\n").unwrap();
        std::fs::set_permissions(&program, std::fs::Permissions::from_mode(0o700)).unwrap();
        let started = std::time::Instant::now();
        assert!(head_with(d.path(), &program, Duration::from_millis(1000)).is_err());
        assert!(started.elapsed() < Duration::from_secs(2));
        let pid: i32 = std::fs::read_to_string(d.path().join("pid"))
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        assert_eq!(unsafe { libc::kill(pid, 0) }, -1);
    }

    #[test]
    fn dangling_and_retargeted_links_fail_closed() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let w = WorkspaceRef {
            node: NodeId::new("n").unwrap(),
            id: WorkspaceId::new("w").unwrap(),
        };
        symlink(outside.path().join("missing"), root.path().join("dangling")).unwrap();
        assert!(inside(root.path(), &w, "dangling").is_none());
        assert!(inside(root.path(), &w, "ordinary-new/file").is_some());
        std::fs::create_dir(root.path().join("a")).unwrap();
        std::fs::create_dir(root.path().join("b")).unwrap();
        symlink("a", root.path().join("parent")).unwrap();
        let before = file_version(&root.path().join("parent")).unwrap();
        std::fs::remove_file(root.path().join("parent")).unwrap();
        symlink("b", root.path().join("parent")).unwrap();
        assert_ne!(before, file_version(&root.path().join("parent")).unwrap());
        std::fs::remove_file(root.path().join("parent")).unwrap();
        symlink(outside.path(), root.path().join("parent")).unwrap();
        assert!(inside(root.path(), &w, "parent/new").is_none());
    }
}

#[cfg(test)]
mod storage_tests {
    use super::admit;

    #[test]
    fn the_limit_holds_until_usage_is_under_low_water() {
        // under the limit: taken
        assert_eq!(admit(false, 50, 10, 100, 80), (true, false));
        // reaching it: refused, and full from now on
        assert_eq!(admit(false, 95, 10, 100, 80), (false, true));
        // full: a small command that would fit still waits while usage is above low water
        assert_eq!(admit(true, 85, 1, 100, 80), (false, true));
        // under low water again: taken, no longer full
        assert_eq!(admit(true, 79, 1, 100, 80), (true, false));
        // no room at all (the disk is past its reserve)
        assert_eq!(admit(false, 0, 1, 0, 80), (false, true));
    }
}
