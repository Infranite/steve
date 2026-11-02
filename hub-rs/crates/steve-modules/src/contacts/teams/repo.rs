//! SQL of teams (contacts.md §1.2: `team_details`, `team_members`; the team itself is a `parties` row of
//! type `team`). Lists go through a [`Viewer`]; writes take a [`Tx`].

use rusqlite::types::Value as Sql;
use rusqlite::{Connection, OptionalExtension, Row, params};
use steve_store::base::{Viewer, bind};
use steve_store::{Cached, Result, Tx, now};

use super::super::repo::Party;

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

    /// A team row whose details are missing reads as an empty virtual team.
    fn from_row(r: &Row) -> rusqlite::Result<Self> {
        Ok(Self {
            kind: r
                .get::<_, Option<String>>("d_kind")?
                .unwrap_or_else(|| "virtual".into()),
            parent_id: r.get("d_parent_id")?,
            org_id: r.get("d_org_id")?,
            purpose: r.get::<_, Option<String>>("d_purpose")?.unwrap_or_default(),
            space_id: r.get("d_space_id")?,
            sync_connection: r.get("d_sync_connection")?,
            external_id: r.get("d_external_id")?,
            synced: r.get("d_synced")?,
        })
    }
}

/// The detail columns, aliased so they do not clash with `parties.*`.
const DETAIL_COLS: &str = "d.kind AS d_kind, d.parent_id AS d_parent_id, d.org_id AS d_org_id, d.purpose AS d_purpose, d.space_id AS d_space_id, d.sync_connection AS d_sync_connection, d.external_id AS d_external_id, d.synced AS d_synced";

fn team_row(r: &Row) -> rusqlite::Result<(Party, Details)> {
    Ok((Party::from_row(r)?, Details::from_row(r)?))
}

pub fn details(c: &Connection, team: &str) -> Result<Details> {
    let sql = format!(
        "SELECT {DETAIL_COLS} FROM parties p LEFT JOIN team_details d ON d.party_id = p.id WHERE p.id = ?"
    );
    Ok(c.prepare_cached(&sql)?
        .query_row([team], Details::from_row)
        .optional()?
        .unwrap_or_default())
}

pub fn details_insert(tx: &Tx, ws: &str, team: &str, d: &Details) -> Result<()> {
    tx.run(
        "INSERT INTO team_details (party_id, workspace_id, kind, parent_id, org_id, purpose, space_id, sync_connection, external_id, synced)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            team,
            ws,
            d.kind,
            d.parent_id,
            d.org_id,
            d.purpose,
            d.space_id,
            d.sync_connection,
            d.external_id,
            d.synced
        ],
    )?;
    Ok(())
}

/// The locally editable details (`purpose`, `space_id`); `None` = unchanged.
pub fn details_update(
    tx: &Tx,
    team: &str,
    purpose: Option<&str>,
    space_id: Option<Option<&str>>,
) -> Result<()> {
    if let Some(p) = purpose {
        tx.run(
            "UPDATE team_details SET purpose = ? WHERE party_id = ?",
            params![p, team],
        )?;
    }
    if let Some(s) = space_id {
        tx.run(
            "UPDATE team_details SET space_id = ? WHERE party_id = ?",
            params![s, team],
        )?;
    }
    Ok(())
}

/// Active teams of the viewer's workspace they can see, by name.
pub fn teams(c: &Connection, viewer: &Viewer) -> Result<Vec<(Party, Details)>> {
    let scope = viewer.sql("p");
    let sql = format!(
        "SELECT p.*, {DETAIL_COLS} FROM parties p LEFT JOIN team_details d ON d.party_id = p.id
         WHERE p.workspace_id = ? AND p.type = 'team' AND p.status = 'active' AND {}
         ORDER BY p.name, p.id",
        scope.sql
    );
    let mut ps: Vec<Sql> = vec![viewer.ws.clone().into()];
    ps.extend(scope.params);
    let mut st = c.prepare(&sql)?;
    let rows = st.query_map(bind(ps), team_row)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// A `team_members` row joined with the contact.
#[derive(Debug, Clone, PartialEq)]
pub struct Member {
    pub contact: Party,
    /// `lead` | `member`
    pub role: String,
    /// `manual` | `sync` | `chat`
    pub source: String,
    pub joined: i64,
}

/// Direct members of a team that are active contacts, leads first, then by joining order.
pub fn members(c: &Connection, ws: &str, team: &str) -> Result<Vec<Member>> {
    let mut st = c.prepare_cached(
        "SELECT p.*, m.role AS m_role, m.source AS m_source, m.joined AS m_joined FROM team_members m JOIN parties p ON p.id = m.contact_id
         WHERE m.workspace_id = ? AND m.team_id = ? AND p.status = 'active'
         ORDER BY m.role != 'lead', m.joined, p.name, p.id",
    )?;
    let rows = st.query_map(params![ws, team], |r| {
        Ok(Member {
            contact: Party::from_row(r)?,
            role: r.get("m_role")?,
            source: r.get("m_source")?,
            joined: r.get("m_joined")?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// `(contact_id, role)` of every member row, whatever the contact's status (for diffs).
pub fn member_rows(c: &Connection, ws: &str, team: &str) -> Result<Vec<(String, String)>> {
    let mut st = c.prepare_cached(
        "SELECT contact_id, role FROM team_members WHERE workspace_id = ? AND team_id = ? ORDER BY joined, rowid",
    )?;
    let rows = st.query_map(params![ws, team], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn member_add(
    tx: &Tx,
    ws: &str,
    team: &str,
    contact: &str,
    role: &str,
    source: &str,
) -> Result<()> {
    tx.run(
        "INSERT INTO team_members (workspace_id, team_id, contact_id, role, source, joined) VALUES (?, ?, ?, ?, ?, ?)",
        params![ws, team, contact, role, source, now()],
    )?;
    Ok(())
}

pub fn member_role(tx: &Tx, ws: &str, team: &str, contact: &str, role: &str) -> Result<()> {
    tx.run(
        "UPDATE team_members SET role = ? WHERE workspace_id = ? AND team_id = ? AND contact_id = ?",
        params![role, ws, team, contact],
    )?;
    Ok(())
}

pub fn member_remove(tx: &Tx, ws: &str, team: &str, contact: &str) -> Result<()> {
    tx.run(
        "DELETE FROM team_members WHERE workspace_id = ? AND team_id = ? AND contact_id = ?",
        params![ws, team, contact],
    )?;
    Ok(())
}

/// Distinct active contacts of a team; with `subtree`, also of every active team below it (§2.4: an
/// org-chart team counts everyone underneath).
pub fn headcount(c: &Connection, ws: &str, team: &str, subtree: bool) -> Result<i64> {
    if !subtree {
        return Ok(c.row(
            "SELECT COUNT(*) FROM team_members m JOIN parties p ON p.id = m.contact_id
             WHERE m.workspace_id = ? AND m.team_id = ? AND p.status = 'active'",
            params![ws, team],
            |r| r.get(0),
        )?);
    }
    Ok(c.row(
        "WITH RECURSIVE sub(id) AS (
           SELECT ?2
           UNION
           SELECT d.party_id FROM team_details d JOIN sub ON d.parent_id = sub.id
             JOIN parties t ON t.id = d.party_id
            WHERE d.workspace_id = ?1 AND t.status = 'active')
         SELECT COUNT(DISTINCT m.contact_id) FROM team_members m JOIN sub ON m.team_id = sub.id
           JOIN parties p ON p.id = m.contact_id
          WHERE m.workspace_id = ?1 AND p.status = 'active'",
        params![ws, team],
        |r| r.get(0),
    )?)
}

/// Active direct sub-teams of `team` the viewer can see, by name.
pub fn children(c: &Connection, viewer: &Viewer, team: &str) -> Result<Vec<String>> {
    let scope = viewer.sql("p");
    let sql = format!(
        "SELECT p.id FROM team_details d JOIN parties p ON p.id = d.party_id
         WHERE d.workspace_id = ? AND d.parent_id = ? AND p.status = 'active' AND {}
         ORDER BY p.name, p.id",
        scope.sql
    );
    let mut ps: Vec<Sql> = vec![viewer.ws.clone().into(), team.to_string().into()];
    ps.extend(scope.params);
    let mut st = c.prepare(&sql)?;
    let rows = st.query_map(bind(ps), |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// The most recent directory sync of the workspace's org chart: `(connection_id, synced)`.
pub fn last_sync(c: &Connection, ws: &str) -> Result<Option<(String, i64)>> {
    Ok(c.prepare_cached(
        "SELECT d.sync_connection, MAX(d.synced) AS at FROM team_details d JOIN parties p ON p.id = d.party_id
         WHERE d.workspace_id = ? AND d.kind = 'chart' AND d.sync_connection IS NOT NULL AND d.synced IS NOT NULL
         GROUP BY d.sync_connection ORDER BY at DESC LIMIT 1",
    )?
    .query_row([ws], |r| Ok((r.get(0)?, r.get(1)?)))
    .optional()?)
}
