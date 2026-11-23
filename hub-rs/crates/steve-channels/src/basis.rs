//! The basis record of an outbound message: which knowledge
//! the reply was written with, each one's level for this audience and how it ended up in the text,
//! the rewrites, the verdict and both models. Only you see it (the space page, the inbox); people in
//! the chat only see the reply.

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use steve_modules::outbound::{BasisEntry, Span};
use steve_store::{Read, W, reply_basis};

use crate::ChannelResult;

/// `reply_basis.verdict`.
pub const PASS: &str = "pass";
pub const REWRITTEN: &str = "rewritten";
/// The check flagged something (a leak, possibly): the text waits for you with the parts marked.
pub const DRAFTED: &str = "drafted";
/// The check could not finish (its model timed out, failed or answered nothing readable): not a
/// leak, not a pass. `check_model` names every model tried; a span says what happened.
pub const UNCHECKED: &str = "unchecked";
/// A draft on your own account whose check model still runs, after the draft was written
/// (steve-channels `Outbox::send_due`); it becomes one of the others, or [`UNCHECKED`] when the
/// draft is dealt with first.
pub const CHECKING: &str = "checking";

/// One rewrite: `{"from","to","reason"}`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Rewrite {
    pub from: String,
    pub to: String,
    pub reason: String,
}

/// What a basis row is written from.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct NewBasis {
    pub ws: String,
    pub message_id: String,
    pub entries: Vec<BasisEntry>,
    pub rewrites: Vec<Rewrite>,
    /// [`PASS`], [`REWRITTEN`], [`DRAFTED`], [`UNCHECKED`] or [`CHECKING`].
    pub verdict: String,
    pub answer_model: Option<String>,
    pub check_model: Option<String>,
    /// The spans still suspicious (drafted): kept with the entries for the inbox card.
    pub spans: Vec<Span>,
}

/// The `basis` of a `ChanMessage`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BasisOut {
    pub id: String,
    pub message_id: String,
    pub verdict: String,
    pub entries: Value,
    pub rewrites: Value,
    /// The parts that may reveal something (drafts).
    pub spans: Value,
    pub answer_model: Option<String>,
    pub check_model: Option<String>,
    pub created: i64,
}

/// Writes the basis and links it from the message (`basis_id`). The table has no column for the
/// suspicious spans, so they are stored in `entries` as `{"source_type":"span","text","reason"}`
/// and split off again when read ([`of_message`]).
pub fn insert(w: &mut W<'_>, b: &NewBasis, now: i64) -> ChannelResult<String> {
    Ok(reply_basis::insert(
        w,
        &b.ws,
        &b.message_id,
        b.answer_model.as_deref(),
        content(b)?,
        now,
    )?)
}

fn content(b: &NewBasis) -> ChannelResult<reply_basis::Basis<'_>> {
    let mut entries = b
        .entries
        .iter()
        .map(serde_json::to_value)
        .collect::<Result<Vec<_>, _>>()?;
    entries.extend(
        b.spans
            .iter()
            .map(|s| json!({"source_type": "span", "text": s.text, "reason": s.reason})),
    );
    Ok(reply_basis::Basis {
        entries,
        rewrites: b
            .rewrites
            .iter()
            .map(serde_json::to_value)
            .collect::<Result<_, _>>()?,
        verdict: &b.verdict,
        check_model: b.check_model.as_deref(),
    })
}

/// The check that ran after the draft was written settles its basis while it is still checking.
pub fn settle(w: &mut W<'_>, id: &str, b: &NewBasis) -> ChannelResult<bool> {
    Ok(reply_basis::settle_check(w, id, content(b)?)?)
}

/// The draft was dealt with before its check finished. Keep the check's reason with the draft.
pub fn checking_stopped(w: &mut W<'_>, message: &str, why: &str) -> ChannelResult<bool> {
    Ok(reply_basis::stop_check(w, message, why)?)
}

fn out_of(r: reply_basis::BasisRow) -> BasisOut {
    let reply_basis::BasisRow {
        id,
        message_id,
        entries,
        rewrites,
        verdict,
        answer_model,
        check_model,
        created,
    } = r;
    let (spans, entries): (Vec<_>, Vec<_>) = entries
        .into_iter()
        .partition(|e| e["source_type"] == "span");
    let spans: Vec<Value> = spans
        .into_iter()
        .map(|s| json!({"text": s["text"], "reason": s["reason"]}))
        .collect();
    BasisOut {
        id,
        message_id,
        verdict,
        entries: Value::Array(entries),
        rewrites: Value::Array(rewrites),
        spans: Value::Array(spans),
        answer_model,
        check_model,
        created,
    }
}

/// The basis of a message (`GET /chan-messages/:id/basis`), when it has one.
pub fn of_message(r: &mut impl Read, ws: &str, message: &str) -> ChannelResult<Option<BasisOut>> {
    Ok(reply_basis::of_message(r, message, Some(ws))?.map(out_of))
}

/// The basis of a visible outbound message.
pub fn summary_of(r: &mut impl Read, message: &str) -> ChannelResult<Value> {
    let b = reply_basis::of_message(r, message, None)?.map(out_of);
    Ok(match b {
        Some(b) => json!({
            "verdict": b.verdict, "entries": b.entries, "rewrites": b.rewrites, "spans": b.spans,
        }),
        None => Value::Null,
    })
}
