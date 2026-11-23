//! Storage for a channel message received through one or several connections.

use sea_query::{Expr, ExprTrait, Func, JoinType, OnConflict, Order, Query, SelectStatement};
use serde_json::Value;

use crate::contract::Q;
use crate::{Read, Result, W};

/// Prefix of locally assigned message IDs that must stay inside the Hub.
pub const SYNTHETIC_PREFIX: &str = "fp:";

/// An inbound message's source data. Attachments and mentions are stored as JSON.
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
    pub content_key: Option<&'a str>,
    /// The connection that supplied it without a provider id.
    pub fallback_from: Option<&'a str>,
    pub history: Option<&'a str>,
    /// Bits for the identities that received it.
    pub seen_by: i64,
}

/// Insert the source and its digest membership together. A duplicate delivery returns no id.
pub fn insert(w: &mut W<'_>, n: &NewInbound<'_>) -> Result<Option<String>> {
    let id = crate::uid("cm");
    let changed = w.exec(
        Query::insert()
            .into_table("chan_messages")
            .columns([
                "id",
                "workspace_id",
                "space_id",
                "external_id",
                "thread_key",
                "reply_to",
                "direction",
                "sender_account_id",
                "sender_party_id",
                "text",
                "attachments",
                "mentions",
                "status",
                "created",
                "content_key",
                "fallback_from",
                "history",
                "seen_by",
            ])
            .values_panic([
                id.as_str().into(),
                n.ws.into(),
                n.space_id.into(),
                n.external_id.into(),
                n.thread_key.into(),
                n.reply_to.into(),
                "in".into(),
                n.sender_account_id.into(),
                n.sender_party_id.into(),
                n.text.into(),
                n.attachments.to_string().into(),
                n.mentions.to_string().into(),
                "received".into(),
                n.created.into(),
                n.content_key.into(),
                n.fallback_from.into(),
                n.history.into(),
                n.seen_by.into(),
            ])
            .on_conflict(OnConflict::new().do_nothing().to_owned()),
    )?;
    if changed == 0 {
        return Ok(None);
    }
    steve_store_sqlite::digest::sync_message(
        &w.legacy(),
        steve_store_sqlite::digest::Source::ChanMessage,
        &id,
        None,
    )?;
    super::emit(w, n.ws, n.space_id, &id);
    Ok(Some(id))
}

/// The identities that have received the same message can add their bits independently.
pub fn mark_seen(w: &mut W<'_>, id: &str, conn: &str) -> Result<()> {
    let space: Option<String> = w.one(
        Query::select()
            .column("space_id")
            .from("chan_messages")
            .and_where(Expr::col("id").eq(id)),
        |row| row.get(0),
    )?;
    let Some(space) = space else {
        return Ok(());
    };
    let bit = super::super::channel_spaces::identity_bit(w, &space, conn)?;
    if bit != 0 {
        w.exec(
            Query::update()
                .table("chan_messages")
                .value(
                    "seen_by",
                    Expr::cust_with_exprs("? | ?", [Expr::col("seen_by"), Expr::val(bit)]),
                )
                .and_where(Expr::col("id").eq(id)),
        )?;
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Found {
    pub id: String,
    pub space_id: String,
    pub out: bool,
}

pub fn by_source(r: &mut impl Read, conn: &str, source: &str) -> Result<Option<Found>> {
    r.one(
        Query::select()
            .columns([("m", "id"), ("m", "space_id"), ("m", "direction")])
            .from_as("chan_message_seen", "x")
            .join_as(
                JoinType::InnerJoin,
                "chan_messages",
                "m",
                Expr::col(("m", "id")).eq(Expr::col(("x", "message_id"))),
            )
            .and_where(Expr::col(("x", "connection_id")).eq(conn))
            .and_where(Expr::col(("x", "source_id")).eq(source)),
        |row| {
            Ok(Found {
                id: row.get(0)?,
                space_id: row.get(1)?,
                out: row.get::<String>(2)? == "out",
            })
        },
    )
}

pub fn by_external(r: &mut impl Read, space: &str, external: &str) -> Result<Option<String>> {
    r.one(
        Query::select()
            .column("id")
            .from("chan_messages")
            .and_where(Expr::col("space_id").eq(space))
            .and_where(Expr::col("external_id").eq(external)),
        |row| row.get(0),
    )
}

pub fn sender_for_external(
    r: &mut impl Read,
    space: &str,
    external: &str,
) -> Result<Option<String>> {
    Ok(r.one(
        Query::select()
            .column("sender_account_id")
            .from("chan_messages")
            .and_where(Expr::col("space_id").eq(space))
            .and_where(Expr::col("external_id").eq(external))
            .and_where(Expr::col("direction").eq("in")),
        |row| row.get(0),
    )?
    .flatten())
}

pub fn has_provider_message(
    r: &mut impl Read,
    ws: &str,
    chat: &str,
    external: &str,
) -> Result<bool> {
    Ok(r.one(
        in_chat(ws, chat)
            .column(("m", "id"))
            .and_where(Expr::col(("m", "external_id")).eq(external))
            .limit(1),
        |_| Ok(()),
    )?
    .is_some())
}

pub fn first_in_chat(
    r: &mut impl Read,
    ws: &str,
    chat: &str,
    external: &str,
) -> Result<Option<String>> {
    r.one(
        in_chat(ws, chat)
            .column(("m", "id"))
            .and_where(Expr::col(("m", "external_id")).eq(external))
            .order_by_columns([(("m", "created"), Order::Asc), (("m", "id"), Order::Asc)])
            .limit(1),
        |row| row.get(0),
    )
}

fn in_chat(ws: &str, chat: &str) -> SelectStatement {
    Query::select()
        .from_as("chan_messages", "m")
        .join_as(
            JoinType::InnerJoin,
            "spaces",
            "s",
            Expr::col(("s", "id")).eq(Expr::col(("m", "space_id"))),
        )
        .and_where(Expr::col(("s", "workspace_id")).eq(ws))
        .and_where(Expr::col(("s", "external_id")).eq(chat))
        .take()
}

pub fn sent_with_id(
    r: &mut impl Read,
    ws: &str,
    chat: &str,
    external: &str,
) -> Result<Option<(String, String)>> {
    r.one(
        in_chat(ws, chat)
            .columns([("m", "id"), ("m", "space_id")])
            .and_where(Expr::col(("m", "external_id")).eq(external))
            .and_where(Expr::col(("m", "direction")).eq("out"))
            .order_by_columns([(("m", "created"), Order::Asc), (("m", "id"), Order::Asc)])
            .limit(1),
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
}

pub struct SendCandidate {
    pub id: String,
    pub space: String,
    pub text: String,
}

pub fn possible_echoes(
    r: &mut impl Read,
    ws: &str,
    chat: &str,
    since: i64,
) -> Result<Vec<SendCandidate>> {
    let sent = Expr::from(Func::coalesce([
        Expr::col(("m", "sent")),
        Expr::col(("m", "created")),
    ]));
    r.all(
        in_chat(ws, chat)
            .columns([("m", "id"), ("m", "space_id"), ("m", "text")])
            .and_where(Expr::col(("m", "workspace_id")).eq(ws))
            .and_where(Expr::col(("m", "direction")).eq("out"))
            .and_where(Expr::col(("m", "status")).is_in(["pending", "sent", "unknown"]))
            .and_where(Expr::col(("m", "external_id")).is_null())
            .and_where(sent.gte(since))
            .order_by_columns([(("m", "created"), Order::Desc), (("m", "id"), Order::Asc)]),
        |row| {
            Ok(SendCandidate {
                id: row.get(0)?,
                space: row.get(1)?,
                text: row.get::<Option<String>>(2)?.unwrap_or_default(),
            })
        },
    )
}

pub fn fill_echo_id(w: &mut W<'_>, message: &str, external: &str) -> Result<()> {
    w.exec(
        Query::update()
            .table("chan_messages")
            .value("external_id", external)
            .and_where(Expr::col("id").eq(message))
            .and_where(Expr::col("external_id").is_null()),
    )?;
    Ok(())
}

/// Fill metadata a first delivery did not carry.
pub fn complete(
    w: &mut W<'_>,
    id: &str,
    thread: Option<&str>,
    reply_to: Option<&str>,
    sender: Option<&str>,
    party: Option<&str>,
) -> Result<()> {
    w.exec(
        Query::update()
            .table("chan_messages")
            .value(
                "thread_key",
                Func::coalesce([Expr::col("thread_key"), Expr::val(thread)]),
            )
            .value(
                "reply_to",
                Func::coalesce([Expr::col("reply_to"), Expr::val(reply_to)]),
            )
            .value(
                "sender_account_id",
                Func::coalesce([Expr::col("sender_account_id"), Expr::val(sender)]),
            )
            .value(
                "sender_party_id",
                Func::coalesce([Expr::col("sender_party_id"), Expr::val(party)]),
            )
            .and_where(Expr::col("id").eq(id)),
    )?;
    Ok(())
}

pub fn link_event(w: &mut W<'_>, message: &str, event: &str) -> Result<()> {
    w.exec(
        Query::update()
            .table("chan_messages")
            .value("event_id", event)
            .and_where(Expr::col("id").eq(message)),
    )?;
    Ok(())
}

/// Replace a synthetic id when the provider's real id becomes known. Replies follow it.
pub fn upgrade_provider_id(w: &mut W<'_>, id: &str, provider_id: &str) -> Result<()> {
    if provider_id.is_empty() || provider_id.starts_with(SYNTHETIC_PREFIX) {
        return Ok(());
    }
    let Some((space, old)) = w.one(
        Query::select()
            .columns(["space_id", "external_id"])
            .from("chan_messages")
            .and_where(Expr::col("id").eq(id)),
        |row| Ok((row.get::<String>(0)?, row.get::<Option<String>>(1)?)),
    )?
    else {
        return Ok(());
    };
    if old
        .as_deref()
        .is_some_and(|old| !old.starts_with(SYNTHETIC_PREFIX))
    {
        return Ok(());
    }
    let existing = Query::select()
        .column("id")
        .from("chan_messages")
        .and_where(Expr::col("space_id").eq(space.as_str()))
        .and_where(Expr::col("external_id").eq(provider_id))
        .take();
    let changed = w.exec(
        Query::update()
            .table("chan_messages")
            .value("external_id", provider_id)
            .and_where(Expr::col("id").eq(id))
            .and_where(Expr::exists(existing).not()),
    )?;
    if changed == 1
        && let Some(old) = old
    {
        w.exec(
            Query::update()
                .table("chan_messages")
                .value("reply_to", provider_id)
                .and_where(Expr::col("space_id").eq(space))
                .and_where(Expr::col("reply_to").eq(old)),
        )?;
    }
    Ok(())
}

/// Keep the delivery id for a copy of the message and remember a copy supplied without a provider id.
pub fn supplied(
    w: &mut W<'_>,
    message: &str,
    conn: &str,
    source: Option<&str>,
    without_id: bool,
) -> Result<()> {
    if let Some(source) = source {
        w.exec(
            Query::insert()
                .into_table("chan_message_seen")
                .columns(["connection_id", "source_id", "message_id"])
                .values_panic([conn.into(), source.into(), message.into()])
                .on_conflict(
                    OnConflict::columns(["connection_id", "source_id"])
                        .do_nothing()
                        .to_owned(),
                ),
        )?;
    }
    if without_id {
        w.exec(
            Query::update()
                .table("chan_messages")
                .value(
                    "fallback_from",
                    Func::coalesce([Expr::col("fallback_from"), Expr::val(conn)]),
                )
                .and_where(Expr::col("id").eq(message)),
        )?;
    }
    Ok(())
}

/// The first live receipt records the sender known to this personal connection.
pub fn trigger_receipt(
    w: &mut W<'_>,
    conn: &str,
    message: &str,
    account: Option<&str>,
    party: Option<&str>,
    from_self: bool,
) -> Result<()> {
    let live = w.one(
        Query::select()
            .column("id")
            .from("chan_messages")
            .and_where(Expr::col("id").eq(message))
            .and_where(Expr::col("direction").eq("in"))
            .and_where(Expr::col("history").is_null()),
        |_| Ok(()),
    )?;
    if live.is_some() {
        w.exec(
            Query::insert()
                .into_table("chan_trigger_receipts")
                .columns([
                    "message_id",
                    "connection_id",
                    "sender_account_id",
                    "sender_party_id",
                    "from_self",
                ])
                .values_panic([
                    message.into(),
                    conn.into(),
                    account.into(),
                    party.into(),
                    i64::from(from_self).into(),
                ])
                .on_conflict(
                    OnConflict::columns(["message_id", "connection_id"])
                        .do_nothing()
                        .to_owned(),
                ),
        )?;
    }
    Ok(())
}

/// Fill the provider's timestamp after reading back a send that did not return it.
pub fn fill_created(w: &mut W<'_>, message: &str, created: i64) -> Result<()> {
    w.exec(
        Query::update()
            .table("chan_messages")
            .value(
                "ext_created",
                Func::coalesce([Expr::col("ext_created"), Expr::val(created)]),
            )
            .and_where(Expr::col("id").eq(message))
            .and_where(Expr::col("direction").eq("out")),
    )?;
    Ok(())
}
