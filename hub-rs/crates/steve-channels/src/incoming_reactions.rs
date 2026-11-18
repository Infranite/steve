//! Incoming reactions, including removal tombstones. Only an identified owner's reaction with
//! explicit manual provenance handles a message. It is never a presence/live signal for later
//! messages. Unknown provenance is context, including native Feishu user-token events: the native
//! envelope cannot distinguish an API call on the user's account from a click.

use rusqlite::{Connection, params};
use serde::Serialize;
use steve_modules::ApiError;
use steve_store::Tx;

use crate::channel::{ReactionEvent, ReactionOrigin};
use crate::{ChannelResult, Channels, messages, presence, spaces};

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct IncomingReaction {
    pub actor: messages::SenderOut,
    pub emoji: String,
    pub origin: String,
    pub at: i64,
}

/// The latest change per actor/emoji wins even when different connections saw it. A removal wins
/// equal timestamps. Apply visibility before folding, so a private identity cannot disclose events
/// to another member through a shared message.
pub fn list(c: &Connection, message: &str, mask: i64) -> ChannelResult<Vec<IncomingReaction>> {
    let mut st = c.prepare_cached(
        "WITH ranked AS (
           SELECT r.*, row_number() OVER (PARTITION BY r.actor_account_id, r.emoji
              ORDER BY r.action_at DESC, r.removed DESC, (r.origin <> 'manual') DESC) AS rank
             FROM chan_message_reactions r JOIN chan_messages m ON m.id = r.message_id
             JOIN space_identities i ON i.space_id = m.space_id AND i.connection_id = r.connection_id
            WHERE r.message_id = ?1 AND (?2 = -1 OR (i.ordinal IS NOT NULL AND ((1 << i.ordinal) & ?2) <> 0)))
         SELECT r.actor_account_id, a.party_id, coalesce(nullif(a.display_name, ''), p.name, ''),
                r.emoji, r.origin, r.action_at
           FROM ranked r JOIN accounts a ON a.id = r.actor_account_id LEFT JOIN parties p ON p.id = a.party_id
          WHERE r.rank = 1 AND r.removed = 0 ORDER BY r.action_at, r.actor_account_id, r.emoji",
    )?;
    Ok(st
        .query_map(params![message, mask], |r| {
            Ok(IncomingReaction {
                actor: messages::SenderOut {
                    account_id: Some(r.get(0)?),
                    party_id: r.get(1)?,
                    name: r.get(2)?,
                },
                emoji: r.get(3)?,
                origin: r.get(4)?,
                at: r.get(5)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?)
}

/// The original message only in groups; in DMs also the same sender's earlier unanswered burst.
fn scope(c: &Connection, ws: &str, message: &str) -> ChannelResult<Vec<String>> {
    let Some((_, _, dm)) = presence::said(c, ws, message)? else {
        return Ok(Vec::new());
    };
    if dm {
        presence::turn(c, ws, message)
    } else {
        Ok(vec![message.into()])
    }
}

/// A manual reaction that handles `target`. Comparing the reacted-to messages, never the time
/// of the click, prevents a reaction on an old message from silencing a newer question.
pub fn owner_reacted(
    c: &Connection,
    ws: &str,
    target: &str,
) -> ChannelResult<Option<presence::Engaged>> {
    let Some((t, space, dm)) = presence::said(c, ws, target)? else {
        return Ok(None);
    };
    if t.owner {
        return Ok(None);
    }
    let mine = presence::my_accounts(c, ws, &space)?;
    let mut st = c.prepare_cached(
        "SELECT DISTINCT r.message_id FROM chan_message_reactions r
           JOIN chan_messages m ON m.id = r.message_id
          WHERE m.workspace_id = ?1 AND m.space_id = ?2 AND m.direction = 'in'
            AND (m.id = ?3 OR (?4 AND m.created >= ?5 AND m.created <= ?6))",
    )?;
    let ids = st
        .query_map(
            params![
                ws,
                space,
                target,
                dm,
                t.at,
                t.at + presence::BURST_GAP_MS * presence::BURST_MAX as i64
            ],
            |r| r.get::<_, String>(0),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for id in ids {
        for r in list(c, &id, -1)? {
            if r.origin == "manual"
                && r.actor
                    .account_id
                    .as_ref()
                    .is_some_and(|a| mine.contains(a))
                && scope(c, ws, &id)?.iter().any(|m| m == target)
            {
                // Steve's own automatic lifecycle emoji is never evidence of a human response,
                // even if a bridge mistakenly labels its echo as manual.
                let ours: bool = c.query_row(
                    "SELECT EXISTS(SELECT 1 FROM chan_reactions WHERE message_id = ? AND as_user = 1 AND emoji = ?)",
                    params![id, r.emoji], |r| r.get(0),
                )?;
                if !ours {
                    return Ok(Some(presence::Engaged::Reacted {
                        message_id: id,
                        at: r.at,
                    }));
                }
            }
        }
    }
    Ok(None)
}

pub(crate) fn supersede(tx: &Tx, ws: &str, message: &str) -> ChannelResult<()> {
    if owner_reacted(tx, ws, message)?.is_none() {
        return Ok(());
    }
    let Some((_, space, dm)) = presence::said(tx, ws, message)? else {
        return Ok(());
    };
    if !spaces::speaks_as_you(tx, ws, &space, None)? {
        return Ok(());
    }
    // A group reaction does not answer the rest of a turn that the composer had taken together.
    // Its in-flight composer will stop at gate 2; the earlier messages keep their own scope.
    if !dm {
        presence::release_turn(tx, message)?;
    }
    // Recheck each candidate: a tombstone or another response may have changed the burst.
    let ids = scope(tx, ws, message)?
        .into_iter()
        .filter_map(|id| match owner_reacted(tx, ws, &id) {
            Ok(Some(_)) => Some(Ok(id)),
            Ok(None) => None,
            Err(e) => Some(Err(e)),
        })
        .collect::<ChannelResult<Vec<_>>>()?;
    presence::supersede(
        tx,
        ws,
        &space,
        presence::Answering::Messages(&ids),
        presence::BY_REACTION,
        message,
    )?;
    Ok(())
}

/// Human-readable context, with uncertain/automated provenance kept explicit. An emoji is data,
/// never an instruction to the composer or an authorization to send.
pub fn context(
    c: &Connection,
    message: &str,
    mine: &crate::context::Mine,
) -> ChannelResult<String> {
    let mut notes = Vec::new();
    for r in list(c, message, -1)?.into_iter().take(50) {
        let yours = r
            .actor
            .account_id
            .as_ref()
            .is_some_and(|a| mine.accounts.contains(a));
        let who = if yours {
            "你"
        } else if crate::context::raw_name(&r.actor.name) {
            "某人"
        } else {
            &r.actor.name
        };
        let how = match r.origin.as_str() {
            "manual" => "手动",
            "automated" => "自动化，仅作上下文",
            _ => "来源未明，仅作上下文",
        };
        notes.push(format!("{who}（{how}）对这条消息回应了 {}", r.emoji));
    }
    Ok(if notes.is_empty() {
        String::new()
    } else {
        format!("\n[表情：{}]", notes.join("；"))
    })
}

/// Resolve only targets this connection has actually seen. Feishu omits the chat id; an external
/// service may omit the platform message id and use the target's delivery id instead.
fn resolve(
    c: &Connection,
    ws: &str,
    conn: &str,
    ev: &ReactionEvent,
) -> ChannelResult<Option<String>> {
    let mut st = c.prepare_cached(
        "SELECT DISTINCT m.id FROM chan_messages m JOIN spaces s ON s.id = m.space_id
           JOIN space_identities i ON i.space_id = s.id AND i.connection_id = ?2
          WHERE m.workspace_id = ?1 AND (?3 = '' OR s.external_id = ?3)
            AND (?4 = '' OR m.external_id = ?4) AND (?5 = '' OR EXISTS (
                SELECT 1 FROM chan_message_seen x WHERE x.message_id = m.id AND x.connection_id = ?2 AND x.source_id = ?5))
          LIMIT 2",
    )?;
    let ids = st
        .query_map(
            params![
                ws,
                conn,
                ev.space_ext_id.as_deref().unwrap_or(""),
                ev.message_ext_id.as_deref().unwrap_or(""),
                ev.message_source_id.as_deref().unwrap_or("")
            ],
            |r| r.get::<_, String>(0),
        )?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok((ids.len() == 1).then(|| ids[0].clone()))
}

impl Channels {
    /// Store before acknowledging the feed. This never grants send permission, replays a message,
    /// revives a closed card, or resends something already out when a reaction is removed.
    pub fn on_reaction(&self, ws: &str, conn: &str, ev: ReactionEvent) -> ChannelResult<()> {
        let valid = |s: &str, max: usize| {
            !s.is_empty() && s.len() <= max && !s.chars().any(char::is_control)
        };
        if !valid(&ev.emoji, 128)
            || ev.at <= 0
            || ev
                .message_ext_id
                .as_deref()
                .is_some_and(|s| !valid(s, 1024))
            || ev
                .message_source_id
                .as_deref()
                .is_some_and(|s| !valid(s, 1024))
            || ev.space_ext_id.as_deref().is_some_and(|s| !valid(s, 1024))
            || (ev.message_ext_id.is_none() && ev.message_source_id.is_none())
        {
            return Err(ApiError::invalid("reaction"));
        }
        let facts = self.facts(ws, conn)?;
        self.store().transaction(|tx| {
            let target = resolve(tx, ws, conn, &ev)?;
            let space = if let Some(id) = &target {
                Some(tx.query_row("SELECT space_id FROM chan_messages WHERE id = ?", [id], |r| r.get::<_, String>(0))?)
            } else if let Some(ext) = &ev.space_ext_id {
                spaces::find(tx, ws, conn, ext)?.map(|s| s.id)
            } else {
                // With neither a known target nor a chat, the event cannot be checked against
                // blocked-chat policy (native Feishu omits the chat). Do not collect it blindly.
                return Ok(());
            };
            if let Some(space) = space {
                if spaces::attention(tx, &space)? == spaces::Attention::Blocked {
                    return Ok(());
                }
            }
            let actor = crate::inbound::identify_handle_as(tx, ws, conn, &facts.created_by,
                &ev.actor, ev.actor_name.as_deref(), "表情回应", ev.origin == ReactionOrigin::Automated)
                .ok_or_else(|| ApiError::invalid("reaction actor"))?;
            tx.execute(
                "INSERT INTO chan_message_reactions (workspace_id, connection_id, space_external_id,
                    message_external_id, message_source_id, message_id, actor_account_id, emoji, origin, removed, action_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)
                 ON CONFLICT (workspace_id, connection_id, space_external_id, message_external_id, message_source_id, actor_account_id, emoji)
                 DO UPDATE SET message_id = coalesce(excluded.message_id, chan_message_reactions.message_id),
                    origin = excluded.origin, removed = excluded.removed, action_at = excluded.action_at
                 WHERE excluded.action_at > chan_message_reactions.action_at OR
                    (excluded.action_at = chan_message_reactions.action_at AND
                     (excluded.removed > chan_message_reactions.removed OR
                      (excluded.removed = chan_message_reactions.removed AND excluded.origin <> 'manual')))",
                params![ws, conn, ev.space_ext_id.as_deref().unwrap_or(""),
                    ev.message_ext_id.as_deref().unwrap_or(""), ev.message_source_id.as_deref().unwrap_or(""),
                    target, actor.account_id, ev.emoji, ev.origin.code(), ev.removed, ev.at],
            )?;
            if let Some(id) = target {
                supersede(tx, ws, &id)?;
                let space: String = tx.query_row("SELECT space_id FROM chan_messages WHERE id = ?", [&id], |r| r.get(0))?;
                messages::event(tx, ws, &space, &id);
            }
            Ok(())
        })
    }
}

/// A reaction can precede the target message in the feed, or arrive while its body is fetched.
/// Attach it before the first presence gate. Never let an older pending create override a delete.
pub(crate) fn attach(tx: &Tx, ws: &str, conn: &str, message: &str) -> ChannelResult<()> {
    tx.execute(
        "UPDATE chan_message_reactions AS r SET message_id = ?3
          WHERE r.workspace_id = ?1 AND r.connection_id = ?2 AND r.message_id IS NULL
            AND EXISTS (SELECT 1 FROM chan_messages m JOIN spaces s ON s.id = m.space_id
                 WHERE m.id = ?3 AND (r.space_external_id = '' OR r.space_external_id = s.external_id)
                   AND (r.message_external_id = '' OR r.message_external_id = m.external_id)
                   AND (r.message_source_id = '' OR EXISTS (SELECT 1 FROM chan_message_seen x
                         WHERE x.message_id = m.id AND x.connection_id = ?2 AND x.source_id = r.message_source_id)))",
        params![ws, conn, message],
    )?;
    supersede(tx, ws, message)
}
