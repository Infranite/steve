//! Message text from a channel space for reply and intent context.

use sea_query::{Expr, ExprTrait, Order, Query, SelectStatement};

use crate::contract::Row;
use crate::{Read, Result};

pub fn bot_notice_samples(
    r: &mut impl Read,
    space: &str,
    message: &str,
    since: i64,
) -> Result<Vec<(String, serde_json::Value)>> {
    r.all(
        Query::select()
            .columns(["text", "attachments"])
            .from("chan_messages")
            .and_where(Expr::col("space_id").eq(space))
            .and_where(Expr::col("direction").eq("in"))
            .and_where(Expr::col("work_item_id").is_not_null())
            .and_where(Expr::col("id").ne(message))
            .and_where(Expr::col("created").gte(since))
            .order_by("created", Order::Desc)
            .limit(50),
        |row| {
            let text: Option<String> = row.get(0)?;
            let attachments: Option<String> = row.get(1)?;
            Ok((
                text.unwrap_or_default(),
                attachments
                    .and_then(|raw| serde_json::from_str(&raw).ok())
                    .unwrap_or(serde_json::Value::Null),
            ))
        },
    )
}

pub fn bot_share(r: &mut impl Read, space: &str, sample_size: u64) -> Result<(i64, i64)> {
    let senders: Vec<(Option<String>, Option<String>)> = r.all(
        Query::select()
            .columns(["sender_account_id", "sender_party_id"])
            .from("chan_messages")
            .and_where(Expr::col("space_id").eq(space))
            .and_where(Expr::col("direction").eq("in"))
            .order_by("created", Order::Desc)
            .limit(sample_size),
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let accounts: Vec<&str> = senders
        .iter()
        .filter_map(|(account, _)| account.as_deref())
        .collect();
    let parties: Vec<&str> = senders
        .iter()
        .filter_map(|(_, party)| party.as_deref())
        .collect();
    let mut bots: std::collections::HashSet<_> = crate::accounts::bot_ids(r, &accounts)?
        .into_iter()
        .collect();
    bots.extend(crate::channel_spaces::members::bots(r, space, &accounts)?);
    let bot_parties: std::collections::HashSet<_> =
        crate::parties::bot_ids(r, &parties)?.into_iter().collect();
    let count = senders
        .iter()
        .filter(|(account, party)| {
            account.as_ref().is_some_and(|id| bots.contains(id))
                || party.as_ref().is_some_and(|id| bot_parties.contains(id))
        })
        .count();
    Ok((senders.len() as i64, count as i64))
}

pub struct Line {
    pub account: Option<String>,
    pub party: Option<String>,
    pub text: String,
}

fn text_lines(space: &str) -> SelectStatement {
    Query::select()
        .columns(["sender_account_id", "sender_party_id", "text"])
        .from("chan_messages")
        .and_where(Expr::col("space_id").eq(space))
        .and_where(Expr::col("text").is_not_null())
        .and_where(Expr::col("text").ne(""))
        .take()
}

fn read_line(row: &Row<'_>) -> Result<Line> {
    Ok(Line {
        account: row.get(0)?,
        party: row.get(1)?,
        text: row.get(2)?,
    })
}

pub fn before(r: &mut impl Read, space: &str, message: &str, limit: u64) -> Result<Vec<Line>> {
    let time = Query::select()
        .column("created")
        .from("chan_messages")
        .and_where(Expr::col("id").eq(message))
        .take();
    let mut lines = r.all(
        text_lines(space)
            .and_where(Expr::col("id").ne(message))
            .and_where(Expr::col("status").is_in(["received", "sent"]))
            .and_where(
                Expr::col("created").lte(Expr::from(sea_query::SubQueryStatement::from(time))),
            )
            .order_by_columns([("created", Order::Desc), ("id", Order::Desc)])
            .limit(limit),
        read_line,
    )?;
    lines.reverse();
    Ok(lines)
}

pub fn quoted(r: &mut impl Read, space: &str, message: &str) -> Result<Option<Line>> {
    let external = Query::select()
        .column("reply_to")
        .from("chan_messages")
        .and_where(Expr::col("space_id").eq(space))
        .and_where(Expr::col("id").eq(message))
        .take();
    r.one(
        text_lines(space).and_where(
            Expr::col("external_id").eq(Expr::from(sea_query::SubQueryStatement::from(external))),
        ),
        read_line,
    )
}
