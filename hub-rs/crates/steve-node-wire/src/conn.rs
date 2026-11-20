//! The multiplexer of one link: requests both ways on three lanes (control before events before
//! bulk, none starved), heartbeats, per-method deadlines and the generation stamp.
//!
//! The failure rule (the only place it lives): a request that never reached the socket fails
//! `offline`; one that did and then lost its link or its deadline fails `outcome_unknown` when it
//! carries an operation, `offline` when it is a pure read. Nothing is ever sent twice from here.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use steve_node_api::upgrade_v1 as upgrade;
use steve_node_api::{ApiError, ApiResult, ErrorCode, OperationId};
use tokio::sync::{Notify, Semaphore, mpsc, oneshot, watch};
use tokio::time::Instant;

use crate::envelope::{Envelope, Frame, FrameSink, FrameStream, Kind, Outcome};
use crate::methods::{self, Lane};

/// What answers the other side's requests.
#[async_trait]
pub trait Handler: Send + Sync + 'static {
    async fn handle(&self, method: &str, p: Value) -> ApiResult<Value>;
    /// Permanent upgrade requests are decoded independently of the ordinary payload/error types.
    async fn handle_upgrade(&self, _method: &str, _p: Value) -> upgrade::Result<Value> {
        Err(upgrade::Error::incompatible())
    }
    /// A control message other than a heartbeat (`paused`, `fenced`…).
    fn control(&self, _m: &str, _p: &Value) {}
}

#[derive(Debug, Clone)]
pub struct ConnOptions {
    pub heartbeat: Duration,
    /// Closed with [`crate::close::DEAD`] after this long without any frame from the peer.
    pub dead_after: Duration,
    /// Prefix of request ids (`h` on the Hub, `n` on the Node), for logs.
    pub side: &'static str,
    /// The authenticated handshake's decision, never a negotiated application version.
    pub mode: upgrade::Mode,
}

impl Default for ConnOptions {
    fn default() -> Self {
        ConnOptions {
            heartbeat: Duration::from_secs(10),
            dead_after: Duration::from_secs(30),
            side: "x",
            mode: upgrade::Mode::Normal,
        }
    }
}

/// Requests of the peer handled at once on one link. Past it a request is answered `exhausted`
/// without reaching the handler (nothing done: the sender may try again), so a peer that pipelines
/// faster than it is served cannot pile up tasks and payloads. Refusing instead of pausing the
/// reader keeps answers to this side's own calls flowing (a handler may be waiting on one).
pub const MAX_INBOUND: usize = 64;
/// The bytes of the peer's requests handled at once on one link, past which a request is
/// answered `exhausted` as past [`MAX_INBOUND`]: a peer of big requests cannot pin more.
pub const MAX_INBOUND_BYTES: usize = 16 << 20;
/// The bytes waiting to be written per lane (control, events, bulk). A request waits for room
/// within its deadline; a control message or a reader's own answer that finds none is dropped,
/// as when its lane is full.
pub const LANE_BYTES: [usize; 3] = [4 << 20, 2 << 20, 2 << 20];
/// After this many frames of a higher lane in a row, one frame of a lower lane that waits goes
/// first: control before events before bulk, but neither of the lower ones starves.
pub const FAIR: u32 = 4;

const QUEUED: u8 = 0;
const WRITING: u8 = 1;
const ABANDONED: u8 = 2;

struct Out {
    text: String,
    /// For requests: QUEUED until the writer takes it (WRITING), or the caller gave up (ABANDONED).
    state: Option<Arc<AtomicU8>>,
    /// Its room in the lane's byte budget, given back once it is written (or dropped).
    _room: Option<tokio::sync::OwnedSemaphorePermit>,
}

/// One lane's queue: frames in order, within a count and a byte budget.
struct LaneQ {
    tx: mpsc::Sender<Out>,
    room: Arc<Semaphore>,
    /// The whole budget, in KiB: a frame takes at most all of it, so it always fits an empty lane.
    cap: u32,
}

/// A frame's share of a lane's byte budget, in KiB.
fn kib(n: usize) -> u32 {
    n.div_ceil(1024).max(1) as u32
}

impl LaneQ {
    fn new(frames: usize, bytes: usize) -> (LaneQ, mpsc::Receiver<Out>) {
        let (tx, rx) = mpsc::channel(frames);
        let cap = (bytes / 1024) as u32;
        let room = Arc::new(Semaphore::new(cap as usize));
        (LaneQ { tx, room, cap }, rx)
    }

    /// Queues now or not at all.
    fn try_send(&self, text: String, state: Option<Arc<AtomicU8>>) -> bool {
        let need = kib(text.len()).min(self.cap);
        let Ok(room) = self.room.clone().try_acquire_many_owned(need) else {
            return false;
        };
        self.tx
            .try_send(Out {
                text,
                state,
                _room: Some(room),
            })
            .is_ok()
    }

    /// Queues once there is room (count and bytes); fails only once the lane is gone.
    async fn send(&self, text: String, state: Option<Arc<AtomicU8>>) -> bool {
        let need = kib(text.len()).min(self.cap);
        let Ok(room) = self.room.clone().acquire_many_owned(need).await else {
            return false;
        };
        self.tx
            .send(Out {
                text,
                state,
                _room: Some(room),
            })
            .await
            .is_ok()
    }
}

/// The payload/error contract selected once, before dispatch or queueing.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Schema {
    Ordinary,
    V1,
}

impl Schema {
    fn version(self) -> u32 {
        match self {
            Self::Ordinary => steve_node_api::PROTOCOL,
            Self::V1 => upgrade::UPGRADE_PROTOCOL,
        }
    }

    fn encode(self, env: Envelope) -> ApiResult<String> {
        match self {
            Self::Ordinary => env.encode(),
            Self::V1 => upgrade::Frame {
                v: env.v,
                t: match env.t {
                    Kind::Req => upgrade::Kind::Req,
                    Kind::Res => upgrade::Kind::Res,
                    Kind::Ctl => upgrade::Kind::Ctl,
                    Kind::Unknown => upgrade::Kind::Unknown,
                },
                id: env.id,
                re: env.re,
                m: env.m,
                generation: env.generation,
                p: env.p,
            }
            .encode()
            .map_err(ApiError::from),
        }
    }

    fn outcome(self, outcome: Outcome) -> Value {
        match self {
            Self::Ordinary => serde_json::to_value(outcome).unwrap(),
            Self::V1 => serde_json::to_value(match outcome {
                Outcome::Ok(value) => upgrade::Outcome::Ok(value),
                Outcome::Err(error) => upgrade::Outcome::Err(error.into()),
            })
            .unwrap(),
        }
    }
}

enum Route {
    Ordinary,
    V1,
    Incompatible,
    Unsupported,
}

/// A mismatched peer's ordinary requests never touch a mutable payload decoder.
fn route(mode: upgrade::Mode, env: &Envelope) -> Route {
    if !(1..=9).contains(&env.v) && env.v != steve_node_api::PROTOCOL {
        return if env.t == Kind::Req {
            Route::Incompatible
        } else {
            Route::Unsupported
        };
    }
    if env.t == Kind::Ctl
        && matches!(env.m.as_str(), upgrade::HEARTBEAT | upgrade::FENCED)
        && env.v != upgrade::UPGRADE_PROTOCOL
    {
        return Route::Unsupported;
    }
    match (env.v, env.t) {
        (upgrade::UPGRADE_PROTOCOL, Kind::Req) if !upgrade::is_method(&env.m) => {
            Route::Incompatible
        }
        (upgrade::UPGRADE_PROTOCOL, Kind::Unknown) => Route::Incompatible,
        (upgrade::UPGRADE_PROTOCOL, _) => Route::V1,
        (_, Kind::Req | Kind::Ctl | Kind::Unknown) if mode == upgrade::Mode::Upgrade => {
            Route::Incompatible
        }
        (steve_node_api::PROTOCOL, _) if mode == upgrade::Mode::Normal => Route::Ordinary,
        _ => Route::Unsupported,
    }
}

/// Inspect the permanent integer discriminator before selecting an independent schema.
fn decode_frame(text: &str) -> Option<Envelope> {
    match crate::envelope::version(text)? {
        1..=9 => {
            let frame = upgrade::Frame::decode(text)?;
            if frame.t == upgrade::Kind::Ctl {
                match frame.m.as_str() {
                    upgrade::HEARTBEAT => {
                        serde_json::from_value::<upgrade::Heartbeat>(frame.p.clone()).ok()?;
                    }
                    upgrade::FENCED => {
                        serde_json::from_value::<upgrade::Fenced>(frame.p.clone()).ok()?;
                    }
                    _ => {}
                }
            }
            Some(Envelope {
                v: frame.v,
                t: match frame.t {
                    upgrade::Kind::Req => Kind::Req,
                    upgrade::Kind::Res => Kind::Res,
                    upgrade::Kind::Ctl => Kind::Ctl,
                    upgrade::Kind::Unknown => Kind::Unknown,
                },
                id: frame.id,
                re: frame.re,
                m: frame.m,
                generation: frame.generation,
                trace: None,
                deadline_at: 0,
                p: frame.p,
            })
        }
        v if v == steve_node_api::PROTOCOL => Envelope::decode(text),
        v => {
            // No ordinary header field other than integer v is permanent. Read only enough
            // to refuse a foreign request; its other fields belong to an unknown schema.
            let foreign: Value = serde_json::from_str(text).ok()?;
            let id = foreign.get("id").and_then(Value::as_str);
            Some(Envelope {
                v,
                t: if foreign.get("t").and_then(Value::as_str) == Some("req") && id.is_some() {
                    Kind::Req
                } else {
                    Kind::Unknown
                },
                id: id.unwrap_or_default().into(),
                re: None,
                m: String::new(),
                generation: 0,
                trace: None,
                deadline_at: 0,
                p: Value::Null,
            })
        }
    }
}

struct Pending {
    tx: oneshot::Sender<Value>,
    schema: Schema,
}

struct CloseReq {
    pre: Option<String>,
    code: u16,
    reason: String,
}

struct Shared {
    generation: u64,
    side: &'static str,
    mode: upgrade::Mode,
    ctl: LaneQ,
    events: LaneQ,
    bulk: LaneQ,
    pending: Mutex<HashMap<String, Pending>>,
    next: AtomicU64,
    closing: AtomicBool,
    close_req: Mutex<Option<CloseReq>>,
    close_wake: Notify,
    closed: watch::Sender<Option<(u16, String)>>,
    /// Frames this side wrote (tests count them).
    written: AtomicU64,
}

pub struct Conn {
    s: Arc<Shared>,
}

impl Conn {
    /// Starts the link's reader, writer and heartbeat. `generation` is the one the Hub's welcome
    /// gave: every request carries it, and a request carrying another one is refused `stale`
    /// without reaching `handler`.
    pub fn spawn(
        sink: FrameSink,
        stream: FrameStream,
        generation: u64,
        handler: Arc<dyn Handler>,
        opts: ConnOptions,
    ) -> Arc<Conn> {
        // This Conn has only the v1 upgrade codec. When adding v2, retain Hello's selected
        // upgrade version here and use it for both encoding and dispatch instead of this default.
        debug_assert_eq!(
            upgrade::UPGRADE_PROTOCOL,
            1,
            "Conn only implements upgrade v1"
        );
        let (ctl, ctl_rx) = LaneQ::new(256, LANE_BYTES[0]);
        let (events, events_rx) = LaneQ::new(16, LANE_BYTES[1]);
        let (bulk, bulk_rx) = LaneQ::new(16, LANE_BYTES[2]);
        let s = Arc::new(Shared {
            generation,
            side: opts.side,
            mode: opts.mode,
            ctl,
            events,
            bulk,
            pending: Mutex::new(HashMap::new()),
            next: AtomicU64::new(1),
            closing: AtomicBool::new(false),
            close_req: Mutex::new(None),
            close_wake: Notify::new(),
            closed: watch::channel(None).0,
            written: AtomicU64::new(0),
        });
        tokio::spawn(writer(
            s.clone(),
            sink,
            [ctl_rx, events_rx, bulk_rx],
            opts.dead_after,
        ));
        tokio::spawn(reader(s.clone(), stream, handler, opts.clone()));
        tokio::spawn(heartbeat(Arc::downgrade(&s), opts.heartbeat));
        Arc::new(Conn { s })
    }

    pub fn generation(&self) -> u64 {
        self.s.generation
    }

    pub fn is_upgrade(&self) -> bool {
        self.s.mode == upgrade::Mode::Upgrade
    }

    pub fn mode(&self) -> upgrade::Mode {
        self.s.mode
    }

    pub fn is_closed(&self) -> bool {
        self.s.closing.load(Ordering::SeqCst)
    }

    pub fn frames_written(&self) -> u64 {
        self.s.written.load(Ordering::SeqCst)
    }

    /// Resolves once the link is down, with its close code and reason.
    pub async fn closed(&self) -> (u16, String) {
        let mut rx = self.s.closed.subscribe();
        loop {
            if let Some(c) = rx.borrow_and_update().clone() {
                return c;
            }
            if rx.changed().await.is_err() {
                return (crate::close::GOODBYE, "gone".into());
            }
        }
    }

    /// Ends the link: what is queued and not yet written is dropped (those calls fail `offline`),
    /// what is in flight resolves by the written rule. `pre` is one last control message
    /// (`fenced`) written before the close frame.
    pub fn close(&self, pre: Option<(&str, Value)>, code: u16, reason: &str) {
        let pre = pre.and_then(|(m, p)| self.s.control(m, p).ok());
        shut(&self.s, pre, code, reason);
    }

    /// A control message (not a request: nobody answers it).
    pub fn notify(&self, m: &str, p: Value) {
        if self.is_upgrade() && !matches!(m, upgrade::HEARTBEAT | upgrade::FENCED) {
            return;
        }
        if let Ok(text) = self.s.control(m, p) {
            let _ = self.s.ctl.try_send(text, None);
        }
    }

    /// Calls `method` with the table's lane and deadline.
    pub async fn call(&self, method: &str, p: Value, op: Option<&OperationId>) -> ApiResult<Value> {
        if self.is_upgrade() {
            return Err(ApiError::new(
                ErrorCode::Offline,
                "ordinary protocol unavailable while upgrading",
            ));
        }
        let spec = methods::spec(method)
            .ok_or_else(|| ApiError::new(ErrorCode::Internal, format!("no method {method}")))?;
        self.call_with(method, p, spec.lane, spec.class.deadline(), op)
            .await
    }

    pub async fn call_with(
        &self,
        method: &str,
        p: Value,
        lane: Lane,
        deadline: Duration,
        op: Option<&OperationId>,
    ) -> ApiResult<Value> {
        if self.is_upgrade() {
            return Err(ApiError::new(
                ErrorCode::Offline,
                "ordinary protocol unavailable while upgrading",
            ));
        }
        let raw = self
            .call_raw(method, p, lane, deadline, op, Schema::Ordinary)
            .await?;
        let outcome: Outcome = serde_json::from_value(raw)
            .map_err(|e| ApiError::new(ErrorCode::Internal, format!("bad result: {e}")))?;
        match outcome {
            Outcome::Ok(value) => Ok(value),
            Outcome::Err(mut error) => {
                error.safe_detail = peer_text(&error.safe_detail);
                Err(error)
            }
        }
    }

    /// The five permanent methods always use v1 frames and v1 errors, even on a normal link.
    pub async fn call_upgrade(
        &self,
        method: &str,
        p: Value,
        op: Option<&OperationId>,
    ) -> ApiResult<Value> {
        if !upgrade::is_method(method) {
            return Err(upgrade::Error::incompatible().into());
        }
        let (lane, budget) = if method == upgrade::UPGRADE_FETCH {
            (Lane::Bulk, Duration::from_secs(60))
        } else {
            (Lane::Control, Duration::from_secs(60))
        };
        let raw = self
            .call_raw(method, p, lane, budget, op, Schema::V1)
            .await?;
        let outcome: upgrade::Outcome = answer_of(method, raw, op)?;
        match outcome {
            upgrade::Outcome::Ok(value) => Ok(value),
            upgrade::Outcome::Err(mut error) => {
                error.detail = peer_text(&error.detail);
                Err(error.into())
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn call_raw(
        &self,
        method: &str,
        p: Value,
        lane: Lane,
        deadline: Duration,
        op: Option<&OperationId>,
        schema: Schema,
    ) -> ApiResult<Value> {
        let offline = |why: &str| ApiError::new(ErrorCode::Offline, format!("{method}: {why}"));
        if self.is_closed() {
            return Err(offline("not connected"));
        }
        let id = self.s.id();
        let env = Envelope {
            v: schema.version(),
            t: Kind::Req,
            id: id.clone(),
            re: None,
            m: method.into(),
            generation: self.s.generation,
            trace: None,
            deadline_at: crate::now_ms() + deadline.as_millis() as i64,
            p,
        };
        let text = schema.encode(env)?;
        let state = Arc::new(AtomicU8::new(QUEUED));
        let (tx, rx) = oneshot::channel();
        self.s
            .pending
            .lock()
            .unwrap()
            .insert(id.clone(), Pending { tx, schema });
        let lane_q = self.s.lane(lane);
        let until = Instant::now() + deadline;
        let lost = |why: &str| {
            // the writer may take it right now: whoever wins the swap decides
            let never_written = state
                .compare_exchange(QUEUED, ABANDONED, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok();
            match op {
                Some(op) if !never_written => {
                    ApiError::uncertain(op, format!("{method}: {why} after it was sent"))
                }
                _ => offline(why),
            }
        };
        let queued = tokio::time::timeout_at(until, lane_q.send(text, Some(state.clone()))).await;
        if !matches!(queued, Ok(true)) {
            self.s.pending.lock().unwrap().remove(&id);
            return Err(lost("could not queue"));
        }
        let got = tokio::time::timeout_at(until, rx).await;
        self.s.pending.lock().unwrap().remove(&id);
        match got {
            Ok(Ok(value)) => Ok(value),
            Ok(Err(_)) => Err(lost("link lost")),
            Err(_) => Err(lost("deadline passed")),
        }
    }
}

impl Drop for Conn {
    fn drop(&mut self) {
        shut(&self.s, None, crate::close::GOODBYE, "dropped");
    }
}

impl Shared {
    fn control(&self, method: &str, payload: Value) -> ApiResult<String> {
        let schema = if matches!(method, upgrade::HEARTBEAT | upgrade::FENCED) {
            Schema::V1
        } else {
            Schema::Ordinary
        };
        let mut env = Envelope::ctl(self.id(), method, self.generation, payload);
        env.v = schema.version();
        schema.encode(env)
    }
    fn id(&self) -> String {
        format!("{}_{}", self.side, self.next.fetch_add(1, Ordering::SeqCst))
    }

    fn lane(&self, lane: Lane) -> &LaneQ {
        match lane {
            Lane::Control => &self.ctl,
            Lane::Events => &self.events,
            Lane::Bulk => &self.bulk,
        }
    }

    /// Answers from the reader itself (stale, unknown kind): never waits, so a full lane drops
    /// the answer and its sender meets its deadline.
    fn respond_now(&self, re: String, outcome: Outcome, generation: u64, schema: Schema) {
        if let Some(text) =
            self.response_payload(re, "", schema.outcome(outcome), generation, schema)
        {
            let _ = self.ctl.try_send(text, None);
        }
    }

    fn incompatible(&self, re: String, method: &str, generation: u64) {
        if let Some(text) = self.response_upgrade(
            re,
            method,
            upgrade::Outcome::Err(upgrade::Error::incompatible()),
            generation,
        ) {
            let _ = self.ctl.try_send(text, None);
        }
    }

    /// `generation`: a refusal of a stale request carries the request's own, so that its sender
    /// hears `stale` instead of waiting out its deadline.
    /// `method`: the request's, so that an answer too large to send says which call it was.
    fn response(
        &self,
        re: String,
        method: &str,
        outcome: Outcome,
        generation: u64,
    ) -> Option<String> {
        let p = serde_json::to_value(&outcome).unwrap_or(Value::Null);
        self.response_payload(re, method, p, generation, Schema::Ordinary)
    }

    fn response_upgrade(
        &self,
        re: String,
        method: &str,
        outcome: upgrade::Outcome,
        generation: u64,
    ) -> Option<String> {
        let p = serde_json::to_value(outcome).ok()?;
        self.response_payload(re, method, p, generation, Schema::V1)
    }

    fn response_payload(
        &self,
        re: String,
        method: &str,
        p: Value,
        generation: u64,
        schema: Schema,
    ) -> Option<String> {
        let env = Envelope {
            v: schema.version(),
            t: Kind::Res,
            id: self.id(),
            re: Some(re),
            m: String::new(),
            generation,
            trace: None,
            deadline_at: 0,
            p,
        };
        let text = match schema.encode(env.clone()) {
            Ok(t) => t,
            Err(mut e) => {
                // too large to answer: answer that instead of breaking the link
                if !method.is_empty() {
                    e.safe_detail =
                        e.safe_detail
                            .replacen("request ", &format!("{method} request "), 1);
                }
                let mut env = env;
                env.p = schema.outcome(Outcome::Err(e));
                schema.encode(env).ok()?
            }
        };
        Some(text)
    }
}

fn shut(s: &Arc<Shared>, pre: Option<String>, code: u16, reason: &str) {
    if s.closing.swap(true, Ordering::SeqCst) {
        return;
    }
    *s.close_req.lock().unwrap() = Some(CloseReq {
        pre,
        code,
        reason: reason.into(),
    });
    s.close_wake.notify_one();
    // dropping the senders wakes every waiting caller, which then applies the written rule
    s.pending.lock().unwrap().clear();
    s.closed.send_replace(Some((code, reason.into())));
}

/// Writes one frame at a time: control first, then events, then bulk, except that after
/// [`FAIR`] frames of higher lanes in a row a lower lane that has one waiting goes once (a
/// download never holds a permission card back, and a busy control lane never stops the rest).
/// `stuck`: how long one write may wait on a peer that does not read (its TCP window full while it
/// still sends heartbeats) before the link is given up, socket and task with it (L1).
async fn writer(
    s: Arc<Shared>,
    mut sink: FrameSink,
    mut lanes: [mpsc::Receiver<Out>; 3],
    stuck: Duration,
) {
    // frames of higher lanes written since each lower lane last had its turn
    let mut since = [0u32; 3];
    let mut stuck_out = false;
    loop {
        let mut out = None;
        // a lower lane that waited long enough (events before bulk)
        for lane in [1, 2] {
            if since[lane] >= FAIR
                && let Ok(o) = lanes[lane].try_recv()
            {
                out = Some((lane, o));
                break;
            }
        }
        let (lane, out) = match out {
            Some(x) => x,
            None => {
                let [ctl, events, bulk] = &mut lanes;
                tokio::select! {
                    biased;
                    _ = s.close_wake.notified() => break,
                    Some(o) = ctl.recv() => (0, o),
                    Some(o) = events.recv() => (1, o),
                    Some(o) = bulk.recv() => (2, o),
                    else => break,
                }
            }
        };
        if s.closing.load(Ordering::SeqCst) {
            break;
        }
        if let Some(st) = &out.state
            && st
                .compare_exchange(QUEUED, WRITING, Ordering::SeqCst, Ordering::SeqCst)
                .is_err()
        {
            continue;
        }
        since[lane] = 0;
        for lower in since.iter_mut().skip(lane + 1) {
            *lower = lower.saturating_add(1);
        }
        let Out { text, _room, .. } = out;
        match tokio::time::timeout(stuck, sink.send(Frame::Text(text))).await {
            Ok(Ok(())) => {}
            Ok(Err(_)) => {
                shut(&s, None, crate::close::GOODBYE, "write failed");
                break;
            }
            Err(_) => {
                shut(&s, None, crate::close::DEAD, "the peer stopped reading");
                stuck_out = true;
                break;
            }
        }
        drop(_room);
        s.written.fetch_add(1, Ordering::SeqCst);
    }
    // whoever still waits for room hears the link is gone
    for q in [&s.ctl, &s.events, &s.bulk] {
        q.room.close();
    }
    if stuck_out {
        // a close frame would wait on the same full window: the socket just goes
        return;
    }
    let req = s.close_req.lock().unwrap().take();
    if let Some(r) = req {
        if let Some(pre) = r.pre {
            let _ = sink.send(Frame::Text(pre)).await;
        }
        let _ = sink.send(Frame::Close(r.code, r.reason)).await;
    }
    let _ = sink.close().await;
}

async fn reader(
    s: Arc<Shared>,
    mut stream: FrameStream,
    handler: Arc<dyn Handler>,
    opts: ConnOptions,
) {
    let mut closed = s.closed.subscribe();
    let inflight = Arc::new(Semaphore::new(MAX_INBOUND));
    let inflight_bytes = Arc::new(Semaphore::new(MAX_INBOUND_BYTES / 1024));
    loop {
        let next = tokio::select! {
            n = tokio::time::timeout(opts.dead_after, stream.next()) => n,
            _ = closed.wait_for(|c| c.is_some()) => return,
        };
        let frame = match next {
            Err(_) => {
                shut(
                    &s,
                    None,
                    crate::close::DEAD,
                    "silent past the heartbeat budget",
                );
                return;
            }
            Ok(None) | Ok(Some(Err(_))) => {
                shut(&s, None, crate::close::GOODBYE, "link closed");
                return;
            }
            Ok(Some(Ok(Frame::Close(code, reason)))) => {
                shut(&s, None, code, &peer_text(&reason));
                return;
            }
            Ok(Some(Ok(Frame::Text(t)))) => t,
        };
        let Some(env) = decode_frame(&frame) else {
            shut(&s, None, crate::close::MALFORMED, "malformed envelope");
            return;
        };
        let schema = match route(s.mode, &env) {
            Route::Ordinary => Schema::Ordinary,
            Route::V1 => Schema::V1,
            Route::Incompatible => {
                s.incompatible(env.id, &env.m, s.generation);
                continue;
            }
            Route::Unsupported => {
                shut(&s, None, crate::close::UNSUPPORTED, "envelope version");
                return;
            }
        };
        match env.t {
            Kind::Res => {
                // a late answer of another generation never reaches a caller
                if env.generation != s.generation {
                    continue;
                }
                let Some(re) = env.re else { continue };
                if let Some(p) = s.pending.lock().unwrap().remove(&re)
                    && p.schema == schema
                {
                    let _ = p.tx.send(env.p);
                }
            }
            Kind::Req => {
                if env.generation != s.generation {
                    if schema == Schema::V1 {
                        let pre = s
                            .control(
                                upgrade::FENCED,
                                serde_json::to_value(upgrade::Fenced {
                                    generation: s.generation,
                                })
                                .unwrap(),
                            )
                            .ok();
                        shut(&s, pre, crate::close::FENCED, "another connection");
                        return;
                    }
                    s.respond_now(
                        env.id,
                        Outcome::Err(ApiError::new(ErrorCode::Stale, "another connection")),
                        env.generation,
                        schema,
                    );
                    continue;
                }
                let permit = inflight.clone().try_acquire_owned().ok().and_then(|n| {
                    inflight_bytes
                        .clone()
                        .try_acquire_many_owned(kib(frame.len()))
                        .ok()
                        .map(|b| (n, b))
                });
                let Some(permit) = permit else {
                    s.respond_now(
                        env.id,
                        Outcome::Err(ApiError::new(
                            ErrorCode::Exhausted,
                            "too many requests in flight on this link",
                        )),
                        s.generation,
                        schema,
                    );
                    continue;
                };
                drop(frame);
                // the answer goes on its request's lane: a file chunk never sits in the
                // control lane in front of a permission answer
                let lane = if env.m == upgrade::UPGRADE_FETCH {
                    Lane::Bulk
                } else {
                    methods::spec(&env.m).map_or(Lane::Control, |m| m.lane)
                };
                let (h, s2) = (handler.clone(), s.clone());
                // the work outlives the link: a port never ties what it started to its caller
                tokio::spawn(async move {
                    if schema == Schema::V1 {
                        let outcome = match h.handle_upgrade(&env.m, env.p).await {
                            Ok(value) => upgrade::Outcome::Ok(value),
                            Err(error) => upgrade::Outcome::Err(error),
                        };
                        if let Some(text) =
                            s2.response_upgrade(env.id, &env.m, outcome, s2.generation)
                        {
                            s2.lane(lane).send(text, None).await;
                        }
                        drop(permit);
                        return;
                    }
                    let outcome = match h.handle(&env.m, env.p).await {
                        Ok(v) => Outcome::Ok(v),
                        Err(e) => Outcome::Err(e),
                    };
                    // the slot is held until the answer is queued: a slow reader of answers
                    // slows the peer's new requests too
                    if let Some(text) = s2.response(env.id, &env.m, outcome, s2.generation) {
                        s2.lane(lane).send(text, None).await;
                    }
                    drop(permit);
                });
            }
            Kind::Ctl => match env.m.as_str() {
                upgrade::HEARTBEAT => {}
                upgrade::FENCED => {
                    if s.mode == upgrade::Mode::Normal {
                        handler.control(&env.m, &env.p);
                    }
                    shut(&s, None, crate::close::FENCED, upgrade::FENCED);
                    return;
                }
                m if schema == Schema::Ordinary && s.mode == upgrade::Mode::Normal => {
                    handler.control(m, &env.p)
                }
                _ => {}
            },
            Kind::Unknown => s.respond_now(
                env.id,
                Outcome::Err(ApiError::new(ErrorCode::Unsupported, "unknown frame kind")),
                s.generation,
                schema,
            ),
        }
    }
}

async fn heartbeat(s: std::sync::Weak<Shared>, every: Duration) {
    let mut tick = tokio::time::interval(every);
    tick.tick().await;
    loop {
        tick.tick().await;
        let Some(s) = s.upgrade() else { return };
        if s.closing.load(Ordering::SeqCst) {
            return;
        }
        let env = s.control(
            upgrade::HEARTBEAT,
            serde_json::to_value(upgrade::Heartbeat {}).unwrap(),
        );
        if let Ok(text) = env {
            let _ = s.ctl.try_send(text, None);
        }
    }
}

/// Unknown methods answer `unsupported`; the link stays up.
/// Text the peer chose (a close reason, an error's detail), made safe to log and show: control
/// characters (a newline would forge a log line) become spaces, and it is cut at 500 characters.
pub fn peer_text(s: &str) -> String {
    let mut out: String = s
        .chars()
        .map(|c| if c.is_control() { ' ' } else { c })
        .take(500)
        .collect();
    if s.chars().count() > 500 {
        out.push('…');
    }
    out
}

pub fn unsupported(method: &str) -> ApiError {
    ApiError::new(ErrorCode::Unsupported, format!("unknown method {method}"))
}

pub(crate) fn to_value<T: serde::Serialize>(v: &T) -> ApiResult<Value> {
    serde_json::to_value(v).map_err(|e| ApiError::new(ErrorCode::Internal, format!("encode: {e}")))
}

/// Decodes a request. A kind this end does not know (a newer peer's variant) is `unsupported`,
/// like an unknown method: nothing ran, the peer may do without it. Any other bad payload is
/// `invalid`. Never for answers: see [`answer_of`].
pub fn from_value<T: serde::de::DeserializeOwned>(v: Value) -> ApiResult<T> {
    serde_json::from_value(v).map_err(|e| {
        let code = if e.to_string().starts_with("unknown variant") {
            ErrorCode::Unsupported
        } else {
            ErrorCode::Invalid
        };
        ApiError::new(code, format!("decode: {e}"))
    })
}

/// Decodes the answer to this end's own call. An answer it cannot read (a newer peer's variant, a
/// bad payload) is never `unsupported`, which would say nothing ran: the peer did answer, so a
/// command (`op`) may have taken effect and is `outcome_unknown` on its operation (ask, do not
/// redo or route elsewhere); a pure read is `invalid`.
pub fn answer_of<T: serde::de::DeserializeOwned>(
    method: &str,
    v: Value,
    op: Option<&OperationId>,
) -> ApiResult<T> {
    serde_json::from_value(v).map_err(|e| match op {
        Some(op) => ApiError::uncertain(op, format!("{method}: unreadable answer: {e}")),
        None => ApiError::new(
            ErrorCode::Invalid,
            format!("{method}: unreadable answer: {e}"),
        ),
    })
}

#[cfg(test)]
mod schema_tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn integer_version_selects_independent_frame_decoders() {
        let ordinary = Envelope::ctl("test", "paused", 7, json!({"paused":true}));
        let text = ordinary.encode().unwrap();
        assert!(decode_frame(&text).is_some());
        assert!(
            upgrade::Frame::decode(&text).is_none(),
            "ordinary-only fields cannot enter v1"
        );
        let mut changed: Value = serde_json::from_str(&text).unwrap();
        changed["v"] = json!(1);
        assert!(decode_frame(&changed.to_string()).is_none());
        for version in [json!("1"), json!(1.5), json!(-1), Value::Null] {
            changed["v"] = version;
            assert!(decode_frame(&changed.to_string()).is_none());
        }
        let fixed = upgrade::Frame::ctl("beat", upgrade::HEARTBEAT, 7, json!({}));
        assert!(decode_frame(&fixed.encode().unwrap()).is_some());
        let mut bad = fixed.clone();
        bad.p = json!({"at":123});
        assert!(decode_frame(&bad.encode().unwrap()).is_none());
        let mut ordinary_heartbeat = ordinary;
        ordinary_heartbeat.m = upgrade::HEARTBEAT.into();
        assert!(matches!(
            route(upgrade::Mode::Normal, &ordinary_heartbeat),
            Route::Unsupported
        ));
        assert!(matches!(
            route(upgrade::Mode::Upgrade, &ordinary_heartbeat),
            Route::Unsupported
        ));
        ordinary_heartbeat.id = "beat".into();
        ordinary_heartbeat.p = json!({});
        assert_eq!(
            Schema::V1
                .encode(Envelope {
                    v: 1,
                    ..ordinary_heartbeat
                })
                .unwrap(),
            fixed.encode().unwrap()
        );
    }
}
