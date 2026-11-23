//! Create outbound messages and copies that need a separate delivery identity.

use sea_query::{Expr, ExprTrait, Order, Query};
use serde_json::Value;

use crate::contract::Q;
use crate::{Read, Result, StoreError, W};

use steve_store_sqlite::digest::{self, Source};

pub struct Approval {
    pub space: String,
    pub status: String,
    pub decision: Option<String>,
}

pub fn approval(r: &mut impl Read, ws: &str, id: &str) -> Result<Option<Approval>> {
    r.one(
        Query::select()
            .columns(["space_id", "status", "decision_id"])
            .from("chan_messages")
            .and_where(Expr::col("id").eq(id))
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("direction").eq("out")),
        |row| {
            Ok(Approval {
                space: row.get(0)?,
                status: row.get(1)?,
                decision: row.get(2)?,
            })
        },
    )
}

pub struct NewMessage<'a> {
    pub id: &'a str,
    pub ws: &'a str,
    pub space: &'a str,
    pub thread: Option<&'a str>,
    pub reply_to: Option<&'a str>,
    pub account: Option<&'a str>,
    pub persona: Option<&'a str>,
    pub on_behalf: &'a str,
    pub text: Option<&'a str>,
    pub status: &'a str,
    pub created: i64,
}

pub fn insert(w: &mut W<'_>, n: &NewMessage<'_>) -> Result<()> {
    let mask = match n.account {
        Some(account) => super::super::channel_spaces::account_identity_mask(w, n.space, account)?,
        None => 0,
    };
    insert_with_provenance(w, n, mask, None, None, None)
}

fn insert_with_provenance(
    w: &mut W<'_>,
    n: &NewMessage<'_>,
    seen_by: i64,
    basis_id: Option<&str>,
    composed: Option<&str>,
    answer_to: Option<&str>,
) -> Result<()> {
    w.exec(
        Query::insert()
            .into_table("chan_messages")
            .columns([
                "id",
                "workspace_id",
                "space_id",
                "thread_key",
                "reply_to",
                "direction",
                "sender_account_id",
                "persona_id",
                "on_behalf",
                "text",
                "status",
                "created",
                "seen_by",
                "basis_id",
                "composed",
                "answer_to",
            ])
            .values_panic([
                n.id.into(),
                n.ws.into(),
                n.space.into(),
                n.thread.into(),
                n.reply_to.into(),
                "out".into(),
                n.account.into(),
                n.persona.into(),
                n.on_behalf.into(),
                n.text.into(),
                n.status.into(),
                n.created.into(),
                seen_by.into(),
                basis_id.into(),
                composed.into(),
                answer_to.into(),
            ]),
    )?;
    digest::sync_message(w.conn, Source::ChanMessage, n.id, None)?;
    Ok(())
}

struct RecoverySource {
    ws: String,
    space: String,
    thread: Option<String>,
    reply_to: Option<String>,
    account: Option<String>,
    persona: Option<String>,
    on_behalf: String,
    text: Option<String>,
    seen_by: i64,
    basis_id: Option<String>,
    composed: Option<String>,
    answer_to: Option<String>,
}

/// A rewritten payload must have a fresh id. Copy its basis and composer provenance with it.
pub fn copy_for_recovery(w: &mut W<'_>, id: &str, next: &str, now: i64) -> Result<()> {
    let source = w
        .one(
            Query::select()
                .columns([
                    "workspace_id",
                    "space_id",
                    "thread_key",
                    "reply_to",
                    "sender_account_id",
                    "persona_id",
                    "on_behalf",
                    "text",
                    "seen_by",
                    "basis_id",
                    "composed",
                    "answer_to",
                ])
                .from("chan_messages")
                .and_where(Expr::col("id").eq(id))
                .and_where(Expr::col("direction").eq("out")),
            |r| {
                Ok(RecoverySource {
                    ws: r.get(0)?,
                    space: r.get(1)?,
                    thread: r.get(2)?,
                    reply_to: r.get(3)?,
                    account: r.get(4)?,
                    persona: r.get(5)?,
                    on_behalf: r.get(6)?,
                    text: r.get(7)?,
                    seen_by: r.get(8)?,
                    basis_id: r.get(9)?,
                    composed: r.get(10)?,
                    answer_to: r.get(11)?,
                })
            },
        )?
        .ok_or_else(|| StoreError::Rejected("recovery draft is missing".into()))?;
    insert_with_provenance(
        w,
        &NewMessage {
            id: next,
            ws: &source.ws,
            space: &source.space,
            thread: source.thread.as_deref(),
            reply_to: source.reply_to.as_deref(),
            account: source.account.as_deref(),
            persona: source.persona.as_deref(),
            on_behalf: &source.on_behalf,
            text: source.text.as_deref(),
            status: "draft",
            created: now,
        },
        source.seen_by,
        source.basis_id.as_deref(),
        source.composed.as_deref(),
        source.answer_to.as_deref(),
    )
}

/// One outbound `chan_messages` row as the sender needs it.
#[derive(Debug, Clone, PartialEq)]
pub struct OutRow {
    pub id: String,
    pub ws: String,
    pub space_id: String,
    pub text: String,
    pub reply_to: Option<String>,
    pub thread_key: Option<String>,
    pub sender_account_id: Option<String>,
    pub persona_id: Option<String>,
    pub on_behalf: String,
    pub status: String,
    pub decision_id: Option<String>,
    pub sent: Option<i64>,
}

pub fn send_row(r: &mut impl Read, id: &str) -> Result<Option<OutRow>> {
    r.one(
        Query::select()
            .columns([
                "id",
                "workspace_id",
                "space_id",
                "text",
                "reply_to",
                "thread_key",
                "sender_account_id",
                "persona_id",
                "on_behalf",
                "status",
                "decision_id",
                "sent",
            ])
            .from("chan_messages")
            .and_where(Expr::col("id").eq(id))
            .and_where(Expr::col("direction").eq("out")),
        |row| {
            Ok(OutRow {
                id: row.get(0)?,
                ws: row.get(1)?,
                space_id: row.get(2)?,
                text: row.get::<Option<String>>(3)?.unwrap_or_default(),
                reply_to: row.get(4)?,
                thread_key: row.get(5)?,
                sender_account_id: row.get(6)?,
                persona_id: row.get(7)?,
                on_behalf: row.get(8)?,
                status: row.get(9)?,
                decision_id: row.get(10)?,
                sent: row.get(11)?,
            })
        },
    )
}

pub fn link_decision(w: &mut W<'_>, id: &str, decision: &str) -> Result<()> {
    w.exec(
        Query::update()
            .table("chan_messages")
            .value("decision_id", decision)
            .and_where(Expr::col("id").eq(id)),
    )?;
    Ok(())
}

pub fn latest_answer(r: &mut impl Read, ws: &str, message: &str) -> Result<Option<String>> {
    r.one(
        Query::select()
            .column("id")
            .from("chan_messages")
            .and_where(Expr::col("answer_to").eq(message))
            .and_where(Expr::col("workspace_id").eq(ws))
            .order_by("created", Order::Desc)
            .order_by("rowid", Order::Desc)
            .limit(1),
        |row| row.get(0),
    )
}

pub fn draft_source(r: &mut impl Read, row: &OutRow) -> Result<Option<String>> {
    let answer = Query::select()
        .column("answer_to")
        .from("chan_messages")
        .and_where(Expr::col("id").eq(row.id.as_str()))
        .take();
    let delivery = Query::select()
        .column("source_message_id")
        .from("output_deliveries")
        .and_where(Expr::col("message_id").eq(row.id.as_str()))
        .take();
    r.one(
        Query::select()
            .column("id")
            .from("chan_messages")
            .and_where(Expr::col("workspace_id").eq(row.ws.as_str()))
            .and_where(Expr::col("space_id").eq(row.space_id.as_str()))
            .and_where(Expr::col("direction").eq("in"))
            .and_where(Expr::col("purged").is_null())
            .and_where(
                Expr::col("id")
                    .in_subquery(answer)
                    .or(Expr::col("id").in_subquery(delivery))
                    .or(Expr::col("external_id").eq(row.reply_to.as_deref())),
            )
            .order_by("created", Order::Desc)
            .limit(1),
        |row| row.get(0),
    )
}

pub fn store_composer_result(w: &mut W<'_>, id: &str, result: &Value) -> Result<()> {
    w.exec(
        Query::update()
            .table("chan_messages")
            .value("composed", result.to_string())
            .and_where(Expr::col("id").eq(id)),
    )?;
    Ok(())
}

/// Editing a draft invalidates the basis of its previously generated text.
pub fn edit_draft(w: &mut W<'_>, id: &str, text: &str) -> Result<()> {
    w.exec(
        Query::update()
            .table("chan_messages")
            .value("text", text)
            .value("basis_id", Option::<String>::None)
            .and_where(Expr::col("id").eq(id))
            .and_where(Expr::col("status").eq("draft")),
    )?;
    digest::sync_message(w.conn, Source::ChanMessage, id, None)?;
    Ok(())
}

pub fn update_draft_text(w: &mut W<'_>, id: &str, text: &str) -> Result<()> {
    w.exec(
        Query::update()
            .table("chan_messages")
            .value("text", text)
            .and_where(Expr::col("id").eq(id))
            .and_where(Expr::col("status").eq("draft")),
    )?;
    Ok(())
}

pub fn composer_result(r: &mut impl Read, id: &str) -> Result<Value> {
    let raw: Option<String> = r
        .one(
            Query::select()
                .column("composed")
                .from("chan_messages")
                .and_where(Expr::col("id").eq(id)),
            |row| row.get(0),
        )?
        .flatten();
    Ok(raw
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or_default())
}

pub fn pending_disclosure(r: &mut impl Read, verdict: &str, limit: u64) -> Result<Vec<String>> {
    r.all(
        Query::select()
            .column(("m", "id"))
            .from_as("chan_messages", "m")
            .join_as(
                sea_query::JoinType::InnerJoin,
                "reply_basis",
                "b",
                Expr::col(("b", "id")).equals(("m", "basis_id")),
            )
            .and_where(Expr::col(("m", "direction")).eq("out"))
            .and_where(Expr::col(("m", "status")).eq("draft"))
            .and_where(Expr::col(("b", "verdict")).eq(verdict))
            .order_by(("m", "created"), Order::Asc)
            .limit(limit),
        |row| row.get(0),
    )
}

pub fn sent_connection(r: &mut impl Read, id: &str) -> Result<Option<String>> {
    Ok(r.one(
        Query::select()
            .column("sent_via")
            .from("chan_messages")
            .and_where(Expr::col("id").eq(id)),
        |row| row.get(0),
    )?
    .flatten())
}
