//! The connection rules: connecting (OAuth code + PKCE, device
//! flow, credential form, module-managed), the credential in the system vault, token refresh with
//! single-use refresh tokens committed before use, health with backoff and the `reauth` decision,
//! pause, capabilities, disconnect.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::http::{HeaderMap, header};
use serde::Serialize;
use serde_json::{Map, Value, json};
use steve_store::base::{self, DecisionKind, Viewer};
use steve_store::{ChangeEvent, NewDecision, Tx, uid};
use zeroize::Zeroizing;

use super::cx::{
    ACCESS_TOKEN, AccessToken, ConnCx, EXPIRES_AT, REFRESH_EXPIRES_AT, REFRESH_TOKEN, SCOPE,
    StartCx, vault_err,
};
use super::oauth::{self, DevicePoll, TokenSet, pkce_challenge, random_token};
use super::pending::{Device, DeviceStatus, Devices, Flow, PendingStates};
use super::provider::{
    AccountInfo, AuthKind, CONFIG_VERSION_KEY, ConnectionProvider, FieldDef, Health, Owner,
    PAIR_KEY, Paired, ProviderError, StewardHint, TokenKind,
};
use super::repo::{self, ConnRow, HealthState};
use crate::accounts::claim::claim_self;
use crate::accounts::identify::{Attach, attach_handle};
use crate::accounts::repo as arepo;
use crate::accounts::service::{
    self as asvc, Record, current_party, default_label, may_connect, normalize, own_account,
    own_system,
};
use crate::contacts::PartyHooks;
use crate::contacts::repo as people;
use crate::contacts::stewards;
use crate::vault::{Vault, VaultError, VaultResult};
use crate::{ApiError, Caller, Check, Modules};

/// The OAuth callback path, on every public origin (like steve-auth's `/auth/callback`).
pub const CALLBACK_PATH: &str = "/auth/connect/callback";
/// `subject_type` of the `reauth` decision.
pub const SUBJECT: &str = "connection";
/// The decision's options.
pub const REAUTH_OPTIONS: [&str; 2] = ["reauth", "pause"];

const MINUTE: i64 = 60_000;
/// On-demand refresh / re-mint this long before expiry.
const USE_MARGIN_MS: i64 = 5 * MINUTE;
/// The refresh job refreshes this long before expiry.
const JOB_MARGIN_MS: i64 = 10 * MINUTE;
/// Health backoff: 15 min × 2^(fails-1), at most 6 h.
const BACKOFF_BASE_MS: i64 = 15 * MINUTE;
const BACKOFF_MAX_MS: i64 = 6 * 60 * MINUTE;
/// `error` after this many failures in a row.
const FAILS_TO_ERROR: i64 = 3;
/// What one scheduled health check may take.
const HEALTH_TIMEOUT: Duration = Duration::from_secs(60);

fn backoff(fails: i64) -> i64 {
    let shift = (fails.max(1) - 1).min(10) as u32;
    (BACKOFF_BASE_MS << shift).min(BACKOFF_MAX_MS)
}

// ---------------------------------------------------------------- config, usage

/// Environment of the framework: the public origins and the OAuth client variables.
#[derive(Clone)]
pub struct ConnectionsConfig {
    /// `STEVE_PUBLIC_URLS` (else `STEVE_PUBLIC_URL`, else `http://127.0.0.1:8440`), no trailing `/`.
    pub public_urls: Vec<String>,
    /// Whether a request's `config.api_base` is kept (tests only; dropped on the real `~/.steve`).
    pub allow_api_base: bool,
    env: Arc<EnvFn>,
}

type EnvFn = dyn Fn(&str) -> Option<String> + Send + Sync;
/// Credential fields by key.
type Secrets = Vec<(String, Zeroizing<String>)>;

impl ConnectionsConfig {
    pub fn new(
        public_urls: Vec<String>,
        allow_api_base: bool,
        env: impl Fn(&str) -> Option<String> + Send + Sync + 'static,
    ) -> Self {
        Self {
            public_urls: public_urls
                .into_iter()
                .map(|u| u.trim().trim_end_matches('/').to_string())
                .filter(|u| !u.is_empty())
                .collect(),
            allow_api_base,
            env: Arc::new(env),
        }
    }

    /// From the process environment the way steve-auth reads its origins. `api_base` overrides only
    /// when `STEVE_HOME` is set and is not `~/.steve`.
    pub fn from_env(var: impl Fn(&str) -> Option<String> + Send + Sync + 'static) -> Self {
        let primary = var("STEVE_PUBLIC_URL").unwrap_or_else(|| "http://127.0.0.1:8440".into());
        let urls = var("STEVE_PUBLIC_URLS").unwrap_or_else(|| primary.clone());
        let home = var("STEVE_HOME");
        let real = var("HOME").map(|h| format!("{}/.steve", h.trim_end_matches('/')));
        let allow = matches!((&home, &real), (Some(h), Some(r)) if h.trim_end_matches('/') != r);
        Self::new(urls.split(',').map(String::from).collect(), allow, var)
    }
}

/// `{id, name}` of something that uses a connection.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct UsageRef {
    pub id: String,
    pub name: String,
}

/// `GET /connections/:id/usage`.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Usage {
    pub flows: Vec<UsageRef>,
    pub spaces: i64,
    pub sources: Vec<UsageRef>,
}

/// What other modules (flows, channels, mail, GitHub) tell about their use of a connection.
pub trait ConnectionUsage: Send + Sync {
    fn usage(&self, c: &rusqlite::Connection, ws: &str, connection_id: &str) -> VaultResult<Usage>;
    /// Called in the disconnect transaction before the row is deleted: drop or detach rows that
    /// reference it (foreign keys are on).
    fn on_disconnect(&self, tx: &Tx, ws: &str, connection_id: &str) -> VaultResult<()> {
        let _ = (tx, ws, connection_id);
        Ok(())
    }
}

// ---------------------------------------------------------------- shapes

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct HealthOut {
    pub last_ok: i64,
    pub last_error: String,
    pub error_at: i64,
}

/// `ConnectionOut`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ConnectionOut {
    pub id: String,
    pub account_id: String,
    pub adapter: String,
    pub auth: String,
    pub status: String,
    pub caps: Vec<String>,
    pub available_caps: Vec<String>,
    pub health: HealthOut,
    pub last_sync: Option<i64>,
    /// The Hub has an adapter for it. A connection whose adapter is not installed (an extension
    /// left out of the assembly) is listed and can be disconnected; it is not tested, refreshed or
    /// re-authorized until the adapter is back, and its spaces, messages and contacts stay.
    pub installed: bool,
}

/// `GET /connections/adapters`.
#[derive(Debug, Clone, Serialize)]
pub struct AdapterOut {
    pub id: String,
    pub system: String,
    pub name: String,
    pub auth: AuthKind,
    pub caps: Vec<String>,
    /// The device flow is offered.
    pub device: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub fields: Option<Vec<FieldDef>>,
}

/// What connecting produced.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Connected {
    pub connection_id: String,
    pub account_id: String,
    /// An existing connection was re-authorized (or reconnected) rather than created.
    pub reconnected: bool,
}

/// `POST /connections/device/start`.
#[derive(Debug, Clone, Serialize)]
pub struct DeviceOut {
    pub id: String,
    pub user_code: String,
    pub verification_uri: String,
    pub verification_uri_complete: Option<String>,
    pub expires_in: i64,
    pub interval: i64,
}

// ---------------------------------------------------------------- the service

/// Builds [`Connections`] with its providers and usage reporters.
pub struct ConnectionsBuilder {
    m: Modules,
    vault: Vault,
    cfg: ConnectionsConfig,
    providers: BTreeMap<String, Arc<dyn ConnectionProvider>>,
    usage: Vec<Arc<dyn ConnectionUsage>>,
    party_hooks: Vec<Arc<dyn PartyHooks>>,
    channels: BTreeMap<String, Value>,
}

impl ConnectionsBuilder {
    /// Registers an adapter; its id must be unique (panics at startup otherwise).
    pub fn provider(mut self, p: Arc<dyn ConnectionProvider>) -> Self {
        let id = p.id().to_string();
        assert!(
            self.providers.insert(id.clone(), p).is_none(),
            "connection provider {id} registered twice"
        );
        self
    }

    pub fn usage(mut self, u: Arc<dyn ConnectionUsage>) -> Self {
        self.usage.push(u);
        self
    }

    /// The contacts hooks a claim into “我” runs ([`ConnectionProvider::claims_self`]: accounts and
    /// channel members move along with the merged contact). Default: the accounts module's alone.
    pub fn party_hooks(mut self, h: Vec<Arc<dyn PartyHooks>>) -> Self {
        self.party_hooks = h;
        self
    }

    /// What the channel side of `adapter` declares, as the 接入 page reads it (mode, features,
    /// how messages come in, how a connection is made): `channel` of its entry in
    /// `GET /connections/integrations`.
    pub fn channel(mut self, adapter: &str, summary: Value) -> Self {
        self.channels.insert(adapter.to_string(), summary);
        self
    }

    pub fn build(self) -> Connections {
        // a connect timeout only: what a whole request may take is each caller's (Feishu sets its
        // own; GitHub `api::REQUEST_TIMEOUT`), so a long download is not cut by a default
        let http = reqwest::Client::builder()
            .user_agent("steve-hub")
            .redirect(reqwest::redirect::Policy::none())
            .connect_timeout(CONNECT_TIMEOUT)
            .build()
            .expect("the HTTP client (TLS backend) could not be built");
        Connections(Arc::new(Inner {
            m: self.m,
            vault: self.vault,
            cfg: self.cfg,
            providers: self.providers,
            usage: self.usage,
            party_hooks: self.party_hooks,
            channels: self.channels,
            flows: PendingStates::default(),
            devices: Devices::default(),
            locks: Mutex::new(HashMap::new()),
            minted: Mutex::new(HashMap::new()),
            forced: Mutex::new(HashSet::new()),
            http,
        }))
    }
}

struct Inner {
    m: Modules,
    vault: Vault,
    cfg: ConnectionsConfig,
    providers: BTreeMap<String, Arc<dyn ConnectionProvider>>,
    usage: Vec<Arc<dyn ConnectionUsage>>,
    party_hooks: Vec<Arc<dyn PartyHooks>>,
    /// The declared channel sides' summaries, by adapter ([`ConnectionsBuilder::channel`]).
    channels: BTreeMap<String, Value>,
    flows: PendingStates<Flow>,
    devices: Devices,
    /// One refresh/mint at a time per connection.
    locks: Mutex<HashMap<String, Arc<tokio::sync::Mutex<()>>>>,
    /// Minted tokens by connection: (expires_at, token).
    minted: Mutex<HashMap<String, (i64, Zeroizing<String>)>>,
    /// Connections whose stored token got a 401: refresh on next use.
    forced: Mutex<HashSet<String>>,
    http: reqwest::Client,
}

/// The connection framework. Cheap to clone.
#[derive(Clone)]
pub struct Connections(Arc<Inner>);

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// A provider failure as an API error (400 with its message; vault trouble keeps its own code).
/// Why a connection may not write to the outside now (see [`Connections::cx_for`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WriteRefusal {
    /// Paused or waiting for a new authorization: the write waits and goes out once it is resumed.
    Later(String),
    /// Revoked, or the capability is turned off: the write can't go out until you change that.
    Never(String),
}

impl WriteRefusal {
    pub fn message(&self) -> &str {
        match self {
            Self::Later(m) | Self::Never(m) => m,
        }
    }
}

impl From<WriteRefusal> for ApiError {
    fn from(r: WriteRefusal) -> Self {
        ApiError::rule(r.message())
    }
}

/// How long a connection to a provider may take to open (the shared client).
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// The statuses in which a write waits rather than fails: it goes out once the connection is resumed
/// or authorized again. Queues skip these connections so they don't hold up others.
pub const WAITING: [&str; 2] = ["paused", "reauth"];

/// May `r` write to the outside now, needing `cap` (if given)? A connection in `error` may: its
/// health only says recent calls failed, and the write itself reports how it went.
pub fn may_write(r: &ConnRow, cap: Option<&str>) -> Result<(), WriteRefusal> {
    match r.status.as_str() {
        "ok" | "error" => {}
        "paused" => return Err(WriteRefusal::Later("这个连接已暂停，恢复后再发出".into())),
        "reauth" => {
            return Err(WriteRefusal::Later(
                "这个连接需要重新授权，授权后再发出".into(),
            ));
        }
        "revoked" => return Err(WriteRefusal::Never("这个连接已经断开".into())),
        s => return Err(WriteRefusal::Never(format!("这个连接现在不能发出（{s}）"))),
    }
    match cap {
        Some(c) if !r.caps.iter().any(|x| x == c) => Err(WriteRefusal::Never(format!(
            "这个连接没有开启「{c}」，在“接入”里打开后才能发出"
        ))),
        _ => Ok(()),
    }
}

/// [`may_write`] for connection `id` of `ws`, read in the caller's transaction.
pub fn may_write_in(
    c: &rusqlite::Connection,
    ws: &str,
    id: &str,
    cap: Option<&str>,
) -> Result<Result<(), WriteRefusal>, ApiError> {
    let r = repo::get(c, id)?
        .filter(|r| r.workspace_id == ws)
        .ok_or_else(|| ApiError::not_found("连接"))?;
    Ok(may_write(&r, cap))
}

fn perr(e: ProviderError) -> VaultError {
    ApiError::rule(e.to_string()).into()
}

fn not_configured(spec_name: &str) -> VaultError {
    ApiError::rule(steve_i18n::t!(
        "modules.connections.oauth_not_configured",
        provider = spec_name
    ))
    .into()
}

/// Sets (`Some`) or removes (`None`) one key of an object `config`.
fn put_key(config: &mut Value, key: &str, v: Option<Value>) {
    if let Some(o) = config.as_object_mut() {
        match v {
            Some(v) => {
                o.insert(key.into(), v);
            }
            None => {
                o.remove(key);
            }
        }
    }
}

/// The [`ConnectionProvider::config_version`] a connection's `config` was stored under (absent: 1).
fn stored_version(config: &Value) -> u64 {
    config
        .get(CONFIG_VERSION_KEY)
        .and_then(Value::as_u64)
        .unwrap_or(1)
}

/// What [`CONFIG_VERSION_KEY`] holds for `version` (nothing for 1, so a provider that never bumps
/// stores what it always did).
fn version_value(version: u32) -> Option<Value> {
    (version != 1).then(|| json!(version))
}

fn merge(into: &mut Value, from: &Value) {
    if !into.is_object() {
        *into = Value::Object(Map::new());
    }
    if let (Some(a), Some(b)) = (into.as_object_mut(), from.as_object()) {
        for (k, v) in b {
            a.insert(k.clone(), v.clone());
        }
    }
}

/// `config.extra_scopes` of an OAuth flow: scopes asked for on top of the provider's own list
/// (re-authorizing with the scopes a call was refused for). Only scope-shaped strings, at most 30.
pub const EXTRA_SCOPES: &str = "extra_scopes";

fn with_extra_scopes(mut spec: super::oauth::OAuthSpec, config: &Value) -> super::oauth::OAuthSpec {
    let shaped = |s: &str| {
        !s.is_empty()
            && s.len() <= 100
            && s.chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, ':' | '.' | '_' | '-'))
    };
    let extra = config
        .get(EXTRA_SCOPES)
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .map(str::trim)
        .filter(|s| shaped(s))
        .take(30);
    for s in extra {
        if !spec.scopes.iter().any(|x| x == s) {
            spec.scopes.push(s.to_string());
        }
    }
    spec
}

fn scopes_of(set: &TokenSet, requested: &[String]) -> Vec<String> {
    match &set.scope {
        Some(s) => s
            .split([' ', ','])
            .filter(|x| !x.is_empty())
            .map(String::from)
            .collect(),
        None => requested.to_vec(),
    }
}

/// The credential fields of a token set. `old_refresh`: kept when the provider didn't rotate it.
fn token_fields(
    set: &TokenSet,
    old_refresh: Option<&Zeroizing<String>>,
    old_refresh_exp: Option<i64>,
    now: i64,
) -> (Secrets, Option<i64>, Option<i64>) {
    let expires_at = set.expires_in.map(|s| now + s * 1000);
    let rotated = set.refresh_token.is_some();
    let refresh_exp = match set.refresh_expires_in {
        Some(s) => Some(now + s * 1000),
        None if rotated => None,
        None => old_refresh_exp,
    };
    let mut f = vec![(ACCESS_TOKEN.to_string(), set.access_token.clone())];
    if let Some(r) = set.refresh_token.as_ref().or(old_refresh) {
        f.push((REFRESH_TOKEN.into(), r.clone()));
    }
    if let Some(e) = expires_at {
        f.push((EXPIRES_AT.into(), Zeroizing::new(e.to_string())));
    }
    if let Some(e) = refresh_exp {
        f.push((REFRESH_EXPIRES_AT.into(), Zeroizing::new(e.to_string())));
    }
    if let Some(s) = &set.scope {
        f.push((SCOPE.into(), Zeroizing::new(s.clone())));
    }
    (f, expires_at, refresh_exp)
}

fn url_host(u: &str) -> Option<String> {
    url::Url::parse(u).ok().and_then(|u| {
        let h = u.host_str()?.to_lowercase();
        Some(match u.port() {
            Some(p) => format!("{h}:{p}"),
            None => h,
        })
    })
}

/// `/#/accounts?error=<中文>`.
pub fn error_location(msg: &str) -> String {
    let q: String = url::form_urlencoded::byte_serialize(msg.as_bytes()).collect();
    format!("/#/accounts?error={q}")
}

impl Connections {
    pub fn builder(m: Modules, vault: Vault, cfg: ConnectionsConfig) -> ConnectionsBuilder {
        ConnectionsBuilder {
            m,
            vault,
            cfg,
            providers: BTreeMap::new(),
            usage: Vec::new(),
            party_hooks: crate::accounts::ports().hooks,
            channels: BTreeMap::new(),
        }
    }

    pub fn modules(&self) -> &Modules {
        &self.0.m
    }
    pub fn vault(&self) -> &Vault {
        &self.0.vault
    }
    pub fn http(&self) -> &reqwest::Client {
        &self.0.http
    }
    pub(crate) fn env(&self, k: &str) -> Option<String> {
        (self.0.cfg.env)(k)
    }
    fn now(&self) -> i64 {
        self.0.m.clock.now()
    }
    pub fn provider(&self, id: &str) -> Option<Arc<dyn ConnectionProvider>> {
        self.0.providers.get(id).cloned()
    }
    /// Every registered provider id, managed ones included.
    pub fn provider_ids(&self) -> Vec<String> {
        self.0.providers.keys().cloned().collect()
    }
    fn provider_or_404(&self, id: &str) -> VaultResult<Arc<dyn ConnectionProvider>> {
        self.provider(id)
            .ok_or_else(|| ApiError::not_found("接入方式").into())
    }

    /// Runs store work off the runtime.
    async fn blocking<R: Send + 'static>(
        &self,
        f: impl FnOnce(&Connections) -> VaultResult<R> + Send + 'static,
    ) -> VaultResult<R> {
        let c = self.clone();
        tokio::task::spawn_blocking(steve_i18n::carry(move || f(&c)))
            .await
            .map_err(|e| VaultError::Api(ApiError::Internal(format!("task failed: {e}"))))?
    }

    /// The public origin the browser used (`Host` / `X-Forwarded-Host` matched against
    /// `STEVE_PUBLIC_URLS`), else the first one: the OAuth redirect goes back where the user is.
    pub fn origin(&self, headers: &HeaderMap) -> String {
        let host = headers
            .get("x-forwarded-host")
            .or_else(|| headers.get(header::HOST))
            .and_then(|v| v.to_str().ok())
            .unwrap_or("")
            .split(',')
            .next()
            .unwrap_or("")
            .trim()
            .to_lowercase();
        let urls = &self.0.cfg.public_urls;
        urls.iter()
            .find(|u| url_host(u).is_some_and(|h| h == host))
            .or(urls.first())
            .cloned()
            .unwrap_or_else(|| "http://127.0.0.1:8440".into())
    }

    // ------------------------------------------------------------ reads

    pub fn adapters(&self) -> Vec<AdapterOut> {
        self.0
            .providers
            .values()
            .filter(|p| !matches!(p.auth(), AuthKind::Managed))
            .map(|p| AdapterOut {
                id: p.id().into(),
                system: p.system().into(),
                name: p.name().into(),
                auth: p.auth(),
                caps: p.caps().iter().map(|c| c.to_string()).collect(),
                device: p.oauth().is_some_and(|s| s.device_url.is_some()),
                fields: matches!(p.auth(), AuthKind::Credential).then(|| p.fields().to_vec()),
            })
            .collect()
    }

    fn out(&self, r: &ConnRow) -> ConnectionOut {
        let available = self.available_of(r);
        ConnectionOut {
            id: r.id.clone(),
            account_id: r.account_id.clone(),
            adapter: r.adapter.clone(),
            auth: r.auth.clone(),
            status: r.status.clone(),
            caps: r.caps.clone(),
            available_caps: available,
            health: HealthOut {
                last_ok: r.health.last_ok,
                last_error: r.health.last_error.clone(),
                error_at: r.health.error_at,
            },
            last_sync: r.last_sync,
            installed: self.provider(&r.adapter).is_some(),
        }
    }

    /// The declared summary of the channel side serving `adapter`, when there is one.
    pub fn channel_summary(&self, adapter: &str) -> Option<&Value> {
        self.0.channels.get(adapter)
    }

    fn viewer(&self, who: &Caller) -> VaultResult<Viewer> {
        Ok(asvc::start(&self.0.m, who)?.0)
    }

    /// The connection `id` if its account is visible to the viewer; `404 没有这个连接` otherwise.
    fn own(&self, c: &rusqlite::Connection, viewer: &Viewer, id: &str) -> VaultResult<ConnRow> {
        let r = repo::get(c, id)?
            .filter(|r| r.workspace_id == viewer.ws)
            .ok_or_else(|| ApiError::not_found("连接"))?;
        let a = own_account(c, viewer, &r.account_id).map_err(|_| ApiError::not_found("连接"))?;
        if !Self::mine(c, viewer, &a.party_id)? {
            return Err(ApiError::not_found("连接").into());
        }
        Ok(r)
    }

    /// A connection of someone's “我” (their mailbox, their user token) is theirs alone, even though
    /// the account is visible to the workspace; the own organization's and teams' are shared.
    fn mine(c: &rusqlite::Connection, viewer: &Viewer, party: &str) -> VaultResult<bool> {
        Ok(current_party(c, party)?
            .and_then(|p| p.user_id)
            .is_none_or(|u| u == viewer.user))
    }

    pub fn list(&self, who: &Caller) -> VaultResult<Vec<ConnectionOut>> {
        let rows = self.visible(who)?;
        Ok(rows.iter().map(|(r, _)| self.out(r)).collect())
    }

    /// The connections the caller may see, with their accounts (the list and 接入).
    pub(super) fn visible(&self, who: &Caller) -> VaultResult<Vec<(ConnRow, arepo::Account)>> {
        let viewer = self.viewer(who)?;
        self.0.m.store.reader(|c| {
            let mut out = Vec::new();
            for r in repo::list(c, &who.ws)? {
                if let Ok(a) = own_account(c, &viewer, &r.account_id)
                    && Self::mine(c, &viewer, &a.party_id)?
                {
                    out.push((r, a));
                }
            }
            Ok::<_, VaultError>(out)
        })
    }

    /// Every registered adapter, managed ones included, by id.
    pub(super) fn providers(&self) -> impl Iterator<Item = &Arc<dyn ConnectionProvider>> {
        self.0.providers.values()
    }

    /// The OAuth redirect URI on every public origin, to be registered at each provider.
    pub fn callback_urls(&self) -> Vec<String> {
        self.0
            .cfg
            .public_urls
            .iter()
            .map(|u| format!("{u}{CALLBACK_PATH}"))
            .collect()
    }

    /// `available_caps` of a row (its provider's view of the granted scopes).
    pub(super) fn available_of(&self, r: &ConnRow) -> Vec<String> {
        match self.provider(&r.adapter) {
            Some(p) => p.available_caps(&r.granted_scopes),
            None => r.caps.clone(),
        }
    }

    pub fn get(&self, who: &Caller, id: &str) -> VaultResult<ConnectionOut> {
        let viewer = self.viewer(who)?;
        let r = self.0.m.store.reader(|c| self.own(c, &viewer, id))?;
        Ok(self.out(&r))
    }

    /// The context an adapter works with, for other modules (sync jobs, senders).
    pub fn cx(&self, ws: &str, id: &str) -> VaultResult<ConnCx> {
        let r = self
            .0
            .m
            .store
            .reader(|c| repo::get(c, id))?
            .filter(|r| r.workspace_id == ws)
            .ok_or_else(|| ApiError::not_found("连接"))?;
        Ok(self.cx_of(&r))
    }

    /// The context for a write to the outside (sending, posting, merging), checked when the write
    /// happens, not when it was approved: refused while the connection is paused, revoked or needs a
    /// new authorization, and when `cap` (if given) is turned off for it. A connection in `error` may
    /// still write: its health only says recent calls failed.
    pub fn cx_for(&self, ws: &str, id: &str, cap: Option<&str>) -> VaultResult<ConnCx> {
        let r = self
            .0
            .m
            .store
            .reader(|c| repo::get(c, id))?
            .filter(|r| r.workspace_id == ws)
            .ok_or_else(|| ApiError::not_found("连接"))?;
        may_write(&r, cap).map_err(ApiError::from)?;
        Ok(self.cx_of(&r))
    }

    fn cx_of(&self, r: &ConnRow) -> ConnCx {
        let paired = self
            .paired(&r.workspace_id, &r.adapter, &r.config)
            .unwrap_or_else(|e| {
                tracing::warn!(connection = %r.id, "connection pair not read: {e}");
                None
            });
        ConnCx {
            conns: self.clone(),
            ws: r.workspace_id.clone(),
            id: r.id.clone(),
            adapter: r.adapter.clone(),
            item: r.credential_item.clone(),
            config: r.config.clone(),
            scopes: r.granted_scopes.clone(),
            paired,
            pending: false,
        }
    }

    // ------------------------------------------------------------ pairing

    /// The connection a connection of `adapter` with `config` is paired with, as it is now: its
    /// provider pairs ([`ConnectionProvider::pairing`]), `config.app_connection` ([`PAIR_KEY`]) names
    /// a connection of `ws` of one of the adapters it pairs with. `ok`: that one is usable (`ok` or
    /// `error`).
    pub fn paired(&self, ws: &str, adapter: &str, config: &Value) -> VaultResult<Option<Paired>> {
        let Some(pairing) = self.provider(adapter).and_then(|p| p.pairing()) else {
            return Ok(None);
        };
        let Some(with) = config.get(PAIR_KEY).and_then(Value::as_str) else {
            return Ok(None);
        };
        let row = self.0.m.store.reader(|c| repo::get(c, with))?;
        Ok(row
            .filter(|r| r.workspace_id == ws && pairing.with.contains(&r.adapter))
            .map(|r| Paired {
                ok: matches!(r.status.as_str(), "ok" | "error"),
                id: r.id,
                adapter: r.adapter,
            }))
    }

    /// Checks (and completes) the pair in `config` of a connection of `p` being connected, re-paired
    /// or re-authorized by `viewer` (`this`: its id, when it has one):
    /// - a provider that does not pair takes no [`PAIR_KEY`] (`400`);
    /// - a pair must be a connection the viewer can see in the same workspace, not revoked, of an
    ///   adapter the provider pairs with, and not the connection itself;
    /// - without one, the only candidate is picked when `auto` (exactly one visible connection of
    ///   those adapters); a required pair that cannot be found refuses.
    fn check_pair(
        &self,
        c: &rusqlite::Connection,
        viewer: &Viewer,
        p: &dyn ConnectionProvider,
        config: &mut Value,
        this: Option<&str>,
        auto: bool,
    ) -> VaultResult<Option<Paired>> {
        let Some(o) = config.as_object_mut() else {
            return Ok(None);
        };
        let named = match o.get(PAIR_KEY) {
            None | Some(Value::Null) => None,
            Some(Value::String(x)) if x.trim().is_empty() => None,
            Some(Value::String(x)) => Some(x.trim().to_string()),
            Some(_) => return Err(ApiError::invalid(PAIR_KEY).into()),
        };
        let Some(pairing) = p.pairing() else {
            if named.is_some() {
                return Err(
                    ApiError::rule(steve_i18n::t!("modules.connections.pair.not_paired")).into(),
                );
            }
            o.remove(PAIR_KEY);
            return Ok(None);
        };
        let row = match named {
            Some(id) => {
                let r = self.own(c, viewer, &id)?;
                if !pairing.with.contains(&r.adapter) || this == Some(r.id.as_str()) {
                    let name = self
                        .provider(&r.adapter)
                        .map(|x| x.name().to_string())
                        .unwrap_or_else(|| r.adapter.clone());
                    return Err(ApiError::rule(steve_i18n::t!(
                        "modules.connections.pair.wrong",
                        name = name
                    ))
                    .into());
                }
                if r.status == "revoked" {
                    return Err(ApiError::conflict(steve_i18n::t!(
                        "modules.connections.pair.revoked"
                    ))
                    .into());
                }
                Some(r)
            }
            None if auto => {
                let mut found = Vec::new();
                for r in repo::list(c, &viewer.ws)? {
                    if pairing.with.contains(&r.adapter)
                        && r.status != "revoked"
                        && this != Some(r.id.as_str())
                        && self.own(c, viewer, &r.id).is_ok()
                    {
                        found.push(r);
                    }
                }
                match found.len() {
                    1 => found.pop(),
                    0 => None,
                    _ if pairing.required => {
                        return Err(ApiError::rule(steve_i18n::t!(
                            "modules.connections.pair.choose"
                        ))
                        .into());
                    }
                    _ => None,
                }
            }
            None => None,
        };
        match row {
            Some(r) => {
                o.insert(PAIR_KEY.into(), Value::String(r.id.clone()));
                Ok(Some(Paired {
                    ok: matches!(r.status.as_str(), "ok" | "error"),
                    id: r.id,
                    adapter: r.adapter,
                }))
            }
            None if pairing.required => {
                let names = pairing
                    .with
                    .iter()
                    .map(|a| {
                        self.provider(a)
                            .map(|x| x.name().to_string())
                            .unwrap_or_else(|| a.clone())
                    })
                    .collect::<Vec<_>>()
                    .join(" / ");
                Err(ApiError::rule(steve_i18n::t!(
                    "modules.connections.pair.needed",
                    names = names
                ))
                .into())
            }
            None => {
                o.remove(PAIR_KEY);
                Ok(None)
            }
        }
    }

    /// `PATCH /connections/:id {app_connection}`: pairs the connection with another one (the same
    /// checks as connecting), or unpairs it (`None`; refused when its pair is required). Emits a
    /// connection event, so what runs for it follows the new pair.
    pub fn set_pairing(
        &self,
        who: &Caller,
        id: &str,
        to: Option<&str>,
    ) -> VaultResult<ConnectionOut> {
        let viewer = self.viewer(who)?;
        let now = self.now();
        let r = self.0.m.store.transaction(|tx| {
            let mut r = self.own(tx, &viewer, id)?;
            let p = self.provider_or_404(&r.adapter)?;
            if p.pairing().is_none() {
                return Err(
                    ApiError::rule(steve_i18n::t!("modules.connections.pair.not_paired")).into(),
                );
            }
            if r.status == "revoked" {
                return Err(
                    ApiError::conflict(steve_i18n::t!("modules.connections.revoked")).into(),
                );
            }
            let mut config = r.config.clone();
            if !config.is_object() {
                config = Value::Object(Map::new());
            }
            config[PAIR_KEY] = to.map_or(Value::Null, |t| Value::String(t.to_string()));
            self.check_pair(tx, &viewer, p.as_ref(), &mut config, Some(&r.id), false)?;
            if config.get(PAIR_KEY) != r.config.get(PAIR_KEY) {
                r.config = config;
                repo::update(tx, &r, now)?;
                base::audit(
                    tx,
                    &viewer.ws,
                    &who.me,
                    "connection.pair",
                    &r.id,
                    &json!({"adapter": r.adapter, PAIR_KEY: r.config.get(PAIR_KEY)}),
                )?;
                tx.publish(ChangeEvent::Connection {
                    id: r.id.clone(),
                    ws: viewer.ws.clone(),
                });
            }
            Ok::<_, VaultError>(r)
        })?;
        self.forget_tokens(&r.id);
        Ok(self.out(&r))
    }

    pub fn usage(&self, who: &Caller, id: &str) -> VaultResult<Usage> {
        let viewer = self.viewer(who)?;
        self.0.m.store.reader(|c| {
            let r = self.own(c, &viewer, id)?;
            let mut u = Usage::default();
            for x in &self.0.usage {
                let v = x.usage(c, &r.workspace_id, &r.id)?;
                u.flows.extend(v.flows);
                u.spaces += v.spaces;
                u.sources.extend(v.sources);
            }
            Ok(u)
        })
    }

    // ------------------------------------------------------------ connecting

    /// Stores the credential, verifies it, then upserts the account and connection. The
    /// common end of every way in; modules with their own flows (the GitHub App installation) call it
    /// directly with the secrets and config they collected. `reauth_of`: the credential must belong
    /// to that connection's account.
    #[allow(clippy::too_many_arguments)]
    pub async fn connect(
        &self,
        who: &Caller,
        adapter: &str,
        secrets: Vec<(String, Zeroizing<String>)>,
        config: Value,
        scopes: Vec<String>,
        caps: Option<Vec<String>>,
        reauth_of: Option<String>,
    ) -> VaultResult<Connected> {
        let p = self.provider_or_404(adapter)?;
        let (expires_at, refresh_exp) = {
            let get = |k: &str| {
                secrets
                    .iter()
                    .find(|(l, _)| l == k)
                    .and_then(|(_, v)| v.parse::<i64>().ok())
            };
            (get(EXPIRES_AT), get(REFRESH_EXPIRES_AT))
        };
        let title = format!("{} 连接", p.name());
        let w = who.clone();
        let item = self
            .blocking(move |c| {
                asvc::start(&c.0.m, &w)?;
                c.0.vault
                    .create_credential(&w.ws, &w.me, &title, None, secrets)
            })
            .await?;
        let paired = self.paired(&who.ws, adapter, &config).unwrap_or(None);
        let cx = ConnCx {
            conns: self.clone(),
            ws: who.ws.clone(),
            id: reauth_of.clone().unwrap_or_else(|| uid("con")),
            adapter: adapter.into(),
            item: Some(item.clone()),
            config: config.clone(),
            scopes: scopes.clone(),
            paired,
            pending: true,
        };
        let info = match p.verify(&cx).await {
            Ok(i) => i,
            Err(e) => {
                tracing::info!(adapter, "connection verify failed");
                self.forget_tokens(&cx.id);
                self.destroy_items(&who.ws, &who.me, vec![item]).await;
                return Err(ApiError::rule(steve_i18n::t!(
                    "modules.connections.verify_failed",
                    error = e
                ))
                .into());
            }
        };
        let (w, it, pid, cid) = (who.clone(), item.clone(), p.clone(), cx.id.clone());
        let res = self
            .blocking(move |c| {
                let viewer = c.viewer(&w)?;
                c.0.m.store.transaction(|tx| {
                    c.upsert_tx(
                        tx,
                        &viewer,
                        &w.me,
                        pid.as_ref(),
                        Upsert {
                            id: cid,
                            item: it,
                            info,
                            config,
                            scopes,
                            caps,
                            reauth_of,
                            expires_at,
                            refresh_exp,
                            fresh_only: false,
                        },
                    )
                })
            })
            .await;
        match res {
            Ok((done, old)) => {
                // tokens cached for the old credential, or under the id verify ran with
                if cx.id != done.connection_id {
                    self.forget_tokens(&cx.id);
                    if done.reconnected {
                        self.forget_tokens(&done.connection_id);
                    }
                }
                if let Some(old) = old {
                    self.destroy_items(&who.ws, &who.me, vec![old]).await;
                }
                tracing::info!(adapter, connection = %done.connection_id, reconnected = done.reconnected, "connection connected");
                Ok(done)
            }
            Err(e) => {
                self.forget_tokens(&cx.id);
                self.destroy_items(&who.ws, &who.me, vec![item]).await;
                Err(e)
            }
        }
    }

    // ------------------------------------------------- registration-owned commit (feishu app bootstrap)

    /// Seals `fields` as a connection credential for `who` without writing it: the caller writes it
    /// with [`Self::insert_prepared`] in the transaction that records it, so a credential exists
    /// exactly when its owner's row says so.
    pub async fn prepare_credential(
        &self,
        who: &Caller,
        title: &str,
        fields: Vec<(String, Zeroizing<String>)>,
    ) -> VaultResult<crate::vault::items::PreparedItem> {
        let (w, title) = (who.clone(), title.to_string());
        self.blocking(move |c| {
            asvc::start(&c.0.m, &w)?;
            c.0.vault
                .prepare_credential(&w.ws, &w.me, &title, None, fields)
        })
        .await
    }

    /// Writes a [`Self::prepare_credential`] item in `tx`; returns its id.
    pub fn insert_prepared(
        &self,
        tx: &Tx,
        p: &crate::vault::items::PreparedItem,
    ) -> VaultResult<String> {
        self.0.vault.insert_prepared(tx, p)
    }

    /// Destroys a credential item in `tx` (missing or already destroyed is fine).
    pub fn destroy_item_tx(&self, tx: &Tx, ws: &str, actor: &str, item: &str) -> VaultResult<()> {
        self.0.vault.destroy_item(tx, ws, actor, item)?;
        Ok(())
    }

    /// Verifies the credential item `item` (already stored by its owner) and binds a **new**
    /// connection on it, unlike [`Self::connect`]:
    /// - a failed verification keeps the item (the caller may hold the only copy of the secret);
    /// - an account that already has a connection here is refused (`409`), never re-authorized;
    /// - `guard` runs in the binding transaction after the connection row, and its error rolls the
    ///   connection back: the caller checks there that it may still bind (not cancelled) and records
    ///   the result in the same commit.
    #[allow(clippy::too_many_arguments)]
    pub async fn bind_existing<G>(
        &self,
        who: &Caller,
        adapter: &str,
        item: &str,
        config: Value,
        scopes: Vec<String>,
        caps: Option<Vec<String>>,
        guard: G,
    ) -> VaultResult<Connected>
    where
        G: FnOnce(&Tx, &Connected) -> VaultResult<()> + Send + 'static,
    {
        let p = self.provider_or_404(adapter)?;
        let config = self.clean_config(who, p.as_ref(), config, None)?;
        let paired = self.paired(&who.ws, adapter, &config).unwrap_or(None);
        let cx = ConnCx {
            conns: self.clone(),
            ws: who.ws.clone(),
            id: uid("con"),
            adapter: adapter.into(),
            item: Some(item.to_string()),
            config: config.clone(),
            scopes: scopes.clone(),
            paired,
            pending: true,
        };
        let info = match p.verify(&cx).await {
            Ok(i) => i,
            Err(e) => {
                tracing::info!(adapter, "connection verify failed (credential kept)");
                self.forget_tokens(&cx.id);
                return Err(ApiError::rule(steve_i18n::t!(
                    "modules.connections.verify_failed",
                    error = e
                ))
                .into());
            }
        };
        let (w, it, pid, cid) = (who.clone(), item.to_string(), p.clone(), cx.id.clone());
        let res = self
            .blocking(move |c| {
                let viewer = c.viewer(&w)?;
                c.0.m.store.transaction(|tx| {
                    let (done, _) = c.upsert_tx(
                        tx,
                        &viewer,
                        &w.me,
                        pid.as_ref(),
                        Upsert {
                            id: cid,
                            item: it,
                            info,
                            config,
                            scopes,
                            caps,
                            reauth_of: None,
                            expires_at: None,
                            refresh_exp: None,
                            fresh_only: true,
                        },
                    )?;
                    guard(tx, &done)?;
                    Ok::<_, VaultError>(done)
                })
            })
            .await;
        if res.is_err() {
            self.forget_tokens(&cx.id);
        }
        if let Ok(d) = &res {
            tracing::info!(adapter, connection = %d.connection_id, "connection bound to a registered credential");
        }
        res
    }

    async fn destroy_items(&self, ws: &str, actor: &str, items: Vec<String>) {
        let (ws, actor) = (ws.to_string(), actor.to_string());
        let r = self
            .blocking(move |c| {
                c.0.m.store.transaction(|tx| {
                    for i in &items {
                        c.0.vault.destroy_item(tx, &ws, &actor, i)?;
                    }
                    Ok::<_, VaultError>(())
                })
            })
            .await;
        if r.is_err() {
            tracing::error!("could not destroy a connection credential item");
        }
    }
}

struct Upsert {
    id: String,
    item: String,
    info: AccountInfo,
    config: Value,
    scopes: Vec<String>,
    caps: Option<Vec<String>>,
    reauth_of: Option<String>,
    expires_at: Option<i64>,
    refresh_exp: Option<i64>,
    /// Refuse an account that already has a connection here instead of re-authorizing it (an app
    /// registration never replaces a connection it did not make).
    fresh_only: bool,
}

impl Connections {
    /// Default 负责人 on connect: the connection is owned by the person whose
    /// account it is, else by the connecting user; a bot party is owned by the person the provider
    /// names (source `sync`), else by the connecting user (source `connect`). Objects that have
    /// stewards already keep them.
    fn link_stewards(
        &self,
        tx: &Tx,
        ws: &str,
        actor: &str,
        account: &arepo::Account,
        connection: &str,
        info: &AccountInfo,
    ) -> VaultResult<()> {
        if !stewards::has_any(tx, ws, "connection", connection)?
            && let Some(p) = stewards::connection_owner(tx, ws, &account.party_id, actor)?
        {
            stewards::link(tx, ws, "connection", connection, &p, "connect", None)?;
        }
        let Some(bot) = current_party(tx, &account.party_id)?
            .filter(|p| p.contact_kind.as_deref() == Some("bot"))
        else {
            return Ok(());
        };
        let hinted = match &info.steward_hint {
            Some(h) => self.hinted_person(tx, ws, h)?,
            None => None,
        };
        match hinted {
            Some(p) => {
                stewards::link(tx, ws, "party", &bot.id, &p, "sync", Some(connection))?;
            }
            None if !stewards::has_any(tx, ws, "party", &bot.id)? => {
                if let Some(me) = people::me_of(tx, ws, actor)? {
                    stewards::link(
                        tx,
                        ws,
                        "party",
                        &bot.id,
                        &me.id,
                        "connect",
                        Some(connection),
                    )?;
                }
            }
            None => {}
        }
        Ok(())
    }

    /// The person behind a provider-named handle, if that handle is a known person's account.
    fn hinted_person(&self, tx: &Tx, ws: &str, h: &StewardHint) -> VaultResult<Option<String>> {
        let Ok(system) = own_system(tx, ws, &h.system) else {
            return Ok(None);
        };
        let Some(n) = normalize(&system, &h.handle) else {
            return Ok(None);
        };
        let Some(id) = arepo::by_handle(tx, ws, &system.family, &n.handle)? else {
            return Ok(None);
        };
        let Some(a) = arepo::get(tx, &id)? else {
            return Ok(None);
        };
        Ok(current_party(tx, &a.party_id)?
            .filter(|p| {
                p.kind == "contact"
                    && p.status == "active"
                    && p.contact_kind.as_deref() != Some("bot")
            })
            .map(|p| p.id))
    }

    /// The account and connection rows for a verified credential. Returns the old credential item
    /// to destroy after the commit when an existing connection got the new one.
    fn upsert_tx(
        &self,
        tx: &Tx,
        viewer: &Viewer,
        actor: &str,
        p: &dyn ConnectionProvider,
        mut u: Upsert,
    ) -> VaultResult<(Connected, Option<String>)> {
        let ws = viewer.ws.as_str();
        let now = self.now();
        let system = own_system(tx, ws, p.system())?;
        let h = normalize(&system, &u.info.handle).ok_or_else(|| {
            ApiError::rule(steve_i18n::t!("modules.connections.verify_no_account"))
        })?;
        // your own identity, proven by the provider: a contact a channel made for it joins “我” first
        if u.reauth_of.is_none() && u.info.owner == Owner::Me && p.claims_self() {
            self.claim_into_me(tx, viewer, actor, &system.id, &u.info)?;
        }
        let known = arepo::by_handle(tx, ws, &system.family, &h.handle)?;
        let owner = match &u.info.owner {
            Owner::Me => people::me_of(tx, ws, actor)?,
            Owner::SelfOrg => people::self_org(tx, ws)?,
            Owner::Team(id) => people::get(tx, id)?
                .filter(|t| t.kind == "team" && t.workspace_id == ws && t.status == "active"),
            // a new app bot gets its own bot party; a known one keeps its party
            Owner::Bot if known.is_none() => Some(stewards::new_bot_party(
                tx,
                ws,
                actor,
                if u.info.display_name.trim().is_empty() {
                    &h.display
                } else {
                    &u.info.display_name
                },
                "connect",
            )?),
            Owner::Bot => people::self_org(tx, ws)?,
        }
        .ok_or_else(|| ApiError::not_found("联系人"))?;
        let mut personal_of: Option<String> = None;
        let account = match known {
            Some(id) => {
                let a = arepo::get(tx, &id)?.ok_or_else(|| ApiError::not_found("账号"))?;
                let party = current_party(tx, &a.party_id)?;
                if !party.as_ref().is_some_and(may_connect) {
                    return Err(asvc::taken(tx, viewer, &a.id)?.into());
                }
                // someone's own account (their “我”) is connected only by that person
                if let Some(p) = party.as_ref().filter(|p| p.is_me()) {
                    if p.user_id.as_deref() != Some(actor) {
                        return Err(asvc::taken(tx, viewer, &a.id)?.into());
                    }
                    personal_of = p.user_id.clone();
                }
                a
            }
            None => {
                if u.reauth_of.is_some() {
                    return Err(ApiError::rule(format!(
                        "这次授权的是另一个账号（{}），请用原来的账号重新授权",
                        h.display
                    ))
                    .into());
                }
                asvc::record_tx(
                    tx,
                    viewer,
                    actor,
                    &Record {
                        system_id: system.id.clone(),
                        party_id: owner.id.clone(),
                        kind: u.info.kind.clone(),
                        handle: u.info.handle.clone(),
                        display_name: u.info.display_name.clone(),
                        verified: true,
                        confidence: 1.0,
                        source: "connect".into(),
                        source_label: default_label("connect").into(),
                    },
                )?
            }
        };
        let existing = repo::of_account(tx, &account.id)?;
        // …and a personal connection is replaced only by the person who made it
        if personal_of.is_some()
            && let Some(c) = &existing
            && c.created_by != actor
        {
            return Err(
                ApiError::conflict(steve_i18n::t!("modules.connections.other_member")).into(),
            );
        }
        if let Some(r) = &u.reauth_of
            && existing.as_ref().is_none_or(|c| &c.id != r)
        {
            return Err(ApiError::rule(steve_i18n::t!(
                "modules.connections.other_account",
                account = h.display
            ))
            .into());
        }
        // the pair again, in the commit: it may have gone since the flow started. A connection that
        // is there keeps the pair it has now, not the copy its flow took at the start (a re-pair
        // made meanwhile stands): on a re-authorization the row's pair alone, on a new connect onto
        // it the flow's only when the row has none.
        let this = existing.as_ref().map_or(u.id.clone(), |c| c.id.clone());
        let mut auto = true;
        if let Some(c) = &existing {
            let current = c.config.get(PAIR_KEY).filter(|v| !v.is_null()).cloned();
            if current.is_some() || u.reauth_of.is_some() {
                put_key(&mut u.config, PAIR_KEY, current);
                // a pair dropped meanwhile is not picked again (unless it is required)
                auto = p.pairing().is_some_and(|x| x.required);
            }
        }
        self.check_pair(tx, viewer, p, &mut u.config, Some(&this), auto)?;
        let pair = u.config.get(PAIR_KEY).cloned();
        let version = version_value(p.config_version());
        let available = p.available_caps(&u.scopes);
        let pick = |req: &Option<Vec<String>>, current: &[String]| -> Vec<String> {
            match req {
                Some(r) => available
                    .iter()
                    .filter(|c| r.contains(c))
                    .cloned()
                    .collect(),
                None if current.is_empty() => available.clone(),
                None => available
                    .iter()
                    .filter(|c| current.contains(c))
                    .cloned()
                    .collect(),
            }
        };
        let mut config = u.config.clone();
        merge(&mut config, &u.info.config);
        // the checked pair and the current version, whatever the merges brought
        put_key(&mut config, PAIR_KEY, pair.clone());
        put_key(&mut config, CONFIG_VERSION_KEY, version.clone());
        let health = HealthState {
            last_ok: now,
            expires_at: u.expires_at,
            refresh_expires_at: u.refresh_exp,
            ..Default::default()
        };
        let (done, old) = match existing {
            Some(c) if u.fresh_only => {
                return Err(ApiError::conflict(steve_i18n::t!(
                    "modules.connections.app_connected",
                    id = c.id
                ))
                .into());
            }
            Some(mut c) => {
                if c.adapter != p.id() {
                    return Err(ApiError::conflict(steve_i18n::t!(
                        "modules.connections.connected_via",
                        adapter = self
                            .provider(&c.adapter)
                            .map(|x| x.name().to_string())
                            .unwrap_or(c.adapter.clone())
                    ))
                    .into());
                }
                let old = c.credential_item.replace(u.item.clone());
                c.status = "ok".into();
                c.caps = pick(&u.caps, &c.caps);
                c.granted_scopes = u.scopes.clone();
                merge(&mut c.config, &config);
                put_key(&mut c.config, PAIR_KEY, pair);
                put_key(&mut c.config, CONFIG_VERSION_KEY, version);
                c.health = health;
                repo::update(tx, &c, now)?;
                // connected again: what it missed meanwhile is history, not news
                repo::set_enabled_at(tx, &c.id, now)?;
                self.settle_decision(tx, ws, &c.id, json!({"reauthorized": true}))?;
                base::audit(
                    tx,
                    ws,
                    actor,
                    "connection.reauth",
                    &c.id,
                    &json!({"adapter": c.adapter, "account": account.id}),
                )?;
                (
                    Connected {
                        connection_id: c.id,
                        account_id: account.id.clone(),
                        reconnected: true,
                    },
                    old.filter(|o| o != &u.item),
                )
            }
            None => {
                let r = ConnRow {
                    id: u.id.clone(),
                    workspace_id: ws.into(),
                    account_id: account.id.clone(),
                    adapter: p.id().into(),
                    auth: p.auth().column().into(),
                    granted_scopes: u.scopes.clone(),
                    caps: pick(&u.caps, &[]),
                    credential_item: Some(u.item.clone()),
                    status: "ok".into(),
                    health,
                    config,
                    last_sync: None,
                    created_by: actor.into(),
                    revision: 1,
                    created: now,
                    updated: now,
                };
                repo::insert(tx, &r)?;
                base::audit(
                    tx,
                    ws,
                    actor,
                    "connection.create",
                    &r.id,
                    &json!({"adapter": r.adapter, "account": account.id, "caps": r.caps}),
                )?;
                (
                    Connected {
                        connection_id: r.id,
                        account_id: account.id.clone(),
                        reconnected: false,
                    },
                    None,
                )
            }
        };
        repo::link_item(tx, &u.item, &account.id)?;
        self.link_stewards(tx, ws, actor, &account, &done.connection_id, &u.info)?;
        for x in &u.info.extra_handles {
            if let Some(eh) = normalize(&system, x) {
                arepo::add_handle(
                    tx,
                    ws,
                    &system.family,
                    &eh.handle,
                    &account.id,
                    Some(&done.connection_id),
                )?;
            }
        }
        // the same identity in other systems: the owner's accounts too, when nobody holds them
        for (sys, handle) in &u.info.aliases {
            attach_handle(
                tx,
                ws,
                actor,
                &account.party_id,
                sys,
                handle,
                &[],
                &Attach {
                    kind: &u.info.kind,
                    display_name: &u.info.display_name,
                    source: "connect",
                    source_label: default_label("connect"),
                },
            )?;
        }
        tx.publish(ChangeEvent::Connection {
            id: done.connection_id.clone(),
            ws: ws.into(),
        });
        tx.publish(ChangeEvent::Account {
            id: account.id.clone(),
            ws: ws.into(),
        });
        Ok((done, old))
    }

    /// [`claim_self`] of a verified identity for `actor`'s “我”: the primary and extra handles in
    /// the provider's system, then each alias in its own. In a savepoint: a merge that fails is
    /// undone and only logged (the connect then says who holds the account).
    fn claim_into_me(
        &self,
        tx: &Tx,
        viewer: &Viewer,
        actor: &str,
        system: &str,
        info: &AccountInfo,
    ) -> VaultResult<()> {
        let Some(me) = people::me_of(tx, &viewer.ws, actor)? else {
            return Ok(());
        };
        tx.execute_batch("SAVEPOINT conn_claim")
            .map_err(steve_store::StoreError::from)?;
        let run = || -> crate::ApiResult<Vec<String>> {
            let hooks = &self.0.party_hooks;
            let mut handles: Vec<&str> = vec![info.handle.as_str()];
            handles.extend(info.extra_handles.iter().map(String::as_str));
            let mut merged = claim_self(tx, hooks, viewer, actor, &me, system, &handles)?;
            for (sys, h) in &info.aliases {
                merged.extend(claim_self(tx, hooks, viewer, actor, &me, sys, &[h])?);
            }
            Ok(merged)
        };
        match run() {
            Ok(_) => tx.execute_batch("RELEASE conn_claim"),
            Err(e) => {
                tracing::warn!(error = %e, "connection: could not merge your contact into 我");
                tx.execute_batch("ROLLBACK TO conn_claim; RELEASE conn_claim")
            }
        }
        .map_err(steve_store::StoreError::from)?;
        Ok(())
    }

    /// Settles the open `reauth` decision of a connection, if any.
    fn settle_decision(&self, tx: &Tx, ws: &str, id: &str, result: Value) -> VaultResult<()> {
        if let Some(d) = base::decisions::open_for(tx, ws, SUBJECT, id)? {
            base::decisions::settle(tx, &d.id, &result, "resolved", &Value::Null)?;
        }
        Ok(())
    }

    // ------------------------------------------------------------ OAuth code flow

    /// `POST /connections/oauth/start` (and the OAuth branch of reauth): a state with the PKCE
    /// verifier kept here; the URL for the browser.
    pub async fn oauth_start(
        &self,
        who: &Caller,
        origin: &str,
        adapter: &str,
        caps: Option<Vec<String>>,
        config: Value,
        reauth_of: Option<String>,
    ) -> VaultResult<String> {
        let p = self.provider_or_404(adapter)?;
        let spec = match (p.auth(), p.oauth()) {
            (AuthKind::OAuth, Some(s)) => s.clone(),
            _ => return Err(ApiError::invalid("adapter").into()),
        };
        let config = self.clean_config(who, p.as_ref(), config, reauth_of.as_deref())?;
        let spec = with_extra_scopes(spec, &config);
        let client = p
            .oauth_client(&StartCx {
                conns: self,
                ws: &who.ws,
                config: &config,
            })
            .await
            .map_err(perr)?
            .ok_or_else(|| not_configured(&spec.provider_name))?;
        let verifier = Zeroizing::new(random_token(32).map_err(perr)?);
        let challenge = pkce_challenge(&verifier);
        let redirect_uri = format!("{origin}{CALLBACK_PATH}");
        let state = self
            .0
            .flows
            .insert(
                self.now(),
                Flow {
                    ws: who.ws.clone(),
                    user: who.me.clone(),
                    adapter: adapter.into(),
                    verifier,
                    redirect_uri: redirect_uri.clone(),
                    caps,
                    config,
                    reauth_of,
                },
            )
            .map_err(perr)?;
        oauth::authorize_url(&spec, &client.id, &redirect_uri, &state, &challenge).map_err(perr)
    }

    /// `GET /auth/connect/callback`: where to send the browser (`/#/accounts/<id>`, or
    /// `/#/accounts?error=…`). The state is consumed whatever happens; it must have been issued here,
    /// within 10 minutes, to the signed-in user.
    pub async fn oauth_callback(
        &self,
        who: Option<&Caller>,
        state: Option<&str>,
        code: Option<&str>,
        error: Option<&str>,
    ) -> String {
        match self.callback_inner(who, state, code, error).await {
            Ok(account) => format!("/#/accounts/{account}"),
            Err(msg) => error_location(&msg),
        }
    }

    async fn callback_inner(
        &self,
        who: Option<&Caller>,
        state: Option<&str>,
        code: Option<&str>,
        error: Option<&str>,
    ) -> Result<String, String> {
        let stale = || steve_i18n::t!("modules.connections.callback.stale");
        let flow = state
            .and_then(|s| self.0.flows.take(self.now(), s))
            .ok_or_else(stale)?;
        let who = who.ok_or_else(|| "请先登录 Steve，再重新接入".to_string())?;
        if who.ws != flow.ws || who.me != flow.user {
            return Err(stale());
        }
        let p = self.provider(&flow.adapter).ok_or_else(stale)?;
        let spec = p.oauth().cloned().ok_or_else(stale)?;
        let spec = with_extra_scopes(spec, &flow.config);
        if let Some(e) = error {
            return Err(if e == "access_denied" {
                steve_i18n::t!(
                    "modules.connections.callback.denied",
                    provider = spec.provider_name
                )
            } else {
                steve_i18n::t!(
                    "modules.connections.callback.failed",
                    error = e.chars().take(60).collect::<String>()
                )
            });
        }
        let code = code.filter(|c| !c.is_empty()).ok_or_else(stale)?;
        let client = p
            .oauth_client(&StartCx {
                conns: self,
                ws: &flow.ws,
                config: &flow.config,
            })
            .await
            .map_err(|e| e.to_string())?
            .ok_or_else(|| {
                steve_i18n::t!(
                    "modules.connections.oauth_not_configured",
                    provider = spec.provider_name
                )
            })?;
        let set = oauth::exchange_code(
            self.http(),
            &spec,
            &client,
            code,
            &flow.redirect_uri,
            &flow.verifier,
        )
        .await
        .map_err(|e| steve_i18n::t!("modules.connections.callback.exchange_failed", error = e))?;
        let scopes = scopes_of(&set, &spec.scopes);
        let (fields, _, _) = token_fields(&set, None, None, self.now());
        let done = self
            .connect(
                who,
                &flow.adapter,
                fields,
                flow.config,
                scopes,
                flow.caps,
                flow.reauth_of,
            )
            .await
            .map_err(|e| e.to_string())?;
        Ok(done.account_id)
    }

    // ------------------------------------------------------------ device flow

    /// `POST /connections/device/start`: asks the provider for a device code and polls in the
    /// background; the page shows the user code and reads [`Connections::device_status`].
    pub async fn device_start(
        &self,
        who: &Caller,
        adapter: &str,
        caps: Option<Vec<String>>,
        config: Value,
    ) -> VaultResult<DeviceOut> {
        let p = self.provider_or_404(adapter)?;
        let spec = match (p.auth(), p.oauth()) {
            (AuthKind::OAuth, Some(s)) if s.device_url.is_some() => s.clone(),
            (AuthKind::OAuth, Some(s)) => {
                return Err(ApiError::rule(steve_i18n::t!(
                    "modules.connections.no_device_flow",
                    provider = s.provider_name
                ))
                .into());
            }
            _ => return Err(ApiError::invalid("adapter").into()),
        };
        let config = self.clean_config(who, p.as_ref(), config, None)?;
        let client = p
            .oauth_client(&StartCx {
                conns: self,
                ws: &who.ws,
                config: &config,
            })
            .await
            .map_err(perr)?
            .ok_or_else(|| not_configured(&spec.provider_name))?;
        let ds = oauth::device_start(self.http(), &spec, &client)
            .await
            .map_err(perr)?;
        let id = uid("dev");
        let now = self.now();
        self.0.devices.insert(
            now,
            &id,
            Device {
                ws: who.ws.clone(),
                user: who.me.clone(),
                status: DeviceStatus::Pending,
                created: now,
                expires_at: now + ds.expires_in * 1000,
            },
        );
        let out = DeviceOut {
            id: id.clone(),
            user_code: ds.user_code.clone(),
            verification_uri: ds.verification_uri.clone(),
            verification_uri_complete: ds.verification_uri_complete.clone(),
            expires_in: ds.expires_in,
            interval: ds.interval,
        };
        let (c, who, adapter) = (self.clone(), who.clone(), adapter.to_string());
        let deadline = tokio::time::Instant::now() + Duration::from_secs(ds.expires_in as u64);
        tokio::spawn(async move {
            let mut interval = ds.interval as u64;
            let status = loop {
                tokio::time::sleep(Duration::from_secs(interval)).await;
                if tokio::time::Instant::now() >= deadline {
                    break DeviceStatus::Failed("设备码已经过期，请重新开始".into());
                }
                match oauth::device_poll(c.http(), &spec, &client, &ds.device_code).await {
                    Ok(DevicePoll::Pending) => continue,
                    Ok(DevicePoll::SlowDown) => interval += 5,
                    Ok(DevicePoll::Failed(m)) => break DeviceStatus::Failed(m),
                    // network trouble: keep polling until the code expires
                    Err(ProviderError::Transient(_)) => continue,
                    Err(e) => break DeviceStatus::Failed(e.to_string()),
                    Ok(DevicePoll::Done(set)) => {
                        let scopes = scopes_of(&set, &spec.scopes);
                        let (fields, _, _) = token_fields(&set, None, None, c.now());
                        break match c
                            .connect(&who, &adapter, fields, config, scopes, caps, None)
                            .await
                        {
                            Ok(d) => DeviceStatus::Ok {
                                account_id: d.account_id,
                            },
                            Err(e) => DeviceStatus::Failed(e.to_string()),
                        };
                    }
                }
            };
            c.0.devices.set(&id, status);
        });
        Ok(out)
    }

    /// `GET /connections/device/:id`: `{status: pending|ok|error, account_id?, error?}`.
    pub fn device_status(&self, who: &Caller, id: &str) -> VaultResult<Value> {
        let d = self
            .0
            .devices
            .get(id)
            .filter(|d| d.ws == who.ws && d.user == who.me)
            .ok_or_else(|| ApiError::not_found("设备码登录"))?;
        Ok(match d.status {
            DeviceStatus::Pending => json!({"status": "pending"}),
            DeviceStatus::Ok { account_id } => json!({"status": "ok", "account_id": account_id}),
            DeviceStatus::Failed(e) => json!({"status": "error", "error": e}),
        })
    }

    // ------------------------------------------------------------ credential form

    /// The config of a connect path, before anything is stored or verified: drops `api_base`
    /// outside tests (adapters only honour it there) and the stored version
    /// ([`CONFIG_VERSION_KEY`], the framework's), runs the provider's
    /// [`ConnectionProvider::check_config`] (`400` when it refuses) and checks the pair
    /// ([`Self::check_pair`], picking the only candidate when none is named). `this`: the
    /// connection being re-authorized.
    fn clean_config(
        &self,
        who: &Caller,
        p: &dyn ConnectionProvider,
        mut config: Value,
        this: Option<&str>,
    ) -> VaultResult<Value> {
        if !config.is_object() {
            config = Value::Object(Map::new());
        }
        if let Some(o) = config.as_object_mut() {
            if !self.0.cfg.allow_api_base {
                o.remove("api_base");
            }
            o.remove(CONFIG_VERSION_KEY);
        }
        p.check_config(
            &StartCx {
                conns: self,
                ws: &who.ws,
                config: &config,
            },
            &config,
        )
        .map_err(|e| {
            VaultError::from(ApiError::rule(steve_i18n::t!(
                "modules.connections.config_rejected",
                error = e
            )))
        })?;
        let viewer = self.viewer(who)?;
        self.0
            .m
            .store
            .reader(|c| self.check_pair(c, &viewer, p, &mut config, this, true))?;
        Ok(config)
    }

    /// Splits a form into vault secrets and plain config by the provider's [`FieldDef`]s. Values
    /// are looked up by key, then by label.
    fn split_fields(
        p: &dyn ConnectionProvider,
        fields: &BTreeMap<String, Zeroizing<String>>,
        config: &mut Value,
    ) -> VaultResult<Vec<(String, Zeroizing<String>)>> {
        let mut ck = Check::new();
        let mut secrets = Vec::new();
        for f in p.fields() {
            let v = fields
                .get(f.key)
                .or_else(|| fields.get(f.label))
                .map(|v| v.trim())
                .filter(|v| !v.is_empty())
                .or(f.default);
            let path = format!("fields.{}", f.key);
            match v {
                None => {
                    ck.that(&path, !f.required);
                }
                Some(v) if f.secret => {
                    ck.len(&path, v, 1, 20_000);
                    secrets.push((f.key.to_string(), Zeroizing::new(v.to_string())));
                }
                Some(v) => {
                    ck.len(&path, v, 1, 500);
                    if let Some(o) = config.as_object_mut() {
                        o.insert(f.key.into(), Value::String(v.to_string()));
                    }
                }
            }
        }
        ck.done()?;
        Ok(secrets)
    }

    /// `POST /connections` (credential adapters).
    pub async fn create(
        &self,
        who: &Caller,
        adapter: &str,
        fields: &BTreeMap<String, Zeroizing<String>>,
        config: Value,
    ) -> VaultResult<ConnectionOut> {
        let p = self.provider_or_404(adapter)?;
        if p.auth() != AuthKind::Credential {
            return Err(ApiError::invalid("adapter").into());
        }
        let mut config = self.clean_config(who, p.as_ref(), config, None)?;
        let secrets = Self::split_fields(p.as_ref(), fields, &mut config)?;
        let done = self
            .connect(who, adapter, secrets, config, Vec::new(), None, None)
            .await?;
        self.get(who, &done.connection_id)
    }

    /// `PUT /connections/:id/credential`: a new credential, verified against the same account, then
    /// swapped in (the old item destroyed only after the new one works).
    pub async fn put_credential(
        &self,
        who: &Caller,
        id: &str,
        fields: &BTreeMap<String, Zeroizing<String>>,
    ) -> VaultResult<ConnectionOut> {
        let viewer = self.viewer(who)?;
        let r = self.0.m.store.reader(|c| self.own(c, &viewer, id))?;
        let p = self.provider_or_404(&r.adapter)?;
        if !matches!(p.auth(), AuthKind::Credential | AuthKind::Managed) {
            return Err(ApiError::rule(steve_i18n::t!("modules.connections.needs_reauth")).into());
        }
        let mut config = self.clean_config(who, p.as_ref(), r.config.clone(), Some(&r.id))?;
        let secrets = Self::split_fields(p.as_ref(), fields, &mut config)?;
        self.connect(
            who,
            &r.adapter,
            secrets,
            config,
            r.granted_scopes.clone(),
            None,
            Some(r.id.clone()),
        )
        .await?;
        self.get(who, id)
    }

    /// `POST /connections/:id/reauth`: OAuth → `{url}`; credential → `{fields}` to fill again.
    pub async fn reauth(&self, who: &Caller, origin: &str, id: &str) -> VaultResult<Value> {
        let viewer = self.viewer(who)?;
        let r = self.0.m.store.reader(|c| self.own(c, &viewer, id))?;
        let p = self.provider_or_404(&r.adapter)?;
        match p.auth() {
            AuthKind::OAuth => {
                let url = self
                    .oauth_start(
                        who,
                        origin,
                        &r.adapter,
                        Some(r.caps.clone()),
                        r.config.clone(),
                        Some(r.id.clone()),
                    )
                    .await?;
                Ok(json!({ "url": url }))
            }
            _ => Ok(json!({
                "fields": p.fields().iter().filter(|f| f.secret).map(|f| f.label).collect::<Vec<_>>()
            })),
        }
    }

    // ------------------------------------------------------------ edits, disconnect

    /// `PATCH /connections/:id {paused?, caps?}`.
    pub fn patch(
        &self,
        who: &Caller,
        id: &str,
        paused: Option<bool>,
        caps: Option<Vec<String>>,
    ) -> VaultResult<ConnectionOut> {
        let viewer = self.viewer(who)?;
        let now = self.now();
        let r = self.0.m.store.transaction(|tx| {
            let mut r = self.own(tx, &viewer, id)?;
            let before = (r.status.clone(), r.caps.clone());
            if let Some(c) = &caps {
                let available = match self.provider(&r.adapter) {
                    Some(p) => p.available_caps(&r.granted_scopes),
                    None => r.caps.clone(),
                };
                let mut ck = Check::new();
                ck.that("caps", c.len() <= 20);
                for (i, x) in c.iter().enumerate() {
                    ck.that(&format!("caps.{i}"), available.contains(x));
                }
                ck.done()?;
                r.caps = available.into_iter().filter(|a| c.contains(a)).collect();
            }
            match paused {
                Some(true) if r.status != "paused" => {
                    if r.status == "revoked" {
                        return Err(ApiError::conflict(steve_i18n::t!(
                            "modules.connections.revoked"
                        ))
                        .into());
                    }
                    r.status = "paused".into();
                }
                Some(false) if r.status == "paused" => {
                    r.status = "ok".into();
                    r.health.fails = 0;
                    r.health.next_at = 0;
                }
                _ => {}
            }
            if (r.status.clone(), r.caps.clone()) != before {
                repo::update(tx, &r, now)?;
                // resumed, or a capability turned on: what comes from before now is history
                let resumed = before.0 == "paused" && r.status != "paused";
                if resumed || r.caps.iter().any(|c| !before.1.contains(c)) {
                    repo::set_enabled_at(tx, &r.id, now)?;
                }
                base::audit(
                    tx,
                    &viewer.ws,
                    &who.me,
                    "connection.patch",
                    &r.id,
                    &json!({"status": r.status, "caps": r.caps}),
                )?;
                tx.publish(ChangeEvent::Connection {
                    id: r.id.clone(),
                    ws: viewer.ws.clone(),
                });
                tx.publish(ChangeEvent::Account {
                    id: r.account_id.clone(),
                    ws: viewer.ws.clone(),
                });
            }
            Ok::<_, VaultError>(r)
        })?;
        Ok(self.out(&r))
    }

    /// `DELETE /connections/:id`: revoke at the provider when possible, destroy the
    /// credential (its data key), delete the row; the account stays, only recorded.
    pub async fn disconnect(&self, who: &Caller, id: &str) -> VaultResult<Usage> {
        let usage = self.usage(who, id)?;
        let viewer = self.viewer(who)?;
        let r = self.0.m.store.reader(|c| self.own(c, &viewer, id))?;
        if let Some(p) = self.provider(&r.adapter)
            && r.credential_item.is_some()
            && let Err(e) = p.revoke(&self.cx_of(&r)).await
        {
            tracing::warn!(connection = %r.id, error = %e, "revoke at the provider failed");
        }
        let (w, rr) = (who.clone(), r.clone());
        self.blocking(move |c| {
            c.0.m.store.transaction(|tx| {
                for u in &c.0.usage {
                    u.on_disconnect(tx, &rr.workspace_id, &rr.id)?;
                }
                if let Some(item) = &rr.credential_item {
                    c.0.vault.destroy_item(tx, &rr.workspace_id, &w.me, item)?;
                }
                repo::delete(tx, &rr.id)?;
                c.settle_decision(tx, &rr.workspace_id, &rr.id, json!({"disconnected": true}))?;
                base::audit(
                    tx,
                    &rr.workspace_id,
                    &w.me,
                    "connection.delete",
                    &rr.id,
                    &json!({"adapter": rr.adapter, "account": rr.account_id}),
                )?;
                tx.publish(ChangeEvent::Connection {
                    id: rr.id.clone(),
                    ws: rr.workspace_id.clone(),
                });
                tx.publish(ChangeEvent::Account {
                    id: rr.account_id.clone(),
                    ws: rr.workspace_id.clone(),
                });
                Ok::<_, VaultError>(())
            })
        })
        .await?;
        self.forget_tokens(&r.id);
        Ok(usage)
    }

    /// Marks a sync done (other modules).
    pub fn touch_sync(&self, ws: &str, id: &str) -> VaultResult<()> {
        let now = self.now();
        self.0.m.store.transaction(|tx| {
            if repo::get(tx, id)?.is_some_and(|r| r.workspace_id == ws) {
                repo::set_last_sync(tx, id, now)?;
                tx.publish(ChangeEvent::Connection {
                    id: id.into(),
                    ws: ws.into(),
                });
            }
            Ok::<_, VaultError>(())
        })
    }

    // ------------------------------------------------------------ tokens

    fn lock_for(&self, id: &str) -> Arc<tokio::sync::Mutex<()>> {
        lock(&self.0.locks).entry(id.into()).or_default().clone()
    }

    fn forget_tokens(&self, id: &str) {
        lock(&self.0.minted).remove(id);
        lock(&self.0.forced).remove(id);
    }

    pub(crate) async fn invalidate(&self, cx: &ConnCx) {
        let kind = self.provider(&cx.adapter).map(|p| p.token_kind());
        match kind {
            Some(TokenKind::Minted) => {
                lock(&self.0.minted).remove(&cx.id);
            }
            Some(TokenKind::Stored) => {
                lock(&self.0.forced).insert(cx.id.clone());
            }
            _ => {}
        }
    }

    pub(crate) async fn access_token(&self, cx: &ConnCx) -> Result<AccessToken, ProviderError> {
        let p = self
            .provider(&cx.adapter)
            .ok_or_else(|| ProviderError::Config("这个接入方式没有装上".into()))?;
        match p.token_kind() {
            TokenKind::Field(k) => Ok(AccessToken(cx.secret(k).await?)),
            TokenKind::None => Err(ProviderError::Unsupported),
            TokenKind::Minted => {
                // verifying a new credential mints from it, never reuses the old one's token
                let now = self.now();
                if !cx.pending
                    && let Some(t) = self.cached_minted(&cx.id, now)
                {
                    return Ok(t);
                }
                let l = self.lock_for(&cx.id);
                let _g = l.lock().await;
                if !cx.pending
                    && let Some(t) = self.cached_minted(&cx.id, now)
                {
                    return Ok(t);
                }
                match p.mint(cx).await {
                    Ok(m) => {
                        let t = AccessToken(m.token.clone());
                        lock(&self.0.minted).insert(cx.id.clone(), (m.expires_at, m.token));
                        Ok(t)
                    }
                    Err(e) => {
                        cx.report(Health::from(&e)).await;
                        Err(e)
                    }
                }
            }
            TokenKind::Stored => {
                let forced = lock(&self.0.forced).remove(&cx.id);
                if let Err(e) = self.refresh_cx(p.as_ref(), cx, USE_MARGIN_MS, forced).await {
                    if forced {
                        lock(&self.0.forced).insert(cx.id.clone());
                    }
                    cx.report(Health::from(&e)).await;
                    return Err(e);
                }
                Ok(AccessToken(cx.secret(ACCESS_TOKEN).await?))
            }
        }
    }

    fn cached_minted(&self, id: &str, now: i64) -> Option<AccessToken> {
        lock(&self.0.minted)
            .get(id)
            .filter(|(exp, _)| exp - now > USE_MARGIN_MS)
            .map(|(_, t)| AccessToken(t.clone()))
    }

    /// Refreshes `cx`'s stored OAuth tokens if they expire within `margin_ms` (or `force`). One at a
    /// time per connection; the state is re-read under the lock, so a refresh someone else just did
    /// is not repeated (a single-use refresh token would fail the second time). The new set is
    /// committed to the vault before this returns and before anyone uses it.
    async fn refresh_cx(
        &self,
        p: &dyn ConnectionProvider,
        cx: &ConnCx,
        margin_ms: i64,
        force: bool,
    ) -> Result<(), ProviderError> {
        let l = self.lock_for(&cx.id);
        let _g = l.lock().await;
        // the item may have been swapped by a reauth meanwhile
        let cx = if cx.pending {
            cx.clone()
        } else {
            self.cx(&cx.ws, &cx.id).map_err(vault_err)?
        };
        let item = cx
            .item
            .clone()
            .ok_or_else(|| ProviderError::Auth("这个连接没有凭据".into()))?;
        let c2 = cx.clone();
        let (refresh, exp, rexp) = tokio::task::spawn_blocking(move || {
            c2.with_credential(|c| {
                let n = |k: &str| c.text(k).and_then(|v| v.parse::<i64>().ok());
                (
                    c.text(REFRESH_TOKEN).map(|v| Zeroizing::new(v.to_string())),
                    n(EXPIRES_AT),
                    n(REFRESH_EXPIRES_AT),
                )
            })
        })
        .await
        .map_err(|_| ProviderError::Config("任务失败".into()))??;
        let now = self.now();
        if !force && exp.is_none_or(|e| e - now > margin_ms) {
            return Ok(());
        }
        let Some(refresh) = refresh else {
            return Err(ProviderError::Auth(
                "没有拿到 refresh token，到期后需要重新授权（飞书：在开发者后台打开“刷新 user_access_token”并发布新版本）"
                    .into(),
            ));
        };
        if rexp.is_some_and(|e| e <= now) {
            return Err(ProviderError::Auth("刷新令牌已经过期，需要重新授权".into()));
        }
        let name = p
            .oauth()
            .map(|s| s.provider_name.clone())
            .unwrap_or_else(|| p.name().to_string());
        let client = p
            .oauth_client(&StartCx {
                conns: self,
                ws: &cx.ws,
                config: &cx.config,
            })
            .await?
            .ok_or_else(|| ProviderError::Config(format!("还没有配置 {name} 的 OAuth 应用")))?;
        let set = p
            .refresh_tokens_for(&cx, self.http(), &client, &refresh)
            .await?;
        let rotated = set.refresh_token.is_some();
        let (fields, exp, rexp) = token_fields(&set, Some(&refresh), rexp, now);
        drop(refresh);
        let scope = set.scope.clone();
        let (ws, id, pending) = (cx.ws.clone(), cx.id.clone(), cx.pending);
        let committed = self
            .blocking(move |c| {
                c.0.vault.replace_credential(&ws, &id, &item, fields)?;
                if !pending {
                    c.0.m.store.transaction(|tx| {
                        if let Some(mut r) = repo::get(tx, &id)? {
                            r.health.expires_at = exp;
                            r.health.refresh_expires_at = rexp;
                            if let Some(s) = &scope {
                                r.granted_scopes = s
                                    .split([' ', ','])
                                    .filter(|x| !x.is_empty())
                                    .map(String::from)
                                    .collect();
                            }
                            repo::set_health(tx, &id, &r.status, &r.health, c.now())?;
                            if scope.is_some() {
                                repo::update(tx, &r, c.now())?;
                            }
                        }
                        Ok::<_, VaultError>(())
                    })?;
                }
                Ok(())
            })
            .await;
        match committed {
            Ok(()) => {
                tracing::info!(connection = %cx.id, "connection tokens refreshed");
                Ok(())
            }
            Err(e) => {
                tracing::error!(connection = %cx.id, "refreshed tokens could not be stored");
                if rotated {
                    // the old refresh token is spent and the new one is lost
                    Err(ProviderError::Auth(
                        "新令牌没能存进保险箱，需要重新授权".into(),
                    ))
                } else {
                    Err(ProviderError::Config(e.to_string()))
                }
            }
        }
    }

    /// Refreshes one connection now (`force`) or if due within the job margin.
    pub async fn refresh(&self, ws: &str, id: &str, force: bool) -> Result<(), ProviderError> {
        let cx = self.cx(ws, id).map_err(vault_err)?;
        let p = self
            .provider(&cx.adapter)
            .ok_or(ProviderError::Unsupported)?;
        let r = self.refresh_cx(p.as_ref(), &cx, JOB_MARGIN_MS, force).await;
        cx.report(match &r {
            Ok(()) => Health::Ok,
            Err(e) => Health::from(e),
        })
        .await;
        r
    }

    // ------------------------------------------------------------ health

    /// Records an outcome: `Ok` resets (and leaves `reauth`/`error`), `Auth` → `reauth` plus
    /// one open decision, `Failed` counts with backoff and → `error` after 3. Paused and revoked
    /// connections are left alone.
    pub async fn record(&self, ws: &str, id: &str, h: Health) {
        let (ws, id) = (ws.to_string(), id.to_string());
        let r = self.blocking(move |c| c.record_sync(&ws, &id, &h)).await;
        if r.is_err() {
            tracing::error!("could not record connection health");
        }
    }

    /// Changes a connection's `config` in place, for what an adapter learned while working (Feishu:
    /// a permission a call was refused for). `f` returns whether it changed anything; only then is
    /// the row written (without a revision bump) and a change published, so the 接入 page updates
    /// while the jobs that act on real changes don't run again. Paused and revoked connections are
    /// left alone.
    pub async fn update_config(
        &self,
        ws: &str,
        id: &str,
        f: impl FnOnce(&mut Value) -> bool + Send + 'static,
    ) {
        let (ws, id) = (ws.to_string(), id.to_string());
        let r = self
            .blocking(move |c| {
                let now = c.now();
                c.0.m.store.transaction(|tx| {
                    let Some(mut r) = repo::get(tx, &id)?.filter(|r| r.workspace_id == ws) else {
                        return Ok(());
                    };
                    if matches!(r.status.as_str(), "paused" | "revoked") || !f(&mut r.config) {
                        return Ok(());
                    }
                    // learned, not edited: no revision bump (see repo::set_config)
                    repo::set_config(tx, &id, &r.config, now)?;
                    tx.publish(ChangeEvent::Connection { id, ws });
                    Ok(())
                })
            })
            .await;
        if r.is_err() {
            tracing::error!("could not update connection config");
        }
    }

    /// Marks `reauth` every connection stored under another [`ConnectionProvider::config_version`]
    /// than its provider's now ([`CONFIG_VERSION_KEY`]; a provider replaced under the same id, or
    /// a new stored shape), with the provider's question; what runs for it stops. Run at startup,
    /// before connections run. Returns how many were marked.
    pub fn check_versions(&self) -> VaultResult<usize> {
        let now = self.now();
        self.0.m.store.transaction(|tx| {
            let mut n = 0;
            for mut r in repo::authorized(tx)? {
                let Some(p) = self.provider(&r.adapter) else {
                    continue;
                };
                let (stored, current) = (stored_version(&r.config), p.config_version());
                if stored == u64::from(current) {
                    continue;
                }
                tracing::info!(connection = %r.id, adapter = %r.adapter, stored, current, "connection stored under another config version: needs a new authorization");
                r.status = "reauth".into();
                r.health.last_error = steve_i18n::t!("modules.connections.config_version");
                r.health.error_at = now;
                repo::set_health(tx, &r.id, &r.status, &r.health, now)?;
                self.open_reauth(tx, &r)?;
                tx.publish(ChangeEvent::Connection {
                    id: r.id.clone(),
                    ws: r.workspace_id.clone(),
                });
                n += 1;
            }
            Ok::<_, VaultError>(n)
        })
    }

    fn record_sync(&self, ws: &str, id: &str, h: &Health) -> VaultResult<()> {
        let now = self.now();
        self.0.m.store.transaction(|tx| {
            let Some(mut r) = repo::get(tx, id)?.filter(|r| r.workspace_id == ws) else {
                return Ok(());
            };
            if matches!(r.status.as_str(), "paused" | "revoked") {
                return Ok(());
            }
            let before = (r.status.clone(), r.health.fails);
            match h {
                Health::Ok => {
                    r.health.last_ok = now;
                    r.health.fails = 0;
                    r.health.next_at = 0;
                    if r.status != "ok" {
                        r.status = "ok".into();
                        self.settle_decision(tx, ws, id, json!({"recovered": true}))?;
                    }
                }
                Health::Auth(msg) => {
                    r.health.last_error = msg.clone();
                    r.health.error_at = now;
                    r.status = "reauth".into();
                    self.open_reauth(tx, &r)?;
                }
                Health::Failed(msg) => {
                    r.health.last_error = msg.clone();
                    r.health.error_at = now;
                    r.health.fails += 1;
                    r.health.next_at = now + backoff(r.health.fails);
                    if r.health.fails >= FAILS_TO_ERROR && r.status == "ok" {
                        r.status = "error".into();
                    }
                }
            }
            repo::set_health(tx, id, &r.status, &r.health, now)?;
            if (r.status.clone(), r.health.fails) != before {
                if r.status != before.0 {
                    tracing::info!(connection = %id, status = %r.status, "connection status changed");
                }
                tx.publish(ChangeEvent::Connection {
                    id: id.into(),
                    ws: ws.into(),
                });
            }
            Ok(())
        })
    }

    /// The “需要重新授权” decision; at most one open per connection.
    fn open_reauth(&self, tx: &Tx, r: &ConnRow) -> VaultResult<()> {
        if base::decisions::open_for(tx, &r.workspace_id, SUBJECT, &r.id)?.is_some() {
            return Ok(());
        }
        let account = arepo::get(tx, &r.account_id)?
            .map(|a| {
                if a.display_name.is_empty() {
                    a.handle_display
                } else {
                    a.display_name
                }
            })
            .unwrap_or_default();
        let question = match self.provider(&r.adapter) {
            Some(p) => p.reauth_question(&account),
            None => format!("{account} 的授权过期了"),
        };
        base::decisions::open(
            tx,
            &NewDecision {
                workspace_id: r.workspace_id.clone(),
                kind: DecisionKind::Fail.code().into(),
                subject_type: SUBJECT.into(),
                subject_id: r.id.clone(),
                item_id: None,
                session_id: None,
                question,
                draft: None,
                options: Some(REAUTH_OPTIONS.map(String::from).to_vec()),
                requested_by: "steve".into(),
            },
        )?;
        Ok(())
    }

    /// `POST /connections/:id/test`: one health check now.
    pub async fn test(&self, who: &Caller, id: &str) -> VaultResult<ConnectionOut> {
        let viewer = self.viewer(who)?;
        let r = self.0.m.store.reader(|c| self.own(c, &viewer, id))?;
        let Some(p) = self.provider(&r.adapter) else {
            return Err(ApiError::rule(steve_i18n::t!(
                "modules.connections.not_installed",
                adapter = r.adapter.as_str()
            ))
            .into());
        };
        {
            let cx = self.cx_of(&r);
            let h = p.health(&cx).await;
            if h == Health::Ok
                && let Some(patch) = p.recheck(&cx).await
            {
                self.update_config(&r.workspace_id, &r.id, move |c| {
                    merge(c, &patch);
                    true
                })
                .await;
            }
            // a check you asked for also lifts a paused/errored backoff
            self.record(&r.workspace_id, &r.id, h).await;
        }
        self.get(who, id)
    }

    // ------------------------------------------------------------ jobs

    /// `vault.token_refresh`: OAuth connections expiring within 10 minutes, not in backoff.
    /// Returns how many were refreshed.
    pub async fn refresh_due(&self) -> usize {
        let now = self.now();
        let rows = match self.0.m.store.reader(repo::live) {
            Ok(r) => r,
            Err(_) => return 0,
        };
        let mut n = 0;
        for r in rows {
            let Some(p) = self.provider(&r.adapter) else {
                continue;
            };
            let due = p.token_kind() == TokenKind::Stored
                && r.health.next_at <= now
                && r.health
                    .expires_at
                    .is_some_and(|e| e - now <= JOB_MARGIN_MS);
            if due && self.refresh(&r.workspace_id, &r.id, false).await.is_ok() {
                n += 1;
            }
        }
        n
    }

    /// `vault.health`: every `ok`/`error` connection not in backoff. Returns how many were checked.
    pub async fn check_health(&self) -> usize {
        let now = self.now();
        let rows = match self.0.m.store.reader(repo::live) {
            Ok(r) => r,
            Err(_) => return 0,
        };
        let mut n = 0;
        for r in rows.into_iter().filter(|r| r.health.next_at <= now) {
            let Some(p) = self.provider(&r.adapter) else {
                continue;
            };
            // one provider that hangs must not hold up the rest of the pass
            let h = tokio::time::timeout(HEALTH_TIMEOUT, p.health(&self.cx_of(&r)))
                .await
                .unwrap_or_else(|_| Health::Failed("health check timed out".into()));
            self.record(&r.workspace_id, &r.id, h).await;
            n += 1;
        }
        n
    }

    /// Starts both jobs (every 5 and 15 minutes). The integrator calls this once the runtime is up.
    pub fn spawn_jobs(&self) -> tokio::task::JoinHandle<()> {
        let c = self.clone();
        tokio::spawn(async move {
            // two loops side by side: a long health pass must not delay a token refresh. A pass
            // that overruns its period starts the next one a full period later, not in a burst.
            let every = |mins: u64| {
                let mut i = tokio::time::interval(Duration::from_secs(mins * 60));
                i.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
                i
            };
            let (mut refresh, mut health) = (every(5), every(15));
            tokio::join!(
                async {
                    loop {
                        refresh.tick().await;
                        c.refresh_due().await;
                    }
                },
                async {
                    loop {
                        health.tick().await;
                        c.check_health().await;
                    }
                },
            );
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn conn(status: &str, caps: &[&str]) -> ConnRow {
        ConnRow {
            id: "con_1".into(),
            workspace_id: "default".into(),
            account_id: "acc_1".into(),
            adapter: "fake".into(),
            auth: "none".into(),
            granted_scopes: vec![],
            caps: caps.iter().map(|c| c.to_string()).collect(),
            credential_item: None,
            status: status.into(),
            health: HealthState::default(),
            config: Value::Null,
            last_sync: None,
            created_by: "usr_1".into(),
            revision: 1,
            created: 0,
            updated: 0,
        }
    }

    #[test]
    fn a_write_waits_while_paused_and_never_goes_out_revoked_or_without_its_capability() {
        let send = Some("mail.send");
        assert_eq!(may_write(&conn("ok", &["mail.send"]), send), Ok(()));
        assert_eq!(
            may_write(&conn("error", &["mail.send"]), send),
            Ok(()),
            "health only"
        );
        assert_eq!(may_write(&conn("ok", &[]), None), Ok(()));
        for s in WAITING {
            assert!(
                matches!(
                    may_write(&conn(s, &["mail.send"]), send),
                    Err(WriteRefusal::Later(_))
                ),
                "{s}"
            );
        }
        assert!(matches!(
            may_write(&conn("revoked", &["mail.send"]), send),
            Err(WriteRefusal::Never(_))
        ));
        assert!(matches!(
            may_write(&conn("ok", &["mail.read"]), send),
            Err(WriteRefusal::Never(_))
        ));
    }

    #[test]
    fn backoff_doubles_and_caps() {
        assert_eq!(backoff(1), 15 * MINUTE);
        assert_eq!(backoff(2), 30 * MINUTE);
        assert_eq!(backoff(3), 60 * MINUTE);
        assert_eq!(backoff(9), BACKOFF_MAX_MS);
        assert_eq!(backoff(100), BACKOFF_MAX_MS);
    }

    #[test]
    fn config_from_env() {
        let c = ConnectionsConfig::from_env(|k| match k {
            "STEVE_PUBLIC_URLS" => {
                Some("https://steve.example.com/, http://mini.local:8440".into())
            }
            "STEVE_HOME" => Some("/Users/user/.steve".into()),
            "HOME" => Some("/Users/user".into()),
            _ => None,
        });
        assert_eq!(
            c.public_urls,
            ["https://steve.example.com", "http://mini.local:8440"]
        );
        assert!(!c.allow_api_base, "never on the real ~/.steve");
        let t = ConnectionsConfig::from_env(|k| match k {
            "STEVE_HOME" => Some("/tmp/t".into()),
            "HOME" => Some("/Users/user".into()),
            _ => None,
        });
        assert!(t.allow_api_base);
        assert_eq!(t.public_urls, ["http://127.0.0.1:8440"]);
    }

    #[test]
    fn error_location_is_encoded() {
        assert_eq!(error_location("a b&c"), "/#/accounts?error=a+b%26c");
    }
}
