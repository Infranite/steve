pub mod ext_ws;
pub mod feishu;

/// The fixed text the adapters declare for the 接入 page and the connection forms (names, field labels
/// and hints, the provider name, notes, what each scope is for), each written in zh-CN where it is
/// declared: these keys hold the same zh-CN text and its translations.
const TEXT_KEYS: &[&str] = &[
    "channels.text.feishu_provider",
    "channels.text.feishu_app_name",
    "channels.text.feishu_user_name",
    "channels.text.ext_ws_name",
    "channels.text.planned_docs",
    "channels.text.planned_sheets",
    "channels.text.planned_calendar",
    "channels.text.planned_tasks",
    "channels.text.planned_approval",
    "channels.text.planned_minutes",
    "channels.text.bot_channel_note",
    "channels.text.bot_directory_note",
    "channels.text.send_as_user_note",
    "channels.text.user_channel_note",
    "channels.text.send_as_user_purpose",
    "channels.text.field_app_id_hint",
    "channels.text.field_webhook_hint",
    "channels.text.field_domain",
    "channels.text.field_domain_hint",
    "channels.text.field_ext_url",
    "channels.text.field_ext_url_hint",
    "channels.text.field_ext_token",
    "channels.text.field_ext_pin",
    "channels.text.field_ext_pin_hint",
    "channels.text.ext_channel_note",
    "channels.text.ext_send_note",
    "channels.scope.im_message",
    "channels.scope.send_as_bot",
    "channels.scope.p2p",
    "channels.scope.group_at",
    "channels.scope.group_msg",
    "channels.scope.chat",
    "channels.scope.resource",
    "channels.scope.reactions",
    "channels.scope.contact",
    "channels.scope.contact_dept",
    "channels.scope.contact_user",
    "channels.scope.contact_email",
    "channels.scope.contact_user_dept",
    "channels.scope.contact_group",
    "channels.scope.members",
    "channels.scope.contact_lookup",
    "channels.scope.tenant",
    "channels.scope.offline",
    "channels.scope.user_message",
    "channels.scope.user_read",
    "channels.scope.user_p2p",
    "channels.scope.user_group",
    "channels.scope.user_chats",
    "channels.scope.user_send",
    "channels.scope.search",
];

/// An adapter's fixed text in the request's language (`ConnectionProvider::localize`).
pub(crate) fn localize(text: &str) -> Option<String> {
    steve_i18n::reword!(text, TEXT_KEYS)
}

#[cfg(test)]
mod tests {
    use steve_modules::accounts::connections::{
        ConnectionProvider, fields_shown, integration_shown, shown,
    };

    fn chinese(s: &str) -> bool {
        s.chars().any(|c| ('\u{4e00}'..='\u{9fff}').contains(&c))
    }

    /// Every text an adapter shows on the 接入 page and in its form, in one language.
    fn texts(p: &dyn ConnectionProvider) -> Vec<String> {
        let i = integration_shown(p);
        let mut out = vec![shown(p, p.name()), i.provider_name];
        for c in i.capabilities {
            out.extend(c.note);
            out.extend(c.scopes.into_iter().map(|s| s.purpose));
        }
        for f in fields_shown(p).unwrap_or_default() {
            out.push(f.label);
            out.extend(f.hint);
        }
        out
    }

    fn adapters() -> Vec<Box<dyn ConnectionProvider>> {
        vec![
            Box::new(super::feishu::auth::FeishuApp::new()),
            Box::new(super::feishu::auth::FeishuUser::new(
                super::feishu::client::Domain::Feishu,
            )),
            Box::new(super::ext_ws::ExtWs::new()),
        ]
    }

    #[test]
    fn adapter_text_is_in_english_for_english_readers() {
        for p in adapters() {
            for t in steve_i18n::with(steve_i18n::EN, || texts(p.as_ref())) {
                assert!(!chinese(&t), "{}: {t}", p.id());
            }
        }
        let en = steve_i18n::with(steve_i18n::EN, || texts(adapters()[0].as_ref()));
        assert!(en.contains(&"Feishu / Lark".to_string()), "{en:?}");
        assert!(en.contains(&"Feishu app bot".to_string()), "{en:?}");
    }

    #[test]
    fn adapter_text_stays_as_declared_in_chinese() {
        for p in adapters() {
            let zh = steve_i18n::with(steve_i18n::ZH_CN, || texts(p.as_ref()));
            let i = p.integration();
            assert_eq!(zh[0], p.name());
            assert_eq!(zh[1], i.provider_name);
        }
        let zh = steve_i18n::with(steve_i18n::ZH_CN, || texts(adapters()[0].as_ref()));
        assert!(zh.contains(&"飞书 / Lark".to_string()), "{zh:?}");
        assert!(zh.contains(&"收群里 @机器人 的消息".to_string()), "{zh:?}");
    }
}
