//! The declarations of the two built-in Feishu channel sides ([`ChannelDecl`]): the bot's app
//! (`feishu_app`) and you (`feishu_user`). Both name their people in the `feishu` system, carry
//! Feishu's message ids and time messages to the ms.

use crate::channel::{ChannelAdapter, ChannelFeatures};
use crate::ext::{ChannelDecl, MessageIds, Mode, Receive, StartMethod};

use super::FeishuApp;
use super::auth::{APP_ADAPTER, USER_ADAPTER, USER_SYSTEM};

/// The bot: its long connection, its own features, made from the credential form.
pub fn app_decl(app: &FeishuApp) -> ChannelDecl {
    ChannelDecl::new(APP_ADAPTER)
        .handle_system(USER_SYSTEM)
        .mode(Mode::Bot)
        .features(app.features())
        .receive(Receive::LongConnection, None)
        .message_ids(MessageIds::Platform)
        .created_precision_ms(1)
        .start(StartMethod::Form)
}

/// You: read by polling while `msg.receive` is on; sends, replies in threads, reacts and recalls
/// as you; authorized by OAuth or a device code.
pub fn user_decl() -> ChannelDecl {
    ChannelDecl::new(USER_ADAPTER)
        .handle_system(USER_SYSTEM)
        .mode(Mode::Own)
        .features(ChannelFeatures {
            as_user: true,
            thread_reply: true,
            recall: true,
            reactions: true,
            ..ChannelFeatures::default()
        })
        .receive(Receive::Poll, Some("msg.receive"))
        .message_ids(MessageIds::Platform)
        .created_precision_ms(1)
        .start(StartMethod::OAuth)
        .start(StartMethod::Device)
}
