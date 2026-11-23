//! What the channel module tells the connections framework about a connection
//! (`ConnectionUsage`): how many spaces it has, and what goes when it is disconnected.
//!
//! `channel_settings` and `chan_cursors` reference the connection and hold no history: they are
//! deleted with it. Spaces and their messages are kept: they become `left` (archived ones stay archived), remember the account (`spaces.account_id`, so a
//! personal account's spaces stay private), lose their settings, and the connection row's delete
//! sets `spaces.connection_id` to NULL (migration 012).
//!
//! Register: `Connections::builder(..).usage(Arc::new(steve_channels::usage::ChannelUsage))`.

use steve_modules::accounts::connections::{ConnectionUsage, Usage};
use steve_modules::vault::VaultResult;
use steve_store::{Store, W, channel_spaces};

pub struct ChannelUsage;

impl ConnectionUsage for ChannelUsage {
    fn usage(&self, store: &Store, ws: &str, connection_id: &str) -> VaultResult<Usage> {
        Ok(Usage {
            spaces: store.read(|r| channel_spaces::active_for_connection(r, ws, connection_id))?,
            ..Usage::default()
        })
    }

    fn on_disconnect(&self, w: &mut W<'_>, ws: &str, connection_id: &str) -> VaultResult<()> {
        channel_spaces::disconnect(w, ws, connection_id, steve_store::now())?;
        Ok(())
    }
}
