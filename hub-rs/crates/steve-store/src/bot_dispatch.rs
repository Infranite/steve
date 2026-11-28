//! Workspace dispatch policy: a session of an item runs on the first eligible Node of its Bot's
//! ordered execution Nodes, narrowed by the item's Team. Candidate evidence never writes an
//! authorization.
use rusqlite::{Connection, OptionalExtension, params};
use sea_query::{Expr, ExprTrait, JoinType, OnConflict, Order, Query};
use serde::{Deserialize, Serialize};
use steve_types::{
    BotPlacement, BotProposal, ConfigEvent, NoExecutorPolicy, PlacementBlocked, WorkEvent,
};

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

/// Workspace setting key controlling what happens when no eligible executor Bot exists.
pub const NO_EXECUTOR_SETTING: &str = "noExecutor";

pub fn no_executor_policy(r: &mut impl Read, ws: &str) -> Result<NoExecutorPolicy> {
    let raw: Option<String> = r.one(
        Query::select()
            .column("value")
            .from("settings")
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("key").eq(NO_EXECUTOR_SETTING)),
        |row| row.get(0),
    )?;
    let parsed = raw.and_then(|v| {
        serde_json::from_str::<NoExecutorPolicy>(&v)
            .ok()
            .or_else(|| NoExecutorPolicy::parse(v.trim_matches('"')))
    });
    Ok(parsed.unwrap_or_default())
}

pub fn set_no_executor_policy(
    w: &mut W<'_>,
    ws: &str,
    policy: NoExecutorPolicy,
    actor: &str,
) -> Result<()> {
    let value = serde_json::to_string(&policy)?;
    w.exec(
        Query::insert()
            .into_table("settings")
            .columns(["workspace_id", "key", "value", "updated"])
            .values_panic([
                ws.into(),
                NO_EXECUTOR_SETTING.into(),
                value.into(),
                crate::now().into(),
            ])
            .on_conflict(
                OnConflict::columns(["workspace_id", "key"])
                    .update_columns(["value", "updated"])
                    .to_owned(),
            ),
    )?;
    w.audit(
        ws,
        actor,
        "settings.update",
        ws,
        &serde_json::json!({ NO_EXECUTOR_SETTING: policy.as_str() }),
    )?;
    w.emit(ConfigEvent::Settings {
        ws: ws.into(),
        id: ws.into(),
    });
    Ok(())
}

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
    let row: Option<(String, String)> = c
        .query_row(
            "SELECT d.team,d.source FROM item_team_snapshots d WHERE d.item_id=?1",
            [item],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
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

/// Include a newly confirmed Team member in a waiting item's snapshot for that same Team.
pub fn add_member_to_snapshot(
    w: &mut crate::W<'_>,
    item: &str,
    team_id: &str,
    bot_id: &str,
) -> Result<bool> {
    let Some((mut team, _)) = item_team_on(w.conn, item)? else {
        return Ok(false);
    };
    if team.id != team_id || team.member(bot_id).is_some() {
        return Ok(false);
    }
    team.members.push(Member {
        bot: bot_id.to_owned(),
        nodes: None,
    });
    w.exec(
        Query::update()
            .table("item_team_snapshots")
            .value("team", serde_json::to_string(&team)?)
            .and_where(Expr::col("item_id").eq(item)),
    )?;
    Ok(true)
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

fn is_main_bot_on(c: &Connection, ws: &str, bot: &str) -> Result<bool> {
    Ok(c.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM bots b
             WHERE b.id = ?1 AND b.main = 1 AND b.deleted = 0
               AND (b.workspace_id = ?2
                    OR EXISTS(SELECT 1 FROM bot_workspaces w WHERE w.bot_id = b.id AND w.workspace_id = ?2))
        )",
        params![bot, ws],
        |r| r.get(0),
    )?)
}

fn scope_for_on(c: &Connection, ws: &str, team: Option<&Team>, bot: &str) -> Result<Scope> {
    let bound = bindings_on(c, ws, bot)?;
    let Some(team) = team else {
        return Ok(Scope::Nodes(bound));
    };
    let Some(member) = team.member(bot) else {
        // Steve (main bot) is the workspace coordinator, never a Team member, yet may still
        // execute when explicitly picked or when taking over via CoordinatorDeferred.
        if is_main_bot_on(c, ws, bot)? {
            return Ok(Scope::Nodes(bound));
        }
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

/// Hard placement constraints persisted across async dispatch and waiting retries.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchConstraints {
    pub required_node: Option<String>,
    pub required_specialties: Vec<String>,
    /// True when the caller explicitly chose `project` (including explicit `None` = no project);
    /// false when `project` was omitted so a chosen Bot's `default_project` should apply.
    pub project_explicit: bool,
}

impl Default for DispatchConstraints {
    fn default() -> Self {
        Self {
            required_node: None,
            required_specialties: Vec::new(),
            project_explicit: true,
        }
    }
}

/// Durable async bot-dispatch job for an unassigned item waiting on Steve to choose an executor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchJob {
    pub item_id: String,
    pub workspace_id: String,
    pub state: String,
    pub attempt_id: Option<String>,
    pub required_node: Option<String>,
    pub required_specialties: Vec<String>,
    pub project_explicit: bool,
    pub created: i64,
    pub updated: i64,
}

impl DispatchJob {
    pub fn constraints(&self) -> DispatchConstraints {
        DispatchConstraints {
            required_node: self.required_node.clone(),
            required_specialties: self.required_specialties.clone(),
            project_explicit: self.project_explicit,
        }
    }
}

pub fn enqueue_dispatch(
    w: &mut W<'_>,
    ws: &str,
    item_id: &str,
    constraints: &DispatchConstraints,
) -> Result<()> {
    let ts = crate::now();
    let spec_json = serde_json::to_string(&constraints.required_specialties)?;
    w.exec(
        Query::insert()
            .into_table("item_dispatch_jobs")
            .columns([
                "item_id",
                "workspace_id",
                "state",
                "attempt_id",
                "required_node",
                "required_specialties",
                "project_explicit",
                "created",
                "updated",
            ])
            .values_panic([
                item_id.into(),
                ws.into(),
                "pending".into(),
                Option::<String>::None.into(),
                constraints.required_node.clone().into(),
                spec_json.into(),
                i64::from(constraints.project_explicit).into(),
                ts.into(),
                ts.into(),
            ])
            .on_conflict(
                OnConflict::column("item_id")
                    .update_columns([
                        "workspace_id",
                        "state",
                        "attempt_id",
                        "required_node",
                        "required_specialties",
                        "project_explicit",
                        "updated",
                    ])
                    .to_owned(),
            ),
    )?;
    w.emit(WorkEvent::Item {
        ws: ws.into(),
        id: item_id.into(),
    });
    Ok(())
}

fn next_pending_dispatch(
    r: &mut impl Read,
) -> Result<Option<(String, String, Option<String>, String, bool, i64)>> {
    r.one(
        Query::select()
            .columns([
                "item_id",
                "workspace_id",
                "required_node",
                "required_specialties",
                "project_explicit",
                "created",
            ])
            .from("item_dispatch_jobs")
            .and_where(Expr::col("state").eq("pending"))
            .order_by("created", Order::Asc)
            .order_by("item_id", Order::Asc)
            .limit(1),
        |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get::<i64>(4)? != 0,
                row.get(5)?,
            ))
        },
    )
}

pub fn claim_next_dispatch(w: &mut W<'_>, attempt_id: &str) -> Result<Option<DispatchJob>> {
    let Some((item_id, workspace_id, required_node, spec_json, project_explicit, created)) =
        next_pending_dispatch(w)?
    else {
        return Ok(None);
    };
    let ts = crate::now();
    let updated = w.exec(
        Query::update()
            .table("item_dispatch_jobs")
            .values([
                ("state", "running".into()),
                ("attempt_id", Some(attempt_id.to_owned()).into()),
                ("updated", ts.into()),
            ])
            .and_where(Expr::col("item_id").eq(&item_id))
            .and_where(Expr::col("state").eq("pending")),
    )?;
    if updated != 1 {
        return Ok(None);
    }
    let required_specialties: Vec<String> = serde_json::from_str(&spec_json).unwrap_or_default();
    Ok(Some(DispatchJob {
        item_id,
        workspace_id,
        state: "running".into(),
        attempt_id: Some(attempt_id.to_owned()),
        required_node,
        required_specialties,
        project_explicit,
        created,
        updated: ts,
    }))
}

/// Reset any `running` dispatch rows left over from a prior Hub process back to `pending`.
pub fn reset_running_dispatches(w: &mut W<'_>) -> Result<u64> {
    let ts = crate::now();
    w.exec(
        Query::update()
            .table("item_dispatch_jobs")
            .values([
                ("state", "pending".into()),
                ("attempt_id", Option::<String>::None.into()),
                ("updated", ts.into()),
            ])
            .and_where(Expr::col("state").eq("running")),
    )
}

/// Complete (delete) a running dispatch job only if `attempt_id` still matches the active claim.
pub fn finish_dispatch_if_matching(
    w: &mut W<'_>,
    ws: &str,
    item_id: &str,
    attempt_id: &str,
) -> Result<bool> {
    let deleted = w.exec(
        Query::delete()
            .from_table("item_dispatch_jobs")
            .and_where(Expr::col("item_id").eq(item_id))
            .and_where(Expr::col("attempt_id").eq(attempt_id)),
    )?;
    if deleted > 0 {
        w.emit(WorkEvent::Item {
            ws: ws.into(),
            id: item_id.into(),
        });
        Ok(true)
    } else {
        Ok(false)
    }
}

/// Cancel any pending or running dispatch job for `item_id` (e.g. owner reassigned, archived, or closed).
pub fn cancel_dispatch(w: &mut W<'_>, ws: &str, item_id: &str) -> Result<bool> {
    let deleted = w.exec(
        Query::delete()
            .from_table("item_dispatch_jobs")
            .and_where(Expr::col("item_id").eq(item_id)),
    )?;
    if deleted > 0 {
        w.emit(WorkEvent::Item {
            ws: ws.into(),
            id: item_id.into(),
        });
        Ok(true)
    } else {
        Ok(false)
    }
}

pub fn is_dispatching(r: &mut impl Read, item_id: &str) -> Result<bool> {
    Ok(r.connection().query_row(
        "SELECT EXISTS(SELECT 1 FROM item_dispatch_jobs WHERE item_id = ?1)",
        [item_id],
        |row| row.get::<_, bool>(0),
    )?)
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlacementWait {
    pub item_id: String,
    pub title: String,
    pub blocked: PlacementBlocked,
    pub proposal_id: Option<String>,
    pub constraints: DispatchConstraints,
    pub created: i64,
}

pub fn record_wait(
    w: &mut W<'_>,
    ws: &str,
    item: &str,
    blocked: &PlacementBlocked,
    proposal_id: Option<&str>,
    constraints: &DispatchConstraints,
) -> Result<()> {
    let spec_json = serde_json::to_string(&constraints.required_specialties)?;
    w.exec(
        Query::insert()
            .into_table("item_placement_waits")
            .columns([
                "item_id",
                "workspace_id",
                "blocked",
                "proposal_id",
                "required_node",
                "required_specialties",
                "project_explicit",
                "created",
            ])
            .values_panic([
                item.into(),
                ws.into(),
                serde_json::to_string(blocked)?.into(),
                proposal_id.into(),
                constraints.required_node.clone().into(),
                spec_json.into(),
                i64::from(constraints.project_explicit).into(),
                crate::now().into(),
            ])
            .on_conflict(
                OnConflict::column("item_id")
                    .update_columns([
                        "blocked",
                        "proposal_id",
                        "required_node",
                        "required_specialties",
                        "project_explicit",
                    ])
                    .to_owned(),
            ),
    )?;
    Ok(())
}

pub fn set_wait_proposal(w: &mut W<'_>, item: &str, proposal_id: &str) -> Result<()> {
    w.exec(
        Query::update()
            .table("item_placement_waits")
            .value("proposal_id", proposal_id)
            .and_where(Expr::col("item_id").eq(item)),
    )?;
    Ok(())
}

pub fn clear_wait(w: &mut W<'_>, item: &str) -> Result<bool> {
    Ok(w.exec(
        Query::delete()
            .from_table("item_placement_waits")
            .and_where(Expr::col("item_id").eq(item)),
    )? > 0)
}

pub fn waiting_items(r: &mut impl Read, ws: &str) -> Result<Vec<PlacementWait>> {
    r.all(
        Query::select()
            .columns([
                ("w", "item_id"),
                ("i", "title"),
                ("w", "blocked"),
                ("w", "proposal_id"),
                ("w", "required_node"),
                ("w", "required_specialties"),
                ("w", "project_explicit"),
                ("w", "created"),
            ])
            .from_as("item_placement_waits", "w")
            .join_as(
                JoinType::InnerJoin,
                "items",
                "i",
                Expr::col(("i", "id")).eq(Expr::col(("w", "item_id"))),
            )
            .and_where(Expr::col(("w", "workspace_id")).eq(ws))
            .and_where(Expr::col(("i", "archived")).eq(false))
            .and_where(
                Expr::col(("i", "bot_id"))
                    .is_null()
                    .or(Expr::col(("i", "bot_id")).eq("")),
            )
            .and_where(Expr::col(("i", "phase")).is_in(["ready", "active", "paused"]))
            .order_by(("w", "created"), Order::Asc)
            .order_by(("i", "no"), Order::Asc),
        |row| {
            let spec_json: String = row.get(5)?;
            let required_specialties: Vec<String> =
                serde_json::from_str(&spec_json).unwrap_or_default();
            Ok(PlacementWait {
                item_id: row.get(0)?,
                title: row.get(1)?,
                blocked: row.get::<Json<PlacementBlocked>>(2)?.0,
                proposal_id: row.get(3)?,
                constraints: DispatchConstraints {
                    required_node: row.get(4)?,
                    required_specialties,
                    project_explicit: row.get::<i64>(6)? != 0,
                },
                created: row.get(7)?,
            })
        },
    )
}

/// An item whose placement failed before there was any execution session.
pub fn creation_blocked(
    r: &mut impl Read,
    ws: &str,
    item: &str,
) -> Result<Option<PlacementBlocked>> {
    if let Some(wait) = r.one(
        Query::select()
            .column("blocked")
            .from("item_placement_waits")
            .and_where(Expr::col("item_id").eq(item))
            .and_where(Expr::col("workspace_id").eq(ws)),
        |row| Ok(row.get::<Json<PlacementBlocked>>(0)?.0),
    )? {
        return Ok(Some(wait));
    }
    let blocked = Expr::cust("json_extract(detail, '$.placement_blocked')");
    r.one(
        Query::select()
            .expr(blocked.clone())
            .from("audit")
            .and_where(Expr::col("target").eq(item))
            .and_where(Expr::col("action").eq("item.create"))
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(blocked.is_not_null())
            .limit(1),
        |row| Ok(row.get::<Json<PlacementBlocked>>(0)?.0),
    )
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

pub fn for_session(r: &mut impl Read, session: &str) -> Result<Option<BotPlacement>> {
    r.one(
        Query::select()
            .column("fact")
            .from("bot_placements")
            .and_where(Expr::col("session_id").eq(session))
            .and_where(Expr::col("fact").is_not_null()),
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

pub fn proposal_by_id(r: &mut impl Read, ws: &str, id: &str) -> Result<Option<Proposal>> {
    r.one(
        Query::select()
            .columns(["draft", "project_id", "confirmed_bot"])
            .from("bot_proposals")
            .and_where(Expr::col("id").eq(id))
            .and_where(Expr::col("workspace_id").eq(ws)),
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BotRecentItem {
    pub id: String,
    pub title: String,
    pub outcome: String,
    pub updated: i64,
}

#[derive(Debug, Clone)]
pub struct WorkspaceBotFacts {
    pub default_team: Option<Team>,
    pub teams: Vec<Team>,
    pub visible_nodes: std::collections::BTreeSet<String>,
    pub projects: std::collections::HashMap<String, String>,
    pub default_projects_by_bot: std::collections::HashMap<String, String>,
    pub bots: Vec<crate::Bot>,
    pub bindings_by_bot: std::collections::HashMap<String, Vec<String>>,
    pub active_items_by_bot: std::collections::HashMap<String, usize>,
    pub running_sessions_by_bot: std::collections::HashMap<String, usize>,
    pub recent_items_by_bot: std::collections::HashMap<String, Vec<BotRecentItem>>,
}

/// Read all workspace Bot facts for `steve_bots_list` in a single consistent SQLite snapshot
/// using aggregate queries and a single batch bindings lookup (no full-table scan or N+1).
pub fn workspace_bot_facts(
    r: &mut impl crate::Read,
    ws: &str,
    max_recent: usize,
) -> Result<WorkspaceBotFacts> {
    let started_tx = if r.connection().is_autocommit() {
        r.connection().execute_batch("BEGIN DEFERRED")?;
        true
    } else {
        false
    };
    let result = (|| -> Result<WorkspaceBotFacts> {
        let default_team = selected(r, ws, None)?;
        let teams = teams(r, ws)?;
        let projects: std::collections::HashMap<String, String> =
            crate::projects::active_names(r, ws)?.into_iter().collect();
        let mut visible_nodes = std::collections::BTreeSet::from(["local".to_string()]);
        for n in crate::node_directory::workspace_candidates(r, ws)? {
            visible_nodes.insert(n.id);
        }
        let c = r.connection();
        let bots: Vec<crate::Bot> = {
            let mut stmt = c.prepare(
                "SELECT b.*
                   FROM bots b
                   JOIN bot_workspaces m ON m.bot_id = b.id
                  WHERE m.workspace_id = ?1 AND b.deleted = 0
                  ORDER BY b.main ASC, b.id ASC",
            )?;
            let rows = stmt.query_map([ws], crate::Bot::from_row)?;
            rows.collect::<rusqlite::Result<Vec<_>>>()?
        };
        let default_projects_by_bot: std::collections::HashMap<String, String> = {
            let mut stmt = c.prepare(
                "SELECT bot_id, default_project
                   FROM bot_workspaces
                  WHERE workspace_id = ?1
                    AND default_project IS NOT NULL
                    AND default_project <> ''",
            )?;
            let rows = stmt.query_map([ws], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        let raw_bindings: std::collections::HashMap<String, Vec<String>> = {
            let mut stmt =
                c.prepare("SELECT bot_id, nodes FROM bot_node_bindings WHERE workspace_id = ?1")?;
            let rows = stmt.query_map([ws], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })?;
            let mut map = std::collections::HashMap::new();
            for row in rows {
                let (bot_id, json_nodes) = row?;
                let nodes: Vec<String> = serde_json::from_str(&json_nodes)?;
                map.insert(bot_id, nodes);
            }
            map
        };
        let mut bindings_by_bot = std::collections::HashMap::with_capacity(bots.len());
        for bot in &bots {
            let raw = raw_bindings
                .get(&bot.id)
                .cloned()
                .unwrap_or_else(|| DEFAULT_NODES.iter().map(|n| (*n).to_string()).collect());
            let visible: Vec<String> = raw
                .into_iter()
                .filter(|id| visible_nodes.contains(id))
                .collect();
            bindings_by_bot.insert(bot.id.clone(), visible);
        }
        let active_items_by_bot: std::collections::HashMap<String, usize> = {
            let mut stmt = c.prepare(
                "SELECT bot_id, COUNT(*)
                   FROM items
                  WHERE workspace_id = ?1
                    AND kind = 'task'
                    AND archived = 0
                    AND phase <> 'closed'
                    AND bot_id IS NOT NULL
                    AND bot_id <> ''
                  GROUP BY bot_id",
            )?;
            let rows = stmt.query_map([ws], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    usize::try_from(row.get::<_, i64>(1)?).unwrap_or(0),
                ))
            })?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        let running_sessions_by_bot: std::collections::HashMap<String, usize> = {
            let mut stmt = c.prepare(
                "SELECT bot_id, COUNT(*)
                   FROM sessions
                  WHERE workspace_id = ?1
                    AND status IN ('running', 'starting')
                    AND bot_id IS NOT NULL
                    AND bot_id <> ''
                  GROUP BY bot_id",
            )?;
            let rows = stmt.query_map([ws], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    usize::try_from(row.get::<_, i64>(1)?).unwrap_or(0),
                ))
            })?;
            rows.collect::<rusqlite::Result<_>>()?
        };
        let mut recent_items_by_bot: std::collections::HashMap<String, Vec<BotRecentItem>> =
            std::collections::HashMap::new();
        if max_recent > 0 {
            let mut stmt = c.prepare(
                "SELECT bot_id, id, title, outcome, updated
                   FROM (
                     SELECT bot_id, id, title, outcome, updated,
                            ROW_NUMBER() OVER (
                              PARTITION BY bot_id
                              ORDER BY updated DESC, id ASC
                            ) AS rn
                       FROM items
                      WHERE workspace_id = ?1
                        AND kind = 'task'
                        AND phase = 'closed'
                        AND outcome IN ('completed', 'failed')
                        AND bot_id IS NOT NULL
                        AND bot_id <> ''
                   )
                  WHERE rn <= ?2
                  ORDER BY bot_id ASC, rn ASC",
            )?;
            let rows = stmt.query_map(params![ws, max_recent as i64], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    BotRecentItem {
                        id: row.get(1)?,
                        title: row.get(2)?,
                        outcome: row.get(3)?,
                        updated: row.get(4)?,
                    },
                ))
            })?;
            for row in rows {
                let (bot_id, item) = row?;
                recent_items_by_bot.entry(bot_id).or_default().push(item);
            }
        }
        Ok(WorkspaceBotFacts {
            default_team,
            teams,
            visible_nodes,
            projects,
            default_projects_by_bot,
            bots,
            bindings_by_bot,
            active_items_by_bot,
            running_sessions_by_bot,
            recent_items_by_bot,
        })
    })();
    if started_tx {
        let _ = r.connection().execute_batch("ROLLBACK");
    }
    result
}
