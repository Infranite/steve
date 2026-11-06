//! The bot connection as a [`ChannelAdapter`], and its long connection feeding C1's inbound
//! pipeline:
//!
//! - [`ChannelAdapter`] for [`FeishuApp`]: features; `space_info` and `members` are
//!   [`chats`]' (F4), `send` and `recall` [`send`]'s (F3).
//! - [`parse_receive`]: `im.message.receive_v1` → [`InboundMessage`] (text with `@_user_n` replaced
//!   by `@名字`, `post` flattened, image/file as attachments, other types “[<类型>消息]”).
//! - [`FeishuInbound`]: the [`WsHandler`]: checks `header.app_id`, drops a repeated `event_id`
//!   (memory LRU, 10 000 entries / 24 h; `chan_messages`' unique index covers restarts), queues the
//!   message (or the member / chat event, [`chats::parse_chat_event`]) and acks at once (Feishu
//!   wants the ack within 3 s); a worker runs `on_inbound` (`on_space_event`).
//! - [`LongConnection`] / [`LongConnections`]: one ws client per `feishu_app` connection.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use serde_json::{Value, json};
use steve_flow::AdapterError;
use steve_modules::accounts::connections::{ConnCx, ProviderError};
use tokio::sync::{Semaphore, mpsc};
use tokio::task::JoinSet;

use super::auth::{APP_SECRET, APP_SYSTEM, FeishuApp, USER_SYSTEM};
use super::client::{self, FeishuClient};
use super::ws::{self, WsError, WsHandle, WsHandler, WsOptions};
use super::{chats, send};
use crate::channel::{
    ChannelAdapter, ChannelFeatures, HandleRef, InboundMessage, MemberInfo, Mention, OutContent,
    ReactionKind, SendTarget, Sender, SpaceEvent, SpaceInfo, SpaceKind,
};
use crate::service::MessageSink;

/// The message event this module maps; member and chat events are [`chats::parse_chat_event`]'s.
pub const RECEIVE: &str = "im.message.receive_v1";
/// `event_id`s remembered at most …
pub const SEEN_MAX: usize = 10_000;
/// … and for this long.
pub const SEEN_TTL_MS: i64 = 24 * 3_600_000;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

fn opt(v: &Value, k: &str) -> Option<String> {
    Some(s(v, k).trim().to_string()).filter(|x| !x.is_empty())
}

// ---------------------------------------------------------------------------------------------
// The adapter.

#[async_trait]
impl ChannelAdapter for FeishuApp {
    /// Phase 1 sends no cards and no buttons; the bot can edit and recall its own.
    fn features(&self) -> ChannelFeatures {
        ChannelFeatures {
            multi_bot: true,
            as_user: true,
            cards: false,
            buttons: false,
            thread_reply: true,
            edit: true,
            recall: true,
            members: true,
            reactions: true,
        }
    }

    /// Every group message only with `im:message.group_msg` granted (the last permission check);
    /// otherwise only the messages that @ the bot.
    fn receives_all(&self, config: &Value) -> bool {
        super::self_poll::receives_all_groups(config)
    }

    fn send_dedup(&self) -> Option<std::time::Duration> {
        Some(super::send::UUID_DEDUP)
    }

    async fn space_info(&self, cx: &ConnCx, ext_id: &str) -> Result<SpaceInfo, AdapterError> {
        chats::space_info(cx, ext_id)
            .await
            .map_err(|e| ProviderError::from(e).into())
    }

    async fn members(&self, cx: &ConnCx, ext_id: &str) -> Result<Vec<MemberInfo>, AdapterError> {
        chats::members(cx, ext_id)
            .await
            .map_err(|e| ProviderError::from(e).into())
    }

    /// Only C6's `outbound::send` calls this, after approval (F3, [`send::send`]).
    async fn send(
        &self,
        cx: &ConnCx,
        to: &SendTarget,
        content: &OutContent,
    ) -> Result<String, AdapterError> {
        send::send(cx, to, content)
            .await
            .map(|s| s.message_id)
            .map_err(|e| ProviderError::from(e).into())
    }

    async fn recall(&self, cx: &ConnCx, ext_msg_id: &str) -> Result<(), AdapterError> {
        send::recall(cx, ext_msg_id)
            .await
            .map_err(|e| ProviderError::from(e).into())
    }

    fn reaction_emoji(&self, kind: ReactionKind) -> &'static str {
        send::emoji_type(kind)
    }

    /// As the bot (tenant token), [`send::add_reaction_with`].
    async fn add_reaction(
        &self,
        cx: &ConnCx,
        ext_msg_id: &str,
        kind: ReactionKind,
    ) -> Result<String, AdapterError> {
        send::add_reaction_with(
            &FeishuClient::for_cx(cx),
            cx,
            ext_msg_id,
            send::emoji_type(kind),
        )
        .await
        .map_err(|e| ProviderError::from(e).into())
    }

    async fn remove_reaction(
        &self,
        cx: &ConnCx,
        ext_msg_id: &str,
        reaction_id: &str,
    ) -> Result<(), AdapterError> {
        send::remove_reaction_with(&FeishuClient::for_cx(cx), cx, ext_msg_id, reaction_id)
            .await
            .map_err(|e| ProviderError::from(e).into())
    }
}

// ---------------------------------------------------------------------------------------------
// Mapping.

/// A user (`union_id`, with the `open_id` valid under this app) or an app (`open_id` only).
pub(crate) fn handle_of(id: &Value, app: bool) -> Option<HandleRef> {
    let open = s(id, "open_id");
    if app {
        return (!open.is_empty()).then(|| HandleRef::new(APP_SYSTEM, open));
    }
    let union = s(id, "union_id");
    if !union.is_empty() {
        return Some(HandleRef::new(USER_SYSTEM, union).with_extra(open));
    }
    (!open.is_empty()).then(|| HandleRef::new(USER_SYSTEM, open))
}

/// `text`: the `@_user_n` keys replaced by `@名字` (longest key first, so `@_user_10` is not hit
/// by `@_user_1`).
fn text_of(content: &Value, mentions: &[(String, String)]) -> String {
    let mut t = s(content, "text").to_string();
    let mut keys: Vec<&(String, String)> = mentions.iter().collect();
    keys.sort_by_key(|(k, _)| std::cmp::Reverse(k.len()));
    for (k, name) in keys {
        if !k.is_empty() {
            t = t.replace(k.as_str(), &format!("@{name}"));
        }
    }
    t
}

/// The language body of a `post`: the receive event carries one language at the top; older
/// payloads (and the message API) nest it (`zh_cn`).
fn post_body(content: &Value) -> &Value {
    if content.get("content").is_some() {
        content
    } else {
        content
            .as_object()
            .and_then(|o| o.values().find(|v| v.get("content").is_some()))
            .unwrap_or(content)
    }
}

/// `post`: title, then each paragraph's `text`, `at`, `a` (and image markers) joined.
fn post_text(content: &Value, mentions: &[(String, String)]) -> String {
    let body = post_body(content);
    let mut lines = Vec::new();
    if let Some(t) = opt(body, "title") {
        lines.push(t);
    }
    for para in body
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        let mut line = String::new();
        for el in para.as_array().into_iter().flatten() {
            match s(el, "tag") {
                "text" | "md" => line.push_str(s(el, "text")),
                "a" => {
                    let (t, href) = (s(el, "text"), s(el, "href"));
                    match (t.is_empty(), href.is_empty()) {
                        (false, false) if t != href => line.push_str(&format!("{t} ({href})")),
                        (false, _) => line.push_str(t),
                        (true, _) => line.push_str(href),
                    }
                }
                "at" => {
                    let key = s(el, "user_id");
                    let own = s(el, "user_name");
                    let name = mentions
                        .iter()
                        .find(|(k, _)| k == key)
                        .map(|(_, n)| n.as_str())
                        .unwrap_or(if own.is_empty() { key } else { own });
                    line.push('@');
                    line.push_str(name);
                }
                "img" => line.push_str("[图片]"),
                "media" => line.push_str("[视频]"),
                "emotion" => line.push_str(&format!("[{}]", s(el, "emoji_type"))),
                "hr" => line.push_str("---"),
                "code_block" => line.push_str(&format!(
                    "```{}\n{}\n```",
                    s(el, "language").to_lowercase(),
                    s(el, "text")
                )),
                _ => {}
            }
        }
        lines.push(line);
    }
    lines.join("\n")
}

/// Resources of a `post`, in document order: `img` → image, `media` → video file, then the
/// attachment zone (`files[]`; a folder cannot be downloaded) (larksuite/cli `resource_extract.go`).
fn post_attachments(content: &Value) -> Vec<Value> {
    let mut out = Vec::new();
    for para in post_body(content)
        .get("content")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        for el in para.as_array().into_iter().flatten() {
            match s(el, "tag") {
                "img" => {
                    out.push(json!({"type": "image", "source": "post", "key": s(el, "image_key")}))
                }
                "media" => out.push(json!({
                    "type": "file", "kind": "video", "source": "post", "key": s(el, "file_key"),
                    "name": opt(el, "file_name"),
                })),
                _ => {}
            }
        }
    }
    for f in content["files"].as_array().into_iter().flatten() {
        let folder = f["is_folder"].as_bool().unwrap_or(false);
        out.push(json!({
            "type": "file", "kind": if folder { "folder" } else { "file" }, "source": "post",
            "key": s(f, "file_key"), "name": opt(f, "file_name"),
        }));
    }
    out
}

/// A standalone media message's attachment (`image`, `file`, `folder`, `audio`, `media`,
/// `sticker`).
fn media_attachment(msg_type: &str, c: &Value) -> Option<Value> {
    let kind = match msg_type {
        "image" => {
            return Some(json!({"type": "image", "source": "message", "key": s(c, "image_key")}));
        }
        "file" => "file",
        "folder" => "folder",
        "audio" => "audio",
        "media" => "video",
        "sticker" => "sticker",
        _ => return None,
    };
    let mut a = json!({
        "type": "file", "kind": kind, "source": "message", "key": s(c, "file_key"),
        "name": opt(c, "file_name"),
    });
    if let Some(d) = c["duration"].as_i64() {
        a["duration_ms"] = json!(d);
    }
    Some(a)
}

/// The same key once (the first one keeps its place, so indices stay put).
fn dedup(atts: Vec<Value>) -> Vec<Value> {
    let mut seen = HashSet::new();
    atts.into_iter()
        .filter(|a| {
            let k = s(a, "key");
            k.is_empty() || seen.insert(k.to_string())
        })
        .collect()
}

fn ms(v: &Value, k: &str) -> Option<i64> {
    v.get(k)
        .and_then(|x| x.as_i64().or_else(|| x.as_str()?.parse().ok()))
}

/// Text and attachments of a message body (`content` as Feishu sends it, a JSON string): text with
/// `@名字`; `post` flattened with its images, videos and attachment zone as attachments; image, file,
/// folder, audio, video and sticker messages as one attachment and no text; a card as its readable
/// text and a `card` attachment first ([`card`]); other types “[<类型>消息]”. `mentions` are
/// `(key, name)`. Shared by the long connection, the self poller (the message API's `body.content`
/// has the same shape as the event's `content`) and [`reparse`].
pub(crate) fn body_of(
    msg_type: &str,
    content: &str,
    mentions: &[(String, String)],
) -> (String, Vec<Value>) {
    if msg_type == "interactive" {
        let c = super::card::parse(content, mentions);
        return (c.text, vec![c.attachment]);
    }
    let content: Value = serde_json::from_str(content).unwrap_or(Value::Null);
    match msg_type {
        "text" => (text_of(&content, mentions), Vec::new()),
        "post" => (
            post_text(&content, mentions),
            dedup(post_attachments(&content)),
        ),
        t => match media_attachment(t, &content) {
            Some(a) => (String::new(), vec![a]),
            None => (format!("[{t}消息]"), Vec::new()),
        },
    }
}

/// A stored message read again from its raw payload (`ext_events.payload.raw`): the long
/// connection's envelope (`event.message`) or the message API's item (`msg_type`, `body.content`).
/// `None` when the payload is neither.
pub(crate) fn reparse(raw: &Value) -> Option<(String, Vec<Value>)> {
    let (ty, content, mentions) = if let Some(m) = raw["event"]["message"].as_object() {
        (m.get("message_type")?, m.get("content")?, m.get("mentions"))
    } else {
        (
            raw.get("msg_type")?,
            raw["body"].get("content")?,
            raw.get("mentions"),
        )
    };
    let keys: Vec<(String, String)> = mentions
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .map(|x| (s(x, "key").to_string(), s(x, "name").to_string()))
        .collect();
    Some(body_of(ty.as_str()?, content.as_str()?, &keys))
}

/// Maps one `im.message.receive_v1` envelope (schema 2.0). `Ok(None)` for other event types;
/// `Err` for a receive event without the fields every message has.
pub fn parse_receive(env: &Value, bot_open_id: &str) -> Result<Option<InboundMessage>, String> {
    let header = &env["header"];
    if s(header, "event_type") != RECEIVE {
        return Ok(None);
    }
    let ev = &env["event"];
    let m = &ev["message"];
    let chat_id = s(m, "chat_id");
    let message_id = s(m, "message_id");
    if chat_id.is_empty() || message_id.is_empty() {
        return Err("飞书消息缺少 chat_id 或 message_id".into());
    }
    let sender = &ev["sender"];
    let is_app = s(sender, "sender_type") == "app";
    let handle = handle_of(&sender["sender_id"], is_app).ok_or("飞书消息没有发送人")?;
    let tenant = s(header, "tenant_key");
    let stk = s(sender, "tenant_key");

    let raw_mentions: Vec<&Value> = m
        .get("mentions")
        .and_then(Value::as_array)
        .map(|a| a.iter().collect())
        .unwrap_or_default();
    let keys: Vec<(String, String)> = raw_mentions
        .iter()
        .map(|x| (s(x, "key").to_string(), s(x, "name").to_string()))
        .collect();
    let mentions = raw_mentions
        .iter()
        .map(|x| {
            let id = &x["id"];
            let is_bot = !bot_open_id.is_empty() && s(id, "open_id") == bot_open_id;
            Mention {
                handle: if is_bot || s(x, "mentioned_type") == "bot" {
                    None
                } else {
                    handle_of(id, false)
                },
                name: s(x, "name").to_string(),
                is_connection: is_bot,
            }
        })
        .collect();

    let (text, attachments) = body_of(s(m, "message_type"), s(m, "content"), &keys);
    let thread_key = opt(m, "thread_id")
        .or_else(|| opt(m, "root_id"))
        .unwrap_or_else(|| message_id.to_string());
    Ok(Some(InboundMessage {
        space_ext_id: chat_id.into(),
        space_kind: match s(m, "chat_type") {
            "p2p" => Some(SpaceKind::Dm),
            "group" => Some(SpaceKind::Group),
            "topic_group" => Some(SpaceKind::Channel),
            _ => None,
        },
        space_name: None,
        external_id: message_id.into(),
        event_id: opt(header, "event_id"),
        thread_key: Some(thread_key),
        reply_to: opt(m, "parent_id"),
        root_id: opt(m, "root_id"),
        sender: Sender {
            handle,
            display_name: None,
            is_bot: is_app,
            external: !tenant.is_empty() && !stk.is_empty() && tenant != stk,
        },
        text,
        attachments,
        mentions,
        created: ms(m, "create_time")
            .or_else(|| ms(header, "create_time"))
            .unwrap_or_else(steve_store::now),
        via: Some("ws".into()),
        raw: env.clone(),
    }))
}

// ---------------------------------------------------------------------------------------------
// The long connection.

/// `event_id`s seen recently.
#[derive(Default)]
pub struct SeenEvents {
    order: VecDeque<(i64, String)>,
    ids: HashSet<String>,
}

impl SeenEvents {
    /// Remembers `id` at `now`; `false` if it was already there.
    pub fn insert(&mut self, id: &str, now: i64) -> bool {
        while let Some((t, _)) = self.order.front() {
            if *t >= now - SEEN_TTL_MS && self.order.len() < SEEN_MAX {
                break;
            }
            if let Some((_, old)) = self.order.pop_front() {
                self.ids.remove(&old);
            }
        }
        if !self.ids.insert(id.to_string()) {
            return false;
        }
        self.order.push_back((now, id.to_string()));
        true
    }
}

/// What the ws handler queues for the worker.
#[derive(Debug, Clone, PartialEq)]
pub enum Queued {
    Message(Box<InboundMessage>),
    Space(SpaceEvent),
}

/// The ws handler of one bot connection.
pub struct FeishuInbound {
    app_id: String,
    bot_open_id: String,
    seen: Mutex<SeenEvents>,
    queue: mpsc::UnboundedSender<Queued>,
    /// Other events go to the channels' [`crate::OtherEvents`] hooks: `(channels, ws, conn)`.
    others: Option<(crate::Channels, String, String)>,
}

impl FeishuInbound {
    /// The handler and the receiving end of its queue (drive it with [`run_worker`]).
    pub fn new(
        app_id: impl Into<String>,
        bot_open_id: impl Into<String>,
    ) -> (Self, mpsc::UnboundedReceiver<Queued>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (
            Self {
                app_id: app_id.into(),
                bot_open_id: bot_open_id.into(),
                seen: Mutex::default(),
                queue: tx,
                others: None,
            },
            rx,
        )
    }

    /// Hands the events that are neither messages nor chat changes (the address book: F5) of
    /// connection `conn` of `ws` to `channels`' hooks.
    pub fn with_others(mut self, channels: crate::Channels, ws: &str, conn: &str) -> Self {
        self.others = Some((channels, ws.into(), conn.into()));
        self
    }

    /// Validates, de-dups and queues one payload; never waits on the pipeline.
    pub fn accept(&self, payload: &[u8]) -> Result<(), String> {
        let env: Value = serde_json::from_slice(payload).map_err(|e| format!("not JSON: {e}"))?;
        let header = &env["header"];
        let app = s(header, "app_id");
        if !app.is_empty() && app != self.app_id {
            // another app's event on our connection: acknowledged, not processed
            tracing::warn!(app, "feishu ws: event of another app ignored");
            return Ok(());
        }
        if let Some(id) = opt(header, "event_id")
            && !lock(&self.seen).insert(&id, steve_store::now())
        {
            tracing::debug!(event_id = id, "feishu ws: repeated event ignored");
            return Ok(());
        }
        let item = match parse_receive(&env, &self.bot_open_id)? {
            Some(m) => Queued::Message(Box::new(m)),
            None => match chats::parse_chat_event(&env) {
                Some(e) => Queued::Space(e),
                None => {
                    if let Some((ch, ws, conn)) = &self.others {
                        ch.other_event(ws, conn, &env);
                    }
                    return Ok(());
                }
            },
        };
        self.queue
            .send(item)
            .map_err(|_| "feishu ws: inbound worker stopped".to_string())
    }
}

impl WsHandler for FeishuInbound {
    async fn on_event(&self, payload: Vec<u8>) -> Result<Option<Value>, String> {
        self.accept(&payload).map(|_| None)
    }
}

/// How many chats of one connection are worked on at the same time.
pub const SPACE_LANES: usize = 8;

impl Queued {
    /// The chat the item belongs to: items of one chat are handled in order.
    fn space_ext_id(&self) -> &str {
        match self {
            Self::Message(m) => &m.space_ext_id,
            Self::Space(ev) => ev.space_ext_id(),
        }
    }
}

/// One chat's lane: its queue and how many of its items are not done yet.
struct Lane {
    tx: mpsc::UnboundedSender<Queued>,
    pending: Arc<AtomicUsize>,
}

/// Runs `on_inbound` (`on_space_event`) for each queued item until the queue closes: in order
/// within a chat, and up to [`SPACE_LANES`] chats at once, so a slow chat (a model call, a slow
/// adapter) doesn't hold up the others.
pub async fn run_worker(
    sink: Arc<dyn MessageSink>,
    ws: String,
    conn: String,
    mut rx: mpsc::UnboundedReceiver<Queued>,
) {
    let permits = Arc::new(Semaphore::new(SPACE_LANES));
    let mut lanes: HashMap<String, Lane> = HashMap::new();
    // Dropping the set (the worker aborted) aborts the lanes with it.
    let mut tasks = JoinSet::new();
    while let Some(item) = rx.recv().await {
        while tasks.try_join_next().is_some() {}
        // A lane with nothing pending is idle; only this loop adds to it, so closing it now
        // can't lose an item or reorder its chat. A lane whose task died goes too.
        lanes.retain(|_, l| l.pending.load(Ordering::Acquire) > 0 && !l.tx.is_closed());
        let key = item.space_ext_id().to_string();
        let lane = lanes.entry(key).or_insert_with(|| {
            let (tx, lane_rx) = mpsc::unbounded_channel();
            let pending = Arc::new(AtomicUsize::new(0));
            tasks.spawn(run_lane(
                sink.clone(),
                ws.clone(),
                conn.clone(),
                lane_rx,
                pending.clone(),
                permits.clone(),
            ));
            Lane { tx, pending }
        });
        lane.pending.fetch_add(1, Ordering::AcqRel);
        if lane.tx.send(item).is_err() {
            // the lane task just died; its chat starts a fresh lane with the next item
            tracing::warn!(conn, "feishu ws: inbound lane gone, item dropped");
        }
    }
    // The queue closed: let the lanes finish what they have.
    drop(lanes);
    while tasks.join_next().await.is_some() {}
}

/// Handles one chat's items in order, each under a permit of the connection's limit.
async fn run_lane(
    sink: Arc<dyn MessageSink>,
    ws: String,
    conn: String,
    mut rx: mpsc::UnboundedReceiver<Queued>,
    pending: Arc<AtomicUsize>,
    permits: Arc<Semaphore>,
) {
    while let Some(item) = rx.recv().await {
        // never closed, so acquiring only waits
        let _permit = permits.acquire().await;
        match item {
            Queued::Message(m) => {
                let ext = m.external_id.clone();
                if let Err(e) = sink.inbound(&ws, &conn, *m).await {
                    tracing::warn!(conn, message = ext, "feishu ws: inbound failed: {e}");
                }
            }
            Queued::Space(ev) => {
                let ext = ev.space_ext_id().to_string();
                if let Err(e) = sink.space_event(&ws, &conn, ev).await {
                    tracing::warn!(conn, space = ext, "feishu ws: space event failed: {e}");
                }
            }
        }
        pending.fetch_sub(1, Ordering::AcqRel);
    }
}

/// One running long connection: the ws client and its worker.
pub struct LongConnection {
    pub ws: WsHandle,
    worker: tokio::task::JoinHandle<()>,
}

impl LongConnection {
    /// Starts the long connection of bot connection `conn` of `ws` (App ID and the bot's `open_id`
    /// from its config, the App Secret from the system vault).
    pub async fn start(channels: &crate::Channels, ws: &str, conn: &str) -> Result<Self, WsError> {
        let cx = channels
            .connections()
            .cx(ws, conn)
            .map_err(|e| WsError::Client {
                code: 0,
                msg: e.to_string(),
            })?;
        let app_id = cx.config_str("app_id").unwrap_or_default().to_string();
        let bot_open_id = cx.config_str("bot_open_id").unwrap_or_default().to_string();
        let secret = cx.secret(APP_SECRET).await.map_err(|e| WsError::Client {
            code: 0,
            msg: e.to_string(),
        })?;
        let opts = WsOptions::new(client::api_base(cx.config()), &app_id, secret.as_str());
        Self::start_with(channels, ws, conn, opts, &bot_open_id)
    }

    /// Like [`LongConnection::start`] with explicit client options (tests: fast reconnects).
    pub fn start_with(
        channels: &crate::Channels,
        ws: &str,
        conn: &str,
        opts: WsOptions,
        bot_open_id: &str,
    ) -> Result<Self, WsError> {
        let (h, rx) = FeishuInbound::new(opts.app_id.clone(), bot_open_id);
        let h = h.with_others(channels.clone(), ws, conn);
        let sink: Arc<dyn MessageSink> = Arc::new(channels.clone());
        let worker = tokio::spawn(run_worker(sink, ws.into(), conn.into(), rx));
        let client = ws::spawn(opts, Arc::new(h))?;
        Ok(Self { ws: client, worker })
    }

    /// Stops the client; queued messages are still processed.
    pub async fn stop(self) -> Result<(), WsError> {
        let r = self.ws.stop().await;
        // the handler (and with it the queue's sender) goes with the client task; give the worker
        // a moment to finish what is queued
        let mut worker = self.worker;
        if tokio::time::timeout(std::time::Duration::from_secs(5), &mut worker)
            .await
            .is_err()
        {
            worker.abort();
        }
        r
    }
}

/// The long connections of all bot connections (the Hub runs exactly one per app: Feishu
/// distributes events among an app's connections at random).
pub struct LongConnections {
    channels: crate::Channels,
    running: tokio::sync::Mutex<HashMap<String, LongConnection>>,
}

impl LongConnections {
    pub fn new(channels: crate::Channels) -> Self {
        Self {
            channels,
            running: tokio::sync::Mutex::default(),
        }
    }

    /// Starts one for every `feishu_app` connection that is `ok` or `error` and not running yet.
    /// Returns how many were started.
    pub async fn start_all(&self) -> usize {
        let conns = self.channels.store().reader(|c| -> crate::ChannelResult<Vec<(String, String)>> {
            let mut st = c.prepare(
                "SELECT id, workspace_id FROM connections WHERE adapter = ? AND status IN ('ok', 'error') ORDER BY created, id",
            )?;
            let rows = st
                .query_map([super::auth::APP_ADAPTER], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        });
        let conns = match conns {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("feishu ws: listing connections failed: {e}");
                return 0;
            }
        };
        let mut running = self.running.lock().await;
        let mut n = 0;
        for (id, ws) in conns {
            if running.contains_key(&id) {
                continue;
            }
            match LongConnection::start(&self.channels, &ws, &id).await {
                Ok(lc) => {
                    running.insert(id, lc);
                    n += 1;
                }
                Err(e) => tracing::warn!(conn = id, "feishu ws: not started: {e}"),
            }
        }
        n
    }

    /// Starts the long connection of bot connection `conn` of `ws` unless one is running (a
    /// connection just connected or resumed). `Ok(true)` when started now.
    pub async fn start_one(&self, ws: &str, conn: &str) -> Result<bool, WsError> {
        let mut running = self.running.lock().await;
        if running.contains_key(conn) {
            return Ok(false);
        }
        let lc = LongConnection::start(&self.channels, ws, conn).await?;
        running.insert(conn.to_string(), lc);
        Ok(true)
    }

    /// Whether a long connection of `conn` is running.
    pub async fn is_running(&self, conn: &str) -> bool {
        self.running.lock().await.contains_key(conn)
    }

    /// Stops the long connection of `conn` (disconnected or paused).
    pub async fn stop(&self, conn: &str) -> bool {
        match self.running.lock().await.remove(conn) {
            Some(lc) => {
                let _ = lc.stop().await;
                true
            }
            None => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture(name: &str) -> Value {
        let p = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/feishu")
            .join(format!("{name}.json"));
        serde_json::from_slice(&std::fs::read(p).expect("fixture")).expect("json")
    }

    const BOT: &str = "ou_bot0000000000000000000000000001";

    fn parse(name: &str) -> InboundMessage {
        parse_receive(&fixture(name), BOT)
            .unwrap()
            .expect("a message")
    }

    #[test]
    fn group_mention_of_the_bot() {
        let m = parse("message_group_at_bot");
        assert_eq!(m.text, "@值班小运 帮我查一下昨晚告警的原因");
        assert!(m.mentions_connection());
        assert_eq!(m.mentions[0].handle, None);
        assert_eq!(m.sender.handle.system, "feishu");
        assert_eq!(m.sender.handle.handle, "on_alice00000000000000000000000001");
        assert_eq!(
            m.sender.handle.extra,
            ["ou_alice00000000000000000000000001"]
        );
        assert!(!m.sender.is_bot && !m.sender.external);
        assert_eq!(m.space_kind, Some(SpaceKind::Group));
        assert_eq!(
            m.thread_key.as_deref(),
            Some("om_groupat00000000000000000000001")
        );
        assert_eq!(m.created, 1_761_900_002_000);
        assert_eq!(
            m.event_id.as_deref(),
            Some("48e1689368f1945fb7f72793b8f5426e")
        );
    }

    #[test]
    fn p2p_post_thread_files_bots_and_external() {
        let m = parse("message_p2p_text");
        assert_eq!(m.space_kind, Some(SpaceKind::Dm));
        assert_eq!(m.text, "明天的发布还按计划吗？");

        let m = parse("message_post_rich");
        assert_eq!(
            m.text,
            "上线检查\n@值班小运 请按这份清单过一遍：\n发布清单 (https://wiki.example.com/release-checklist)，有问题 @李鹏翔"
        );
        assert!(m.mentions[0].is_connection);
        assert_eq!(
            m.mentions[1].handle.as_ref().map(|h| h.handle.as_str()),
            Some("on_owner00000000000000000000000001")
        );

        let m = parse("message_thread_reply");
        assert_eq!(m.thread_key.as_deref(), Some("omt_fake0000000001"));
        assert_eq!(
            m.reply_to.as_deref(),
            Some("om_topicroot000000000000000000001")
        );
        assert_eq!(
            m.root_id.as_deref(),
            Some("om_topicroot000000000000000000001")
        );

        let m = parse("message_image");
        assert_eq!(m.text, "");
        assert_eq!(m.attachments[0]["type"], "image");
        let m = parse("message_file");
        assert_eq!(m.attachments[0]["name"], "告警截图说明.txt");

        let m = parse("message_from_bot");
        assert!(m.sender.is_bot);
        assert_eq!(m.sender.handle.system, "feishu_app");
        assert_eq!(m.sender.handle.handle, "ou_otherbot00000000000000000000001");

        let m = parse("message_external_user");
        assert!(m.sender.external);
    }

    #[test]
    fn other_events_and_types() {
        assert_eq!(parse_receive(&fixture("bot_added"), BOT).unwrap(), None);
        let mut env = fixture("message_p2p_text");
        env["event"]["message"]["message_type"] = json!("sticker");
        env["event"]["message"]["content"] = json!("{\"file_key\":\"x\"}");
        let m = parse_receive(&env, BOT).unwrap().unwrap();
        assert_eq!(m.text, "", "a sticker is an attachment, not text");
        assert_eq!(
            m.attachments,
            [
                json!({"type": "file", "kind": "sticker", "source": "message", "key": "x", "name": null})
            ]
        );
        env["event"]["message"]["message_type"] = json!("location");
        env["event"]["message"]["content"] = json!("{\"name\":\"x\"}");
        assert_eq!(
            parse_receive(&env, BOT).unwrap().unwrap().text,
            "[location消息]"
        );
        env["event"]["message"]["chat_id"] = json!("");
        assert!(parse_receive(&env, BOT).is_err());
        // longer keys first
        let t = text_of(
            &json!({"text": "@_user_10 和 @_user_1"}),
            &[
                ("@_user_1".into(), "甲".into()),
                ("@_user_10".into(), "乙".into()),
            ],
        );
        assert_eq!(t, "@乙 和 @甲");
    }

    #[test]
    fn seen_events_expire_and_cap() {
        let mut s = SeenEvents::default();
        assert!(s.insert("a", 0));
        assert!(!s.insert("a", 1));
        assert!(s.insert("a", SEEN_TTL_MS + 1), "forgotten after 24 h");
        let mut s = SeenEvents::default();
        for i in 0..SEEN_MAX + 5 {
            s.insert(&i.to_string(), 10);
        }
        assert!(s.ids.len() <= SEEN_MAX);
        assert!(s.insert("0", 10), "the oldest went first");
    }

    #[test]
    fn accept_checks_app_and_dedups() {
        let (h, mut rx) = FeishuInbound::new("cli_a5fake0000000001", BOT);
        let body = serde_json::to_vec(&fixture("message_group_at_bot")).unwrap();
        h.accept(&body).unwrap();
        h.accept(&body).unwrap();
        assert!(rx.try_recv().is_ok());
        assert!(rx.try_recv().is_err(), "the repeat was dropped");
        let (h, mut rx) = FeishuInbound::new("cli_other", BOT);
        h.accept(&body).unwrap();
        assert!(rx.try_recv().is_err(), "another app's event");
        assert!(h.accept(b"nope").is_err());
    }

    /// Records which messages ran; the first message of chat A waits for `gate`.
    struct Gated {
        done: tokio::sync::mpsc::UnboundedSender<String>,
        gate: Arc<tokio::sync::Notify>,
    }

    #[async_trait]
    impl MessageSink for Gated {
        async fn inbound(
            &self,
            _ws: &str,
            _conn: &str,
            msg: InboundMessage,
        ) -> crate::ChannelResult<crate::inbound::Inbound> {
            if msg.external_id == "a1" {
                self.gate.notified().await;
            }
            let _ = self.done.send(msg.external_id);
            Err(crate::ChannelError::not_found("test"))
        }
    }

    #[tokio::test]
    async fn worker_keeps_chat_order_but_does_not_wait_on_other_chats() {
        let (done, mut out) = tokio::sync::mpsc::unbounded_channel();
        let gate = Arc::new(tokio::sync::Notify::new());
        let sink: Arc<dyn MessageSink> = Arc::new(Gated {
            done,
            gate: gate.clone(),
        });
        let (tx, rx) = mpsc::unbounded_channel();
        let worker = tokio::spawn(run_worker(sink, "ws".into(), "conn".into(), rx));
        let msg = |space: &str, id: &str| {
            let mut m = parse("message_group_at_bot");
            m.space_ext_id = space.into();
            m.external_id = id.into();
            Queued::Message(Box::new(m))
        };
        for (space, id) in [("oc_a", "a1"), ("oc_a", "a2"), ("oc_b", "b1")] {
            tx.send(msg(space, id)).unwrap();
        }
        let wait = std::time::Duration::from_secs(5);
        let next = async |out: &mut tokio::sync::mpsc::UnboundedReceiver<String>| {
            tokio::time::timeout(wait, out.recv())
                .await
                .unwrap()
                .unwrap()
        };
        assert_eq!(next(&mut out).await, "b1", "chat B ran while A was busy");
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        assert!(out.try_recv().is_err(), "a2 waits for a1");
        gate.notify_one();
        assert_eq!(next(&mut out).await, "a1");
        assert_eq!(next(&mut out).await, "a2");
        // An idle chat gets a fresh lane; closing the queue ends the worker.
        tx.send(msg("oc_b", "b2")).unwrap();
        assert_eq!(next(&mut out).await, "b2");
        drop(tx);
        tokio::time::timeout(wait, worker).await.unwrap().unwrap();
    }
}
