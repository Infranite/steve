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
    pub(crate) fn raw(&mut self) -> &mut C {
        self.0
    }

    pub fn new(handle: &mut C) -> Members<'_, C> {
        Members(handle)
    }

    pub fn owners(&mut self, ws: &str) -> Result<Vec<String>> {
        self.0.all(
            Query::select()
                .column("user_id")
                .from("workspace_members")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("role").eq("owner"))
                .order_by("created", Order::Asc),
            |row| row.get(0),
        )
    }

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

pub struct Workspaces<'a, C>(pub &'a mut C);

impl<C: Read> Workspaces<'_, C> {
    pub fn name(&mut self, id: &str) -> Result<Option<String>> {
        self.0.one(
            Query::select()
                .from("workspaces")
                .column("name")
                .and_where(Expr::col("id").eq(id)),
            |row| row.get(0),
        )
    }

    pub fn ids(&mut self) -> Result<Vec<String>> {
        self.0.all(
            Query::select()
                .column("id")
                .from("workspaces")
                .order_by("created", Order::Asc)
                .order_by("id", Order::Asc),
            |r| r.get(0),
        )
    }

    pub fn has_bot(&mut self, ws: &str, id: &str) -> Result<bool> {
        Ok(self
            .0
            .one(
                Query::select()
                    .column("bot_id")
                    .from("bot_workspaces")
                    .and_where(Expr::col("workspace_id").eq(ws))
                    .and_where(Expr::col("bot_id").eq(id)),
                |_| Ok(()),
            )?
            .is_some())
    }
}

impl Workspaces<'_, W<'_>> {
    pub fn insert(&mut self, id: &str, name: &str, created: i64) -> Result<()> {
        self.0.exec(
            Query::insert()
                .into_table("workspaces")
                .columns(["id", "name", "created"])
                .values_panic([id.into(), name.into(), created.into()]),
        )?;
        Ok(())
    }
}

impl Members<'_, W<'_>> {
    pub fn add(&mut self, ws: &str, user: &str, role: &str, created: i64) -> Result<()> {
        self.0.exec(
            Query::insert()
                .into_table("workspace_members")
                .columns(["workspace_id", "user_id", "role", "created"])
                .values_panic([ws.into(), user.into(), role.into(), created.into()]),
        )?;
        Ok(())
    }
}

/// Instance upgrade requests from MCP require the sole member to be an instance owner.
impl<C: Read> Workspaces<'_, C> {
    pub fn sole_instance_owner(&mut self, ws: &str) -> Result<bool> {
        use sea_query::{Func, JoinType};
        let owners = Expr::case(
            Expr::col(("u", "role")).eq("owner"),
            Expr::Constant(1_i64.into()),
        )
        .finally(Expr::Constant(0_i64.into()));
        let row: Option<(i64, Option<i64>)> = self.0.one(
            Query::select()
                .expr(Func::count(Expr::col(("m", "user_id"))))
                .expr(Func::sum(owners))
                .from_as("workspace_members", "m")
                .join_as(
                    JoinType::InnerJoin,
                    "users",
                    "u",
                    Expr::col(("u", "id")).eq(Expr::col(("m", "user_id"))),
                )
                .and_where(Expr::col(("m", "workspace_id")).eq(ws)),
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;
        Ok(row == Some((1, Some(1))))
    }
}

impl<C: Read> Members<'_, C> {
    pub fn workspace_ids(&mut self, user: &str) -> Result<Vec<String>> {
        self.0.all(
            Query::select()
                .column("workspace_id")
                .from("workspace_members")
                .and_where(Expr::col("user_id").eq(user))
                .order_by_columns([("created", Order::Asc), ("workspace_id", Order::Asc)]),
            |row| row.get(0),
        )
    }
}
