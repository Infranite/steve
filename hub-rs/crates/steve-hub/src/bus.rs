//! Typed, ordered in-process notifications. Encoding belongs to SSE and plugin boundaries.
use bytes::Bytes;
use std::collections::{HashMap, VecDeque};
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};
pub use steve_types::{BusEvent, Scope};
use steve_types::{DomainEvent, Envelope, LiveEvent, Rev, Scoped};
use tokio::sync::broadcast;

/// Who a stream is for: the workspace it shows, and who is signed in.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Viewer {
    pub ws: String,
    /// `users.id`; empty for none.
    pub user: String,
    /// The Hub's owner (`users.role`): sees what concerns any user's devices.
    pub owner: bool,
}

impl From<&str> for Viewer {
    /// Nobody signed in, in workspace `ws`.
    fn from(ws: &str) -> Viewer {
        Viewer {
            ws: ws.into(),
            ..Default::default()
        }
    }
}

impl From<String> for Viewer {
    fn from(ws: String) -> Viewer {
        Viewer::from(ws.as_str())
    }
}

/// A typed notification in dispatcher order, with its optional replay position.
#[derive(Debug)]
pub struct Delivery {
    pub envelope: Arc<Envelope<BusEvent>>,
    /// Only retained notifications take a position in the change log.
    pub id: Option<u64>,
    /// Opaque egress bytes, initialized by the transport on first use, never by the bus.
    pub(crate) encoded: OnceLock<Bytes>,
}

impl Delivery {
    pub fn visible_to(&self, ws: &str) -> bool {
        self.visible(&Viewer::from(ws))
    }

    pub fn visible(&self, v: &Viewer) -> bool {
        match self.envelope.scope() {
            Scope::Hub => true,
            Scope::Ws(w) => w.as_str() == v.ws,
            Scope::User(u) => v.owner || (!u.as_str().is_empty() && u.as_str() == v.user),
        }
    }
}

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
    Frames {
        frames: Vec<Arc<Delivery>>,
        head: u64,
    },
    /// The cursor can't be continued (another epoch, ahead of the log, or older than what is retained): the
    /// client has to refetch everything; `head` is where it continues from.
    Reset { head: u64 },
}

/// The retained tail of the change log. Ids are contiguous: only kept notifications take one.
/// Keep typed envelopes so replay has no dependency on a transport encoder.
#[derive(Debug)]
struct Ring {
    frames: VecDeque<(Arc<Delivery>, usize)>,
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

    fn push(&mut self, f: Arc<Delivery>, bytes: usize) {
        self.bytes += bytes;
        self.frames.push_back((f, bytes));
        // the newest frame always stays, however large
        while self.frames.len() > 1
            && (self.frames.len() > self.max_frames || self.bytes > self.max_bytes)
        {
            if let Some((old, bytes)) = self.frames.pop_front() {
                self.bytes -= bytes;
                self.dropped = old.id.unwrap_or(self.dropped);
            }
        }
    }
}

/// Retained events are never slimmed. Measure the historical byte budget without populating
/// the egress cache at publication, including the eight-byte per-entry allowance.
fn retained_size(event: &Envelope<BusEvent>) -> usize {
    struct Count(usize);
    impl std::io::Write for Count {
        fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
            self.0 += bytes.len();
            Ok(bytes.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let mut count = Count(8);
    serde_json::to_writer(&mut count, event).expect("typed event size");
    count.0
}

/// The change log shared by the Bus and its dispatcher.
struct Log {
    epoch: String,
    ring: Mutex<Ring>,
    /// the id of the last kept frame, stored after it is in the ring and before it is broadcast
    head: AtomicU64,
}

type EventFilter = Box<dyn Fn(&BusEvent) -> bool + Send + Sync>;
type Taps = Arc<Mutex<Vec<TapSlot>>>;

struct TapSlot {
    filter: EventFilter,
    tx: tokio::sync::mpsc::Sender<Arc<Envelope<BusEvent>>>,
    overflow: VecDeque<(Option<u64>, Arc<Envelope<BusEvent>>)>,
    evicted_since_warn: u64,
    last_warn: Option<std::time::Instant>,
}

impl TapSlot {
    fn new(filter: EventFilter, tx: tokio::sync::mpsc::Sender<Arc<Envelope<BusEvent>>>) -> Self {
        Self {
            filter,
            tx,
            overflow: VecDeque::new(),
            evicted_since_warn: 0,
            last_warn: None,
        }
    }

    fn matches(&self, event: &BusEvent) -> bool {
        (self.filter)(event)
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
    fn deliver(&mut self, v: Arc<Envelope<BusEvent>>, now: std::time::Instant) -> bool {
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

    fn push_overflow(&mut self, v: Arc<Envelope<BusEvent>>, now: std::time::Instant) {
        let (h, dup_pos) = match v.event.coalesce_key() {
            Some(k) => {
                let h = hash_tap_key(&k);
                let pos = self.overflow.iter().position(|(eh, ev)| {
                    *eh == Some(h) && ev.event.coalesce_key().as_ref() == Some(&k)
                });
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
                    pending = self.overflow.len(),
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

fn hash_tap_key(k: &(&'static str, Option<steve_types::WorkspaceId>, String)) -> u64 {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    k.hash(&mut h);
    h.finish()
}

struct Inner {
    generation: AtomicU64,
    global_generation: AtomicU64,
    ws_generations: Mutex<HashMap<String, u64>>,
    stats_generation: AtomicU64,
    tx: std::sync::mpsc::Sender<Arc<Envelope<BusEvent>>>,
    revision: Mutex<u64>,
    out: broadcast::Sender<Arc<Delivery>>,
    taps: Taps,
    log: Arc<Log>,
}
/// Publishing is synchronous under the store write lock; a dispatcher shares typed envelopes
/// with internal taps and the replay log; egress owns encoding. Revisions are independent of cursors.
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
    pub(crate) fn with_window(frames: usize, bytes: usize) -> Self {
        let (tx, rx) = std::sync::mpsc::channel();
        let out = broadcast::channel(Self::CAPACITY).0;
        let taps: Taps = Arc::default();
        let log = Arc::new(Log {
            epoch: steve_store::uid("ep"),
            ring: Mutex::new(Ring::new(frames, bytes)),
            head: AtomicU64::new(0),
        });
        let (o, t, l) = (out.clone(), taps.clone(), log.clone());
        if let Err(e) = std::thread::Builder::new()
            .name("steve-bus".into())
            .spawn(move || dispatch(rx, o, t, l))
        {
            tracing::error!("bus dispatcher: {e}");
        }
        Self(Arc::new(Inner {
            generation: AtomicU64::new(0),
            global_generation: AtomicU64::new(0),
            ws_generations: Mutex::new(HashMap::new()),
            stats_generation: AtomicU64::new(0),
            tx,
            revision: Mutex::new(0),
            out,
            taps,
            log,
        }))
    }
    /// Typed domain/live envelopes, in publish order, selected by a Rust predicate.
    pub fn tap(
        &self,
        filter: impl Fn(&BusEvent) -> bool + Send + Sync + 'static,
    ) -> tokio::sync::mpsc::Receiver<Arc<Envelope<BusEvent>>> {
        let (tx, rx) = tokio::sync::mpsc::channel(Self::TAP_CAPACITY);
        self.0
            .taps
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(TapSlot::new(Box::new(filter), tx));
        rx
    }
    pub fn publish(&self, event: impl Into<BusEvent>) {
        self.publish_batch(vec![event.into()]);
    }
    pub fn live(&self, event: LiveEvent) {
        self.publish(event);
    }
    /// One committed batch, still under the writer lock. A live-only batch uses the current rev.
    pub fn publish_batch(&self, events: Vec<BusEvent>) {
        let mut revision = self.0.revision.lock().unwrap_or_else(|e| e.into_inner());
        if events.iter().any(|e| matches!(e, BusEvent::Domain(_))) {
            *revision += 1;
        }
        for event in events {
            let stateless = matches!(
                &event,
                BusEvent::Domain(
                    DomainEvent::Stats(_)
                        | DomainEvent::Devices(steve_types::DevicesEvent::Client { .. })
                ) | BusEvent::Live(LiveEvent::Work(
                    steve_types::WorkLiveEvent::Event { .. }
                        | steve_types::WorkLiveEvent::SteveDraft { .. }
                        | steve_types::WorkLiveEvent::SteveTool { .. }
                ))
            );
            if !stateless {
                self.0.generation.fetch_add(1, Ordering::SeqCst);
                match event.scope() {
                    Scope::Ws(ws) => self.invalidate_workspace(ws.as_str()),
                    Scope::Hub | Scope::User(_) => {
                        self.0.global_generation.fetch_add(1, Ordering::SeqCst);
                    }
                }
            }
            if matches!(&event, BusEvent::Domain(DomainEvent::Stats(_))) {
                self.0.stats_generation.fetch_add(1, Ordering::SeqCst);
            }
            let _ = self.0.tx.send(Arc::new(Envelope {
                event,
                rev: Rev(*revision),
            }));
        }
    }
    fn invalidate_workspace(&self, ws: &str) {
        let mut map = self
            .0
            .ws_generations
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if let Some(g) = map.get_mut(ws) {
            *g = g.wrapping_add(1);
        } else {
            if map.len() >= WS_GENERATION_MAX {
                let max = map.values().copied().max().unwrap_or(0);
                self.0.global_generation.fetch_add(max, Ordering::SeqCst);
                map.clear();
            }
            map.insert(ws.into(), 1);
        }
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

    pub fn subscribe(&self) -> broadcast::Receiver<Arc<Delivery>> {
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
            .partition_point(|(f, _)| f.id.is_some_and(|id| id <= after.n));
        Replay::Frames {
            frames: ring.frames.range(from..).map(|(f, _)| f.clone()).collect(),
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
    rx: std::sync::mpsc::Receiver<Arc<Envelope<BusEvent>>>,
    out: broadcast::Sender<Arc<Delivery>>,
    taps: Taps,
    log: Arc<Log>,
) {
    let mut out = Sender {
        out,
        log,
        next_id: 0,
    };
    let mut stats = StatsThrottle::default();
    let mut tap_overflow = false;
    loop {
        let tap_due = tap_overflow.then(|| std::time::Instant::now() + TAP_DRAIN_INTERVAL);
        let deadline = match (stats.deadline(), tap_due) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        let next = match deadline {
            None => rx
                .recv()
                .map_err(|_| std::sync::mpsc::RecvTimeoutError::Disconnected),
            Some(at) => rx.recv_timeout(at.saturating_duration_since(std::time::Instant::now())),
        };
        let event = match next {
            Ok(event) => event,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                if tap_overflow {
                    let mut t = taps.lock().unwrap_or_else(|e| e.into_inner());
                    t.retain_mut(TapSlot::flush);
                    tap_overflow = t.iter().any(|s| !s.overflow.is_empty());
                }
                for frame in stats.due(std::time::Instant::now()) {
                    out.send(frame);
                }
                continue;
            }
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return,
        };
        let now = std::time::Instant::now();
        {
            let mut t = taps.lock().unwrap_or_else(|e| e.into_inner());
            t.retain_mut(|slot| {
                if slot.matches(&event.event) {
                    slot.deliver(event.clone(), now)
                } else {
                    slot.flush()
                }
            });
            tap_overflow = t.iter().any(|slot| !slot.overflow.is_empty());
        }
        if matches!(&event.event, BusEvent::Domain(DomainEvent::Stats(_))) {
            if let Some(frame) = stats.offer(event, now) {
                out.send(frame);
            }
        } else {
            out.send(event);
        }
    }
}
/// The dispatcher's end: numbers, logs and broadcasts frames. Only the dispatcher thread holds it, so ids
/// follow send order.
struct Sender {
    out: broadcast::Sender<Arc<Delivery>>,
    log: Arc<Log>,
    next_id: u64,
}

impl Sender {
    fn send(&mut self, event: Arc<Envelope<BusEvent>>) {
        let kept = !TRANSIENT.contains(&event.event.wire_type());
        let id = kept.then(|| {
            self.next_id += 1;
            self.next_id
        });
        let frame = Arc::new(Delivery {
            envelope: event,
            id,
            encoded: OnceLock::new(),
        });
        if let Some(id) = id {
            let bytes = retained_size(&frame.envelope);
            let mut ring = self.log.ring.lock().unwrap_or_else(|e| e.into_inner());
            ring.push(frame.clone(), bytes);
            self.log.head.store(id, Ordering::SeqCst);
        }
        let _ = self.out.send(frame);
    }
}

/// Latest statistics notice per workspace in each five-second window.
#[derive(Default)]
struct StatsThrottle {
    last: HashMap<String, std::time::Instant>,
    held: HashMap<String, Arc<Envelope<BusEvent>>>,
}
impl StatsThrottle {
    const WINDOW: std::time::Duration = std::time::Duration::from_secs(5);
    fn offer(
        &mut self,
        frame: Arc<Envelope<BusEvent>>,
        now: std::time::Instant,
    ) -> Option<Arc<Envelope<BusEvent>>> {
        let Scope::Ws(ws) = frame.scope() else {
            unreachable!("statistics are workspace scoped")
        };
        let ws = ws.to_string();
        match self.last.get(&ws) {
            Some(at) if now.duration_since(*at) < Self::WINDOW => {
                self.held.insert(ws, frame);
                None
            }
            _ => {
                if self.last.len() > 10_000 {
                    self.last
                        .retain(|_, at| now.duration_since(*at) < Self::WINDOW);
                }
                self.last.insert(ws, now);
                Some(frame)
            }
        }
    }
    fn deadline(&self) -> Option<std::time::Instant> {
        self.held
            .keys()
            .filter_map(|ws| self.last.get(ws))
            .map(|at| *at + Self::WINDOW)
            .min()
    }
    fn due(&mut self, now: std::time::Instant) -> Vec<Arc<Envelope<BusEvent>>> {
        let keys: Vec<_> = self
            .held
            .keys()
            .filter(|ws| {
                self.last
                    .get(*ws)
                    .is_none_or(|at| now.duration_since(*at) >= Self::WINDOW)
            })
            .cloned()
            .collect();
        keys.into_iter()
            .filter_map(|ws| {
                let frame = self.held.remove(&ws)?;
                self.last.insert(ws, now);
                Some(frame)
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::time::Duration;
    use steve_types::*;
    fn item(id: &str, ws: &str) -> BusEvent {
        WorkEvent::Item {
            id: id.into(),
            ws: ws.into(),
        }
        .into()
    }
    fn connection(id: &str, ws: &str) -> BusEvent {
        ChannelsEvent::Connection {
            id: id.into(),
            ws: ws.into(),
        }
        .into()
    }
    fn memory(id: &str, ws: &str) -> BusEvent {
        ConfigEvent::Mm {
            id: id.into(),
            ws: ws.into(),
        }
        .into()
    }
    fn stats(ws: &str, day: &str) -> BusEvent {
        StatsEvent::Stats {
            ws: ws.into(),
            day: Some(day.into()),
        }
        .into()
    }
    fn enveloped(e: BusEvent) -> Arc<Envelope<BusEvent>> {
        Arc::new(Envelope {
            event: e,
            rev: Rev(1),
        })
    }
    fn json(frame: &Delivery) -> Value {
        serde_json::to_value(&*frame.envelope).unwrap()
    }
    async fn next(rx: &mut broadcast::Receiver<Arc<Delivery>>) -> Arc<Delivery> {
        tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .unwrap()
            .unwrap()
    }
    async fn tapped(rx: &mut tokio::sync::mpsc::Receiver<Notification>) -> Notification {
        tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .unwrap()
            .unwrap()
    }
    fn frames(r: Replay) -> (Vec<Arc<Delivery>>, u64) {
        match r {
            Replay::Frames { frames, head } => (frames, head),
            Replay::Reset { head } => panic!("unexpected reset {head}"),
        }
    }
    fn reset(r: Replay) -> u64 {
        match r {
            Replay::Reset { head } => head,
            _ => panic!("expected reset"),
        }
    }
    fn ids(f: &[Arc<Delivery>]) -> Vec<u64> {
        f.iter().filter_map(|f| f.id).collect()
    }

    #[test]
    fn stats_table_preserves_leading_latest_trailing_and_boundary() {
        let mut t = StatsThrottle::default();
        let at = std::time::Instant::now();
        for (ms, day, immediate) in [(0, "1", true), (1000, "2", false), (4999, "3", false)] {
            assert_eq!(
                t.offer(encoded(stats("a", day)), at + Duration::from_millis(ms))
                    .is_some(),
                immediate
            );
        }
        assert!(t.due(at + Duration::from_millis(4999)).is_empty());
        let due = t.due(at + Duration::from_secs(5));
        assert_eq!(due.len(), 1);
        assert!(due[0].json.contains("\"day\":\"3\""));
        assert!(
            t.offer(encoded(stats("a", "4")), at + Duration::from_millis(5001))
                .is_none()
        );
        assert!(t.due(at + Duration::from_millis(9999)).is_empty());
        assert!(
            t.offer(encoded(stats("a", "5")), at + Duration::from_secs(10))
                .is_some()
        );
        // The old implementation still has a held frame at the exact offer boundary;
        // dispatcher due-before-offer ordering normally drains it first.
    }

    #[test]
    fn stats_are_coalesced_per_workspace() {
        let mut t = StatsThrottle::default();
        let at = std::time::Instant::now();
        let sec = Duration::from_secs(1);
        assert!(t.offer(enveloped(stats("a", "1")), at).is_some());
        assert!(t.offer(enveloped(stats("a", "2")), at + sec).is_none());
        assert!(t.offer(enveloped(stats("a", "3")), at + 2 * sec).is_none());
        assert!(t.offer(enveloped(stats("b", "1")), at + 2 * sec).is_some());
        assert_eq!(t.deadline(), Some(at + 5 * sec));
        assert!(t.due(at + 4 * sec).is_empty());
        let due = t.due(at + 5 * sec);
        assert_eq!(due.len(), 1);
        assert_eq!(serde_json::to_value(&*due[0]).unwrap()["day"], "3");
        assert_eq!(t.deadline(), None);
    }
    #[test]
    fn generation_for_isolates_workspaces_and_keeps_global_monotonicity() {
        let bus = Bus::new();
        bus.publish(item("a", "a"));
        assert_eq!((bus.generation_for("a"), bus.generation_for("b")), (1, 0));
        bus.publish(item("b", "b"));
        bus.publish(DomainEvent::from(DevicesEvent::Node {
            id: "n".into(),
            user: "u".into(),
        }));
        assert_eq!((bus.generation_for("a"), bus.generation_for("b")), (2, 2));
        let before = (bus.generation_for("a"), bus.generation_for("b"));
        for i in 0..WS_GENERATION_MAX + 50 {
            bus.publish(memory("m", &format!("w-{i}")));
        }
        assert!(bus.0.ws_generations.lock().unwrap().len() <= WS_GENERATION_MAX);
        assert!(bus.generation_for("a") >= before.0 && bus.generation_for("b") >= before.1);
        let b = bus.generation_for("b");
        let a = bus.generation_for("a");
        bus.publish(memory("m", "a"));
        assert_eq!(bus.generation_for("a"), a + 1);
        assert_eq!(bus.generation_for("b"), b);
    }
    #[tokio::test]
    async fn fan_out_in_order_with_scopes() {
        let bus = Bus::new();
        let mut rx = bus.subscribe();
        bus.publish(item("one", "a"));
        bus.publish(item("two", "b"));
        let first = next(&mut rx).await;
        let second = next(&mut rx).await;
        assert_eq!(json(&first)["id"], "one");
        assert_eq!(json(&second)["id"], "two");
        assert!(first.visible_to("a") && !first.visible_to("b"));
        assert!(second.visible_to("b") && !second.visible_to("a"));
        bus.publish(DomainEvent::from(DevicesEvent::Node {
            id: "n".into(),
            user: "u".into(),
        }));
        let personal = next(&mut rx).await;
        for (user, owner, want) in [
            ("u", false, true),
            ("other", false, false),
            ("owner", true, true),
            ("", false, false),
        ] {
            assert_eq!(
                personal.visible(&Viewer {
                    ws: "a".into(),
                    user: user.into(),
                    owner
                }),
                want
            );
        }
        bus.publish(DomainEvent::from(DevicesEvent::Device { id: "hub".into() }));
        let all = next(&mut rx).await;
        assert!(all.visible_to("a") && all.visible_to("b"));
    }
    #[tokio::test]
    async fn stats_frames_stay_in_their_workspace() {
        let bus = Bus::new();
        let mut rx = bus.subscribe();
        bus.publish(stats("a", "1"));
        bus.publish(stats("b", "2"));
        let a = next(&mut rx).await;
        let b = next(&mut rx).await;
        assert_eq!(json(&a)["ws"], "a");
        assert!(!a.visible_to("b"));
        assert_eq!(json(&b)["ws"], "b");
        assert!(!b.visible_to("a"));
    }
    #[tokio::test]
    async fn tap_overflow_coalesces_same_key_and_drains_without_new_publishes() {
        let bus = Bus::new();
        let mut tap = bus.tap(|e| {
            matches!(
                e.as_domain(),
                Some(DomainEvent::Channels(ChannelsEvent::Connection { .. }))
            )
        });
        let mut sse = bus.subscribe();
        for i in 0..TAP_CAPACITY {
            bus.publish(connection(&format!("c-{i}"), "a"));
            next(&mut sse).await;
        }
        for _ in 0..40 {
            bus.publish(connection("alpha", "a"));
        }
        for _ in 0..25 {
            bus.publish(connection("beta", "a"));
        }
        for _ in 0..15 {
            bus.publish(connection("alpha", "b"));
        }
        for _ in 0..80 {
            next(&mut sse).await;
        }
        assert_eq!(bus.0.taps.lock().unwrap()[0].overflow.len(), 3);
        for i in 0..TAP_CAPACITY {
            let e = tapped(&mut tap).await;
            assert!(
                matches!(e.event.as_domain(),Some(DomainEvent::Channels(ChannelsEvent::Connection {id,..})) if id.as_str()==format!("c-{i}"))
            );
        }
        let mut got = Vec::new();
        for _ in 0..3 {
            let e = tapped(&mut tap).await;
            if let Some(DomainEvent::Channels(ChannelsEvent::Connection { id, ws })) =
                e.event.as_domain()
            {
                got.push((id.to_string(), ws.to_string(), e.rev.0));
            }
        }
        assert_eq!(
            got,
            vec![
                ("alpha".into(), "a".into(), TAP_CAPACITY as u64 + 40),
                ("beta".into(), "a".into(), TAP_CAPACITY as u64 + 65),
                ("alpha".into(), "b".into(), TAP_CAPACITY as u64 + 80)
            ]
        );
    }
    #[tokio::test]
    async fn tap_flood_never_blocks_bounds_memory_evicts_oldest_and_drops_closed_tap() {
        let bus = Bus::new();
        let mut tap = bus.tap(|e| {
            matches!(
                e.as_domain(),
                Some(DomainEvent::Config(ConfigEvent::Mm { .. }))
            )
        });
        let mut sse = bus.subscribe();
        for i in 0..TAP_CAPACITY {
            bus.publish(memory(&format!("init-{i}"), "a"));
            next(&mut sse).await;
        }
        for i in 0..TAP_OVERFLOW_MAX + 200 {
            bus.publish(memory(&format!("overflow-{i}"), "a"));
            next(&mut sse).await;
        }
        assert_eq!(
            bus.0.taps.lock().unwrap()[0].overflow.len(),
            TAP_OVERFLOW_MAX
        );
        for _ in 0..TAP_CAPACITY {
            tapped(&mut tap).await;
        }
        for i in 200..TAP_OVERFLOW_MAX + 200 {
            let e = tapped(&mut tap).await;
            assert!(
                matches!(e.event.as_domain(),Some(DomainEvent::Config(ConfigEvent::Mm {id,..})) if id.as_str()==format!("overflow-{i}"))
            );
        }
        let abandoned = bus.tap(|_| true);
        for i in 0..TAP_CAPACITY + 1 {
            bus.publish(memory(&format!("later-{i}"), "a"));
            next(&mut sse).await;
        }
        drop(abandoned);
        tokio::time::timeout(Duration::from_secs(5), async {
            loop {
                if bus.0.taps.lock().unwrap().len() == 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }
    #[tokio::test]
    async fn transcript_overflow_keeps_distinct_chunks_and_mixed_event_order() {
        let bus = Bus::new();
        let mut tap = bus.tap(|_| true);
        let mut sse = bus.subscribe();
        for i in 0..TAP_CAPACITY {
            bus.publish(memory(&format!("m-{i}"), "a"));
            next(&mut sse).await;
        }
        for seq in 1..=20 {
            bus.live(
                WorkLiveEvent::Event {
                    session_id: "s".into(),
                    seq: Seq(seq),
                    kind: "text".into(),
                    data: RawJson::from_str("{}").unwrap(),
                    ws: "a".into(),
                }
                .into(),
            );
            next(&mut sse).await;
        }
        assert_eq!(bus.0.taps.lock().unwrap()[0].overflow.len(), 20);
        for _ in 0..TAP_CAPACITY {
            tapped(&mut tap).await;
        }
        for seq in 1..=20 {
            let e = tapped(&mut tap).await;
            assert!(
                matches!(e.event.as_live(),Some(LiveEvent::Work(WorkLiveEvent::Event {seq:actual,..})) if actual.0==seq)
            );
        }
        bus.publish(item("next", "a"));
        bus.live(
            WorkLiveEvent::SteveThinking {
                on: true,
                ws: "a".into(),
                phase: None,
            }
            .into(),
        );
        assert!(matches!(tapped(&mut tap).await.event, BusEvent::Domain(_)));
        assert!(matches!(tapped(&mut tap).await.event, BusEvent::Live(_)));
    }
    #[tokio::test]
    async fn ids_follow_send_order_even_for_held_stats() {
        let bus = Bus::new();
        let mut rx = bus.subscribe();
        let start = bus.cursor();
        bus.publish(stats("a", "1"));
        bus.publish(stats("a", "2"));
        bus.publish(item("later", "a"));
        let a = next(&mut rx).await;
        let b = next(&mut rx).await;
        let c = next(&mut rx).await;
        assert_eq!(json(&a)["day"], "1");
        assert_eq!(json(&b)["id"], "later");
        assert_eq!(json(&c)["day"], "2");
        assert!(a.id < b.id && b.id < c.id);
        let (log, head) = frames(bus.replay(&start));
        assert_eq!(ids(&log), vec![1, 2, 3]);
        assert_eq!(head, 3);
    }
    #[tokio::test]
    async fn transient_frames_are_not_kept() {
        let bus = Bus::new();
        let mut rx = bus.subscribe();
        let start = bus.cursor();
        bus.live(
            WorkLiveEvent::Event {
                session_id: "s".into(),
                seq: Seq(1),
                kind: "text".into(),
                data: RawJson::from_str("{}").unwrap(),
                ws: "a".into(),
            }
            .into(),
        );
        bus.live(
            WorkLiveEvent::SteveDraft {
                ws: "a".into(),
                mid: "m".into(),
                thought: "".into(),
                text: "draft".into(),
            }
            .into(),
        );
        bus.publish(item("kept", "a"));
        assert!(next(&mut rx).await.id.is_none());
        assert!(next(&mut rx).await.id.is_none());
        let kept = next(&mut rx).await;
        assert_eq!(kept.id, Some(start.n + 1));
        let (log, _) = frames(bus.replay(&start));
        assert_eq!(ids(&log), vec![start.n + 1]);
    }
    async fn publish_items(bus: &Bus, rx: &mut broadcast::Receiver<Arc<Delivery>>, n: usize) {
        for i in 0..n {
            bus.publish(item(&format!("ST-{i:03}"), "a"));
        }
        for _ in 0..n {
            next(rx).await;
        }
    }
    #[tokio::test]
    async fn eviction_by_count_resets_older_cursors() {
        let bus = Bus::with_window(3, usize::MAX);
        let mut rx = bus.subscribe();
        publish_items(&bus, &mut rx, 5).await;
        let at = |n| Cursor {
            epoch: bus.epoch().into(),
            n,
        };
        assert_eq!(ids(&frames(bus.replay(&at(2))).0), vec![3, 4, 5]);
        assert_eq!(reset(bus.replay(&at(1))), 5);
    }
    #[tokio::test]
    async fn eviction_by_bytes_resets_older_cursors() {
        let one = retained_size(&enveloped(item("ST-000", "a")));
        let bus = Bus::with_window(1000, 2 * one + 1);
        let mut rx = bus.subscribe();
        publish_items(&bus, &mut rx, 4).await;
        let at = |n| Cursor {
            epoch: bus.epoch().into(),
            n,
        };
        assert_eq!(ids(&frames(bus.replay(&at(2))).0), vec![3, 4]);
        assert_eq!(reset(bus.replay(&at(1))), 4);
        let bus = Bus::with_window(1000, 1);
        let mut rx = bus.subscribe();
        publish_items(&bus, &mut rx, 2).await;
        assert_eq!(
            reset(bus.replay(&Cursor {
                epoch: bus.epoch().into(),
                n: 0
            })),
            2
        );
    }
    #[tokio::test]
    async fn foreign_future_or_malformed_cursors_reset() {
        let bus = Bus::new();
        let mut rx = bus.subscribe();
        publish_items(&bus, &mut rx, 2).await;
        let other = Bus::new();
        assert_ne!(bus.epoch(), other.epoch());
        assert_eq!(reset(bus.replay(&other.cursor())), 2);
        assert_eq!(
            reset(bus.replay(&Cursor {
                epoch: bus.epoch().into(),
                n: 3
            })),
            2
        );
        for s in [
            "",
            "nonsense",
            ":1",
            "epoch:-1",
            "epoch:1x",
            "epoch:9999999999999999999999999",
        ] {
            assert!(Cursor::parse(s).is_none());
            assert_eq!(reset(bus.replay_from(s)), 2);
        }
        assert_eq!(Cursor::parse(&bus.cursor().to_string()), Some(bus.cursor()));
    }
    #[tokio::test]
    async fn replay_at_head_is_empty_and_cursor_tracks_each_kept_frame() {
        let bus = Bus::new();
        assert!(frames(bus.replay(&bus.cursor())).0.is_empty());
        let mut rx = bus.subscribe();
        for i in 1..=20 {
            bus.publish(item(&format!("{i}"), "a"));
            let e = next(&mut rx).await;
            assert_eq!(bus.cursor().n, e.id.unwrap());
            assert!(frames(bus.replay(&bus.cursor())).0.is_empty());
        }
    }
}
