//! REST of flows and runs, relative to `/api`:
//!
//! | route | |
//! | --- | --- |
//! | `GET /flows?kind=flow\|schedule` | `FlowSummary[]` |
//! | `POST /flows` | `{name, description?, graph?, origin?}` → `Flow` (disabled) |
//! | `GET /flows/:id` | `Flow` |
//! | `GET /flows/:id/versions/:v` | `{version, author, note, created, graph}` |
//! | `PUT /flows/:id` | `{graph, note?}` → new version, `Flow` |
//! | `PATCH /flows/:id` | `{name?, description?, enabled?}` → `Flow` |
//! | `DELETE /flows/:id` | `{ok:true}` |
//! | `POST /flows/:id/duplicate` | `Flow` (“ 副本”, disabled) |
//! | `POST /flows/:id/rollback` | `{version}` → `Flow` |
//! | `POST /flows/:id/dry-run` | `{event_id?, sample?, prefer?, graph?}` → `Run` (`dry_run:true`) |
//! | `POST /flows/:id/run` | `{input?}` → `{run_id}` (manual and time triggers) |
//! | `GET /flows/:id/runs?limit=&dry=0\|1` | `Run[]` without steps (`dry` absent = all) |
//! | `GET /runs/:id` | `Run` |
//! | `POST /runs/:id/cancel` | `Run` |
//! | `GET /flow-settings/allow-private` | `{hosts}`: the hosts `http` nodes may reach on internal addresses |
//! | `PUT /flow-settings/allow-private` | `{hosts}` → `{hosts}` (the workspace's owner; Steve's drafts never write it) |

use std::sync::Arc;

use axum::extract::{Path, Query};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use rusqlite::{Connection, params};
use serde::Deserialize;
use serde_json::{Value, json};
use steve_modules::{ApiError, ApiResult, Body, Caller, Check, Modules};

use crate::FlowCx;
use crate::engine;
use crate::flows::{self, DESCRIPTION_MAX, FlowRow, NAME_MAX, NewFlow};
use crate::graph::{self, Graph};
use crate::nodes::http;
use crate::runs;
use crate::signals;

type Cx = Extension<Arc<FlowCx>>;

pub fn routes() -> Router<Modules> {
    Router::new()
        .route("/flows", get(list).post(create))
        .route(
            "/flows/{id}",
            get(detail).put(save).patch(patch).delete(remove),
        )
        .route("/flows/{id}/versions/{v}", get(version))
        .route("/flows/{id}/duplicate", post(duplicate))
        .route("/flows/{id}/rollback", post(rollback))
        .route("/flows/{id}/dry-run", post(dry_run))
        .route("/flows/{id}/run", post(run_now))
        .route("/flows/{id}/runs", get(history))
        .route("/runs/{id}", get(run_detail))
        .route("/runs/{id}/cancel", post(cancel))
        .route(
            "/flow-settings/allow-private",
            get(allow_private_get).put(allow_private_put),
        )
}

/// Hosts in [`http::ALLOW_PRIVATE`] at most.
pub const ALLOW_PRIVATE_MAX: usize = 50;

async fn allow_private_get(Extension(cx): Cx, who: Caller) -> ApiResult<Json<Value>> {
    let mut hosts: Vec<String> = cx
        .store()
        .reader(|c| Ok::<_, ApiError>(http::allow_private(c, &who.ws)))?
        .into_iter()
        .collect();
    hosts.sort();
    Ok(Json(json!({ "hosts": hosts })))
}

#[derive(Debug, Deserialize)]
struct AllowPrivateBody {
    hosts: Vec<String>,
}

/// A host name or an IP literal (`nas.local`, `192.168.1.10`, `[::1]`, `localhost`).
fn valid_host(h: &str) -> bool {
    !h.is_empty()
        && h.len() <= 253
        && h.chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | ':' | '[' | ']' | '_'))
}

/// Your explicit setting: only a person, in this form, adds a host. Steve's drafts and conversational
/// edits write graphs, never this.
async fn allow_private_put(
    Extension(cx): Cx,
    who: Caller,
    Body(b): Body<AllowPrivateBody>,
) -> ApiResult<Json<Value>> {
    let mut hosts: Vec<String> = Vec::new();
    for h in &b.hosts {
        let h = h.trim().trim_end_matches('.').to_ascii_lowercase();
        if h.is_empty() {
            continue;
        }
        if !valid_host(&h) {
            return Err(ApiError::invalid("hosts"));
        }
        if !hosts.contains(&h) {
            hosts.push(h);
        }
    }
    if hosts.len() > ALLOW_PRIVATE_MAX {
        return Err(ApiError::invalid("hosts"));
    }
    hosts.sort();
    let now = cx.now();
    cx.store().transaction(|tx| -> ApiResult<()> {
        let role: Option<String> = rusqlite::OptionalExtension::optional(tx.query_row(
            "SELECT role FROM workspace_members WHERE workspace_id = ? AND user_id = ?",
            params![who.ws, who.me],
            |r| r.get(0),
        ))?;
        if role.as_deref() != Some("owner") {
            return Err(ApiError::forbidden(steve_i18n::t!("flow.settings.owner_only")));
        }
        tx.execute(
            "INSERT INTO settings (workspace_id, key, value, updated) VALUES (?, ?, ?, ?)
             ON CONFLICT(workspace_id, key) DO UPDATE SET value = excluded.value, updated = excluded.updated",
            params![who.ws, http::ALLOW_PRIVATE, json!(hosts).to_string(), now],
        )?;
        steve_store::base::audit(
            tx,
            &who.ws,
            &who.me,
            "flow.allow_private",
            http::ALLOW_PRIVATE,
            &json!({ "hosts": hosts }),
        )?;
        Ok(())
    })?;
    Ok(Json(json!({ "hosts": hosts })))
}

/// Schedules are edited on their own page (their graph is generated from the form).
fn not_schedule(f: &FlowRow) -> ApiResult<()> {
    if f.kind == "schedule" {
        return Err(ApiError::rule(steve_i18n::t!(
            "flow.draft.schedule_elsewhere"
        )));
    }
    Ok(())
}

/// Validation plus the references inside the graph: called flows and bots must be of this
/// workspace (`400`, the graph is the request body).
pub fn check_graph(c: &Connection, cx: &FlowCx, ws: &str, v: &Value) -> ApiResult<Graph> {
    let g = graph::validate(v, &cx.nodes).map_err(ApiError::rule)?;
    for n in &g.nodes {
        if n.kind == "flow"
            && let Some(id) = n.config["flow_id"].as_str()
            && flows::own(c, ws, id).is_err()
        {
            return Err(ApiError::rule(steve_i18n::t!(
                "flow.graph.no_such_flow",
                node = n.name()
            )));
        }
        // `bot` of actions, `bot_id` of `bot` nodes
        if let Some(b) = n.config["bot"]
            .as_str()
            .or_else(|| n.config["bot_id"].as_str())
            .filter(|b| !b.is_empty() && !b.contains("{{"))
        {
            let ok: bool = c.query_row(
                "SELECT EXISTS (SELECT 1 FROM bot_workspaces bw JOIN bots b ON b.id = bw.bot_id WHERE bw.bot_id = ? AND bw.workspace_id = ? AND b.deleted = 0)",
                params![b, ws],
                |r| r.get(0),
            )?;
            if !ok {
                return Err(ApiError::rule(steve_i18n::t!(
                    "flow.graph.no_such_bot",
                    node = n.name()
                )));
            }
        }
    }
    Ok(g)
}

fn view(cx: &FlowCx, ws: &str, id: &str) -> ApiResult<Json<Value>> {
    let now = cx.now();
    cx.store().reader(|c| -> ApiResult<_> {
        let f = flows::own(c, ws, id)?;
        Ok(Json(flows::view(c, &f, now)?))
    })
}

#[derive(Debug, Deserialize)]
struct KindQuery {
    kind: Option<String>,
}

async fn list(
    Extension(cx): Cx,
    who: Caller,
    Query(q): Query<KindQuery>,
) -> ApiResult<Json<Value>> {
    let kind = q.kind.filter(|k| !k.is_empty());
    if let Some(k) = &kind
        && !["flow", "schedule", "response"].contains(&k.as_str())
    {
        return Err(ApiError::invalid("kind"));
    }
    let now = cx.now();
    cx.store().reader(|c| -> ApiResult<_> {
        let out = flows::list(c, &who.ws, kind.as_deref())?
            .iter()
            .map(|f| flows::summary(c, f, now))
            .collect::<ApiResult<Vec<_>>>()?;
        Ok(Json(Value::Array(out)))
    })
}

#[derive(Debug, Deserialize)]
struct CreateBody {
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    graph: Option<Value>,
    #[serde(default)]
    origin: Option<String>,
}

fn check_meta(name: Option<&str>, description: Option<&str>) -> ApiResult<()> {
    let mut c = Check::new();
    if let Some(n) = name {
        c.len("name", n.trim(), 1, NAME_MAX);
    }
    c.opt_len("description", description, 0, DESCRIPTION_MAX);
    c.done()
}

async fn create(
    Extension(cx): Cx,
    who: Caller,
    Body(b): Body<CreateBody>,
) -> ApiResult<Json<Value>> {
    check_meta(Some(&b.name), b.description.as_deref())?;
    let origin = b.origin.unwrap_or_else(|| "me".into());
    let ok_origin = origin == "me"
        || origin == "steve"
        || origin
            .strip_prefix("template:")
            .is_some_and(|t| !t.is_empty())
        || origin
            .strip_prefix("plugin:")
            .is_some_and(|t| !t.is_empty());
    if !ok_origin || origin.len() > 80 {
        return Err(ApiError::invalid("origin"));
    }
    let now = cx.now();
    let id = cx.store().transaction(|tx| -> ApiResult<String> {
        let g = match &b.graph {
            Some(v) => check_graph(tx, &cx, &who.ws, v)?,
            None => Graph::starter("mail"),
        };
        flows::create(
            tx,
            &who.ws,
            &who.me,
            &NewFlow {
                kind: "flow",
                name: b.name.trim(),
                description: b.description.as_deref().unwrap_or("").trim(),
                graph: &g,
                origin: &origin,
                origin_label: flows::origin_label(&origin),
                enabled: false,
            },
            now,
        )
    })?;
    view(&cx, &who.ws, &id)
}

async fn detail(Extension(cx): Cx, who: Caller, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    view(&cx, &who.ws, &id)
}

async fn version(
    Extension(cx): Cx,
    who: Caller,
    Path((id, v)): Path<(String, String)>,
) -> ApiResult<Json<Value>> {
    cx.store().reader(|c| -> ApiResult<_> {
        flows::own(c, &who.ws, &id)?;
        let v = v
            .parse::<i64>()
            .ok()
            .and_then(|v| flows::version(c, &id, v).transpose())
            .transpose()?
            .ok_or_else(|| ApiError::not_found("版本"))?;
        Ok(Json(v))
    })
}

#[derive(Debug, Deserialize)]
struct SaveBody {
    graph: Value,
    #[serde(default)]
    note: Option<String>,
}

async fn save(
    Extension(cx): Cx,
    who: Caller,
    Path(id): Path<String>,
    Body(b): Body<SaveBody>,
) -> ApiResult<Json<Value>> {
    let mut c = Check::new();
    c.opt_len("note", b.note.as_deref(), 0, 200);
    c.done()?;
    let now = cx.now();
    cx.store().transaction(|tx| -> ApiResult<()> {
        let f = flows::own(tx, &who.ws, &id)?;
        not_schedule(&f)?;
        let g = check_graph(tx, &cx, &who.ws, &b.graph)?;
        flows::save_version(
            tx,
            &f,
            &g,
            &who.me,
            b.note.as_deref().unwrap_or("").trim(),
            now,
        )?;
        Ok(())
    })?;
    cx.wake();
    view(&cx, &who.ws, &id)
}

#[derive(Debug, Deserialize)]
struct PatchBody {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    enabled: Option<bool>,
}

async fn patch(
    Extension(cx): Cx,
    who: Caller,
    Path(id): Path<String>,
    Body(b): Body<PatchBody>,
) -> ApiResult<Json<Value>> {
    check_meta(b.name.as_deref(), b.description.as_deref())?;
    let now = cx.now();
    cx.store().transaction(|tx| -> ApiResult<()> {
        let f = flows::own(tx, &who.ws, &id)?;
        not_schedule(&f)?;
        if b.enabled == Some(true) {
            // a flow that no longer validates (a node kind went away) cannot be switched on
            check_graph(tx, &cx, &who.ws, &f.graph)?;
        }
        flows::patch(
            tx,
            &f,
            &who.me,
            b.name.as_deref().map(str::trim),
            b.description.as_deref().map(str::trim),
            b.enabled,
            now,
        )
    })?;
    cx.wake();
    view(&cx, &who.ws, &id)
}

async fn remove(Extension(cx): Cx, who: Caller, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let now = cx.now();
    cx.store().transaction(|tx| -> ApiResult<()> {
        let f = flows::own(tx, &who.ws, &id)?;
        not_schedule(&f)?;
        flows::delete(tx, &f, &who.me, now)
    })?;
    Ok(Json(json!({ "ok": true })))
}

async fn duplicate(
    Extension(cx): Cx,
    who: Caller,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    let now = cx.now();
    let new_id = cx.store().transaction(|tx| -> ApiResult<String> {
        let f = flows::own(tx, &who.ws, &id)?;
        not_schedule(&f)?;
        let g = Graph::from_value(&f.graph).map_err(ApiError::rule)?;
        let mut name = format!("{} 副本", f.name);
        if name.chars().count() > NAME_MAX {
            name = name
                .chars()
                .rev()
                .take(NAME_MAX)
                .collect::<Vec<_>>()
                .into_iter()
                .rev()
                .collect();
        }
        flows::create(
            tx,
            &who.ws,
            &who.me,
            &NewFlow {
                kind: &f.kind,
                name: &name,
                description: &f.description,
                graph: &g,
                origin: &f.origin,
                origin_label: &f.origin_label,
                enabled: false,
            },
            now,
        )
    })?;
    view(&cx, &who.ws, &new_id)
}

#[derive(Debug, Deserialize)]
struct RollbackBody {
    version: i64,
}

async fn rollback(
    Extension(cx): Cx,
    who: Caller,
    Path(id): Path<String>,
    Body(b): Body<RollbackBody>,
) -> ApiResult<Json<Value>> {
    let now = cx.now();
    cx.store().transaction(|tx| -> ApiResult<()> {
        let f = flows::own(tx, &who.ws, &id)?;
        not_schedule(&f)?;
        let g =
            flows::version_graph(tx, &id, b.version)?.ok_or_else(|| ApiError::not_found("版本"))?;
        let g = check_graph(tx, &cx, &who.ws, &g)?;
        flows::save_version(tx, &f, &g, &who.me, &format!("回到 v{}", b.version), now)?;
        Ok(())
    })?;
    cx.wake();
    view(&cx, &who.ws, &id)
}

#[derive(Debug, Default, Deserialize)]
struct DryRunBody {
    #[serde(default)]
    event_id: Option<String>,
    #[serde(default)]
    sample: Option<Value>,
    #[serde(default)]
    prefer: Option<Vec<String>>,
    /// The canvas's unsaved working copy.
    #[serde(default)]
    graph: Option<Value>,
}

/// The latest visible event the trigger would take.
fn latest_event(c: &Connection, cx: &FlowCx, who: &Caller, g: &Graph) -> ApiResult<Value> {
    let Some(t) = g.trigger() else {
        return Ok(Value::Null);
    };
    let source = t.config["source"].as_str().unwrap_or("");
    if matches!(source, "time" | "manual") {
        return Ok(Value::Null);
    }
    let types: Vec<String> = crate::nodes::str_list(&t.config, "types").unwrap_or_default();
    let viewer = cx.modules.viewer(who)?;
    let mut st = c.prepare(
        "SELECT id, type FROM ext_events WHERE workspace_id = ? AND source = ? ORDER BY received DESC, rowid DESC LIMIT 50",
    )?;
    let rows = st
        .query_map(params![who.ws, source], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    for (id, ty) in rows {
        if (types.is_empty() || types.contains(&ty)) && signals::own(c, &viewer, &id).is_ok() {
            return crate::ingest::event_json(c, &id);
        }
    }
    Ok(Value::Null)
}

async fn dry_run(
    Extension(cx): Cx,
    who: Caller,
    Path(id): Path<String>,
    body: axum::body::Bytes,
) -> ApiResult<Json<Value>> {
    let b: DryRunBody = optional(&body)?;
    let mut c = Check::new();
    c.list("prefer", b.prefer.as_deref().unwrap_or(&[]), 60, 16);
    c.that("sample", b.sample.as_ref().is_none_or(Value::is_object));
    c.done()?;
    let (f, event, event_id, graph) = cx.store().reader(|c| -> ApiResult<_> {
        let f = flows::own(c, &who.ws, &id)?;
        let graph = match &b.graph {
            Some(g) => Some(check_graph(c, &cx, &who.ws, g)?.to_value()),
            None => None,
        };
        let g = Graph::from_value(graph.as_ref().unwrap_or(&f.graph)).map_err(ApiError::rule)?;
        let event = match (&b.event_id, &b.sample) {
            (Some(e), _) => {
                let viewer = cx.modules.viewer(&who)?;
                signals::own(c, &viewer, e)?;
                crate::ingest::event_json(c, e)?
            }
            (None, Some(s)) => {
                let mut s = s.clone();
                let t = g.trigger().map(|t| t.config.clone()).unwrap_or(Value::Null);
                if s.get("source").is_none() {
                    s["source"] = t["source"].clone();
                }
                if s.get("type").is_none() {
                    s["type"] = t["types"][0].clone();
                }
                s
            }
            (None, None) => latest_event(c, &cx, &who, &g)?,
        };
        // a stored event (the one asked for, or the latest the trigger would take), not a sample
        let event_id = match (&b.event_id, &b.sample) {
            (Some(e), _) => Some(e.clone()),
            (None, None) => event["id"].as_str().map(String::from),
            (None, Some(_)) => None,
        };
        Ok((f, event, event_id, graph))
    })?;
    let run = engine::dry_run(
        &cx,
        &f,
        event,
        event_id.as_deref(),
        b.prefer.unwrap_or_default(),
        graph,
    )
    .await?;
    run_json(&cx, &who.ws, &run)
}

#[derive(Debug, Default, Deserialize)]
struct RunBody {
    #[serde(default)]
    input: Option<Value>,
}

/// An optional JSON body: empty = defaults.
fn optional<T: serde::de::DeserializeOwned + Default>(bytes: &[u8]) -> ApiResult<T> {
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(T::default());
    }
    steve_modules::parse(bytes)
}

async fn run_now(
    Extension(cx): Cx,
    who: Caller,
    Path(id): Path<String>,
    body: axum::body::Bytes,
) -> ApiResult<Json<Value>> {
    let b: RunBody = optional(&body)?;
    let f = cx.store().reader(|c| flows::own(c, &who.ws, &id))?;
    if f.kind == "schedule" {
        let (run, go) = crate::scheduler::start_manual(&cx, &who.ws, &id)?;
        if go {
            crate::scheduler::spawn_drive(cx.clone(), run.clone());
        }
        return Ok(Json(json!({ "run_id": run })));
    }
    let g = Graph::from_value(&f.graph).map_err(ApiError::rule)?;
    let source = g
        .trigger()
        .and_then(|t| t.config["source"].as_str())
        .unwrap_or("");
    if source != "manual" && source != "time" {
        return Err(ApiError::rule(steve_i18n::t!("flow.run.not_manual")));
    }
    let now = cx.now();
    let run = cx.store().transaction(|tx| {
        engine::open_run(
            tx,
            &f,
            "manual",
            json!({ "input": b.input.unwrap_or_else(|| json!({ "at": now })), "at": now }),
            None,
            false,
            None,
            0,
            now,
        )
    })?;
    crate::scheduler::spawn_drive(cx.clone(), run.clone());
    Ok(Json(json!({ "run_id": run })))
}

#[derive(Debug, Deserialize)]
struct HistoryQuery {
    limit: Option<i64>,
    /// `1` only dry runs, `0` only real ones.
    dry: Option<String>,
}

async fn history(
    Extension(cx): Cx,
    who: Caller,
    Path(id): Path<String>,
    Query(q): Query<HistoryQuery>,
) -> ApiResult<Json<Value>> {
    cx.store().reader(|c| -> ApiResult<_> {
        let dry = match q.dry.as_deref() {
            None => None,
            Some("1") => Some(true),
            Some("0") => Some(false),
            Some(_) => return Err(ApiError::invalid("dry")),
        };
        flows::own(c, &who.ws, &id)?;
        let rows = runs::page_of(c, &who.ws, &id, None, q.limit.unwrap_or(20), dry)?;
        Ok(Json(Value::Array(
            rows.iter()
                .map(|r| {
                    json!({
                        "id": r.id, "flow_id": r.flow_id, "flow_version": r.flow_version,
                        "status": r.status, "dry_run": r.dry_run, "event_id": r.event_id,
                        "outcome": r.outcome, "outcome_label": r.outcome_label,
                        "error": r.error, "started": r.started, "ended": r.ended,
                    })
                })
                .collect(),
        )))
    })
}

fn run_json(cx: &FlowCx, ws: &str, id: &str) -> ApiResult<Json<Value>> {
    cx.store().reader(|c| -> ApiResult<_> {
        let r = runs::get(c, id)?
            .filter(|r| r.workspace_id == ws)
            .ok_or_else(|| ApiError::not_found("运行"))?;
        let mut v = signals::run_view(c, id)?.ok_or_else(|| ApiError::not_found("运行"))?;
        v["error"] = json!(r.error);
        Ok(Json(v))
    })
}

async fn run_detail(
    Extension(cx): Cx,
    who: Caller,
    Path(id): Path<String>,
) -> ApiResult<Json<Value>> {
    run_json(&cx, &who.ws, &id)
}

async fn cancel(Extension(cx): Cx, who: Caller, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let now = cx.now();
    let evt = cx.store().transaction(|tx| -> ApiResult<Option<String>> {
        let r = runs::get(tx, &id)?
            .filter(|r| r.workspace_id == who.ws)
            .ok_or_else(|| ApiError::not_found("运行"))?;
        if !engine::cancel(tx, &r, now)? {
            return Err(ApiError::conflict(steve_i18n::t!("flow.run.already_ended")));
        }
        steve_store::base::audit(tx, &who.ws, &who.me, "run.cancel", &id, &json!({}))?;
        Ok(r.event_id.filter(|_| !r.dry_run))
    })?;
    if let Some(e) = evt {
        cx.signals.signal(cx.store(), &who.ws, &e);
    }
    run_json(&cx, &who.ws, &id)
}
