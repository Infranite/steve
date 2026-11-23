//! Durable GitHub delivery queue and poll state.

use sea_query::{Expr, ExprTrait, Func, JoinType, OnConflict, Order, Query, SelectStatement};

use crate::contract::Json;
use crate::{Read, Result, W};
use serde_json::Value;

mod repo;
pub use repo::*;

pub struct Repositories<'a, C>(&'a mut C);

pub struct PollTarget {
    pub id: String,
    pub ingest: String,
    pub last_delivery: Option<i64>,
    pub state: Value,
}

fn poll_target(r: &crate::contract::Row<'_>) -> Result<PollTarget> {
    Ok(PollTarget {
        id: r.get(0)?,
        ingest: r.get(1)?,
        last_delivery: r.get(2)?,
        state: r.get::<Json<Value>>(3)?.0,
    })
}

fn enabled_poll_targets() -> SelectStatement {
    Query::select()
        .columns(["id", "ingest", "last_delivery", "poll_state"])
        .from("gh_repos")
        .and_where(Expr::col("enabled").eq(1_i64))
        .take()
}

impl<'a, C: Read> Repositories<'a, C> {
    pub fn new(handle: &'a mut C) -> Self {
        Self(handle)
    }

    pub fn poll_state(&mut self, id: &str) -> Result<Option<Value>> {
        self.0.one(
            Query::select()
                .column("poll_state")
                .from("gh_repos")
                .and_where(Expr::col("id").eq(id)),
            |r| Ok(r.get::<Json<Value>>(0)?.0),
        )
    }

    pub fn poll_targets(&mut self) -> Result<Vec<PollTarget>> {
        self.0.all(
            enabled_poll_targets()
                .order_by_columns([("workspace_id", Order::Asc), ("id", Order::Asc)]),
            poll_target,
        )
    }

    pub fn poll_target(&mut self, id: &str) -> Result<Option<PollTarget>> {
        self.0.one(
            enabled_poll_targets().and_where(Expr::col("id").eq(id)),
            poll_target,
        )
    }

    pub fn has_open_linked_pr(&mut self, id: &str) -> Result<bool> {
        Ok(self
            .0
            .one(
                Query::select()
                    .column("id")
                    .from("gh_prs")
                    .and_where(Expr::col("repo_id").eq(id))
                    .and_where(Expr::col("state").eq("open"))
                    .and_where(Expr::col("item_id").is_not_null())
                    .limit(1),
                |_| Ok(()),
            )?
            .is_some())
    }
}

impl Repositories<'_, W<'_>> {
    pub fn set_poll_state(&mut self, id: &str, state: &Value) -> Result<()> {
        self.0.exec(
            Query::update()
                .table("gh_repos")
                .value("poll_state", state.to_string())
                .and_where(Expr::col("id").eq(id)),
        )?;
        Ok(())
    }
}

/// A delivery receipt, retained for deduplication after processing.
#[derive(Debug, Clone, PartialEq)]
pub struct DeliveryRow {
    pub delivery_id: String,
    pub workspace_id: Option<String>,
    pub repo_id: Option<String>,
    pub event: String,
    pub action: Option<String>,
    /// `received` (not processed yet) | `ok|ignored|failed`.
    pub status: String,
    pub error: Option<String>,
    pub received: i64,
}

/// Not processed yet: the worker (and the recovery on start) takes these.
pub const RECEIVED: &str = "received";

const DELIVERY_COLS: [&str; 8] = [
    "delivery_id",
    "workspace_id",
    "repo_id",
    "event",
    "action",
    "status",
    "error",
    "received",
];

/// The information needed to resume processing a delivery. Source and payload are JSON text.
pub struct DeliveryInput {
    pub delivery_id: String,
    pub event: String,
    pub source: String,
    pub payload: String,
}

fn due_deliveries(now: i64) -> SelectStatement {
    Query::select()
        .from_as("gh_deliveries", "d")
        .join_as(
            JoinType::InnerJoin,
            "gh_delivery_inputs",
            "i",
            Expr::col(("i", "delivery_id")).eq(Expr::col(("d", "delivery_id"))),
        )
        .and_where(Expr::col(("d", "status")).eq(RECEIVED))
        .and_where(Expr::col(("i", "next_at")).lte(now))
        .take()
}

pub struct Deliveries<'a, C>(&'a mut C);

impl<'a, C: Read> Deliveries<'a, C> {
    pub fn new(handle: &'a mut C) -> Self {
        Self(handle)
    }

    /// Deliveries due now, oldest first.
    pub fn received_deliveries(&mut self, now: i64, limit: u64) -> Result<Vec<DeliveryInput>> {
        let order = self.0.db().dialect().insertion_order(Some("d"));
        self.0.all(
            due_deliveries(now)
                .columns([
                    ("d", "delivery_id"),
                    ("d", "event"),
                    ("i", "source"),
                    ("i", "payload"),
                ])
                .order_by(("d", "received"), Order::Asc)
                .order_by_expr(order, Order::Asc)
                .limit(limit),
            |r| {
                Ok(DeliveryInput {
                    delivery_id: r.get(0)?,
                    event: r.get(1)?,
                    source: r.get(2)?,
                    payload: r.get(3)?,
                })
            },
        )
    }

    /// Rechecked after claiming the in-memory queue slot, since another worker may have
    /// processed the delivery since the recovery query.
    pub fn delivery_due(&mut self, id: &str, now: i64) -> Result<bool> {
        Ok(self
            .0
            .one(
                due_deliveries(now)
                    .column(("d", "delivery_id"))
                    .and_where(Expr::col(("d", "delivery_id")).eq(id))
                    .limit(1),
                |_| Ok(()),
            )?
            .is_some())
    }

    pub fn delivery_attempts(&mut self, id: &str) -> Result<i64> {
        Ok(self
            .0
            .one(
                Query::select()
                    .column("attempts")
                    .from("gh_delivery_inputs")
                    .and_where(Expr::col("delivery_id").eq(id)),
                |r| r.get(0),
            )?
            .unwrap_or(0))
    }

    pub fn is_received(&mut self, id: &str) -> Result<bool> {
        Ok(self
            .0
            .one(
                Query::select()
                    .column("delivery_id")
                    .from("gh_deliveries")
                    .and_where(Expr::col("delivery_id").eq(id))
                    .and_where(Expr::col("status").eq(RECEIVED)),
                |_| Ok(()),
            )?
            .is_some())
    }
}

impl Deliveries<'_, W<'_>> {
    /// Inserts the receipt and input for a new delivery. A redelivery returns `false`.
    pub fn insert_delivery(
        &mut self,
        d: &DeliveryRow,
        source: &str,
        payload: &str,
    ) -> Result<bool> {
        let n = self.0.exec(
            Query::insert()
                .into_table("gh_deliveries")
                .columns(DELIVERY_COLS)
                .values_panic([
                    d.delivery_id.as_str().into(),
                    d.workspace_id.as_deref().into(),
                    d.repo_id.as_deref().into(),
                    d.event.as_str().into(),
                    d.action.as_deref().into(),
                    d.status.as_str().into(),
                    d.error.as_deref().into(),
                    d.received.into(),
                ])
                .on_conflict(OnConflict::column("delivery_id").do_nothing().to_owned()),
        )?;
        if n == 1 {
            self.0.exec(
                Query::insert()
                    .into_table("gh_delivery_inputs")
                    .columns(["delivery_id", "source", "payload"])
                    .values_panic([d.delivery_id.as_str().into(), source.into(), payload.into()]),
            )?;
        }
        Ok(n == 1)
    }

    /// Records the failure and schedules another attempt.
    pub fn delivery_retry(&mut self, id: &str, error: &str, next_at: i64) -> Result<()> {
        self.0.exec(
            Query::update()
                .table("gh_delivery_inputs")
                .value("attempts", Expr::col("attempts").add(1_i64))
                .value("next_at", next_at)
                .and_where(Expr::col("delivery_id").eq(id)),
        )?;
        self.0.exec(
            Query::update()
                .table("gh_deliveries")
                .value("error", error)
                .and_where(Expr::col("delivery_id").eq(id)),
        )?;
        Ok(())
    }

    pub fn set_delivery_result(
        &mut self,
        id: &str,
        ws: Option<&str>,
        repo: Option<&str>,
        status: &str,
        error: Option<&str>,
    ) -> Result<()> {
        self.0.exec(
            Query::update()
                .table("gh_deliveries")
                .value(
                    "workspace_id",
                    Func::coalesce([Expr::val(ws), Expr::col("workspace_id")]),
                )
                .value(
                    "repo_id",
                    Func::coalesce([Expr::val(repo), Expr::col("repo_id")]),
                )
                .value("status", status)
                .value("error", error)
                .and_where(Expr::col("delivery_id").eq(id)),
        )?;
        // Keep the input for terminal failures, for diagnosis.
        if status != "failed" {
            self.0.exec(
                Query::delete()
                    .from_table("gh_delivery_inputs")
                    .and_where(Expr::col("delivery_id").eq(id)),
            )?;
        }
        Ok(())
    }

    /// Expires processed receipts when their dedupe window is over.
    pub fn prune_deliveries(&mut self, before: i64) -> Result<usize> {
        let retained_inputs = Query::select()
            .column("delivery_id")
            .from("gh_delivery_inputs")
            .take();
        Ok(self.0.exec(
            Query::delete()
                .from_table("gh_deliveries")
                .and_where(Expr::col("received").lt(before))
                .and_where(Expr::col("status").ne(RECEIVED))
                .and_where(Expr::col("delivery_id").not_in_subquery(retained_inputs)),
        )? as usize)
    }
}

#[cfg(feature = "testkit")]
pub mod fixtures;
