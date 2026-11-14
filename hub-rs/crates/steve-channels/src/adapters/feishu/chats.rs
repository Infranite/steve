//! Groups and members:
//!
//! - [`space_info`]: `GET im/v1/chats/{id}` → name, kind (`chat_mode`: `group`→group,
//!   `topic`→channel, `p2p`→dm), member count (`user_count`), `has_external` (`external`);
//! - [`members`]: `GET im/v1/chats/{id}/members?member_id_type=union_id`, every page; a member of
//!   another `tenant_key` than the chat's is external;
//! - [`bot_chats`] / [`sync_bot_chats`]: the groups the bot is in (`GET im/v1/chats`), made spaces
//!   when the bot is connected (`configured=0`, no “新群” decision each);
//! - [`parse_chat_event`]: `im.chat.member.bot.added_v1` / `bot.deleted_v1` /
//!   `user.added_v1` / `user.deleted_v1`, `im.chat.updated_v1`, `im.chat.disbanded_v1` →
//!   [`SpaceEvent`] for [`crate::Channels::on_space_event`].

use serde_json::Value;
use steve_modules::ApiError;
use steve_modules::accounts::connections::ConnCx;

use super::auth::USER_SYSTEM;
use super::client::{self, FeishuClient, FeishuError};
use crate::channel::{HandleRef, MemberInfo, SpaceEvent, SpaceInfo, SpaceKind};
use crate::{ChannelResult, Channels};

pub const BOT_ADDED: &str = "im.chat.member.bot.added_v1";
pub const BOT_DELETED: &str = "im.chat.member.bot.deleted_v1";
pub const USER_ADDED: &str = "im.chat.member.user.added_v1";
pub const USER_DELETED: &str = "im.chat.member.user.deleted_v1";
pub const CHAT_UPDATED: &str = "im.chat.updated_v1";
pub const CHAT_DISBANDED: &str = "im.chat.disbanded_v1";

/// Pages read at most: 100 pages of 100 is the largest Feishu group, and a runaway page token must
/// not loop forever.
const MAX_PAGES: usize = 100;

fn s<'a>(v: &'a Value, k: &str) -> &'a str {
    v.get(k).and_then(Value::as_str).unwrap_or("")
}

fn opt(v: &Value, k: &str) -> Option<String> {
    Some(s(v, k).trim().to_string()).filter(|x| !x.is_empty())
}

/// A chat id that goes into a path.
fn chat_id(id: &str) -> Result<&str, FeishuError> {
    let id = id.trim();
    if id.is_empty() || id.len() > 200 || id.contains(['/', '?', '#', '%']) || id.contains("..") {
        return Err(FeishuError::Api {
            code: 0,
            msg: "群 id 不对".into(),
            status: 400,
        });
    }
    Ok(id)
}

/// `chat_mode` → kind.
pub fn kind_of(chat_mode: &str) -> SpaceKind {
    match chat_mode {
        "p2p" => SpaceKind::Dm,
        "topic" => SpaceKind::Channel,
        _ => SpaceKind::Group,
    }
}

fn count(v: &Value, k: &str) -> i64 {
    v.get(k)
        .and_then(|x| x.as_i64().or_else(|| x.as_str()?.parse().ok()))
        .unwrap_or(0)
}

/// `GET im/v1/chats/{id}`, the whole answer.
pub async fn chat(cx: &ConnCx, ext_id: &str) -> Result<Value, FeishuError> {
    let id = chat_id(ext_id)?;
    FeishuClient::for_cx(cx)
        .get(
            cx,
            &format!("/open-apis/im/v1/chats/{id}"),
            &[("user_id_type", "union_id")],
        )
        .await
}

/// The chat's info.
pub async fn space_info(cx: &ConnCx, ext_id: &str) -> Result<SpaceInfo, FeishuError> {
    let v = chat(cx, ext_id).await?;
    let d = client::data(&v);
    Ok(SpaceInfo {
        name: s(d, "name").trim().to_string(),
        kind: kind_of(s(d, "chat_mode")),
        member_count: count(d, "user_count"),
        has_external: d.get("external").and_then(Value::as_bool).unwrap_or(false),
    })
}

/// All members (people; Feishu does not list bots here), by `union_id`.
pub async fn members(cx: &ConnCx, ext_id: &str) -> Result<Vec<MemberInfo>, FeishuError> {
    let info = chat(cx, ext_id).await?;
    let tenant = s(client::data(&info), "tenant_key").to_string();
    let c = FeishuClient::for_cx(cx);
    let path = format!("/open-apis/im/v1/chats/{}/members", chat_id(ext_id)?);
    let mut out = Vec::new();
    let mut token = String::new();
    for _ in 0..MAX_PAGES {
        let mut q = vec![("member_id_type", "union_id"), ("page_size", "100")];
        if !token.is_empty() {
            q.push(("page_token", token.as_str()));
        }
        let v = c.get(cx, &path, &q).await?;
        let d = client::data(&v);
        for m in d
            .get("items")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let id = s(m, "member_id");
            if id.is_empty() {
                continue;
            }
            let tk = s(m, "tenant_key");
            out.push(MemberInfo {
                handle: HandleRef::new(USER_SYSTEM, id),
                aliases: Vec::new(),
                name: s(m, "name").to_string(),
                role: "member".into(),
                external: !tk.is_empty() && !tenant.is_empty() && tk != tenant,
            });
        }
        let more = d.get("has_more").and_then(Value::as_bool).unwrap_or(false);
        token = s(d, "page_token").to_string();
        if !more || token.is_empty() {
            break;
        }
    }
    Ok(out)
}

/// One group the bot is in, as `GET im/v1/chats` lists it (no member count there).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BotChat {
    pub chat_id: String,
    pub name: String,
    pub kind: SpaceKind,
    pub external: bool,
}

/// The groups the bot is in (Feishu lists no p2p chats for a bot), every page.
pub async fn bot_chats(cx: &ConnCx) -> Result<Vec<BotChat>, FeishuError> {
    let c = FeishuClient::for_cx(cx);
    let mut out = Vec::new();
    let mut token = String::new();
    for _ in 0..MAX_PAGES {
        let mut q = vec![("page_size", "100")];
        if !token.is_empty() {
            q.push(("page_token", token.as_str()));
        }
        let v = c.get(cx, "/open-apis/im/v1/chats", &q).await?;
        let d = client::data(&v);
        for x in d
            .get("items")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            let id = s(x, "chat_id");
            let status = s(x, "chat_status");
            if id.is_empty() || !(status.is_empty() || status == "normal") {
                continue;
            }
            out.push(BotChat {
                chat_id: id.to_string(),
                name: s(x, "name").trim().to_string(),
                kind: kind_of(s(x, "chat_mode")),
                external: x.get("external").and_then(Value::as_bool).unwrap_or(false),
            });
        }
        let more = d.get("has_more").and_then(Value::as_bool).unwrap_or(false);
        token = s(d, "page_token").to_string();
        if !more || token.is_empty() {
            break;
        }
    }
    Ok(out)
}

/// Creates the spaces of the groups bot connection `conn` of `ws` is in (on connect, or from the
/// spaces page). Returns how many were new.
pub async fn sync_bot_chats(channels: &Channels, ws: &str, conn: &str) -> ChannelResult<usize> {
    let cx = channels
        .connections()
        .cx(ws, conn)
        .map_err(|e| ApiError::rule(e.to_string()))?;
    let chats = bot_chats(&cx)
        .await
        .map_err(|e| ApiError::rule(e.to_string()))?;
    let infos: Vec<(String, SpaceInfo)> = chats
        .into_iter()
        .map(|c| {
            (
                c.chat_id,
                SpaceInfo {
                    name: c.name,
                    kind: c.kind,
                    member_count: 0,
                    has_external: c.external,
                },
            )
        })
        .collect();
    channels.sync_spaces(ws, conn, &infos).await
}

/// A user of a member event: `union_id` with the `open_id` valid under this app.
fn user_handle(id: &Value) -> Option<HandleRef> {
    let (union, open) = (s(id, "union_id"), s(id, "open_id"));
    if !union.is_empty() {
        return Some(HandleRef::new(USER_SYSTEM, union).with_extra(open));
    }
    (!open.is_empty()).then(|| HandleRef::new(USER_SYSTEM, open))
}

fn flag(ev: &Value, k: &str) -> Option<bool> {
    ev.get(k).and_then(Value::as_bool)
}

/// Maps a member or chat event (schema 2.0). `None` for other event types and for events without
/// a `chat_id`.
pub fn parse_chat_event(env: &Value) -> Option<SpaceEvent> {
    let header = &env["header"];
    let ev = &env["event"];
    let chat = opt(ev, "chat_id")?;
    let external = flag(ev, "external");
    let tenant = s(header, "tenant_key");
    let users = || {
        ev.get("users")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
    };
    Some(match s(header, "event_type") {
        BOT_ADDED => SpaceEvent::Joined {
            space_ext_id: chat,
            name: opt(ev, "name"),
            has_external: external,
        },
        BOT_DELETED => SpaceEvent::Left {
            space_ext_id: chat,
            archived: false,
        },
        CHAT_DISBANDED => SpaceEvent::Left {
            space_ext_id: chat,
            archived: true,
        },
        USER_ADDED => SpaceEvent::MembersAdded {
            space_ext_id: chat,
            members: users()
                .filter_map(|u| {
                    let tk = s(u, "tenant_key");
                    Some(MemberInfo {
                        handle: user_handle(&u["user_id"])?,
                        aliases: Vec::new(),
                        name: s(u, "name").to_string(),
                        role: "member".into(),
                        external: !tk.is_empty() && !tenant.is_empty() && tk != tenant,
                    })
                })
                .collect(),
            has_external: external,
        },
        USER_DELETED => SpaceEvent::MembersRemoved {
            space_ext_id: chat,
            members: users().filter_map(|u| user_handle(&u["user_id"])).collect(),
            has_external: external,
        },
        CHAT_UPDATED => SpaceEvent::Updated {
            space_ext_id: chat,
            name: opt(&ev["after_change"], "name"),
            has_external: external,
        },
        _ => return None,
    })
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

    #[test]
    fn maps_member_and_chat_events() {
        assert_eq!(
            parse_chat_event(&fixture("bot_added")),
            Some(SpaceEvent::Joined {
                space_ext_id: "oc_newgroup0000000000000000000001".into(),
                name: Some("新项目群".into()),
                has_external: Some(false),
            })
        );
        assert_eq!(
            parse_chat_event(&fixture("bot_deleted")),
            Some(SpaceEvent::Left {
                space_ext_id: "oc_group000000000000000000000001".into(),
                archived: false,
            })
        );
        assert_eq!(
            parse_chat_event(&fixture("chat_disbanded")),
            Some(SpaceEvent::Left {
                space_ext_id: "oc_topic000000000000000000000001".into(),
                archived: true,
            })
        );
        let Some(SpaceEvent::MembersAdded {
            members,
            has_external,
            ..
        }) = parse_chat_event(&fixture("user_added"))
        else {
            panic!("members added")
        };
        assert_eq!(has_external, Some(true));
        assert_eq!(members.len(), 1);
        assert_eq!(members[0].name, "孙采购");
        assert!(members[0].external);
        assert_eq!(
            members[0].handle.handle,
            "on_ext0000000000000000000000000002"
        );
        assert_eq!(
            members[0].handle.extra,
            ["ou_ext0000000000000000000000000002"]
        );
        let Some(SpaceEvent::MembersRemoved { members, .. }) =
            parse_chat_event(&fixture("user_deleted"))
        else {
            panic!("members removed")
        };
        assert_eq!(members.len(), 1);
        assert_eq!(
            parse_chat_event(&fixture("chat_updated")),
            Some(SpaceEvent::Updated {
                space_ext_id: "oc_group000000000000000000000001".into(),
                name: Some("平台值班群（夜班）".into()),
                has_external: Some(false),
            })
        );
        assert_eq!(parse_chat_event(&fixture("message_p2p_text")), None);
    }

    #[test]
    fn kinds_and_ids() {
        assert_eq!(kind_of("topic"), SpaceKind::Channel);
        assert_eq!(kind_of("p2p"), SpaceKind::Dm);
        assert_eq!(kind_of("group"), SpaceKind::Group);
        assert_eq!(kind_of(""), SpaceKind::Group);
        assert!(chat_id("oc_1").is_ok());
        assert!(chat_id("oc_1/members").is_err());
        assert!(chat_id("").is_err());
    }
}
