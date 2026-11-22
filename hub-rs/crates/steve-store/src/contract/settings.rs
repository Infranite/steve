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
