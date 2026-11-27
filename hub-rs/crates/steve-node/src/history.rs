//! Discovery on this machine: read-only local harness history and CLI binary probes.
//!
//! All harness-specific transcript formats, directory layouts, and CLI probe flags live in
//! `steve_acp::adapter::history`. This module is harness-agnostic orchestration:
//!
//! - [`candidates`] lists importable transcripts across all registered [`HistoryAdapter`]s,
//!   newest first, skipping Steve's own runs.
//! - [`listed`] verifies an import key names a file produced by [`candidates`].
//! - [`read`] reads and parses one transcript via its [`HistoryAdapter`]; [`events`] converts
//!   those ACP-shaped rows into [`NodeEvent`]s through the shared [`Normalizer`].
//! - [`find_transcript`] and [`locate`] resolve a harness session's transcript for resume.
//! - [`probe`] queries configured harness binaries for version and (when supported) models.

use std::collections::{BTreeMap, HashMap};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_json::Value;
use steve_acp::adapter::history::{
    HistoryAdapter, history_adapter, history_adapter_for_key, history_adapters, mtime_ms,
};
use steve_acp::normalize::{Fact, Normalizer};
use steve_node_api::*;

pub use steve_acp::adapter::history::HistorySession as LocalSession;

/// Where the harnesses keep their sessions on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Homes {
    /// The user's home under which harness history directories live.
    pub user_home: PathBuf,
    /// `STEVE_HOME`: transcripts of runs inside it are Steve's own and never listed.
    pub steve_home: PathBuf,
    /// Optional per-harness state directory overrides keyed by harness id.
    /// Harnesses absent from this map default to `user_home.join(adapter.default_state_rel())`.
    pub state_dirs: BTreeMap<String, PathBuf>,
}

impl Homes {
    /// Effective state directory for `adapter`: configured override or `user_home/<default>`.
    pub fn state_dir_for(&self, adapter: &dyn HistoryAdapter) -> PathBuf {
        self.state_dirs
            .get(adapter.id())
            .filter(|p| !p.as_os_str().is_empty())
            .cloned()
            .unwrap_or_else(|| self.user_home.join(adapter.default_state_rel()))
    }
}

/// Configured harness CLI binaries [`probe`] queries, keyed by harness id.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Binaries {
    pub bins: BTreeMap<String, PathBuf>,
}

impl Binaries {
    pub fn get(&self, harness_id: &str) -> Option<&Path> {
        self.bins
            .get(harness_id)
            .map(PathBuf::as_path)
            .filter(|p| !p.as_os_str().is_empty())
    }
}

/// What a [`crate::LocalNode`] discovers with ([`crate::LocalNode::set_discovery`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryConfig {
    pub homes: Homes,
    pub binaries: Binaries,
}

/// Canonical harness id owning `key` (`"<prefix>:<path>"`), if any registered adapter recognizes it.
pub fn harness_of_key(key: &str) -> Option<&'static str> {
    history_adapter_for_key(key).map(|(a, _)| a.id())
}

// ── the listing ──

/// Importable local sessions across all registered history adapters: newest first, Steve's own skipped. Blocking.
pub fn candidates(h: &Homes, limit: usize) -> Vec<LocalSession> {
    let mut files: Vec<(&'static dyn HistoryAdapter, PathBuf, f64)> = history_adapters()
        .iter()
        .copied()
        .flat_map(|adapter| {
            adapter
                .list_files(&h.user_home)
                .into_iter()
                .filter_map(move |f| mtime_ms(&f).map(|t| (adapter, f, t)))
        })
        .collect();
    files.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
    files
        .into_iter()
        .take(limit * 2)
        .filter_map(|(adapter, f, _)| adapter.peek(&h.steve_home, &f))
        .take(limit)
        .collect()
}

/// Whether `key` is one [`candidates`] lists: it names one of the transcripts this machine's
/// harnesses keep, compared with the listing's own keys, and that transcript is not Steve's own
/// run. A key a client made up (any other path, a `..` in it, the other harness's prefix) is not.
/// Blocking.
pub fn listed(h: &Homes, key: &str) -> bool {
    let Some((adapter, _)) = history_adapter_for_key(key) else {
        return false;
    };
    let prefix = adapter.key_prefix();
    let Some(f) = adapter
        .list_files(&h.user_home)
        .into_iter()
        .find(|f| format!("{prefix}:{}", f.display()) == key)
    else {
        return false;
    };
    adapter.peek(&h.steve_home, &f).is_some()
}

/// One transcript read for import.
#[derive(Debug, Clone, PartialEq)]
pub enum Read {
    Missing,
    Unreadable,
    Ready {
        peek: LocalSession,
        harness: String,
        /// ACP-shaped rows: `user` `{text, via}`, and `agent_message_chunk`/`tool_call` updates.
        rows: Vec<(String, Value)>,
    },
}

/// Reads and converts the transcript a key names (blocking). It does not check the key against the
/// listing: [`listed`] does.
pub fn read(steve_home: &Path, key: &str) -> Read {
    let Some((adapter, file)) = history_adapter_for_key(key) else {
        return Read::Missing;
    };
    let path = Path::new(file);
    if file.is_empty() || !path.exists() {
        return Read::Missing;
    }
    let Some(peek) = adapter.peek(steve_home, path) else {
        return Read::Unreadable;
    };
    let Ok(text) = std::fs::read(path).map(|b| String::from_utf8_lossy(&b).into_owned()) else {
        return Read::Unreadable;
    };
    let rows = adapter.parse_rows(&text);
    Read::Ready {
        peek,
        harness: adapter.id().into(),
        rows,
    }
}

/// Parse raw transcript text for `harness` into ACP-shaped rows.
pub fn parse_rows(harness: &str, text: &str) -> Vec<(String, Value)> {
    history_adapter(harness)
        .map(|a| a.parse_rows(text))
        .unwrap_or_default()
}

// ── as Steve's events ──

/// The prefix and meter of the import normalizer: message ids and tool views of imported history.
const PREFIX: &str = "import";

/// The rows of one transcript as Steve's events, under the import's operation. The ACP updates go
/// through the driver's normalizer (`cwd`: paths inside it are shown relative); a user's message is
/// a message of its own (`role: user`), outside the normalizer, as the Hub's own rows were.
pub fn events(op: &OperationId, cwd: &str, rows: &[(String, Value)], at: i64) -> Vec<NodeEvent> {
    let epoch = Epoch::new("import").expect("a valid epoch");
    let stream = StreamId::new(op.as_str()).expect("an operation id is a valid stream id");
    let mut norm = Normalizer::new(cwd, PREFIX, Epoch::new(PREFIX).expect("a valid epoch"));
    let mut bodies = vec![];
    let mut users = 0u64;
    for (kind, data) in rows {
        if kind == "user" {
            users += 1;
            let message = format!("history.u{users}");
            let block = format!("{message}.b1");
            bodies.push(EventBody::MessageStarted {
                message: message.clone(),
                block: block.clone(),
                role: Role::User,
            });
            bodies.push(EventBody::TextDelta {
                message: message.clone(),
                block,
                utf8_offset: 0,
                text: data
                    .get("text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .into(),
            });
            bodies.push(EventBody::MessageCompleted { message });
            continue;
        }
        for f in norm.update(data) {
            if let Fact::Event(body) = f {
                bodies.push(body);
            }
        }
    }
    for f in norm.finish() {
        if let Fact::Event(body) = f {
            bodies.push(body);
        }
    }
    bodies
        .into_iter()
        .enumerate()
        .map(|(i, body)| NodeEvent {
            schema: EVENT_SCHEMA,
            id: EventId::new(format!("e{}", i + 1)).expect("a valid event id"),
            at: Timestamp(at),
            cursor: Watermark {
                stream: stream.clone(),
                epoch: epoch.clone(),
                seq: i as u64 + 1,
            },
            subject: Subject::Operation {
                operation: op.clone(),
            },
            body,
        })
        .collect()
}

// ── resuming ──

/// A harness session id as a file name part: letters, digits, `-` and `_` only.
pub fn plain_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// The transcript of a harness session, by its id (`cwd`: where it ran, when known). Blocking.
pub fn find_transcript(h: &Homes, harness: &str, id: &str, cwd: &str) -> Option<PathBuf> {
    let adapter = history_adapter(harness)?;
    let state_dir = h.state_dir_for(adapter);
    adapter.find_transcript(&state_dir, id, cwd)
}

/// The `sessionId`/`id` and `cwd` the transcript itself records, from its first lines.
pub fn recorded(file: &Path, harness: &str) -> (Option<String>, Option<String>) {
    history_adapter(harness)
        .map(|a| a.recorded(file))
        .unwrap_or((None, None))
}

/// A path for people: the user's home as `~`.
fn display(h: &Homes, p: &Path) -> String {
    match p.strip_prefix(&h.user_home) {
        Ok(rest) if !h.user_home.as_os_str().is_empty() => format!("~/{}", rest.display()),
        _ => p.display().to_string(),
    }
}

/// Where a history entry is: a key [`listed`] lists, or `session:<id>` (a harness session,
/// looked up by its id). Anything else is `invalid`. Blocking.
pub fn locate(h: &Homes, source: &HistoryRef) -> ApiResult<HistoryLocation> {
    let not_listed = || Error::new(Code::Invalid, "not a history this node lists");
    let (file, id) = if let Some(id) = source.key.strip_prefix("session:") {
        // an id, never a path: it is joined into a file name
        if !plain_id(id) {
            return Err(not_listed());
        }
        let file = find_transcript(h, &source.harness, id, "").ok_or_else(not_listed)?;
        (file, id.to_string())
    } else {
        let Some((adapter, path_str)) = history_adapter_for_key(&source.key) else {
            return Err(not_listed());
        };
        if adapter.id() != source.harness || !listed(h, &source.key) {
            return Err(not_listed());
        }
        let file = PathBuf::from(path_str);
        let id = adapter.session_id_from_file(&file);
        (file, id)
    };
    let (recorded_id, recorded_cwd) = recorded(&file, &source.harness);
    let updated = mtime_ms(&file).map(|t| Timestamp(t as i64));
    Ok(HistoryLocation {
        source: source.clone(),
        display: display(h, &file),
        resume: ResumeHandle::new(id).ok(),
        updated,
        recorded_id,
        recorded_cwd,
    })
}

/// Probe working directory existence, non-default state env vars, and optional transcript metadata
/// for resuming a harness session on this machine. Blocking.
pub fn resume_probe(
    h: &Homes,
    declared: &HashMap<String, steve_harness_spec::Declaration>,
    req: &ResumeProbeRequest,
) -> ResumeProbeResponse {
    let cwd_exists = !req.cwd.is_empty() && Path::new(&req.cwd).is_dir();
    let props = steve_harness_spec::spec(&req.harness, declared.get(&req.harness)).properties;
    let principal_dir = req
        .principal_state_dir
        .as_deref()
        .filter(|s| !s.is_empty())
        .map(Path::new);
    let mut state_env = Vec::new();
    match (props.state.as_ref(), principal_dir) {
        (Some(state), Some(pdir)) => {
            for (k, v) in state.isolation_env(pdir) {
                let vp = PathBuf::from(v);
                let vp = if vp.is_relative() {
                    std::path::absolute(&vp).unwrap_or(vp)
                } else {
                    vp
                };
                state_env.push((
                    k.to_string_lossy().into_owned(),
                    vp.to_string_lossy().into_owned(),
                ));
            }
        }
        (Some(state), None) if !state.env.is_empty() => {
            let default = if state.home_override {
                h.user_home.clone()
            } else {
                h.user_home.join(&state.default)
            };
            let dir = h
                .state_dirs
                .get(&req.harness)
                .filter(|p| !p.as_os_str().is_empty())
                .cloned()
                .or_else(|| {
                    std::env::var_os(&state.env)
                        .filter(|v| !v.is_empty())
                        .map(PathBuf::from)
                })
                .unwrap_or_else(|| default.clone());
            if dir != default {
                let abs = if dir.is_relative() {
                    std::path::absolute(&dir).unwrap_or_else(|_| h.user_home.join(&dir))
                } else {
                    dir
                };
                state_env.push((state.env.clone(), abs.to_string_lossy().into_owned()));
            }
        }
        _ => {}
    }

    let transcript = if req.check_transcript && props.local_transcript {
        req.session_id
            .as_deref()
            .filter(|id| plain_id(id))
            .and_then(|id| {
                let mut eff = h.clone();
                if let (Some(state), Some(pdir)) = (props.state.as_ref(), principal_dir) {
                    for (k, v) in state.isolation_env(pdir) {
                        if k == "HOME" {
                            eff.user_home = PathBuf::from(v);
                        } else if !state.env.is_empty() && k == state.env.as_str() {
                            eff.state_dirs.insert(req.harness.clone(), PathBuf::from(v));
                        }
                    }
                }
                let file = find_transcript(&eff, &req.harness, id, &req.cwd)?;
                let (recorded_id, recorded_cwd) = recorded(&file, &req.harness);
                let updated = mtime_ms(&file).map(|t| Timestamp(t as i64));
                Some(HistoryLocation {
                    source: HistoryRef {
                        harness: req.harness.clone(),
                        key: format!("session:{id}"),
                    },
                    display: display(&eff, &file),
                    resume: ResumeHandle::new(id.to_string()).ok(),
                    updated,
                    recorded_id,
                    recorded_cwd,
                })
            })
    } else {
        None
    };

    ResumeProbeResponse {
        cwd_exists,
        transcript,
        state_env,
    }
}

// ── imports ──

/// How long an import's pages are kept to be read, and how many imports at most.
const IMPORT_TTL: Duration = Duration::from_secs(600);
const IMPORTS_MAX: usize = 16;

struct Import {
    digest: Digest,
    at: Instant,
    events: Vec<NodeEvent>,
}

/// The imports a Node read and has not dropped yet, by operation id. In memory only: an import
/// reads files the harness keeps, so a restart reads them again under a new operation.
#[derive(Default)]
pub(crate) struct Imports(std::sync::Mutex<HashMap<OperationId, Import>>);

impl Imports {
    /// The same operation with the same content answers what it read before; with another content
    /// it is a `conflict`.
    pub(crate) fn seen(&self, op: &OperationId, digest: &Digest) -> ApiResult<bool> {
        let mut m = self.0.lock().unwrap_or_else(|e| e.into_inner());
        m.retain(|_, i| i.at.elapsed() < IMPORT_TTL);
        match m.get(op) {
            Some(i) if &i.digest == digest => Ok(true),
            Some(_) => Err(Error::new(Code::Conflict, "another import under this id")
                .with_param("operation", op.to_string())),
            None => Ok(false),
        }
    }

    pub(crate) fn keep(&self, op: OperationId, digest: Digest, events: Vec<NodeEvent>) {
        let mut m = self.0.lock().unwrap_or_else(|e| e.into_inner());
        while m.len() >= IMPORTS_MAX {
            let Some(oldest) = m.iter().min_by_key(|(_, i)| i.at).map(|(k, _)| k.clone()) else {
                break;
            };
            m.remove(&oldest);
        }
        m.insert(
            op,
            Import {
                digest,
                at: Instant::now(),
                events,
            },
        );
    }

    pub(crate) fn page(&self, op: &OperationId, page: &PageRequest) -> ApiResult<ImportedPage> {
        let m = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let Some(i) = m.get(op) else {
            return Err(
                Error::new(Code::Invalid, "no such import").with_param("operation", op.to_string())
            );
        };
        let from = match &page.cursor {
            None => 0,
            Some(c) => c
                .as_str()
                .parse::<usize>()
                .map_err(|_| Error::new(Code::Invalid, "bad cursor"))?,
        };
        let limit = (page.limit.clamp(1, 1000)) as usize;
        let end = (from + limit).min(i.events.len());
        let events = i
            .events
            .get(from..end)
            .map(<[_]>::to_vec)
            .unwrap_or_default();
        let next = (end < i.events.len())
            .then(|| Cursor::new(end.to_string()).expect("a number is a valid cursor"));
        Ok(ImportedPage { events, next })
    }
}

// ── harness binaries ──

/// How long a probe counts as current.
const PROBE_TTL: Duration = Duration::from_secs(300);
/// Per-binary `--version` probe timeout.
const VERSION_PROBE_TIMEOUT: Duration = Duration::from_secs(8);
/// Per-binary model-list probe timeout.
const MODEL_PROBE_TIMEOUT: Duration = Duration::from_secs(15);
/// Per-stream output ceiling for `--version` and model-list CLI probes (1 MiB bounds unexpected
/// output without truncating real version strings or model catalogs).
const HISTORY_PROBE_MAX_OUTPUT_BYTES: usize = 1024 * 1024;

/// What one harness binary says about itself.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Probed {
    pub version: String,
    pub models: Vec<String>,
}

/// The last probe, so asking again within five minutes runs nothing.
#[derive(Default)]
pub(crate) struct Probes(tokio::sync::Mutex<Option<(Instant, HashMap<String, Probed>)>>);

impl Probes {
    pub(crate) async fn forget(&self) {
        *self.0.lock().await = None;
    }

    pub(crate) async fn get(&self, d: &DiscoveryConfig, fresh: bool) -> HashMap<String, Probed> {
        let mut last = self.0.lock().await;
        if !fresh
            && let Some((at, v)) = last.as_ref()
            && at.elapsed() < PROBE_TTL
        {
            return v.clone();
        }
        let v = probe(&d.binaries, &d.homes).await;
        *last = Some((Instant::now(), v.clone()));
        v
    }
}

/// `execFileSync(cmd, args, {timeout})` → trimmed stdout, `""` on any failure.
async fn run(cmd: &Path, args: &[&str], env: &[(&str, &Path)], timeout: Duration) -> String {
    let mut c = std::process::Command::new(cmd);
    c.env_remove("STEVE_UPGRADE_CONTROL_TOKEN")
        .env_remove("STEVE_SUPERVISOR_PID")
        .env_remove("STEVE_UPGRADE_ID")
        .args(args);
    for (k, v) in env {
        c.env(k, v);
    }
    let program = cmd.to_string_lossy().into_owned();
    match steve_runtime::process::run_bounded_async(
        c,
        program,
        steve_runtime::process::BoundedOptions {
            max_output_bytes: HISTORY_PROBE_MAX_OUTPUT_BYTES,
            ..steve_runtime::process::BoundedOptions::capture("history_probe", timeout)
        },
    )
    .await
    {
        Ok(o) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => String::new(),
    }
}

/// `<bin> --version` when the file exists, `""` otherwise.
async fn version_of(bin: &Path) -> String {
    if !bin.as_os_str().is_empty() && bin.exists() {
        run(bin, &["--version"], &[], VERSION_PROBE_TIMEOUT).await
    } else {
        String::new()
    }
}

/// Probe every registered history adapter's CLI binary (version and, when supported, models).
pub async fn probe(b: &Binaries, homes: &Homes) -> HashMap<String, Probed> {
    let mut set = tokio::task::JoinSet::new();
    for adapter in history_adapters().iter().copied() {
        let Some(bin) = b.get(adapter.id()).map(Path::to_path_buf) else {
            set.spawn(async move { (adapter.id().to_string(), Probed::default()) });
            continue;
        };
        let state_dir = homes.state_dir_for(adapter);
        set.spawn(async move {
            let models_fut = async {
                let Some((args, env_var)) = adapter.model_probe_cmd() else {
                    return vec![];
                };
                if !bin.exists() {
                    return vec![];
                }
                let out = run(&bin, args, &[(env_var, &state_dir)], MODEL_PROBE_TIMEOUT).await;
                adapter.parse_probed_models(&out)
            };
            let (version, models) = tokio::join!(version_of(&bin), models_fut);
            (adapter.id().to_string(), Probed { version, models })
        });
    }
    let mut out = HashMap::new();
    while let Some(res) = set.join_next().await {
        if let Ok((id, probed)) = res {
            out.insert(id, probed);
        }
    }
    out
}
