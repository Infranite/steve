use sea_query::{Expr, ExprTrait, Order, Query};

use crate::{Read, Result, W};

pub struct UserRows<'a, C>(pub &'a mut C);
pub struct AuthSessions<'a, C>(pub &'a mut C);

impl<C: Read> UserRows<'_, C> {
    pub fn email(&mut self, user: &str) -> Result<Option<String>> {
        Ok(self
            .0
            .one(
                Query::select()
                    .from("users")
                    .column("email")
                    .and_where(Expr::col("id").eq(user)),
                |row| row.get::<Option<String>>(0),
            )?
            .flatten())
    }

    /// The instance owner who first registered, for instance-wide Node administration.
    pub fn first_owner(&mut self) -> Result<Option<String>> {
        let order = self.0.db().dialect().insertion_order(None);
        self.0.one(
            Query::select()
                .column("id")
                .from("users")
                .and_where(Expr::col("role").eq("owner"))
                .order_by("created", Order::Asc)
                .order_by_expr(order, Order::Asc)
                .limit(1),
            |r| r.get(0),
        )
    }

    pub fn role(&mut self, user: &str) -> Result<Option<String>> {
        self.0.one(
            Query::select()
                .from("users")
                .column("role")
                .and_where(Expr::col("id").eq(user)),
            |row| row.get(0),
        )
    }

    pub fn name(&mut self, user: &str) -> Result<Option<String>> {
        self.0.one(
            Query::select()
                .from("users")
                .column("name")
                .and_where(Expr::col("id").eq(user)),
            |row| row.get(0),
        )
    }
}

impl<C: Read> AuthSessions<'_, C> {
    pub fn active_start(&mut self, token: &str, user: &str, at: i64) -> Result<Option<i64>> {
        self.0.one(
            Query::select()
                .from("auth_sessions")
                .column("created")
                .and_where(Expr::col("token_hash").eq(crate::sha(token)))
                .and_where(Expr::col("user_id").eq(user))
                .and_where(Expr::col("expires").gt(at)),
            |row| row.get(0),
        )
    }
}

impl UserRows<'_, W<'_>> {
    pub fn insert(
        &mut self,
        id: &str,
        ws: &str,
        subject: &str,
        name: &str,
        role: &str,
        created: i64,
    ) -> Result<()> {
        self.0.exec(
            Query::insert()
                .into_table("users")
                .columns(["id", "workspace_id", "subject", "name", "role", "created"])
                .values_panic([
                    id.into(),
                    ws.into(),
                    subject.into(),
                    name.into(),
                    role.into(),
                    created.into(),
                ]),
        )?;
        Ok(())
    }
}
