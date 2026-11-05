//! You: the self poller (F8, feishu-lark-cli.md §2.3). Feishu pushes nothing a user receives, so the
//! `feishu_user` connection reads your chats with your user token every 20 s and feeds
//! what is new into C1's inbound pipeline with `via=poll`.
//!
//! One round:
//! 1. `GET im/v1/chats?types=p2p,group&sort_type=ByActiveTimeDesc&page_size=20`;
//! 2. the first `top_n` (10) chats, skipping groups a bot connection's long connection covers (an
//!    active space of a `feishu_app` connection), each read once from its cursor
//!    (`start_time=<cursor s>&sort_type=ByCreateTimeAsc&page_size=50`);
//! 3. a chat seen for the first time only gets its cursor (its newest message), no backfill;
//! 4. a new topic root (`thread_id`) makes its thread a container too; up to 5 threads a round
//!    (new ones first, then the least recently read of those active in the last 3 days);
//! 5. a message the long connection already stored (in a bot connection's space) is skipped.
//!
//! Cursors (`chan_cursors`, migration 011) hold the largest `create_time` read and the ids at
//! exactly that millisecond (`start_time` is in seconds, so a second is read again).
//!
//! Pace ([`SelfPoller::next_delay`]): `self_poll.interval` (10–300 s, default 20); 2 minutes in
//! the workspace's quiet hours (setting `quiet`, default “23:00 – 08:00”); 10 s for 5 minutes after
//! a round that read a new private message from someone (this wins over quiet hours: someone is
//! talking to you right now). A rate-limited request ends the round: the wait is the
//! `x-ogw-ratelimit-reset` header's (at least one interval) and the chats not read yet go first
//! next round.
//!
//! Budget: one list plus ≤ 10 chats plus ≤ 5 threads a round, far below the app's 1000/min.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use chrono::{TimeZone, Timelike};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};
use steve_modules::accounts::connections::ConnCx;
use tokio_util::sync::{CancellationToken, DropGuard};

use super::auth::{APP_ADAPTER, APP_SYSTEM, FeishuUser, USER_ADAPTER, USER_SYSTEM};
use super::client::{self, FeishuClient, FeishuError};
use super::inbound::{body_of, handle_of};
use crate::channel::{HandleRef, InboundMessage, Mention, Sender, SpaceKind};
use crate::inbound::Next;
use crate::service::MessageSink;
use crate::{ChannelResult, Channels};

/// `InboundMessage.via` of polled messages.
pub const VIA: &str = "poll";
/// What you see in Feishu should show up here within seconds, not a minute: a round is one chat list plus
/// ≤ 10 chats and ≤ 5 threads, so 20 s is ~50 requests a minute (10 s in a burst ~100), well inside the quota.
pub const DEFAULT_INTERVAL: Duration = Duration::from_secs(20);
pub const MIN_INTERVAL: Duration = Duration::from_secs(10);
pub const MAX_INTERVAL: Duration = Duration::from_secs(300);
/// The pace in quiet hours.
pub const QUIET_INTERVAL: Duration = Duration::from_secs(120);
/// The pace after a new private message …
pub const BURST_INTERVAL: Duration = Duration::from_secs(10);
/// … for this long.
pub const BURST_FOR_MS: i64 = 5 * 60_000;
pub const DEFAULT_TOP_N: usize = 10;
/// Threads read per round at most.
pub const MAX_THREADS: usize = 5;
/// A thread with nothing new for this long is no longer read.
pub const THREAD_ACTIVE_MS: i64 = 3 * 24 * 3_600_000;
/// A chat Feishu refused is tried again after this long.
pub const ERROR_RETRY_MS: i64 = 3_600_000;
/// The workspace setting with the quiet hours, and its default.
pub const QUIET_SETTING: &str = "quiet";
pub const DEFAULT_QUIET: &str = "23:00 – 08:00";

const CHATS_PAGE: &str = "20";
const MESSAGES_PAGE: &str = "50";
const KIND_CHAT: &str = "chat";
const KIND_THREAD: &str = "thread";

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

fn opt(v: &Value, k: &str) -> Option<String> {
    Some(s(v, k).trim().to_string()).filter(|x| !x.is_empty())
}

fn ms(v: &Value, k: &str) -> Option<i64> {
    v.get(k)
        .and_then(|x| x.as_i64().or_else(|| x.as_str()?.parse().ok()))
}

// ---------------------------------------------------------------------------------------------
// Settings.

/// `config.self_poll` of the `feishu_user` connection: `{interval: <s>, top_n: <n>}`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PollSettings {
    pub interval: Duration,
    pub top_n: usize,
}

impl Default for PollSettings {
    fn default() -> Self {
        Self {
            interval: DEFAULT_INTERVAL,
            top_n: DEFAULT_TOP_N,
        }
    }
}

impl PollSettings {
    /// Reads and clamps (interval 10–300 s, top_n 1–20).
    pub fn from_config(config: &Value) -> Self {
        let p = &config["self_poll"];
        let interval = p["interval"]
            .as_u64()
            .map(Duration::from_secs)
            .unwrap_or(DEFAULT_INTERVAL)
            .clamp(MIN_INTERVAL, MAX_INTERVAL);
        let top_n = p["top_n"]
            .as_u64()
            .map(|n| n.clamp(1, 20) as usize)
            .unwrap_or(DEFAULT_TOP_N);
        Self { interval, top_n }
    }
}

/// Quiet hours as minutes of the day `[from, to)`, wrapping midnight when `from > to`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Quiet {
    pub from: u32,
    pub to: u32,
}

impl Quiet {
    /// “23:00 – 08:00” (en dash or hyphen); “关闭” and anything unreadable → `None`.
    pub fn parse(s: &str) -> Option<Self> {
        let (a, b) = s.split_once('–').or_else(|| s.split_once('-'))?;
        let hm = |x: &str| -> Option<u32> {
            let (h, m) = x.trim().split_once(':')?;
            let (h, m) = (h.trim().parse::<u32>().ok()?, m.trim().parse::<u32>().ok()?);
            (h < 24 && m < 60).then_some(h * 60 + m)
        };
        let (from, to) = (hm(a)?, hm(b)?);
        (from != to).then_some(Self { from, to })
    }

    pub fn contains(&self, minute: u32) -> bool {
        if self.from < self.to {
            (self.from..self.to).contains(&minute)
        } else {
            minute >= self.from || minute < self.to
        }
    }
}

/// The workspace's quiet hours (`quiet`, a JSON string) in its `tz`; is `now` inside them?
fn in_quiet_hours(c: &rusqlite::Connection, ws: &str, now: i64) -> ChannelResult<bool> {
    let get = |key: &str| -> ChannelResult<Option<String>> {
        let v: Option<String> = c
            .query_row(
                "SELECT value FROM settings WHERE workspace_id = ? AND key = ?",
                params![ws, key],
                |r| r.get(0),
            )
            .optional()?;
        Ok(v.map(|v| serde_json::from_str::<String>(&v).unwrap_or(v)))
    };
    let quiet = get(QUIET_SETTING)?.unwrap_or_else(|| DEFAULT_QUIET.into());
    let Some(q) = Quiet::parse(&quiet) else {
        return Ok(false);
    };
    let tz = get("tz")?
        .and_then(|t| steve_flow::cron::parse_tz(&t).ok())
        .unwrap_or(chrono_tz_default());
    let Some(local) = tz.timestamp_millis_opt(now).single() else {
        return Ok(false);
    };
    Ok(q.contains(local.hour() * 60 + local.minute()))
}

/// The Hub's default time zone (`DEFAULT_TZ` of the schedules).
fn chrono_tz_default() -> chrono_tz::Tz {
    steve_flow::cron::parse_tz(steve_flow::cron::DEFAULT_TZ).unwrap_or(chrono_tz::UTC)
}

// ---------------------------------------------------------------------------------------------
// Cursors.

/// One `chan_cursors` row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cursor {
    pub container: String,
    pub kind: String,
    pub chat_id: String,
    /// `None`: never read successfully (the first read only sets it).
    pub cursor: Option<i64>,
    pub boundary: Vec<String>,
    pub last_new: Option<i64>,
    pub polled: Option<i64>,
    pub error: Option<String>,
    pub error_at: Option<i64>,
}

const CURSOR_COLS: &str =
    "container, kind, chat_id, cursor, boundary, last_new, polled, error, error_at";

fn cursor_row(r: &rusqlite::Row) -> rusqlite::Result<Cursor> {
    let b: String = r.get(4)?;
    Ok(Cursor {
        container: r.get(0)?,
        kind: r.get(1)?,
        chat_id: r.get(2)?,
        cursor: r.get(3)?,
        boundary: serde_json::from_str(&b).unwrap_or_default(),
        last_new: r.get(5)?,
        polled: r.get(6)?,
        error: r.get(7)?,
        error_at: r.get(8)?,
    })
}

/// The cursor of `container` on connection `conn`.
pub fn cursor(
    c: &rusqlite::Connection,
    conn: &str,
    container: &str,
) -> ChannelResult<Option<Cursor>> {
    Ok(c.query_row(
        &format!(
            "SELECT {CURSOR_COLS} FROM chan_cursors WHERE connection_id = ? AND container = ?"
        ),
        params![conn, container],
        cursor_row,
    )
    .optional()?)
}

fn save_cursor(channels: &Channels, ws: &str, conn: &str, k: &Cursor) -> ChannelResult<()> {
    let now = channels.now();
    let boundary = serde_json::to_string(&k.boundary).unwrap_or_else(|_| "[]".into());
    channels.store().transaction(|tx| -> ChannelResult<()> {
        tx.execute(
            "INSERT INTO chan_cursors (connection_id, workspace_id, container, kind, chat_id, cursor, boundary, last_new, polled, error, error_at, created, updated)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?12)
             ON CONFLICT (connection_id, container) DO UPDATE SET
               cursor = excluded.cursor, boundary = excluded.boundary, last_new = excluded.last_new,
               polled = excluded.polled, error = excluded.error, error_at = excluded.error_at,
               updated = excluded.updated",
            params![
                conn, ws, k.container, k.kind, k.chat_id, k.cursor, boundary, k.last_new,
                k.polled, k.error, k.error_at, now
            ],
        )?;
        Ok(())
    })
}

// ---------------------------------------------------------------------------------------------
// One round.

/// What a round did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RoundReport {
    /// Requests made to Feishu.
    pub requests: u32,
    /// Chats read (first reads included).
    pub chats: usize,
    /// Chats whose cursor was set for the first time (nothing backfilled).
    pub first_seen: usize,
    /// Groups the long connection covers, not read.
    pub skipped_bot: usize,
    /// Chats Feishu refused (recorded on the cursor; tried again in an hour).
    pub failed: usize,
    pub threads: usize,
    /// Messages handed to the pipeline that it stored.
    pub new_messages: usize,
    /// … of them private messages from someone else (they start the fast pace).
    pub new_dms: usize,
    /// Messages the pipeline already had.
    pub duplicates: usize,
    /// Messages the long connection already stored.
    pub by_bot: usize,
    /// The round was cut short by a rate limit: wait at least this long.
    pub rate_limited: Option<Duration>,
    /// Chats moved to the next round.
    pub carried: usize,
    /// The round did not run or stopped (token refused, connection off).
    pub error: Option<String>,
}

/// A chat as the list returns it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ChatRow {
    chat_id: String,
    name: String,
    kind: SpaceKind,
    tenant_key: String,
}

impl ChatRow {
    fn of(v: &Value) -> Option<Self> {
        let chat_id = opt(v, "chat_id")?;
        let kind = match s(v, "chat_mode") {
            "p2p" => SpaceKind::Dm,
            "topic" => SpaceKind::Channel,
            _ => SpaceKind::Group,
        };
        Some(Self {
            chat_id,
            name: s(v, "name").trim().to_string(),
            kind,
            tenant_key: s(v, "tenant_key").to_string(),
        })
    }
}

/// Who “you” are on this connection, and the paired bot app.
#[derive(Debug, Clone, Default)]
struct Ids {
    /// Your `open_id` under this app (mentions of you).
    me_open_id: String,
    app_id: String,
    bot_open_id: String,
}

/// How a request ended for the round.
enum Stop {
    Limited(Option<Duration>),
    Fatal(String),
}

fn stop_of(e: FeishuError) -> Result<FeishuError, Stop> {
    match e {
        FeishuError::RateLimited { reset, .. } => Err(Stop::Limited(reset)),
        FeishuError::TokenInvalid { .. } | FeishuError::Token(_) => Err(Stop::Fatal(e.to_string())),
        other => Ok(other),
    }
}

/// The poller of one `feishu_user` connection. [`SelfPoller::round`] once, or [`SelfPoller::run`]
/// forever.
pub struct SelfPoller {
    channels: Channels,
    user: Arc<FeishuUser>,
    sink: Arc<dyn MessageSink>,
    ws: String,
    conn: String,
    /// Chats a rate limit kept from being read: first next round.
    carry: Vec<ChatRow>,
    /// Unix ms until which the fast pace holds.
    burst_until: i64,
    settings: PollSettings,
}

impl SelfPoller {
    /// `user` is the registered `feishu_user` provider (its hosts, its token refresh).
    pub fn new(channels: Channels, user: Arc<FeishuUser>, ws: &str, conn: &str) -> Self {
        let sink: Arc<dyn MessageSink> = Arc::new(channels.clone());
        Self {
            channels,
            user,
            sink,
            ws: ws.into(),
            conn: conn.into(),
            carry: Vec::new(),
            burst_until: 0,
            settings: PollSettings::default(),
        }
    }

    /// Where messages go (default: [`Channels::on_inbound`]); C3 may wrap it with the rules.
    pub fn with_sink(mut self, sink: Arc<dyn MessageSink>) -> Self {
        self.sink = sink;
        self
    }

    pub fn connection_id(&self) -> &str {
        &self.conn
    }

    /// The settings of the last round (`config.self_poll`).
    pub fn settings(&self) -> PollSettings {
        self.settings
    }

    /// Fast pace until (Unix ms; 0 = not started).
    pub fn burst_until(&self) -> i64 {
        self.burst_until
    }

    /// How long to wait after a round that reported `r`, at `now`.
    pub fn next_delay(&self, r: &RoundReport, now: i64) -> Duration {
        let quiet = self
            .channels
            .store()
            .reader(|c| in_quiet_hours(c, &self.ws, now))
            .unwrap_or(false);
        delay(self.settings.interval, now < self.burst_until, quiet, r)
    }

    /// One round (see the module docs). Errors are in the report; nothing panics or returns early
    /// without saying why.
    pub async fn round(&mut self) -> RoundReport {
        let mut r = RoundReport::default();
        if let Err(e) = self.round_inner(&mut r).await {
            r.error = Some(e);
        }
        if r.new_dms > 0 {
            self.burst_until = self.channels.now() + BURST_FOR_MS;
        }
        r
    }

    async fn round_inner(&mut self, r: &mut RoundReport) -> Result<(), String> {
        let (ws, conn) = (self.ws.clone(), self.conn.clone());
        let cx = self
            .channels
            .connections()
            .cx(&ws, &conn)
            .map_err(|e| e.to_string())?;
        if cx.adapter() != USER_ADAPTER {
            return Err("轮询只用“飞书 · 你本人”的连接".into());
        }
        self.settings = PollSettings::from_config(cx.config());
        let (enabled, ids) = self
            .channels
            .store()
            .reader(|c| facts(c, &conn, cx.config_str("app_connection")))
            .map_err(|e| e.to_string())?;
        if !enabled {
            return Err("连接未启用或没有“接收消息”能力".into());
        }
        let client = self.user.user_client(&cx);

        // 1. your chats, most recently active first
        r.requests += 1;
        let list = client
            .get(
                &cx,
                "/open-apis/im/v1/chats",
                &[
                    ("types", "p2p,group"),
                    ("sort_type", "ByActiveTimeDesc"),
                    ("page_size", CHATS_PAGE),
                    ("user_id_type", "open_id"),
                ],
            )
            .await;
        let list = match list {
            Ok(v) => v,
            Err(e) => {
                return match stop_of(e) {
                    Err(Stop::Limited(reset)) => {
                        r.rate_limited = Some(reset.unwrap_or(Duration::ZERO));
                        r.carried = self.carry.len();
                        Ok(())
                    }
                    Err(Stop::Fatal(m)) => Err(m),
                    Ok(e) => Err(e.to_string()),
                };
            }
        };
        let listed: Vec<ChatRow> = client::data(&list)
            .get("items")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(ChatRow::of)
            .collect();

        // 2. which of them: the carried ones first, then by activity; not the bot's groups
        let now = self.channels.now();
        let covered = self
            .channels
            .store()
            .reader(|c| bot_spaces(c, &ws))
            .map_err(|e| e.to_string())?;
        let mut seen = HashSet::new();
        let mut picked = Vec::new();
        for chat in std::mem::take(&mut self.carry).into_iter().chain(listed) {
            if !seen.insert(chat.chat_id.clone()) {
                continue;
            }
            if chat.kind != SpaceKind::Dm && covered.contains(&chat.chat_id) {
                r.skipped_bot += 1;
                continue;
            }
            if picked.len() < self.settings.top_n {
                picked.push(chat);
            }
        }

        // 3. read them
        let mut new_threads: Vec<Cursor> = Vec::new();
        let mut rest = picked.into_iter();
        while let Some(chat) = rest.next() {
            let k = self
                .channels
                .store()
                .reader(|c| cursor(c, &conn, &chat.chat_id))
                .map_err(|e| e.to_string())?;
            if let Some(k) = &k
                && k.error_at.is_some_and(|t| now - t < ERROR_RETRY_MS)
            {
                continue;
            }
            let k = k.unwrap_or_else(|| Cursor {
                container: chat.chat_id.clone(),
                kind: KIND_CHAT.into(),
                chat_id: chat.chat_id.clone(),
                cursor: None,
                boundary: Vec::new(),
                last_new: None,
                polled: None,
                error: None,
                error_at: None,
            });
            match self
                .read(&cx, &client, &ids, &chat, k, r, &mut new_threads)
                .await
            {
                Ok(()) => r.chats += 1,
                Err(Stop::Limited(reset)) => {
                    r.rate_limited = Some(reset.unwrap_or(Duration::ZERO));
                    self.carry = std::iter::once(chat).chain(rest).collect();
                    r.carried = self.carry.len();
                    return Ok(());
                }
                Err(Stop::Fatal(m)) => return Err(m),
            }
        }

        // 4. threads: the new ones, then the least recently read active ones
        let mut threads = new_threads;
        let more = self
            .channels
            .store()
            .reader(|c| active_threads(c, &conn, now - THREAD_ACTIVE_MS))
            .map_err(|e| e.to_string())?;
        for t in more {
            if !threads.iter().any(|x| x.container == t.container) {
                threads.push(t);
            }
        }
        threads.truncate(MAX_THREADS);
        for t in threads {
            let chat = ChatRow {
                chat_id: t.chat_id.clone(),
                name: String::new(),
                kind: SpaceKind::Group,
                tenant_key: String::new(),
            };
            match self
                .read(&cx, &client, &ids, &chat, t, r, &mut Vec::new())
                .await
            {
                Ok(()) => r.threads += 1,
                Err(Stop::Limited(reset)) => {
                    r.rate_limited = Some(reset.unwrap_or(Duration::ZERO));
                    return Ok(());
                }
                Err(Stop::Fatal(m)) => return Err(m),
            }
        }
        Ok(())
    }

    /// Reads one container from its cursor and hands what is new to the pipeline.
    #[allow(clippy::too_many_arguments)]
    async fn read(
        &self,
        cx: &ConnCx,
        client: &FeishuClient,
        ids: &Ids,
        chat: &ChatRow,
        mut k: Cursor,
        r: &mut RoundReport,
        new_threads: &mut Vec<Cursor>,
    ) -> Result<(), Stop> {
        let (ws, conn) = (self.ws.as_str(), self.conn.as_str());
        let now = self.channels.now();
        let first = k.cursor.is_none();
        let start = k.cursor.map(|c| (c.max(0) / 1000).to_string());
        let mut q = vec![
            ("container_id_type", k.kind.as_str()),
            ("container_id", k.container.as_str()),
            // cards as they were sent (1.0 / 2.0 JSON) instead of the flattened default
            ("card_msg_content_type", "user_card_content"),
        ];
        match &start {
            // the first read: only the newest message, for the cursor
            None => q.extend([("sort_type", "ByCreateTimeDesc"), ("page_size", "1")]),
            Some(st) => q.extend([
                ("start_time", st.as_str()),
                ("sort_type", "ByCreateTimeAsc"),
                ("page_size", MESSAGES_PAGE),
            ]),
        }
        r.requests += 1;
        let res = client.get(cx, "/open-apis/im/v1/messages", &q).await;
        k.polled = Some(now);
        let v = match res {
            Ok(v) => v,
            Err(e) => {
                let e = stop_of(e)?;
                // refused for this chat (no scope for its kind, the bot is not in it …)
                tracing::info!(
                    conn,
                    chat = k.container,
                    "feishu self poll: not readable: {e}"
                );
                r.failed += 1;
                k.error = Some(e.to_string());
                k.error_at = Some(now);
                self.save(&k);
                return Ok(());
            }
        };
        k.error = None;
        k.error_at = None;
        let items: Vec<Value> = client::data(&v)
            .get("items")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        if first {
            r.first_seen += 1;
            match items.first() {
                Some(m) => {
                    k.cursor = Some(ms(m, "create_time").unwrap_or(now));
                    k.boundary = opt(m, "message_id").into_iter().collect();
                }
                None => k.cursor = Some(now),
            }
            self.save(&k);
            return Ok(());
        }

        let mut cur = k.cursor.unwrap_or(0);
        let mut boundary: Vec<String> = k.boundary.clone();
        for m in &items {
            let (Some(id), Some(t)) = (opt(m, "message_id"), ms(m, "create_time")) else {
                continue;
            };
            if t < cur || (t == cur && boundary.contains(&id)) {
                continue;
            }
            let deleted = m.get("deleted").and_then(Value::as_bool).unwrap_or(false);
            if !deleted {
                let bot_has = self
                    .channels
                    .store()
                    .reader(|c| bot_has(c, ws, &chat.chat_id, &id))
                    .unwrap_or(false);
                if bot_has {
                    r.by_bot += 1;
                } else {
                    let msg = map_message(m, chat, ids);
                    let is_root = k.kind == KIND_CHAT
                        && opt(m, "thread_id").is_some()
                        && opt(m, "root_id").is_none_or(|x| x == id);
                    match self.sink.inbound(ws, conn, msg).await {
                        Ok(i) if i.duplicate => r.duplicates += 1,
                        Ok(i) => {
                            r.new_messages += 1;
                            k.last_new = Some(now);
                            if i.type_key == "im.dm" && i.next != Next::Own {
                                r.new_dms += 1;
                            }
                        }
                        Err(e) => {
                            // stop here: the next round tries this one again
                            tracing::warn!(
                                conn,
                                message = id,
                                "feishu self poll: inbound failed: {e}"
                            );
                            break;
                        }
                    }
                    if is_root && let Some(th) = opt(m, "thread_id") {
                        new_threads.push(Cursor {
                            container: th,
                            kind: KIND_THREAD.into(),
                            chat_id: chat.chat_id.clone(),
                            cursor: Some(t),
                            boundary: vec![id.clone()],
                            last_new: Some(now),
                            polled: None,
                            error: None,
                            error_at: None,
                        });
                    }
                }
            }
            if t > cur {
                cur = t;
                boundary.clear();
            }
            if !boundary.contains(&id) {
                boundary.push(id);
            }
        }
        k.cursor = Some(cur);
        k.boundary = boundary;
        self.save(&k);
        for th in new_threads.iter() {
            let exists = self
                .channels
                .store()
                .reader(|c| cursor(c, conn, &th.container))
                .ok()
                .flatten()
                .is_some();
            if !exists {
                self.save(th);
            }
        }
        Ok(())
    }

    fn save(&self, k: &Cursor) {
        if let Err(e) = save_cursor(&self.channels, &self.ws, &self.conn, k) {
            tracing::warn!(conn = self.conn, "feishu self poll: cursor not saved: {e}");
        }
    }

    /// Rounds until `stop` is cancelled.
    pub async fn run(mut self, stop: CancellationToken) {
        loop {
            if stop.is_cancelled() {
                return;
            }
            let r = self.round().await;
            if let Some(e) = &r.error {
                tracing::info!(conn = self.conn, "feishu self poll: {e}");
            }
            let d = self.next_delay(&r, self.channels.now());
            tokio::select! {
                _ = tokio::time::sleep(d) => {}
                _ = stop.cancelled() => return,
            }
        }
    }
}

/// The pace rule (module docs).
fn delay(interval: Duration, burst: bool, quiet: bool, r: &RoundReport) -> Duration {
    let base = if burst || r.new_dms > 0 {
        BURST_INTERVAL
    } else if quiet {
        QUIET_INTERVAL.max(interval)
    } else {
        interval
    };
    match r.rate_limited {
        Some(reset) => base.max(reset).max(interval),
        None => base,
    }
}

/// Is the connection on with `msg.receive`, and the ids the mapping needs.
fn facts(
    c: &rusqlite::Connection,
    conn: &str,
    app_conn: Option<&str>,
) -> ChannelResult<(bool, Ids)> {
    let row: Option<(String, String, String)> = c
        .query_row(
            "SELECT status, caps, account_id FROM connections WHERE id = ?",
            [conn],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((status, caps, account)) = row else {
        return Ok((false, Ids::default()));
    };
    let caps: Vec<String> = serde_json::from_str(&caps).unwrap_or_default();
    let enabled =
        matches!(status.as_str(), "ok" | "error") && caps.iter().any(|x| x == "msg.receive");
    let me_open_id: String = c
        .query_row(
            "SELECT handle FROM account_handles WHERE account_id = ? AND handle LIKE 'ou\\_%' ESCAPE '\\' ORDER BY created LIMIT 1",
            [&account],
            |r| r.get(0),
        )
        .optional()?
        .unwrap_or_default();
    let mut ids = Ids {
        me_open_id,
        ..Ids::default()
    };
    if let Some(app) = app_conn {
        let config: Option<String> = c
            .query_row("SELECT config FROM connections WHERE id = ?", [app], |r| {
                r.get(0)
            })
            .optional()?;
        let v: Value = config
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(Value::Null);
        ids.app_id = s(&v, "app_id").to_string();
        ids.bot_open_id = s(&v, "bot_open_id").to_string();
    }
    Ok((enabled, ids))
}

/// Chats a bot connection's long connection really covers: an active space of a healthy (`ok`) bot whose
/// last permission check found `im:message.group_msg` granted. Without that scope a bot receives only the
/// messages that @ it, so a group it merely sits in is still read here (what the bot did store is skipped
/// message by message, [`bot_has`]); a bot in `error` may be offline. Unknown counts as not covered.
fn bot_spaces(c: &rusqlite::Connection, ws: &str) -> ChannelResult<HashSet<String>> {
    let mut st = c.prepare(
        "SELECT s.external_id, c.config FROM spaces s JOIN space_identities i ON i.space_id = s.id AND i.left_at IS NULL
           JOIN connections c ON c.id = i.connection_id
          WHERE s.workspace_id = ? AND c.adapter = ? AND c.status = 'ok' AND s.status = 'active'",
    )?;
    let rows = st
        .query_map(params![ws, APP_ADAPTER], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows
        .into_iter()
        .filter(|(_, config)| all_group_messages(config))
        .map(|(chat, _)| chat)
        .collect())
}

/// The bot's recorded permission check says it receives every group message (`im:message.group_msg`).
fn all_group_messages(config: &str) -> bool {
    serde_json::from_str::<Value>(config)
        .ok()
        .and_then(|v| v.get("permissions").and_then(Value::as_array).cloned())
        .is_some_and(|ps| {
            ps.iter().any(|p| {
                p.get("scope").and_then(Value::as_str) == Some("im:message.group_msg")
                    && p.get("status").and_then(Value::as_str) == Some("granted")
            })
        })
}

/// The long connection already stored message `ext` of chat `chat`.
fn bot_has(c: &rusqlite::Connection, ws: &str, chat: &str, ext: &str) -> ChannelResult<bool> {
    Ok(c.query_row(
        "SELECT EXISTS (SELECT 1 FROM spaces s JOIN space_identities i ON i.space_id = s.id
                          JOIN connections c ON c.id = i.connection_id
                          JOIN chan_messages m ON m.space_id = s.id AND m.external_id = ?3
                         WHERE s.workspace_id = ?1 AND s.external_id = ?2 AND c.adapter = ?4)",
        params![ws, chat, ext, APP_ADAPTER],
        |r| r.get(0),
    )?)
}

/// Thread cursors with something new since `since`, least recently read first.
fn active_threads(c: &rusqlite::Connection, conn: &str, since: i64) -> ChannelResult<Vec<Cursor>> {
    let mut st = c.prepare(&format!(
        "SELECT {CURSOR_COLS} FROM chan_cursors WHERE connection_id = ? AND kind = 'thread' AND last_new >= ?
          ORDER BY coalesce(polled, 0), container LIMIT ?"
    ))?;
    let rows = st
        .query_map(params![conn, since, MAX_THREADS as i64], cursor_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// One item of `GET im/v1/messages` → [`InboundMessage`] (`via=poll`).
fn map_message(m: &Value, chat: &ChatRow, ids: &Ids) -> InboundMessage {
    let id = s(m, "message_id").to_string();
    let sender = &m["sender"];
    let is_app = s(sender, "sender_type") == "app";
    let sid = s(sender, "id");
    let handle = if is_app {
        // the list names apps by app_id; the bot account is known by its open_id
        if !ids.app_id.is_empty() && sid == ids.app_id && !ids.bot_open_id.is_empty() {
            HandleRef::new(APP_SYSTEM, ids.bot_open_id.clone())
        } else {
            HandleRef::new(APP_SYSTEM, sid)
        }
    } else {
        HandleRef::new(USER_SYSTEM, sid)
    };
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
            // the message API gives a string id (`id_type`), the event an object
            let (open, h) = match &x["id"] {
                Value::String(i) => (
                    i.clone(),
                    (!i.is_empty() && matches!(s(x, "id_type"), "" | "open_id"))
                        .then(|| HandleRef::new(USER_SYSTEM, i.clone())),
                ),
                obj => (s(obj, "open_id").to_string(), handle_of(obj, false)),
            };
            let me = !ids.me_open_id.is_empty() && open == ids.me_open_id;
            Mention {
                handle: if me { None } else { h },
                name: s(x, "name").to_string(),
                is_connection: me,
            }
        })
        .collect();
    let (text, attachments) = body_of(s(m, "msg_type"), s(&m["body"], "content"), &keys);
    let mine = !ids.me_open_id.is_empty() && sid == ids.me_open_id;
    // a p2p chat is named after the other party: that is who sent it, unless you did
    let display_name = (chat.kind == SpaceKind::Dm && !mine && !is_app && !chat.name.is_empty())
        .then(|| chat.name.clone());
    let thread_key = opt(m, "thread_id")
        .or_else(|| opt(m, "root_id"))
        .unwrap_or_else(|| id.clone());
    InboundMessage {
        space_ext_id: chat.chat_id.clone(),
        space_kind: Some(chat.kind),
        space_name: (!chat.name.is_empty()).then(|| chat.name.clone()),
        external_id: id,
        event_id: None,
        thread_key: Some(thread_key),
        reply_to: opt(m, "parent_id"),
        root_id: opt(m, "root_id"),
        sender: Sender {
            handle,
            display_name,
            is_bot: is_app,
            external: !chat.tenant_key.is_empty() && !stk.is_empty() && stk != chat.tenant_key,
        },
        text,
        attachments,
        mentions,
        created: ms(m, "create_time").unwrap_or_else(steve_store::now),
        via: Some(VIA.into()),
        raw: json!(m),
    }
}

// ---------------------------------------------------------------------------------------------
// All pollers.

struct Running {
    /// Cancels the poller when it is stopped or dropped (the pollers with it).
    stop: DropGuard,
    task: tokio::task::JoinHandle<()>,
}

impl Running {
    fn spawn(p: SelfPoller) -> Self {
        let stop = CancellationToken::new();
        let task = tokio::spawn(p.run(stop.clone()));
        Self {
            stop: stop.drop_guard(),
            task,
        }
    }
}

/// The pollers of all `feishu_user` connections with `msg.receive` (one task each).
pub struct SelfPollers {
    channels: Channels,
    user: Arc<FeishuUser>,
    sink: Option<Arc<dyn MessageSink>>,
    running: tokio::sync::Mutex<std::collections::HashMap<String, Running>>,
}

impl SelfPollers {
    pub fn new(channels: Channels, user: Arc<FeishuUser>) -> Self {
        Self {
            channels,
            user,
            sink: None,
            running: tokio::sync::Mutex::default(),
        }
    }

    /// See [`SelfPoller::with_sink`].
    pub fn with_sink(mut self, sink: Arc<dyn MessageSink>) -> Self {
        self.sink = Some(sink);
        self
    }

    /// Starts one for every `feishu_user` connection that is `ok` or `error`, has `msg.receive`
    /// and is not running yet. Returns how many were started.
    pub async fn start_all(&self) -> usize {
        let conns = self
            .channels
            .store()
            .reader(|c| -> ChannelResult<Vec<(String, String)>> {
                let mut st = c.prepare(
                    "SELECT id, workspace_id FROM connections
                  WHERE adapter = ? AND status IN ('ok', 'error') AND caps LIKE '%\"msg.receive\"%'
                  ORDER BY created, id",
                )?;
                let rows = st
                    .query_map([USER_ADAPTER], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<rusqlite::Result<Vec<_>>>()?;
                Ok(rows)
            });
        let conns = match conns {
            Ok(c) => c,
            Err(e) => {
                tracing::warn!("feishu self poll: listing connections failed: {e}");
                return 0;
            }
        };
        let mut running = self.running.lock().await;
        running.retain(|_, r| !r.task.is_finished());
        let mut n = 0;
        for (id, ws) in conns {
            if running.contains_key(&id) {
                continue;
            }
            let mut p = SelfPoller::new(self.channels.clone(), self.user.clone(), &ws, &id);
            if let Some(s) = &self.sink {
                p = p.with_sink(s.clone());
            }
            running.insert(id, Running::spawn(p));
            n += 1;
        }
        n
    }

    /// Starts the poller of `feishu_user` connection `conn` of `ws` unless one is running (a
    /// connection just authorized or resumed). The caller checks status and `msg.receive`.
    /// Returns whether it was started now.
    pub async fn start_one(&self, ws: &str, conn: &str) -> bool {
        let mut running = self.running.lock().await;
        running.retain(|_, r| !r.task.is_finished());
        if running.contains_key(conn) {
            return false;
        }
        let mut p = SelfPoller::new(self.channels.clone(), self.user.clone(), ws, conn);
        if let Some(s) = &self.sink {
            p = p.with_sink(s.clone());
        }
        running.insert(conn.to_string(), Running::spawn(p));
        true
    }

    /// Stops the poller of `conn` (disconnected or paused); a round in progress finishes.
    pub async fn stop(&self, conn: &str) -> bool {
        let Some(r) = self.running.lock().await.remove(conn) else {
            return false;
        };
        let Running { stop, mut task } = r;
        drop(stop);
        if tokio::time::timeout(Duration::from_secs(30), &mut task)
            .await
            .is_err()
        {
            task.abort();
        }
        true
    }

    /// Connection ids with a running poller.
    pub async fn running(&self) -> Vec<String> {
        let r = self.running.lock().await;
        r.iter()
            .filter(|(_, x)| !x.task.is_finished())
            .map(|(k, _)| k.clone())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn settings_are_clamped() {
        assert_eq!(
            PollSettings::from_config(&json!({})),
            PollSettings::default()
        );
        let p = PollSettings::from_config(&json!({"self_poll": {"interval": 5, "top_n": 99}}));
        assert_eq!(p.interval, MIN_INTERVAL);
        assert_eq!(p.top_n, 20);
        let p = PollSettings::from_config(&json!({"self_poll": {"interval": 9999}}));
        assert_eq!(p.interval, MAX_INTERVAL);
        let p = PollSettings::from_config(&json!({"self_poll": {"interval": 120, "top_n": 3}}));
        assert_eq!((p.interval, p.top_n), (Duration::from_secs(120), 3));
    }

    #[test]
    fn quiet_hours_parse_and_wrap() {
        let q = Quiet::parse("23:00 – 08:00").expect("quiet");
        assert_eq!(
            q,
            Quiet {
                from: 23 * 60,
                to: 8 * 60
            }
        );
        assert!(q.contains(23 * 60) && q.contains(0) && q.contains(7 * 60 + 59));
        assert!(!q.contains(8 * 60) && !q.contains(12 * 60));
        let d = Quiet::parse("09:00-17:30").expect("day");
        assert!(d.contains(9 * 60) && !d.contains(17 * 60 + 30) && !d.contains(8 * 60));
        assert_eq!(Quiet::parse("关闭"), None);
        assert_eq!(Quiet::parse("25:00 – 08:00"), None);
        assert_eq!(Quiet::parse("08:00 – 08:00"), None);
    }

    #[test]
    fn pace() {
        let i = DEFAULT_INTERVAL;
        let r = RoundReport::default();
        assert_eq!(delay(i, false, false, &r), i);
        assert_eq!(delay(i, false, true, &r), QUIET_INTERVAL);
        assert_eq!(
            delay(i, true, true, &r),
            BURST_INTERVAL,
            "a conversation wins"
        );
        let dm = RoundReport {
            new_dms: 1,
            ..RoundReport::default()
        };
        assert_eq!(delay(i, false, false, &dm), BURST_INTERVAL);
        let limited = |s| RoundReport {
            rate_limited: Some(Duration::from_secs(s)),
            ..RoundReport::default()
        };
        assert_eq!(
            delay(i, false, false, &limited(1)),
            i,
            "at least one interval"
        );
        assert_eq!(
            delay(i, false, false, &limited(90)),
            Duration::from_secs(90)
        );
        assert_eq!(delay(i, true, false, &limited(1)), i);
    }

    #[test]
    fn maps_a_listed_message() {
        let chat = ChatRow {
            chat_id: "oc_1".into(),
            name: "张伟".into(),
            kind: SpaceKind::Dm,
            tenant_key: "tk".into(),
        };
        let ids = Ids {
            me_open_id: "ou_me".into(),
            app_id: "cli_a".into(),
            bot_open_id: "ou_bot".into(),
        };
        let m = json!({
            "message_id": "om_1", "msg_type": "text", "create_time": "1700000000123",
            "chat_id": "oc_1", "thread_id": "omt_1",
            "sender": {"id": "ou_bob", "id_type": "open_id", "sender_type": "user", "tenant_key": "tk"},
            "body": {"content": "{\"text\":\"@_user_1 在吗\"}"},
            "mentions": [{"key": "@_user_1", "id": "ou_me", "id_type": "open_id", "name": "李鹏翔", "tenant_key": "tk"}],
        });
        let x = map_message(&m, &chat, &ids);
        assert_eq!(x.text, "@李鹏翔 在吗");
        assert_eq!(x.sender.handle, HandleRef::new(USER_SYSTEM, "ou_bob"));
        assert_eq!(x.sender.display_name.as_deref(), Some("张伟"));
        assert!(x.mentions_connection());
        assert_eq!(x.created, 1_700_000_000_123);
        assert_eq!(x.thread_key.as_deref(), Some("omt_1"));
        assert_eq!(x.space_kind, Some(SpaceKind::Dm));
        assert_eq!(x.via.as_deref(), Some(VIA));

        let mut bot = m.clone();
        bot["sender"] =
            json!({"id": "cli_a", "id_type": "app_id", "sender_type": "app", "tenant_key": "tk"});
        let x = map_message(&bot, &chat, &ids);
        assert!(x.sender.is_bot);
        assert_eq!(x.sender.handle, HandleRef::new(APP_SYSTEM, "ou_bot"));
        assert_eq!(x.sender.display_name, None);

        let mut mine = m;
        mine["sender"]["id"] = json!("ou_me");
        assert_eq!(map_message(&mine, &chat, &ids).sender.display_name, None);
    }
}
