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

pub struct PartyAudience {
    pub id: String,
    pub user_id: Option<String>,
    pub org_kind: Option<String>,
    pub name: String,
    pub status: String,
    pub important: bool,
    pub visibility: crate::base::Visibility,
    pub created_by: String,
}

pub fn audience(c: &mut impl Read, id: &str) -> Result<Option<PartyAudience>> {
    c.one(
        Query::select()
            .columns([
                "id",
                "user_id",
                "org_kind",
                "name",
                "status",
                "visibility",
                "created_by",
                "important",
            ])
            .from("parties")
            .and_where(Expr::col("id").eq(id)),
        |r| {
            Ok(PartyAudience {
                id: r.get(0)?,
                user_id: r.get(1)?,
                org_kind: r.get(2)?,
                name: r.get(3)?,
                status: r.get(4)?,
                visibility: crate::base::Visibility::from_db(&r.get::<String>(5)?),
                created_by: r.get(6)?,
                important: r.get::<i64>(7)? != 0,
            })
        },
    )
}

pub fn current_audience(c: &mut impl Read, id: &str) -> Result<Option<PartyAudience>> {
    let id = Parties::new(c).current(id.to_owned())?;
    audience(c, &id)
}

pub fn in_own_domain(c: &mut impl Read, ws: &str, domain: &str) -> Result<bool> {
    Ok(c.one(
        Query::select()
            .expr(Expr::cust("1"))
            .from_as("org_domains", "d")
            .join_as(
                sea_query::JoinType::InnerJoin,
                "parties",
                "o",
                Expr::col(("o", "id")).eq(Expr::col(("d", "org_id"))),
            )
            .and_where(Expr::col(("d", "workspace_id")).eq(ws))
            .and_where(Expr::col(("d", "domain")).eq(domain))
            .and_where(Expr::col(("o", "org_kind")).eq("self"))
            .limit(1),
        |_| Ok(()),
    )?
    .is_some())
}

pub fn merged_ids(c: &mut impl Read, party: &str) -> Result<Vec<String>> {
    let dialect = c.db().dialect();
    let mut ids = vec![party.to_owned()];
    let mut start = 0;
    while start < ids.len() {
        let candidates = dialect.text_array(&ids[start..])?;
        start = ids.len();
        let children = c.all(
            Query::select()
                .column("id")
                .from("parties")
                .and_where(Expr::col("merged_into").in_subquery(candidates)),
            |r| r.get::<String>(0),
        )?;
        for child in children {
            if !ids.contains(&child) {
                ids.push(child);
            }
        }
    }
    Ok(ids)
}
