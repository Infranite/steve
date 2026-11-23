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
