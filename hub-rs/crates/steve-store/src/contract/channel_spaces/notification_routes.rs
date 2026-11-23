//! Membership and identity selection for a private notification destination.

use sea_query::{Expr, ExprTrait, JoinType, Order, Query};

use crate::{Read, Result};

pub fn is_self_dm(r: &mut impl Read, ws: &str, party: &str, space: &str) -> Result<bool> {
    let bot = Query::select()
        .column("account_id")
        .from("connections")
        .and_where(Expr::col("id").equals(("s", "connection_id")))
        .take();
    let outside = Query::select()
        .column(("m", "account_id"))
        .from_as("space_members", "m")
        .join_as(
            JoinType::InnerJoin,
            "accounts",
            "a",
            Expr::col(("a", "id")).equals(("m", "account_id")),
        )
        .and_where(Expr::col(("m", "space_id")).equals(("s", "id")))
        .and_where(Expr::col(("a", "party_id")).ne(party))
        .and_where(Expr::col(("a", "id")).ne(Expr::from(sea_query::SubQueryStatement::from(bot))))
        .take();
    Ok(r.one(
        Query::select()
            .column(("s", "id"))
            .from_as("spaces", "s")
            .and_where(Expr::col(("s", "id")).eq(space))
            .and_where(Expr::col(("s", "workspace_id")).eq(ws))
            .and_where(Expr::col(("s", "kind")).eq("dm"))
            .and_where(Expr::exists(outside).not()),
        |_| Ok(()),
    )?
    .is_some())
}

#[derive(Debug, Clone, serde::Serialize, PartialEq, Eq)]
pub struct Target {
    pub party_id: String,
    pub space_id: String,
    pub party_name: String,
    pub space_name: String,
    pub channel: String,
}

pub fn targets(r: &mut impl Read, ws: &str, user: &str) -> Result<Vec<Target>> {
    r.all(
        Query::select()
            .distinct()
            .from_as("parties", "p")
            .columns([
                ("p", "id"),
                ("s", "id"),
                ("p", "name"),
                ("s", "name"),
                ("s", "system_id"),
            ])
            .join_as(
                JoinType::InnerJoin,
                "accounts",
                "a",
                Expr::col(("a", "party_id"))
                    .equals(("p", "id"))
                    .and(Expr::col(("a", "workspace_id")).equals(("p", "workspace_id"))),
            )
            .join_as(
                JoinType::InnerJoin,
                "space_members",
                "m",
                Expr::col(("m", "account_id"))
                    .equals(("a", "id"))
                    .and(Expr::col(("m", "workspace_id")).equals(("p", "workspace_id"))),
            )
            .join_as(
                JoinType::InnerJoin,
                "spaces",
                "s",
                Expr::col(("s", "id"))
                    .equals(("m", "space_id"))
                    .and(Expr::col(("s", "workspace_id")).equals(("p", "workspace_id"))),
            )
            .and_where(Expr::col(("p", "workspace_id")).eq(ws))
            .and_where(Expr::col(("p", "user_id")).eq(user))
            .and_where(Expr::col(("p", "status")).eq("active"))
            .and_where(
                Expr::col(("p", "contact_kind"))
                    .is_null()
                    .or(Expr::col(("p", "contact_kind")).ne("bot")),
            )
            .and_where(Expr::col(("p", "type")).ne("bot"))
            .and_where(Expr::col(("a", "status")).eq("active"))
            .and_where(Expr::col(("a", "kind")).is_not_in(["bot", "app_bot"]))
            .and_where(Expr::col(("s", "kind")).eq("dm"))
            .order_by_columns([(("p", "id"), Order::Asc), (("s", "id"), Order::Asc)]),
        |row| {
            Ok(Target {
                party_id: row.get(0)?,
                space_id: row.get(1)?,
                party_name: row.get(2)?,
                space_name: row.get(3)?,
                channel: row.get(4)?,
            })
        },
    )
}

/// An active bot can address only the user party and the bot itself in this DM.
pub fn bot_route(
    r: &mut impl Read,
    ws: &str,
    connection: &str,
    party: &str,
    space: &str,
) -> Result<bool> {
    let outsiders = Query::select()
        .column(("m", "account_id"))
        .from_as("space_members", "m")
        .join_as(
            JoinType::LeftJoin,
            "accounts",
            "a",
            Expr::col(("a", "id")).equals(("m", "account_id")),
        )
        .and_where(Expr::col(("m", "space_id")).eq(space))
        .and_where(
            Expr::col(("m", "workspace_id"))
                .ne(ws)
                .or(Expr::col(("a", "id")).is_null())
                .or(Expr::col(("a", "workspace_id")).ne(ws))
                .or(Expr::col(("a", "status")).ne("active"))
                .or(Expr::col(("a", "id")).ne(Expr::col(("bot", "id"))).and(
                    Expr::col(("a", "party_id"))
                        .ne(party)
                        .or(Expr::col(("a", "kind")).is_in(["bot", "app_bot"])),
                )),
        )
        .take();
    Ok(r.one(
        Query::select()
            .column(("c", "id"))
            .from_as("connections", "c")
            .join_as(
                JoinType::InnerJoin,
                "accounts",
                "bot",
                Expr::col(("bot", "id"))
                    .equals(("c", "account_id"))
                    .and(Expr::col(("bot", "workspace_id")).equals(("c", "workspace_id"))),
            )
            .join_as(
                JoinType::InnerJoin,
                "parties",
                "p",
                Expr::col(("p", "id"))
                    .equals(("bot", "party_id"))
                    .and(Expr::col(("p", "workspace_id")).equals(("c", "workspace_id"))),
            )
            .and_where(Expr::col(("c", "id")).eq(connection))
            .and_where(Expr::col(("c", "workspace_id")).eq(ws))
            .and_where(Expr::col(("bot", "status")).eq("active"))
            .and_where(Expr::col(("bot", "kind")).is_in(["bot", "app_bot"]))
            .and_where(Expr::col(("p", "status")).eq("active"))
            .and_where(Expr::col(("p", "user_id")).is_null())
            .and_where(Expr::col(("p", "id")).ne(party))
            .and_where(Expr::exists(outsiders).not()),
        |_| Ok(()),
    )?
    .is_some())
}
