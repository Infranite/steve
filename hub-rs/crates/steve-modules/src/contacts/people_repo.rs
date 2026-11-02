//! SQL of slice P3 (contacts.md §2.6, §2.8, §3.2, §4): items linked to parties, the workspace's bots for
//! the `@` menu, item timeline lines, and the candidate side of memories (`mem_candidates`, migration 8).
//! Kept apart from [`super::repo`] so the slices of one wave do not edit the same file.

use rusqlite::types::Value as Sql;
use rusqlite::{Connection, OptionalExtension, params};
use steve_store::base::{Viewer, bind};
use steve_store::{Cached, ChangeEvent, Result, Tx, now};

use super::repo::Party;

/// The item ↔ party roles of `links` (§2.3).
pub const ROLES: [&str; 5] = ["raised_by", "related", "waiting_on", "about", "attendee"];

/// The few item columns the contacts module needs.
#[derive(Debug, Clone, PartialEq)]
pub struct ItemHead {
    pub id: String,
    pub workspace_id: String,
    pub title: String,
    pub archived: bool,
}

pub fn item(c: &Connection, id: &str) -> Result<Option<ItemHead>> {
    Ok(
        c.prepare_cached("SELECT id, workspace_id, title, archived FROM items WHERE id = ?")?
            .query_row([id], |r| {
                Ok(ItemHead {
                    id: r.get(0)?,
                    workspace_id: r.get(1)?,
                    title: r.get(2)?,
                    archived: r.get::<_, i64>(3)? != 0,
                })
            })
            .optional()?,
    )
}

/// Active parties of the given types visible to `viewer`, important first, then most recently updated.
/// `confirmed_only` leaves out `unconfirmed` contacts (they live on the “待确认” page).
pub fn parties(
    c: &Connection,
    viewer: &Viewer,
    kinds: &[&str],
    confirmed_only: bool,
) -> Result<Vec<Party>> {
    let scope = viewer.sql("p");
    let mut sql = format!(
        "SELECT p.* FROM parties p WHERE p.workspace_id = ? AND p.status = 'active' AND p.type IN ({}) AND {}",
        vec!["?"; kinds.len()].join(", "),
        scope.sql
    );
    if confirmed_only {
        sql.push_str(" AND COALESCE(p.contact_kind, '') != 'unconfirmed'");
    }
    sql.push_str(" ORDER BY p.important DESC, p.updated DESC, p.id");
    let mut ps: Vec<Sql> = vec![viewer.ws.clone().into()];
    ps.extend(kinds.iter().map(|k| Sql::Text((*k).into())));
    ps.extend(scope.params);
    let mut st = c.prepare(&sql)?;
    let rows = st.query_map(bind(ps), Party::from_row)?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// One item ↔ party link with the item's state.
#[derive(Debug, Clone, PartialEq)]
pub struct ItemLink {
    pub item_id: String,
    pub party_id: String,
    pub role: String,
    pub source: String,
    pub confidence: f64,
    pub created: i64,
    pub archived: bool,
    pub title: String,
}

/// Links from existing items of `ws` to any of `parties`, newest first. `unarchived` drops archived items.
pub fn item_links_to(
    c: &Connection,
    ws: &str,
    parties: &[String],
    unarchived: bool,
) -> Result<Vec<ItemLink>> {
    if parties.is_empty() {
        return Ok(Vec::new());
    }
    let mut sql = format!(
        "SELECT l.subject_id, l.object_id, l.predicate, l.source, l.confidence, l.created, i.archived, i.title
           FROM links l JOIN items i ON i.id = l.subject_id AND i.workspace_id = l.workspace_id
          WHERE l.workspace_id = ? AND l.subject_type = 'item' AND l.object_type = 'party'
            AND l.predicate IN ({}) AND l.object_id IN ({})",
        vec!["?"; ROLES.len()].join(", "),
        vec!["?"; parties.len()].join(", ")
    );
    if unarchived {
        sql.push_str(" AND i.archived = 0");
    }
    sql.push_str(" ORDER BY l.created DESC, l.subject_id, l.predicate");
    let mut ps: Vec<Sql> = vec![ws.to_string().into()];
    ps.extend(ROLES.iter().map(|r| Sql::Text((*r).into())));
    ps.extend(parties.iter().cloned().map(Sql::Text));
    let mut st = c.prepare(&sql)?;
    let rows = st.query_map(bind(ps), |r| {
        Ok(ItemLink {
            item_id: r.get(0)?,
            party_id: r.get(1)?,
            role: r.get(2)?,
            source: r.get(3)?,
            confidence: r.get(4)?,
            created: r.get(5)?,
            archived: r.get::<_, i64>(6)? != 0,
            title: r.get(7)?,
        })
    })?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Direct members of a team plus the members of all its sub-teams (contact ids, distinct).
pub fn team_member_ids(c: &Connection, ws: &str, team: &str) -> Result<Vec<String>> {
    let mut st = c.prepare_cached(
        "WITH RECURSIVE sub(id) AS (
           SELECT ?2
           UNION
           SELECT d.party_id FROM team_details d JOIN sub ON d.parent_id = sub.id
             JOIN parties t ON t.id = d.party_id WHERE t.workspace_id = ?1 AND t.status = 'active')
         SELECT DISTINCT m.contact_id FROM team_members m JOIN sub ON m.team_id = sub.id
          WHERE m.workspace_id = ?1 ORDER BY m.contact_id",
    )?;
    let rows = st.query_map(params![ws, team], |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// Is the team an org-chart team (its count goes over the sub-tree, §2.4)?
pub fn is_chart(c: &Connection, team: &str) -> Result<bool> {
    Ok(
        c.prepare_cached("SELECT 1 FROM team_details WHERE party_id = ? AND kind = 'chart'")?
            .exists([team])?,
    )
}

/// Enabled, non-deleted bots of the workspace, main first then by name: `(id, name)`.
pub fn bots(c: &Connection, ws: &str) -> Result<Vec<(String, String)>> {
    let mut st = c.prepare_cached(
        "SELECT b.id, b.name FROM bots b JOIN bot_workspaces m ON m.bot_id = b.id
          WHERE m.workspace_id = ? AND b.deleted = 0 AND b.enabled = 1 ORDER BY b.main DESC, b.name, b.id",
    )?;
    let rows = st.query_map([ws], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}

/// `timeline.add(item, actor, kind, text)` inside a transaction (the store's own repository takes the
/// write lock and cannot be called here). Publishes `{type:"item"}` like it.
pub fn timeline_add(tx: &Tx, item: &str, actor: &str, kind: &str, text: &str) -> Result<()> {
    tx.run(
        "INSERT INTO timeline (item_id, ts, actor, kind, text) VALUES (?, ?, ?, ?, ?)",
        params![item, now(), actor, kind, text],
    )?;
    tx.publish(ChangeEvent::Item { id: item.into() });
    Ok(())
}

// ---------------------------------------------------------------- candidate memories (knowledge.md §5)

/// The `mem_candidates` row of a candidate written by an agent tool.
pub struct NewCandidate<'a> {
    pub mem_id: &'a str,
    pub origin: &'a str,
    pub origin_id: &'a str,
    pub origin_label: &'a str,
    pub evidence: &'a str,
}

pub fn candidate_insert(tx: &Tx, ws: &str, n: &NewCandidate) -> Result<()> {
    tx.run(
        "INSERT OR IGNORE INTO mem_candidates (mem_id, workspace_id, origin, origin_id, origin_label, evidence, created) VALUES (?, ?, ?, ?, ?, ?, ?)",
        params![n.mem_id, ws, n.origin, n.origin_id, n.origin_label, n.evidence, now()],
    )?;
    Ok(())
}

pub fn candidate_delete(tx: &Tx, mem_id: &str) -> Result<()> {
    tx.run("DELETE FROM mem_candidates WHERE mem_id = ?", [mem_id])?;
    Ok(())
}

/// Open `confirm` decisions about memories in the workspace (at most 20 at a time, knowledge.md §5.2).
pub fn open_mem_confirms(c: &Connection, ws: &str) -> Result<i64> {
    Ok(c.row(
        "SELECT COUNT(*) FROM decisions WHERE workspace_id = ? AND kind = 'confirm' AND subject_type = 'mem' AND status = 'open'",
        [ws],
        |r| r.get(0),
    )?)
}

/// Memories about `(object_type, object_id)` and nothing else.
pub fn mems_only_about(
    c: &Connection,
    ws: &str,
    object_type: &str,
    object_id: &str,
) -> Result<Vec<String>> {
    let mut st = c.prepare_cached(
        "SELECT a.mem_id FROM mem_about a WHERE a.workspace_id = ? AND a.object_type = ? AND a.object_id = ?
           AND NOT EXISTS (SELECT 1 FROM mem_about b WHERE b.mem_id = a.mem_id AND (b.object_type != a.object_type OR b.object_id != a.object_id))
         ORDER BY a.mem_id",
    )?;
    let rows = st.query_map(params![ws, object_type, object_id], |r| r.get(0))?;
    Ok(rows.collect::<rusqlite::Result<Vec<_>>>()?)
}
