use sea_query::{Expr, ExprTrait, Query};

use crate::{Read, Result};

pub struct Settings<'a, C>(&'a mut C);
pub struct NotificationPolicies<'a, C>(&'a mut C);

impl<'a, C: Read> Settings<'a, C> {
    pub fn new(handle: &'a mut C) -> Self {
        Self(handle)
    }

    /// The workspace's configured zone name. The caller resolves the system default.
    pub fn timezone(&mut self, ws: &str) -> Result<Option<String>> {
        let value = self
            .0
            .one(
                Query::select()
                    .column("value")
                    .from("settings")
                    .and_where(Expr::col("workspace_id").eq(ws))
                    .and_where(Expr::col("key").eq("tz")),
                |r| r.get::<Option<String>>(0),
            )?
            .flatten();
        Ok(value.and_then(|v| serde_json::from_str(&v).ok()))
    }
}

impl<'a, C: Read> NotificationPolicies<'a, C> {
    pub fn new(handle: &'a mut C) -> Self {
        Self(handle)
    }

    pub fn timezone(&mut self, ws: &str, user: &str) -> Result<Option<String>> {
        self.0.one(
            Query::select()
                .column("timezone")
                .from("notification_policies")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("user_id").eq(user)),
            |r| r.get(0),
        )
    }
}

/// The workspace's item id prefix (`ST`).
pub fn item_prefix(c: &mut impl Read, ws: &str) -> Result<String> {
    Ok(c.one(
        Query::select()
            .columns(["item_prefix"])
            .from("workspaces")
            .and_where(Expr::col("id").eq(ws)),
        |r| r.get::<String>(0),
    )?
    .unwrap_or_else(|| "ST".into()))
}

pub fn workspace_name(c: &mut impl Read, ws: &str) -> Result<Option<String>> {
    c.one(
        Query::select()
            .columns(["name"])
            .from("workspaces")
            .and_where(Expr::col("id").eq(ws)),
        |r| r.get(0),
    )
}

pub fn user_is_member(c: &mut impl Read, ws: &str, user: &str) -> Result<bool> {
    c.one(
        Query::select()
            .expr(Expr::cust("1"))
            .from_as("users", "u")
            .join_as(
                sea_query::JoinType::InnerJoin,
                "workspace_members",
                "wm",
                Expr::col(("wm", "user_id")).eq(Expr::col(("u", "id"))),
            )
            .and_where(Expr::col(("u", "id")).eq(user))
            .and_where(Expr::col(("wm", "workspace_id")).eq(ws))
            .limit(1),
        |_| Ok(()),
    )
    .map(|r| r.is_some())
}
