//! Bindings of personas to accounts and spaces.

use sea_query::{Expr, ExprTrait, OnConflict, Order, Query};
use serde::Serialize;

use crate::contract::Q;
use crate::{Read, Result, W, connections};

pub fn bound_id(r: &mut impl Read, ws: &str, account: &str, space: &str) -> Result<Option<String>> {
    r.one(
        Query::select()
            .column("persona_id")
            .from("persona_bindings")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("account_id").eq(account))
            .and_where(Expr::col("space_id").eq(space)),
        |row| row.get(0),
    )
}

pub fn bind(
    w: &mut W<'_>,
    ws: &str,
    account: &str,
    space: &str,
    persona: &str,
    now: i64,
) -> Result<()> {
    w.exec(
        Query::insert()
            .into_table("persona_bindings")
            .columns([
                "workspace_id",
                "account_id",
                "space_id",
                "persona_id",
                "created",
            ])
            .values_panic([
                ws.into(),
                account.into(),
                space.into(),
                persona.into(),
                now.into(),
            ])
            .on_conflict(
                OnConflict::columns(["workspace_id", "account_id", "space_id"])
                    .update_columns(["persona_id", "created"])
                    .to_owned(),
            ),
    )?;
    Ok(())
}

pub fn clear(w: &mut W<'_>, ws: &str, account: &str, space: &str) -> Result<()> {
    w.exec(
        Query::delete()
            .from_table("persona_bindings")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("account_id").eq(account))
            .and_where(Expr::col("space_id").eq(space)),
    )?;
    Ok(())
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Moved {
    pub accounts: usize,
    pub spaces: usize,
}

/// Switch account defaults to the workspace's fallback and clear specific space bindings.
pub fn release(w: &mut W<'_>, ws: &str, persona: &str, fallback: &str, now: i64) -> Result<Moved> {
    let bindings: Vec<(String, String)> = w.all(
        Query::select()
            .columns(["space_id", "account_id"])
            .from("persona_bindings")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("persona_id").eq(persona))
            .order_by("space_id", Order::Asc),
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let mut moved = Moved::default();
    for (space, account) in bindings {
        let ids = connections::lifecycle::ids_for_account(w, &account)?;
        if space.is_empty() {
            moved.accounts += ids.len().max(1);
            for id in ids {
                w.emit(steve_types::ChannelsEvent::Channel {
                    id: id.into(),
                    ws: ws.into(),
                });
            }
        } else {
            let count = ids.len().max(1);
            moved.spaces += count;
            for _ in 0..count {
                w.emit(steve_types::ChannelsEvent::Space {
                    id: space.clone().into(),
                    ws: ws.into(),
                });
            }
        }
    }
    w.exec(
        Query::update()
            .table("persona_bindings")
            .value("persona_id", fallback)
            .value("created", now)
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("persona_id").eq(persona))
            .and_where(Expr::col("space_id").eq("")),
    )?;
    w.exec(
        Query::delete()
            .from_table("persona_bindings")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("persona_id").eq(persona))
            .and_where(Expr::col("space_id").ne("")),
    )?;
    Ok(moved)
}
