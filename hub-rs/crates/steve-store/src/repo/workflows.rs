//! Persistent item work graphs. Scheduling decisions and the child item they reserve commit in
//! one transaction; no process or credential is created here. The core starts the child's ordinary
//! Node session after the reservation, and recovery resumes that same child.

use std::collections::{HashMap, HashSet};

use rusqlite::{Connection, OptionalExtension, params};
use sea_query::{Expr, ExprTrait, Query};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use steve_types::orchestration::{Finding, ReviewReceipt, ReviewScope, ReviewedUnit, ReworkReason};

use super::{all, next_counter, one, to_json};
use crate::{Item, Parent, Result, Store, StoreError, W, now};

fn required() -> bool {
    true
}
fn work() -> String {
    "work".into()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnitSpec {
    pub id: String,
    pub title: String,
    pub goal: String,
    pub bot_id: String,
    #[serde(default = "work")]
    pub kind: String,
    #[serde(default)]
    pub depends_on: Vec<String>,
    #[serde(default)]
    pub resources: Vec<String>,
    #[serde(default = "required")]
    pub required: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkUnit {
    #[serde(flatten)]
    pub spec: UnitSpec,
    pub status: String,
    pub item_id: Option<String>,
    pub session_id: Option<String>,
    pub attempt: i64,
    pub summary: String,
    pub outputs: Value,
    pub checks: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub verified_output: Option<String>,
    #[serde(skip)]
    pub inputs: Option<Vec<ReviewedUnit>>,
    pub reason: Option<String>,
}

impl WorkUnit {
    pub fn active(&self) -> bool {
        matches!(self.status.as_str(), "dispatching" | "running")
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct WorkflowHistory {
    pub revision: i64,
    pub action: String,
    pub reason: String,
    pub unit_id: Option<String>,
    pub detail: Value,
    pub created: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ArchivedWorkflowAttempt {
    pub item_id: String,
    pub unit_id: String,
    pub snapshot: WorkUnit,
    pub created: i64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct WorkflowActivity {
    pub state: &'static str,
    pub observed_at: i64,
    pub node_id: String,
    pub session_id: String,
    pub turn_id: String,
    pub attempt_id: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Workflow {
    pub item_id: String,
    pub revision: i64,
    pub cycle: i64,
    pub goal_version: i64,
    pub paused: bool,
    pub max_parallel: usize,
    pub max_attempts: i64,
    pub units: Vec<WorkUnit>,
    pub history: Vec<WorkflowHistory>,
}

/// Who paused a work graph ([`Workflow::paused_by`]).
pub const PAUSED_BY_AGENT: &str = "agent";
pub const PAUSED_BY_USER: &str = "user";
pub const PAUSED_BY_ITEM: &str = "item";

/// The history notes the store writes itself (zh-CN; steve-core words them for the reader).
pub const NOTE_CLAIMED: &str = "调度就绪工作";
pub const NOTE_SESSION_BOUND: &str = "工作会话已绑定";

impl Workflow {
    /// Who paused the graph and why, while it is paused: [`PAUSED_BY_AGENT`] (its coordinator or
    /// another Bot, with the tool), [`PAUSED_BY_USER`] or [`PAUSED_BY_ITEM`] (the item itself was
    /// paused or dropped). A pause recorded without saying who counts as the agent's.
    pub fn paused_by(&self) -> Option<(&str, &str)> {
        if !self.paused {
            return None;
        }
        let h = self.history.iter().rev().find(|h| h.action == "pause")?;
        Some((
            h.detail["by"].as_str().unwrap_or(PAUSED_BY_AGENT),
            h.reason.as_str(),
        ))
    }

    /// The existing append-only history is the authority for an explicit acceptance-mode change.
    /// Replanning reinstates collaboration; ordinary pause/cancel never waive required evidence.
    pub fn single_session(&self) -> bool {
        self.history
            .iter()
            .rev()
            .find(|h| matches!(h.action.as_str(), "single_session" | "plan" | "replan"))
            .is_some_and(|h| h.action == "single_session")
    }

    /// A result depending on an unresolved predecessor remains on record while awaiting evidence.
    pub fn dependency_ready(&self, id: &str) -> bool {
        let mut seen = HashSet::new();
        let mut pending = vec![id];
        while let Some(id) = pending.pop() {
            if !seen.insert(id) {
                continue;
            }
            let Some(unit) = self.units.iter().find(|u| u.spec.id == id) else {
                return false;
            };
            if unit.status != "succeeded" {
                return false;
            }
            pending.extend(unit.spec.depends_on.iter().map(String::as_str));
        }
        true
    }

    pub fn depends_on(&self, unit: &WorkUnit, target: &str) -> bool {
        let mut seen = HashSet::new();
        let mut pending: Vec<&str> = unit.spec.depends_on.iter().map(String::as_str).collect();
        while let Some(id) = pending.pop() {
            if id == target {
                return true;
            }
            if seen.insert(id)
                && let Some(parent) = self.units.iter().find(|u| u.spec.id == id)
            {
                pending.extend(parent.spec.depends_on.iter().map(String::as_str));
            }
        }
        false
    }

    fn reviewed_units(&self, review: &WorkUnit) -> Option<Vec<ReviewedUnit>> {
        review
            .spec
            .depends_on
            .iter()
            .map(|id| {
                let unit = self.units.iter().find(|u| &u.spec.id == id)?;
                self.dependency_ready(id).then(|| ReviewedUnit {
                    unit: id.clone(),
                    attempt: unit.attempt,
                    input_digest: unit.verified_output.clone(),
                })
            })
            .collect()
    }

    pub fn review_scope(&self, review: &WorkUnit) -> Option<ReviewScope> {
        if review.spec.kind != "review" || review.status != "failed" {
            return None;
        }
        let units = self.reviewed_units(review)?;
        if review.inputs.as_ref() != Some(&units) {
            return None;
        }
        Some(ReviewScope {
            cycle: self.cycle,
            graph_revision: self.revision,
            review_unit: review.spec.id.clone(),
            review_attempt: review.attempt,
            units,
        })
    }

    /// Optional work may be waived by cancelling it; required units need successful evidence unless
    /// the user explicitly chose single-session acceptance. Active work always prevents closing.
    pub fn blockers(&self) -> Vec<String> {
        let mut out: Vec<_> = self
            .units
            .iter()
            .filter(|u| {
                (!self.single_session() && u.spec.required && !self.dependency_ready(&u.spec.id))
                    || u.active()
            })
            .map(|u| format!("{}（{}）", u.spec.title, u.status))
            .collect();
        if self.paused && !self.single_session() {
            out.push("协作工作已暂停".into());
        }
        out
    }
}

impl Store {
    pub fn workflows(&self) -> Workflows<'_> {
        Workflows(self)
    }
}

pub struct Workflows<'a>(&'a Store);

fn conflict(message: impl Into<String>) -> StoreError {
    StoreError::Conflict(message.into())
}

fn decode<T: serde::de::DeserializeOwned>(r: &rusqlite::Row<'_>, col: &str) -> rusqlite::Result<T> {
    let text: String = r.get(col)?;
    serde_json::from_str(&text).map_err(|e| {
        rusqlite::Error::FromSqlConversionFailure(
            r.as_ref().column_index(col).unwrap_or(0),
            rusqlite::types::Type::Text,
            Box::new(e),
        )
    })
}

fn unit_row(r: &rusqlite::Row<'_>) -> rusqlite::Result<WorkUnit> {
    Ok(WorkUnit {
        spec: decode(r, "spec")?,
        status: r.get("status")?,
        item_id: r.get("item_id")?,
        session_id: r.get("session_id")?,
        attempt: r.get("attempt")?,
        summary: r.get("summary")?,
        outputs: decode(r, "outputs")?,
        checks: decode(r, "checks")?,
        verified_output: r.get("verified_output")?,
        inputs: r
            .get::<_, Option<String>>("inputs")?
            .map(|_| decode(r, "inputs"))
            .transpose()?,
        reason: r.get("reason")?,
    })
}

pub fn get(c: &Connection, parent: &str) -> Result<Option<Workflow>> {
    let Some(mut graph) = one(
        c,
        "SELECT * FROM workflows WHERE item_id=?",
        [parent],
        |r| {
            Ok(Workflow {
                item_id: r.get("item_id")?,
                revision: r.get("revision")?,
                cycle: r.get("cycle")?,
                goal_version: r.get("goal_version")?,
                paused: r.get("paused")?,
                max_parallel: r.get::<_, i64>("max_parallel")? as usize,
                max_attempts: r.get("max_attempts")?,
                units: Vec::new(),
                history: Vec::new(),
            })
        },
    )?
    else {
        return Ok(None);
    };
    graph.units = all(
        c,
        "SELECT * FROM workflow_units WHERE workflow_id=? ORDER BY position",
        [parent],
        unit_row,
    )?;
    graph.history = all(
        c,
        "SELECT * FROM workflow_history WHERE workflow_id=? ORDER BY revision",
        [parent],
        |r| {
            Ok(WorkflowHistory {
                revision: r.get("revision")?,
                action: r.get("action")?,
                reason: r.get("reason")?,
                unit_id: r.get("unit_id")?,
                detail: decode(r, "detail")?,
                created: r.get("created")?,
            })
        },
    )?;
    Ok(Some(graph))
}

fn must_graph(c: &Connection, parent: &str) -> Result<Workflow> {
    get(c, parent)?.ok_or(StoreError::NotFound("没有这个协作工作图"))
}

fn must_unit<'a>(graph: &'a Workflow, id: &str) -> Result<&'a WorkUnit> {
    graph
        .units
        .iter()
        .find(|u| u.spec.id == id)
        .ok_or(StoreError::NotFound("没有这个工作单元"))
}

fn open_parent(c: &Connection, parent: &str) -> Result<Item> {
    let item = one(
        c,
        "SELECT * FROM items WHERE id=?",
        [parent],
        Item::from_row,
    )?
    .ok_or(StoreError::NotFound("没有这个事项"))?;
    if item.phase == steve_types::ItemPhase::Closed || item.archived {
        return Err(conflict("事项已结束，不能调整协作工作"));
    }
    if item.kind != "task" {
        return Err(conflict("只有任务事项可以编排协作工作"));
    }
    Ok(item)
}

fn bot_available(c: &Connection, ws: &str, bot: &str) -> Result<()> {
    let valid: bool = c.query_row(
        "SELECT EXISTS(SELECT 1 FROM bots b JOIN bot_workspaces w ON w.bot_id=b.id
         WHERE b.id=? AND w.workspace_id=? AND b.enabled=1 AND b.deleted=0)",
        params![bot, ws],
        |r| r.get(0),
    )?;
    if !valid {
        return Err(conflict(format!("Bot {bot} 不在当前工作空间或已停用")));
    }
    Ok(())
}

/// Validates the complete graph before any write. Reviews cover work transitively, and integration
/// follows both required work and required reviews. Author and reviewer must be different Bots.
pub fn validate(specs: &[UnitSpec], max_parallel: usize, max_attempts: i64) -> Result<()> {
    if max_parallel == 0 || max_parallel > i64::MAX as usize || max_attempts <= 0 {
        return Err(conflict("并发数和尝试预算必须大于零"));
    }
    let mut by_id = HashMap::new();
    for u in specs {
        if u.id.trim().is_empty()
            || u.title.trim().is_empty()
            || u.goal.trim().is_empty()
            || u.bot_id.trim().is_empty()
        {
            return Err(conflict("工作单元必须有 id、标题、目标和 Bot"));
        }
        if !matches!(u.kind.as_str(), "work" | "review" | "integrate") {
            return Err(conflict("工作单元类型必须是 work、review 或 integrate"));
        }
        if by_id.insert(u.id.as_str(), u).is_some() {
            return Err(conflict(format!("工作单元 id 重复：{}", u.id)));
        }
        if u.resources.iter().any(|r| r.trim().is_empty()) {
            return Err(conflict("资源声明不能是空字符串"));
        }
    }
    for u in specs {
        let mut seen = HashSet::new();
        for dep in &u.depends_on {
            if !by_id.contains_key(dep.as_str()) {
                return Err(conflict(format!("{} 的依赖 {dep} 不存在", u.id)));
            }
            if !seen.insert(dep) {
                return Err(conflict(format!("{} 的依赖重复：{dep}", u.id)));
            }
        }
    }
    // Kahn's algorithm avoids a stack depth limit for large user-defined graphs.
    let mut pending: HashMap<&str, usize> = specs
        .iter()
        .map(|u| (u.id.as_str(), u.depends_on.len()))
        .collect();
    let mut dependents: HashMap<&str, Vec<&str>> = HashMap::new();
    for u in specs {
        for dep in &u.depends_on {
            dependents.entry(dep).or_default().push(&u.id);
        }
    }
    let mut ready: Vec<&str> = pending
        .iter()
        .filter(|(_, n)| **n == 0)
        .map(|(id, _)| *id)
        .collect();
    let mut visited = 0;
    while let Some(id) = ready.pop() {
        visited += 1;
        if let Some(next) = dependents.get(id) {
            for dep in next {
                let n = pending.get_mut(dep).expect("validated dependency");
                *n -= 1;
                if *n == 0 {
                    ready.push(dep);
                }
            }
        }
    }
    if visited != specs.len() {
        return Err(conflict("工作依赖形成环"));
    }
    let ancestors = |u: &UnitSpec| -> HashSet<String> {
        let mut set = HashSet::new();
        let mut todo = u.depends_on.clone();
        while let Some(id) = todo.pop() {
            if set.insert(id.clone()) {
                todo.extend(by_id[id.as_str()].depends_on.clone());
            }
        }
        set
    };
    let reviews: Vec<_> = specs
        .iter()
        .filter(|u| u.kind == "review" && u.required)
        .collect();
    let integrates: Vec<_> = specs
        .iter()
        .filter(|u| u.kind == "integrate" && u.required)
        .collect();
    if reviews.is_empty() || integrates.is_empty() {
        return Err(conflict("协作工作图必须包含必需的独立评审和集成验收单元"));
    }
    for review in specs.iter().filter(|u| u.kind == "review") {
        let upstream = ancestors(review);
        if !upstream.iter().any(|id| by_id[id.as_str()].kind == "work") {
            return Err(conflict("评审单元必须依赖要评审的工作"));
        }
        if upstream.iter().any(|id| {
            let u = by_id[id.as_str()];
            u.kind == "work" && u.bot_id == review.bot_id
        }) {
            return Err(conflict(format!(
                "{} 不能由被评审工作的 Bot 自评",
                review.id
            )));
        }
    }
    for u in specs.iter().filter(|u| u.required && u.kind == "work") {
        if !reviews.iter().any(|r| ancestors(r).contains(&u.id)) {
            return Err(conflict(format!("必需工作 {} 没有独立评审覆盖", u.id)));
        }
    }
    // One final integration must cover the entire required result; unrelated integrations cannot
    // accidentally satisfy the unified acceptance gate.
    if !integrates.iter().any(|i| {
        let upstream = ancestors(i);
        specs
            .iter()
            .filter(|u| u.required && u.id != i.id)
            .all(|u| upstream.contains(&u.id))
    }) {
        return Err(conflict("需要一个集成验收单元覆盖全部必需工作和评审"));
    }
    Ok(())
}

fn record(
    w: &mut W<'_>,
    parent: &str,
    action: &str,
    reason: &str,
    unit: Option<&str>,
    detail: &Value,
) -> Result<()> {
    use crate::contract::Q;
    let at = now();
    let revision: i64 = w
        .one(
            Query::update()
                .table("workflows")
                .value("revision", Expr::col("revision").add(1_i64))
                .value("updated", at)
                .and_where(Expr::col("item_id").eq(parent))
                .returning(Query::returning().column("revision")),
            |row| row.get(0),
        )?
        .ok_or(StoreError::NotFound("没有这个协作工作图"))?;
    w.exec(
        Query::insert()
            .into_table("workflow_history")
            .columns([
                "workflow_id",
                "revision",
                "action",
                "reason",
                "unit_id",
                "detail",
                "created",
            ])
            .values_panic([
                parent.into(),
                revision.into(),
                action.into(),
                reason.into(),
                unit.into(),
                to_json(detail)?.into(),
                at.into(),
            ]),
    )?;
    if let Some(ws) = crate::item_repo::item_workspace(w, parent)? {
        w.emit(steve_types::WorkEvent::Item {
            ws: ws.into(),
            id: parent.into(),
        });
    }
    Ok(())
}

/// Keeps a retired attempt's snapshot (one row per child item and attempt: a retry reuses the
/// unit's child), cancelled by the reset or replan (a success is invalidated
/// with it). A failure stays failed with its own reason: it is the authoritative outcome of that
/// attempt, and a turn end that comes late closes its child as failed, never as cancelled.
fn archive_attempt(tx: &Connection, parent: &str, unit: &WorkUnit, reason: &str) -> Result<()> {
    if let Some(child) = &unit.item_id {
        let mut old = unit.clone();
        if old.status != "failed" {
            old.status = "cancelled".into();
            old.reason = Some(format!("尝试已失效：{reason}"));
        }
        tx.execute(
            "INSERT OR IGNORE INTO workflow_attempts(item_id,attempt,workflow_id,unit_id,snapshot,created)
             VALUES (?,?,?,?,?,?)",
            params![child, unit.attempt, parent, unit.spec.id, to_json(&old)?, now()],
        )?;
    }
    Ok(())
}

// A new item cycle supersedes the graph, not the evidence. Retried attempts are invalidated by
// archive_attempt; completed units from an older cycle keep their actual verdict for inspection.
fn archive_cycle_unit(tx: &Connection, parent: &str, unit: &WorkUnit) -> Result<()> {
    if let Some(child) = &unit.item_id {
        tx.execute(
            "INSERT OR IGNORE INTO workflow_attempts(item_id,attempt,workflow_id,unit_id,snapshot,created)
             VALUES (?,?,?,?,?,?)",
            params![child, unit.attempt, parent, unit.spec.id, to_json(unit)?, now()],
        )?;
    }
    Ok(())
}

/// Claims exactly one ready unit inside the caller's write transaction. Its first claim creates the
/// unit's child item; a retry reuses that child (#182), whose new cycle the caller opened in the same
/// transaction (`ready`): the item keeps its number, branch and worktree, and the attempt gets a new
/// session. Retrying an already claimed unit returns the same child; a process restart never creates
/// a second attempt.
pub fn claim_in(w: &mut W<'_>, parent: &str, id: &str) -> Result<WorkUnit> {
    let tx = w.conn;
    let item = open_parent(tx, parent)?;
    if item.phase == steve_types::ItemPhase::Closing {
        return Err(conflict("事项正在关闭，需要先恢复执行才能调度工作"));
    }
    let graph = must_graph(tx, parent)?;
    let goal_version = crate::item_repo::latest_goal(w, parent)?.map_or(0, |(version, _)| version);
    if graph.cycle != item.cycle || graph.goal_version != goal_version {
        return Err(conflict(
            "主事项目标或执行周期已变化，需要重新规划后才能调度",
        ));
    }
    let unit = must_unit(&graph, id)?;
    if unit.active() {
        return Ok(unit.clone());
    }
    if graph.paused || graph.single_session() {
        return Err(conflict("协作工作已暂停"));
    }
    if unit.status != "pending" {
        return Err(conflict("工作单元需要先重置，才能再次调度"));
    }
    if unit.attempt >= graph.max_attempts {
        return Err(conflict("工作单元已达到尝试预算"));
    }
    if graph.units.iter().filter(|u| u.active()).count() >= graph.max_parallel {
        return Err(conflict("已达到工作图并发上限"));
    }
    for dep in &unit.spec.depends_on {
        if !graph.dependency_ready(dep) {
            return Err(conflict(format!("前置工作 {dep} 尚未成功")));
        }
    }
    if graph.units.iter().filter(|u| u.active()).any(|u| {
        unit.spec
            .resources
            .iter()
            .any(|r| u.spec.resources.contains(r))
    }) {
        return Err(conflict("工作资源仍被另一单元使用"));
    }
    bot_available(tx, &item.workspace_id, &unit.spec.bot_id)?;
    if let Some(project) = &item.project_id {
        let isolated: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM projects WHERE id=? AND workspace_id=? AND worktree=1 AND kind='git' AND archived=0)",
                params![project,item.workspace_id],|r|r.get(0)
            )?;
        if !isolated {
            return Err(conflict(
                "协作项目需要启用独立 Git worktree，才能隔离工作目录",
            ));
        }
    }
    let inputs = graph
        .reviewed_units(unit)
        .ok_or_else(|| conflict("work unit inputs unavailable"))?;
    tx.execute(
        "UPDATE workflow_units SET inputs=? WHERE workflow_id=? AND id=?",
        params![to_json(&inputs)?, parent, id],
    )?;
    if let Some(child) = unit.item_id.as_deref() {
        return reclaim_in(w, &item, unit, child);
    }
    let inherited: Vec<String> = all(tx, super::catalog::ITEM_TAGS, [parent], |r| r.get(0))?;
    let tags = super::kinds::checked(tx, &item.workspace_id, &inherited)?;
    let prefix: String = tx.query_row(
        "SELECT item_prefix FROM workspaces WHERE id=?",
        [&item.workspace_id],
        |r| r.get(0),
    )?;
    let no = next_counter(tx, &format!("item:{prefix}"))?;
    let child = format!("{prefix}-{no}");
    let t = now();
    let link = Parent {
        item: Some(Some(parent.into())),
        ..Parent::default()
    };
    // Splitting work preserves who asked for it, not the assigned Bot's or Node's owner.
    // Membership is resolved when the child's Node session is bound, just as for its parent.
    let requested_by = crate::item_repo::requested_by(w, parent)?;
    w.exec(
        Query::insert()
            .into_table("items")
            .columns([
                "id",
                "workspace_id",
                "no",
                "kind",
                "title",
                "phase",
                "bot_id",
                "project_id",
                "source",
                "parent",
                "created",
                "updated",
                "requested_by",
            ])
            .values_panic([
                child.as_str().into(),
                item.workspace_id.as_str().into(),
                no.into(),
                "task".into(),
                unit.spec.title.as_str().into(),
                "active".into(),
                unit.spec.bot_id.as_str().into(),
                item.project_id.as_deref().into(),
                to_json(&("协作工作", "workflow"))?.into(),
                to_json(&link)?.into(),
                t.into(),
                t.into(),
                requested_by.into(),
            ]),
    )?;
    super::put_item_tags(tx, &child, &tags)?;
    add_goal(w, &child, 1, &unit.spec.goal, t)?;
    w.capture_created(&item.workspace_id, &child, t);
    reserve(w, parent, id, &child)?;
    super::items::mark_phase_change(tx, &item.workspace_id, &child, t, None, "active")?;
    w.emit(steve_types::WorkEvent::Item {
        ws: item.workspace_id.clone().into(),
        id: child.clone().into(),
    });
    record(
        w,
        parent,
        "claim",
        NOTE_CLAIMED,
        Some(id),
        &json!({"child":child,"attempt":unit.attempt+1}),
    )?;
    Ok(must_unit(&must_graph(tx, parent)?, id)?.clone())
}

/// The retry of `unit` on its own `child`: the child must belong to this parent and be at the start
/// of a new cycle (the core opens it with the lifecycle's write, in this transaction). Title, Bot and
/// goal follow the unit's current spec.
fn reclaim_in(w: &mut W<'_>, item: &Item, unit: &WorkUnit, child: &str) -> Result<WorkUnit> {
    let tx = w.conn;
    let parent = item.id.as_str();
    let row: Option<(String, String, Option<String>)> = tx
        .query_row(
            "SELECT workspace_id,phase,json_extract(parent,'$.item') FROM items WHERE id=?",
            [child],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((ws, phase, owner)) = row else {
        return Err(conflict("工作单元的执行事项已不存在"));
    };
    if ws != item.workspace_id || owner.as_deref() != Some(parent) {
        return Err(conflict("工作单元的执行事项不属于这个主事项"));
    }
    if phase != "ready" {
        return Err(conflict(
            "上一次尝试的执行事项还没有开始新周期，不能再次调度",
        ));
    }
    let latest = crate::item_repo::latest_goal(w, child)?;
    let t = now();
    if latest
        .as_ref()
        .is_none_or(|(_, text)| *text != unit.spec.goal)
    {
        add_goal(
            w,
            child,
            latest.map_or(1, |(v, _)| v + 1),
            &unit.spec.goal,
            t,
        )?;
    }
    w.exec(
        Query::update()
            .table("items")
            .value("title", unit.spec.title.as_str())
            .value("bot_id", unit.spec.bot_id.as_str())
            .value("updated", t)
            .and_where(Expr::col("id").eq(child)),
    )?;
    reserve(w, parent, &unit.spec.id, child)?;
    w.emit(steve_types::WorkEvent::Item {
        ws: item.workspace_id.clone().into(),
        id: child.into(),
    });
    record(
        w,
        parent,
        "claim",
        NOTE_CLAIMED,
        Some(&unit.spec.id),
        &json!({"child":child,"attempt":unit.attempt+1,"reused":true}),
    )?;
    Ok(must_unit(&must_graph(tx, parent)?, &unit.spec.id)?.clone())
}

fn add_goal(w: &mut W<'_>, item: &str, version: i64, goal: &str, now: i64) -> Result<()> {
    w.exec(
        Query::insert()
            .into_table("item_goals")
            .columns(["item_id", "version", "text", "created"])
            .values_panic([item.into(), version.into(), goal.into(), now.into()]),
    )?;
    Ok(())
}

fn reserve(w: &mut W<'_>, parent: &str, unit: &str, child: &str) -> Result<()> {
    w.exec(
        Query::update()
            .table("workflow_units")
            .value("status", "dispatching")
            .value("item_id", child)
            .value("session_id", None::<String>)
            .value("attempt", Expr::col("attempt").add(1_i64))
            .value("reason", None::<String>)
            .and_where(Expr::col("workflow_id").eq(parent))
            .and_where(Expr::col("id").eq(unit)),
    )?;
    Ok(())
}

/// The latest retired attempt of `child`: an authoritative failure in the append-only history (the
/// same child and session) stands over a snapshot archived as cancelled (rows archived before failed
/// snapshots were kept).
fn retired(c: &Connection, child: &str) -> Result<Option<(String, WorkUnit)>> {
    one(
        c,
        "SELECT a.workflow_id,a.snapshot,
                (SELECT h.reason FROM workflow_history h
                 WHERE h.workflow_id=a.workflow_id AND h.action='deliver'
                   AND json_extract(h.detail,'$.child')=a.item_id
                   AND json_extract(h.detail,'$.session') IS json_extract(a.snapshot,'$.session_id')
                   AND json_extract(h.detail,'$.status')='failed'
                 ORDER BY h.revision DESC LIMIT 1) AS failed
         FROM workflow_attempts a WHERE a.item_id=? ORDER BY a.attempt DESC LIMIT 1",
        [child],
        |r| {
            let mut u: WorkUnit = decode(r, "snapshot")?;
            if let Some(why) = r.get::<_, Option<String>>("failed")?
                && u.status != "failed"
            {
                u.status = "failed".into();
                u.reason = Some(why);
            }
            Ok((r.get("workflow_id")?, u))
        },
    )
}

/// Records a unit's evidence inside the caller's write transaction. The core uses this with the
/// child's closing intent so a committed success can never be left without its lifecycle checkpoint.
/// Scope equality can be proven for immutable Hub deliveries. Names or mutable paths alone
/// never qualify as evidence that two outputs are equivalent.
fn verified_output(
    tx: &Connection,
    spec: &UnitSpec,
    child: &str,
    session: &str,
    outputs: &Value,
) -> Result<Option<String>> {
    let outputs = outputs
        .as_array()
        .into_iter()
        .flatten()
        .filter(|out| out.get("role").and_then(Value::as_str) != Some("record"));
    let mut measured = Vec::new();
    for output in outputs {
        let Some(url) = output.get("location").and_then(Value::as_str) else {
            return Ok(None);
        };
        let artifact = tx
            .query_row(
                "SELECT d.sha256,d.mime,d.size FROM deliveries d JOIN items i ON i.id=d.item_id
             WHERE d.url=? AND d.item_id=? AND d.session_id=? AND i.project_id IS NULL",
                params![url, child, session],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, String>(1)?,
                        r.get::<_, i64>(2)?,
                    ))
                },
            )
            .optional()?;
        let Some((digest, mime, size)) = artifact else {
            return Ok(None);
        };
        measured.push((
            output.get("title").and_then(Value::as_str),
            digest,
            mime,
            size,
        ));
    }
    if measured.is_empty() {
        return Ok(None);
    }
    measured.sort_unstable();
    Ok(Some(
        steve_node_api::digest_of(&(spec, measured))
            .map_err(|e| conflict(e.to_string()))?
            .to_string(),
    ))
}

fn reopen_unit(
    w: &mut W<'_>,
    graph: &Workflow,
    unit: &WorkUnit,
    reason: ReworkReason,
) -> Result<()> {
    if unit.active() {
        return Err(conflict("工作单元仍在执行"));
    }
    archive_cycle_unit(w.conn, &graph.item_id, unit)?;
    w.conn.execute(
        "UPDATE workflow_units SET status='pending',session_id=NULL,reason=?
         WHERE workflow_id=? AND id=?",
        params![reason.as_str(), graph.item_id, unit.spec.id],
    )?;
    Ok(())
}

/// On settlement of a new upstream attempt, invalidate only immediate dependents whose basis
/// changed. Their own successors wait for the replacement output and the same comparison.
fn propagate_output(
    w: &mut W<'_>,
    graph: &Workflow,
    unit: &WorkUnit,
    output: Option<&str>,
) -> Result<()> {
    let why = match (unit.verified_output.as_deref(), output) {
        (Some(old), Some(new)) if old == new => return Ok(()),
        (Some(_), Some(_)) => ReworkReason::DependencyOutputChanged,
        (_, _) => ReworkReason::EvidenceUnverifiable,
    };
    for downstream in &graph.units {
        if downstream.status == "succeeded" && downstream.spec.depends_on.contains(&unit.spec.id) {
            reopen_unit(w, graph, downstream, why)?;
        }
    }
    Ok(())
}

pub fn deliver_in(
    w: &mut W<'_>,
    child: &str,
    session: &str,
    summary: &str,
    outputs: &Value,
    checks: &Value,
    passed: bool,
) -> Result<Workflow> {
    let tx = w.conn;
    let (parent, unit) = one(
        tx,
        "SELECT * FROM workflow_units WHERE item_id=?",
        [child],
        |r| Ok((r.get::<_, String>("workflow_id")?, unit_row(r)?)),
    )?
    .ok_or(StoreError::NotFound("没有这个工作尝试"))?;
    open_parent(tx, &parent)?;
    if unit.session_id.as_deref() != Some(session) {
        return Err(conflict("只有当前工作会话可以交付"));
    }
    if !matches!(unit.status.as_str(), "running" | "succeeded" | "failed") {
        return Err(conflict("当前工作尝试已失效或未开始"));
    }
    let status = if passed { "succeeded" } else { "failed" };
    if unit.status != "running" {
        if unit.status == status
            && unit.summary == summary
            && unit.outputs == *outputs
            && unit.checks == *checks
        {
            return must_graph(tx, &parent);
        }
        return Err(conflict("工作尝试已经交付，修改成果需要重置"));
    }
    if summary.trim().is_empty() || !outputs.is_array() || !checks.is_array() {
        return Err(conflict("工作交付需要摘要、交付物数组和检查数组"));
    }
    fn scope_of(c: &Value) -> &str {
        c.get("scope").and_then(Value::as_str).unwrap_or("unit")
    }
    if checks.as_array().is_some_and(|cs| {
        cs.iter()
            .any(|c| !matches!(scope_of(c), "unit" | "baseline" | "external"))
    }) {
        return Err(conflict("检查范围 scope 只能是 unit、baseline 或 external"));
    }
    if passed
        && (checks.as_array().is_none_or(|cs| {
            !cs.iter()
                .any(|c| scope_of(c) == "unit" && c.get("passed") == Some(&Value::Bool(true)))
        }) || checks.as_array().is_some_and(|cs| {
            cs.iter()
                .any(|c| scope_of(c) == "unit" && c.get("passed") != Some(&Value::Bool(true)))
        }))
    {
        return Err(conflict(
            "成功交付必须包含至少一项通过的单元检查，且本单元范围内的检查必须全部通过",
        ));
    }
    if passed {
        let graph = must_graph(tx, &parent)?;
        let inputs = graph.reviewed_units(&unit);
        if inputs.is_none() || inputs != unit.inputs {
            return Err(conflict("work unit inputs changed during execution"));
        }
    }
    let fingerprint = if passed {
        verified_output(tx, &unit.spec, child, session, outputs)?
    } else {
        None
    };
    if passed && unit.attempt > 1 {
        let graph = must_graph(tx, &parent)?;
        propagate_output(w, &graph, &unit, fingerprint.as_deref())?;
    }
    w.exec(
        Query::update()
            .table("workflow_units")
            .value("status", status)
            .value("summary", summary)
            .value("outputs", to_json(outputs)?)
            .value("checks", to_json(checks)?)
            .value("verified_output", fingerprint)
            .value("reason", (!passed).then_some(summary))
            .and_where(Expr::col("workflow_id").eq(parent.as_str()))
            .and_where(Expr::col("id").eq(unit.spec.id.as_str())),
    )?;
    record(
        w,
        &parent,
        "deliver",
        summary,
        Some(&unit.spec.id),
        &json!({"child":child,"session":session,"status":status,"outputs":outputs,"checks":checks}),
    )?;
    must_graph(tx, &parent)
}

impl Workflows<'_> {
    /// A current turn is confirmed only by applied activity from the exact active Node attempt.
    /// The Hub's receive time, not the Node's clock, sets freshness. This is a read-only display
    /// fact; uncertain or stale work remains active but unconfirmed in the UI.
    pub fn confirmed_activity(
        &self,
        child: &str,
        session: &str,
        at: i64,
    ) -> Result<Option<WorkflowActivity>> {
        self.0.read_conn(|c| crate::read_snapshot(c, |c| {
            let active: Option<(String, String, String)> = c.query_row(
                "SELECT a.attempt_id,t.turn_id,s.node_id FROM runtime_sessions s
                 JOIN sessions biz ON biz.id=s.session_id AND biz.item_id=?1
                 JOIN runtime_turns t ON t.session_id=s.session_id AND t.active_attempt=s.active_attempt AND t.state='running'
                 JOIN runtime_attempts a ON a.attempt_id=s.active_attempt AND a.turn_id=t.turn_id AND a.node_id=s.node_id
                 JOIN nodes n ON n.node_id=s.node_id AND n.state='active'
                 WHERE s.session_id=?2 AND s.phase='active' AND s.release_pending=0
                   AND a.state='running' AND a.cancel_requested=0",
                params![child, session],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            ).optional()?;
            let Some((attempt, turn, node)) = active else { return Ok(None) };
            let observed_at: Option<i64> = c.query_row(
                "SELECT a.active_at FROM runtime_attempts a WHERE a.attempt_id=?1 AND a.active_at BETWEEN ?3 AND ?4
                   AND NOT EXISTS(SELECT 1 FROM node_streams s WHERE s.node_id=?2 AND s.session_id=?5 AND s.fault IS NOT NULL)",
                params![attempt, node, at.saturating_sub(600_000), at, session],
                |r| r.get(0),
            ).optional()?.flatten();
            let Some(observed_at) = observed_at else { return Ok(None) };
            Ok((std::cmp::max(at.saturating_sub(observed_at), 0) < 600_000).then(|| WorkflowActivity {
                    state: "confirmed", observed_at, node_id: node, session_id: session.into(),
                    turn_id: turn, attempt_id: attempt,
                }))
        }))
    }

    pub fn archived_attempts_recent(
        &self,
        parent: &str,
        limit: i64,
    ) -> Result<(Vec<ArchivedWorkflowAttempt>, i64)> {
        self.0.read_conn(|c| crate::read_snapshot(c, |c| {
            let total: i64 = c.query_row("SELECT COUNT(*) FROM workflow_attempts WHERE workflow_id=?", [parent], |r| r.get(0))?;
            let rows = all(c,
                "SELECT item_id,unit_id,snapshot,created FROM workflow_attempts WHERE workflow_id=? ORDER BY created DESC,item_id DESC,attempt DESC LIMIT ?",
                params![parent, limit],
                |r| Ok(ArchivedWorkflowAttempt {
                    item_id: r.get("item_id")?,
                    unit_id: r.get("unit_id")?,
                    snapshot: decode(r, "snapshot")?,
                    created: r.get("created")?,
                }),
            )?;
            Ok((rows, total))
        }))
    }

    pub fn get(&self, parent: &str) -> Result<Option<Workflow>> {
        self.0.read_conn(|c| {
            // Header, units and history are one read snapshot even if another Hub process commits a
            // new plan between these SELECTs. Free get() remains usable inside an existing write.
            crate::read_snapshot(c, |tx| get(tx, parent))
        })
    }

    /// The unit `child` belongs to: the graph's current one (a retried unit keeps its child, so this
    /// is also the unit between two of its attempts), else the child's latest retired attempt.
    pub fn by_child(&self, child: &str) -> Result<Option<(String, WorkUnit)>> {
        self.0.read_conn(|c| {
            let current = one(
                c,
                "SELECT * FROM workflow_units WHERE item_id=?",
                [child],
                |r| Ok((r.get("workflow_id")?, unit_row(r)?)),
            )?;
            if current.is_some() {
                return Ok(current);
            }
            retired(c, child)
        })
    }

    /// The latest retired attempt of `child`, whether or not its unit still holds the child.
    pub fn retired_of(&self, child: &str) -> Result<Option<(String, WorkUnit)>> {
        self.0.read_conn(|c| retired(c, child))
    }

    /// Retired attempts whose child item was left active, once per child (its latest attempt). A child
    /// its unit still holds is the graph's to settle (a retry reuses it). A failed delivery is identified
    /// from the append-only history (older rows archived a failed attempt's snapshot as cancelled).
    pub fn retired_open(&self) -> Result<Vec<(WorkUnit, bool)>> {
        self.0.read_conn(|c| {
            all(
                c,
                "SELECT a.snapshot,
                        EXISTS(SELECT 1 FROM workflow_history h
                               WHERE h.workflow_id=a.workflow_id AND h.action='deliver'
                                 AND json_extract(h.detail,'$.child')=a.item_id
                                 AND json_extract(h.detail,'$.session') IS json_extract(a.snapshot,'$.session_id')
                                 AND json_extract(h.detail,'$.status')='failed') AS failed
                 FROM workflow_attempts a JOIN items i ON i.id=a.item_id
                 WHERE i.phase='active'
                   AND a.attempt=(SELECT max(b.attempt) FROM workflow_attempts b WHERE b.item_id=a.item_id)
                   AND NOT EXISTS(SELECT 1 FROM workflow_units u WHERE u.item_id=a.item_id)
                 ORDER BY a.created,a.item_id",
                [],
                |r| Ok((decode(r, "snapshot")?, r.get::<_, i64>("failed")? != 0)),
            )
        })
    }

    pub fn active(&self) -> Result<Vec<Workflow>> {
        self.list_open(true)
    }

    /// Includes paused and closing parents for replaying durable stop intents after a restart.
    /// Archived-but-open parents remain visible here so their old executions can still be stopped.
    pub fn all_open(&self) -> Result<Vec<Workflow>> {
        self.list_open(false)
    }

    fn list_open(&self, active_only: bool) -> Result<Vec<Workflow>> {
        self.0.read_conn(|c| {
            let ids: Vec<String> = all(
                c,
                "SELECT w.item_id FROM workflows w JOIN items i ON i.id=w.item_id
                 WHERE i.phase<>'closed' AND
                 (?=0 OR (w.paused=0 AND i.phase IN ('ready','active') AND i.archived=0))
                 ORDER BY w.created",
                [active_only],
                |r| r.get(0),
            )?;
            ids.iter().map(|id| must_graph(c, id)).collect()
        })
    }

    /// expected_revision=0 creates a graph. Later writes compare the observed revision, including
    /// scheduling progress; stale replans cannot erase a unit that started or delivered meanwhile.
    pub fn replace(
        &self,
        parent: &str,
        expected_revision: i64,
        reason: &str,
        specs: &[UnitSpec],
        max_parallel: usize,
        max_attempts: i64,
    ) -> Result<Workflow> {
        validate(specs, max_parallel, max_attempts)?;
        if reason.trim().is_empty() {
            return Err(conflict("重规划必须说明原因"));
        }
        self.0.write(|w| {
            let tx = w.conn;
            let item = open_parent(tx, parent)?;
            let nested: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM workflow_units WHERE item_id=?)",
                [parent],
                |r| r.get(0),
            )?;
            if nested || item.source.1 == "workflow" {
                return Err(conflict("工作单元由主事项统一编排，不能自行创建嵌套工作图"));
            }
            for spec in specs {
                bot_available(tx, &item.workspace_id, &spec.bot_id)?;
            }
            let old = get(tx, parent)?;
            if old.as_ref().map_or(0, |w| w.revision) != expected_revision {
                return Err(conflict("工作图已更新，请重新读取后再规划"));
            }
            let goal_version: i64 = tx.query_row(
                "SELECT coalesce(max(version),0) FROM item_goals WHERE item_id=?",
                [parent], |r| r.get(0),
            )?;
            let context = (item.cycle, goal_version);
            let mut new_cycle = false;
            if let Some(old) = &old {
                if (old.cycle,old.goal_version) != context {
                    if !old.paused || old.units.iter().any(WorkUnit::active) {
                        return Err(conflict("旧周期仍在编排或执行；先暂停并确认工作已停止，再重新规划"));
                    }
                    new_cycle = true;
                }
                if !new_cycle && old.units.iter().filter(|u| u.active()).count() > max_parallel {
                    return Err(conflict("新的并发上限小于正在运行的工作数"));
                }
                for unit in &old.units {
                    if new_cycle { continue; }
                    let next = specs.iter().find(|s| s.id == unit.spec.id);
                    if (unit.active() || unit.status == "succeeded") && next != Some(&unit.spec) {
                        return Err(conflict(format!(
                            "{} 正在执行或已有成果，先取消或重置后再修改",
                            unit.spec.id
                        )));
                    }
                }
                tx.execute(
                    "UPDATE workflows SET max_parallel=?,max_attempts=?,cycle=?,goal_version=? WHERE item_id=?",
                    params![max_parallel as i64, max_attempts, context.0,context.1,parent],
                )?;
                if new_cycle {
                    for unit in &old.units {
                        archive_cycle_unit(tx, parent, unit)?;
                    }
                    tx.execute("DELETE FROM workflow_units WHERE workflow_id=?", [parent])?;
                }
                for unit in &old.units {
                    if new_cycle { continue; }
                    if !specs.iter().any(|s| s.id == unit.spec.id) {
                        archive_attempt(tx,parent,unit,reason)?;
                        tx.execute(
                            "DELETE FROM workflow_units WHERE workflow_id=? AND id=?",
                            params![parent, unit.spec.id],
                        )?;
                    }
                }
            } else {
                tx.execute(
                    "INSERT INTO workflows(item_id,revision,cycle,goal_version,max_parallel,max_attempts,created,updated)
                     VALUES (?,1,?,?,?,?,?,?)",
                    params![parent,context.0,context.1,max_parallel as i64,max_attempts,now(),now()],
                )?;
            }
            for (position, spec) in specs.iter().enumerate() {
                tx.execute(
                    "INSERT INTO workflow_units(workflow_id,id,position,spec,status) VALUES (?,?,?,?,'pending')
                     ON CONFLICT(workflow_id,id) DO UPDATE SET position=excluded.position,spec=excluded.spec",
                    params![parent, spec.id, position as i64, to_json(spec)?],
                )?;
            }
            if let Some(old) = &old {
                record(w, parent, "replan", reason, None, &json!({
                    "before":{"revision":old.revision,"cycle":old.cycle,"goal_version":old.goal_version,"units":old.units,"max_parallel":old.max_parallel,"max_attempts":old.max_attempts},
                    "cycle":context.0,"goal_version":context.1,"units":specs,"max_parallel":max_parallel,"max_attempts":max_attempts
                }))?;
            } else {
                tx.execute(
                    "INSERT INTO workflow_history(workflow_id,revision,action,reason,detail,created)
                     VALUES (?,1,'plan',?,?,?)",
                    params![parent, reason, to_json(&json!({"cycle":context.0,"goal_version":context.1,"units":specs,"max_parallel":max_parallel,"max_attempts":max_attempts}))?, now()],
                )?;
                if let Some(ws) = crate::repo::one(
                    tx,
                    "SELECT workspace_id FROM items WHERE id = ?",
                    [parent],
                    |r| r.get::<_, String>(0),
                )? {
                    w.emit(steve_types::DomainEvent::from(
                        steve_types::WorkEvent::Item {
                            ws: ws.into(),
                            id: parent.into(),
                        },
                    ));
                }
            }
            must_graph(tx, parent)
        })
    }

    /// Claims exactly one ready unit and creates its child item atomically ([`claim_in`]).
    pub fn claim(&self, parent: &str, id: &str) -> Result<WorkUnit> {
        self.0.write(|w| claim_in(w, parent, id))
    }

    pub fn bind_session(
        &self,
        parent: &str,
        id: &str,
        child: &str,
        session: &str,
    ) -> Result<WorkUnit> {
        self.0.write(|w| {
            let tx = w.conn;
            let item = open_parent(tx,parent)?;
            if item.phase == steve_types::ItemPhase::Closing {
                return Err(conflict("事项正在关闭，需要先恢复执行才能启动会话"));
            }
            let graph = must_graph(tx,parent)?;
            let goal_version: i64 = tx.query_row(
                "SELECT coalesce(max(version),0) FROM item_goals WHERE item_id=?",
                [parent], |r|r.get(0)
            )?;
            if graph.cycle != item.cycle || graph.goal_version != goal_version {
                return Err(conflict("主事项目标或执行周期已变化，需要重新规划后才能启动会话"));
            }
            let unit = must_unit(&graph,id)?;
            if !unit.active() || unit.item_id.as_deref()!=Some(child) {
                return Err(conflict("会话不属于当前工作尝试"));
            }
            if let Some(bound) = &unit.session_id {
                return if bound == session { Ok(unit.clone()) } else { Err(conflict("工作尝试已有会话，不能重复启动")) };
            }
            let valid: bool = tx.query_row(
                "SELECT EXISTS(SELECT 1 FROM sessions WHERE id=? AND item_id=? AND workspace_id=? AND bot_id=? AND status<>'ended')",
                params![session,child,item.workspace_id,unit.spec.bot_id],|r|r.get(0)
            )?;
            if !valid {
                return Err(conflict("会话的事项、Bot 或工作空间不匹配"));
            }
            tx.execute(
                "UPDATE workflow_units SET status='running',session_id=? WHERE workflow_id=? AND id=?",
                params![session,parent,id],
            )?;
            record(w,parent,"start",NOTE_SESSION_BOUND,Some(id),&json!({"session":session,"child":child}))?;
            Ok(must_unit(&must_graph(tx,parent)?,id)?.clone())
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn deliver(
        &self,
        child: &str,
        session: &str,
        summary: &str,
        outputs: &Value,
        checks: &Value,
        passed: bool,
    ) -> Result<Workflow> {
        self.0
            .write(|w| deliver_in(w, child, session, summary, outputs, checks, passed))
    }

    pub fn retry_unit(
        &self,
        parent: &str,
        revision: i64,
        id: &str,
        reason: &str,
    ) -> Result<Workflow> {
        self.0.write(|w| {
            open_parent(w.conn, parent)?;
            let graph = must_graph(w.conn, parent)?;
            if graph.revision != revision {
                return Err(conflict("work graph changed"));
            }
            let unit = must_unit(&graph, id)?;
            if unit.active()
                || graph
                    .units
                    .iter()
                    .any(|u| u.active() && graph.depends_on(u, id))
            {
                return Err(conflict("工作单元仍在执行"));
            }
            if unit.attempt >= graph.max_attempts {
                return Err(conflict("工作单元已达到尝试预算"));
            }
            reopen_unit(w, &graph, unit, ReworkReason::ExplicitRetry)?;
            record(
                w,
                parent,
                "retry",
                reason,
                Some(id),
                &json!({"cause":ReworkReason::ExplicitRetry}),
            )?;
            must_graph(w.conn, parent)
        })
    }

    /// Apply a failed review against a frozen scope. Replays return the prior verdict and stale
    /// findings remain archived without modifying the graph.
    pub fn apply_review_findings(
        &self,
        parent: &str,
        scope: &ReviewScope,
        findings: &[Finding],
        reason: &str,
    ) -> Result<ReviewReceipt> {
        let fingerprint =
            steve_node_api::digest_of(&(scope, findings)).map_err(|e| conflict(e.to_string()))?;
        self.0.write(|w| {
            let tx = w.conn;
            if let Some(applied) = tx.query_row(
                "SELECT applied_revision FROM workflow_review_findings WHERE workflow_id=? AND fingerprint=?",
                params![parent,fingerprint.as_str()], |r| r.get::<_, Option<i64>>(0),
            ).optional()? {
                return Ok(if applied.is_some() { ReviewReceipt::Replay } else { ReviewReceipt::Unverified });
            }
            let parent_item = open_parent(tx, parent)?;
            let graph = must_graph(tx, parent)?;
            let review = graph.units.iter().find(|u| u.spec.id == scope.review_unit);
            let current = review.and_then(|u| graph.review_scope(u)).as_ref() == Some(scope);
            let valid = current && parent_item.project_id.is_none() && !findings.is_empty() && findings.iter().all(|f| {
                scope.units.iter().any(|u| u.unit == f.affected_unit)
                    && !f.criterion.trim().is_empty() && !f.evidence_ref.trim().is_empty()
            });
            let mut invalidated = HashSet::new();
            let mut verified = valid;
            if let Some(review) = review.filter(|_| verified) {
                for finding in findings {
                    let affected = must_unit(&graph, &finding.affected_unit)?;
                    let criterion = affected.checks.as_array().into_iter().flatten()
                        .any(|check| check.get("name").and_then(Value::as_str) == Some(finding.criterion.as_str()));
                    let receipt = tx.query_row(
                        "SELECT EXISTS(SELECT 1 FROM deliveries WHERE url=? AND item_id=? AND session_id=?)",
                        params![finding.evidence_ref,review.item_id,review.session_id],
                        |row| row.get::<_, bool>(0),
                    )?;
                    if !criterion || !receipt {
                        verified = false;
                        break;
                    }
                    invalidated.insert(affected.spec.id.as_str());
                }
            }
            if let Some(review) = review.filter(|_| verified) {
                invalidated.insert(review.spec.id.as_str());
                if graph.units.iter().any(|u| invalidated.contains(u.spec.id.as_str())
                    && (u.active() || u.attempt >= graph.max_attempts))
                {
                    return Err(conflict("review scope is still running or has exhausted its budget"));
                }
                for u in &graph.units {
                    if invalidated.contains(u.spec.id.as_str()) {
                        reopen_unit(w, &graph, u, ReworkReason::ReviewFinding)?;
                    }
                }
                record(w, parent, "apply_review_findings", reason, Some(&scope.review_unit),
                       &json!({"cause":ReworkReason::ReviewFinding,"fingerprint":fingerprint.as_str()}))?;
            }
            w.conn.execute(
                "INSERT INTO workflow_review_findings(workflow_id,fingerprint,scope,findings,applied_revision)
                 VALUES (?,?,?,?,?)",
                params![parent,fingerprint.as_str(),to_json(scope)?,to_json(&findings)?,verified.then_some(graph.revision+1)],
            )?;
            Ok(if verified { ReviewReceipt::Applied } else { ReviewReceipt::Unverified })
        })
    }

    /// Core propagates cancellation to each returned active session. Cancelled required units never
    /// satisfy acceptance and their results are rejected even if the old execution reports late.
    pub fn cancel(&self, parent: &str, id: &str, reason: &str) -> Result<Workflow> {
        if reason.trim().is_empty() {
            return Err(conflict("取消必须说明原因"));
        }
        self.0.write(|w| {
            let tx = w.conn;
            open_parent(tx,parent)?;
            let graph=must_graph(tx,parent)?;
            let unit=must_unit(&graph,id)?;
            if unit.status=="cancelled" { return Ok(graph); }
            if unit.status=="succeeded" {
                return Err(conflict("已成功的工作需要重置，不能直接取消"));
            }
            tx.execute(
                "UPDATE workflow_units SET status='cancelled',reason=? WHERE workflow_id=? AND id=?",
                params![reason,parent,id],
            )?;
            record(w,parent,"cancel",reason,Some(id),&serde_json::to_value(unit)?)?;
            must_graph(tx,parent)
        })
    }

    /// Called only after a human acceptance-mode change. Keep units and evidence for audit;
    /// cancellation is not success. Core additionally checks live/uncertain Node executions.
    pub fn accept_single_session(
        &self,
        parent: &str,
        expected_revision: i64,
        reason: &str,
        actor: &str,
    ) -> Result<Workflow> {
        if reason.trim().is_empty() || actor.trim().is_empty() {
            return Err(conflict("切换单会话验收必须记录用户和原因"));
        }
        self.0.write(|w| {
            let tx = w.conn;
            let item = open_parent(tx, parent)?;
            let graph = must_graph(tx, parent)?;
            if graph.revision != expected_revision {
                return Err(conflict("工作图已更新，请重新读取后再确认验收方式"));
            }
            let goal: i64 = tx.query_row(
                "SELECT coalesce(max(version),0) FROM item_goals WHERE item_id=?",
                [parent],
                |r| r.get(0),
            )?;
            if graph.cycle != item.cycle || graph.goal_version != goal {
                return Err(conflict("主事项目标或周期已变化，请先重新规划"));
            }
            if graph.single_session() {
                return Ok(graph);
            }
            if !graph.paused {
                return Err(conflict("请先暂停协作并确认所有工作执行已停止"));
            }
            tx.execute(
                "UPDATE workflow_units SET status='cancelled',reason=?
                 WHERE workflow_id=? AND status NOT IN ('succeeded','cancelled')",
                params![reason, parent],
            )?;
            record(
                w,
                parent,
                "single_session",
                reason,
                None,
                &json!({
                    "actor": actor, "cycle": graph.cycle, "goal_version": graph.goal_version,
                    "before": graph.units, "cancelled_units_are_passed": false
                }),
            )?;
            must_graph(tx, parent)
        })
    }

    /// [`Self::pause_by`] you.
    pub fn pause(&self, parent: &str, paused: bool, reason: &str) -> Result<Workflow> {
        self.pause_by(parent, paused, reason, PAUSED_BY_USER)
    }

    /// `by`: who pauses or resumes it (see [`Workflow::paused_by`]).
    pub fn pause_by(&self, parent: &str, paused: bool, reason: &str, by: &str) -> Result<Workflow> {
        if reason.trim().is_empty() {
            return Err(conflict("暂停或恢复必须说明原因"));
        }
        self.0.write(|w| {
            let tx = w.conn;
            open_parent(tx, parent)?;
            let graph = must_graph(tx, parent)?;
            if !paused && graph.single_session() {
                return Err(conflict("当前为单会话验收，恢复协作前必须重新规划"));
            }
            if graph.paused == paused {
                return Ok(graph);
            }
            tx.execute(
                "UPDATE workflows SET paused=? WHERE item_id=?",
                params![paused, parent],
            )?;
            record(
                w,
                parent,
                if paused { "pause" } else { "resume" },
                reason,
                None,
                &json!({ "by": by }),
            )?;
            must_graph(tx, parent)
        })
    }
}
