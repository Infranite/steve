//! Membership of a channel space.

use sea_query::{Expr, ExprTrait, Func, JoinType, OnConflict, Order, Query, SelectStatement};

use crate::{Read, Result, W};

pub struct Member {
    pub account_id: String,
    pub name: String,
    pub handle: String,
    pub role: String,
    pub joined: i64,
    pub party_id: String,
}

pub fn count(r: &mut impl Read, space: &str) -> Result<i64> {
    Ok(r.one(
        Query::select()
            .expr(Expr::cust("count(*)"))
            .from("space_members")
            .and_where(Expr::col("space_id").eq(space)),
        |row| row.get(0),
    )?
    .unwrap_or_default())
}

pub(crate) fn bots(r: &mut impl Read, space: &str, ids: &[&str]) -> Result<Vec<String>> {
    if ids.is_empty() {
        return Ok(vec![]);
    }
    let ids = r.db().dialect().text_array(ids)?;
    r.all(
        Query::select()
            .column("account_id")
            .from("space_members")
            .and_where(Expr::col("space_id").eq(space))
            .and_where(Expr::col("account_id").in_subquery(ids))
            .and_where(Expr::col("role").eq("bot")),
        |row| row.get(0),
    )
}

pub fn page(r: &mut impl Read, space: &str, offset: u64, limit: u64) -> Result<Vec<Member>> {
    let account_name: Expr = Func::coalesce([
        Expr::cust_with_expr("NULLIF(?, '')", Expr::col(("a", "display_name"))),
        Expr::col(("a", "handle_display")),
    ])
    .into();
    let role = Expr::col(("m", "role"));
    let precedence = Expr::case(role.clone().eq("owner"), 0i64)
        .case(role.clone().eq("admin"), 1i64)
        .case(role.eq("bot"), 3i64)
        .finally(2i64);
    r.all(
        Query::select()
            .column(("m", "account_id"))
            .expr(account_name.clone())
            .columns([
                ("a", "handle_display"),
                ("m", "role"),
                ("m", "joined"),
                ("a", "party_id"),
            ])
            .from_as("space_members", "m")
            .join_as(
                JoinType::InnerJoin,
                "accounts",
                "a",
                Expr::col(("a", "id")).eq(Expr::col(("m", "account_id"))),
            )
            .and_where(Expr::col(("m", "space_id")).eq(space))
            .order_by_expr(precedence.into(), Order::Asc)
            .order_by_expr(account_name, Order::Asc)
            .order_by(("m", "account_id"), Order::Asc)
            .limit(limit)
            .offset(offset),
        |row| {
            Ok(Member {
                account_id: row.get(0)?,
                name: row.get(1)?,
                handle: row.get(2)?,
                role: row.get(3)?,
                joined: row.get(4)?,
                party_id: row.get(5)?,
            })
        },
    )
}

/// Keep any existing member unchanged, including the role and first join time.
pub fn add(
    w: &mut W<'_>,
    ws: &str,
    space: &str,
    account: &str,
    role: &str,
    now: i64,
) -> Result<bool> {
    Ok(w.exec(
        Query::insert()
            .into_table("space_members")
            .columns(["workspace_id", "space_id", "account_id", "role", "joined"])
            .values_panic([
                ws.into(),
                space.into(),
                account.into(),
                role.into(),
                now.into(),
            ])
            .on_conflict(
                OnConflict::columns(["space_id", "account_id"])
                    .do_nothing()
                    .to_owned(),
            ),
    )? == 1)
}

pub fn remove(w: &mut W<'_>, space: &str, account: &str) -> Result<bool> {
    Ok(w.exec(
        Query::delete()
            .from_table("space_members")
            .and_where(Expr::col("space_id").eq(space))
            .and_where(Expr::col("account_id").eq(account)),
    )? == 1)
}

pub(in crate::contract) fn bot_accounts(space: &str) -> sea_query::SelectStatement {
    Query::select()
        .column("account_id")
        .from("space_members")
        .and_where(Expr::col("space_id").eq(space))
        .and_where(Expr::col("role").eq("bot"))
        .take()
}

fn member_accounts(space: &str) -> SelectStatement {
    Query::select()
        .from_as("space_members", "m")
        .join_as(
            JoinType::InnerJoin,
            "accounts",
            "a",
            Expr::col(("a", "id")).equals(("m", "account_id")),
        )
        .and_where(Expr::col(("m", "space_id")).eq(space))
        .take()
}

pub fn has_party(r: &mut impl Read, ws: &str, space: &str, party: &str) -> Result<bool> {
    if !crate::parties::Parties::new(r).is_active(ws, party)? {
        return Ok(false);
    }
    Ok(r.one(
        member_accounts(space)
            .column(("a", "id"))
            .and_where(Expr::col(("m", "workspace_id")).eq(ws))
            .and_where(Expr::col(("a", "party_id")).eq(party))
            .limit(1),
        |_| Ok(()),
    )?
    .is_some())
}

pub fn direct_chats(r: &mut impl Read, ws: &str, party: &str) -> Result<Vec<String>> {
    r.all(
        Query::select()
            .distinct()
            .column(("s", "id"))
            .from_as("spaces", "s")
            .join_as(
                JoinType::InnerJoin,
                "space_members",
                "m",
                Expr::col(("m", "space_id")).equals(("s", "id")),
            )
            .join_as(
                JoinType::InnerJoin,
                "accounts",
                "a",
                Expr::col(("a", "id")).equals(("m", "account_id")),
            )
            .and_where(Expr::col(("a", "party_id")).eq(party))
            .and_where(Expr::col(("s", "workspace_id")).eq(ws))
            .and_where(Expr::col(("s", "kind")).eq("dm"))
            .order_by(("s", "id"), Order::Asc),
        |row| row.get(0),
    )
}

pub struct AudienceMember {
    pub account: String,
    pub party: Option<String>,
    pub user: Option<String>,
    pub name: String,
}

pub fn audience(r: &mut impl Read, space: &str, sender: &str) -> Result<Vec<AudienceMember>> {
    audience_rows(r, space, Some(sender))
}

pub fn people(r: &mut impl Read, space: &str) -> Result<Vec<AudienceMember>> {
    audience_rows(r, space, None)
}

pub fn recorded_party(r: &mut impl Read, space: &str, party: &str) -> Result<bool> {
    Ok(r.one(
        member_accounts(space)
            .column(("a", "id"))
            .and_where(Expr::col(("a", "party_id")).eq(party))
            .limit(1),
        |_| Ok(()),
    )?
    .is_some())
}

fn audience_rows(
    r: &mut impl Read,
    space: &str,
    sender_account: Option<&str>,
) -> Result<Vec<AudienceMember>> {
    let mut query = Query::select();
    query
        .columns([("m", "account_id"), ("a", "party_id"), ("p", "user_id")])
        .expr(Func::coalesce([
            Expr::cust_with_expr("NULLIF(?, '')", Expr::col(("a", "display_name"))),
            Expr::col(("p", "name")),
            Expr::col(("a", "handle_display")),
        ]))
        .from_as("space_members", "m")
        .join_as(
            JoinType::InnerJoin,
            "accounts",
            "a",
            Expr::col(("a", "id")).equals(("m", "account_id")),
        )
        .join_as(
            JoinType::LeftJoin,
            "parties",
            "p",
            Expr::col(("p", "id")).equals(("a", "party_id")),
        )
        .and_where(Expr::col(("m", "space_id")).eq(space))
        .order_by(("m", "joined"), Order::Asc);
    match sender_account {
        Some(sender) => query.and_where(Expr::col(("m", "account_id")).ne(sender)),
        None => query.and_where(Expr::col(("a", "kind")).is_not_in(["app_bot", "bot"])),
    };
    r.all(&query, |row| {
        Ok(AudienceMember {
            account: row.get(0)?,
            party: row.get(1)?,
            user: row.get(2)?,
            name: row.get(3)?,
        })
    })
}

pub fn people_count(r: &mut impl Read, space: &str) -> Result<i64> {
    Ok(r.one(
        Query::select()
            .expr(Func::count(Expr::col(("m", "account_id"))))
            .from_as("space_members", "m")
            .join_as(
                JoinType::InnerJoin,
                "accounts",
                "a",
                Expr::col(("a", "id")).equals(("m", "account_id")),
            )
            .and_where(Expr::col(("m", "space_id")).eq(space))
            .and_where(Expr::col(("a", "kind")).is_not_in(["app_bot", "bot"])),
        |row| row.get(0),
    )?
    .unwrap_or_default())
}
