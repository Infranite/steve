//! Discovery on this machine: the harnesses' own history (Claude Code's `~/.claude/projects`,
//! Codex's `~/.codex/sessions`), read-only, and what the harness binaries say about themselves.
//!
//! - [`candidates`] lists the importable transcripts, newest first, Steve's own runs skipped.
//! - [`listed`] is the import check: a key names one of those files, never just any path.
//! - [`read`] reads and converts one transcript into ACP-shaped rows (the format the Hub's old
//!   import stored and still reads); [`events`] turns those rows into Steve's events with the
//!   same normalizer a Node's driver uses, so the Hub stores the very same transcript entries.
//! - [`find_transcript`] locates a harness session's transcript by its id (resuming it).
//! - [`probe`] asks the harness binaries for their version and models.
//!
//! The listing, the conversion and the resume lookup are the Hub's old ones, moved here: the Hub
//! still calls them directly while the local Node is off.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, UNIX_EPOCH};

use serde::Serialize;
use serde_json::{Value, json};
use steve_acp::normalize::{Fact, Normalizer};
use steve_node_api::*;

/// Where the harnesses keep their sessions on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Homes {
    /// The user's home: the history listed for import is under `~/.claude/projects` and
    /// `~/.codex/sessions`.
    pub user_home: PathBuf,
    /// `STEVE_HOME`: transcripts of runs inside it are Steve's own and never listed.
    pub steve_home: PathBuf,
    /// `$CLAUDE_CONFIG_DIR`, default `~/.claude` (resuming a session).
    pub claude_dir: PathBuf,
    /// `$CODEX_HOME`, default `~/.codex`.
    pub codex_home: PathBuf,
}

/// The harness binaries [`probe`] asks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Binaries {
    pub claude: PathBuf,
    pub codex: PathBuf,
}

/// What a [`crate::LocalNode`] discovers with ([`crate::LocalNode::set_discovery`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiscoveryConfig {
    pub homes: Homes,
    pub binaries: Binaries,
}

pub const CLAUDE_CODE: &str = "claude-code";
pub const CODEX: &str = "codex";

// ── the listing ──

/// One importable local session.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LocalSession {
    pub key: String,
    /// `Claude Code`, `Codex`.
    pub harness: String,
    pub title: String,
    pub cwd: String,
    /// `mtimeMs`, a float in Node.
    #[serde(serialize_with = "js_float")]
    pub when: f64,
    pub size: u64,
}

impl LocalSession {
    /// The harness's id (`claude-code`, `codex`).
    pub fn harness_id(&self) -> &'static str {
        if self.key.starts_with("cc:") {
            CLAUDE_CODE
        } else {
            CODEX
        }
    }

    /// Changes whenever the transcript does.
    pub fn version(&self) -> Revision {
        Revision::new(format!("m{}-s{}", self.when as i64, self.size)).expect("a valid revision")
    }

    pub fn entry(&self) -> HistoryEntry {
        HistoryEntry {
            source: HistoryRef {
                harness: self.harness_id().into(),
                key: self.key.clone(),
            },
            title: self.title.clone(),
            directory: (!self.cwd.is_empty()).then(|| self.cwd.clone()),
            updated: Timestamp(self.when as i64),
            version: self.version(),
            size: Some(self.size),
        }
    }
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
                Some(t) => js_string(t),
            })
            .collect(),
        _ => String::new(),
    }
}

pub fn peek_claude(steve_home: &Path, file: &Path) -> Option<LocalSession> {
    let (mut title, mut cwd) = (String::new(), String::new());
    for l in first_lines(file, 200_000).ok()? {
        if l.is_empty() {
            continue;
        }
        let Ok(m) = serde_json::from_str::<Value>(&l) else {
            continue;
        };
        if cwd.is_empty() && truthy(m.get("cwd")) {
            cwd = js_string(&m["cwd"]);
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
        title: slice(&title, 80),
        cwd,
        when: mtime_ms(file)?,
        size: std::fs::metadata(file).ok()?.len(),
    })
}

pub fn peek_codex(steve_home: &Path, file: &Path) -> Option<LocalSession> {
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
                Some(c) => js_string(c),
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
        title: slice(&title, 80),
        cwd,
        when: mtime_ms(file)?,
        size: std::fs::metadata(file).ok()?.len(),
    })
}

/// `localSessions(limit)` without the `imported` flag: newest first, Steve's own runs skipped. Blocking.
pub fn candidates(h: &Homes, limit: usize) -> Vec<LocalSession> {
    let mut files: Vec<(bool, PathBuf, f64)> = claude_files(&h.user_home)
        .into_iter()
        .map(|f| (true, f))
        .chain(codex_files(&h.user_home).into_iter().map(|f| (false, f)))
        .filter_map(|(cc, f)| mtime_ms(&f).map(|t| (cc, f, t)))
        .collect();
    files.sort_by(|a, b| b.2.partial_cmp(&a.2).unwrap_or(std::cmp::Ordering::Equal));
    files
        .into_iter()
        .take(limit * 2)
        .filter_map(|(cc, f, _)| {
            if cc {
                peek_claude(&h.steve_home, &f)
            } else {
                peek_codex(&h.steve_home, &f)
            }
        })
        .take(limit)
        .collect()
}

/// Whether `key` is one [`candidates`] lists: it names one of the transcripts this machine's
/// harnesses keep, compared with the listing's own keys, and that transcript is not Steve's own
/// run. A key a client made up (any other path, a `..` in it, the other harness's prefix) is not.
/// Blocking.
pub fn listed(h: &Homes, key: &str) -> bool {
    let (files, cc) = match key.split_once(':') {
        Some(("cc", _)) => (claude_files(&h.user_home), true),
        Some(("cx", _)) => (codex_files(&h.user_home), false),
        _ => return false,
    };
    let prefix = &key[..3];
    let Some(f) = files
        .into_iter()
        .find(|f| key.len() > 3 && format!("{prefix}{}", f.display()) == key)
    else {
        return false;
    };
    if cc {
        peek_claude(&h.steve_home, &f).is_some()
    } else {
        peek_codex(&h.steve_home, &f).is_some()
    }
}

/// One transcript read for import.
#[derive(Debug, Clone, PartialEq)]
pub enum Read {
    Missing,
    Unreadable,
    Ready {
        peek: LocalSession,
        /// `claude-code`, `codex`.
        harness: String,
        /// ACP-shaped rows: `user` `{text, via}`, and `agent_message_chunk`/`tool_call` updates.
        rows: Vec<(String, Value)>,
    },
}

/// Reads and converts the transcript a key names (blocking). It does not check the key against the
/// listing: [`listed`] does.
pub fn read(steve_home: &Path, key: &str) -> Read {
    let (kind, file) = match key.split_once(':') {
        Some((k, f)) => (k, f),
        None => (key, ""),
    };
    let path = Path::new(file);
    if file.is_empty() || !path.exists() {
        return Read::Missing;
    }
    let cc = kind == "cc";
    let peek = if cc {
        peek_claude(steve_home, path)
    } else {
        peek_codex(steve_home, path)
    };
    let Some(peek) = peek else {
        return Read::Unreadable;
    };
    let Ok(text) = std::fs::read(path).map(|b| String::from_utf8_lossy(&b).into_owned()) else {
        return Read::Unreadable;
    };
    let rows = if cc {
        claude_rows(&text)
    } else {
        codex_rows(&text)
    };
    Read::Ready {
        peek,
        harness: if cc { CLAUDE_CODE } else { CODEX }.into(),
        rows,
    }
}

fn agent_chunk(text: &str) -> (String, Value) {
    (
        "agent_message_chunk".into(),
        json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": text } }),
    )
}

/// A Claude Code transcript as ACP-shaped rows.
pub fn claude_rows(text: &str) -> Vec<(String, Value)> {
    let read = |n: &str| ["Read", "Grep", "Glob"].iter().any(|x| n.contains(x));
    let edit = |n: &str| ["Edit", "Write"].iter().any(|x| n.contains(x));
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
                if cty == Some("text") && truthy(c.get("text")) {
                    out.push(agent_chunk(&js_string(&c["text"])));
                }
                if cty == Some("tool_use") {
                    let name = c
                        .get("name")
                        .map(js_string)
                        .unwrap_or_else(|| "undefined".into());
                    let input = c.get("input");
                    let extra = if truthy(input.and_then(|i| i.get("command"))) {
                        format!(
                            " {}",
                            slice(
                                &js_string(
                                    &input.map(|i| i["command"].clone()).unwrap_or_default()
                                ),
                                120
                            )
                        )
                    } else if truthy(input.and_then(|i| i.get("file_path"))) {
                        format!(
                            " {}",
                            js_string(&input.map(|i| i["file_path"].clone()).unwrap_or_default())
                        )
                    } else {
                        String::new()
                    };
                    let kind = if read(&name) {
                        "read"
                    } else if edit(&name) {
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

/// A Codex rollout as ACP-shaped rows.
pub fn codex_rows(text: &str) -> Vec<(String, Value)> {
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
                None | Some(Value::Null) => json!(tool_uid()),
                Some(v) => v.clone(),
            };
            let arg = [p.get("input"), p.get("arguments")]
                .into_iter()
                .flatten()
                .find(|v| !v.is_null())
                .map(js_string)
                .unwrap_or_default();
            let name = p
                .get("name")
                .map(js_string)
                .unwrap_or_else(|| "undefined".into());
            out.push((
                "tool_call".into(),
                json!({ "sessionUpdate": "tool_call", "toolCallId": id, "title": format!("{name} {}", slice(&arg, 120)), "kind": "execute", "status": "completed" }),
            ));
        }
    }
    out
}

/// `t_` + 16 hex characters of a random UUID (a tool call with no id of its own).
fn tool_uid() -> String {
    let u = uuid::Uuid::new_v4().simple().to_string();
    format!("t_{}", &u[..16])
}

// ── as Steve's events ──

/// The prefix and meter of the normalizer the Hub's old rows are read with, so message ids and
/// tool views come out the same.
const PREFIX: &str = "legacy";

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

/// Claude Code's project directory name for a working directory.
pub fn claude_project(cwd: &str) -> String {
    cwd.chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect()
}

fn claude_transcript(dir: &Path, id: &str, cwd: &str) -> Option<PathBuf> {
    let projects = dir.join("projects");
    let name = format!("{id}.jsonl");
    let direct = projects.join(claude_project(cwd)).join(&name);
    if direct.is_file() {
        return Some(direct);
    }
    // a long path gets a shortened directory name: look in every project
    std::fs::read_dir(&projects)
        .ok()?
        .filter_map(|e| e.ok())
        .map(|e| e.path().join(&name))
        .find(|p| p.is_file())
}

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
    match harness {
        CLAUDE_CODE => claude_transcript(&h.claude_dir, id, cwd),
        CODEX => steve_acp::codex_rollout(&h.codex_home, id),
        _ => None,
    }
}

/// The `sessionId`/`id` and `cwd` the transcript itself records, from its first lines.
pub fn recorded(file: &Path, harness: &str) -> (Option<String>, Option<String>) {
    use std::io::{BufRead, BufReader, Read};
    let Ok(f) = std::fs::File::open(file) else {
        return (None, None);
    };
    let (mut id, mut cwd) = (None, None);
    let field = |v: &Value, k: &str| v.get(k).and_then(Value::as_str).map(str::to_owned);
    // the first 8 MB (a pasted screenshot makes a line of a megabyte or more); a line cut there does not parse
    for line in BufReader::new(f.take(8 << 20)).lines().take(50) {
        let Ok(line) = line else { break };
        let Ok(v) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if harness == CODEX {
            // the first line is `session_meta`
            if v.get("type").and_then(Value::as_str) == Some("session_meta")
                && let Some(p) = v.get("payload")
            {
                return (field(p, "id"), field(p, "cwd"));
            }
            continue;
        }
        id = id.or_else(|| field(&v, "sessionId"));
        cwd = cwd.or_else(|| field(&v, "cwd"));
        if id.is_some() && cwd.is_some() {
            break;
        }
    }
    (id, cwd)
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
    let not_listed = || ApiError::new(ErrorCode::Invalid, "not a history this node lists");
    let (file, id) = if let Some(id) = source.key.strip_prefix("session:") {
        // an id, never a path: it is joined into a file name
        if !plain_id(id) {
            return Err(not_listed());
        }
        let file = find_transcript(h, &source.harness, id, "").ok_or_else(not_listed)?;
        (file, id.to_string())
    } else {
        let harness_of_key = match source.key.get(..3) {
            Some("cc:") => CLAUDE_CODE,
            Some("cx:") => CODEX,
            _ => return Err(not_listed()),
        };
        if harness_of_key != source.harness || !listed(h, &source.key) {
            return Err(not_listed());
        }
        let file = PathBuf::from(&source.key[3..]);
        let stem = file.file_stem().and_then(|s| s.to_str()).unwrap_or("");
        let id = match harness_of_key {
            CLAUDE_CODE => stem.to_string(),
            _ => stem
                .get(stem.len().saturating_sub(36)..)
                .unwrap_or("")
                .to_string(),
        };
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
            Some(_) => {
                Err(ApiError::new(ErrorCode::Conflict, "another import under this id").on(op))
            }
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
            return Err(ApiError::new(ErrorCode::Invalid, "no such import").on(op));
        };
        let from = match &page.cursor {
            None => 0,
            Some(c) => c
                .as_str()
                .parse::<usize>()
                .map_err(|_| ApiError::new(ErrorCode::Invalid, "bad cursor"))?,
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
    pub(crate) async fn get(&self, d: &DiscoveryConfig, fresh: bool) -> HashMap<String, Probed> {
        let mut last = self.0.lock().await;
        if !fresh
            && let Some((at, v)) = last.as_ref()
            && at.elapsed() < PROBE_TTL
        {
            return v.clone();
        }
        let v = probe(&d.binaries, &d.homes.codex_home).await;
        *last = Some((Instant::now(), v.clone()));
        v
    }
}

/// `execFileSync(cmd, args, {timeout})` → trimmed stdout, `""` on any failure.
async fn run(cmd: &Path, args: &[&str], env: &[(&str, &Path)], timeout: Duration) -> String {
    let mut c = tokio::process::Command::new(cmd);
    c.env_remove("STEVE_UPGRADE_CONTROL_TOKEN")
        .env_remove("STEVE_SUPERVISOR_PID")
        .env_remove("STEVE_UPGRADE_ID")
        .args(args)
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true);
    for (k, v) in env {
        c.env(k, v);
    }
    match tokio::time::timeout(timeout, c.output()).await {
        Ok(Ok(o)) if o.status.success() => String::from_utf8_lossy(&o.stdout).trim().to_string(),
        _ => String::new(),
    }
}

/// `<bin> --version` when the file exists, `""` otherwise.
async fn version_of(bin: &Path) -> String {
    if !bin.as_os_str().is_empty() && bin.exists() {
        run(bin, &["--version"], &[], Duration::from_secs(8)).await
    } else {
        String::new()
    }
}

/// The ids `codex debug models` lists (`visibility: list`), in Codex's own order.
pub fn codex_models(json_text: &str) -> Vec<String> {
    let Ok(v) = serde_json::from_str::<Value>(json_text) else {
        return vec![];
    };
    let Some(list) = v.get("models").and_then(Value::as_array).or(v.as_array()) else {
        return vec![];
    };
    let mut out: Vec<(i64, String)> = list
        .iter()
        .filter(|m| m.get("visibility").and_then(Value::as_str) == Some("list"))
        .filter_map(|m| {
            let id = m.get("slug").and_then(Value::as_str)?;
            Some((
                m.get("priority").and_then(Value::as_i64).unwrap_or(99),
                id.to_string(),
            ))
        })
        .collect();
    out.sort();
    let mut seen = HashSet::new();
    out.into_iter()
        .map(|(_, id)| id)
        .filter(|id| seen.insert(id.clone()))
        .collect()
}

/// Both harnesses' versions, and Codex's models (`codex debug models` with `CODEX_HOME`, 15 s at
/// most), side by side.
pub async fn probe(b: &Binaries, codex_home: &Path) -> HashMap<String, Probed> {
    let codex_models = async {
        if b.codex.as_os_str().is_empty() || !b.codex.exists() {
            return vec![];
        }
        let out = run(
            &b.codex,
            &["debug", "models"],
            &[("CODEX_HOME", codex_home)],
            Duration::from_secs(15),
        )
        .await;
        codex_models(&out)
    };
    let (claude, codex, models) =
        tokio::join!(version_of(&b.claude), version_of(&b.codex), codex_models);
    HashMap::from([
        (
            CLAUDE_CODE.to_string(),
            Probed {
                version: claude,
                models: vec![],
            },
        ),
        (
            CODEX.to_string(),
            Probed {
                version: codex,
                models,
            },
        ),
    ])
}

// ── JavaScript's ways, as the Hub's old import had them ──

/// `s.slice(0, n)` on UTF-16 units (a cut surrogate pair becomes U+FFFD).
fn slice(s: &str, n: usize) -> String {
    let mut out = String::new();
    let mut used = 0;
    for c in s.chars() {
        let w = c.len_utf16();
        if used + w > n {
            if used < n {
                out.push('\u{FFFD}');
            }
            break;
        }
        out.push(c);
        used += w;
    }
    out
}

fn number_string(n: &serde_json::Number) -> String {
    if let Some(i) = n.as_i64() {
        return i.to_string();
    }
    if let Some(u) = n.as_u64() {
        return u.to_string();
    }
    let f = n.as_f64().unwrap_or(f64::NAN);
    if f.fract() == 0.0 && f.abs() < 1e21 {
        format!("{f:.0}")
    } else if f.abs() >= 1e21 {
        let s = format!("{f:e}");
        match s.split_once('e') {
            Some((m, e)) if !e.starts_with('-') => format!("{m}e+{e}"),
            _ => s,
        }
    } else {
        f.to_string()
    }
}

/// `String(v)`.
fn js_string(v: &Value) -> String {
    match v {
        Value::Null => "null".into(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => number_string(n),
        Value::String(s) => s.clone(),
        Value::Array(a) => a
            .iter()
            .map(|x| {
                if x.is_null() {
                    String::new()
                } else {
                    js_string(x)
                }
            })
            .collect::<Vec<_>>()
            .join(","),
        Value::Object(_) => "[object Object]".into(),
    }
}

fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|f| f != 0.0),
        Some(Value::String(s)) => !s.is_empty(),
        Some(_) => true,
    }
}
