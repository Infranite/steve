//! Errors of the new module APIs. Body `{"error":"<中文>"}`; unlike the old API,
//! domain errors are never 500: validation and rule refusals are 400, owner-only actions 403,
//! missing/foreign/invisible 404, state conflicts 409.

use axum::Json;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use serde_json::json;
use steve_store::StoreError;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ApiError {
    /// `400 参数不对：<field>、<field>` (zod-style dotted paths), in the request's language.
    #[error("{}", invalid_message(.0))]
    Invalid(Vec<String>),
    /// `404 没有这个<noun>`: not there, in another workspace, or not visible to the caller. A noun
    /// in Latin letters gets a space, as the TS Hub writes it (“没有这个 Bot”). The noun stays the Chinese
    /// one (callers match on it); the message is in the request's language ([`not_found_message`]).
    #[error("{}", not_found_message(.0))]
    NotFound(String),
    /// `409`: already handled, already merged, duplicate name, stale draft.
    #[error("{0}")]
    Conflict(String),
    /// `400`: a domain rule refused (“不能删除你自己”).
    #[error("{0}")]
    Rule(String),
    /// `403`: allowed for someone else (“只有工作空间的所有者能改内置来源”).
    #[error("{0}")]
    Forbidden(String),
    /// `401`: no caller on the request (the integrator's auth layer is missing).
    #[error("unauthenticated")]
    Unauthenticated,
    /// `500`: storage or a bug. The message is logged, the client gets a generic one.
    #[error("{0}")]
    Internal(String),
}

/// "参数不对：a、b" / "Invalid parameters: a, b".
fn invalid_message(paths: &[String]) -> String {
    let fields = paths.join(&steve_i18n::t!("modules.error.list_sep"));
    steve_i18n::t!("modules.error.invalid", fields = fields)
}

/// "没有这个联系人" / "No such contact": the whole sentence of a known noun (this crate's and the crates built on
/// [`ApiError`]), in the request's language. A noun not in the table keeps the zh-CN sentence.
fn not_found_message(noun: &str) -> String {
    use steve_i18n::t;
    match noun {
        "联系人" => t!("modules.not_found.contact"),
        "组织" => t!("modules.not_found.org"),
        "团队" => t!("modules.not_found.team"),
        "空间" => t!("modules.not_found.space"),
        "账号" => t!("modules.not_found.account"),
        "系统" => t!("modules.not_found.system"),
        "规则" => t!("modules.not_found.rule"),
        "决定" => t!("modules.not_found.decision"),
        "面板" => t!("modules.not_found.panel"),
        "操作" => t!("modules.not_found.action"),
        "任职" => t!("modules.not_found.job"),
        "属性" => t!("modules.not_found.attribute"),
        "负责人提议" => t!("modules.not_found.steward_proposal"),
        "合并记录" => t!("modules.not_found.merge"),
        "合并提议" => t!("modules.not_found.merge_proposal"),
        "记忆" => t!("modules.not_found.memory"),
        "群" => t!("modules.not_found.group"),
        "连接" => t!("modules.not_found.connection"),
        "接入" => t!("modules.not_found.connection"),
        "接入方式" => t!("modules.not_found.provider"),
        "设备码登录" => t!("modules.not_found.device_login"),
        "设备" => t!("modules.not_found.device"),
        "身份" => t!("modules.not_found.persona"),
        "机器人" => t!("modules.not_found.bot"),
        "Bot" => t!("modules.not_found.bot_latin"),
        "事项" => t!("modules.not_found.item"),
        "项目" => t!("modules.not_found.project"),
        "会话" => t!("modules.not_found.conversation"),
        "授权" => t!("modules.not_found.grant"),
        "条目" => t!("modules.not_found.vault_item"),
        "字段" => t!("modules.not_found.field"),
        "使用记录" => t!("modules.not_found.secret_use"),
        "通行密钥" => t!("modules.not_found.passkey"),
        "窗口" => t!("modules.not_found.window"),
        "记录" => t!("modules.not_found.record"),
        "文档" => t!("modules.not_found.doc"),
        "笔记" => t!("modules.not_found.note"),
        "附件" => t!("modules.not_found.file"),
        "知识来源" => t!("modules.not_found.source"),
        "知识库" => t!("modules.not_found.knowledge_base"),
        "工作空间成员" => t!("modules.not_found.member"),
        "草稿" => t!("modules.not_found.draft"),
        "仓库" => t!("modules.not_found.repo"),
        "PR" => t!("modules.not_found.pr"),
        "Issue" => t!("modules.not_found.issue"),
        "保险库" => t!("modules.not_found.vault"),
        "模板" => t!("modules.not_found.template"),
        "运行" => t!("modules.not_found.run"),
        "消息" => t!("modules.not_found.message"),
        "对话" => t!("modules.not_found.chat"),
        "邮件" => t!("modules.not_found.mail"),
        "对外身份" => t!("modules.not_found.outward_identity"),
        "往来" => t!("modules.not_found.thread"),
        "流程" => t!("modules.not_found.flow"),
        "版本" => t!("modules.not_found.version"),
        "标签" => t!("modules.not_found.tag"),
        "定时任务" => t!("modules.not_found.schedule"),
        "图片" => t!("modules.not_found.image"),
        "信号" => t!("modules.not_found.signal"),
        "事件源" => t!("modules.not_found.event_source"),
        "HTML 正文" => t!("modules.not_found.html_body"),
        "应用注册" => t!("modules.not_found.app_registration"),
        "工作空间" => t!("modules.not_found.workspace"),
        n => format!("没有这个{}{n}", noun_gap(n)),
    }
}

/// The space between “没有这个” and a noun in Latin letters.
fn noun_gap(noun: &str) -> &'static str {
    if noun.starts_with(|c: char| c.is_ascii_alphanumeric()) {
        " "
    } else {
        ""
    }
}

impl ApiError {
    pub fn invalid(field: impl Into<String>) -> Self {
        Self::Invalid(vec![field.into()])
    }
    pub fn not_found(noun: impl Into<String>) -> Self {
        Self::NotFound(noun.into())
    }
    pub fn conflict(msg: impl Into<String>) -> Self {
        Self::Conflict(msg.into())
    }
    pub fn rule(msg: impl Into<String>) -> Self {
        Self::Rule(msg.into())
    }
    pub fn forbidden(msg: impl Into<String>) -> Self {
        Self::Forbidden(msg.into())
    }

    pub fn status(&self) -> u16 {
        match self {
            Self::Invalid(_) | Self::Rule(_) => 400,
            Self::Unauthenticated => 401,
            Self::Forbidden(_) => 403,
            Self::NotFound(_) => 404,
            Self::Conflict(_) => 409,
            Self::Internal(_) => 500,
        }
    }
}

impl From<StoreError> for ApiError {
    fn from(e: StoreError) -> Self {
        match e {
            // the store's own “没有这个…” messages are complete sentences already
            StoreError::NotFound(m) => Self::NotFound(m.trim_start_matches("没有这个").to_string()),
            e => Self::Internal(e.to_string()),
        }
    }
}

impl From<rusqlite::Error> for ApiError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Internal(e.to_string())
    }
}

impl From<serde_json::Error> for ApiError {
    fn from(e: serde_json::Error) -> Self {
        Self::Internal(e.to_string())
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let code = StatusCode::from_u16(self.status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        let msg = match &self {
            Self::Internal(m) => {
                tracing::error!("module api: {m}");
                steve_i18n::t!("modules.error.internal")
            }
            e => e.to_string(),
        };
        (code, Json(json!({ "error": msg }))).into_response()
    }
}

pub type ApiResult<T> = Result<T, ApiError>;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::vault::VaultError;

    #[tokio::test]
    async fn messages_in_the_request_language() {
        let invalid = ApiError::Invalid(vec!["name".into(), "rels.0".into()]);
        let contact = ApiError::not_found("联系人");
        let bot = ApiError::not_found("Bot");
        let odd = ApiError::not_found("奇怪的东西");
        assert_eq!(invalid.to_string(), "参数不对：name、rels.0");
        assert_eq!(contact.to_string(), "没有这个联系人");
        assert_eq!(bot.to_string(), "没有这个 Bot");
        assert_eq!(odd.to_string(), "没有这个奇怪的东西");
        assert_eq!(
            VaultError::Locked.to_string(),
            "保险箱暂时打不开：Mac mini 的钥匙串被锁了"
        );
        steve_i18n::scope(steve_i18n::EN, async {
            assert_eq!(invalid.to_string(), "Invalid parameters: name, rels.0");
            assert_eq!(contact.to_string(), "No such contact");
            assert_eq!(bot.to_string(), "No such bot");
            // a noun without an entry keeps the zh-CN sentence
            assert_eq!(odd.to_string(), "没有这个奇怪的东西");
            assert_eq!(
                VaultError::BadRecoveryCode.to_string(),
                "Wrong recovery code"
            );
            assert_eq!(
                VaultError::Api(ApiError::not_found("条目")).to_string(),
                "No such vault item"
            );
        })
        .await;
    }

    #[tokio::test]
    async fn the_locale_reaches_blocking_work() {
        let env = crate::testkit::TestEnv::new(|_| {});
        let m = env.modules.clone();
        let msg = steve_i18n::scope(steve_i18n::EN, async {
            m.blocking(|_| Err::<(), _>(ApiError::not_found("账号")))
                .await
                .unwrap_err()
                .to_string()
        })
        .await;
        assert_eq!(msg, "No such account");
    }
}
