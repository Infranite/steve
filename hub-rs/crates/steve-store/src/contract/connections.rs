use sea_query::{Expr, ExprTrait, Order, Query};

use crate::contract::{Json, Row};
use crate::{Read, Result};

pub struct Connections<'a, C>(&'a mut C);

/// Connection fields that determine provider routing and ownership.
pub struct ConnectionSource {
    pub id: String,
    pub workspace_id: String,
    pub account_id: String,
    pub adapter: String,
    pub status: String,
    pub caps: Vec<String>,
    pub created_by: String,
}

const COLUMNS: [&str; 7] = [
    "id",
    "workspace_id",
    "account_id",
    "adapter",
    "status",
    "caps",
    "created_by",
];

fn source(r: &Row<'_>) -> Result<ConnectionSource> {
    Ok(ConnectionSource {
        id: r.get(0)?,
        workspace_id: r.get(1)?,
        account_id: r.get(2)?,
        adapter: r.get(3)?,
        status: r.get(4)?,
        caps: r.get::<Json<Vec<String>>>(5)?.0,
        created_by: r.get(6)?,
    })
}

impl<'a, C: Read> Connections<'a, C> {
    pub fn new(handle: &'a mut C) -> Self {
        Self(handle)
    }

    pub fn source(&mut self, ws: &str, id: &str) -> Result<Option<ConnectionSource>> {
        self.0.one(
            Query::select()
                .columns(COLUMNS)
                .from("connections")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("id").eq(id)),
            source,
        )
    }

    /// Every live connection the provider jobs should consider.
    pub fn live_sources(&mut self) -> Result<Vec<ConnectionSource>> {
        self.0.all(
            Query::select()
                .columns(COLUMNS)
                .from("connections")
                .and_where(Expr::col("status").is_in(["ok", "error"]))
                .order_by_columns([("created", Order::Asc), ("id", Order::Asc)]),
            source,
        )
    }
}
