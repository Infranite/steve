use crate::{Read, Result, W};
use sea_query::{Expr, ExprTrait, JoinType, Order, Query};

/// A bot's display name.
pub fn bot_name(c: &mut impl Read, bot: &str) -> Result<Option<String>> {
    c.one(
        Query::select()
            .columns(["name"])
            .from("bots")
            .and_where(Expr::col("id").eq(bot)),
        |r| r.get(0),
    )
}

pub fn scoped_bot(c: &mut impl Read, ws: &str, id: &str) -> Result<bool> {
    c.one(
        Query::select()
            .expr(Expr::cust("1"))
            .from_as("bots", "b")
            .join_as(
                JoinType::InnerJoin,
                "bot_workspaces",
                "w",
                Expr::col(("w", "bot_id"))
                    .eq(Expr::col(("b", "id")))
                    .and(Expr::col(("w", "workspace_id")).eq(ws)),
            )
            .and_where(Expr::col(("b", "id")).eq(id))
            .and_where(Expr::col(("b", "deleted")).eq(0_i64))
            .limit(1),
        |_| Ok(()),
    )
    .map(|row| row.is_some())
}

pub fn main_bot(c: &mut impl Read) -> Result<Option<String>> {
    c.one(
        Query::select()
            .columns(["id"])
            .from("bots")
            .and_where(Expr::col("main").eq(1_i64))
            .and_where(Expr::col("deleted").eq(0_i64))
            .order_by("updated", Order::Asc)
            .limit(1),
        |r| r.get(0),
    )
}

pub fn bot_enabled(c: &mut impl Read, id: &str) -> Result<Option<bool>> {
    c.one(
        Query::select()
            .columns(["enabled"])
            .from("bots")
            .and_where(Expr::col("id").eq(id)),
        |r| r.get(0),
    )
}

pub fn named_bots(c: &mut impl Read, ws: &str, name: &str) -> Result<Vec<String>> {
    c.all(
        Query::select()
            .columns([("b", "id")])
            .from_as("bots", "b")
            .join_as(
                JoinType::InnerJoin,
                "bot_workspaces",
                "w",
                Expr::col(("w", "bot_id"))
                    .eq(Expr::col(("b", "id")))
                    .and(Expr::col(("w", "workspace_id")).eq(ws)),
            )
            .and_where(Expr::col(("b", "deleted")).eq(0_i64))
            .and_where(
                Expr::col(("b", "id"))
                    .eq(name)
                    .or(Expr::col(("b", "name")).eq(name)),
            )
            .order_by_expr(Expr::col(("b", "id")).eq(name), Order::Desc)
            .limit(2),
        |r| r.get(0),
    )
}

#[derive(Debug, Clone, PartialEq)]
pub struct BotInfo {
    pub id: String,
    pub name: String,
    pub role: String,
    pub rules: String,
}

pub fn for_judgement(
    read: &mut impl Read,
    ws: &str,
    configured: Option<&str>,
) -> Result<Option<BotInfo>> {
    let mut q = Query::select();
    q.columns([("b", "id"), ("b", "name"), ("b", "role"), ("b", "rules")])
        .from_as("bots", "b")
        .join_as(
            JoinType::InnerJoin,
            "bot_workspaces",
            "w",
            Expr::col(("w", "bot_id")).eq(Expr::col(("b", "id"))),
        )
        .and_where(Expr::col(("w", "workspace_id")).eq(ws))
        .and_where(Expr::col(("b", "deleted")).eq(0_i64))
        .limit(1);
    if let Some(id) = configured {
        q.and_where(Expr::col(("b", "id")).eq(id))
            .and_where(Expr::col(("b", "enabled")).eq(1_i64));
    } else {
        q.and_where(Expr::col(("b", "main")).eq(1_i64))
            .order_by(("b", "updated"), Order::Asc);
    }
    read.one(&q, |r| {
        Ok(BotInfo {
            id: r.get(0)?,
            name: r.get(1)?,
            role: r.get(2)?,
            rules: r.get(3)?,
        })
    })
}

/// The workspace's bot `id` (`bot_workspaces`): `(name, role, rules)`.
pub fn bot(c: &mut impl Read, ws: &str, id: &str) -> Result<Option<(String, String, String)>> {
    c.one(
        Query::select()
            .columns([("b", "name"), ("b", "role"), ("b", "rules")])
            .from_as("bots", "b")
            .join_as(
                sea_query::JoinType::InnerJoin,
                "bot_workspaces",
                "w",
                Expr::col(("w", "bot_id")).eq(Expr::col(("b", "id"))),
            )
            .and_where(Expr::col(("w", "workspace_id")).eq(ws))
            .and_where(Expr::col(("b", "id")).eq(id))
            .and_where(Expr::col(("b", "deleted")).eq(0_i64)),
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
    )
}

/// The main bot (the default drafter when there is no mail bot).
pub fn workspace_main_bot(c: &mut impl Read, ws: &str) -> Result<Option<String>> {
    c.one(
        Query::select()
            .columns([("b", "id")])
            .from_as("bots", "b")
            .join_as(
                sea_query::JoinType::InnerJoin,
                "bot_workspaces",
                "w",
                Expr::col(("w", "bot_id")).eq(Expr::col(("b", "id"))),
            )
            .and_where(Expr::col(("w", "workspace_id")).eq(ws))
            .and_where(Expr::col(("b", "main")).eq(1_i64))
            .and_where(Expr::col(("b", "deleted")).eq(0_i64))
            .order_by(("b", "updated"), Order::Asc)
            .limit(1),
        |r| r.get(0),
    )
}

pub(crate) fn model(c: &mut impl Read, id: &str) -> Result<Option<String>> {
    Ok(c.one(
        Query::select()
            .column("model")
            .from("bots")
            .and_where(Expr::col("id").eq(id)),
        |r| r.get(0),
    )?
    .flatten())
}

pub fn fill_missing_harness(w: &mut W<'_>, id: &str, harness: &str) -> Result<()> {
    w.exec(
        Query::update()
            .table("bots")
            .value("harness", harness)
            .and_where(Expr::col("id").eq(id))
            .and_where(Expr::col("harness").eq("")),
    )?;
    Ok(())
}

pub struct SeededHarness {
    pub harness: String,
    pub workspace: String,
}

pub fn pristine_harness(c: &mut impl Read, id: &str) -> Result<Option<SeededHarness>> {
    c.one(
        Query::select()
            .columns(["harness", "workspace_id"])
            .from("bots")
            .and_where(Expr::col("id").eq(id))
            .and_where(Expr::col("deleted").eq(0_i64))
            .and_where(Expr::col("revision").eq(1_i64))
            .and_where(Expr::col("model").is_null())
            .and_where(Expr::col("reasoning_effort").is_null()),
        |r| {
            Ok(SeededHarness {
                harness: r.get(0)?,
                workspace: r.get(1)?,
            })
        },
    )
}

pub fn move_pristine_harness(
    w: &mut W<'_>,
    id: &str,
    harness: &str,
    permission: &str,
    at: i64,
) -> Result<bool> {
    Ok(w.exec(
        Query::update()
            .table("bots")
            .value("harness", harness)
            .value("perm_mode", permission)
            .value("updated", at)
            .and_where(Expr::col("id").eq(id))
            .and_where(Expr::col("revision").eq(1_i64)),
    )? > 0)
}
