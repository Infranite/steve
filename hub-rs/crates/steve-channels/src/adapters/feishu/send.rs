//! Sending, replying and recalling as the bot. Only C6's
//! `outbound::send` calls these, after the rules or you approved the message: nothing here
//! decides to send.
//!
//! - a new message: `POST im/v1/messages?receive_id_type=chat_id|open_id|union_id` (by the id's
//!   prefix); a reply: `POST im/v1/messages/{id}/reply` with `reply_in_thread`;
//! - text only in phase 1: `{"text": "<at user_id=\"ou_…\"></at> …"}`, at most [`MAX_TEXT`]
//!   characters (longer ones are cut and end with [`TRUNCATED`]);
//! - `uuid` = the outbound `cm_…` (one is made up when absent): Feishu drops a repeat within an
//!   hour, so the rate-limit retries ([`ratelimit::with_retries`], at most
//!   [`ratelimit::SEND_RETRIES`]) and C6's own retries cannot double-send;
//! - recall: `DELETE im/v1/messages/{id}` (the bot can only recall its own; the tenant sets how
//!   long).

use reqwest::Method;
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use steve_modules::accounts::connections::ConnCx;
use steve_modules::accounts::connections::oauth::random_token;

use super::client::{FeishuClient, FeishuError, data};
use super::ratelimit::{self, Limiter};
use crate::ChannelResult;
use crate::channel::{OutContent, ReactionKind, SendTarget, SpaceKind};

/// The longest text sent, in characters.
pub const MAX_TEXT: usize = 4000;
/// What a cut text ends with.
pub const TRUNCATED: &str = "（太长，已截断）";
/// Feishu's limit on `uuid`.
pub const MAX_UUID: usize = 50;
/// How long Feishu drops a repeat of a send with the same `uuid` (its API documents one hour).
pub const UUID_DEDUP: std::time::Duration = std::time::Duration::from_secs(3600);

/// What Feishu accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sent {
    /// `om_…`: the outbound `chan_messages.external_id`.
    pub message_id: String,
    pub chat_id: String,
    pub thread_id: Option<String>,
    /// The `uuid` sent.
    pub uuid: String,
    /// Rate-limit retries it took.
    pub retries: u32,
}

/// The text with the @s in front, cut to [`MAX_TEXT`] characters.
pub fn text_body(text: &str, at: &[String]) -> String {
    let n = text.chars().count();
    let body = if n > MAX_TEXT {
        let keep = MAX_TEXT - TRUNCATED.chars().count();
        let mut t: String = text.chars().take(keep).collect();
        t.push_str(TRUNCATED);
        t
    } else {
        text.to_string()
    };
    let mut out = String::new();
    for id in at.iter().map(|s| s.trim()).filter(|s| !s.is_empty()) {
        // an id is never markup; anything else is not an id Feishu knows
        if id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            out.push_str(&format!("<at user_id=\"{id}\"></at> "));
        }
    }
    out.push_str(&body);
    out
}

/// `receive_id_type` of a send target: users by `open_id` (`ou_`) or `union_id` (`on_`), the rest
/// are chats.
pub fn receive_id_type(id: &str) -> &'static str {
    if id.starts_with("ou_") {
        "open_id"
    } else if id.starts_with("on_") {
        "union_id"
    } else {
        "chat_id"
    }
}

/// Whether a reply goes into the message's topic: in a topic group always, else
/// when the message is in a topic already (its `thread_key` is a Feishu thread id, `omt_…`).
pub fn reply_in_thread(kind: SpaceKind, thread_key: Option<&str>) -> bool {
    kind == SpaceKind::Channel || thread_key.is_some_and(|t| t.starts_with("omt_"))
}

/// The id to @ an account by under bot connection `conn`: its `open_id` there (C6 puts it into
/// [`OutContent::at`] for a group reply). `None` when the bot never saw one.
pub fn at_handle(
    c: &Connection,
    ws: &str,
    conn: &str,
    account: &str,
) -> ChannelResult<Option<String>> {
    Ok(c.query_row(
        "SELECT handle FROM account_handles WHERE workspace_id = ? AND account_id = ? AND handle LIKE 'ou\\_%' ESCAPE '\\'
           ORDER BY connection_id = ? DESC, created LIMIT 1",
        params![ws, account, conn],
        |r| r.get(0),
    )
    .optional()?)
}

/// An id that goes into a path.
fn path_id(id: &str) -> Result<&str, FeishuError> {
    let id = id.trim();
    if id.is_empty() || id.len() > 200 || id.contains(['/', '?', '#', '%']) || id.contains("..") {
        return Err(FeishuError::Api {
            code: 0,
            msg: "消息 id 不对".into(),
            status: 400,
        });
    }
    Ok(id)
}

fn uuid_of(content: &OutContent) -> Result<String, FeishuError> {
    match content
        .uuid
        .as_deref()
        .map(str::trim)
        .filter(|u| !u.is_empty())
    {
        Some(u) => Ok(u.chars().take(MAX_UUID).collect()),
        None => random_token(16).map_err(FeishuError::Token),
    }
}

fn sent(v: &Value, uuid: String, retries: u32) -> Result<Sent, FeishuError> {
    let d = data(v);
    let s = |k: &str| {
        d.get(k)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(String::from)
    };
    Ok(Sent {
        message_id: s("message_id").ok_or(FeishuError::Decode)?,
        chat_id: s("chat_id").unwrap_or_default(),
        thread_id: s("thread_id"),
        uuid,
        retries,
    })
}

/// Sends `content` to `to` as the bot of connection `cx`, waiting its turn ([`Limiter::global`])
/// and retrying rate limits with the same `uuid`.
pub async fn send(cx: &ConnCx, to: &SendTarget, content: &OutContent) -> Result<Sent, FeishuError> {
    send_with(Limiter::global(), cx, to, content).await
}

/// [`send`] with an explicit limiter.
pub async fn send_with(
    limiter: &Limiter,
    cx: &ConnCx,
    to: &SendTarget,
    content: &OutContent,
) -> Result<Sent, FeishuError> {
    let uuid = uuid_of(content)?;
    let body_text = json!({ "text": text_body(&content.text, &content.at) }).to_string();
    let client = FeishuClient::for_cx(cx);
    let conn = cx.id().to_string();
    let (path, query, body, chat) = match to {
        SendTarget::Space { ext_id } => {
            let id = ext_id.trim();
            if id.is_empty() {
                return Err(FeishuError::Api {
                    code: 0,
                    msg: "没有发送对象".into(),
                    status: 400,
                });
            }
            (
                "/open-apis/im/v1/messages".to_string(),
                vec![("receive_id_type", receive_id_type(id))],
                json!({"receive_id": id, "msg_type": "text", "content": body_text, "uuid": uuid}),
                Some(id.to_string()),
            )
        }
        SendTarget::Reply {
            ext_msg_id,
            in_thread,
        } => (
            format!("/open-apis/im/v1/messages/{}/reply", path_id(ext_msg_id)?),
            Vec::new(),
            json!({"msg_type": "text", "content": body_text, "reply_in_thread": in_thread, "uuid": uuid}),
            content.space_ext_id.clone(),
        ),
    };
    let (v, retries) = ratelimit::with_retries(ratelimit::SEND_RETRIES, || async {
        limiter.bot_send(&conn, chat.as_deref()).await;
        client.post(cx, &path, &query, &body).await
    })
    .await?;
    sent(&v, uuid, retries)
}

/// Feishu's `emoji_type` for each reaction kind (open platform “表情文案说明”): `OnIt` (在处理),
/// `DONE` (完成), `CrossMark` (❌).
pub fn emoji_type(kind: ReactionKind) -> &'static str {
    match kind {
        ReactionKind::Ack => EMOJI_ACK,
        ReactionKind::Done => EMOJI_DONE,
        ReactionKind::Failed => EMOJI_FAILED,
    }
}
pub const EMOJI_ACK: &str = "OnIt";
pub const EMOJI_DONE: &str = "DONE";
pub const EMOJI_FAILED: &str = "CrossMark";

/// `POST im/v1/messages/{id}/reactions {"reaction_type":{"emoji_type":…}}` with `client`'s token
/// (the bot's tenant token, or your user token for `/bot` on your account); the `reaction_id`.
/// Needs `im:message.reactions:write_only` (or `im:message`).
pub async fn add_reaction_with(
    client: &FeishuClient,
    cx: &ConnCx,
    ext_msg_id: &str,
    emoji: &str,
) -> Result<String, FeishuError> {
    let path = format!(
        "/open-apis/im/v1/messages/{}/reactions",
        path_id(ext_msg_id)?
    );
    let body = json!({"reaction_type": {"emoji_type": emoji}});
    let (v, _) = ratelimit::with_retries(1, || client.post(cx, &path, &[], &body)).await?;
    data(&v)
        .get("reaction_id")
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .ok_or(FeishuError::Decode)
}

/// `DELETE im/v1/messages/{id}/reactions/{reaction_id}`: only the one who added it can.
pub async fn remove_reaction_with(
    client: &FeishuClient,
    cx: &ConnCx,
    ext_msg_id: &str,
    reaction_id: &str,
) -> Result<(), FeishuError> {
    let path = format!(
        "/open-apis/im/v1/messages/{}/reactions/{}",
        path_id(ext_msg_id)?,
        path_id(reaction_id)?
    );
    ratelimit::with_retries(1, || client.request(cx, Method::DELETE, &path, &[], None))
        .await
        .map(|_| ())
}

/// Recalls a message the bot sent.
pub async fn recall(cx: &ConnCx, ext_msg_id: &str) -> Result<(), FeishuError> {
    let path = format!("/open-apis/im/v1/messages/{}", path_id(ext_msg_id)?);
    let client = FeishuClient::for_cx(cx);
    ratelimit::with_retries(ratelimit::SEND_RETRIES, || {
        client.request(cx, Method::DELETE, &path, &[], None)
    })
    .await
    .map(|_| ())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn text_with_ats_and_cut() {
        assert_eq!(text_body("好的", &[]), "好的");
        assert_eq!(
            text_body("好的", &["ou_a".into(), " ".into(), "<x>".into()]),
            "<at user_id=\"ou_a\"></at> 好的"
        );
        let long = "字".repeat(MAX_TEXT + 1);
        let t = text_body(&long, &[]);
        assert_eq!(t.chars().count(), MAX_TEXT);
        assert!(t.ends_with(TRUNCATED));
        let exact = "字".repeat(MAX_TEXT);
        assert_eq!(text_body(&exact, &[]), exact);
    }

    #[test]
    fn targets_and_threads() {
        assert_eq!(receive_id_type("oc_1"), "chat_id");
        assert_eq!(receive_id_type("ou_1"), "open_id");
        assert_eq!(receive_id_type("on_1"), "union_id");
        assert!(reply_in_thread(SpaceKind::Channel, None));
        assert!(reply_in_thread(SpaceKind::Group, Some("omt_1")));
        assert!(!reply_in_thread(SpaceKind::Group, Some("om_1")));
        assert!(!reply_in_thread(SpaceKind::Dm, None));
    }

    #[test]
    fn emoji_per_kind() {
        assert_eq!(emoji_type(ReactionKind::Ack), "OnIt");
        assert_eq!(emoji_type(ReactionKind::Done), "DONE");
        assert_eq!(emoji_type(ReactionKind::Failed), "CrossMark");
    }

    #[test]
    fn ids_and_answers() {
        assert!(path_id("om_1").is_ok());
        for bad in ["", "a/b", "a?b", "..", "a%2f"] {
            assert!(path_id(bad).is_err(), "{bad}");
        }
        let mut c = OutContent::text("x");
        c.uuid = Some("u".repeat(80));
        assert_eq!(uuid_of(&c).unwrap().len(), MAX_UUID);
        c.uuid = None;
        assert!(!uuid_of(&c).unwrap().is_empty());
        let v = json!({"code": 0, "data": {"message_id": "om_1", "chat_id": "oc_1"}});
        assert_eq!(sent(&v, "u".into(), 1).unwrap().message_id, "om_1");
        assert_eq!(
            sent(&json!({"code": 0, "data": {}}), "u".into(), 0),
            Err(FeishuError::Decode)
        );
    }
}
