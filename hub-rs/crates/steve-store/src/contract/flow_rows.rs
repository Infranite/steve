use sea_query::{Expr, ExprTrait, Order, Query, SelectStatement};
use serde::Serialize;
use serde_json::Value;

use super::{Json, Row};
use crate::{Read, Result, W};

pub struct FlowRows<'a, C>(pub &'a mut C);

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

fn definition() -> SelectStatement {
    Query::select()
        .columns([
            "id",
            "workspace_id",
            "kind",
            "name",
            "description",
            "enabled",
            "graph",
            "version",
            "origin",
            "origin_label",
            "deleted",
            "created_by",
            "created",
            "updated",
        ])
        .from("flows")
        .take()
}

fn flow(row: &Row<'_>) -> Result<FlowRow> {
    Ok(FlowRow {
        id: row.get(0)?,
        workspace_id: row.get(1)?,
        kind: row.get(2)?,
        name: row.get(3)?,
        description: row.get(4)?,
        enabled: row.get(5)?,
        graph: row.get::<Json<_>>(6)?.0,
        version: row.get(7)?,
        origin: row.get(8)?,
        origin_label: row.get(9)?,
        deleted: row.get(10)?,
        created_by: row.get(11)?,
        created: row.get(12)?,
        updated: row.get(13)?,
    })
}

#[derive(Serialize)]
pub struct VersionInfo {
    pub version: i64,
    pub author: String,
    pub note: String,
    pub created: i64,
}

#[derive(Serialize)]
pub struct FlowVersion {
    #[serde(flatten)]
    pub info: VersionInfo,
    pub graph: Value,
}

fn version_info(row: &Row<'_>) -> Result<VersionInfo> {
    Ok(VersionInfo {
        version: row.get(0)?,
        author: row.get(1)?,
        note: row.get(2)?,
        created: row.get(3)?,
    })
}

fn versions(id: &str) -> SelectStatement {
    Query::select()
        .columns(["version", "author", "note", "created"])
        .from("flow_versions")
        .and_where(Expr::col("flow_id").eq(id))
        .take()
}

impl<C: Read> FlowRows<'_, C> {
    pub fn has_origin(&mut self, ws: &str, origin: &str) -> Result<bool> {
        self.0
            .one(
                Query::select()
                    .column("id")
                    .from("flows")
                    .and_where(Expr::col("workspace_id").eq(ws))
                    .and_where(Expr::col("origin").eq(origin))
                    .limit(1),
                |_| Ok(()),
            )
            .map(|row| row.is_some())
    }

    pub fn schedule_config(&mut self, id: &str) -> Result<Option<Value>> {
        Ok(self
            .0
            .one(
                Query::select()
                    .column("schedule")
                    .from("flows")
                    .and_where(Expr::col("id").eq(id)),
                |row| {
                    Ok(row
                        .get::<Option<String>>(0)?
                        .and_then(|raw| serde_json::from_str(&raw).ok()))
                },
            )?
            .flatten())
    }

    pub fn labels<'a>(
        &mut self,
        ws: &str,
        ids: impl IntoIterator<Item = &'a str>,
    ) -> Result<std::collections::HashMap<String, (String, String, bool)>> {
        let ids: Vec<_> = ids.into_iter().collect();
        let mut out = std::collections::HashMap::new();
        for ids in ids.chunks(500) {
            out.extend(
                self.0.all(
                    Query::select()
                        .columns(["id", "name", "kind", "deleted"])
                        .from("flows")
                        .and_where(Expr::col("workspace_id").eq(ws))
                        .and_where(Expr::col("id").is_in(ids.iter().copied())),
                    |row| {
                        Ok((
                            row.get::<String>(0)?,
                            (
                                row.get::<String>(1)?,
                                row.get::<String>(2)?,
                                row.get::<bool>(3)?,
                            ),
                        ))
                    },
                )?,
            );
        }
        Ok(out)
    }

    pub fn graph(&mut self, id: &str, version: i64) -> Result<Option<Value>> {
        let pinned = self.0.one(
            Query::select()
                .column("graph")
                .from("flow_versions")
                .and_where(Expr::col("flow_id").eq(id))
                .and_where(Expr::col("version").eq(version)),
            |row| Ok(row.get::<Json<_>>(0)?.0),
        )?;
        if pinned.is_some() {
            return Ok(pinned);
        }
        self.0.one(
            Query::select()
                .column("graph")
                .from("flows")
                .and_where(Expr::col("id").eq(id)),
            |row| Ok(row.get::<Json<_>>(0)?.0),
        )
    }

    pub fn identity(&mut self, id: &str) -> Result<Option<(String, String)>> {
        self.0.one(
            Query::select()
                .columns(["name", "created_by"])
                .from("flows")
                .and_where(Expr::col("id").eq(id)),
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
    }

    pub fn get(&mut self, id: &str) -> Result<Option<FlowRow>> {
        self.0
            .one(definition().and_where(Expr::col("id").eq(id)), flow)
    }

    pub fn list(&mut self, ws: &str, kind: Option<&str>) -> Result<Vec<FlowRow>> {
        let tie = self.0.db().dialect().insertion_order(None);
        let mut query = definition();
        query
            .and_where(Expr::col("workspace_id").eq(ws))
            .and_where(Expr::col("deleted").eq(0_i64))
            .order_by("created", Order::Desc)
            .order_by_expr(tie, Order::Desc);
        if let Some(kind) = kind {
            query.and_where(Expr::col("kind").eq(kind));
        }
        self.0.all(&query, flow)
    }

    pub fn version(&mut self, id: &str, version: i64) -> Result<Option<FlowVersion>> {
        self.0.one(
            versions(id)
                .column("graph")
                .and_where(Expr::col("version").eq(version)),
            |row| {
                Ok(FlowVersion {
                    info: version_info(row)?,
                    graph: row.get::<Json<_>>(4)?.0,
                })
            },
        )
    }

    pub fn versions(&mut self, id: &str) -> Result<Vec<VersionInfo>> {
        self.0
            .all(versions(id).order_by("version", Order::Desc), version_info)
    }
}

pub struct NewFlowRow<'a> {
    pub id: &'a str,
    pub ws: &'a str,
    pub actor: &'a str,
    pub kind: &'a str,
    pub name: &'a str,
    pub description: &'a str,
    pub enabled: bool,
    pub graph: &'a Value,
    pub origin: &'a str,
    pub origin_label: &'a str,
    pub schedule: Option<&'a Value>,
    pub now: i64,
}

pub struct SchedulePatch<'a> {
    pub name: &'a str,
    pub enabled: bool,
    pub graph: &'a Value,
    pub version: i64,
    pub spec: &'a Value,
    pub updated: i64,
}

impl FlowRows<'_, W<'_>> {
    pub fn create(&mut self, row: &NewFlowRow<'_>) -> Result<()> {
        self.0.exec(
            Query::insert()
                .into_table("flows")
                .columns([
                    "id",
                    "workspace_id",
                    "kind",
                    "name",
                    "description",
                    "enabled",
                    "graph",
                    "version",
                    "origin",
                    "origin_label",
                    "created_by",
                    "created",
                    "updated",
                    "schedule",
                ])
                .values_panic([
                    row.id.into(),
                    row.ws.into(),
                    row.kind.into(),
                    row.name.into(),
                    row.description.into(),
                    row.enabled.into(),
                    row.graph.to_string().into(),
                    1_i64.into(),
                    row.origin.into(),
                    row.origin_label.into(),
                    row.actor.into(),
                    row.now.into(),
                    row.now.into(),
                    row.schedule.map(Value::to_string).into(),
                ]),
        )?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn append_version(
        &mut self,
        id: &str,
        ws: &str,
        version: i64,
        graph: &Value,
        author: &str,
        note: &str,
        now: i64,
    ) -> Result<()> {
        self.0.exec(
            Query::insert()
                .into_table("flow_versions")
                .columns([
                    "flow_id",
                    "workspace_id",
                    "version",
                    "graph",
                    "author",
                    "note",
                    "created",
                ])
                .values_panic([
                    id.into(),
                    ws.into(),
                    version.into(),
                    graph.to_string().into(),
                    author.into(),
                    note.into(),
                    now.into(),
                ]),
        )?;
        Ok(())
    }

    pub fn set_graph(&mut self, id: &str, graph: &Value, version: i64, at: i64) -> Result<()> {
        self.0.exec(
            Query::update()
                .table("flows")
                .value("graph", graph.to_string())
                .value("version", version)
                .value("updated", at)
                .and_where(Expr::col("id").eq(id)),
        )?;
        Ok(())
    }

    pub fn patch(
        &mut self,
        id: &str,
        name: &str,
        description: &str,
        enabled: bool,
        at: i64,
    ) -> Result<()> {
        self.0.exec(
            Query::update()
                .table("flows")
                .value("name", name)
                .value("description", description)
                .value("enabled", enabled)
                .value("updated", at)
                .and_where(Expr::col("id").eq(id)),
        )?;
        Ok(())
    }

    pub fn patch_schedule(&mut self, id: &str, patch: &SchedulePatch<'_>) -> Result<()> {
        self.0.exec(
            Query::update()
                .table("flows")
                .value("name", patch.name)
                .value("enabled", patch.enabled)
                .value("graph", patch.graph.to_string())
                .value("version", patch.version)
                .value("schedule", patch.spec.to_string())
                .value("updated", patch.updated)
                .and_where(Expr::col("id").eq(id)),
        )?;
        Ok(())
    }

    pub fn delete(&mut self, id: &str, at: i64) -> Result<()> {
        self.0.exec(
            Query::update()
                .table("flows")
                .value("deleted", true)
                .value("enabled", false)
                .value("updated", at)
                .and_where(Expr::col("id").eq(id)),
        )?;
        Ok(())
    }
}

pub struct NewTrigger<'a> {
    pub id: &'a str,
    pub ws: &'a str,
    pub flow: &'a str,
    pub node: &'a str,
    pub kind: &'a str,
    pub source: Option<&'a str>,
    pub types: Vec<&'a str>,
    pub connections: Vec<&'a str>,
    pub scopes: Vec<&'a str>,
    pub spaces: Vec<&'a str>,
    pub cron: Option<&'a str>,
    pub tz: Option<&'a str>,
    pub next_at: Option<i64>,
    pub missed: Option<&'a str>,
    pub enabled: bool,
}

impl crate::FlowTriggers<'_, W<'_>> {
    pub fn add(&mut self, trigger: &NewTrigger<'_>) -> Result<()> {
        self.0.exec(
            Query::insert()
                .into_table("flow_triggers")
                .columns([
                    "id",
                    "workspace_id",
                    "flow_id",
                    "node_id",
                    "kind",
                    "source",
                    "types",
                    "connections",
                    "scopes",
                    "spaces",
                    "cron",
                    "tz",
                    "next_at",
                    "missed",
                    "enabled",
                ])
                .values_panic([
                    trigger.id.into(),
                    trigger.ws.into(),
                    trigger.flow.into(),
                    trigger.node.into(),
                    trigger.kind.into(),
                    trigger.source.into(),
                    serde_json::to_string(&trigger.types)?.into(),
                    serde_json::to_string(&trigger.connections)?.into(),
                    serde_json::to_string(&trigger.scopes)?.into(),
                    serde_json::to_string(&trigger.spaces)?.into(),
                    trigger.cron.into(),
                    trigger.tz.into(),
                    trigger.next_at.into(),
                    trigger.missed.into(),
                    trigger.enabled.into(),
                ]),
        )?;
        Ok(())
    }

    pub fn retime(
        &mut self,
        flow: &str,
        cron: &str,
        tz: &str,
        missed: &str,
        enabled: bool,
        next_at: Option<i64>,
    ) -> Result<()> {
        self.0.exec(
            Query::update()
                .table("flow_triggers")
                .value("cron", cron)
                .value("tz", tz)
                .value("missed", missed)
                .value("enabled", enabled)
                .value("next_at", next_at)
                .and_where(Expr::col("flow_id").eq(flow))
                .and_where(Expr::col("kind").eq("time")),
        )?;
        Ok(())
    }

    pub fn remove_for_flow(&mut self, flow: &str) -> Result<()> {
        self.0.exec(
            Query::delete()
                .from_table("flow_triggers")
                .and_where(Expr::col("flow_id").eq(flow)),
        )?;
        Ok(())
    }

    pub fn disable_for_flow(&mut self, flow: &str) -> Result<()> {
        self.0.exec(
            Query::update()
                .table("flow_triggers")
                .value("enabled", false)
                .value("next_at", Option::<i64>::None)
                .and_where(Expr::col("flow_id").eq(flow)),
        )?;
        Ok(())
    }
}
