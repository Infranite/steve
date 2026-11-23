//! Connection membership in the channel spaces a workspace retains.

use sea_query::{Expr, ExprTrait, Func, JoinType, Order, Query, SelectStatement};

use crate::{Read, Result, W};

pub mod account_settings;
pub mod activity;
pub mod cleanup;
pub mod conversations;
pub mod direct;
pub mod members;
pub mod notification_routes;
pub mod preferences;
pub mod registration;
pub mod rows;

pub struct PlatformChat {
    pub ws: String,
    pub external_id: String,
}

pub fn platform_chat(r: &mut impl Read, id: &str) -> Result<Option<PlatformChat>> {
    r.one(
        Query::select()
            .columns(["workspace_id", "external_id"])
            .from("spaces")
            .and_where(Expr::col("id").eq(id)),
        |row| {
            Ok(PlatformChat {
                ws: row.get(0)?,
                external_id: row.get(1)?,
            })
        },
    )
}

fn identities() -> SelectStatement {
    Query::select()
        .column(("i", "space_id"))
        .from_as("space_identities", "i")
        .and_where(Expr::col(("i", "space_id")).eq(Expr::col(("s", "id"))))
        .take()
}

pub fn active_connections(r: &mut impl Read, space: &str) -> Result<Vec<String>> {
    r.all(
        Query::select()
            .column("connection_id")
            .from("space_identities")
            .and_where(Expr::col("space_id").eq(space))
            .and_where(Expr::col("left_at").is_null()),
        |row| row.get(0),
    )
}

pub fn is_active_connection(r: &mut impl Read, space: &str, connection: &str) -> Result<bool> {
    Ok(r.one(
        Query::select()
            .column("connection_id")
            .from("space_identities")
            .and_where(Expr::col("space_id").eq(space))
            .and_where(Expr::col("connection_id").eq(connection))
            .and_where(Expr::col("left_at").is_null()),
        |_| Ok(()),
    )?
    .is_some())
}

pub fn identity_bit(r: &mut impl Read, space: &str, connection: &str) -> Result<i64> {
    let ordinal: Option<i64> = r
        .one(
            Query::select()
                .column("ordinal")
                .from("space_identities")
                .and_where(Expr::col("space_id").eq(space))
                .and_where(Expr::col("connection_id").eq(connection)),
            |row| row.get(0),
        )?
        .flatten();
    Ok(ordinal.map_or(0, bit))
}

pub(crate) fn account_identity_mask(r: &mut impl Read, space: &str, account: &str) -> Result<i64> {
    let ordinals: Vec<i64> = r.all(
        Query::select()
            .column("ordinal")
            .from("space_identities")
            .and_where(Expr::col("space_id").eq(space))
            .and_where(Expr::col("ordinal").is_not_null())
            .and_where(
                Expr::col("connection_id")
                    .in_subquery(super::connections::lifecycle::for_account(account)),
            ),
        |row| row.get(0),
    )?;
    Ok(ordinals
        .into_iter()
        .fold(0, |mask, ordinal| mask | bit(ordinal)))
}

fn bit(ordinal: i64) -> i64 {
    u32::try_from(ordinal)
        .ok()
        .and_then(|shift| 1i64.checked_shl(shift))
        .unwrap_or_default()
}

/// Updates the last activity without moving it backwards. An external sender marks the space.
pub fn touch(w: &mut W<'_>, id: &str, at: i64, external: bool) -> Result<()> {
    use crate::contract::Q;

    let last = w.db().dialect().greatest(
        Func::coalesce([Expr::col("last_message"), Expr::cust("0")]).into(),
        Expr::val(at),
    );
    let mut update = Query::update();
    update
        .table("spaces")
        .value("last_message", last)
        .and_where(Expr::col("id").eq(id));
    if external {
        update.value("has_external", 1i64);
    }
    w.exec(&update)?;
    Ok(())
}

pub fn active_chat_connections(r: &mut impl Read, ws: &str, chat: &str) -> Result<Vec<String>> {
    r.all(
        Query::select()
            .column(("i", "connection_id"))
            .from_as("spaces", "s")
            .join_as(
                JoinType::InnerJoin,
                "space_identities",
                "i",
                Expr::col(("i", "space_id")).eq(Expr::col(("s", "id"))),
            )
            .and_where(Expr::col(("s", "workspace_id")).eq(ws))
            .and_where(Expr::col(("s", "external_id")).eq(chat))
            .and_where(Expr::col(("s", "status")).eq("active"))
            .and_where(Expr::col(("i", "left_at")).is_null())
            .distinct(),
        |row| row.get(0),
    )
}

pub fn active_for_connection(r: &mut impl Read, ws: &str, conn: &str) -> Result<i64> {
    let active_identity = identities()
        .and_where(Expr::col(("i", "connection_id")).eq(conn))
        .and_where(Expr::col(("i", "left_at")).is_null())
        .take();
    let original = Expr::col(("s", "connection_id"))
        .eq(conn)
        .and(Expr::exists(identities()).not());
    Ok(r.one(
        Query::select()
            .expr(Expr::cust("count(*)"))
            .from_as("spaces", "s")
            .and_where(Expr::col(("s", "workspace_id")).eq(ws))
            .and_where(Expr::col(("s", "status")).eq("active"))
            .and_where(Expr::exists(active_identity).or(original)),
        |row| row.get(0),
    )?
    .unwrap_or_default())
}

pub fn recent_visible_chats(
    r: &mut impl Read,
    ws: &str,
    connection: &str,
    limit: u64,
) -> Result<Vec<String>> {
    r.all(
        Query::select()
            .column(("s", "external_id"))
            .from_as("spaces", "s")
            .join_as(
                sea_query::JoinType::InnerJoin,
                "space_identities",
                "i",
                Expr::col(("i", "space_id"))
                    .eq(Expr::col(("s", "id")))
                    .and(Expr::col(("i", "left_at")).is_null()),
            )
            .and_where(Expr::col(("i", "connection_id")).eq(connection))
            .and_where(Expr::col(("s", "workspace_id")).eq(ws))
            .and_where(Expr::col(("s", "status")).eq("active"))
            .group_by_col(("s", "external_id"))
            .order_by_expr(Expr::col(("s", "updated")).max(), Order::Desc)
            .limit(limit),
        |row| row.get(0),
    )
}

pub fn active_chats(r: &mut impl Read, ws: &str, connections: &[String]) -> Result<Vec<String>> {
    if connections.is_empty() {
        return Ok(vec![]);
    }
    r.all(
        spaces_with_identities(ws, connections)
            .column(("s", "external_id"))
            .and_where(Expr::col(("s", "status")).eq("active"))
            .and_where(Expr::col(("i", "left_at")).is_null())
            .distinct(),
        |row| row.get(0),
    )
}

pub fn spaces_for_chat(
    r: &mut impl Read,
    ws: &str,
    external_id: &str,
    connections: &[String],
) -> Result<Vec<String>> {
    if connections.is_empty() {
        return Ok(vec![]);
    }
    r.all(
        spaces_with_identities(ws, connections)
            .column(("s", "id"))
            .and_where(Expr::col(("s", "external_id")).eq(external_id))
            .distinct(),
        |row| row.get(0),
    )
}

fn spaces_with_identities(ws: &str, connections: &[String]) -> SelectStatement {
    Query::select()
        .from_as("spaces", "s")
        .join_as(
            JoinType::InnerJoin,
            "space_identities",
            "i",
            Expr::col(("i", "space_id")).eq(Expr::col(("s", "id"))),
        )
        .and_where(Expr::col(("s", "workspace_id")).eq(ws))
        .and_where(Expr::col(("i", "connection_id")).is_in(connections.iter().map(String::as_str)))
        .take()
}

fn affected(ws: &str, conn: &str) -> SelectStatement {
    let from_identity = identities()
        .and_where(Expr::col(("i", "connection_id")).eq(conn))
        .take();
    Query::select()
        .column(("s", "id"))
        .from_as("spaces", "s")
        .and_where(Expr::col(("s", "workspace_id")).eq(ws))
        .and_where(
            Expr::col(("s", "connection_id"))
                .eq(conn)
                .or(Expr::exists(from_identity)),
        )
        .take()
}

pub fn disconnect(w: &mut W<'_>, ws: &str, conn: &str, now: i64) -> Result<()> {
    use crate::contract::Q;

    let account = super::connections::owner(w, ws, conn)?.map(|owner| owner.account);
    let affected = affected(ws, conn);
    let ids: Vec<String> = w.all(affected.clone().order_by(("s", "id"), Order::Asc), |row| {
        row.get(0)
    })?;
    w.exec(
        Query::update()
            .table("space_identities")
            .value("left_at", now)
            .and_where(Expr::col("connection_id").eq(conn))
            .and_where(Expr::col("left_at").is_null()),
    )?;
    let remaining = identities()
        .and_where(Expr::col(("i", "left_at")).is_null())
        .take();
    let mut abandoned = affected;
    abandoned.and_where(Expr::exists(remaining).not());

    for table in ["space_settings", "space_prefs"] {
        w.exec(
            Query::delete()
                .from_table(table)
                .and_where(Expr::col("space_id").in_subquery(abandoned.clone())),
        )?;
    }
    w.exec(
        Query::update()
            .table("spaces")
            .value(
                "status",
                Expr::case(Expr::col("status").eq("archived"), "archived").finally("left"),
            )
            .value(
                "account_id",
                Func::coalesce([Expr::col("account_id"), Expr::val(account)]),
            )
            .value("updated", now)
            .and_where(Expr::col("id").in_subquery(abandoned)),
    )?;
    for id in ids {
        w.emit(steve_types::ChannelsEvent::Space {
            ws: ws.into(),
            id: id.into(),
        });
    }
    super::channel_positions::remove_connection(w, conn)?;
    w.exec(
        Query::delete()
            .from_table("channel_settings")
            .and_where(Expr::col("connection_id").eq(conn)),
    )?;
    Ok(())
}

pub fn active_connection_for_account(
    r: &mut impl Read,
    space: &str,
    account: &str,
) -> Result<Option<String>> {
    r.one(
        Query::select()
            .column(("i", "connection_id"))
            .from_as("space_identities", "i")
            .join_as(
                JoinType::InnerJoin,
                "connections",
                "c",
                Expr::col(("c", "id")).equals(("i", "connection_id")),
            )
            .and_where(Expr::col(("i", "space_id")).eq(space))
            .and_where(Expr::col(("c", "account_id")).eq(account))
            .and_where(Expr::col(("i", "left_at")).is_null())
            .order_by(("i", "joined"), Order::Asc)
            .limit(1),
        |row| row.get(0),
    )
}
