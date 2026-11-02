//! The two Feishu connections of one 企业自建应用 (feishu-lark-cli.md §2.1), as
//! [`ConnectionProvider`]s of the connections framework (A2):
//!
//! - [`FeishuApp`] (`feishu_app`): the bot. Credential form App ID + App Secret (+ optional
//!   Verification Token and Encrypt Key for the webhook fallback); the secrets go to the system
//!   vault, App ID and domain to `config`. Tenant tokens are minted from them
//!   (`tenant_access_token/internal`) and cached by the framework until 5 minutes before expiry.
//! - [`FeishuUser`] (`feishu_user`): you, through the same app. OAuth authorization code + PKCE with
//!   the app's App ID/Secret as client; refresh tokens are single-use (the framework commits the new
//!   set before using it); tokens live in the vault, not a keychain, no DPoP. The device flow of
//!   lark-cli is [`DeviceFlows`] (its endpoints want Basic auth to start and the secret in the form
//!   to poll, which one generic [`OAuthSpec`] can't express).

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use axum::extract::Path;
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};
use steve_modules::accounts::connections::cx::{
    ACCESS_TOKEN, EXPIRES_AT, REFRESH_EXPIRES_AT, REFRESH_TOKEN, SCOPE,
};
use steve_modules::accounts::connections::integration::{
    CapSpec, CapState, Integration, ScopeSpec,
};
use steve_modules::accounts::connections::oauth::{self, DevicePoll};
use steve_modules::accounts::connections::{
    AccountInfo, AuthKind, ClientAuth, ConnCx, ConnectionProvider, Connections, DeviceOut,
    FieldDef, Health, Minted, OAuthClient, OAuthSpec, ProviderError, StartCx, TokenKind, TokenSet,
    TokenStyle,
};
use steve_modules::{ApiError, Caller, Modules};
use zeroize::Zeroizing;

use super::client::{self, Domain, FeishuClient, short};
use super::user_oauth::{FeishuIds, SELF_CLAIM, SelfClaims, claim_self_for};
use super::verify;
use steve_modules::accounts::AccountHooks;
use steve_modules::contacts::PartyHooks;

/// `connections.adapter` and `systems.id` of the bot.
pub const APP_ADAPTER: &str = "feishu_app";
pub const APP_SYSTEM: &str = "feishu_app";
/// `connections.adapter` of the user connection; its accounts are system `feishu` (`union_id`).
pub const USER_ADAPTER: &str = "feishu_user";
pub const USER_SYSTEM: &str = "feishu";

/// Credential keys of the bot connection (vault field labels).
pub const APP_SECRET: &str = "app_secret";
pub const VERIFICATION_TOKEN: &str = "verification_token";
pub const ENCRYPT_KEY: &str = "encrypt_key";

/// Wrong App ID / App Secret (10014), invalid app_id (10003).
const BAD_APP_CODES: &[i64] = &[10003, 10014];

// ---------------------------------------------------------------------------------------------
// Tenant token.

/// `POST /open-apis/auth/v3/tenant_access_token/internal {app_id, app_secret}` →
/// `{tenant_access_token, expire}` (flat). `now` is Unix ms.
pub async fn mint_tenant_token(
    http: &reqwest::Client,
    base: &str,
    app_id: &str,
    app_secret: &str,
    now: i64,
) -> Result<Minted, ProviderError> {
    let body = Zeroizing::new(json!({"app_id": app_id, "app_secret": app_secret}).to_string());
    let url = format!(
        "{}/open-apis/auth/v3/tenant_access_token/internal",
        base.trim_end_matches('/')
    );
    client::guard_url(&url).map_err(|e| ProviderError::Config(e.to_string()))?;
    let res = http
        .post(url)
        .header(
            reqwest::header::CONTENT_TYPE,
            "application/json; charset=utf-8",
        )
        .body(body.to_string())
        .timeout(Duration::from_secs(30))
        .send()
        .await
        .map_err(|e| ProviderError::Transient(format!("飞书暂时连不上（{}）", short(&e))))?;
    let status = res.status().as_u16();
    let v: Value = res.json().await.unwrap_or(Value::Null);
    let code = v.get("code").and_then(Value::as_i64);
    match code {
        Some(0) => {}
        Some(c) if BAD_APP_CODES.contains(&c) => {
            return Err(ProviderError::Auth(format!(
                "App ID 或 App Secret 不对（{c}）"
            )));
        }
        _ if status == 429 || status >= 500 => {
            return Err(ProviderError::Transient(format!(
                "飞书暂时不可用（HTTP {status}）"
            )));
        }
        Some(c) => {
            return Err(ProviderError::Rejected(format!(
                "飞书没有给出应用令牌（{c}）"
            )));
        }
        None => return Err(ProviderError::Transient("飞书的回答看不懂".into())),
    }
    let token = v
        .get("tenant_access_token")
        .and_then(Value::as_str)
        .filter(|t| !t.is_empty())
        .ok_or_else(|| ProviderError::Transient("飞书没有给出应用令牌".into()))?;
    let expire = v
        .get("expire")
        .and_then(Value::as_i64)
        .unwrap_or(7200)
        .max(0);
    Ok(Minted {
        token: Zeroizing::new(token.to_string()),
        expires_at: now + expire * 1000,
    })
}

// ---------------------------------------------------------------------------------------------
// The 接入 page.

/// The provider both connections belong to.
pub const PROVIDER: &str = "feishu";
pub const PROVIDER_NAME: &str = "飞书 / Lark";

/// What Feishu could do for Steve beyond messages, not built yet (规划中).
const PLANNED: [(&str, &str); 6] = [
    ("docs", "读写云文档和知识库"),
    ("sheets", "电子表格、多维表格"),
    ("calendar", "日程和会议"),
    ("tasks", "飞书任务"),
    ("approval", "审批单的提醒和处理"),
    ("minutes", "会议妙记的纪要和待办"),
];

fn scopes(
    rows: &[verify::Permission],
    pick: impl Fn(verify::Level) -> Option<bool>,
) -> Vec<ScopeSpec> {
    rows.iter()
        .filter_map(|p| pick(p.level).map(|req| ScopeSpec::new(p.scope, p.purpose, req)))
        .collect()
}

/// One identity of the app: its supported areas, then the planned ones.
fn integration(identity: &str, mut caps: Vec<CapSpec>) -> Integration {
    caps.extend(PLANNED.iter().map(|(a, n)| CapSpec::planned(a, n)));
    Integration {
        provider: PROVIDER.into(),
        provider_name: PROVIDER_NAME.into(),
        identity: identity.into(),
        capabilities: caps,
    }
}

/// The bot: messages in and out (the permission table's message rows), the address book.
pub fn app_integration() -> Integration {
    use verify::Level;
    integration(
        "bot",
        vec![
            CapSpec::supported("channel", &["msg.receive", "msg.send"])
                .with_note("机器人在群里和私聊里收发消息")
                .with_scopes(scopes(verify::BOT_PERMISSIONS, |l| match l {
                    Level::Required => Some(true),
                    Level::Suggested => Some(false),
                    _ => None,
                })),
            CapSpec::supported("directory", &["directory.read"])
                .with_note(
                    "同步部门和成员到联系人；成员名字需要 im:chat.members:read（群成员）或 \
                     contact:user.base:readonly（通讯录），公司名称需要 tenant:tenant:readonly",
                )
                .with_scopes(scopes(verify::BOT_PERMISSIONS, |l| match l {
                    Level::Directory => Some(true),
                    Level::Names => Some(false),
                    _ => None,
                })),
        ],
    )
}

/// The scope for sending as you. Some tenants don't let members request it.
pub const SEND_AS_USER_SCOPE: &str = "im:message.send_as_user";
/// Shown under 以你的名义发送.
pub const SEND_AS_USER_NOTE: &str = "以你本人身份发出你批准的回复。有些飞书组织不开放申请这个权限；\
     没有它，Steve 不会替你起草或回复，只记录，需要时在 Steve 里问你";

/// You: your chats read about once a minute, replies sent as you after your approval.
pub fn user_integration() -> Integration {
    use verify::Level;
    integration(
        "self",
        vec![
            CapSpec::supported("channel", &["msg.receive"])
                .with_note(
                    "读你本人的私聊和群消息；成员名字先用你的身份查 \
                     （im:chat.members:read 群成员、contact:user.base:readonly 通讯录）",
                )
                .with_scopes(
                    scopes(verify::USER_PERMISSIONS, |l| match l {
                        Level::Required => Some(true),
                        Level::Optional | Level::Names => Some(false),
                        _ => None,
                    })
                    .into_iter()
                    .filter(|s| s.scope != SEND_AS_USER_SCOPE)
                    .collect(),
                ),
            // its own area: not every tenant lets members request it, and without it nothing is
            // drafted or sent as you (domain-model.md §5)
            CapSpec::supported("send_as_you", &["msg.send_as_user"])
                .with_note(SEND_AS_USER_NOTE)
                .with_scopes(vec![ScopeSpec::new(
                    SEND_AS_USER_SCOPE,
                    "以你本人身份发出你批准的回复",
                    true,
                )]),
        ],
    )
}

// ---------------------------------------------------------------------------------------------
// The bot.

/// The bot connection (`feishu_app`).
pub struct FeishuApp {
    fields: Vec<FieldDef>,
}

impl Default for FeishuApp {
    fn default() -> Self {
        Self::new()
    }
}

impl FeishuApp {
    pub fn new() -> Self {
        Self {
            fields: vec![
                FieldDef::plain("app_id", "App ID").with_hint("开发者后台 → 凭证与基础信息"),
                FieldDef::secret(APP_SECRET, "App Secret"),
                FieldDef::secret(VERIFICATION_TOKEN, "Verification Token")
                    .optional()
                    .with_hint("只有用 webhook 接收事件时需要；长连接不需要"),
                FieldDef::secret(ENCRYPT_KEY, "Encrypt Key")
                    .optional()
                    .with_hint("只有用 webhook 接收事件时需要；长连接不需要"),
                FieldDef::plain("domain", "域名")
                    .with_kind("select")
                    .with_default("feishu")
                    .with_hint("feishu = 飞书，lark = Lark"),
            ],
        }
    }
}

#[async_trait]
impl ConnectionProvider for FeishuApp {
    fn id(&self) -> &str {
        APP_ADAPTER
    }
    fn system(&self) -> &str {
        APP_SYSTEM
    }
    fn name(&self) -> &str {
        "飞书应用机器人"
    }
    fn caps(&self) -> &[&'static str] {
        &["msg.receive", "msg.send", "directory.read", "events"]
    }
    fn integration(&self) -> Integration {
        app_integration()
    }
    fn auth(&self) -> AuthKind {
        AuthKind::Credential
    }
    fn fields(&self) -> &[FieldDef] {
        &self.fields
    }
    fn token_kind(&self) -> TokenKind {
        TokenKind::Minted
    }

    async fn mint(&self, cx: &ConnCx) -> Result<Minted, ProviderError> {
        let app_id = cx
            .config_str("app_id")
            .ok_or_else(|| ProviderError::Config("缺少 App ID".into()))?;
        let secret = cx.secret(APP_SECRET).await?;
        mint_tenant_token(
            cx.http(),
            &client::api_base(cx.config()),
            app_id,
            &secret,
            cx.connections().modules().clock.now(),
        )
        .await
    }

    async fn verify(&self, cx: &ConnCx) -> Result<AccountInfo, ProviderError> {
        verify::verify_app(cx).await
    }

    fn integration_detail(&self, config: &Value, _caps: &[CapState]) -> Option<Value> {
        verify::app_detail(config)
    }

    async fn recheck(&self, cx: &ConnCx) -> Option<Value> {
        verify::recheck_app(cx).await
    }

    /// The token still mints and the bot still answers (one `bot/v3/info`).
    async fn health(&self, cx: &ConnCx) -> Health {
        match verify::bot_info(&FeishuClient::for_cx(cx), cx).await {
            Ok(_) => Health::Ok,
            Err(e) => Health::from(&ProviderError::from(e)),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// You.

/// The user scopes asked for in one go (feishu-lark-cli.md §2.4; `search:message` stays optional).
pub const USER_SCOPES: &[&str] = &[
    "offline_access",
    "im:message",
    "im:message:readonly",
    "im:message.p2p_msg:get_as_user",
    "im:message.group_msg:get_as_user",
    "im:message.send_as_user",
    "im:chat:read",
];

/// The user connection (`feishu_user`).
pub struct FeishuUser {
    /// Feishu's (the code flow and every connection without `domain`).
    spec: OAuthSpec,
    /// Open-platform host of `user_info` and the token endpoint. The user connection's own config
    /// has no `api_base`: its hosts are the provider's.
    open: String,
    /// Accounts host of the device flow.
    accounts: String,
    /// The same two for Lark (a connection with `domain: "lark"`, made through a Lark bot app).
    lark_open: String,
    lark_accounts: String,
    /// Claim tickets of the F6 start routes (who started a flow; see [`super::user_oauth`]).
    claims: SelfClaims,
    /// The contacts hooks a [`super::user_oauth::claim_self`] merge runs (accounts move along).
    hooks: Vec<Arc<dyn PartyHooks>>,
}

impl FeishuUser {
    /// On the real hosts of `domain`.
    pub fn new(domain: Domain) -> Self {
        let _ = domain;
        Self::on(
            Domain::Feishu.open_base(),
            Domain::Feishu.accounts_base(),
            Domain::Lark.open_base(),
            Domain::Lark.accounts_base(),
        )
    }

    /// Every endpoint on one base (the fake Feishu); Lark's under `{base}/lark`.
    pub fn with_base(base: &str) -> Self {
        let b = base.trim_end_matches('/');
        let lark = format!("{b}/lark");
        Self::on(b, b, &lark, &lark)
    }

    fn on(open: &str, accounts: &str, lark_open: &str, lark_accounts: &str) -> Self {
        let (open, accounts) = (open.trim_end_matches('/'), accounts.trim_end_matches('/'));
        let mut spec = OAuthSpec::new(
            "FEISHU",
            "飞书",
            &format!("{accounts}/open-apis/authen/v1/authorize"),
            &format!("{open}/open-apis/authen/v2/oauth/token"),
            USER_SCOPES,
        );
        spec.style = TokenStyle::Json;
        spec.client_auth = ClientAuth::Body;
        Self {
            spec,
            open: open.to_string(),
            accounts: accounts.to_string(),
            lark_open: lark_open.trim_end_matches('/').to_string(),
            lark_accounts: lark_accounts.trim_end_matches('/').to_string(),
            claims: SelfClaims::default(),
            hooks: vec![Arc::new(AccountHooks)],
        }
    }

    /// The contacts [`PartyHooks`] for the merge of a contact the bot made for you into “我”: pass
    /// the same list the contacts router gets (default: the accounts module's alone).
    pub fn with_party_hooks(mut self, hooks: Vec<Arc<dyn PartyHooks>>) -> Self {
        self.hooks = hooks;
        self
    }

    /// The claim tickets, for [`super::user_oauth::user_routes`].
    pub fn claims(&self) -> &SelfClaims {
        &self.claims
    }

    /// A client on the user endpoints' host, for calls with this connection's user token (the
    /// user connection's config has no `api_base`: its hosts are the provider's).
    pub fn user_client(&self, cx: &ConnCx) -> FeishuClient {
        let (open, _) = self.hosts(Domain::from_config(cx.config()));
        FeishuClient::new(cx.http().clone(), open.to_string())
    }

    /// `(open, accounts)` of a brand.
    fn hosts(&self, d: Domain) -> (&str, &str) {
        match d {
            Domain::Feishu => (&self.open, &self.accounts),
            Domain::Lark => (&self.lark_open, &self.lark_accounts),
        }
    }

    /// The OAuth spec on a brand's hosts (authorize, token).
    fn spec_for(&self, d: Domain) -> OAuthSpec {
        let (open, accounts) = self.hosts(d);
        let mut s = self.spec.clone();
        s.authorize_url = format!("{accounts}/open-apis/authen/v1/authorize");
        s.token_url = format!("{open}/open-apis/authen/v2/oauth/token");
        s
    }

    fn client(&self, cx: &ConnCx) -> FeishuClient {
        self.user_client(cx)
    }

    /// The token and device endpoints are requested by the connections framework, not by
    /// [`FeishuClient`]: the in-process test belt is applied here (`STEVE_FORBID_REAL_HOSTS` is
    /// checked by `oauth` itself before each request).
    fn guard_hosts(&self) {
        client::guard_real_feishu(&self.spec.token_url);
        client::guard_real_feishu(&self.accounts);
        client::guard_real_feishu(&self.lark_open);
        client::guard_real_feishu(&self.lark_accounts);
    }

    /// Device authorization: Basic `app_id:app_secret`, form `client_id`, `scope`.
    fn device_start_spec(&self) -> OAuthSpec {
        self.device_start_spec_on(Domain::Feishu)
    }

    #[cfg(test)]
    fn device_poll_spec(&self) -> OAuthSpec {
        self.device_poll_spec_on(Domain::Feishu)
    }

    fn device_start_spec_on(&self, d: Domain) -> OAuthSpec {
        let (_, accounts) = self.hosts(d);
        let mut s = self.spec_for(d);
        s.device_url = Some(format!("{accounts}/oauth/v1/device_authorization"));
        s.style = TokenStyle::Form;
        s.client_auth = ClientAuth::Basic;
        s
    }

    /// Device token polling: form with `client_id` and `client_secret`.
    fn device_poll_spec_on(&self, d: Domain) -> OAuthSpec {
        let (_, accounts) = self.hosts(d);
        let mut s = self.spec_for(d);
        s.device_token_url = Some(format!("{accounts}/oauth/v3/token"));
        s.style = TokenStyle::Form;
        s.client_auth = ClientAuth::Body;
        s
    }
}

/// The brand of the bot connection `id` in `ws` (its `config.domain`).
pub fn app_domain(conns: &Connections, ws: &str, id: &str) -> Result<Domain, ProviderError> {
    let c = conns
        .modules()
        .store
        .reader(|c| steve_modules::accounts::connections::repo::get(c, id))
        .map_err(|e| ProviderError::Config(e.to_string()))?
        .filter(|c| c.workspace_id == ws)
        .ok_or_else(|| ProviderError::Config("没有这个机器人连接".into()))?;
    Ok(Domain::from_config(&c.config))
}

/// The App ID and Secret of the bot connection `id` in `ws`, as an OAuth client.
pub async fn app_client(
    conns: &Connections,
    ws: &str,
    id: &str,
) -> Result<OAuthClient, ProviderError> {
    let app = conns
        .cx(ws, id)
        .map_err(|_| ProviderError::Config("飞书应用连接不见了".into()))?;
    if app.adapter() != APP_ADAPTER {
        return Err(ProviderError::Config("这不是飞书应用连接".into()));
    }
    let app_id = app
        .config_str("app_id")
        .ok_or_else(|| ProviderError::Config("飞书应用连接缺少 App ID".into()))?
        .to_string();
    Ok(OAuthClient {
        id: app_id,
        secret: app.secret(APP_SECRET).await?,
    })
}

#[async_trait]
impl ConnectionProvider for FeishuUser {
    fn id(&self) -> &str {
        USER_ADAPTER
    }
    fn system(&self) -> &str {
        USER_SYSTEM
    }
    fn name(&self) -> &str {
        "飞书 · 你本人"
    }
    fn caps(&self) -> &[&'static str] {
        &["msg.receive", "msg.send_as_user"]
    }
    fn integration(&self) -> Integration {
        user_integration()
    }
    fn auth(&self) -> AuthKind {
        AuthKind::OAuth
    }
    fn oauth(&self) -> Option<&OAuthSpec> {
        Some(&self.spec)
    }
    fn reauth_question(&self, _account: &str) -> String {
        "飞书需要重新授权".into()
    }
    fn integration_detail(&self, config: &Value, _caps: &[CapState]) -> Option<Value> {
        verify::user_detail(config)
    }

    /// Polling your chats needs a read scope, the per-kind `get_as_user` and `im:chat:read`;
    /// sending as you needs `im:message` and `im:message.send_as_user`.
    fn available_caps(&self, granted: &[String]) -> Vec<String> {
        let has = |s: &str| granted.iter().any(|g| g == s);
        let mut caps = Vec::new();
        if (has("im:message") || has("im:message:readonly"))
            && (has("im:message.p2p_msg:get_as_user") || has("im:message.group_msg:get_as_user"))
            && has("im:chat:read")
        {
            caps.push("msg.receive".to_string());
        }
        if has("im:message") && has("im:message.send_as_user") {
            caps.push("msg.send_as_user".to_string());
        }
        caps
    }

    /// The bot connection's App ID and Secret (`config.app_connection`), not `STEVE_OAUTH_FEISHU_*`.
    async fn oauth_client(&self, cx: &StartCx<'_>) -> Result<Option<OAuthClient>, ProviderError> {
        self.guard_hosts();
        let Some(app) = cx.config.get("app_connection").and_then(Value::as_str) else {
            return Ok(None);
        };
        // the browser code flow runs on Feishu's hosts only: a Lark app's credential never goes
        // there (a Lark connection, made by device code, carries `domain: "lark"` for its refresh)
        if app_domain(cx.connections(), cx.ws, app)? == Domain::Lark
            && Domain::from_config(cx.config) != Domain::Lark
        {
            return Err(ProviderError::Config(
                "Lark 的应用暂时不支持网页回调授权，请用设备码授权".into(),
            ));
        }
        app_client(cx.connections(), cx.ws, app).await.map(Some)
    }

    /// `user_info`, the granted scopes; and when the flow carries a claim ticket, the contact the
    /// bot already made for this `union_id` merged into “我” first (F6).
    async fn verify(&self, cx: &ConnCx) -> Result<AccountInfo, ProviderError> {
        let mut info = verify::verify_user(cx, &self.client(cx)).await?;
        if let Some(ticket) = cx.config_str(SELF_CLAIM) {
            let m = cx.connections().modules().clone();
            match self.claims.take(ticket, cx.ws(), m.clock.now()) {
                Some(user) => {
                    let ids = FeishuIds {
                        union_id: info.handle.clone(),
                        open_id: info.extra_handles.first().cloned().unwrap_or_default(),
                    };
                    let r =
                        claim_self_for(m, self.hooks.clone(), cx.ws().to_string(), user, ids).await;
                    if let Err(e) = r {
                        // the connect then reports who holds the account
                        tracing::warn!(error = %e, "feishu: could not merge your contact into 我");
                    }
                }
                None => tracing::info!("feishu: claim ticket unknown or expired, no merge"),
            }
            // spent: don't keep it in the connection's config
            info.config[SELF_CLAIM] = Value::Null;
        }
        Ok(info)
    }

    async fn refresh_tokens(
        &self,
        http: &reqwest::Client,
        client: &OAuthClient,
        refresh_token: &str,
    ) -> Result<TokenSet, ProviderError> {
        self.guard_hosts();
        oauth::refresh(http, &self.spec, client, refresh_token).await
    }

    /// On the connection's brand (`config.domain`).
    async fn refresh_tokens_for(
        &self,
        cx: &ConnCx,
        http: &reqwest::Client,
        client: &OAuthClient,
        refresh_token: &str,
    ) -> Result<TokenSet, ProviderError> {
        self.guard_hosts();
        let spec = self.spec_for(Domain::from_config(cx.config()));
        oauth::refresh(http, &spec, client, refresh_token).await
    }

    async fn health(&self, cx: &ConnCx) -> Health {
        match verify::user_info(&self.client(cx), cx).await {
            Ok(_) => Health::Ok,
            Err(e) => Health::from(&ProviderError::from(e)),
        }
    }
}

// ---------------------------------------------------------------------------------------------
// Device flow (lark-cli `internal/auth/device_flow.go`).

/// The credential fields of a token set (the framework's layout: [`ACCESS_TOKEN`], …; times in
/// Unix ms as text). lark-cli: no `refresh_token_expires_in` → 7 days.
pub fn token_fields(set: &TokenSet, now: i64) -> Vec<(String, Zeroizing<String>)> {
    let mut f = vec![(ACCESS_TOKEN.to_string(), set.access_token.clone())];
    if let Some(r) = &set.refresh_token {
        f.push((REFRESH_TOKEN.into(), r.clone()));
        let rexp = now + set.refresh_expires_in.unwrap_or(604_800) * 1000;
        f.push((REFRESH_EXPIRES_AT.into(), Zeroizing::new(rexp.to_string())));
    }
    if let Some(e) = set.expires_in {
        f.push((
            EXPIRES_AT.into(),
            Zeroizing::new((now + e * 1000).to_string()),
        ));
    }
    if let Some(s) = &set.scope {
        f.push((SCOPE.into(), Zeroizing::new(s.clone())));
    }
    f
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DeviceState {
    Pending,
    /// Tokens came back and the connection is being written: a cancel now waits for the outcome.
    Binding,
    Ok {
        account_id: String,
    },
    Failed(String),
    /// Cancelled before binding: never bound afterwards.
    Cancelled,
}

struct DeviceEntry {
    ws: String,
    user: String,
    state: DeviceState,
    /// Unix ms; the entry is dropped 10 minutes after.
    expires_at: i64,
}

/// Device-code logins of `feishu_user` in progress (in memory, like the framework's).
#[derive(Clone)]
pub struct DeviceFlows {
    conns: Connections,
    user: Arc<FeishuUser>,
    entries: Arc<Mutex<HashMap<String, DeviceEntry>>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn api_err(e: ProviderError) -> ApiError {
    match e {
        ProviderError::Unsupported => ApiError::rule(e.to_string()),
        e => ApiError::rule(format!("飞书设备码登录没有开始：{e}")),
    }
}

impl DeviceFlows {
    /// `user` must be the provider registered as `feishu_user` (same endpoints).
    pub fn new(conns: Connections, user: Arc<FeishuUser>) -> Self {
        Self {
            conns,
            user,
            entries: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    fn now(&self) -> i64 {
        self.conns.modules().clock.now()
    }

    /// Starts a device login for `who` with the bot connection `app_connection`'s client; polls in
    /// the background and ends in [`Connections::connect`] (the tokens to the vault first).
    pub async fn start(
        &self,
        who: &Caller,
        app_connection: &str,
        caps: Option<Vec<String>>,
    ) -> Result<DeviceOut, ApiError> {
        self.start_with(who, app_connection, caps, None, None).await
    }

    /// [`Self::start`] for a scope set: `scopes` (each declared by `feishu_user`) and/or
    /// `reauth_of` (your own `feishu_user` connection here). Requested, with `offline_access` always:
    /// - `reauth_of` without `scopes`: exactly what that connection was granted (a refresh never
    ///   widens it, e.g. never adds sending as you to a read-only connection);
    /// - `reauth_of` with `scopes`: what it was granted plus `scopes` (the missing ones);
    /// - `scopes` alone: just those;
    /// - neither: every declared scope (as before).
    pub async fn start_with(
        &self,
        who: &Caller,
        app_connection: &str,
        caps: Option<Vec<String>>,
        scopes: Option<Vec<String>>,
        reauth_of: Option<String>,
    ) -> Result<DeviceOut, ApiError> {
        // visible to the caller, in their workspace
        self.conns
            .get(who, app_connection)
            .map_err(|_| ApiError::not_found("连接"))?;
        let requested = self.requested_scopes(who, app_connection, scopes, reauth_of.as_deref())?;
        let client = app_client(&self.conns, &who.ws, app_connection)
            .await
            .map_err(api_err)?;
        // the device flow runs on the bot app's brand, and the connection remembers it
        let domain = app_domain(&self.conns, &who.ws, app_connection).map_err(api_err)?;
        self.user.guard_hosts();
        let mut start_spec = self.user.device_start_spec_on(domain);
        start_spec.scopes = requested.clone();
        let ds = oauth::device_start(self.conns.http(), &start_spec, &client)
            .await
            .map_err(api_err)?;
        let id = format!("dev_{}", oauth::random_token(12).map_err(api_err)?);
        let now = self.now();
        {
            let mut m = lock(&self.entries);
            m.retain(|_, e| e.expires_at + 10 * 60_000 > now);
            m.insert(
                id.clone(),
                DeviceEntry {
                    ws: who.ws.clone(),
                    user: who.me.clone(),
                    state: DeviceState::Pending,
                    expires_at: now + ds.expires_in * 1000,
                },
            );
        }
        let out = DeviceOut {
            id: id.clone(),
            user_code: ds.user_code.clone(),
            verification_uri: ds.verification_uri.clone(),
            verification_uri_complete: ds.verification_uri_complete.clone(),
            expires_in: ds.expires_in,
            interval: ds.interval,
        };
        let me = self.clone();
        let who = who.clone();
        // a re-authorization keeps who it is: no claim (no contact merge before the account check)
        let config = match &reauth_of {
            None => {
                let ticket = self.user.claims.issue(&who, now)?;
                json!({"app_connection": app_connection, SELF_CLAIM: ticket, "domain": domain.code()})
            }
            Some(_) => json!({"app_connection": app_connection, "domain": domain.code()}),
        };
        let mut poll_spec = self.user.device_poll_spec_on(domain);
        poll_spec.scopes = requested.clone();
        tokio::spawn(async move {
            let deadline =
                tokio::time::Instant::now() + Duration::from_secs(ds.expires_in.max(1) as u64);
            let mut interval = ds.interval.max(1) as u64;
            let state = loop {
                tokio::time::sleep(Duration::from_secs(interval)).await;
                if me.is_cancelled(&id) {
                    return;
                }
                if tokio::time::Instant::now() >= deadline {
                    break DeviceState::Failed("设备码已经过期，请重新开始".into());
                }
                match oauth::device_poll(me.conns.http(), &poll_spec, &client, &ds.device_code)
                    .await
                {
                    Ok(DevicePoll::Pending) => continue,
                    Ok(DevicePoll::SlowDown) => interval += 5,
                    Ok(DevicePoll::Failed(m)) => break DeviceState::Failed(m),
                    Err(ProviderError::Transient(_)) => continue,
                    Err(e) => break DeviceState::Failed(e.to_string()),
                    Ok(DevicePoll::Done(set)) => {
                        let scopes: Vec<String> = match &set.scope {
                            Some(s) => s
                                .split([' ', ','])
                                .filter(|x| !x.is_empty())
                                .map(String::from)
                                .collect(),
                            None => requested.clone(),
                        };
                        // a cancel committed before this point wins: nothing is bound
                        if !me.claim_binding(&id) {
                            return;
                        }
                        let fields = token_fields(&set, me.now());
                        break match me
                            .conns
                            .connect(&who, USER_ADAPTER, fields, config, scopes, caps, reauth_of)
                            .await
                        {
                            Ok(d) => DeviceState::Ok {
                                account_id: d.account_id,
                            },
                            Err(e) => DeviceState::Failed(e.to_string()),
                        };
                    }
                }
            };
            if let Some(e) = lock(&me.entries).get_mut(&id)
                && !matches!(e.state, DeviceState::Cancelled)
            {
                e.state = state;
            }
        });
        Ok(out)
    }

    /// The scopes a start asks for (see [`Self::start_with`]); 400 for an undeclared one, 404 for
    /// a `reauth_of` that is not your own `feishu_user` connection here.
    fn requested_scopes(
        &self,
        who: &Caller,
        app_connection: &str,
        scopes: Option<Vec<String>>,
        reauth_of: Option<&str>,
    ) -> Result<Vec<String>, ApiError> {
        let declared = &self.user.device_start_spec().scopes;
        let mut out: Vec<String> = Vec::new();
        let mut add = |s: &str| {
            if !out.iter().any(|x| x == s) {
                out.push(s.to_string());
            }
        };
        add("offline_access");
        if let Some(id) = reauth_of {
            let c = self
                .conns
                .modules()
                .store
                .reader(|c| steve_modules::accounts::connections::repo::get(c, id))
                .map_err(|e| ApiError::Internal(e.to_string()))?
                .filter(|c| {
                    c.workspace_id == who.ws && c.adapter == USER_ADAPTER && c.created_by == who.me
                })
                .ok_or_else(|| ApiError::not_found("连接"))?;
            // re-authorizing goes through the app it was made with, never moves it to another app
            if c.config.get("app_connection").and_then(Value::as_str) != Some(app_connection) {
                return Err(ApiError::rule("重新授权要用这个连接原来的机器人应用"));
            }
            for s in &c.granted_scopes {
                add(s);
            }
        }
        match scopes {
            Some(xs) => {
                if let Some(bad) = xs.iter().find(|s| !declared.iter().any(|d| d == *s)) {
                    return Err(ApiError::invalid(format!("scopes.{bad}")));
                }
                for s in &xs {
                    add(s);
                }
            }
            None if reauth_of.is_none() => {
                for s in declared {
                    add(s);
                }
            }
            None => {}
        }
        Ok(out)
    }

    fn is_cancelled(&self, id: &str) -> bool {
        lock(&self.entries)
            .get(id)
            .is_none_or(|e| matches!(e.state, DeviceState::Cancelled))
    }

    /// `Pending → Binding` (false when it was cancelled first).
    fn claim_binding(&self, id: &str) -> bool {
        let mut m = lock(&self.entries);
        match m.get_mut(id) {
            Some(e) if matches!(e.state, DeviceState::Pending) => {
                e.state = DeviceState::Binding;
                true
            }
            _ => false,
        }
    }

    /// `POST …/cancel`: before binding, cancelled for good; once binding began, waits for it and
    /// answers its outcome (never claims a cancel that did not happen).
    pub async fn cancel(&self, who: &Caller, id: &str) -> Result<Value, ApiError> {
        {
            let mut m = lock(&self.entries);
            let e = m
                .get_mut(id)
                .filter(|e| e.ws == who.ws && e.user == who.me)
                .ok_or_else(|| ApiError::not_found("设备码登录"))?;
            if matches!(e.state, DeviceState::Pending) {
                e.state = DeviceState::Cancelled;
            }
        }
        // binding: the connect is in flight; its outcome is what happened
        for _ in 0..600 {
            let binding = lock(&self.entries)
                .get(id)
                .is_some_and(|e| matches!(e.state, DeviceState::Binding));
            if !binding {
                break;
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
        self.status(who, id)
    }

    /// `{status: pending|ok|error, account_id?, error?}` — only for the user who started it.
    pub fn status(&self, who: &Caller, id: &str) -> Result<Value, ApiError> {
        let m = lock(&self.entries);
        let e = m
            .get(id)
            .filter(|e| e.ws == who.ws && e.user == who.me)
            .ok_or_else(|| ApiError::not_found("设备码登录"))?;
        Ok(match &e.state {
            DeviceState::Pending | DeviceState::Binding => json!({"status": "pending"}),
            DeviceState::Cancelled => json!({"status": "cancelled"}),
            DeviceState::Ok { account_id } => json!({"status": "ok", "account_id": account_id}),
            DeviceState::Failed(m) => json!({"status": "error", "error": m}),
        })
    }
}

#[derive(Deserialize)]
struct DeviceStartBody {
    app_connection: String,
    #[serde(default)]
    caps: Option<Vec<String>>,
    #[serde(default)]
    scopes: Option<Vec<String>>,
    #[serde(default)]
    reauth_of: Option<String>,
}

/// `POST /channels/feishu/device/start {app_connection, caps?}` → the device code to show;
/// `GET /channels/feishu/device/{id}` → its state. Relative to `/api`, behind the `Caller` layer.
pub fn device_routes(flows: DeviceFlows) -> Router<Modules> {
    Router::new()
        .route("/channels/feishu/device/start", post(device_start))
        .route("/channels/feishu/device/{id}", get(device_status))
        .route("/channels/feishu/device/{id}/cancel", post(device_cancel))
        .layer(Extension(flows))
}

async fn device_start(
    Extension(f): Extension<DeviceFlows>,
    who: Caller,
    body: steve_modules::Body<DeviceStartBody>,
) -> Result<Json<Value>, ApiError> {
    let b = body.0;
    let out = f
        .start_with(&who, &b.app_connection, b.caps, b.scopes, b.reauth_of)
        .await?;
    Ok(Json(serde_json::to_value(out).map_err(ApiError::from)?))
}

async fn device_cancel(
    Extension(f): Extension<DeviceFlows>,
    who: Caller,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    f.cancel(&who, &id).await.map(Json)
}

async fn device_status(
    Extension(f): Extension<DeviceFlows>,
    who: Caller,
    Path(id): Path<String>,
) -> Result<Json<Value>, ApiError> {
    f.status(&who, &id).map(Json)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn user_caps_follow_scopes() {
        let u = FeishuUser::with_base("http://127.0.0.1:1");
        let s = |v: &[&str]| v.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        assert_eq!(
            u.available_caps(&s(USER_SCOPES)),
            vec!["msg.receive", "msg.send_as_user"]
        );
        assert_eq!(
            u.available_caps(&s(&["offline_access", "im:message", "im:chat:read"])),
            Vec::<String>::new()
        );
        assert_eq!(
            u.available_caps(&s(&[
                "im:message:readonly",
                "im:message.p2p_msg:get_as_user",
                "im:chat:read"
            ])),
            vec!["msg.receive"]
        );
    }

    #[test]
    fn integration_lists_live_and_planned_areas() {
        let app = FeishuApp::new().integration();
        assert_eq!(
            (app.provider.as_str(), app.identity.as_str()),
            ("feishu", "bot")
        );
        let areas: Vec<_> = app
            .capabilities
            .iter()
            .map(|c| (c.area.as_str(), c.planned))
            .collect();
        assert_eq!(
            areas,
            [
                ("channel", false),
                ("directory", false),
                ("docs", true),
                ("sheets", true),
                ("calendar", true),
                ("tasks", true),
                ("approval", true),
                ("minutes", true),
            ]
        );
        let ch = &app.capabilities[0];
        assert!(
            ch.scopes
                .iter()
                .any(|s| s.scope == "im:chat:readonly" && s.required)
        );
        assert!(
            ch.scopes
                .iter()
                .any(|s| s.scope == "im:message.group_msg" && !s.required)
        );
        assert!(ch.scopes.iter().all(|s| !s.scope.starts_with("contact:")));
        let user = FeishuUser::with_base("http://127.0.0.1:1").integration();
        assert_eq!(user.identity, "self");
        // sending as you is its own area with its scope required; the channel area doesn't list it
        assert!(
            !user.capabilities[0]
                .scopes
                .iter()
                .any(|s| s.scope == SEND_AS_USER_SCOPE)
        );
        let send = user
            .capabilities
            .iter()
            .find(|c| c.area == "send_as_you")
            .expect("send_as_you area");
        assert_eq!(send.caps, vec!["msg.send_as_user".to_string()]);
        assert!(
            send.scopes
                .iter()
                .any(|s| s.scope == SEND_AS_USER_SCOPE && s.required)
        );
    }

    #[test]
    fn real_hosts() {
        let u = FeishuUser::new(Domain::Feishu);
        assert_eq!(
            u.spec.authorize_url,
            "https://accounts.feishu.cn/open-apis/authen/v1/authorize"
        );
        assert_eq!(
            u.spec.token_url,
            "https://open.feishu.cn/open-apis/authen/v2/oauth/token"
        );
        assert_eq!(
            u.device_start_spec().device_url.as_deref(),
            Some("https://accounts.feishu.cn/oauth/v1/device_authorization")
        );
        assert_eq!(
            u.device_poll_spec().device_token_url.as_deref(),
            Some("https://accounts.feishu.cn/oauth/v3/token")
        );
        assert!(
            u.spec.device_url.is_none(),
            "the generic device route can't do Feishu's"
        );
    }

    #[test]
    fn token_fields_default_refresh_ttl() {
        let set = TokenSet {
            access_token: Zeroizing::new("a".into()),
            refresh_token: Some(Zeroizing::new("r".into())),
            expires_in: Some(7200),
            refresh_expires_in: None,
            scope: Some("offline_access".into()),
        };
        let f = token_fields(&set, 1000);
        let get = |k: &str| f.iter().find(|(l, _)| l == k).map(|(_, v)| v.to_string());
        assert_eq!(get(REFRESH_EXPIRES_AT).as_deref(), Some("604801000"));
        assert_eq!(get(EXPIRES_AT).as_deref(), Some("7201000"));
    }
}
