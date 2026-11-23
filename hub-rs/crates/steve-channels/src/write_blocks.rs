//! Writes a channel refused for good, remembered (`chan_write_blocks`, ext_ws protocol §3.6).
//!
//! A [`ProviderError::Blocked`] send says no retry will help until a permission changes at the
//! provider: the connection is read-only ([`BlockScope::Connection`], every chat of it) or one chat
//! refuses writes ([`BlockScope::Target`]). It is remembered for [`MEMORY_MS`]; meanwhile:
//!
//! - automatic replies (a rule's reply or draft, the acknowledgements) are not drafted there
//!   ([`active`]), so no model runs and no card appears for a message that cannot go out;
//! - repeated automatic refusals share one notice ([`record`]); explicit requests and
//!   manual retries always get their own recoverable failure card;
//! - YOLO never retries a permission refusal. Human retry rechecks the actual permission; clearing
//!   this cache grants nothing and never revives discarded messages.
//!
//! It is lifted early when the provider reports the connection writable again ([`lift_connection`],
//! the channel service's session status) or a write there goes through ([`lift_after_send`]).
//!
//! [`ProviderError::Blocked`]: steve_modules::accounts::connections::ProviderError::Blocked

use steve_modules::accounts::connections::BlockScope;
use steve_store::W;
use steve_store::channel_write_policy::{self as repo, WriteRefusal};

pub use steve_store::channel_write_policy::{
    active, explicit_request, lift_after_send, lift_connection,
};

/// How long a refusal is remembered.
pub const MEMORY_MS: i64 = 24 * 60 * 60 * 1000;

/// Records a refusal. Returns true when it opens a new notice.
pub fn record(
    w: &mut W<'_>,
    ws: &str,
    connection: &str,
    space: &str,
    scope: BlockScope,
    reason: &str,
    now: i64,
) -> steve_store::Result<bool> {
    let chat = match scope {
        BlockScope::Connection => None,
        BlockScope::Target => Some(space),
    };
    repo::record(
        w,
        WriteRefusal {
            ws,
            connection,
            chat,
            reason,
            now,
            memory_ms: MEMORY_MS,
        },
    )
}
