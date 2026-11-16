//! Bounded, attempt-local reads of a declared rollout meter. Totals in this file belong to a
//! session, not an attempt. Prime before sending the prompt, then difference each new tick. A
//! missing baseline or a reset needs `last_token_usage`; missing fields stay unknown.

use std::fs;
use std::io::{Read, Seek, SeekFrom};
use std::path::{Path, PathBuf};

use serde_json::Value;
use steve_node_api::Tokens;

const WALK_ENTRIES: usize = 16_384;
const PRIME_BYTES: u64 = 4 << 20;
const POLL_BYTES: u64 = 512 << 10;
const LINE_BYTES: usize = 256 << 10;

/// Exact filename matching, with a bounded directory walk and no process-global session cache:
/// two principals can use the same session id in different configured homes.
pub fn find(home: &Path, sid: &str) -> Option<PathBuf> {
    if sid.is_empty()
        || sid.len() > 200
        || !sid
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        return None;
    }
    fn walk(dir: &Path, depth: usize, suffix: &str, budget: &mut usize) -> Option<PathBuf> {
        if *budget == 0 {
            return None;
        }
        let mut paths = Vec::new();
        for entry in fs::read_dir(dir).ok()? {
            if *budget == 0 {
                break;
            }
            *budget -= 1;
            let Ok(entry) = entry else { continue };
            let Ok(kind) = entry.file_type() else {
                continue;
            };
            if (depth > 0 && kind.is_dir()) || (depth == 0 && kind.is_file()) {
                paths.push(entry.path());
            }
        }
        paths.sort_unstable();
        for path in paths.into_iter().rev() {
            if depth > 0 {
                if let Some(found) = walk(&path, depth - 1, suffix, budget) {
                    return Some(found);
                }
            } else if path
                .file_name()
                .and_then(|n| n.to_str())
                .is_some_and(|n| n.starts_with("rollout-") && n.ends_with(suffix))
            {
                return Some(path);
            }
        }
        None
    }
    let mut budget = WALK_ENTRIES;
    walk(
        &home.join("sessions"),
        3,
        &format!("-{sid}.jsonl"),
        &mut budget,
    )
}

// input (including cache reads and creation), cached input, output (including reasoning),
// reasoning, cache creation. Older sources omit creation; their input only includes reads.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Counts([Option<u64>; 5]);

impl Counts {
    fn of(v: &Value) -> Option<Self> {
        let fields = [
            "input_tokens",
            "cached_input_tokens",
            "output_tokens",
            "reasoning_output_tokens",
            "cache_write_input_tokens",
        ];
        let c = Self(fields.map(|k| v.get(k).and_then(Value::as_u64)));
        c.0.iter().any(Option::is_some).then_some(c)
    }

    fn delta(self, before: Option<Self>, last: Option<Self>) -> Self {
        let reset = before.is_some_and(|p| {
            (0..5).any(|i| matches!((self.0[i], p.0[i]), (Some(a), Some(b)) if a < b))
        });
        Self(std::array::from_fn(|i| {
            if !reset && let (Some(a), Some(b)) = (self.0[i], before.and_then(|p| p.0[i])) {
                return a.checked_sub(b);
            }
            last.and_then(|p| p.0[i])
        }))
    }

    fn plus(self, other: Self) -> Self {
        Self(std::array::from_fn(|i| self.0[i]?.checked_add(other.0[i]?)))
    }

    fn tokens(self) -> Tokens {
        Tokens {
            input: self.0[0].and_then(|i| {
                i.checked_sub(self.0[1]?)?
                    .checked_sub(self.0[4].unwrap_or(0))
            }),
            cache_read: self.0[1],
            output: self.0[2],
            reasoning: self.0[3].filter(|r| self.0[2].is_none_or(|o| *r <= o)),
            cache_write: self.0[4],
        }
    }
}

/// Milliseconds of Codex's UTC RFC3339 timestamp.
pub(crate) fn parse_ms(s: &str) -> Option<i64> {
    let s = s.strip_suffix('Z')?;
    let (date, time) = s.split_once('T')?;
    let mut d = date.splitn(3, '-').map(|x| x.parse::<i64>().ok());
    let (y, m, day) = (d.next()??, d.next()??, d.next()??);
    let (hms, frac) = time.split_once('.').unwrap_or((time, "0"));
    let mut t = hms.splitn(3, ':').map(|x| x.parse::<i64>().ok());
    let (hh, mm, ss) = (t.next()??, t.next()??, t.next()??);
    if !(1..=12).contains(&m) || !(1..=31).contains(&day) || hh > 23 || mm > 59 || ss > 60 {
        return None;
    }
    let digits: String = frac.chars().take(3).collect();
    let ms = format!("{digits:0<3}").parse::<i64>().ok()?;
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(((days * 24 + hh) * 60 + mm) * 60_000 + ss * 1000 + ms)
}

pub(crate) struct Reader {
    home: PathBuf,
    sid: String,
    path: Option<PathBuf>,
    offset: u64,
    identity: Option<(u64, u64)>,
    pending: Vec<u8>,
    discard: bool,
    previous: Option<Counts>,
    source_reset: bool,
    total: Option<Counts>,
    started: i64,
    newest: i64,
    replay_through: Option<i64>,
    turn: Option<String>,
    sealed: bool,
    invalid: bool,
    pub incomplete: bool,
    pub caught_up: bool,
}

impl Reader {
    pub fn new(home: PathBuf, sid: String, started: i64) -> Self {
        Self {
            home,
            sid,
            path: None,
            offset: 0,
            identity: None,
            pending: vec![],
            discard: false,
            previous: None,
            source_reset: false,
            total: None,
            started,
            newest: started,
            replay_through: None,
            turn: None,
            sealed: false,
            invalid: false,
            incomplete: false,
            caught_up: true,
        }
    }

    pub fn tokens(&self) -> Tokens {
        if self.invalid || self.incomplete {
            return Tokens::default();
        }
        self.total.map(Counts::tokens).unwrap_or_default()
    }

    /// Prime from a bounded tail. No old tokens are booked. Even a very large history costs at
    /// most PRIME_BYTES once; the running read thereafter costs at most POLL_BYTES per poll.
    pub fn prime(&mut self) {
        self.read(true);
    }
    pub fn poll(&mut self) {
        self.read(false);
    }
    pub fn unsettled(&self) -> bool {
        self.pending.windows(13).any(|w| w == b"\"token_count\"")
    }

    fn read(&mut self, prime: bool) {
        if self.invalid {
            return;
        }
        if self.path.as_ref().is_none_or(|p| !p.is_file()) {
            // A source lost after a live reading cannot prove a complete final settlement.
            self.incomplete |= self.path.is_some();
            self.path = find(&self.home, &self.sid);
        }
        let Some(path) = self.path.as_ref() else {
            return;
        };
        let Ok(mut file) = fs::File::open(path) else {
            self.incomplete = true;
            return;
        };
        let Ok(meta) = file.metadata() else { return };
        #[cfg(unix)]
        let identity = {
            use std::os::unix::fs::MetadataExt;
            (meta.dev(), meta.ino())
        };
        #[cfg(not(unix))]
        let identity = (0, 0);
        let replaced = self.identity.is_some_and(|old| old != identity) || meta.len() < self.offset;
        if replaced {
            self.offset = 0;
            self.pending.clear();
            self.discard = false;
            self.source_reset = true;
            if self.total.is_some() {
                self.replay_through = Some(self.newest);
            }
            // Keep the attempt's accumulated consumption and timestamp watermark: old rows
            // replayed into the replacement are not another model response.
        }
        self.identity = Some(identity);
        if prime {
            self.offset = meta.len().saturating_sub(PRIME_BYTES);
            self.discard = self.offset > 0;
        }
        if file.seek(SeekFrom::Start(self.offset)).is_err() {
            self.incomplete = true;
            return;
        }
        let mut bytes = Vec::new();
        let cap = if prime { PRIME_BYTES } else { POLL_BYTES };
        if file.take(cap).read_to_end(&mut bytes).is_err() {
            self.incomplete = true;
            return;
        }
        self.offset += bytes.len() as u64;
        self.caught_up = self.offset >= meta.len();
        for byte in bytes {
            if byte == b'\n' {
                if !self.discard {
                    let line = std::mem::take(&mut self.pending);
                    self.row(&line, prime);
                }
                self.pending.clear();
                self.discard = false;
            } else if !self.discard {
                if self.pending.len() < LINE_BYTES {
                    self.pending.push(byte);
                } else {
                    if self.pending.windows(13).any(|w| w == b"\"token_count\"") {
                        self.incomplete = true;
                    }
                    self.pending.clear();
                    self.discard = true;
                }
            }
        }
    }

    fn row(&mut self, line: &[u8], prime: bool) {
        // Most rows are large prompt/tool bodies. Never parse or retain them as usage evidence.
        if !line.windows(13).any(|w| w == b"\"token_count\"")
            && !line.windows(14).any(|w| w == b"\"session_meta\"")
            && !line.windows(14).any(|w| w == b"\"task_started\"")
            && !line.windows(15).any(|w| w == b"\"task_complete\"")
        {
            return;
        }
        let Ok(v) = serde_json::from_slice::<Value>(line) else {
            // A malformed token row can hide spend, unlike an unrelated malformed tool row.
            if !prime && line.windows(13).any(|w| w == b"\"token_count\"") {
                self.incomplete = true;
            }
            return;
        };
        let p = &v["payload"];
        if v["type"] == "session_meta" {
            if p["id"].as_str().is_some_and(|id| id != self.sid) {
                self.invalid = true;
            }
            return;
        }
        if v["type"] != "event_msg" {
            return;
        }
        let Some(ms) = v["timestamp"].as_str().and_then(parse_ms) else {
            return;
        };
        if !prime
            && (self.sealed
                || ms < self.started
                || ms < self.newest
                || self.replay_through.is_some_and(|t| ms <= t))
        {
            return;
        }
        if !prime && p["type"] == "task_started" {
            if let Some(id) = p["turn_id"].as_str() {
                if self.turn.as_deref().is_some_and(|old| old != id) {
                    self.sealed = true;
                } else {
                    self.turn = Some(id.to_owned());
                }
            }
        }
        if !prime && p["type"] == "task_complete" {
            self.sealed = self.turn.as_deref().is_some_and(|id| p["turn_id"] == id);
        }
        if p["type"] != "token_count" {
            return;
        }
        let info = &p["info"];
        let Some(total) = Counts::of(&info["total_token_usage"]) else {
            return;
        };
        let last = Counts::of(&info["last_token_usage"]);
        if !prime {
            // Repeated context/rate-limit notifications carry the same total. Only an explicit
            // all-zero response can establish real zero in an otherwise empty attempt.
            if self.previous == Some(total)
                && !(self.total.is_none() && last.is_some_and(|c| c.0[..4] == [Some(0); 4]))
            {
                return;
            }
            let delta = total.delta(
                if self.source_reset {
                    None
                } else {
                    self.previous
                },
                last,
            );
            self.source_reset = false;
            self.total = Some(self.total.map_or(delta, |c| c.plus(delta)));
        }
        self.previous = Some(total);
        self.newest = self.newest.max(ms);
    }
}

#[cfg(test)]
mod tests;
