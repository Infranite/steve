//! [`ExtWs`]: the Hub's side of one kind of connection to an external channel service. One object
//! is every view of the channel side: how a connection is made and verified
//! ([`ConnectionProvider`]), acting as you ([`ActAsUser`]), the media of its messages
//! ([`MediaFetch`]), history for an agent ([`HistoryRead`]) and people ([`ProfileAdapter`]). The
//! feed itself is [`super::ExtWsHook`].
//!
//! Every call goes over the connection's live session when its feed runs, or else over a session
//! opened for that call and closed after it.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use serde_json::{Value, json};
use steve_flow::AdapterError;
use steve_modules::accounts::connections::{
    AccountInfo, AuthKind, CapSpec, ConnCx, ConnectionProvider, FieldDef, Health, Integration,
    ProviderError, StartCx,
};
use tokio::time::Instant;
use zeroize::Zeroizing;

use super::endpoint::{self, Endpoint};
use super::session::{CallError, OpenError, Session, Timing};
use super::wire::{
    self, Handle, HandleKind, Reason, SessionStatus, WireMessage, WireSender, WritePolicy,
};
use super::{ADAPTER, SYSTEM};
use crate::channel::{
    ActAsUser, ActTarget, HandleRef, InboundMessage, Mention, MessageRef, OutContent, Profile,
    ProfileAdapter, ProfileAnswer, ProfileQuery, ReactionKind, SendKey, Sender, Sent,
};
use crate::history::HistoryRead;
use crate::media::{MediaError, MediaFetch};

/// The credential form's keys: the URL and the pin go to `config`, the token to the vault.
pub const URL: &str = "url";
pub const TOKEN: &str = "token";
pub const PIN: &str = "pin";
/// `config` keys `verify` stores: the realm and your primary handle as the service gave them.
pub const REALM: &str = "realm";
pub const ME: &str = "me";

/// How many times a read or a write is tried again (§2, §7).
const RETRIES: usize = 3;
/// Messages per history page and pages per gap and per chat (§5.5).
pub const GAP_PAGE: usize = 50;
pub const GAP_PAGES: usize = 10;

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// A token bucket pacing one connection's writes (§2).
struct Bucket {
    tokens: f64,
    at: Instant,
}

/// The live sessions of the feeds, by connection id, and each connection's write pacing.
#[derive(Default)]
pub(crate) struct Registry {
    live: Mutex<HashMap<String, Arc<Session>>>,
    buckets: Mutex<HashMap<String, Bucket>>,
}

/// The external channel connection (`ext_ws`).
pub struct ExtWs {
    fields: Vec<FieldDef>,
    pub(crate) timing: Timing,
    pub(crate) reg: Registry,
}

impl Default for ExtWs {
    fn default() -> Self {
        Self::new()
    }
}

/// Who `me.get` says is signed in (§4.1).
#[derive(Debug, Clone, PartialEq)]
pub struct Me {
    pub realm: String,
    pub primary: Handle,
    pub display: String,
    pub aliases: Vec<Handle>,
}

impl ExtWs {
    pub fn new() -> Self {
        Self::with_timing(Timing::default())
    }

    /// With other waits (tests).
    pub fn with_timing(timing: Timing) -> Self {
        Self {
            fields: vec![
                FieldDef::plain(URL, "渠道服务地址")
                    .with_hint("wss://…；明文 ws:// 只允许 127.0.0.1 或 ::1"),
                FieldDef::secret(TOKEN, "访问令牌"),
                FieldDef::plain(PIN, "证书指纹")
                    .optional()
                    .with_hint("自签名证书时填 sha256:<64 位十六进制>"),
            ],
            timing,
            reg: Registry::default(),
        }
    }

    /// The feed's session of connection `conn` is this one from now on (`None`: it ended).
    pub(crate) fn set_live(&self, conn: &str, s: Option<Arc<Session>>) {
        let mut live = lock(&self.reg.live);
        match s {
            Some(s) => {
                live.insert(conn.to_string(), s);
            }
            None => {
                live.remove(conn);
            }
        }
    }

    fn live(&self, conn: &str) -> Option<Arc<Session>> {
        lock(&self.reg.live)
            .get(conn)
            .filter(|s| !s.is_closed())
            .cloned()
    }

    /// The endpoint and token of connection `cx`, checked again before every dial (§1).
    pub(crate) async fn endpoint(
        &self,
        cx: &ConnCx,
    ) -> Result<(Endpoint, Zeroizing<String>), ProviderError> {
        let url = cx.config_str(URL).unwrap_or_default();
        let ep = endpoint::check(url, cx.config_str(PIN))
            .map_err(|e| ProviderError::Rejected(e.message()))?;
        let token = cx.secret(TOKEN).await?;
        if !endpoint::token_ok(&token) {
            return Err(ProviderError::Rejected(endpoint::Refused::Token.message()));
        }
        Ok((ep, token))
    }

    /// Opens a session of connection `cx` and asks who is signed in.
    pub(crate) async fn open(
        &self,
        cx: &ConnCx,
    ) -> Result<
        (
            Arc<Session>,
            tokio::sync::mpsc::Receiver<super::session::Note>,
            Me,
        ),
        OpenFail,
    > {
        let (ep, token) = self.endpoint(cx).await.map_err(OpenFail::Provider)?;
        let now = cx.connections().modules().clock.now();
        let (s, notes) = Session::open(&ep, &token, self.timing, now)
            .await
            .map_err(OpenFail::Open)?;
        drop(token);
        match me(&s).await {
            Ok(m) => Ok((s, notes, m)),
            Err(e) => {
                s.close(1000, "me.get failed");
                Err(OpenFail::Provider(call_err(e)))
            }
        }
    }

    /// A session to call on: the feed's, or one opened for this call (closed when the returned
    /// guard goes) whose identity is checked first.
    async fn session(&self, cx: &ConnCx) -> Result<Use, ProviderError> {
        if let Some(s) = self.live(cx.id()) {
            return Ok(Use { s, own: false });
        }
        let (s, _notes, me) = self.open(cx).await.map_err(OpenFail::into_provider)?;
        if let Err(e) = same_identity(cx.config(), &me) {
            s.close(1000, "account changed");
            return Err(e);
        }
        Ok(Use { s, own: true })
    }

    /// Waits for the connection's next write slot (§2: `write_per_sec`, `write_burst`).
    async fn pace(&self, conn: &str, s: &Session) {
        let Some(rate) = s.limits().write_per_sec else {
            return;
        };
        let burst = f64::from(s.limits().write_burst.unwrap_or(1).max(1));
        loop {
            let wait = {
                let mut b = lock(&self.reg.buckets);
                let now = Instant::now();
                let e = b.entry(conn.to_string()).or_insert(Bucket {
                    tokens: burst,
                    at: now,
                });
                e.tokens = (e.tokens + now.duration_since(e.at).as_secs_f64() * rate).min(burst);
                e.at = now;
                if e.tokens >= 1.0 {
                    e.tokens -= 1.0;
                    return;
                }
                Duration::from_secs_f64((1.0 - e.tokens) / rate)
            };
            tokio::time::sleep(wait).await;
        }
    }

    /// One write as you: paced, retried with the same idempotency key on `rate_limited` (after
    /// its hint plus up to 20 %) and on transient errors (§2, §4.4).
    async fn write(
        &self,
        cx: &ConnCx,
        method: &str,
        feature: &str,
        mut params: Value,
        key: Option<String>,
    ) -> Result<Value, ProviderError> {
        let u = self.session(cx).await?;
        let s = &u.s;
        writable(s)?;
        let mut method = method;
        if !s.has(feature) {
            // a service that cannot reply in place still posts in the chat
            if method == "messages.reply" && s.has("messages.send") {
                method = "messages.send";
                if let Some(o) = params.as_object_mut() {
                    o.remove("to");
                    o.remove("in_thread");
                }
            } else {
                return Err(ProviderError::Unsupported);
            }
        }
        params["expected_sender"] = expected_sender(cx)?;
        if let Some(k) = key {
            params["idempotency_key"] = k.into();
        }
        let mut backoff = Duration::from_secs(1);
        for attempt in 0..=RETRIES {
            self.pace(cx.id(), s).await;
            match s.call(method, params.clone(), s.timing().call).await {
                Ok(v) => return Ok(v),
                Err(e) => {
                    let wait = match e.reason() {
                        Some(Reason::RateLimited) => Some(match &e {
                            CallError::Rpc(r) => r
                                .retry_after_ms()
                                .map(|ms| Duration::from_millis(ms + jitter(ms / 5)))
                                .unwrap_or_else(|| full_jitter(backoff)),
                            _ => backoff,
                        }),
                        Some(Reason::Internal | Reason::UpstreamUnavailable | Reason::Timeout) => {
                            Some(full_jitter(backoff))
                        }
                        // the outcome is unknown: only the same key goes again
                        None if matches!(e, CallError::Timeout)
                            && params.get("idempotency_key").is_some() =>
                        {
                            Some(full_jitter(backoff))
                        }
                        _ => None,
                    };
                    if let Some(Reason::WriteForbidden) = e.reason() {
                        s.forbid_writes();
                    }
                    if let Some(Reason::AccountChanged | Reason::Unauthorized) = e.reason() {
                        s.close(1000, "account changed");
                    }
                    match wait {
                        Some(w) if attempt < RETRIES => {
                            tokio::time::sleep(w).await;
                            backoff = (backoff * 2).min(Duration::from_secs(60));
                        }
                        _ => return Err(call_err(e)),
                    }
                }
            }
        }
        unreachable!("the loop returns")
    }

    /// One read, retried up to 3 times on transient errors (§2).
    pub(crate) async fn read(
        s: &Session,
        method: &str,
        params: Value,
        timeout: Duration,
    ) -> Result<Value, CallError> {
        let mut backoff = Duration::from_millis(500);
        let mut last = CallError::Timeout;
        for attempt in 0..=RETRIES {
            match s.call(method, params.clone(), timeout).await {
                Ok(v) => return Ok(v),
                Err(e) => {
                    let wait = match (&e, e.reason()) {
                        (CallError::Rpc(r), Some(Reason::RateLimited)) => r
                            .retry_after_ms()
                            .map(|ms| Duration::from_millis(ms + jitter(ms / 5)))
                            .or(Some(full_jitter(backoff))),
                        (
                            _,
                            Some(Reason::Internal | Reason::UpstreamUnavailable | Reason::Timeout),
                        ) => Some(full_jitter(backoff)),
                        (CallError::Timeout, None) => Some(Duration::ZERO),
                        _ => None,
                    };
                    if e.reason() == Some(Reason::Unsupported) {
                        s.drop_feature(method);
                    }
                    last = e;
                    match wait {
                        Some(w) if attempt < RETRIES && !s.is_closed() => {
                            tokio::time::sleep(w).await;
                            backoff = (backoff * 2).min(Duration::from_secs(30));
                        }
                        _ => return Err(last),
                    }
                }
            }
        }
        Err(last)
    }
}

/// Why opening a session for a connection failed.
#[derive(Debug)]
pub(crate) enum OpenFail {
    Provider(ProviderError),
    Open(OpenError),
}

impl OpenFail {
    pub(crate) fn into_provider(self) -> ProviderError {
        match self {
            Self::Provider(p) => p,
            Self::Open(o) => open_err(o),
        }
    }
}

/// An [`OpenError`] for the connection's health and the user.
pub(crate) fn open_err(e: OpenError) -> ProviderError {
    match e {
        OpenError::Auth(_) => ProviderError::Auth(steve_i18n::t!("channels.ext_ws.unauthorized")),
        OpenError::Refused(m) => ProviderError::Rejected(m),
        OpenError::Version(m) => {
            ProviderError::Rejected(steve_i18n::t!("channels.ext_ws.version", detail = m))
        }
        OpenError::Missing(m) => ProviderError::Rejected(steve_i18n::t!(
            "channels.ext_ws.missing_features",
            features = m.join(", ")
        )),
        OpenError::Transport(m) => {
            ProviderError::Transient(steve_i18n::t!("channels.ext_ws.unreachable", detail = m))
        }
    }
}

/// A [`CallError`] for the caller (§7's "What the Hub does").
pub(crate) fn call_err(e: CallError) -> ProviderError {
    match e {
        CallError::Rpc(r) => match r.reason {
            Reason::Unauthorized => {
                ProviderError::Auth(steve_i18n::t!("channels.ext_ws.unauthorized"))
            }
            Reason::AccountChanged => {
                ProviderError::Auth(steve_i18n::t!("channels.ext_ws.account_changed"))
            }
            Reason::Unsupported => ProviderError::Unsupported,
            Reason::SessionExpired => {
                ProviderError::Rejected(steve_i18n::t!("channels.ext_ws.session_expired"))
            }
            Reason::SessionLocked => {
                ProviderError::Rejected(steve_i18n::t!("channels.ext_ws.session_locked"))
            }
            Reason::WriteForbidden => {
                ProviderError::Rejected(steve_i18n::t!("channels.ext_ws.read_only"))
            }
            Reason::TargetForbidden => {
                ProviderError::Rejected(steve_i18n::t!("channels.ext_ws.target_forbidden"))
            }
            Reason::Invalid
            | Reason::NotFound
            | Reason::Forbidden
            | Reason::TooLarge
            | Reason::IdempotencyConflict => ProviderError::Rejected(steve_i18n::t!(
                "channels.ext_ws.refused",
                detail = r.to_string()
            )),
            _ => ProviderError::Transient(steve_i18n::t!(
                "channels.ext_ws.unavailable",
                detail = r.to_string()
            )),
        },
        CallError::TooLarge => ProviderError::Rejected(steve_i18n::t!("channels.ext_ws.too_large")),
        e => ProviderError::Transient(steve_i18n::t!(
            "channels.ext_ws.unavailable",
            detail = e.to_string()
        )),
    }
}

/// A session borrowed for one call; one opened for it is closed after.
struct Use {
    s: Arc<Session>,
    own: bool,
}

impl Drop for Use {
    fn drop(&mut self) {
        if self.own {
            self.s.close(1000, "done");
        }
    }
}

/// `0..=max` ms.
fn jitter(max: u64) -> u64 {
    if max == 0 {
        0
    } else {
        rand::random_range(0..=max)
    }
}

/// Full jitter: anywhere in `0..=d`.
pub(crate) fn full_jitter(d: Duration) -> Duration {
    Duration::from_millis(jitter(d.as_millis() as u64))
}

/// Writes may go out: not read-only, the remote session usable (§3.6, §9.2).
fn writable(s: &Session) -> Result<(), ProviderError> {
    let st = s.status();
    if st.write_policy == WritePolicy::Forbidden {
        return Err(ProviderError::Rejected(steve_i18n::t!(
            "channels.ext_ws.read_only"
        )));
    }
    match st.status {
        SessionStatus::Ok => Ok(()),
        SessionStatus::Expired => Err(ProviderError::Rejected(steve_i18n::t!(
            "channels.ext_ws.session_expired"
        ))),
        SessionStatus::Locked => Err(ProviderError::Rejected(steve_i18n::t!(
            "channels.ext_ws.session_locked"
        ))),
    }
}

/// `me.get`, checked (§4.1).
pub(crate) async fn me(s: &Session) -> Result<Me, CallError> {
    let v = ExtWs::read(s, "me.get", json!({}), s.timing().call).await?;
    let bad = |m: &str| CallError::Rpc(wire::RpcError::new(Reason::Invalid, m));
    let realm = v
        .get("realm")
        .and_then(Value::as_str)
        .unwrap_or(&s.hello().realm)
        .to_string();
    if realm != s.hello().realm {
        return Err(bad("me.get names another realm than hello"));
    }
    let user = v.get("user").ok_or_else(|| bad("me.get without a user"))?;
    let primary = Handle::parse(user.get("id").unwrap_or(&Value::Null)).map_err(|e| bad(&e))?;
    if primary.kind == HandleKind::Platform {
        return Err(bad("a platform handle is never a primary handle"));
    }
    Ok(Me {
        realm,
        display: wire::text_field(user, "display", 256).unwrap_or_else(|| primary.value.clone()),
        aliases: wire::aliases(user),
        primary,
    })
}

/// The connection's account handle for `me` (in [`SYSTEM`]).
fn account_handle(me: &Me) -> String {
    format!(
        "{}:{}:{}",
        me.realm,
        me.primary.kind.name(),
        me.primary.value
    )
}

/// The identity is the one the connection was made with (§4.1, §5.7): else `account_changed`.
pub(crate) fn same_identity(config: &Value, me: &Me) -> Result<(), ProviderError> {
    let realm = config.get(REALM).and_then(Value::as_str);
    let stored = config.get(ME).and_then(|m| Handle::parse(m).ok());
    let same = realm.is_none_or(|r| r == me.realm) && stored.is_none_or(|h| h == me.primary);
    if same {
        Ok(())
    } else {
        Err(ProviderError::Auth(steve_i18n::t!(
            "channels.ext_ws.account_changed"
        )))
    }
}

/// `expected_sender` of every write (§4.4): the primary handle `me.get` gave at connect time.
fn expected_sender(cx: &ConnCx) -> Result<Value, ProviderError> {
    cx.config()
        .get(ME)
        .and_then(|m| Handle::parse(m).ok())
        .map(|h| h.to_value())
        .ok_or_else(|| ProviderError::Auth(steve_i18n::t!("channels.ext_ws.account_changed")))
}

/// `{source_id?, id?}` of a stored message (§4.4).
fn target(msg: &MessageRef) -> Result<Value, ProviderError> {
    let mut t = json!({});
    if let Some(s) = msg.source_id.as_deref().filter(|s| !s.is_empty()) {
        t["source_id"] = s.into();
    }
    if let Some(i) = msg.id() {
        t["id"] = i.into();
    }
    if t.as_object().is_some_and(|o| o.is_empty()) {
        return Err(ProviderError::Rejected(steve_i18n::t!(
            "channels.ext_ws.no_target"
        )));
    }
    Ok(t)
}

/// The handle systems the Hub knows in workspace `ws` (a `platform` alias of another system is
/// dropped, §6).
pub(crate) fn known_systems(cx: &ConnCx) -> HashSet<String> {
    cx.connections()
        .modules()
        .store
        .reader(|c| steve_modules::accounts::repo::systems(c, cx.ws()))
        .map(|v| v.into_iter().map(|s| s.id).collect())
        .unwrap_or_default()
}

/// One handle in the Hub (`None`: a platform handle of a system the Hub does not know).
fn hub_ref(h: &Handle, realm: &str, known: &HashSet<String>) -> Option<HandleRef> {
    let (system, handle) = h.hub(realm);
    if h.kind == HandleKind::Platform && !known.contains(&system) {
        return None;
    }
    Some(HandleRef::new(system, handle))
}

fn sender(s: &WireSender, realm: &str, known: &HashSet<String>) -> Sender {
    let primary = hub_ref(&s.id, realm, known).unwrap_or_else(|| {
        let (sys, h) = s.id.hub(realm);
        HandleRef::new(sys, h)
    });
    let mut out = Sender::new(primary).bot(s.is_bot).external(s.external);
    if let Some(d) = &s.display {
        out = out.display_name(d.clone());
    }
    for a in &s.aliases {
        if let Some(h) = hub_ref(a, realm, known) {
            out = out.alias(h);
        }
    }
    out
}

/// A wire message as the Hub takes it in (§6). `via`: `ws` or `history`.
pub(crate) fn inbound(
    m: &WireMessage,
    realm: &str,
    known: &HashSet<String>,
    via: &str,
) -> InboundMessage {
    let mut msg = InboundMessage::without_id(
        m.chat_id.clone(),
        sender(&m.sender, realm, known),
        m.text.clone(),
        m.created_ms,
    )
    .source_id(m.source_id.clone())
    .mine(m.is_mine);
    msg.external_id = m.id.clone().unwrap_or_default();
    msg.event_id = Some(m.source_id.clone());
    msg.space_kind = m.chat_kind;
    msg.space_name = m.chat_name.clone();
    msg.thread_key = m.thread_id.clone();
    msg.reply_to = m.reply_to.clone();
    msg.root_id = m.root_id.clone();
    msg.mentions = m
        .mentions
        .iter()
        .map(|x| Mention {
            handle: x.user.as_ref().and_then(|u| hub_ref(u, realm, known)),
            name: x.name.clone(),
            is_connection: x.is_me,
        })
        .collect();
    msg.attachments = m.attachments.clone();
    msg.via = Some(via.into());
    msg.raw = m.raw.clone();
    msg.edited = m.edited_ms;
    msg
}

/// One page of `messages.history` (§4.3): checked messages, the next page token, whether the
/// remote saw the whole range. A malformed message is dropped and logged.
pub(crate) async fn history_page(
    s: &Session,
    chat: &str,
    from_ms: i64,
    to_ms: i64,
    limit: usize,
    page: Option<&str>,
    now: i64,
) -> Result<(Vec<WireMessage>, Option<String>, bool), CallError> {
    let mut params = json!({"chat_id": chat, "from_ms": from_ms, "to_ms": to_ms,
        "limit": limit.clamp(1, s.limits().history_max_limit), "order": "asc"});
    if let Some(p) = page {
        params["page_token"] = p.into();
    }
    let v = ExtWs::read(s, "messages.history", params, s.timing().history).await?;
    let max_text = s.limits().max_text_bytes;
    let msgs = v
        .get("messages")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|m| match WireMessage::parse(m, now, max_text) {
                    Ok(m)
                        if m.chat_id == chat && m.created_ms >= from_ms && m.created_ms < to_ms =>
                    {
                        Some(m)
                    }
                    Ok(_) => None,
                    Err(e) => {
                        tracing::warn!("ext_ws: a history message dropped: {e}");
                        None
                    }
                })
                .collect()
        })
        .unwrap_or_default();
    let next = v
        .get("next_page_token")
        .and_then(Value::as_str)
        .filter(|t| wire::opaque_ok(t, 1024))
        .map(str::to_string);
    let complete = v.get("complete").and_then(Value::as_bool).unwrap_or(true);
    Ok((msgs, next, complete))
}

/// Reads `[from_ms, to_ms)` of `chat`, oldest first: at most `max` messages and `pages` pages,
/// each range at most 31 days. `bool`: everything in the range was read.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn read_range(
    s: &Session,
    chat: &str,
    from_ms: i64,
    to_ms: i64,
    max: usize,
    page_limit: usize,
    pages: usize,
    now: i64,
) -> Result<(Vec<WireMessage>, bool), CallError> {
    let mut out: Vec<WireMessage> = Vec::new();
    let mut complete = true;
    let mut start = from_ms;
    let mut used = 0;
    while start < to_ms {
        let end = (start + wire::MAX_HISTORY_RANGE_MS).min(to_ms);
        let mut token: Option<String> = None;
        loop {
            if used >= pages || out.len() >= max {
                return Ok((out, false));
            }
            used += 1;
            let want = page_limit.min(max - out.len());
            let (msgs, next, whole) =
                history_page(s, chat, start, end, want, token.as_deref(), now).await?;
            complete &= whole;
            out.extend(msgs);
            match next {
                Some(t) if Some(&t) != token.as_ref() => token = Some(t),
                _ => break,
            }
        }
        start = end;
    }
    Ok((out, complete))
}

#[async_trait]
impl ConnectionProvider for ExtWs {
    fn id(&self) -> &str {
        ADAPTER
    }

    fn system(&self) -> &str {
        SYSTEM
    }

    fn name(&self) -> &str {
        "外部渠道"
    }

    fn caps(&self) -> &[&'static str] {
        &["msg.receive", "msg.send_as_user"]
    }

    fn auth(&self) -> AuthKind {
        AuthKind::Credential
    }

    fn fields(&self) -> &[FieldDef] {
        &self.fields
    }

    fn integration(&self) -> Integration {
        Integration {
            provider: SYSTEM.into(),
            provider_name: "外部渠道".into(),
            identity: "self".into(),
            capabilities: vec![
                CapSpec::supported("channel", &["msg.receive"])
                    .with_note("通过渠道服务（WebSocket 协议）接收你的消息"),
                CapSpec::supported("send_as_you", &["msg.send_as_user"])
                    .with_note("以你本人的身份发送、回复、撤回和表情回应"),
            ],
        }
    }

    fn reauth_question(&self, account: &str) -> String {
        format!("{account} 的渠道服务需要重新连接")
    }

    /// The URL and pin, when the request carries them in `config` (the form's plain fields are
    /// checked again by `verify`, and before every dial).
    fn check_config(&self, _cx: &StartCx<'_>, config: &Value) -> Result<(), ProviderError> {
        if let Some(url) = config.get(URL).and_then(Value::as_str) {
            endpoint::check(url, config.get(PIN).and_then(Value::as_str))
                .map_err(|e| ProviderError::Rejected(e.message()))?;
        }
        Ok(())
    }

    /// The channel service vouches for who is signed in there; the connection is yours.
    fn claims_self(&self) -> bool {
        true
    }

    async fn verify(&self, cx: &ConnCx) -> Result<AccountInfo, ProviderError> {
        let (s, _notes, me) = self.open(cx).await.map_err(OpenFail::into_provider)?;
        s.close(1000, "verified");
        let known = known_systems(cx);
        let mut info = AccountInfo::new(account_handle(&me), me.display.clone()).config(json!({
            REALM: me.realm,
            ME: me.primary.to_value(),
            "server": s.hello().server,
        }));
        if me.primary.kind == HandleKind::Email {
            info = info.alias("mail", &me.primary.value.to_lowercase());
        }
        for a in &me.aliases {
            if let Some(h) = hub_ref(a, &me.realm, &known) {
                info = info.alias(&h.system, &h.handle);
            }
        }
        Ok(info)
    }

    /// The feed's session when it runs (a session that is not usable is a failure, never a
    /// re-authorization: the channel service brings it back); else a dial.
    async fn health(&self, cx: &ConnCx) -> Health {
        if let Some(s) = self.live(cx.id()) {
            return match s.status().status {
                SessionStatus::Ok => Health::Ok,
                SessionStatus::Expired => {
                    Health::Failed(steve_i18n::t!("channels.ext_ws.session_expired"))
                }
                SessionStatus::Locked => {
                    Health::Failed(steve_i18n::t!("channels.ext_ws.session_locked"))
                }
            };
        }
        match self.open(cx).await {
            Ok((s, _, me)) => {
                s.close(1000, "health");
                match same_identity(cx.config(), &me) {
                    Ok(()) => Health::Ok,
                    Err(e) => Health::from(&e),
                }
            }
            Err(e) => Health::from(&e.into_provider()),
        }
    }
}

#[async_trait]
impl ActAsUser for ExtWs {
    fn adapter_id(&self) -> &str {
        ADAPTER
    }

    async fn send(
        &self,
        cx: &ConnCx,
        to: &ActTarget,
        content: &OutContent,
        key: &SendKey,
    ) -> Result<Sent, AdapterError> {
        let realm = cx.config_str(REALM).unwrap_or_default().to_string();
        let mentions: Vec<Value> = content
            .at
            .iter()
            .filter_map(|h| Handle::from_hub(SYSTEM, h, &realm))
            .map(|h| h.to_value())
            .collect();
        let mut params = json!({"text": content.text});
        if !mentions.is_empty() {
            params["mentions"] = mentions.into();
        }
        let key = Some(wire::idempotency_key(&key.0));
        let v = match to {
            ActTarget::Space { ext_id } => {
                params["chat_id"] = ext_id.clone().into();
                self.write(cx, "messages.send", "messages.send", params, key)
                    .await?
            }
            ActTarget::Reply { to, in_thread } => {
                params["chat_id"] = to.space_ext_id.clone().into();
                params["to"] = target(to)?;
                params["in_thread"] = (*in_thread).into();
                self.write(cx, "messages.reply", "messages.reply", params, key)
                    .await?
            }
        };
        let source_id = v
            .get("source_id")
            .and_then(Value::as_str)
            .filter(|s| wire::opaque_ok(s, 256))
            .ok_or_else(|| {
                ProviderError::Transient(steve_i18n::t!(
                    "channels.ext_ws.unavailable",
                    detail = "a send result without a source_id"
                ))
            })?;
        let mut sent = match wire::opt_id(&v, "id").ok().flatten() {
            Some(id) => Sent::id(id),
            None => Sent::without_id(),
        }
        .source_id(source_id);
        if let Some(t) = v.get("created_ms").and_then(Value::as_i64) {
            sent = sent.created(t);
        }
        if let Some(t) = wire::opt_id(&v, "thread_id").ok().flatten() {
            sent = sent.thread_key(t);
        }
        Ok(sent)
    }

    fn supports_reactions(&self) -> bool {
        true
    }

    fn supports_recall(&self) -> bool {
        true
    }

    async fn add_reaction(
        &self,
        cx: &ConnCx,
        msg: &MessageRef,
        kind: ReactionKind,
    ) -> Result<String, AdapterError> {
        let params = json!({"chat_id": msg.space_ext_id, "target": target(msg)?,
            "emoji": self.reaction_emoji(kind)});
        let key = format!(
            "r:{}:{}",
            msg.source_id.as_deref().or(msg.id()).unwrap_or_default(),
            kind.code()
        );
        let v = self
            .write(
                cx,
                "reactions.add",
                "reactions",
                params,
                Some(wire::idempotency_key(&key)),
            )
            .await?;
        Ok(v.get("reaction_id")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string())
    }

    async fn remove_reaction(
        &self,
        cx: &ConnCx,
        msg: &MessageRef,
        reaction_id: &str,
    ) -> Result<(), AdapterError> {
        let params = json!({"chat_id": msg.space_ext_id, "target": target(msg)?,
            "reaction_id": reaction_id});
        self.write(cx, "reactions.remove", "reactions", params, None)
            .await?;
        Ok(())
    }

    async fn recall(&self, cx: &ConnCx, msg: &MessageRef) -> Result<(), AdapterError> {
        let params = json!({"chat_id": msg.space_ext_id, "target": target(msg)?});
        self.write(cx, "messages.recall", "messages.recall", params, None)
            .await?;
        Ok(())
    }
}

#[async_trait]
impl MediaFetch for ExtWs {
    fn adapter_id(&self) -> &str {
        ADAPTER
    }

    /// `GET <origin>/v1/media/<key>` with the bearer, same origin only, no redirects, at most
    /// `max` bytes (§8).
    async fn download(
        &self,
        cx: &ConnCx,
        msg: &MessageRef,
        key: &str,
        _image: bool,
        max: usize,
    ) -> Result<Vec<u8>, MediaError> {
        if key.is_empty() {
            return Err(MediaError::NoKey);
        }
        let (ep, token) = self.endpoint(cx).await.map_err(|e| match e {
            ProviderError::Auth(_) => MediaError::NoPermission,
            _ => MediaError::Disconnected,
        })?;
        ep.guard().map_err(|_| MediaError::NoPermission)?;
        let url = ep
            .media_url(key, &msg.space_ext_id, msg.source_id.as_deref(), msg.id())
            .map_err(|_| MediaError::NoKey)?;
        let mut b = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .timeout(self.timing.media);
        if let Ok(Some(tls)) = steve_node_wire::tls::client_config(&ep.trust) {
            b = b.use_preconfigured_tls((*tls).clone());
        }
        let client = b.build().map_err(|_| MediaError::Provider)?;
        let res = client
            .get(url)
            .bearer_auth(token.as_str())
            .header("X-Max-Bytes", max.to_string())
            .send()
            .await
            .map_err(|e| {
                if e.is_timeout() {
                    MediaError::Timeout
                } else {
                    MediaError::Provider
                }
            })?;
        drop(token);
        let status = res.status().as_u16();
        match status {
            200 => {}
            401 => {
                cx.report(Health::Auth(steve_i18n::t!("channels.ext_ws.unauthorized")))
                    .await;
                return Err(MediaError::NoPermission);
            }
            403 => return Err(MediaError::NoPermission),
            404 => return Err(MediaError::NotFound),
            410 => {
                let body: Value = res.json().await.unwrap_or(Value::Null);
                return Err(
                    if body.get("reason").and_then(Value::as_str) == Some("recalled") {
                        MediaError::Recalled
                    } else {
                        MediaError::Gone
                    },
                );
            }
            413 => return Err(MediaError::TooLarge),
            429 => return Err(MediaError::Busy),
            300..=399 => return Err(MediaError::Provider),
            500..=599 => return Err(MediaError::Timeout),
            _ => return Err(MediaError::Provider),
        }
        if res.content_length().is_some_and(|n| n as usize > max) {
            return Err(MediaError::TooLarge);
        }
        let mut res = res;
        let mut out = Vec::new();
        loop {
            match res.chunk().await {
                Ok(Some(c)) => {
                    if out.len() + c.len() > max {
                        return Err(MediaError::TooLarge);
                    }
                    out.extend_from_slice(&c);
                }
                Ok(None) => break,
                Err(e) if e.is_timeout() => return Err(MediaError::Timeout),
                Err(_) => return Err(MediaError::Provider),
            }
        }
        Ok(out)
    }
}

#[async_trait]
impl HistoryRead for ExtWs {
    fn adapter_id(&self) -> &str {
        ADAPTER
    }

    async fn read(
        &self,
        cx: &ConnCx,
        space_ext_id: &str,
        from_ms: i64,
        to_ms: i64,
        limit: usize,
    ) -> Result<Vec<InboundMessage>, AdapterError> {
        let u = self.session(cx).await?;
        let now = cx.connections().modules().clock.now();
        let realm = u.s.hello().realm.clone();
        // the Hub's range includes `to_ms`; the protocol's does not
        let (msgs, _) = read_range(
            &u.s,
            space_ext_id,
            from_ms,
            to_ms.saturating_add(1),
            limit,
            u.s.limits().history_max_limit,
            GAP_PAGES,
            now,
        )
        .await
        .map_err(call_err)?;
        let known = known_systems(cx);
        Ok(msgs
            .iter()
            .map(|m| inbound(m, &realm, &known, "history"))
            .collect())
    }
}

#[async_trait]
impl ProfileAdapter for ExtWs {
    fn adapter_id(&self) -> &str {
        ADAPTER
    }

    /// `people.get` for the handles of this service (§4.5); the space's member list is not read.
    async fn resolve_profiles(
        &self,
        cx: &ConnCx,
        q: &ProfileQuery,
    ) -> Result<ProfileAnswer, AdapterError> {
        let mut answer = ProfileAnswer::default();
        let realm = cx.config_str(REALM).unwrap_or_default().to_string();
        let users: Vec<Handle> = q
            .handles
            .iter()
            .filter_map(|h| Handle::from_hub(&h.system, &h.handle, &realm))
            .collect();
        if users.is_empty() {
            return Ok(answer);
        }
        let u = self.session(cx).await?;
        if !u.s.has("people.get") {
            return Ok(answer);
        }
        let known = known_systems(cx);
        for chunk in users.chunks(u.s.limits().max_ids_per_call.max(1)) {
            let users: Vec<Value> = chunk.iter().map(Handle::to_value).collect();
            let v = ExtWs::read(
                &u.s,
                "people.get",
                json!({"users": users}),
                u.s.timing().call,
            )
            .await
            .map_err(call_err)?;
            for p in v
                .get("people")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                let Some(h) = p.get("user").and_then(|h| Handle::parse(h).ok()) else {
                    continue;
                };
                let Some(handle) = hub_ref(&h, &realm, &known) else {
                    continue;
                };
                answer.profiles.push(Profile {
                    handle: Some(handle),
                    name: wire::text_field(p, "display", 256).unwrap_or_default(),
                    avatar_url: wire::text_field(p, "avatar_url", 2048),
                    email: wire::text_field(p, "email", 256),
                    directory: p.get("directory").and_then(Value::as_bool).unwrap_or(false),
                    title: wire::text_field(p, "title", 256),
                    ..Profile::default()
                });
            }
            for m in v
                .get("missing_permissions")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if let Some(scope) = wire::text_field(m, "scope", 256) {
                    answer.missing(
                        &scope,
                        &wire::text_field(m, "purpose", 256).unwrap_or_default(),
                    );
                }
            }
        }
        Ok(answer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_identity_must_stay_the_same() {
        let me = Me {
            realm: "acme".into(),
            primary: Handle::id("u_123"),
            display: "Ada".into(),
            aliases: Vec::new(),
        };
        let cfg = json!({REALM: "acme", ME: {"kind": "id", "value": "u_123"}});
        assert!(same_identity(&cfg, &me).is_ok());
        let other = Me {
            primary: Handle::id("u_999"),
            ..me.clone()
        };
        assert!(matches!(
            same_identity(&cfg, &other),
            Err(ProviderError::Auth(_))
        ));
        let realm = Me {
            realm: "other".into(),
            ..me.clone()
        };
        assert!(same_identity(&cfg, &realm).is_err());
        assert_eq!(account_handle(&me), "acme:id:u_123");
    }
}
