//! Scheduled tasks as stored: a `flows` row of `kind='schedule'` with the form
//! in `flows.schedule`, a generated two-node graph, a version history and one `flow_triggers` row
//! of `kind='time'`. Everything here is synchronous SQL; resolving a `when` phrase (which may ask the
//! model) happens before, in [`crate::api`] and [`crate::drafts`].

use chrono::DateTime;
use chrono_tz::Tz;
use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use steve_modules::ApiError;
use steve_store::{ChangeEvent, Tx, uid};

use crate::FlowResult;
use crate::cron::{CronExpr, DEFAULT_TZ, parse_tz};
use crate::policy::{MissedPolicy, OverlapPolicy};
use crate::runs;

/// `budget_usd` of a new `report` or `propose` schedule (one model call, or a proposal).
pub const DEFAULT_BUDGET_USD: f64 = 1.0;
/// `budget_usd` of a new `item` or `item_auto` schedule: a coding agent's run costs several
/// dollars (about $9 on average), so $1 would stop ordinary runs.
pub const DEFAULT_ITEM_BUDGET_USD: f64 = 20.0;

/// `budget_usd` of a new schedule in `mode`.
pub fn default_budget_for(mode: Mode) -> f64 {
    match mode {
        Mode::ItemAuto | Mode::Item => DEFAULT_ITEM_BUDGET_USD,
        Mode::Propose | Mode::Report => DEFAULT_BUDGET_USD,
    }
}
/// Upper bound of `budget_usd`.
pub const MAX_BUDGET_USD: f64 = 50.0;

/// `schedule.mode`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Mode {
    /// Item, started, accepted automatically.
    ItemAuto,
    /// Item, started, accepted per bot and global setting.
    Item,
    /// Item that starts with a proposal.
    Propose,
    /// Steve posts a report in the main thread.
    Report,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::ItemAuto => "item_auto",
            Mode::Item => "item",
            Mode::Propose => "propose",
            Mode::Report => "report",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "item_auto" => Mode::ItemAuto,
            "item" => Mode::Item,
            "propose" => Mode::Propose,
            "report" => Mode::Report,
            _ => return None,
        })
    }

    /// The draft card's wording, in the request's language.
    pub fn shown(self) -> String {
        match self {
            Mode::ItemAuto => steve_i18n::t!("flow.schedule.mode_item_auto"),
            Mode::Item => steve_i18n::t!("flow.schedule.mode_item"),
            Mode::Propose => steve_i18n::t!("flow.schedule.mode_propose"),
            Mode::Report => steve_i18n::t!("flow.schedule.mode_report"),
        }
    }

    /// How the conversational config's model sees it and says it back (zh-CN, matched by `parse_mode`).
    pub fn label(self) -> &'static str {
        match self {
            Mode::ItemAuto => "建事项，自动验收",
            Mode::Item => "建事项",
            Mode::Propose => "先给方案",
            Mode::Report => "在对话里汇报",
        }
    }
}

/// `flows.schedule`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Spec {
    /// Explicit personal execution; absent on existing schedules means workspace service access.
    #[serde(default)]
    pub requester: Option<String>,
    /// The phrase as first said (“每周五 09:00”); the UI shows `describe(cron)`.
    pub when: String,
    pub cron: String,
    pub tz: String,
    pub what: String,
    pub bot: String,
    pub mode: Mode,
    #[serde(default)]
    pub project_id: Option<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub missed: MissedPolicy,
    #[serde(default)]
    pub overlap: OverlapPolicy,
    #[serde(default = "default_budget")]
    pub budget_usd: f64,
}

fn default_budget() -> f64 {
    DEFAULT_BUDGET_USD
}

impl Spec {
    pub fn expr(&self) -> FlowResult<CronExpr> {
        CronExpr::parse(&self.cron).map_err(|e| ApiError::Internal(format!("stored cron: {e}")))
    }

    pub fn zone(&self) -> Tz {
        parse_tz(&self.tz).unwrap_or(chrono_tz::Asia::Taipei)
    }

    /// The generated graph: `trigger(time) → action`.
    pub fn graph(&self) -> Value {
        let action = match self.mode {
            Mode::Report => json!({ "action": "report", "what": self.what, "bot": self.bot }),
            m => json!({
                "action": "item.create",
                "mode": if m == Mode::Propose { "propose" } else { "start" },
                "accept": if m == Mode::ItemAuto { "auto" } else { "inherit" },
                "bot": self.bot,
                "project": self.project_id,
                "tags": self.tags,
                "goal": self.what,
                "requester": self.requester,
            }),
        };
        json!({
            "nodes": [
                { "id": "t", "kind": "trigger", "config": { "source": "time", "cron": self.cron, "tz": self.tz, "missed": self.missed.as_str() } },
                { "id": "a", "kind": "action", "config": action },
            ],
            "edges": [{ "from": "t", "to": "a" }],
        })
    }
}

/// A schedule as stored.
#[derive(Debug, Clone, PartialEq)]
pub struct Schedule {
    pub id: String,
    pub workspace_id: String,
    pub name: String,
    pub enabled: bool,
    pub version: i64,
    pub spec: Spec,
    /// `flow_triggers.next_at`; `None` when disabled.
    pub next_at: Option<i64>,
    pub created_by: String,
    pub created: i64,
    /// Also the revision the conversational config checks.
    pub updated: i64,
}

const SELECT: &str = "SELECT f.id, f.workspace_id, f.name, f.enabled, f.version, f.schedule, f.created_by, f.created, f.updated, (SELECT t.next_at FROM flow_triggers t WHERE t.flow_id = f.id AND t.kind = 'time' LIMIT 1) AS next_at FROM flows f";

fn from_row(r: &Row) -> rusqlite::Result<Schedule> {
    let spec: Option<String> = r.get("schedule")?;
    let spec = spec
        .and_then(|s| serde_json::from_str::<Spec>(&s).ok())
        .ok_or_else(|| {
            rusqlite::Error::InvalidColumnType(5, "schedule".into(), rusqlite::types::Type::Text)
        })?;
    let enabled: i64 = r.get("enabled")?;
    Ok(Schedule {
        id: r.get("id")?,
        workspace_id: r.get("workspace_id")?,
        name: r.get("name")?,
        enabled: enabled != 0,
        version: r.get("version")?,
        spec,
        next_at: if enabled != 0 {
            r.get("next_at")?
        } else {
            None
        },
        created_by: r.get("created_by")?,
        created: r.get("created")?,
        updated: r.get("updated")?,
    })
}

/// A live schedule of `ws`, or `None`.
pub fn get(c: &Connection, ws: &str, id: &str) -> FlowResult<Option<Schedule>> {
    Ok(c.query_row(
        &format!("{SELECT} WHERE f.id = ? AND f.workspace_id = ? AND f.kind = 'schedule' AND f.deleted = 0"),
        params![id, ws],
        from_row,
    )
    .optional()?)
}

/// By id, deleted or not (runs of a schedule deleted mid-run still finish).
pub fn get_any(c: &Connection, id: &str) -> FlowResult<Option<Schedule>> {
    Ok(c.query_row(
        &format!("{SELECT} WHERE f.id = ? AND f.kind = 'schedule'"),
        [id],
        from_row,
    )
    .optional()?)
}

/// `own_schedule`: `404 没有这个定时任务`.
pub fn own(c: &Connection, ws: &str, id: &str) -> FlowResult<Schedule> {
    get(c, ws, id)?.ok_or_else(|| ApiError::not_found("定时任务"))
}

pub fn list(c: &Connection, ws: &str) -> FlowResult<Vec<Schedule>> {
    let mut st = c.prepare(&format!(
        "{SELECT} WHERE f.workspace_id = ? AND f.kind = 'schedule' AND f.deleted = 0 ORDER BY f.created"
    ))?;
    let rows = st
        .query_map([ws], from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// The workspace's `tz` setting (a JSON string), else [`DEFAULT_TZ`].
pub fn workspace_tz(c: &Connection, ws: &str) -> FlowResult<String> {
    let v: Option<String> = c
        .query_row(
            "SELECT value FROM settings WHERE workspace_id = ? AND key = 'tz'",
            [ws],
            |r| r.get(0),
        )
        .optional()?;
    Ok(v.and_then(|v| serde_json::from_str::<String>(&v).ok())
        .filter(|t| parse_tz(t).is_ok())
        .unwrap_or_else(|| DEFAULT_TZ.to_string()))
}

/// A bot usable by `ws`: exists, not deleted, member of the workspace.
pub fn bot_of(c: &Connection, ws: &str, id: &str) -> FlowResult<Option<(String, bool)>> {
    Ok(c.query_row(
        "SELECT b.name, b.enabled FROM bots b JOIN bot_workspaces w ON w.bot_id = b.id AND w.workspace_id = ? WHERE b.id = ? AND b.deleted = 0",
        params![ws, id],
        |r| Ok((r.get::<_, String>(0)?, r.get::<_, i64>(1)? != 0)),
    )
    .optional()?)
}

/// The ownership checks: `bot`, `project_id` and `tags` belong to the workspace.
pub fn check_refs(c: &Connection, ws: &str, s: &Spec) -> FlowResult<()> {
    if s.requester.is_some() && s.mode == Mode::Report {
        return Err(ApiError::invalid("requester"));
    }
    if let Some(user) = &s.requester {
        let member: bool = c.query_row(
            "SELECT EXISTS(SELECT 1 FROM workspace_members WHERE workspace_id = ? AND user_id = ?)",
            params![ws, user],
            |r| r.get(0),
        )?;
        if !member {
            return Err(ApiError::invalid("requester"));
        }
    }
    if bot_of(c, ws, &s.bot)?.is_none() {
        return Err(ApiError::not_found("Bot"));
    }
    if let Some(p) = &s.project_id {
        let ok: bool = c.query_row(
            "SELECT EXISTS (SELECT 1 FROM projects WHERE id = ? AND workspace_id = ?)",
            params![p, ws],
            |r| r.get(0),
        )?;
        if !ok {
            return Err(ApiError::not_found("项目"));
        }
    }
    for t in &s.tags {
        let ok: bool = c.query_row(
            "SELECT EXISTS (SELECT 1 FROM tags WHERE id = ? AND workspace_id = ?)",
            params![t, ws],
            |r| r.get(0),
        )?;
        if !ok {
            return Err(ApiError::not_found("标签"));
        }
    }
    Ok(())
}

fn first_run(s: &Spec, now: i64) -> FlowResult<Option<i64>> {
    Ok(s.expr()?.next_after_ms(s.zone(), now))
}

/// Who made a schedule (`flows.origin`, `origin_label`).
#[derive(Debug, Clone)]
pub struct Origin {
    pub origin: String,
    pub label: String,
}

impl Default for Origin {
    fn default() -> Self {
        Self {
            origin: "me".into(),
            label: String::new(),
        }
    }
}

/// The editable part of a schedule.
#[derive(Debug, Clone, Copy)]
pub struct Fields<'a> {
    pub name: &'a str,
    pub spec: &'a Spec,
    pub enabled: bool,
}

/// Inserts a schedule (flow, version 1, trigger). The spec must be validated and its references
/// checked already.
pub fn insert(
    tx: &Tx,
    ws: &str,
    actor: &str,
    f: &Fields,
    origin: &Origin,
    now: i64,
) -> FlowResult<String> {
    let Fields {
        name,
        spec,
        enabled,
    } = *f;
    if spec.requester.as_deref().is_some_and(|u| u != actor) {
        return Err(ApiError::Forbidden(
            "Only the requester can authorize personal schedule execution".into(),
        ));
    }
    let id = uid("flw");
    let graph = spec.graph().to_string();
    tx.execute(
        "INSERT INTO flows (id, workspace_id, kind, name, enabled, graph, version, origin, origin_label, schedule, created_by, created, updated) VALUES (?, ?, 'schedule', ?, ?, ?, 1, ?, ?, ?, ?, ?, ?)",
        params![id, ws, name, enabled, graph, origin.origin, origin.label, serde_json::to_string(spec)?, actor, now, now],
    )?;
    tx.execute(
        "INSERT INTO flow_versions (flow_id, workspace_id, version, graph, author, note, created) VALUES (?, ?, 1, ?, ?, '', ?)",
        params![id, ws, graph, actor, now],
    )?;
    let next_at = if enabled { first_run(spec, now)? } else { None };
    tx.execute(
        "INSERT INTO flow_triggers (id, workspace_id, flow_id, node_id, kind, source, cron, tz, next_at, missed, enabled) VALUES (?, ?, ?, 't', 'time', 'time', ?, ?, ?, ?, ?)",
        params![uid("ftr"), ws, id, spec.cron, spec.tz, next_at, spec.missed.as_str(), enabled],
    )?;
    tx.publish(ChangeEvent::Flow {
        id: id.clone(),
        ws: ws.into(),
    });
    Ok(id)
}

/// Writes a changed schedule. A changed graph is a new version; a changed time, time zone, or
/// re-enabling recomputes `next_at` from `now` (time spent disabled is never “missed”).
pub fn update(
    tx: &Tx,
    old: &Schedule,
    f: &Fields,
    author: &str,
    note: &str,
    now: i64,
) -> FlowResult<()> {
    let spec = f.spec;
    if old.spec.requester.as_deref().is_some_and(|u| u != author)
        || spec.requester.as_deref().is_some_and(|u| u != author)
    {
        return Err(ApiError::Forbidden(
            "Only the requester can edit a personal schedule".into(),
        ));
    }
    write_update(tx, old, f, author, note, now)
}

/// Deleting a Bot cannot transfer its personal authorization to a different Bot. Service
/// schedules keep their existing handoff; personal schedules are disabled and authorization
/// revoked. Only this narrow system transition bypasses the requester's edit check.
pub(crate) fn reassign_deleted_bot(
    tx: &Tx,
    old: &Schedule,
    heir: &str,
    author: &str,
    note: &str,
    now: i64,
) -> FlowResult<()> {
    let mut spec = old.spec.clone();
    spec.bot = heir.into();
    let personal = spec.requester.take().is_some();
    write_update(
        tx,
        old,
        &Fields {
            name: &old.name,
            spec: &spec,
            enabled: old.enabled && !personal,
        },
        author,
        note,
        now,
    )
}

fn write_update(
    tx: &Tx,
    old: &Schedule,
    f: &Fields,
    author: &str,
    note: &str,
    now: i64,
) -> FlowResult<()> {
    let Fields {
        name,
        spec,
        enabled,
    } = *f;
    let graph = spec.graph();
    let new_version = graph != old.spec.graph();
    let version = old.version + i64::from(new_version);
    // Strictly increasing, so two quick edits never look like the same revision.
    let updated = now.max(old.updated + 1);
    tx.execute(
        "UPDATE flows SET name = ?, enabled = ?, graph = ?, version = ?, schedule = ?, updated = ? WHERE id = ?",
        params![name, enabled, graph.to_string(), version, serde_json::to_string(spec)?, updated, old.id],
    )?;
    if new_version {
        tx.execute(
            "INSERT INTO flow_versions (flow_id, workspace_id, version, graph, author, note, created) VALUES (?, ?, ?, ?, ?, ?, ?)",
            params![old.id, old.workspace_id, version, graph.to_string(), author, note, now],
        )?;
    }
    let retime = spec.cron != old.spec.cron || spec.tz != old.spec.tz || (enabled && !old.enabled);
    let next_at = match (enabled, retime) {
        (false, _) => None,
        (true, true) => first_run(spec, now)?,
        (true, false) => old.next_at,
    };
    tx.execute(
        "UPDATE flow_triggers SET cron = ?, tz = ?, missed = ?, enabled = ?, next_at = ? WHERE flow_id = ? AND kind = 'time'",
        params![spec.cron, spec.tz, spec.missed.as_str(), enabled, next_at, old.id],
    )?;
    tx.publish(ChangeEvent::Flow {
        id: old.id.clone(),
        ws: old.workspace_id.clone(),
    });
    Ok(())
}

/// Deletes like a flow: `deleted=1`, triggers removed; runs and items stay.
pub fn delete(tx: &Tx, s: &Schedule, now: i64) -> FlowResult<()> {
    tx.execute(
        "UPDATE flows SET deleted = 1, enabled = 0, updated = ? WHERE id = ?",
        params![now.max(s.updated + 1), s.id],
    )?;
    tx.execute("DELETE FROM flow_triggers WHERE flow_id = ?", [&s.id])?;
    tx.publish(ChangeEvent::Flow {
        id: s.id.clone(),
        ws: s.workspace_id.clone(),
    });
    Ok(())
}

/// `Schedule.last`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Last {
    pub at: i64,
    pub status: String,
    pub text: String,
    pub item_id: Option<String>,
    pub cost_usd: f64,
}

/// The `Schedule` shape of the API.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ScheduleView {
    pub requester: Option<String>,
    pub id: String,
    pub name: String,
    pub when: String,
    pub describe: String,
    pub cron: String,
    pub tz: String,
    pub what: String,
    pub bot: String,
    pub mode: Mode,
    pub project_id: Option<String>,
    pub tags: Vec<String>,
    pub missed: MissedPolicy,
    pub overlap: OverlapPolicy,
    pub budget_usd: f64,
    pub enabled: bool,
    pub next_at: Option<i64>,
    pub last: Option<Last>,
    pub version: i64,
    pub created: i64,
    pub updated: i64,
}

pub fn view(c: &Connection, s: &Schedule) -> FlowResult<ScheduleView> {
    let last = runs::last_real(c, &s.workspace_id, &s.id, None)?.map(|r| Last {
        at: r.at(),
        status: r.status.clone(),
        text: r.outcome_label.clone(),
        item_id: r.work_item_id.clone(),
        cost_usd: r.cost,
    });
    let describe = s
        .spec
        .expr()
        .map(|e| e.describe())
        .unwrap_or_else(|_| steve_i18n::t!("flow.cron.raw", cron = s.spec.cron));
    Ok(ScheduleView {
        requester: s.spec.requester.clone(),
        id: s.id.clone(),
        name: s.name.clone(),
        when: s.spec.when.clone(),
        describe,
        cron: s.spec.cron.clone(),
        tz: s.spec.tz.clone(),
        what: s.spec.what.clone(),
        bot: s.spec.bot.clone(),
        mode: s.spec.mode,
        project_id: s.spec.project_id.clone(),
        tags: s.spec.tags.clone(),
        missed: s.spec.missed,
        overlap: s.spec.overlap,
        budget_usd: s.spec.budget_usd,
        enabled: s.enabled,
        next_at: s.next_at,
        last,
        version: s.version,
        created: s.created,
        updated: s.updated,
    })
}

/// Local wall time of `ms` in `tz`, formatted.
pub fn local(ms: i64, tz: Tz, fmt: &str) -> String {
    DateTime::from_timestamp_millis(ms)
        .map(|t| t.with_timezone(&tz).format(fmt).to_string())
        .unwrap_or_default()
}
