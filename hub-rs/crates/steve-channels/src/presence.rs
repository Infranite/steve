//! Whether you are there yourself: on your own account (`Mode::Own`) a reply drafted in your name is
//! wrong once you answered yourself, however well it is written. One function decides it,
//! [`engaged`] (over the store: [`owner_engaged`]), and every step that would say something as you
//! asks it again, because drafting takes minutes and you may answer meanwhile:
//!
//! 1. before drafting ([`crate::respond`]: no composer runs, no card opens);
//! 2. after the composer answered, before its draft or question opens ([`crate::replies`]);
//! 3. before YOLO approves the draft or retries it ([`crate::outbound`]'s decision handler);
//! 4. before it is sent, unless you approved it yourself ([`crate::outbound`]'s send).
//!
//! And when your own message comes in, every draft and card about an older message in that space
//! closes as superseded ([`supersede`], “你已亲自回复”).
//!
//! You are engaged in a space when you said something there after the message (`Replied`), or
//! shortly before it ([`OWNER_LIVE_MS`]) and the message goes on with that conversation (`Live`): in
//! a private chat always; in a group when it answers or @-mentions you, is in the reply chain of one
//! of your messages, or nobody spoke between you and it. Spaces are judged by time, not by thread:
//! most messages are alone in their thread, and a private chat gives every message its own.
//!
//! Consecutive messages of one sender are one turn ([`burst`]) only while nobody else (you included)
//! speaks between them and each follows the one before within [`BURST_GAP_MS`]: the composer answers
//! them together, and the drafts and cards of the earlier ones close as merged.
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

/// You said something in the space this long before the message, and it goes on with that
/// conversation: you are there. In the eval sweep (`examples/reply_eval.rs sweep`, 44 cases of one
/// day) every window from 100 s to 9 min gave the same decisions; shorter ones missed you chatting a
/// minute and a half earlier, 10 min and more also silenced a message that needed you nine minutes
/// after you last spoke. 3 min sits inside, away from both ends. Revisit with more cases.
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
/// What a message you were there for says on its event.
pub const LABEL_REPLIED: &str = "你已亲自回复，不替你起草";
pub const LABEL_LIVE: &str = "你正在这里聊，不替你起草";
/// The reason codes (logs, the eval, audit).
pub const REASON_REPLIED: &str = "owner_replied";
pub const REASON_LIVE: &str = "owner_live";

/// One message of a space, as the presence rules see it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Said {
    pub id: String,
    /// When it was written (ms).
    pub at: i64,
    /// You wrote it yourself (not Steve in your name).
    pub owner: bool,
    /// Who wrote it: equal for one sender.
    pub sender: String,
    pub thread_key: Option<String>,
    /// Its provider id, and the provider id of the message it answers.
    pub external_id: Option<String>,
    pub reply_to: Option<String>,
    /// It @-mentions you.
    pub mentions_owner: bool,
}

/// You are there for a message.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Engaged {
    /// You said something in the space after it: your latest such message.
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
    // after it: whatever you said, in any thread (you saw it)
    if let Some(r) = others
        .iter()
        .filter(|s| s.owner && s.at > target.at && s.at <= now)
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
/// sender, while nobody else spoke between and each follows the last within `gap_ms` (at most
/// [`BURST_MAX`]). `before`: the space's messages before it, any order.
pub fn burst(target: &Said, before: &[Said], gap_ms: i64) -> Vec<String> {
    let mut earlier: Vec<&Said> = before
        .iter()
        .filter(|s| s.id != target.id && s.at <= target.at)
        .collect();
    earlier.sort_by_key(|s| std::cmp::Reverse(s.at));
    let mut out = vec![target.id.clone()];
    let mut next = target.at;
    for s in earlier {
        if out.len() >= BURST_MAX || s.owner || s.sender != target.sender || next - s.at > gap_ms {
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
const MINE: &str = "WITH RECURSIVE mine(id) AS (
       SELECT p.id FROM parties p JOIN workspace_members wm
           ON wm.user_id = p.user_id AND wm.workspace_id = ?1 AND wm.role = 'owner'
        WHERE p.workspace_id = ?1
       UNION SELECT a.party_id FROM spaces s JOIN connections c ON c.id = s.connection_id
           JOIN accounts a ON a.id = c.account_id
           JOIN channel_settings cs ON cs.connection_id = c.id AND cs.mode = 'self'
        WHERE s.id = ?2
       UNION SELECT p.id FROM parties p JOIN mine ON p.merged_into = mine.id
     ), my_accounts(id) AS (SELECT a.id FROM accounts a WHERE a.party_id IN (SELECT id FROM mine))";

/// `owner` of a stored inbound message, as an SQL expression over `m` with [`MINE`] in scope.
const IS_MINE: &str = "(m.sender_party_id IN (SELECT id FROM mine)
       OR m.sender_account_id IN (SELECT id FROM my_accounts))";

fn said_of(r: &rusqlite::Row<'_>, my: &[String]) -> rusqlite::Result<Said> {
    let mentions: Option<String> = r.get(7)?;
    let mentions_owner = mentions
        .and_then(|s| serde_json::from_str::<Vec<Value>>(&s).ok())
        .unwrap_or_default()
        .iter()
        .any(|m| {
            m["account_id"]
                .as_str()
                .is_some_and(|a| my.iter().any(|x| x == a))
        });
    Ok(Said {
        id: r.get(0)?,
        at: r.get(1)?,
        owner: r.get(2)?,
        sender: r.get(3)?,
        thread_key: r.get(4)?,
        external_id: r.get(5)?,
        reply_to: r.get(6)?,
        mentions_owner,
    })
}

const SAID_COLUMNS: &str = "m.id, m.created, {IS_MINE},
       coalesce(m.sender_party_id, m.sender_account_id, ''), m.thread_key, m.external_id, m.reply_to, m.mentions";

fn columns() -> String {
    SAID_COLUMNS.replace("{IS_MINE}", IS_MINE)
}

fn my_accounts(c: &Connection, ws: &str, space: &str) -> ChannelResult<Vec<String>> {
    let mut st = c.prepare_cached(&format!("{MINE} SELECT id FROM my_accounts"))?;
    let v = st
        .query_map(params![ws, space], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(v)
}

/// An inbound message as [`Said`], with whether its space is a private chat; `None` when it is not
/// an inbound message of `ws`.
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
    let around = lines(c, ws, &space, t.at - OWNER_LIVE_MS, now.max(t.at), &my)?;
    Ok(engaged(dm, &t, &around, now, OWNER_LIVE_MS))
}

/// [`burst`] for stored inbound message `target`: the ids and what each says (with the note on what
/// Steve cannot read of its attachments), oldest first.
pub fn owner_burst(c: &Connection, ws: &str, target: &str) -> ChannelResult<Vec<(String, String)>> {
    let Some((t, space, _)) = said(c, ws, target)? else {
        return Ok(Vec::new());
    };
    let my = my_accounts(c, ws, &space)?;
    let from = t.at - BURST_GAP_MS * BURST_MAX as i64;
    let before = lines(c, ws, &space, from, t.at, &my)?;
    let ids = burst(&t, &before, BURST_GAP_MS);
    let mut out = Vec::new();
    for id in ids {
        let (text, att): (String, Option<String>) = c.query_row(
            "SELECT coalesce(text, ''), attachments FROM chan_messages WHERE id = ?",
            [&id],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        let att: Value = att
            .and_then(|s| serde_json::from_str(&s).ok())
            .unwrap_or(Value::Null);
        out.push((id, crate::media::with_unread_note(&text, &att)));
    }
    Ok(out)
}

/// What [`supersede`] closes: the answers to messages written before a time, or to these messages.
pub enum Answering<'a> {
    Before(i64),
    Messages(&'a [String]),
}

/// Closes in `space` the drafts in your name (not your `/bot`'s) still waiting for you, the failed
/// ones waiting for a retry, and the hand-off cards about the messages `what` names that someone
/// else wrote: the draft is discarded, its card expires with `result.superseded = why`, `by` (your
/// message, or the later one) goes to the audit. Returns how many cards closed.
pub fn supersede(
    tx: &Tx,
    ws: &str,
    space: &str,
    what: Answering<'_>,
    why: &str,
    by: &str,
) -> ChannelResult<usize> {
    let (cond, arg) = match what {
        Answering::Before(at) => ("src.created < ?3", json!(at)),
        Answering::Messages(ids) => (
            "src.id IN (SELECT value FROM json_each(?3))",
            json!(serde_json::to_string(ids)?),
        ),
    };
    let arg: rusqlite::types::Value = match arg {
        Value::Number(n) => rusqlite::types::Value::Integer(n.as_i64().unwrap_or(0)),
        Value::String(s) => rusqlite::types::Value::Text(s),
        _ => rusqlite::types::Value::Null,
    };
    // the drafts: in your name, answering someone else's message
    let drafts: Vec<(String, String)> = {
        let mut st = tx.prepare(&format!(
            "{MINE} SELECT o.id, o.status FROM chan_messages o
               JOIN chan_messages src ON src.space_id = o.space_id AND src.direction = 'in'
                    AND src.external_id = o.reply_to
               JOIN chan_messages m ON m.id = src.id
              WHERE o.space_id = ?2 AND o.direction = 'out' AND o.status IN ('draft', 'failed')
                AND o.on_behalf = 'none' AND o.persona_id IS NULL
                AND o.sender_account_id IN (SELECT id FROM my_accounts)
                AND NOT {IS_MINE} AND {cond}"
        ))?;
        st.query_map(params![ws, space, arg], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?
    };
    let mut closed = 0;
    for (id, status) in &drafts {
        tx.execute(
            "UPDATE chan_messages SET status = 'discarded' WHERE id = ? AND status = ?",
            params![id, status],
        )?;
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
    for (d, src) in &asks {
        if settle(tx, d, why)? {
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
        let around = [s("o", 1_030_000, true, "me")];
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
    }
}
