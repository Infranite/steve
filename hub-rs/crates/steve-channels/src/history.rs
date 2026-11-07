//! Reading a conversation's history when a task needs it (`steve_im_read`, [`crate::im_mcp`]).
//!
//! Adapters do not pull history by themselves: the agent decides which conversation and which time
//! range it needs, and [`Channels::read_history`] reads it then —
//!
//! 1. the space must be one the viewer sees ([`spaces::own`]); a space set to 屏蔽, or whose
//!    disclosure is `never`, is not read;
//! 2. through a connection of the space whose side registered a [`HistoryRead`] view (the
//!    viewer's own account or a workspace bot, never someone else's personal account), else, when
//!    none has one or it fails, from the messages the Hub stored;
//! 3. what comes back is context only: it is not stored, identifies nobody, runs no rule, flow,
//!    model or action. (Stored history would be a write behind a read tool; the messages the Hub
//!    should keep already came in through the connection, under the watermark of
//!    [`crate::inbound::history_of`].)

use std::time::Duration;

use async_trait::async_trait;
use rusqlite::{Connection, OptionalExtension, params};
use steve_flow::AdapterError;
use steve_modules::ApiError;
use steve_modules::accounts::connections::ConnCx;
use steve_store::base::{Level, Viewer};

use crate::channel::InboundMessage;
use crate::spaces::{self, Attention};
use crate::{ChannelResult, Channels};

/// The history view of a channel side: the messages of one conversation in a time range, read
/// from the remote as the connection. Registered with [`crate::ext::ChannelSide::history`] under
/// the `connections.adapter` it serves; optional (without it the Hub answers from the messages it
/// stored). Only `steve_im_read` calls it, when an agent asks; nothing is stored or acted on.
#[async_trait]
pub trait HistoryRead: Send + Sync {
    /// The `connections.adapter` it serves.
    fn adapter_id(&self) -> &str;

    /// The messages of the conversation `space_ext_id` (what [`InboundMessage::space_ext_id`] says)
    /// written from `from_ms` to `to_ms` (unix ms, both included), oldest first, at most `limit`
    /// (1 … [`MAX_LIMIT`]): the earliest ones when there are more. Each built like an inbound
    /// message (`external_id`, `sender`, `text`, `attachments`, `created`, `thread_key`,
    /// `reply_to`). The Hub drops anything outside the range or past `limit`, and keeps one per
    /// `external_id`.
    async fn read(
        &self,
        cx: &ConnCx,
        space_ext_id: &str,
        from_ms: i64,
        to_ms: i64,
        limit: usize,
    ) -> Result<Vec<InboundMessage>, AdapterError>;
}

/// Messages of one read: default and maximum.
pub const DEFAULT_LIMIT: usize = 50;
pub const MAX_LIMIT: usize = 200;
/// The range read when the agent gives none.
pub const DEFAULT_RANGE_MS: i64 = 24 * 3_600_000;
/// The longest range one read covers.
pub const MAX_RANGE_MS: i64 = 90 * 24 * 3_600_000;
/// How long the remote may take.
pub const TIMEOUT: Duration = Duration::from_secs(20);

/// Where the messages of a [`Page`] come from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// Read from the remote just now ([`HistoryRead`]).
    Remote,
    /// The messages the Hub stored.
    Stored,
}

/// One message of a [`Page`].
#[derive(Debug, Clone, PartialEq)]
pub struct Line {
    pub at: i64,
    pub sender: String,
    /// Sent by Steve (as the bot, or as you).
    pub out: bool,
    /// `None`: the text is past the retention period.
    pub text: Option<String>,
    /// Kinds of what is attached (`image`, `file`, …).
    pub attachments: Vec<String>,
    /// Only recorded as history when it came in ([`crate::inbound::History`]).
    pub history: Option<String>,
}

/// What [`Channels::read_history`] answers.
#[derive(Debug, Clone, PartialEq)]
pub struct Page {
    pub space_id: String,
    pub space_name: String,
    /// The space's disclosure level: what may be done with what it says.
    pub level: Level,
    pub from: i64,
    pub to: i64,
    pub source: Source,
    pub lines: Vec<Line>,
    /// There is more in the range after the last line.
    pub more: bool,
}

/// A connection of `space` to read it through: the viewer's own account or a workspace bot, of a
/// usable connection. `(connection, adapter)`, the voice first.
fn readers(c: &Connection, viewer: &Viewer, space: &str) -> ChannelResult<Vec<(String, String)>> {
    let voice = spaces::voice(c, space)?;
    let mut st = c.prepare_cached(
        "SELECT c.id, c.adapter FROM connections c JOIN accounts a ON a.id = c.account_id
           LEFT JOIN parties p ON p.id = a.party_id
          WHERE c.workspace_id = ?1 AND c.status IN ('ok', 'error')
            AND (p.user_id IS NULL OR p.user_id = ?3)
            AND (c.id IN (SELECT connection_id FROM space_identities WHERE space_id = ?2 AND left_at IS NULL)
                 OR c.id = (SELECT connection_id FROM spaces WHERE id = ?2))
          ORDER BY c.created, c.id",
    )?;
    let mut rows = st
        .query_map(params![viewer.ws, space, viewer.user], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    if let Some(v) = voice
        && let Some(i) = rows.iter().position(|(id, _)| *id == v)
    {
        let x = rows.remove(i);
        rows.insert(0, x);
    }
    Ok(rows)
}

/// The name a remote message's sender is known by here: the account holding one of its handles,
/// else the name the adapter gave, else the handle. Read only: nobody is identified.
fn sender_name(c: &Connection, ws: &str, m: &InboundMessage) -> ChannelResult<String> {
    let s = &m.sender;
    for h in std::iter::once(&s.handle)
        .chain(s.aliases.iter())
        .flat_map(|h| std::iter::once(&h.handle).chain(h.extra.iter()))
    {
        let name: Option<String> = c
            .query_row(
                "SELECT coalesce(nullif(a.display_name, ''), nullif(p.name, ''), a.handle_display)
                   FROM account_handles h JOIN accounts a ON a.id = h.account_id
                   LEFT JOIN parties p ON p.id = a.party_id
                  WHERE h.workspace_id = ? AND h.handle = ? LIMIT 1",
                params![ws, h.trim()],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(n) = name.filter(|n| !n.trim().is_empty()) {
            return Ok(n);
        }
    }
    Ok(s.display_name
        .clone()
        .filter(|n| !n.trim().is_empty())
        .unwrap_or_else(|| s.handle.handle.clone()))
}

fn kinds(atts: &[serde_json::Value]) -> Vec<String> {
    atts.iter()
        .map(|a| a["type"].as_str().unwrap_or("file").to_string())
        .collect()
}

/// The stored messages of `space` in the range, oldest first, at most `limit + 1`.
fn stored(
    c: &Connection,
    space: &str,
    from: i64,
    to: i64,
    limit: usize,
) -> ChannelResult<Vec<Line>> {
    let mut st = c.prepare_cached(
        "SELECT m.created, coalesce(nullif(a.display_name, ''), nullif(p.name, ''), a.handle_display, ''),
                m.direction = 'out', m.text, m.attachments, m.history
           FROM chan_messages m LEFT JOIN accounts a ON a.id = m.sender_account_id
           LEFT JOIN parties p ON p.id = m.sender_party_id
          WHERE m.space_id = ?1 AND m.created >= ?2 AND m.created <= ?3
            AND (m.direction = 'in' OR m.status IN ('sent', 'recalled'))
          ORDER BY m.created, m.id LIMIT ?4",
    )?;
    let rows = st
        .query_map(params![space, from, to, (limit + 1) as i64], |r| {
            let atts: Option<String> = r.get(4)?;
            Ok(Line {
                at: r.get(0)?,
                sender: r.get(1)?,
                out: r.get(2)?,
                text: r.get(3)?,
                attachments: atts
                    .and_then(|a| serde_json::from_str::<Vec<serde_json::Value>>(&a).ok())
                    .map(|a| kinds(&a))
                    .unwrap_or_default(),
                history: r.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

impl Channels {
    /// The registered history view for `connections.adapter = id`.
    pub fn history(&self, id: &str) -> Option<std::sync::Arc<dyn HistoryRead>> {
        self.history_views().get(id).cloned()
    }

    /// The messages of `space` (an id the viewer sees) written from `from` to `to` (unix ms), at
    /// most `limit`, oldest first: read from the remote when a connection of the space can, else
    /// the stored ones. Context only: nothing is stored or acted on.
    pub async fn read_history(
        &self,
        viewer: &Viewer,
        space: &str,
        from: i64,
        to: i64,
        limit: usize,
    ) -> ChannelResult<Page> {
        let limit = limit.clamp(1, MAX_LIMIT);
        if from > to {
            return Err(ApiError::rule(steve_i18n::t!(
                "channels.im_read.error.range"
            )));
        }
        if to - from > MAX_RANGE_MS {
            return Err(ApiError::rule(steve_i18n::t!(
                "channels.im_read.error.range_long",
                days = MAX_RANGE_MS / 86_400_000
            )));
        }
        let store = self.store().clone();
        let (row, set, readers) = store.reader(|c| -> ChannelResult<_> {
            let row = spaces::own(c, viewer, space).map_err(|_| {
                ApiError::rule(steve_i18n::t!(
                    "channels.im_read.error.no_space",
                    space = space
                ))
            })?;
            let set = spaces::settings(c, &row.id)?;
            Ok((row.clone(), set, readers(c, viewer, &row.id)?))
        })?;
        let level = Level::parse(&set.disclosure).unwrap_or(Level::Reasoning);
        if set.attention == Attention::Blocked || level == Level::Never {
            return Err(ApiError::rule(steve_i18n::t!(
                "channels.im_read.error.not_readable"
            )));
        }
        let space_name = crate::respond::space_label(&row);
        let mut page = Page {
            space_id: row.id.clone(),
            space_name,
            level,
            from,
            to,
            source: Source::Stored,
            lines: Vec::new(),
            more: false,
        };
        for (conn, adapter) in readers {
            let Some(view) = self.history(&adapter) else {
                continue;
            };
            let cx = match self.connections().cx(&viewer.ws, &conn) {
                Ok(cx) => cx,
                Err(e) => {
                    tracing::warn!(conn, "im read: no connection context: {e}");
                    continue;
                }
            };
            let got = tokio::time::timeout(
                TIMEOUT,
                view.read(&cx, &row.external_id, from, to, limit + 1),
            )
            .await;
            let mut msgs = match got {
                Ok(Ok(m)) => m,
                Ok(Err(e)) => {
                    tracing::warn!(conn, "im read: the remote refused: {e}");
                    continue;
                }
                Err(_) => {
                    tracing::warn!(conn, "im read: the remote took too long");
                    continue;
                }
            };
            msgs.retain(|m| m.created >= from && m.created <= to);
            msgs.sort_by_key(|m| m.created);
            let mut seen = std::collections::HashSet::new();
            msgs.retain(|m| m.external_id.is_empty() || seen.insert(m.external_id.clone()));
            let ws = viewer.ws.clone();
            let lines = store.reader(|c| -> ChannelResult<Vec<Line>> {
                let (account, _) = crate::bindings::connection_account(c, &ws, &conn)?;
                let mut out = Vec::new();
                for m in msgs.iter().take(limit + 1) {
                    let out_by_us: bool = c.query_row(
                        "SELECT EXISTS (SELECT 1 FROM chan_messages WHERE space_id = ? AND external_id = ? AND direction = 'out')",
                        params![row.id, m.external_id],
                        |r| r.get(0),
                    )?;
                    let mine: bool = c.query_row(
                        "SELECT EXISTS (SELECT 1 FROM account_handles WHERE account_id = ? AND handle = ?)",
                        params![account, m.sender.handle.handle.trim()],
                        |r| r.get(0),
                    )?;
                    out.push(Line {
                        at: m.created,
                        sender: sender_name(c, &ws, m)?,
                        out: out_by_us || (mine && m.sender.is_bot),
                        text: Some(m.text.clone()),
                        attachments: kinds(&m.attachments),
                        history: None,
                    });
                }
                Ok(out)
            })?;
            page.source = Source::Remote;
            page.lines = lines;
            break;
        }
        if page.source == Source::Stored {
            page.lines = store.reader(|c| stored(c, &row.id, from, to, limit))?;
        }
        page.more = page.lines.len() > limit;
        page.lines.truncate(limit);
        Ok(page)
    }
}
