//! Channel message removal and its digest queue membership.

use sea_query::{Expr, ExprTrait, Query};
use steve_store_sqlite::digest::{self, MessageFilter, Source};

use crate::contract::Q;
use crate::{Result, W};

pub fn remove(w: &mut W<'_>, ids: &[String]) -> Result<()> {
    if ids.is_empty() {
        return Ok(());
    }
    let json = serde_json::to_string(ids)?;
    let queued = Source::ChanMessage
        .messages(&w.legacy(), MessageFilter::IdsJson(&json))?
        .into_iter()
        .map(|message| (message.scope, message.id));
    let keys = w.db().dialect().text_array(ids)?;
    w.exec(
        Query::delete()
            .from_table("chan_messages")
            .and_where(Expr::col("id").in_subquery(keys)),
    )?;
    digest::remove_message(&w.legacy(), queued)?;
    Ok(())
}

pub fn clear_purged_attachments(w: &mut W<'_>) -> Result<()> {
    w.exec(
        Query::update()
            .table("chan_messages")
            .value("attachments", "[]")
            .and_where(Expr::col("purged").is_not_null())
            .and_where(Expr::col("attachments").ne("[]")),
    )?;
    Ok(())
}

pub fn purge_before(w: &mut W<'_>, ws: &str, cutoff: i64, now: i64, limit: u64) -> Result<u64> {
    let due = Query::select()
        .column("id")
        .from("chan_messages")
        .and_where(Expr::col("workspace_id").eq(ws))
        .and_where(Expr::col("purged").is_null())
        .and_where(Expr::col("created").lt(cutoff))
        .limit(limit)
        .take();
    w.exec(
        Query::update()
            .table("chan_messages")
            .value("text", Option::<String>::None)
            .value("attachments", "[]")
            .value("purged", now)
            .and_where(Expr::col("id").in_subquery(due)),
    )
}
