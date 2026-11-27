//! Per-harness local history discovery, transcript parsing, resume lookup, and CLI model
//! probing.
//!
//! All harness-specific transcript formats, directory layouts, and CLI probe flags stay inside
//! `steve-acp::adapter`. `steve-node::history` consumes only [`HistoryAdapter`] and unified
//! [`HistorySession`] values.

use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use serde::Serialize;
use serde_json::{Value, json};
use steve_node_api::{HistoryEntry, HistoryRef, Revision, Timestamp};

use crate::adapter::{claude_code::ClaudeCodeAdapter, codex::CodexAdapter};

/// One importable local harness session discovered on this machine.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HistorySession {
    pub key: String,
    /// Canonical harness id (e.g. `"claude-code"`, `"codex"`).
    #[serde(skip_serializing)]
    pub harness_id: &'static str,
    /// Human-readable harness name (e.g. `"Claude Code"`, `"Codex"`).
    pub harness: String,
    pub title: String,
    pub cwd: String,
    /// `mtimeMs`, a float matching Node's `stat.mtimeMs`.
    #[serde(serialize_with = "js_float")]
    pub when: f64,
    pub size: u64,
}

impl HistorySession {
    /// The canonical harness id (`claude-code`, `codex`, ...).
    pub fn harness_id(&self) -> &'static str {
        self.harness_id
    }

    /// Changes whenever the transcript does.
    pub fn version(&self) -> Revision {
        Revision::new(format!("m{}-s{}", self.when as i64, self.size)).expect("a valid revision")
    }

    pub fn entry(&self) -> HistoryEntry {
        HistoryEntry {
            source: HistoryRef {
                harness: self.harness_id.into(),
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

/// Per-harness local transcript discovery, parsing, resume lookup, and CLI model probing.
pub trait HistoryAdapter: Send + Sync {
    /// Canonical harness id (`"claude-code"`, `"codex"`, ...).
    fn id(&self) -> &'static str;

    /// Human-readable display name (`"Claude Code"`, `"Codex"`, ...).
    fn display_name(&self) -> &'static str;

    /// Short key prefix (`"cc"`, `"cx"`, ...) used in `"<prefix>:<path>"` import keys.
    fn key_prefix(&self) -> &'static str;

    /// Default state directory relative to `user_home` (e.g. `".claude"`, `".codex"`).
    fn default_state_rel(&self) -> &'static str;

    /// List raw transcript files under `user_home`.
    fn list_files(&self, user_home: &Path) -> Vec<PathBuf>;

    /// Peek one transcript file for title, cwd, mtime, and size; `None` if unreadable or Steve's own.
    fn peek(&self, steve_home: &Path, file: &Path) -> Option<HistorySession>;

    /// Convert raw transcript text into ACP-shaped `(kind, payload)` rows.
    fn parse_rows(&self, text: &str) -> Vec<(String, Value)>;

    /// Locate a transcript file by harness session id and optional cwd under `state_dir`.
    fn find_transcript(&self, state_dir: &Path, id: &str, cwd: &str) -> Option<PathBuf>;

    /// Extract `(recorded_id, recorded_cwd)` from the opening lines of a transcript file.
    fn recorded(&self, file: &Path) -> (Option<String>, Option<String>);

    /// Extract the harness session id from a transcript file path.
    fn session_id_from_file(&self, file: &Path) -> String;

    /// Optional CLI arguments and state-directory environment variable used to probe models from
    /// the harness binary (e.g. `(["debug", "models"], "CODEX_HOME")`).
    fn model_probe_cmd(&self) -> Option<(&'static [&'static str], &'static str)> {
        None
    }

    /// Parse stdout from [`Self::model_probe_cmd`] into ordered model ids.
    fn parse_probed_models(&self, _stdout: &str) -> Vec<String> {
        vec![]
    }
}

static HISTORY_ADAPTERS: [&dyn HistoryAdapter; 2] = [&ClaudeCodeAdapter, &CodexAdapter];

/// Every built-in adapter that supports local transcript discovery and probing.
pub fn history_adapters() -> &'static [&'static dyn HistoryAdapter] {
    &HISTORY_ADAPTERS
}

/// Resolve a [`HistoryAdapter`] by canonical harness id.
pub fn history_adapter(id: &str) -> Option<&'static dyn HistoryAdapter> {
    HISTORY_ADAPTERS.iter().copied().find(|a| a.id() == id)
}

/// Resolve a [`HistoryAdapter`] and path from an import key (`"<prefix>:<path>"`).
pub fn history_adapter_for_key(key: &str) -> Option<(&'static dyn HistoryAdapter, &str)> {
    let (prefix, path) = key.split_once(':')?;
    let adapter = HISTORY_ADAPTERS
        .iter()
        .copied()
        .find(|a| a.key_prefix() == prefix)?;
    Some((adapter, path))
}

// ── Shared helpers for adapter implementations ──

fn js_float<S: serde::Serializer>(f: &f64, s: S) -> Result<S::Ok, S::Error> {
    if f.fract() == 0.0 && f.abs() < 9e15 {
        s.serialize_i64(*f as i64)
    } else {
        s.serialize_f64(*f)
    }
}

/// Node's `stat.mtimeMs`: `sec * 1e3 + nsec / 1e6` in f64.
pub fn mtime_ms(p: &Path) -> Option<f64> {
    let m = std::fs::metadata(p).ok()?.modified().ok()?;
    let d = m.duration_since(UNIX_EPOCH).ok()?;
    Some(d.as_secs() as f64 * 1e3 + d.subsec_nanos() as f64 / 1e6)
}

/// `readdirSync` order (libuv sorts by name bytes).
pub(crate) fn read_dir_sorted(d: &Path) -> std::io::Result<Vec<String>> {
    let mut names: Vec<String> = std::fs::read_dir(d)?
        .filter_map(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    Ok(names)
}

pub(crate) fn real_user_text(t: &str) -> bool {
    !t.is_empty()
        && !t.starts_with('<')
        && !t.starts_with("# AGENTS.md")
        && !t.starts_with("Caveat:")
}

/// The first `max` bytes as UTF-8 (a cut character becomes U+FFFD, like `Buffer.toString`), split on `\n`.
pub(crate) fn first_lines(file: &Path, max: usize) -> std::io::Result<Vec<String>> {
    let buf = std::fs::read(file)?;
    let end = buf.len().min(max);
    Ok(String::from_utf8_lossy(&buf[..end])
        .split('\n')
        .map(str::to_string)
        .collect())
}

pub(crate) fn is_steve(steve_home: &Path, cwd: &str) -> bool {
    [
        steve_home.to_string_lossy().as_ref(),
        "/tmp/steve",
        "/private/tmp/steve",
    ]
    .iter()
    .any(|d| cwd.starts_with(d))
}

pub(crate) fn str_field<'a>(v: &'a Value, path: &[&str]) -> Option<&'a Value> {
    path.iter().try_fold(v, |acc, k| acc.get(k))
}

pub(crate) fn agent_chunk(text: &str) -> (String, Value) {
    (
        "agent_message_chunk".into(),
        json!({ "sessionUpdate": "agent_message_chunk", "content": { "type": "text", "text": text } }),
    )
}

/// `t_` + 16 hex characters (a tool call with no id of its own).
pub(crate) fn tool_uid() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static SEQ: AtomicU64 = AtomicU64::new(1);
    let n = SEQ.fetch_add(1, Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_nanos() as u64);
    let mixed = nanos.wrapping_mul(0x9e37_79b9_7f4a_7c15) ^ n.wrapping_mul(0xbf58_476d_1ce4_e5b9);
    format!("t_{mixed:016x}")
}

/// `s.slice(0, n)` on UTF-16 units (a cut surrogate pair becomes U+FFFD).
pub(crate) fn slice(s: &str, n: usize) -> String {
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
pub(crate) fn js_string(v: &Value) -> String {
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

pub(crate) fn truthy(v: Option<&Value>) -> bool {
    match v {
        None | Some(Value::Null) => false,
        Some(Value::Bool(b)) => *b,
        Some(Value::Number(n)) => n.as_f64().is_some_and(|x| x != 0.0 && !x.is_nan()),
        Some(Value::String(s)) => !s.is_empty(),
        Some(Value::Array(_) | Value::Object(_)) => true,
    }
}
