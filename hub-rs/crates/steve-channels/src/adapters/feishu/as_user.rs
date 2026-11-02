//! Sending as you (F6, feishu.md §2.2 “以你本人发送”, feishu-lark-cli.md §2.1): the bot's send and
//! reply endpoints with the `feishu_user` connection's user token, `receive_id_type=chat_id`.
//!
//! Feishu allows each user at most 5 sends per second (code 230020). A limited send waits (the
//! `x-ogw-ratelimit-reset` header when there is one, else one second) and is tried once more with
//! the same `uuid`, so Feishu drops it if the first one did get through; a second refusal is the
//! caller's error. Approval and the trigger phrase (only you, only where configured) are the
//! outbound module's (C6) business: this only sends.

use async_trait::async_trait;
use serde_json::{Value, json};
use steve_modules::accounts::connections::ConnCx;
use steve_modules::accounts::connections::oauth::random_token;

use super::auth::{FeishuUser, USER_ADAPTER};
use super::client::{FeishuClient, FeishuError, NO_SCOPE_CODES, data};
use super::ratelimit;

/// What to send: a Feishu `msg_type` and its `content` object (`{"text": "…"}` for `text`, the
/// `post` / `interactive` structures otherwise).
#[derive(Debug, Clone, PartialEq)]
pub struct UserMessage {
    pub msg_type: String,
    pub content: Value,
    /// Feishu's de-duplication key (≤ 50 characters, one hour). One is made up when absent, so the
    /// retry after a rate limit can't double-send.
    pub uuid: Option<String>,
}

impl UserMessage {
    /// A plain text message.
    pub fn text(s: impl Into<String>) -> Self {
        Self {
            msg_type: "text".into(),
            content: json!({ "text": s.into() }),
            uuid: None,
        }
    }
}

/// A message Feishu accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SentAsUser {
    /// `om_…`: the `external_id` of the `chan_messages` row.
    pub message_id: String,
    pub chat_id: String,
    /// Feishu's `create_time` (Unix ms as text), when given.
    pub create_time: Option<String>,
    pub thread_id: Option<String>,
    /// The `uuid` sent (the given one or the one made up).
    pub uuid: String,
    /// The send was limited once and went through on the retry.
    pub retried: bool,
}

/// What the outbound module (C6) calls to send as you. Implemented by [`FeishuUser`]; the
/// integrator hands C6 the same `Arc<FeishuUser>` registered as the `feishu_user` provider.
#[async_trait]
pub trait SendAsUser: Send + Sync {
    /// A new message in chat `chat_id` (`oc_…`), as the user of connection `cx`.
    async fn send_as_user(
        &self,
        cx: &ConnCx,
        chat_id: &str,
        msg: &UserMessage,
    ) -> Result<SentAsUser, FeishuError>;

    /// A reply to `message_id` (`om_…`); `in_thread` opens or continues its topic.
    async fn reply_as_user(
        &self,
        cx: &ConnCx,
        message_id: &str,
        msg: &UserMessage,
        in_thread: bool,
    ) -> Result<SentAsUser, FeishuError>;

    /// A reaction on `message_id` as you (`/bot` on your own account: the “收到 → 完成” marks are
    /// yours, like the reply). The provider's reaction id. Default: unsupported.
    async fn react_as_user(
        &self,
        cx: &ConnCx,
        message_id: &str,
        emoji: &str,
    ) -> Result<String, FeishuError> {
        let _ = (cx, message_id, emoji);
        Err(FeishuError::Token(
            steve_modules::accounts::connections::ProviderError::Unsupported,
        ))
    }

    /// Takes your reaction off again. Default: unsupported.
    async fn unreact_as_user(
        &self,
        cx: &ConnCx,
        message_id: &str,
        reaction_id: &str,
    ) -> Result<(), FeishuError> {
        let _ = (cx, message_id, reaction_id);
        Err(FeishuError::Token(
            steve_modules::accounts::connections::ProviderError::Unsupported,
        ))
    }
}

/// The grant lacks `im:message.send_as_user` (or `im:message`): no call is made. An empty grant
/// means Feishu didn't say; Feishu decides then.
fn check_scopes(cx: &ConnCx) -> Result<(), FeishuError> {
    if cx.adapter() != USER_ADAPTER {
        return Err(FeishuError::Token(
            steve_modules::accounts::connections::ProviderError::Config(
                "以你本人发送要用“飞书 · 你本人”的连接".into(),
            ),
        ));
    }
    let granted = cx.scopes();
    let has = |s: &str| granted.iter().any(|g| g == s);
    if !granted.is_empty() && !(has("im:message") && has("im:message.send_as_user")) {
        return Err(FeishuError::NoScope {
            code: NO_SCOPE_CODES[1],
            msg: "你本人的授权里没有“以你本人身份发送”（im:message.send_as_user），请重新授权"
                .into(),
            scopes: vec!["im:message.send_as_user".into()],
            help_url: None,
        });
    }
    Ok(())
}

fn uuid_of(msg: &UserMessage) -> Result<String, FeishuError> {
    match msg.uuid.as_deref().map(str::trim).filter(|u| !u.is_empty()) {
        Some(u) => Ok(u.chars().take(50).collect()),
        None => random_token(16).map_err(FeishuError::Token),
    }
}

fn sent(v: &Value, uuid: String, retried: bool) -> Result<SentAsUser, FeishuError> {
    let d = data(v);
    let s = |k: &str| {
        d.get(k)
            .and_then(Value::as_str)
            .filter(|s| !s.is_empty())
            .map(String::from)
    };
    Ok(SentAsUser {
        message_id: s("message_id").ok_or(FeishuError::Decode)?,
        chat_id: s("chat_id").unwrap_or_default(),
        create_time: s("create_time"),
        thread_id: s("thread_id"),
        uuid,
        retried,
    })
}

/// One POST, and once more after a rate limit (the shared [`ratelimit::with_retries`]).
async fn post_with_one_retry(
    client: &FeishuClient,
    cx: &ConnCx,
    path: &str,
    query: &[(&str, &str)],
    body: &Value,
) -> Result<(Value, bool), FeishuError> {
    ratelimit::with_retries(ratelimit::USER_RETRIES, || {
        client.post(cx, path, query, body)
    })
    .await
    .map(|(v, n)| (v, n > 0))
}

#[async_trait]
impl SendAsUser for FeishuUser {
    async fn send_as_user(
        &self,
        cx: &ConnCx,
        chat_id: &str,
        msg: &UserMessage,
    ) -> Result<SentAsUser, FeishuError> {
        check_scopes(cx)?;
        let uuid = uuid_of(msg)?;
        let body = json!({
            "receive_id": chat_id,
            "msg_type": msg.msg_type,
            "content": msg.content.to_string(),
            "uuid": uuid,
        });
        let (v, retried) = post_with_one_retry(
            &self.user_client(cx),
            cx,
            "/open-apis/im/v1/messages",
            &[("receive_id_type", "chat_id")],
            &body,
        )
        .await?;
        sent(&v, uuid, retried)
    }

    async fn reply_as_user(
        &self,
        cx: &ConnCx,
        message_id: &str,
        msg: &UserMessage,
        in_thread: bool,
    ) -> Result<SentAsUser, FeishuError> {
        check_scopes(cx)?;
        if message_id.is_empty() || message_id.contains(['/', '?', '#']) {
            return Err(FeishuError::Api {
                code: 0,
                msg: "消息 id 不对".into(),
                status: 400,
            });
        }
        let uuid = uuid_of(msg)?;
        let body = json!({
            "msg_type": msg.msg_type,
            "content": msg.content.to_string(),
            "uuid": uuid,
            "reply_in_thread": in_thread,
        });
        let path = format!("/open-apis/im/v1/messages/{message_id}/reply");
        let (v, retried) =
            post_with_one_retry(&self.user_client(cx), cx, &path, &[], &body).await?;
        sent(&v, uuid, retried)
    }

    /// With your user token: `im:message` (which you granted to send as you) covers reactions.
    async fn react_as_user(
        &self,
        cx: &ConnCx,
        message_id: &str,
        emoji: &str,
    ) -> Result<String, FeishuError> {
        super::send::add_reaction_with(&self.user_client(cx), cx, message_id, emoji).await
    }

    async fn unreact_as_user(
        &self,
        cx: &ConnCx,
        message_id: &str,
        reaction_id: &str,
    ) -> Result<(), FeishuError> {
        super::send::remove_reaction_with(&self.user_client(cx), cx, message_id, reaction_id).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_sent_message() {
        let v = json!({"code": 0, "data": {"message_id": "om_1", "chat_id": "oc_1", "create_time": "1700000000000"}});
        let s = sent(&v, "u".into(), true).expect("sent");
        assert_eq!(s.message_id, "om_1");
        assert_eq!(s.chat_id, "oc_1");
        assert!(s.retried);
        assert_eq!(
            sent(&json!({"code": 0, "data": {}}), "u".into(), false),
            Err(FeishuError::Decode)
        );
    }

    #[test]
    fn uuids_are_kept_or_made() {
        let mut m = UserMessage::text("hi");
        assert_eq!(m.content, json!({"text": "hi"}));
        let a = uuid_of(&m).expect("uuid");
        assert!(!a.is_empty() && a.len() <= 50);
        m.uuid = Some("x".repeat(80));
        assert_eq!(uuid_of(&m).expect("uuid").len(), 50);
    }
}
