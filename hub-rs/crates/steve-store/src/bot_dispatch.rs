//! Workspace dispatch policy. Candidate evidence never writes an authorization.
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{Result, StoreError};

pub const LEGACY: &str = "legacy";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Member {
    pub bot: String,
    /// None: take all of the Bot's bound Nodes in binding order. Some: further restrict.
    pub nodes: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Team {
    pub id: String,
    pub name: String,
    pub members: Vec<Member>,
}

impl Team {
    pub fn member(&self, bot: &str) -> Option<&Member> {
        self.members.iter().find(|m| m.bot == bot)
    }
}

fn parse_team(row: (String, String, String)) -> Result<Team> {
    Ok(Team {
        id: row.0,
        name: row.1,
        members: serde_json::from_str(&row.2)?,
    })
}

pub fn selected(c: &Connection, ws: &str, id: Option<&str>) -> Result<Option<Team>> {
    let row = match id {
        Some(LEGACY) => return Ok(None),
        Some(id) => Some(c.query_row(
            "SELECT id,name,members FROM bot_teams WHERE workspace_id=?1 AND id=?2",
            params![ws, id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        ).optional()?.ok_or(StoreError::NotFound("Bot Team not found in this workspace"))?),
        None => c.query_row(
            "SELECT t.id,t.name,t.members FROM bot_team_defaults d JOIN bot_teams t ON t.id=d.team_id
             WHERE d.workspace_id=?1 AND t.workspace_id=?1",
            [ws], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        ).optional()?,
    };
    row.map(parse_team).transpose()
}

pub fn teams(c: &Connection, ws: &str) -> Result<Vec<Team>> {
    let mut q =
        c.prepare("SELECT id,name,members FROM bot_teams WHERE workspace_id=? ORDER BY name,id")?;
    q.query_map([ws], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
        .map(|r| parse_team(r?))
        .collect()
}

pub fn owner(c: &Connection, ws: &str, user: &str) -> Result<bool> {
    Ok(c.query_row("SELECT EXISTS(SELECT 1 FROM workspace_members WHERE workspace_id=? AND user_id=? AND role='owner')",
        params![ws, user], |r| r.get(0))?)
}

pub fn bindings(c: &Connection, ws: &str, bot: &str) -> Result<Option<Vec<String>>> {
    let row: Option<String> = c
        .query_row(
            "SELECT nodes FROM bot_node_bindings WHERE workspace_id=? AND bot_id=?",
            params![ws, bot],
            |r| r.get(0),
        )
        .optional()?;
    row.map(|s| serde_json::from_str(&s).map_err(Into::into))
        .transpose()
}

pub fn item_team(c: &Connection, item: &str) -> Result<Option<(Team, String)>> {
    // Workflow children are created by their own repository. The parent's snapshot is their ceiling.
    let row: Option<(String, String)> = c
        .query_row(
            "SELECT d.team,d.source FROM item_dispatch d WHERE d.item_id=?1 AND d.team IS NOT NULL",
            [item],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let row = match row {
        Some(row) => Some(row),
        None => c.query_row(
            "SELECT d.team,'inherited' FROM workflow_units u JOIN workflows w ON w.item_id=u.workflow_id
             JOIN item_dispatch d ON d.item_id=w.item_id JOIN items p ON p.id=w.item_id
             JOIN items i ON i.workspace_id=p.workspace_id AND i.id=u.item_id
             WHERE u.item_id=?1 AND d.team IS NOT NULL",
            [item], |r| Ok((r.get(0)?, r.get(1)?)),
        ).optional()?,
    };
    row.map(|(s, source)| Ok((serde_json::from_str(&s)?, source)))
        .transpose()
}

pub fn snapshot(c: &Connection, item: &str, team: &Team, source: &str) -> Result<()> {
    c.execute(
        "INSERT INTO item_dispatch(item_id,team,source) VALUES (?,?,?)",
        params![item, serde_json::to_string(team)?, source],
    )?;
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
pub enum Scope {
    Legacy,
    Restricted(Vec<String>),
    OutsideTeam,
}

/// A Team cannot turn a candidate into an authorization. The current bindings are always the
/// ceiling. When a Team is in use, an unconfigured Bot has no authorized Nodes.
pub fn scope(c: &Connection, ws: &str, item: &str, bot: &str) -> Result<Scope> {
    let bound = bindings(c, ws, bot)?;
    let Some((team, _)) = item_team(c, item)? else {
        return Ok(bound.map_or(Scope::Legacy, Scope::Restricted));
    };
    let Some(member) = team.member(bot) else {
        return Ok(Scope::OutsideTeam);
    };
    Ok(Scope::Restricted(
        bound
            .unwrap_or_default()
            .into_iter()
            .filter(|node| {
                member
                    .nodes
                    .as_ref()
                    .is_none_or(|nodes| nodes.contains(node))
            })
            .collect(),
    ))
}

pub fn decision(c: &Connection, item: &str, value: &Value) -> Result<()> {
    c.execute(
        "INSERT INTO item_dispatch(item_id,last_decision) VALUES (?,?)
               ON CONFLICT(item_id) DO UPDATE SET last_decision=excluded.last_decision",
        params![item, serde_json::to_string(value)?],
    )?;
    Ok(())
}

pub fn last_decision(c: &Connection, item: &str) -> Result<Option<Value>> {
    let row: Option<String> = c
        .query_row(
            "SELECT last_decision FROM item_dispatch WHERE item_id=? AND last_decision IS NOT NULL",
            [item],
            |r| r.get(0),
        )
        .optional()?;
    row.map(|s| serde_json::from_str(&s).map_err(Into::into))
        .transpose()
}

pub fn legacy_spread(c: &Connection, node: &str) -> Result<bool> {
    Ok(c.query_row(
        "SELECT EXISTS(SELECT 1 FROM legacy_spread_nodes WHERE node_id=?)",
        [node],
        |r| r.get(0),
    )?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_narrows_live_bindings_in_bot_priority_order() {
        let dir = tempfile::tempdir().unwrap();
        let store = crate::Store::open(dir.path().join("dispatch.db"), crate::NoPublisher).unwrap();
        store.transaction(|tx| {
            tx.execute_batch("INSERT INTO workspaces(id,name,created) VALUES ('one','One',0),('two','Two',0);
                INSERT INTO bots(id,workspace_id,name,harness,updated)
                    VALUES ('worker','one','Worker','fixture',0);
                INSERT INTO items(id,workspace_id,no,title,status,stage,created,updated)
                    VALUES ('ITEM-1','one',1,'Test','wait','test',0,0);")?;
            assert_eq!(scope(tx, "one", "ITEM-1", "worker")?, Scope::Legacy);
            let team = Team { id:"team".into(), name:"Preset".into(), members:vec![Member {
                bot:"worker".into(), nodes:Some(vec!["a".into(),"b".into(),"candidate".into()]),
            }] };
            tx.execute("INSERT INTO bot_teams(id,workspace_id,name,members) VALUES (?,'one',?,?)",
                params![team.id, team.name, serde_json::to_string(&team.members)?])?;
            tx.execute("INSERT INTO bot_team_defaults(workspace_id,team_id) VALUES ('one',?)", [&team.id])?;
            assert_eq!(selected(tx,"one",None)?, Some(team.clone()));
            assert!(selected(tx,"two",Some(&team.id)).is_err());
            snapshot(tx, "ITEM-1", &team, "default")?;
            assert_eq!(scope(tx, "one", "ITEM-1", "worker")?, Scope::Restricted(vec![]));
            tx.execute("INSERT INTO bot_node_bindings(workspace_id,bot_id,nodes)
                VALUES ('one','worker','[\"b\",\"other\",\"a\"]')", [])?;
            tx.execute("DELETE FROM bot_team_defaults WHERE workspace_id='one'", [])?;
            tx.execute("UPDATE bot_teams SET name='New name',members='[]' WHERE id='team'", [])?;
            assert_eq!(item_team(tx, "ITEM-1")?.unwrap().0, team);
            assert_eq!(scope(tx,"one","ITEM-1","worker")?, Scope::Restricted(vec!["b".into(),"a".into()]));
            assert_eq!(scope(tx,"one","ITEM-1","steve")?, Scope::OutsideTeam);
            tx.execute("UPDATE bot_node_bindings SET nodes='[\"a\"]'", [])?;
            assert_eq!(scope(tx,"one","ITEM-1","worker")?, Scope::Restricted(vec!["a".into()]));
            Ok::<_, StoreError>(())
        }).unwrap();
    }
}
