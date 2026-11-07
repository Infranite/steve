//! What a connection adapter implements ([`ConnectionProvider`])
//! and the types it exchanges with the framework. Adapters live in their own slices (Gmail/Outlook in
//! A3/E2, IMAP in E1, Feishu in F1/F6, GitHub in G1); the framework does the OAuth dance, keeps the
//! credential in the system vault, refreshes tokens and tracks health.

use async_trait::async_trait;
use serde::Serialize;
use serde_json::Value;

use super::cx::{ConnCx, StartCx};
use super::integration::Integration;
use super::oauth::{self, OAuthClient, OAuthSpec, TokenSet};

/// The capability codes. Adapters declare the subset they support.
pub const CAPS: [&str; 12] = [
    "msg.receive",
    "msg.send",
    "msg.send_as_user",
    "mail.read",
    "mail.draft",
    "mail.send",
    "cal.read",
    "cal.write",
    "directory.read",
    "repo.read",
    "repo.write",
    "events",
];

/// The `config` key naming the connection a connection is paired with (a sending-as-you connection
/// with the bot connection of the same app).
pub const PAIR_KEY: &str = "app_connection";

/// The `config` key holding the [`ConnectionProvider::config_version`] a connection was stored
/// under (absent: 1). Set by the framework only; a request's own value is dropped.
pub const CONFIG_VERSION_KEY: &str = "_v";

/// Which connections a provider's connections pair with ([`ConnectionProvider::pairing`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Pairing {
    /// The adapters a pair may be of.
    pub with: Vec<String>,
    /// A connection without a working pair does not run.
    pub required: bool,
}

/// The connection a connection is paired with, as it is now.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paired {
    pub id: String,
    pub adapter: String,
    /// It is usable: `ok` or `error`, not paused, revoked or waiting for a new authorization.
    pub ok: bool,
}

/// How a connection is authorized (`connections.auth`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AuthKind {
    /// Authorization code + PKCE (and optionally the device flow): [`ConnectionProvider::oauth`].
    #[serde(rename = "oauth")]
    OAuth,
    /// A form of [`FieldDef`]s posted to `POST /connections` (app secrets, PATs, app passwords).
    Credential,
    /// Created by its own module through [`super::Connections::connect`] (the GitHub App manifest
    /// and installation flow); not offered in the generic “new connection” dialog.
    Managed,
    /// No secret (a plain webhook source).
    None,
}

impl AuthKind {
    /// The `connections.auth` column (`managed` is stored as `credential`: it has one).
    pub fn column(self) -> &'static str {
        match self {
            Self::OAuth => "oauth",
            Self::Credential | Self::Managed => "credential",
            Self::None => "none",
        }
    }
}

/// Where [`ConnCx::access_token`] gets the token from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    /// OAuth tokens kept in the credential item, refreshed through the provider's token endpoint.
    Stored,
    /// Short-lived tokens the provider mints from its credential ([`ConnectionProvider::mint`]:
    /// GitHub installation tokens, Feishu tenant tokens), cached in memory until 5 minutes before
    /// they expire, never written anywhere.
    Minted,
    /// One credential field used as is (a PAT): its key.
    Field(&'static str),
    /// The connection has no bearer token (IMAP: use [`ConnCx::secret`]).
    None,
}

/// One field of a credential form. Secret fields go to the system vault, the others to
/// `connections.config` under `key`.
#[derive(Debug, Clone, Serialize)]
pub struct FieldDef {
    /// Stable key: the vault field label for secrets, the config key otherwise.
    pub key: &'static str,
    /// What the form shows (Chinese).
    pub label: &'static str,
    pub secret: bool,
    pub required: bool,
    /// `text|password|number|select`.
    #[serde(rename = "type")]
    pub kind: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hint: Option<&'static str>,
}

impl FieldDef {
    /// A required secret field.
    pub const fn secret(key: &'static str, label: &'static str) -> Self {
        Self {
            key,
            label,
            secret: true,
            required: true,
            kind: "password",
            default: None,
            hint: None,
        }
    }
    /// A required plain field (goes into `config`).
    pub const fn plain(key: &'static str, label: &'static str) -> Self {
        Self {
            key,
            label,
            secret: false,
            required: true,
            kind: "text",
            default: None,
            hint: None,
        }
    }
    pub const fn optional(mut self) -> Self {
        self.required = false;
        self
    }
    pub const fn with_default(mut self, v: &'static str) -> Self {
        self.default = Some(v);
        self
    }
    pub const fn with_kind(mut self, k: &'static str) -> Self {
        self.kind = k;
        self
    }
    pub const fn with_hint(mut self, h: &'static str) -> Self {
        self.hint = Some(h);
        self
    }
}

/// Whose account a new connection is (only these may have one).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Owner {
    /// The connecting user's “我” (OAuth user tokens, PATs, mailboxes).
    Me,
    /// The workspace's own organization (app bots, GitHub Apps).
    SelfOrg,
    /// A team party.
    Team(String),
    /// A bot party of its own, named after the account (an app bot is not a person).
    /// Its 负责人 is the person the provider names ([`AccountInfo::steward_hint`], source `sync`),
    /// else the connecting user (source `connect`).
    Bot,
}

/// A person the provider names as responsible for the account (Feishu's app owner): a handle of
/// `system`, looked up among the known accounts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StewardHint {
    pub system: String,
    pub handle: String,
}

/// What `verify` learned about the account behind a credential.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct AccountInfo {
    /// The primary handle, before normalization (email, `union_id`, `app_id`, login).
    pub handle: String,
    pub display_name: String,
    /// `accounts.kind` (`personal`, `app_bot`, `work_email`…).
    pub kind: String,
    pub owner: Owner,
    /// Handles valid only under this connection (Feishu `open_id`s): written to `account_handles`
    /// with the connection id.
    pub extra_handles: Vec<String>,
    /// Merged into `connections.config` (non-secret facts: tenant, installation id, open_id).
    pub config: Value,
    /// For [`Owner::Bot`]: who the provider says is responsible.
    pub steward_hint: Option<StewardHint>,
    /// The same identity's handles in other systems, as `(system, handle)` (an email next to an
    /// internal user id): recorded as accounts of the same owner when nobody holds them yet, and
    /// claimed into “我” with the primary handle ([`ConnectionProvider::claims_self`]).
    pub aliases: Vec<(String, String)>,
}

impl AccountInfo {
    pub fn new(handle: impl Into<String>, display_name: impl Into<String>) -> Self {
        Self {
            handle: handle.into(),
            display_name: display_name.into(),
            kind: "personal".into(),
            owner: Owner::Me,
            extra_handles: Vec::new(),
            config: Value::Null,
            steward_hint: None,
            aliases: Vec::new(),
        }
    }

    /// One more handle of the same identity in another system.
    pub fn alias(mut self, system: &str, handle: &str) -> Self {
        let (system, handle) = (system.trim(), handle.trim());
        if !system.is_empty()
            && !handle.is_empty()
            && !self.aliases.iter().any(|(s, h)| s == system && h == handle)
        {
            self.aliases.push((system.to_string(), handle.to_string()));
        }
        self
    }

    pub fn kind(mut self, kind: impl Into<String>) -> Self {
        self.kind = kind.into();
        self
    }

    pub fn owner(mut self, owner: Owner) -> Self {
        self.owner = owner;
        self
    }

    /// A handle valid only under this connection ([`Self::extra_handles`]).
    pub fn extra_handle(mut self, h: impl Into<String>) -> Self {
        let h = h.into();
        if !h.trim().is_empty() && !self.extra_handles.contains(&h) {
            self.extra_handles.push(h);
        }
        self
    }

    pub fn config(mut self, config: Value) -> Self {
        self.config = config;
        self
    }

    pub fn steward_hint(mut self, hint: StewardHint) -> Self {
        self.steward_hint = Some(hint);
        self
    }
}

/// What went wrong talking to a provider. Messages are short Chinese summaries meant for the user;
/// they must never carry a secret (adapters: don't echo tokens or response bodies that may hold one).
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ProviderError {
    /// The provider rejected the credential (invalid_grant, 401): the connection needs `reauth`.
    #[error("{0}")]
    Auth(String),
    /// The provider answered but refused for another reason (missing permission, wrong account).
    #[error("{0}")]
    Rejected(String),
    /// Network, timeout, 5xx, rate limit: counts toward `error` with backoff.
    #[error("{0}")]
    Transient(String),
    /// Steve's side is not set up (no OAuth client configured, vault locked).
    #[error("{0}")]
    Config(String),
    #[error("这个接入方式不支持这个操作")]
    Unsupported,
}

/// A health check's outcome.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Health {
    Ok,
    /// Authentication failed: `reauth` and a decision.
    Auth(String),
    /// Anything else: `error` after 3 in a row.
    Failed(String),
}

impl From<&ProviderError> for Health {
    fn from(e: &ProviderError) -> Self {
        match e {
            ProviderError::Auth(m) => Self::Auth(m.clone()),
            e => Self::Failed(e.to_string()),
        }
    }
}

/// A token minted by [`ConnectionProvider::mint`]. No `Debug`/`Clone` of the value.
pub struct Minted {
    pub token: zeroize::Zeroizing<String>,
    /// Unix ms.
    pub expires_at: i64,
}

/// One kind of connection (an adapter). Registered once at startup with
/// [`super::ConnectionsBuilder::provider`]; the other modules' adapter traits (`MailAdapter`,
/// channels, directory) extend it.
#[async_trait]
pub trait ConnectionProvider: Send + Sync + 'static {
    /// `connections.adapter`: `gmail`, `outlook`, `imap`, `feishu_app`, `feishu_user`, `github_app`…
    fn id(&self) -> &str;
    /// The `systems.id` of its accounts (`gmail`, `mail`, `feishu`, `feishu_app`, `github`).
    fn system(&self) -> &str;
    /// Shown in the new-connection dialog.
    fn name(&self) -> &str;
    /// The capabilities it can offer at most.
    fn caps(&self) -> &[&'static str];
    fn auth(&self) -> AuthKind;

    /// The OAuth endpoints, for [`AuthKind::OAuth`].
    fn oauth(&self) -> Option<&OAuthSpec> {
        None
    }
    /// The credential form, for [`AuthKind::Credential`] (and the secrets a managed one stores).
    fn fields(&self) -> &[FieldDef] {
        &[]
    }
    fn token_kind(&self) -> TokenKind {
        match self.auth() {
            AuthKind::OAuth => TokenKind::Stored,
            _ => TokenKind::None,
        }
    }

    /// The capabilities the granted scopes allow (`available_caps`). Default: all of [`Self::caps`].
    fn available_caps(&self, granted_scopes: &[String]) -> Vec<String> {
        let _ = granted_scopes;
        self.caps().iter().map(|c| c.to_string()).collect()
    }

    /// How it appears on the 接入 page: its provider, whose identity a connection is, and the
    /// capability areas it supports today or plans. Default: provider = [`Self::system`], one
    /// supported area per cap family of [`Self::caps`].
    fn integration(&self) -> Integration {
        Integration::from_caps(self.system(), self.name(), self.caps())
    }

    /// Extra, provider-specific facts for the 接入 page (`ConnectionInfo.detail`), from the
    /// connection's `config` and the computed area states: e.g. Feishu's app id, the console links
    /// that preselect the missing permissions. Never secrets. Default: none.
    fn integration_detail(
        &self,
        config: &Value,
        caps: &[super::integration::CapState],
    ) -> Option<Value> {
        let _ = (config, caps);
        None
    }

    /// What a check you asked for (`POST /connections/:id/test`) learned besides health: a patch
    /// merged into `config` (Feishu: the permission check again, after you enabled scopes in the
    /// console). Default: nothing.
    async fn recheck(&self, cx: &ConnCx) -> Option<Value> {
        let _ = cx;
        None
    }

    /// The question of the `reauth` decision.
    fn reauth_question(&self, account: &str) -> String {
        format!("{account} 的授权过期了")
    }

    /// The OAuth client for a flow or a refresh. Default: `STEVE_OAUTH_<provider>_CLIENT_ID/SECRET`.
    /// `cx.config` is the start request's `config` (flows) or the connection's (refresh); Feishu
    /// user OAuth overrides this to use its app connection's App ID and Secret.
    async fn oauth_client(&self, cx: &StartCx<'_>) -> Result<Option<OAuthClient>, ProviderError> {
        Ok(self.oauth().and_then(|s| cx.env_client(s)))
    }

    /// Exchanges a refresh token. Default: the generic refresh grant of [`Self::oauth`].
    async fn refresh_tokens(
        &self,
        http: &reqwest::Client,
        client: &OAuthClient,
        refresh_token: &str,
    ) -> Result<TokenSet, ProviderError> {
        let spec = self.oauth().ok_or(ProviderError::Unsupported)?;
        oauth::refresh(http, spec, client, refresh_token).await
    }

    /// [`Self::refresh_tokens`] for the connection `cx` (a provider with hosts per connection, e.g.
    /// Feishu / Lark, picks them from `cx.config`). Default: [`Self::refresh_tokens`].
    async fn refresh_tokens_for(
        &self,
        cx: &ConnCx,
        http: &reqwest::Client,
        client: &OAuthClient,
        refresh_token: &str,
    ) -> Result<TokenSet, ProviderError> {
        let _ = cx;
        self.refresh_tokens(http, client, refresh_token).await
    }

    /// Who the credential belongs to. Called right after the credential is stored, before the
    /// account and connection rows exist (`cx.id` is already the connection's id for new ones).
    async fn verify(&self, cx: &ConnCx) -> Result<AccountInfo, ProviderError>;

    /// The periodic check. Default: `verify` succeeds.
    async fn health(&self, cx: &ConnCx) -> Health {
        match self.verify(cx).await {
            Ok(_) => Health::Ok,
            Err(e) => Health::from(&e),
        }
    }

    /// A fresh short-lived token, for [`TokenKind::Minted`].
    async fn mint(&self, cx: &ConnCx) -> Result<Minted, ProviderError> {
        let _ = cx;
        Err(ProviderError::Unsupported)
    }

    /// The connections this provider's connections pair with (`config.app_connection`, [`PAIR_KEY`]).
    /// Default: none.
    fn pairing(&self) -> Option<Pairing> {
        None
    }

    /// The `config` of every connect path (create, the OAuth and device starts, a new credential,
    /// a re-authorization, a bind), before anything is verified or stored. Validate what the
    /// provider will reach with it: endpoints (scheme, a host allowlist from [`StartCx::var`], no
    /// userinfo or secrets in the query). `Err` refuses the request (`400`). Default: accepted.
    fn check_config(&self, cx: &StartCx<'_>, config: &Value) -> Result<(), ProviderError> {
        let _ = (cx, config);
        Ok(())
    }

    /// Whether a verified connection may claim contacts into “我”: before its account is attached,
    /// an unconfirmed contact already holding its primary handle (or an extra handle, or an alias)
    /// is merged into the connecting user's “我”. Default: a `self` identity
    /// ([`Integration::identity`]) authorized by OAuth (code or device flow), where the provider
    /// proved who signed in. A [`AuthKind::Credential`] provider returning `true` attests
    /// [`AccountInfo::handle`] itself (from its paired connection, [`ConnCx::paired`]).
    fn claims_self(&self) -> bool {
        self.auth() == AuthKind::OAuth && self.integration().identity == "self"
    }

    /// The shape of what this provider stores (config and credential). Bump it when that shape
    /// changes, or when a provider replaces another under the same id: a connection stored under
    /// another version ([`CONFIG_VERSION_KEY`], absent = 1) is marked `reauth` at startup, with
    /// [`Self::reauth_question`], and a new authorization stores the current one. Default: 1.
    fn config_version(&self) -> u32 {
        1
    }

    /// Revokes at the provider on disconnect (best effort). Default: RFC 7009 at the spec's
    /// `revoke_url` when there is one, else nothing.
    async fn revoke(&self, cx: &ConnCx) -> Result<(), ProviderError> {
        match self.oauth() {
            Some(spec) if spec.revoke_url.is_some() => oauth::revoke(cx, spec).await,
            _ => Ok(()),
        }
    }
}
