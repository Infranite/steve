//! What this machine is and what it can run (system.ts): the Hub registers itself as a device, reports the
//! harnesses it finds, and imports local harness history through the Node as read-only sessions.
//! Host commands run through `tokio::process` with an 8 s timeout.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

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

/// The single capability report used by runtime display, model selection and dispatch.
#[derive(Clone)]
pub(crate) struct RuntimeReport {
    pub caps: Vec<steve_node_api::HarnessCapability>,
    pub checked_at: i64,
    pub received: tokio::time::Instant,
    pub link_generation: Option<u64>,
}
impl RuntimeReport {
    pub(crate) fn recent(&self) -> bool {
        self.received.elapsed() < Duration::from_secs(120)
    }
}
pub(crate) static RUNTIME_SPACE: steve_runtime::cache::CacheSpace<String, RuntimeReport> =
    steve_runtime::cache::CacheSpace {
        name: "runtimes",
        ttl: RUNTIMES_MAX_AGE,
        // Devices can show the previous report while discovery refreshes; admission requires fresh evidence.
        stale_for: Duration::from_secs(300),
        share_pct: 10,
        tags: |node| vec![steve_runtime::cache::Tag::of("runtimes", node)],
        weigh: |v| serde_json::to_vec(&v.caps).map_or(1, |v| v.len().min(u32::MAX as usize) as u32),
    };
#[derive(Clone, Copy)]
pub(crate) enum ReportError {
    Unavailable,
    Superseded,
}
pub(crate) struct RuntimeReports {
    pub cache: steve_runtime::cache::Cached<String, RuntimeReport>,
    caches: Arc<steve_runtime::cache::Caches>,
    nodes: std::sync::RwLock<std::collections::BTreeSet<String>>,
    updated: std::sync::OnceLock<Box<dyn Fn() + Send + Sync>>,
    model_node: std::sync::OnceLock<String>,
}
impl RuntimeReports {
    pub(crate) fn new(caches: Arc<steve_runtime::cache::Caches>) -> Arc<Self> {
        Arc::new(Self {
            cache: caches.space(&RUNTIME_SPACE),
            caches,
            nodes: Default::default(),
            updated: Default::default(),
            model_node: Default::default(),
        })
    }
    pub(crate) fn bind_model_node(&self, id: &str) {
        let _ = self.model_node.set(id.to_owned());
    }
    pub(crate) fn model_node(&self) -> Option<&str> {
        self.model_node.get().map(String::as_str)
    }
    fn invalidate_local(&self) {
        self.invalidate("local");
        if let Some(node) = self.model_node().filter(|node| *node != "local") {
            self.invalidate(node);
        }
    }
    pub(crate) fn on_update(&self, callback: impl Fn() + Send + Sync + 'static) {
        let _ = self.updated.set(Box::new(callback));
    }
    fn changed(&self) {
        self.caches
            .invalidate(&[steve_runtime::cache::Tag::global("state")]);
        if let Some(callback) = self.updated.get() {
            callback();
        }
    }
    pub(crate) fn retain_nodes(&self, linked: &std::collections::BTreeSet<String>) {
        let mut nodes = self.nodes.write().unwrap();
        for node in nodes.iter().filter(|id| !linked.contains(*id)) {
            self.invalidate(node);
        }
        nodes.retain(|id| linked.contains(id));
    }
    pub(crate) fn nodes(&self) -> Vec<String> {
        self.nodes.read().unwrap().iter().cloned().collect()
    }
    pub(crate) fn invalidate(&self, node: &str) {
        self.caches
            .invalidate(&[steve_runtime::cache::Tag::of("runtimes", node)]);
    }
    pub(crate) fn peek(&self, node: &str) -> Option<(Arc<RuntimeReport>, bool)> {
        use futures::FutureExt;
        self.cache.peek(&node.to_owned()).now_or_never().flatten()
    }
    pub(crate) async fn read(&self, node: &str) -> Option<(Arc<RuntimeReport>, bool)> {
        self.cache.peek(&node.to_owned()).await
    }
    pub(crate) async fn load(
        &self,
        id: &str,
        node: Option<&dyn steve_node_api::Node>,
        generation: Option<u64>,
        fresh: bool,
        force: bool,
    ) -> Result<Arc<RuntimeReport>, ReportError> {
        self.nodes.write().unwrap().insert(id.to_owned());
        // A cold caller shares discovery already in flight. Revoking an absent
        // entry would force a duplicate probe before the caller's outer deadline.
        if self
            .read(id)
            .await
            .is_some_and(|(v, _)| force || v.link_generation != generation)
        {
            self.invalidate(id);
        }
        let mut loaded = false;
        let result = self
            .cache
            .get_or_load(id.to_owned(), || async {
                loaded = true;
                let tag = steve_runtime::cache::Tag::of("runtimes", id);
                let version = self.caches.generation(&tag);
                let caps = match node {
                    Some(node) => {
                        match tokio::time::timeout(Duration::from_secs(45), node.probe(fresh)).await
                        {
                            Ok(Ok(report)) if report.node.as_str() == id => report.harnesses,
                            Ok(Ok(report)) => {
                                tracing::warn!(node=id, reported=%report.node, "runtime probe returned another Node identity");
                                self.invalidate(id); return Err(ReportError::Unavailable);
                            }
                            Ok(Err(error)) => {
                                tracing::debug!(node=id, "runtime probe failed: {}", error.safe_detail);
                                self.invalidate(id); return Err(ReportError::Unavailable);
                            }
                            Err(_) => {
                                tracing::warn!(node=id, "runtime probe timed out");
                                self.invalidate(id); return Err(ReportError::Unavailable);
                            }
                        }
                    }
                    None => {
                        self.invalidate(id);
                        return Err(ReportError::Unavailable);
                    }
                };
                // Cached permits the reader that started a load before invalidation to
                // finish with its old result. Devices has a stronger contract: even an
                // already waiting HTTP request must observe an accepted installation.
                // Reject that result here; reload makes at most one subsequent read.
                if self.caches.generation(&tag) != version {
                    return Err(ReportError::Superseded);
                }
                Ok(RuntimeReport {
                    caps,
                    checked_at: steve_store::now(),
                    received: tokio::time::Instant::now(),
                    link_generation: generation,
                })
            })
            .await;
        if loaded {
            self.changed();
        }
        result
    }
    #[cfg(test)]
    pub(crate) async fn seed(
        &self,
        node: &str,
        caps: Vec<steve_node_api::HarnessCapability>,
        generation: Option<u64>,
    ) {
        self.nodes.write().unwrap().insert(node.to_owned());
        self.invalidate(node);
        self.cache
            .get_or_load(node.to_owned(), || async {
                Ok::<_, ()>(RuntimeReport {
                    caps,
                    checked_at: steve_store::now(),
                    received: tokio::time::Instant::now(),
                    link_generation: generation,
                })
            })
            .await
            .unwrap();
        self.changed();
    }
}
#[derive(Clone)]
pub struct Runtimes {
    cfg: Arc<Config>,
    pub(crate) reports: Arc<RuntimeReports>,
    refreshing: Arc<AtomicBool>,
    node: Arc<std::sync::OnceLock<Arc<dyn steve_node_api::Node>>>,
}
impl Runtimes {
    pub(crate) fn from_reports(cfg: Arc<Config>, reports: Arc<RuntimeReports>) -> Self {
        Self {
            cfg,
            reports,
            refreshing: Arc::default(),
            node: Arc::default(),
        }
    }
    #[cfg(test)]
    fn new(cfg: Arc<Config>, caches: Arc<steve_runtime::cache::Caches>) -> Self {
        Self::from_reports(cfg, RuntimeReports::new(caches))
    }
    pub fn set_node(&self, node: Arc<dyn steve_node_api::Node>) {
        let _ = self.node.set(node);
    }
    /// Both views must read the same report handle, never publish into one another.
    pub fn set_harnesses(&self, harnesses: Arc<crate::harnesses::HubHarnesses>) {
        debug_assert!(Arc::ptr_eq(&self.reports, &harnesses.reports));
    }
    pub async fn install(&self, harness: String) -> Result<(), steve_node_api::Error> {
        use steve_node_api::*;
        let Some(node) = self.node.get() else {
            return Err(Error::new(
                Code::Unavailable,
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

    fn invalidate(&self) {
        self.reports.invalidate_local();
    }
    pub async fn get(&self, fresh: bool) -> Value {
        shown(self.list(fresh).await)
    }
    async fn reload(&self, fresh: bool) -> Value {
        // A write can overtake an in-flight discovery. One subsequent read is bounded;
        // continuing churn is unknown, never permission to publish an earlier report.
        for _ in 0..2 {
            match self
                .reports
                .load(
                    "local",
                    self.node.get().map(|n| n.as_ref()),
                    None,
                    fresh,
                    false,
                )
                .await
            {
                Ok(report) => return Value::Array(rows(&self.cfg, Some(&report.caps))),
                Err(ReportError::Unavailable) => break,
                Err(ReportError::Superseded) => {}
            }
        }
        Value::Array(rows(&self.cfg, None))
    }
    async fn list(&self, fresh: bool) -> Value {
        // No configured local Node is not a failed observation. In particular an
        // idle state poll must not revoke dependency generations or reconcile models.
        if self.node.get().is_none() {
            return Value::Array(rows(&self.cfg, None));
        }
        if fresh {
            self.invalidate();
        }
        if !fresh && let Some((report, current)) = self.reports.read("local").await {
            if !current
                && self
                    .refreshing
                    .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
            {
                let me = self.clone();
                tokio::spawn(async move {
                    me.reload(false).await;
                    me.refreshing.store(false, Ordering::SeqCst);
                });
            }
            return Value::Array(rows(&self.cfg, Some(&report.caps)));
        }
        self.reload(fresh).await
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

fn rows(cfg: &Config, harnesses: Option<&[steve_node_api::HarnessCapability]>) -> Vec<Value> {
    use steve_node_api::AdapterStatus;
    cfg.adapters
        .iter()
        .map(|(id, path)| {
            let declared = cfg.declared_adapters.get(id);
            let spec = crate::harnesses::adapter_spec(id, declared);
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
                (reported.is_none() && !steve_harness_spec::components(id).is_empty()).then(|| steve_node_api::HarnessSetup {
                    state: steve_node_api::SetupState::Unknown,
                    components: steve_harness_spec::components(id).iter().map(|c| steve_node_api::HarnessComponent {
                        id: c.id.into(), name: c.name.into(), package: c.package.into(), version: None, path: None, status: steve_node_api::ComponentStatus::Unknown, detected: None,
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

// ── local harness history ──

pub use steve_node::history::LocalSession;
use steve_node_api::{
    Code, Command, Discovery, GrantId, HistoryEntry, HistoryImport, NodeEvent, Observed,
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
            Err(Code::Stale) => Prepared::Missing,
            Err(_) => Prepared::Unreadable { key: key.clone() },
        });
    }
    out
}

/// One entry's events, read page by page.
async fn node_import(node: &dyn Discovery, entry: &HistoryEntry) -> Result<Vec<NodeEvent>, Code> {
    let now = steve_store::now();
    let op = OperationId::new(uid("imp")).map_err(|_| Code::Internal)?;
    let r = node
        .import_history(Command {
            id: op.clone(),
            issued_at: Timestamp(now),
            accept_before: Timestamp(now + 60_000),
            grant: GrantId::new("hub").map_err(|_| Code::Internal)?,
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
        _ => return Err(Code::Internal),
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
    #[tokio::test]
    async fn a_changed_node_never_serves_the_previous_runtime_display() {
        let home = tempfile::tempdir().unwrap();
        let cfg = Arc::new(
            Config::from_vars(|k| match k {
                "HOME" | "STEVE_HOME" => Some(home.path().display().to_string()),
                k if k.starts_with("STEVE_ADAPTER_") => Some("/usr/bin/false".into()),
                _ => None,
            })
            .unwrap(),
        );
        let node = steve_node_api::testkit::FakeNode::new("local");
        node.set_harnesses(vec![ready_fixture()]);
        let caches = Arc::new(steve_runtime::cache::Caches::new(cfg.cache));
        let rt = Runtimes::new(cfg.clone(), caches.clone());
        rt.set_node(node.clone());
        rt.get(true).await;
        let mut changed = ready_fixture();
        changed.version = "2".into();
        changed.adapter.as_mut().unwrap().version = Some("2".into());
        node.set_harnesses(vec![changed]);
        caches.invalidate(&[steve_runtime::cache::Tag::of("runtimes", "local")]);
        let list = rt.get(false).await;
        let pi = list
            .as_array()
            .unwrap()
            .iter()
            .find(|v| v["id"] == "pi")
            .unwrap();
        assert_eq!(pi["version"], "2");
    }

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
            let caches = Arc::new(steve_runtime::cache::Caches::new(cfg.cache));
            let rt = Arc::new(Runtimes::new(cfg.clone(), caches.clone()));
            let observed = crate::harnesses::HubHarnesses::of(&cfg, rt.reports.clone());
            rt.set_node(node.clone());
            rt.set_harnesses(observed.clone());
            rt.get(true).await;
            assert_eq!(
                observed.list().iter().find(|h| h.id == "pi").unwrap().ready,
                Some(true)
            );
            node.arm();
            let in_flight = if background {
                caches.invalidate(&[steve_runtime::cache::Tag::global("runtimes")]);
                let r = rt.clone();
                Some(tokio::spawn(async move { r.list(false).await }))
            } else {
                let rt = rt.clone();
                Some(tokio::spawn(async move { rt.get(true).await }))
            };
            node.entered.notified().await;
            assert_eq!(
                observed.list().iter().find(|h| h.id == "pi").unwrap().ready,
                None,
                "invalidating the only shared report makes model evidence unknown too"
            );
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
        use steve_node_api::{Code, SetupState};
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
                    let caches = Arc::new(steve_runtime::cache::Caches::new(cfg.cache));
                    let rt = Arc::new(Runtimes::new(cfg.clone(), caches.clone()));
                    let observed = crate::harnesses::HubHarnesses::of(&cfg, rt.reports.clone());
                    rt.set_node(node.clone());
                    rt.set_harnesses(observed.clone());
                    rt.get(true).await;
                    node.arm();
                    let old = if old_reader == "background" {
                        rt.invalidate();
                        let r = rt.clone();
                        Some(tokio::spawn(async move {
                            r.list(false).await;
                        }))
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
                            Code::Unavailable
                        } else {
                            Code::Internal
                        };
                        // Stay offline throughout release and retries, until explicit reconnect.
                        node.disconnect(code);
                    }
                    // Both views use the same single-flight slot. Revocation is
                    // immediately visible to models while discovery is still pending.
                    rt.invalidate();
                    assert_ne!(
                        observed.list().iter().find(|h| h.id == "pi").unwrap().ready,
                        Some(true)
                    );
                    node.gate.add_permits(1);
                    if let Some(task) = old {
                        task.await.unwrap();
                    }
                    if publisher_is_runtime {
                        rt.get(true).await;
                    } else {
                        observed.refresh(node.as_ref(), false).await;
                    }
                    let before = observed.list().into_iter().find(|h| h.id == "pi").unwrap();
                    assert_ne!(before.ready, Some(true));
                    assert!(before.models.is_empty());
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
    async fn no_local_node_stays_unknown_without_invalidating_idle_state() {
        let home = tempfile::tempdir().unwrap();
        let cfg = Arc::new(
            Config::from_vars(|key| match key {
                "HOME" | "STEVE_HOME" => Some(home.path().display().to_string()),
                _ => None,
            })
            .unwrap(),
        );
        let caches = Arc::new(steve_runtime::cache::Caches::new(cfg.cache));
        let runtimes = Runtimes::new(cfg, caches.clone());
        for fresh in [false, false, true] {
            let list = runtimes.list(fresh).await;
            assert!(
                list.as_array()
                    .unwrap()
                    .iter()
                    .all(|row| row["note"] == "unknown")
            );
        }
        assert_eq!(
            caches.generation(&steve_runtime::cache::Tag::of("runtimes", "local")),
            0
        );
        assert_eq!(
            caches.generation(&steve_runtime::cache::Tag::global("state")),
            0
        );
        assert!(runtimes.reports.nodes().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn expired_runtimes_are_served_while_refreshing() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path().display().to_string();
        let vars = [
            ("STEVE_HOME", home.as_str()),
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
        let rt = Runtimes::new(
            Arc::new(cfg),
            Arc::new(steve_runtime::cache::Caches::new(Default::default())),
        );

        let node = steve_node_api::testkit::FakeNode::new("local");
        node.set_harnesses(vec![]);
        rt.set_node(node);
        rt.reports.seed("local", vec![ready_fixture()], None).await;
        tokio::time::advance(RUNTIMES_MAX_AGE).await;
        // the expired list comes back at once; a background probe replaces it
        assert!(
            rt.get(false)
                .await
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["id"] == "pi" && r["ok"] == true)
        );
        tokio::time::timeout(Duration::from_secs(30), async {
            while rt.refreshing.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("background probe completed");
        let list = rt.get(false).await;
        let claude = list
            .as_array()
            .unwrap()
            .iter()
            .find(|r| r["id"] == "claude-code")
            .unwrap();
        assert_eq!(claude["ok"], json!(false));
    }

    #[tokio::test(start_paused = true)]
    async fn a_failed_background_probe_discards_previous_readiness() {
        let home = tempfile::tempdir().unwrap();
        let cfg = Config::from_vars(|key| match key {
            "HOME" | "STEVE_HOME" => Some(home.path().display().to_string()),
            _ => None,
        })
        .unwrap();
        let rt = Runtimes::new(
            Arc::new(cfg),
            Arc::new(steve_runtime::cache::Caches::new(Default::default())),
        );
        let node = steve_node_api::testkit::FakeNode::new("local");
        node.refuse_next(
            "probe",
            steve_node_api::Error::new(steve_node_api::Code::Unavailable, "node offline"),
        );
        rt.set_node(node);

        rt.reports.seed("local", vec![ready_fixture()], None).await;
        tokio::time::advance(RUNTIMES_MAX_AGE).await;
        assert!(
            rt.list(false)
                .await
                .as_array()
                .unwrap()
                .iter()
                .any(|r| r["id"] == "pi" && r["ok"] == true)
        );
        tokio::time::timeout(Duration::from_secs(30), async {
            while rt.refreshing.load(Ordering::SeqCst) {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("background probe completed");
        assert!(
            rt.reports.read("local").await.is_none(),
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
        let home = dir.path().display().to_string();
        let vars = [
            ("STEVE_HOME", home.as_str()),
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
        let rt = Runtimes::new(
            Arc::new(cfg),
            Arc::new(steve_runtime::cache::Caches::new(Default::default())),
        );
        let node = steve_node_api::testkit::FakeNode::new("local");
        node.refuse_next(
            "probe",
            steve_node_api::Error::new(steve_node_api::Code::Unavailable, "node offline"),
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
        assert!(rt.reports.read("local").await.is_none(), "not kept");
        // the Node answers now: its report, kept
        rt.list(false).await;
        assert!(rt.reports.read("local").await.is_some());
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
            steve_harness_spec::builtin_ids().count() + 1,
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
            read_attached: false,
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
                capabilities: None,
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

pub(crate) const CACHE_SPEC: (&str, u8) = (RUNTIME_SPACE.name, RUNTIME_SPACE.share_pct);

#[cfg(test)]
pub(crate) fn register_cache_spaces(caches: &steve_runtime::cache::Caches) {
    let _ = caches.space(&RUNTIME_SPACE);
}
