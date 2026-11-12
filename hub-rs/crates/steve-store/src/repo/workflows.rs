//! Persistent item work graphs. Scheduling decisions and the child item they reserve commit in
//! one transaction; no process or credential is created here. The core starts the child's ordinary
//! Node session after the reservation, and recovery resumes that same child.

use std::collections::{HashMap, HashSet};

use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::{all, next_counter, one, to_json};
use crate::{ChangeEvent, Item, Parent, Result, Store, StoreError, Tx, now};

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

impl Workflow {
    /// The existing append-only history is the authority for an explicit acceptance-mode change.
    /// Replanning reinstates collaboration; ordinary pause/cancel never waive required evidence.
    pub fn single_session(&self) -> bool {
        self.history
            .iter()
            .rev()
            .find(|h| matches!(h.action.as_str(), "single_session" | "plan" | "replan"))
            .is_some_and(|h| h.action == "single_session")
    }

    /// Optional work may be waived by cancelling it; required units need successful evidence unless
    /// the user explicitly chose single-session acceptance. Active work always prevents closing.
    pub fn blockers(&self) -> Vec<String> {
        let mut out: Vec<_> = self
            .units
            .iter()
            .filter(|u| {
                (!self.single_session() && u.spec.required && u.status != "succeeded") || u.active()
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
    if item.phase == "closed" || item.archived {
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
    tx: &Tx<'_>,
    parent: &str,
    action: &str,
    reason: &str,
    unit: Option<&str>,
    detail: &Value,
) -> Result<()> {
    let revision: i64 = tx.query_row(
        "UPDATE workflows SET revision=revision+1,updated=? WHERE item_id=? RETURNING revision",
        params![now(), parent],
        |r| r.get(0),
    )?;
    tx.execute(
        "INSERT INTO workflow_history(workflow_id,revision,action,reason,unit_id,detail,created)
         VALUES (?,?,?,?,?,?,?)",
        params![
            parent,
            revision,
            action,
            reason,
            unit,
            to_json(detail)?,
            now()
        ],
    )?;
    tx.publish(ChangeEvent::Item { id: parent.into() });
    Ok(())
}

fn archive_attempt(tx: &Tx<'_>, parent: &str, unit: &WorkUnit, reason: &str) -> Result<()> {
    if let Some(child) = &unit.item_id {
        let mut old = unit.clone();
        old.status = "cancelled".into();
        old.reason = Some(format!("尝试已失效：{reason}"));
        tx.execute(
            "INSERT OR IGNORE INTO workflow_attempts(item_id,workflow_id,unit_id,snapshot,created)
             VALUES (?,?,?,?,?)",
            params![child, parent, unit.spec.id, to_json(&old)?, now()],
        )?;
    }
    Ok(())
}

/// Records a unit's evidence inside the caller's write transaction. The core uses this with the
/// child's closing intent so a committed success can never be left without its lifecycle checkpoint.
pub fn deliver_in(
    tx: &Tx<'_>,
    child: &str,
    session: &str,
    summary: &str,
    outputs: &Value,
    checks: &Value,
    passed: bool,
) -> Result<Workflow> {
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
    if passed
        && (checks.as_array().is_none_or(Vec::is_empty)
            || checks.as_array().is_some_and(|cs| {
                cs.iter()
                    .any(|c| c.get("passed") != Some(&Value::Bool(true)))
            }))
    {
        return Err(conflict("成功交付必须包含全部通过的检查"));
    }
    tx.execute(
        "UPDATE workflow_units SET status=?,summary=?,outputs=?,checks=?,reason=? WHERE workflow_id=? AND id=?",
        params![status,summary,to_json(outputs)?,to_json(checks)?,
            if passed {None} else {Some(summary)},parent,unit.spec.id],
    )?;
    record(
        tx,
        &parent,
        "deliver",
        summary,
        Some(&unit.spec.id),
        &json!({"child":child,"session":session,"status":status,"outputs":outputs,"checks":checks}),
    )?;
    must_graph(tx, &parent)
}

impl Workflows<'_> {
    pub fn get(&self, parent: &str) -> Result<Option<Workflow>> {
        self.0.read(|c| {
            // Header, units and history are one read snapshot even if another Hub process commits a
            // new plan between these SELECTs. Free get() remains usable inside an existing Tx.
            crate::read_snapshot(c, |tx| get(tx, parent))
        })
    }

    pub fn by_child(&self, child: &str) -> Result<Option<(String, WorkUnit)>> {
        self.0.read(|c| {
            let current = one(
                c,
                "SELECT * FROM workflow_units WHERE item_id=?",
                [child],
                |r| Ok((r.get("workflow_id")?, unit_row(r)?)),
            )?;
            if current.is_some() {
                return Ok(current);
            }
            one(
                c,
                "SELECT workflow_id,snapshot FROM workflow_attempts WHERE item_id=?",
                [child],
                |r| Ok((r.get("workflow_id")?, decode(r, "snapshot")?)),
            )
        })
    }

    /// Retired attempts whose child item was left active. A failed delivery remains identifiable
    /// in the append-only history even though archiving changes the snapshot status to cancelled.
    pub fn retired_open(&self) -> Result<Vec<(WorkUnit, bool)>> {
        self.0.read(|c| {
            all(
                c,
                "SELECT a.snapshot,
                        EXISTS(SELECT 1 FROM workflow_history h
                               WHERE h.workflow_id=a.workflow_id AND h.action='deliver'
                                 AND json_extract(h.detail,'$.child')=a.item_id
                                 AND json_extract(h.detail,'$.status')='failed') AS failed
                 FROM workflow_attempts a JOIN items i ON i.id=a.item_id
                 WHERE i.phase='active' ORDER BY a.created,a.item_id",
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
        self.0.read(|c| {
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
        self.0.transaction(|tx| {
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
            let mut context = (item.cycle, goal_version);
            if let Some(old) = &old {
                if (old.cycle,old.goal_version) != context
                    && old.units.iter().any(|u|u.active() || u.status=="succeeded")
                {
                    let only_raise_budget = max_attempts > old.max_attempts
                        && max_parallel == old.max_parallel
                        && old.units.len() == specs.len()
                        && old.units.iter().zip(specs).all(|(u,s)|u.spec==*s);
                    if !only_raise_budget {
                        return Err(conflict("主事项目标或执行周期已变化，请先取消运行并重置旧成果，再重新规划"));
                    }
                    // A budget-only increase can unblock resets of exhausted old attempts. It must
                    // not silently make those old results count for the new goal or item cycle.
                    context = (old.cycle,old.goal_version);
                }
                if old.units.iter().filter(|u| u.active()).count() > max_parallel {
                    return Err(conflict("新的并发上限小于正在运行的工作数"));
                }
                for unit in &old.units {
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
                for unit in &old.units {
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
                record(tx, parent, "replan", reason, None, &json!({
                    "before":{"revision":old.revision,"cycle":old.cycle,"goal_version":old.goal_version,"units":old.units,"max_parallel":old.max_parallel,"max_attempts":old.max_attempts},
                    "cycle":context.0,"goal_version":context.1,"units":specs,"max_parallel":max_parallel,"max_attempts":max_attempts
                }))?;
            } else {
                tx.execute(
                    "INSERT INTO workflow_history(workflow_id,revision,action,reason,detail,created)
                     VALUES (?,1,'plan',?,?,?)",
                    params![parent, reason, to_json(&json!({"cycle":context.0,"goal_version":context.1,"units":specs,"max_parallel":max_parallel,"max_attempts":max_attempts}))?, now()],
                )?;
                tx.publish(ChangeEvent::Item { id: parent.into() });
            }
            must_graph(tx, parent)
        })
    }

    /// Claims exactly one ready unit and creates its child item atomically. Retrying an already
    /// claimed unit returns the same child; a process restart never creates a second attempt.
    pub fn claim(&self, parent: &str, id: &str) -> Result<WorkUnit> {
        self.0.transaction(|tx| {
            let item = open_parent(tx, parent)?;
            if item.phase == "closing" {
                return Err(conflict("事项正在关闭，需要先恢复执行才能调度工作"));
            }
            let graph = must_graph(tx, parent)?;
            let goal_version: i64 = tx.query_row(
                "SELECT coalesce(max(version),0) FROM item_goals WHERE item_id=?",
                [parent], |r|r.get(0)
            )?;
            if graph.cycle != item.cycle || graph.goal_version != goal_version {
                return Err(conflict("主事项目标或执行周期已变化，需要重新规划后才能调度"));
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
                if must_unit(&graph, dep)?.status != "succeeded" {
                    return Err(conflict(format!("前置工作 {dep} 尚未成功")));
                }
            }
            if graph.units.iter().filter(|u| u.active()).any(|u| {
                unit.spec.resources.iter().any(|r| u.spec.resources.contains(r))
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
                    return Err(conflict("协作项目需要启用独立 Git worktree，才能隔离工作目录"));
                }
            }
            let prefix: String = tx.query_row(
                "SELECT item_prefix FROM workspaces WHERE id=?",
                [&item.workspace_id],
                |r| r.get(0),
            )?;
            let no = next_counter(tx, &format!("item:{prefix}"))?;
            let child = format!("{prefix}-{no}");
            let t = now();
            let link = Parent { item: Some(Some(parent.into())), ..Parent::default() };
            // Splitting work preserves who asked for it, not the assigned Bot's or Node's owner.
            // Membership is resolved when the child's Node session is bound, just as for its parent.
            tx.execute(
                "INSERT INTO items(id,workspace_id,no,kind,title,status,stage,bot_id,project_id,source,parent,created,updated,requested_by)
                 VALUES (?,?,?,'task',?,'run','执行',?,?,?,?,?,?,(SELECT requested_by FROM items WHERE id=?))",
                params![child,item.workspace_id,no,unit.spec.title,unit.spec.bot_id,item.project_id,
                    to_json(&("协作工作", "workflow"))?,to_json(&link)?,t,t,parent],
            )?;
            tx.execute(
                "INSERT INTO item_tags(item_id,tag_id) SELECT ?,tag_id FROM item_tags WHERE item_id=?",
                params![child,parent],
            )?;
            tx.execute(
                "INSERT INTO item_goals(item_id,version,text,created) VALUES (?,1,?,?)",
                params![child,unit.spec.goal,t],
            )?;
            tx.capture_created(&item.workspace_id, &child, t);
            tx.execute(
                "UPDATE workflow_units SET status='dispatching',item_id=?,session_id=NULL,
                 attempt=attempt+1,reason=NULL WHERE workflow_id=? AND id=?",
                params![child,parent,id],
            )?;
            super::stats::mark_in(tx, &crate::NewMark {
                workspace_id:item.workspace_id, item_id:child.clone(),ts:t,kind:"status".into(),
                to_v:Some("run".into()),..Default::default()
            })?;
            tx.publish(ChangeEvent::Item { id: child.clone() });
            record(tx,parent,"claim","调度就绪工作",Some(id),&json!({"child":child,"attempt":unit.attempt+1}))?;
            Ok(must_unit(&must_graph(tx,parent)?,id)?.clone())
        })
    }

    pub fn bind_session(
        &self,
        parent: &str,
        id: &str,
        child: &str,
        session: &str,
    ) -> Result<WorkUnit> {
        self.0.transaction(|tx| {
            let item = open_parent(tx,parent)?;
            if item.phase == "closing" {
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
            record(tx,parent,"start","工作会话已绑定",Some(id),&json!({"session":session,"child":child}))?;
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
            .transaction(|tx| deliver_in(tx, child, session, summary, outputs, checks, passed))
    }

    /// Resetting a result invalidates every descendant's evidence. Active work must be cancelled
    /// first so old Node executions cannot race a new plan; attempt budgets and history survive.
    pub fn reset(&self, parent: &str, id: &str, reason: &str) -> Result<Workflow> {
        if reason.trim().is_empty() {
            return Err(conflict("重置必须说明原因"));
        }
        self.0.transaction(|tx| {
            open_parent(tx,parent)?;
            let graph=must_graph(tx,parent)?;
            must_unit(&graph,id)?;
            let mut affected=HashSet::from([id.to_string()]);
            loop {
                let old=affected.len();
                for u in &graph.units {
                    if u.spec.depends_on.iter().any(|d|affected.contains(d)) {
                        affected.insert(u.spec.id.clone());
                    }
                }
                if affected.len()==old { break; }
            }
            let before:Vec<_>=graph.units.iter().filter(|u|affected.contains(&u.spec.id)).collect();
            if before.iter().any(|u|u.active()) {
                return Err(conflict("当前工作或下游仍在运行，请先取消并停止其会话"));
            }
            if before.iter().any(|u|u.attempt >= graph.max_attempts) {
                return Err(conflict("当前工作或下游已达到尝试预算，请先提高预算再重置"));
            }
            for u in &before {
                archive_attempt(tx,parent,u,reason)?;
                tx.execute(
                    "UPDATE workflow_units SET status='pending',item_id=NULL,session_id=NULL,summary='',
                     outputs='[]',checks='[]',reason=? WHERE workflow_id=? AND id=?",
                    params![reason,parent,u.spec.id],
                )?;
            }
            record(tx,parent,"reset",reason,Some(id),&json!({"invalidated":before}))?;
            must_graph(tx,parent)
        })
    }

    /// Core propagates cancellation to each returned active session. Cancelled required units never
    /// satisfy acceptance and their results are rejected even if the old execution reports late.
    pub fn cancel(&self, parent: &str, id: &str, reason: &str) -> Result<Workflow> {
        if reason.trim().is_empty() {
            return Err(conflict("取消必须说明原因"));
        }
        self.0.transaction(|tx| {
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
            record(tx,parent,"cancel",reason,Some(id),&serde_json::to_value(unit)?)?;
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
        self.0.transaction(|tx| {
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
                tx,
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

    pub fn pause(&self, parent: &str, paused: bool, reason: &str) -> Result<Workflow> {
        if reason.trim().is_empty() {
            return Err(conflict("暂停或恢复必须说明原因"));
        }
        self.0.transaction(|tx| {
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
                tx,
                parent,
                if paused { "pause" } else { "resume" },
                reason,
                None,
                &json!({}),
            )?;
            must_graph(tx, parent)
        })
    }
}
