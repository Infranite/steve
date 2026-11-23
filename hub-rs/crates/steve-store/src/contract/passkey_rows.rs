use sea_query::{Expr, ExprTrait, Func, Order, Query, SelectStatement};
use serde::Serialize;

use crate::{Read, Result, W};

pub struct PasskeyRows<'a, C>(pub &'a mut C);

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct PasskeyInfo {
    pub id: String,
    pub name: String,
    pub rp_id: String,
    pub created: i64,
    pub last_used: Option<i64>,
}

fn owned(ws: &str, user: &str) -> SelectStatement {
    Query::select()
        .from("passkeys")
        .and_where(Expr::col("workspace_id").eq(ws))
        .and_where(Expr::col("user_id").eq(user))
        .order_by("created", Order::Asc)
        .take()
}

impl<C: Read> PasskeyRows<'_, C> {
    pub fn count(&mut self, ws: &str) -> Result<i64> {
        Ok(self
            .0
            .one(
                Query::select()
                    .from("passkeys")
                    .expr(Func::count(Expr::col("id")))
                    .and_where(Expr::col("workspace_id").eq(ws)),
                |row| row.get(0),
            )?
            .unwrap_or(0))
    }

    pub fn credentials(
        &mut self,
        ws: &str,
        user: &str,
        rp_id: &str,
    ) -> Result<Vec<(String, String)>> {
        self.0.all(
            owned(ws, user)
                .columns(["id", "credential"])
                .and_where(Expr::col("rp_id").eq(rp_id)),
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
    }

    pub fn registered_credentials(&mut self, rp_id: &str) -> Result<Vec<String>> {
        self.0.all(
            Query::select()
                .from("passkeys")
                .column("credential")
                .and_where(Expr::col("rp_id").eq(rp_id)),
            |row| row.get(0),
        )
    }

    pub fn list(&mut self, ws: &str, user: &str) -> Result<Vec<PasskeyInfo>> {
        self.0.all(
            owned(ws, user).columns(["id", "name", "rp_id", "created", "last_used"]),
            |row| {
                Ok(PasskeyInfo {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    rp_id: row.get(2)?,
                    created: row.get(3)?,
                    last_used: row.get(4)?,
                })
            },
        )
    }
}

impl PasskeyRows<'_, W<'_>> {
    pub fn insert(
        &mut self,
        ws: &str,
        user: &str,
        key: &PasskeyInfo,
        credential: &str,
    ) -> Result<()> {
        self.0.exec(
            Query::insert()
                .into_table("passkeys")
                .columns([
                    "id",
                    "workspace_id",
                    "user_id",
                    "name",
                    "rp_id",
                    "credential",
                    "created",
                ])
                .values_panic([
                    key.id.as_str().into(),
                    ws.into(),
                    user.into(),
                    key.name.as_str().into(),
                    key.rp_id.as_str().into(),
                    credential.into(),
                    key.created.into(),
                ]),
        )?;
        Ok(())
    }

    pub fn delete(&mut self, ws: &str, user: &str, id: &str) -> Result<bool> {
        Ok(self.0.exec(
            Query::delete()
                .from_table("passkeys")
                .and_where(Expr::col("id").eq(id))
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("user_id").eq(user)),
        )? != 0)
    }

    pub fn mark_used(&mut self, id: &str, at: i64, credential: Option<&str>) -> Result<()> {
        let mut update = Query::update();
        update
            .table("passkeys")
            .value("last_used", at)
            .and_where(Expr::col("id").eq(id));
        if let Some(value) = credential {
            update.value("credential", value);
        }
        self.0.exec(&update)?;
        Ok(())
    }
}
