//! Long-connection (WebSocket) event client, a port of larksuite/oapi-sdk-go `ws/`
//! (read at commit db9f52d, 2026-10-15). feishu-lark-cli.md §2.2.
//!
//! Protocol, as the Go SDK implements it:
//! 1. `POST {base}/callback/ws/endpoint {"AppID","AppSecret","ClientAssertion":""}` →
//!    `{code, msg, data:{URL, ClientConfig:{ReconnectCount, ReconnectInterval, ReconnectNonce,
//!    PingInterval}}}` (seconds). The URL carries `device_id` (connection id) and `service_id`.
//!    A non-200 HTTP status, code 1 or code 1000040343 are server errors (retried); any other
//!    non-zero code is a client error (not retried).
//! 2. Dial the URL. A refused upgrade carries `Handshake-Status` / `Handshake-Msg` /
//!    `Handshake-Autherrcode`; 403 and 514+1000040350 (too many connections) are client errors.
//! 3. Every message is a binary protobuf [`Frame`]. Ping right after connecting, then every
//!    `PingInterval`; the pong (control frame, `type=pong`) may carry a new `ClientConfig`.
//!    No frame for `2 × PingInterval + 5 s` means the connection is dead.
//! 4. Data frames with `type=event` carry the schema-2.0 event JSON, possibly split into
//!    `sum` parts by `seq` under one `message_id` (parts expire after 5 s). The ack is the same
//!    frame with a `biz_rt` header added and payload `{"code":200,"headers":null,"data":null}`
//!    (500 when the handler failed). Feishu expects it within 3 s, else it redelivers.
//!    `type=card` frames are ignored, as the Go SDK does.
//! 5. On failure, reconnect: the first attempt after a random delay in `[0, ReconnectNonce)`,
//!    then every `ReconnectInterval`, at most `ReconnectCount` attempts (negative = forever).
//!    Each attempt fetches a fresh endpoint.
//!
//! Deviations, both defensive: a `PingInterval` of 0 from the server keeps the previous value
//! (the Go SDK would spin), and split parts with an out-of-range `seq` are dropped (Go panics).

use std::collections::HashMap;
use std::future::Future;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::{SinkExt as _, StreamExt as _};
use serde::Deserialize;
use serde_json::Value;
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::tungstenite;
use tokio_tungstenite::tungstenite::Message as WsMessage;
use tokio_util::sync::CancellationToken;

use super::pbbp2::{self, Frame, code, header, kind};

/// How long split parts wait for their siblings (`ws/client_message.go` `combine`).
const PART_TTL: Duration = Duration::from_secs(5);
/// Upper bound on `sum`, so a bad frame can't make us allocate without limit.
const MAX_PARTS: i64 = 1024;
/// Feishu's ack deadline; slower handlers are logged.
pub const ACK_DEADLINE: Duration = Duration::from_secs(3);

/// Connection parameters the server hands out (and may update in pongs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientConfig {
    /// Reconnect attempts per outage; negative = unlimited.
    pub reconnect_count: i64,
    pub reconnect_interval: Duration,
    /// Upper bound of the random delay before the first reconnect attempt.
    pub reconnect_nonce: Duration,
    pub ping_interval: Duration,
}

impl Default for ClientConfig {
    /// The Go SDK's defaults before the server sends its own.
    fn default() -> Self {
        Self {
            reconnect_count: -1,
            reconnect_interval: Duration::from_secs(120),
            reconnect_nonce: Duration::from_secs(30),
            ping_interval: Duration::from_secs(120),
        }
    }
}

/// `ClientConfig` as JSON (`ws/model.go`), all values in seconds.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
struct RawClientConfig {
    #[serde(rename = "ReconnectCount", default)]
    reconnect_count: i64,
    #[serde(rename = "ReconnectInterval", default)]
    reconnect_interval: u64,
    #[serde(rename = "ReconnectNonce", default)]
    reconnect_nonce: u64,
    #[serde(rename = "PingInterval", default)]
    ping_interval: u64,
}

impl ClientConfig {
    /// Applies a server config wholesale, like `applyConfigLocked`, except a zero ping interval.
    fn apply(&mut self, raw: RawClientConfig) {
        self.reconnect_count = raw.reconnect_count;
        self.reconnect_interval = Duration::from_secs(raw.reconnect_interval);
        self.reconnect_nonce = Duration::from_secs(raw.reconnect_nonce);
        if raw.ping_interval > 0 {
            self.ping_interval = Duration::from_secs(raw.ping_interval);
        }
    }
}

/// What [`fetch_endpoint`] returns.
#[derive(Debug, Clone)]
pub struct Endpoint {
    pub url: String,
    pub config: Option<ClientConfig>,
}

/// Client settings. `api_base` is `https://open.feishu.cn` (or the fake's base URL).
#[derive(Debug, Clone)]
pub struct WsOptions {
    pub api_base: String,
    pub app_id: String,
    pub app_secret: String,
    /// Reconnect after failures (default true).
    pub auto_reconnect: bool,
    /// Used until the endpoint response provides the server's config.
    pub initial_config: ClientConfig,
    /// Added to `2 × PingInterval` for the read deadline (Go: 5 s).
    pub pong_grace: Duration,
    /// Per-write timeout (Go: 10 s).
    pub write_timeout: Duration,
    /// Timeout of the endpoint request (Go: 10 s).
    pub http_timeout: Duration,
}

impl WsOptions {
    pub fn new(
        api_base: impl Into<String>,
        app_id: impl Into<String>,
        app_secret: impl Into<String>,
    ) -> Self {
        Self {
            api_base: api_base.into(),
            app_id: app_id.into(),
            app_secret: app_secret.into(),
            auto_reconnect: true,
            initial_config: ClientConfig::default(),
            pong_grace: Duration::from_secs(5),
            write_timeout: Duration::from_secs(10),
            http_timeout: Duration::from_secs(10),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum WsError {
    /// Not retried: bad credentials, forbidden, connection limit reached.
    #[error("feishu ws client error {code}: {msg}")]
    Client { code: i64, msg: String },
    /// Retried: busy, internal error, non-200 endpoint response, refused upgrade.
    #[error("feishu ws server error {code}: {msg}")]
    Server { code: i64, msg: String },
    #[error("feishu ws endpoint request: {0}")]
    Http(#[from] reqwest::Error),
    #[error("feishu ws transport: {0}")]
    Transport(String),
    #[error("feishu ws: no frame for {0:?}")]
    Timeout(Duration),
    #[error("feishu ws: unable to connect to server after {0} retries")]
    ReconnectExhausted(i64),
}

/// The test guard's refusal: a client error, so it is not retried.
fn forbidden(e: steve_modules::net::HostForbidden) -> WsError {
    WsError::Client {
        code: 0,
        msg: e.to_string(),
    }
}

impl WsError {
    /// Whether a reconnect may help (`isRetryableConnectionError`: everything but client errors).
    pub fn retryable(&self) -> bool {
        !matches!(self, Self::Client { .. })
    }
}

/// Connection state, published through [`WsHandle::state`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WsState {
    Connecting,
    /// `conn_id` is the URL's `device_id`.
    Connected {
        conn_id: String,
        service_id: i32,
    },
    Reconnecting {
        attempt: u32,
        last_error: String,
    },
    Stopped {
        error: Option<String>,
    },
}

/// Receives events. Must answer quickly (validate, de-dup, enqueue): the ack goes out when this
/// returns and Feishu redelivers after [`ACK_DEADLINE`]. `Ok(Some(v))` puts `v` (as JSON bytes)
/// into the ack's `data`; `Err` acks with 500.
pub trait WsHandler: Send + Sync + 'static {
    fn on_event(
        &self,
        payload: Vec<u8>,
    ) -> impl Future<Output = Result<Option<Value>, String>> + Send;
}

/// POSTs the endpoint request (`fetchEndpoint`).
pub async fn fetch_endpoint(
    http: &reqwest::Client,
    api_base: &str,
    app_id: &str,
    app_secret: &str,
) -> Result<Endpoint, WsError> {
    if app_secret.is_empty() {
        return Err(WsError::Client {
            code: 0,
            msg: "appSecret cannot be empty".into(),
        });
    }
    let url = format!("{}{}", api_base.trim_end_matches('/'), pbbp2::ENDPOINT_PATH);
    super::client::guard_url(&url).map_err(forbidden)?;
    let body = serde_json::json!({"AppID": app_id, "AppSecret": app_secret, "ClientAssertion": ""});
    let resp = http
        .post(url)
        .header("locale", "zh")
        .json(&body)
        .send()
        .await?;
    let status = resp.status();
    let bytes = resp.bytes().await?;
    let v: Value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    let msg = v
        .get("msg")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    if status != reqwest::StatusCode::OK {
        return Err(WsError::Server {
            code: i64::from(status.as_u16()),
            msg: if msg.is_empty() {
                "system busy".into()
            } else {
                msg
            },
        });
    }
    if v.is_null() {
        return Err(WsError::Server {
            code: 500,
            msg: "endpoint response is not JSON".into(),
        });
    }
    match v.get("code").and_then(Value::as_i64).unwrap_or(0) {
        code::OK => {}
        code::SYSTEM_BUSY => {
            return Err(WsError::Server {
                code: code::SYSTEM_BUSY,
                msg: "system busy".into(),
            });
        }
        code::INTERNAL_ERROR => {
            return Err(WsError::Server {
                code: code::INTERNAL_ERROR,
                msg,
            });
        }
        other => return Err(WsError::Client { code: other, msg }),
    }
    let data = v.get("data").cloned().unwrap_or(Value::Null);
    let url = data
        .get("URL")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| WsError::Server {
            code: 500,
            msg: "endpoint is null".into(),
        })?
        .to_string();
    let config = data
        .get("ClientConfig")
        .filter(|c| c.is_object())
        .and_then(|c| serde_json::from_value::<RawClientConfig>(c.clone()).ok())
        .map(|raw| {
            let mut c = ClientConfig::default();
            c.apply(raw);
            c
        });
    Ok(Endpoint { url, config })
}

/// Query parameter `key` of `url` (ids are plain digits / tokens, no decoding needed).
fn query_param<'a>(url: &'a str, key: &str) -> Option<&'a str> {
    let q = url.split_once('?')?.1;
    let q = q.split('#').next().unwrap_or(q);
    q.split('&')
        .filter_map(|kv| kv.split_once('='))
        .find(|(k, _)| *k == key)
        .map(|(_, v)| v)
}

/// Maps a refused upgrade to an error (`parseHandshakeError`).
fn handshake_error(resp: &tungstenite::http::Response<Option<Vec<u8>>>) -> WsError {
    let h = |k: &str| {
        resp.headers()
            .get(k)
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .to_string()
    };
    let status: i64 = h(header::HANDSHAKE_STATUS)
        .parse()
        .ok()
        .filter(|c| *c != 0)
        .unwrap_or_else(|| i64::from(resp.status().as_u16()));
    let msg = h(header::HANDSHAKE_MSG);
    match status {
        code::AUTH_FAILED
            if h(header::HANDSHAKE_AUTH_ERR_CODE).parse::<i64>().ok()
                == Some(code::EXCEED_CONN_LIMIT) =>
        {
            WsError::Client { code: status, msg }
        }
        code::FORBIDDEN => WsError::Client { code: status, msg },
        _ => WsError::Server { code: status, msg },
    }
}

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

struct Conn {
    socket: Socket,
    conn_id: String,
    service_id: i32,
}

/// Last-touched time and the parts received so far (`None` = missing).
type Partial = (Instant, Vec<Option<Vec<u8>>>);

/// Parts of split messages waiting for their siblings.
#[derive(Default)]
struct Parts {
    by_id: HashMap<String, Partial>,
}

impl Parts {
    /// Stores one part; returns the whole payload once every part is present.
    fn combine(&mut self, msg_id: &str, sum: i64, seq: i64, part: Vec<u8>) -> Option<Vec<u8>> {
        let now = Instant::now();
        self.by_id
            .retain(|_, (t, _)| now.duration_since(*t) < PART_TTL);
        if !(2..=MAX_PARTS).contains(&sum) || !(0..sum).contains(&seq) {
            tracing::warn!(
                msg_id,
                sum,
                seq,
                "feishu ws: dropping part with bad sum/seq"
            );
            return None;
        }
        let sum = sum as usize;
        let entry = self
            .by_id
            .entry(msg_id.to_string())
            .or_insert_with(|| (now, vec![None; sum]));
        if entry.1.len() != sum {
            *entry = (now, vec![None; sum]);
        }
        // Go refreshes the 5 s expiry on every part.
        entry.0 = now;
        entry.1[seq as usize] = Some(part);
        if entry.1.iter().any(Option::is_none) {
            return None;
        }
        let (_, parts) = self.by_id.remove(msg_id)?;
        Some(parts.into_iter().flatten().flatten().collect())
    }
}

struct Inner<H> {
    opts: WsOptions,
    handler: Arc<H>,
    http: reqwest::Client,
    config: Mutex<ClientConfig>,
    parts: Mutex<Parts>,
    state: watch::Sender<WsState>,
}

/// A running client. Dropping the handle does not stop it; call [`WsHandle::stop`].
pub struct WsHandle {
    state: watch::Receiver<WsState>,
    stop: CancellationToken,
    task: tokio::task::JoinHandle<Result<(), WsError>>,
}

impl WsHandle {
    /// Current state.
    pub fn state(&self) -> WsState {
        self.state.borrow().clone()
    }

    /// A receiver to watch state changes (e.g. for `health()`).
    pub fn subscribe(&self) -> watch::Receiver<WsState> {
        self.state.clone()
    }

    /// Waits until `pred` holds for the state, or `timeout` passes. Returns the last state.
    pub async fn wait_for(
        &self,
        timeout: Duration,
        mut pred: impl FnMut(&WsState) -> bool,
    ) -> Result<WsState, WsState> {
        let mut rx = self.state.clone();
        let r = tokio::time::timeout(timeout, rx.wait_for(|s| pred(s))).await;
        match r {
            Ok(Ok(s)) => Ok(s.clone()),
            _ => Err(self.state()),
        }
    }

    /// Whether the run has ended (stopped or failed for good).
    pub fn is_finished(&self) -> bool {
        self.task.is_finished()
    }

    /// Stops the client and returns the run's result.
    pub async fn stop(self) -> Result<(), WsError> {
        self.stop.cancel();
        self.join().await
    }

    /// Waits for the run to end on its own (non-retryable error or exhausted reconnects).
    pub async fn join(self) -> Result<(), WsError> {
        match self.task.await {
            Ok(r) => r,
            Err(e) => Err(WsError::Transport(format!("ws task: {e}"))),
        }
    }
}

/// Starts the client on the current tokio runtime.
pub fn spawn<H: WsHandler>(opts: WsOptions, handler: Arc<H>) -> Result<WsHandle, WsError> {
    let http = reqwest::Client::builder()
        .timeout(opts.http_timeout)
        .build()?;
    let (state_tx, state_rx) = watch::channel(WsState::Connecting);
    let stop = CancellationToken::new();
    let inner = Arc::new(Inner {
        config: Mutex::new(opts.initial_config),
        opts,
        handler,
        http,
        parts: Mutex::new(Parts::default()),
        state: state_tx,
    });
    let token = stop.clone();
    let task = tokio::spawn(async move {
        let r = inner.clone().run(token).await;
        let error = r.as_ref().err().map(ToString::to_string);
        inner.state.send_replace(WsState::Stopped { error });
        r
    });
    Ok(WsHandle {
        state: state_rx,
        stop,
        task,
    })
}

/// Sleeps `d` unless stopped first; false when stopped.
async fn sleep_or_stop(d: Duration, stop: &CancellationToken) -> bool {
    tokio::select! {
        biased;
        _ = stop.cancelled() => false,
        _ = tokio::time::sleep(d) => true,
    }
}

/// Why one connection ended.
enum Exit {
    Stopped,
    Failed(WsError),
}

impl<H: WsHandler> Inner<H> {
    fn config(&self) -> ClientConfig {
        *self.config.lock().unwrap_or_else(|e| e.into_inner())
    }

    fn set_config(&self, c: ClientConfig) {
        *self.config.lock().unwrap_or_else(|e| e.into_inner()) = c;
    }

    /// `runCoordinator`.
    async fn run(self: Arc<Self>, stop: CancellationToken) -> Result<(), WsError> {
        let mut conn = match self.establish().await {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!(error = %e, "feishu ws: initial connection failed");
                match self.reconnect(e, &stop).await? {
                    Some(c) => c,
                    None => return Ok(()),
                }
            }
        };
        loop {
            match self.serve(conn, &stop).await {
                Exit::Stopped => return Ok(()),
                Exit::Failed(e) => {
                    tracing::warn!(error = %e, "feishu ws: connection failed");
                    conn = match self.reconnect(e, &stop).await? {
                        Some(c) => c,
                        None => return Ok(()),
                    };
                }
            }
        }
    }

    /// `reconnectAfterFailure`: `Ok(None)` when stopped meanwhile.
    async fn reconnect(
        &self,
        mut failure: WsError,
        stop: &CancellationToken,
    ) -> Result<Option<Conn>, WsError> {
        if !self.opts.auto_reconnect || !failure.retryable() {
            return Err(failure);
        }
        let mut attempts: u32 = 0;
        loop {
            let cfg = self.config();
            if cfg.reconnect_count >= 0 && i64::from(attempts) >= cfg.reconnect_count {
                return Err(WsError::ReconnectExhausted(cfg.reconnect_count));
            }
            let delay = if attempts == 0 {
                random_below(cfg.reconnect_nonce)
            } else {
                cfg.reconnect_interval
            };
            self.state.send_replace(WsState::Reconnecting {
                attempt: attempts + 1,
                last_error: failure.to_string(),
            });
            if !sleep_or_stop(delay, stop).await {
                return Ok(None);
            }
            attempts += 1;
            tracing::info!(attempt = attempts, "feishu ws: reconnecting");
            match self.establish().await {
                Ok(c) => return Ok(Some(c)),
                Err(e) if !e.retryable() => return Err(e),
                Err(e) => {
                    tracing::warn!(attempt = attempts, error = %e, "feishu ws: reconnect failed");
                    failure = e;
                }
            }
        }
    }

    /// `establishConnectionOnce`: endpoint, config, dial.
    async fn establish(&self) -> Result<Conn, WsError> {
        let ep = fetch_endpoint(
            &self.http,
            &self.opts.api_base,
            &self.opts.app_id,
            &self.opts.app_secret,
        )
        .await?;
        if let Some(c) = ep.config {
            self.set_config(c);
        }
        let scheme_ok = ep.url.starts_with("ws://") || ep.url.starts_with("wss://");
        if !scheme_ok {
            return Err(WsError::Server {
                code: 500,
                msg: "websocket endpoint is invalid".into(),
            });
        }
        let conn_id = query_param(&ep.url, "device_id")
            .unwrap_or_default()
            .to_string();
        let service_id = match query_param(&ep.url, "service_id") {
            None | Some("") => 0,
            Some(s) => s.parse::<i32>().map_err(|_| WsError::Server {
                code: 500,
                msg: "websocket endpoint has a bad service_id".into(),
            })?,
        };
        super::client::guard_url(&ep.url).map_err(forbidden)?;
        let socket = match tokio_tungstenite::connect_async(ep.url.as_str()).await {
            Ok((s, _)) => s,
            Err(tungstenite::Error::Http(resp)) => return Err(handshake_error(&resp)),
            Err(e) => return Err(WsError::Transport(e.to_string())),
        };
        Ok(Conn {
            socket,
            conn_id,
            service_id,
        })
    }

    /// Runs one connection until it fails or the client is stopped.
    async fn serve(self: &Arc<Self>, conn: Conn, stop: &CancellationToken) -> Exit {
        let Conn {
            socket,
            conn_id,
            service_id,
        } = conn;
        tracing::info!(conn_id, "feishu ws: connected");
        self.state.send_replace(WsState::Connected {
            conn_id: conn_id.clone(),
            service_id,
        });
        let (mut sink, mut stream) = socket.split();
        let (out_tx, mut out_rx) = mpsc::channel::<Vec<u8>>(64);
        let (fail_tx, mut fail_rx) = mpsc::channel::<WsError>(1);
        let write_timeout = self.opts.write_timeout;
        let writer = tokio::spawn(async move {
            while let Some(bytes) = out_rx.recv().await {
                let send = sink.send(WsMessage::Binary(bytes.into()));
                let err = match tokio::time::timeout(write_timeout, send).await {
                    Ok(Ok(())) => continue,
                    Ok(Err(e)) => WsError::Transport(e.to_string()),
                    Err(_) => WsError::Transport("write timed out".into()),
                };
                let _ = fail_tx.try_send(err);
                break;
            }
            let _ = sink.close().await;
        });

        let _ = out_tx.send(Frame::ping(service_id).to_bytes()).await;
        let mut next_ping = tokio::time::Instant::now() + self.config().ping_interval;
        let mut deadline = tokio::time::Instant::now() + self.read_window();
        let exit = loop {
            tokio::select! {
                _ = stop.cancelled() => break Exit::Stopped,
                Some(e) = fail_rx.recv() => break Exit::Failed(e),
                _ = tokio::time::sleep_until(next_ping) => {
                    let _ = out_tx.send(Frame::ping(service_id).to_bytes()).await;
                    next_ping = tokio::time::Instant::now() + self.config().ping_interval;
                }
                _ = tokio::time::sleep_until(deadline) => {
                    break Exit::Failed(WsError::Timeout(self.read_window()));
                }
                msg = stream.next() => {
                    let msg = match msg {
                        None => break Exit::Failed(WsError::Transport("connection closed".into())),
                        Some(Err(e)) => break Exit::Failed(WsError::Transport(e.to_string())),
                        Some(Ok(m)) => m,
                    };
                    deadline = tokio::time::Instant::now() + self.read_window();
                    match msg {
                        // Handled inline so events reach the handler in arrival order; the handler
                        // only enqueues, and the ACK waits on the writer like a ping does.
                        WsMessage::Binary(b) => {
                            self.clone().handle_message(b.to_vec(), out_tx.clone()).await;
                        }
                        WsMessage::Close(f) => {
                            break Exit::Failed(WsError::Transport(format!("closed by server: {f:?}")));
                        }
                        // Pings are answered by tungstenite; text frames aren't part of the protocol.
                        WsMessage::Text(_) => tracing::warn!("feishu ws: unexpected text message"),
                        _ => {}
                    }
                }
            }
        };
        drop(out_tx);
        writer.abort();
        tracing::info!(conn_id, "feishu ws: disconnected");
        exit
    }

    /// `pongWait`: how long the connection may stay silent.
    fn read_window(&self) -> Duration {
        2 * self.config().ping_interval + self.opts.pong_grace
    }

    /// `handleMessageTask`: decode, then control or data.
    async fn handle_message(self: Arc<Self>, bytes: Vec<u8>, out: mpsc::Sender<Vec<u8>>) {
        let frame = match Frame::from_bytes(&bytes) {
            Ok(f) => f,
            Err(e) => {
                tracing::error!(error = %e, "feishu ws: frame decode failed");
                return;
            }
        };
        match frame.method {
            pbbp2::METHOD_CONTROL => self.handle_control(&frame),
            pbbp2::METHOD_DATA => self.handle_data(frame, out).await,
            _ => {}
        }
    }

    fn handle_control(&self, frame: &Frame) {
        if frame.kind() != kind::PONG {
            return;
        }
        let Some(payload) = frame.payload.as_deref().filter(|p| !p.is_empty()) else {
            return;
        };
        match serde_json::from_slice::<RawClientConfig>(payload) {
            Ok(raw) => {
                let mut c = self.config();
                c.apply(raw);
                self.set_config(c);
            }
            Err(_) => tracing::warn!("feishu ws: client config decode failed"),
        }
    }

    async fn handle_data(&self, mut frame: Frame, out: mpsc::Sender<Vec<u8>>) {
        let sum = frame.header_int(header::SUM);
        let seq = frame.header_int(header::SEQ);
        let msg_id = frame.header(header::MESSAGE_ID).unwrap_or("").to_string();
        let mut payload = frame.payload.take().unwrap_or_default();
        if sum > 1 {
            let whole = self
                .parts
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .combine(&msg_id, sum, seq, payload);
            match whole {
                Some(p) => payload = p,
                None => return,
            }
        }
        if frame.kind() != kind::EVENT {
            tracing::debug!(
                kind = frame.kind(),
                msg_id,
                "feishu ws: ignoring non-event frame"
            );
            return;
        }
        let started = Instant::now();
        let result = self.handler.on_event(payload).await;
        let took = started.elapsed();
        if took > ACK_DEADLINE {
            tracing::warn!(
                msg_id,
                ?took,
                "feishu ws: handler exceeded the 3 s ack deadline"
            );
        }
        frame.add_header(header::BIZ_RT, took.as_millis().to_string());
        let body = match result {
            Ok(None) => pbbp2::response_payload(200, None),
            Ok(Some(v)) => match serde_json::to_vec(&v) {
                Ok(d) => pbbp2::response_payload(200, Some(&d)),
                Err(_) => pbbp2::response_payload(500, None),
            },
            Err(e) => {
                tracing::error!(msg_id, error = %e, "feishu ws: handler failed");
                pbbp2::response_payload(500, None)
            }
        };
        frame.payload = Some(body);
        if out.send(frame.to_bytes()).await.is_err() {
            tracing::error!(msg_id, "feishu ws: ack not sent, connection gone");
        }
    }
}

/// Uniform in `[0, d)`, millisecond resolution; zero for zero.
fn random_below(d: Duration) -> Duration {
    let ms = d.as_millis() as u64;
    if ms == 0 {
        return Duration::ZERO;
    }
    Duration::from_millis(rand::random_range(0..ms))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn combine_out_of_order_and_bad_parts() {
        let mut p = Parts::default();
        assert_eq!(p.combine("m", 3, 2, b"c".to_vec()), None);
        assert_eq!(p.combine("m", 3, 0, b"a".to_vec()), None);
        assert_eq!(p.combine("m", 3, 1, b"b".to_vec()), Some(b"abc".to_vec()));
        assert!(p.by_id.is_empty());
        assert_eq!(p.combine("x", 2, 5, b"z".to_vec()), None);
        assert_eq!(p.combine("x", 99_999, 0, b"z".to_vec()), None);
    }

    #[test]
    fn config_apply_keeps_ping_on_zero() {
        let mut c = ClientConfig::default();
        c.apply(RawClientConfig {
            reconnect_count: 3,
            reconnect_interval: 7,
            reconnect_nonce: 0,
            ping_interval: 0,
        });
        assert_eq!(c.reconnect_count, 3);
        assert_eq!(c.reconnect_interval, Duration::from_secs(7));
        assert_eq!(c.reconnect_nonce, Duration::ZERO);
        assert_eq!(c.ping_interval, Duration::from_secs(120));
    }

    #[test]
    fn query_params() {
        let u = "wss://h/ws/v2?fpid=1&device_id=77&service_id=33554678&ticket=t";
        assert_eq!(query_param(u, "device_id"), Some("77"));
        assert_eq!(query_param(u, "service_id"), Some("33554678"));
        assert_eq!(query_param("wss://h/ws", "device_id"), None);
    }

    #[test]
    fn handshake_errors() {
        let resp = |status: &str, auth: &str| {
            tungstenite::http::Response::builder()
                .status(400)
                .header(header::HANDSHAKE_STATUS, status)
                .header(header::HANDSHAKE_MSG, "m")
                .header(header::HANDSHAKE_AUTH_ERR_CODE, auth)
                .body(None)
                .unwrap()
        };
        assert!(!handshake_error(&resp("514", "1000040350")).retryable());
        assert!(handshake_error(&resp("514", "1")).retryable());
        assert!(!handshake_error(&resp("403", "")).retryable());
        assert!(handshake_error(&resp("", "")).retryable());
    }
}
