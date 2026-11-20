//! SG16: pin the existing status/body contract before changing the noun lookup.
use axum::body::to_bytes;
use axum::response::IntoResponse;
use serde_json::json;
use steve_modules::ApiError;

async fn response_is(error: ApiError, status: u16, message: &str) {
    assert_eq!(error.status(), status);
    let response = error.into_response();
    assert_eq!(response.status().as_u16(), status);
    assert_eq!(response.headers()["content-type"], "application/json");
    let body = to_bytes(response.into_body(), 4096).await.unwrap();
    assert_eq!(
        body.as_ref(),
        serde_json::to_vec(&json!({"error": message})).unwrap()
    );
}

#[tokio::test]
async fn every_error_variant_keeps_its_status_and_response_body() {
    for (locale, invalid, empty, missing, internal) in [
        (
            steve_i18n::ZH_CN,
            "参数不对：name、rels.0",
            "参数不对：",
            "没有这个联系人",
            "出了点问题，请稍后再试",
        ),
        (
            steve_i18n::EN,
            "Invalid parameters: name, rels.0",
            "Invalid parameters: ",
            "No such contact",
            "Something went wrong, please try again later",
        ),
    ] {
        steve_i18n::scope(locale, async {
            for (error, status, expected) in [
                (
                    ApiError::Invalid(vec!["name".into(), "rels.0".into()]),
                    400,
                    invalid,
                ),
                (ApiError::Invalid(vec![]), 400, empty),
                (ApiError::NotFound("联系人".into()), 404, missing),
                (ApiError::Conflict("已经处理".into()), 409, "已经处理"),
                (
                    ApiError::Rule("不能删除你自己".into()),
                    400,
                    "不能删除你自己",
                ),
                (
                    ApiError::Forbidden("只有所有者能操作".into()),
                    403,
                    "只有所有者能操作",
                ),
                (ApiError::Unauthenticated, 401, "unauthenticated"),
                (
                    ApiError::Internal("private storage detail".into()),
                    500,
                    internal,
                ),
            ] {
                response_is(error, status, expected).await;
            }
        })
        .await;
    }
}

#[tokio::test]
async fn every_known_noun_keeps_its_translated_not_found_sentence() {
    let cases = [
        ("联系人", "没有这个联系人", "No such contact"),
        ("组织", "没有这个组织", "No such organization"),
        ("团队", "没有这个团队", "No such team"),
        ("空间", "没有这个空间", "No such space"),
        ("账号", "没有这个账号", "No such account"),
        ("系统", "没有这个系统", "No such system"),
        ("规则", "没有这个规则", "No such rule"),
        ("决定", "没有这个决定", "No such decision"),
        ("面板", "没有这个面板", "No such panel"),
        ("操作", "没有这个操作", "No such action"),
        ("任职", "没有这个任职", "No such job"),
        ("属性", "没有这个属性", "No such attribute"),
        ("负责人提议", "没有这个负责人提议", "No such owner proposal"),
        ("合并记录", "没有这个合并记录", "No such merge"),
        ("合并提议", "没有这个合并提议", "No such merge proposal"),
        ("记忆", "没有这个记忆", "No such memory"),
        ("群", "没有这个群", "No such group chat"),
        ("连接", "没有这个连接", "No such connection"),
        ("接入", "没有这个连接", "No such connection"),
        ("接入方式", "没有这个接入方式", "No such connection type"),
        (
            "设备码登录",
            "没有这个设备码登录",
            "No such device code sign-in",
        ),
        ("设备", "没有这个设备", "No such device"),
        ("身份", "没有这个身份", "No such persona"),
        ("机器人", "没有这个机器人", "No such bot"),
        ("MCP 服务器", "没有这个 MCP 服务器", "No such MCP server"),
        ("技能", "没有这个技能", "No such skill"),
        ("远程 Agent", "没有这个远程 Agent", "No such remote agent"),
        ("Bot", "没有这个 Bot", "No such bot"),
        ("事项", "没有这个事项", "No such item"),
        ("项目", "没有这个项目", "No such project"),
        ("会话", "没有这个会话", "No such conversation"),
        ("授权", "没有这个授权", "No such grant"),
        ("条目", "没有这个条目", "No such vault item"),
        ("字段", "没有这个字段", "No such field"),
        ("使用记录", "没有这个使用记录", "No such use record"),
        ("通行密钥", "没有这个通行密钥", "No such passkey"),
        ("窗口", "没有这个窗口", "No such window"),
        ("记录", "没有这个记录", "No such record"),
        ("文档", "没有这个文档", "No such document"),
        ("笔记", "没有这个笔记", "No such note"),
        ("附件", "没有这个附件", "No such attachment"),
        ("知识来源", "没有这个知识来源", "No such knowledge source"),
        ("知识库", "没有这个知识库", "No such knowledge base"),
        (
            "工作空间成员",
            "没有这个工作空间成员",
            "No such workspace member",
        ),
        ("草稿", "没有这个草稿", "No such draft"),
        ("仓库", "没有这个仓库", "No such repository"),
        ("PR", "没有这个 PR", "No such PR"),
        ("Issue", "没有这个 Issue", "No such issue"),
        ("保险库", "没有这个保险库", "No such vault"),
        ("模板", "没有这个模板", "No such template"),
        ("运行", "没有这个运行", "No such run"),
        ("消息", "没有这个消息", "No such message"),
        ("对话", "没有这个对话", "No such conversation"),
        ("邮件", "没有这个邮件", "No such email"),
        ("对外身份", "没有这个对外身份", "No such outward identity"),
        ("往来", "没有这个往来", "No such thread"),
        ("流程", "没有这个流程", "No such flow"),
        ("版本", "没有这个版本", "No such version"),
        ("标签", "没有这个标签", "No such tag"),
        ("定时任务", "没有这个定时任务", "No such scheduled task"),
        ("图片", "没有这个图片", "No such image"),
        ("信号", "没有这个信号", "No such signal"),
        ("事件源", "没有这个事件源", "No such event source"),
        ("HTML 正文", "没有这个 HTML 正文", "No such HTML body"),
        ("应用注册", "没有这个应用注册", "No such app registration"),
        ("工作空间", "没有这个工作空间", "No such workspace"),
    ];
    for (noun, zh, en) in cases {
        for (locale, expected) in [(steve_i18n::ZH_CN, zh), (steve_i18n::EN, en)] {
            steve_i18n::scope(locale, async {
                let error = ApiError::not_found(noun);
                assert_eq!(error.to_string(), expected, "{noun}");
                response_is(error, 404, expected).await;
            })
            .await;
        }
    }
}

#[tokio::test]
async fn unknown_nouns_keep_the_chinese_fallback_and_ascii_gap() {
    for locale in [steve_i18n::ZH_CN, steve_i18n::EN] {
        steve_i18n::scope(locale, async {
            for (noun, expected) in [
                ("Widget", "没有这个 Widget"),
                ("7zip", "没有这个 7zip"),
                ("éclair", "没有这个éclair"),
                ("_hidden", "没有这个_hidden"),
                ("未知", "没有这个未知"),
                ("", "没有这个"),
            ] {
                let error = ApiError::not_found(noun);
                assert_eq!(error.to_string(), expected);
                response_is(error, 404, expected).await;
            }
        })
        .await;
    }
}
