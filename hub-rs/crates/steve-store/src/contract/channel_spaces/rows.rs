//! The stored space and the identity that speaks for it.

use sea_query::{
    Expr, ExprTrait, Func, JoinType, LikeExpr, Order, Query, SelectStatement, SubQueryStatement,
};

use crate::contract::Row;
use crate::{Read, Result};

#[derive(Debug, Clone, PartialEq)]
pub struct Space {
    pub id: String,
    pub workspace_id: String,
    pub system_id: String,
    /// The active connection chosen to speak for the space.
    pub connection_id: String,
    pub kind: String,
    pub external_id: String,
    pub name: String,
    pub member_count: i64,
    pub has_external: bool,
    pub status: String,
    pub last_message: Option<i64>,
    pub created: i64,
    pub updated: i64,
    /// The connection the space was first seen through.
    pub via: String,
}

impl Space {
    pub fn people(&self) -> i64 {
        if self.kind == "dm" {
            1
        } else {
            self.member_count
        }
    }
}

pub fn get(r: &mut impl Read, id: &str) -> Result<Option<Space>> {
    r.one(select().and_where(Expr::col(("s", "id")).eq(id)), read)
}

/// Single-user space visibility is scoped to the workspace.
pub fn get_in_workspace(r: &mut impl Read, ws: &str, id: &str) -> Result<Option<Space>> {
    r.one(
        select()
            .and_where(Expr::col(("s", "workspace_id")).eq(ws))
            .and_where(Expr::col(("s", "id")).eq(id)),
        read,
    )
}

pub fn by_chat(r: &mut impl Read, ws: &str, external: &str) -> Result<Vec<Space>> {
    r.all(
        select()
            .and_where(Expr::col(("s", "workspace_id")).eq(ws))
            .and_where(Expr::col(("s", "external_id")).eq(external))
            .order_by_columns([(("s", "created"), Order::Asc), (("s", "id"), Order::Asc)]),
        read,
    )
}

pub fn chat_connections(r: &mut impl Read, ws: &str, external: &str) -> Result<Vec<String>> {
    r.all(
        Query::select()
            .distinct()
            .column(("i", "connection_id"))
            .from_as("space_identities", "i")
            .join_as(
                JoinType::InnerJoin,
                "spaces",
                "s",
                Expr::col(("s", "id")).eq(Expr::col(("i", "space_id"))),
            )
            .and_where(Expr::col(("s", "workspace_id")).eq(ws))
            .and_where(Expr::col(("s", "external_id")).eq(external)),
        |row| row.get(0),
    )
}

/// Active spaces still visible to this connection.
pub fn active_count(r: &mut impl Read, ws: &str, conn: &str) -> Result<i64> {
    let still_sees = super::identities()
        .and_where(Expr::col(("i", "connection_id")).eq(conn))
        .and_where(Expr::col(("i", "left_at")).is_null())
        .take();
    Ok(r.one(
        Query::select()
            .expr(Func::count(Expr::col(("s", "id"))))
            .from_as("spaces", "s")
            .and_where(Expr::col(("s", "workspace_id")).eq(ws))
            .and_where(Expr::col(("s", "status")).eq("active"))
            .and_where(Expr::exists(still_sees)),
        |row| row.get(0),
    )?
    .unwrap_or(0))
}

pub fn list(
    r: &mut impl Read,
    ws: &str,
    connection: Option<&str>,
    search: Option<&str>,
) -> Result<Vec<Space>> {
    let mut query = select();
    query.and_where(Expr::col(("s", "workspace_id")).eq(ws));
    if let Some(conn) = connection {
        let sees = super::identities()
            .and_where(Expr::col(("i", "connection_id")).eq(conn))
            .and_where(Expr::col(("i", "left_at")).is_null())
            .take();
        query.and_where(Expr::exists(sees));
    }
    if let Some(search) = search.map(str::trim).filter(|s| !s.is_empty()) {
        let escaped = search
            .replace('\\', "\\\\")
            .replace('%', "\\%")
            .replace('_', "\\_");
        query.and_where(
            Expr::col(("s", "name")).like(LikeExpr::new(format!("%{escaped}%")).escape('\\')),
        );
    }
    query
        .order_by_expr(
            Func::coalesce([
                Expr::col(("s", "last_message")),
                Expr::col(("s", "created")),
            ])
            .into(),
            Order::Desc,
        )
        .order_by(("s", "id"), Order::Asc);
    r.all(&query, read)
}

fn select() -> SelectStatement {
    Query::select()
        .from_as("spaces", "s")
        .columns([
            ("s", "id"),
            ("s", "workspace_id"),
            ("s", "system_id"),
            ("s", "kind"),
            ("s", "external_id"),
            ("s", "name"),
            ("s", "member_count"),
            ("s", "has_external"),
            ("s", "status"),
            ("s", "last_message"),
            ("s", "created"),
            ("s", "updated"),
            ("s", "connection_id"),
        ])
        .expr(voice_expr())
        .take()
}

fn read(row: &Row<'_>) -> Result<Space> {
    Ok(Space {
        id: row.get(0)?,
        workspace_id: row.get(1)?,
        system_id: row.get(2)?,
        kind: row.get(3)?,
        external_id: row.get(4)?,
        name: row.get(5)?,
        member_count: row.get(6)?,
        has_external: row.get::<i64>(7)? != 0,
        status: row.get(8)?,
        last_message: row.get(9)?,
        created: row.get(10)?,
        updated: row.get(11)?,
        via: row.get::<Option<String>>(12)?.unwrap_or_default(),
        connection_id: row.get::<Option<String>>(13)?.unwrap_or_default(),
    })
}

fn voice_expr() -> Expr {
    // A manually selected voice is usable while its connection still sees the space.
    let chosen: SubQueryStatement = Query::select()
        .column(("p", "voice"))
        .from_as("space_prefs", "p")
        .join_as(
            JoinType::InnerJoin,
            "space_identities",
            "i",
            Expr::col(("i", "space_id"))
                .eq(Expr::col(("p", "space_id")))
                .and(Expr::col(("i", "connection_id")).eq(Expr::col(("p", "voice"))))
                .and(Expr::col(("i", "left_at")).is_null()),
        )
        .join_as(
            JoinType::InnerJoin,
            "connections",
            "c",
            Expr::col(("c", "id")).eq(Expr::col(("i", "connection_id"))),
        )
        .and_where(Expr::col(("p", "space_id")).eq(Expr::col(("s", "id"))))
        .take()
        .into();

    // Automatically choose a bot before a personal account, then the first identity to join.
    let first: SubQueryStatement = Query::select()
        .column(("i", "connection_id"))
        .from_as("space_identities", "i")
        .join_as(
            JoinType::InnerJoin,
            "connections",
            "c",
            Expr::col(("c", "id")).eq(Expr::col(("i", "connection_id"))),
        )
        .join_as(
            JoinType::InnerJoin,
            "accounts",
            "a",
            Expr::col(("a", "id")).eq(Expr::col(("c", "account_id"))),
        )
        .join_as(
            JoinType::LeftJoin,
            "parties",
            "p",
            Expr::col(("p", "id")).eq(Expr::col(("a", "party_id"))),
        )
        .and_where(Expr::col(("i", "space_id")).eq(Expr::col(("s", "id"))))
        .and_where(Expr::col(("i", "left_at")).is_null())
        .order_by_expr(Expr::col(("p", "user_id")).is_not_null(), Order::Asc)
        .order_by_columns([
            (("i", "joined"), Order::Asc),
            (("i", "connection_id"), Order::Asc),
        ])
        .limit(1)
        .take()
        .into();

    Func::coalesce([
        Expr::from(chosen),
        Expr::from(first),
        Expr::col(("s", "connection_id")),
    ])
    .into()
}

/// Readers that have a recorded identity for this space and a usable connection.
pub fn history_readers(
    r: &mut impl Read,
    ws: &str,
    user: &str,
    space: &Space,
) -> Result<Vec<(String, String)>> {
    let mut ids = super::active_connections(r, &space.id)?;
    if !space.via.is_empty() {
        ids.push(space.via.clone());
    }
    if ids.is_empty() {
        return Ok(vec![]);
    }
    let ids = r.db().dialect().text_array(&ids)?;
    r.all(
        Query::select()
            .columns([("c", "id"), ("c", "adapter")])
            .from_as("connections", "c")
            .join_as(
                JoinType::InnerJoin,
                "accounts",
                "a",
                Expr::col(("a", "id")).equals(("c", "account_id")),
            )
            .join_as(
                JoinType::LeftJoin,
                "parties",
                "p",
                Expr::col(("p", "id")).equals(("a", "party_id")),
            )
            .and_where(Expr::col(("c", "id")).in_subquery(ids))
            .and_where(Expr::col(("c", "workspace_id")).eq(ws))
            .and_where(Expr::col(("c", "status")).is_in(["ok", "error"]))
            .and_where(
                Expr::col(("p", "user_id"))
                    .is_null()
                    .or(Expr::col(("p", "user_id")).eq(user)),
            )
            .order_by_columns([(("c", "created"), Order::Asc), (("c", "id"), Order::Asc)]),
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
}
