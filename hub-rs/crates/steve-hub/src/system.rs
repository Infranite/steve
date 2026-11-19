//! What this machine is and what it can run (system.ts): the Hub registers itself as a device, reports the
//! harnesses it finds, and imports local harness history through the Node as read-only sessions.
//! Host commands run through `tokio::process` with an 8 s timeout.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use crate::backend::{BResult, BackendError, ImportedSession, Repo};
use crate::bus::Bus;
use crate::config::Config;
use crate::js;

/// `uid(p)`: `p_` + 16 hex chars of a random UUID.
pub fn uid(p: &str) -> String {
    let u = uuid::Uuid::new_v4().simple().to_string();
    format!("{p}_{}", &u[..16])
}

pub fn sha_hex(s: &str) -> String {
    hex::encode(Sha256::digest(s.as_bytes()))
}

/// `execFileSync(cmd, args, {timeout: 8000})` → trimmed stdout, `""` on any failure.
pub async fn run(cmd: &str, args: &[&str]) -> String {
    let child = tokio::process::Command::new(cmd)
        .env_remove("STEVE_UPGRADE_CONTROL_TOKEN")
        .env_remove("STEVE_SUPERVISOR_PID")
        .env_remove("STEVE_UPGRADE_ID")
        .args(args)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .output();
    match tokio::time::timeout(Duration::from_secs(8), child).await {
        Ok(Ok(o)) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => String::new(),
    }
}

// ── device ──

async fn os_label() -> String {
    let gb = |bytes: f64| (bytes / (1u64 << 30) as f64).round() as i64;
    if cfg!(target_os = "macos") {
        let v = run("sw_vers", &["-productVersion"]).await;
        let cpu = run("sysctl", &["-n", "machdep.cpu.brand_string"]).await;
        let mem = run("sysctl", &["-n", "hw.memsize"])
            .await
            .parse::<f64>()
            .unwrap_or(0.0);
        format!("macOS {v} · {cpu} · {} GB", gb(mem))
    } else {
        let rel = run("uname", &["-r"]).await;
        let kb = std::fs::read_to_string("/proc/meminfo")
            .ok()
            .and_then(|m| {
                m.lines()
                    .find(|l| l.starts_with("MemTotal:"))
                    .and_then(|l| l.split_whitespace().nth(1)?.parse::<f64>().ok())
            })
            .unwrap_or(0.0);
        format!("{} {rel} · {} GB", std::env::consts::OS, gb(kb * 1024.0))
    }
}

/// Name and OS line of this machine for the `hub` device row.
pub async fn hub_device(version: &str) -> (String, String, String) {
    let mut name = run("scutil", &["--get", "ComputerName"]).await;
    if name.is_empty() {
        name = run("hostname", &[]).await;
    }
    (name, os_label().await, format!("Steve {version}"))
}

/// Boot `registerHub()`: upsert device `hub` (no SSE, as in TS).
pub fn register_hub(r: &dyn Repo, name: &str, os: &str, version: &str) -> BResult<()> {
    r.device_upsert("hub", name, &["Hub".into(), "执行节点".into()], os, version)
}

// ── runtimes ──

/// How long a probe result counts as current; an older one is still served while a new probe runs.
const RUNTIMES_MAX_AGE: Duration = Duration::from_secs(300);

#[derive(Default)]
struct RuntimeSnapshot {
    generation: u64,
    value: Option<(Instant, Value)>,
}
type RuntimesCache = Arc<std::sync::Mutex<RuntimeSnapshot>>;

/// `runtimes(fresh)` with its 5-minute cache. The probe never blocks the async runtime, and once there is a
/// result `/api/state` never waits for one: an expired result is served while a background probe replaces it.
pub struct Runtimes {
    cfg: Arc<Config>,
    cache: RuntimesCache,
    /// a background refresh is running
    refreshing: Arc<AtomicBool>,
    /// callers that must wait for a probe (nothing cached yet, or `fresh`) probe one at a time
    probing: Arc<Mutex<()>>,
    /// The local Node (`STEVE_NODE_MODE=local`): it runs the harness binaries, not the Hub.
    node: std::sync::OnceLock<Arc<dyn steve_node_api::Node>>,
    observed: std::sync::OnceLock<Arc<crate::harnesses::HubHarnesses>>,
}

impl Runtimes {
    pub fn new(cfg: Arc<Config>) -> Self {
        Runtimes {
            cfg,
            cache: Arc::default(),
            refreshing: Arc::default(),
            probing: Arc::default(),
            node: std::sync::OnceLock::new(),
            observed: std::sync::OnceLock::new(),
        }
    }

    /// Probe through the local Node from now on.
    pub fn set_node(&self, node: Arc<dyn steve_node_api::Node>) {
        let _ = self.node.set(node);
    }

    /// The session/model catalogue sees the very same probe as Devices, immediately after setup.
    pub fn set_harnesses(&self, harnesses: Arc<crate::harnesses::HubHarnesses>) {
        let _ = self.observed.set(harnesses);
    }

    pub async fn install(&self, harness: String) -> Result<(), steve_node_api::ApiError> {
        use steve_node_api::*;
        let Some(node) = self.node.get() else {
            return Err(ApiError::new(
                ErrorCode::Offline,
                "no local Node is connected; enable the local Node before installing harnesses",
            ));
        };
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;
        // Invalidate on both sides of the asynchronous admission. A probe that captured a
        // pre-install snapshot cannot publish after acceptance, including the model source.
        self.invalidate();
        let result = node
            .install_harness(Command {
                id: OperationId::new(format!("install_{}", uuid::Uuid::new_v4().simple()))
                    .expect("generated id"),
                issued_at: Timestamp(now),
                accept_before: Timestamp(now + 30_000),
                grant: GrantId::new("instance_owner").expect("static id"),
                body: InstallHarness { harness },
            })
            .await;
        self.invalidate();
        result.map(|_| ())
    }

    pub fn generation(&self) -> u64 {
        self.cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .generation
    }

    fn invalidate(&self) {
        let mut cache = self.cache.lock().unwrap_or_else(|e| e.into_inner());
        cache.generation += 1;
        cache.value = None;
        if let Some(observed) = self.observed.get() {
            observed.invalidate();
        }
    }

    fn cached(&self) -> Option<(Instant, Value)> {
        self.cache
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .value
            .clone()
    }

    /// Commit a probe as one generation-checked publication, including the model catalogue.
    fn publish(
        cache: &RuntimesCache,
        generation: u64,
        observed: Option<&crate::harnesses::HubHarnesses>,
        observed_generation: u64,
        result: &(Value, bool, Vec<steve_node_api::HarnessCapability>),
    ) -> bool {
        let mut cache = cache.lock().unwrap_or_else(|e| e.into_inner());
        if cache.generation != generation {
            return false;
        }
        if let Some(observed) = observed {
            let caps = result.1.then(|| result.2.clone());
            if !observed.update_if_current(observed_generation, caps) {
                return false;
            }
        }
        cache.generation += 1;
        cache.value = result.1.then(|| (Instant::now(), result.0.clone()));
        true
    }

    /// The harness list, its notes in the request's language (the cache keeps note codes).
    pub async fn get(&self, fresh: bool) -> Value {
        shown(self.list(fresh).await)
    }

    async fn list(&self, fresh: bool) -> Value {
        if !fresh && let Some((at, list)) = self.cached() {
            if at.elapsed() >= RUNTIMES_MAX_AGE
                && self
                    .refreshing
                    .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
            {
                let (cfg, cache, refreshing, probing, node, observed) = (
                    self.cfg.clone(),
                    self.cache.clone(),
                    self.refreshing.clone(),
                    self.probing.clone(),
                    self.node.get().cloned(),
                    self.observed.get().cloned(),
                );
                tokio::spawn(async move {
                    // A background probe must not restore an older ready result after a
                    // forced refresh has already observed a disconnected Node.
                    let _probing = probing.lock().await;
                    let generation = cache.lock().unwrap_or_else(|e| e.into_inner()).generation;
                    let observed_generation = observed.as_ref().map_or(0, |h| h.generation());
                    let result = probe(&cfg, node.as_deref(), false).await;
                    Self::publish(
                        &cache,
                        generation,
                        observed.as_deref(),
                        observed_generation,
                        &result,
                    );
                    refreshing.store(false, Ordering::SeqCst);
                });
            }
            return list;
        }
        let _probing = self.probing.lock().await;
        // another caller may have filled the cache while this one waited
        if !fresh && let Some((_, list)) = self.cached() {
            return list;
        }
        loop {
            let generation = self.generation();
            let observed = self.observed.get().map(|h| h.as_ref());
            let observed_generation = observed.map_or(0, |h| h.generation());
            let result = probe(&self.cfg, self.node.get().map(|n| n.as_ref()), fresh).await;
            if Self::publish(
                &self.cache,
                generation,
                observed,
                observed_generation,
                &result,
            ) {
                return result.0;
            }
            // Installation or a newer catalogue result crossed this probe. Ask the Node
            // for the current state rather than returning or caching the obsolete snapshot.
        }
    }
}

/// A probed list as the web shows it: each `note` code (see [`probe`]) as text, a missing path as "not found".
fn shown(mut list: Value) -> Value {
    for r in list.as_array_mut().into_iter().flatten() {
        let note = match r["note"].as_str().unwrap_or_default() {
            "acp" => steve_i18n::t!("hub.runtime.via_acp"),
            "adapter_missing" => match r["install_hint"].as_str().filter(|h| !h.is_empty()) {
                Some(hint) if hint.contains(' ') => {
                    steve_i18n::t!("hub.runtime.install_cmd", cmd = hint)
                }
                Some(hint) => steve_i18n::t!("hub.runtime.no_adapter", adapter = hint),
                None => steve_i18n::t!("hub.runtime.adapter_missing"),
            },
            "initialize_failed" => steve_i18n::t!("hub.runtime.initialize_failed"),
            "session_failed" => steve_i18n::t!("hub.runtime.session_failed"),
            "unknown" => steve_i18n::t!("hub.runtime.unknown"),
            code => code.to_string(),
        };
        r["note"] = json!(note);
        if r["path"] == "" {
            r["path"] = json!(steve_i18n::t!("hub.runtime.path_missing"));
        }
    }
    list
}

/// What the local Node's probe reports about its harnesses; `None` when it cannot say (not linked
/// yet, as just after the Hub starts), which is not "no adapter".
async fn node_harnesses(
    node: &dyn steve_node_api::Node,
    fresh: bool,
) -> Option<Vec<steve_node_api::HarnessCapability>> {
    match node.probe(fresh).await {
        Ok(c) => Some(c.harnesses),
        // not linked is how every start begins: the caller says "not known yet", nothing is wrong
        Err(e) if e.code == steve_node_api::ErrorCode::Offline => {
            tracing::info!("the local Node is not linked: its harnesses are not known yet");
            None
        }
        Err(e) => {
            tracing::warn!("probe of the local Node: {e}");
            None
        }
    }
}

/// One row per harness adapter the Hub knows (built-in or its manifest's), alike: its name, a
/// status (ready, adapter missing, initialize failed, no scratch session; notes are codes, see
/// [`shown`]), its version, the models, efforts and permission modes it offers and what it can
/// do, as the local Node's probe said. Without a Node: unknown.
fn rows(cfg: &Config, harnesses: Option<&[steve_node_api::HarnessCapability]>) -> Vec<Value> {
    use steve_node_api::AdapterStatus;
    cfg.adapters
        .iter()
        .map(|(id, path)| {
            let declared = cfg.declared_adapters.get(id);
            let spec = steve_acp::adapters::spec(id, declared);
            let reported = harnesses.and_then(|hs| hs.iter().find(|h| &h.name == id));
            let adapter = reported.and_then(|h| h.adapter.as_ref());
            let session = adapter.and_then(|a| a.session.as_ref()).filter(|_| {
                reported.and_then(|h| h.setup.as_ref()).is_none_or(|s| s.state == steve_node_api::SetupState::Ready)
            });
            let (ok, note) = match (adapter.map(|a| a.status), harnesses) {
                (Some(AdapterStatus::Ready), _) if session.is_some() => (true, "acp"),
                (Some(AdapterStatus::Ready), _) => (false, "session_failed"),
                (Some(AdapterStatus::AdapterMissing), _) => (false, "adapter_missing"),
                (Some(AdapterStatus::InitializeFailed), _) => (false, "initialize_failed"),
                // older protocol without `adapter`: trust `sessions` from the Node's probe
                (None, Some(_)) if reported.is_some_and(|r| r.sessions) => (true, "acp"),
                // without a Node's answer: there or not
                (None, None) => (false, "unknown"),
                (None, _) => (false, "adapter_missing"),
            };
            // a name the manifest gave wins, then the adapter's own for one of one's own
            let name = match (declared.and_then(|d| d.name.clone()), adapter) {
                (Some(n), _) => n,
                (None, Some(a)) if !spec.builtin => a.title.clone(),
                _ => spec.name.clone(),
            };
            let models: Vec<String> = match session {
                Some(o) if !o.models.is_empty() => o.models.iter().map(|m| m.id.clone()).collect(),
                _ if reported.and_then(|h| h.setup.as_ref()).is_some_and(|s| s.state != steve_node_api::SetupState::Ready) => vec![],
                _ => spec.properties.fallback_models.clone(),
            };
            // A missing login/scratch session does not make declared permission limits unknown.
            let modes = session.map(|o| o.modes.clone()).or_else(|| {
                steve_core::harnesses::Harness::declared(id, declared).modes
            });
            let setup = reported.and_then(|h| h.setup.clone()).or_else(|| {
                (reported.is_none() && !steve_acp::adapters::components(id).is_empty()).then(|| steve_node_api::HarnessSetup {
                    state: steve_node_api::SetupState::Unknown,
                    components: steve_acp::adapters::components(id).iter().map(|c| steve_node_api::HarnessComponent {
                        id: c.id.into(), name: c.name.into(), package: c.package.into(), installed: false, version: None, path: None, status: Some(steve_node_api::ComponentStatus::Unknown), detected: None,
                    }).collect(),
                    can_install: false,
                    reason: Some("The Node is disconnected, has not probed yet, or is too old to report component setup".into()),
                    help: Some("Connect or update the local Node and refresh discovery; separate Nodes are installed by their operator".into()), error: None,
                })
            });
            let ok = ok && setup.as_ref().is_none_or(|s| s.state == steve_node_api::SetupState::Ready);
            let mut row=json!({
                "id": id,
                "name": name,
                "builtin": spec.builtin,
                "custom": !spec.builtin,
                "path": path,
                "version": reported.map(|h| version_of(&h.version)).unwrap_or_default(),
                "adapter_version": adapter.and_then(|a| a.version.clone()),
                // one Steve installed: its update check (steve_acp::managed)
                "update": adapter.and_then(|a| a.update.clone()),
                "setup": setup,
                "models": models,
                "default_model": session.and_then(|o| o.default_model.clone()),
                "efforts": session.map_or_else(|| spec.properties.fallback_efforts.clone(), |o| o.efforts.clone()),
                "modes": modes,
                "caps": reported.map(|h| json!({
                    "sessions": h.sessions, "jobs": h.jobs, "images": h.images, "resume": h.resume,
                    "history": adapter.is_some_and(|a| a.list_sessions),
                    "mcp_http": adapter.is_some_and(|a| a.mcp_http),
                    "system_prompt": adapter.is_some_and(|a| a.system_prompt),
                })),
                "auth": adapter.map(|a| a.auth.clone()).unwrap_or_default(),
                "resume_command": adapter
                    .and_then(|a| a.resume_command.clone())
                    .or_else(|| spec.properties.resume_cli.clone()),
                "fork_command": adapter
                    .and_then(|a| a.fork_command.clone())
                    .or_else(|| spec.properties.fork_cli.clone()),
                "ok": ok,
                "note": note,
                "error": adapter.and_then(|a| a.error.clone().or_else(|| a.session_error.clone())),
                "install_hint": spec.install_hint,
                // it reads skills where Steve can put a Bot's (declared by its adapter)
                "skills": spec.properties.skills.is_some(),
            });
            if row["setup"].is_null() { row.as_object_mut().expect("row").remove("setup"); }
            row
        })
        .collect()
}

/// The version number in what a harness says of its version (`2.1.0 (Claude Code)`,
/// `codex-cli 0.80.0` → the number); as it is when there is none.
fn version_of(said: &str) -> String {
    said.split_whitespace()
        .find(|w| w.starts_with(|c: char| c.is_ascii_digit()))
        .unwrap_or(said)
        .to_string()
}

/// The harness list. `node`: the local Node runs the adapters (`fresh`: it asks them again).
/// The rows, and whether they are the Node's answer. A Node that could not be asked leaves the
/// rows unknown, and any previously cached list is discarded.
async fn probe(
    cfg: &Config,
    node: Option<&dyn steve_node_api::Node>,
    fresh: bool,
) -> (Value, bool, Vec<steve_node_api::HarnessCapability>) {
    let harnesses = match node {
        Some(n) => node_harnesses(n, fresh).await,
        None => None,
    };
    let answered = node.is_none() || harnesses.is_some();
    (
        json!(rows(cfg, harnesses.as_deref())),
        answered,
        harnesses.unwrap_or_default(),
    )
}

// ── local harness history ──

pub use steve_node::history::LocalSession;
use steve_node_api::{
    Command, Discovery, ErrorCode, GrantId, HistoryEntry, HistoryImport, NodeEvent, Observed,
    OperationId, OperationResult, PageRequest, Timestamp,
};

/// The file side of `importLocal`, done before taking the write lock.
#[derive(Debug, Clone, PartialEq)]
pub enum Prepared {
    Missing,
    Unreadable {
        key: String,
    },
    /// A key the Node does not list (a path a client made up): nothing was read.
    NotListed,
    /// Read by the Node: already Steve's entries.
    Converted {
        key: String,
        peek: LocalSession,
        harness: String,
        entries: Vec<steve_transcript::Entry>,
    },
}

/// The entries of the Node's history an import looks through for its keys: first as many as the
/// import list shows, then (a key not among them) this many.
const NODE_HISTORY_LIST: usize = 60;
const NODE_HISTORY_MAX: usize = 500;
const NODE_HISTORY_PAGE: u32 = 500;
/// Events per page of an import.
const IMPORT_PAGE: u32 = 500;

/// A history entry of the Node as the import list shows it.
fn listed_session(e: &HistoryEntry) -> LocalSession {
    LocalSession {
        key: e.source.key.clone(),
        harness: steve_core::models::harness_name(&e.source.harness).to_string(),
        title: e.title.clone(),
        cwd: e.directory.clone().unwrap_or_default(),
        when: e.updated.0 as f64,
        size: e.size.unwrap_or(0),
    }
}

/// The Node's history, newest first, at most `max` entries.
async fn node_history(node: &dyn Discovery, max: usize) -> Result<Vec<HistoryEntry>, String> {
    let mut out = vec![];
    let mut cursor = None;
    while out.len() < max {
        let p = node
            .history(PageRequest {
                cursor,
                limit: NODE_HISTORY_PAGE.min((max - out.len()) as u32),
            })
            .await
            .map_err(|e| e.safe_detail)?;
        out.extend(p.items);
        match p.next {
            Some(c) => cursor = Some(c),
            None => break,
        }
    }
    out.truncate(max);
    Ok(out)
}

/// What the Hub's jobs Node lists for import, including when session execution is off.
pub async fn node_candidates(
    node: &dyn Discovery,
    limit: usize,
) -> Result<Vec<LocalSession>, String> {
    Ok(node_history(node, limit)
        .await?
        .iter()
        .map(listed_session)
        .collect())
}

/// The file side of an import through the Node: each key is matched against the Node's own list,
/// never read as a path, then the Node reads and converts it.
pub async fn node_prepare(node: &dyn Discovery, keys: &[String]) -> Vec<Prepared> {
    let mut listed = node_history(node, NODE_HISTORY_LIST).await;
    if let Ok(l) = &listed
        && l.len() == NODE_HISTORY_LIST
        && !keys.iter().all(|k| l.iter().any(|e| &e.source.key == k))
    {
        listed = node_history(node, NODE_HISTORY_MAX).await;
    }
    let listed = match listed {
        Ok(l) => l,
        Err(e) => {
            tracing::warn!("history of the local Node: {e}");
            return keys
                .iter()
                .map(|k| Prepared::Unreadable { key: k.clone() })
                .collect();
        }
    };
    let mut out = vec![];
    for key in keys {
        let Some(entry) = listed.iter().find(|e| &e.source.key == key) else {
            out.push(Prepared::NotListed);
            continue;
        };
        out.push(match node_import(node, entry).await {
            Ok(events) => Prepared::Converted {
                key: key.clone(),
                peek: listed_session(entry),
                harness: entry.source.harness.clone(),
                entries: steve_transcript::of_history(&events),
            },
            Err(ErrorCode::Stale) => Prepared::Missing,
            Err(_) => Prepared::Unreadable { key: key.clone() },
        });
    }
    out
}

/// One entry's events, read page by page.
async fn node_import(
    node: &dyn Discovery,
    entry: &HistoryEntry,
) -> Result<Vec<NodeEvent>, ErrorCode> {
    let now = steve_store::now();
    let op = OperationId::new(uid("imp")).map_err(|_| ErrorCode::Internal)?;
    let r = node
        .import_history(Command {
            id: op.clone(),
            issued_at: Timestamp(now),
            accept_before: Timestamp(now + 60_000),
            grant: GrantId::new("hub").map_err(|_| ErrorCode::Internal)?,
            body: HistoryImport {
                source: entry.source.clone(),
                expected: entry.version.clone(),
            },
        })
        .await
        .map_err(|e| e.code)?;
    let pages = match r.state {
        Observed::Finished {
            result: Some(OperationResult::Pages { operation }),
            ..
        } => operation,
        Observed::Finished { error: Some(e), .. } => return Err(e.code),
        _ => return Err(ErrorCode::Internal),
    };
    let mut events = vec![];
    let mut cursor = None;
    loop {
        let p = node
            .import_page(
                pages.clone(),
                PageRequest {
                    cursor,
                    limit: IMPORT_PAGE,
                },
            )
            .await
            .map_err(|e| e.code)?;
        events.extend(p.events);
        match p.next {
            Some(c) => cursor = Some(c),
            None => break,
        }
    }
    Ok(events)
}

/// The database side of `importLocal` (under the write lock): returns the new session id or the TS error.
pub fn import_prepared(r: &dyn Repo, bus: &Bus, ws: &str, p: Prepared) -> BResult<String> {
    let key = match &p {
        Prepared::Missing => return Err(BackendError::msg(steve_i18n::t!("hub.import.gone"))),
        Prepared::NotListed => {
            return Err(BackendError::msg(steve_i18n::t!("hub.import.not_listed")));
        }
        Prepared::Unreadable { key } | Prepared::Converted { key, .. } => key.clone(),
    };
    if r.import_keys()?.contains(&key) {
        return Err(BackendError::msg(steve_i18n::t!("hub.import.already")));
    }
    let (peek, harness, rows) = match p {
        Prepared::Converted {
            peek,
            harness,
            entries,
            ..
        } => (peek, harness, entries),
        _ => return Err(BackendError::msg(steve_i18n::t!("hub.import.unreadable"))),
    };
    let id = r.session_insert_imported(&ImportedSession {
        workspace_id: ws.into(),
        harness,
        cwd: peek.cwd.clone(),
        title: peek.title.clone(),
        source: format!("本机 {}", peek.harness),
        import_key: key,
        created: peek.when,
    })?;
    for x in &rows {
        r.events_add(&id, &x.kind, &x.data)?;
    }
    bus.publish(steve_types::DomainEvent::from(
        steve_types::WorkEvent::Session {
            ws: ws.into(),
            id: {
                let value: String = id.clone();
                steve_types::SessionId::from(value)
            },
            item_id: {
                let value: Option<Option<String>> = None;
                value.map(|value| value.map(Into::into))
            },
        },
    ));
    Ok(id)
}

/// `removeImported`: only read-only (imported) sessions can be removed.
pub fn remove_imported(r: &dyn Repo, id: &str) -> BResult<()> {
    let ro = r
        .session_get(id)?
        .is_some_and(|s| js::truthy(s.get("readonly")));
    if !ro {
        return Err(BackendError::msg(steve_i18n::t!(
            "hub.import.only_imported"
        )));
    }
    // the store publishes `session` after the delete
    r.session_delete(id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use regex::Regex;
    use steve_node::history::{claude_rows as claude_events, codex_rows as codex_events};

    mod setup_node {
        include!(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/tests/support/setup_node.rs"
        ));
    }

    fn ready_fixture() -> steve_node_api::HarnessCapability {
        serde_json::from_value(json!({
            "name":"pi", "version":"1", "sessions":true, "jobs":true, "models":[],
            "resume":true,"cancel":true,"permissions":false,"images":true,"enforcement":[],
            "adapter":{"title":"Pi","builtin":true,"status":"ready","session":{
                "modes":["full_access"],"models":[{"id":"fixture/test"}]
            }},
            "setup":{"state":"ready","components":[],"can_install":true}
        }))
        .unwrap()
    }

    #[tokio::test]
    async fn installation_revokes_explicit_background_and_model_source_probes() {
        for background in [false, true] {
            let home = tempfile::tempdir().unwrap();
            let cfg = Arc::new(
                Config::from_vars(|k| match k {
                    "HOME" | "STEVE_HOME" => Some(home.path().display().to_string()),
                    _ => None,
                })
                .unwrap(),
            );
            let base = steve_node_api::testkit::FakeNode::new("local");
            base.set_harnesses(vec![ready_fixture()]);
            let node = setup_node::Delayed::new(base);
            let rt = Arc::new(Runtimes::new(cfg.clone()));
            let observed = crate::harnesses::HubHarnesses::of(&cfg);
            rt.set_node(node.clone());
            rt.set_harnesses(observed.clone());
            rt.get(true).await;
            assert_eq!(
                observed.list().iter().find(|h| h.id == "pi").unwrap().ready,
                Some(true)
            );
            node.arm();
            let in_flight = if background {
                rt.cache.lock().unwrap().value.as_mut().unwrap().0 =
                    Instant::now().checked_sub(RUNTIMES_MAX_AGE * 2).unwrap();
                rt.list(false).await;
                None
            } else {
                let rt = rt.clone();
                Some(tokio::spawn(async move { rt.get(true).await }))
            };
            node.entered.notified().await;
            rt.install("pi".into()).await.unwrap();
            node.gate.add_permits(1);
            if let Some(task) = in_flight {
                task.await.unwrap();
            }
            while rt.refreshing.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
            let list = rt.get(false).await;
            let pi = list
                .as_array()
                .unwrap()
                .iter()
                .find(|r| r["id"] == "pi")
                .unwrap();
            assert_eq!(pi["setup"]["state"], "installing");
            assert_eq!(pi["ok"], false);
            assert_eq!(
                observed.list().iter().find(|h| h.id == "pi").unwrap().ready,
                Some(false)
            );

            // The catalogue also has its own periodic follower, independent of Runtimes.
            node.base.set_harnesses(vec![ready_fixture()]);
            node.arm();
            let old = observed.clone();
            let n = node.clone();
            let task = tokio::spawn(async move { old.refresh(n.as_ref(), false).await });
            node.entered.notified().await;
            rt.install("pi".into()).await.unwrap();
            node.gate.add_permits(1);
            task.await.unwrap();
            assert_eq!(
                observed.list().iter().find(|h| h.id == "pi").unwrap().ready,
                None,
                "the old independent follower must not restore readiness evidence"
            );
        }
    }

    // Each new publication (including a failed probe) must revoke a held ready snapshot,
    // whether it comes from Devices or another independent model follower.
    #[tokio::test]
    async fn newer_evidence_revokes_held_model_and_runtime_successes() {
        use steve_node_api::{ErrorCode, SetupState};
        for publisher_is_runtime in [true, false] {
            for old_reader in ["follower", "runtime", "background"] {
                // Runtimes serializes its own probes; the independent follower is the other writer.
                if publisher_is_runtime && old_reader != "follower" {
                    continue;
                }
                for outcome in ["offline", "internal", "unavailable"] {
                    let home = tempfile::tempdir().unwrap();
                    let cfg = Arc::new(
                        Config::from_vars(|k| match k {
                            "HOME" | "STEVE_HOME" => Some(home.path().display().to_string()),
                            _ => None,
                        })
                        .unwrap(),
                    );
                    let base = steve_node_api::testkit::FakeNode::new("local");
                    base.set_harnesses(vec![ready_fixture()]);
                    let node = setup_node::Delayed::new(base);
                    let rt = Arc::new(Runtimes::new(cfg.clone()));
                    let observed = crate::harnesses::HubHarnesses::of(&cfg);
                    rt.set_node(node.clone());
                    rt.set_harnesses(observed.clone());
                    rt.get(true).await;
                    node.arm();
                    let old = if old_reader == "background" {
                        rt.cache.lock().unwrap().value.as_mut().unwrap().0 =
                            Instant::now().checked_sub(RUNTIMES_MAX_AGE * 2).unwrap();
                        rt.list(false).await;
                        None
                    } else {
                        let r = rt.clone();
                        let h = observed.clone();
                        let n = node.clone();
                        Some(tokio::spawn(async move {
                            if old_reader == "runtime" {
                                r.get(true).await;
                            } else {
                                h.refresh(n.as_ref(), false).await;
                            }
                        }))
                    };
                    node.entered.notified().await;
                    if outcome == "unavailable" {
                        let mut cap = ready_fixture();
                        // Keep the obsolete session offer in the fixture: setup evidence wins.
                        cap.setup.as_mut().unwrap().state = SetupState::Unavailable;
                        node.base.set_harnesses(vec![cap]);
                    } else {
                        let code = if outcome == "offline" {
                            ErrorCode::Offline
                        } else {
                            ErrorCode::Internal
                        };
                        // Stay offline throughout release and retries, until explicit reconnect.
                        node.disconnect(code);
                    }
                    if publisher_is_runtime {
                        rt.get(true).await;
                    } else {
                        observed.refresh(node.as_ref(), false).await;
                    }
                    let before = observed.list().into_iter().find(|h| h.id == "pi").unwrap();
                    assert_ne!(before.ready, Some(true));
                    assert!(before.models.is_empty());
                    node.gate.add_permits(1);
                    if let Some(task) = old {
                        task.await.unwrap();
                    }
                    while rt.refreshing.load(Ordering::SeqCst) {
                        tokio::task::yield_now().await;
                    }
                    let after = observed.list().into_iter().find(|h| h.id == "pi").unwrap();
                    assert_eq!(
                        after.ready, before.ready,
                        "{publisher_is_runtime}/{old_reader}/{outcome}"
                    );
                    assert!(after.models.is_empty());
                    assert_eq!(after.modes, before.modes);
                    assert_eq!(after.efforts, before.efforts);

                    // Explicit reconnect: a newly probed ready answer is immediately usable.
                    node.reconnect();
                    node.base.set_harnesses(vec![ready_fixture()]);
                    rt.get(true).await;
                    let restored = observed.list().into_iter().find(|h| h.id == "pi").unwrap();
                    assert_eq!(restored.ready, Some(true));
                    assert_eq!(restored.model_ids(), ["fixture/test"]);
                }
            }
        }
    }

    #[test]
    fn uid_format() {
        let u = uid("ses");
        assert!(
            Regex::new("^ses_[0-9a-f]{16}$")
                .map(|r| r.is_match(&u))
                .unwrap_or(false),
            "{u}"
        );
        assert_eq!(
            sha_hex("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn claude_history_conversion() {
        let text = [
            r#"{"type":"user","cwd":"/w","message":{"content":"<command-name>x"}}"#,
            r#"{"type":"user","message":{"content":"fix the bug"}}"#,
            r#"{"type":"assistant","message":{"content":[{"type":"text","text":"ok"},{"type":"tool_use","id":"t1","name":"Read","input":{"file_path":"/a"}},{"type":"tool_use","name":"Bash","input":{"command":"ls"}}]}}"#,
            "not json",
        ]
        .join("\n");
        let ev = claude_events(&text);
        assert_eq!(ev.len(), 4);
        assert_eq!(
            ev[0],
            ("user".into(), json!({"text": "fix the bug", "via": "me"}))
        );
        assert_eq!(
            ev[1].1,
            json!({"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "ok"}})
        );
        assert_eq!(
            serde_json::to_string(&ev[2].1).unwrap_or_default(),
            r#"{"sessionUpdate":"tool_call","toolCallId":"t1","title":"Read /a","kind":"read","status":"completed"}"#
        );
        assert_eq!(
            serde_json::to_string(&ev[3].1).unwrap_or_default(),
            r#"{"sessionUpdate":"tool_call","title":"Bash ls","kind":"execute","status":"completed"}"#
        );
    }

    #[test]
    fn codex_history_conversion() {
        let text = [
            r#"{"type":"session_meta","payload":{"cwd":"/w"}}"#,
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"text":"<environment_context>"}]}}"#,
            r#"{"type":"response_item","payload":{"type":"message","role":"user","content":[{"text":"hello"},{"text":" there"}]}}"#,
            r#"{"type":"response_item","payload":{"type":"function_call","call_id":"c1","name":"shell","arguments":"{\"cmd\":\"ls\"}"}}"#,
        ]
        .join("\n");
        let ev = codex_events(&text);
        assert_eq!(ev.len(), 2);
        assert_eq!(ev[0].1, json!({"text": "hello there", "via": "me"}));
        assert_eq!(ev[1].1["title"], json!("shell {\"cmd\":\"ls\"}"));
    }

    #[tokio::test]
    async fn expired_runtimes_are_served_while_refreshing() {
        let vars = [
            ("CLAUDE_CODE_EXECUTABLE", "/nonexistent/claude"),
            ("STEVE_ADAPTER_CLAUDE", "/x/claude.js"),
            ("STEVE_ADAPTER_CODEX", "/x/codex"),
        ];
        let cfg = Config::from_vars(|k| {
            vars.iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| v.to_string())
                .or_else(|| k.starts_with("STEVE_ADAPTER_").then(|| "/x/adapter".into()))
        })
        .expect("config");
        let rt = Runtimes::new(Arc::new(cfg));
        let old = Instant::now()
            .checked_sub(RUNTIMES_MAX_AGE * 2)
            .expect("instant");
        rt.cache.lock().expect("cache").value = Some((old, json!("stale")));
        // the expired list comes back at once; a background probe replaces it
        assert_eq!(rt.get(false).await, json!("stale"));
        let deadline = Instant::now() + Duration::from_secs(30);
        while rt.refreshing.load(Ordering::SeqCst) && Instant::now() < deadline {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        let list = rt.get(false).await;
        let claude = list
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == "claude-code")
            .unwrap();
        assert_eq!(claude["ok"], json!(false));
    }

    #[tokio::test]
    async fn a_failed_background_probe_discards_previous_readiness() {
        let home = tempfile::tempdir().unwrap();
        let cfg = Config::from_vars(|key| match key {
            "HOME" | "STEVE_HOME" => Some(home.path().display().to_string()),
            _ => None,
        })
        .unwrap();
        let rt = Runtimes::new(Arc::new(cfg));
        let node = steve_node_api::testkit::FakeNode::new("local");
        node.refuse_next(
            "probe",
            steve_node_api::ApiError::new(steve_node_api::ErrorCode::Offline, "node offline"),
        );
        rt.set_node(node);
        let old = Instant::now().checked_sub(RUNTIMES_MAX_AGE * 2).unwrap();
        rt.cache.lock().unwrap().value = Some((old, json!("stale")));
        assert_eq!(rt.list(false).await, json!("stale"));
        tokio::time::timeout(Duration::from_secs(30), async {
            while rt.refreshing.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("background probe completed");
        assert!(
            rt.cached().is_none(),
            "failed refresh must revoke the old result"
        );
    }

    /// A Node that cannot answer yet is unknown; a local file is not proof of a usable session.
    #[tokio::test]
    async fn a_node_not_answering_is_not_a_missing_adapter() {
        let dir = tempfile::tempdir().unwrap();
        let claude = dir.path().join("claude.js");
        std::fs::write(&claude, "").unwrap();
        let claude = claude.to_string_lossy().into_owned();
        let vars = [
            ("STEVE_ADAPTER_CLAUDE", claude.as_str()),
            ("STEVE_ADAPTER_CODEX", "/nonexistent/codex"),
        ];
        let cfg = Config::from_vars(|k| {
            vars.iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| v.to_string())
                .or_else(|| {
                    k.starts_with("STEVE_ADAPTER_")
                        .then(|| "/nonexistent/adapter".into())
                })
        })
        .expect("config");
        let rt = Runtimes::new(Arc::new(cfg));
        let node = steve_node_api::testkit::FakeNode::new("local");
        node.refuse_next(
            "probe",
            steve_node_api::ApiError::new(steve_node_api::ErrorCode::Offline, "node offline"),
        );
        rt.set_node(node.clone());
        let list = rt.list(false).await;
        let row = |l: &Value, id: &str| {
            l.as_array()
                .unwrap()
                .iter()
                .find(|r| r["id"] == id)
                .unwrap()
                .clone()
        };
        assert_eq!(
            row(&list, "claude-code")["ok"],
            json!(false),
            "a file cannot establish readiness: {list}"
        );
        assert_eq!(row(&list, "codex")["note"], json!("unknown"));
        assert_eq!(row(&list, "pi")["setup"]["state"], json!("unknown"));
        assert_eq!(row(&list, "pi")["setup"]["can_install"], json!(false));
        assert!(rt.cached().is_none(), "not kept");
        // the Node answers now: its report, kept
        rt.list(false).await;
        assert!(rt.cached().is_some());
    }

    /// Harness parity S1: an adapter the Hub's manifest adds is a runtime like the built-in ones,
    /// with what the local Node's probe said about it.
    #[test]
    fn declared_adapters_are_runtimes() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("harness.json"),
            r#"{"adapters": {"mine": {"command": "/nonexistent/mine", "name": "Mine"}}}"#,
        )
        .unwrap();
        let home = dir.path().to_string_lossy().into_owned();
        let vars = [
            ("STEVE_HOME", home.as_str()),
            ("STEVE_ADAPTER_CLAUDE", "/x/claude.js"),
            ("STEVE_ADAPTER_CODEX", "/x/codex"),
        ];
        let cfg = Config::from_vars(|k| {
            vars.iter()
                .find(|(n, _)| *n == k)
                .map(|(_, v)| v.to_string())
                .or_else(|| k.starts_with("STEVE_ADAPTER_").then(|| "/x/adapter".into()))
        })
        .expect("config");
        let all = rows(&cfg, None);
        let native = |rows: &[Value]| rows.iter().find(|r| r["id"] == "pi").unwrap().clone();
        assert_eq!(native(&all)["modes"], json!(["full_access"]));
        assert_eq!(
            all.len(),
            steve_acp::adapters::builtin_ids().count() + 1,
            "the built-in ones and the declared one alike"
        );
        let mine = |rows: &[Value]| rows.iter().find(|r| r["id"] == "mine").unwrap().clone();
        let row = mine(&all);
        assert_eq!(row["name"], json!("Mine"));
        assert_eq!(row["custom"], json!(true));
        assert_eq!(row["ok"], json!(false));
        assert_eq!(row["note"], json!("unknown"));
        let reported = steve_node_api::HarnessCapability {
            name: "mine".into(),
            version: "1.0".into(),
            sessions: true,
            jobs: false,
            models: vec!["m1".into()],
            resume: true,
            cancel: true,
            permissions: true,
            images: false,
            enforcement: vec![],
            setup: None,
            adapter: Some(steve_node_api::AdapterInfo {
                title: "Mine".into(),
                builtin: false,
                status: steve_node_api::AdapterStatus::Ready,
                error: None,
                version: Some("1.0".into()),
                load_session: true,
                list_sessions: false,
                mcp_http: true,
                embedded_context: false,
                auth: vec![],
                install_hint: None,
                system_prompt: false,
                session: Some(steve_node_api::AdapterOffer {
                    modes: vec![steve_node_api::PermissionMode::Ask],
                    models: vec![steve_node_api::AdapterModel {
                        id: "m1".into(),
                        name: None,
                        description: None,
                    }],
                    default_model: Some("m1".into()),
                    efforts: vec![],
                }),
                session_error: None,
                session_auth_required: None,
                execution_modes: None,
                update: None,
                resume_command: None,
                fork_command: None,
            }),
        };
        let mut no_session = reported.clone();
        let row = mine(&rows(&cfg, Some(&[reported])));
        assert_eq!(row["ok"], json!(true));
        assert_eq!(row["models"], json!(["m1"]));
        assert_eq!(row["modes"], json!(["ask"]));
        assert_eq!(row["caps"]["mcp_http"], json!(true));
        let shown = shown(json!([row]));
        assert!(
            shown[0]["note"]
                .as_str()
                .is_some_and(|n| !n.is_empty() && n != "acp")
        );
        // initialize answered but no scratch session: said so
        no_session.adapter.as_mut().unwrap().session = None;
        no_session.adapter.as_mut().unwrap().session_error = Some("sign in first".into());
        let mut native_no_login = no_session.clone();
        native_no_login.name = "pi".into();
        assert_eq!(
            native(&rows(&cfg, Some(&[native_no_login])))["modes"],
            json!(["full_access"])
        );
        let row = mine(&rows(&cfg, Some(&[no_session])));
        assert_eq!(
            (&row["ok"], &row["note"]),
            (&json!(false), &json!("session_failed"))
        );
        assert_eq!(row["error"], json!("sign in first"));
        assert!(cfg.harness_ids().contains(&"mine".to_string()));
    }
}
