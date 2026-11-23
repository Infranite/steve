//! Persistent send claims and channel message delivery transitions.

use sea_query::{Expr, ExprTrait, Func, Query, UpdateStatement};

use crate::{Result, W};

use steve_store_sqlite::digest::{self, Source};

const SOURCE: Source = Source::ChanMessage;

#[derive(Clone, Copy)]
pub enum SentAt {
    Keep,
    Clear,
    ClearClaim(i64),
}

pub fn transition(
    w: &mut W<'_>,
    id: &str,
    from: &[&str],
    to: &str,
    sent_at: SentAt,
) -> Result<bool> {
    let mut update = message(id);
    update.value("status", to);
    if !from.is_empty() {
        update.and_where(Expr::col("status").is_in(from.iter().copied()));
    }
    if !matches!(sent_at, SentAt::Keep) {
        update.value("sent", Option::<i64>::None);
    }
    if let SentAt::ClearClaim(claim) = sent_at {
        update.and_where(Expr::col("sent").eq(claim));
    }
    sync(w, id, &update)
}

pub fn claim(w: &mut W<'_>, id: &str, now: i64, stale: i64, retry_unknown: bool) -> Result<bool> {
    let pending = Expr::col("status")
        .eq("pending")
        .and(Expr::col("sent").is_null().or(Expr::col("sent").lt(stale)));
    let mut update = message(id);
    update
        .value("status", "pending")
        .value("sent", now)
        .and_where(if retry_unknown {
            pending.or(Expr::col("status").eq("unknown"))
        } else {
            pending
        });
    sync(w, id, &update)
}

pub fn release_claim(w: &mut W<'_>, id: &str, claim: i64) -> Result<bool> {
    let next = if crate::channel_sends::attempted(w, id)? {
        "unknown"
    } else {
        "pending"
    };
    transition(w, id, &["pending"], next, SentAt::ClearClaim(claim))
}

pub struct Sent<'a> {
    pub external_id: Option<&'a str>,
    pub text: &'a str,
    pub now: i64,
    pub connection: &'a str,
    pub provider_created: Option<i64>,
    pub thread_key: Option<&'a str>,
    pub content_key: Option<&'a str>,
    pub source_id: Option<&'a str>,
}

pub fn finish(w: &mut W<'_>, id: &str, claim: i64, sent: &Sent<'_>) -> Result<bool> {
    let mut update = message(id);
    update
        .value("status", "sent")
        .value("external_id", sent.external_id)
        .value("text", sent.text)
        .value("sent", sent.now)
        .value("sent_via", sent.connection)
        .value("ext_created", sent.provider_created)
        .value(
            "thread_key",
            Func::coalesce([Expr::col("thread_key"), Expr::val(sent.thread_key)]),
        )
        .value("content_key", sent.content_key)
        .and_where(Expr::col("status").eq("pending"))
        .and_where(Expr::col("sent").eq(claim));
    let changed = sync(w, id, &update)?;
    if changed {
        super::ingress::supplied(w, id, sent.connection, sent.source_id, false)?;
    }
    Ok(changed)
}

pub fn approve_edit(w: &mut W<'_>, id: &str, text: &str) -> Result<bool> {
    let mut update = message(id);
    update
        .value("text", text)
        .value("status", "pending")
        .value("sent", Option::<i64>::None)
        .and_where(Expr::col("status").eq("draft"));
    sync(w, id, &update)
}

fn message(id: &str) -> UpdateStatement {
    Query::update()
        .table("chan_messages")
        .and_where(Expr::col("id").eq(id))
        .take()
}

fn sync(w: &mut W<'_>, id: &str, update: &UpdateStatement) -> Result<bool> {
    let old = SOURCE.scope(w.conn, id)?;
    let changed = w.exec(update)? != 0;
    if changed {
        digest::sync_message(w.conn, SOURCE, id, old.as_ref())?;
    }
    Ok(changed)
}

pub fn has_claim(r: &mut impl crate::Read, message: &str, claim: i64) -> Result<bool> {
    Ok(r.one(
        Query::select()
            .column("id")
            .from("chan_messages")
            .and_where(Expr::col("id").eq(message))
            .and_where(Expr::col("status").eq("pending"))
            .and_where(Expr::col("sent").eq(claim)),
        |_| Ok(()),
    )?
    .is_some())
}
