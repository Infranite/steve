use sea_query::{Expr, ExprTrait, Order, Query};

use crate::{R, Read, Result, W};

pub struct Members<'a, C>(&'a mut C);

impl R<'_> {
    pub fn members(&mut self) -> Members<'_, Self> {
        Members(self)
    }
}

impl W<'_> {
    pub fn members(&mut self) -> Members<'_, Self> {
        Members(self)
    }
}

impl<C: Read> Members<'_, C> {
    pub fn role(&mut self, ws: &str, user: &str) -> Result<Option<String>> {
        self.0.one(
            Query::select()
                .column("role")
                .from("workspace_members")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("user_id").eq(user)),
            |row| row.get(0),
        )
    }

    pub fn owner(&mut self, ws: &str) -> Result<Option<String>> {
        self.0.one(
            Query::select()
                .column("user_id")
                .from("workspace_members")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("role").eq("owner"))
                .order_by("created", Order::Asc)
                .limit(1),
            |row| row.get(0),
        )
    }
}
