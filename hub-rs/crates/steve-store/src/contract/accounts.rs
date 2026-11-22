use sea_query::{Expr, ExprTrait, Func, JoinType, OnConflict, Query};

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
