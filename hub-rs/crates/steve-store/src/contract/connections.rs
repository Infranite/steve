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

pub struct WatchedConnection {
    pub id: String,
    pub workspace_id: String,
    pub adapter: String,
    pub status: String,
    pub created_by: String,
}

pub fn watched(c: &mut impl Read) -> Result<Vec<WatchedConnection>> {
    c.all(
        Query::select()
            .columns(["id", "workspace_id", "adapter", "status", "created_by"])
            .from("connections")
            .order_by_columns([("created", Order::Asc), ("id", Order::Asc)]),
        |r| {
            Ok(WatchedConnection {
                id: r.get(0)?,
                workspace_id: r.get(1)?,
                adapter: r.get(2)?,
                status: r.get(3)?,
                created_by: r.get(4)?,
            })
        },
    )
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

/// The `github_app` connection of an App in a workspace, with its installation id.
pub fn app_connection(c: &mut impl Read, ws: &str, app_id: i64) -> Result<Option<(String, i64)>> {
    let dialect = c.db().dialect();
    c.one(
        Query::select()
            .column("id")
            .expr(dialect.json_integer(Expr::col("config"), "installation_id"))
            .from("connections")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("adapter").eq("github_app"))
            .and_where(
                dialect
                    .json_integer(Expr::col("config"), "app_id")
                    .eq(app_id),
            )
            .order_by("created", Order::Asc)
            .limit(1),
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
}

/// Every `github_app` connection of an installation in a workspace (normally one).
pub fn installation_connections(
    c: &mut impl Read,
    ws: &str,
    installation_id: i64,
) -> Result<Vec<String>> {
    let dialect = c.db().dialect();
    c.all(
        Query::select()
            .columns(["id"])
            .from("connections")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("adapter").eq("github_app"))
            .and_where(
                dialect
                    .json_integer(Expr::col("config"), "installation_id")
                    .eq(installation_id),
            )
            .order_by("created", Order::Asc),
        |r| r.get(0),
    )
}

/// `(workspace, adapter)` of a connection.
pub fn connection_kind(c: &mut impl Read, id: &str) -> Result<Option<(String, String)>> {
    c.one(
        Query::select()
            .columns(["workspace_id", "adapter"])
            .from("connections")
            .and_where(Expr::col("id").eq(id)),
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
}

/// The adapter, account label and status displayed for a connection.
pub fn connection_label(c: &mut impl Read, id: &str) -> Result<Option<(String, String, String)>> {
    let fields: Option<(String, String, String)> = c.one(
        Query::select()
            .columns(["adapter", "account_id", "status"])
            .from("connections")
            .and_where(Expr::col("id").eq(id)),
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )?;
    let Some((adapter, account, status)) = fields else {
        return Ok(None);
    };
    let handle = super::accounts::display_handle(c, &account)?.unwrap_or_default();
    Ok(Some((adapter, handle, status)))
}

/// The App id of an installation, from any workspace's `github_app` connection.
pub fn app_of_installation(c: &mut impl Read, installation_id: i64) -> Result<Option<i64>> {
    let dialect = c.db().dialect();
    c.one(
        Query::select()
            .expr(dialect.json_integer(Expr::col("config"), "app_id"))
            .from("connections")
            .and_where(Expr::col("adapter").eq("github_app"))
            .and_where(
                dialect
                    .json_integer(Expr::col("config"), "installation_id")
                    .eq(installation_id),
            )
            .order_by("created", Order::Asc)
            .limit(1),
        |r| r.get(0),
    )
}

pub fn app_connections(c: &mut impl Read, ws: &str, app_id: i64) -> Result<Vec<String>> {
    let dialect = c.db().dialect();
    c.all(
        Query::select()
            .columns(["id"])
            .from("connections")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("adapter").eq("github_app"))
            .and_where(
                dialect
                    .json_integer(Expr::col("config"), "app_id")
                    .eq(app_id),
            ),
        |r| r.get(0),
    )
}

pub const WAITING: [&str; 2] = ["paused", "reauth"];

pub fn waiting_ids(c: &mut impl Read) -> Result<Vec<String>> {
    c.all(
        Query::select()
            .column("id")
            .from("connections")
            .and_where(Expr::col("status").is_in(WAITING)),
        |r| r.get(0),
    )
}

pub fn address(c: &mut impl Read, id: &str) -> Result<Option<String>> {
    Ok(config(c, id)?.and_then(|v| v["address"].as_str().map(String::from)))
}

pub fn config(c: &mut impl Read, id: &str) -> Result<Option<serde_json::Value>> {
    c.one(
        Query::select()
            .column("config")
            .from("connections")
            .and_where(Expr::col("id").eq(id)),
        |r| Ok(r.get::<Json<serde_json::Value>>(0)?.0),
    )
}

pub fn workspace_sources(c: &mut impl Read, ws: &str) -> Result<Vec<ConnectionSource>> {
    c.all(
        Query::select()
            .columns(COLUMNS)
            .from("connections")
            .and_where(Expr::col("workspace_id").eq(ws))
            .order_by_columns([("created", Order::Asc), ("id", Order::Asc)]),
        source,
    )
}
