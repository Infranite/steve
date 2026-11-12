//! What an item's work is doing now (ST-10): one judgement, from durable state, for the turn end, the
//! stall watch, Steve's continues and the diagnostics. A coordinator that ends its turn while its
//! work graph runs, queues, waits for you or is paused is waiting on purpose; a required unit that
//! failed is a real failure it has to act on. Never judged from the session's last words or its last
//! system line (an old release notice says nothing about why it is idle now).

use steve_store::{WorkUnit, Workflow};

use crate::cycle::CLOSED;
use crate::repo::Item;
use crate::util::js_head;
use crate::{Result, Tx};

/// How many unit titles a line names.
const NAMES: usize = 3;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Work {
    /// A turn runs, or its end is not confirmed yet: the item's own or a work unit's.
    Running(String),
    /// Input or work waits its turn: queued messages, units waiting for a slot, a start or a resume.
    /// (A request in the merge lane is not: it silences Steve's continues, not your own stop.)
    Queued(String),
    /// An open card of the item or of one of its units waits for you.
    User(String),
    /// Stopped on purpose: the item or its work graph is paused.
    Paused(String),
    /// Waits on registered prerequisites.
    Blocked(String),
    /// The work graph needs its coordinator: a required unit failed or was cancelled, it is out of
    /// attempts, the goal moved under it, or the graph cannot run here.
    Failed(String),
    /// Nothing goes on and nothing is owed: the agent stopped.
    Idle,
}

impl Work {
    /// For diagnostics.
    pub(crate) fn code(&self) -> &'static str {
        match self {
            Work::Running(_) => "running",
            Work::Queued(_) => "queued",
            Work::User(_) => "waiting_user",
            Work::Paused(_) => "paused",
            Work::Blocked(_) => "dependency_wait",
            Work::Failed(_) => "failed",
            Work::Idle => "idle",
        }
    }

    /// What it is, in words; `None` when idle.
    pub(crate) fn text(&self) -> Option<&str> {
        match self {
            Work::Running(t)
            | Work::Queued(t)
            | Work::User(t)
            | Work::Paused(t)
            | Work::Blocked(t)
            | Work::Failed(t) => Some(t),
            Work::Idle => None,
        }
    }

    /// Someone is at it or it waits on purpose: nothing stopped, nobody is continued.
    pub(crate) fn waiting(&self) -> bool {
        !matches!(self, Work::Failed(_) | Work::Idle)
    }
}

/// How a session's last turn ended, kept from its turn end for the stall watch (in memory: after a
/// restart the Hub's own recovery says what became of the turns it cut).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LastStop {
    pub stop_reason: String,
    pub error: Option<String>,
}

impl LastStop {
    /// Why an idle session is idle, from how its last turn ended; `true` when you stopped it
    /// yourself (it is yours to go on with, Steve does not continue it).
    pub(crate) fn cause(stop: Option<&LastStop>) -> (String, bool) {
        match stop {
            Some(s) if s.stop_reason == "cancelled" => ("你中断了这一回合".into(), true),
            Some(LastStop { error: Some(e), .. }) if !e.trim().is_empty() => {
                (format!("上一回合出错：{}", js_head(e, 160)), false)
            }
            Some(_) => ("上一回合结束后没有交付、提问，也没有后续工作".into(), false),
            None => (
                "会话空闲：没有回合在运行、没有排队输入，也没有交付或提问".into(),
                false,
            ),
        }
    }
}

fn names(xs: &[String]) -> String {
    let mut s = xs
        .iter()
        .take(NAMES)
        .cloned()
        .collect::<Vec<_>>()
        .join("、");
    if xs.len() > NAMES {
        s.push_str(&format!(" 等 {} 项", xs.len()));
    }
    s
}

impl Tx<'_> {
    /// What `item`'s work is doing now. The Hub's own holds (a release, maintenance) are not part of
    /// it: they delay turns, they do not explain an item.
    pub(crate) fn work(&self, item: &Item) -> Result<Work> {
        if item.status == "pause" {
            return Ok(Work::Paused("事项已暂停".into()));
        }
        if self.dependency_waiting(&item.id)? {
            let open = crate::lifecycle::deps_open(self.db(), &item.id)?;
            return Ok(Work::Blocked(if open.is_empty() {
                "前置事项已完成，等待续跑".into()
            } else {
                format!("等待前置事项完成：{}", open.join("、"))
            }));
        }
        if let Some(s) = self.active_session(&item.id)? {
            if self.busy(&s.id)
                || self.rt().uncertain(&s.id)
                || matches!(s.status.as_str(), "running" | "starting")
            {
                return Ok(Work::Running("回合执行中".into()));
            }
            if !self.db().queued(&s.id)?.is_empty() {
                return Ok(Work::Queued("有排队输入等待发送".into()));
            }
        }
        if let Some(d) = self
            .db()
            .decisions_of_item(&item.id)?
            .into_iter()
            .find(|d| d.status == "open")
        {
            return Ok(Work::User(format!(
                "等你处理：{}",
                js_head(&d.question, 80)
            )));
        }
        if let Some(g) = self.db().store().workflows().get(&item.id)?
            && !g.single_session()
            && let Some(w) = self.graph_work(&g)?
        {
            return Ok(w);
        }
        Ok(Work::Idle)
    }

    /// The graph's part: `None` when every unit is through and only the coordinator is left to act.
    pub(crate) fn graph_work(&self, g: &Workflow) -> Result<Option<Work>> {
        if g.paused {
            return Ok(Some(Work::Paused("协作已暂停".into())));
        }
        if !self.workflow_current(g)? {
            return Ok(Some(Work::Failed(
                "主事项目标或周期已变化，工作图需要重规划".into(),
            )));
        }
        let (mut running, mut user, mut queued, mut failed) = (vec![], vec![], vec![], vec![]);
        let mut to_start = false;
        /* pending units downstream of a failed or cancelled one wait on nothing that will come */
        let mut held: std::collections::HashSet<&str> = g
            .units
            .iter()
            .filter(|u| matches!(u.status.as_str(), "failed" | "cancelled"))
            .map(|u| u.spec.id.as_str())
            .collect();
        loop {
            let before = held.len();
            for u in g.units.iter().filter(|u| u.status == "pending") {
                if u.spec.depends_on.iter().any(|d| held.contains(d.as_str())) {
                    held.insert(u.spec.id.as_str());
                }
            }
            if held.len() == before {
                break;
            }
        }
        let mut downstream = vec![];
        for u in &g.units {
            let title = u.spec.title.clone();
            let open = match u.item_id.as_deref() {
                Some(child) => self
                    .db()
                    .decisions_of_item(child)?
                    .iter()
                    .any(|d| d.status == "open"),
                None => false,
            };
            if u.active() || self.workflow_busy(u)? {
                if open {
                    user.push(title);
                } else if self.unit_turn(u)? {
                    running.push(title);
                } else {
                    to_start = true;
                    queued.push(title);
                }
                continue;
            }
            match u.status.as_str() {
                "pending" if held.contains(u.spec.id.as_str()) => downstream.push(title),
                "pending" if u.attempt >= g.max_attempts => {
                    failed.push(format!("{title} 尝试次数已用完"));
                }
                "pending" => {
                    to_start = true;
                    queued.push(title);
                }
                "failed" if u.spec.required => failed.push(format!("{title} {}", said(u))),
                "cancelled" if u.spec.required => failed.push(format!("{title} 已取消")),
                "succeeded"
                    if u.item_id.as_deref().is_some_and(|child| {
                        self.db().item(child).ok().flatten().is_some_and(|i| {
                            i.phase != CLOSED || i.outcome.as_deref() != Some("completed")
                        })
                    }) =>
                {
                    queued.push(format!("{title}（交付待冻结）"));
                }
                _ => {}
            }
        }
        if failed.is_empty() && !downstream.is_empty() {
            /* held only by optional work that failed: still for the coordinator to sort out */
            failed.push("可选工作未通过".into());
        }
        let held = if downstream.is_empty() {
            String::new()
        } else {
            format!("；受阻：{}", names(&downstream))
        };
        if !running.is_empty() {
            return Ok(Some(Work::Running(format!(
                "协作执行中：{}",
                names(&running)
            ))));
        }
        if !user.is_empty() {
            return Ok(Some(Work::User(format!(
                "协作工作等你处理：{}",
                names(&user)
            ))));
        }
        if to_start && let Err(e) = self.rt().check_workflow_execution(&g.item_id) {
            return Ok(Some(Work::Failed(format!(
                "协作工作无法开始：{}",
                js_head(&e.to_string(), 160)
            ))));
        }
        if !queued.is_empty() {
            return Ok(Some(Work::Queued(format!(
                "协作排队中：{}",
                names(&queued)
            ))));
        }
        if !failed.is_empty() {
            return Ok(Some(Work::Failed(format!(
                "协作工作未通过：{}{held}",
                names(&failed)
            ))));
        }
        Ok(None)
    }

    /// A turn of the unit's session runs or is not confirmed over.
    fn unit_turn(&self, u: &WorkUnit) -> Result<bool> {
        let Some(child) = u.item_id.as_deref() else {
            return Ok(false);
        };
        for s in self.db().sessions_of_item(child)? {
            if self.busy(&s.id)
                || self.rt().uncertain(&s.id)
                || matches!(s.status.as_str(), "running" | "starting")
            {
                return Ok(true);
            }
        }
        Ok(false)
    }
}

/// A failed or cancelled unit, in a few words: its reason when it gave one.
fn said(u: &WorkUnit) -> String {
    if u.status == "cancelled" {
        return "已取消".into();
    }
    let why = u
        .reason
        .as_deref()
        .filter(|r| !r.trim().is_empty())
        .unwrap_or(u.summary.as_str())
        .trim();
    if why.is_empty() {
        "未通过".into()
    } else {
        format!("未通过：{}", js_head(why, 80))
    }
}
