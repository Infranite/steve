//! Flows and Bots: what a Bot's “定时与流程” tab lists
//! (`GET /bots/:id/flows`) and what happens to flows when a Bot is deleted ([`on_bot_deleted`]).
//!
//! A flow uses a Bot through the `bot` of an action (`item.create`, `report`), the `bot_id` of a
//! `bot` node, a schedule's `bot`, or — for `bot` nodes without `bot_id` — the `triage_bot` of the
//! source it is triggered by.

use std::sync::Arc;

use axum::extract::Path;
use axum::routing::get;
use axum::{Extension, Json, Router};
use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde_json::{Value, json};
use steve_modules::{ApiError, ApiResult, Caller, Modules};
use steve_store::Tx;

use crate::graph::Graph;
use crate::schedules::{self, Fields};
use crate::{FlowCx, FlowResult, flows};

/// Author of the versions [`on_bot_deleted`] saves.
pub const AUTHOR: &str = "steve";

/// Node config keys that name a Bot (`bot` of actions, `bot_id` of `bot` nodes).
const BOT_KEYS: [&str; 2] = ["bot", "bot_id"];

/// Does any node of `graph` name `bot` directly?
pub fn graph_uses(graph: &Value, bot: &str) -> bool {
    nodes(graph).any(|n| BOT_KEYS.iter().any(|k| n["config"][*k] == bot))
}

fn nodes(graph: &Value) -> impl Iterator<Item = &Value> {
    graph["nodes"].as_array().into_iter().flatten()
}

/// Replaces every direct reference to `bot` with `heir`; returns how many changed.
pub fn replace_bot(graph: &mut Value, bot: &str, heir: &str) -> usize {
    let mut n = 0;
    for node in graph["nodes"].as_array_mut().into_iter().flatten() {
        for k in BOT_KEYS {
            if node["config"][k] == bot {
                node["config"][k] = json!(heir);
                n += 1;
            }
        }
    }
    n
}

/// `ownBot`: the Bot is a member of `ws` (deleted or not, like the TS `bot_workspaces` check).
pub fn own_bot(c: &Connection, ws: &str, bot: &str) -> FlowResult<()> {
    let ok: bool = c.query_row(
        "SELECT EXISTS (SELECT 1 FROM bot_workspaces WHERE bot_id = ? AND workspace_id = ?)",
        params![bot, ws],
        |r| r.get(0),
    )?;
    if ok {
        Ok(())
    } else {
        // TS `ownBot`: “没有这个 Bot” (with the space)
        Err(ApiError::not_found(" Bot"))
    }
}

/// The connections of `ws` whose “先交给谁判断” is `bot`, with their `ext_events.source` (when the
/// adapter is registered).
fn triaged_by(
    cx: &FlowCx,
    c: &Connection,
    ws: &str,
    bot: &str,
) -> FlowResult<Vec<(String, Option<&'static str>)>> {
    let mut st = c.prepare_cached(
        "SELECT s.connection_id, c.adapter FROM source_settings s JOIN connections c ON c.id = s.connection_id
          WHERE s.workspace_id = ? AND s.triage_bot = ? ORDER BY s.connection_id",
    )?;
    let rows = st
        .query_map(params![ws, bot], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows
        .into_iter()
        .map(|(id, adapter)| {
            let source = cx.events.get(&adapter).map(|a| a.source());
            (id, source)
        })
        .collect())
}

/// A flow whose `bot` nodes without `bot_id` fall back to the triage Bot of one of `conns`: it has
/// such a node and its trigger takes events from one of them.
fn uses_triage(g: &Graph, conns: &[(String, Option<&'static str>)]) -> bool {
    let fallback = g
        .nodes
        .iter()
        .any(|n| n.kind == "bot" && n.config["bot_id"].as_str().is_none_or(str::is_empty));
    let Some(t) = g.trigger().filter(|_| fallback) else {
        return false;
    };
    let listed: Vec<&str> = t.config["connections"]
        .as_array()
        .map(|a| a.iter().filter_map(Value::as_str).collect())
        .unwrap_or_default();
    let source = t.config["source"].as_str().unwrap_or("");
    conns.iter().any(|(id, src)| {
        if listed.is_empty() {
            src.is_some_and(|s| s == source)
        } else {
            listed.contains(&id.as_str())
        }
    })
}

/// `GET /bots/:id/flows`: `{flows: FlowSummary[], schedules: Schedule[]}`.
pub fn bot_flows(cx: &FlowCx, c: &Connection, ws: &str, bot: &str) -> FlowResult<Value> {
    own_bot(c, ws, bot)?;
    let now = cx.now();
    let triage = triaged_by(cx, c, ws, bot)?;
    let mut out = Vec::new();
    for f in flows::list(c, ws, None)?
        .into_iter()
        .filter(|f| f.kind != "schedule")
    {
        let g = Graph::from_value(&f.graph).unwrap_or_default();
        if graph_uses(&f.graph, bot) || uses_triage(&g, &triage) {
            out.push(flows::summary(c, &f, now)?);
        }
    }
    let scheds = schedules::list(c, ws)?
        .iter()
        .filter(|s| s.spec.bot == bot)
        .map(|s| schedules::view(c, s))
        .collect::<FlowResult<Vec<_>>>()?;
    Ok(json!({ "flows": out, "schedules": scheds }))
}

/// What [`on_bot_deleted`] changed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct BotMoved {
    /// Flows that got a new version.
    pub flows: Vec<String>,
    /// Schedules handed to the heir.
    pub schedules: Vec<String>,
    /// Connections whose `triage_bot` was cleared (back to the main Bot).
    pub sources: Vec<String>,
}

fn bot_name(c: &Connection, id: &str) -> FlowResult<String> {
    Ok(
        c.query_row("SELECT name FROM bots WHERE id = ?", [id], |r| {
            r.get::<_, String>(0)
        })
        .optional()?
        .unwrap_or_else(|| id.to_string()),
    )
}

/// The note of the versions [`on_bot_deleted`] saves: “<Bot> 已删除，交给 <heir>”.
pub fn note(c: &Connection, bot: &str, heir: &str) -> FlowResult<String> {
    Ok(format!(
        "{} 已删除，交给 {}",
        bot_name(c, bot)?,
        bot_name(c, heir)?
    ))
}

/// The Bot `bot` was deleted and `heir` takes over in `ws`. In one transaction:
/// - every flow (not deleted, enabled or not) whose graph names `bot` gets the heir instead, as a
///   new version by `steve` with the note “<Bot> 已删除，交给 <heir>”;
/// - every schedule of `bot` is handed to `heir` (a new version, same note);
/// - `source_settings.triage_bot = bot` becomes `NULL` (the main Bot judges again).
///
/// Runs already started keep the version they started on. Calling it twice is harmless (nothing
/// names `bot` the second time).
pub fn on_bot_deleted(tx: &Tx, ws: &str, bot: &str, heir: &str, now: i64) -> FlowResult<BotMoved> {
    let mut moved = BotMoved::default();
    if bot == heir {
        return Ok(moved);
    }
    let note = note(tx, bot, heir)?;
    for f in flows::list(tx, ws, None)?
        .into_iter()
        .filter(|f| f.kind != "schedule" && graph_uses(&f.graph, bot))
    {
        let mut g = f.graph.clone();
        replace_bot(&mut g, bot, heir);
        let graph = Graph::from_value(&g).map_err(ApiError::Internal)?;
        flows::save_version(tx, &f, &graph, AUTHOR, &note, now)?;
        moved.flows.push(f.id);
    }
    for s in schedules::list(tx, ws)?
        .into_iter()
        .filter(|s| s.spec.bot == bot)
    {
        let mut spec = s.spec.clone();
        spec.bot = heir.to_string();
        schedules::update(
            tx,
            &s,
            &Fields {
                name: &s.name,
                spec: &spec,
                enabled: s.enabled,
            },
            AUTHOR,
            &note,
            now,
        )?;
        steve_store::base::revision(
            tx,
            ws,
            "flow",
            &s.id,
            s.version + 1,
            &json!({ "bot": bot }),
            &json!({ "bot": heir }),
            AUTHOR,
        )?;
        steve_store::base::audit(
            tx,
            ws,
            AUTHOR,
            "schedule.bot_moved",
            &s.id,
            &json!({ "from": bot, "to": heir }),
        )?;
        moved.schedules.push(s.id);
    }
    let mut st = tx.prepare_cached(
        "SELECT connection_id FROM source_settings WHERE workspace_id = ? AND triage_bot = ? ORDER BY connection_id",
    )?;
    let conns = st
        .query_map(params![ws, bot], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    drop(st);
    for id in &conns {
        tx.execute(
            "UPDATE source_settings SET triage_bot = NULL, revision = revision + 1, updated = ? WHERE connection_id = ?",
            params![now, id],
        )?;
        steve_store::base::audit(
            tx,
            ws,
            AUTHOR,
            "source.triage_cleared",
            id,
            &json!({ "bot": bot }),
        )?;
    }
    moved.sources = conns;
    if !moved.flows.is_empty() || !moved.schedules.is_empty() || !moved.sources.is_empty() {
        steve_store::base::audit(
            tx,
            ws,
            AUTHOR,
            "flow.bot_deleted",
            bot,
            &json!({ "heir": heir, "flows": moved.flows, "schedules": moved.schedules, "sources": moved.sources }),
        )?;
    }
    Ok(moved)
}

type Cx = Extension<Arc<FlowCx>>;

pub(crate) fn routes() -> Router<Modules> {
    Router::new().route("/bots/{id}/flows", get(list))
}

async fn list(Extension(cx): Cx, who: Caller, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    Ok(Json(
        cx.store().reader(|c| bot_flows(&cx, c, &who.ws, &id))?,
    ))
}
