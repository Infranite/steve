//! One WebSocket session with a channel service (§2, §3, §9): the dial with the bearer token, the
//! `hello`, requests matched to their answers under a timeout each, the server's notifications
//! handed on, a `ping` every 20 s, and the socket dropped after 60 s without a frame.
//!
//! Everything a remote sends is bounded: frames over the cap close the socket (1009), anything
//! that is not one JSON-RPC object closes it (1003), at most `max_inflight_requests` requests are
//! open, and notifications queue only as far as the feed window allows (a server that sends more
//! than its window of unacknowledged batches is closed, never buffered without end).

use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use tokio::sync::{Semaphore, mpsc, oneshot};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::http::HeaderValue;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;
use tokio_tungstenite::tungstenite::protocol::frame::CloseFrame;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_tungstenite::{Connector, MaybeTlsStream, WebSocketStream};
use tokio_util::sync::CancellationToken;

use super::endpoint::Endpoint;
use super::wire::{
    self, Hello, HelloError, Incoming, Limits, MAX_FRAME, Reason, RpcError, Status, WritePolicy,
};

/// The waits of a session and of the feed's reconnects. [`Timing::default`] is the protocol's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing {
    /// Between two `ping`s (§9.1).
    pub ping: Duration,
    /// Without a frame from the server this long, the socket is dropped (§9.1).
    pub silence: Duration,
    /// The dial, the upgrade, hello and ping (§7).
    pub hello: Duration,
    /// Reads and writes (§7).
    pub call: Duration,
    /// One history page (§7).
    pub history: Duration,
    /// One media download (§8).
    pub media: Duration,
    /// The first wait before a redial; doubled up to `backoff_max`, with full jitter (§9.3).
    pub backoff_min: Duration,
    pub backoff_max: Duration,
    /// Connected this long, the backoff starts again from `backoff_min` (§9.3).
    pub stable: Duration,
}

impl Default for Timing {
    fn default() -> Self {
        Self {
            ping: Duration::from_secs(20),
            silence: Duration::from_secs(60),
            hello: Duration::from_secs(10),
            call: Duration::from_secs(30),
            history: Duration::from_secs(60),
            media: Duration::from_secs(120),
            backoff_min: Duration::from_secs(1),
            backoff_max: Duration::from_secs(5 * 60),
            stable: Duration::from_secs(60),
        }
    }
}

/// Why a session did not open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenError {
    /// The URL, pin or token is not acceptable.
    Refused(String),
    /// The upgrade was refused with 401 or 403, or hello with `unauthorized` (§3.1).
    Auth(String),
    /// No common protocol version (§3.4): wait at the maximum backoff.
    Version(String),
    /// Required features missing (§3.5): wait at the maximum backoff.
    Missing(Vec<String>),
    /// The network, TLS, a timeout, a malformed hello: try again.
    Transport(String),
}

impl std::fmt::Display for OpenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused(m) | Self::Auth(m) | Self::Version(m) | Self::Transport(m) => {
                f.write_str(m)
            }
            Self::Missing(m) => write!(f, "missing features: {}", m.join(", ")),
        }
    }
}

/// Why a request did not get a result.
#[derive(Debug, Clone, PartialEq)]
pub enum CallError {
    /// The server answered with an error.
    Rpc(RpcError),
    /// No answer in time (a write's outcome is unknown).
    Timeout,
    /// The socket is gone.
    Closed(String),
    /// The request would not fit in a frame.
    TooLarge,
}

impl CallError {
    pub fn reason(&self) -> Option<Reason> {
        match self {
            Self::Rpc(e) => Some(e.reason),
            _ => None,
        }
    }
}

impl std::fmt::Display for CallError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Rpc(e) => e.fmt(f),
            Self::Timeout => f.write_str("timeout"),
            Self::Closed(w) => write!(f, "closed: {w}"),
            Self::TooLarge => f.write_str("request over the frame cap"),
        }
    }
}

/// A server notification (§4.6).
#[derive(Debug, Clone, PartialEq)]
pub struct Note {
    pub method: String,
    pub params: Value,
}

type Pending = Mutex<HashMap<u64, oneshot::Sender<Result<Value, RpcError>>>>;

/// One open session. Cheap to share; dropping the last handle does not close it, [`Self::close`]
/// does (and so does the server, or silence).
pub struct Session {
    out: mpsc::Sender<Message>,
    pending: Arc<Pending>,
    next: AtomicU64,
    inflight: Semaphore,
    closed: CancellationToken,
    why: Arc<Mutex<Option<String>>>,
    hello: Hello,
    status: Mutex<Status>,
    /// Features the server answered `unsupported` for: absent for the session (§7).
    absent: Mutex<HashSet<String>>,
    timing: Timing,
}

impl std::fmt::Debug for Session {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Session")
            .field("realm", &self.hello.realm)
            .field("closed", &self.closed.is_cancelled())
            .finish()
    }
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The capacity of the notification queue: a full window of batches plus room for status notes.
fn note_capacity(limits: &Limits) -> usize {
    limits.max_unacked_batches + 16
}

type Ws = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

/// Opens the socket: the outbound guard, TCP, TLS (webpki roots or the pin), the upgrade with the
/// bearer token. 401/403 at the upgrade is [`OpenError::Auth`].
async fn dial(ep: &Endpoint, token: &str, t: &Timing) -> Result<Ws, OpenError> {
    ep.guard().map_err(OpenError::Refused)?;
    let mut req = ep
        .url
        .as_str()
        .into_client_request()
        .map_err(|e| OpenError::Refused(e.to_string()))?;
    let mut auth = HeaderValue::from_str(&format!("Bearer {token}"))
        .map_err(|_| OpenError::Refused("token".into()))?;
    auth.set_sensitive(true);
    req.headers_mut().insert("authorization", auth);
    let cfg = steve_node_wire::tls::client_config(&ep.trust).map_err(OpenError::Transport)?;
    let connector = match cfg {
        Some(c) => Connector::Rustls(c),
        None => Connector::Plain,
    };
    let uri = req.uri().clone();
    let host = uri
        .host()
        .ok_or_else(|| OpenError::Refused("no host".into()))?;
    let port = uri
        .port_u16()
        .or(match uri.scheme_str() {
            Some("wss") => Some(443),
            Some("ws") => Some(80),
            _ => None,
        })
        .ok_or_else(|| OpenError::Refused("scheme".into()))?;
    let tcp = tokio::time::timeout(
        t.hello,
        tokio::net::TcpStream::connect(format!("{host}:{port}")),
    )
    .await
    .map_err(|_| OpenError::Transport("connect timed out".into()))?
    .map_err(|e| OpenError::Transport(format!("connect: {e}")))?;
    let _ = tcp.set_nodelay(true);
    let cfg = WebSocketConfig::default()
        .max_message_size(Some(MAX_FRAME))
        .max_frame_size(Some(MAX_FRAME));
    let (ws, _) = tokio::time::timeout(
        t.hello,
        tokio_tungstenite::client_async_tls_with_config(req, tcp, Some(cfg), Some(connector)),
    )
    .await
    .map_err(|_| OpenError::Transport("upgrade timed out".into()))?
    .map_err(|e| match e {
        WsError::Http(r) if matches!(r.status().as_u16(), 401 | 403) => {
            OpenError::Auth(format!("upgrade refused ({})", r.status().as_u16()))
        }
        WsError::Http(r) => OpenError::Transport(format!("upgrade refused ({})", r.status())),
        e => OpenError::Transport(e.to_string()),
    })?;
    Ok(ws)
}

impl Session {
    /// Dials, says hello and checks the answer (§3). Returns the session and its notifications.
    /// `now_ms`: the Hub's clock, sent in hello.
    pub async fn open(
        ep: &Endpoint,
        token: &str,
        timing: Timing,
        now_ms: i64,
    ) -> Result<(Arc<Session>, mpsc::Receiver<Note>), OpenError> {
        let ws = dial(ep, token, &timing).await?;
        let (sink, stream) = ws.split();
        let (out_tx, out_rx) = mpsc::channel::<Message>(64);
        let closed = CancellationToken::new();
        let why = Arc::new(Mutex::new(None));
        let pending: Arc<Pending> = Arc::default();
        // until hello says otherwise, the Hub's ceilings
        let (note_tx, note_rx) = mpsc::channel::<Note>(note_capacity(&Limits::CEILING));
        tokio::spawn(write_loop(sink, out_rx, closed.clone()));
        tokio::spawn(read_loop(
            stream,
            out_tx.clone(),
            pending.clone(),
            note_tx,
            closed.clone(),
            why.clone(),
            timing.silence,
        ));
        let boot = Boot {
            out: out_tx.clone(),
            pending: pending.clone(),
            closed: closed.clone(),
            why: why.clone(),
        };
        let hello = match boot
            .call(
                "hello",
                wire::hello_params(env!("CARGO_PKG_VERSION"), now_ms),
                timing.hello,
            )
            .await
        {
            Ok(v) => v,
            Err(CallError::Rpc(e)) => {
                boot.close(1000, "hello refused");
                return Err(match e.reason {
                    Reason::UnsupportedVersion => OpenError::Version(format!(
                        "no common protocol version (the Hub speaks {}, the service {})",
                        wire::PROTOCOL,
                        e.data
                            .get("supported")
                            .map(Value::to_string)
                            .unwrap_or_else(|| "?".into())
                    )),
                    Reason::Unauthorized | Reason::AccountChanged => OpenError::Auth(e.to_string()),
                    _ => OpenError::Transport(format!("hello: {e}")),
                });
            }
            Err(e) => {
                boot.close(1000, "hello failed");
                return Err(OpenError::Transport(format!("hello: {e}")));
            }
        };
        let hello = match Hello::parse(&hello) {
            Ok(h) => h,
            Err(e) => {
                boot.close(1000, "hello refused");
                return Err(match e {
                    HelloError::Version(v) => OpenError::Version(format!(
                        "the service answered protocol {v}, the Hub speaks {}",
                        wire::PROTOCOL
                    )),
                    HelloError::Missing(m) => OpenError::Missing(m),
                    HelloError::Malformed(m) => OpenError::Transport(m),
                });
            }
        };
        if let Some(t) = hello.time_ms
            && (t - now_ms).abs() > 5 * 60_000
        {
            tracing::warn!(
                realm = hello.realm,
                skew_ms = t - now_ms,
                "ext_ws: the channel service's clock is off by more than 5 minutes"
            );
        }
        let session = Arc::new(Session {
            out: out_tx,
            pending,
            next: AtomicU64::new(2),
            inflight: Semaphore::new(hello.limits.max_inflight_requests),
            closed: closed.clone(),
            why,
            status: Mutex::new(hello.session.clone()),
            hello,
            absent: Mutex::default(),
            timing,
        });
        tokio::spawn(ping_loop(Arc::downgrade(&session), timing.ping, closed));
        Ok((session, note_rx))
    }

    pub fn hello(&self) -> &Hello {
        &self.hello
    }

    pub fn limits(&self) -> &Limits {
        &self.hello.limits
    }

    pub fn timing(&self) -> &Timing {
        &self.timing
    }

    /// The server offers `feature` and has not answered `unsupported` for it.
    pub fn has(&self, feature: &str) -> bool {
        self.hello.has(feature) && !lock(&self.absent).contains(feature)
    }

    /// The feature counts as absent for the rest of the session (§7 `unsupported`).
    pub fn drop_feature(&self, feature: &str) {
        lock(&self.absent).insert(feature.to_string());
    }

    pub fn status(&self) -> Status {
        lock(&self.status).clone()
    }

    pub fn set_status(&self, s: Status) {
        *lock(&self.status) = s;
    }

    /// A write crossed a change to `forbidden` (§3.6).
    pub fn forbid_writes(&self) {
        lock(&self.status).write_policy = WritePolicy::Forbidden;
    }

    pub fn is_closed(&self) -> bool {
        self.closed.is_cancelled()
    }

    /// Resolves once the session is closed.
    pub async fn closed(&self) {
        self.closed.cancelled().await
    }

    /// Why it closed, once it has.
    pub fn close_reason(&self) -> Option<String> {
        lock(&self.why).clone()
    }

    /// Sends a close frame and ends the session.
    pub fn close(&self, code: u16, reason: &str) {
        close_with(&self.out, &self.closed, &self.why, code, reason);
    }

    /// One request, answered within `timeout` (bounded by `max_inflight_requests`).
    pub async fn call(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, CallError> {
        let _permit = tokio::select! {
            p = self.inflight.acquire() => p.map_err(|_| CallError::Closed("closed".into()))?,
            _ = tokio::time::sleep(timeout) => return Err(CallError::Timeout),
            _ = self.closed.cancelled() => return Err(CallError::Closed(self.close_reason().unwrap_or_default())),
        };
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let text = wire::request(id, method, params);
        if text.len() > self.hello.limits.max_frame_bytes {
            return Err(CallError::TooLarge);
        }
        send_call(
            &self.out,
            &self.pending,
            &self.closed,
            &self.why,
            id,
            text,
            timeout,
        )
        .await
    }

    /// A `ping` (§9.1).
    pub async fn ping(&self) -> Result<Value, CallError> {
        self.call("ping", json!({}), self.timing.hello).await
    }
}

/// What `hello` is sent with, before the session exists.
struct Boot {
    out: mpsc::Sender<Message>,
    pending: Arc<Pending>,
    closed: CancellationToken,
    why: Arc<Mutex<Option<String>>>,
}

impl Boot {
    async fn call(
        &self,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, CallError> {
        let text = wire::request(1, method, params);
        send_call(
            &self.out,
            &self.pending,
            &self.closed,
            &self.why,
            1,
            text,
            timeout,
        )
        .await
    }

    fn close(&self, code: u16, reason: &str) {
        close_with(&self.out, &self.closed, &self.why, code, reason);
    }
}

async fn send_call(
    out: &mpsc::Sender<Message>,
    pending: &Pending,
    closed: &CancellationToken,
    why: &Mutex<Option<String>>,
    id: u64,
    text: String,
    timeout: Duration,
) -> Result<Value, CallError> {
    let (tx, rx) = oneshot::channel();
    lock(pending).insert(id, tx);
    let gone = || CallError::Closed(lock(why).clone().unwrap_or_default());
    if out.send(Message::Text(text.into())).await.is_err() {
        lock(pending).remove(&id);
        return Err(gone());
    }
    // an answer that came before the close is the answer
    let r = tokio::select! {
        biased;
        r = rx => match r {
            Ok(r) => r.map_err(CallError::Rpc),
            Err(_) => Err(gone()),
        },
        _ = tokio::time::sleep(timeout) => Err(CallError::Timeout),
        _ = closed.cancelled() => Err(gone()),
    };
    lock(pending).remove(&id);
    r
}

fn close_with(
    out: &mpsc::Sender<Message>,
    closed: &CancellationToken,
    why: &Mutex<Option<String>>,
    code: u16,
    reason: &str,
) {
    if closed.is_cancelled() {
        return;
    }
    lock(why).get_or_insert_with(|| format!("{code} {reason}"));
    let _ = out.try_send(Message::Close(Some(CloseFrame {
        code: CloseCode::from(code),
        reason: reason.to_string().into(),
    })));
    closed.cancel();
}

async fn write_loop(
    mut sink: futures_util::stream::SplitSink<Ws, Message>,
    mut rx: mpsc::Receiver<Message>,
    closed: CancellationToken,
) {
    loop {
        tokio::select! {
            biased;
            m = rx.recv() => {
                let Some(m) = m else { break };
                let last = matches!(m, Message::Close(_));
                if sink.send(m).await.is_err() || last {
                    break;
                }
            }
            _ = closed.cancelled() => {
                // what was queued before the close (its close frame) still goes out
                while let Ok(m) = rx.try_recv() {
                    let last = matches!(m, Message::Close(_));
                    if sink.send(m).await.is_err() || last {
                        break;
                    }
                }
                break;
            }
        }
    }
    let _ = sink.close().await;
    closed.cancel();
}

async fn read_loop(
    mut stream: futures_util::stream::SplitStream<Ws>,
    out: mpsc::Sender<Message>,
    pending: Arc<Pending>,
    notes: mpsc::Sender<Note>,
    closed: CancellationToken,
    why: Arc<Mutex<Option<String>>>,
    silence: Duration,
) {
    let close = |code: u16, reason: &str| close_with(&out, &closed, &why, code, reason);
    loop {
        let next = tokio::select! {
            n = tokio::time::timeout(silence, stream.next()) => n,
            _ = closed.cancelled() => break,
        };
        let msg = match next {
            Err(_) => {
                close(1001, "no frame from the channel service for too long");
                break;
            }
            Ok(None) => {
                close(1006, "the channel service closed the socket");
                break;
            }
            Ok(Some(Err(WsError::Capacity(_)))) => {
                close(1009, "frame over the cap");
                break;
            }
            Ok(Some(Err(e))) => {
                close(1006, &format!("socket error: {e}"));
                break;
            }
            Ok(Some(Ok(m))) => m,
        };
        let text = match msg {
            Message::Text(t) => t,
            Message::Binary(_) => {
                close(1003, "binary frame");
                break;
            }
            Message::Close(c) => {
                let reason = c.map(|c| format!("{} {}", u16::from(c.code), c.reason));
                lock(&why).get_or_insert_with(|| {
                    format!(
                        "closed by the channel service {}",
                        reason.unwrap_or_default()
                    )
                });
                closed.cancel();
                break;
            }
            // pings are answered by the socket; any frame counts against silence
            _ => continue,
        };
        match wire::classify(text.as_str()) {
            Err(e) => {
                tracing::warn!("ext_ws: protocol violation: {e}");
                close(1003, &e);
                break;
            }
            Ok(Incoming::Response { id, result }) => {
                if let Some(tx) = lock(&pending).remove(&id) {
                    let _ = tx.send(result);
                }
            }
            Ok(Incoming::Request { id }) => {
                // the server never asks the Hub for anything (§10)
                let err = RpcError::new(Reason::Unsupported, "the Hub takes no requests");
                let _ = out.try_send(Message::Text(wire::response(&id, Err(err)).into()));
            }
            Ok(Incoming::Note { method, params }) => {
                match notes.try_send(Note { method, params }) {
                    Ok(()) => {}
                    // nobody listens (a session for one write): dropped
                    Err(mpsc::error::TrySendError::Closed(_)) => {}
                    Err(mpsc::error::TrySendError::Full(_)) => {
                        tracing::warn!("ext_ws: the channel service sent past its window");
                        close(1008, "more unacknowledged batches than the window");
                        break;
                    }
                }
            }
        }
    }
    // every open request ends with the session
    lock(&pending).clear();
    closed.cancel();
}

async fn ping_loop(s: std::sync::Weak<Session>, every: Duration, closed: CancellationToken) {
    loop {
        tokio::select! {
            _ = tokio::time::sleep(every) => {}
            _ = closed.cancelled() => return,
        }
        let Some(s) = s.upgrade() else { return };
        // the answer is a frame like any other: silence is judged by the reader
        let _ = s.ping().await;
    }
}
