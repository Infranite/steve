//! Workspace dispatch policy: a session of an item runs on the first eligible Node of its Bot's
//! ordered execution Nodes, narrowed by the item's Team. Candidate evidence never writes an
//! authorization.
use rusqlite::{Connection, OptionalExtension, params};
use sea_query::{Expr, ExprTrait, Query};
use serde::{Deserialize, Serialize};
use steve_types::{BotPlacement, BotProposal, PlacementBlocked, WorkEvent};

use crate::{Read, W, contract::Json};

use crate::{Result, StoreError};

/// Store trimmed, unique operator labels in their original order and spelling.
pub fn specialties(tags: Vec<String>) -> Vec<String> {
    let mut seen = std::collections::HashSet::new();
    tags.into_iter()
        .filter_map(|tag| {
            let trimmed = tag.trim();
            (!trimmed.is_empty() && seen.insert(trimmed.to_lowercase())).then(|| trimmed.to_owned())
        })
        .collect()
}

/// The Team choice that asks for no Team, even where the workspace has a default one.
pub const NO_TEAM: &str = "none";

/// The execution Nodes of a Bot nobody configured: the Hub's own Node.
pub const DEFAULT_NODES: &[&str] = &["local"];

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

pub fn selected(r: &mut impl crate::Read, ws: &str, id: Option<&str>) -> Result<Option<Team>> {
    use sea_query::JoinType;

    if id == Some(NO_TEAM) {
        return Ok(None);
    }
    let mut query = Query::select();
    query
        .columns([("t", "id"), ("t", "name"), ("t", "members")])
        .from_as("bot_teams", "t")
        .and_where(Expr::col(("t", "workspace_id")).eq(ws));
    match id {
        Some(id) => {
            query.and_where(Expr::col(("t", "id")).eq(id));
        }
        None => {
            query
                .join_as(
                    JoinType::InnerJoin,
                    "bot_team_defaults",
                    "d",
                    Expr::col(("d", "team_id")).eq(Expr::col(("t", "id"))),
                )
                .and_where(Expr::col(("d", "workspace_id")).eq(ws));
        }
    };
    let team = r.one(&query, |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?;
    if id.is_some() && team.is_none() {
        return Err(StoreError::NotFound("Bot Team not found in this workspace"));
    }
    team.map(parse_team).transpose()
}

pub(crate) fn teams_on(c: &Connection, ws: &str) -> Result<Vec<Team>> {
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

/// The Bot's execution Nodes in this workspace, in priority order ([`DEFAULT_NODES`] until
/// configured). An empty list runs nothing.
pub(crate) fn bindings_on(c: &Connection, ws: &str, bot: &str) -> Result<Vec<String>> {
    let row: Option<String> = c
        .query_row(
            "SELECT nodes FROM bot_node_bindings WHERE workspace_id=? AND bot_id=?",
            params![ws, bot],
            |r| r.get(0),
        )
        .optional()?;
    match row {
        Some(s) => Ok(serde_json::from_str(&s)?),
        None => Ok(DEFAULT_NODES.iter().map(|n| n.to_string()).collect()),
    }
}

pub(crate) fn item_team_on(c: &Connection, item: &str) -> Result<Option<(Team, String)>> {
    // Workflow children are created by their own repository. The parent's snapshot is their ceiling.
    let row: Option<(String, String)> = c
        .query_row(
            "SELECT d.team,d.source FROM item_team_snapshots d WHERE d.item_id=?1",
            [item],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let row = match row {
        Some(row) => Some(row),
        None => c.query_row(
            "SELECT d.team,'inherited' FROM workflow_units u JOIN workflows w ON w.item_id=u.workflow_id
             JOIN item_team_snapshots d ON d.item_id=w.item_id JOIN items p ON p.id=w.item_id
             JOIN items i ON i.workspace_id=p.workspace_id AND i.id=u.item_id
             WHERE u.item_id=?1",
            [item], |r| Ok((r.get(0)?, r.get(1)?)),
        ).optional()?,
    };
    row.map(|(s, source)| Ok((serde_json::from_str(&s)?, source)))
        .transpose()
}

pub fn snapshot(w: &mut crate::W<'_>, item: &str, team: &Team, source: &str) -> Result<()> {
    w.exec(
        sea_query::Query::insert()
            .into_table("item_team_snapshots")
            .columns(["item_id", "team", "source"])
            .values_panic([
                item.into(),
                serde_json::to_string(team)?.into(),
                source.into(),
            ]),
    )?;
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
pub enum Scope {
    /// The Nodes the session may run on, in priority order.
    Nodes(Vec<String>),
    OutsideTeam,
}

/// A Team cannot turn a candidate into an authorization. The current bindings are always the
/// ceiling.
pub(crate) fn scope_on(c: &Connection, ws: &str, item: &str, bot: &str) -> Result<Scope> {
    let team = item_team_on(c, item)?.map(|(team, _)| team);
    scope_for_on(c, ws, team.as_ref(), bot)
}

fn scope_for_on(c: &Connection, ws: &str, team: Option<&Team>, bot: &str) -> Result<Scope> {
    let bound = bindings_on(c, ws, bot)?;
    let Some(team) = team else {
        return Ok(Scope::Nodes(bound));
    };
    let Some(member) = team.member(bot) else {
        return Ok(Scope::OutsideTeam);
    };
    Ok(Scope::Nodes(
        bound
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

pub fn scope_for(r: &mut impl Read, ws: &str, team: Option<&Team>, bot: &str) -> Result<Scope> {
    scope_for_on(r.connection(), ws, team, bot)
}

pub fn record(
    w: &mut W<'_>,
    ws: &str,
    item: &str,
    session: Option<&str>,
    fact: std::result::Result<&BotPlacement, &PlacementBlocked>,
) -> Result<()> {
    let (fact, blocked) = match fact {
        Ok(fact) => (Some(serde_json::to_string(fact)?), None),
        Err(blocked) => (None, Some(serde_json::to_string(blocked)?)),
    };
    w.exec(
        sea_query::Query::insert()
            .into_table("bot_placements")
            .columns(["item_id", "session_id", "fact", "blocked"])
            .values_panic([item.into(), session.into(), fact.into(), blocked.into()]),
    )?;
    w.emit(WorkEvent::Item {
        ws: ws.into(),
        id: item.into(),
    });
    if let Some(session) = session {
        w.emit(WorkEvent::Session {
            ws: ws.into(),
            id: session.into(),
            item_id: Some(Some(item.into())),
        });
    }
    Ok(())
}

pub fn initial(r: &mut impl Read, item: &str) -> Result<Option<BotPlacement>> {
    r.one(
        Query::select()
            .column("fact")
            .from("bot_placements")
            .and_where(Expr::col("item_id").eq(item))
            .and_where(Expr::col("session_id").is_null()),
        |row| Ok(row.get::<Json<BotPlacement>>(0)?.0),
    )
}

pub fn refusal(r: &mut impl Read, session: &str) -> Result<Option<PlacementBlocked>> {
    r.one(
        Query::select()
            .column("blocked")
            .from("bot_placements")
            .and_where(Expr::col("session_id").eq(session))
            .and_where(Expr::col("blocked").is_not_null()),
        |row| Ok(row.get::<Json<PlacementBlocked>>(0)?.0),
    )
}

pub struct Proposal {
    pub draft: BotProposal,
    pub project: Option<String>,
    pub confirmed_bot: Option<String>,
}

pub fn save_proposal(
    w: &mut W<'_>,
    ws: &str,
    actor: &str,
    project: Option<&str>,
    draft: &BotProposal,
) -> Result<()> {
    w.exec(
        Query::insert()
            .into_table("bot_proposals")
            .columns([
                "id",
                "workspace_id",
                "actor",
                "project_id",
                "created",
                "draft",
            ])
            .values_panic([
                draft.id.as_str().into(),
                ws.into(),
                actor.into(),
                project.into(),
                crate::now().into(),
                serde_json::to_string(draft)?.into(),
            ]),
    )?;
    Ok(())
}

pub fn proposal(r: &mut impl Read, ws: &str, actor: &str, id: &str) -> Result<Option<Proposal>> {
    r.one(
        Query::select()
            .columns(["draft", "project_id", "confirmed_bot"])
            .from("bot_proposals")
            .and_where(Expr::col("id").eq(id))
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("actor").eq(actor)),
        |row| {
            Ok(Proposal {
                draft: row.get::<Json<BotProposal>>(0)?.0,
                project: row.get(1)?,
                confirmed_bot: row.get(2)?,
            })
        },
    )
}

pub fn confirm(w: &mut W<'_>, id: &str, bot: &str) -> Result<()> {
    let updated = w.exec(
        Query::update()
            .table("bot_proposals")
            .value("confirmed_bot", bot)
            .and_where(Expr::col("id").eq(id))
            .and_where(Expr::col("confirmed_bot").is_null()),
    )?;
    if updated != 1 {
        return Err(StoreError::Conflict(
            "Bot proposal already confirmed".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn snapshot_narrows_live_bindings_in_bot_priority_order() {
        let dir = tempfile::tempdir().unwrap();
        let store =
            crate::Store::open(dir.path().join("steve.sqlite3"), crate::NoPublisher).unwrap();
        store.write(|w| {
            let tx = w.conn;
            tx.execute_batch("INSERT INTO workspaces(id,name,created) VALUES ('one','One',0),('two','Two',0);
                INSERT INTO bots(id,workspace_id,name,harness,updated)
                    VALUES ('worker','one','Worker','fixture',0);
                INSERT INTO items(id, workspace_id, no, title, phase, created, updated) VALUES ('ITEM-1','one',1,'Test','active',0,0);")?;
            assert_eq!(scope(w, "one", "ITEM-1", "worker")?, Scope::Nodes(vec!["local".into()]));
            let team = Team { id:"team".into(), name:"Preset".into(), members:vec![Member {
                bot:"worker".into(), nodes:Some(vec!["a".into(),"b".into(),"candidate".into()]),
            }] };
            tx.execute("INSERT INTO bot_teams(id,workspace_id,name,members) VALUES (?,'one',?,?)",
                params![team.id, team.name, serde_json::to_string(&team.members)?])?;
            tx.execute("INSERT INTO bot_team_defaults(workspace_id,team_id) VALUES ('one',?)", [&team.id])?;
            assert_eq!(selected(w, "one", None)?, Some(team.clone()));
            assert!(selected(w, "two", Some(&team.id)).is_err());
            snapshot(w, "ITEM-1", &team, "default")?;
            assert_eq!(scope(w, "one", "ITEM-1", "worker")?, Scope::Nodes(vec![]));
            tx.execute("INSERT INTO bot_node_bindings(workspace_id,bot_id,nodes)
                VALUES ('one','worker','[\"b\",\"other\",\"a\"]')", [])?;
            tx.execute("DELETE FROM bot_team_defaults WHERE workspace_id='one'", [])?;
            tx.execute("UPDATE bot_teams SET name='New name',members='[]' WHERE id='team'", [])?;
            assert_eq!(item_team(w, "ITEM-1")?.unwrap().0, team);
            assert_eq!(scope(w,"one","ITEM-1","worker")?, Scope::Nodes(vec!["b".into(),"a".into()]));
            assert_eq!(scope(w,"one","ITEM-1","steve")?, Scope::OutsideTeam);
            tx.execute("UPDATE bot_node_bindings SET nodes='[\"a\"]'", [])?;
            assert_eq!(scope(w,"one","ITEM-1","worker")?, Scope::Nodes(vec!["a".into()]));
            Ok::<_, StoreError>(())
        }).unwrap();
    }
}

pub fn teams(r: &mut impl crate::Read, ws: &str) -> Result<Vec<Team>> {
    teams_on(r.connection(), ws)
}

pub fn bindings(r: &mut impl crate::Read, ws: &str, bot: &str) -> Result<Vec<String>> {
    bindings_on(r.connection(), ws, bot)
}

pub fn scope(r: &mut impl crate::Read, ws: &str, item: &str, bot: &str) -> Result<Scope> {
    scope_on(r.connection(), ws, item, bot)
}

pub fn item_team(r: &mut impl crate::Read, item: &str) -> Result<Option<(Team, String)>> {
    item_team_on(r.connection(), item)
}
