//! What a channel-side adapter may read and keep about its own connection: whether a space is
//! already covered by another connection, whether a message is already stored, a stored message as
//! a [`MessageRef`], its own cursors, and a person's handle in another system.
//!
//! [`Queries`] is handed to the extensions at start; it only answers through [`Queries::for_cx`],
//! bound to one connection ([`ConnQueries`]), so an adapter cannot read or write another
//! connection's or workspace's rows by id. Part of the public extension API (`ext`).

use rusqlite::{OptionalExtension, params};
use serde_json::Value;
use steve_modules::accounts::connections::ConnCx;
use steve_modules::accounts::{repo as accounts, service as handles};

use crate::channel::{HandleRef, InboundMessage, MessageRef};
use crate::{ChannelResult, Channels};

/// The `chan_cursors.kind` of an adapter's own cursors (the built-in self poller's are `chat` and
/// `thread`).
const CURSOR_KIND: &str = "ext";

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
        let others = self.channels.store().reader(|c| -> ChannelResult<_> {
            let mut st = c.prepare(
                "SELECT DISTINCT c.adapter, c.config FROM spaces s
                   JOIN space_identities i ON i.space_id = s.id AND i.left_at IS NULL
                   JOIN connections c ON c.id = i.connection_id
                  WHERE s.workspace_id = ?1 AND s.external_id = ?2 AND s.status = 'active'
                    AND c.workspace_id = ?1 AND c.id <> ?3 AND c.status = 'ok'",
            )?;
            let rows = st
                .query_map(params![self.ws, space_ext, self.conn], |r| {
                    Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })?;
        Ok(others.into_iter().any(|(adapter, config)| {
            let config: Value = serde_json::from_str(&config).unwrap_or(Value::Null);
            self.channels
                .adapter(&adapter)
                .is_some_and(|a| a.receives_all(&config))
        }))
    }

    /// The message is already stored (through any connection of the workspace), by its provider
    /// id. A message without one is never known this way.
    pub fn known(&self, msg: &InboundMessage) -> ChannelResult<bool> {
        if msg.external_id.is_empty() {
            return Ok(false);
        }
        self.channels.store().reader(|c| -> ChannelResult<bool> {
            Ok(c.query_row(
                "SELECT EXISTS (SELECT 1 FROM chan_messages m JOIN spaces s ON s.id = m.space_id
                                 WHERE s.workspace_id = ?1 AND s.external_id = ?2 AND m.external_id = ?3)",
                params![self.ws, msg.space_ext_id, msg.external_id],
                |r| r.get(0),
            )?)
        })
    }

    /// Stored message `ext` of space `space_ext`, if there is one.
    pub fn message_ref(&self, space_ext: &str, ext: &str) -> ChannelResult<Option<MessageRef>> {
        if ext.is_empty() {
            return Ok(None);
        }
        self.channels.store().reader(|c| -> ChannelResult<_> {
            let id: Option<String> = c
                .query_row(
                    "SELECT m.id FROM chan_messages m JOIN spaces s ON s.id = m.space_id
                      WHERE s.workspace_id = ?1 AND s.external_id = ?2 AND m.external_id = ?3
                      ORDER BY m.created, m.id LIMIT 1",
                    params![self.ws, space_ext, ext],
                    |r| r.get(0),
                )
                .optional()?;
            match id {
                Some(id) => crate::messages::message_ref(c, &id, &self.conn),
                None => Ok(None),
            }
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
        self.channels.store().reader(|c| -> ChannelResult<_> {
            let id: Option<String> = c
                .query_row(
                    "SELECT m.id FROM chan_message_seen x JOIN chan_messages m ON m.id = x.message_id
                       JOIN spaces s ON s.id = m.space_id
                      WHERE x.connection_id = ?1 AND x.source_id = ?2 AND s.workspace_id = ?3
                        AND s.external_id = ?4",
                    params![self.conn, source_id, self.ws, space_ext],
                    |r| r.get(0),
                )
                .optional()?;
            match id {
                Some(id) => crate::messages::message_ref(c, &id, &self.conn),
                None => Ok(None),
            }
        })
    }

    /// This connection's cursor for `container` (a space, a thread, a feed: the adapter's name).
    pub fn cursor(&self, container: &str) -> ChannelResult<Option<String>> {
        self.channels.store().reader(|c| -> ChannelResult<_> {
            Ok(c.query_row(
                "SELECT token FROM chan_cursors WHERE connection_id = ? AND container = ? AND kind = ?",
                params![self.conn, container, CURSOR_KIND],
                |r| r.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten())
        })
    }

    /// Keeps this connection's cursor for `container`. Deleted with the connection.
    pub fn save_cursor(&self, container: &str, cursor: &str) -> ChannelResult<()> {
        let now = self.channels.now();
        self.channels.store().transaction(|tx| -> ChannelResult<()> {
            // only a connection of this workspace (not one deleted meanwhile)
            let ours: bool = tx.query_row(
                "SELECT EXISTS (SELECT 1 FROM connections WHERE id = ? AND workspace_id = ?)",
                params![self.conn, self.ws],
                |r| r.get(0),
            )?;
            if !ours {
                return Err(crate::ChannelError::not_found("连接"));
            }
            let n = tx.execute(
                "INSERT INTO chan_cursors (connection_id, workspace_id, container, kind, chat_id, token, created, updated)
                 VALUES (?1, ?2, ?3, ?4, ?3, ?5, ?6, ?6)
                 ON CONFLICT (connection_id, container) DO UPDATE SET token = excluded.token, updated = excluded.updated
                  WHERE chan_cursors.kind = excluded.kind",
                params![self.conn, self.ws, container, CURSOR_KIND, cursor, now],
            )?;
            if n == 0 {
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
        self.channels.store().reader(|c| -> ChannelResult<_> {
            let Some(sys) = accounts::system(c, &self.ws, &from.system)? else {
                return Ok(None);
            };
            let Some(want) = accounts::system(c, &self.ws, want_system)? else {
                return Ok(None);
            };
            let mut account = None;
            for raw in std::iter::once(&from.handle).chain(&from.extra) {
                let Some(h) = handles::normalize(&sys, raw) else {
                    continue;
                };
                account = accounts::by_handle(c, &self.ws, &sys.family, &h.handle)?;
                if account.is_some() {
                    break;
                }
            }
            let Some(account) = account else {
                return Ok(None);
            };
            let handle: Option<String> = c
                .query_row(
                    "SELECT h.handle FROM account_handles h JOIN accounts a ON a.id = h.account_id
                      WHERE a.workspace_id = ?1 AND h.workspace_id = ?1 AND h.family = ?2
                        AND a.party_id = (SELECT party_id FROM accounts WHERE id = ?3)
                        AND (h.connection_id IS NULL OR h.connection_id = ?4)
                      ORDER BY h.connection_id IS NULL, a.system_id <> ?5, h.handle <> a.handle, h.created, h.rowid
                      LIMIT 1",
                    params![self.ws, want.family, account, self.conn, want.id],
                    |r| r.get(0),
                )
                .optional()?;
            Ok(handle.map(|h| HandleRef::new(want.id.clone(), h)))
        })
    }
}
