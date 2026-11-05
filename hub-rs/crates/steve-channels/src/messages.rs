//! Channel messages (`chan_messages`): storing inbound ones, the
//! `ChanMessage` shape, paging, the `channels.retention` job (text → NULL after `keepRaw` days) and
//! the contact page's “往来” ([`MessageTimeline`]).

use std::sync::Arc;
use std::time::Duration;

use rusqlite::types::Value as Sql;
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde_json::{Value, json};
use steve_modules::contacts::{TimelineEntry, TimelineSource};
use steve_modules::{ApiResult, Modules};
use steve_store::base::Viewer;
use steve_store::{ChangeEvent, StoreError, Tx, uid};

use crate::ChannelResult;

/// `GET /spaces/:id` shows this many recent messages.
pub const RECENT: usize = 30;
/// `GET /spaces/:id/messages`: default and maximum page size.
pub const PAGE: usize = 50;
pub const PAGE_MAX: usize = 100;
/// The timeline line shows this many characters of a message.
const TIMELINE_CUT: usize = 60;

/// An inbound message to store.
#[derive(Debug, Clone)]
pub struct NewInbound<'a> {
    pub ws: &'a str,
    pub space_id: &'a str,
    pub external_id: &'a str,
    pub thread_key: Option<&'a str>,
    pub reply_to: Option<&'a str>,
    pub sender_account_id: Option<&'a str>,
    pub sender_party_id: Option<&'a str>,
    pub text: &'a str,
    pub attachments: &'a Value,
    pub mentions: &'a Value,
    pub created: i64,
}

/// Stores an inbound message; `None` when `(space, external_id)` is already there (the unique index
/// stops a redelivery, or the same message seen by the long connection and by polling).
pub fn insert_inbound(tx: &Tx, n: &NewInbound) -> ChannelResult<Option<String>> {
    let id = uid("cm");
    let added = tx.execute(
        "INSERT OR IGNORE INTO chan_messages (id, workspace_id, space_id, external_id, thread_key, reply_to, direction,
            sender_account_id, sender_party_id, text, attachments, mentions, status, created)
         VALUES (?, ?, ?, ?, ?, ?, 'in', ?, ?, ?, ?, ?, 'received', ?)",
        params![
            id,
            n.ws,
            n.space_id,
            n.external_id,
            n.thread_key,
            n.reply_to,
            n.sender_account_id,
            n.sender_party_id,
            n.text,
            n.attachments.to_string(),
            n.mentions.to_string(),
            n.created
        ],
    )?;
    if added == 0 {
        return Ok(None);
    }
    event(tx, n.ws, n.space_id, &id);
    Ok(Some(id))
}

/// The message with this provider id in a space.
pub fn by_external(c: &Connection, space: &str, ext: &str) -> ChannelResult<Option<String>> {
    Ok(c.query_row(
        "SELECT id FROM chan_messages WHERE space_id = ? AND external_id = ?",
        params![space, ext],
        |r| r.get(0),
    )
    .optional()?)
}

/// Steve's own outbound message coming back in (the self poller reads what Steve sent as you or as
/// the bot; a long connection sees what Steve sent as you): an outbound message of the same chat with
/// this provider id, or, when the send has not written its provider id back yet, a pending/sent one
/// from the last `window_ms` with the same text (compared without Steve's `[AI]` / proxy marks and
/// a trailing signature, [`crate::arbiter::same_text`]). `(message, space)`.
pub fn our_echo(
    c: &Connection,
    ws: &str,
    space_ext: &str,
    ext: &str,
    text: &str,
    since: i64,
) -> ChannelResult<Option<(String, String)>> {
    let by_id: Option<(String, String)> = c
        .query_row(
            "SELECT m.id, m.space_id FROM chan_messages m JOIN spaces s ON s.id = m.space_id
              WHERE s.workspace_id = ?1 AND s.external_id = ?2 AND m.external_id = ?3 AND m.direction = 'out'
              ORDER BY m.created, m.id LIMIT 1",
            params![ws, space_ext, ext],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    if by_id.is_some() {
        return Ok(by_id);
    }
    if crate::arbiter::echo_key(text).is_empty() {
        return Ok(None);
    }
    // `m.workspace_id` too, so the (workspace_id, space_id, created) index applies.
    let mut st = c.prepare_cached(
        "SELECT m.id, m.space_id, coalesce(m.text, '') FROM chan_messages m JOIN spaces s ON s.id = m.space_id
          WHERE s.workspace_id = ?1 AND s.external_id = ?2 AND m.workspace_id = ?1 AND m.direction = 'out'
            AND m.status IN ('pending', 'sent', 'unknown') AND m.external_id IS NULL AND coalesce(m.sent, m.created) >= ?3
          ORDER BY m.created DESC, m.id",
    )?;
    let rows = st
        .query_map(params![ws, space_ext, since], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
            ))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows
        .into_iter()
        .find(|(_, _, t)| crate::arbiter::same_text(t, text))
        .map(|(id, sp, _)| (id, sp)))
}

/// A redelivery seen through another path (polling first, long connection later): fills what the
/// first one lacked, triggers nothing.
pub fn complete(
    tx: &Tx,
    id: &str,
    thread_key: Option<&str>,
    reply_to: Option<&str>,
) -> ChannelResult<()> {
    tx.execute(
        "UPDATE chan_messages SET thread_key = coalesce(thread_key, ?), reply_to = coalesce(reply_to, ?) WHERE id = ?",
        params![thread_key, reply_to, id],
    )?;
    Ok(())
}

/// The same provider message stored under another connection of the workspace, where one of the
/// two is your own (`mode=self`): the self poller and a bot's long connection see the same chat
/// (same `chat_id`, same `message_id`) as different spaces. `(message, space)`.
pub fn seen_elsewhere(
    c: &Connection,
    ws: &str,
    conn: &str,
    own: bool,
    space_ext: &str,
    ext: &str,
) -> ChannelResult<Option<(String, String)>> {
    Ok(c.query_row(
        "SELECT m.id, m.space_id FROM chan_messages m JOIN spaces s ON s.id = m.space_id
           LEFT JOIN channel_settings cs ON cs.connection_id = s.connection_id
          WHERE s.workspace_id = ?1 AND s.connection_id <> ?2 AND s.external_id = ?3 AND m.external_id = ?4
            AND m.direction = 'in' AND (?5 OR cs.mode = 'self')
          ORDER BY m.created, m.id LIMIT 1",
        params![ws, conn, space_ext, ext, own],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()?)
}

/// A message seen through another connection (see [`seen_elsewhere`]): fills what the first copy
/// lacked, triggers nothing.
pub fn complete_from(
    tx: &Tx,
    id: &str,
    thread_key: Option<&str>,
    reply_to: Option<&str>,
    sender_account: Option<&str>,
    sender_party: Option<&str>,
) -> ChannelResult<()> {
    complete(tx, id, thread_key, reply_to)?;
    tx.execute(
        "UPDATE chan_messages SET sender_account_id = coalesce(sender_account_id, ?), sender_party_id = coalesce(sender_party_id, ?) WHERE id = ?",
        params![sender_account, sender_party, id],
    )?;
    Ok(())
}

/// Is one of these provider ids a message the account wrote in this space (your own messages
/// come in as `direction=in` on a self connection)?
pub fn is_by(c: &Connection, space: &str, exts: &[&str], account: &str) -> ChannelResult<bool> {
    for e in exts {
        let hit: bool = c.query_row(
            "SELECT EXISTS (SELECT 1 FROM chan_messages WHERE space_id = ? AND external_id = ? AND sender_account_id = ?)",
            params![space, e, account],
            |r| r.get(0),
        )?;
        if hit {
            return Ok(true);
        }
    }
    Ok(false)
}

/// The `n` messages before `id` in its space, oldest first: `(sender name, text)` (purged ones
/// left out). The `intent` judgement's context.
pub fn context_before(
    c: &Connection,
    space: &str,
    id: &str,
    n: usize,
) -> ChannelResult<Vec<(String, String)>> {
    let mut st = c.prepare_cached(
        "SELECT coalesce(nullif(a.display_name, ''), a.handle_display, p.name, ''), m.text
           FROM chan_messages m LEFT JOIN accounts a ON a.id = m.sender_account_id
           LEFT JOIN parties p ON p.id = m.sender_party_id
          WHERE m.space_id = ?1 AND m.id <> ?2 AND m.text IS NOT NULL AND m.text <> '' AND m.status IN ('received', 'sent')
            AND m.created <= (SELECT created FROM chan_messages WHERE id = ?2)
          ORDER BY m.created DESC, m.id DESC LIMIT ?3",
    )?;
    let mut rows = st
        .query_map(params![space, id, n as i64], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    rows.reverse();
    Ok(rows)
}

/// The message `id` replies to (its `reply_to`, a provider id in the same space), as `(who, text)`,
/// wherever it is in the history: the composer shows it apart, so an answer to an old message is
/// read against that message.
pub fn replied_to(
    c: &Connection,
    space: &str,
    id: &str,
) -> ChannelResult<Option<(String, String)>> {
    Ok(c
        .query_row(
            "SELECT coalesce(nullif(a.display_name, ''), a.handle_display, p.name, ''), m.text
               FROM chan_messages q JOIN chan_messages m ON m.space_id = q.space_id AND m.external_id = q.reply_to
               LEFT JOIN accounts a ON a.id = m.sender_account_id
               LEFT JOIN parties p ON p.id = m.sender_party_id
              WHERE q.id = ?2 AND q.space_id = ?1 AND m.text IS NOT NULL AND m.text <> ''",
            params![space, id],
            |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)),
        )
        .optional()?)
}

/// Links the message to its `ext_events` row.
pub fn set_event(tx: &Tx, id: &str, event_id: &str) -> ChannelResult<()> {
    tx.execute(
        "UPDATE chan_messages SET event_id = ? WHERE id = ?",
        params![event_id, id],
    )?;
    Ok(())
}

/// Is one of these provider ids a message Steve sent in this space (`im.reply`)?
pub fn is_ours_out(c: &Connection, space: &str, exts: &[&str]) -> ChannelResult<bool> {
    for e in exts {
        let hit: bool = c.query_row(
            "SELECT EXISTS (SELECT 1 FROM chan_messages WHERE space_id = ? AND external_id = ? AND direction = 'out')",
            params![space, e],
            |r| r.get(0),
        )?;
        if hit {
            return Ok(true);
        }
    }
    Ok(false)
}

/// SSE `{type:"chan_message", id, space_id, ws}`.
pub fn event(tx: &Tx, ws: &str, space: &str, id: &str) {
    tx.publish(ChangeEvent::ChanMessage {
        id: id.into(),
        space_id: space.into(),
        ws: ws.into(),
    });
}

// ---------------------------------------------------------------- shape

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SenderOut {
    pub name: String,
    pub party_id: Option<String>,
    pub account_id: Option<String>,
}

/// The `ChanMessage` shape of §5.1. `basis` is `null` for inbound messages and messages without one.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChanMessage {
    pub id: String,
    pub space_id: String,
    pub direction: String,
    pub sender: SenderOut,
    pub persona_id: Option<String>,
    pub on_behalf: String,
    /// `null` once the retention period is over (“原文已过保留期限”).
    pub text: Option<String>,
    pub attachments: Value,
    pub mentions: Value,
    pub status: String,
    pub thread_key: Option<String>,
    pub reply_to: Option<String>,
    pub event_id: Option<String>,
    pub work_item_id: Option<String>,
    pub decision_id: Option<String>,
    pub created: i64,
    pub sent: Option<i64>,
    pub purged: Option<i64>,
    pub basis: Value,
}

const MSG_COLS: &str = "m.id, m.space_id, m.direction, coalesce(a.display_name, a.handle_display, p.name, ''), m.sender_party_id,
    m.sender_account_id, m.persona_id, m.on_behalf, m.text, m.attachments, m.mentions, m.status, m.thread_key, m.reply_to,
    m.event_id, m.work_item_id, m.decision_id, m.created, m.sent, m.purged";
const MSG_FROM: &str = "FROM chan_messages m LEFT JOIN accounts a ON a.id = m.sender_account_id
    LEFT JOIN parties p ON p.id = m.sender_party_id";

fn jv(s: Option<String>) -> Value {
    s.and_then(|s| serde_json::from_str(&s).ok())
        .unwrap_or_else(|| json!([]))
}

fn msg_row(r: &rusqlite::Row) -> rusqlite::Result<ChanMessage> {
    let name: String = r.get(3)?;
    Ok(ChanMessage {
        id: r.get(0)?,
        space_id: r.get(1)?,
        direction: r.get(2)?,
        sender: SenderOut {
            name,
            party_id: r.get(4)?,
            account_id: r.get(5)?,
        },
        persona_id: r.get(6)?,
        on_behalf: r.get(7)?,
        text: r.get(8)?,
        attachments: jv(r.get(9)?),
        mentions: jv(r.get(10)?),
        status: r.get(11)?,
        thread_key: r.get(12)?,
        reply_to: r.get(13)?,
        event_id: r.get(14)?,
        work_item_id: r.get(15)?,
        decision_id: r.get(16)?,
        created: r.get(17)?,
        sent: r.get(18)?,
        purged: r.get(19)?,
        basis: Value::Null,
    })
}

pub fn get(c: &Connection, id: &str) -> ChannelResult<Option<ChanMessage>> {
    let m = c
        .query_row(
            &format!("SELECT {MSG_COLS} {MSG_FROM} WHERE m.id = ?"),
            [id],
            msg_row,
        )
        .optional()?;
    match m {
        Some(mut m) => {
            with_basis(c, &mut m)?;
            Ok(Some(m))
        }
        None => Ok(None),
    }
}

/// The outbound message's basis (C5), for the interfaces only you see; attachments in their public
/// form (no provider keys; `url` or `unavailable`) and an old card / post read again from its raw
/// event ([`crate::media::present`]).
fn with_basis(c: &Connection, m: &mut ChanMessage) -> ChannelResult<()> {
    if m.direction == "out" {
        m.basis = crate::basis::summary_of(c, &m.id)?;
    }
    if let Some((text, atts)) = crate::media::present(c, &m.id) {
        m.text = text;
        m.attachments = atts;
    }
    Ok(())
}

/// Messages of a space older than `before` (all when `None`), newest first.
pub fn page(
    c: &Connection,
    space: &str,
    before: Option<i64>,
    limit: usize,
) -> ChannelResult<Vec<ChanMessage>> {
    let mut st = c.prepare_cached(&format!(
        "SELECT {MSG_COLS} {MSG_FROM} WHERE m.space_id = ?1 AND (?2 IS NULL OR m.created < ?2)
          ORDER BY m.created DESC, m.id DESC LIMIT ?3"
    ))?;
    let mut rows = st
        .query_map(params![space, before, limit as i64], msg_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for m in &mut rows {
        with_basis(c, m)?;
    }
    Ok(rows)
}

// ---------------------------------------------------------------- retention

/// How often `channels.retention` runs.
pub const RETENTION_EVERY: Duration = Duration::from_secs(3600);

/// Messages purged per write transaction, so a large purge doesn't hold the write lock long.
const PURGE_BATCH: usize = 1000;

/// One pass at `now`: in every workspace, message text and attachments (card blocks, file names,
/// resource keys) older than `keepRaw` days are cleared and `purged = now`. Returns the messages
/// purged. Blocking: runs in batches of [`PURGE_BATCH`], each in its own transaction.
pub fn sweep(m: &Modules, now: i64) -> ApiResult<usize> {
    let wss: Vec<String> = m.store.reader(|c| -> ApiResult<_> {
        let mut st = c.prepare("SELECT id FROM workspaces ORDER BY id")?;
        Ok(st
            .query_map([], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?)
    })?;
    // what a sweep before attachments were cleared left behind
    m.store.transaction(|tx| -> ApiResult<()> {
        tx.execute(
            "UPDATE chan_messages SET attachments = '[]' WHERE purged IS NOT NULL AND attachments <> '[]'",
            [],
        )?;
        Ok(())
    })?;
    let mut n = 0;
    for ws in wss {
        loop {
            let done = m.store.transaction(|tx| -> ApiResult<usize> {
                let cut = now - steve_flow::retention::keep_raw_days(tx, &ws)? * 24 * 3_600_000;
                Ok(tx.execute(
                    "UPDATE chan_messages SET text = NULL, attachments = '[]', purged = ?1
                      WHERE rowid IN (SELECT rowid FROM chan_messages
                                       WHERE workspace_id = ?2 AND purged IS NULL AND created < ?3
                                       LIMIT ?4)",
                    params![now, ws, cut, PURGE_BATCH as i64],
                )?)
            })?;
            n += done;
            if done < PURGE_BATCH {
                break;
            }
        }
    }
    Ok(n)
}

/// The hourly job; each sweep runs off the async runtime.
pub async fn retention(m: Modules) {
    loop {
        match m.blocking(|m| sweep(m, m.clock.now())).await {
            Ok(n) if n > 0 => tracing::info!(purged = n, "channels.retention"),
            Ok(_) => {}
            Err(e) => tracing::warn!("channels.retention: {e}"),
        }
        tokio::time::sleep(RETENTION_EVERY).await;
    }
}

// ---------------------------------------------------------------- timeline

/// The contact page's “往来”, kind `message`: what a party said in spaces the viewer sees, newest
/// first. Messages recorded under a contact later merged into this one count too.
pub struct MessageTimeline;

impl MessageTimeline {
    pub fn arc() -> Arc<dyn TimelineSource> {
        Arc::new(Self)
    }
}

fn cut(s: &str, max: usize) -> String {
    let s = s.trim();
    match s.char_indices().nth(max) {
        Some((i, _)) => format!("{}…", &s[..i]),
        None => s.to_string(),
    }
}

impl TimelineSource for MessageTimeline {
    fn entries(
        &self,
        c: &Connection,
        viewer: &Viewer,
        party: &str,
        before: Option<i64>,
        limit: usize,
    ) -> ApiResult<Vec<TimelineEntry>> {
        let sql = format!("WITH RECURSIVE ids(id) AS (SELECT ?1 UNION SELECT p.id FROM parties p JOIN ids ON p.merged_into = ids.id)
             SELECT m.space_id, s.name, m.text, m.created FROM chan_messages m JOIN spaces s ON s.id = m.space_id
              WHERE m.workspace_id = ?2 AND m.direction = 'in' AND m.sender_party_id IN (SELECT id FROM ids)
                AND (?3 IS NULL OR m.created < ?3)
                AND {}
              ORDER BY m.created DESC, m.id DESC LIMIT ?4",
            crate::spaces::visible_sql("?5")
        );
        let ps = vec![
            Sql::Text(party.into()),
            Sql::Text(viewer.ws.clone()),
            before.map(Sql::Integer).unwrap_or(Sql::Null),
            Sql::Integer(limit as i64),
            Sql::Text(viewer.user.clone()),
        ];
        let mut st = c.prepare_cached(&sql).map_err(StoreError::from)?;
        let rows = st
            .query_map(steve_store::base::bind(ps), |r| {
                let (space, name, text, ts): (String, String, Option<String>, i64) =
                    (r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?);
                let name = if name.is_empty() {
                    "私聊".to_string()
                } else {
                    name
                };
                Ok(TimelineEntry {
                    ts,
                    kind: "message".into(),
                    text: match text.as_deref().map(str::trim).filter(|t| !t.is_empty()) {
                        Some(t) => format!("在「{name}」说：{}", cut(t, TIMELINE_CUT)),
                        None if text.is_none() => {
                            format!("在「{name}」发了一条消息（原文已过保留期限）")
                        }
                        None => format!("在「{name}」发了一条消息"),
                    },
                    href: format!("#/channels/space/{space}"),
                })
            })
            .map_err(StoreError::from)?;
        Ok(rows
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(StoreError::from)?)
    }
}
