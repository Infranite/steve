//! Workspace owners administer Bot bindings and Team presets. Reading a candidate changes nothing.
use axum::{
    Extension, Json,
    extract::{Path, State},
};
use serde::Deserialize;
use serde_json::{Value, json};
use steve_store::{
    Members, Read, Store, StoreError, W, bot_admin,
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

fn is_bot(c: &mut impl Read, ws: &str, id: &str) -> steve_store::Result<bool> {
    bot_admin::selectable(c, ws, id, false)
}

fn is_owner(c: &mut impl Read, ws: &str, user: &str) -> steve_store::Result<bool> {
    Ok(Members::new(c).role(ws, user)?.as_deref() == Some("owner"))
}

pub async fn list(State(st): State<AppState>, Extension(cx): Extension<Ctx>) -> ApiResult {
    let s = required_store(&st)?;
    operate(move || {
        s.read(|c| {
            let mut teams = s.reader(|db| bot_dispatch::teams(db, &cx.ws))?;
            // A previously private workspace can gain members. Hide stale remote grants in
            // preset reads too; do not turn an empty restricted list into "all bound Nodes".
            for member in teams.iter_mut().flat_map(|team| &mut team.members) {
                if let Some(nodes) = &mut member.nodes {
                    let mut visible = Vec::with_capacity(nodes.len());
                    for node in nodes.drain(..) {
                        if node == "local"
                            || crate::wire::node_binding::node_takes(c, &node, &cx.ws)?
                        {
                            visible.push(node);
                        }
                    }
                    *nodes = visible;
                }
            }
            Ok(json!({
                "teams":teams,
                "default":s.reader(|db| bot_dispatch::selected(db, &cx.ws, None))?.map_or_else(|| LEGACY.into(), |t| t.id),
                "editable":is_owner(c, &cx.ws, &cx.me)?,
            }))
        }).map_err(error)
    })
    .await
}

pub async fn candidates(
    State(st): State<AppState>,
    Extension(cx): Extension<Ctx>,
    Path(id): Path<String>,
) -> ApiResult {
    let s = required_store(&st)?;
    let own = s.read(|c| is_bot(c, &cx.ws, &id)).map_err(error)?;
    if !own {
        return Err(ApiError::not_found(steve_i18n::t!(
            "hub.dispatch.not_found"
        )));
    }
    let mut v = st.domain.bot_nodes(&cx.ws, &id).await?;
    v["editable"] = json!(s.read(|c| is_owner(c, &cx.ws, &cx.me)).map_err(error)?);
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

fn require_owner(c: &mut impl Read, cx: &Ctx) -> Result<(), ApiError> {
    if !is_owner(c, &cx.ws, &cx.me).map_err(error)? {
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
    if !s.read(|c| is_owner(c, &cx.ws, &cx.me)).map_err(error)? {
        return Err(ApiError::Json(
            403,
            steve_i18n::t!("hub.dispatch.owner_only"),
        ));
    }
    if !s.read(|c| is_bot(c, &cx.ws, &id)).map_err(error)? {
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
        let result: Result<Value, DispatchWriteError> = s.write(|tx| {
            require_owner(tx, &cx)?;
            if !is_bot(tx, &cx.ws, &id)? {
                return Err(StoreError::NotFound("Bot").into());
            }
            let old = s
                .reader(|db| bot_dispatch::bindings(db, &cx.ws, &id))?
                .unwrap_or_default();
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
            }
            bot_admin::set_bindings(tx, &cx.ws, &id, nodes.as_deref())?;
            audit(tx, &cx, "bot.nodes", &id, &json!({"nodes":nodes}))?;
            tx.emit(steve_types::DomainEvent::from(
                steve_types::ConfigEvent::Bot {
                    ws: cx.ws.clone().into(),
                    id: {
                        let value: String = id;
                        steve_types::BotId::from(value)
                    },
                },
            ));
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
    c: &mut W<'_>,
    cx: &Ctx,
    action: &str,
    id: &str,
    detail: &Value,
) -> steve_store::Result<()> {
    c.audit(&cx.ws, &cx.me, action, id, detail)
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
        let result: Result<Value, DispatchWriteError> = s.write(|tx| {
            require_owner(tx, &cx)?;
            for m in &members {
                if !is_bot(tx, &cx.ws, &m.bot)? {
                    return Err(StoreError::NotFound("Bot").into());
                }
                if let Some(nodes) = &m.nodes {
                    let bound = s
                        .reader(|db| bot_dispatch::bindings(db, &cx.ws, &m.bot))?
                        .unwrap_or_default();
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
            let id = bot_admin::save(tx, &cx.ws, id.as_deref(), &name, &members)?;
            audit(
                tx,
                &cx,
                "bot.team.save",
                &id,
                &json!({"name": name, "members": members}),
            )?;
            tx.emit(steve_types::DomainEvent::from(
                steve_types::ConfigEvent::Settings {
                    ws: cx.ws.clone().into(),
                    id: {
                        let value: String = cx.ws.clone();
                        steve_types::WorkspaceId::from(value)
                    },
                },
            ));
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
        let result: Result<Value, DispatchWriteError> = s.write(|tx| {
            require_owner(tx, &cx)?;
            let team = s.reader(|db| bot_dispatch::selected(db, &cx.ws, Some(&b.team)))?;
            bot_admin::set_default(tx, &cx.ws, team.as_ref().map(|team| team.id.as_str()))?;
            audit(tx, &cx, "bot.team.default", &b.team, &json!({}))?;
            tx.emit(steve_types::DomainEvent::from(
                steve_types::ConfigEvent::Settings {
                    ws: cx.ws.clone().into(),
                    id: {
                        let value: String = cx.ws.clone();
                        steve_types::WorkspaceId::from(value)
                    },
                },
            ));
            Ok(json!({"default":b.team}))
        });
        result.map_err(Into::into)
    })
    .await
}

/// Item detail reports the actual binding, not a projection of the workspace's current default.
pub(super) fn item_dispatch(store: &Store, id: &str, ws: &str) -> steve_store::Result<Value> {
    store.read(|c| {
        let (team, last) = store.reader(|db| {
            Ok::<_, StoreError>((
                bot_dispatch::item_team(db, id)?,
                bot_dispatch::last_decision(db, id)?,
            ))
        })?;
        let actual = bot_admin::actual(c, id, ws)?;
        let mut placement = match actual {
            Some(bot_admin::Actual {
                node,
                session,
                bot,
                created,
            }) if last.as_ref().is_none_or(|p| {
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
                let name = steve_store::node_directory::name(c, &node)?
                    .ok_or_else(|| StoreError::NotFound("Node"))?;
                placement["node_name"] = json!(name);
            } else {
                placement["node"] = json!("unavailable");
            }
        }
        Ok(json!({
            "team":team.map(|(t, source)| json!({"id":t.id,"name":t.name,"source":source})),
            "placement":placement
        }))
    })
}

/// Routes mounted behind the shared API login and body-limit layers.
pub(crate) fn routes() -> axum::Router<super::AppState> {
    use super::*;
    Router::new()
        .route(
            "/bot-teams",
            get(bot_dispatch::list).post(bot_dispatch::create),
        )
        .route("/bot-teams/default", put(bot_dispatch::set_default))
        .route("/bot-teams/{id}", put(bot_dispatch::update))
        .route(
            "/bots/{id}/execution-nodes",
            get(bot_dispatch::candidates).put(bot_dispatch::set_bindings),
        )
}
