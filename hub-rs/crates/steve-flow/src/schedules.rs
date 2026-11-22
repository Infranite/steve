//! Scheduled tasks as stored: a `flows` row of `kind='schedule'` with the form
//! in `flows.schedule`, a generated two-node graph, a version history and one `flow_triggers` row
//! of `kind='time'`. The storage contract is synchronous; resolving a `when` phrase (which may ask the
//! model) happens before, in [`crate::api`] and [`crate::drafts`].

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use steve_modules::ApiError;
use steve_runtime::tz::Zone;
use steve_store::{
    BotRows, FlowRows, FlowTriggers, NewFlowRow, NewTrigger, ProjectRows, Read, SchedulePatch,
    ScheduleRow, Schedules, TagRows, W, uid,
};

use crate::FlowResult;
use crate::cron::{DescribeCron, zone};
use crate::policy::{MissedPolicy, OverlapPolicy};
use steve_runtime::schedule::CronExpr;

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
        // composed by rule: no model, no session
        Mode::Brief => 0.0,
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
    /// Steve publishes a report to the workspace Feed.
    Report,
    /// The requester's morning brief (steve-digest `brief`), for them only. Composed by rule, without a
    /// Bot; its time is expanded into `time_alarms` rows (family `brief`), so it has no `flow_triggers`
    /// row and the scheduler never runs it.
    Brief,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::ItemAuto => "item_auto",
            Mode::Item => "item",
            Mode::Propose => "propose",
            Mode::Report => "report",
            Mode::Brief => "brief",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "item_auto" => Mode::ItemAuto,
            "item" => Mode::Item,
            "propose" => Mode::Propose,
            "report" => Mode::Report,
            "brief" => Mode::Brief,
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
            Mode::Brief => steve_i18n::t!("flow.schedule.mode_brief"),
        }
    }

    /// How the conversational config's model sees it and says it back (zh-CN, matched by `parse_mode`).
    pub fn label(self) -> &'static str {
        match self {
            Mode::ItemAuto => "建事项，自动验收",
            Mode::Item => "建事项",
            Mode::Propose => "先给方案",
            Mode::Report => "发布到信息流，工作空间成员可见",
            Mode::Brief => "晨报，只发给你自己",
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

    pub fn zone(&self) -> Zone {
        zone(&self.tz).unwrap_or_else(|_| steve_runtime::tz::Zone::system())
    }

    /// The generated graph: `trigger(time) → action`.
    pub fn graph(&self) -> Value {
        let action = match self.mode {
            Mode::Brief => json!({ "action": "brief", "requester": self.requester }),
            Mode::Report => {
                json!({ "action": "report", "what": self.what, "bot": self.bot, "destination": "workspace_feed" })
            }
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

fn from_row(row: ScheduleRow) -> steve_store::Result<Schedule> {
    Ok(Schedule {
        id: row.id,
        workspace_id: row.workspace_id,
        name: row.name,
        enabled: row.enabled,
        version: row.version,
        spec: serde_json::from_value(row.spec)?,
        next_at: row.next_at,
        created_by: row.created_by,
        created: row.created,
        updated: row.updated,
    })
}

/// Reads a morning schedule in the caller's storage snapshot. Its next time is owned by alarms.
pub fn brief(c: &mut impl Read, ws: &str, id: &str) -> steve_store::Result<Option<Schedule>> {
    Schedules(c).live(ws, id)?.map(from_row).transpose()
}

/// A live schedule of `ws`.
pub fn get(c: &mut impl Read, ws: &str, id: &str) -> steve_store::Result<Option<Schedule>> {
    Schedules(c)
        .live_schedule(ws, id, true)?
        .map(from_row)
        .transpose()
}

/// Runs already in progress can still look up a deleted schedule.
pub fn get_any(c: &mut impl Read, id: &str) -> steve_store::Result<Option<Schedule>> {
    Schedules(c).get(id)?.map(from_row).transpose()
}

pub fn own(c: &mut impl Read, ws: &str, id: &str) -> FlowResult<Schedule> {
    get(c, ws, id)?.ok_or_else(|| ApiError::not_found("定时任务"))
}

pub fn list(c: &mut impl Read, ws: &str) -> steve_store::Result<Vec<Schedule>> {
    Schedules(c).list(ws)?.into_iter().map(from_row).collect()
}

pub fn workspace_tz(c: &mut impl Read, ws: &str) -> steve_store::Result<String> {
    person_tz(c, ws, None)
}

pub fn person_tz(c: &mut impl Read, ws: &str, user: Option<&str>) -> steve_store::Result<String> {
    Ok(steve_modules::zone::Zone::for_user(c, ws, user)?
        .name()
        .to_string())
}

/// Whether a brief's cron fires at most once a local day: one minute and one hour.
pub fn once_a_day(cron: &str) -> bool {
    let f: Vec<&str> = cron.split_whitespace().collect();
    f.len() == 5 && f[0].parse::<u8>().is_ok() && f[1].parse::<u8>().is_ok()
}

impl Schedule {
    /// Who may see and change it: anyone in the workspace, except a brief, which is its requester's own.
    pub fn visible_to(&self, user: &str) -> bool {
        self.spec.mode != Mode::Brief || self.spec.requester.as_deref() == Some(user)
    }
}

/// [`own`] for `user`: someone else's brief is not found.
pub fn own_for(c: &mut impl Read, ws: &str, id: &str, user: &str) -> FlowResult<Schedule> {
    Some(own(c, ws, id)?)
        .filter(|s| s.visible_to(user))
        .ok_or_else(|| ApiError::not_found("定时任务"))
}

/// The ownership checks: `bot`, `project_id` and `tags` belong to the workspace. A brief is a member's
/// own (a requester), at most daily, with no Bot, project or tags.
pub fn check_refs(c: &mut impl Read, ws: &str, s: &Spec) -> FlowResult<()> {
    if s.requester.is_some() && s.mode == Mode::Report {
        return Err(ApiError::invalid("requester"));
    }
    if s.mode == Mode::Brief {
        if s.requester.is_none() {
            return Err(ApiError::invalid("requester"));
        }
        if !s.bot.is_empty() || s.project_id.is_some() || !s.tags.is_empty() {
            return Err(ApiError::invalid("mode"));
        }
        if !once_a_day(&s.cron) {
            return Err(ApiError::rule(steve_i18n::t!("flow.schedule.brief_daily")));
        }
    }
    if let Some(user) = &s.requester
        && steve_store::Members::new(c).role(ws, user)?.is_none()
    {
        return Err(ApiError::invalid("requester"));
    }
    if s.mode != Mode::Brief && BotRows(c).local(ws, &s.bot)?.is_none() {
        return Err(ApiError::not_found("Bot"));
    }
    if let Some(p) = &s.project_id
        && !ProjectRows(c).contains(ws, p)?
    {
        return Err(ApiError::not_found("项目"));
    }
    for tag in &s.tags {
        if !TagRows(c).contains(ws, tag)? {
            return Err(ApiError::not_found("标签"));
        }
    }
    Ok(())
}

/// The preflight of an enabled schedule that makes items (#222): its principal and Bot can run
/// ([`crate::Core::can_run`]). A report makes no session; a disabled schedule runs nothing.
pub fn can_run(
    core: &dyn crate::Core,
    store: &steve_store::Store,
    ws: &str,
    s: &Spec,
) -> Result<(), String> {
    if matches!(s.mode, Mode::Report | Mode::Brief) {
        return Ok(());
    }
    core.can_run(store, ws, s.requester.as_deref(), &s.bot)
}

/// [`can_run`] as the API's refusal: `400` with the reason and where to fix it.
pub fn check_can_run(
    core: &dyn crate::Core,
    store: &steve_store::Store,
    ws: &str,
    s: &Spec,
    enabled: bool,
) -> FlowResult<()> {
    if !enabled {
        return Ok(());
    }
    can_run(core, store, ws, s).map_err(ApiError::rule)
}

fn first_run(s: &Spec, now: i64) -> FlowResult<Option<i64>> {
    Ok(s.expr()?.next_after_ms(&s.zone(), now))
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

/// Creates the owner's morning schedule in the same write as their first todo.
pub fn insert_brief(
    w: &mut steve_store::W<'_>,
    ws: &str,
    owner: &str,
    fields: &Fields<'_>,
    origin: &Origin,
    now: i64,
) -> FlowResult<String> {
    check_refs(w, ws, fields.spec)?;
    insert(w, ws, owner, fields, origin, now)
}

/// Inserts a schedule (flow, version 1, trigger). The spec must be validated and its references
/// checked already.
pub fn insert(
    w: &mut W<'_>,
    ws: &str,
    actor: &str,
    f: &Fields<'_>,
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
    let graph = spec.graph();
    let mut rows = FlowRows(w);
    rows.create(&NewFlowRow {
        id: &id,
        ws,
        actor,
        kind: "schedule",
        name,
        description: "",
        enabled,
        graph: &graph,
        origin: &origin.origin,
        origin_label: &origin.label,
        schedule: Some(&serde_json::to_value(spec)?),
        now,
    })?;
    rows.append_version(&id, ws, 1, &graph, actor, "", now)?;
    // The morning brief is planned by alarms, with no time trigger.
    if spec.mode != Mode::Brief {
        let next_at = if enabled { first_run(spec, now)? } else { None };
        FlowTriggers(w).add(&NewTrigger {
            id: &uid("ftr"),
            ws,
            flow: &id,
            node: "t",
            kind: "time",
            source: Some("time"),
            types: vec![],
            connections: vec![],
            scopes: vec![],
            spaces: vec![],
            cron: Some(&spec.cron),
            tz: Some(&spec.tz),
            next_at,
            missed: Some(spec.missed.as_str()),
            enabled,
        })?;
    }
    crate::flows::publish(w, ws, &id);
    Ok(id)
}

/// Writes a changed schedule. A changed graph is a new version; a changed time, time zone, or
/// re-enabling recomputes `next_at` from `now` (time spent disabled is never “missed”).
pub fn update(
    w: &mut W<'_>,
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
    write_update(w, old, f, author, note, now)
}

/// Deleting a Bot cannot transfer its personal authorization to a different Bot. Service
/// schedules keep their existing handoff; personal schedules are disabled and authorization
/// revoked. Only this narrow system transition bypasses the requester's edit check.
pub(crate) fn reassign_deleted_bot(
    w: &mut W<'_>,
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
        w,
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
    w: &mut W<'_>,
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
    let updated = now.max(old.updated + 1);
    let mut rows = FlowRows(w);
    rows.patch_schedule(
        &old.id,
        &SchedulePatch {
            name,
            enabled,
            graph: &graph,
            version,
            spec: &serde_json::to_value(spec)?,
            updated,
        },
    )?;
    if new_version {
        rows.append_version(
            &old.id,
            &old.workspace_id,
            version,
            &graph,
            author,
            note,
            now,
        )?;
    }
    let retime = spec.cron != old.spec.cron || spec.tz != old.spec.tz || (enabled && !old.enabled);
    let next_at = match (enabled, retime) {
        (false, _) => None,
        (true, true) => first_run(spec, now)?,
        (true, false) => old.next_at,
    };
    FlowTriggers(w).retime(
        &old.id,
        &spec.cron,
        &spec.tz,
        spec.missed.as_str(),
        enabled,
        next_at,
    )?;
    crate::flows::publish(w, &old.workspace_id, &old.id);
    Ok(())
}

/// Deletes like a flow: marks it deleted and removes its triggers.
pub fn delete(w: &mut W<'_>, s: &Schedule, now: i64) -> FlowResult<()> {
    FlowRows(w).delete(&s.id, now.max(s.updated + 1))?;
    FlowTriggers(w).remove_for_flow(&s.id)?;
    crate::flows::publish(w, &s.workspace_id, &s.id);
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
    /// Why its items could not run now ([`can_run`]), filled by the API; `None`: they can.
    pub blocked: Option<String>,
    pub version: i64,
    pub created: i64,
    pub updated: i64,
}

/// [`view`] with [`ScheduleView::blocked`] filled (enabled schedules only).
pub fn view_checked(
    core: &dyn crate::Core,
    store: &steve_store::Store,
    c: &mut impl steve_store::Read,
    s: &Schedule,
) -> steve_store::Result<ScheduleView> {
    let mut v = view(c, s)?;
    if s.enabled {
        v.blocked = can_run(core, store, &s.workspace_id, &s.spec).err();
    }
    Ok(v)
}

pub fn view(c: &mut impl steve_store::Read, s: &Schedule) -> steve_store::Result<ScheduleView> {
    let last = steve_store::RunRows(c)
        .last_real(&s.workspace_id, &s.id, None)?
        .map(|r| Last {
            at: r.at(),
            status: r.status.clone(),
            text: crate::runs::display_text(&r).outcome_label,
            item_id: r.work_item_id.clone(),
            cost_usd: r.cost,
        });
    let describe = s
        .spec
        .expr()
        .map(|e| e.describe())
        .unwrap_or_else(|_| steve_i18n::t!("flow.cron.raw", cron = s.spec.cron));
    // a brief: next is its planned alarm, on its requester's clock
    let (next_at, tz) = match (s.spec.mode, &s.spec.requester) {
        (Mode::Brief, Some(user)) => {
            let next = steve_store::Alarms(c).next(&s.workspace_id, &format!("brief:{}", s.id))?;
            let tz = steve_modules::zone::Zone::for_user(c, &s.workspace_id, Some(user))?;
            (next.filter(|_| s.enabled), tz.name().to_string())
        }
        _ => (s.next_at, s.spec.tz.clone()),
    };
    Ok(ScheduleView {
        requester: s.spec.requester.clone(),
        id: s.id.clone(),
        name: s.name.clone(),
        when: s.spec.when.clone(),
        describe,
        cron: s.spec.cron.clone(),
        tz,
        what: s.spec.what.clone(),
        bot: s.spec.bot.clone(),
        mode: s.spec.mode,
        project_id: s.spec.project_id.clone(),
        tags: s.spec.tags.clone(),
        missed: s.spec.missed,
        overlap: s.spec.overlap,
        budget_usd: s.spec.budget_usd,
        enabled: s.enabled,
        next_at,
        last,
        blocked: None,
        version: s.version,
        created: s.created,
        updated: s.updated,
    })
}

/// Local wall time of `ms` in `tz`, formatted.
pub fn local(ms: i64, tz: Zone, fmt: &str) -> String {
    tz.local(ms)
        .ok()
        .map(|t| t.format(fmt).to_string())
        .unwrap_or_default()
}
