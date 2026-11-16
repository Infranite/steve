//! Feishu's links into its own client ([`crate::open_link::PlatformLinks`]): an AppLink that opens
//! a chat in the Feishu (or Lark) desktop client on a computer and in the app on a phone.
//!
//! `https://applink.feishu.cn/client/chat/open?openChatId=oc_…` (Lark: `applink.larksuite.com`),
//! for the chats the signed-in person has joined in their own tenant. The brand is the
//! connection's `config.domain` ([`Domain`]); a channel service that reaches Feishu names no brand,
//! so its chats open on Feishu unless a native connection of the same chat says otherwise (the
//! caller asks native connections first).
//!
//! Feishu has no documented AppLink to one message by its id (`om_…`): a message link is its
//! chat's link ([`PlatformLinks::message`] stays `None`, the caller falls back to the chat).

use serde_json::Value;

use super::auth::{APP_ADAPTER, USER_ADAPTER};
use super::client::Domain;
use super::directory::SYSTEM;
use crate::open_link::PlatformLinks;

/// The longest chat id a link is made for.
const CHAT_ID_MAX: usize = 128;

/// The AppLink host of a brand.
pub fn applink_base(d: Domain) -> &'static str {
    match d {
        Domain::Feishu => "https://applink.feishu.cn",
        Domain::Lark => "https://applink.larksuite.com",
    }
}

/// The link that opens chat `open_chat_id` (`oc_…`) on brand `d`; `None` for anything that is not
/// an open chat id (an empty one, a Hub-made one, a service's own id for a chat).
pub fn chat_url(d: Domain, open_chat_id: &str) -> Option<String> {
    let id = open_chat_id.trim();
    let ok = id.len() <= CHAT_ID_MAX
        && id.strip_prefix("oc_").is_some_and(|rest| {
            !rest.is_empty()
                && rest
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
        });
    ok.then(|| format!("{}/client/chat/open?openChatId={id}", applink_base(d)))
}

/// Feishu / Lark, as [`crate::open_link`] knows it.
pub struct FeishuLinks;

/// The one instance the registry holds.
pub static FEISHU: FeishuLinks = FeishuLinks;

impl PlatformLinks for FeishuLinks {
    fn id(&self) -> &'static str {
        SYSTEM
    }

    fn native(&self, adapter: &str) -> bool {
        adapter == APP_ADAPTER || adapter == USER_ADAPTER
    }

    fn chat(&self, config: &Value, chat_id: &str) -> Option<String> {
        chat_url(Domain::from_config(config), chat_id)
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn a_chat_opens_on_its_brand() {
        assert_eq!(
            chat_url(Domain::Feishu, "oc_0123abcdef").as_deref(),
            Some("https://applink.feishu.cn/client/chat/open?openChatId=oc_0123abcdef")
        );
        assert_eq!(
            FEISHU.chat(&json!({"domain": "lark"}), "oc_x-1").as_deref(),
            Some("https://applink.larksuite.com/client/chat/open?openChatId=oc_x-1")
        );
        // no brand said: Feishu
        assert_eq!(
            FEISHU.chat(&json!({}), "oc_x").as_deref(),
            Some("https://applink.feishu.cn/client/chat/open?openChatId=oc_x")
        );
    }

    #[test]
    fn only_an_open_chat_id_gets_a_link() {
        for id in [
            "",
            "oc_",
            "fp:v1:abc",
            "c_42",
            "ou_123",
            "oc_a&b=c",
            "oc_a b",
            "oc_中文",
        ] {
            assert_eq!(chat_url(Domain::Feishu, id), None, "{id:?}");
        }
        assert_eq!(
            chat_url(Domain::Feishu, &format!("oc_{}", "a".repeat(130))),
            None
        );
    }

    #[test]
    fn its_own_adapters_are_native_and_no_message_has_a_link_of_its_own() {
        assert!(FEISHU.native(APP_ADAPTER) && FEISHU.native(USER_ADAPTER));
        assert!(!FEISHU.native("ext_ws"));
        assert_eq!(FEISHU.id(), "feishu");
        assert_eq!(FEISHU.message(&json!({}), "oc_x", "om_y"), None);
    }
}
