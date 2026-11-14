//! What this machine is and what it can run (system.ts): the Hub registers itself as a device, reports the
//! harnesses it finds, and imports local harness history (Claude Code, Codex) as read-only sessions.
//! External commands run through `tokio::process` with the TS 8 s timeout; file scans run on the blocking pool.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tokio::sync::Mutex;

use crate::backend::{BResult, BackendError, ImportedSession, Repo};
use crate::bus::{Bus, HubEvent};
use crate::config::{Config, home_dir};
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

type RuntimesCache = Arc<std::sync::Mutex<Option<(Instant, Value)>>>;

/// `runtimes(fresh)` with its 5-minute cache. The probe never blocks the async runtime, and once there is a
/// result `/api/state` never waits for one: an expired result is served while a background probe replaces it.
pub struct Runtimes {
    cfg: Arc<Config>,
    cache: RuntimesCache,
    /// a background refresh is running
    refreshing: Arc<AtomicBool>,
    /// callers that must wait for a probe (nothing cached yet, or `fresh`) probe one at a time
    probing: Mutex<()>,
    /// The local Node (`STEVE_NODE_MODE=local`): it runs the harness binaries, not the Hub.
    node: std::sync::OnceLock<Arc<dyn steve_node_api::Node>>,
}

impl Runtimes {
    pub fn new(cfg: Arc<Config>) -> Self {
        Runtimes {
            cfg,
            cache: Arc::default(),
            refreshing: Arc::default(),
            probing: Mutex::new(()),
            node: std::sync::OnceLock::new(),
        }
    }

    /// Probe through the local Node from now on.
    pub fn set_node(&self, node: Arc<dyn steve_node_api::Node>) {
        let _ = self.node.set(node);
    }

    fn cached(&self) -> Option<(Instant, Value)> {
        self.cache.lock().unwrap_or_else(|e| e.into_inner()).clone()
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
                let (cfg, cache, refreshing, node) = (
                    self.cfg.clone(),
                    self.cache.clone(),
                    self.refreshing.clone(),
                    self.node.get().cloned(),
                );
                tokio::spawn(async move {
                    let (list, answered) = probe(&cfg, node.as_deref(), false).await;
                    if answered {
                        *cache.lock().unwrap_or_else(|e| e.into_inner()) =
                            Some((Instant::now(), list));
                    }
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
        let (list, answered) = probe(&self.cfg, self.node.get().map(|n| n.as_ref()), fresh).await;
        if answered {
            *self.cache.lock().unwrap_or_else(|e| e.into_inner()) =
                Some((Instant::now(), list.clone()));
        }
        list
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
/// do, as the local Node's probe said. Without a Node: whether its command is there.
fn rows(cfg: &Config, harnesses: Option<&[steve_node_api::HarnessCapability]>) -> Vec<Value> {
    use steve_node_api::AdapterStatus;
    cfg.adapters
        .iter()
        .map(|(id, path)| {
            let declared = cfg.declared_adapters.get(id);
            let spec = steve_acp::adapters::spec(id, declared);
            let reported = harnesses.and_then(|hs| hs.iter().find(|h| &h.name == id));
            let adapter = reported.and_then(|h| h.adapter.as_ref());
            let session = adapter.and_then(|a| a.session.as_ref());
            let (ok, note) = match (adapter.map(|a| a.status), harnesses) {
                (Some(AdapterStatus::Ready), _) if session.is_some() => (true, "acp"),
                (Some(AdapterStatus::Ready), _) => (false, "session_failed"),
                (Some(AdapterStatus::AdapterMissing), _) => (false, "adapter_missing"),
                (Some(AdapterStatus::InitializeFailed), _) => (false, "initialize_failed"),
                // older protocol without `adapter`: trust `sessions` from the Node's probe
                (None, Some(_)) if reported.is_some_and(|r| r.sessions) => (true, "acp"),
                // without a Node's answer: there or not
                (None, None) if command_present(path) => (true, "acp"),
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
                _ => spec.properties.fallback_models.clone(),
            };
            // A missing login/scratch session does not make declared permission limits unknown.
            let modes = session.map(|o| o.modes.clone()).or_else(|| {
                steve_core::harnesses::Harness::declared(id, declared).modes
            });
            json!({
                "id": id,
                "name": name,
                "builtin": spec.builtin,
                "custom": !spec.builtin,
                "path": path,
                "version": reported.map(|h| version_of(&h.version)).unwrap_or_default(),
                "adapter_version": adapter.and_then(|a| a.version.clone()),
                // one Steve installed: its update check (steve_acp::managed)
                "update": adapter.and_then(|a| a.update.clone()),
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
            })
        })
        .collect()
}

fn command_present(path: &str) -> bool {
    if path.is_empty() {
        return false;
    }
    let p = Path::new(path);
    if p.components().count() > 1 || p.is_absolute() {
        if steve_acp::is_node_script(p) {
            p.is_file()
        } else {
            steve_acp::is_executable_file(p)
        }
    } else {
        steve_acp::which_in(path, std::env::var_os("PATH").as_deref()).is_some()
    }
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
/// rows to whether each adapter's command is there, and the list is not kept.
async fn probe(
    cfg: &Config,
    node: Option<&dyn steve_node_api::Node>,
    fresh: bool,
) -> (Value, bool) {
    let harnesses = match node {
        Some(n) => node_harnesses(n, fresh).await,
        None => None,
    };
    let answered = node.is_none() || harnesses.is_some();
    (json!(rows(cfg, harnesses.as_deref())), answered)
}

// ── local harness history ──

pub use steve_node::history::LocalSession;
use steve_node_api::{
    Command, Discovery, ErrorCode, GrantId, HistoryEntry, HistoryImport, NodeEvent, Observed,
    OperationId, OperationResult, PageRequest, Timestamp,
};

/// Where this machine's harnesses keep their history, as the Hub reads it itself (the local Node off).
fn homes(steve_home: &Path) -> steve_node::history::Homes {
    let user_home = home_dir();
    steve_node::history::Homes {
        claude_dir: user_home.join(".claude"),
        codex_home: user_home.join(".codex"),
        user_home,
        steve_home: steve_home.to_path_buf(),
    }
}

/// `localSessions(limit)` without the `imported` flag: newest first, Steve's own runs skipped. Blocking.
pub fn local_candidates(steve_home: &Path, limit: usize) -> Vec<LocalSession> {
    steve_node::history::candidates(&homes(steve_home), limit)
}

/// The file side of `importLocal`, done before taking the write lock.
#[derive(Debug, Clone, PartialEq)]
pub enum Prepared {
    Missing,
    Unreadable {
        key: String,
    },
    /// A key the Node does not list (a path a client made up): nothing was read.
    NotListed,
    /// Read by the Hub: ACP-shaped rows, read into Steve's terms when stored.
    Ready {
        key: String,
        peek: LocalSession,
        harness: String,
        events: Vec<(String, Value)>,
    },
    /// Read by the Node: already Steve's entries.
    Converted {
        key: String,
        peek: LocalSession,
        harness: String,
        entries: Vec<steve_transcript::Entry>,
    },
}

/// Reads and converts a local session file (blocking).
pub fn prepare_import(steve_home: &Path, key: &str) -> Prepared {
    match steve_node::history::read(steve_home, key) {
        steve_node::history::Read::Missing => Prepared::Missing,
        steve_node::history::Read::Unreadable => Prepared::Unreadable { key: key.into() },
        steve_node::history::Read::Ready {
            peek,
            harness,
            rows,
        } => Prepared::Ready {
            key: key.into(),
            peek,
            harness,
            events: rows,
        },
    }
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

/// What the Hub's own device's Node lists for import (the local Node on): `limit` newest.
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
        Prepared::Unreadable { key }
        | Prepared::Ready { key, .. }
        | Prepared::Converted { key, .. } => key.clone(),
    };
    if r.import_keys()?.contains(&key) {
        return Err(BackendError::msg(steve_i18n::t!("hub.import.already")));
    }
    let (peek, harness, rows) = match p {
        Prepared::Ready {
            peek,
            harness,
            events,
            ..
        } => {
            // stored in Steve's own terms, like everything written now
            let mut reader = steve_transcript::LegacyReader::new(&peek.cwd);
            let rows: Vec<steve_transcript::Entry> = events
                .iter()
                .flat_map(|(kind, data)| reader.read(kind, data))
                .collect();
            (peek, harness, rows)
        }
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
    bus.publish(HubEvent::Session {
        id: id.clone(),
        item_id: None,
    });
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
    use steve_node::history::{
        claude_rows as claude_events, codex_rows as codex_events, peek_claude,
    };

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

    #[test]
    fn peek_and_prepare() {
        let dir = tempfile::tempdir().expect("tmp");
        let f = dir.path().join("a.jsonl");
        std::fs::write(
            &f,
            "{\"type\":\"user\",\"cwd\":\"/proj\",\"message\":{\"content\":\"hi\"}}\n",
        )
        .expect("write");
        let steve_home = dir.path().join("steve");
        let p = peek_claude(&steve_home, &f).expect("peek");
        assert_eq!(
            (p.title.as_str(), p.cwd.as_str(), p.harness.as_str()),
            ("hi", "/proj", "Claude Code")
        );
        assert!(matches!(
            prepare_import(&steve_home, &format!("cc:{}", f.display())),
            Prepared::Ready { .. }
        ));
        assert_eq!(
            prepare_import(&steve_home, "cc:/nonexistent/x.jsonl"),
            Prepared::Missing
        );
        assert_eq!(prepare_import(&steve_home, "nocolon"), Prepared::Missing);
        // runs inside STEVE_HOME are Steve's own
        assert!(peek_claude(Path::new("/pro"), &f).is_none());
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
        *rt.cache.lock().expect("cache") = Some((old, json!("stale")));
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

    /// A Node that cannot answer yet (just after the Hub starts) is not "no adapter": the rows say
    /// whether each adapter is there, and the next request asks the Node again.
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
            json!(true),
            "there: {list}"
        );
        assert_eq!(row(&list, "codex")["note"], json!("adapter_missing"));
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
        assert_eq!(row["note"], json!("adapter_missing"));
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
