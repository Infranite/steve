//! What a channel-side adapter may read and keep about its own connection: whether a space is
//! already covered by another connection, whether a message is already stored, a stored message as
//! a [`MessageRef`], its own cursors, and a person's handle in another system.
//!
//! [`Queries`] is handed to the extensions at start; it only answers through [`Queries::for_cx`],
//! bound to one connection ([`ConnQueries`]), so an adapter cannot read or write another
//! connection's or workspace's rows by id. Part of the public extension API (`ext`).

use steve_modules::accounts::connections::ConnCx;
use steve_modules::accounts::service as handles;
use steve_store::{Accounts, Systems};

use steve_store::{channel_message_repo::ingress, channel_positions, channel_spaces, connections};

use crate::channel::{HandleRef, InboundMessage, MessageRef};
use crate::{ChannelResult, Channels};

/// The Hub's read views, for one connection at a time.
#[derive(Clone)]
pub struct Queries {
    channels: Channels,
}

impl Queries {
    pub fn new(channels: Channels) -> Self {
        Self { channels }
    }

    /// The views of connection `cx`.
    pub fn for_cx(&self, cx: &ConnCx) -> ConnQueries {
        ConnQueries::bound(self.channels.clone(), cx.ws(), cx.id())
    }
}

/// [`Queries`] bound to one connection of one workspace.
#[derive(Clone)]
pub struct ConnQueries {
    channels: Channels,
    ws: String,
    conn: String,
}

impl std::fmt::Debug for ConnQueries {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ConnQueries")
            .field("ws", &self.ws)
            .field("conn", &self.conn)
            .finish()
    }
}

impl ConnQueries {
    pub(crate) fn bound(channels: Channels, ws: &str, conn: &str) -> Self {
        Self {
            channels,
            ws: ws.to_string(),
            conn: conn.to_string(),
        }
    }

    /// Another connection of the workspace already receives every message of space `space_ext`:
    /// an active space of a healthy (`ok`) connection whose channel adapter says it receives all
    /// ([`crate::ChannelAdapter::receives_all`]). Unknown counts as not covered.
    pub fn covered(&self, space_ext: &str) -> ChannelResult<bool> {
        let others = self.channels.store().read(|r| {
            let mut ids = channel_spaces::active_chat_connections(r, &self.ws, space_ext)?;
            ids.retain(|id| id != &self.conn);
            connections::lifecycle::healthy_adapter_configs(r, &self.ws, &ids)
        })?;
        Ok(others.into_iter().any(|(adapter, config)| {
            self.channels
                .adapter(&adapter)
                .is_some_and(|a| a.receives_all(&config))
        }))
    }

    /// The message is already stored: by its provider id (through any connection of the
    /// workspace), or, for a message without one, by the delivery id this connection gave it
    /// ([`InboundMessage::source_id`]). A message without either is never known this way.
    pub fn known(&self, msg: &InboundMessage) -> ChannelResult<bool> {
        if msg.external_id.is_empty() {
            return match msg.source_id.as_deref().filter(|s| !s.is_empty()) {
                Some(src) => Ok(self
                    .channels
                    .store()
                    .read(|r| ingress::by_source(r, &self.conn, src))?
                    .is_some()),
                None => Ok(false),
            };
        }
        Ok(self.channels.store().read(|r| {
            ingress::has_provider_message(r, &self.ws, &msg.space_ext_id, &msg.external_id)
        })?)
    }

    /// Stored message `ext` of space `space_ext`, if there is one: by its provider id, else by the
    /// delivery id this connection knows it by ([`Self::message_ref_by_source`]).
    pub fn message_ref(&self, space_ext: &str, ext: &str) -> ChannelResult<Option<MessageRef>> {
        if ext.is_empty() || crate::fallback::is_fallback(ext) {
            return Ok(None);
        }
        match self.by_external(space_ext, ext)? {
            Some(m) => Ok(Some(m)),
            None => self.message_ref_by_source(space_ext, ext),
        }
    }

    fn by_external(&self, space_ext: &str, ext: &str) -> ChannelResult<Option<MessageRef>> {
        self.channels.store().read(|r| {
            let Some(id) = ingress::first_in_chat(r, &self.ws, space_ext, ext)? else {
                return Ok(None);
            };
            crate::messages::message_ref(r, &id, &self.conn)
        })
    }

    /// Stored message of space `space_ext` that this connection delivered, or sent, with remote
    /// id `source_id`.
    pub fn message_ref_by_source(
        &self,
        space_ext: &str,
        source_id: &str,
    ) -> ChannelResult<Option<MessageRef>> {
        if source_id.is_empty() {
            return Ok(None);
        }
        self.channels.store().read(|r| {
            let Some(message) = ingress::by_source(r, &self.conn, source_id)? else {
                return Ok(None);
            };
            let chat = channel_spaces::platform_chat(r, &message.space_id)?;
            if !chat.is_some_and(|chat| chat.ws == self.ws && chat.external_id == space_ext) {
                return Ok(None);
            }
            crate::messages::message_ref(r, &message.id, &self.conn)
        })
    }

    /// This connection's cursor for `container` (a space, a thread, a feed: the adapter's name).
    pub fn cursor(&self, container: &str) -> ChannelResult<Option<String>> {
        Ok(self
            .channels
            .store()
            .read(|r| channel_positions::external_token(r, &self.conn, container))?)
    }

    /// Keeps this connection's cursor for `container`. Deleted with the connection.
    pub fn save_cursor(&self, container: &str, cursor: &str) -> ChannelResult<()> {
        let now = self.channels.now();
        self.channels.store().write(|w| {
            if !connections::connection_kind(w, &self.conn)?.is_some_and(|(ws, _)| ws == self.ws) {
                return Err(crate::ChannelError::not_found("连接"));
            }
            if !channel_positions::save_external_token(
                w, &self.ws, &self.conn, container, cursor, now,
            )? {
                return Err(crate::ChannelError::Conflict(steve_i18n::t!(
                    "channels.error.cursor_not_ext",
                    container = container
                )));
            }
            Ok(())
        })
    }

    /// The handle in system `want_system` of whoever `from` is, when the workspace knows both:
    /// one valid everywhere or under this connection.
    pub fn handle_for(
        &self,
        from: &HandleRef,
        want_system: &str,
    ) -> ChannelResult<Option<HandleRef>> {
        Ok(self
            .channels
            .store()
            .read::<_, steve_store::StoreError>(|r| {
                let Some(sys) = Systems(r).get(&self.ws, &from.system)? else {
                    return Ok(None);
                };
                let Some(want) = Systems(r).get(&self.ws, want_system)? else {
                    return Ok(None);
                };
                let mut accounts = Accounts(r);
                let mut account = None;
                for raw in std::iter::once(&from.handle).chain(&from.extra) {
                    let Some(h) = handles::normalize(&sys, raw) else {
                        continue;
                    };
                    account = accounts.by_handle(&self.ws, &sys.family, &h.handle)?;
                    if account.is_some() {
                        break;
                    }
                }
                let Some(account) = account else {
                    return Ok(None);
                };
                let Some(owner) = accounts.get(&account)? else {
                    return Ok(None);
                };
                let handle = accounts.preferred_handle(
                    &self.ws,
                    &owner.party_id,
                    &want.family,
                    &want.id,
                    Some(&self.conn),
                )?;
                Ok(handle.map(|h| HandleRef::new(want.id, h)))
            })?)
    }
}
