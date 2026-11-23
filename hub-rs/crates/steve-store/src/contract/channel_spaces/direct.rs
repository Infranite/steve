//! Accounts used to identify a direct conversation's counterpart.

use sea_query::{Expr, ExprTrait, Func, JoinType, Order, Query};

use crate::{Read, Result};

pub fn original_account(r: &mut impl Read, space: &str) -> Result<Option<String>> {
    let connection = Query::select()
        .column("account_id")
        .from("connections")
        .and_where(Expr::col("id").equals(("s", "connection_id")))
        .take();
    Ok(r.one(
        Query::select()
            .expr(Func::coalesce([
                Expr::from(sea_query::SubQueryStatement::from(connection)),
                Expr::col(("s", "account_id")),
            ]))
            .from_as("spaces", "s")
            .and_where(Expr::col(("s", "id")).eq(space)),
        |row| row.get(0),
    )?
    .flatten())
}

pub fn identity_accounts(r: &mut impl Read, space: &str) -> Result<Vec<String>> {
    r.all(
        Query::select()
            .column(("c", "account_id"))
            .from_as("space_identities", "i")
            .join_as(
                JoinType::InnerJoin,
                "connections",
                "c",
                Expr::col(("c", "id")).equals(("i", "connection_id")),
            )
            .join_as(
                JoinType::InnerJoin,
                "accounts",
                "a",
                Expr::col(("a", "id")).equals(("c", "account_id")),
            )
            .and_where(Expr::col(("i", "space_id")).eq(space))
            .and_where(Expr::col(("i", "left_at")).is_null()),
        |row| row.get(0),
    )
}

pub fn other_member(r: &mut impl Read, space: &str, excluded: &[String]) -> Result<Option<String>> {
    let excluded = r.db().dialect().text_array(excluded)?;
    r.one(
        Query::select()
            .column("account_id")
            .from("space_members")
            .and_where(Expr::col("space_id").eq(space))
            .and_where(Expr::col("account_id").not_in_subquery(excluded))
            .order_by_expr(Expr::col("role").eq(Expr::cust("'bot'")), Order::Asc)
            .order_by("joined", Order::Asc)
            .limit(1),
        |row| row.get(0),
    )
}
