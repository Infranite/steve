//! Whether you are there yourself: on your own account (`Mode::Own`) a reply drafted in your name is
//! wrong once you answered yourself, however well it is written. One function decides it,
//! [`engaged`] (over the store: [`owner_engaged`]; with the turn a message was merged into,
//! [`stop`]), and every step that would say something as you asks it again, because drafting takes
//! minutes and you may answer meanwhile:
//!
//! 1. before drafting ([`crate::respond`]: no composer runs, no card opens);
//! 2. after the composer answered, before its draft or question opens ([`crate::replies`]);
//! 3. before YOLO approves the draft or retries it ([`crate::outbound`]'s decision handler);
//! 4. before it is sent, unless you approved it yourself ([`crate::outbound`]'s send).
//!
//! Gates 3 and 4 also hold what the space's `reply_as_you` no longer allows (a draft made before you
//! changed it). And when your own message comes in, every draft and card about an older message in
//! that space that you are now there for closes as superseded ([`supersede`], “你已亲自回复”).
//!
//! You are engaged for a message when you said something after it (`Replied`) — in a private chat
//! anything; in a group only what is linked to it ([`about`]): a reply to it or to its sender's
//! message in its reply chain or its turn, @-mentioning its sender, or its sender going on right
//! after you (nobody in between). Merely speaking in the group soon after it is not an answer: in a
//! busy group your remark to someone else would silence everyone who spoke in the minutes before
//! (the owner's decision of 2026-11-17, from the drafts they dismissed). Or you are engaged when you
//! said something shortly before it ([`OWNER_LIVE_MS`]) and it goes on with that conversation
//! (`Live`): in a private chat always; in a group when it answers or @-mentions you, is in the reply
//! chain of one of your messages, or nobody spoke between you and it. Spaces are judged by time, the
//! thread only adds a reply-chain signal: most messages are alone in their thread, and a private chat
//! gives every message its own. Only your own words count: what another assistant posted on your
//! account by itself ([`crate::arbiter::looks_automated`]) does not, nor what Steve sent as you.
//!
//! Times: a message's time is the provider's (when it was said); relations between messages compare
//! those. The moment a gate judges is the later of the Hub's clock and the newest time stored for
//! the space ([`owner_engaged`]): a message Steve has stored has happened, even when the provider's
//! clock runs ahead of the Hub's, and your own message that reached the Hub late (a rate-limited
//! gateway) counts as soon as it is stored — gates 3 and 4 judge again right before YOLO approves
//! and before anything is sent, so one that missed an earlier gate is still seen.
//!
//! Consecutive messages of one sender are one turn ([`burst`]) only while nobody else (you included)
//! speaks between them, each follows the one before within [`BURST_GAP_MS`], and none of them has a
//! reply in your name approved or sent already (that one was answered: it ends the turn and stays
//! out). The composer answers the turn together; the earlier messages are marked as taken by it
//! (`chan_messages.turn_of`), so their own drafts and cards close and are never opened, approved by
//! YOLO or sent, whichever composer finishes first.
//!
//! The window lengths were swept with the offline reply eval (`examples/reply_eval.rs sweep`) on the
//! operator's cases; see the constants. They are provisional: one day of cases.

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde_json::{Value, json};
use steve_store::Tx;
use steve_store::base;

use crate::ChannelResult;
use crate::respond::SUBJECT_MESSAGE;
use crate::spaces::ReplyAsYou;

/// You said something in the space this long before the message, and it goes on with that
/// conversation: you are there. In a group, its sender answering you within this after you spoke is
/// still that conversation ([`about`]). In the eval sweep (`examples/reply_eval.rs sweep`, 44 cases of one day) every window
/// from 100 s to 9 min gave the same decisions; shorter ones missed you chatting a minute and a half
/// earlier, 10 min and more also silenced a message that needed you nine minutes after you last
/// spoke. 3 min sits inside, away from both ends. Revisit with more cases.
pub const OWNER_LIVE_MS: i64 = 180_000;
/// Two messages of one sender are one turn when the second follows within this (and nobody spoke
/// between). In the same sweep every gap from 10 s to 90 s gave the same decisions; from 100 s two
/// separate questions a minute and a half apart were answered as one, from 7.5 min an unrelated later
/// message took over one that needed you. 1 min sits inside.
pub const BURST_GAP_MS: i64 = 60_000;
/// At most this many messages make one turn (the newest).
pub const BURST_MAX: usize = 10;
/// Lines read around a message to decide (a busy group's minutes).
const AROUND_MAX: usize = 400;

/// `result.superseded` of a card closed because you answered yourself.
pub const BY_YOU: &str = "你已亲自回复";
/// `result.superseded` of a card closed because the sender went on and the turn was answered as one.
pub const BY_LATER: &str = "对方接着发了消息，已合在一起处理";
/// `result.superseded` of a card closed because the space is set not to reply for you any more.
pub const BY_SETTING: &str = "这里已设为不替你回复";
/// What a message you were there for says on its event.
pub const LABEL_REPLIED: &str = "你已亲自回复，不替你起草";
pub const LABEL_LIVE: &str = "你正在这里聊，不替你起草";
/// The reason codes (logs, the eval, audit).
pub const REASON_REPLIED: &str = "owner_replied";
pub const REASON_LIVE: &str = "owner_live";
pub const REASON_MERGED: &str = "merged";
pub const REASON_SETTING: &str = "reply_as_you";
/// The reply statuses that mean a message was answered in your name: approved and waiting, out, out
/// or not (unknown), taken back after it went out.
const ANSWERED: &str = "('pending', 'sent', 'unknown', 'recalled')";

/// One message of a space, as the presence rules see it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Said {
    pub id: String,
    /// When it was written (ms).
    pub at: i64,
    /// You wrote it yourself: from your account or contact, and not an automatic reply another
    /// assistant posted there ([`crate::arbiter::looks_automated`]).
    pub owner: bool,
    /// Who wrote it: equal for one sender.
    pub sender: String,
    /// The account that wrote it (what an @-mention names).
    pub sender_account: Option<String>,
    pub thread_key: Option<String>,
    /// Its provider id, and the provider id of the message it answers.
    pub external_id: Option<String>,
    pub reply_to: Option<String>,
    /// It @-mentions you.
    pub mentions_owner: bool,
    /// The accounts it @-mentions.
    pub mentions: Vec<String>,
    /// A reply to it in your name is approved or out already: it was answered.
    pub answered: bool,
}

/// You are there for a message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Engaged {
    /// You said something about it after it: your latest such message.
    Replied { message_id: String, at: i64 },
    /// You said something shortly before it and it goes on with that: your latest such message.
    Live { message_id: String, at: i64 },
}

impl Engaged {
    pub fn reason(&self) -> &'static str {
        match self {
            Self::Replied { .. } => REASON_REPLIED,
            Self::Live { .. } => REASON_LIVE,
        }
    }

    pub fn label(&self) -> &'static str {
        match self {
            Self::Replied { .. } => LABEL_REPLIED,
            Self::Live { .. } => LABEL_LIVE,
        }
    }

    /// When you last spoke.
    pub fn at(&self) -> i64 {
        match self {
            Self::Replied { at, .. } | Self::Live { at, .. } => *at,
        }
    }
}

/// Why nothing is said in your name about a message any more (the gates).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Stop {
    /// You are there yourself.
    You { engaged: Engaged },
    /// The sender went on: the message is answered with their later one, `into`.
    Merged { into: String },
    /// The space no longer lets Steve reply for you (gates 3 and 4).
    Setting { reply_as_you: ReplyAsYou },
}

impl Stop {
    pub fn reason(&self) -> &'static str {
        match self {
            Self::You { engaged } => engaged.reason(),
            Self::Merged { .. } => REASON_MERGED,
            Self::Setting { .. } => REASON_SETTING,
        }
    }

    /// What the message's event says.
    pub fn label(&self) -> &'static str {
        match self {
            Self::You { engaged } => engaged.label(),
            Self::Merged { .. } => BY_LATER,
            Self::Setting { .. } => BY_SETTING,
        }
    }

    /// `result.superseded` of the card it closes.
    pub fn why(&self) -> &'static str {
        match self {
            Self::You { .. } => BY_YOU,
            Self::Merged { .. } => BY_LATER,
            Self::Setting { .. } => BY_SETTING,
        }
    }
}

/// The provider ids of `target` and of its sender's messages linked to it: in its reply chain (the
/// same thread, the message it answers, one answering it) or in its turn (the sender's messages
/// right before and after it, nobody in between, each within [`BURST_GAP_MS`] of the next).
fn chain(target: &Said, around: &[&Said]) -> Vec<String> {
    let mut all: Vec<&Said> = around
        .iter()
        .copied()
        .filter(|s| s.id != target.id)
        .collect();
    all.push(target);
    all.sort_by_key(|s| s.at);
    let at = all.iter().position(|s| s.id == target.id).unwrap_or(0);
    let mut turn = vec![target];
    for w in all[..at].iter().rev() {
        let last = turn.last().map_or(target.at, |s| s.at);
        if w.sender != target.sender || last - w.at > BURST_GAP_MS {
            break;
        }
        turn.push(w);
    }
    let mut next = target.at;
    for w in &all[at + 1..] {
        if w.sender != target.sender || w.at - next > BURST_GAP_MS {
            break;
        }
        turn.push(w);
        next = w.at;
    }
    let linked = |s: &Said| {
        s.sender == target.sender
            && ((target.thread_key.is_some() && s.thread_key == target.thread_key)
                || (s.external_id.is_some() && s.external_id == target.reply_to)
                || (target.external_id.is_some() && s.reply_to == target.external_id))
    };
    turn.into_iter()
        .chain(around.iter().copied().filter(|s| linked(s)))
        .filter_map(|s| s.external_id.clone())
        .collect()
}

/// Is what you said in `o`, after `target`, an answer to it? In a group only by how it is linked,
/// never by time alone: `o` replies to it or to its sender's message in its reply chain or turn
/// ([`chain`]), is in its thread without replying to someone else, @-mentions its sender, or its
/// sender goes on right after `o` (the next message in the space is theirs, within `live_ms`).
fn about(target: &Said, o: &Said, around: &[&Said], live_ms: i64) -> bool {
    let chain = chain(target, around);
    let replies = o.reply_to.as_ref().is_some_and(|r| chain.contains(r));
    let in_thread =
        target.thread_key.is_some() && o.thread_key == target.thread_key && o.reply_to.is_none();
    let mentions = target
        .sender_account
        .as_ref()
        .is_some_and(|a| o.mentions.contains(a));
    let answered_back = around
        .iter()
        .filter(|s| s.id != o.id && s.at > o.at)
        .min_by_key(|s| s.at)
        .is_some_and(|n| n.sender == target.sender && n.at - o.at <= live_ms);
    replies || in_thread || mentions || answered_back
}

/// Are you there for `target` at `now`? `around`: the space's other messages from `live_ms` before
/// it up to `now` (any order; the target itself is ignored). `dm`: a private chat.
pub fn engaged(
    dm: bool,
    target: &Said,
    around: &[Said],
    now: i64,
    live_ms: i64,
) -> Option<Engaged> {
    let others: Vec<&Said> = around.iter().filter(|s| s.id != target.id).collect();
    // after it: in a private chat whatever you said (you saw it); in a group what is about it
    if let Some(r) = others
        .iter()
        .filter(|s| s.owner && s.at > target.at && s.at <= now)
        .filter(|s| dm || about(target, s, &others, live_ms))
        .max_by_key(|s| s.at)
    {
        return Some(Engaged::Replied {
            message_id: r.id.clone(),
            at: r.at,
        });
    }
    let before: Vec<&Said> = others
        .iter()
        .copied()
        .filter(|s| s.at <= target.at)
        .collect();
    let yours: Vec<&Said> = before
        .iter()
        .copied()
        .filter(|s| s.owner && target.at - s.at <= live_ms)
        .collect();
    let last = yours.iter().max_by_key(|s| s.at)?;
    let goes_on = dm
        || target.mentions_owner
        || yours.iter().any(|s| {
            let answers = target.reply_to.is_some() && target.reply_to == s.external_id;
            let thread = target.thread_key.is_some() && target.thread_key == s.thread_key;
            answers || thread
        })
        // nobody spoke between you and it
        || before
            .iter()
            .max_by_key(|s| s.at)
            .is_some_and(|s| s.owner);
    goes_on.then(|| Engaged::Live {
        message_id: last.id.clone(),
        at: last.at,
    })
}

/// The ids of the turn `target` ends, oldest first: it and the messages right before it from the same
/// sender, while nobody else spoke between, each follows the last within `gap_ms` and none was
/// answered in your name already (at most [`BURST_MAX`]). `before`: the space's messages before it,
/// any order.
pub fn burst(target: &Said, before: &[Said], gap_ms: i64) -> Vec<String> {
    let mut earlier: Vec<&Said> = before
        .iter()
        .filter(|s| s.id != target.id && s.at <= target.at)
        .collect();
    earlier.sort_by_key(|s| std::cmp::Reverse(s.at));
    let mut out = vec![target.id.clone()];
    let mut next = target.at;
    for s in earlier {
        if out.len() >= BURST_MAX
            || s.owner
            || s.answered
            || s.sender != target.sender
            || next - s.at > gap_ms
        {
            break;
        }
        out.push(s.id.clone());
        next = s.at;
    }
    out.reverse();
    out
}

// ---------------------------------------------------------------- over the store

/// Your contacts in `ws` (the workspace owners', and those merged into them) and the personal
/// accounts of the space's own connection, as SQL: `?1` = ws, `?2` = space.
pub(crate) const MINE: &str = "WITH RECURSIVE mine(id) AS (
       SELECT p.id FROM parties p JOIN workspace_members wm
           ON wm.user_id = p.user_id AND wm.workspace_id = ?1 AND wm.role = 'owner'
        WHERE p.workspace_id = ?1
       UNION SELECT a.party_id FROM spaces s JOIN connections c ON c.id = s.connection_id
           JOIN accounts a ON a.id = c.account_id
           JOIN channel_settings cs ON cs.connection_id = c.id AND cs.mode = 'self'
        WHERE s.id = ?2
       UNION SELECT p.id FROM parties p JOIN mine ON p.merged_into = mine.id
     ), my_accounts(id) AS (SELECT a.id FROM accounts a WHERE a.party_id IN (SELECT id FROM mine))";

/// A stored inbound message from your account or contact, as an SQL expression over `m` with
/// [`MINE`] in scope (who sent it only: [`said_of`] also leaves out automatic replies).
const IS_MINE: &str = "(m.sender_party_id IN (SELECT id FROM mine)
       OR m.sender_account_id IN (SELECT id FROM my_accounts))";

fn said_of(r: &rusqlite::Row<'_>, my: &[String]) -> rusqlite::Result<Said> {
    let mentions: Option<String> = r.get(7)?;
    let mentions: Vec<String> = mentions
        .and_then(|s| serde_json::from_str::<Vec<Value>>(&s).ok())
        .unwrap_or_default()
        .iter()
        .filter_map(|m| m["account_id"].as_str().map(String::from))
        .collect();
    let from_you: bool = r.get(2)?;
    let text: String = r.get(9)?;
    Ok(Said {
        id: r.get(0)?,
        at: r.get(1)?,
        // another assistant's automatic reply on your account is not you (as `inbound` records it)
        owner: from_you && !crate::arbiter::looks_automated(&text),
        sender: r.get(3)?,
        sender_account: r.get(8)?,
        thread_key: r.get(4)?,
        external_id: r.get(5)?,
        reply_to: r.get(6)?,
        mentions_owner: mentions.iter().any(|a| my.contains(a)),
        mentions,
        answered: r.get(10)?,
    })
}

const SAID_COLUMNS: &str = "m.id, m.created, {IS_MINE},
       coalesce(m.sender_party_id, m.sender_account_id, ''), m.thread_key, m.external_id, m.reply_to, m.mentions,
       m.sender_account_id, coalesce(m.text, ''),
       EXISTS (SELECT 1 FROM chan_messages o WHERE o.space_id = m.space_id AND o.direction = 'out'
                  AND o.reply_to = m.external_id AND o.status IN {ANSWERED}
                  AND o.on_behalf = 'none' AND o.persona_id IS NULL
                  AND o.sender_account_id IN (SELECT id FROM my_accounts))";

fn columns() -> String {
    SAID_COLUMNS
        .replace("{IS_MINE}", IS_MINE)
        .replace("{ANSWERED}", ANSWERED)
}

fn my_accounts(c: &Connection, ws: &str, space: &str) -> ChannelResult<Vec<String>> {
    let mut st = c.prepare_cached(&format!("{MINE} SELECT id FROM my_accounts"))?;
    let v = st
        .query_map(params![ws, space], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(v)
}

/// An inbound message as [`Said`], with its space and whether that is a private chat; `None` when it
/// is not an inbound message of `ws`.
pub fn said(c: &Connection, ws: &str, id: &str) -> ChannelResult<Option<(Said, String, bool)>> {
    let space: Option<(String, String)> = c
        .query_row(
            "SELECT m.space_id, coalesce(s.kind, '') FROM chan_messages m JOIN spaces s ON s.id = m.space_id
              WHERE m.id = ? AND m.workspace_id = ? AND m.direction = 'in'",
            params![id, ws],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((space, kind)) = space else {
        return Ok(None);
    };
    let my = my_accounts(c, ws, &space)?;
    let s = c
        .query_row(
            &format!(
                "{MINE} SELECT {} FROM chan_messages m WHERE m.id = ?3",
                columns()
            ),
            params![ws, space, id],
            |r| said_of(r, &my),
        )
        .optional()?;
    Ok(s.map(|s| (s, space, kind == "dm")))
}

/// The space's inbound messages written from `from` to `to` (ms), oldest first.
fn lines(
    c: &Connection,
    ws: &str,
    space: &str,
    from: i64,
    to: i64,
    my: &[String],
) -> ChannelResult<Vec<Said>> {
    let mut st = c.prepare_cached(&format!(
        "{MINE} SELECT {} FROM chan_messages m
          WHERE m.space_id = ?2 AND m.direction = 'in' AND m.created >= ?3 AND m.created <= ?4
          ORDER BY m.created, m.rowid LIMIT {AROUND_MAX}",
        columns()
    ))?;
    let v = st
        .query_map(params![ws, space, from, to], |r| said_of(r, my))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(v)
}

/// Whether you said something yourself in `space` from `from` to `to` (ms): your own words only,
/// not what another assistant posted on your account by itself ([`crate::arbiter::looks_automated`])
/// nor what Steve sent as you.
pub fn you_spoke(c: &Connection, ws: &str, space: &str, from: i64, to: i64) -> ChannelResult<bool> {
    let mut st = c.prepare_cached(&format!(
        "{MINE} SELECT coalesce(m.text, '') FROM chan_messages m
          WHERE m.space_id = ?2 AND m.direction = 'in' AND m.created >= ?3 AND m.created <= ?4
            AND {IS_MINE}"
    ))?;
    let texts = st
        .query_map(params![ws, space, from, to], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(texts.iter().any(|t| !crate::arbiter::looks_automated(t)))
}

/// [`engaged`] for stored inbound message `target` at `now`, with [`OWNER_LIVE_MS`]. `None` also for
/// a message that is yours or not found.
pub fn owner_engaged(
    c: &Connection,
    ws: &str,
    target: &str,
    now: i64,
) -> ChannelResult<Option<Engaged>> {
    let Some((t, space, dm)) = said(c, ws, target)? else {
        return Ok(None);
    };
    if t.owner {
        return Ok(None);
    }
    let my = my_accounts(c, ws, &space)?;
    // everything stored has happened: the moment judged is the later of the Hub's clock and the
    // newest time stored (a provider clock ahead of the Hub's does not hide your message)
    let around = lines(c, ws, &space, t.at - OWNER_LIVE_MS, i64::MAX, &my)?;
    let now = around.iter().map(|s| s.at).fold(now.max(t.at), i64::max);
    Ok(engaged(dm, &t, &around, now, OWNER_LIVE_MS))
}

/// [`owner_engaged`] over the whole chat of inbound message `target`: the spaces every connection of
/// `ws` keeps for it (your own account's and a bot's see one chat as two spaces, each message stored
/// once, in whichever came first). What a starred contact's notification asks
/// ([`crate::starred`]): you may have answered through the other copy.
pub fn owner_engaged_chat(
    c: &Connection,
    ws: &str,
    target: &str,
    now: i64,
) -> ChannelResult<Option<Engaged>> {
    let Some((mut t, space, dm)) = said(c, ws, target)? else {
        return Ok(None);
    };
    if t.owner {
        return Ok(None);
    }
    let mut st = c.prepare_cached(
        "SELECT o.id FROM spaces s JOIN spaces o ON o.workspace_id = s.workspace_id AND o.external_id = s.external_id
          WHERE s.id = ?1 AND s.workspace_id = ?2 AND o.id <> ?1 ORDER BY o.id",
    )?;
    let others = st
        .query_map(params![space, ws], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let (mut around, mut mine) = (Vec::new(), Vec::new());
    for sp in std::iter::once(space).chain(others) {
        let my = my_accounts(c, ws, &sp)?;
        around.extend(lines(c, ws, &sp, t.at - OWNER_LIVE_MS, now.max(t.at), &my)?);
        mine.extend(my);
    }
    t.mentions_owner |= t.mentions.iter().any(|a| mine.contains(a));
    Ok(engaged(dm, &t, &around, now, OWNER_LIVE_MS))
}

/// The later message whose turn took inbound message `id` ([`take_turn`]), if one did.
pub fn merged_into(c: &Connection, id: &str) -> ChannelResult<Option<String>> {
    Ok(c.query_row(
        "SELECT turn_of FROM chan_messages WHERE id = ? AND direction = 'in'",
        [id],
        |r| r.get::<_, Option<String>>(0),
    )
    .optional()?
    .flatten())
}

/// Gates 1 to 4 for stored inbound message `target` at `now`: taken by a later message's turn, or
/// you are there ([`owner_engaged`]).
pub fn stop(c: &Connection, ws: &str, target: &str, now: i64) -> ChannelResult<Option<Stop>> {
    if let Some(into) = merged_into(c, target)? {
        return Ok(Some(Stop::Merged { into }));
    }
    Ok(owner_engaged(c, ws, target, now)?.map(|engaged| Stop::You { engaged }))
}

/// The ids of the turn stored inbound message `target` ends ([`burst`]), oldest first, without marking
/// anything: what the gates before drafting read ([`crate::decide::gate`]). Empty when it is not an
/// inbound message of `ws`.
pub fn turn(c: &Connection, ws: &str, target: &str) -> ChannelResult<Vec<String>> {
    let Some((t, space, _)) = said(c, ws, target)? else {
        return Ok(Vec::new());
    };
    let my = my_accounts(c, ws, &space)?;
    let from = t.at - BURST_GAP_MS * BURST_MAX as i64;
    let before = lines(c, ws, &space, from, t.at, &my)?;
    Ok(burst(&t, &before, BURST_GAP_MS))
}

/// The turn stored inbound message `target` ends ([`burst`]): the ids and what each says (with the
/// note on what Steve cannot read of its attachments), oldest first. The earlier messages are marked
/// as taken by it (`turn_of`) in the same transaction, so from now on no reply of their own is
/// opened, approved by YOLO or sent ([`stop`]); [`release_turn`] undoes it.
pub fn take_turn(tx: &Tx, ws: &str, target: &str) -> ChannelResult<Vec<(String, String)>> {
    let Some((t, space, _)) = said(tx, ws, target)? else {
        return Ok(Vec::new());
    };
    let my = my_accounts(tx, ws, &space)?;
    let from = t.at - BURST_GAP_MS * BURST_MAX as i64;
    let before = lines(tx, ws, &space, from, t.at, &my)?;
    let ids = burst(&t, &before, BURST_GAP_MS);
    let mut out = Vec::new();
    for id in ids {
        let (text, att): (String, Option<String>) = tx.query_row(
            "SELECT coalesce(text, ''), attachments FROM chan_messages WHERE id = ?",
            [&id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        if id != target {
            tx.execute(
                "UPDATE chan_messages SET turn_of = ? WHERE id = ?",
                params![target, id],
            )?;
        }
        let att: Value = att
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(Value::Null);
        out.push((id, crate::media::with_unread_note(&text, &att)));
    }
    Ok(out)
}

/// Undoes [`take_turn`] for `target` (its composer gave no answer): the earlier messages are their
/// own again.
pub fn release_turn(tx: &Tx, target: &str) -> ChannelResult<()> {
    tx.execute(
        "UPDATE chan_messages SET turn_of = NULL WHERE turn_of = ?",
        [target],
    )?;
    Ok(())
}

/// What [`supersede`] closes.
pub enum Answering<'a> {
    /// The answers to messages written before `before` that you are there for at `now`
    /// ([`owner_engaged`]): after your own message.
    YouThere { before: i64, now: i64 },
    /// The answers to these messages (a turn answered as one).
    Messages(&'a [String]),
    /// Every answer in the space (it no longer lets Steve reply for you).
    All,
}

/// Closes in `space` the drafts in your name (not your `/bot`'s) still waiting for you, the failed
/// ones waiting for a retry, and the hand-off cards about the messages `what` names that someone
/// else wrote: the draft is discarded, its card expires with `result.superseded = why`, `by` (your
/// message, the later one, the setting) goes to the audit. Returns how many cards closed.
pub fn supersede(
    tx: &Tx,
    ws: &str,
    space: &str,
    what: Answering<'_>,
    why: &str,
    by: &str,
) -> ChannelResult<usize> {
    let (cond, arg): (&str, rusqlite::types::Value) = match &what {
        Answering::YouThere { before, .. } => ("src.created < ?3", (*before).into()),
        Answering::Messages(ids) => (
            "src.id IN (SELECT value FROM json_each(?3))",
            serde_json::to_string(ids)?.into(),
        ),
        Answering::All => ("?3 IS NOT NULL", 1i64.into()),
    };
    // the drafts: in your name, answering someone else's message
    let drafts: Vec<(String, String, String)> = {
        let mut st = tx.prepare(&format!(
            "{MINE} SELECT o.id, o.status, src.id FROM chan_messages o
               JOIN chan_messages src ON src.space_id = o.space_id AND src.direction = 'in'
                    AND src.external_id = o.reply_to
               JOIN chan_messages m ON m.id = src.id
              WHERE o.space_id = ?2 AND o.direction = 'out' AND o.status IN ('draft', 'failed')
                AND o.on_behalf = 'none' AND o.persona_id IS NULL
                AND o.sender_account_id IN (SELECT id FROM my_accounts)
                AND NOT {IS_MINE} AND {cond}"
        ))?;
        st.query_map(params![ws, space, arg], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?
    };
    // the hand-offs: “X 找你” about someone else's message
    let asks: Vec<(String, String)> = {
        let mut st = tx.prepare(&format!(
            "{MINE} SELECT d.id, src.id FROM decisions d
               JOIN chan_messages src ON src.id = d.subject_id AND src.direction = 'in'
               JOIN chan_messages m ON m.id = src.id
              WHERE d.workspace_id = ?1 AND d.subject_type = '{SUBJECT_MESSAGE}' AND d.kind = 'ask'
                AND d.status = 'open' AND src.space_id = ?2 AND NOT {IS_MINE} AND {cond}"
        ))?;
        st.query_map(params![ws, space, arg], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    // after your message: only what you are there for now (in a group, what you said is about it)
    let mut there = std::collections::HashMap::new();
    let mut closes = |src: &str| -> ChannelResult<bool> {
        let Answering::YouThere { now, .. } = &what else {
            return Ok(true);
        };
        if let Some(v) = there.get(src) {
            return Ok(*v);
        }
        let v = owner_engaged(tx, ws, src, *now)?.is_some();
        there.insert(src.to_string(), v);
        Ok(v)
    };
    let mut closed = 0;
    for (id, status, src) in &drafts {
        if !closes(src)? {
            continue;
        }
        steve_store_sqlite::digest::status_changed(
            tx,
            steve_store_sqlite::digest::Source::ChanMessage,
            id,
            tx.execute(
                "UPDATE chan_messages SET status = 'discarded' WHERE id = ? AND status = ?",
                params![id, status],
            )?,
        )?;
        // closed while its disclosure check still ran: the record says the check did not finish
        crate::basis::checking_stopped(tx, id, crate::outbound::R_DEALT_FIRST)?;
        if let Some(d) = base::decisions::open_for(tx, ws, SUBJECT_MESSAGE, id)? {
            closed += settle(tx, &d.id, why)? as usize;
        }
        base::audit(
            tx,
            ws,
            "steve",
            "chan_message.superseded",
            id,
            &json!({"why": why, "by": by}),
        )?;
        crate::messages::event(tx, ws, space, id);
    }
    for (d, src) in &asks {
        if closes(src)? && settle(tx, d, why)? {
            closed += 1;
            base::audit(
                tx,
                ws,
                "steve",
                "chan_message.superseded",
                src,
                &json!({"why": why, "by": by, "decision": d}),
            )?;
            crate::messages::event(tx, ws, space, src);
        }
    }
    Ok(closed)
}

/// Closes one open card as superseded (`expired`, `result.superseded = why`).
pub fn settle(tx: &Tx, decision: &str, why: &str) -> ChannelResult<bool> {
    Ok(base::decisions::settle(
        tx,
        decision,
        &json!({ "superseded": why }),
        "expired",
        &Value::Null,
    )?)
}

/// The inbound message an outbound one answers (its `reply_to` in its space), when someone else wrote
/// it: what the gates of a draft in your name ask about. `None` for anything else (a new message, an
/// answer to your own `/bot`, a bot's reply).
pub fn answered_message(c: &Connection, ws: &str, out: &str) -> ChannelResult<Option<String>> {
    let row: Option<(
        String,
        Option<String>,
        String,
        Option<String>,
        Option<String>,
    )> = c
        .query_row(
            "SELECT space_id, reply_to, on_behalf, persona_id, sender_account_id FROM chan_messages
              WHERE id = ? AND workspace_id = ? AND direction = 'out'",
            params![out, ws],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .optional()?;
    let Some((space, Some(reply_to), on_behalf, None, Some(sender))) = row else {
        return Ok(None);
    };
    if on_behalf != "none" || !my_accounts(c, ws, &space)?.contains(&sender) {
        return Ok(None);
    }
    let src: Option<(String, bool)> = c
        .query_row(
            &format!(
                "{MINE} SELECT m.id, {IS_MINE} FROM chan_messages m
                  WHERE m.space_id = ?2 AND m.direction = 'in' AND m.external_id = ?3"
            ),
            params![ws, space, reply_to],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    Ok(src.filter(|(_, mine)| !mine).map(|(id, _)| id))
}

/// Gates 3 and 4 for outbound message `out` in your name answering someone else's message: the
/// space's `reply_as_you` (a reply in your name is your own account speaking), then [`stop`]. `None`
/// for anything else.
pub fn stop_out(c: &Connection, ws: &str, out: &str, now: i64) -> ChannelResult<Option<Stop>> {
    let Some(src) = answered_message(c, ws, out)? else {
        return Ok(None);
    };
    let space: String = c.query_row(
        "SELECT space_id FROM chan_messages WHERE id = ?",
        [out],
        |r| r.get(0),
    )?;
    let set = crate::spaces::reply_as_you(c, &space)?;
    if set != ReplyAsYou::Draft {
        return Ok(Some(Stop::Setting { reply_as_you: set }));
    }
    stop(c, ws, &src, now)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(id: &str, at: i64, owner: bool, sender: &str) -> Said {
        Said {
            id: id.into(),
            at,
            owner,
            sender: sender.into(),
            external_id: Some(format!("x_{id}")),
            thread_key: Some(format!("t_{id}")),
            ..Default::default()
        }
    }

    const W: i64 = OWNER_LIVE_MS;

    #[test]
    fn you_answered_after_it() {
        let t = s("t", 1_000_000, false, "a");
        let mut o = s("o", 1_030_000, true, "me");
        o.reply_to = t.external_id.clone();
        let around = [o];
        assert_eq!(
            engaged(false, &t, &around, 1_040_000, W),
            Some(Engaged::Replied {
                message_id: "o".into(),
                at: 1_030_000
            })
        );
        // not yet at the time asked
        assert_eq!(engaged(true, &t, &around, 1_020_000, W), None);
    }

    #[test]
    fn you_chatting_right_before_counts_in_a_private_chat() {
        let t = s("t", 1_000_000, false, "a");
        let around = [s("o", 1_000_000 - 60_000, true, "me")];
        assert!(matches!(
            engaged(true, &t, &around, 1_000_000, W),
            Some(Engaged::Live { .. })
        ));
        // too long ago
        let old = [s("o", 1_000_000 - W - 1, true, "me")];
        assert_eq!(engaged(true, &t, &old, 1_000_000, W), None);
    }

    #[test]
    fn in_a_group_only_when_it_goes_on_with_you() {
        let mut t = s("t", 1_000_000, false, "a");
        let you = s("o", 1_000_000 - 60_000, true, "me");
        let b = s("b", 1_000_000 - 30_000, false, "b");
        // someone else spoke between, and it neither answers nor mentions you
        assert_eq!(
            engaged(false, &t, &[you.clone(), b.clone()], 1_000_000, W),
            None
        );
        // nobody between
        assert!(engaged(false, &t, std::slice::from_ref(&you), 1_000_000, W).is_some());
        // it answers your message
        t.reply_to = you.external_id.clone();
        assert!(engaged(false, &t, &[you.clone(), b.clone()], 1_000_000, W).is_some());
        // or is in its reply chain
        t.reply_to = None;
        t.thread_key = you.thread_key.clone();
        assert!(engaged(false, &t, &[you.clone(), b.clone()], 1_000_000, W).is_some());
        // or @-mentions you
        t.thread_key = None;
        t.mentions_owner = true;
        assert!(engaged(false, &t, &[you, b], 1_000_000, W).is_some());
    }

    #[test]
    fn in_a_group_only_what_you_said_about_it_counts_as_an_answer() {
        let mut t = s("t", 1_000_000, false, "a");
        t.sender_account = Some("acc_a".into());
        let later = 1_000_000 + W + 60_000;
        let mut o = s("o", later, true, "me");
        // later, to someone else: not about it
        assert_eq!(engaged(false, &t, std::slice::from_ref(&o), later, W), None);
        // in a private chat whatever you said counts
        assert!(engaged(true, &t, std::slice::from_ref(&o), later, W).is_some());
        // soon after it is not enough by itself: your next line may be to anyone
        let mut soon = o.clone();
        soon.at = 1_000_000 + 5_000;
        assert_eq!(
            engaged(false, &t, std::slice::from_ref(&soon), later, W),
            None
        );
        // … unless its sender answers you right after it, nobody in between
        let back = s("a3", soon.at + 20_000, false, "a");
        assert!(engaged(false, &t, &[soon.clone(), back.clone()], later, W).is_some());
        let b0 = s("b0", soon.at + 10_000, false, "b");
        assert_eq!(
            engaged(false, &t, &[soon.clone(), b0, back], later, W),
            None
        );
        // an answer to it
        o.reply_to = t.external_id.clone();
        assert!(engaged(false, &t, std::slice::from_ref(&o), later, W).is_some());
        // in its reply chain
        o.reply_to = None;
        o.thread_key = t.thread_key.clone();
        assert!(engaged(false, &t, std::slice::from_ref(&o), later, W).is_some());
        // … but not when it replies to someone else's message there
        let c = s("c", 1_000_000 + 10_000, false, "c");
        o.reply_to = c.external_id.clone();
        assert_eq!(engaged(false, &t, &[c, o.clone()], later, W), None);
        // @-mentioning its sender
        o.thread_key = None;
        o.mentions = vec!["acc_a".into()];
        assert!(engaged(false, &t, std::slice::from_ref(&o), later, W).is_some());
        // an answer to another message of its sender in its turn (right after it, nobody between)
        o.mentions.clear();
        let a2 = s("a2", 1_000_000 + 30_000, false, "a");
        o.reply_to = a2.external_id.clone();
        assert!(engaged(false, &t, &[a2.clone(), o.clone()], later, W).is_some());
        // … not to a message of theirs about something else much later
        let a9 = s("a9", 1_000_000 + 3 * W, false, "a");
        let b1 = s("b1", 1_000_000 + 2 * W, false, "b");
        o.reply_to = a9.external_id.clone();
        let mut o9 = o.clone();
        o9.at = a9.at + 1_000;
        assert_eq!(engaged(false, &t, &[b1, a9, o9], o.at.max(later), W), None);
        // … not to someone else's
        let b = s("b", 1_000_000 + 30_000, false, "b");
        o.reply_to = b.external_id.clone();
        assert_eq!(engaged(false, &t, &[b, o], later, W), None);
    }

    /// A busy group (timelines shaped like the drafts the owner dismissed on 2026-11-17: only gaps,
    /// who spoke and the kind of chat). Five people talk; you say one line to one of them. It
    /// silences nobody else's message, before or after it; only the person you answered is.
    #[test]
    fn in_a_busy_group_your_remark_to_one_person_silences_nobody_else() {
        let t0 = 1_000_000;
        let mut msgs: Vec<Said> = Vec::new();
        for (i, (dt, who)) in [
            (0, "a"),
            (15_000, "b"),
            (30_000, "c"),
            (42_000, "d"),
            (55_000, "b"),
            (70_000, "e"),
        ]
        .iter()
        .enumerate()
        {
            let mut m = s(&format!("m{i}"), t0 + dt, false, who);
            m.sender_account = Some(format!("acc_{who}"));
            msgs.push(m);
        }
        // your one line, 80 s in, quoting c's message
        let mut you = s("you", t0 + 80_000, true, "me");
        you.reply_to = msgs[2].external_id.clone();
        let mut around = msgs.clone();
        around.push(you.clone());
        // the talk goes on after you: f right after you (nobody between: f is talking with you),
        // then d
        let mut next = s("m6", t0 + 90_000, false, "f");
        next.sender_account = Some("acc_f".into());
        let mut after = s("m7", t0 + 95_000, false, "d");
        after.sender_account = Some("acc_d".into());
        around.push(next.clone());
        around.push(after.clone());
        let now = t0 + 120_000;
        for m in msgs.iter().chain([&next, &after]) {
            let e = engaged(false, m, &around, now, W);
            match m.id.as_str() {
                "m2" => assert!(
                    matches!(e, Some(Engaged::Replied { .. })),
                    "the one you answered"
                ),
                "m6" => assert!(
                    matches!(e, Some(Engaged::Live { .. })),
                    "right after you, nobody between"
                ),
                _ => assert_eq!(e, None, "{} is not yours to have answered", m.id),
            }
        }
        // the old rule (anything you said within the window counts) silenced all of them
        assert!(
            msgs.iter()
                .filter(|m| you.at - m.at <= W && you.at > m.at)
                .count()
                >= 5
        );
        // an @ of the sender counts, and so does that sender answering you right back
        let mut at_e = s("you2", t0 + 100_000, true, "me");
        at_e.mentions = vec!["acc_e".into()];
        let mut around2 = around.clone();
        around2.push(at_e);
        assert!(engaged(false, &msgs[5], &around2, now, W).is_some());
        let mut back = s("m8", t0 + 101_000, false, "d");
        back.sender_account = Some("acc_d".into());
        around2.push(back);
        assert!(engaged(false, &after, &around2, now, W).is_some());
    }

    /// Case-shaped: a private chat, they write; you answer 38.5 s later, 1.7 s before the draft
    /// would open (your message reached the Hub after that). Whenever your answer is stored, the
    /// next gate sees it: in a private chat anything you said after their message is an answer.
    #[test]
    fn your_answer_that_reached_the_hub_late_still_stops_the_reply_in_a_private_chat() {
        let them = s("t", 1_000_000, false, "a");
        let you = s("o", 1_000_000 + 38_500, true, "me");
        // before it is stored: nothing to see
        assert_eq!(engaged(true, &them, &[], 1_000_000 + 40_200, W), None);
        // stored by the time YOLO would approve, or the send would go
        for at in [1_000_000 + 40_200, 1_000_000 + 600_000] {
            assert!(engaged(true, &them, std::slice::from_ref(&you), at, W).is_some());
        }
    }

    /// Case-shaped: a group of six; someone @-s several people including you; you speak 27 s later
    /// and again a minute later, quoting nobody and @-ing nobody, and the next voices are other
    /// people's. That is not an answer to them: the message still reaches you (a card or a draft),
    /// where the old window rule silenced it. Had you quoted it, it would be answered.
    #[test]
    fn in_a_group_a_cc_is_not_answered_by_you_talking_nearby() {
        let mut t = s("t", 1_000_000, false, "b");
        t.mentions_owner = true;
        t.sender_account = Some("acc_b".into());
        let you1 = s("o1", 1_000_000 + 27_000, true, "me");
        let you2 = s("o2", 1_000_000 + 69_200, true, "me");
        let c = s("c", 1_000_000 + 241_300, false, "c");
        let d = s("d", 1_000_000 + 250_200, false, "d");
        let around = [you1.clone(), you2.clone(), c.clone(), d.clone()];
        assert_eq!(engaged(false, &t, &around, 1_000_000 + 600_000, W), None);
        let mut quoted = you2.clone();
        quoted.reply_to = t.external_id.clone();
        assert!(engaged(false, &t, &[you1, quoted, c, d], 1_000_000 + 600_000, W).is_some());
    }

    #[test]
    fn a_turn_is_one_sender_without_interruption() {
        let t = s("t", 1_000_000, false, "a");
        let before = [
            s("a1", 1_000_000 - 30_000, false, "a"),
            s("a0", 1_000_000 - 60_000, false, "a"),
            s("b", 1_000_000 - 90_000, false, "b"),
            s("a_", 1_000_000 - 100_000, false, "a"),
        ];
        assert_eq!(burst(&t, &before, BURST_GAP_MS), ["a0", "a1", "t"]);
        // you between them ends it
        let before = [
            s("a1", 1_000_000 - 30_000, false, "a"),
            s("o", 1_000_000 - 40_000, true, "me"),
            s("a0", 1_000_000 - 60_000, false, "a"),
        ];
        assert_eq!(burst(&t, &before, BURST_GAP_MS), ["a1", "t"]);
        // so does a long pause
        let before = [s("a0", 1_000_000 - BURST_GAP_MS - 1, false, "a")];
        assert_eq!(burst(&t, &before, BURST_GAP_MS), ["t"]);
        // and a message already answered in your name: it ends the turn and stays out
        let mut a1 = s("a1", 1_000_000 - 20_000, false, "a");
        a1.answered = true;
        let before = [a1, s("a0", 1_000_000 - 40_000, false, "a")];
        assert_eq!(burst(&t, &before, BURST_GAP_MS), ["t"]);
    }
}
