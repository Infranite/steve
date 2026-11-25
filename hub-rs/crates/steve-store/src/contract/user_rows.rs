use sea_query::{Cond, Expr, ExprTrait, JoinType, Order, Query};
use steve_types::{AvatarRef, MemberIdentity};

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

    /// Person display name for a user in a workspace: linked active contact's name, else login name.
    pub fn display_name(&mut self, ws: &str, user: &str) -> Result<Option<String>> {
        let party: Option<String> = self.0.one(
            Query::select()
                .from("parties")
                .column("name")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("user_id").eq(user))
                .and_where(Expr::col("status").eq("active"))
                .limit(1),
            |row| row.get(0),
        )?;
        if let Some(name) = party.filter(|n| !n.trim().is_empty()) {
            return Ok(Some(name));
        }
        self.name(user)
    }

    /// Active workspace members with their linked person identity (display name, avatar, subtitle).
    pub fn member_identities(&mut self, ws: &str) -> Result<Vec<MemberIdentity>> {
        let rows = self.0.all(
            Query::select()
                .columns([
                    ("m", "user_id"),
                    ("m", "role"),
                    ("u", "name"),
                    ("u", "email"),
                    ("p", "id"),
                    ("p", "name"),
                    ("p", "avatar"),
                ])
                .from_as("workspace_members", "m")
                .join_as(
                    JoinType::InnerJoin,
                    "users",
                    "u",
                    Expr::col(("u", "id")).equals(("m", "user_id")),
                )
                .join_as(
                    JoinType::LeftJoin,
                    "parties",
                    "p",
                    Cond::all()
                        .add(Expr::col(("p", "workspace_id")).equals(("m", "workspace_id")))
                        .add(Expr::col(("p", "user_id")).equals(("m", "user_id")))
                        .add(Expr::col(("p", "status")).eq("active")),
                )
                .and_where(Expr::col(("m", "workspace_id")).eq(ws))
                .order_by(("m", "created"), Order::Asc)
                .order_by(("m", "user_id"), Order::Asc),
            |row| {
                Ok((
                    row.get::<String>(0)?,
                    row.get::<String>(1)?,
                    row.get::<String>(2)?,
                    row.get::<Option<String>>(3)?,
                    row.get::<Option<String>>(4)?,
                    row.get::<Option<String>>(5)?,
                    row.get::<Option<String>>(6)?,
                ))
            },
        )?;
        let mut out = Vec::with_capacity(rows.len());
        for (user_id, role, login, email, party_id, party_name, party_avatar) in rows {
            let subtitle = match party_id.as_deref() {
                Some(pid) => self.party_job_subtitle(ws, pid)?,
                None => None,
            };
            let name = party_name
                .filter(|n| !n.trim().is_empty())
                .or_else(|| (!login.trim().is_empty()).then(|| login.clone()))
                .unwrap_or_else(|| user_id.clone());
            let avatar = party_avatar
                .as_deref()
                .and_then(|raw| serde_json::from_str::<AvatarRef>(raw).ok())
                .unwrap_or_default();
            out.push(MemberIdentity {
                user_id: user_id.into(),
                party_id: party_id.map(Into::into),
                name,
                login,
                email,
                role,
                avatar,
                subtitle,
            });
        }
        Ok(out)
    }

    /// Single member identity in a workspace; falls back to the `users` row if not yet linked.
    pub fn member_identity(&mut self, ws: &str, user: &str) -> Result<Option<MemberIdentity>> {
        if let Some(m) = self
            .member_identities(ws)?
            .into_iter()
            .find(|m| m.user_id.as_str() == user)
        {
            return Ok(Some(m));
        }
        let Some((id, login, email, role)) = self.0.one(
            Query::select()
                .from("users")
                .columns(["id", "name", "email", "role"])
                .and_where(Expr::col("id").eq(user)),
            |row| {
                Ok((
                    row.get::<String>(0)?,
                    row.get::<String>(1)?,
                    row.get::<Option<String>>(2)?,
                    row.get::<String>(3)?,
                ))
            },
        )?
        else {
            return Ok(None);
        };
        let name = if login.trim().is_empty() {
            id.clone()
        } else {
            login.clone()
        };
        Ok(Some(MemberIdentity {
            user_id: id.into(),
            party_id: None,
            name,
            login,
            email,
            role,
            avatar: AvatarRef::default(),
            subtitle: None,
        }))
    }

    fn party_job_subtitle(&mut self, ws: &str, party_id: &str) -> Result<Option<String>> {
        let dialect = self.0.db().dialect();
        let row: Option<(Option<String>, String)> = self.0.one(
            Query::select()
                .expr(dialect.json_field(Expr::col(("l", "props")), "title"))
                .column(("o", "name"))
                .from_as("links", "l")
                .join_as(
                    JoinType::InnerJoin,
                    "parties",
                    "o",
                    Expr::col(("o", "id")).equals(("l", "object_id")),
                )
                .and_where(Expr::col(("l", "workspace_id")).eq(ws))
                .and_where(Expr::col(("l", "subject_type")).eq("party"))
                .and_where(Expr::col(("l", "subject_id")).eq(party_id))
                .and_where(Expr::col(("l", "predicate")).eq("works_at"))
                .and_where(Expr::col(("l", "object_type")).eq("party"))
                .and_where(
                    dialect
                        .json_field(Expr::col(("l", "props")), "to")
                        .is_null(),
                )
                .and_where(Expr::col(("o", "status")).eq("active"))
                .order_by(("l", "created"), Order::Asc)
                .limit(1),
            |r| Ok((r.get(0)?, r.get(1)?)),
        )?;
        Ok(row.and_then(|(title, org)| {
            let title = title
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty());
            let org = (!org.trim().is_empty()).then(|| org.trim().to_string());
            match (title, org) {
                (Some(t), Some(o)) => Some(format!("{t} · {o}")),
                (Some(t), None) => Some(t),
                (None, Some(o)) => Some(o),
                (None, None) => None,
            }
        }))
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
