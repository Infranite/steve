//! Who is behind a Feishu id: the [`ProfileAdapter`] of both identities of an app.
//! [`crate::names`] asks you (`feishu_user`) first, then the bot (`feishu_app`).
//!
//! One lookup, each step only when the previous left names open:
//! 1. a chat's members, `GET im/v1/chats/{chat_id}/members?member_id_type=open_id`, every page:
//!    names and `tenant_key`s of everyone in it, external members included. Works with both tokens
//!    for chats the identity is in; needs `im:chat.members:read` (the bot also accepts
//!    `im:chat:readonly`, but Feishu answers 99991672 when neither is enabled).
//! 2. the address book, `GET contact/v3/users/batch?user_ids=…&user_id_type=open_id|union_id` (50
//!    per call): name, avatar, email, `union_id`, departments — only for people in the app's contact
//!    visibility range; needs `contact:user.base:readonly` (email: `contact:user.email:readonly`).
//! 3. the bot's own organization, `GET tenant/v2/tenant/query` (`tenant:tenant:readonly`); you carry
//!    `tenant_key` in your connection's config since you connected.
//!
//! A missing permission is reported with the scope that would fix it, so the 接入 page can offer it;
//! nothing here fails the lookup for that.

use async_trait::async_trait;
use serde_json::Value;
use steve_flow::AdapterError;
use steve_modules::accounts::connections::{ConnCx, ProviderError};

use super::auth::{APP_ADAPTER, FeishuApp, FeishuUser, USER_ADAPTER, USER_SYSTEM};
use super::client::{self, FeishuClient, FeishuError};
use crate::channel::{HandleRef, Profile, ProfileAdapter, ProfileAnswer, ProfileQuery, TenantInfo};

/// The scope that names chat members.
pub const MEMBERS_SCOPE: &str = "im:chat.members:read";
/// The scope that names people by id from the address book.
pub const CONTACT_SCOPE: &str = "contact:user.base:readonly";
/// The scope that names the bot's own organization.
pub const TENANT_SCOPE: &str = "tenant:tenant:readonly";
/// What each is for, as the 接入 page shows it.
pub const MEMBERS_PURPOSE: &str = "成员名字：读群成员的名字和所属组织";
pub const CONTACT_PURPOSE: &str = "成员名字：按 ID 查同事的名字、头像和部门";
pub const TENANT_PURPOSE: &str = "公司名称：读本公司在飞书里的名字";

/// Answers meaning “this identity is not in that chat” (or it is gone): try the next identity.
const NOT_IN_CHAT: &[i64] = &[230002, 232006, 232009, 232010, 232011, 232024, 232033];
/// Answers meaning “outside the contact visibility range”.
const NO_AUTHORITY: &[i64] = &[40004, 41050, 41012];
/// Pages read at most (100 × 100 is the largest group).
const MAX_PAGES: usize = 100;
/// Ids per `users/batch` call (Feishu's limit).
const BATCH: usize = 50;

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

fn opt(v: &Value, k: &str) -> Option<String> {
    Some(s(v, k).trim().to_string()).filter(|x| !x.is_empty())
}

/// A chat id that may go into a path.
fn chat_path(id: &str) -> Option<String> {
    let id = id.trim();
    (!id.is_empty() && id.len() <= 200 && !id.contains(['/', '?', '#', '%']) && !id.contains(".."))
        .then(|| format!("/open-apis/im/v1/chats/{id}/members"))
}

/// What to do with an error of one step: `Ok(())` when it only means “no answer here” (recorded in
/// `ans`), `Err` when the identity itself failed.
fn absorb(
    e: FeishuError,
    ans: &mut ProfileAnswer,
    scope: &str,
    purpose: &str,
    not_here: &[i64],
) -> Result<(), FeishuError> {
    match &e {
        FeishuError::NoScope { .. } => {
            ans.missing(scope, purpose);
            Ok(())
        }
        FeishuError::Api { code, .. } if not_here.contains(code) => Ok(()),
        FeishuError::TokenInvalid { .. }
        | FeishuError::Token(_)
        | FeishuError::HostForbidden(_)
        | FeishuError::Network(_)
        | FeishuError::RateLimited { .. } => Err(e),
        // anything else (a server hiccup, an odd answer) is no answer this time
        _ => {
            tracing::debug!(scope, "feishu profiles: no answer: {e}");
            Ok(())
        }
    }
}

/// Step 1: everyone in the chat, by `open_id`.
async fn members(
    c: &FeishuClient,
    cx: &ConnCx,
    chat: &str,
    ans: &mut ProfileAnswer,
) -> Result<(), FeishuError> {
    let Some(path) = chat_path(chat) else {
        return Ok(());
    };
    let mut token = String::new();
    let mut found = Vec::new();
    for _ in 0..MAX_PAGES {
        let mut q = vec![("member_id_type", "open_id"), ("page_size", "100")];
        if !token.is_empty() {
            q.push(("page_token", token.as_str()));
        }
        let v = match c.get(cx, &path, &q).await {
            Ok(v) => v,
            Err(FeishuError::Api { code, .. }) if NOT_IN_CHAT.contains(&code) => {
                ans.not_in_space = true;
                return Ok(());
            }
            Err(e) => return absorb(e, ans, MEMBERS_SCOPE, MEMBERS_PURPOSE, NOT_IN_CHAT),
        };
        let d = client::data(&v);
        for m in d
            .get("items")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let id = s(m, "member_id");
            let name = s(m, "name").trim();
            if id.is_empty() {
                continue;
            }
            found.push(Profile {
                handle: Some(HandleRef::new(USER_SYSTEM, id)),
                name: name.to_string(),
                tenant_key: opt(m, "tenant_key"),
                ..Profile::default()
            });
        }
        let more = d.get("has_more").and_then(Value::as_bool).unwrap_or(false);
        token = s(d, "page_token").to_string();
        if !more || token.is_empty() {
            break;
        }
    }
    ans.granted(MEMBERS_SCOPE);
    ans.profiles.extend(found);
    Ok(())
}

/// One address-book user.
fn user_profile(u: &Value) -> Option<Profile> {
    let (union, open) = (s(u, "union_id"), s(u, "open_id"));
    let handle = if !union.is_empty() {
        HandleRef::new(USER_SYSTEM, union).with_extra(open)
    } else if !open.is_empty() {
        HandleRef::new(USER_SYSTEM, open)
    } else {
        return None;
    };
    let avatar = u
        .get("avatar")
        .and_then(|a| opt(a, "avatar_72").or_else(|| opt(a, "avatar_240")));
    Some(Profile {
        handle: Some(handle),
        name: s(u, "name").trim().to_string(),
        avatar_url: avatar,
        email: opt(u, "enterprise_email").or_else(|| opt(u, "email")),
        tenant_key: None,
        directory: true,
        title: opt(u, "job_title"),
        department_ids: u
            .get("department_ids")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(|d| d.as_str().map(String::from))
            .collect(),
    })
}

/// Step 2: these ids in the address book.
async fn batch(
    c: &FeishuClient,
    cx: &ConnCx,
    ids: &[String],
    id_type: &str,
    ans: &mut ProfileAnswer,
) -> Result<(), FeishuError> {
    for chunk in ids.chunks(BATCH) {
        let mut q: Vec<(&str, &str)> = vec![
            ("user_id_type", id_type),
            ("department_id_type", "open_department_id"),
        ];
        q.extend(chunk.iter().map(|i| ("user_ids", i.as_str())));
        let v = match c.get(cx, "/open-apis/contact/v3/users/batch", &q).await {
            Ok(v) => v,
            Err(e) => {
                let stop = matches!(e, FeishuError::NoScope { .. });
                absorb(e, ans, CONTACT_SCOPE, CONTACT_PURPOSE, NO_AUTHORITY)?;
                if stop {
                    return Ok(());
                }
                continue;
            }
        };
        ans.granted(CONTACT_SCOPE);
        let d = client::data(&v);
        for u in d
            .get("items")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(p) = user_profile(u) {
                ans.profiles.push(p);
            }
        }
    }
    Ok(())
}

/// Step 3 for the bot: its organization.
async fn tenant(c: &FeishuClient, cx: &ConnCx, ans: &mut ProfileAnswer) -> Result<(), FeishuError> {
    match c.get(cx, "/open-apis/tenant/v2/tenant/query", &[]).await {
        Ok(v) => {
            let t = &client::data(&v)["tenant"];
            if let Some(key) = opt(t, "tenant_key") {
                ans.granted(TENANT_SCOPE);
                ans.tenant = Some(TenantInfo {
                    key,
                    name: opt(t, "name"),
                });
            }
            Ok(())
        }
        Err(e) => absorb(e, ans, TENANT_SCOPE, TENANT_PURPOSE, &[]),
    }
}

/// The handles of `q` that no profile in `ans` names yet, split into `(open_ids, union_ids)`.
fn unnamed(q: &ProfileQuery, ans: &ProfileAnswer) -> (Vec<String>, Vec<String>) {
    let named = |h: &str| {
        ans.profiles
            .iter()
            .any(|p| !p.name.is_empty() && p.handle.as_ref().is_some_and(|x| x.is(h)))
    };
    let (mut open, mut union) = (Vec::new(), Vec::new());
    for h in &q.handles {
        if h.system != USER_SYSTEM {
            continue;
        }
        let all: Vec<&String> = std::iter::once(&h.handle).chain(h.extra.iter()).collect();
        if all.iter().any(|x| named(x)) {
            continue;
        }
        // one id per person is enough: the open_id (this app's) when there is one
        if let Some(o) = all.iter().find(|x| x.starts_with("ou_")) {
            if !open.contains(*o) {
                open.push((*o).clone());
            }
        } else if let Some(u) = all.iter().find(|x| x.starts_with("on_"))
            && !union.contains(*u)
        {
            union.push((*u).clone());
        }
    }
    (open, union)
}

async fn resolve(
    c: &FeishuClient,
    cx: &ConnCx,
    q: &ProfileQuery,
    bot: bool,
) -> Result<ProfileAnswer, FeishuError> {
    let mut ans = ProfileAnswer::default();
    if let Some(chat) = &q.space_ext_id {
        members(c, cx, chat, &mut ans).await?;
    }
    let (open, union) = unnamed(q, &ans);
    if !open.is_empty() {
        batch(c, cx, &open, "open_id", &mut ans).await?;
    }
    if !union.is_empty() && !ans.missing_scopes.iter().any(|(s, _)| s == CONTACT_SCOPE) {
        batch(c, cx, &union, "union_id", &mut ans).await?;
    }
    if q.want_tenant {
        if bot {
            tenant(c, cx, &mut ans).await?;
        } else if let Some(key) = cx.config_str("tenant_key").filter(|k| !k.is_empty()) {
            ans.tenant = Some(TenantInfo {
                key: key.to_string(),
                name: None,
            });
        }
    }
    Ok(ans)
}

fn err(e: FeishuError) -> AdapterError {
    ProviderError::from(e).into()
}

#[async_trait]
impl ProfileAdapter for FeishuApp {
    fn adapter_id(&self) -> &str {
        APP_ADAPTER
    }

    async fn resolve_profiles(
        &self,
        cx: &ConnCx,
        q: &ProfileQuery,
    ) -> Result<ProfileAnswer, AdapterError> {
        resolve(&FeishuClient::for_cx(cx), cx, q, true)
            .await
            .map_err(err)
    }
}

#[async_trait]
impl ProfileAdapter for FeishuUser {
    fn adapter_id(&self) -> &str {
        USER_ADAPTER
    }

    async fn resolve_profiles(
        &self,
        cx: &ConnCx,
        q: &ProfileQuery,
    ) -> Result<ProfileAnswer, AdapterError> {
        resolve(&self.user_client(cx), cx, q, false)
            .await
            .map_err(err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn address_book_users_map() {
        let p = user_profile(&json!({
            "union_id": "on_1", "open_id": "ou_1", "name": " 张伟 ",
            "avatar": {"avatar_72": "https://x/a.png"}, "enterprise_email": "zw@acme.cn",
            "department_ids": ["od_1"], "job_title": " 后端工程师 ",
        }))
        .expect("profile");
        assert_eq!(p.title.as_deref(), Some("后端工程师"));
        assert_eq!(p.name, "张伟");
        assert_eq!(p.handle.as_ref().map(|h| h.handle.as_str()), Some("on_1"));
        assert!(p.handle.as_ref().is_some_and(|h| h.is("ou_1")));
        assert_eq!(p.email.as_deref(), Some("zw@acme.cn"));
        assert!(p.directory);
        assert!(user_profile(&json!({"name": "x"})).is_none());
    }

    #[test]
    fn only_unnamed_handles_are_looked_up() {
        let q = ProfileQuery {
            space_ext_id: None,
            handles: vec![
                HandleRef::new(USER_SYSTEM, "ou_a"),
                HandleRef::new(USER_SYSTEM, "on_b").with_extra("ou_b"),
                HandleRef::new(USER_SYSTEM, "on_c"),
                HandleRef::new("feishu_app", "cli_x"),
            ],
            want_tenant: false,
        };
        let mut ans = ProfileAnswer::default();
        ans.profiles.push(Profile {
            handle: Some(HandleRef::new(USER_SYSTEM, "ou_a")),
            name: "甲".into(),
            ..Profile::default()
        });
        assert_eq!(
            unnamed(&q, &ans),
            (vec!["ou_b".to_string()], vec!["on_c".to_string()])
        );
    }
}
