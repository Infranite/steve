//! [`FakeExtWs`]: an in-process channel service speaking the external channel protocol
//! ([`crate::adapters::ext_ws::protocol`]) as its server side, for the Hub's conformance tests
//! (§11.3). It keeps the messages a test puts in, a feed of batches with seqs and cursors, the
//! writes the Hub made, and every request in order; a test scripts the rest (a lost ack, a
//! rate limit, another person signed in, a session status, a closed socket).

use std::collections::{HashMap, HashSet};
use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::Router;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::adapters::ext_ws::wire::{self, Reason, RpcError};

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// One batch of the feed, kept as sent: a redelivery repeats it exactly.
#[derive(Debug, Clone, PartialEq)]
pub struct Batch {
    pub seq: u64,
    pub cursor: String,
    pub items: Vec<Value>,
}

/// What a test scripts and reads.
pub struct World {
    pub token: String,
    pub realm: String,
    /// Independent declarations allow regression tests of hello/account disagreements.
    pub identity: Option<String>,
    pub account_identity: Option<String>,
    /// The primary handle `me.get` answers.
    pub me: Value,
    pub me_display: String,
    pub me_aliases: Vec<Value>,
    /// Who is signed in now: writes whose `expected_sender` differs get `account_changed`.
    pub signed_in: Value,
    pub versions: Vec<u32>,
    pub features: Vec<String>,
    pub connector: Option<Value>,
    pub connector_results: HashMap<String, Value>,
    /// Execute one connector write, then close before its response (unknown outcome).
    pub lose_connector_response: bool,
    pub limits: Value,
    pub write_policy: String,
    pub source_only: bool,
    pub session: String,
    /// The fake's clock (unix ms); `None`: the real one.
    pub now: Option<i64>,
    /// Every full message, by insertion.
    pub messages: Vec<Value>,
    pub chats: HashMap<String, Value>,
    pub people: HashMap<String, Value>,
    pub media: HashMap<String, Vec<u8>>,
    pub batches: Vec<Batch>,
    /// Cursors answered `cursor_too_old`.
    pub too_old: HashSet<String>,
    /// History reads answer `complete: false`.
    pub incomplete: bool,
    /// The next write answers `rate_limited` with this hint.
    pub rate_limit_next: Option<u64>,
    /// Acks are taken but forgotten (a lost ack: the batch is redelivered).
    pub lose_acks: bool,
    /// Every request, in order: `(method, params)`.
    pub log: Vec<(String, Value)>,
    /// Writes the remote carried out: `(method, params, result)`.
    pub writes: Vec<(String, Value, Value)>,
    idem: HashMap<String, (Value, Value)>,
    next_seq: u64,
    acked: u64,
    pub upgrades: usize,
    pub refused_upgrades: usize,
    sockets: Vec<Sock>,
    next_out: u64,
}

struct Sock {
    tx: mpsc::UnboundedSender<Message>,
    sub: Option<String>,
    /// The highest seq sent to it.
    sent: u64,
    hello: bool,
}

impl World {
    fn now(&self) -> i64 {
        self.now.unwrap_or_else(steve_store::now)
    }

    fn window(&self) -> u64 {
        self.limits
            .get("max_unacked_batches")
            .and_then(Value::as_u64)
            .unwrap_or(4)
    }

    /// Sends every socket the batches it may have now.
    fn pump(&mut self) {
        let window = self.window();
        let (acked, batches) = (self.acked, self.batches.clone());
        for s in &mut self.sockets {
            let Some(sub) = s.sub.clone() else { continue };
            let from = s.sent;
            for b in batches.iter().filter(|b| b.seq > from) {
                if b.seq.saturating_sub(acked) > window {
                    break;
                }
                let _ = s.tx.send(text(wire::notification(
                    "changes.batch",
                    json!({"subscription": sub, "seq": b.seq, "cursor": b.cursor, "items": b.items}),
                )));
                s.sent = b.seq;
            }
        }
    }
}

fn text(s: String) -> Message {
    Message::Text(s.into())
}

/// The fake service on `127.0.0.1`, plain `ws://` (allowed to a literal loopback address).
#[derive(Clone)]
pub struct FakeExtWs {
    addr: SocketAddr,
    pub world: Arc<Mutex<World>>,
}

/// The access token the fake accepts by default.
pub const TOKEN: &str = "fake-token-0123456789";
pub const REALM: &str = "acme-7f3a";
pub const ME: &str = "u_123";

impl FakeExtWs {
    pub async fn start() -> std::io::Result<Self> {
        Self::start_at(SocketAddr::from(([127, 0, 0, 1], 0))).await
    }

    /// The fake on an address it is given (a service started where it was told to listen).
    pub async fn start_at(at: SocketAddr) -> std::io::Result<Self> {
        let world = World {
            token: TOKEN.into(),
            realm: REALM.into(),
            identity: None,
            account_identity: None,
            me: json!({"kind": "id", "value": ME}),
            me_display: "Ada Lovelace".into(),
            me_aliases: vec![json!({"kind": "email", "value": "ada@example.com"})],
            signed_in: json!({"kind": "id", "value": ME}),
            versions: vec![1],
            features: [
                "me",
                "chats.get",
                "messages.get",
                "messages.history",
                "changes",
                "media.download",
                "messages.send",
                "messages.reply",
                "messages.recall",
                "reactions",
                "people.get",
            ]
            .iter()
            .map(|s| s.to_string())
            .collect(),
            connector: None,
            connector_results: HashMap::new(),
            lose_connector_response: false,
            limits: json!({"max_frame_bytes": 1_048_576, "max_text_bytes": 153_600,
                "max_batch_items": 100, "max_unacked_batches": 4, "max_ids_per_call": 50,
                "history_max_limit": 50, "write_per_sec": 5, "write_burst": 5,
                "max_attachment_bytes": 104_857_600, "max_inflight_requests": 32}),
            write_policy: "allowed".into(),
            source_only: false,
            session: "ok".into(),
            now: None,
            messages: Vec::new(),
            chats: HashMap::new(),
            people: HashMap::new(),
            media: HashMap::new(),
            batches: Vec::new(),
            too_old: HashSet::new(),
            incomplete: false,
            rate_limit_next: None,
            lose_acks: false,
            log: Vec::new(),
            writes: Vec::new(),
            idem: HashMap::new(),
            next_seq: 1,
            acked: 0,
            upgrades: 0,
            refused_upgrades: 0,
            sockets: Vec::new(),
            next_out: 1,
        };
        let world = Arc::new(Mutex::new(world));
        let app = Router::new()
            .route("/ws", get(upgrade))
            .route("/v1/media/{key}", get(media))
            .with_state(world.clone());
        let listener = tokio::net::TcpListener::bind(at).await?;
        let addr = listener.local_addr()?;
        tokio::spawn(async move {
            let _ = axum::serve(listener, app).await;
        });
        Ok(Self { addr, world })
    }

    /// `ws://127.0.0.1:<port>/ws`.
    pub fn url(&self) -> String {
        format!("ws://{}/ws", self.addr)
    }

    pub fn with<R>(&self, f: impl FnOnce(&mut World) -> R) -> R {
        f(&mut lock(&self.world))
    }

    /// A full message the remote knows (for `messages.get` and `messages.history`).
    pub fn add_message(&self, m: Value) {
        self.with(|w| w.messages.push(m));
    }

    /// A new batch of these items, sent to every subscribed socket its window allows.
    pub fn push(&self, items: Vec<Value>) -> u64 {
        self.with(|w| {
            let seq = w.next_seq;
            w.next_seq += 1;
            w.batches.push(Batch {
                seq,
                cursor: format!("c:{seq}"),
                items,
            });
            w.pump();
            seq
        })
    }

    /// A message the remote knows, and its feed item (metadata only).
    pub fn push_message(&self, m: Value) -> u64 {
        self.add_message(m.clone());
        self.push(vec![item_of(&m)])
    }

    /// Every unacked batch again, as it was (an ack timeout).
    pub fn redeliver(&self) {
        self.with(|w| {
            let acked = w.acked;
            for s in &mut w.sockets {
                s.sent = s.sent.min(acked);
            }
            w.pump();
        });
    }

    /// A notification to every socket.
    pub fn notify(&self, method: &str, params: Value) {
        self.with(|w| {
            for s in &w.sockets {
                let _ = s.tx.send(text(wire::notification(method, params.clone())));
            }
        });
    }

    /// Closes every socket (a crash of the service).
    pub fn drop_all(&self) {
        self.with(|w| {
            for s in w.sockets.drain(..) {
                let _ = s.tx.send(Message::Close(None));
            }
        });
    }

    /// The highest acked seq.
    pub fn acked(&self) -> u64 {
        self.with(|w| w.acked)
    }

    /// The requests of `method`, in order.
    pub fn calls(&self, method: &str) -> Vec<Value> {
        self.with(|w| {
            w.log
                .iter()
                .filter(|(m, _)| m == method)
                .map(|(_, p)| p.clone())
                .collect()
        })
    }

    pub fn sockets(&self) -> usize {
        self.with(|w| w.sockets.len())
    }
}

/// A feed item of a full message: its metadata, no body.
pub fn item_of(m: &Value) -> Value {
    let mut it = json!({"type": "message"});
    for k in [
        "chat_id",
        "source_id",
        "id",
        "created_ms",
        "sender",
        "is_mine",
        "chat_kind",
        "thread_id",
    ] {
        if let Some(v) = m.get(k) {
            it[k] = v.clone();
        }
    }
    if it.get("is_mine").is_none() {
        it["is_mine"] = false.into();
    }
    it
}

/// A full message of `chat` by user `from`.
pub fn message(chat: &str, source: &str, from: &str, text: &str, created_ms: i64) -> Value {
    json!({"chat_id": chat, "source_id": source, "id": format!("m_{source}"),
        "created_ms": created_ms, "sender": {"id": {"kind": "id", "value": from},
        "display": from}, "is_mine": false, "chat_kind": "group", "chat_name": format!("Chat {chat}"),
        "kind": "text", "text": text})
}

fn bearer_ok(w: &World, h: &HeaderMap) -> bool {
    h.get("authorization").and_then(|v| v.to_str().ok()) == Some(&format!("Bearer {}", w.token))
}

async fn upgrade(
    State(world): State<Arc<Mutex<World>>>,
    headers: HeaderMap,
    ws: WebSocketUpgrade,
) -> Response {
    {
        let mut w = lock(&world);
        if !bearer_ok(&w, &headers) {
            w.refused_upgrades += 1;
            return StatusCode::UNAUTHORIZED.into_response();
        }
        w.upgrades += 1;
    }
    ws.max_message_size(wire::MAX_FRAME)
        .on_upgrade(move |socket| serve(socket, world))
}

async fn media(
    State(world): State<Arc<Mutex<World>>>,
    Path(key): Path<String>,
    headers: HeaderMap,
) -> Response {
    let w = lock(&world);
    if !bearer_ok(&w, &headers) {
        return StatusCode::UNAUTHORIZED.into_response();
    }
    let Some(body) = w.media.get(&key).cloned() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let max = headers
        .get("x-max-bytes")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<usize>().ok())
        .unwrap_or(usize::MAX);
    if body.len() > max {
        return StatusCode::PAYLOAD_TOO_LARGE.into_response();
    }
    ([("content-type", "application/octet-stream")], body).into_response()
}

async fn serve(socket: WebSocket, world: Arc<Mutex<World>>) {
    use futures_util::{SinkExt, StreamExt};
    let (mut sink, mut stream) = socket.split();
    let (tx, mut rx) = mpsc::unbounded_channel::<Message>();
    let idx = {
        let mut w = lock(&world);
        w.sockets.push(Sock {
            tx: tx.clone(),
            sub: None,
            sent: 0,
            hello: false,
        });
        w.sockets.len() - 1
    };
    let me = tx.clone();
    let writer = tokio::spawn(async move {
        while let Some(m) = rx.recv().await {
            let last = matches!(m, Message::Close(_));
            if sink.send(m).await.is_err() || last {
                break;
            }
        }
    });
    let hello_deadline = tokio::time::sleep(Duration::from_secs(10));
    tokio::pin!(hello_deadline);
    loop {
        let msg = tokio::select! {
            m = stream.next() => m,
            _ = &mut hello_deadline, if !said_hello(&world, &me) => {
                close(&me, 1008, "no hello");
                break;
            }
        };
        let Some(Ok(msg)) = msg else { break };
        let t = match msg {
            Message::Text(t) => t.to_string(),
            Message::Close(_) => break,
            Message::Binary(_) => {
                close(&me, 1003, "binary");
                break;
            }
            _ => continue,
        };
        let v: Value = match serde_json::from_str(&t) {
            Ok(v) => v,
            Err(_) => {
                close(&me, 1003, "not json");
                break;
            }
        };
        let (Some(id), Some(method)) = (
            v.get("id").cloned(),
            v.get("method").and_then(Value::as_str),
        ) else {
            continue;
        };
        let params = v.get("params").cloned().unwrap_or(Value::Null);
        let first = !said_hello(&world, &me);
        if first && method != "hello" {
            let _ = me.send(text(wire::response(
                &id,
                Err(RpcError::new(Reason::NotReady, "hello first")),
            )));
            close(&me, 1008, "hello first");
            break;
        }
        let r = answer(&world, &me, method, &params);
        if method == "connector.call" && lock(&world).lose_connector_response {
            lock(&world).lose_connector_response = false;
            close(&me, 1011, "lost connector reply");
            break;
        }
        let stop = matches!(&r, Err(e) if e.reason == Reason::UnsupportedVersion);
        let _ = me.send(text(wire::response(&id, r)));
        if stop {
            close(&me, 1000, "unsupported version");
            break;
        }
        lock(&world).pump();
    }
    {
        let mut w = lock(&world);
        w.sockets.retain(|s| !s.tx.same_channel(&me));
    }
    let _ = idx;
    drop(tx);
    let _ = writer.await;
}

fn said_hello(world: &Mutex<World>, me: &mpsc::UnboundedSender<Message>) -> bool {
    lock(world)
        .sockets
        .iter()
        .any(|s| s.tx.same_channel(me) && s.hello)
}

fn close(me: &mpsc::UnboundedSender<Message>, code: u16, reason: &str) {
    let _ = me.send(Message::Close(Some(CloseFrame {
        code,
        reason: reason.to_string().into(),
    })));
}

fn err(r: Reason, m: &str) -> Result<Value, RpcError> {
    Err(RpcError::new(r, m))
}

fn answer(
    world: &Mutex<World>,
    me: &mpsc::UnboundedSender<Message>,
    method: &str,
    p: &Value,
) -> Result<Value, RpcError> {
    let mut w = lock(world);
    w.log.push((method.to_string(), p.clone()));
    let now = w.now();
    match method {
        "hello" => {
            let min = p["protocol"]["min"].as_u64().unwrap_or(1) as u32;
            let max = p["protocol"]["max"].as_u64().unwrap_or(1) as u32;
            match wire::negotiate(min, max, &w.versions) {
                Ok(v) => {
                    for s in &mut w.sockets {
                        if s.tx.same_channel(me) {
                            s.hello = true;
                        }
                    }
                    let mut result = json!({"protocol": v, "server": {"name": "fake", "version": "1"},
                        "realm": w.realm, "time_precision_ms": 1, "time_ms": now,
                        "features": w.features, "limits": w.limits,
                        "session": {"status": w.session}, "upstream": {"status": "ok"},
                        "write_policy": w.write_policy, "connector": w.connector});
                    if let Some(identity) = &w.identity {
                        result["identity"] = identity.clone().into();
                    }
                    Ok(result)
                }
                Err(supported) => Err(RpcError {
                    reason: Reason::UnsupportedVersion,
                    message: "no common version".into(),
                    data: json!({"reason": "unsupported_version", "supported": supported}),
                }),
            }
        }
        "connector.call" => {
            let name = p["name"].as_str().unwrap_or("");
            let declared = w
                .connector
                .as_ref()
                .and_then(|m| m["operations"].as_array())
                .and_then(|ops| ops.iter().find(|o| o["name"] == name));
            let Some(op) = declared else {
                return err(Reason::Unsupported, "operation not declared");
            };
            let write = op["annotations"]["readOnlyHint"] != true;
            if write
                && (p["expected_sender"] != w.signed_in
                    || w.write_policy != "allowed"
                    || p["expected_identity"].as_str()
                        != Some(w.account_identity.as_deref().unwrap_or("self")))
            {
                return err(Reason::WriteForbidden, "sender/write policy");
            }
            let result = w
                .connector_results
                .get(name)
                .cloned()
                .unwrap_or(json!({"ok": true}));
            if write {
                w.writes.push((method.into(), p.clone(), result.clone()));
            }
            Ok(result)
        }
        "ping" => Ok(json!({"time_ms": now})),
        "me.get" => {
            let mut result = json!({"realm": w.realm, "user": {"id": w.signed_in,
                "display": w.me_display, "aliases": w.me_aliases}});
            if let Some(identity) = &w.account_identity {
                result["identity"] = identity.clone().into();
            }
            Ok(result)
        }
        "chats.get" => {
            let chat = p["chat_id"].as_str().unwrap_or_default();
            match w.chats.get(chat) {
                Some(c) => Ok(c.clone()),
                None => Ok(
                    json!({"chat_id": chat, "kind": "group", "name": format!("Chat {chat}"),
                    "member_count": 3, "has_external": false}),
                ),
            }
        }
        "messages.get" => {
            let items = p["items"].as_array().cloned().unwrap_or_default();
            let out: Vec<Value> = items
                .iter()
                .map(|it| {
                    w.messages
                        .iter()
                        .find(|m| {
                            m["chat_id"] == it["chat_id"]
                                && (m["source_id"] == it["source_id"]
                                    || (!it["id"].is_null() && m["id"] == it["id"]))
                        })
                        .cloned()
                        .unwrap_or(Value::Null)
                })
                .collect();
            Ok(json!({"messages": out}))
        }
        "messages.history" => {
            let chat = p["chat_id"].as_str().unwrap_or_default();
            let (from, to) = (
                p["from_ms"].as_i64().unwrap_or(0),
                p["to_ms"].as_i64().unwrap_or(0),
            );
            let limit = p["limit"].as_u64().unwrap_or(50) as usize;
            let skip = p["page_token"]
                .as_str()
                .and_then(|t| t.strip_prefix("p:"))
                .and_then(|n| n.parse::<usize>().ok())
                .unwrap_or(0);
            let mut all: Vec<Value> = w
                .messages
                .iter()
                .filter(|m| {
                    let c = m["created_ms"].as_i64().unwrap_or(0);
                    m["chat_id"] == chat && c >= from && c < to
                })
                .cloned()
                .collect();
            all.sort_by_key(|m| m["created_ms"].as_i64().unwrap_or(0));
            let page: Vec<Value> = all.iter().skip(skip).take(limit).cloned().collect();
            let next = (skip + limit < all.len()).then(|| format!("p:{}", skip + limit));
            Ok(json!({"messages": page, "next_page_token": next, "complete": !w.incomplete}))
        }
        "changes.subscribe" => {
            let sub = format!("s_{}", w.next_seq);
            let next_seq = w.next_seq;
            let max = w.next_seq - 1;
            let (from_seq, resumed) = match &p["from"] {
                Value::String(s) if s == "now" => (max, false),
                Value::Object(o) => {
                    let c = o.get("cursor").and_then(Value::as_str).unwrap_or_default();
                    if w.too_old.contains(c) {
                        return err(Reason::CursorTooOld, "cursor too old");
                    }
                    match c.strip_prefix("c:").and_then(|n| n.parse::<u64>().ok()) {
                        Some(n) if n <= max => (n, true),
                        _ => return err(Reason::UnknownCursor, "unknown cursor"),
                    }
                }
                _ => return err(Reason::Invalid, "from"),
            };
            if resumed {
                w.acked = w.acked.max(from_seq);
            } else {
                w.acked = max;
            }
            for s in &mut w.sockets {
                if s.tx.same_channel(me) {
                    s.sub = Some(sub.clone());
                    s.sent = from_seq;
                }
            }
            Ok(
                json!({"subscription": sub, "watermark_ms": now, "resumed": resumed,
                "next_seq": if resumed { from_seq + 1 } else { next_seq }}),
            )
        }
        "changes.ack" => {
            let seq = p["seq"].as_u64().unwrap_or(0);
            if seq >= w.next_seq {
                return err(Reason::Invalid, "ack above the highest seq sent");
            }
            if !w.lose_acks {
                w.acked = w.acked.max(seq);
            }
            Ok(json!({}))
        }
        "people.get" => {
            let users = p["users"].as_array().cloned().unwrap_or_default();
            let people: Vec<Value> = users
                .iter()
                .map(|u| {
                    let id = u["value"].as_str().unwrap_or_default();
                    w.people.get(id).cloned().unwrap_or(Value::Null)
                })
                .collect();
            Ok(json!({"people": people, "missing_permissions": []}))
        }
        "messages.send" | "messages.reply" | "messages.recall" | "reactions.add"
        | "reactions.remove" => write(&mut w, method, p, now),
        _ => err(Reason::Unsupported, "unknown method"),
    }
}

fn write(w: &mut World, method: &str, p: &Value, now: i64) -> Result<Value, RpcError> {
    if w.write_policy == "forbidden" {
        return err(Reason::WriteForbidden, "writes are forbidden");
    }
    // the signed-in user, before any side effect
    if p["expected_sender"] != w.signed_in
        || p["expected_identity"].as_str() != Some(w.account_identity.as_deref().unwrap_or("self"))
    {
        return err(Reason::AccountChanged, "someone else is signed in");
    }
    if let Some(ms) = w.rate_limit_next.take() {
        return Err(RpcError {
            reason: Reason::RateLimited,
            message: "slow down".into(),
            data: json!({"reason": "rate_limited", "retry_after_ms": ms}),
        });
    }
    let key = p["idempotency_key"].as_str().map(str::to_string);
    let mut bare = p.clone();
    if let Some(o) = bare.as_object_mut() {
        o.remove("idempotency_key");
    }
    if let Some(k) = &key
        && let Some((params, result)) = w.idem.get(k)
    {
        return if *params == bare {
            Ok(result.clone())
        } else {
            err(
                Reason::IdempotencyConflict,
                "the key was used for other params",
            )
        };
    }
    let result = match method {
        "messages.send" | "messages.reply" => {
            let n = w.next_out;
            w.next_out += 1;
            json!({"source_id": format!("out_{n}"),
                "id": if w.source_only { None } else { Some(format!("m_out_{n}")) },
                "created_ms": now, "thread_id": null})
        }
        "reactions.add" => {
            let n = w.next_out;
            w.next_out += 1;
            json!({"reaction_id": format!("r_{n}")})
        }
        _ => json!({}),
    };
    if let Some(k) = key {
        w.idem.insert(k, (bare, result.clone()));
    }
    w.writes
        .push((method.to_string(), p.clone(), result.clone()));
    Ok(result)
}
