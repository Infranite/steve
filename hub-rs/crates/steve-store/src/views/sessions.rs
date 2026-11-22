//! Session views, including recorded execution identity and the queue's count.
use sea_query::{Alias, Expr, ExprTrait, Order, Query, SelectStatement};
use steve_types::{Millis, SessionView, SessionWaiting};

use super::{Page, page, read_ids};
use crate::contract::Row;
use crate::runtime::{HUB_DEVICE, HUB_NODE, OCCUPYING, SESSIONS_DEFAULT, SESSIONS_MAX, WAITING};
use crate::{Read, Result};

fn select() -> SelectStatement {
    let mut q = Query::select();
    let capacity = format!(
        "(SELECT count(*) FROM node_commands c WHERE c.node_id=n.node_id AND {OCCUPYING})
         >= min({SESSIONS_MAX}, max(1, coalesce(CASE WHEN n.node_id='{HUB_NODE}'
             THEN (SELECT max_sessions FROM devices WHERE id='{HUB_DEVICE}')
             ELSE (SELECT max_sessions FROM nodes WHERE node_id=n.node_id) END, {SESSIONS_DEFAULT})))"
    );
    let waiting = format!(
        "SELECT CASE WHEN {capacity} THEN 'capacity' WHEN detail IS NOT NULL THEN 'node' END
         FROM (SELECT c.node_id, json_extract(c.receipt,'$.last_error.safe_detail') AS detail
               FROM node_commands c WHERE c.node_id=(SELECT node_id FROM runtime_sessions WHERE session_id=sessions.id)
                 AND json_extract(c.request,'$.body.session')=sessions.id AND {WAITING}
               ORDER BY c.created,c.rowid LIMIT 1) n"
    );
    q.from(Alias::new("sessions"))
        .columns([
            "id", "workspace_id", "item_id", "no", "bot_id", "harness", "model", "reasoning_effort",
            "cwd", "device_id", "title", "source", "readonly", "status", "acp_session_id",
            "used", "size", "cost", "created", "updated", "import_key",
        ].map(Alias::new))
        .expr(Expr::cust("(SELECT node_id FROM runtime_sessions WHERE session_id=sessions.id)"))
        .expr(Expr::cust("(SELECT count(*) FROM queued WHERE session_id=sessions.id)"))
        .expr(Expr::cust("EXISTS(SELECT 1 FROM runtime_sessions WHERE session_id=sessions.id AND (active_attempt IS NOT NULL OR phase IN ('opening','releasing','active','configuring')))
                       OR EXISTS(SELECT 1 FROM runtime_turns WHERE session_id=sessions.id AND state<>'finished')"))
        .expr(Expr::cust(format!("({waiting})")))
        .expr(Expr::cust(format!("(SELECT json_extract(c.receipt,'$.last_error.safe_detail') FROM node_commands c WHERE c.node_id=(SELECT node_id FROM runtime_sessions WHERE session_id=sessions.id) AND json_extract(c.request,'$.body.session')=sessions.id AND {WAITING} ORDER BY c.created,c.rowid LIMIT 1)")));
    q
}

fn nonempty(v: Option<String>) -> Option<String> {
    v.filter(|s| !s.trim().is_empty())
}

fn row(r: &Row<'_>) -> Result<SessionView> {
    let native: Option<String> = r.get(14)?;
    let imported: Option<String> = r.get(20)?;
    let waiting = r
        .get::<Option<String>>(24)?
        .map(|reason| -> Result<_> {
            Ok(SessionWaiting {
                reason,
                detail: r.get(25)?,
            })
        })
        .transpose()?;
    Ok(SessionView {
        id: r.get::<String>(0)?.into(),
        ws: r.get::<String>(1)?.into(),
        item_id: r.get::<Option<String>>(2)?.map(Into::into),
        no: r.get(3)?,
        bot_id: r.get::<Option<String>>(4)?.map(Into::into),
        harness: nonempty(r.get(5)?),
        model: nonempty(r.get(6)?),
        reasoning_effort: nonempty(r.get(7)?),
        cwd: r.get(8)?,
        device_id: nonempty(r.get(9)?).map(Into::into),
        title: r.get(10)?,
        source: r.get(11)?,
        readonly: r.get(12)?,
        status: r.get(13)?,
        harness_session_id: crate::native_id(native.as_deref(), imported.as_deref()),
        used: r.get(15)?,
        size: r.get(16)?,
        cost: r
            .get::<Option<f64>>(17)?
            .and_then(serde_json::Number::from_f64),
        created: Millis(r.get(18)?),
        updated: Millis(r.get(19)?),
        node_id: nonempty(r.get(21)?).map(Into::into),
        queued: r.get::<i64>(22)? as u32,
        busy: r.get(23)?,
        waiting,
    })
}

pub struct SessionViews<'a, C>(pub(super) &'a mut C);

impl<C: Read> SessionViews<'_, C> {
    pub fn hot(&mut self) -> Result<Vec<SessionView>> {
        self.0.all(
            select().and_where(Expr::col(Alias::new("status")).ne("ended")),
            row,
        )
    }

    pub fn of(&mut self, ws: &str, ids: &[String]) -> Result<Vec<SessionView>> {
        read_ids(self.0, select(), ws, ids, row)
    }

    pub fn of_items(&mut self, ws: &str, ids: &[String]) -> Result<Vec<SessionView>> {
        let mut rows = Vec::new();
        for ids in ids.chunks(super::CHUNK) {
            rows.extend(
                self.0.all(
                    select()
                        .and_where(Expr::col(Alias::new("workspace_id")).eq(ws))
                        .and_where(
                            Expr::col(Alias::new("item_id")).is_in(ids.iter().map(String::as_str)),
                        )
                        .order_by(Alias::new("no"), Order::Asc)
                        .order_by(Alias::new("id"), Order::Asc),
                    row,
                )?,
            );
        }
        Ok(rows)
    }

    pub fn ended(
        &mut self,
        ws: &str,
        options: &Page,
    ) -> Result<(Vec<SessionView>, Option<String>)> {
        page(
            self.0,
            select().and_where(Expr::col(Alias::new("status")).eq("ended")),
            ws,
            options,
            "updated",
            row,
            |v| (v.updated.0, v.id.as_str()),
        )
    }
}
