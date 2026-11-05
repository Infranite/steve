//! The multiplexer of one link: requests both ways, a control lane drained before the bulk lane,
//! heartbeats, per-method deadlines and the generation stamp.
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
use serde_json::{Value, json};
use steve_node_api::{ApiError, ApiResult, ErrorCode, OperationId};
use tokio::sync::{Notify, Semaphore, mpsc, oneshot, watch};
use tokio::time::Instant;

use crate::envelope::{Envelope, Frame, FrameSink, FrameStream, Kind, Outcome};
use crate::methods::{self, Lane};

/// What answers the other side's requests.
#[async_trait]
pub trait Handler: Send + Sync + 'static {
    async fn handle(&self, method: &str, p: Value) -> ApiResult<Value>;
    /// A control message other than a heartbeat (`paused`, `fenced`, `goodbye`…).
    fn control(&self, _m: &str, _p: &Value) {}
}

#[derive(Debug, Clone)]
pub struct ConnOptions {
    pub heartbeat: Duration,
    /// Closed with [`crate::close::DEAD`] after this long without any frame from the peer.
    pub dead_after: Duration,
    /// Prefix of request ids (`h` on the Hub, `n` on the Node), for logs.
    pub side: &'static str,
}

impl Default for ConnOptions {
    fn default() -> Self {
        ConnOptions {
            heartbeat: Duration::from_secs(10),
            dead_after: Duration::from_secs(30),
            side: "x",
        }
    }
}

/// Requests of the peer handled at once on one link. Past it a request is answered `exhausted`
/// without reaching the handler (nothing done: the sender may try again), so a peer that pipelines
/// faster than it is served cannot pile up tasks and payloads. Refusing instead of pausing the
/// reader keeps answers to this side's own calls flowing (a handler may be waiting on one).
pub const MAX_INBOUND: usize = 64;

const QUEUED: u8 = 0;
const WRITING: u8 = 1;
const ABANDONED: u8 = 2;

struct Out {
    text: String,
    /// For requests: QUEUED until the writer takes it (WRITING), or the caller gave up (ABANDONED).
    state: Option<Arc<AtomicU8>>,
}

struct Pending {
    tx: oneshot::Sender<Outcome>,
}

struct CloseReq {
    pre: Option<String>,
    code: u16,
    reason: String,
}

struct Shared {
    generation: u64,
    side: &'static str,
    ctl: mpsc::Sender<Out>,
    bulk: mpsc::Sender<Out>,
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
        let (ctl, ctl_rx) = mpsc::channel(256);
        let (bulk, bulk_rx) = mpsc::channel(16);
        let s = Arc::new(Shared {
            generation,
            side: opts.side,
            ctl,
            bulk,
            pending: Mutex::new(HashMap::new()),
            next: AtomicU64::new(1),
            closing: AtomicBool::new(false),
            close_req: Mutex::new(None),
            close_wake: Notify::new(),
            closed: watch::channel(None).0,
            written: AtomicU64::new(0),
        });
        tokio::spawn(writer(s.clone(), sink, ctl_rx, bulk_rx));
        tokio::spawn(reader(s.clone(), stream, handler, opts.clone()));
        tokio::spawn(heartbeat(Arc::downgrade(&s), opts.heartbeat));
        Arc::new(Conn { s })
    }

    pub fn generation(&self) -> u64 {
        self.s.generation
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
    /// (`fenced`, `goodbye`) written before the close frame.
    pub fn close(&self, pre: Option<(&str, Value)>, code: u16, reason: &str) {
        let pre = pre.and_then(|(m, p)| {
            Envelope::ctl(self.s.id(), m, self.s.generation, p)
                .encode()
                .ok()
        });
        shut(&self.s, pre, code, reason);
    }

    /// A control message (not a request: nobody answers it).
    pub fn notify(&self, m: &str, p: Value) {
        if let Ok(text) = Envelope::ctl(self.s.id(), m, self.s.generation, p).encode() {
            let _ = self.s.ctl.try_send(Out { text, state: None });
        }
    }

    /// Calls `method` with the table's lane and deadline.
    pub async fn call(&self, method: &str, p: Value, op: Option<&OperationId>) -> ApiResult<Value> {
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
        let offline = |why: &str| ApiError::new(ErrorCode::Offline, format!("{method}: {why}"));
        if self.is_closed() {
            return Err(offline("not connected"));
        }
        let id = self.s.id();
        let env = Envelope {
            v: crate::WIRE,
            t: Kind::Req,
            id: id.clone(),
            re: None,
            m: method.into(),
            generation: self.s.generation,
            trace: None,
            deadline_at: crate::now_ms() + deadline.as_millis() as i64,
            p,
        };
        let text = env.encode()?;
        let state = Arc::new(AtomicU8::new(QUEUED));
        let (tx, rx) = oneshot::channel();
        self.s
            .pending
            .lock()
            .unwrap()
            .insert(id.clone(), Pending { tx });
        let lane_tx = match lane {
            Lane::Control => &self.s.ctl,
            Lane::Bulk => &self.s.bulk,
        };
        let until = Instant::now() + deadline;
        let out = Out {
            text,
            state: Some(state.clone()),
        };
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
        let queued = tokio::time::timeout_at(until, lane_tx.send(out)).await;
        if !matches!(queued, Ok(Ok(()))) {
            self.s.pending.lock().unwrap().remove(&id);
            return Err(lost("could not queue"));
        }
        let got = tokio::time::timeout_at(until, rx).await;
        self.s.pending.lock().unwrap().remove(&id);
        match got {
            Ok(Ok(Outcome::Ok(v))) => Ok(v),
            Ok(Ok(Outcome::Err(e))) => Err(e),
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
    fn id(&self) -> String {
        format!("{}_{}", self.side, self.next.fetch_add(1, Ordering::SeqCst))
    }

    /// Answers from the reader itself (stale, unknown kind): never waits, so a full lane drops
    /// the answer and its sender meets its deadline.
    fn respond_now(&self, re: String, outcome: Outcome, generation: u64) {
        if let Some(out) = self.response(re, outcome, generation) {
            let _ = self.ctl.try_send(out);
        }
    }

    /// `generation`: a refusal of a stale request carries the request's own, so that its sender
    /// hears `stale` instead of waiting out its deadline.
    fn response(&self, re: String, outcome: Outcome, generation: u64) -> Option<Out> {
        let p = serde_json::to_value(&outcome).unwrap_or(Value::Null);
        let env = Envelope {
            v: crate::WIRE,
            t: Kind::Res,
            id: self.id(),
            re: Some(re),
            m: String::new(),
            generation,
            trace: None,
            deadline_at: 0,
            p,
        };
        let text = match env.encode() {
            Ok(t) => t,
            Err(e) => {
                // too large to answer: answer that instead of breaking the link
                let mut env = env;
                env.p = serde_json::to_value(Outcome::Err(e)).unwrap_or(Value::Null);
                env.encode().ok()?
            }
        };
        Some(Out { text, state: None })
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

async fn writer(
    s: Arc<Shared>,
    mut sink: FrameSink,
    mut ctl: mpsc::Receiver<Out>,
    mut bulk: mpsc::Receiver<Out>,
) {
    loop {
        let out = tokio::select! {
            biased;
            _ = s.close_wake.notified() => break,
            Some(o) = ctl.recv() => o,
            Some(o) = bulk.recv() => o,
            else => break,
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
        if sink.send(Frame::Text(out.text)).await.is_err() {
            shut(&s, None, crate::close::GOODBYE, "write failed");
            break;
        }
        s.written.fetch_add(1, Ordering::SeqCst);
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
                shut(&s, None, code, &reason);
                return;
            }
            Ok(Some(Ok(Frame::Text(t)))) => t,
        };
        let Some(env) = Envelope::decode(&frame) else {
            shut(&s, None, crate::close::MALFORMED, "malformed envelope");
            return;
        };
        if env.v.0 != crate::WIRE.0 {
            shut(&s, None, crate::close::UNSUPPORTED, "envelope version");
            return;
        }
        match env.t {
            Kind::Res => {
                // a late answer of another generation never reaches a caller
                if env.generation != s.generation {
                    continue;
                }
                let Some(re) = env.re else { continue };
                let outcome = serde_json::from_value::<Outcome>(env.p).unwrap_or_else(|e| {
                    Outcome::Err(ApiError::new(
                        ErrorCode::Internal,
                        format!("bad result: {e}"),
                    ))
                });
                if let Some(p) = s.pending.lock().unwrap().remove(&re) {
                    let _ = p.tx.send(outcome);
                }
            }
            Kind::Req => {
                if env.generation != s.generation {
                    s.respond_now(
                        env.id,
                        Outcome::Err(ApiError::new(ErrorCode::Stale, "another connection")),
                        env.generation,
                    );
                    continue;
                }
                let Ok(permit) = inflight.clone().try_acquire_owned() else {
                    s.respond_now(
                        env.id,
                        Outcome::Err(ApiError::new(
                            ErrorCode::Exhausted,
                            "too many requests in flight on this link",
                        )),
                        s.generation,
                    );
                    continue;
                };
                let (h, s2) = (handler.clone(), s.clone());
                // the work outlives the link: a port never ties what it started to its caller
                tokio::spawn(async move {
                    let outcome = match h.handle(&env.m, env.p).await {
                        Ok(v) => Outcome::Ok(v),
                        Err(e) => Outcome::Err(e),
                    };
                    // the slot is held until the answer is queued: a slow reader of answers
                    // slows the peer's new requests too
                    if let Some(out) = s2.response(env.id, outcome, s2.generation) {
                        let _ = s2.ctl.send(out).await;
                    }
                    drop(permit);
                });
            }
            Kind::Ctl => match env.m.as_str() {
                "heartbeat" => {}
                "fenced" => {
                    handler.control(&env.m, &env.p);
                    shut(&s, None, crate::close::FENCED, "fenced");
                    return;
                }
                "goodbye" => {
                    handler.control(&env.m, &env.p);
                    shut(&s, None, crate::close::GOODBYE, "goodbye");
                    return;
                }
                m => handler.control(m, &env.p),
            },
            Kind::Unknown => s.respond_now(
                env.id,
                Outcome::Err(ApiError::new(ErrorCode::Unsupported, "unknown frame kind")),
                s.generation,
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
        let env = Envelope::ctl(
            s.id(),
            "heartbeat",
            s.generation,
            json!({"at": crate::now_ms()}),
        );
        if let Ok(text) = env.encode() {
            let _ = s.ctl.try_send(Out { text, state: None });
        }
    }
}

/// Unknown methods answer `unsupported`; the link stays up.
pub fn unsupported(method: &str) -> ApiError {
    ApiError::new(ErrorCode::Unsupported, format!("unknown method {method}"))
}

pub(crate) fn to_value<T: serde::Serialize>(v: &T) -> ApiResult<Value> {
    serde_json::to_value(v).map_err(|e| ApiError::new(ErrorCode::Internal, format!("encode: {e}")))
}

pub(crate) fn from_value<T: serde::de::DeserializeOwned>(v: Value) -> ApiResult<T> {
    serde_json::from_value(v).map_err(|e| ApiError::new(ErrorCode::Invalid, format!("decode: {e}")))
}
