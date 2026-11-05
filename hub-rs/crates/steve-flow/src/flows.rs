//! Flows as stored: rows, versions, the trigger index, and the
//! `FlowSummary` / `Flow` shapes. Saving is a new version (`flow_versions`, `revisions`) and rebuilds
//! `flow_triggers` from the trigger node; enabling and disabling only flip `enabled`; deleting marks
//! the row and drops its triggers (runs and items stay). Synchronous SQL only.

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde_json::{Value, json};
use steve_modules::ApiError;
use steve_store::{ChangeEvent, Tx, uid};

use crate::FlowResult;
use crate::cron::{CronExpr, parse_tz};
use crate::graph::Graph;
use crate::nodes::trigger::index_kind;
use crate::policy::MissedPolicy;

/// Flow names: 1..40 characters.
pub const NAME_MAX: usize = 40;
pub const DESCRIPTION_MAX: usize = 500;

/// One `flows` row.
#[derive(Debug, Clone, PartialEq)]
pub struct FlowRow {
    pub id: String,
    pub workspace_id: String,
    pub kind: String,
    pub name: String,
    pub description: String,
    pub enabled: bool,
    pub graph: Value,
    pub version: i64,
    pub origin: String,
    pub origin_label: String,
    pub deleted: bool,
    pub created_by: String,
    pub created: i64,
    pub updated: i64,
}

impl FlowRow {
    fn from_row(r: &Row) -> rusqlite::Result<Self> {
        let g: String = r.get("graph")?;
        Ok(Self {
            id: r.get("id")?,
            workspace_id: r.get("workspace_id")?,
            kind: r.get("kind")?,
            name: r.get("name")?,
            description: r.get("description")?,
            enabled: r.get("enabled")?,
            graph: serde_json::from_str(&g).unwrap_or_else(|_| json!({"nodes": [], "edges": []})),
            version: r.get("version")?,
            origin: r.get("origin")?,
            origin_label: r.get("origin_label")?,
            deleted: r.get("deleted")?,
            created_by: r.get("created_by")?,
            created: r.get("created")?,
            updated: r.get("updated")?,
        })
    }
}

pub fn get(c: &Connection, id: &str) -> FlowResult<Option<FlowRow>> {
    Ok(
        c.query_row("SELECT * FROM flows WHERE id = ?", [id], FlowRow::from_row)
            .optional()?,
    )
}

/// A flow of `ws` that is not deleted, or `404 没有这个流程`.
pub fn own(c: &Connection, ws: &str, id: &str) -> FlowResult<FlowRow> {
    get(c, id)?
        .filter(|f| f.workspace_id == ws && !f.deleted)
        .ok_or_else(|| ApiError::not_found("流程"))
}

pub fn name_of(c: &Connection, id: &str) -> FlowResult<String> {
    Ok(get(c, id)?.map(|f| f.name).unwrap_or_default())
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

fn publish(tx: &Tx, ws: &str, id: &str) {
    tx.publish(ChangeEvent::Flow {
        id: id.into(),
        ws: ws.into(),
    });
}

/// Inserts a flow at version 1 with its triggers.
pub fn create(tx: &Tx, ws: &str, actor: &str, f: &NewFlow, now: i64) -> FlowResult<String> {
    create_as(tx, &uid("flw"), ws, actor, f, now)
}

/// [`create`] with an id chosen beforehand (a change operation names the flow before it exists).
pub fn create_as(
    tx: &Tx,
    id: &str,
    ws: &str,
    actor: &str,
    f: &NewFlow,
    now: i64,
) -> FlowResult<String> {
    let id = id.to_string();
    let graph = f.graph.to_value().to_string();
    tx.execute(
        "INSERT INTO flows (id, workspace_id, kind, name, description, enabled, graph, version, origin, origin_label, created_by, created, updated) VALUES (?, ?, ?, ?, ?, ?, ?, 1, ?, ?, ?, ?, ?)",
        params![id, ws, f.kind, f.name, f.description, f.enabled, graph, f.origin, f.origin_label, actor, now, now],
    )?;
    tx.execute(
        "INSERT INTO flow_versions (flow_id, workspace_id, version, graph, author, note, created) VALUES (?, ?, 1, ?, ?, '', ?)",
        params![id, ws, graph, actor, now],
    )?;
    rebuild_triggers(tx, ws, &id, f.graph, f.enabled, now)?;
    steve_store::base::audit(
        tx,
        ws,
        actor,
        "flow.create",
        &id,
        &json!({ "name": f.name }),
    )?;
    publish(tx, ws, &id);
    Ok(id)
}

/// Saves `graph` as the next version. Returns the new version.
pub fn save_version(
    tx: &Tx,
    f: &FlowRow,
    graph: &Graph,
    author: &str,
    note: &str,
    now: i64,
) -> FlowResult<i64> {
    let version = f.version + 1;
    let g = graph.to_value();
    tx.execute(
        "UPDATE flows SET graph = ?, version = ?, updated = ? WHERE id = ?",
        params![g.to_string(), version, now.max(f.updated + 1), f.id],
    )?;
    tx.execute(
        "INSERT INTO flow_versions (flow_id, workspace_id, version, graph, author, note, created) VALUES (?, ?, ?, ?, ?, ?, ?)",
        params![f.id, f.workspace_id, version, g.to_string(), author, note, now],
    )?;
    steve_store::base::revision(
        tx,
        &f.workspace_id,
        "flow",
        &f.id,
        version,
        &json!({ "graph": f.graph }),
        &json!({ "graph": g }),
        author,
    )?;
    rebuild_triggers(tx, &f.workspace_id, &f.id, graph, f.enabled, now)?;
    steve_store::base::audit(
        tx,
        &f.workspace_id,
        author,
        "flow.save",
        &f.id,
        &json!({ "version": version, "note": note }),
    )?;
    publish(tx, &f.workspace_id, &f.id);
    Ok(version)
}

fn list_json(cfg: &Value, key: &str) -> String {
    let xs: Vec<String> = cfg[key]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|x| x.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default();
    serde_json::to_string(&xs).unwrap_or_else(|_| "[]".into())
}

fn first_at(cfg: &Value, tz: &str, now: i64) -> Option<i64> {
    let expr = CronExpr::parse(cfg["cron"].as_str().unwrap_or("")).ok()?;
    expr.next_after_ms(parse_tz(tz).ok()?, now)
}

/// Rebuilds the trigger index of a flow from its trigger node (dispatch reads only this table).
/// Time triggers get their next run computed from `now`.
pub fn rebuild_triggers(
    tx: &Tx,
    ws: &str,
    flow_id: &str,
    graph: &Graph,
    enabled: bool,
    now: i64,
) -> FlowResult<()> {
    tx.execute("DELETE FROM flow_triggers WHERE flow_id = ?", [flow_id])?;
    let Some(t) = graph.trigger() else {
        return Ok(());
    };
    let cfg = &t.config;
    let kind = index_kind(cfg);
    let (cron, tz, next_at, missed) = if kind == "time" {
        let tz = cfg["tz"]
            .as_str()
            .map(String::from)
            .map_or_else(|| crate::schedules::workspace_tz(tx, ws), Ok)?;
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
    tx.execute(
        "INSERT INTO flow_triggers (id, workspace_id, flow_id, node_id, kind, source, types, connections, scopes, spaces, cron, tz, next_at, missed, enabled) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
        params![
            uid("trg"),
            ws,
            flow_id,
            t.id,
            kind,
            cfg["source"].as_str(),
            list_json(cfg, "types"),
            list_json(cfg, "connections"),
            list_json(cfg, "scopes"),
            list_json(cfg, "spaces"),
            cron,
            tz,
            next_at,
            missed,
            enabled
        ],
    )?;
    Ok(())
}

/// `PATCH /flows/:id`: name, description, enabled (no new version).
pub fn patch(
    tx: &Tx,
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
    tx.execute(
        "UPDATE flows SET name = ?, description = ?, enabled = ?, updated = ? WHERE id = ?",
        params![name, description, enabled, now.max(f.updated + 1), f.id],
    )?;
    if enabled != f.enabled {
        let graph = Graph::from_value(&f.graph).unwrap_or_default();
        if enabled {
            // re-enabling computes the next time from now: time spent disabled is never “missed”
            rebuild_triggers(tx, &f.workspace_id, &f.id, &graph, true, now)?;
        } else {
            tx.execute(
                "UPDATE flow_triggers SET enabled = 0, next_at = NULL WHERE flow_id = ?",
                [&f.id],
            )?;
        }
    }
    steve_store::base::audit(
        tx,
        &f.workspace_id,
        actor,
        "flow.patch",
        &f.id,
        &json!({ "name": name, "enabled": enabled }),
    )?;
    publish(tx, &f.workspace_id, &f.id);
    Ok(())
}

/// Delete.
pub fn delete(tx: &Tx, f: &FlowRow, actor: &str, now: i64) -> FlowResult<()> {
    tx.execute(
        "UPDATE flows SET deleted = 1, enabled = 0, updated = ? WHERE id = ?",
        params![now.max(f.updated + 1), f.id],
    )?;
    tx.execute("DELETE FROM flow_triggers WHERE flow_id = ?", [&f.id])?;
    steve_store::base::audit(tx, &f.workspace_id, actor, "flow.delete", &f.id, &json!({}))?;
    publish(tx, &f.workspace_id, &f.id);
    Ok(())
}

/// One version: `{version, author, note, created, graph}`.
pub fn version(c: &Connection, flow_id: &str, version: i64) -> FlowResult<Option<Value>> {
    let v = c
        .query_row(
            "SELECT version, author, note, created, graph FROM flow_versions WHERE flow_id = ? AND version = ?",
            params![flow_id, version],
            |r| {
                Ok(json!({
                    "version": r.get::<_, i64>(0)?,
                    "author": r.get::<_, String>(1)?,
                    "note": r.get::<_, String>(2)?,
                    "created": r.get::<_, i64>(3)?,
                    "graph": serde_json::from_str::<Value>(&r.get::<_, String>(4)?)
                        .unwrap_or(Value::Null),
                }))
            },
        )
        .optional()?;
    Ok(v)
}

/// The graph of a version.
pub fn version_graph(c: &Connection, flow_id: &str, version: i64) -> FlowResult<Option<Value>> {
    let g: Option<String> = c
        .query_row(
            "SELECT graph FROM flow_versions WHERE flow_id = ? AND version = ?",
            params![flow_id, version],
            |r| r.get(0),
        )
        .optional()?;
    Ok(g.and_then(|g| serde_json::from_str(&g).ok()))
}

/// Flows of `ws` (not deleted) of `kind` (`None` = all), newest first.
pub fn list(c: &Connection, ws: &str, kind: Option<&str>) -> FlowResult<Vec<FlowRow>> {
    let mut st = c.prepare_cached(
        "SELECT * FROM flows WHERE workspace_id = ?1 AND deleted = 0 AND (?2 IS NULL OR kind = ?2) ORDER BY created DESC, rowid DESC",
    )?;
    let rows = st
        .query_map(params![ws, kind], FlowRow::from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// `FlowSummary`.
pub fn summary(c: &Connection, f: &FlowRow, now: i64) -> FlowResult<Value> {
    let graph = Graph::from_value(&f.graph).unwrap_or_default();
    let trigger = graph.trigger().map_or(Value::Null, |t| {
        json!({
            "source": t.config["source"],
            "types": t.config.get("types").cloned().unwrap_or_else(|| json!([])),
        })
    });
    let (runs24, last_run): (i64, Option<i64>) = c.query_row(
        "SELECT count(CASE WHEN started >= ? THEN 1 END), max(started) FROM flow_runs WHERE workspace_id = ? AND flow_id = ? AND dry_run = 0 AND status != 'skipped'",
        params![now - 24 * 3_600_000, f.workspace_id, f.id],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )?;
    Ok(json!({
        "id": f.id,
        "kind": f.kind,
        "name": f.name,
        "description": f.description,
        "enabled": f.enabled,
        "version": f.version,
        "origin": f.origin,
        "origin_label": f.origin_label,
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
pub fn view(c: &Connection, f: &FlowRow, now: i64) -> FlowResult<Value> {
    let mut v = summary(c, f, now)?;
    let mut st = c.prepare_cached(
        "SELECT version, author, note, created FROM flow_versions WHERE flow_id = ? ORDER BY version DESC",
    )?;
    let versions = st
        .query_map([&f.id], |r| {
            Ok(json!({
                "version": r.get::<_, i64>(0)?,
                "author": r.get::<_, String>(1)?,
                "note": r.get::<_, String>(2)?,
                "created": r.get::<_, i64>(3)?,
            }))
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    v["graph"] = f.graph.clone();
    v["versions"] = Value::Array(versions);
    Ok(v)
}
