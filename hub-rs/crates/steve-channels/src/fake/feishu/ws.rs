//! Fake long connection (feishu-lark-cli.md §2.2, F0 delta): `POST /callback/ws/endpoint`,
//! the `wss` frame server, and Feishu's delivery behaviour.
//!
//! Behaviour modelled on larksuite/oapi-sdk-go `ws/` and the open-platform docs:
//! - the endpoint hands out a one-time ticket URL with `device_id` and `service_id`, plus
//!   `ClientConfig` (seconds); a refused upgrade answers with `Handshake-*` headers;
//! - at most [`MAX_CONNECTIONS`] connections per app (514 + 1000040350 beyond that);
//! - pings are answered with a pong carrying the current `ClientConfig`;
//! - "cluster mode": each delivery goes to ONE randomly chosen connection, never broadcast;
//! - an event not acked with `code:200` within the ack timeout (3 s) is redelivered on the
//!   retry schedule, possibly to another connection, with the same `event_id`.
//!
//! Faults: split payloads into parts (optionally out of order), drop connections, go silent
//! (no pongs, no frames — the client's read deadline must notice), refuse handshakes, fail the
//! endpoint, ignore acks, redeliver on demand.

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::ws::{Message as AxMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use futures_util::{SinkExt as _, StreamExt as _};
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot};

use super::push::{encrypt, envelope, record_history};
use super::{FakeFeishu, Shared, now_ms};
use crate::adapters::feishu::pbbp2::{self, Frame, code, header, kind};

/// Feishu's limit of simultaneous long connections per app.
pub const MAX_CONNECTIONS: usize = 50;
/// Feishu's ack deadline.
pub const ACK_TIMEOUT: Duration = Duration::from_secs(3);

/// `ClientConfig` the fake hands out, in seconds as on the wire.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WsClientConfig {
    pub reconnect_count: i64,
    pub reconnect_interval: u64,
    pub reconnect_nonce: u64,
    pub ping_interval: u64,
}

impl Default for WsClientConfig {
    fn default() -> Self {
        Self {
            reconnect_count: -1,
            reconnect_interval: 120,
            reconnect_nonce: 30,
            ping_interval: 120,
        }
    }
}

impl WsClientConfig {
    fn json(&self) -> Value {
        json!({
            "ReconnectCount": self.reconnect_count,
            "ReconnectInterval": self.reconnect_interval,
            "ReconnectNonce": self.reconnect_nonce,
            "PingInterval": self.ping_interval,
        })
    }
}

/// A connected client.
#[derive(Debug, Clone)]
pub struct WsConnInfo {
    pub conn_id: String,
    pub service_id: i32,
    pub pings: u32,
    pub connected_at: Instant,
}

/// The client's answer to one delivery.
#[derive(Debug, Clone)]
pub struct WsAck {
    /// `code` of the ack payload (200 = handled).
    pub code: i64,
    /// `data` of the ack payload, base64-decoded and parsed as JSON when possible.
    pub data: Option<Value>,
    /// The `biz_rt` header the client added.
    pub biz_rt: Option<String>,
    /// Time from the last part sent to the ack received.
    pub latency: Duration,
    /// The ack frame as received.
    pub frame: Frame,
}

/// One delivery attempt of an event.
#[derive(Debug, Clone)]
pub struct WsDelivery {
    pub event_id: String,
    /// 0 for the first push, then 1, 2… for redeliveries.
    pub attempt: u32,
    /// `None` when no connection was available.
    pub conn_id: Option<String>,
    /// The frames' `message_id` header (fresh per attempt).
    pub frame_message_id: String,
    pub parts: usize,
    /// `None` until (unless) the client acked.
    pub ack: Option<WsAck>,
    /// True when the ack came after the ack timeout (Feishu has already scheduled a retry).
    pub late: bool,
}

/// Refusal of the next handshakes: `Handshake-Status`, `Handshake-Msg`, `Handshake-Autherrcode`.
#[derive(Debug, Clone)]
struct Refusal {
    status: i64,
    msg: String,
    auth_err: Option<i64>,
}

enum Out {
    Frame(Vec<u8>),
    /// Drop the TCP connection without a close frame.
    Kill,
}

struct Conn {
    info: WsConnInfo,
    tx: mpsc::UnboundedSender<Out>,
}

struct Pending {
    delivery: usize,
    sent_at: Instant,
    tx: Option<oneshot::Sender<i64>>,
}

/// Long-connection state of the fake.
pub(crate) struct WsRuntime {
    config: WsClientConfig,
    max_conns: usize,
    /// ticket → (device id, service id), single use.
    tickets: HashMap<String, (String, i32)>,
    conns: BTreeMap<String, Conn>,
    connects_total: u64,
    silent: bool,
    ack_timeout: Duration,
    retry_schedule: Vec<Duration>,
    split: usize,
    shuffle_parts: bool,
    encrypt: bool,
    ignore_acks: u32,
    refuse: Vec<Refusal>,
    endpoint_fail: Vec<(i64, String)>,
    deliveries: Vec<WsDelivery>,
    /// frame message id → waiting delivery.
    pending: HashMap<String, Pending>,
    pushed: HashMap<String, Value>,
    next: u64,
}

impl std::fmt::Debug for WsRuntime {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WsRuntime")
            .field("conns", &self.conns.keys().collect::<Vec<_>>())
            .field("deliveries", &self.deliveries.len())
            .finish_non_exhaustive()
    }
}

impl WsRuntime {
    pub(crate) fn new() -> Self {
        Self {
            config: WsClientConfig::default(),
            max_conns: MAX_CONNECTIONS,
            tickets: HashMap::new(),
            conns: BTreeMap::new(),
            connects_total: 0,
            silent: false,
            ack_timeout: ACK_TIMEOUT,
            retry_schedule: super::FEISHU_RETRY_SCHEDULE.to_vec(),
            split: 1,
            shuffle_parts: false,
            encrypt: false,
            ignore_acks: 0,
            refuse: Vec::new(),
            endpoint_fail: Vec::new(),
            deliveries: Vec::new(),
            pending: HashMap::new(),
            pushed: HashMap::new(),
            next: 1,
        }
    }

    fn next_id(&mut self) -> u64 {
        let n = self.next;
        self.next += 1;
        n
    }
}

type St = State<Arc<Shared>>;

// ---------------------------------------------------------------------------------------------
// HTTP side: endpoint discovery and the upgrade.

/// `POST /callback/ws/endpoint {"AppID","AppSecret"}`.
pub(crate) async fn endpoint(
    State(sh): St,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let reply = |code: i64, msg: &str, data: Value| {
        sh.rt().requests.push(super::RecordedRequest {
            endpoint: super::Endpoint::WsEndpoint,
            method: "POST".into(),
            path: pbbp2::ENDPOINT_PATH.into(),
            query: String::new(),
            token: None,
            code,
        });
        axum::Json(json!({"code": code, "msg": msg, "data": data})).into_response()
    };
    let v: Value = serde_json::from_slice(&body).unwrap_or(Value::Null);
    let app = sh.world().app.clone();
    let field = |k: &str| v.get(k).and_then(Value::as_str).unwrap_or("").to_string();
    if field("AppID") != app.app_id || field("AppSecret") != app.app_secret {
        return reply(
            super::codes::BAD_APP_SECRET,
            "app secret invalid",
            json!({}),
        );
    }
    if !app.self_built {
        return reply(
            code::FORBIDDEN,
            "long connection is only available to self-built apps",
            json!({}),
        );
    }
    let host = headers
        .get(axum::http::header::HOST)
        .and_then(|h| h.to_str().ok())
        .unwrap_or("127.0.0.1")
        .to_string();
    let mut rt = sh.rt();
    if !rt.ws.endpoint_fail.is_empty() {
        let (c, m) = rt.ws.endpoint_fail.remove(0);
        drop(rt);
        return reply(c, &m, json!({}));
    }
    let n = rt.ws.next_id();
    let device_id = format!("{}", 7_000_000_000_000_000_000u64 + n);
    let service_id: i32 = 33_554_000 + (n as i32 % 1000);
    let ticket = format!("{:032x}", rand::random::<u128>());
    rt.ws
        .tickets
        .insert(ticket.clone(), (device_id.clone(), service_id));
    let cfg = rt.ws.config.json();
    drop(rt);
    let url = format!(
        "ws://{host}/ws/v2?fpid=493&aid={}&device_id={device_id}&access_key={:x}&service_id={service_id}&ticket={ticket}",
        app.app_id,
        rand::random::<u64>(),
    );
    reply(0, "ok", json!({"URL": url, "ClientConfig": cfg}))
}

fn refuse(r: &Refusal) -> Response {
    let mut resp = StatusCode::BAD_REQUEST.into_response();
    let h = resp.headers_mut();
    let mut put = |k: &'static str, v: String| {
        if let Ok(v) = v.parse() {
            h.insert(k, v);
        }
    };
    put(header::HANDSHAKE_STATUS, r.status.to_string());
    put(header::HANDSHAKE_MSG, r.msg.clone());
    if let Some(a) = r.auth_err {
        put(header::HANDSHAKE_AUTH_ERR_CODE, a.to_string());
    }
    resp
}

/// `GET /ws/v2?…&ticket=` — the upgrade.
pub(crate) async fn upgrade(
    State(sh): St,
    Query(q): Query<HashMap<String, String>>,
    ws: WebSocketUpgrade,
) -> Response {
    let refusal = {
        let mut rt = sh.rt();
        let w = &mut rt.ws;
        if !w.refuse.is_empty() {
            Err(w.refuse.remove(0))
        } else if w.conns.len() >= w.max_conns {
            Err(Refusal {
                status: code::AUTH_FAILED,
                msg: "exceed connection limit".into(),
                auth_err: Some(code::EXCEED_CONN_LIMIT),
            })
        } else {
            match q.get("ticket").and_then(|t| w.tickets.remove(t)) {
                Some(ids) => Ok(ids),
                None => Err(Refusal {
                    status: code::AUTH_FAILED,
                    msg: "invalid ticket".into(),
                    auth_err: None,
                }),
            }
        }
    };
    let (device_id, service_id) = match refusal {
        Ok(ids) => ids,
        Err(r) => return refuse(&r),
    };
    ws.on_upgrade(move |socket| serve(sh, socket, device_id, service_id))
}

/// One connection: registers it, writes queued frames, reads pings and acks.
async fn serve(sh: Arc<Shared>, socket: WebSocket, conn_id: String, service_id: i32) {
    let (tx, mut rx) = mpsc::unbounded_channel::<Out>();
    {
        let mut rt = sh.rt();
        rt.ws.connects_total += 1;
        rt.ws.conns.insert(
            conn_id.clone(),
            Conn {
                info: WsConnInfo {
                    conn_id: conn_id.clone(),
                    service_id,
                    pings: 0,
                    connected_at: Instant::now(),
                },
                tx: tx.clone(),
            },
        );
    }
    let (mut sink, mut stream) = socket.split();
    loop {
        tokio::select! {
            out = rx.recv() => match out {
                Some(Out::Frame(b)) => {
                    if sink.send(AxMessage::Binary(b.into())).await.is_err() {
                        break;
                    }
                }
                Some(Out::Kill) | None => break,
            },
            msg = stream.next() => match msg {
                Some(Ok(AxMessage::Binary(b))) => on_client_frame(&sh, &conn_id, service_id, &b, &tx),
                Some(Ok(AxMessage::Close(_))) | None | Some(Err(_)) => break,
                Some(Ok(_)) => {}
            },
        }
    }
    sh.rt().ws.conns.remove(&conn_id);
    // Dropping both halves closes TCP without a close frame (for Kill) or after the client's.
}

fn on_client_frame(
    sh: &Shared,
    conn_id: &str,
    service_id: i32,
    bytes: &[u8],
    tx: &mpsc::UnboundedSender<Out>,
) {
    let Ok(frame) = Frame::from_bytes(bytes) else {
        tracing::warn!(conn_id, "fake feishu ws: undecodable frame");
        return;
    };
    let mut rt = sh.rt();
    let w = &mut rt.ws;
    match (frame.method, frame.kind()) {
        (pbbp2::METHOD_CONTROL, kind::PING) => {
            if let Some(c) = w.conns.get_mut(conn_id) {
                c.info.pings += 1;
            }
            if w.silent {
                return;
            }
            let mut pong = Frame {
                method: pbbp2::METHOD_CONTROL,
                service: service_id,
                payload: Some(w.config.json().to_string().into_bytes()),
                ..Frame::default()
            };
            pong.add_header(header::TYPE, kind::PONG);
            let _ = tx.send(Out::Frame(pong.to_bytes()));
        }
        (pbbp2::METHOD_DATA, _) => {
            let Some(mid) = frame.header(header::MESSAGE_ID).map(str::to_string) else {
                return;
            };
            if w.ignore_acks > 0 {
                w.ignore_acks -= 1;
                return;
            }
            let Some(mut p) = w.pending.remove(&mid) else {
                return;
            };
            let payload: Value = frame
                .payload
                .as_deref()
                .and_then(|b| serde_json::from_slice(b).ok())
                .unwrap_or(Value::Null);
            let code = payload.get("code").and_then(Value::as_i64).unwrap_or(0);
            let data = payload.get("data").and_then(Value::as_str).and_then(|d| {
                use base64::Engine as _;
                let raw = base64::engine::general_purpose::STANDARD.decode(d).ok()?;
                serde_json::from_slice(&raw).ok()
            });
            let latency = p.sent_at.elapsed();
            let late = latency > w.ack_timeout;
            if let Some(d) = w.deliveries.get_mut(p.delivery) {
                d.ack = Some(WsAck {
                    code,
                    data,
                    biz_rt: frame.header(header::BIZ_RT).map(str::to_string),
                    latency,
                    frame: frame.clone(),
                });
                d.late = late;
            }
            if let Some(t) = p.tx.take() {
                let _ = t.send(code);
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------------------------
// Delivery.

/// Data frames of one delivery: the payload split into `parts` (at least one byte each).
fn data_frames(service: i32, msg_id: &str, payload: &[u8], parts: usize) -> Vec<Frame> {
    let parts = parts.clamp(1, payload.len().max(1));
    let chunk = payload.len().div_ceil(parts).max(1);
    let chunks: Vec<&[u8]> = if payload.is_empty() {
        vec![payload]
    } else {
        payload.chunks(chunk).collect()
    };
    let trace = format!("{:x}", rand::random::<u64>());
    let ts = now_ms().to_string();
    let sum = chunks.len();
    chunks
        .into_iter()
        .enumerate()
        .map(|(i, c)| {
            let mut f = Frame {
                seq_id: rand::random::<u32>() as u64,
                log_id: rand::random::<u32>() as u64,
                service,
                method: pbbp2::METHOD_DATA,
                payload: Some(c.to_vec()),
                ..Frame::default()
            };
            f.add_header(header::TYPE, kind::EVENT);
            f.add_header(header::MESSAGE_ID, msg_id);
            f.add_header(header::SUM, sum.to_string());
            f.add_header(header::SEQ, i.to_string());
            f.add_header(header::TRACE_ID, trace.clone());
            f.add_header(header::TIMESTAMP, ts.clone());
            f
        })
        .collect()
}

/// One attempt: picks a random connection, sends, waits for the ack. Returns the ack code.
async fn attempt(sh: &Arc<Shared>, event_id: &str, attempt: u32) -> Option<i64> {
    // World before runtime: the lock order used everywhere else.
    let app = sh.world().app.clone();
    let (rx, timeout) = {
        let mut rt = sh.rt();
        let envelope = rt.ws.pushed.get(event_id).cloned()?;
        let w = &mut rt.ws;
        let frame_mid = format!("{:032x}", rand::random::<u128>());
        let mut d = WsDelivery {
            event_id: event_id.to_string(),
            attempt,
            conn_id: None,
            frame_message_id: frame_mid.clone(),
            parts: 0,
            ack: None,
            late: false,
        };
        let live: Vec<String> = if w.silent {
            Vec::new()
        } else {
            w.conns.keys().cloned().collect()
        };
        if live.is_empty() {
            w.deliveries.push(d);
            return None;
        }
        let pick = live[rand::random_range(0..live.len())].clone();
        let conn = w.conns.get(&pick)?;
        let body = if w.encrypt {
            json!({"encrypt": encrypt(&app.encrypt_key, envelope.to_string().as_bytes())})
                .to_string()
        } else {
            envelope.to_string()
        };
        let mut frames = data_frames(conn.info.service_id, &frame_mid, body.as_bytes(), w.split);
        if w.shuffle_parts && frames.len() > 1 {
            frames.reverse();
        }
        d.conn_id = Some(pick.clone());
        d.parts = frames.len();
        for f in &frames {
            let _ = conn.tx.send(Out::Frame(f.to_bytes()));
        }
        let (tx, rx) = oneshot::channel();
        w.deliveries.push(d);
        let idx = w.deliveries.len() - 1;
        w.pending.insert(
            frame_mid,
            Pending {
                delivery: idx,
                sent_at: Instant::now(),
                tx: Some(tx),
            },
        );
        (rx, w.ack_timeout)
    };
    match tokio::time::timeout(timeout, rx).await {
        Ok(Ok(code)) => Some(code),
        // Late acks are still recorded (the pending entry stays) but don't stop the retries.
        _ => None,
    }
}

/// Delivers until acked with 200 or the retry schedule is exhausted.
async fn deliver(sh: Arc<Shared>, event_id: String, first_attempt: u32) {
    let schedule = sh.rt().ws.retry_schedule.clone();
    let mut n = first_attempt;
    if attempt(&sh, &event_id, n).await == Some(200) {
        return;
    }
    for delay in schedule {
        tokio::time::sleep(delay).await;
        n += 1;
        if attempt(&sh, &event_id, n).await == Some(200) {
            return;
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Test API.

impl FakeFeishu {
    /// `ClientConfig` returned by the endpoint and in pongs (seconds).
    pub fn ws_set_client_config(&self, c: WsClientConfig) {
        self.shared.rt().ws.config = c;
    }

    /// Ack deadline before a redelivery is scheduled (default 3 s).
    pub fn ws_set_ack_timeout(&self, d: Duration) {
        self.shared.rt().ws.ack_timeout = d;
    }

    /// Delays between redeliveries of an unacked event (default [`super::FEISHU_RETRY_SCHEDULE`],
    /// which is the webhook schedule; the long-connection one is to be measured in F7).
    pub fn ws_set_retry_schedule(&self, s: Vec<Duration>) {
        self.shared.rt().ws.retry_schedule = s;
    }

    /// Split every following delivery into `parts` frames; `shuffle` sends them last-first.
    pub fn ws_split(&self, parts: usize, shuffle: bool) {
        let mut rt = self.shared.rt();
        rt.ws.split = parts.max(1);
        rt.ws.shuffle_parts = shuffle;
    }

    /// Wrap pushed events in `{"encrypt": …}` with the Encrypt Key (whether Feishu does this on
    /// the long connection is to be measured; default off).
    pub fn ws_encrypt_events(&self, on: bool) {
        self.shared.rt().ws.encrypt = on;
    }

    /// Lower the per-app connection limit (default [`MAX_CONNECTIONS`]).
    pub fn ws_set_max_connections(&self, n: usize) {
        self.shared.rt().ws.max_conns = n;
    }

    /// Stop answering pings and sending events, without closing — a half-dead connection.
    pub fn ws_set_silent(&self, silent: bool) {
        self.shared.rt().ws.silent = silent;
    }

    /// Ignore the next `n` acks (the events are then redelivered).
    pub fn ws_ignore_acks(&self, n: u32) {
        self.shared.rt().ws.ignore_acks += n;
    }

    /// Refuse the next handshake with these `Handshake-*` values.
    pub fn ws_refuse_next_handshake(&self, status: i64, msg: &str, auth_err: Option<i64>) {
        self.shared.rt().ws.refuse.push(Refusal {
            status,
            msg: msg.into(),
            auth_err,
        });
    }

    /// Fail the next endpoint request with `code` (1 = busy, retried; others are client errors).
    pub fn ws_fail_next_endpoint(&self, code: i64, msg: &str) {
        self.shared.rt().ws.endpoint_fail.push((code, msg.into()));
    }

    /// Drops every connection's TCP stream without a close frame.
    pub fn ws_disconnect_all(&self) {
        for c in self.shared.rt().ws.conns.values() {
            let _ = c.tx.send(Out::Kill);
        }
    }

    /// Drops one connection.
    pub fn ws_disconnect(&self, conn_id: &str) {
        if let Some(c) = self.shared.rt().ws.conns.get(conn_id) {
            let _ = c.tx.send(Out::Kill);
        }
    }

    /// Currently connected clients.
    pub fn ws_connections(&self) -> Vec<WsConnInfo> {
        self.shared
            .rt()
            .ws
            .conns
            .values()
            .map(|c| c.info.clone())
            .collect()
    }

    /// Successful upgrades so far.
    pub fn ws_connects_total(&self) -> u64 {
        self.shared.rt().ws.connects_total
    }

    /// Waits until at least `n` clients are connected.
    pub async fn ws_wait_connections(&self, n: usize, timeout: Duration) -> bool {
        let end = Instant::now() + timeout;
        while Instant::now() < end {
            if self.shared.rt().ws.conns.len() >= n {
                return true;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        false
    }

    /// Wraps `event` in a schema-2.0 envelope and delivers it over the long connection. Returns
    /// the `event_id`. `im.message.receive_v1` messages also land in the chat history.
    pub fn ws_push(&self, event_type: &str, event: Value) -> String {
        let env = envelope(&self.shared, event_type, event);
        self.ws_push_envelope(env)
    }

    /// Delivers a complete envelope (e.g. a fixture) as-is.
    pub fn ws_push_envelope(&self, envelope: Value) -> String {
        push_envelope(&self.shared, envelope)
    }

    /// Delivers an already pushed event again (a duplicate push, same `event_id`).
    pub fn ws_redeliver(&self, event_id: &str) -> bool {
        let next = {
            let rt = self.shared.rt();
            if !rt.ws.pushed.contains_key(event_id) {
                return false;
            }
            rt.ws
                .deliveries
                .iter()
                .filter(|d| d.event_id == event_id)
                .map(|d| d.attempt + 1)
                .max()
                .unwrap_or(0)
        };
        let sh = self.shared.clone();
        let id = event_id.to_string();
        tokio::spawn(async move {
            attempt(&sh, &id, next).await;
        });
        true
    }

    /// Every delivery attempt so far, oldest first.
    pub fn ws_deliveries(&self) -> Vec<WsDelivery> {
        self.shared.rt().ws.deliveries.clone()
    }

    /// Attempts of one event.
    pub fn ws_deliveries_of(&self, event_id: &str) -> Vec<WsDelivery> {
        self.ws_deliveries()
            .into_iter()
            .filter(|d| d.event_id == event_id)
            .collect()
    }

    /// Waits until some attempt of `event_id` was acked with 200 in time; returns it.
    pub async fn ws_wait_acked(&self, event_id: &str, timeout: Duration) -> Option<WsDelivery> {
        let end = Instant::now() + timeout;
        while Instant::now() < end {
            let hit = self
                .ws_deliveries_of(event_id)
                .into_iter()
                .find(|d| d.ack.as_ref().is_some_and(|a| a.code == 200));
            if hit.is_some() {
                return hit;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        None
    }
}

/// Records and starts delivering `envelope`; returns its `event_id`.
pub(crate) fn push_envelope(sh: &Arc<Shared>, envelope: Value) -> String {
    let event_id = envelope
        .pointer("/header/event_id")
        .and_then(Value::as_str)
        .map(str::to_string)
        .unwrap_or_else(|| format!("{:032x}", rand::random::<u128>()));
    if envelope
        .pointer("/header/event_type")
        .and_then(Value::as_str)
        == Some("im.message.receive_v1")
    {
        record_history(sh, &envelope);
    }
    sh.rt().ws.pushed.insert(event_id.clone(), envelope);
    tokio::spawn(deliver(sh.clone(), event_id.clone(), 0));
    event_id
}

/// `POST /_fake/ws/push` (`{event_type, event}` or a full envelope) → `{event_id}`.
pub(crate) async fn ctl_push(State(sh): St, body: axum::body::Bytes) -> Response {
    let Ok(v) = serde_json::from_slice::<Value>(&body) else {
        return (StatusCode::BAD_REQUEST, "body must be JSON").into_response();
    };
    let env = match (v.get("event_type").and_then(Value::as_str), v.get("event")) {
        (Some(t), Some(e)) if v.get("header").is_none() => envelope(&sh, t, e.clone()),
        _ => v,
    };
    let id = push_envelope(&sh, env);
    axum::Json(json!({"event_id": id})).into_response()
}

/// `GET /_fake/ws/deliveries` → attempts with their ack codes; `{connections}` too.
pub(crate) async fn ctl_deliveries(State(sh): St) -> Response {
    let rt = sh.rt();
    let ds: Vec<Value> = rt
        .ws
        .deliveries
        .iter()
        .map(|d| {
            json!({
                "event_id": d.event_id,
                "attempt": d.attempt,
                "conn_id": d.conn_id,
                "parts": d.parts,
                "ack_code": d.ack.as_ref().map(|a| a.code),
                "biz_rt": d.ack.as_ref().and_then(|a| a.biz_rt.clone()),
                "late": d.late,
            })
        })
        .collect();
    let conns: Vec<Value> = rt
        .ws
        .conns
        .values()
        .map(|c| json!({"conn_id": c.info.conn_id, "service_id": c.info.service_id, "pings": c.info.pings}))
        .collect();
    axum::Json(json!({"deliveries": ds, "connections": conns})).into_response()
}

/// `POST /_fake/ws/disconnect` drops every connection.
pub(crate) async fn ctl_disconnect(State(sh): St) -> Response {
    for c in sh.rt().ws.conns.values() {
        let _ = c.tx.send(Out::Kill);
    }
    StatusCode::NO_CONTENT.into_response()
}

/// `GET /open-apis/event/v1/connection` → `{online_instance_cnt}` (what lark-cli's
/// `remote_preflight.go` checks before consuming events).
pub(crate) fn online_count(sh: &Shared) -> usize {
    sh.rt().ws.conns.len()
}
