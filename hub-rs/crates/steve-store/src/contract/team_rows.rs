use sea_query::{
    CommonTableExpression, Expr, ExprTrait, Func, JoinType, OnConflict, Order, Query, UnionType,
    WithClause,
};

use super::party_rows::{PARTY_COLUMNS, party};
use crate::base::Viewer;
use crate::{Contacts, Party, Read, Result, W, now};

pub struct Teams<'a, C>(pub &'a mut C);

/// A `team_details` row.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Details {
    /// `chart` (组织架构, synced) | `virtual` (虚拟团队, yours)
    pub kind: String,
    pub parent_id: Option<String>,
    pub org_id: Option<String>,
    pub purpose: String,
    pub space_id: Option<String>,
    pub sync_connection: Option<String>,
    pub external_id: Option<String>,
    pub synced: Option<i64>,
}

impl Details {
    pub fn is_chart(&self) -> bool {
        self.kind == "chart"
    }
}

const DETAILS: [&str; 8] = [
    "kind",
    "parent_id",
    "org_id",
    "purpose",
    "space_id",
    "sync_connection",
    "external_id",
    "synced",
];

fn details(row: &super::Row<'_>, offset: usize) -> Result<Details> {
    Ok(Details {
        kind: row
            .get::<Option<String>>(offset)?
            .unwrap_or_else(|| "virtual".into()),
        parent_id: row.get(offset + 1)?,
        org_id: row.get(offset + 2)?,
        purpose: row.get::<Option<String>>(offset + 3)?.unwrap_or_default(),
        space_id: row.get(offset + 4)?,
        sync_connection: row.get(offset + 5)?,
        external_id: row.get(offset + 6)?,
        synced: row.get(offset + 7)?,
    })
}

#[derive(Debug, Clone, PartialEq)]
pub struct TeamMemberRow {
    pub contact: Party,
    pub role: String,
    pub source: String,
    pub joined: i64,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct TeamRef {
    pub id: String,
    pub name: String,
    pub kind: String,
    pub role: String,
}

#[derive(serde::Serialize)]
pub struct MembershipRow {
    #[serde(rename = "team_id")]
    pub team: String,
    pub role: String,
    pub source: String,
    pub joined: i64,
}

pub struct SyncedTeam {
    pub party: Party,
    pub external_id: String,
    pub parent_id: Option<String>,
}

impl<C: Read> Teams<'_, C> {
    pub fn parties_in_space(&mut self, ws: &str, space: &str) -> Result<Vec<String>> {
        self.0.all(
            Query::select()
                .column("party_id")
                .from("team_details")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("space_id").eq(space))
                .order_by("party_id", sea_query::Order::Asc),
            |row| row.get(0),
        )
    }

    pub fn details(&mut self, team: &str) -> Result<Details> {
        Ok(self
            .0
            .one(
                Query::select()
                    .from_as("parties", "p")
                    .columns(DETAILS.map(|col| ("d", col)))
                    .join_as(
                        JoinType::LeftJoin,
                        "team_details",
                        "d",
                        Expr::col(("d", "party_id")).equals(("p", "id")),
                    )
                    .and_where(Expr::col(("p", "id")).eq(team)),
                |row| details(row, 0),
            )?
            .unwrap_or_default())
    }

    pub fn list(&mut self, viewer: &Viewer) -> Result<Vec<(Party, Details)>> {
        let mut query = Query::select();
        query
            .from_as("parties", "p")
            .columns(PARTY_COLUMNS.map(|col| ("p", col)))
            .columns(DETAILS.map(|col| ("d", col)))
            .join_as(
                JoinType::LeftJoin,
                "team_details",
                "d",
                Expr::col(("d", "party_id")).equals(("p", "id")),
            )
            .and_where(Expr::col(("p", "workspace_id")).eq(viewer.ws.as_str()))
            .and_where(Expr::col(("p", "type")).eq("team"))
            .and_where(Expr::col(("p", "status")).eq("active"))
            .order_by(("p", "name"), Order::Asc)
            .order_by(("p", "id"), Order::Asc);
        if viewer.is_service() {
            query.and_where(Expr::col(("p", "visibility")).eq("workspace"));
        }
        self.0.all(&query, |row| {
            Ok((party(row)?, details(row, PARTY_COLUMNS.len())?))
        })
    }

    pub fn chart(&mut self, ws: &str, connection: &str) -> Result<Vec<SyncedTeam>> {
        self.0.all(
            Query::select()
                .from_as("team_details", "d")
                .columns(PARTY_COLUMNS.map(|col| ("p", col)))
                .columns([("d", "external_id"), ("d", "parent_id")])
                .join_as(
                    JoinType::InnerJoin,
                    "parties",
                    "p",
                    Expr::col(("p", "id")).equals(("d", "party_id")),
                )
                .and_where(Expr::col(("d", "workspace_id")).eq(ws))
                .and_where(Expr::col(("d", "kind")).eq("chart"))
                .and_where(Expr::col(("d", "sync_connection")).eq(connection))
                .and_where(Expr::col(("d", "external_id")).is_not_null())
                .and_where(Expr::col(("p", "status")).is_in(["active", "dissolved"]))
                .order_by(("p", "created"), Order::Asc)
                .order_by(("p", "id"), Order::Asc),
            |row| {
                Ok(SyncedTeam {
                    party: party(row)?,
                    external_id: row.get(PARTY_COLUMNS.len())?,
                    parent_id: row.get(PARTY_COLUMNS.len() + 1)?,
                })
            },
        )
    }

    pub fn members(&mut self, ws: &str, team: &str) -> Result<Vec<(String, String, String)>> {
        let order = self.0.db().dialect().insertion_order(None);
        self.0.all(
            Query::select()
                .from("team_members")
                .columns(["contact_id", "role", "source"])
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("team_id").eq(team))
                .order_by("joined", Order::Asc)
                .order_by_expr(order, Order::Asc),
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
    }

    pub fn memberships(&mut self, ws: &str, contact: &str) -> Result<Vec<MembershipRow>> {
        self.0.all(
            Query::select()
                .from("team_members")
                .columns(["team_id", "role", "source", "joined"])
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("contact_id").eq(contact))
                .order_by("team_id", Order::Asc),
            |r| {
                Ok(MembershipRow {
                    team: r.get(0)?,
                    role: r.get(1)?,
                    source: r.get(2)?,
                    joined: r.get(3)?,
                })
            },
        )
    }

    pub fn has_member(&mut self, team: &str, contact: &str) -> Result<bool> {
        Ok(self
            .0
            .one(
                Query::select()
                    .from("team_members")
                    .column("contact_id")
                    .and_where(Expr::col("team_id").eq(team))
                    .and_where(Expr::col("contact_id").eq(contact))
                    .limit(1),
                |_| Ok(()),
            )?
            .is_some())
    }

    pub fn of_member(&mut self, viewer: &Viewer, contact: &str) -> Result<Vec<TeamRef>> {
        let mut q = Query::select();
        q.from_as("team_members", "m")
            .columns([("p", "id"), ("p", "name")])
            .expr(Func::coalesce([
                Expr::col(("d", "kind")),
                Expr::Constant("virtual".into()),
            ]))
            .column(("m", "role"))
            .join_as(
                JoinType::InnerJoin,
                "parties",
                "p",
                Expr::col(("p", "id")).equals(("m", "team_id")),
            )
            .join_as(
                JoinType::LeftJoin,
                "team_details",
                "d",
                Expr::col(("d", "party_id")).equals(("p", "id")),
            )
            .and_where(Expr::col(("m", "workspace_id")).eq(&viewer.ws))
            .and_where(Expr::col(("m", "contact_id")).eq(contact))
            .and_where(Expr::col(("p", "workspace_id")).eq(&viewer.ws))
            .and_where(Expr::col(("p", "status")).eq("active"))
            .order_by(("p", "name"), Order::Asc)
            .order_by(("p", "id"), Order::Asc);
        if viewer.is_service() {
            q.and_where(Expr::col(("p", "visibility")).eq("workspace"));
        }
        self.0.all(&q, |row| {
            Ok(TeamRef {
                id: row.get(0)?,
                name: row.get(1)?,
                kind: row.get(2)?,
                role: row.get(3)?,
            })
        })
    }

    pub fn active_members(&mut self, ws: &str, team: &str) -> Result<Vec<TeamMemberRow>> {
        self.0.all(
            Query::select()
                .from_as("team_members", "m")
                .columns(PARTY_COLUMNS.map(|col| ("p", col)))
                .columns([("m", "role"), ("m", "source"), ("m", "joined")])
                .join_as(
                    JoinType::InnerJoin,
                    "parties",
                    "p",
                    Expr::col(("p", "id")).equals(("m", "contact_id")),
                )
                .and_where(Expr::col(("m", "workspace_id")).eq(ws))
                .and_where(Expr::col(("m", "team_id")).eq(team))
                .and_where(Expr::col(("p", "status")).eq("active"))
                .order_by_expr(
                    Expr::col(("m", "role")).ne(Expr::Constant("lead".into())),
                    Order::Asc,
                )
                .order_by(("m", "joined"), Order::Asc)
                .order_by(("p", "name"), Order::Asc)
                .order_by(("p", "id"), Order::Asc),
            |row| {
                Ok(TeamMemberRow {
                    contact: party(row)?,
                    role: row.get(PARTY_COLUMNS.len())?,
                    source: row.get(PARTY_COLUMNS.len() + 1)?,
                    joined: row.get(PARTY_COLUMNS.len() + 2)?,
                })
            },
        )
    }

    pub fn children(&mut self, viewer: &Viewer, team: &str) -> Result<Vec<String>> {
        let mut query = Query::select();
        query
            .from_as("team_details", "d")
            .column(("p", "id"))
            .join_as(
                JoinType::InnerJoin,
                "parties",
                "p",
                Expr::col(("p", "id")).equals(("d", "party_id")),
            )
            .and_where(Expr::col(("d", "workspace_id")).eq(viewer.ws.as_str()))
            .and_where(Expr::col(("d", "parent_id")).eq(team))
            .and_where(Expr::col(("p", "status")).eq("active"))
            .order_by(("p", "name"), Order::Asc)
            .order_by(("p", "id"), Order::Asc);
        if viewer.is_service() {
            query.and_where(Expr::col(("p", "visibility")).eq("workspace"));
        }
        self.0.all(&query, |row| row.get(0))
    }

    pub fn last_sync(&mut self, ws: &str) -> Result<Option<(String, i64)>> {
        self.0.one(
            Query::select()
                .from("team_details")
                .column("sync_connection")
                .expr_as(Func::max(Expr::col("synced")), "at")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("kind").eq("chart"))
                .and_where(Expr::col("sync_connection").is_not_null())
                .and_where(Expr::col("synced").is_not_null())
                .group_by_col("sync_connection")
                .order_by("at", Order::Desc)
                .limit(1),
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
    }

    pub fn headcount(&mut self, ws: &str, team: &str, subtree: bool) -> Result<i64> {
        let members = active_members(ws, team, subtree);
        let count = Query::select()
            .from_subquery(members, "members")
            .expr(Func::count(Expr::col("contact_id")))
            .take();
        Ok(self.0.one(&count, |row| row.get(0))?.unwrap_or(0))
    }

    pub fn chart_member_ids(&mut self, ws: &str, team: &str) -> Result<Vec<String>> {
        self.0.all(
            active_members(ws, team, true).order_by(("m", "contact_id"), Order::Asc),
            |row| row.get(0),
        )
    }

    pub fn exists(&mut self, ws: &str, id: &str) -> Result<bool> {
        Ok(Contacts::new(self.0)
            .party(id)?
            .is_some_and(|p| p.workspace_id == ws && p.kind == "team" && p.status == "active"))
    }
}

fn update_details(team: &str) -> sea_query::UpdateStatement {
    Query::update()
        .table("team_details")
        .and_where(Expr::col("party_id").eq(team))
        .take()
}

fn active_members(ws: &str, team: &str, subtree: bool) -> sea_query::SelectStatement {
    let mut members = Query::select();
    members
        .from_as("team_members", "m")
        .column(("m", "contact_id"))
        .distinct()
        .join_as(
            JoinType::InnerJoin,
            "parties",
            "p",
            Expr::col(("p", "id")).equals(("m", "contact_id")),
        )
        .and_where(Expr::col(("m", "workspace_id")).eq(ws))
        .and_where(Expr::col(("p", "status")).eq("active"));
    if !subtree {
        members.and_where(Expr::col(("m", "team_id")).eq(team));
        return members;
    }
    let descendants = Query::select()
        .from_as("team_details", "d")
        .column(("d", "party_id"))
        .join(
            JoinType::InnerJoin,
            "sub",
            Expr::col(("sub", "id")).equals(("d", "parent_id")),
        )
        .join_as(
            JoinType::InnerJoin,
            "parties",
            "t",
            Expr::col(("t", "id")).equals(("d", "party_id")),
        )
        .and_where(Expr::col(("d", "workspace_id")).eq(ws))
        .and_where(Expr::col(("t", "status")).eq("active"))
        .take();
    let root = Query::select()
        .from("parties")
        .column("id")
        .and_where(Expr::col("id").eq(team))
        .union(UnionType::Distinct, descendants)
        .take();
    let cte = CommonTableExpression::new()
        .table_name("sub")
        .column("id")
        .query(root)
        .to_owned();
    let with = WithClause::new().recursive(true).cte(cte).to_owned();
    members.join(
        JoinType::InnerJoin,
        "sub",
        Expr::col(("sub", "id")).equals(("m", "team_id")),
    );
    members.with_cte(with);
    members
}

impl Teams<'_, W<'_>> {
    pub fn insert_details(&mut self, ws: &str, team: &str, d: &Details) -> Result<()> {
        self.0.exec(
            Query::insert()
                .into_table("team_details")
                .columns(["party_id", "workspace_id"].into_iter().chain(DETAILS))
                .values_panic([
                    team.into(),
                    ws.into(),
                    d.kind.as_str().into(),
                    d.parent_id.as_deref().into(),
                    d.org_id.as_deref().into(),
                    d.purpose.as_str().into(),
                    d.space_id.as_deref().into(),
                    d.sync_connection.as_deref().into(),
                    d.external_id.as_deref().into(),
                    d.synced.into(),
                ]),
        )?;
        Ok(())
    }

    pub fn update_details(
        &mut self,
        team: &str,
        purpose: Option<&str>,
        space: Option<Option<&str>>,
    ) -> Result<()> {
        let columns = [
            ("purpose", purpose.map(Expr::value)),
            ("space_id", space.map(Expr::value)),
        ];
        if columns.iter().any(|(_, value)| value.is_some()) {
            self.0.exec(
                update_details(team).values(
                    columns
                        .into_iter()
                        .filter_map(|(col, value)| value.map(|expr| (col, expr))),
                ),
            )?;
        }
        Ok(())
    }

    pub fn set_parent(&mut self, team: &str, parent: Option<&str>) -> Result<()> {
        self.0
            .exec(update_details(team).value("parent_id", parent))?;
        Ok(())
    }

    pub fn set_synced(&mut self, team: &str, at: i64) -> Result<()> {
        self.0.exec(update_details(team).value("synced", at))?;
        Ok(())
    }

    pub fn add_member(
        &mut self,
        ws: &str,
        team: &str,
        contact: &str,
        role: &str,
        source: &str,
    ) -> Result<()> {
        self.0.exec(
            Query::insert()
                .into_table("team_members")
                .columns([
                    "workspace_id",
                    "team_id",
                    "contact_id",
                    "role",
                    "source",
                    "joined",
                ])
                .values_panic([
                    ws.into(),
                    team.into(),
                    contact.into(),
                    role.into(),
                    source.into(),
                    now().into(),
                ]),
        )?;
        Ok(())
    }

    pub fn set_member_role(
        &mut self,
        ws: &str,
        team: &str,
        contact: &str,
        role: &str,
    ) -> Result<()> {
        self.0.exec(
            Query::update()
                .table("team_members")
                .value("role", role)
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("team_id").eq(team))
                .and_where(Expr::col("contact_id").eq(contact)),
        )?;
        Ok(())
    }

    pub fn remove_member(&mut self, ws: &str, team: &str, contact: &str) -> Result<()> {
        self.0.exec(
            Query::delete()
                .from_table("team_members")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("team_id").eq(team))
                .and_where(Expr::col("contact_id").eq(contact)),
        )?;
        Ok(())
    }

    pub fn move_member(&mut self, team: &str, from: &str, to: &str) -> Result<()> {
        self.0.exec(
            Query::update()
                .table("team_members")
                .value("contact_id", to)
                .and_where(Expr::col("team_id").eq(team))
                .and_where(Expr::col("contact_id").eq(from)),
        )?;
        Ok(())
    }

    pub fn restore_member(
        &mut self,
        ws: &str,
        contact: &str,
        member: &MembershipRow,
    ) -> Result<()> {
        self.0.exec(
            Query::insert()
                .into_table("team_members")
                .columns([
                    "workspace_id",
                    "team_id",
                    "contact_id",
                    "role",
                    "source",
                    "joined",
                ])
                .values_panic([
                    ws.into(),
                    member.team.as_str().into(),
                    contact.into(),
                    member.role.as_str().into(),
                    member.source.as_str().into(),
                    member.joined.into(),
                ])
                .on_conflict(OnConflict::new().do_nothing().to_owned()),
        )?;
        Ok(())
    }

    pub fn clear_details(&mut self, ws: &str, team: &str) -> Result<()> {
        self.0.exec(
            Query::delete()
                .from_table("team_members")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("team_id").eq(team)),
        )?;
        self.0.exec(
            Query::delete()
                .from_table("team_details")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("party_id").eq(team)),
        )?;
        Ok(())
    }

    pub fn remove_memberships(&mut self, ws: &str, contact: &str) -> Result<usize> {
        Ok(self.0.exec(
            Query::delete()
                .from_table("team_members")
                .and_where(Expr::col("workspace_id").eq(ws))
                .and_where(Expr::col("contact_id").eq(contact)),
        )? as usize)
    }
}

impl<C: Read> Teams<'_, C> {
    pub fn active_ids_for_contact(&mut self, ws: &str, contact: &str) -> Result<Vec<String>> {
        self.0.all(
            Query::select()
                .column(("m", "team_id"))
                .from_as("team_members", "m")
                .join_as(
                    JoinType::InnerJoin,
                    "parties",
                    "t",
                    Expr::col(("t", "id")).equals(("m", "team_id")),
                )
                .and_where(Expr::col(("t", "status")).eq("active"))
                .and_where(Expr::col(("m", "workspace_id")).eq(ws))
                .and_where(Expr::col(("m", "contact_id")).eq(contact))
                .order_by(("m", "team_id"), Order::Asc),
            |row| row.get(0),
        )
    }
}
