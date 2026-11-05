//! What this machine is and what it can run (system.ts): the Hub registers itself as a device, reports the
//! harnesses it finds, and imports local harness history (Claude Code, Codex) as read-only sessions.
//! External commands run through `tokio::process` with the TS 8 s timeout; file scans run on the blocking pool.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant, UNIX_EPOCH};

use regex::Regex;
use serde::Serialize;
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
}

impl Runtimes {
    pub fn new(cfg: Arc<Config>) -> Self {
        Runtimes {
            cfg,
            cache: Arc::default(),
            refreshing: Arc::default(),
            probing: Mutex::new(()),
        }
    }

    fn cached(&self) -> Option<(Instant, Value)> {
        self.cache.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub async fn get(&self, fresh: bool) -> Value {
        if !fresh && let Some((at, list)) = self.cached() {
            if at.elapsed() >= RUNTIMES_MAX_AGE
                && self
                    .refreshing
                    .compare_exchange(false, true, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
            {
                let (cfg, cache, refreshing) = (
                    self.cfg.clone(),
                    self.cache.clone(),
                    self.refreshing.clone(),
                );
                tokio::spawn(async move {
                    let list = probe(&cfg).await;
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
        let list = probe(&self.cfg).await;
        *self.cache.lock().unwrap_or_else(|e| e.into_inner()) =
            Some((Instant::now(), list.clone()));
        list
    }
}

/// `<bin> --version` when the file exists, `""` otherwise.
async fn version_of(bin: &str) -> String {
    if !bin.is_empty() && Path::new(bin).exists() {
        run(bin, &["--version"]).await
    } else {
        String::new()
    }
}

async fn probe(cfg: &Config) -> Value {
    let exists = |p: &str| Path::new(p).exists();
    let codex_bin = ["/opt/homebrew/bin/codex", "/usr/local/bin/codex"]
        .into_iter()
        .find(|p| exists(p))
        .unwrap_or("");
    // two CLI start-ups, side by side
    let (claude_v, codex_v) =
        tokio::join!(version_of(&cfg.claude_executable), version_of(codex_bin));
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
            "note": if claude_v.is_empty() { "没有找到 claude" } else if !claude_adapter { "缺少 ACP 适配器 @agentclientprotocol/claude-agent-acp" } else { "通过 ACP 适配器运行" },
        },
        {
            "id": "codex", "name": "Codex", "path": if codex_bin.is_empty() { "未找到" } else { codex_bin },
            "version": codex_version,
            // ids codex-acp offers in its "model" config option (from the user's model catalog); the first is Codex's default
            "models": steve_core::models::CODEX_MODELS,
            "ok": !codex_v.is_empty() && codex_adapter,
            "note": if codex_v.is_empty() { "没有找到 codex" } else if !codex_adapter { "缺少 ACP 适配器 @zed-industries/codex-acp" } else { "通过 ACP 适配器运行" },
        }
    ])
}

// ── local harness history ──

/// One importable local session (`imported` is added by the API).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LocalSession {
    pub key: String,
    pub harness: String,
    pub title: String,
    pub cwd: String,
    /// `mtimeMs`, a float in Node.
    #[serde(serialize_with = "js_float")]
    pub when: f64,
    pub size: u64,
}

fn js_float<S: serde::Serializer>(f: &f64, s: S) -> Result<S::Ok, S::Error> {
    if f.fract() == 0.0 && f.abs() < 9e15 {
        s.serialize_i64(*f as i64)
    } else {
        s.serialize_f64(*f)
    }
}

/// Node's `stat.mtimeMs`: `sec * 1e3 + nsec / 1e6` in f64.
fn mtime_ms(p: &Path) -> Option<f64> {
    let m = std::fs::metadata(p).ok()?.modified().ok()?;
    let d = m.duration_since(UNIX_EPOCH).ok()?;
    Some(d.as_secs() as f64 * 1e3 + d.subsec_nanos() as f64 / 1e6)
}

/// `readdirSync` order (libuv sorts by name bytes).
fn read_dir_sorted(d: &Path) -> std::io::Result<Vec<String>> {
    let mut names: Vec<String> = std::fs::read_dir(d)?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    Ok(names)
}

fn real_user_text(t: &str) -> bool {
    !t.is_empty()
        && !t.starts_with('<')
        && !t.starts_with("# AGENTS.md")
        && !t.starts_with("Caveat:")
}

/// The first `max` bytes as UTF-8 (a cut character becomes U+FFFD, like `Buffer.toString`), split on `\n`.
fn first_lines(file: &Path, max: usize) -> std::io::Result<Vec<String>> {
    let buf = std::fs::read(file)?;
    let end = buf.len().min(max);
    Ok(String::from_utf8_lossy(&buf[..end])
        .split('\n')
        .map(str::to_string)
        .collect())
}

fn claude_files(home: &Path) -> Vec<PathBuf> {
    let root = home.join(".claude/projects");
    let Ok(dirs) = read_dir_sorted(&root) else {
        return vec![];
    };
    dirs.into_iter()
        .flat_map(|d| {
            let dir = root.join(&d);
            read_dir_sorted(&dir)
                .map(|fs| {
                    fs.into_iter()
                        .filter(|f| f.ends_with(".jsonl"))
                        .map(|f| dir.join(f))
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default()
        })
        .collect()
}

fn codex_files(home: &Path) -> Vec<PathBuf> {
    fn walk(d: &Path, depth: usize, out: &mut Vec<PathBuf>) {
        if depth > 4 || !d.exists() {
            return;
        }
        for f in read_dir_sorted(d).unwrap_or_default() {
            let p = d.join(&f);
            if f.ends_with(".jsonl") {
                out.push(p);
            } else if depth < 3 && p.is_dir() {
                walk(&p, depth + 1, out);
            }
        }
    }
    let mut out = vec![];
    walk(&home.join(".codex/sessions"), 0, &mut out);
    out
}

fn is_steve(steve_home: &Path, cwd: &str) -> bool {
    [
        steve_home.to_string_lossy().as_ref(),
        "/tmp/steve",
        "/private/tmp/steve",
    ]
    .iter()
    .any(|d| cwd.starts_with(d))
}

fn str_field<'a>(v: &'a Value, path: &[&str]) -> Option<&'a Value> {
    path.iter().try_fold(v, |acc, k| acc.get(k))
}

/// `content?.map((c) => c.text ?? "").join("") ?? ""`.
fn codex_text(p: &Value) -> String {
    match p.get("content") {
        Some(Value::Array(a)) => a
            .iter()
            .map(|c| match c.get("text") {
                None | Some(Value::Null) => String::new(),
                Some(t) => js::string(t),
            })
            .collect(),
        _ => String::new(),
    }
}

fn peek_claude(steve_home: &Path, file: &Path) -> Option<LocalSession> {
    let (mut title, mut cwd) = (String::new(), String::new());
    for l in first_lines(file, 200_000).ok()? {
        if l.is_empty() {
            continue;
        }
        let Ok(m) = serde_json::from_str::<Value>(&l) else {
            continue;
        };
        if cwd.is_empty() && js::truthy(m.get("cwd")) {
            cwd = js::string(&m["cwd"]);
        }
        if m.get("type").and_then(Value::as_str) == Some("user")
            && let Some(Value::String(c)) = str_field(&m, &["message", "content"])
            && real_user_text(c)
        {
            title = c.clone();
            break;
        }
    }
    if title.is_empty() || is_steve(steve_home, &cwd) {
        return None;
    }
    Some(LocalSession {
        key: format!("cc:{}", file.display()),
        harness: "Claude Code".into(),
        title: js::slice(&title, 80),
        cwd,
        when: mtime_ms(file)?,
        size: std::fs::metadata(file).ok()?.len(),
    })
}

fn peek_codex(steve_home: &Path, file: &Path) -> Option<LocalSession> {
    let (mut title, mut cwd) = (String::new(), String::new());
    for l in first_lines(file, 400_000).ok()? {
        if l.is_empty() {
            continue;
        }
        let Ok(m) = serde_json::from_str::<Value>(&l) else {
            continue;
        };
        let ty = m.get("type").and_then(Value::as_str);
        if ty == Some("session_meta") {
            cwd = match str_field(&m, &["payload", "cwd"]) {
                None | Some(Value::Null) => String::new(),
                Some(c) => js::string(c),
            };
        }
        if ty == Some("response_item")
            && let Some(p) = m.get("payload")
            && p.get("type").and_then(Value::as_str) == Some("message")
            && p.get("role").and_then(Value::as_str) == Some("user")
        {
            let t = codex_text(p);
            if real_user_text(&t) {
                title = t;
                break;
            }
        }
    }
    if title.is_empty() || is_steve(steve_home, &cwd) {
        return None;
    }
    Some(LocalSession {
        key: format!("cx:{}", file.display()),
        harness: "Codex".into(),
        title: js::slice(&title, 80),
        cwd,
        when: mtime_ms(file)?,
        size: std::fs::metadata(file).ok()?.len(),
    })
}

/// `localSessions(limit)` without the `imported` flag: newest first, Steve's own runs skipped. Blocking.
pub fn local_candidates(steve_home: &Path, limit: usize) -> Vec<LocalSession> {
    let home = home_dir();
    let mut files: Vec<(bool, PathBuf, f64)> = claude_files(&home)
        .into_iter()
        .map(|f| (true, f))
        .chain(codex_files(&home).into_iter().map(|f| (false, f)))
        .filter_map(|(cc, f)| mtime_ms(&f).map(|t| (cc, f, t)))
        .collect();
    files.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
    files
        .into_iter()
        .take(limit * 2)
        .filter_map(|(cc, f, _)| {
            if cc {
                peek_claude(steve_home, &f)
            } else {
                peek_codex(steve_home, &f)
            }
        })
        .take(limit)
        .collect()
}

/// The file side of `importLocal`, done before taking the write lock.
#[derive(Debug, Clone, PartialEq)]
pub enum Prepared {
    Missing,
    Unreadable {
        key: String,
    },
    Ready {
        key: String,
        peek: LocalSession,
        harness: String,
        events: Vec<(String, Value)>,
    },
}

/// Reads and converts a local session file (blocking).
pub fn prepare_import(steve_home: &Path, key: &str) -> Prepared {
    let (kind, file) = match key.split_once(':') {
        Some((k, f)) => (k, f),
        None => (key, ""),
    };
    let path = Path::new(file);
    if file.is_empty() || !path.exists() {
        return Prepared::Missing;
    }
    let cc = kind == "cc";
    let peek = if cc {
        peek_claude(steve_home, path)
    } else {
        peek_codex(steve_home, path)
    };
    let Some(peek) = peek else {
        return Prepared::Unreadable { key: key.into() };
    };
    let Ok(text) = std::fs::read(path).map(|b| String::from_utf8_lossy(&b).into_owned()) else {
        return Prepared::Unreadable { key: key.into() };
    };
    let events = if cc {
        claude_events(&text)
    } else {
        codex_events(&text)
    };
    Prepared::Ready {
        key: key.into(),
        peek,
        harness: if cc { "claude-code" } else { "codex" }.into(),
        events,
    }
}

fn agent_chunk(text: &str) -> (String, Value) {
    (
        "agent_message_chunk".into(),
        json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": text } }),
    )
}

fn claude_events(text: &str) -> Vec<(String, Value)> {
    let read = Regex::new("Read|Grep|Glob").ok();
    let edit = Regex::new("Edit|Write").ok();
    let mut out = vec![];
    for l in text.split('\n') {
        if l.is_empty() {
            continue;
        }
        let Ok(m) = serde_json::from_str::<Value>(l) else {
            continue;
        };
        let ty = m.get("type").and_then(Value::as_str);
        if ty == Some("user")
            && let Some(Value::String(c)) = str_field(&m, &["message", "content"])
            && real_user_text(c)
        {
            out.push(("user".into(), json!({ "text": c, "via": "me" })));
        }
        if ty == Some("assistant")
            && let Some(Value::Array(cs)) = str_field(&m, &["message", "content"])
        {
            for c in cs {
                let cty = c.get("type").and_then(Value::as_str);
                if cty == Some("text") && js::truthy(c.get("text")) {
                    out.push(agent_chunk(&js::string(&c["text"])));
                }
                if cty == Some("tool_use") {
                    let name = c
                        .get("name")
                        .map(js::string)
                        .unwrap_or_else(|| "undefined".into());
                    let input = c.get("input");
                    let extra = if js::truthy(input.and_then(|i| i.get("command"))) {
                        format!(
                            " {}",
                            js::slice(
                                &js::string(
                                    &input.map(|i| i["command"].clone()).unwrap_or_default()
                                ),
                                120
                            )
                        )
                    } else if js::truthy(input.and_then(|i| i.get("file_path"))) {
                        format!(
                            " {}",
                            js::string(&input.map(|i| i["file_path"].clone()).unwrap_or_default())
                        )
                    } else {
                        String::new()
                    };
                    let kind = if read.as_ref().is_some_and(|r| r.is_match(&name)) {
                        "read"
                    } else if edit.as_ref().is_some_and(|r| r.is_match(&name)) {
                        "edit"
                    } else {
                        "execute"
                    };
                    let mut call = serde_json::Map::new();
                    call.insert("sessionUpdate".into(), json!("tool_call"));
                    // `toolCallId: undefined` drops the key
                    if let Some(id) = c.get("id") {
                        call.insert("toolCallId".into(), id.clone());
                    }
                    call.insert("title".into(), json!(format!("{name}{extra}")));
                    call.insert("kind".into(), json!(kind));
                    call.insert("status".into(), json!("completed"));
                    out.push(("tool_call".into(), Value::Object(call)));
                }
            }
        }
    }
    out
}

fn codex_events(text: &str) -> Vec<(String, Value)> {
    let mut out = vec![];
    for l in text.split('\n') {
        if l.is_empty() {
            continue;
        }
        let Ok(m) = serde_json::from_str::<Value>(l) else {
            continue;
        };
        if m.get("type").and_then(Value::as_str) != Some("response_item") {
            continue;
        }
        let Some(p) = m.get("payload") else { continue };
        let pty = p.get("type").and_then(Value::as_str);
        if pty == Some("message") {
            let t = codex_text(p);
            let role = p.get("role").and_then(Value::as_str);
            if role == Some("user") && real_user_text(&t) {
                out.push(("user".into(), json!({ "text": t, "via": "me" })));
            }
            if role == Some("assistant") && !t.is_empty() {
                out.push(agent_chunk(&t));
            }
        }
        if pty == Some("custom_tool_call") || pty == Some("function_call") {
            let id = match p.get("call_id") {
                None | Some(Value::Null) => json!(uid("t")),
                Some(v) => v.clone(),
            };
            let arg = [p.get("input"), p.get("arguments")]
                .into_iter()
                .flatten()
                .find(|v| !v.is_null())
                .map(js::string)
                .unwrap_or_default();
            let name = p
                .get("name")
                .map(js::string)
                .unwrap_or_else(|| "undefined".into());
            out.push((
                "tool_call".into(),
                json!({ "sessionUpdate": "tool_call", "toolCallId": id, "title": format!("{name} {}", js::slice(&arg, 120)), "kind": "execute", "status": "completed" }),
            ));
        }
    }
    out
}

/// The database side of `importLocal` (under the write lock): returns the new session id or the TS error.
pub fn import_prepared(r: &dyn Repo, bus: &Bus, ws: &str, p: Prepared) -> BResult<String> {
    let key = match &p {
        Prepared::Missing => return Err(BackendError::msg("本机记录不存在了")),
        Prepared::Unreadable { key } | Prepared::Ready { key, .. } => key.clone(),
    };
    if r.import_keys()?.contains(&key) {
        return Err(BackendError::msg("已经导入过了"));
    }
    let Prepared::Ready {
        peek,
        harness,
        events,
        ..
    } = p
    else {
        return Err(BackendError::msg("读不出这个会话"));
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
    // stored in Steve's own terms, like everything written now
    let mut reader = steve_transcript::LegacyReader::new(&peek.cwd);
    for (kind, data) in &events {
        for x in reader.read(kind, data) {
            r.events_add(&id, &x.kind, &x.data)?;
        }
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
        return Err(BackendError::msg("只能移除导入的会话"));
    }
    // the store publishes `session` after the delete
    r.session_delete(id)
}

#[cfg(test)]
mod tests {
    use super::*;

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
