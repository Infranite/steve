//! Fixed Chinese text stored in the database and shown on the pages: kept as stored (it is compared and
//! copied), put in the request's language only where an API answer carries it.

use steve_i18n::t;

/// A `source_label` as stored (“你填的”, “飞书通讯录”, “<来源> · Agent 记下”) in the request's language.
/// Anything else (a connection's own label) is shown as stored.
pub fn source_label(stored: &str) -> String {
    match stored {
        "你填的" => t!("modules.source.me_filled"),
        "你说的" => t!("modules.source.me_said"),
        "你写的" => t!("modules.source.me_wrote"),
        "你接入" => t!("modules.source.connect"),
        "邮箱前缀规则" => t!("modules.source.email_prefix_rule"),
        "通讯录同步" => t!("modules.source.directory_sync"),
        "飞书通讯录" => t!("modules.source.feishu_directory"),
        s => match s.strip_suffix(" · Agent 记下") {
            Some(origin) => t!("modules.source.agent_noted", origin = origin),
            None => s.to_string(),
        },
    }
}

/// The name of a system: a built-in one still under its seeded name in the request's language.
pub fn system_name(id: &str, builtin: bool, stored: &str) -> String {
    // the external-channel extension declares its system under its zh-CN name
    if id == "ext_ws"
        && let Some(s) = steve_i18n::reword!(stored, &["modules.system.ext_ws"])
    {
        return s;
    }
    if !builtin {
        return stored.to_string();
    }
    let seeded = crate::accounts::repo::BUILTIN_SYSTEMS
        .iter()
        .any(|(i, n, ..)| *i == id && *n == stored);
    if !seeded {
        return stored.to_string();
    }
    match id {
        "mail" => t!("modules.system.mail"),
        "feishu" => t!("modules.system.feishu"),
        "feishu_app" => t!("modules.system.feishu_app"),
        "gcal" => t!("modules.system.gcal"),
        "phone" => t!("modules.system.phone"),
        "bank" => t!("modules.system.bank"),
        "feishu_tenant" => t!("modules.system.feishu_tenant"),
        _ => stored.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_external_channel_system_by_language() {
        let name = |l| steve_i18n::with(l, || system_name("ext_ws", false, "其他聊天工具"));
        assert_eq!(name(steve_i18n::EN), "Other chat app");
        assert_eq!(name(steve_i18n::ZH_CN), "其他聊天工具");
        // renamed: as stored
        let own = steve_i18n::with(steve_i18n::EN, || system_name("ext_ws", false, "公司渠道"));
        assert_eq!(own, "公司渠道");
    }
}
