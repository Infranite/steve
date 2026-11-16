//! Claude's persisted assistant usage is per message, repeated for content blocks. It is
//! neither a cumulative session counter nor the last assistant's usage. Read only the exact
//! session in the configured home; timestamps and the pre-prompt ID baseline fence attempts.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};
use std::sync::{
    Arc,
    atomic::{AtomicI64, Ordering},
};

use serde_json::Value;
use steve_node_api::{Tokens, UsageScope};

use crate::rollout::parse_ms;
use crate::usage::Reading;

const PRIME_BYTES: u64 = 4 << 20;
const POLL_BYTES: u64 = 512 << 10;
const LINE_BYTES: usize = 256 << 10;
const FILES: usize = 128;
const MESSAGES: usize = 65_536;

#[derive(Default)]
struct Cursor {
    offset: u64,
    identity: Option<(u64, u64)>,
    pending: Vec<u8>,
    discard: bool,
}

struct Message {
    model: String,
    main: bool,
    tokens: Tokens,
    source_at: i64,
}

pub(crate) struct Reader {
    home: PathBuf,
    sid: String,
    started: i64,
    pub end_fence: Arc<AtomicI64>,
    paths: BTreeMap<PathBuf, Cursor>,
    baseline: BTreeSet<String>,
    messages: BTreeMap<String, Message>,
    prompt: Option<String>,
    sealed: bool,
    pub incomplete: bool,
    pub caught_up: bool,
}

impl Reader {
    pub fn new(home: PathBuf, sid: String, started: i64) -> Self {
        Self {
            home,
            sid,
            started,
            end_fence: Arc::new(AtomicI64::new(i64::MAX)),
            paths: BTreeMap::new(),
            baseline: BTreeSet::new(),
            messages: BTreeMap::new(),
            prompt: None,
            sealed: false,
            incomplete: false,
            caught_up: true,
        }
    }

    // Exact session names, one root. A duplicate root is ambiguous, never an invitation to
    // merge two projects. Symlinks and arbitrary directories are not followed.
    fn discover(&mut self) {
        if self.sid.is_empty()
            || self.sid.len() > 200
            || !self
                .sid
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        {
            return;
        }
        let Ok(projects) = fs::read_dir(self.home.join("projects")) else {
            return;
        };
        let mut roots = vec![];
        for (i, entry) in projects.enumerate() {
            if i >= 4096 {
                self.incomplete = true;
                break;
            }
            let Ok(entry) = entry else {
                continue;
            };
            if !entry.file_type().is_ok_and(|t| t.is_dir()) {
                continue;
            }
            let path = entry.path().join(format!("{}.jsonl", self.sid));
            if fs::symlink_metadata(&path).is_ok_and(|m| m.is_file()) {
                roots.push(path);
            }
        }
        if roots.len() > 1 {
            self.incomplete = true;
            return;
        }
        let Some(root) = roots.pop() else {
            return;
        };
        if !self.paths.is_empty() && !self.paths.contains_key(&root) {
            self.incomplete = true;
            return;
        }
        let session_dir = root.with_extension("");
        let sub = session_dir.join("subagents");
        self.paths.entry(root).or_default();
        if fs::symlink_metadata(&session_dir).is_ok_and(|m| m.is_dir())
            && fs::symlink_metadata(&sub).is_ok_and(|m| m.is_dir())
            && let Ok(entries) = fs::read_dir(sub)
        {
            for entry in entries.take(FILES + 1).flatten() {
                if self.paths.len() >= FILES {
                    self.incomplete = true;
                    break;
                }
                if entry.file_type().is_ok_and(|t| t.is_file())
                    && entry
                        .file_name()
                        .to_str()
                        .is_some_and(|n| n.starts_with("agent-") && n.ends_with(".jsonl"))
                {
                    self.paths.entry(entry.path()).or_default();
                }
            }
        }
    }

    pub fn prime(&mut self) {
        self.read(true);
    }
    pub fn poll(&mut self) {
        self.read(false);
    }
    pub fn unsettled(&self) -> bool {
        self.paths
            .values()
            .any(|c| !c.pending.is_empty() || c.discard)
    }

    fn read(&mut self, prime: bool) {
        self.discover();
        let mut budget = if prime { PRIME_BYTES } else { POLL_BYTES };
        self.caught_up = true;
        for path in self.paths.keys().cloned().collect::<Vec<_>>() {
            let child = path
                .parent()
                .and_then(Path::file_name)
                .is_some_and(|n| n == "subagents");
            let mut cursor = self.paths.remove(&path).unwrap_or_default();
            if path
                .parent()
                .is_some_and(|p| !fs::symlink_metadata(p).is_ok_and(|m| m.is_dir()))
            {
                self.incomplete = true;
                self.paths.insert(path, cursor);
                continue;
            }
            if budget == 0 {
                self.caught_up = false;
                self.paths.insert(path, cursor);
                continue;
            }
            // O_NONBLOCK also prevents a replaced FIFO from holding a blocking worker forever.
            let mut opts = fs::OpenOptions::new();
            opts.read(true);
            #[cfg(unix)]
            {
                use std::os::unix::fs::OpenOptionsExt;
                opts.custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK);
            }
            let Ok(mut file) = opts.open(&path) else {
                self.incomplete = true;
                self.paths.insert(path, cursor);
                continue;
            };
            let Ok(meta) = file.metadata() else {
                self.incomplete = true;
                self.paths.insert(path, cursor);
                continue;
            };
            if !meta.is_file() {
                self.incomplete = true;
                self.paths.insert(path, cursor);
                continue;
            }
            #[cfg(unix)]
            let identity = {
                use std::os::unix::fs::MetadataExt;
                (meta.dev(), meta.ino())
            };
            #[cfg(not(unix))]
            let identity = (0, 0);
            if cursor.identity.is_some_and(|old| old != identity) || meta.len() < cursor.offset {
                cursor.offset = 0;
                cursor.pending.clear();
                cursor.discard = false;
            }
            cursor.identity = Some(identity);
            if prime {
                cursor.offset = meta.len().saturating_sub(budget);
                cursor.discard = cursor.offset > 0;
            }
            let mut bytes = vec![];
            if file.seek(SeekFrom::Start(cursor.offset)).is_err()
                || file.take(budget).read_to_end(&mut bytes).is_err()
            {
                self.incomplete = true;
                self.paths.insert(path, cursor);
                continue;
            }
            budget -= bytes.len() as u64;
            cursor.offset += bytes.len() as u64;
            self.caught_up &= cursor.offset >= meta.len();
            for byte in bytes {
                if byte == b'\n' {
                    if !cursor.discard {
                        self.row(&cursor.pending, prime, child);
                    }
                    cursor.pending.clear();
                    cursor.discard = false;
                } else if !cursor.discard {
                    if cursor.pending.len() < LINE_BYTES {
                        cursor.pending.push(byte);
                    } else {
                        self.incomplete |= !prime;
                        cursor.pending.clear();
                        cursor.discard = true;
                    }
                }
            }
            self.paths.insert(path, cursor);
        }
    }

    fn row(&mut self, line: &[u8], prime: bool, child: bool) {
        let Ok(v) = serde_json::from_slice::<Value>(line) else {
            self.incomplete |= !prime;
            return;
        };
        let session_matches =
            v["sessionId"] == self.sid || (child && v["parentSessionId"] == self.sid);
        if !session_matches {
            return;
        }
        let Some(ms) = v["timestamp"].as_str().and_then(parse_ms) else {
            return;
        };
        if !prime
            && (ms < self.started || ms > self.end_fence.load(Ordering::Relaxed) || self.sealed)
        {
            return;
        }
        let main = !child && v["isSidechain"] != true;
        // A new user text, unlike tool_result, bounds the next turn. Do not keep its content.
        if !prime && main && v["type"] == "user" && v["isMeta"] != true {
            let content = &v["message"]["content"];
            let prompt = content.is_string()
                || content
                    .as_array()
                    .is_some_and(|a| a.iter().any(|c| c["type"] == "text"));
            if prompt && let Some(id) = v["uuid"].as_str() {
                if self.prompt.as_deref().is_some_and(|old| old != id) {
                    self.sealed = true;
                } else {
                    self.prompt = Some(id.into());
                }
            }
            return;
        }
        if v["type"] != "assistant" {
            return;
        }
        let m = &v["message"];
        let (Some(id), Some(model)) = (
            m["id"].as_str().filter(|s| !s.is_empty()),
            m["model"].as_str().filter(|s| !s.is_empty()),
        ) else {
            return;
        };
        if model == "<synthetic>" {
            return;
        }
        if prime {
            if self.baseline.len() >= MESSAGES {
                self.incomplete = true;
                return;
            }
            self.baseline.insert(id.into());
            return;
        }
        if self.baseline.contains(id) {
            return;
        }
        let u = &m["usage"];
        let num = |k: &str| u[k].as_u64();
        let output = num("output_tokens");
        let tokens = Tokens {
            input: num("input_tokens"),
            cache_read: num("cache_read_input_tokens"),
            cache_write: num("cache_creation_input_tokens"),
            output,
            reasoning: u
                .pointer("/output_tokens_details/thinking_tokens")
                .and_then(Value::as_u64)
                .filter(|r| output.is_none_or(|o| *r <= o)),
        };
        if tokens == Tokens::default() {
            return;
        }
        if let Some(old) = self.messages.get_mut(id) {
            if old.model != model {
                self.incomplete = true;
                return;
            }
            // A response can be mirrored in subagent history. Its message ID is still one
            // response; a copy on the main chain is evidence for Main, not another All spend.
            old.main |= main;
            old.source_at = old.source_at.min(ms);
            // Repeated blocks can refine a streamed message. A smaller or missing field in a
            // replay cannot erase a previously observed response; never sum repeated IDs.
            old.tokens = maximum(old.tokens, tokens);
        } else if self.messages.len() < MESSAGES {
            self.messages.insert(
                id.into(),
                Message {
                    model: model.into(),
                    main,
                    tokens,
                    source_at: ms,
                },
            );
        } else {
            self.incomplete = true;
        }
    }

    pub fn readings(&self) -> Vec<Reading> {
        if self.incomplete {
            return vec![];
        }
        let mut groups: BTreeMap<String, (Option<Tokens>, Option<Tokens>)> = BTreeMap::new();
        for m in self.messages.values() {
            if m.source_at > self.end_fence.load(Ordering::Relaxed) {
                continue;
            }
            let (main, all) = groups.entry(m.model.clone()).or_default();
            *all = Some(all.map_or(m.tokens, |t| plus(t, m.tokens)));
            if m.main {
                *main = Some(main.map_or(m.tokens, |t| plus(t, m.tokens)));
            }
        }
        let mut out = vec![];
        for (model, (main, all)) in groups {
            if let Some(tokens) = main {
                out.push(Reading {
                    model: Some(model.clone()),
                    scope: UsageScope::Main,
                    tokens,
                });
            }
            if let Some(tokens) = all {
                out.push(Reading {
                    model: Some(model),
                    scope: UsageScope::All,
                    tokens,
                });
            }
        }
        out
    }
}

fn maximum(a: Tokens, b: Tokens) -> Tokens {
    let max = |a: Option<u64>, b: Option<u64>| match (a, b) {
        (Some(a), Some(b)) => Some(a.max(b)),
        (a, b) => a.or(b),
    };
    Tokens {
        input: max(a.input, b.input),
        output: max(a.output, b.output),
        cache_read: max(a.cache_read, b.cache_read),
        cache_write: max(a.cache_write, b.cache_write),
        reasoning: max(a.reasoning, b.reasoning),
    }
}
fn plus(a: Tokens, b: Tokens) -> Tokens {
    let add = |a: Option<u64>, b: Option<u64>| a?.checked_add(b?);
    Tokens {
        input: add(a.input, b.input),
        output: add(a.output, b.output),
        cache_read: add(a.cache_read, b.cache_read),
        cache_write: add(a.cache_write, b.cache_write),
        reasoning: add(a.reasoning, b.reasoning),
    }
}

#[cfg(test)]
mod tests;
