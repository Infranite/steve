use sea_query::{Expr, ExprTrait, OnConflict, Query};

use crate::{Read, Result, W};

pub(crate) struct WorkspaceSettings<'a, C>(pub(crate) &'a mut C);

impl<C: Read> WorkspaceSettings<'_, C> {
    pub(crate) fn get(&mut self, ws: &str, key: &str) -> Result<Option<String>> {
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
    pub(crate) fn set(&mut self, ws: &str, key: &str, value: &str, now: i64) -> Result<()> {
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
