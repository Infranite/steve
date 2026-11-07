//! Messages without a provider id: how the Hub tells them apart and matches their copies.
//!
//! An adapter without platform message ids ([`crate::ext::MessageIds::None`]) gives each delivery
//! its remote's own id when it has one ([`crate::InboundMessage::source_id`]): a redelivery with the
//! same id through the same connection is the message stored before (`chan_message_seen`). Every
//! message also gets a content key: a versioned hash of its sender (in the side's handle system),
//! its text and the kinds of its attachments. A copy that has no provider id, or whose other copy
//! has none, is matched by it within the time precision of the sides that see the space, nearest
//! first; a connection never matches a message it supplied itself, so two identical messages stay
//! two. When the sender's party has other handles than the one its key was made of (a side that
//! reported a handle of another system first), a copy without a key match also tries the keys made
//! of each of them. A copy with a provider id upgrades the message to it. Messages stored without
//! one get an id of the Hub's own (`fp:…`), which never leaves the Hub.
//!
//! Known costs: a redelivery without a delivery id is a new message, as two identical messages
//! are, since a connection never matches its own copies: an adapter without platform ids gives a
//! `source_id` or keeps its own cursor so it does not deliver a message twice. An edit's new text
//! is not applied.

use rusqlite::{Connection, OptionalExtension, params};
use serde_json::Value;
use sha2::{Digest, Sha256};
use steve_modules::accounts::repo as accounts;
use steve_store::Tx;

use crate::channel::HandleRef;
use crate::ext::PRECISION_MS;
use crate::{ChannelResult, Channels};

/// The version of the content key's formula (part of every key).
pub const KEY_VERSION: &str = "v1";
/// The prefix of the Hub's own ids of messages without a provider id.
const FP: &str = "fp:";
/// How far a provider's time of a send may be from the Hub's when the provider did not say it.
pub const SEND_SLACK_MS: i64 = 30_000;
/// The time precision of a side that declared none.
const DEFAULT_PRECISION_MS: i64 = 1000;

/// A Hub-made id (`fp:…`), not a provider's.
pub fn is_fallback(ext: &str) -> bool {
    ext.starts_with(FP)
}

/// A provider id as it may leave the Hub: `None` for none, an empty one or a Hub-made one.
pub fn platform(ext: Option<String>) -> Option<String> {
    ext.filter(|x| !x.is_empty() && !is_fallback(x))
}

/// The content key of a message from `system:handle` with this text and these attachments:
/// `v1:` and the first 32 hex digits of a SHA-256. The text is compared as echoes are
/// ([`crate::arbiter::echo_key`]).
pub fn key_of(system: &str, handle: &str, text: &str, attachments: &[Value]) -> String {
    let kinds: Vec<&str> = attachments
        .iter()
        .filter_map(|a| a["type"].as_str())
        .collect();
    let body = format!(
        "{system}:{handle}\n{}\n{}",
        crate::arbiter::echo_key(text),
        kinds.join(",")
    );
    let hash = hex::encode(Sha256::digest(body.as_bytes()));
    format!("{KEY_VERSION}:{}", &hash[..32])
}

/// Whom a key names the sender by: the party's first handle in `system` that is valid everywhere,
/// so every copy of a message names the same person the same way whichever handle its side
/// reported; else the handle reported.
pub fn key_sender(
    c: &Connection,
    ws: &str,
    party: Option<&str>,
    system: &str,
    reported: &HandleRef,
) -> ChannelResult<(String, String)> {
    if let Some(p) = party
        && let Some(sys) = accounts::system(c, ws, system)?
    {
        let h: Option<String> = c
            .query_row(
                "SELECT h.handle FROM account_handles h JOIN accounts a ON a.id = h.account_id
                  WHERE a.workspace_id = ?1 AND h.workspace_id = ?1 AND h.family = ?2 AND a.party_id = ?3
                    AND h.connection_id IS NULL
                  ORDER BY a.system_id <> ?4, h.handle <> a.handle, h.created, h.rowid LIMIT 1",
                params![ws, sys.family, p, sys.id],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(h) = h {
            return Ok((sys.id, h));
        }
    }
    Ok((reported.system.clone(), reported.handle.trim().to_string()))
}

/// The handle system a key of connection side `adapter` uses: its declared one, else `reported`.
pub fn key_system(ch: &Channels, adapter: &str, reported: &str) -> String {
    ch.decl(adapter)
        .map(|d| d.handle_system.clone())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| reported.to_string())
}

/// The content key of a message sent through connection `conn` with this text: its account is the
/// sender.
pub fn out_key(
    ch: &Channels,
    c: &Connection,
    ws: &str,
    conn: &str,
    text: &str,
) -> ChannelResult<Option<String>> {
    let row: Option<(String, String, String, Option<String>)> = c
        .query_row(
            "SELECT cn.adapter, a.system_id, a.handle, a.party_id FROM connections cn
               JOIN accounts a ON a.id = cn.account_id WHERE cn.id = ? AND cn.workspace_id = ?",
            params![conn, ws],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let Some((adapter, system, handle, party)) = row else {
        return Ok(None);
    };
    let want = key_system(ch, &adapter, &system);
    let (s, h) = key_sender(
        c,
        ws,
        party.as_deref(),
        &want,
        &HandleRef::new(system, handle),
    )?;
    Ok(Some(key_of(&s, &h, text, &[])))
}

/// How far apart two copies' times may be in space `space_ext`: the coarsest precision of the
/// sides that see it and of `adapter`'s.
pub fn tolerance(
    ch: &Channels,
    c: &Connection,
    ws: &str,
    adapter: &str,
    space_ext: &str,
) -> ChannelResult<i64> {
    let precision = |a: &str| {
        ch.decl(a)
            .map(|d| d.created_precision_ms)
            .unwrap_or(DEFAULT_PRECISION_MS)
            .clamp(*PRECISION_MS.start(), *PRECISION_MS.end())
    };
    let mut st = c.prepare_cached(
        "SELECT DISTINCT cn.adapter FROM spaces s JOIN space_identities i ON i.space_id = s.id
           JOIN connections cn ON cn.id = i.connection_id
          WHERE s.workspace_id = ?1 AND s.external_id = ?2",
    )?;
    let others = st
        .query_map(params![ws, space_ext], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(others
        .iter()
        .map(|a| precision(a))
        .fold(precision(adapter), i64::max))
}

/// A stored message a copy was matched to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub id: String,
    pub space_id: String,
    /// One of Steve's sends (read back), not a received message.
    pub out: bool,
}

/// The message connection `conn` delivered (or sent) before with delivery id `source`.
pub fn by_source(c: &Connection, conn: &str, source: &str) -> ChannelResult<Option<Found>> {
    Ok(c.query_row(
        "SELECT m.id, m.space_id, m.direction = 'out' FROM chan_message_seen x
           JOIN chan_messages m ON m.id = x.message_id
          WHERE x.connection_id = ? AND x.source_id = ?",
        params![conn, source],
        |r| {
            Ok(Found {
                id: r.get(0)?,
                space_id: r.get(1)?,
                out: r.get(2)?,
            })
        },
    )
    .optional()?)
}

/// What a copy is matched by.
#[derive(Debug, Clone)]
pub struct Probe<'a> {
    pub ws: &'a str,
    pub conn: &'a str,
    /// The space the copy came into.
    pub space_id: &'a str,
    pub space_ext: &'a str,
    /// The connection is your own (`mode=self`): it may match the copies of any other connection
    /// of the chat, as messages seen elsewhere do ([`crate::messages::seen_elsewhere`]).
    pub own: bool,
    pub key: &'a str,
    pub created: i64,
    pub tol: i64,
    /// The copy has a provider id: only a message without one can be its other copy.
    pub has_id: bool,
    pub party: Option<&'a str>,
}

/// The candidates' common conditions: the same chat, a received message or a sent one, a space the
/// connection may match in, one side without a provider id, not supplied by this connection.
const CANDIDATE: &str = "FROM spaces s JOIN chan_messages m ON m.space_id = s.id
       LEFT JOIN channel_settings cs ON cs.connection_id = s.connection_id
      WHERE s.workspace_id = ?1 AND s.external_id = ?2 AND m.workspace_id = ?1
        AND (m.direction = 'in' OR m.status = 'sent')
        AND (s.id = ?3 OR ?4 OR cs.mode = 'self')
        AND (NOT ?5 OR m.external_id IS NULL OR m.external_id LIKE 'fp:%')
        AND coalesce(m.fallback_from, '') <> ?6
        AND NOT EXISTS (SELECT 1 FROM chan_message_seen x WHERE x.message_id = m.id AND x.connection_id = ?6)";

/// The other copy of the message by content key: within the tolerance (wider for a send whose
/// provider time is unknown), the nearest in time, then the oldest.
pub fn by_content(c: &Connection, p: &Probe) -> ChannelResult<Option<Found>> {
    by_keys(c, p, &[p.key.to_string()])
}

/// The keys the same message may have been stored under by a side that named its sender by
/// another handle (one of another system, before the party had one in the side's): one for each
/// handle of the sender's party, other than the probe's own key.
pub fn other_keys(
    c: &Connection,
    p: &Probe,
    text: &str,
    attachments: &[Value],
) -> ChannelResult<Vec<String>> {
    let Some(party) = p.party else {
        return Ok(Vec::new());
    };
    let mut st = c.prepare_cached(
        "SELECT a.system_id, a.handle FROM accounts a WHERE a.workspace_id = ?1 AND a.party_id = ?2
         UNION
         SELECT a.system_id, h.handle FROM account_handles h JOIN accounts a ON a.id = h.account_id
          WHERE a.workspace_id = ?1 AND h.workspace_id = ?1 AND a.party_id = ?2
         LIMIT 64",
    )?;
    let handles = st
        .query_map(params![p.ws, party], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut keys: Vec<String> = handles
        .iter()
        .map(|(s, h)| key_of(s, h.trim(), text, attachments))
        .filter(|k| k != p.key)
        .collect();
    keys.sort();
    keys.dedup();
    Ok(keys)
}

/// [`by_content`] for any of `keys`.
pub fn by_keys(c: &Connection, p: &Probe, keys: &[String]) -> ChannelResult<Option<Found>> {
    if keys.is_empty() {
        return Ok(None);
    }
    let keys = serde_json::to_string(keys).unwrap_or_else(|_| "[]".into());
    let slack = p.tol + SEND_SLACK_MS;
    // a message's time: the provider's when known, else the Hub's send time (a draft's row is made
    // long before it goes out)
    let at = "coalesce(m.ext_created, CASE WHEN m.direction = 'out' THEN coalesce(m.sent, m.created) ELSE m.created END)";
    let mut st = c.prepare_cached(&format!(
        "SELECT m.id, m.space_id, m.direction = 'out', {at},
                m.direction = 'out' AND m.ext_created IS NULL, m.created, m.rowid
           {CANDIDATE}
            AND m.content_key IN (SELECT value FROM json_each(?7)) AND {at} BETWEEN ?8 - ?9 AND ?8 + ?9
          ORDER BY m.created DESC, m.rowid DESC LIMIT 100"
    ))?;
    let rows = st
        .query_map(
            params![
                p.ws,
                p.space_ext,
                p.space_id,
                p.own,
                p.has_id,
                p.conn,
                keys,
                p.created,
                slack
            ],
            |r| {
                Ok((
                    Found {
                        id: r.get(0)?,
                        space_id: r.get(1)?,
                        out: r.get(2)?,
                    },
                    r.get::<_, i64>(3)?,
                    r.get::<_, bool>(4)?,
                    (r.get::<_, i64>(5)?, r.get::<_, i64>(6)?),
                ))
            },
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows
        .into_iter()
        .filter_map(|(f, t, loose, created)| {
            let d = (t - p.created).abs();
            (d <= if loose { slack } else { p.tol }).then_some((d, created, f))
        })
        // the oldest: by the time stored, then the order stored in
        .min_by_key(|(d, created, _)| (*d, *created))
        .map(|(_, _, f)| f))
}

/// When every side times its messages to the ms: the one other message of the same sender at
/// exactly the same time (an edit changes the text, not the time). Several: none.
pub fn by_exact_time(c: &Connection, p: &Probe) -> ChannelResult<Option<Found>> {
    let Some(party) = p.party else {
        return Ok(None);
    };
    if p.tol != 1 {
        return Ok(None);
    }
    let mut st = c.prepare_cached(&format!(
        "SELECT m.id, m.space_id, m.direction = 'out'
           {CANDIDATE}
            AND m.sender_party_id = ?7
            AND coalesce(m.ext_created, CASE WHEN m.direction = 'in' THEN m.created END) = ?8
          LIMIT 2"
    ))?;
    let mut rows = st
        .query_map(
            params![
                p.ws,
                p.space_ext,
                p.space_id,
                p.own,
                p.has_id,
                p.conn,
                party,
                p.created
            ],
            |r| {
                Ok(Found {
                    id: r.get(0)?,
                    space_id: r.get(1)?,
                    out: r.get(2)?,
                })
            },
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(if rows.len() == 1 { rows.pop() } else { None })
}

/// The Hub's own id for a message without a provider id in `space`:
/// `fp:v1:{hash}:{created}:{n}`, `n` one past the highest in use with the same key and time (a
/// message upgraded to a provider id no longer holds its Hub-made one, so counting them could give
/// one still in use).
pub fn new_id(c: &Connection, space: &str, key: &str, created: i64) -> ChannelResult<String> {
    let base = format!("{FP}{key}:{created}:");
    let mut n: i64 = c.query_row(
        "SELECT coalesce(max(CAST(substr(external_id, length(?2) + 1) AS INTEGER)) + 1, 0)
           FROM chan_messages WHERE space_id = ?1 AND substr(external_id, 1, length(?2)) = ?2",
        params![space, base],
        |r| r.get(0),
    )?;
    // never one in use, whatever the stored ones look like
    let taken = |id: &str| -> rusqlite::Result<bool> {
        c.query_row(
            "SELECT EXISTS (SELECT 1 FROM chan_messages WHERE space_id = ? AND external_id = ?)",
            params![space, id],
            |r| r.get(0),
        )
    };
    while taken(&format!("{base}{n}"))? {
        n += 1;
    }
    Ok(format!("{base}{n}"))
}

/// A copy with provider id `ext` arrived for message `id`: the message takes it when it has none
/// (or only a Hub-made one), and the replies that named the Hub-made one name it instead.
pub fn upgrade(tx: &Tx, id: &str, ext: &str) -> ChannelResult<()> {
    if ext.is_empty() || is_fallback(ext) {
        return Ok(());
    }
    let row: Option<(String, Option<String>)> = tx
        .query_row(
            "SELECT space_id, external_id FROM chan_messages WHERE id = ?",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((space, old)) = row else {
        return Ok(());
    };
    if old.as_deref().is_some_and(|o| !is_fallback(o)) {
        return Ok(());
    }
    let n = tx.execute(
        "UPDATE OR IGNORE chan_messages SET external_id = ? WHERE id = ?",
        params![ext, id],
    )?;
    if n == 1
        && let Some(old) = old
    {
        tx.execute(
            "UPDATE chan_messages SET reply_to = ? WHERE space_id = ? AND reply_to = ?",
            params![ext, space, old],
        )?;
    }
    Ok(())
}

/// Connection `conn` supplied a copy of message `id`: its delivery id is kept, and a copy without a
/// provider id marks the message as supplied by it (so its next identical message is never taken
/// for this one).
pub fn supplied(
    tx: &Tx,
    id: &str,
    conn: &str,
    source: Option<&str>,
    without_id: bool,
) -> ChannelResult<()> {
    if let Some(s) = source {
        tx.execute(
            "INSERT OR IGNORE INTO chan_message_seen (connection_id, source_id, message_id) VALUES (?, ?, ?)",
            params![conn, s, id],
        )?;
    }
    if without_id {
        tx.execute(
            "UPDATE chan_messages SET fallback_from = coalesce(fallback_from, ?) WHERE id = ?",
            params![conn, id],
        )?;
    }
    Ok(())
}

/// A read-back of send `id` tells its provider time: kept when the send did not say it.
pub fn fill_created(tx: &Tx, id: &str, created: i64) -> ChannelResult<()> {
    tx.execute(
        "UPDATE chan_messages SET ext_created = coalesce(ext_created, ?) WHERE id = ? AND direction = 'out'",
        params![created, id],
    )?;
    Ok(())
}
