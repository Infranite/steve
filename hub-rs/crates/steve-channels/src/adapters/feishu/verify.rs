//! Verifying a Feishu connection (`verify()`):
//! who the bot or user is, that the app is a 企业自建应用, and which
//! permissions are actually enabled.

use serde::Serialize;
use serde_json::{Value, json};
use steve_modules::accounts::connections::{
    AccountInfo, ConnCx, Integration, Owner, ProviderError, StewardHint,
};

use super::client::{
    Domain, FeishuClient, FeishuError, TokenSource, VerifyTokens, data, is_grant_link, is_scope,
};
use super::ws;

/// How much a permission matters in phase one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Level {
    /// 必须.
    Required,
    /// 建议 (`im:message.group_msg`: sensitive, needs tenant admin approval).
    Suggested,
    /// Directory sync (P6).
    Directory,
    /// Names of the people Steve meets: without them people stay “未知飞书用户”.
    Names,
    /// Optional, asked for only with the user's explicit consent.
    Optional,
}

/// One row of the permission table.
#[derive(Debug, Clone, Copy)]
pub struct Permission {
    /// The scope as the console shows it.
    pub scope: &'static str,
    /// What Steve uses it for (Chinese, shown in the dialog).
    pub purpose: &'static str,
    pub level: Level,
}

const fn p(scope: &'static str, purpose: &'static str, level: Level) -> Permission {
    Permission {
        scope,
        purpose,
        level,
    }
}

/// The bot (tenant) permissions Steve checks.
pub const BOT_PERMISSIONS: &[Permission] = &[
    p("im:message", "发消息、回复", Level::Required),
    p(
        "im:message:send_as_bot",
        "以应用身份发消息",
        Level::Required,
    ),
    p(
        "im:message.p2p_msg:readonly",
        "收用户发给机器人的私聊",
        Level::Required,
    ),
    p(
        "im:message.group_at_msg:readonly",
        "收群里 @机器人 的消息",
        Level::Required,
    ),
    p(
        "im:message.group_msg",
        "收群里所有消息（敏感权限，需要租户管理员审批）",
        Level::Suggested,
    ),
    p(
        "im:chat:readonly",
        "群信息、成员、机器人所在的群",
        Level::Required,
    ),
    p("im:resource", "取消息里的图片和文件", Level::Required),
    p(
        "im:message.reactions:write_only",
        "给消息加“收到/完成”表情回复（已开 im:message 时可不开）",
        Level::Required,
    ),
    p(
        "contact:contact.base:readonly",
        "通讯录同步",
        Level::Directory,
    ),
    p(
        "contact:department.base:readonly",
        "通讯录同步：部门",
        Level::Directory,
    ),
    p(
        "contact:user.base:readonly",
        "通讯录同步：成员",
        Level::Directory,
    ),
    p(
        "contact:user.email:readonly",
        "通讯录同步：邮箱",
        Level::Directory,
    ),
    p(
        "contact:user.department:readonly",
        "通讯录同步：成员的部门和上级",
        Level::Directory,
    ),
    p(
        super::profiles::MEMBERS_SCOPE,
        super::profiles::MEMBERS_PURPOSE,
        Level::Names,
    ),
    p(
        super::profiles::TENANT_SCOPE,
        super::profiles::TENANT_PURPOSE,
        Level::Names,
    ),
];

/// The user permissions.
pub const USER_PERMISSIONS: &[Permission] = &[
    p("offline_access", "保持授权（刷新令牌）", Level::Required),
    p("im:message", "以你本人读写消息", Level::Required),
    p("im:message:readonly", "读你本人的消息", Level::Required),
    p(
        "im:message.p2p_msg:get_as_user",
        "读别人发给你本人的私聊",
        Level::Required,
    ),
    p(
        "im:message.group_msg:get_as_user",
        "读你所在群的消息",
        Level::Required,
    ),
    p("im:chat:read", "列出你本人的会话", Level::Required),
    p(
        "im:message.send_as_user",
        "以你本人身份发送（需要你明确同意）",
        Level::Optional,
    ),
    p("search:message", "搜索消息", Level::Optional),
    p(
        super::profiles::MEMBERS_SCOPE,
        super::profiles::MEMBERS_PURPOSE,
        Level::Names,
    ),
    p(
        super::profiles::CONTACT_SCOPE,
        super::profiles::CONTACT_PURPOSE,
        Level::Names,
    ),
];

/// What a check found.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    /// 已开通.
    Granted,
    /// 未开通.
    Missing,
    /// Can't be told without side effects (sending a message): 以控制台为准.
    Unknown,
    /// An optional user scope Steve didn't ask for (`search:message`).
    NotRequested,
}

/// One checked row, as stored in `connections.config.permissions` and shown in the dialog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PermissionCheck {
    pub scope: &'static str,
    pub purpose: &'static str,
    pub level: Level,
    pub status: Status,
}

impl PermissionCheck {
    fn of(p: &Permission, status: Status) -> Self {
        Self {
            scope: p.scope,
            purpose: p.purpose,
            level: p.level,
            status,
        }
    }
}

/// The scopes among `checks` found missing.
pub fn missing(checks: &[PermissionCheck]) -> Vec<&'static str> {
    checks
        .iter()
        .filter(|c| c.status == Status::Missing)
        .map(|c| c.scope)
        .collect()
}

// ---------------------------------------------------------------------------------------------
// Permissions learned at runtime, and the links that fix them.

/// `config` key: the last permission-page link Feishu itself gave in a refusal, with the scopes
/// it covers (`{url, scopes, at}`).
pub const PERMISSION_HELP: &str = "permission_help";

/// A call of connection `config` was refused for a missing scope (`alternatives`: the scopes
/// Feishu named, any one of which would do). Marks the matching rows of `table` 未开通 in
/// `config.permissions` (a scope outside the table gets a row of its own) through the one writer,
/// `connections::repo::note_permissions_in`, and keeps Feishu's link. Returns whether anything
/// changed. `now`: Unix ms.
pub fn note_refused(
    config: &mut Value,
    table: &[Permission],
    alternatives: &[String],
    help_url: Option<&str>,
    now: i64,
) -> bool {
    if alternatives.is_empty() || !config.is_object() {
        return false;
    }
    // before any check: the table's rows, not checked yet
    if config.get("permissions").is_none() {
        config["permissions"] = table
            .iter()
            .map(|p| json!(PermissionCheck::of(p, Status::Unknown)))
            .collect();
    }
    let known: Vec<&String> = alternatives
        .iter()
        .filter(|a| table.iter().any(|p| p.scope == a.as_str()))
        .collect();
    // the scopes of the table Feishu named; else the first one it named
    let mark: Vec<&String> = if known.is_empty() {
        alternatives.iter().take(1).collect()
    } else {
        known
    };
    let missing: Vec<(&str, &str)> = mark
        .iter()
        .map(|s| (s.as_str(), "Steve 工作时飞书要求的权限"))
        .collect();
    let mut changed =
        steve_modules::accounts::connections::repo::note_permissions_in(config, &missing, &[], now);
    if let Some(url) = help_url.filter(|u| super::client::is_grant_link(u)) {
        let old = config
            .get(PERMISSION_HELP)
            .and_then(|h| h.get("url"))
            .and_then(Value::as_str);
        if old != Some(url) {
            config[PERMISSION_HELP] = json!({"url": url, "scopes": alternatives, "at": now});
            changed = true;
        }
    }
    changed
}

/// The permission table of a Feishu identity, by what its provider says it is (the integration's
/// provider and identity), whatever adapter id serves it: the bot's or yours; `None` for anything
/// that is not a Feishu identity.
pub fn table_of(i: &Integration) -> Option<&'static [Permission]> {
    if i.provider != super::auth::PROVIDER {
        return None;
    }
    match i.identity.as_str() {
        "bot" => Some(BOT_PERMISSIONS),
        "self" => Some(USER_PERMISSIONS),
        _ => None,
    }
}

/// [`note_refused`] on the connection of `cx` (bot or you, its table by [`table_of`]), written through the connections
/// framework (no-op while the connection is still being created).
pub async fn record_refused(cx: &ConnCx, scopes: &[String], help_url: Option<&str>) {
    let Some(table) = cx
        .connections()
        .provider(cx.adapter())
        .and_then(|p| table_of(&p.integration()))
    else {
        return;
    };
    let (scopes, url) = (scopes.to_vec(), help_url.map(String::from));
    let now = cx.connections().modules().clock.now();
    cx.connections()
        .update_config(cx.ws(), cx.id(), move |c| {
            note_refused(c, table, &scopes, url.as_deref(), now)
        })
        .await;
}

/// The rows of `config.permissions` that are 未开通.
pub fn missing_rows(config: &Value) -> Vec<Value> {
    config
        .get("permissions")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|r| r.get("status").and_then(Value::as_str) == Some("missing"))
        .cloned()
        .collect()
}

/// The link that opens app `app_id`'s permission page with `scopes` preselected: the open-platform
/// form `https://open.feishu.cn/app/<app_id>/auth?q=<a,b>&op_from=openapi&token_type=tenant` (the
/// one Feishu puts into 99991672 answers; `token_type` only for the app identity — the user
/// identity's page is the same link without it, as in the docs' example).
pub fn grant_url(domain: Domain, app_id: &str, scopes: &[&str], tenant: bool) -> String {
    let q: Vec<&str> = scopes.iter().copied().filter(|s| is_scope(s)).collect();
    let mut u = format!(
        "{}/app/{}/auth?q={}&op_from=openapi",
        domain.open_base(),
        app_id.trim(),
        q.join(",")
    );
    if tenant {
        u.push_str("&token_type=tenant");
    }
    u
}

/// The 接入 page's facts about the bot connection: the app, the console pages, what is 未开通 and
/// the one link that requests all of it (Feishu's own when it covers them all).
pub fn app_detail(config: &Value) -> Option<Value> {
    let app_id = config
        .get("app_id")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())?;
    let domain = Domain::from_config(config);
    let open = domain.open_base();
    let missing = missing_rows(config);
    let scopes: Vec<&str> = missing
        .iter()
        .filter_map(|r| r.get("scope").and_then(Value::as_str))
        .collect();
    let feishu_link = config.get(PERMISSION_HELP).and_then(|h| {
        let covers: Vec<&str> = h
            .get("scopes")?
            .as_array()?
            .iter()
            .filter_map(Value::as_str)
            .collect();
        let url = h.get("url")?.as_str()?;
        (scopes.iter().all(|s| covers.contains(s)) && is_grant_link(url)).then_some(url)
    });
    let grant = match (scopes.is_empty(), feishu_link) {
        (true, _) => Value::Null,
        (false, Some(u)) => json!({"url": u, "from": "feishu"}),
        (false, None) => json!({"url": grant_url(domain, app_id, &scopes, true), "from": "steve"}),
    };
    let refused_at = missing
        .iter()
        .filter_map(|r| r.get("refused_at").and_then(Value::as_i64))
        .max();
    Some(json!({
        "app_id": app_id,
        "domain": domain.code(),
        "app_name": config.get("app_name").cloned().unwrap_or(Value::Null),
        "console_url": format!("{open}/app/{app_id}"),
        "permissions_url": format!("{open}/app/{app_id}/auth"),
        "publish_url": format!("{open}/app/{app_id}/version"),
        "missing": missing,
        "grant": grant,
        "checked_at": config.get("permissions_checked_at").cloned().unwrap_or(Value::Null),
        "refused_at": refused_at,
    }))
}

/// The 接入 page's facts about your connection: through which app, what the grant lacks (the
/// re-authorization asks for all of it), the scopes asked for by default.
pub fn user_detail(config: &Value) -> Option<Value> {
    let missing = missing_rows(config);
    let refused_at = missing
        .iter()
        .filter_map(|r| r.get("refused_at").and_then(Value::as_i64))
        .max();
    Some(json!({
        "app_connection": config.get("app_connection").cloned().unwrap_or(Value::Null),
        "missing": missing,
        "requested": super::auth::USER_SCOPES,
        "refused_at": refused_at,
    }))
}

/// The permission check again (`POST /connections/:id/test` after you enabled scopes): the same
/// patch `verify_app` stores, Feishu's old link dropped.
pub async fn recheck_app(cx: &ConnCx) -> Option<Value> {
    let client = FeishuClient::for_cx(cx);
    let tokens = VerifyTokens::new(cx);
    let perms = check_bot_permissions(&client, &tokens).await;
    Some(json!({
        "permissions": perms,
        "missing_permissions": missing(&perms),
        "permissions_checked_at": cx.connections().modules().clock.now(),
        PERMISSION_HELP: Value::Null,
    }))
}

/// `bot/v3/info`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BotInfo {
    pub app_name: String,
    pub open_id: String,
    pub avatar_url: String,
    /// 2 = 启用.
    pub activate_status: i64,
}

/// `GET /open-apis/bot/v3/info` (answers `bot` at the top level).
pub async fn bot_info(
    client: &FeishuClient,
    tokens: &dyn TokenSource,
) -> Result<BotInfo, FeishuError> {
    let v = client.get(tokens, "/open-apis/bot/v3/info", &[]).await?;
    let b = v.get("bot").unwrap_or_else(|| data(&v));
    let s = |k: &str| {
        b.get(k)
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string()
    };
    let info = BotInfo {
        app_name: s("app_name"),
        open_id: s("open_id"),
        avatar_url: s("avatar_url"),
        activate_status: b
            .get("activate_status")
            .and_then(Value::as_i64)
            .unwrap_or(0),
    };
    if info.open_id.is_empty() {
        return Err(FeishuError::Decode);
    }
    Ok(info)
}

/// What kind of app the credential belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppKind {
    /// 企业自建应用.
    SelfBuilt,
    /// 应用商店应用 (or a personal store app): `scene_type`.
    Store(i64),
    /// Neither check could tell.
    Unknown,
}

/// Self-built or not: `application/v6/applications/{app_id}` `scene_type` (0 = 自建) when the app
/// may read its own info; otherwise the long-connection endpoint, which only self-built apps get
/// (403 = not self-built).
pub async fn app_kind(
    client: &FeishuClient,
    tokens: &dyn TokenSource,
    app_id: &str,
    app_secret: &str,
) -> AppKind {
    app_info(client, tokens, app_id, app_secret).await.0
}

/// [`app_kind`] and the app's owner from the same app info: `owner.owner_id` as a `union_id` (the
/// handle of a Feishu user account) when the owner is a member of the tenant (`owner.type` 2, or no
/// type given). That person becomes the app bot's 负责人.
pub async fn app_info(
    client: &FeishuClient,
    tokens: &dyn TokenSource,
    app_id: &str,
    app_secret: &str,
) -> (AppKind, Option<String>) {
    let path = format!("/open-apis/application/v6/applications/{app_id}");
    let params = [("lang", "zh_cn"), ("user_id_type", "union_id")];
    match client.get(tokens, &path, &params).await {
        Ok(v) => {
            let app = data(&v).get("app");
            let owner = app
                .and_then(|a| a.get("owner"))
                .filter(|o| o.get("type").and_then(Value::as_i64).is_none_or(|t| t == 2))
                .and_then(|o| o.get("owner_id"))
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string);
            if let Some(t) = app
                .and_then(|a| a.get("scene_type"))
                .and_then(Value::as_i64)
            {
                let kind = if t == 0 {
                    AppKind::SelfBuilt
                } else {
                    AppKind::Store(t)
                };
                return (kind, owner);
            }
        }
        Err(e) => {
            tracing::debug!(error = %e, "feishu: app info not readable, probing the long connection")
        }
    }
    let kind = match ws::fetch_endpoint(client.http(), client.base(), app_id, app_secret).await {
        Ok(_) => AppKind::SelfBuilt,
        Err(ws::WsError::Client { code, .. }) if code == super::pbbp2::code::FORBIDDEN => {
            AppKind::Store(-1)
        }
        Err(_) => AppKind::Unknown,
    };
    (kind, None)
}

/// The bot permission check of the connect dialog: `GET im/v1/chats?page_size=1`
/// tells `im:chat:readonly`, then one message of a group the bot is in tells `im:message.group_msg`,
/// and `contact/v3/scopes` tells the directory base scope. A missing-scope answer marks the row
/// 未开通; the rest can't be checked without sending and stay 以控制台为准.
pub async fn check_bot_permissions(
    client: &FeishuClient,
    tokens: &dyn TokenSource,
) -> Vec<PermissionCheck> {
    let status = |r: &Result<Value, FeishuError>| match r {
        Ok(_) => Status::Granted,
        Err(e) if e.is_no_scope() => Status::Missing,
        Err(_) => Status::Unknown,
    };
    let chats = client
        .get(tokens, "/open-apis/im/v1/chats", &[("page_size", "20")])
        .await;
    let chat_status = status(&chats);
    let group = chats.as_ref().ok().and_then(|v| {
        data(v)
            .get("items")
            .and_then(Value::as_array)
            .and_then(|items| {
                items
                    .iter()
                    .find(|c| c.get("chat_mode").and_then(Value::as_str) != Some("p2p"))
                    .and_then(|c| c.get("chat_id").and_then(Value::as_str))
                    .map(String::from)
            })
    });
    let group_msg = match group {
        Some(id) => {
            let r = client
                .get(
                    tokens,
                    "/open-apis/im/v1/messages",
                    &[
                        ("container_id_type", "chat"),
                        ("container_id", &id),
                        ("page_size", "1"),
                    ],
                )
                .await;
            status(&r)
        }
        None => Status::Unknown,
    };
    let contact = client
        .get(
            tokens,
            "/open-apis/contact/v3/scopes",
            &[
                ("user_id_type", "union_id"),
                ("department_id_type", "open_department_id"),
                ("page_size", "1"),
            ],
        )
        .await;
    let contact_status = status(&contact);
    BOT_PERMISSIONS
        .iter()
        .map(|p| {
            let s = match p.scope {
                "im:chat:readonly" => chat_status,
                "im:message.group_msg" => group_msg,
                "contact:contact.base:readonly" => contact_status,
                _ => Status::Unknown,
            };
            PermissionCheck::of(p, s)
        })
        .collect()
}

/// The user permission check: the scopes the grant actually has, as stored.
/// An empty list means Feishu didn't say: everything stays unknown.
pub fn check_user_scopes(granted: &[String]) -> Vec<PermissionCheck> {
    USER_PERMISSIONS
        .iter()
        .map(|p| {
            let s = if granted.iter().any(|g| g == p.scope) {
                Status::Granted
            } else if !super::auth::USER_SCOPES.contains(&p.scope) {
                Status::NotRequested
            } else if granted.is_empty() {
                Status::Unknown
            } else {
                Status::Missing
            };
            PermissionCheck::of(p, s)
        })
        .collect()
}

/// The rejection of a store app (long connection and the user identity
/// only work for self-built apps).
pub const NOT_SELF_BUILT: &str = "这不是企业自建应用。Steve 只支持企业自建应用：请在飞书开发者后台新建一个企业自建应用并打开机器人能力";

/// `verify()` of the bot connection: bot info (`app_name`, the bot's `open_id`), self-built check,
/// permission check. Account: handle `app_id` (`feishu_app`), display name `app_name`, the bot's
/// `open_id` as a handle under this connection, owned by a bot party of its own.
pub async fn verify_app(cx: &ConnCx) -> Result<AccountInfo, ProviderError> {
    let app_id = cx
        .config_str("app_id")
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| ProviderError::Config("缺少 App ID".into()))?
        .to_string();
    let client = FeishuClient::for_cx(cx);
    let tokens = VerifyTokens::new(cx);
    let bot = bot_info(&client, &tokens).await?;
    let secret = cx.secret("app_secret").await?;
    let (kind, owner) = app_info(&client, &tokens, &app_id, &secret).await;
    drop(secret);
    if let AppKind::Store(t) = kind {
        tracing::info!(scene_type = t, "feishu: not a self-built app");
        return Err(ProviderError::Rejected(NOT_SELF_BUILT.into()));
    }
    let perms = check_bot_permissions(&client, &tokens).await;
    let mut a = AccountInfo::new(app_id, bot.app_name.clone());
    a.kind = "app_bot".into();
    // an app bot is its own bot party; the app's owner is its 负责人
    a.owner = Owner::Bot;
    a.steward_hint = owner.map(|handle| StewardHint {
        system: super::auth::USER_SYSTEM.into(),
        handle,
    });
    a.extra_handles.push(bot.open_id.clone());
    a.config = json!({
        "app_name": bot.app_name,
        "bot_open_id": bot.open_id,
        "bot_avatar_url": bot.avatar_url,
        "bot_activate_status": bot.activate_status,
        "self_built": match kind { AppKind::SelfBuilt => Value::Bool(true), _ => Value::Null },
        "permissions": perms,
        "missing_permissions": missing(&perms),
        "permissions_checked_at": cx.connections().modules().clock.now(),
    });
    Ok(a)
}

/// `GET /open-apis/authen/v1/user_info` with the user token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserInfo {
    pub union_id: String,
    pub open_id: String,
    pub name: String,
    pub email: Option<String>,
    pub tenant_key: Option<String>,
}

pub async fn user_info(
    client: &FeishuClient,
    tokens: &dyn TokenSource,
) -> Result<UserInfo, FeishuError> {
    let v = client
        .get(tokens, "/open-apis/authen/v1/user_info", &[])
        .await?;
    let d = data(&v);
    let s = |k: &str| {
        d.get(k)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(String::from)
    };
    Ok(UserInfo {
        union_id: s("union_id").ok_or(FeishuError::Decode)?,
        open_id: s("open_id").unwrap_or_default(),
        name: s("name").unwrap_or_default(),
        email: s("enterprise_email").or_else(|| s("email")),
        tenant_key: s("tenant_key"),
    })
}

/// The hint when Feishu gave no refresh token (lark-cli `no_refresh_token.go`).
pub const NO_REFRESH_HINT: &str = "飞书没有给出 refresh_token：请在开发者后台的安全设置里打开“刷新 user_access_token”，并发布新版本，然后重新授权";

/// `verify()` of the user connection: `user_info` → handle `union_id` (system `feishu`, the same id
/// the bot sees as sender, so both land on the same contact), `open_id` as a handle under this
/// connection; the granted scopes checked against [`USER_PERMISSIONS`].
pub async fn verify_user(cx: &ConnCx, client: &FeishuClient) -> Result<AccountInfo, ProviderError> {
    let tokens = VerifyTokens::new(cx);
    let u = user_info(client, &tokens).await?;
    let has_refresh = cx
        .secret(steve_modules::accounts::connections::cx::REFRESH_TOKEN)
        .await
        .is_ok();
    let perms = check_user_scopes(cx.scopes());
    let mut a = AccountInfo::new(u.union_id.clone(), u.name.clone());
    a.kind = "personal".into();
    a.owner = Owner::Me;
    if !u.open_id.is_empty() {
        a.extra_handles.push(u.open_id.clone());
    }
    let mut cfg = json!({
        "open_id": u.open_id,
        "union_id": u.union_id,
        "permissions": perms,
        "missing_permissions": missing(&perms),
    });
    if let Some(t) = u.tenant_key {
        cfg["tenant_key"] = json!(t);
    }
    if !has_refresh {
        cfg["warning"] = json!(NO_REFRESH_HINT);
    }
    a.config = cfg;
    Ok(a)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_table_follows_the_identity_not_the_adapter_id() {
        use super::super::auth::{app_integration, user_integration};
        let scopes = |t: Option<&'static [Permission]>| {
            t.map(|t| t.iter().map(|p| p.scope).collect::<Vec<_>>())
        };
        let of = |t: &'static [Permission]| Some(t.iter().map(|p| p.scope).collect::<Vec<_>>());
        assert_eq!(scopes(table_of(&app_integration())), of(BOT_PERMISSIONS));
        assert_eq!(scopes(table_of(&user_integration())), of(USER_PERMISSIONS));
        // another provider's identity has no Feishu table
        let mut other = user_integration();
        other.provider = "mail".into();
        assert!(table_of(&other).is_none());
        let mut app = app_integration();
        app.identity = "account".into();
        assert!(table_of(&app).is_none());
    }

    #[test]
    fn user_scopes_are_flagged() {
        let granted: Vec<String> = ["offline_access", "im:message", "im:chat:read"]
            .iter()
            .map(|s| s.to_string())
            .collect();
        let c = check_user_scopes(&granted);
        let m = missing(&c);
        assert!(m.contains(&"im:message.p2p_msg:get_as_user"));
        assert!(m.contains(&"im:message.send_as_user"));
        assert!(!m.contains(&"offline_access"));
        assert!(m.iter().all(|s| *s != "search:message"), "not asked for");
        assert!(
            check_user_scopes(&[])
                .iter()
                .all(|c| matches!(c.status, Status::Unknown | Status::NotRequested))
        );
    }

    #[test]
    fn grant_links() {
        assert_eq!(
            grant_url(
                Domain::Feishu,
                "cli_x",
                &["im:chat:readonly", "im:resource"],
                true
            ),
            "https://open.feishu.cn/app/cli_x/auth?q=im:chat:readonly,im:resource&op_from=openapi&token_type=tenant"
        );
        assert_eq!(
            grant_url(
                Domain::Lark,
                "cli_x",
                &["im:message.send_as_user", "a b"],
                false
            ),
            "https://open.larksuite.com/app/cli_x/auth?q=im:message.send_as_user&op_from=openapi"
        );
    }

    #[test]
    fn refusals_mark_rows() {
        let mut c = json!({"app_id": "cli_x"});
        let s = |xs: &[&str]| xs.iter().map(|x| x.to_string()).collect::<Vec<_>>();
        // no stored check yet: the table, the named scope missing
        assert!(note_refused(
            &mut c,
            BOT_PERMISSIONS,
            &s(&["im:resource"]),
            None,
            5
        ));
        assert_eq!(c["missing_permissions"], json!(["im:resource"]));
        assert_eq!(
            c["permissions"].as_array().map(Vec::len),
            Some(BOT_PERMISSIONS.len())
        );
        assert!(!note_refused(
            &mut c,
            BOT_PERMISSIONS,
            &s(&["im:resource"]),
            None,
            6
        ));
        // a scope outside the table gets its own row; a foreign link is ignored
        assert!(note_refused(
            &mut c,
            BOT_PERMISSIONS,
            &s(&["im:message.reactions:read", "im:message"]),
            Some("https://x.example/app/a/auth"),
            7
        ));
        assert_eq!(
            c["missing_permissions"],
            json!(["im:message", "im:resource"])
        );
        assert!(note_refused(
            &mut c,
            BOT_PERMISSIONS,
            &s(&["drive:drive"]),
            None,
            8
        ));
        assert_eq!(
            c["missing_permissions"],
            json!(["im:message", "im:resource", "drive:drive"])
        );
        assert!(c.get(PERMISSION_HELP).is_none());
        let d = app_detail(&c).expect("detail");
        assert_eq!(d["grant"]["from"], "steve");
        assert_eq!(d["refused_at"], 8);
        assert!(!note_refused(&mut c, BOT_PERMISSIONS, &[], None, 9));
    }

    #[test]
    fn serializes_for_the_dialog() {
        let c = PermissionCheck::of(&BOT_PERMISSIONS[4], Status::Missing);
        assert_eq!(
            serde_json::to_value(c).unwrap_or_default(),
            json!({"scope": "im:message.group_msg", "purpose": "收群里所有消息（敏感权限，需要租户管理员审批）", "level": "suggested", "status": "missing"})
        );
    }
}
