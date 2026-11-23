//! Flows as stored: rows, versions, the trigger index, and the
//! `FlowSummary` / `Flow` shapes. Saving is a new version (`flow_versions`, `revisions`) and rebuilds
//! `flow_triggers` from the trigger node; enabling and disabling only flip `enabled`; deleting marks
//! the row and drops its triggers (runs and items stay). Synchronous SQL only.

use serde_json::{Value, json};
use steve_modules::ApiError;
use steve_store::{
    FlowRows, FlowTriggers, NewFlowRow, NewTrigger, Read, Revisions, RunRows, W, uid,
};

use crate::FlowResult;
use crate::cron::zone;
use crate::graph::Graph;
use crate::nodes::trigger::index_kind;
use crate::policy::MissedPolicy;
use steve_runtime::schedule::CronExpr;

/// Flow names: 1..40 characters.
pub const NAME_MAX: usize = 40;
pub const DESCRIPTION_MAX: usize = 500;

pub use steve_store::FlowRow;

pub fn get(c: &mut impl Read, id: &str) -> steve_store::Result<Option<FlowRow>> {
    FlowRows(c).get(id)
}

pub fn own(c: &mut impl Read, ws: &str, id: &str) -> FlowResult<FlowRow> {
    get(c, id)?
        .filter(|f| f.workspace_id == ws && !f.deleted)
        .ok_or_else(|| ApiError::not_found("流程"))
}

pub fn name_of(c: &mut impl Read, id: &str) -> steve_store::Result<String> {
    Ok(FlowRows(c)
        .identity(id)?
        .map_or_else(String::new, |(name, _)| name))
}

/// `origin_label` for an `origin` code.
pub fn origin_label(origin: &str) -> &'static str {
    match origin {
        "me" => "你画的",
        "steve" => "Steve 起草，待你确认",
        "correction" => "按你的纠正",
        o if o.starts_with("plugin:") => "插件提供",
        o if o.starts_with("template:") => "来自模板",
        _ => "",
    }
}

/// The stored `origin_label` as the UI shows it: the standard label of `origin` in the request's language,
/// any other label (a plugin's own) as stored.
pub fn origin_label_shown(origin: &str, stored: &str) -> String {
    use steve_i18n::t;
    if stored != origin_label(origin) {
        return stored.to_string();
    }
    match origin {
        "me" => t!("flow.origin.me"),
        "steve" => t!("flow.origin.steve"),
        "correction" => t!("flow.origin.correction"),
        o if o.starts_with("plugin:") => t!("flow.origin.plugin"),
        o if o.starts_with("template:") => t!("flow.origin.template"),
        _ => stored.to_string(),
    }
}

/// A flow to insert (its graph already validated).
pub struct NewFlow<'a> {
    pub kind: &'a str,
    pub name: &'a str,
    pub description: &'a str,
    pub graph: &'a Graph,
    pub origin: &'a str,
    pub origin_label: &'a str,
    pub enabled: bool,
}

pub(crate) fn publish(w: &mut W<'_>, ws: &str, id: &str) {
    w.emit(steve_types::DomainEvent::from(
        steve_types::AutomationEvent::Flow {
            id: id.into(),
            ws: ws.into(),
        },
    ));
}

/// Inserts a flow at version 1 with its triggers.
pub fn create(w: &mut W<'_>, ws: &str, actor: &str, f: &NewFlow, now: i64) -> FlowResult<String> {
    create_as(w, &uid("flw"), ws, actor, f, (actor, ""), now)
}

/// [`create`] with an id chosen beforehand (a change operation names the flow before it exists).
pub(crate) fn create_as(
    w: &mut W<'_>,
    id: &str,
    ws: &str,
    actor: &str,
    f: &NewFlow,
    version_note: (&str, &str),
    now: i64,
) -> FlowResult<String> {
    let graph = f.graph.to_value();
    let mut rows = FlowRows(w);
    rows.create(&NewFlowRow {
        id,
        ws,
        actor,
        kind: f.kind,
        name: f.name,
        description: f.description,
        enabled: f.enabled,
        graph: &graph,
        origin: f.origin,
        origin_label: f.origin_label,
        schedule: None,
        now,
    })?;
    rows.append_version(id, ws, 1, &graph, version_note.0, version_note.1, now)?;
    rebuild_triggers(w, ws, id, f.graph, f.enabled, now)?;
    w.audit(ws, actor, "flow.create", id, &json!({ "name": f.name }))?;
    publish(w, ws, id);
    Ok(id.to_string())
}

/// Saves `graph` as the next version. Returns the new version.
pub fn save_version(
    w: &mut W<'_>,
    f: &FlowRow,
    graph: &Graph,
    author: &str,
    note: &str,
    now: i64,
) -> FlowResult<i64> {
    let version = f.version + 1;
    let g = graph.to_value();
    let mut rows = FlowRows(w);
    rows.set_graph(&f.id, &g, version, now.max(f.updated + 1))?;
    rows.append_version(&f.id, &f.workspace_id, version, &g, author, note, now)?;
    Revisions(w).append(
        &f.workspace_id,
        "flow",
        &f.id,
        version,
        &json!({ "before": { "graph": f.graph }, "after": { "graph": g } }),
        author,
    )?;
    rebuild_triggers(w, &f.workspace_id, &f.id, graph, f.enabled, now)?;
    w.audit(
        &f.workspace_id,
        author,
        "flow.save",
        &f.id,
        &json!({ "version": version, "note": note }),
    )?;
    publish(w, &f.workspace_id, &f.id);
    Ok(version)
}

fn string_list<'a>(cfg: &'a Value, key: &str) -> Vec<&'a str> {
    cfg[key]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(Value::as_str)
        .collect()
}

fn first_at(cfg: &Value, tz: &str, now: i64) -> Option<i64> {
    let expr = CronExpr::parse(cfg["cron"].as_str().unwrap_or("")).ok()?;
    expr.next_after_ms(&zone(tz).ok()?, now)
}

/// Rebuilds the trigger index of a flow from its trigger node (dispatch reads only this table).
/// Time triggers get their next run computed from `now`.
pub fn rebuild_triggers(
    w: &mut W<'_>,
    ws: &str,
    flow_id: &str,
    graph: &Graph,
    enabled: bool,
    now: i64,
) -> FlowResult<()> {
    FlowTriggers(w).remove_for_flow(flow_id)?;
    let Some(t) = graph.trigger() else {
        return Ok(());
    };
    let cfg = &t.config;
    let kind = index_kind(cfg);
    let (cron, tz, next_at, missed) = if kind == "time" {
        let tz = match cfg["tz"].as_str() {
            Some(tz) => tz.to_string(),
            None => steve_modules::zone::Zone::for_user(w, ws, None)?
                .name()
                .to_string(),
        };
        let missed = MissedPolicy::from_db(cfg["missed"].as_str()).as_str();
        let next = if enabled {
            first_at(cfg, &tz, now)
        } else {
            None
        };
        (
            cfg["cron"].as_str().map(String::from),
            Some(tz),
            next,
            Some(missed),
        )
    } else {
        (None, None, None, None)
    };
    FlowTriggers(w).add(&NewTrigger {
        id: &uid("trg"),
        ws,
        flow: flow_id,
        node: &t.id,
        kind,
        source: cfg["source"].as_str(),
        types: string_list(cfg, "types"),
        connections: string_list(cfg, "connections"),
        scopes: string_list(cfg, "scopes"),
        spaces: string_list(cfg, "spaces"),
        cron: cron.as_deref(),
        tz: tz.as_deref(),
        next_at,
        missed,
        enabled,
    })?;
    Ok(())
}

/// `PATCH /flows/:id`: name, description, enabled (no new version).
pub fn patch(
    w: &mut W<'_>,
    f: &FlowRow,
    actor: &str,
    name: Option<&str>,
    description: Option<&str>,
    enabled: Option<bool>,
    now: i64,
) -> FlowResult<()> {
    let name = name.unwrap_or(&f.name);
    let description = description.unwrap_or(&f.description);
    let enabled = enabled.unwrap_or(f.enabled);
    FlowRows(w).patch(&f.id, name, description, enabled, now.max(f.updated + 1))?;
    if enabled != f.enabled {
        if enabled {
            let graph = Graph::from_value(&f.graph).unwrap_or_default();
            // Re-enabling starts at now. Time spent disabled is never missed.
            rebuild_triggers(w, &f.workspace_id, &f.id, &graph, true, now)?;
        } else {
            FlowTriggers(w).disable_for_flow(&f.id)?;
        }
    }
    w.audit(
        &f.workspace_id,
        actor,
        "flow.patch",
        &f.id,
        &json!({ "name": name, "enabled": enabled }),
    )?;
    publish(w, &f.workspace_id, &f.id);
    Ok(())
}

/// Delete.
pub fn delete(w: &mut W<'_>, f: &FlowRow, actor: &str, now: i64) -> FlowResult<()> {
    FlowRows(w).delete(&f.id, now.max(f.updated + 1))?;
    FlowTriggers(w).remove_for_flow(&f.id)?;
    w.audit(&f.workspace_id, actor, "flow.delete", &f.id, &json!({}))?;
    publish(w, &f.workspace_id, &f.id);
    Ok(())
}

pub fn version(c: &mut impl Read, flow: &str, version: i64) -> steve_store::Result<Option<Value>> {
    FlowRows(c)
        .version(flow, version)?
        .map(serde_json::to_value)
        .transpose()
        .map_err(Into::into)
}

pub fn version_graph(
    c: &mut impl Read,
    flow: &str,
    version: i64,
) -> steve_store::Result<Option<Value>> {
    Ok(FlowRows(c)
        .version(flow, version)?
        .map(|version| version.graph))
}

/// Live flows, newest first.
pub fn list(c: &mut impl Read, ws: &str, kind: Option<&str>) -> steve_store::Result<Vec<FlowRow>> {
    FlowRows(c).list(ws, kind)
}

/// `FlowSummary`.
pub fn summary(c: &mut impl Read, f: &FlowRow, now: i64) -> FlowResult<Value> {
    let graph = Graph::from_value(&f.graph).unwrap_or_default();
    let trigger = graph.trigger().map_or(Value::Null, |t| {
        json!({
            "source": t.config["source"],
            "types": t.config.get("types").cloned().unwrap_or_else(|| json!([])),
        })
    });
    let (runs24, last_run) = RunRows(c).activity(&f.workspace_id, &f.id, now - 24 * 3_600_000)?;
    Ok(json!({
        "id": f.id,
        "kind": f.kind,
        "name": f.name,
        "description": f.description,
        "enabled": f.enabled,
        "version": f.version,
        "origin": f.origin,
        "origin_label": origin_label_shown(&f.origin, &f.origin_label),
        "trigger": trigger,
        "uses_ai": graph.uses_ai(),
        "outcomes": graph.nodes.iter().filter(|n| n.kind == "action").count(),
        "runs24": runs24,
        "last_run": last_run,
        "created": f.created,
        "updated": f.updated,
    }))
}

/// `Flow` = `FlowSummary` + `graph` + `versions`.
pub fn view(c: &mut impl Read, f: &FlowRow, now: i64) -> FlowResult<Value> {
    let mut v = summary(c, f, now)?;
    v["graph"] = f.graph.clone();
    v["versions"] = serde_json::to_value(FlowRows(c).versions(&f.id)?)?;
    // branch/edge mismatches: shown on the canvas, never a refusal
    let g = Graph::from_value(&f.graph).unwrap_or_default();
    v["warnings"] = serde_json::to_value(crate::graph::branch_warnings(&g))?;
    Ok(v)
}
