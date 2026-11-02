//! SQL of the directory sync: the org-chart teams of one connection, the few columns the sync owns, and
//! the directory connections in `connections` (migration 5, read-only here except `last_sync`).

use rusqlite::{Connection, OptionalExtension, params};
use steve_store::{Cached, Result, Tx};

use super::super::repo::Party;

/// An org-chart team written by a connection's sync, dissolved ones included (they come back when the
/// department reappears).
#[derive(Debug, Clone)]
pub struct SyncedTeam {
    pub party: Party,
    pub external_id: String,
    pub parent_id: Option<String>,
}

pub fn chart_teams(c: &Connection, ws: &str, connection: &str) -> Result<Vec<SyncedTeam>> {
    let mut st = c.prepare_cached(
        "SELECT p.*, d.external_id AS d_external_id, d.parent_id AS d_parent_id
           FROM team_details d JOIN parties p ON p.id = d.party_id
          WHERE d.workspace_id = ? AND d.kind = 'chart' AND d.sync_connection = ?
            AND d.external_id IS NOT NULL AND p.status IN ('active', 'dissolved')
          ORDER BY p.created, p.id",
    )?;
    let rows = st.query_map(params![ws, connection], |r| {
        Ok(SyncedTeam {
            party: Party::from_row(r)?,
            external_id: r.get("d_external_id")?,
            parent_id: r.get("d_parent_id")?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

pub fn set_parent(tx: &Tx, team: &str, parent: Option<&str>) -> Result<()> {
    tx.run(
        "UPDATE team_details SET parent_id = ? WHERE party_id = ?",
        params![parent, team],
    )?;
    Ok(())
}

pub fn set_synced(tx: &Tx, team: &str, at: i64) -> Result<()> {
    tx.run(
        "UPDATE team_details SET synced = ? WHERE party_id = ?",
        params![at, team],
    )?;
    Ok(())
}

/// A contact created by the sync: `source = sync`, `source_ref` = its directory id.
pub fn set_source(tx: &Tx, party: &str, source: &str, source_ref: &str) -> Result<()> {
    tx.run(
        "UPDATE parties SET source = ?, source_ref = ? WHERE id = ?",
        params![source, source_ref, party],
    )?;
    Ok(())
}

/// `(contact_id, role, source)` of every member row of a team.
pub fn member_rows(c: &Connection, ws: &str, team: &str) -> Result<Vec<(String, String, String)>> {
    let mut st = c.prepare_cached(
        "SELECT contact_id, role, source FROM team_members WHERE workspace_id = ? AND team_id = ? ORDER BY joined, rowid",
    )?;
    let rows = st.query_map(params![ws, team], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// A connection that can read a directory, with what the sync needs about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionRow {
    pub id: String,
    pub workspace_id: String,
    pub adapter: String,
    pub created_by: String,
}

const DIRECTORY_CONNECTIONS: &str = "SELECT id, workspace_id, adapter, created_by FROM connections
  WHERE status = 'ok' AND EXISTS (SELECT 1 FROM json_each(connections.caps) WHERE value = 'directory.read')";

fn connection_row(r: &rusqlite::Row) -> rusqlite::Result<ConnectionRow> {
    Ok(ConnectionRow {
        id: r.get(0)?,
        workspace_id: r.get(1)?,
        adapter: r.get(2)?,
        created_by: r.get(3)?,
    })
}

/// Healthy connections with `directory.read` enabled, of one workspace or of all, oldest first.
pub fn directory_connections(c: &Connection, ws: Option<&str>) -> Result<Vec<ConnectionRow>> {
    let sql = format!(
        "{DIRECTORY_CONNECTIONS} AND (?1 IS NULL OR workspace_id = ?1) ORDER BY created, id"
    );
    let mut st = c.prepare_cached(&sql)?;
    let rows = st.query_map(params![ws], connection_row)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// One directory connection of `ws` (`None` also when it lacks `directory.read` or is not healthy).
pub fn directory_connection(c: &Connection, ws: &str, id: &str) -> Result<Option<ConnectionRow>> {
    let sql = format!("{DIRECTORY_CONNECTIONS} AND workspace_id = ? AND id = ?");
    Ok(c.prepare_cached(&sql)?
        .query_row(params![ws, id], connection_row)
        .optional()?)
}

/// Does `ws` have a connection with this id at all (to tell 404 from “cannot read a directory”)?
pub fn connection_exists(c: &Connection, ws: &str, id: &str) -> Result<bool> {
    Ok(c.row(
        "SELECT EXISTS (SELECT 1 FROM connections WHERE workspace_id = ? AND id = ?)",
        params![ws, id],
        |r| r.get(0),
    )?)
}

pub fn set_last_sync(tx: &Tx, connection: &str, at: i64) -> Result<()> {
    tx.run(
        "UPDATE connections SET last_sync = ? WHERE id = ?",
        params![at, connection],
    )?;
    Ok(())
}

/// Has this connection never finished a sync?
pub fn never_synced(c: &Connection, connection: &str) -> Result<bool> {
    Ok(c.row(
        "SELECT last_sync IS NULL FROM connections WHERE id = ?",
        params![connection],
        |r| r.get::<_, bool>(0),
    )
    .optional()?
    .unwrap_or(false))
}

/// `connections.config.directory` of the workspace's directory connections: `(id, state json)`.
pub fn directory_states(c: &Connection, ws: &str) -> Result<Vec<(String, String)>> {
    let mut st = c.prepare_cached(
        "SELECT id, json_extract(config, '$.directory') FROM connections
          WHERE workspace_id = ? AND json_extract(config, '$.directory') IS NOT NULL ORDER BY created, id",
    )?;
    let rows = st.query_map(params![ws], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}
