//! Incoming channel reactions (including removal tombstones), implementing issue #277:
//! when the owner reacts to an inbound message with an emoji, Steve treats that message as
//! handled — suppressing drafting and closing waiting drafts/cards — without turning a reaction
//! into a live-presence signal for later unrelated messages.
//!
//! Provenance rule (owner ruling, TARGET.md):
//! - Both `manual` and `unknown` reactions from an identified human owner account count as handled
//!   (native Feishu user reactions cannot distinguish clicks from user-token automation).
//! - `automated` reactions (including bots) never count as handled.
//! - Steve's own outgoing user-account lifecycle reactions (`chan_reactions` with `as_user = 1`
//!   and the same `emoji`) never count as handled, even if echoed back by a bridge.
//!
//! Scope rule:
//! - Group chats: exact reacted message only (never earlier or later messages).
//! - DMs: reacted message plus the same sender's earlier unanswered burst (`presence::turn`),
//!   strictly excluding messages at equal or later timestamps outside that burst.

use serde::Serialize;
use serde_json::Value;
use steve_modules::ApiError;
use steve_store::channel_message_repo::incoming_reactions::{self as repo, UpsertReaction};
use steve_store::{Read, W};

use crate::channel::{ReactionEvent, ReactionOrigin};
use crate::{ChannelResult, Channels, messages, presence, spaces};

/// Max reactions included in one message's composer context block.
const MAX_CONTEXT_REACTIONS: usize = 50;
/// Max length of an emoji identifier or name.
const MAX_EMOJI_LEN: usize = 128;
/// Max length of external/source identifiers on a reaction event.
const MAX_EXT_ID_LEN: usize = 1024;

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct IncomingReaction {
    pub actor: messages::SenderOut,
    pub emoji: String,
    pub origin: String,
    pub at: i64,
    pub time_source: String,
    pub revision: Option<i64>,
}

/// Whether a reaction's origin qualifies as an owner-handled signal (when from a human owner
/// account and not Steve's own outgoing lifecycle echo).
pub fn qualifies_as_handled(origin: &str) -> bool {
    matches!(origin, "manual" | "unknown")
}

/// Active visible incoming reactions on `message` for visibility `mask` (`-1` for internal full
/// view). Visibility is applied before folding so private identities never leak across members.
pub fn list(r: &mut impl Read, message: &str, mask: i64) -> ChannelResult<Vec<IncomingReaction>> {
    Ok(repo::list_visible(r, message, mask)?
        .into_iter()
        .map(|row| IncomingReaction {
            actor: messages::SenderOut {
                account_id: Some(row.actor_account_id),
                party_id: row.actor_party_id,
                name: row.actor_name,
                avatar: Value::Object(Default::default()),
            },
            emoji: row.emoji,
            origin: row.origin,
            at: row.at,
            time_source: row.time_source,
            revision: row.revision,
        })
        .collect())
}

/// The original message only in groups; in DMs also the same sender's earlier unanswered burst.
fn scope(r: &mut impl Read, ws: &str, message: &str) -> ChannelResult<Vec<String>> {
    let Some((_, _, dm)) = presence::said(r, ws, message)? else {
        return Ok(Vec::new());
    };
    if dm {
        presence::turn(r, ws, message)
    } else {
        Ok(vec![message.into()])
    }
}

/// A qualifying owner reaction that handles `target`. Comparing the reacted-to messages — never
/// the timestamp of the click — prevents a reaction on an old message from silencing a newer one.
pub fn owner_reacted(
    r: &mut impl Read,
    ws: &str,
    target: &str,
) -> ChannelResult<Option<presence::Engaged>> {
    let Some((t, space, dm)) = presence::said(r, ws, target)? else {
        return Ok(None);
    };
    if t.owner {
        return Ok(None);
    }
    let mine = steve_store::channel_message_repo::presence::accounts(r, ws, &space)?;
    let candidates = repo::reacted_candidates(
        r,
        ws,
        &space,
        target,
        dm,
        t.at,
        t.at + presence::BURST_GAP_MS * presence::BURST_MAX as i64,
    )?;
    for id in candidates {
        for reaction in repo::list_visible(r, &id, -1)? {
            if qualifies_as_handled(&reaction.origin)
                && mine.iter().any(|a| a == &reaction.actor_account_id)
                && scope(r, ws, &id)?.iter().any(|m| m == target)
                && !repo::is_our_outgoing_reaction(r, &id, &reaction.emoji)?
            {
                return Ok(Some(presence::Engaged::Reacted {
                    message_id: id,
                    at: reaction.at,
                    emoji: reaction.emoji,
                }));
            }
        }
    }
    Ok(None)
}

/// Same as [`owner_reacted`] across every aliased space of the chat (e.g. bot + personal copies).
pub fn owner_reacted_chat(
    r: &mut impl Read,
    ws: &str,
    target: &str,
) -> ChannelResult<Option<presence::Engaged>> {
    owner_reacted(r, ws, target)
}

/// Close waiting drafts and hand-off cards covered by a qualifying owner reaction on `message`.
pub(crate) fn supersede(w: &mut W<'_>, ws: &str, message: &str) -> ChannelResult<usize> {
    if owner_reacted(w, ws, message)?.is_none() {
        return Ok(0);
    }
    let Some((_, space, dm)) = presence::said(w, ws, message)? else {
        return Ok(0);
    };
    if !spaces::speaks_as_you(w, ws, &space, None)? {
        return Ok(0);
    }
    // A group reaction handles only the reacted message; release any earlier turn claim so
    // earlier messages keep their own independent scope.
    if !dm {
        steve_store::channel_message_repo::turns::release(w, message)?;
    }
    // Re-check every message in scope: a tombstone or another response may have changed the burst.
    let ids = scope(w, ws, message)?
        .into_iter()
        .filter_map(|id| match owner_reacted(w, ws, &id) {
            Ok(Some(_)) => Some(Ok(id)),
            Ok(None) => None,
            Err(e) => Some(Err(e)),
        })
        .collect::<ChannelResult<Vec<_>>>()?;
    if ids.is_empty() {
        return Ok(0);
    }
    presence::supersede(
        w,
        ws,
        &space,
        presence::Answering::Messages(&ids),
        presence::BY_REACTED,
        message,
    )
}

/// Human-readable reaction context for the composer. An emoji is data, never an instruction or an
/// outbound send authorization.
pub fn context(
    r: &mut impl Read,
    message: &str,
    mine: &crate::context::Mine,
) -> ChannelResult<String> {
    let mut notes = Vec::new();
    for reaction in repo::list_visible(r, message, -1)?
        .into_iter()
        .take(MAX_CONTEXT_REACTIONS)
    {
        let yours = mine
            .accounts
            .iter()
            .any(|a| a == &reaction.actor_account_id);
        let who = if yours {
            "你"
        } else if crate::context::raw_name(&reaction.actor_name) {
            "某人"
        } else {
            reaction.actor_name.as_str()
        };
        let how = match reaction.origin.as_str() {
            "manual" => "手动",
            "automated" => "自动化，仅作上下文",
            _ => "账号表情",
        };
        notes.push(format!("{who}（{how}）对这条消息回应了 {}", reaction.emoji));
    }
    Ok(if notes.is_empty() {
        String::new()
    } else {
        format!("\n[表情：{}]", notes.join("；"))
    })
}

/// Attach any reactions that arrived before `message_id` was stored, and supersede if an owner
/// reaction already covers it.
pub(crate) fn attach(w: &mut W<'_>, ws: &str, conn: &str, message_id: &str) -> ChannelResult<()> {
    let attached = repo::attach_for_message(w, ws, conn, message_id)?;
    if attached > 0 {
        supersede(w, ws, message_id)?;
    }
    Ok(())
}

impl Channels {
    /// Persist an incoming reaction (or removal tombstone) before acknowledging the feed.
    /// Never grants send permission, replays a message, revives a closed card, or resends on
    /// removal.
    pub fn on_reaction(&self, ws: &str, conn: &str, mut ev: ReactionEvent) -> ChannelResult<()> {
        let valid = |s: &str, max: usize| {
            !s.is_empty() && s.len() <= max && !s.chars().any(char::is_control)
        };
        if !valid(&ev.emoji, MAX_EMOJI_LEN)
            || ev.at <= 0
            || ev.revision.is_some_and(|n| n <= 0)
            || ev
                .message_ext_id
                .as_deref()
                .is_some_and(|s| !valid(s, MAX_EXT_ID_LEN))
            || ev
                .message_source_id
                .as_deref()
                .is_some_and(|s| !valid(s, MAX_EXT_ID_LEN))
            || ev
                .space_ext_id
                .as_deref()
                .is_some_and(|s| !valid(s, MAX_EXT_ID_LEN))
            || (ev.message_ext_id.is_none() && ev.message_source_id.is_none())
        {
            return Err(ApiError::invalid("reaction"));
        }
        if ev.actor_is_bot {
            ev.origin = ReactionOrigin::Automated;
        }
        let change = if ev.removed { "removed" } else { "created" };
        let origin = ev.origin.code();
        let (matched, superseded) = self.store().write(|w| -> ChannelResult<(bool, usize)> {
            let Some(conn_row) = steve_store::Connections(w)
                .get(conn)?
                .filter(|c| c.workspace_id == ws)
            else {
                return Err(ApiError::not_found("连接"));
            };
            let source_label = format!("channel:{}", conn_row.adapter);
            // Actor identity (`actor_is_bot`) is separate from action origin: an automated action
            // on a human account must never reclassify that human account as a bot.
            let actor = crate::inbound::identify_handle_as(
                w,
                ws,
                conn,
                &conn_row.created_by,
                &ev.actor,
                ev.actor_name.as_deref(),
                &source_label,
                ev.actor_is_bot,
            )
            .ok_or_else(|| ApiError::invalid("reaction"))?;
            let target = repo::resolve_target(
                w,
                ws,
                conn,
                ev.space_ext_id.as_deref().unwrap_or(""),
                ev.message_ext_id.as_deref().unwrap_or(""),
                ev.message_source_id.as_deref().unwrap_or(""),
            )?;
            repo::upsert(
                w,
                &UpsertReaction {
                    ws,
                    conn,
                    space_ext_id: ev.space_ext_id.as_deref().unwrap_or(""),
                    message_ext_id: ev.message_ext_id.as_deref().unwrap_or(""),
                    message_source_id: ev.message_source_id.as_deref().unwrap_or(""),
                    message_id: target.as_deref(),
                    actor_account_id: &actor.account_id,
                    emoji: &ev.emoji,
                    origin,
                    removed: ev.removed,
                    at: ev.at,
                    time_source: ev.time_source.code(),
                    revision: ev.revision,
                },
            )?;
            if let Some(id) = target {
                let closed = supersede(w, ws, &id)?;
                if let Some(space) = steve_store::channel_message_repo::space_of(w, ws, &id)? {
                    steve_store::channel_message_repo::emit(w, ws, &space, &id);
                }
                Ok((true, closed))
            } else {
                Ok((false, 0))
            }
        })?;
        metrics::counter!(
            "steve_channel_incoming_reactions_total",
            "change" => change,
            "origin" => origin,
            "matched" => if matched { "yes" } else { "pending" },
            "superseded" => if superseded > 0 { "yes" } else { "no" },
        )
        .increment(1);
        Ok(())
    }
}
