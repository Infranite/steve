//! Durable startup failure and its decision card.

use sea_query::{Expr, ExprTrait, OnConflict, Query};
use steve_types::{DomainEvent, WorkEvent};

use crate::{Decision, Read, Result, W};

const REASON: &str = crate::SESSION_START_FAILURE;

pub struct Scope {
    pub workspace: String,
    pub item: Option<String>,
    pub bot: Option<String>,
}

pub fn scope(c: &mut impl Read, session: &str) -> Result<Scope> {
    c.one(
        Query::select()
            .columns(["workspace_id", "item_id", "bot_id"])
            .from("sessions")
            .and_where(Expr::col("id").eq(session)),
        |r| {
            Ok(Scope {
                workspace: r.get(0)?,
                item: r.get(1)?,
                bot: r.get(2)?,
            })
        },
    )?
    .ok_or_else(|| crate::StoreError::Rejected(format!("missing session {session}")))
}

pub fn open_decisions(
    c: &mut impl Read,
    session: &str,
    item: Option<&str>,
) -> Result<Vec<Decision>> {
    crate::contract::decisions::open_for_session_or_item(c, session, item)
}

pub fn active(c: &mut impl Read, session: &str) -> Result<bool> {
    Ok(c.one(
        Query::select()
            .column("session_id")
            .from("session_stops")
            .and_where(Expr::col("session_id").eq(session))
            .and_where(Expr::col("stop_reason").eq(REASON)),
        |_| Ok(()),
    )?
    .is_some())
}

pub fn record_stop(w: &mut W<'_>, session: &str, detail: &str, at: i64) -> Result<()> {
    w.exec(
        Query::insert()
            .into_table("session_stops")
            .columns(["session_id", "stop_reason", "error", "ended", "updated"])
            .values_panic([
                session.into(),
                REASON.into(),
                detail.into(),
                at.into(),
                at.into(),
            ])
            .on_conflict(
                OnConflict::column("session_id")
                    .update_columns(["stop_reason", "error", "ended", "updated"])
                    .to_owned(),
            ),
    )?;
    Ok(())
}

pub fn already_open(w: &mut W<'_>, session: &str, question: &str) -> Result<bool> {
    use crate::contract::Q;

    Ok(w.one(
        Query::select()
            .column("id")
            .from("decisions")
            .and_where(Expr::col("session_id").eq(session))
            .and_where(Expr::col("subject_type").eq(REASON))
            .and_where(Expr::col("question").eq(question))
            .and_where(Expr::col("status").eq("open"))
            .limit(1),
        |_| Ok(()),
    )?
    .is_some())
}

pub fn hold_item(w: &mut W<'_>, workspace: &str, item: &str, at: i64) -> Result<()> {
    w.exec(
        Query::update()
            .table("items")
            .value("updated", at)
            .and_where(Expr::col("id").eq(item))
            .and_where(Expr::col("phase").ne("closed"))
            .and_where(Expr::col("phase").ne("paused")),
    )?;
    w.emit(DomainEvent::from(WorkEvent::Item {
        ws: workspace.to_string().into(),
        id: item.to_string().into(),
    }));
    Ok(())
}

pub fn clear_stop(w: &mut W<'_>, session: &str) -> Result<()> {
    w.exec(
        Query::delete()
            .from_table("session_stops")
            .and_where(Expr::col("session_id").eq(session))
            .and_where(Expr::col("stop_reason").eq(REASON)),
    )?;
    Ok(())
}
