use sea_query::{Expr, ExprTrait, Order, Query, SelectStatement};

use crate::{Read, Result};

pub struct Parties<'a, C>(&'a mut C);

fn parties<const N: usize>(cols: [&'static str; N]) -> SelectStatement {
    Query::select().columns(cols).from("parties").take()
}

impl<'a, C: Read> Parties<'a, C> {
    pub fn new(handle: &'a mut C) -> Self {
        Self(handle)
    }

    pub fn name(&mut self, id: &str) -> Result<Option<String>> {
        self.0
            .one(parties(["name"]).and_where(Expr::col("id").eq(id)), |r| {
                r.get(0)
            })
    }

    pub fn active_user(&mut self, ws: &str, user: &str) -> Result<Option<String>> {
        self.0.one(
            parties(["id"])
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("user_id").eq(user))
                .and_where(Expr::col("status").eq("active"))
                .order_by("created", Order::Asc)
                .limit(1),
            |r| r.get(0),
        )
    }

    pub fn is_active(&mut self, ws: &str, id: &str) -> Result<bool> {
        self.has_party(ws, id, Expr::col("status").eq("active"))
    }

    pub fn is_present(&mut self, ws: &str, id: &str) -> Result<bool> {
        self.has_party(ws, id, Expr::col("status").ne("deleted"))
    }

    fn has_party(&mut self, ws: &str, id: &str, status: Expr) -> Result<bool> {
        Ok(self
            .0
            .one(
                parties(["id"])
                    .and_where(Expr::col("workspace_id").eq(ws))
                    .and_where(Expr::col("id").eq(id))
                    .and_where(status),
                |_| Ok(()),
            )?
            .is_some())
    }

    pub fn current(&mut self, mut id: String) -> Result<String> {
        for _ in 0..32 {
            let next = self
                .0
                .one(
                    parties(["merged_into"])
                        .and_where(Expr::col("id").eq(&id))
                        .and_where(Expr::col("status").eq("merged")),
                    |r| r.get::<Option<String>>(0),
                )?
                .flatten();
            match next {
                Some(next) => id = next,
                None => break,
            }
        }
        Ok(id)
    }

    /// Returns up to two active matches so callers can detect ambiguity.
    pub fn active_by_name(&mut self, ws: &str, name: &str) -> Result<Vec<String>> {
        let name = self
            .0
            .db()
            .dialect()
            .ascii_nocase(Expr::col("name"))
            .eq(name);
        self.0.all(
            parties(["id"])
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("status").eq("active"))
                .and_where(name)
                .limit(2),
            |r| r.get(0),
        )
    }
}
