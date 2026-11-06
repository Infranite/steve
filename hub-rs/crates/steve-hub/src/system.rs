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
                    let list = probe(&cfg, node.as_deref(), false).await;
                    *cache.lock().unwrap_or_else(|e| e.into_inner()) = Some((Instant::now(), list));
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
        let list = probe(&self.cfg, self.node.get().map(|n| n.as_ref()), fresh).await;
        *self.cache.lock().unwrap_or_else(|e| e.into_inner()) =
            Some((Instant::now(), list.clone()));
        list
    }
}

/// A probed list as the web shows it: each `note` code (see [`probe`]) as text, a missing path as "not found".
fn shown(mut list: Value) -> Value {
    for r in list.as_array_mut().into_iter().flatten() {
        let note = match r["note"].as_str().unwrap_or_default() {
            "no_claude" => steve_i18n::t!("hub.runtime.no_claude"),
            "no_codex" => steve_i18n::t!("hub.runtime.no_codex"),
            "acp" => steve_i18n::t!("hub.runtime.via_acp"),
            code => match code.strip_prefix("no_adapter:") {
                Some(adapter) => steve_i18n::t!("hub.runtime.no_adapter", adapter = adapter),
                None => code.to_string(),
            },
        };
        r["note"] = json!(note);
        if r["id"] == "codex" && r["path"] == "" {
            r["path"] = json!(steve_i18n::t!("hub.runtime.path_missing"));
        }
    }
    list
}

/// `<bin> --version` when the file exists, `""` otherwise.
async fn version_of(bin: &str) -> String {
    if !bin.is_empty() && Path::new(bin).exists() {
        run(bin, &["--version"]).await
    } else {
        String::new()
    }
}

/// The versions the local Node's probe reports (`claude-code`, `codex`), `""` when it has none.
async fn node_versions(node: &dyn steve_node_api::Node, fresh: bool) -> (String, String) {
    let caps = match node.probe(fresh).await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!("probe of the local Node: {e}");
            return Default::default();
        }
    };
    let of = |name: &str| {
        caps.harnesses
            .iter()
            .find(|h| h.name == name)
            .map(|h| h.version.clone())
            .unwrap_or_default()
    };
    (of("claude-code"), of("codex"))
}

/// The harness list. `node`: the local Node runs the binaries (`fresh`: it probes again).
async fn probe(cfg: &Config, node: Option<&dyn steve_node_api::Node>, fresh: bool) -> Value {
    let exists = |p: &str| Path::new(p).exists();
    let codex_bin = ["/opt/homebrew/bin/codex", "/usr/local/bin/codex"]
        .into_iter()
        .find(|p| exists(p))
        .unwrap_or("");
    // two CLI start-ups, side by side
    let (claude_v, codex_v) = match node {
        Some(n) => node_versions(n, fresh).await,
        None => tokio::join!(version_of(&cfg.claude_executable), version_of(codex_bin)),
    };
    let claude_adapter = cfg
        .adapters
        .get("claude-code")
        .map(|p| exists(p))
        .unwrap_or(false);
    let codex_adapter = cfg
        .adapters
        .get("codex")
        .map(|p| exists(p))
        .unwrap_or(false);
    let codex_version = codex_v
        .strip_prefix("codex-cli")
        .map(|s| s.trim_start().to_string())
        .unwrap_or_else(|| codex_v.clone());
    json!([
        {
            "id": "claude-code", "name": "Claude Code", "path": cfg.claude_executable,
            "version": claude_v.split(' ').next().unwrap_or(""),
            "models": steve_core::models::CLAUDE_MODELS,
            "ok": !claude_v.is_empty() && claude_adapter,
            // a code: [`shown`] puts it in the request's language
            "note": if claude_v.is_empty() { "no_claude" } else if !claude_adapter { "no_adapter:@agentclientprotocol/claude-agent-acp" } else { "acp" },
        },
        {
            "id": "codex", "name": "Codex", "path": codex_bin,
            "version": codex_version,
            // ids codex-acp offers in its "model" config option (from the user's model catalog); the first is Codex's default
            "models": steve_core::models::CODEX_MODELS,
            "ok": !codex_v.is_empty() && codex_adapter,
            "note": if codex_v.is_empty() { "no_codex" } else if !codex_adapter { "no_adapter:@zed-industries/codex-acp" } else { "acp" },
        }
    ])
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
        assert_eq!(list[0]["id"], json!("claude-code"));
        assert_eq!(list[0]["ok"], json!(false));
    }
}
