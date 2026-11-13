//! The external channel (`ext_ws`): a messaging system bridged by a separate channel service,
//! which the Hub dials over a WebSocket and talks to in the protocol of [`protocol`]. The service
//! runs on its own (on your machine or LAN, with its own sign-in at the messaging system); a crash
//! or a disconnect of it never affects the Hub.
//!
//! A connection is yours (`Mode::Own`): you enter the service's URL, the access token it issued
//! for this Hub and, for a self-signed certificate, its pin. The Hub reads your messages from the
//! service's changes feed, and sends, replies, reacts and recalls as you through it.
//!
//! - [`ExtWs`]: the provider and the views of the channel side;
//! - [`ExtWsHook`]: the feed of each receiving connection;
//! - [`decl`]: the side's declaration.

pub mod endpoint;
mod feed;
pub mod protocol;
mod provider;
pub mod session;
pub mod wire;

pub use feed::{CURSOR, ExtWsHook};
pub use provider::{ExtWs, ME, Me, PIN, REALM, TOKEN, URL};
pub use session::Timing;

use crate::channel::ChannelFeatures;
use crate::ext::{ChannelDecl, MessageIds, Mode, Receive, StartMethod};

/// `connections.adapter`.
pub const ADAPTER: &str = "ext_ws";
/// The handle system of the people its messages come from (`<realm>:id:<value>`).
pub const SYSTEM: &str = "ext_ws";
/// The system's label.
pub const SYSTEM_LABEL: &str = "其他聊天工具";

/// You, through a channel service: a long connection the Hub keeps open while `msg.receive` is
/// on; sends, replies in threads, reacts and recalls as you (a service that lacks one of them
/// answers `Unsupported` locally); messages carry a delivery id and maybe a platform id; made from
/// the credential form.
pub fn decl() -> ChannelDecl {
    ChannelDecl::new(ADAPTER)
        .handle_system(SYSTEM)
        .mode(Mode::Own)
        .features(ChannelFeatures {
            as_user: true,
            thread_reply: true,
            recall: true,
            reactions: true,
            ..ChannelFeatures::default()
        })
        .receive(Receive::LongConnection, Some("msg.receive"))
        .message_ids(MessageIds::None)
        .created_precision_ms(1000)
        .start(StartMethod::Form)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use crate::ext::ChannelSide;

    use super::*;

    #[test]
    fn the_side_passes_its_check() {
        let ext = Arc::new(ExtWs::new());
        let side = ChannelSide::new(decl(), ext.clone())
            .act_as_user(ext.clone())
            .media(ext.clone())
            .history(ext.clone())
            .profiles(ext);
        assert_eq!(side.check(), Ok(()));
    }
}
