//! A channel conversation's full record, for 对话 (issue #243).
//!
//! The conversation of a space ([`crate::conversations`]) holds only the turns Steve was handed. Its
//! record is everything the space's chat had that the viewer may see: every channel message, received
//! or sent, and for each received one what Steve did with it ([`Handling`]): answered, waiting for
//! you (a draft or a card), handed off to an item, still working, failed (with the reason), skipped
//! or only recorded (with the label and reason its event got), or decided by you. Each sent one
//! carries its send state and, when it failed, the provider's last error. A message that has a turn in
//! the conversation names it (`turn`), so the page shows it once, with the turn. Your `/bot` command
//! where no identity of yours may answer goes to your web conversation instead
//! ([`crate::conversations::MAIN_CONV`]): what Steve said to that turn there is its answer, and the
//! handling names the turn (`main_turn`).
//!
//! The channel switcher ([`channels`]) lists the channel accounts the viewer sees with how many of
//! their conversations have new messages; “new” is counted from the viewer's read marker
//! (`conversation_preferences.read_up_to`, set by [`mark_read`] when a conversation is shown) and
//! never further back than [`UNREAD_HORIZON_MS`].
//!
//! Visibility is the conversation list's: the space must be one the viewer sees
//! ([`crate::spaces::own`]). Everything here is read-only but the viewer's own read marker.

use std::collections::HashMap;

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use steve_store::Store;
use steve_store::base::Viewer;

use crate::conversations::{MAIN_CONV, NO_REPLY, conv_space, out_state, shown_space};
use crate::{ChannelResult, Channels, messages, spaces};

/// Messages of a record page when the request names no limit.
pub const RECORD_PAGE: usize = 100;
/// The most messages one record request may ask for.
pub const RECORD_MAX: usize = 500;
/// How far back a message still counts as new: a space never opened in 对话 does not count its
/// whole history.
pub const UNREAD_HORIZON_MS: i64 = 7 * 24 * 3_600_000;
/// New messages are counted up to this many per conversation (the page shows “99+” past it).
pub const UNREAD_MAX: i64 = 100;
/// Event labels that only say where a message came from (your own, Steve's own read back, already
/// stored through another identity): nothing was decided about it, so the record shows no handling.
const BOOKKEEPING: [&str; 3] = [
    crate::inbound::LABEL_OWN,
    crate::inbound::LABEL_ECHO,
    crate::inbound::LABEL_ELSEWHERE,
];
/// Characters of a failed turn's error kept as the reason.
const ERROR_CHARS: usize = 300;

/// The newest a message may be and still be new for a viewer whose marker is `read_up_to`.
fn new_since(read_up_to: Option<i64>, now: i64) -> i64 {
    read_up_to
        .unwrap_or(i64::MIN)
        .max(now.saturating_sub(UNREAD_HORIZON_MS))
}

/// New messages of space `space` for user `user` (marker `read_up_to`) at `now`, up to
/// [`UNREAD_MAX`]: received ones the user may see, live (not history read back), newer than the
/// marker, not sent by the user themselves.
pub fn unread(
    c: &Connection,
    space: &str,
    user: &str,
    read_up_to: Option<i64>,
    now: i64,
) -> ChannelResult<i64> {
    count_new(c, space, user, new_since(read_up_to, now), UNREAD_MAX)
}

fn count_new(c: &Connection, space: &str, user: &str, since: i64, cap: i64) -> ChannelResult<i64> {
    let mut st = c.prepare_cached(&format!(
        "SELECT count(*) FROM (SELECT 1 FROM chan_messages m
           WHERE m.space_id = ?1 AND m.direction = 'in' AND m.history IS NULL AND m.created > ?3
             AND NOT EXISTS (SELECT 1 FROM parties pp WHERE pp.id = m.sender_party_id AND pp.user_id = ?2)
           LIMIT ?4)"
    ))?;
    Ok(st.query_row(params![space, user, since, cap], |r| r.get(0))?)
}

/// What Steve did with a received message, as the record shows it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Handling {
    /// Its answer went out.
    Answered,
    /// A reply was drafted, but has not been sent.
    Drafted,
    /// The channel or its permission boundary refused the send.
    Blocked,
    /// The provider has not confirmed whether the attempt went out.
    Unknown,
    /// The reply was recalled after sending.
    Recalled,
    /// An answer exists only in Steve, without a confirmed channel send.
    Local,
    /// A draft or a card waits for you.
    Waiting,
    /// Handed off to an item.
    Item,
    /// Steve is on it (the “处理中” reaction, or its answer is being sent).
    Working,
    /// Answering it failed.
    Failed,
    /// Steve looked at it and did not answer (or you discarded the draft).
    Skipped,
    /// Its card was answered by you.
    Decided,
    /// Only recorded, for the reason its event says (Steve's own message, quiet space, history…).
    Recorded,
    /// Nothing is known beyond the message.
    None,
}

/// What is known about a received message, from its event, reaction, decision, answer and turn.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Facts {
    /// `ext_events.outcome` (`pending | need | item | … | recorded | failed`).
    pub outcome: Option<String>,
    /// `ext_events.outcome_label`.
    pub label: String,
    /// `chan_reactions.state` (`ack | done | failed | cleared`).
    pub reaction: Option<String>,
    /// The item it was handed off to.
    pub item: Option<String>,
    /// One of its decisions (or its answer's) is open.
    pub decision_open: bool,
    /// It has a decision, none of them open.
    pub decision_closed: bool,
    /// The send state of its answer ([`out_state`]).
    pub answer: Option<String>,
    /// It was a turn of the space's conversation.
    pub turn: bool,
    /// That turn's answer was an error.
    pub turn_failed: bool,
    /// Steve has not said anything to that turn yet.
    pub turn_open: bool,
    /// The turn is in your web conversation, and Steve said something to it there: that is its
    /// answer (nothing goes back to the space, so its reaction says nothing about it).
    pub turn_answered_in_main: bool,
}

/// A retained outbound result decides delivery. Without one, processing failures, handoffs and
/// open cards take precedence; an explicit skip then wins over stale processing markers. A reply
/// available only in Steve stays separate from a confirmed channel send.
pub fn handling(f: &Facts) -> Handling {
    // A persisted outbound result wins over events and reaction lifecycle state. A failed
    // reaction cannot invalidate a confirmed send, nor can “done” send a draft. Generation
    // errors remain visible even when the fixed notice explaining the failure went out.
    match f.answer.as_deref() {
        // A fixed failure notice can be sent successfully while the answer itself failed.
        Some("sent") if f.turn_failed => return Handling::Failed,
        Some("sent") => return Handling::Answered,
        Some("draft") => return Handling::Drafted,
        Some("blocked") => return Handling::Blocked,
        Some("failed") => return Handling::Failed,
        Some("unknown") => return Handling::Unknown,
        Some("recalled") => return Handling::Recalled,
        Some("discarded" | "skipped") => return Handling::Skipped,
        Some("pending") => return Handling::Working,
        _ => {}
    }
    if f.turn_failed || f.outcome.as_deref() == Some("failed") {
        return Handling::Failed;
    }
    if f.item.is_some() {
        return Handling::Item;
    }
    if f.decision_open {
        return Handling::Waiting;
    }
    // A completed skip can leave an old acknowledgement or an unanswered turn behind. Keep
    // the recorded reason ahead of those stale markers; persisted outbound results above still
    // decide sent/draft/blocked/failed/unknown outcomes.
    if (f.outcome.as_deref() == Some("recorded") && f.label.starts_with(NO_REPLY))
        || f.reaction.as_deref() == Some("cleared")
        || f.outcome.as_deref() == Some("ignored")
    {
        return Handling::Skipped;
    }
    if f.turn_answered_in_main {
        return Handling::Local;
    }
    if f.reaction.as_deref() == Some("ack") || f.turn_open {
        return Handling::Working;
    }
    if f.turn {
        return Handling::Skipped;
    }
    if f.decision_closed {
        return Handling::Decided;
    }
    if !f.label.is_empty() && !BOOKKEEPING.contains(&f.label.as_str()) {
        return Handling::Recorded;
    }
    Handling::None
}

/// `GET /conversations/:space/record`: before the message `(before, before_id)`; `limit` messages
/// (default [`RECORD_PAGE`], at most [`RECORD_MAX`]).
#[derive(Debug, Default, Deserialize)]
pub struct RecordQuery {
    pub before: Option<i64>,
    pub before_id: Option<String>,
    pub limit: Option<usize>,
}

/// The conversation's turns and answers, by the channel messages they stand for; and the turns of the
/// space's messages that went to your web conversation, with what Steve said to them there.
#[derive(Default)]
struct Turns {
    /// received channel message → the turn posted for it
    turn_of: HashMap<String, String>,
    /// received channel message → its turn in your web conversation
    main_turn_of: HashMap<String, String>,
    /// sent channel message → Steve's conversation message it said
    said_by: HashMap<String, String>,
    /// turn → the actual outbound answer linked by chan_out.message
    answer_of: HashMap<String, String>,
    /// turn → the error its answer was
    failed: HashMap<String, String>,
    /// turns Steve said something to
    answered: std::collections::HashSet<String>,
    /// turn → the item its answer handed it to
    item_of: HashMap<String, String>,
}

/// One message of a conversation as [`turns`] reads it.
struct Row {
    id: String,
    author: String,
    text: String,
    item: Option<String>,
    /// `meta.chan.message`: the channel message a turn stands for
    chan: Option<String>,
    /// `meta.chan_out.message`: the channel message an answer was sent as
    out: Option<String>,
    /// `meta.origin`: the turn an answer is to
    origin: Option<String>,
    /// `meta.error`
    error: bool,
}

impl Turns {
    /// Takes in the rows of one conversation; `main`: they are your web conversation's.
    fn add(&mut self, rows: Vec<Row>, main: bool) {
        for r in rows {
            if r.author == "me"
                && let Some(cm) = r.chan
            {
                if main {
                    self.main_turn_of.insert(cm, r.id.clone());
                } else {
                    self.turn_of.insert(cm, r.id.clone());
                }
            }
            if r.author == "steve" {
                if let Some(o) = &r.origin {
                    self.answered.insert(o.clone());
                    if let Some(i) = &r.item {
                        self.item_of.entry(o.clone()).or_insert_with(|| i.clone());
                    }
                    if !r.error {
                        self.failed.remove(o);
                    }
                }
                if let Some(o) = r.out {
                    if let Some(origin) = &r.origin {
                        self.answer_of.insert(origin.clone(), o.clone());
                    }
                    self.said_by.insert(o, r.id.clone());
                }
                if r.error
                    && let Some(o) = r.origin
                {
                    self.failed
                        .insert(o, r.text.trim().chars().take(ERROR_CHARS).collect());
                }
            }
        }
    }
}

const ROW_COLS: &str = "id, author, text, item_id, json_extract(meta, '$.chan.message'),
    json_extract(meta, '$.chan_out.message'), json_extract(meta, '$.origin'),
    coalesce(json_extract(meta, '$.error'), 0)";

fn row(r: &rusqlite::Row) -> rusqlite::Result<Row> {
    Ok(Row {
        id: r.get(0)?,
        author: r.get(1)?,
        text: r.get(2)?,
        item: r.get(3)?,
        chan: r.get(4)?,
        out: r.get(5)?,
        origin: r.get(6)?,
        error: r.get::<_, i64>(7)? != 0,
    })
}

/// The turns of space `space`: its conversation's, and those of its messages in your web conversation
/// (only those turns and what Steve said to them are read from there).
fn turns(c: &Connection, ws: &str, space: &str) -> ChannelResult<Turns> {
    let mut t = Turns::default();
    let mut st = c.prepare_cached(&format!(
        "SELECT {ROW_COLS} FROM messages WHERE workspace_id = ? AND space = ? ORDER BY created, rowid"
    ))?;
    let rows = st
        .query_map(params![ws, conv_space(space)], row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    t.add(rows, false);
    let mut st = c.prepare_cached(&format!(
        "SELECT {ROW_COLS} FROM messages WHERE workspace_id = ?1 AND space = ?3 AND (
            (author = 'me' AND json_extract(meta, '$.chan.space') = ?2)
            OR (author = 'steve' AND json_extract(meta, '$.origin') IN (
                SELECT id FROM messages WHERE workspace_id = ?1 AND space = ?3 AND author = 'me'
                   AND json_extract(meta, '$.chan.space') = ?2))) ORDER BY created, rowid"
    ))?;
    let rows = st
        .query_map(params![ws, space, MAIN_CONV], row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    t.add(rows, true);
    Ok(t)
}

/// Is decision `id` open (`Some(true)`), resolved (`Some(false)`) or unknown?
fn decision_open(c: &Connection, ws: &str, id: &str) -> ChannelResult<Option<bool>> {
    Ok(c.query_row(
        "SELECT status = 'open' FROM decisions WHERE id = ? AND workspace_id = ?",
        params![id, ws],
        |r| r.get(0),
    )
    .optional()?)
}

/// The last error the provider gave for sending message `id`.
fn send_error(c: &Connection, id: &str) -> ChannelResult<Option<String>> {
    Ok(c.query_row(
        "SELECT nullif(last_error, '') FROM chan_send_state WHERE message_id = ?",
        [id],
        |r| r.get(0),
    )
    .optional()?
    .flatten())
}

/// Why sent message `id` did not go out: the provider's last error while it is unsettled, else
/// the question of the card it failed with (what 待处理 shows).
pub(crate) fn out_error(c: &Connection, ws: &str, id: &str) -> ChannelResult<Option<String>> {
    if let Some(e) = send_error(c, id)? {
        return Ok(Some(e));
    }
    Ok(c.query_row(
        "SELECT d.question FROM chan_messages m JOIN decisions d ON d.id = m.decision_id AND d.kind = ?3
          WHERE m.id = ?1 AND m.workspace_id = ?2",
        params![id, ws, steve_store::base::DecisionKind::Fail.code()],
        |r| r.get(0),
    )
    .optional()?)
}

/// The open card a failed send waits on (retry or discard).
fn open_fail(c: &Connection, ws: &str, id: &str) -> ChannelResult<Option<String>> {
    Ok(c.query_row(
        "SELECT d.id FROM chan_messages m JOIN decisions d ON d.id = m.decision_id AND d.status = 'open'
          WHERE m.id = ?1 AND m.workspace_id = ?2",
        params![id, ws],
        |r| r.get(0),
    )
    .optional()?)
}

/// The event of a received message: `(outcome, label, reason, item, decision)`.
type Event = (String, String, String, Option<String>, Option<String>);

fn event_of(c: &Connection, ws: &str, id: &str) -> ChannelResult<Option<Event>> {
    Ok(c.query_row(
        "SELECT outcome, outcome_label, reason, work_item_id, decision_id FROM ext_events
          WHERE id = ? AND workspace_id = ?",
        params![id, ws],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
    )
    .optional()?)
}

/// `handling` of received message `m`: `{state, label, reason, error, item, decision, main_turn}`;
/// `decision` is an open one waiting for you, `main_turn` its turn in your web conversation.
fn received(
    r: &mut impl steve_store::Read,
    c: &Connection,
    ws: &str,
    m: &messages::ChanMessage,
    t: &Turns,
) -> ChannelResult<Value> {
    let ev = match &m.event_id {
        Some(e) => event_of(c, ws, e)?,
        None => None,
    };
    let reaction = steve_store::channel_message_repo::reactions::get(r, &m.id)?;
    let main_turn = t.main_turn_of.get(&m.id);
    let turn = t.turn_of.get(&m.id).or(main_turn);
    // The turn/outbound relation is persisted even when reactions fail or a draft is held. A
    // reaction's answer id is only a legacy fallback; its lifecycle never establishes delivery.
    let answer_id = turn
        .and_then(|id| t.answer_of.get(id))
        .cloned()
        .or_else(|| reaction.as_ref().and_then(|r| r.answer_id.clone()));
    let answer = match &answer_id {
        Some(a) => out_state(c, ws, a)?,
        None => None,
    };
    let mut decisions: Vec<String> = Vec::new();
    decisions.extend(m.decision_id.clone());
    decisions.extend(ev.as_ref().and_then(|e| e.4.clone()));
    let (mut open, mut closed) = (None, false);
    for d in &decisions {
        match decision_open(c, ws, d)? {
            Some(true) => open = open.or(Some(d.clone())),
            Some(false) => closed = true,
            None => {}
        }
    }
    if let Some((_, Some(d))) = &answer {
        open = open.or(Some(d.clone()));
    } else if let Some(id) = &answer_id {
        open = open.or(open_fail(c, ws, id)?);
    }
    let turn_error = turn.and_then(|x| t.failed.get(x));
    let item = m
        .work_item_id
        .clone()
        .or_else(|| ev.as_ref().and_then(|e| e.3.clone()))
        .or_else(|| turn.and_then(|x| t.item_of.get(x)).cloned());
    let (outcome, label, reason) = match &ev {
        Some((o, l, r, _, _)) => (Some(o.clone()), l.clone(), r.clone()),
        None => (None, String::new(), String::new()),
    };
    let facts = Facts {
        outcome,
        label: label.clone(),
        reaction: reaction.as_ref().map(|r| r.state.clone()),
        item: item.clone(),
        decision_open: open.is_some(),
        decision_closed: closed && open.is_none(),
        answer: answer.as_ref().map(|(s, _)| s.clone()),
        turn: turn.is_some(),
        turn_failed: turn_error.is_some(),
        turn_open: turn.is_some_and(|x| !t.answered.contains(x)),
        turn_answered_in_main: main_turn.is_some_and(|x| t.answered.contains(x)),
    };
    let state = handling(&facts);
    let error = if matches!(
        state,
        Handling::Failed | Handling::Blocked | Handling::Unknown
    ) {
        match &answer_id {
            Some(a) => out_error(c, ws, a)?.or_else(|| turn_error.cloned()),
            None => turn_error.cloned(),
        }
    } else {
        None
    };
    Ok(json!({
        "state": state,
        "label": label,
        "reason": reason,
        "error": error,
        "item": item,
        "decision": open,
        "main_turn": main_turn,
        "out_message_id": answer_id,
        "out_status": answer.as_ref().map(|(status, _)| status),
    }))
}

/// `GET /conversations/:space/record`: a page of the space's record, oldest first, and the cursor of
/// the page before it (`next`, `null` at the start).
pub fn record(
    store: &Store,
    ws: &str,
    viewer: &Viewer,
    conv: &str,
    q: &RecordQuery,
) -> ChannelResult<Value> {
    store.read(|r| {
        store.reader(|c| {
            let space = shown_space(c, ws, viewer, conv)?;
            let limit = q.limit.unwrap_or(RECORD_PAGE);
            if !(1..=RECORD_MAX).contains(&limit) {
                return Err(steve_modules::ApiError::invalid("limit"));
            }
            let cursor = match (q.before, q.before_id.as_deref()) {
                (Some(at), Some(id)) => Some((at, id)),
                (None, None) => None,
                _ => return Err(steve_modules::ApiError::invalid("before")),
            };
            let mut page = messages::page_before(store, space, &viewer.user, cursor, limit + 1)?;
            let has_more = page.len() > limit;
            page.truncate(limit);
            page.reverse();
            let t = turns(c, ws, space)?;
            let mut out = Vec::with_capacity(page.len());
            for m in &page {
                let mut v = serde_json::to_value(m).map_err(steve_modules::ApiError::from)?;
                if m.direction == "in" {
                    v["turn"] = json!(t.turn_of.get(&m.id));
                    v["handling"] = received(r, c, ws, m, &t)?;
                } else {
                    v["turn"] = json!(t.said_by.get(&m.id));
                    let (status, mut decision) =
                        out_state(c, ws, &m.id)?.unwrap_or_else(|| (m.status.clone(), None));
                    let error = if matches!(status.as_str(), "failed" | "blocked" | "unknown") {
                        if decision.is_none() {
                            decision = open_fail(c, ws, &m.id)?;
                        }
                        out_error(c, ws, &m.id)?
                    } else {
                        None
                    };
                    v["out"] = json!({"status": status, "decision": decision, "error": error});
                }
                out.push(v);
            }
            let next = if has_more {
                page.first()
                    .map(|m| json!({"before": m.created, "before_id": m.id}))
            } else {
                None
            };
            Ok(json!({"records": out, "has_more": has_more, "next": next}))
        })
    })
}

/// `POST /conversations/:space/read`: the viewer was shown the conversation up to `at` (never
/// later than `now`); the marker only moves forward.
pub fn mark_read(
    c: &Connection,
    ws: &str,
    viewer: &Viewer,
    conv: &str,
    at: i64,
    now: i64,
) -> ChannelResult<()> {
    let id = shown_space(c, ws, viewer, conv)?;
    if at < 0 {
        return Err(steve_modules::ApiError::invalid("at"));
    }
    c.execute(
        "INSERT INTO conversation_preferences (workspace_id, user_id, space_id, read_up_to) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(workspace_id, user_id, space_id)
         DO UPDATE SET read_up_to = max(coalesce(read_up_to, 0), excluded.read_up_to)",
        params![ws, viewer.user, id, at.min(now)],
    )?;
    Ok(())
}

/// One entry of the channel switcher.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChannelEntry {
    pub connection: String,
    pub name: String,
    pub provider: String,
    pub mode: spaces::Mode,
    /// `connections.status`
    pub status: String,
    /// Its conversations not closed by the viewer (spaces with something to show).
    pub conversations: i64,
    /// How many of them have new messages.
    pub unread: i64,
}

/// `GET /conversations/channels`: the channel accounts the viewer sees, oldest first.
pub fn channels(
    ch: &Channels,
    store: &steve_store::Store,
    viewer: &Viewer,
    now: i64,
) -> ChannelResult<Vec<ChannelEntry>> {
    store.read(|r| store.reader(|c| {
        crate::conversations::member(c, &viewer.ws, viewer)?;
    let mut st = c.prepare_cached(&format!(
        "SELECT s.id, p.read_up_to FROM spaces s
           LEFT JOIN conversation_preferences p ON p.workspace_id = s.workspace_id AND p.user_id = ?2 AND p.space_id = s.id
          WHERE s.workspace_id = ?1 AND {visibility} AND coalesce(p.closed, 0) = 0
            AND (s.connection_id = ?3 OR EXISTS (SELECT 1 FROM space_identities li
              WHERE li.space_id = s.id AND li.connection_id = ?3 AND li.left_at IS NULL))
            AND (EXISTS (SELECT 1 FROM messages x WHERE x.workspace_id = ?1 AND x.space = 'chan:' || s.id)
              OR EXISTS (SELECT 1 FROM chan_messages m WHERE m.space_id = s.id))",
        visibility = spaces::visible_sql("?2"),
    ))?;
    let mut out = Vec::new();
    for a in crate::channel_accounts::list(ch, r, viewer)? {
        let rows = st
            .query_map(params![viewer.ws, viewer.user, a.connection_id], |r| {
                Ok((r.get::<_, String>(0)?, r.get::<_, Option<i64>>(1)?))
            })?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        let mut unread = 0;
        for (space, read) in &rows {
            if count_new(c, space, &viewer.user, new_since(*read, now), 1)? > 0 {
                unread += 1;
            }
        }
        out.push(ChannelEntry {
            connection: a.connection_id,
            name: a.name,
            provider: a.provider,
            mode: a.mode,
            status: a.status,
            conversations: rows.len() as i64,
            unread,
        });
    }
        Ok(out)
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn f() -> Facts {
        Facts::default()
    }

    #[test]
    fn only_the_persisted_outbound_result_confirms_channel_delivery() {
        for (status, want) in [
            ("sent", Handling::Answered),
            ("draft", Handling::Drafted),
            ("blocked", Handling::Blocked),
            ("failed", Handling::Failed),
            ("pending", Handling::Working),
            ("unknown", Handling::Unknown),
            ("recalled", Handling::Recalled),
            ("discarded", Handling::Skipped),
        ] {
            for reaction in ["done", "ack", "failed", "cleared"] {
                assert_eq!(
                    handling(&Facts {
                        answer: Some(status.into()),
                        reaction: Some(reaction.into()),
                        decision_open: true,
                        turn: true,
                        turn_failed: false,
                        outcome: Some("failed".into()),
                        ..f()
                    }),
                    want
                );
            }
        }
        assert_eq!(
            handling(&Facts {
                answer: Some("sent".into()),
                turn_failed: true,
                ..f()
            }),
            Handling::Failed
        );
        assert_eq!(
            handling(&Facts {
                reaction: Some("done".into()),
                ..f()
            }),
            Handling::None
        );
        assert_eq!(
            handling(&Facts {
                reaction: Some("failed".into()),
                ..f()
            }),
            Handling::None
        );
        assert_eq!(
            handling(&Facts {
                turn: true,
                turn_answered_in_main: true,
                ..f()
            }),
            Handling::Local
        );
        assert_eq!(
            handling(&Facts {
                decision_open: true,
                ..f()
            }),
            Handling::Waiting
        );
        assert_eq!(
            handling(&Facts {
                turn_failed: true,
                ..f()
            }),
            Handling::Failed
        );
        assert_eq!(
            handling(&Facts {
                item: Some("ST-2".into()),
                ..f()
            }),
            Handling::Item
        );
        assert_eq!(
            handling(&Facts {
                turn_open: true,
                ..f()
            }),
            Handling::Working
        );
        assert_eq!(handling(&Facts { turn: true, ..f() }), Handling::Skipped);
        assert_eq!(
            handling(&Facts {
                decision_closed: true,
                ..f()
            }),
            Handling::Decided
        );
        assert_eq!(
            handling(&Facts {
                label: crate::inbound::LABEL_QUIET.into(),
                ..f()
            }),
            Handling::Recorded
        );
        assert_eq!(
            handling(&Facts {
                label: crate::inbound::LABEL_OWN.into(),
                ..f()
            }),
            Handling::None
        );
    }

    #[test]
    fn a_finished_skip_wins_over_stale_processing_markers() {
        for finished in [
            Facts {
                outcome: Some("recorded".into()),
                label: format!("{}没有明确的请求", crate::conversations::NO_REPLY),
                ..f()
            },
            Facts {
                outcome: Some("ignored".into()),
                ..f()
            },
            Facts {
                reaction: Some("cleared".into()),
                ..f()
            },
            Facts {
                answer: Some(crate::outbound::DISCARDED.into()),
                ..f()
            },
        ] {
            let x = Facts {
                reaction: finished.reaction.or(Some("ack".into())),
                turn: true,
                turn_open: true,
                ..finished
            };
            assert_eq!(handling(&x), Handling::Skipped, "{x:?}");
        }
    }

    #[test]
    fn an_active_send_or_open_decision_keeps_its_priority() {
        for answer in ["pending", "unknown"] {
            let x = Facts {
                answer: Some(answer.into()),
                reaction: Some("cleared".into()),
                ..f()
            };
            let expected = if answer == "unknown" {
                Handling::Unknown
            } else {
                Handling::Working
            };
            assert_eq!(handling(&x), expected, "{x:?}");
        }
        let x = Facts {
            decision_open: true,
            answer: Some("draft".into()),
            reaction: Some("cleared".into()),
            turn: true,
            turn_open: true,
            ..f()
        };
        assert_eq!(handling(&x), Handling::Drafted);
        assert_eq!(
            handling(&Facts {
                decision_open: true,
                reaction: Some("cleared".into()),
                ..f()
            }),
            Handling::Waiting
        );
    }

    #[test]
    fn a_finished_skip_is_not_a_local_or_channel_reply() {
        let facts = Facts {
            outcome: Some("recorded".into()),
            label: format!("{NO_REPLY}nothing to add"),
            reaction: Some("ack".into()),
            turn: true,
            turn_open: true,
            turn_answered_in_main: true,
            ..f()
        };
        assert_eq!(handling(&facts), Handling::Skipped);
    }

    #[test]
    fn new_counts_from_the_marker_but_never_beyond_the_horizon() {
        let now = 10 * UNREAD_HORIZON_MS;
        assert_eq!(new_since(None, now), now - UNREAD_HORIZON_MS);
        assert_eq!(new_since(Some(0), now), now - UNREAD_HORIZON_MS);
        assert_eq!(new_since(Some(now - 5), now), now - 5);
    }
}
