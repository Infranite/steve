//! The basis record of an outbound message: which knowledge
//! the reply was written with, each one's level for this audience and how it ended up in the text,
//! the rewrites, the verdict and both models. Only you see it (the space page, the inbox); people in
//! the chat only see the reply.

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use steve_modules::outbound::{BasisEntry, Span};
use steve_store::{Tx, uid};

use crate::ChannelResult;

/// `reply_basis.verdict`.
pub const PASS: &str = "pass";
pub const REWRITTEN: &str = "rewritten";
pub const DRAFTED: &str = "drafted";

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
    /// [`PASS`], [`REWRITTEN`] or [`DRAFTED`].
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
pub fn insert(tx: &Tx, b: &NewBasis, now: i64) -> ChannelResult<String> {
    let id = uid("bs");
    let mut entries = serde_json::to_value(&b.entries).unwrap_or_else(|_| json!([]));
    if let Value::Array(a) = &mut entries {
        for s in &b.spans {
            a.push(json!({"source_type": "span", "text": s.text, "reason": s.reason}));
        }
    }
    tx.execute(
        "INSERT INTO reply_basis (id, workspace_id, message_id, entries, rewrites, verdict, answer_model, check_model, created)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            id,
            b.ws,
            b.message_id,
            entries.to_string(),
            serde_json::to_string(&b.rewrites).unwrap_or_else(|_| "[]".into()),
            b.verdict,
            b.answer_model,
            b.check_model,
            now
        ],
    )?;
    tx.execute(
        "UPDATE chan_messages SET basis_id = ? WHERE id = ?",
        params![id, b.message_id],
    )?;
    Ok(id)
}

/// A `reply_basis` row as stored.
struct Raw {
    id: String,
    message_id: String,
    entries: String,
    rewrites: String,
    verdict: String,
    answer_model: Option<String>,
    check_model: Option<String>,
    created: i64,
}

fn raw(r: &rusqlite::Row) -> rusqlite::Result<Raw> {
    Ok(Raw {
        id: r.get(0)?,
        message_id: r.get(1)?,
        entries: r.get(2)?,
        rewrites: r.get(3)?,
        verdict: r.get(4)?,
        answer_model: r.get(5)?,
        check_model: r.get(6)?,
        created: r.get(7)?,
    })
}

fn out_of(r: Raw) -> BasisOut {
    let Raw {
        id,
        message_id,
        entries,
        rewrites,
        verdict,
        answer_model,
        check_model,
        created,
    } = r;
    let all: Vec<Value> = serde_json::from_str(&entries).unwrap_or_default();
    let (spans, entries): (Vec<Value>, Vec<Value>) =
        all.into_iter().partition(|e| e["source_type"] == "span");
    let spans: Vec<Value> = spans
        .into_iter()
        .map(|s| json!({"text": s["text"], "reason": s["reason"]}))
        .collect();
    BasisOut {
        id,
        message_id,
        verdict,
        entries: Value::Array(entries),
        rewrites: serde_json::from_str(&rewrites).unwrap_or_else(|_| json!([])),
        spans: Value::Array(spans),
        answer_model,
        check_model,
        created,
    }
}

/// The basis of a message (`GET /chan-messages/:id/basis`), when it has one.
pub fn of_message(c: &Connection, ws: &str, message_id: &str) -> ChannelResult<Option<BasisOut>> {
    Ok(c.query_row(
        "SELECT b.id, b.message_id, b.entries, b.rewrites, b.verdict, b.answer_model, b.check_model, b.created
           FROM chan_messages m JOIN reply_basis b ON b.id = m.basis_id
          WHERE m.id = ? AND m.workspace_id = ?",
        params![message_id, ws],
        raw,
    )
    .optional()?
    .map(out_of))
}

/// The `basis` field of a `ChanMessage`: `{verdict, entries, rewrites, spans}` or `null`.
/// The caller has checked the message is the caller's to see.
pub fn summary_of(c: &Connection, message_id: &str) -> ChannelResult<Value> {
    let b = c
        .query_row(
            "SELECT b.id, b.message_id, b.entries, b.rewrites, b.verdict, b.answer_model, b.check_model, b.created
               FROM chan_messages m JOIN reply_basis b ON b.id = m.basis_id WHERE m.id = ?",
            [message_id],
            raw,
        )
        .optional()?
        .map(out_of);
    Ok(match b {
        Some(b) => json!({
            "verdict": b.verdict, "entries": b.entries, "rewrites": b.rewrites, "spans": b.spans,
        }),
        None => Value::Null,
    })
}
