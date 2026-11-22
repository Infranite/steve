use sea_query::{Expr, ExprTrait, Query};

use crate::{Read, Result};

pub struct ProjectRows<'a, C>(pub &'a mut C);
pub struct TagRows<'a, C>(pub &'a mut C);

impl<C: Read> ProjectRows<'_, C> {
    pub fn contains(&mut self, ws: &str, id: &str) -> Result<bool> {
        self.0
            .one(
                Query::select()
                    .column("id")
                    .from("projects")
                    .and_where(Expr::col("workspace_id").eq(ws))
                    .and_where(Expr::col("id").eq(id)),
                |_| Ok(()),
            )
            .map(|row| row.is_some())
    }

    pub fn local_path(&mut self, ws: &str, id: &str) -> Result<Option<String>> {
        self.0.one(
            Query::select()
                .column("path")
                .from("projects")
                .and_where(Expr::col("id").eq(id))
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("archived").eq(0_i64))
                .and_where(Expr::col("device_id").eq("hub")),
            |row| row.get(0),
        )
    }
}

impl<C: Read> TagRows<'_, C> {
    pub fn contains(&mut self, ws: &str, id: &str) -> Result<bool> {
        self.0
            .one(
                Query::select()
                    .column("id")
                    .from("tags")
                    .and_where(Expr::col("workspace_id").eq(ws))
                    .and_where(Expr::col("id").eq(id)),
                |_| Ok(()),
            )
            .map(|row| row.is_some())
    }
}
