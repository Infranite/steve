use sea_query::{Expr, ExprTrait, OnConflict, Query};

use crate::{Read, Result, W};

pub struct WorkspaceSettings<'a, C>(pub &'a mut C);

pub const HTTP_ALLOW_PRIVATE: &str = "http_allow_private";

impl<C: Read> WorkspaceSettings<'_, C> {
    pub fn get(&mut self, ws: &str, key: &str) -> Result<Option<String>> {
        self.0.one(
            Query::select()
                .column("value")
                .from("settings")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("key").eq(key)),
            |row| row.get(0),
        )
    }

    pub(crate) fn keys_starting_with(&mut self, ws: &str, prefix: &str) -> Result<Vec<String>> {
        self.0.all(
            Query::select()
                .column("key")
                .from("settings")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("key").like(format!("{prefix}%"))),
            |row| row.get(0),
        )
    }
}

impl WorkspaceSettings<'_, W<'_>> {
    pub fn set_http_allow_private(&mut self, ws: &str, hosts: &[String], now: i64) -> Result<()> {
        self.set(ws, HTTP_ALLOW_PRIVATE, &serde_json::to_string(hosts)?, now)
    }

    pub fn set(&mut self, ws: &str, key: &str, value: &str, now: i64) -> Result<()> {
        self.0.exec(
            Query::insert()
                .into_table("settings")
                .columns(["workspace_id", "key", "value", "updated"])
                .values_panic([ws.into(), key.into(), value.into(), now.into()])
                .on_conflict(
                    OnConflict::columns(["workspace_id", "key"])
                        .update_columns(["value", "updated"])
                        .to_owned(),
                ),
        )?;
        Ok(())
    }

    pub(crate) fn remove<'a>(
        &mut self,
        ws: &str,
        keys: impl IntoIterator<Item = &'a str>,
    ) -> Result<()> {
        self.0.exec(
            Query::delete()
                .from_table("settings")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("key").is_in(keys)),
        )?;
        Ok(())
    }
}
