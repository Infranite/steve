use sea_query::{Expr, ExprTrait, Func, JoinType, LikeExpr, Order, Query, SelectStatement};

use crate::{Read, Result};

pub struct Accounts<'a, C>(&'a mut C);

impl<'a, C: Read> Accounts<'a, C> {
    pub fn new(handle: &'a mut C) -> Self {
        Self(handle)
    }

    pub fn party(&mut self, id: &str) -> Result<Option<String>> {
        self.0.one(
            Query::select()
                .column("party_id")
                .from("accounts")
                .and_where(Expr::col("id").eq(id)),
            |r| r.get(0),
        )
    }

    pub fn party_by_email(&mut self, ws: &str, email: &str) -> Result<Option<String>> {
        self.0.one(
            Query::select()
                .column(("accounts", "party_id"))
                .from("account_handles")
                .join(
                    JoinType::InnerJoin,
                    "accounts",
                    Expr::col(("accounts", "id")).eq(Expr::col(("account_handles", "account_id"))),
                )
                .and_where(Expr::col(("account_handles", "workspace_id")).eq(ws))
                .and_where(Expr::col(("account_handles", "family")).eq("email"))
                .and_where(Expr::col(("account_handles", "handle")).eq(email))
                .and_where(Expr::col(("accounts", "status")).eq("active"))
                .limit(1),
            |r| r.get(0),
        )
    }
}

/// Is `login` the GitHub account of one of the workspace's users (“你”)?
pub fn is_user_handle(c: &mut impl Read, ws: &str, login: &str) -> Result<bool> {
    c.one(
        Query::select()
            .expr(Expr::cust("1"))
            .from_as("accounts", "a")
            .join_as(
                JoinType::InnerJoin,
                "parties",
                "p",
                Expr::col(("p", "id")).eq(Expr::col(("a", "party_id"))),
            )
            .and_where(Expr::col(("a", "workspace_id")).eq(ws))
            .and_where(Expr::col(("a", "family")).eq("github_user"))
            .and_where(Expr::col(("a", "status")).eq("active"))
            .and_where(
                Expr::from(Func::lower(Expr::col(("a", "handle"))))
                    .eq(Expr::from(Func::lower(login))),
            )
            .and_where(Expr::col(("p", "user_id")).is_not_null())
            .limit(1),
        |_| Ok(()),
    )
    .map(|row| row.is_some())
}

pub(crate) fn display_handle(c: &mut impl Read, id: &str) -> Result<Option<String>> {
    c.one(
        Query::select()
            .column("handle_display")
            .from("accounts")
            .and_where(Expr::col("id").eq(id)),
        |row| row.get(0),
    )
}

pub(crate) fn sender_name(account: Expr) -> SelectStatement {
    Query::select()
        .expr(Func::coalesce([
            Expr::col("display_name"),
            Expr::col("handle_display"),
        ]))
        .from("accounts")
        .and_where(Expr::col("id").eq(account))
        .take()
}

pub fn display_name(c: &mut impl Read, id: &str) -> Result<Option<String>> {
    c.one(
        Query::select()
            .column("display_name")
            .from("accounts")
            .and_where(Expr::col("id").eq(id)),
        |row| row.get(0),
    )
}

pub fn names_and_handles(r: &mut impl Read, ids: &[&str]) -> Result<Vec<(String, String)>> {
    if ids.is_empty() {
        return Ok(vec![]);
    }
    let ids = r.db().dialect().text_array(ids)?;
    r.all(
        Query::select()
            .columns(["id", "display_name", "handle_display"])
            .from("accounts")
            .and_where(Expr::col("id").in_subquery(ids)),
        |row| {
            let name: String = row.get(1)?;
            Ok((
                row.get(0)?,
                if name.is_empty() { row.get(2)? } else { name },
            ))
        },
    )
}

pub(crate) fn bot_ids(r: &mut impl Read, ids: &[&str]) -> Result<Vec<String>> {
    if ids.is_empty() {
        return Ok(vec![]);
    }
    let ids = r.db().dialect().text_array(ids)?;
    r.all(
        Query::select()
            .column("id")
            .from("accounts")
            .and_where(Expr::col("id").in_subquery(ids))
            .and_where(Expr::col("kind").is_in(["bot", "app_bot"])),
        |row| row.get(0),
    )
}

pub fn system_handle(c: &mut impl Read, id: &str) -> Result<Option<(String, String)>> {
    c.one(
        Query::select()
            .columns(["system_id", "handle"])
            .from("accounts")
            .and_where(Expr::col("id").eq(id)),
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
}

pub fn by_email(c: &mut impl Read, ws: &str, handle: &str) -> Result<Option<String>> {
    c.one(
        Query::select()
            .column("account_id")
            .from("account_handles")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("family").eq("email"))
            .and_where(Expr::col("handle").eq(handle))
            .limit(1),
        |r| r.get(0),
    )
}

pub fn by_any_handle(c: &mut impl Read, ws: &str, handle: &str) -> Result<Option<String>> {
    c.one(
        Query::select()
            .column("account_id")
            .from("account_handles")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("handle").eq(handle))
            .limit(1),
        |row| row.get(0),
    )
}

pub fn owned_party(c: &mut impl Read, ws: &str, id: &str) -> Result<Option<String>> {
    c.one(
        Query::select()
            .column("party_id")
            .from("accounts")
            .and_where(Expr::col("id").eq(id))
            .and_where(Expr::col("workspace_id").eq(ws)),
        |r| r.get(0),
    )
}

/// The earliest handle of `account` matching `prefix` (an adapter-specific handle family prefix).
pub fn scoped_handle(r: &mut impl Read, account: &str, prefix: &str) -> Result<Option<String>> {
    r.one(
        handles_with_prefix(account, Some(prefix))
            .order_by("created", Order::Asc)
            .limit(1),
        |row| row.get(0),
    )
}

/// Prefer the handle learned through `connection` for mentioning `account` in a channel.
/// When `prefix` is supplied by the channel adapter, only handles with that prefix are considered.
pub fn connection_mention_handle(
    r: &mut impl Read,
    ws: &str,
    connection: &str,
    account: &str,
    prefix: Option<&str>,
) -> Result<Option<String>> {
    r.one(
        handles_with_prefix(account, prefix)
            .and_where(Expr::col("workspace_id").eq(ws))
            .order_by_expr(
                Expr::case(Expr::col("connection_id").eq(connection), 2_i64)
                    .case(Expr::col("connection_id").is_null(), 0_i64)
                    .finally(1_i64)
                    .into(),
                Order::Desc,
            )
            .order_by("created", Order::Asc)
            .limit(1),
        |row| row.get(0),
    )
}

fn handles_with_prefix(account: &str, prefix: Option<&str>) -> sea_query::SelectStatement {
    let mut q = Query::select();
    q.column("handle")
        .from("account_handles")
        .and_where(Expr::col("account_id").eq(account));
    if let Some(prefix) = prefix {
        let escaped = prefix
            .replace('\\', r"\\")
            .replace('%', r"\%")
            .replace('_', r"\_");
        q.and_where(Expr::col("handle").like(LikeExpr::new(format!("{escaped}%")).escape('\\')));
    }
    q.take()
}

pub(crate) fn app_bot_ids() -> SelectStatement {
    Query::select()
        .column("id")
        .from("accounts")
        .and_where(Expr::col("kind").eq("app_bot"))
        .take()
}

pub fn has_handle(r: &mut impl Read, account: &str, handle: &str) -> Result<bool> {
    Ok(r.one(
        Query::select()
            .column("account_id")
            .from("account_handles")
            .and_where(Expr::col("account_id").eq(account))
            .and_where(Expr::col("handle").eq(handle))
            .limit(1),
        |_| Ok(()),
    )?
    .is_some())
}
