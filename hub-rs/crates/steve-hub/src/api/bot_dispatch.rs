//! Workspace owners administer Bot bindings and Team presets. Reading a candidate changes nothing.
use axum::{
    Extension, Json,
    extract::{Path, State},
};
use rusqlite::{OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};
use steve_store::{
    ChangeEvent, Store, StoreError,
    bot_dispatch::{self, LEGACY, Member},
};

use super::{ApiResult, AppState, Ctx, ok};
use crate::error::ApiError;

pub(super) fn store(st: &AppState) -> Option<&Store> {
    st.modules.as_ref().map(|m| &m.store)
}

fn required_store(st: &AppState) -> Result<Store, ApiError> {
    store(st)
        .cloned()
        .ok_or_else(|| ApiError::Json(503, "Bot Team storage unavailable".into()))
}

fn error(e: StoreError) -> ApiError {
    match e {
        StoreError::NotFound(_) => ApiError::not_found(steve_i18n::t!("hub.dispatch.not_found")),
        StoreError::Conflict(s) => ApiError::Json(409, s),
        e => {
            tracing::error!("Bot dispatch: {e}");
            ApiError::internal(steve_i18n::t!("hub.error.generic"))
        }
    }
}

async fn operate(f: impl FnOnce() -> Result<Value, ApiError> + Send + 'static) -> ApiResult {
    let value = tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| ApiError::internal(e.to_string()))??;
    ok(value)
}

fn is_bot(c: &rusqlite::Connection, ws: &str, id: &str) -> steve_store::Result<bool> {
    Ok(c.query_row(
        "SELECT EXISTS(SELECT 1 FROM bots b JOIN bot_workspaces m ON m.bot_id=b.id
          WHERE m.workspace_id=?1 AND b.id=?2 AND b.deleted=0 AND b.main=0)",
        params![ws, id],
        |r| r.get(0),
    )?)
}

pub async fn list(State(st): State<AppState>, Extension(cx): Extension<Ctx>) -> ApiResult {
    let s = required_store(&st)?;
    operate(move || {
        s.reader(|c| Ok(json!({
        "teams":bot_dispatch::teams(c, &cx.ws)?,
        "default":bot_dispatch::selected(c, &cx.ws, None)?.map_or_else(||LEGACY.into(), |t|t.id),
        "editable":bot_dispatch::owner(c, &cx.ws, &cx.me)?,
    }))).map_err(error)
    })
    .await
}

pub async fn candidates(
    State(st): State<AppState>,
    Extension(cx): Extension<Ctx>,
    Path(id): Path<String>,
) -> ApiResult {
    let s = required_store(&st)?;
    let own = s.reader(|c| is_bot(c, &cx.ws, &id)).map_err(error)?;
    if !own {
        return Err(ApiError::not_found(steve_i18n::t!(
            "hub.dispatch.not_found"
        )));
    }
    let mut v = st.domain.bot_nodes(&cx.ws, &id).await?;
    v["editable"] = json!(
        s.reader(|c| bot_dispatch::owner(c, &cx.ws, &cx.me))
            .map_err(error)?
    );
    ok(v)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bindings {
    // Explicit null restores legacy; a missing property is invalid.
    nodes: Value,
}

fn identifiers(choices: &[String]) -> bool {
    choices.len() <= 32
        && choices.iter().enumerate().all(|(i, n)| {
            steve_node_api::NodeId::new(n.clone()).is_ok() && !choices[..i].contains(n)
        })
}

fn require_owner(c: &rusqlite::Connection, cx: &Ctx) -> Result<(), ApiError> {
    if !bot_dispatch::owner(c, &cx.ws, &cx.me).map_err(error)? {
        return Err(ApiError::Json(
            403,
            steve_i18n::t!("hub.dispatch.owner_only"),
        ));
    }
    Ok(())
}

pub async fn set_bindings(
    State(st): State<AppState>,
    Extension(cx): Extension<Ctx>,
    Path(id): Path<String>,
    Json(b): Json<Bindings>,
) -> ApiResult {
    let s = required_store(&st)?;
    let nodes: Option<Vec<String>> = serde_json::from_value(b.nodes).map_err(|_| bad())?;
    if nodes.as_ref().is_some_and(|ids| !identifiers(ids)) {
        return Err(bad());
    }
    if !s
        .reader(|c| bot_dispatch::owner(c, &cx.ws, &cx.me))
        .map_err(error)?
    {
        return Err(ApiError::Json(
            403,
            steve_i18n::t!("hub.dispatch.owner_only"),
        ));
    }
    if !s.reader(|c| is_bot(c, &cx.ws, &id)).map_err(error)? {
        return Err(ApiError::not_found(steve_i18n::t!(
            "hub.dispatch.not_found"
        )));
    }
    let report = if nodes.as_ref().is_some_and(|n| !n.is_empty()) {
        st.domain.bot_nodes(&cx.ws, &id).await?
    } else {
        Value::Null
    };
    operate(move || {
        let result: Result<Value, DispatchWriteError> = s.transaction(|tx| {
            require_owner(tx, &cx)?;
            if !is_bot(tx, &cx.ws, &id)? {
                return Err(StoreError::NotFound("Bot").into());
            }
            let old = bot_dispatch::bindings(tx, &cx.ws, &id)?.unwrap_or_default();
            if let Some(nodes) = &nodes {
                for node in nodes.iter().filter(|id| !old.contains(id)) {
                    if (node != "local"
                        && !crate::wire::node_binding::node_takes(tx, node, &cx.ws)?)
                        || !report["candidates"].as_array().is_some_and(|cs| {
                            cs.iter()
                                .any(|r| r["id"] == node.as_str() && r["state"] == "candidate")
                        })
                    {
                        return Err(ApiError::Json(
                            409,
                            steve_i18n::t!("hub.dispatch.not_candidate"),
                        )
                        .into());
                    }
                }
                tx.execute(
                    "INSERT INTO bot_node_bindings(workspace_id,bot_id,nodes) VALUES (?,?,?)
                     ON CONFLICT(workspace_id,bot_id) DO UPDATE SET nodes=excluded.nodes",
                    params![
                        cx.ws,
                        id,
                        serde_json::to_string(nodes).map_err(StoreError::from)?
                    ],
                )?;
            } else {
                tx.execute(
                    "DELETE FROM bot_node_bindings WHERE workspace_id=? AND bot_id=?",
                    params![cx.ws, id],
                )?;
            }
            audit(tx, &cx, "bot.nodes", &id, &json!({"nodes":nodes}))?;
            tx.publish(ChangeEvent::Bot { id });
            Ok(json!({"ok":true}))
        });
        result.map_err(Into::into)
    })
    .await
}

#[derive(Debug, thiserror::Error)]
enum DispatchWriteError {
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Api(#[from] ApiError),
}

impl From<rusqlite::Error> for DispatchWriteError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Store(e.into())
    }
}
impl From<DispatchWriteError> for ApiError {
    fn from(e: DispatchWriteError) -> Self {
        match e {
            DispatchWriteError::Store(e) => error(e),
            DispatchWriteError::Api(e) => e,
        }
    }
}

fn bad() -> ApiError {
    ApiError::bad_request(steve_i18n::t!("hub.dispatch.invalid"))
}

fn audit(
    c: &steve_store::Tx<'_>,
    cx: &Ctx,
    action: &str,
    id: &str,
    detail: &Value,
) -> steve_store::Result<()> {
    steve_store::base::audit(c, &cx.ws, &cx.me, action, id, detail)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Preset {
    name: String,
    members: Vec<Member>,
}

pub async fn create(
    State(st): State<AppState>,
    Extension(cx): Extension<Ctx>,
    Json(b): Json<Preset>,
) -> ApiResult {
    save(st, cx, None, b).await
}

pub async fn update(
    State(st): State<AppState>,
    Extension(cx): Extension<Ctx>,
    Path(id): Path<String>,
    Json(b): Json<Preset>,
) -> ApiResult {
    save(st, cx, Some(id), b).await
}

async fn save(st: AppState, cx: Ctx, id: Option<String>, b: Preset) -> ApiResult {
    let s = required_store(&st)?;
    let name = b.name.trim().to_string();
    let members = b.members;
    let ids: Vec<_> = members.iter().map(|m| m.bot.clone()).collect();
    if name.is_empty()
        || name.chars().count() > 80
        || name.chars().any(char::is_control)
        || members.is_empty()
        || !identifiers(&ids)
        || members
            .iter()
            .filter_map(|m| m.nodes.as_ref())
            .any(|n| !identifiers(n))
    {
        return Err(bad());
    }
    operate(move || -> Result<Value, ApiError> {
        let result: Result<Value, DispatchWriteError> = s.transaction(|tx| {
            require_owner(tx, &cx)?;
            for m in &members {
                if !is_bot(tx, &cx.ws, &m.bot)? {
                    return Err(StoreError::NotFound("Bot").into());
                }
                if let Some(nodes) = &m.nodes {
                    let bound = bot_dispatch::bindings(tx, &cx.ws, &m.bot)?.unwrap_or_default();
                    for id in nodes {
                        if !bound.contains(id)
                            || (id != "local"
                                && !crate::wire::node_binding::node_takes(tx, id, &cx.ws)?)
                        {
                            return Err(ApiError::Json(
                                409,
                                steve_i18n::t!("hub.dispatch.not_bound"),
                            )
                            .into());
                        }
                    }
                }
            }
            let data = serde_json::to_string(&members).map_err(StoreError::from)?;
            let id = match id {
                Some(id) => {
                    if tx.execute(
                        "UPDATE bot_teams SET name=?,members=? WHERE workspace_id=? AND id=?",
                        params![name, data, cx.ws, id],
                    )? == 0
                    {
                        return Err(StoreError::NotFound("Bot Team").into());
                    }
                    id
                }
                None => {
                    let id = steve_store::uid("team");
                    tx.execute(
                        "INSERT INTO bot_teams (id,workspace_id,name,members) VALUES (?,?,?,?)",
                        params![id, cx.ws, name, data],
                    )?;
                    id
                }
            };
            audit(
                tx,
                &cx,
                "bot.team.save",
                &id,
                &json!({"name": name, "members": members}),
            )?;
            tx.publish(ChangeEvent::Settings { id: cx.ws.clone() });
            Ok(json!({"id":id,"name":name,"members":members}))
        });
        result.map_err(Into::into)
    })
    .await
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DefaultTeam {
    team: String,
}

pub async fn set_default(
    State(st): State<AppState>,
    Extension(cx): Extension<Ctx>,
    Json(b): Json<DefaultTeam>,
) -> ApiResult {
    let s = required_store(&st)?;
    operate(move || {
        let result: Result<Value, DispatchWriteError> = s.transaction(|tx| {
            require_owner(tx, &cx)?;
            if let Some(team) = bot_dispatch::selected(tx, &cx.ws, Some(&b.team))? {
                tx.execute(
                    "INSERT INTO bot_team_defaults(workspace_id,team_id) VALUES (?,?)
                     ON CONFLICT(workspace_id) DO UPDATE SET team_id=excluded.team_id",
                    params![cx.ws, team.id],
                )?;
            } else {
                tx.execute(
                    "DELETE FROM bot_team_defaults WHERE workspace_id=?",
                    [&cx.ws],
                )?;
            }
            audit(tx, &cx, "bot.team.default", &b.team, &json!({}))?;
            tx.publish(ChangeEvent::Settings { id: cx.ws.clone() });
            Ok(json!({"default":b.team}))
        });
        result.map_err(Into::into)
    })
    .await
}

/// Item detail reports the actual binding, not a projection of the workspace's current default.
pub(super) fn item_dispatch(
    c: &rusqlite::Connection,
    id: &str,
    ws: &str,
) -> steve_store::Result<Value> {
    let team = bot_dispatch::item_team(c, id)?;
    let last = bot_dispatch::last_decision(c, id)?;
    let actual: Option<(String, String, Option<String>, i64)> = c.query_row(
        "SELECT r.node_id,s.id,s.bot_id,s.created FROM runtime_sessions r JOIN sessions s ON s.id=r.session_id
         WHERE s.item_id=?1 AND s.workspace_id=?2 ORDER BY s.no DESC LIMIT 1",
        params![id, ws], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
    ).optional()?;
    let mut placement = match actual {
        Some((node, session, bot, created))
            if last.as_ref().is_none_or(|p| {
                p["session"] != session && p["at"].as_i64().is_none_or(|at| created > at)
            }) =>
        {
            json!({"node":node,"session":session,"bot":bot,"reason":"legacy"})
        }
        _ => last.unwrap_or(Value::Null),
    };
    if let Some(node) = placement["node"].as_str().map(str::to_string)
        && node != "local"
    {
        if crate::wire::node_binding::node_takes(c, &node, ws)? {
            let name: String =
                c.query_row("SELECT name FROM nodes WHERE node_id=?", [&node], |r| {
                    r.get(0)
                })?;
            placement["node_name"] = json!(name);
        } else {
            placement["node"] = json!("unavailable");
        }
    }
    Ok(json!({
        "team":team.map(|(t, source)| json!({"id":t.id,"name":t.name,"source":source})),
        "placement":placement
    }))
}
