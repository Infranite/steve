//! In-process domain events (bus.ts). Store writes and the orchestrator publish here; every SSE stream subscribes.

use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

use bytes::Bytes;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::broadcast;

/// One bus event; serialized it is exactly the SSE `data:` payload of the TS Hub.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum HubEvent {
    #[serde(rename = "item")]
    Item { id: String },
    /// `item_id`: `None` = key absent, `Some(None)` = `null`.
    #[serde(rename = "decision")]
    Decision {
        id: String,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            with = "double_option"
        )]
        item_id: Option<Option<String>>,
    },
    #[serde(rename = "session")]
    Session {
        id: String,
        #[serde(
            default,
            skip_serializing_if = "Option::is_none",
            with = "double_option"
        )]
        item_id: Option<Option<String>>,
    },
    /// A session event; `data` is the stored object as written (not re-read).
    #[serde(rename = "event")]
    Event {
        session_id: String,
        seq: i64,
        kind: String,
        data: serde_json::Value,
    },
    #[serde(rename = "message")]
    Message { id: String },
    /// `phase`: `route` → `answer` | `config` | `item` | `supplement` while one message is handled.
    #[serde(rename = "steve.thinking")]
    Thinking {
        on: bool,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ws: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        phase: Option<String>,
    },
    /// The bot id may be `"*"` (all memories of a workspace cleared).
    #[serde(rename = "bot")]
    Bot { id: String },
    #[serde(rename = "project")]
    Project { id: String },
    #[serde(rename = "tag")]
    Tag { id: String },
    #[serde(rename = "workspace")]
    Workspace { id: String },
    #[serde(rename = "settings")]
    Settings { id: String },
    #[serde(rename = "device")]
    Device { id: String },
}

/// `Option<Option<T>>` that tells "absent" from `null`.
mod double_option {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub fn serialize<S: Serializer, T: Serialize>(
        v: &Option<Option<T>>,
        s: S,
    ) -> Result<S::Ok, S::Error> {
        match v {
            Some(inner) => inner.serialize(s),
            None => s.serialize_none(),
        }
    }

    pub fn deserialize<'de, D: Deserializer<'de>, T: Deserialize<'de>>(
        d: D,
    ) -> Result<Option<Option<T>>, D::Error> {
        Option::<T>::deserialize(d).map(Some)
    }
}

/// Who may see a frame: every subscriber, the subscribers of one workspace, or nobody (a session event of a
/// session that doesn't exist; TS compared `undefined` with the workspace id).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Scope {
    All,
    Ws(String),
    Nobody,
}

/// One event as it goes out on every SSE stream: serialized (and slimmed) once, with its audience.
#[derive(Debug)]
pub struct Frame {
    /// `data: <json>\n\n`
    pub bytes: Bytes,
    pub scope: Scope,
    /// Position in the change log ([`Bus::replay`]), assigned by the dispatcher in send order; `None` for
    /// frames that are not kept (see [`TRANSIENT`]) and frames nobody may see.
    pub id: Option<u64>,
}

impl Frame {
    pub fn visible_to(&self, ws: &str) -> bool {
        match &self.scope {
            Scope::All => true,
            Scope::Ws(w) => w == ws,
            Scope::Nobody => false,
        }
    }
}

/// Events that change nothing `/api/state` reads, so they leave [`Bus::generation`] alone: transcript chunks
/// (stored only in `events`; a turn's end publishes `session`), a chat answer being written and its research
/// tool calls (the stored reply publishes `message`), and statistics (counted by their own generation).
const STATELESS: &[&str] = &["event", "steve.draft", "steve.tool", "stats"];

/// Frames the change log doesn't keep: transcript chunks (the per-session events API is their cursor) and a
/// chat answer being written (each one carries the full text so far).
const TRANSIENT: &[&str] = &["event", "steve.draft"];

/// The change log keeps at most this many frames ...
pub const RING_FRAMES: usize = 16_384;
/// ... and at most this many bytes of them (the oldest go first).
pub const RING_BYTES: usize = 32 << 20;
/// Bounded channel capacity per [`Bus::tap`] subscriber.
pub const TAP_CAPACITY: usize = 1024;
/// Maximum pending/coalesced overflow entries per [`Bus::tap`] subscriber when its channel is full.
pub const TAP_OVERFLOW_MAX: usize = 512;

const TAP_DRAIN_INTERVAL: std::time::Duration = std::time::Duration::from_millis(10);
const TAP_WARN_INTERVAL: std::time::Duration = std::time::Duration::from_secs(5);
const SESSION_CACHE_HALF: usize = 50_000;
const WS_GENERATION_MAX: usize = 10_000;

/// A position in the change log: the Bus's epoch (random per process, so a Hub restart never matches) and
/// the id of the last frame seen. On the wire it is `<epoch>:<n>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cursor {
    pub epoch: String,
    pub n: u64,
}

impl Cursor {
    /// `<epoch>:<n>`; `None` when malformed.
    pub fn parse(s: &str) -> Option<Cursor> {
        let (epoch, n) = s.trim().rsplit_once(':')?;
        if epoch.is_empty() || n.is_empty() || !n.bytes().all(|b| b.is_ascii_digit()) {
            return None;
        }
        Some(Cursor {
            epoch: epoch.to_string(),
            n: n.parse().ok()?,
        })
    }
}

impl std::fmt::Display for Cursor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}:{}", self.epoch, self.n)
    }
}

/// What [`Bus::replay`] found after a cursor.
#[derive(Debug)]
pub enum Replay {
    /// Every kept frame after the cursor, oldest first (unfiltered: the caller applies `visible_to`), up to
    /// `head`.
    Frames { frames: Vec<Arc<Frame>>, head: u64 },
    /// The cursor can't be continued (another epoch, ahead of the log, or older than what is retained): the
    /// client has to refetch everything; `head` is where it continues from.
    Reset { head: u64 },
}

/// The retained tail of the change log. Ids are contiguous: only kept frames take one.
#[derive(Debug)]
struct Ring {
    frames: VecDeque<Arc<Frame>>,
    bytes: usize,
    /// the highest id evicted so far (0: none)
    dropped: u64,
    max_frames: usize,
    max_bytes: usize,
}

impl Ring {
    fn new(max_frames: usize, max_bytes: usize) -> Self {
        Ring {
            frames: VecDeque::new(),
            bytes: 0,
            dropped: 0,
            max_frames: max_frames.max(1),
            max_bytes,
        }
    }

    fn push(&mut self, f: Arc<Frame>) {
        self.bytes += f.bytes.len();
        self.frames.push_back(f);
        // the newest frame always stays, however large
        while self.frames.len() > 1
            && (self.frames.len() > self.max_frames || self.bytes > self.max_bytes)
        {
            if let Some(old) = self.frames.pop_front() {
                self.bytes -= old.bytes.len();
                self.dropped = old.id.unwrap_or(self.dropped);
            }
        }
    }
}

/// The change log shared by the Bus and its dispatcher.
struct Log {
    epoch: String,
    ring: Mutex<Ring>,
    /// the id of the last kept frame, stored after it is in the ring and before it is broadcast
    head: AtomicU64,
}

type Resolver = Box<dyn Fn(&str) -> Option<String> + Send + Sync>;
type Taps = Arc<Mutex<Vec<TapSlot>>>;

struct TapSlot {
    types: Vec<String>,
    tx: tokio::sync::mpsc::Sender<Value>,
    overflow: VecDeque<(Option<u64>, Value)>,
    evicted_since_warn: u64,
    last_warn: Option<std::time::Instant>,
}

impl TapSlot {
    fn new(types: Vec<String>, tx: tokio::sync::mpsc::Sender<Value>) -> Self {
        Self {
            types,
            tx,
            overflow: VecDeque::new(),
            evicted_since_warn: 0,
            last_warn: None,
        }
    }

    fn matches(&self, ty: &str) -> bool {
        self.types.iter().any(|t| t == ty)
    }

    /// Flushes queued overflow items into `tx` in FIFO order without blocking.
    /// Returns `false` once the receiver has closed so the caller drops this tap.
    fn flush(&mut self) -> bool {
        if self.tx.is_closed() {
            return false;
        }
        while let Some((h, v)) = self.overflow.pop_front() {
            match self.tx.try_send(v) {
                Ok(()) => {}
                Err(tokio::sync::mpsc::error::TrySendError::Full(v)) => {
                    self.overflow.push_front((h, v));
                    break;
                }
                Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => {
                    return false;
                }
            }
        }
        true
    }

    /// Delivers `v` without blocking. Returns `false` if the receiver disconnected.
    fn deliver(&mut self, v: Value, now: std::time::Instant) -> bool {
        if !self.flush() {
            return false;
        }
        if !self.overflow.is_empty() {
            self.push_overflow(v, now);
            return true;
        }
        match self.tx.try_send(v) {
            Ok(()) => true,
            Err(tokio::sync::mpsc::error::TrySendError::Full(v)) => {
                self.push_overflow(v, now);
                true
            }
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => false,
        }
    }

    fn push_overflow(&mut self, v: Value, now: std::time::Instant) {
        let (h, dup_pos) = match tap_key_fields(&v) {
            Some(k) => {
                let h = hash_tap_key(k);
                let pos = self
                    .overflow
                    .iter()
                    .position(|(eh, ev)| *eh == Some(h) && tap_key_fields(ev) == Some(k));
                (Some(h), pos)
            }
            None => (None, None),
        };
        if let Some(pos) = dup_pos {
            self.overflow.remove(pos);
            self.overflow.push_back((h, v));
            return;
        }
        while self.overflow.len() >= TAP_OVERFLOW_MAX {
            self.overflow.pop_front();
            self.evicted_since_warn += 1;
            if self
                .last_warn
                .is_none_or(|t| now.duration_since(t) >= TAP_WARN_INTERVAL)
            {
                tracing::warn!(
                    types = ?self.types,
                    evicted = self.evicted_since_warn,
                    overflow_max = TAP_OVERFLOW_MAX,
                    "bus tap consumer lagging; evicted oldest overflow event(s)"
                );
                self.evicted_since_warn = 0;
                self.last_warn = Some(now);
            }
        }
        self.overflow.push_back((h, v));
    }
}

fn tap_key_fields(v: &Value) -> Option<(&str, &str, &str)> {
    let ty = v.get("type").and_then(Value::as_str)?;
    if ty == "event" {
        return None;
    }
    let ws = v.get("ws").and_then(Value::as_str).unwrap_or_default();
    let id = v.get("id").and_then(Value::as_str).unwrap_or_default();
    Some((ty, ws, id))
}

fn hash_tap_key(k: (&str, &str, &str)) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    k.hash(&mut h);
    h.finish()
}

/// Two-generation session -> workspace cache (at most `2 * half_cap` entries). Rotating `hot` into
/// `cold` evicts at most half the cache at once while promoting active `cold` sessions back into `hot`.
struct SessionWsCache {
    hot: HashMap<String, String>,
    cold: HashMap<String, String>,
    half_cap: usize,
}

impl SessionWsCache {
    fn new(half_cap: usize) -> Self {
        Self {
            hot: HashMap::new(),
            cold: HashMap::new(),
            half_cap: half_cap.max(1),
        }
    }

    fn get(&mut self, sid: &str) -> Option<String> {
        if let Some(ws) = self.hot.get(sid) {
            return Some(ws.clone());
        }
        if let Some((sid, ws)) = self.cold.remove_entry(sid) {
            self.insert(sid, ws.clone());
            return Some(ws);
        }
        None
    }

    fn insert(&mut self, sid: String, ws: String) {
        if !self.hot.contains_key(&sid) && self.hot.len() >= self.half_cap {
            self.cold = std::mem::take(&mut self.hot);
        }
        self.cold.remove(&sid);
        self.hot.insert(sid, ws);
    }

    #[cfg(test)]
    fn len(&self) -> usize {
        self.hot.len() + self.cold.len()
    }
}

struct Inner {
    /// bumped by every publish that can change `/api/state`, synchronously: anything computed from the database
    /// before a write is stale after it
    generation: std::sync::atomic::AtomicU64,
    /// bumped by non-`STATELESS` global events (`Scope::All`) and monotonic `ws_generations` prunes
    global_generation: AtomicU64,
    /// per-workspace counters bumped only when a non-`STATELESS` event carries a non-empty `"ws"`
    ws_generations: Mutex<HashMap<String, u64>>,
    /// bumped by every `stats` event before it is coalesced: cached statistics older than this are stale
    stats_generation: std::sync::atomic::AtomicU64,
    tx: std::sync::mpsc::Sender<Value>,
    out: broadcast::Sender<Arc<Frame>>,
    taps: Taps,
    resolver: Arc<OnceLock<Resolver>>,
    log: Arc<Log>,
}

/// Fan-out of bus events. Publishing is synchronous and never blocks (it can run under the store's write
/// lock); one dispatcher thread, in publish order, resolves a session event's workspace (cached; sessions
/// never move), slims its data (`evOut`), serializes once and broadcasts the bytes. A subscriber that falls
/// more than [`Bus::CAPACITY`] frames behind is told it lagged (the SSE layer asks the web to refetch).
///
/// The dispatcher also numbers the frames it sends and keeps the last [`RING_FRAMES`] / [`RING_BYTES`] of
/// them in memory (the change log: [`Bus::cursor`], [`Bus::replay`]), except the [`TRANSIENT`] ones and those
/// nobody may see. Ids increase in send order, which is not publish order: a held `stats` frame gets its id
/// when it goes out. The log is only sound because every store write publishes after its commit
/// (steve-store `tx.rs`): a frame describes a change that a read made after it can already see. A
/// publisher that published before committing could have a snapshot whose cursor covers a change it misses.
#[derive(Clone)]
pub struct Bus(Arc<Inner>);

impl Default for Bus {
    fn default() -> Self {
        Self::new()
    }
}

impl Bus {
    pub const CAPACITY: usize = 4096;
    pub const TAP_CAPACITY: usize = TAP_CAPACITY;
    pub const TAP_OVERFLOW_MAX: usize = TAP_OVERFLOW_MAX;

    pub fn new() -> Self {
        Self::with_window(RING_FRAMES, RING_BYTES)
    }

    /// A Bus whose change log keeps at most `frames` frames and `bytes` bytes.
    pub(crate) fn with_window(frames: usize, bytes: usize) -> Self {
        let (tx, rx) = std::sync::mpsc::channel::<Value>();
        let out = broadcast::channel(Self::CAPACITY).0;
        let resolver: Arc<OnceLock<Resolver>> = Arc::new(OnceLock::new());
        let taps: Taps = Arc::default();
        let log = Arc::new(Log {
            epoch: steve_store::uid("ep"),
            ring: Mutex::new(Ring::new(frames, bytes)),
            head: AtomicU64::new(0),
        });
        let (o, r, t, l) = (out.clone(), resolver.clone(), taps.clone(), log.clone());
        let spawned = std::thread::Builder::new()
            .name("steve-bus".into())
            .spawn(move || dispatch(rx, o, r, t, l));
        if let Err(e) = spawned {
            tracing::error!("bus dispatcher: {e}");
        }
        Bus(Arc::new(Inner {
            generation: std::sync::atomic::AtomicU64::new(0),
            global_generation: AtomicU64::new(0),
            ws_generations: Mutex::new(HashMap::new()),
            stats_generation: std::sync::atomic::AtomicU64::new(0),
            tx,
            out,
            taps,
            resolver,
            log,
        }))
    }

    /// Every published event whose `type` is one of `types`, as JSON, in publish order (module jobs that
    /// react to store changes, e.g. steve-knowledge's `sync::on_event`).
    pub fn tap(&self, types: &[&str]) -> tokio::sync::mpsc::Receiver<Value> {
        let (tx, rx) = tokio::sync::mpsc::channel(Self::TAP_CAPACITY);
        let mut t = self.0.taps.lock().unwrap_or_else(|e| e.into_inner());
        t.push(TapSlot::new(
            types.iter().map(|s| s.to_string()).collect(),
            tx,
        ));
        rx
    }

    /// How to find a session's workspace (set once storage is open).
    pub fn set_session_workspace(
        &self,
        f: impl Fn(&str) -> Option<String> + Send + Sync + 'static,
    ) {
        let _ = self.0.resolver.set(Box::new(f));
    }

    /// Delivers `e` to every current subscriber that may see it, in publish order.
    pub fn publish(&self, e: HubEvent) {
        if let Ok(v) = serde_json::to_value(e) {
            self.publish_value(v);
        }
    }

    /// Any event shaped `{type, …}` (steve-store's and steve-core's events serialize to the same JSON).
    pub fn publish_value(&self, v: Value) {
        let ty = v.get("type").and_then(Value::as_str);
        if !ty.is_some_and(|t| STATELESS.contains(&t)) {
            self.0
                .generation
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            match v.get("ws") {
                Some(Value::String(w)) if !w.is_empty() => {
                    let mut map = self
                        .0
                        .ws_generations
                        .lock()
                        .unwrap_or_else(|e| e.into_inner());
                    if let Some(g) = map.get_mut(w.as_str()) {
                        *g = g.wrapping_add(1);
                    } else {
                        if map.len() >= WS_GENERATION_MAX {
                            let max_evicted = map.values().copied().max().unwrap_or(0);
                            self.0
                                .global_generation
                                .fetch_add(max_evicted, Ordering::SeqCst);
                            map.clear();
                        }
                        map.insert(w.clone(), 1);
                    }
                }
                Some(_) => {}
                None if steve_modules::sse::requires_workspace(ty.unwrap_or_default()) => {}
                None => {
                    self.0.global_generation.fetch_add(1, Ordering::SeqCst);
                }
            }
        }
        if ty == Some("stats") {
            self.0
                .stats_generation
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        }
        let _ = self.0.tx.send(v);
    }

    /// How many events were published so far.
    pub fn generation(&self) -> u64 {
        self.0.generation.load(std::sync::atomic::Ordering::SeqCst)
    }

    /// State generation visible to `ws`: bumped by global state changes and changes scoped to `ws`,
    /// while leaving other workspaces untouched.
    pub fn generation_for(&self, ws: &str) -> u64 {
        let map = self
            .0
            .ws_generations
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        let ws_gen = map.get(ws).copied().unwrap_or(0);
        let global = self.0.global_generation.load(Ordering::SeqCst);
        global.wrapping_add(ws_gen)
    }

    /// How many `stats` events were published so far (before coalescing).
    pub fn stats_generation(&self) -> u64 {
        self.0
            .stats_generation
            .load(std::sync::atomic::Ordering::SeqCst)
    }

    pub fn subscribe(&self) -> broadcast::Receiver<Arc<Frame>> {
        self.0.out.subscribe()
    }

    /// This process's change-log epoch.
    pub fn epoch(&self) -> &str {
        &self.0.log.epoch
    }

    /// The change log's head: every kept frame up to it has been broadcast.
    pub fn cursor(&self) -> Cursor {
        Cursor {
            epoch: self.0.log.epoch.clone(),
            n: self.0.log.head.load(Ordering::SeqCst),
        }
    }

    /// The kept frames after `after`, or a reset when they can't all be given.
    pub fn replay(&self, after: &Cursor) -> Replay {
        let ring = self.0.log.ring.lock().unwrap_or_else(|e| e.into_inner());
        let head = self.0.log.head.load(Ordering::SeqCst);
        if after.epoch != self.0.log.epoch || after.n > head || after.n < ring.dropped {
            return Replay::Reset { head };
        }
        // ids are contiguous, so the first one after the cursor is found by position; only Arcs are copied
        let from = ring
            .frames
            .partition_point(|f| f.id.is_some_and(|id| id <= after.n));
        Replay::Frames {
            frames: ring.frames.range(from..).cloned().collect(),
            head,
        }
    }

    /// [`Bus::replay`] after a cursor as sent on the wire; a malformed one is a reset.
    pub fn replay_from(&self, after: &str) -> Replay {
        match Cursor::parse(after) {
            Some(c) => self.replay(&c),
            None => Replay::Reset {
                head: self.0.log.head.load(Ordering::SeqCst),
            },
        }
    }
}

fn dispatch(
    rx: std::sync::mpsc::Receiver<Value>,
    out: broadcast::Sender<Arc<Frame>>,
    resolver: Arc<OnceLock<Resolver>>,
    taps: Taps,
    log: Arc<Log>,
) {
    let mut out = Sender {
        out,
        log,
        next_id: 0,
    };
    let mut seen = SessionWsCache::new(SESSION_CACHE_HALF);
    let mut stats = StatsThrottle::default();
    let mut tap_overflow = false;
    loop {
        let tap_due = tap_overflow.then(|| std::time::Instant::now() + TAP_DRAIN_INTERVAL);
        let deadline = match (stats.deadline(), tap_due) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        };
        let next = match deadline {
            None => rx
                .recv()
                .map_err(|_| std::sync::mpsc::RecvTimeoutError::Disconnected),
            Some(at) => rx.recv_timeout(at.saturating_duration_since(std::time::Instant::now())),
        };
        let mut v = match next {
            Ok(v) => v,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                let now = std::time::Instant::now();
                if tap_overflow {
                    let mut t = taps.lock().unwrap_or_else(|e| e.into_inner());
                    t.retain_mut(TapSlot::flush);
                    tap_overflow = t.iter().any(|s| !s.overflow.is_empty());
                }
                for v in stats.due(now) {
                    out.send(&v, Scope::Ws(ws_of(&v)));
                }
                continue;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
        };
        let ty = v
            .get("type")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        {
            let now = std::time::Instant::now();
            let mut t = taps.lock().unwrap_or_else(|e| e.into_inner());
            t.retain_mut(|slot| {
                if slot.matches(&ty) {
                    slot.deliver(v.clone(), now)
                } else {
                    slot.flush()
                }
            });
            tap_overflow = t.iter().any(|s| !s.overflow.is_empty());
        }
        if ty == "stats" {
            // at most one per workspace every 5 s: the first at once, the rest merged into one at the window's end
            if let Some(v) = stats.offer(v, std::time::Instant::now()) {
                out.send(&v, Scope::Ws(ws_of(&v)));
            }
            continue;
        }
        let scope = if ty == "event" {
            let sid = v
                .get("session_id")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let ws = match seen.get(&sid) {
                Some(w) => Some(w),
                None => {
                    let w = resolver.get().and_then(|f| f(&sid));
                    if let Some(w) = &w {
                        seen.insert(sid, w.clone());
                    }
                    w
                }
            };
            if let Some(d) = v.get_mut("data") {
                *d = crate::api::slim::ev_out(d.take());
            }
            ws.map(Scope::Ws).unwrap_or(Scope::Nobody)
        } else {
            // Explicit workspace metadata is authoritative, including newly registered event
            // types. Known private types with missing/malformed metadata fail closed.
            match v.get("ws") {
                Some(Value::String(w)) if !w.is_empty() => Scope::Ws(w.clone()),
                Some(_) => Scope::Nobody,
                None if steve_modules::sse::requires_workspace(&ty) => Scope::Nobody,
                None => Scope::All,
            }
        };
        out.send(&v, scope);
    }
}

/// The dispatcher's end: numbers, logs and broadcasts frames. Only the dispatcher thread holds it, so ids
/// follow send order.
struct Sender {
    out: broadcast::Sender<Arc<Frame>>,
    log: Arc<Log>,
    next_id: u64,
}

impl Sender {
    fn send(&mut self, v: &Value, scope: Scope) {
        let json = serde_json::to_string(v).unwrap_or_default();
        let ty = v.get("type").and_then(Value::as_str).unwrap_or_default();
        let kept = scope != Scope::Nobody && !TRANSIENT.contains(&ty);
        let id = kept.then(|| {
            self.next_id += 1;
            self.next_id
        });
        let frame = Arc::new(Frame {
            bytes: Bytes::from(format!("data: {json}\n\n")),
            scope,
            id,
        });
        if let Some(id) = id {
            let mut ring = self.log.ring.lock().unwrap_or_else(|e| e.into_inner());
            ring.push(frame.clone());
            self.log.head.store(id, Ordering::SeqCst);
        }
        let _ = self.out.send(frame);
    }
}

fn ws_of(v: &Value) -> String {
    v.get("ws")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string()
}

/// `{type:"stats"}` events, coalesced per workspace: a quiet workspace's event goes out at
/// once; within [`StatsThrottle::WINDOW`] of the last one sent, the latest is held and sent when the window
/// ends. So a browser gets at most one every 5 seconds and never misses the last change.
#[derive(Debug, Default)]
struct StatsThrottle {
    last: HashMap<String, std::time::Instant>,
    held: HashMap<String, Value>,
}

impl StatsThrottle {
    const WINDOW: std::time::Duration = std::time::Duration::from_secs(5);

    /// The event to send now, if any.
    fn offer(&mut self, v: Value, now: std::time::Instant) -> Option<Value> {
        let ws = ws_of(&v);
        match self.last.get(&ws) {
            Some(t) if now.duration_since(*t) < Self::WINDOW => {
                self.held.insert(ws, v);
                None
            }
            _ => {
                if self.last.len() > 10_000 {
                    self.last
                        .retain(|_, t| now.duration_since(*t) < Self::WINDOW);
                }
                self.last.insert(ws, now);
                Some(v)
            }
        }
    }

    /// When the earliest held event is due.
    fn deadline(&self) -> Option<std::time::Instant> {
        self.held
            .keys()
            .filter_map(|ws| self.last.get(ws))
            .map(|t| *t + Self::WINDOW)
            .min()
    }

    /// The held events whose window has ended.
    fn due(&mut self, now: std::time::Instant) -> Vec<Value> {
        let ready: Vec<String> = self
            .held
            .keys()
            .filter(|ws| {
                self.last
                    .get(*ws)
                    .is_none_or(|t| now.duration_since(*t) >= Self::WINDOW)
            })
            .cloned()
            .collect();
        ready
            .into_iter()
            .filter_map(|ws| {
                let v = self.held.remove(&ws)?;
                self.last.insert(ws, now);
                Some(v)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn js(e: &HubEvent) -> String {
        serde_json::to_string(e).expect("json")
    }

    #[test]
    fn shapes_match_ts() {
        assert_eq!(
            js(&HubEvent::Item { id: "ST-1".into() }),
            r#"{"type":"item","id":"ST-1"}"#
        );
        assert_eq!(
            js(&HubEvent::Session {
                id: "s".into(),
                item_id: None
            }),
            r#"{"type":"session","id":"s"}"#
        );
        assert_eq!(
            js(&HubEvent::Session {
                id: "s".into(),
                item_id: Some(None)
            }),
            r#"{"type":"session","id":"s","item_id":null}"#
        );
        assert_eq!(
            js(&HubEvent::Decision {
                id: "d".into(),
                item_id: Some(Some("ST-2".into()))
            }),
            r#"{"type":"decision","id":"d","item_id":"ST-2"}"#
        );
        assert_eq!(
            js(&HubEvent::Event {
                session_id: "s".into(),
                seq: 3,
                kind: "user".into(),
                data: json!({"text":"hi","via":"me"})
            }),
            r#"{"type":"event","session_id":"s","seq":3,"kind":"user","data":{"text":"hi","via":"me"}}"#
        );
        assert_eq!(
            js(&HubEvent::Thinking {
                on: true,
                ws: Some("default".into()),
                phase: None
            }),
            r#"{"type":"steve.thinking","on":true,"ws":"default"}"#
        );
        assert_eq!(
            js(&HubEvent::Bot { id: "*".into() }),
            r#"{"type":"bot","id":"*"}"#
        );
        let back: HubEvent =
            serde_json::from_str(r#"{"type":"session","id":"s","item_id":null}"#).expect("parse");
        assert_eq!(
            back,
            HubEvent::Session {
                id: "s".into(),
                item_id: Some(None)
            }
        );
    }

    #[test]
    fn stats_are_coalesced_per_workspace() {
        let mut t = StatsThrottle::default();
        let at = std::time::Instant::now();
        let ev = |ws: &str, n: i64| json!({"type": "stats", "ws": ws, "n": n});
        let s = std::time::Duration::from_secs(1);
        assert_eq!(t.offer(ev("a", 1), at), Some(ev("a", 1)));
        assert_eq!(t.offer(ev("a", 2), at + s), None);
        assert_eq!(t.offer(ev("a", 3), at + 2 * s), None);
        // another workspace has its own window
        assert_eq!(t.offer(ev("b", 1), at + 2 * s), Some(ev("b", 1)));
        assert_eq!(t.deadline(), Some(at + 5 * s));
        assert!(t.due(at + 4 * s).is_empty());
        // the window's end sends the latest of the held ones, once
        assert_eq!(t.due(at + 5 * s), vec![ev("a", 3)]);
        assert_eq!(t.deadline(), None);
        assert_eq!(t.offer(ev("a", 4), at + 6 * s), None);
        assert_eq!(t.due(at + 10 * s), vec![ev("a", 4)]);
    }

    #[test]
    fn only_state_changes_bump_the_generation() {
        let bus = Bus::new();
        let g = bus.generation();
        bus.publish_value(
            json!({"type": "event", "session_id": "s", "seq": 1, "kind": "k", "data": {}}),
        );
        bus.publish_value(
            json!({"type": "steve.draft", "ws": "w1", "mid": "m", "thought": "", "text": "x"}),
        );
        bus.publish_value(json!({"type": "steve.tool", "ws": "w1"}));
        bus.publish_value(json!({"type": "stats", "ws": "w1"}));
        assert_eq!(bus.generation(), g);
        bus.publish_value(json!({"type": "session", "id": "s"}));
        bus.publish_value(json!({"type": "message", "id": "m"}));
        assert_eq!(bus.generation(), g + 2);
    }

    #[tokio::test]
    async fn stats_frames_stay_in_their_workspace() {
        let bus = Bus::new();
        let mut a = bus.subscribe();
        let g = bus.stats_generation();
        bus.publish_value(json!({"type": "stats", "ws": "w1", "day": "2026-11-01"}));
        bus.publish_value(json!({"type": "stats", "ws": "w1"}));
        assert_eq!(bus.stats_generation(), g + 2);
        let f = a.recv().await.expect("first");
        assert_eq!(f.scope, Scope::Ws("w1".into()));
        assert!(f.visible_to("w1") && !f.visible_to("w2"));
        // the second is held for the rest of the window
        assert!(
            tokio::time::timeout(std::time::Duration::from_secs(3), a.recv())
                .await
                .is_err()
        );
        let f = tokio::time::timeout(std::time::Duration::from_secs(4), a.recv())
            .await
            .expect("trailing")
            .expect("frame");
        assert_eq!(
            &f.bytes[..],
            &b"data: {\"type\":\"stats\",\"ws\":\"w1\"}\n\n"[..]
        );
    }

    #[tokio::test]
    async fn fan_out_in_order_with_scopes() {
        let bus = Bus::new();
        bus.set_session_workspace(|sid| (sid == "s1").then(|| "w1".to_string()));
        let mut a = bus.subscribe();
        bus.publish(HubEvent::Item { id: "1".into() });
        bus.publish(HubEvent::Event {
            session_id: "s1".into(),
            seq: 1,
            kind: "x".into(),
            data: json!({"_meta": 1, "a": 2}),
        });
        bus.publish(HubEvent::Event {
            session_id: "nope".into(),
            seq: 2,
            kind: "x".into(),
            data: json!({}),
        });
        bus.publish(HubEvent::Thinking {
            on: true,
            ws: Some("w2".into()),
            phase: None,
        });
        let f = a.recv().await.expect("1");
        assert_eq!(
            (&f.bytes[..], &f.scope),
            (
                &b"data: {\"type\":\"item\",\"id\":\"1\"}\n\n"[..],
                &Scope::All
            )
        );
        let f = a.recv().await.expect("2");
        assert_eq!(&f.bytes[..], &b"data: {\"type\":\"event\",\"session_id\":\"s1\",\"seq\":1,\"kind\":\"x\",\"data\":{\"a\":2}}\n\n"[..]);
        assert!(f.visible_to("w1") && !f.visible_to("w2"));
        assert_eq!(a.recv().await.expect("3").scope, Scope::Nobody);
        assert!(a.recv().await.expect("4").visible_to("w2"));
        /* a chat answer being written stays in its workspace */
        bus.publish_value(
            json!({"type": "steve.draft", "ws": "w1", "mid": "m", "thought": "", "text": "一"}),
        );
        let f = a.recv().await.expect("5");
        assert_eq!(f.scope, Scope::Ws("w1".into()));
    }

    #[test]
    fn generation_for_isolates_workspaces_and_keeps_global_monotonicity() {
        let bus = Bus::new();
        assert_eq!(bus.generation(), 0);
        assert_eq!(bus.generation_for("w1"), 0);
        assert_eq!(bus.generation_for("w2"), 0);

        // STATELESS events touch neither global nor workspace generations.
        for ev in [
            json!({"type": "event", "session_id": "s1", "seq": 1, "kind": "k", "data": {}}),
            json!({"type": "steve.draft", "ws": "w1", "mid": "m", "thought": "", "text": "x"}),
            json!({"type": "steve.tool", "ws": "w1"}),
            json!({"type": "stats", "ws": "w1"}),
        ] {
            bus.publish_value(ev);
        }
        assert_eq!(bus.generation(), 0);
        assert_eq!(bus.generation_for("w1"), 0);
        assert_eq!(bus.generation_for("w2"), 0);

        // Workspace-scoped events in w1 bump w1 (and legacy generation()), leaving w2 untouched.
        for ev in [
            json!({"type": "party", "id": "p1", "ws": "w1"}),
            json!({"type": "mm", "id": "m1", "ws": "w1"}),
            json!({"type": "yolo", "ws": "w1"}),
            json!({"type": "yolo_action", "id": "ya1", "ws": "w1"}),
            json!({"type": "thread.run", "id": "tr1", "ws": "w1"}),
            json!({"type": "steve.thinking", "on": true, "ws": "w1"}),
        ] {
            bus.publish_value(ev);
        }
        assert_eq!(bus.generation(), 6);
        assert_eq!(bus.generation_for("w1"), 6);
        assert_eq!(bus.generation_for("w2"), 0);

        // Workspace-scoped events in w2 bump w2, leaving w1 untouched.
        bus.publish_value(json!({"type": "connection", "id": "c1", "ws": "w2"}));
        assert_eq!(bus.generation(), 7);
        assert_eq!(bus.generation_for("w1"), 6);
        assert_eq!(bus.generation_for("w2"), 1);

        // Malformed/empty/missing ws on workspace-required types map to Scope::Nobody and do not
        // invalidate other workspaces via generation_for.
        for ev in [
            json!({"type": "party"}),
            json!({"type": "party", "ws": ""}),
            json!({"type": "party", "ws": null}),
            json!({"type": "plugin.custom"}),
        ] {
            bus.publish_value(ev);
        }
        assert_eq!(bus.generation(), 11);
        assert_eq!(bus.generation_for("w1"), 6);
        assert_eq!(bus.generation_for("w2"), 1);

        // Global events (no ws, Scope::All) bump every workspace's generation_for.
        bus.publish(HubEvent::Item { id: "ST-1".into() });
        bus.publish(HubEvent::Session {
            id: "s1".into(),
            item_id: None,
        });
        assert_eq!(bus.generation(), 13);
        assert_eq!(bus.generation_for("w1"), 8);
        assert_eq!(bus.generation_for("w2"), 3);
        assert_eq!(bus.generation_for("w3"), 2);

        // Exceeding WS_GENERATION_MAX prunes safely while keeping generation_for strictly monotonic.
        let before_w1 = bus.generation_for("w1");
        let before_w2 = bus.generation_for("w2");
        for i in 0..(WS_GENERATION_MAX + 50) {
            bus.publish_value(json!({"type": "mm", "id": "m", "ws": format!("wx-{i}")}));
        }
        assert!(bus.0.ws_generations.lock().unwrap().len() <= WS_GENERATION_MAX);
        let after_w1 = bus.generation_for("w1");
        let after_w2 = bus.generation_for("w2");
        assert!(after_w1 >= before_w1, "{after_w1} < {before_w1}");
        assert!(after_w2 >= before_w2, "{after_w2} < {before_w2}");
        bus.publish_value(json!({"type": "mm", "id": "m2", "ws": "w1"}));
        assert_eq!(bus.generation_for("w1"), after_w1 + 1);
        assert_eq!(bus.generation_for("w2"), after_w2);
    }

    #[test]
    fn session_ws_cache_evicts_generationally_without_clear_cliff() {
        let mut cache = SessionWsCache::new(4);
        for i in 0..4 {
            cache.insert(format!("s{i}"), format!("w{i}"));
        }
        assert_eq!(cache.len(), 4);
        // Trigger rotation: s0..s3 move to cold, s4 enters hot (no full clear).
        cache.insert("s4".into(), "w4".into());
        assert_eq!(cache.len(), 5);
        // Hot lookup on active cold entry s1 promotes s1 into hot.
        assert_eq!(cache.get("s1").as_deref(), Some("w1"));
        // Fill hot and trigger another rotation: promoted s1 survives, stale cold entries s0/s2/s3 evict.
        cache.insert("s5".into(), "w5".into());
        cache.insert("s6".into(), "w6".into());
        cache.insert("s7".into(), "w7".into());
        assert!(cache.len() <= 8, "bounded at 2 * half_cap: {}", cache.len());
        assert_eq!(cache.get("s1").as_deref(), Some("w1"));
        assert_eq!(cache.get("s0"), None);
        assert_eq!(cache.get("s2"), None);
        assert_eq!(cache.get("s3"), None);
    }

    #[tokio::test]
    async fn tap_overflow_coalesces_same_key_and_drains_without_new_publishes() {
        let bus = Bus::new();
        let mut tap = bus.tap(&["connection"]);
        let mut sse = bus.subscribe();

        // Fill the tap's bounded channel (TAP_CAPACITY).
        for i in 0..TAP_CAPACITY {
            bus.publish_value(json!({"type": "connection", "ws": "w1", "id": format!("c-{i}")}));
            let _ = sse.recv().await.expect("sse");
        }
        assert_eq!(bus.0.taps.lock().unwrap()[0].overflow.len(), 0);

        // Publish repeated updates for 3 distinct (type, ws, id) keys while consumer is stalled.
        for rev in 1..=40 {
            bus.publish_value(json!({"type": "connection", "ws": "w1", "id": "alpha", "rev": rev}));
        }
        for rev in 1..=25 {
            bus.publish_value(json!({"type": "connection", "ws": "w1", "id": "beta", "rev": rev}));
        }
        for rev in 1..=15 {
            bus.publish_value(json!({"type": "connection", "ws": "w2", "id": "alpha", "rev": rev}));
        }
        for _ in 0..(40 + 25 + 15) {
            let _ = sse.recv().await.expect("sse");
        }

        // All 80 overflow events coalesced into just 3 pending entries.
        assert_eq!(bus.0.taps.lock().unwrap()[0].overflow.len(), 3);

        // With NO further publishes, draining the receiver flushes the overflow via TAP_DRAIN_INTERVAL.
        for i in 0..TAP_CAPACITY {
            let v = tokio::time::timeout(std::time::Duration::from_secs(2), tap.recv())
                .await
                .expect("in time")
                .expect("event");
            assert_eq!(v["id"], format!("c-{i}"));
        }
        let mut coalesced = Vec::new();
        for _ in 0..3 {
            let v = tokio::time::timeout(std::time::Duration::from_secs(2), tap.recv())
                .await
                .expect("overflow drained")
                .expect("event");
            coalesced.push((
                v["ws"].as_str().unwrap().to_string(),
                v["id"].as_str().unwrap().to_string(),
                v["rev"].as_i64().unwrap(),
            ));
        }
        assert_eq!(
            coalesced,
            vec![
                ("w1".into(), "alpha".into(), 40),
                ("w1".into(), "beta".into(), 25),
                ("w2".into(), "alpha".into(), 15),
            ]
        );
        assert!(bus.0.taps.lock().unwrap()[0].overflow.is_empty());
    }

    #[tokio::test]
    async fn tap_flood_never_blocks_bounds_memory_evicts_oldest_and_drops_closed_tap() {
        let bus = Bus::new();
        let mut tap = bus.tap(&["mm"]);
        let mut sse = bus.subscribe();

        for i in 0..TAP_CAPACITY {
            bus.publish_value(json!({"type": "mm", "ws": "w1", "id": format!("init-{i}")}));
            let _ = sse.recv().await.expect("sse");
        }

        // Flood TAP_OVERFLOW_MAX + 200 distinct events; publisher never blocks and overflow stays capped.
        let extra = 200usize;
        let flood = TAP_OVERFLOW_MAX + extra;
        for i in 0..flood {
            bus.publish_value(json!({"type": "mm", "ws": "w1", "id": format!("ov-{i}"), "seq": i}));
            let _ = sse.recv().await.expect("sse");
        }
        assert_eq!(
            bus.0.taps.lock().unwrap()[0].overflow.len(),
            TAP_OVERFLOW_MAX
        );

        // Drain channel + bounded overflow: oldest `extra` overflow items were evicted, newest kept in order.
        for i in 0..TAP_CAPACITY {
            let v = tokio::time::timeout(std::time::Duration::from_secs(2), tap.recv())
                .await
                .expect("channel item")
                .expect("value");
            assert_eq!(v["id"], format!("init-{i}"));
        }
        for i in extra..flood {
            let v = tokio::time::timeout(std::time::Duration::from_secs(2), tap.recv())
                .await
                .expect("overflow item")
                .expect("value");
            assert_eq!(v["seq"], i);
        }

        // Now create a second tap, overflow it, drop its receiver with NO new publishes, and verify cleanup.
        let tap2 = bus.tap(&["mm"]);
        assert_eq!(bus.0.taps.lock().unwrap().len(), 2);
        for i in 0..(TAP_CAPACITY + 32) {
            bus.publish_value(json!({"type": "mm", "ws": "w1", "id": format!("drop-{i}")}));
            let _ = sse.recv().await.expect("sse");
        }
        drop(tap2);
        let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(2);
        while bus.0.taps.lock().unwrap().len() > 1 {
            assert!(
                tokio::time::Instant::now() < deadline,
                "closed tap with overflow was not dropped"
            );
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(bus.0.taps.lock().unwrap().len(), 1);
    }
}

#[cfg(test)]
mod scope_regression {
    use super::*;
    use serde_json::json;

    #[tokio::test]
    async fn private_and_new_events_never_escape_their_workspace() {
        let bus = Bus::new();
        let mut rx = bus.subscribe();
        for ty in [
            "steve.tool",
            "steve.draft",
            "steve.thinking",
            "plugin.future",
        ] {
            bus.publish_value(json!({"type":ty,"ws":"private-a","brief":"private"}));
            let f = rx.recv().await.unwrap();
            assert!(f.visible_to("private-a"));
            assert!(!f.visible_to("private-b"), "{ty}");
        }
        for ev in [
            json!({"type":"steve.tool"}),
            json!({"type":"plugin.future"}),
            json!({"type":"mail"}),
            json!({"type":"plugin.future","ws":null}),
            json!({"type":"steve.tool","ws":""}),
        ] {
            bus.publish_value(ev);
            assert_eq!(rx.recv().await.unwrap().scope, Scope::Nobody);
        }
    }
}

#[cfg(test)]
mod change_log {
    use super::*;
    use serde_json::json;
    use std::time::Duration;

    async fn next(rx: &mut broadcast::Receiver<Arc<Frame>>) -> Arc<Frame> {
        tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("in time")
            .expect("frame")
    }

    fn frames(r: Replay) -> (Vec<Arc<Frame>>, u64) {
        match r {
            Replay::Frames { frames, head } => (frames, head),
            Replay::Reset { head } => panic!("reset at {head}"),
        }
    }

    fn reset(r: Replay) -> u64 {
        match r {
            Replay::Reset { head } => head,
            Replay::Frames { frames, .. } => panic!("{} frames, not a reset", frames.len()),
        }
    }

    fn ids(f: &[Arc<Frame>]) -> Vec<u64> {
        f.iter().filter_map(|f| f.id).collect()
    }

    #[tokio::test]
    async fn ids_follow_send_order_even_for_held_stats() {
        let bus = Bus::new();
        let mut rx = bus.subscribe();
        let start = bus.cursor();
        bus.publish_value(json!({"type": "stats", "ws": "w1", "n": 1}));
        bus.publish_value(json!({"type": "stats", "ws": "w1", "n": 2}));
        bus.publish(HubEvent::Item { id: "ST-1".into() });
        let a = next(&mut rx).await;
        let b = next(&mut rx).await;
        // the held stats frame goes out after the item published later, and its id is the larger one
        let c = tokio::time::timeout(Duration::from_secs(7), rx.recv())
            .await
            .expect("trailing stats")
            .expect("frame");
        assert!(a.bytes.starts_with(b"data: {\"type\":\"stats\""));
        assert!(b.bytes.starts_with(b"data: {\"type\":\"item\""));
        assert!(c.bytes.ends_with(b"\"n\":2}\n\n"));
        let (a, b, c) = (a.id.unwrap(), b.id.unwrap(), c.id.unwrap());
        assert!(a < b && b < c, "{a} {b} {c}");
        let (log, head) = frames(bus.replay(&start));
        assert_eq!(ids(&log), vec![a, b, c]);
        assert_eq!(head, c);
    }

    #[tokio::test]
    async fn transient_and_invisible_frames_are_not_kept() {
        let bus = Bus::new();
        bus.set_session_workspace(|sid| (sid == "s1").then(|| "w1".to_string()));
        let mut rx = bus.subscribe();
        let start = bus.cursor();
        bus.publish(HubEvent::Event {
            session_id: "s1".into(),
            seq: 1,
            kind: "x".into(),
            data: json!({}),
        });
        bus.publish_value(json!({"type": "steve.draft", "ws": "w1", "mid": "m", "text": "x"}));
        bus.publish_value(json!({"type": "steve.tool"}));
        bus.publish(HubEvent::Session {
            id: "s1".into(),
            item_id: None,
        });
        for _ in 0..3 {
            assert_eq!(next(&mut rx).await.id, None);
        }
        let kept = next(&mut rx).await;
        assert_eq!(kept.id, Some(start.n + 1));
        let (log, head) = frames(bus.replay(&start));
        assert_eq!(ids(&log), vec![start.n + 1]);
        assert_eq!(&log[0].bytes[..], &kept.bytes[..]);
        assert_eq!(head, start.n + 1);
    }

    async fn publish_items(bus: &Bus, rx: &mut broadcast::Receiver<Arc<Frame>>, n: usize) {
        for i in 0..n {
            bus.publish(HubEvent::Item {
                id: format!("ST-{i:03}"),
            });
        }
        for _ in 0..n {
            next(rx).await;
        }
    }

    #[tokio::test]
    async fn eviction_by_count_resets_older_cursors() {
        let bus = Bus::with_window(3, usize::MAX);
        let mut rx = bus.subscribe();
        let epoch = bus.epoch().to_string();
        publish_items(&bus, &mut rx, 5).await;
        let at = |n| Cursor {
            epoch: epoch.clone(),
            n,
        };
        // ids 3..=5 are kept: a cursor at 2 continues, one at 1 has lost frame 2
        assert_eq!(ids(&frames(bus.replay(&at(2))).0), vec![3, 4, 5]);
        assert_eq!(ids(&frames(bus.replay(&at(4))).0), vec![5]);
        assert_eq!(reset(bus.replay(&at(1))), 5);
        assert_eq!(reset(bus.replay(&at(0))), 5);
    }

    #[tokio::test]
    async fn eviction_by_bytes_resets_older_cursors() {
        // every item frame here is `data: {"type":"item","id":"ST-00i"}\n\n`
        let one = b"data: {\"type\":\"item\",\"id\":\"ST-000\"}\n\n".len();
        let bus = Bus::with_window(1000, 2 * one + 1);
        let mut rx = bus.subscribe();
        publish_items(&bus, &mut rx, 4).await;
        let at = |n| Cursor {
            epoch: bus.epoch().to_string(),
            n,
        };
        assert_eq!(ids(&frames(bus.replay(&at(2))).0), vec![3, 4]);
        assert_eq!(reset(bus.replay(&at(1))), 4);
        // a frame larger than the whole window is still kept on its own
        let bus = Bus::with_window(1000, 1);
        let mut rx = bus.subscribe();
        publish_items(&bus, &mut rx, 2).await;
        let at = |n| Cursor {
            epoch: bus.epoch().to_string(),
            n,
        };
        assert_eq!(ids(&frames(bus.replay(&at(1))).0), vec![2]);
        assert_eq!(reset(bus.replay(&at(0))), 2);
    }

    #[tokio::test]
    async fn foreign_future_or_malformed_cursors_reset() {
        let bus = Bus::new();
        let mut rx = bus.subscribe();
        publish_items(&bus, &mut rx, 2).await;
        let other = Bus::new();
        assert_ne!(bus.epoch(), other.epoch());
        assert_eq!(reset(bus.replay(&other.cursor())), 2);
        let ahead = Cursor {
            epoch: bus.epoch().to_string(),
            n: 3,
        };
        assert_eq!(reset(bus.replay(&ahead)), 2);
        for bad in [
            "",
            "nonsense",
            ":1",
            &format!("{}:", bus.epoch()),
            &format!("{}:-1", bus.epoch()),
            &format!("{}:1x", bus.epoch()),
            &format!("{}:99999999999999999999999", bus.epoch()),
        ] {
            assert_eq!(Cursor::parse(bad), None, "{bad}");
            assert_eq!(reset(bus.replay_from(bad)), 2, "{bad}");
        }
        let c = bus.cursor();
        assert_eq!(Cursor::parse(&c.to_string()), Some(c.clone()));
        assert_eq!(
            ids(&frames(bus.replay_from(&format!("{}:1", c.epoch))).0),
            vec![2]
        );
    }

    #[tokio::test]
    async fn replay_at_head_is_empty() {
        let bus = Bus::new();
        let (log, head) = frames(bus.replay(&bus.cursor()));
        assert!(log.is_empty());
        assert_eq!(head, 0);
        let mut rx = bus.subscribe();
        publish_items(&bus, &mut rx, 3).await;
        let (log, head) = frames(bus.replay(&bus.cursor()));
        assert!(log.is_empty());
        assert_eq!(head, 3);
    }

    #[tokio::test]
    async fn cursor_after_dispatch_is_the_last_frame_id() {
        let bus = Bus::new();
        let mut rx = bus.subscribe();
        for i in 0..20 {
            bus.publish(HubEvent::Item { id: format!("{i}") });
            bus.publish_value(json!({"type": "steve.draft", "ws": "w1", "text": "x"}));
            let f = next(&mut rx).await;
            next(&mut rx).await;
            assert_eq!(bus.cursor().n, f.id.expect("id"));
            assert_eq!(bus.cursor().epoch, bus.epoch());
        }
    }
}
