use sea_query::{Expr, ExprTrait, JoinType, OnConflict, Query};

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
