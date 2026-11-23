use sea_query::{Expr, ExprTrait, OnConflict, Query};

use super::Event;
use crate::{Result, W};

pub(super) fn claim(w: &mut W<'_>, consumer: &str, ev: &Event, now: i64) -> Result<bool> {
    Ok(w.exec(
        Query::insert()
            .into_table("outbox_receipts")
            .columns([
                "event_seq",
                "consumer_id",
                "workspace_id",
                "done",
                "outcome",
            ])
            .values_panic([
                ev.seq.into(),
                consumer.into(),
                ev.workspace_id.as_str().into(),
                now.into(),
                "".into(),
            ])
            .on_conflict(
                OnConflict::columns(["event_seq", "consumer_id"])
                    .do_nothing()
                    .to_owned(),
            ),
    )? == 1)
}

pub(super) fn outcome(w: &mut W<'_>, consumer: &str, seq: i64, value: &str) -> Result<()> {
    w.exec(
        Query::update()
            .table("outbox_receipts")
            .value("outcome", value)
            .and_where(Expr::col("event_seq").eq(seq))
            .and_where(Expr::col("consumer_id").eq(consumer)),
    )?;
    Ok(())
}

pub(super) fn release(w: &mut W<'_>, consumer: &str, seq: i64) -> Result<()> {
    w.exec(
        Query::delete()
            .from_table("outbox_leases")
            .and_where(Expr::col("event_seq").eq(seq))
            .and_where(Expr::col("consumer_id").eq(consumer)),
    )?;
    Ok(())
}
