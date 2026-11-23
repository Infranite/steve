use sea_query::{
    Expr, ExprTrait, Func, JoinType, LikeExpr, OnConflict, Order, Query, SelectStatement,
};

use crate::{Read, Result, W};

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

impl Accounts<'_, W<'_>> {
    /// Records a system declared by an installed provider.
    pub fn upsert_declared_system(
        &mut self,
        id: &str,
        name: &str,
        ext: &str,
        now: i64,
    ) -> Result<()> {
        let plugin = format!("ext:{ext}");
        let handle_rule = serde_json::json!({ "normalize": "none" }).to_string();
        self.0.exec(
            Query::insert()
                .into_table("systems")
                .columns([
                    "id",
                    "workspace_id",
                    "name",
                    "kind",
                    "family",
                    "plugin",
                    "handle_rule",
                    "created",
                ])
                .values_panic([
                    id.into(),
                    "*".into(),
                    name.into(),
                    "comm".into(),
                    id.into(),
                    plugin.as_str().into(),
                    handle_rule.into(),
                    now.into(),
                ])
                .on_conflict(
                    OnConflict::columns(["workspace_id", "id"])
                        .update_columns(["name", "plugin"])
                        .action_and_where(
                            Expr::col(("systems", "plugin"))
                                .is_null()
                                .or(Expr::col(("systems", "plugin")).ne(plugin))
                                .or(Expr::col(("systems", "name")).ne(name)),
                        )
                        .to_owned(),
                ),
        )?;
        Ok(())
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

pub fn email_handles(c: &mut impl Read, ws: &str, party: &str) -> Result<Vec<String>> {
    c.all(
        Query::select()
            .column(("h", "handle"))
            .from_as("account_handles", "h")
            .join_as(
                sea_query::JoinType::InnerJoin,
                "accounts",
                "a",
                Expr::col(("a", "id")).eq(Expr::col(("h", "account_id"))),
            )
            .and_where(Expr::col(("h", "workspace_id")).eq(ws))
            .and_where(Expr::col(("h", "family")).eq("email"))
            .and_where(Expr::col(("a", "party_id")).eq(party))
            .order_by(("h", "handle"), Order::Asc),
        |r| Ok(r.get::<String>(0)?.to_lowercase()),
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

/// The earliest Open ID learned for a Feishu account.
pub fn feishu_open_id(r: &mut impl Read, account: &str) -> Result<Option<String>> {
    r.one(
        feishu_handles(account)
            .order_by("created", Order::Asc)
            .limit(1),
        |row| row.get(0),
    )
}

/// Prefer the Open ID the sending connection learned for this account.
pub fn feishu_mention_handle(
    r: &mut impl Read,
    ws: &str,
    connection: &str,
    account: &str,
) -> Result<Option<String>> {
    r.one(
        feishu_handles(account)
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

fn feishu_handles(account: &str) -> sea_query::SelectStatement {
    Query::select()
        .column("handle")
        .from("account_handles")
        .and_where(Expr::col("account_id").eq(account))
        .and_where(Expr::col("handle").like(LikeExpr::new(r"ou\_%").escape('\\')))
        .take()
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
