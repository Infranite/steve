use crate::{Read, Result, W};
use sea_query::{Expr, ExprTrait, Order, Query};

/// The capability of “this bot may comment in this repo without asking” (`grants.capability`).
pub const COMMENT_CAP: &str = "github.comment";

/// The open `github.comment` grant of `bot` on `repo` at `now`, longest-lasting first.
pub fn comment_grant(
    c: &mut impl Read,
    ws: &str,
    bot: &str,
    repo: &str,
    now: i64,
) -> Result<Option<String>> {
    c.one(
        Query::select()
            .columns(["id"])
            .from("grants")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("capability").eq("github.comment"))
            .and_where(Expr::col("resource_type").eq("repo"))
            .and_where(Expr::col("resource_id").eq(repo))
            .and_where(Expr::col("subject_type").eq("bot"))
            .and_where(Expr::col("subject_id").eq(bot))
            .and_where(Expr::col("revoked").is_null())
            .and_where(Expr::col("until").gt(now))
            .order_by("until", Order::Desc)
            .order_by("id", Order::Asc)
            .limit(1),
        |r| r.get(0),
    )
}

/// Writes a `github.comment` grant of `bot` on `repo` until `until`.
#[allow(clippy::too_many_arguments)]
pub fn insert_comment_grant(
    c: &mut W<'_>,
    id: &str,
    ws: &str,
    bot: &str,
    repo: &str,
    until: i64,
    created_by: &str,
    decision_id: Option<&str>,
    now: i64,
) -> Result<()> {
    c.exec(
        Query::insert()
            .into_table("grants")
            .columns([
                "id",
                "workspace_id",
                "subject_type",
                "subject_id",
                "capability",
                "resource_type",
                "resource_id",
                "scope",
                "until",
                "created_by",
                "decision_id",
                "revoked",
                "created",
            ])
            .values_panic([
                Expr::val(id),
                Expr::val(ws),
                Expr::val("bot"),
                Expr::val(bot),
                Expr::val("github.comment"),
                Expr::val("repo"),
                Expr::val(repo),
                Expr::val("{}"),
                Expr::val(until),
                Expr::val(created_by),
                Expr::val(decision_id),
                Expr::val(None::<String>),
                Expr::val(now),
            ]),
    )?;
    Ok(())
}

pub fn comment_grants(
    c: &mut impl Read,
    ws: &str,
    now: i64,
) -> Result<Vec<(String, String, String, i64)>> {
    c.all(
        Query::select()
            .columns(["id", "subject_id", "resource_id", "until"])
            .from("grants")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("capability").eq(COMMENT_CAP))
            .and_where(Expr::col("revoked").is_null())
            .and_where(Expr::col("until").gt(now))
            .order_by_columns([("created", Order::Asc), ("id", Order::Asc)]),
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    )
}

pub fn revoke(c: &mut W<'_>, ws: &str, id: &str, now: i64) -> Result<u64> {
    c.exec(
        Query::update()
            .table("grants")
            .value("revoked", now)
            .and_where(Expr::col("id").eq(id))
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("revoked").is_null()),
    )
}
