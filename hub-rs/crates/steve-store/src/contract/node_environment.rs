//! Environment rows owned by a principal on a Node.

use sea_query::{Expr, ExprTrait, OnConflict, Order, Query};
use serde::Serialize;

use crate::{Read, Result, W};

const TABLE: &str = "node_env";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Row {
    pub key: String,
    pub value: Option<String>,
    pub vault_ref: Option<String>,
    pub note: String,
    pub updated_by: String,
    pub updated: i64,
}

fn for_principal(node: &str, principal: &str) -> Expr {
    Expr::col("node_id")
        .eq(node)
        .and(Expr::col("principal").eq(principal))
}

/// The owner used for a local run's environment. The identifier breaks equal creation times.
pub fn local_owner(c: &mut impl Read) -> Result<Option<String>> {
    c.one(
        Query::select()
            .column("id")
            .from("users")
            .and_where(Expr::col("role").eq("owner"))
            .order_by_columns([("created", Order::Asc), ("id", Order::Asc)])
            .limit(1),
        |r| r.get(0),
    )
}

pub fn has_other_member(c: &mut impl Read, ws: &str, user: &str) -> Result<bool> {
    Ok(c.one(
        Query::select()
            .column("user_id")
            .from("workspace_members")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("user_id").ne(user))
            .limit(1),
        |_| Ok(()),
    )?
    .is_some())
}

pub fn has_multiple_users(c: &mut impl Read) -> Result<bool> {
    Ok(c.one(
        Query::select()
            .column("id")
            .from("users")
            .limit(1)
            .offset(1),
        |_| Ok(()),
    )?
    .is_some())
}

pub fn values(c: &mut impl Read, node: &str, principal: &str) -> Result<Vec<(String, String)>> {
    c.all(
        Query::select()
            .columns(["key", "value"])
            .from(TABLE)
            .and_where(for_principal(node, principal))
            .and_where(Expr::col("value").is_not_null())
            .order_by("key", Order::Asc),
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
}

pub fn list(c: &mut impl Read, node: &str, principal: &str) -> Result<Vec<Row>> {
    c.all(
        Query::select()
            .columns(["key", "value", "vault_ref", "note", "updated_by", "updated"])
            .from(TABLE)
            .and_where(for_principal(node, principal))
            .order_by("key", Order::Asc),
        |r| {
            Ok(Row {
                key: r.get(0)?,
                value: r.get(1)?,
                vault_ref: r.get(2)?,
                note: r.get(3)?,
                updated_by: r.get(4)?,
                updated: r.get(5)?,
            })
        },
    )
}

pub struct Change<'a> {
    pub node: &'a str,
    pub principal: &'a str,
    pub key: &'a str,
    pub value: &'a str,
    pub note: &'a str,
    pub actor: &'a str,
}

pub fn put(w: &mut W<'_>, change: Change<'_>, at: i64) -> Result<()> {
    w.exec(
        Query::insert()
            .into_table(TABLE)
            .columns([
                "node_id",
                "principal",
                "key",
                "value",
                "note",
                "updated_by",
                "updated",
            ])
            .values_panic([
                change.node.into(),
                change.principal.into(),
                change.key.into(),
                change.value.into(),
                change.note.into(),
                change.actor.into(),
                at.into(),
            ])
            .on_conflict(
                OnConflict::columns(["node_id", "principal", "key"])
                    .update_columns(["value", "note", "updated_by", "updated"])
                    .value("vault_ref", Expr::val(None::<String>))
                    .to_owned(),
            ),
    )?;
    Ok(())
}

pub fn delete(w: &mut W<'_>, node: &str, principal: &str, key: &str) -> Result<bool> {
    Ok(w.exec(
        Query::delete()
            .from_table(TABLE)
            .and_where(for_principal(node, principal))
            .and_where(Expr::col("key").eq(key)),
    )? > 0)
}
