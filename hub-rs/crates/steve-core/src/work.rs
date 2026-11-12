//! What an item's work is doing now (ST-10): one judgement, from durable state, for the turn end, the
//! stall watch, Steve's continues and the diagnostics. A coordinator that ends its turn while its
//! work graph runs, queues, waits for you or is paused by you is waiting on purpose; a graph it
//! paused itself without asking waits for you, and you are asked; a required unit that failed is a
//! real failure it has to act on. Never judged from the session's last words or its last
//! system line (an old release notice says nothing about why it is idle now).

use steve_store::{WorkUnit, Workflow};

use crate::cycle::CLOSED;
use crate::lifecycle::{stop_card, supersede_where};
use crate::repo::Item;
use crate::util::js_head;
use crate::{Result, Tx};

/// How input waiting on a Node with no link reads (followed by why).
pub(crate) const NODE_WAIT: &str = "等待 Node 连接：";

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
    /// Stopped on purpose: you paused the item or its work graph.
    Paused(String),
    /// The coordinator paused its own work graph and asked you nothing: it waits for you to decide,
    /// so it is a stop that reaches you (a card), not a quiet wait. Steve does not continue it: the
    /// pause was the agent's own call (ST-5, ST-10, ST-27).
    Halted(String),
    /// Waits on registered prerequisites.
    Blocked(String),
    /// Waits for a time: the model's quota is back then ([`crate::quota`]).
    Until(String),
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
            Work::Halted(_) => "paused_by_agent",
            Work::Blocked(_) => "dependency_wait",
            Work::Until(_) => "quota_wait",
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
            | Work::Halted(t)
            | Work::Blocked(t)
            | Work::Until(t)
            | Work::Failed(t) => Some(t),
            Work::Idle => None,
        }
    }

    /// Someone is at it or it waits on purpose: nothing stopped, nobody is continued.
    pub(crate) fn waiting(&self) -> bool {
        !matches!(self, Work::Failed(_) | Work::Halted(_) | Work::Idle)
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
        .join(&steve_i18n::t!("core.now.list_sep"));
    if xs.len() > NAMES {
        s = steve_i18n::t!("core.now.names_more", names = s, count = xs.len());
    }
    s
}

impl Tx<'_> {
    /// What `item`'s work is doing now. The Hub's own holds (a release, maintenance) are not part of
    /// it: they delay turns, they do not explain an item. Its words are written into the item's `now`,
    /// so they are zh-CN whoever asks ([`crate::now_line`] shows them in the reader's language).
    pub(crate) fn work(&self, item: &Item) -> Result<Work> {
        steve_i18n::stored(|| self.work_in(item))
    }

    fn work_in(&self, item: &Item) -> Result<Work> {
        if item.status == "pause" {
            return Ok(Work::Paused(steve_i18n::t!("core.now.item_paused")));
        }
        if self.dependency_waiting(&item.id)? {
            let open = crate::lifecycle::deps_open(self.db(), &item.id)?;
            return Ok(Work::Blocked(if open.is_empty() {
                steve_i18n::t!("core.now.deps_done")
            } else {
                steve_i18n::t!(
                    "core.now.deps_wait",
                    deps = open.join(&steve_i18n::t!("core.now.list_sep"))
                )
            }));
        }
        if let Some(s) = self.active_session(&item.id)? {
            if self.busy(&s.id)
                || self.rt().uncertain(&s.id)
                || matches!(s.status.as_str(), "running" | "starting")
            {
                return Ok(Work::Running(steve_i18n::t!("core.now.turn_running")));
            }
            if !self.db().queued(&s.id)?.is_empty() {
                return Ok(Work::Queued(match self.rt().node_offline(&s.id) {
                    Some(why) => steve_i18n::t!("core.now.node_wait", why = js_head(&why, 160)),
                    None => steve_i18n::t!("core.now.input_queued"),
                }));
            }
        }
        let open: Vec<_> = self
            .db()
            .decisions_of_item(&item.id)?
            .into_iter()
            .filter(|d| d.status == "open")
            .collect();
        let user = |d: &steve_store::Decision| {
            Work::User(steve_i18n::t!(
                "core.now.waiting_user",
                question = js_head(&d.question, 80)
            ))
        };
        if let Some(d) = open.iter().find(|d| !stop_card(d)) {
            return Ok(user(d));
        }
        if open.is_empty()
            && let Some(t) = self.quota_text(&item.id)
        {
            return Ok(Work::Until(t));
        }
        /* a card saying it stopped is outdated by units at work (one left from before a restart,
         * or from before these rules): the graph's state stands, and [`Tx::work_tidy`] expires it */
        if let Some(g) = self.db().store().workflows().get(&item.id)?
            && !g.single_session()
            && let Some(w) = self.graph_work(&g)?
            && (open.is_empty() || w.waiting())
        {
            return Ok(w);
        }
        if let Some(d) = open.first() {
            return Ok(user(d));
        }
        Ok(Work::Idle)
    }

    /// Cards the work has moved past, on every pass of the stall watch (so also right after a
    /// restart, when no unit start is going to tell): a parent's stop cards while its units run,
    /// queue, wait for you or are paused; the leftover questions and stop cards of a unit that failed
    /// or was cancelled. Cards about an effect nobody knows, a release, an approval or an acceptance
    /// stay.
    pub(crate) fn work_tidy(&mut self) -> Result<usize> {
        let mut n = 0;
        for g in self.db().store().workflows().all_open()? {
            if g.single_session() {
                continue;
            }
            if self
                .db()
                .item(&g.item_id)?
                .is_some_and(|i| !i.archived && i.phase == crate::cycle::ACTIVE)
                && let Some(w) = self.graph_work(&g)?
                && w.waiting()
                && let Some(text) = w.text()
            {
                let gone = supersede_where(self.db(), &g.item_id, stop_card, text)?;
                if gone > 0 {
                    self.db().item_patch(&g.item_id, crate::patch![now: text])?;
                }
                n += gone;
            }
            for u in &g.units {
                if let Some(child) = u.item_id.as_deref()
                    && matches!(u.status.as_str(), "failed" | "cancelled")
                {
                    n += self.unit_cards_expire(child, u)?;
                }
            }
        }
        Ok(n)
    }

    /// A unit that failed or was cancelled waits on nobody: its own questions and stop cards go;
    /// the coordinator retries or replans it.
    pub(crate) fn unit_cards_expire(&mut self, child: &str, u: &WorkUnit) -> Result<usize> {
        let why = format!(
            "工作单元 {} {}，由主事项负责人重试或重规划",
            u.spec.title,
            if u.status == "cancelled" {
                "已取消"
            } else {
                "未通过"
            }
        );
        supersede_where(self.db(), child, |d| stop_card(d) || d.kind == "ask", &why)
    }

    /// The graph's part: `None` when every unit is through and only the coordinator is left to act.
    /// In zh-CN like [`Self::work`].
    pub(crate) fn graph_work(&self, g: &Workflow) -> Result<Option<Work>> {
        steve_i18n::stored(|| self.graph_work_in(g))
    }

    fn graph_work_in(&self, g: &Workflow) -> Result<Option<Work>> {
        if let Some((by, why)) = g.paused_by() {
            return Ok(Some(if by == steve_store::PAUSED_BY_AGENT {
                Work::Halted(steve_i18n::t!(
                    "core.now.workflow_halted",
                    why = js_head(why, 160)
                ))
            } else {
                Work::Paused(steve_i18n::t!("core.now.workflow_paused"))
            }));
        }
        if !self.workflow_current(g)? {
            return Ok(Some(Work::Failed(steve_i18n::t!(
                "core.now.workflow_replan"
            ))));
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
                    failed.push(steve_i18n::t!(
                        "core.now.unit_out_of_attempts",
                        title = title
                    ));
                }
                "pending" => {
                    to_start = true;
                    queued.push(title);
                }
                "failed" | "cancelled" if u.spec.required => failed.push(said(&title, u)),
                "succeeded"
                    if u.item_id.as_deref().is_some_and(|child| {
                        self.db().item(child).ok().flatten().is_some_and(|i| {
                            i.phase != CLOSED || i.outcome.as_deref() != Some("completed")
                        })
                    }) =>
                {
                    queued.push(steve_i18n::t!(
                        "core.now.unit_frozen_pending",
                        title = title
                    ));
                }
                _ => {}
            }
        }
        if failed.is_empty() && !downstream.is_empty() {
            /* held only by optional work that failed: still for the coordinator to sort out */
            failed.push(steve_i18n::t!("core.now.optional_failed"));
        }
        if !running.is_empty() {
            return Ok(Some(Work::Running(steve_i18n::t!(
                "core.now.workflow_running",
                names = names(&running)
            ))));
        }
        if !user.is_empty() {
            return Ok(Some(Work::User(steve_i18n::t!(
                "core.now.workflow_user",
                names = names(&user)
            ))));
        }
        if to_start && let Err(e) = self.rt().check_workflow_execution(&g.item_id) {
            return Ok(Some(Work::Failed(steve_i18n::t!(
                "core.now.workflow_cannot_start",
                why = js_head(&e.to_string(), 160)
            ))));
        }
        if !queued.is_empty() {
            return Ok(Some(Work::Queued(steve_i18n::t!(
                "core.now.workflow_queued",
                names = names(&queued)
            ))));
        }
        if !failed.is_empty() {
            return Ok(Some(Work::Failed(if downstream.is_empty() {
                steve_i18n::t!("core.now.workflow_failed", names = names(&failed))
            } else {
                steve_i18n::t!(
                    "core.now.workflow_failed_held",
                    names = names(&failed),
                    held = names(&downstream)
                )
            })));
        }
        Ok(None)
    }

    /// A turn of the unit's session runs or is not confirmed over.
    pub(crate) fn unit_turn(&self, u: &WorkUnit) -> Result<bool> {
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
fn said(title: &str, u: &WorkUnit) -> String {
    if u.status == "cancelled" {
        return steve_i18n::t!("core.now.unit_cancelled", title = title);
    }
    let why = u
        .reason
        .as_deref()
        .filter(|r| !r.trim().is_empty())
        .unwrap_or(u.summary.as_str())
        .trim();
    if why.is_empty() {
        steve_i18n::t!("core.now.unit_failed", title = title)
    } else {
        steve_i18n::t!(
            "core.now.unit_failed_why",
            title = title,
            why = js_head(&crate::workflow::workflow_error(why), 80)
        )
    }
}
