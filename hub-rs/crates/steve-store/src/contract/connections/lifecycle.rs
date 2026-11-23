//! Connection data used by the channel runtime.

use sea_query::{Expr, ExprTrait, Order, Query, SelectStatement};
use serde_json::Value;

use crate::contract::Json;
use crate::{Read, Result};

pub(crate) fn for_account(account: &str) -> SelectStatement {
    Query::select()
        .column("id")
        .from("connections")
        .and_where(Expr::col("account_id").eq(account))
        .take()
}

pub(crate) fn ids_for_account(r: &mut impl Read, account: &str) -> Result<Vec<String>> {
    r.all(for_account(account).order_by("id", Order::Asc), |row| {
        row.get(0)
    })
}

pub struct RuntimeConnection {
    pub ws: String,
    pub adapter: String,
    pub status: String,
    pub caps: Vec<String>,
    pub credential: Option<String>,
    pub config: Value,
}

pub fn has_enabled_cap(r: &mut impl Read, id: &str, cap: &str) -> Result<bool> {
    let caps: Option<String> = r.one(
        Query::select()
            .column("caps")
            .from("connections")
            .and_where(Expr::col("id").eq(id))
            .and_where(Expr::col("status").ne("revoked")),
        |row| row.get(0),
    )?;
    Ok(caps
        .and_then(|raw| serde_json::from_str::<Vec<String>>(&raw).ok())
        .is_some_and(|caps| caps.iter().any(|enabled| enabled == cap)))
}

pub fn is_live_account(r: &mut impl Read, ws: &str, conn: &str, account: &str) -> Result<bool> {
    Ok(r.one(
        Query::select()
            .column("id")
            .from("connections")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("id").eq(conn))
            .and_where(Expr::col("account_id").eq(account))
            .and_where(Expr::col("status").is_in(["ok", "error"])),
        |_| Ok(()),
    )?
    .is_some())
}

/// The configuration advertised by the channel adapter, when one was saved.
pub fn channel_config(r: &mut impl Read, id: &str) -> Result<Value> {
    let config: Option<String> = r.one(
        Query::select()
            .column("config")
            .from("connections")
            .and_where(Expr::col("id").eq(id)),
        |row| row.get(0),
    )?;
    Ok(config
        .and_then(|raw| serde_json::from_str(&raw).ok())
        .unwrap_or(Value::Null))
}

pub fn personal_user(r: &mut impl Read, ws: &str, connection: &str) -> Result<Option<String>> {
    let Some(owner) = super::owner(r, ws, connection)? else {
        return Ok(None);
    };
    let Some(party) = crate::accounts::owned_party(r, ws, &owner.account)? else {
        return Ok(None);
    };
    crate::parties::user_id(r, &party)
}

pub fn get(r: &mut impl Read, id: &str) -> Result<Option<RuntimeConnection>> {
    r.one(
        Query::select()
            .columns([
                "workspace_id",
                "adapter",
                "status",
                "caps",
                "credential_item",
                "config",
            ])
            .from("connections")
            .and_where(Expr::col("id").eq(id)),
        |row| {
            Ok(RuntimeConnection {
                ws: row.get(0)?,
                adapter: row.get(1)?,
                status: row.get(2)?,
                caps: row.get::<Json<_>>(3)?.0,
                credential: row.get(4)?,
                config: row.get::<Json<_>>(5)?.0,
            })
        },
    )
}

pub fn adapter_ids(r: &mut impl Read) -> Result<Vec<(String, String)>> {
    r.all(
        Query::select()
            .columns(["id", "adapter"])
            .from("connections")
            .order_by_columns([("created", Order::Asc), ("id", Order::Asc)]),
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
}

pub fn healthy_adapter_configs(
    r: &mut impl Read,
    ws: &str,
    ids: &[String],
) -> Result<Vec<(String, Value)>> {
    if ids.is_empty() {
        return Ok(vec![]);
    }
    let ids = r.db().dialect().text_array(ids)?;
    r.all(
        Query::select()
            .columns(["adapter", "config"])
            .from("connections")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("status").eq("ok"))
            .and_where(Expr::col("id").in_subquery(ids))
            .distinct(),
        |row| {
            let config: String = row.get(1)?;
            Ok((
                row.get(0)?,
                serde_json::from_str(&config).unwrap_or(Value::Null),
            ))
        },
    )
}

pub fn paired_to(r: &mut impl Read, ws: &str, id: &str, key: &'static str) -> Result<Vec<String>> {
    let paired = r.db().dialect().json_field(Expr::col("config"), key);
    r.all(
        Query::select()
            .column("id")
            .from("connections")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(paired.eq(id))
            .order_by_columns([("created", Order::Asc), ("id", Order::Asc)]),
        |row| row.get(0),
    )
}
