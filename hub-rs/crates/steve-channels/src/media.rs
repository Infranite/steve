//! Images, files and cards of channel messages:
//!
//! - [`present`]: what a stored message shows — provider keys removed, a `url` for each resource
//!   that can be fetched now or the reason it cannot (`unavailable`); a message stored before
//!   cards and post images were read is read again from its own raw event when that is still kept
//!   ([`effective`]).
//! - [`fetch_for_viewer`] (the `GET /chan-messages/:id/resources/:n` route) and [`fetch_bound`]
//!   (the agent's composer): one resource, located only through the stored message — its
//!   workspace, space, provider message id and the key recorded at index `n` — fetched as the
//!   connection that received it through its adapter's [`MediaFetch`], bounded in size, time and
//!   concurrency.
//! - [`with_unread_note`]: what the agent is told about attachments it cannot read.

use std::collections::{BTreeMap, HashMap};
use std::sync::{Arc, LazyLock, Mutex, RwLock};
use std::time::Duration;

use async_trait::async_trait;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use steve_modules::accounts::connections::ConnCx;
use steve_store::base::Viewer;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

use crate::channel::MessageRef;
use crate::{Channels, spaces};

/// The media view of a channel side: fetching a stored message's resources (images, files) as the
/// connection that received it. Registered with [`crate::ChannelsBuilder::media`] under the
/// `connections.adapter` it serves; the Hub calls only the view of that adapter.
#[async_trait]
pub trait MediaFetch: Send + Sync {
    /// The `connections.adapter` it serves.
    fn adapter_id(&self) -> &str;

    /// The bytes of resource `key` of message `msg` (`image`: an image, else a file), at most
    /// `max` bytes: past it, [`MediaError::TooLarge`].
    async fn download(
        &self,
        cx: &ConnCx,
        msg: &MessageRef,
        key: &str,
        image: bool,
        max: usize,
    ) -> Result<Vec<u8>, MediaError>;

    /// A message stored before its attachments were read fully, read again from the remote's raw
    /// payload kept for it: `(text, attachments)`. `None`: keep it as stored. Nothing is fetched.
    fn reparse(&self, msg: &MessageRef, raw: &Value) -> Option<(String, Vec<Value>)> {
        let _ = (msg, raw);
        None
    }
}

/// The media views of the channels built in this process, for what shows a stored message without
/// a [`Channels`] at hand ([`present`]); fetching uses the [`Channels`]' own.
static VIEWS: LazyLock<RwLock<BTreeMap<String, Arc<dyn MediaFetch>>>> =
    LazyLock::new(|| RwLock::new(BTreeMap::new()));

/// Makes `m` known to [`present`] (done when [`Channels`] are built).
pub(crate) fn publish(m: &Arc<dyn MediaFetch>) {
    VIEWS
        .write()
        .unwrap_or_else(|e| e.into_inner())
        .insert(m.adapter_id().to_string(), m.clone());
}

/// Every published view, in adapter order.
fn views() -> Vec<Arc<dyn MediaFetch>> {
    VIEWS
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .values()
        .cloned()
        .collect()
}

fn view(adapter: &str) -> Option<Arc<dyn MediaFetch>> {
    VIEWS
        .read()
        .unwrap_or_else(|e| e.into_inner())
        .get(adapter)
        .cloned()
}

/// The size ceiling of one resource (the web's preview has the same).
pub const MAX_BYTES: u64 = 25 * 1024 * 1024;
/// The time ceiling of one fetch.
pub const TIMEOUT: Duration = Duration::from_secs(30);
/// Fetches in flight per connection …
pub const PER_CONNECTION: usize = 3;
/// … and per Hub.
pub const PER_HUB: usize = 8;
/// A fetch waits this long for a slot, then is refused (`busy`).
pub const QUEUE_WAIT: Duration = Duration::from_secs(5);

/// Limits of one fetch.
#[derive(Debug, Clone, Copy)]
pub struct Limits {
    pub max_bytes: u64,
    pub timeout: Duration,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_bytes: MAX_BYTES,
            timeout: TIMEOUT,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ResourceKind {
    Image,
    File,
}

/// One fetchable entry of a message (`index` into its `attachments`).
#[derive(Debug, Clone, PartialEq)]
pub struct ResourceRef {
    pub index: usize,
    pub kind: ResourceKind,
    pub name: Option<String>,
    /// `file`, `audio`, `video` for files.
    pub file_kind: Option<String>,
}

/// A fetched resource.
#[derive(Debug, Clone)]
pub struct Fetched {
    pub bytes: Vec<u8>,
    /// Sniffed from the bytes: `image/png|jpeg|gif|webp`, else `application/octet-stream`.
    pub mime: &'static str,
    /// Safe as a file name.
    pub name: String,
    pub kind: ResourceKind,
}

/// Why a resource cannot be had. `reason()` is the contract's `Reason`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[non_exhaustive]
pub enum MediaError {
    #[error("没有这个附件")]
    NotFound,
    #[error("原文已过保留期限")]
    Purged,
    #[error("消息已撤回")]
    Recalled,
    #[error("飞书不能通过接口下载文件夹，请在飞书中查看")]
    Folder,
    #[error("飞书不能下载表情包")]
    Sticker,
    #[error("这条消息没有可下载的附件标识")]
    NoKey,
    #[error("这个空间的接入账号已经断开")]
    Disconnected,
    #[error("这个渠道不支持下载附件")]
    Unsupported,
    #[error("附件超过 25 MB，请在飞书中查看")]
    TooLarge,
    #[error("下载超时，请重试")]
    Timeout,
    #[error("下载的人太多，请稍后再试")]
    Busy,
    #[error("接入账号没有读取这条消息附件的权限")]
    NoPermission,
    #[error("机器人不在这个群里，读不到附件")]
    NotInChat,
    #[error("这条消息处于保密或防泄密模式，不能下载")]
    Restricted,
    #[error("这条消息对接入账号不可见")]
    Invisible,
    #[error("附件已被删除或不属于这条消息")]
    Gone,
    #[error("飞书限流了，请稍后再试")]
    RateLimited,
    #[error("飞书暂时取不到这个附件")]
    Provider,
}

impl MediaError {
    pub fn reason(&self) -> &'static str {
        match self {
            Self::NotFound => "not_found",
            Self::Purged => "purged",
            Self::Recalled => "recalled",
            Self::Folder => "folder",
            Self::Sticker => "sticker",
            Self::NoKey => "no_key",
            Self::Disconnected => "disconnected",
            Self::Unsupported => "unsupported",
            Self::TooLarge => "too_large",
            Self::Timeout => "timeout",
            Self::Busy => "busy",
            Self::NoPermission => "no_permission",
            Self::NotInChat => "not_in_chat",
            Self::Restricted => "restricted",
            Self::Invisible => "invisible",
            Self::Gone => "gone",
            Self::RateLimited => "rate_limited",
            Self::Provider => "provider",
        }
    }

    pub fn status(&self) -> u16 {
        match self {
            Self::NotFound => 404,
            Self::Folder | Self::Sticker | Self::NoKey | Self::Disconnected | Self::Unsupported => {
                409
            }
            Self::Purged | Self::Recalled => 410,
            Self::TooLarge => 413,
            Self::Busy | Self::RateLimited => 429,
            Self::Timeout => 504,
            _ => 502,
        }
    }

    pub fn message(&self) -> String {
        self.to_string()
    }
}

// ---------------------------------------------------------------- the stored message

/// What resource access needs of a `chan_messages` row.
#[derive(Debug, Clone)]
struct Row {
    id: String,
    ws: String,
    space_id: String,
    direction: String,
    status: String,
    event_id: Option<String>,
    text: Option<String>,
    attachments: Value,
    purged: Option<i64>,
}

impl Row {
    /// The message as the [`MediaFetch`] of connection `conn` sees it: the same [`MessageRef`]
    /// recall and reactions get (the delivery id that connection knows it by, the sender, the
    /// provider's create time). `None` when the row is gone.
    fn message_ref(&self, c: &Connection, conn: Option<&str>) -> Option<MessageRef> {
        crate::messages::message_ref(c, &self.id, conn.unwrap_or_default())
            .ok()
            .flatten()
    }
}

fn load(c: &Connection, id: &str) -> rusqlite::Result<Option<Row>> {
    c.query_row(
        "SELECT id, workspace_id, space_id, direction, status, event_id, text, attachments, purged
           FROM chan_messages WHERE id = ?",
        [id],
        |r| {
            Ok(Row {
                id: r.get(0)?,
                ws: r.get(1)?,
                space_id: r.get(2)?,
                direction: r.get(3)?,
                status: r.get(4)?,
                event_id: r.get(5)?,
                text: r.get(6)?,
                attachments: r
                    .get::<_, Option<String>>(7)?
                    .and_then(|s| serde_json::from_str(&s).ok())
                    .unwrap_or_else(|| json!([])),
                purged: r.get(8)?,
            })
        },
    )
    .optional()
}

/// Text that says a stored message was read before cards / media were (`[interactive消息]`,
/// `[audio消息]` …, a post with `[图片]`).
fn legacy_text(t: &str) -> bool {
    matches!(
        t,
        "[interactive消息]" | "[audio消息]" | "[media消息]" | "[sticker消息]" | "[folder消息]"
    ) || t.contains("[图片]")
        || t.contains("[视频]")
}

/// Attachments in the shape before this contract: no `source`, no card.
fn legacy_attachments(a: &Value) -> bool {
    a.as_array().is_none_or(|xs| {
        xs.iter()
            .all(|x| x.get("source").is_none() && x["type"] != "card")
    })
}

/// The message as it reads now: `(text, attachments)`. An inbound message stored before cards and
/// post media were read is read again from its own raw event, when the event is still kept for
/// exactly this message (same workspace, space, message, provider message id) and nothing was
/// purged, by the [`MediaFetch`] of the connection's adapter — or, once no connection of the
/// message is left, by the view that recognizes the event as this message. Nothing is written,
/// nothing replayed, nothing fetched.
fn effective(c: &Connection, r: &Row) -> (Option<String>, Value) {
    let unchanged = (r.text.clone(), r.attachments.clone());
    let (Some(text), Some(ev)) = (&r.text, &r.event_id) else {
        return unchanged;
    };
    if r.direction != "in"
        || r.purged.is_some()
        || !legacy_attachments(&r.attachments)
        || !legacy_text(text)
    {
        return unchanged;
    }
    let payload: Option<String> = c
        .query_row(
            "SELECT payload FROM ext_events
              WHERE id = ?1 AND workspace_id = ?2 AND space_id = ?3 AND message_id = ?4
                AND payload IS NOT NULL AND payload_purged IS NULL",
            params![ev, r.ws, r.space_id, r.id],
            |x| x.get(0),
        )
        .optional()
        .ok()
        .flatten();
    let Some(payload) = payload.and_then(|p| serde_json::from_str::<Value>(&p).ok()) else {
        return unchanged;
    };
    let raw = &payload["raw"];
    let conn = connection_of(c, r, None);
    let Some(msg) = r.message_ref(c, conn.as_ref().map(|(x, _)| x.as_str())) else {
        return unchanged;
    };
    // the view of the connection's adapter; once that connection is gone, the view that knows
    // this raw event as this very message (each refuses another message's event)
    let read = match conn {
        Some((_, a)) => view(&a).and_then(|m| m.reparse(&msg, raw).map(|x| (a, x))),
        None => views().iter().find_map(|m| {
            m.reparse(&msg, raw)
                .map(|x| (m.adapter_id().to_string(), x))
        }),
    };
    match read {
        Some((a, (t, atts))) => (Some(t), Value::Array(stamped(&atts, &a))),
        None => unchanged,
    }
}

/// The connection to fetch as: the one whose event recorded the message, else the one the space
/// was first seen through, else its voice; with `want`, only a connection of that adapter (the
/// one the resource's key belongs to). `(connection, adapter)`; `None` when none is left.
fn connection_of(c: &Connection, r: &Row, want: Option<&str>) -> Option<(String, String)> {
    let mut cands: Vec<String> = Vec::new();
    if let Some(ev) = &r.event_id
        && let Ok(Some(Some(conn))) = c
            .query_row(
                "SELECT connection_id FROM ext_events
                  WHERE id = ?1 AND workspace_id = ?2 AND space_id = ?3 AND message_id = ?4",
                params![ev, r.ws, r.space_id, r.id],
                |x| x.get::<_, Option<String>>(0),
            )
            .optional()
    {
        cands.push(conn);
    }
    if let Ok(Some(s)) = spaces::get(c, &r.space_id) {
        cands.push(s.via);
        cands.push(s.connection_id);
    }
    cands
        .into_iter()
        .filter(|x| !x.is_empty())
        .find_map(|conn| {
            c.query_row(
                "SELECT adapter FROM connections WHERE id = ? AND workspace_id = ?",
                params![conn, r.ws],
                |x| x.get::<_, String>(0),
            )
            .optional()
            .ok()
            .flatten()
            .filter(|a| want.is_none_or(|w| w == a))
            .map(|a| (conn, a))
        })
}

fn supported(adapter: &str) -> bool {
    view(adapter).is_some()
}

/// Why entry `a` of message `r` cannot be fetched, if it cannot (connection aside).
fn blocked(r: &Row, a: &Value) -> Option<MediaError> {
    if r.purged.is_some() {
        return Some(MediaError::Purged);
    }
    if !matches!(r.status.as_str(), "received" | "sent") {
        return Some(MediaError::Recalled);
    }
    match a["kind"].as_str() {
        Some("folder") => return Some(MediaError::Folder),
        Some("sticker") => return Some(MediaError::Sticker),
        _ => {}
    }
    if a["key"].as_str().is_none_or(str::is_empty) {
        return Some(MediaError::NoKey);
    }
    None
}

fn is_resource(a: &Value) -> bool {
    matches!(a["type"].as_str(), Some("image" | "file"))
}

/// The key of a stored resource entry naming the `connections.adapter` its `key` belongs to.
const ADAPTER_STAMP: &str = "adapter";

/// `atts` as stored for a message received through `adapter`: every resource entry names it, so
/// only that adapter's [`MediaFetch`] is ever handed its key (whatever an entry said before).
pub(crate) fn stamped(atts: &[Value], adapter: &str) -> Vec<Value> {
    atts.iter()
        .map(|a| {
            let mut a = a.clone();
            if is_resource(&a)
                && let Some(o) = a.as_object_mut()
            {
                o.insert(ADAPTER_STAMP.into(), json!(adapter));
            }
            a
        })
        .collect()
}

/// The adapter a stored resource entry names; `None` for one stored before entries were stamped.
fn stamp_of(a: &Value) -> Option<&str> {
    a.get(ADAPTER_STAMP)
        .and_then(Value::as_str)
        .filter(|x| !x.is_empty())
}

/// The public form of one stored resource entry.
fn public_entry(msg: &str, i: usize, a: &Value, why: Option<MediaError>) -> Value {
    let mut o = a.as_object().cloned().unwrap_or_default();
    o.remove("key");
    o.remove(ADAPTER_STAMP);
    o.entry("source").or_insert(json!("message"));
    if a["type"] == "file" {
        o.entry("kind").or_insert(json!("file"));
    }
    match why {
        Some(e) => {
            o.insert("unavailable".into(), json!(e.reason()));
        }
        None => {
            o.insert(
                "url".into(),
                json!(format!("/api/chan-messages/{msg}/resources/{i}")),
            );
        }
    }
    Value::Object(o)
}

/// What a stored message shows: `(text, attachments)` with keys removed and `url` / `unavailable`
/// on each resource; outbound messages have no resources (their entries show `unsupported`).
pub fn present(c: &Connection, id: &str) -> Option<(Option<String>, Value)> {
    let r = load(c, id).ok().flatten()?;
    // purged (or text gone): nothing of it shows, whatever an older sweep left in `attachments`
    if r.purged.is_some() || r.text.is_none() {
        return Some((None, json!([])));
    }
    let (text, atts) = effective(c, &r);
    let Some(xs) = atts.as_array().filter(|x| !x.is_empty()) else {
        return Some((text, json!([])));
    };
    let out = xs
        .iter()
        .enumerate()
        .map(|(i, a)| {
            if !is_resource(a) {
                return a.clone();
            }
            let why = blocked(&r, a).or(if r.direction != "in" {
                Some(MediaError::Unsupported)
            } else {
                match &connection_of(c, &r, stamp_of(a)) {
                    None => Some(MediaError::Disconnected),
                    Some((_, ad)) if !supported(ad) => Some(MediaError::Unsupported),
                    _ => None,
                }
            });
            public_entry(&r.id, i, a, why)
        })
        .collect();
    Some((text, Value::Array(out)))
}

/// The fetchable resources of stored attachments.
pub fn resources_of(attachments: &Value) -> Vec<ResourceRef> {
    attachments
        .as_array()
        .into_iter()
        .flatten()
        .enumerate()
        .filter(|(_, a)| is_resource(a))
        .filter(|(_, a)| {
            !matches!(a["kind"].as_str(), Some("folder" | "sticker"))
                && a["key"].as_str().is_some_and(|k| !k.is_empty())
        })
        .map(|(i, a)| ResourceRef {
            index: i,
            kind: if a["type"] == "image" {
                ResourceKind::Image
            } else {
                ResourceKind::File
            },
            name: a["name"].as_str().map(str::to_string),
            file_kind: a["kind"].as_str().map(str::to_string),
        })
        .collect()
}

/// The resources the agent may fetch of a message (same rules as [`fetch_bound`]).
pub fn resources_bound(
    ch: &Channels,
    ws: &str,
    space_id: &str,
    message_id: &str,
) -> Vec<ResourceRef> {
    ch.store()
        .reader(|c| -> crate::ChannelResult<Vec<ResourceRef>> {
            Ok(match load(c, message_id)? {
                Some(r) if r.ws == ws && r.space_id == space_id && r.direction == "in" => {
                    resources_of(&effective(c, &r).1)
                }
                _ => Vec::new(),
            })
        })
        .unwrap_or_default()
}

// ---------------------------------------------------------------- the agent's note

/// The message text with a note for each attachment Steve does not read:
/// `[图片：未读取内容]`, `[文件 a.pdf：未读取内容]`, `[文件夹 x：无法读取]`, `[表情包]`. Cards are read
/// (their text is the message text) and get no note.
pub fn with_unread_note(text: &str, attachments: &Value) -> String {
    let notes = unread_note(attachments);
    match (text.trim().is_empty(), notes.is_empty()) {
        (_, true) => text.to_string(),
        (true, false) => notes,
        (false, false) => format!("{text}\n{notes}"),
    }
}

/// The notes alone ([`with_unread_note`]).
pub fn unread_note(attachments: &Value) -> String {
    attachments
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|a| {
            let name = a["name"]
                .as_str()
                .map(|n| sanitize_name(n, ""))
                .filter(|n| !n.is_empty());
            let named = |what: &str, tail: &str| match &name {
                Some(n) => format!("[{what} {n}：{tail}]"),
                None => format!("[{what}：{tail}]"),
            };
            match (a["type"].as_str()?, a["kind"].as_str()) {
                ("image", _) => Some("[图片：未读取内容]".to_string()),
                ("file", Some("folder")) => Some(named("文件夹", "无法读取")),
                ("file", Some("sticker")) => Some("[表情包]".to_string()),
                ("file", Some("audio")) => Some(named("音频", "未读取内容")),
                ("file", Some("video")) => Some(named("视频", "未读取内容")),
                ("file", _) => Some(named("文件", "未读取内容")),
                _ => None,
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

// ---------------------------------------------------------------- fetching

static HUB: LazyLock<Arc<Semaphore>> = LazyLock::new(|| Arc::new(Semaphore::new(PER_HUB)));
static PER_CONN: LazyLock<Mutex<HashMap<String, Arc<Semaphore>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

async fn slot(conn: &str) -> Result<(OwnedSemaphorePermit, OwnedSemaphorePermit), MediaError> {
    let mine = {
        let mut m = PER_CONN.lock().unwrap_or_else(|e| e.into_inner());
        m.entry(conn.to_string())
            .or_insert_with(|| Arc::new(Semaphore::new(PER_CONNECTION)))
            .clone()
    };
    let wait = async {
        let a = mine.acquire_owned().await.map_err(|_| MediaError::Busy)?;
        let b = HUB
            .clone()
            .acquire_owned()
            .await
            .map_err(|_| MediaError::Busy)?;
        Ok((a, b))
    };
    tokio::time::timeout(QUEUE_WAIT, wait)
        .await
        .map_err(|_| MediaError::Busy)?
}

/// What a fetch needs, read under one reader.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Plan {
    space_id: String,
    conn: String,
    adapter: String,
    ws: String,
    msg: MessageRef,
    key: String,
    kind: ResourceKind,
    name: Option<String>,
}

fn plan(c: &Connection, r: &Row, index: usize) -> Result<Plan, MediaError> {
    if r.direction != "in" {
        return Err(MediaError::NotFound);
    }
    if r.purged.is_some() || r.text.is_none() {
        return Err(MediaError::Purged);
    }
    if !matches!(r.status.as_str(), "received" | "sent") {
        return Err(MediaError::Recalled);
    }
    let (_, atts) = effective(c, r);
    let a = atts
        .as_array()
        .and_then(|x| x.get(index))
        .filter(|a| is_resource(a))
        .ok_or(MediaError::NotFound)?;
    if let Some(e) = blocked(r, a) {
        return Err(e);
    }
    let (conn, adapter) = connection_of(c, r, stamp_of(a)).ok_or(MediaError::Disconnected)?;
    if !supported(&adapter) {
        return Err(MediaError::Unsupported);
    }
    let msg = r.message_ref(c, Some(&conn)).ok_or(MediaError::NotFound)?;
    Ok(Plan {
        space_id: r.space_id.clone(),
        conn,
        adapter,
        ws: r.ws.clone(),
        msg,
        key: a["key"].as_str().unwrap_or_default().to_string(),
        kind: if a["type"] == "image" {
            ResourceKind::Image
        } else {
            ResourceKind::File
        },
        name: a["name"].as_str().map(str::to_string),
    })
}

/// The route's fetch: message `message_id` must be in a space `viewer` sees.
pub async fn fetch_for_viewer(
    ch: &Channels,
    viewer: &Viewer,
    message_id: &str,
    index: usize,
    limits: Limits,
) -> Result<Fetched, MediaError> {
    let check = || {
        ch.store()
            .reader(|c| -> crate::ChannelResult<Result<Plan, MediaError>> {
                let Some(r) = load(c, message_id)? else {
                    return Ok(Err(MediaError::NotFound));
                };
                if r.ws != viewer.ws
                    || !member(c, &viewer.ws, &viewer.user)?
                    || !spaces::visible(c, viewer, &r.space_id)?
                {
                    return Ok(Err(MediaError::NotFound));
                }
                Ok(plan(c, &r, index))
            })
            .map_err(|_| MediaError::Provider)?
    };
    run(ch, check, limits).await
}

/// Is `user` (still) a member of `ws`? A viewer loaded before a removal must not keep reading.
fn member(c: &Connection, ws: &str, user: &str) -> rusqlite::Result<bool> {
    c.query_row(
        "SELECT EXISTS (SELECT 1 FROM workspace_members WHERE workspace_id = ? AND user_id = ?)",
        params![ws, user],
        |r| r.get(0),
    )
}

/// The agent's fetch: bound to the workspace and the space the turn came from (no viewer); the
/// same connection choice, guards, limits and slots as the route.
pub async fn fetch_bound(
    ch: &Channels,
    ws: &str,
    space_id: &str,
    message_id: &str,
    index: usize,
    limits: Limits,
) -> Result<Fetched, MediaError> {
    let check = || {
        ch.store()
            .reader(|c| -> crate::ChannelResult<Result<Plan, MediaError>> {
                Ok(match load(c, message_id)? {
                    Some(r) if r.ws == ws && r.space_id == space_id => plan(c, &r, index),
                    _ => Err(MediaError::NotFound),
                })
            })
            .map_err(|_| MediaError::Provider)?
    };
    run(ch, check, limits).await
}

/// Plans, downloads, then plans again: a purge, recall, lost visibility or any change of the key,
/// message or connection while the bytes were coming drops them (the second check's error, or
/// `not_found`).
async fn run(
    ch: &Channels,
    check: impl Fn() -> Result<Plan, MediaError>,
    limits: Limits,
) -> Result<Fetched, MediaError> {
    let p = check()?;
    let max = limits.max_bytes.min(MAX_BYTES);
    let go = async {
        let m = ch.media(&p.adapter).ok_or(MediaError::Unsupported)?;
        let _slot = slot(&p.conn).await?;
        let cx = ch
            .connections()
            .cx(&p.ws, &p.conn)
            .map_err(|_| MediaError::Disconnected)?;
        m.download(
            &cx,
            &p.msg,
            &p.key,
            p.kind == ResourceKind::Image,
            usize::try_from(max).unwrap_or(usize::MAX),
        )
        .await
    };
    let bytes = tokio::time::timeout(limits.timeout, go)
        .await
        .map_err(|_| MediaError::Timeout)??;
    if check()? != p {
        return Err(MediaError::NotFound);
    }
    let mime = sniff(&bytes);
    let fallback = match mime {
        "image/png" => "image.png",
        "image/jpeg" => "image.jpg",
        "image/gif" => "image.gif",
        "image/webp" => "image.webp",
        _ => "file",
    };
    Ok(Fetched {
        name: sanitize_name(p.name.as_deref().unwrap_or(""), fallback),
        bytes,
        mime,
        kind: p.kind,
    })
}

/// The raster types a browser may show inline; everything else is a download.
pub fn sniff(b: &[u8]) -> &'static str {
    if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        "image/png"
    } else if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        "image/jpeg"
    } else if b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") {
        "image/gif"
    } else if b.len() >= 12 && &b[..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        "image/webp"
    } else {
        "application/octet-stream"
    }
}

/// A file name without control characters, path separators, `:*?"<>|` or leading dots, ≤120
/// characters; `fallback` when nothing is left.
pub fn sanitize_name(n: &str, fallback: &str) -> String {
    let s: String = n
        .chars()
        .filter(|c| !c.is_control() && !"/\\:*?\"<>|".contains(*c))
        .collect();
    let s = s.trim().trim_start_matches('.').trim();
    let s: String = s.chars().take(120).collect();
    if s.is_empty() {
        fallback.to_string()
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffing_and_names() {
        assert_eq!(sniff(b"\x89PNG\r\n\x1a\nxx"), "image/png");
        assert_eq!(sniff(&[0xFF, 0xD8, 0xFF, 0xE0]), "image/jpeg");
        assert_eq!(sniff(b"GIF89a.."), "image/gif");
        assert_eq!(sniff(b"RIFF\0\0\0\0WEBPVP8 "), "image/webp");
        assert_eq!(sniff(b"<svg xmlns="), "application/octet-stream");
        assert_eq!(sniff(b"<html>"), "application/octet-stream");
        assert_eq!(sanitize_name("../../etc/passwd", "file"), "etcpasswd");
        assert_eq!(sanitize_name("..\u{0}\n", "file"), "file");
        assert_eq!(sanitize_name("报告 v2.pdf", "file"), "报告 v2.pdf");
        assert_eq!(sanitize_name(&"长".repeat(300), "f").chars().count(), 120);
    }

    #[test]
    fn notes_say_what_is_not_read() {
        let a = json!([
            {"type": "card", "blocks": []},
            {"type": "image", "key": "img_1"},
            {"type": "file", "kind": "file", "name": "季度/报告.pdf", "key": "f"},
            {"type": "file", "kind": "folder", "name": "资料", "key": "d"},
            {"type": "file", "kind": "sticker", "key": "s"},
            {"type": "file", "kind": "audio", "key": "a"},
            {"type": "file", "key": "old"},
        ]);
        assert_eq!(
            with_unread_note("看下", &a),
            "看下\n[图片：未读取内容] [文件 季度报告.pdf：未读取内容] [文件夹 资料：无法读取] [表情包] [音频：未读取内容] [文件：未读取内容]"
        );
        assert_eq!(
            with_unread_note("", &json!([{"type": "image"}])),
            "[图片：未读取内容]"
        );
        assert_eq!(with_unread_note("好", &json!([])), "好");
    }

    #[test]
    fn errors_have_stable_reasons_and_statuses() {
        assert_eq!(MediaError::Purged.status(), 410);
        assert_eq!(MediaError::TooLarge.reason(), "too_large");
        assert_eq!(MediaError::Folder.status(), 409);
        assert_eq!(MediaError::Invisible.status(), 502);
        assert_eq!(MediaError::Busy.status(), 429);
        assert_eq!(MediaError::Timeout.status(), 504);
    }

    #[test]
    fn resources_skip_cards_folders_and_missing_keys() {
        let a = json!([
            {"type": "card"},
            {"type": "image", "key": "img_1"},
            {"type": "file", "kind": "folder", "key": "d"},
            {"type": "file", "kind": "video", "key": "v", "name": "x.mp4"},
            {"type": "image", "key": ""},
        ]);
        let r = resources_of(&a);
        assert_eq!(r.iter().map(|x| x.index).collect::<Vec<_>>(), [1, 3]);
        assert_eq!(r[1].file_kind.as_deref(), Some("video"));
    }
}
