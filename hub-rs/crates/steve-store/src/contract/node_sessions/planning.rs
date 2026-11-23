//! Queue order and session state used when planning a Node turn.

use sea_query::{Expr, ExprTrait, Func, Query};

use super::Phase;
use crate::{Read, Result, StoreError};

pub fn required_phase(r: &mut impl Read, session: &str) -> Result<Phase> {
    super::phase(r, session, None)?
        .ok_or_else(|| StoreError::Rejected(format!("missing runtime session {session}")))
}

pub fn queue_head(r: &mut impl Read, session: &str, text: &str) -> Result<(Option<i64>, bool)> {
    let queue = || {
        Query::select()
            .from("queued")
            .and_where(Expr::col("session_id").eq(session))
            .to_owned()
    };
    let duplicate = queue()
        .column("id")
        .and_where(Expr::col("text").eq(text))
        .take();
    Ok(r.one(
        queue()
            .expr(Func::min(Expr::col("created")))
            .expr(Expr::exists(duplicate)),
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?
    .unwrap_or_default())
}

pub fn has_queued(r: &mut impl Read, session: &str) -> Result<bool> {
    Ok(r.one(
        Query::select()
            .column("id")
            .from("queued")
            .and_where(Expr::col("session_id").eq(session))
            .limit(1),
        |_| Ok(()),
    )?
    .is_some())
}

pub fn has_running_harness(r: &mut impl Read, session: &str) -> Result<bool> {
    Ok(r.one(
        Query::select()
            .column("id")
            .from("sessions")
            .and_where(Expr::col("id").eq(session))
            .and_where(
                Expr::col("acp_session_id")
                    .is_not_null()
                    .or(Expr::col("status").eq("running")),
            ),
        |_| Ok(()),
    )?
    .is_some())
}

pub fn local_admits_work(r: &mut impl Read) -> Result<bool> {
    let paused = r.one(
        Query::select()
            .column("id")
            .from("devices")
            .and_where(Expr::col("id").eq(crate::runtime::HUB_DEVICE))
            .and_where(Expr::col("status").eq("暂停接活")),
        |_| Ok(()),
    )?;
    Ok(paused.is_none())
}
