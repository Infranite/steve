//! Fake Feishu open platform (feishu.md §6).
//!
//! [`FakeFeishu::start`] binds an axum server on `127.0.0.1:<random>` that implements the
//! open-platform endpoints of feishu.md §2 against an in-memory [`World`]. Tests point a
//! connection's `config.api_base` at [`FakeFeishu::base_url`], inject faults (expired tokens,
//! rate limits, missing scopes), push signed + encrypted events with [`FakeFeishu::pusher`], and
//! assert on [`FakeFeishu::sent`].

mod api;
pub mod codes;
pub mod model;
mod push;
mod registration;
mod user;
mod ws;

use std::collections::{BTreeSet, HashMap, HashSet};
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

pub use model::{
    App, Chat, Department, Member, Mention, Message, Resource, User, UserGroup, UserIds, World, ids,
};
pub use push::{Delivery, EventPusher, FEISHU_RETRY_SCHEDULE, PushError, SignedRequest, encrypt};
pub use ws::{ACK_TIMEOUT, MAX_CONNECTIONS, WsAck, WsClientConfig, WsConnInfo, WsDelivery};

/// The fake's endpoints, used to aim injected faults at one of them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Endpoint {
    /// `POST /open-apis/im/v1/messages/{message_id}/reactions`.
    AddReaction,
    /// `DELETE /open-apis/im/v1/messages/{message_id}/reactions/{reaction_id}`.
    DeleteReaction,
    TenantToken,
    BotInfo,
    SendMessage,
    ReplyMessage,
    DeleteMessage,
    ListMessages,
    GetResource,
    GetChat,
    ChatMembers,
    ListChats,
    ContactScopes,
    DepartmentChildren,
    UsersByDepartment,
    /// `GET /open-apis/contact/v3/departments/{department_id}`.
    GetDepartment,
    /// `GET /open-apis/contact/v3/users/{user_id}`.
    GetUser,
    /// `GET /open-apis/contact/v3/users/batch?user_ids=…`.
    BatchUsers,
    /// `GET /open-apis/tenant/v2/tenant/query`.
    TenantQuery,
    /// `GET /open-apis/contact/v3/group/{group_id}/member/simplelist`.
    GroupMembers,
    OAuthAuthorize,
    OAuthToken,
    UserInfo,
    /// `POST /oauth/v1/device_authorization` (accounts host).
    DeviceAuthorization,
    /// `POST /oauth/v3/token` (accounts host, device-code grant).
    DeviceToken,
    /// `POST /open-apis/im/v1/messages/search`.
    SearchMessages,
    /// `GET /open-apis/im/v1/messages/mget`.
    MgetMessages,
    /// `POST /callback/ws/endpoint` (long connection).
    WsEndpoint,
    /// `GET /open-apis/event/v1/connection`.
    EventConnection,
    /// `GET /open-apis/application/v6/applications/{app_id}` (app info, `scene_type`).
    AppInfo,
}

/// One message the fake accepted through send or reply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SentMessage {
    pub message_id: String,
    /// `None` for plain sends, the replied-to message for replies.
    pub reply_to: Option<String>,
    /// Query `receive_id_type` (sends only).
    pub receive_id_type: Option<String>,
    /// Body `receive_id` (sends only).
    pub receive_id: Option<String>,
    pub chat_id: String,
    pub msg_type: String,
    /// The `content` JSON string exactly as sent.
    pub content: String,
    pub uuid: Option<String>,
    pub reply_in_thread: bool,
    /// Open id of the user when sent with a user access token (`send_as_user`), else the bot.
    pub as_user: Option<String>,
    /// True on an attempt that the uuid de-dup collapsed into an earlier message.
    pub deduplicated: bool,
}

impl SentMessage {
    /// `content` parsed; `{"text":…}` for text messages.
    pub fn content_json(&self) -> serde_json::Value {
        serde_json::from_str(&self.content).unwrap_or(serde_json::Value::Null)
    }

    /// The `text` of a text message, if any.
    pub fn text(&self) -> Option<String> {
        self.content_json()
            .get("text")
            .and_then(|t| t.as_str())
            .map(str::to_string)
    }
}

/// One reaction on a message, as the fake keeps it.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct FakeReaction {
    pub reaction_id: String,
    pub message_id: String,
    pub emoji_type: String,
    /// Open id of the user with a user token, else `None` (the bot).
    pub as_user: Option<String>,
    /// Taken off again (`DELETE …/reactions/{id}`).
    pub deleted: bool,
}

/// One HTTP request the fake received (any endpoint), for asserting call patterns.
#[derive(Debug, Clone)]
pub struct RecordedRequest {
    pub endpoint: Endpoint,
    pub method: String,
    pub path: String,
    pub query: String,
    /// The bearer token, if any.
    pub token: Option<String>,
    /// The response `code` the fake returned (0 on success).
    pub code: i64,
}

#[derive(Debug, Clone)]
pub(crate) enum TokenKind {
    Tenant,
    /// `scopes`: what the user granted (space-separated in the token response).
    User {
        open_id: String,
        scopes: Vec<String>,
    },
}

#[derive(Debug, Clone)]
pub(crate) struct TokenInfo {
    pub kind: TokenKind,
    pub expires_at: Instant,
    /// Set by [`FakeFeishu::expire_tokens`]: answered with the expired-token code.
    pub revoked: bool,
}

/// Mutable runtime state besides the world.
#[derive(Debug)]
pub(crate) struct Runtime {
    pub tokens: HashMap<String, TokenInfo>,
    pub refresh_tokens: HashMap<String, user::RefreshInfo>,
    pub oauth_codes: HashMap<String, user::OAuthCode>,
    /// device_code → pending device authorization.
    pub device: HashMap<String, user::DeviceAuth>,
    /// `interval` of device authorizations, seconds; polling faster gets `slow_down`.
    pub device_interval: u64,
    pub device_ttl: Duration,
    /// `refresh_token_expires_in`.
    pub refresh_ttl: Duration,
    /// Per-user send / reply limit (Feishu: 5 QPS per user).
    pub user_send_qps: usize,
    pub user_sends: HashMap<String, std::collections::VecDeque<Instant>>,
    pub token_ttl: Duration,
    pub seq: u64,
    pub next_id: u64,
    pub rate_limit_at: BTreeSet<u64>,
    pub rate_limit_next: HashMap<Endpoint, u32>,
    pub rate_limit_reset_secs: u64,
    pub denied: HashSet<Endpoint>,
    /// Denied to tenant (bot) tokens only.
    pub denied_tenant: HashSet<Endpoint>,
    pub sent: Vec<SentMessage>,
    pub attempts: Vec<SentMessage>,
    pub deleted: Vec<String>,
    /// Reactions added (and whether they were taken off), in order.
    pub reactions: Vec<FakeReaction>,
    /// uuid → (message id, first seen). Feishu de-dups the same uuid for an hour.
    pub uuids: HashMap<String, (String, Instant)>,
    pub requests: Vec<RecordedRequest>,
    pub hook_url: Option<String>,
    /// Plaintext envelopes pushed so far, by event id, for redelivery.
    pub pushed: HashMap<String, serde_json::Value>,
    pub ws: ws::WsRuntime,
    pub reg: registration::RegFake,
}

impl Runtime {
    fn new() -> Self {
        Self {
            tokens: HashMap::new(),
            refresh_tokens: HashMap::new(),
            oauth_codes: HashMap::new(),
            device: HashMap::new(),
            device_interval: 5,
            device_ttl: Duration::from_secs(240),
            refresh_ttl: Duration::from_secs(604_800),
            user_send_qps: 5,
            user_sends: HashMap::new(),
            token_ttl: Duration::from_secs(7200),
            seq: 0,
            next_id: 1,
            rate_limit_at: BTreeSet::new(),
            rate_limit_next: HashMap::new(),
            rate_limit_reset_secs: 1,
            denied: HashSet::new(),
            denied_tenant: HashSet::new(),
            sent: Vec::new(),
            attempts: Vec::new(),
            deleted: Vec::new(),
            reactions: Vec::new(),
            uuids: HashMap::new(),
            requests: Vec::new(),
            hook_url: None,
            pushed: HashMap::new(),
            ws: ws::WsRuntime::new(),
            reg: registration::RegFake::default(),
        }
    }

    /// Fresh id with Feishu's prefix, e.g. `om_…`, 32 hex chars after the prefix.
    pub fn id(&mut self, prefix: &str) -> String {
        let n = self.next_id;
        self.next_id += 1;
        let salt: u64 = rand::random();
        format!("{prefix}{:016x}{n:016x}", salt)
    }
}

pub(crate) struct Shared {
    pub world: Mutex<World>,
    pub rt: Mutex<Runtime>,
    pub http: reqwest::Client,
}

impl Shared {
    pub fn world(&self) -> MutexGuard<'_, World> {
        self.world.lock().unwrap_or_else(|e| e.into_inner())
    }
    pub fn rt(&self) -> MutexGuard<'_, Runtime> {
        self.rt.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// A running fake. Dropping it stops the server.
pub struct FakeFeishu {
    base_url: String,
    addr: SocketAddr,
    shared: Arc<Shared>,
    shutdown: Option<tokio::sync::oneshot::Sender<()>>,
}

impl FakeFeishu {
    /// Starts on a random localhost port with [`World::sample`].
    pub async fn start() -> std::io::Result<Self> {
        Self::start_with(World::sample()).await
    }

    /// Starts on a random localhost port with the given world.
    pub async fn start_with(world: World) -> std::io::Result<Self> {
        Self::bind(world, SocketAddr::from(([127, 0, 0, 1], 0))).await
    }

    /// Starts on a fixed address (the `steve-fake-feishu` binary).
    pub async fn bind(world: World, addr: SocketAddr) -> std::io::Result<Self> {
        // From here on this process must not talk to the real Feishu.
        crate::adapters::feishu::client::forbid_real_hosts();
        let shared = Arc::new(Shared {
            world: Mutex::new(world),
            rt: Mutex::new(Runtime::new()),
            http: reqwest::Client::builder()
                .timeout(Duration::from_secs(10))
                .build()
                .map_err(std::io::Error::other)?,
        });
        let listener = tokio::net::TcpListener::bind(addr).await?;
        let addr = listener.local_addr()?;
        let (tx, rx) = tokio::sync::oneshot::channel::<()>();
        let app = api::router(shared.clone());
        tokio::spawn(async move {
            let serve = axum::serve(listener, app).with_graceful_shutdown(async {
                let _ = rx.await;
            });
            if let Err(e) = serve.await {
                tracing::warn!(error = %e, "fake feishu server stopped");
            }
        });
        Ok(Self {
            base_url: format!("http://{addr}"),
            addr,
            shared,
            shutdown: Some(tx),
        })
    }

    /// `http://127.0.0.1:<port>`; use as the connection's `api_base` (paths start `/open-apis/`).
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    /// The app's credentials and bot identity.
    pub fn app(&self) -> App {
        self.shared.world().app.clone()
    }

    /// Runs `f` on the world (seed chats, users, history, resources; flip flags).
    pub fn world<R>(&self, f: impl FnOnce(&mut World) -> R) -> R {
        f(&mut self.shared.world())
    }

    /// Messages accepted by send / reply, de-duplicated by uuid, oldest first.
    pub fn sent(&self) -> Vec<SentMessage> {
        self.shared.rt().sent.clone()
    }

    /// Every send / reply attempt that passed auth and fault injection, including uuid repeats.
    pub fn send_attempts(&self) -> Vec<SentMessage> {
        self.shared.rt().attempts.clone()
    }

    /// Every reaction added (with `deleted` set once taken off), in order.
    pub fn reactions(&self) -> Vec<FakeReaction> {
        self.shared.rt().reactions.clone()
    }

    /// The emoji types on a message now.
    pub fn reactions_on(&self, message_id: &str) -> Vec<String> {
        self.shared
            .rt()
            .reactions
            .iter()
            .filter(|r| r.message_id == message_id && !r.deleted)
            .map(|r| r.emoji_type.clone())
            .collect()
    }

    /// Message ids recalled through `DELETE /im/v1/messages/{id}`.
    pub fn deleted(&self) -> Vec<String> {
        self.shared.rt().deleted.clone()
    }

    /// All requests received, oldest first.
    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.shared.rt().requests.clone()
    }

    /// Requests to one endpoint.
    pub fn requests_to(&self, ep: Endpoint) -> Vec<RecordedRequest> {
        self.requests()
            .into_iter()
            .filter(|r| r.endpoint == ep)
            .collect()
    }

    /// Forgets sent messages, attempts and the request log (not the uuid de-dup window).
    pub fn clear_log(&self) {
        let mut rt = self.shared.rt();
        rt.sent.clear();
        rt.attempts.clear();
        rt.requests.clear();
        rt.deleted.clear();
    }

    /// Lifetime of newly issued tenant / user tokens (`expire` in the token response).
    pub fn set_token_ttl(&self, ttl: Duration) {
        self.shared.rt().token_ttl = ttl;
    }

    /// Every token issued so far now answers with the expired-token error until a new one is
    /// fetched — simulates a token invalidated before the cached expiry.
    pub fn expire_tokens(&self) {
        for t in self.shared.rt().tokens.values_mut() {
            t.revoked = true;
        }
    }

    /// Number of tenant tokens issued so far.
    pub fn tokens_issued(&self) -> usize {
        self.requests_to(Endpoint::TenantToken)
            .iter()
            .filter(|r| r.code == 0)
            .count()
    }

    /// The `n`-th request (1-based, counting every endpoint) gets a rate-limit error.
    pub fn rate_limit_at(&self, n: u64) {
        self.shared.rt().rate_limit_at.insert(n);
    }

    /// The next `times` requests to `ep` get a rate-limit error.
    pub fn rate_limit_next(&self, ep: Endpoint, times: u32) {
        *self.shared.rt().rate_limit_next.entry(ep).or_default() += times;
    }

    /// Value of `x-ogw-ratelimit-reset` on rate-limit responses (seconds until the window resets).
    pub fn set_rate_limit_reset(&self, secs: u64) {
        self.shared.rt().rate_limit_reset_secs = secs;
    }

    /// `ep` answers with the missing-scope error, as when a permission isn't granted.
    pub fn deny(&self, ep: Endpoint) {
        self.shared.rt().denied.insert(ep);
    }

    /// Undoes [`Self::deny`] (and [`Self::deny_bot`]).
    pub fn allow(&self, ep: Endpoint) {
        let mut rt = self.shared.rt();
        rt.denied.remove(&ep);
        rt.denied_tenant.remove(&ep);
    }

    /// `ep` answers tenant (bot) tokens with the missing-scope error; user tokens pass.
    pub fn deny_bot(&self, ep: Endpoint) {
        self.shared.rt().denied_tenant.insert(ep);
    }

    /// The user granted more scopes: every user token of `open_id` issued so far has them too (as
    /// after a re-authorization, without the round trip).
    pub fn grant_user_scopes(&self, open_id: &str, scopes: &[&str]) {
        for t in self.shared.rt().tokens.values_mut() {
            if let TokenKind::User {
                open_id: o,
                scopes: s,
            } = &mut t.kind
                && o == open_id
            {
                for x in scopes {
                    if !s.iter().any(|y| y == x) {
                        s.push(x.to_string());
                    }
                }
            }
        }
    }

    /// Event pusher aimed at `hook_url` (the Hub's `/hooks/<connection_id>`), using the app's
    /// Verification Token and Encrypt Key. Pushed `im.message.receive_v1` messages are also added
    /// to the fake's chat history, as Feishu would have them.
    pub fn pusher(&self, hook_url: impl Into<String>) -> EventPusher {
        EventPusher::new(self.shared.clone(), hook_url.into())
    }

    /// Hook URL used by the `/_fake/*` control routes of the binary.
    pub fn set_hook_url(&self, url: impl Into<String>) {
        self.shared.rt().hook_url = Some(url.into());
    }
}

impl Drop for FakeFeishu {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
    }
}

/// Directory of the recorded sample payloads (`tests/fixtures/feishu`).
pub fn fixtures_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/feishu")
}

/// Loads a fixture by file stem, e.g. `fixture("message_p2p_text")`.
pub fn fixture(name: &str) -> std::io::Result<serde_json::Value> {
    let path = fixtures_dir().join(format!("{name}.json"));
    let raw = std::fs::read(&path)?;
    serde_json::from_slice(&raw).map_err(std::io::Error::other)
}

/// Milliseconds since the Unix epoch.
pub(crate) fn now_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0)
}
