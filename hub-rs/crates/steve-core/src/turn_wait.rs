//! Facts that keep the stall watcher from treating an idle turn as a failure.

use steve_store::{SessionStop, WorkUnit, Workflow};

use crate::cycle::{ACTIVE, CLOSED, CLOSING};
use crate::lifecycle::{stop_card, supersede_where};
use crate::repo::Item;
use crate::util::js_head;
use crate::{Result, Tx};

/// Work that needs a coordinator's intervention.
pub(crate) enum CoordinatorAttention {
    PausedByAgent(String),
    Failed(String),
}

enum GraphProgress {
    Pending,
    NeedsAttention(CoordinatorAttention),
    Settled,
}

/// Why an idle session is idle, from how its last turn ended (kept in `session_stops`, so the same
/// after a restart); `true` when you stopped it yourself: it is yours to go on with, Steve does not
/// continue it. Never from the session's last system line.
pub(crate) fn stopped(stop: Option<&SessionStop>) -> (String, bool) {
    match stop {
        Some(s) if yours(s) => ("你中断了这一回合".into(), true),
        Some(s) if s.stop_asked.is_some() => {
            ("你要求停止这一回合，还没有确认它已停下".into(), true)
        }
        Some(SessionStop {
            error: Some(e),
            stop_reason: Some(_),
            ..
        }) if !e.trim().is_empty() => (format!("上一回合出错：{}", js_head(e, 160)), false),
        Some(SessionStop {
            stop_reason: Some(_),
            ..
        }) => ("上一回合结束后没有交付、提问，也没有后续工作".into(), false),
        _ => (
            "会话空闲：没有回合在运行、没有排队输入，也没有交付或提问".into(),
            false,
        ),
    }
}

/// The last turn was stopped by you: your Stop reached it, or it ended cancelled (the Hub's own
/// cancels, a pause or a steer, leave nothing idle to judge).
pub(crate) fn yours(s: &SessionStop) -> bool {
    s.yours || s.stop_reason.as_deref() == Some("cancelled")
}

/// Your Stop holds the session: its last turn was stopped by you ([`yours`]), or you asked for a
/// Stop whose turn end has not come (the backend could not confirm it, or that turn's end is still
/// on its way). Either way going on is yours, never Steve's or YOLO's.
pub(crate) fn held(s: &SessionStop) -> bool {
    s.stop_reason.is_some() && yours(s) || s.stop_asked.is_some()
}

fn names(xs: &[String]) -> String {
    let mut s = xs
        .iter()
        .take(3)
        .cloned()
        .collect::<Vec<_>>()
        .join(&steve_i18n::t!("core.now.list_sep"));
    if xs.len() > 3 {
        s = steve_i18n::t!("core.now.names_more", names = s, count = xs.len());
    }
    s
}

struct UnitWork {
    in_progress: bool,
    waiting_to_finish: bool,
    failed: Vec<String>,
    downstream: Vec<String>,
    to_start: bool,
}

fn held_units(g: &Workflow) -> std::collections::HashSet<&str> {
    /* pending units downstream of a failed or cancelled one, or of one out of attempts (it
     * never starts again: workflow_drive starts only a unit with attempts left), wait on
     * nothing that will come */
    let mut held: std::collections::HashSet<&str> = g
        .units
        .iter()
        .filter(|u| {
            matches!(u.status.as_str(), "failed" | "cancelled")
                || u.status == "pending" && u.attempt >= g.max_attempts
        })
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
    held
}

impl Tx<'_> {
    /// A live turn, queued input, an explicit hold or an open decision can all explain an idle
    /// session. When retrying a stop, ignore the card that the retry would answer.
    pub(crate) fn turn_has_wait(&self, item: &Item, include_stop_cards: bool) -> Result<bool> {
        if item.is_closed()
            || item.is_paused()
            || item.phase == CLOSING
            || self.dependency_waiting(&item.id)?
        {
            return Ok(true);
        }
        if let Some(s) = self.active_session(&item.id)?
            && (self.busy(&s.id)
                || self.rt().uncertain(&s.id)
                || matches!(s.status.as_str(), "running" | "starting")
                || !self.db().queued(&s.id)?.is_empty())
        {
            return Ok(true);
        }
        let cards = self.db().decisions_of_item(&item.id)?;
        let open = |d: &&steve_store::Decision| d.status == "open";
        if cards.iter().filter(open).any(|d| !stop_card(d)) || self.quota_waiting(&item.id) {
            return Ok(true);
        }
        if let Some(g) = self.db().store().workflows().get(&item.id)?
            && !g.single_session()
            && matches!(self.coordinator_progress(&g)?, GraphProgress::Pending)
        {
            return Ok(true);
        }
        Ok(include_stop_cards && cards.iter().any(|d| d.status == "open"))
    }

    /// Failed required units and an agent's own pause need an explicit response.
    pub(crate) fn coordinator_attention(&self, item: &str) -> Result<Option<CoordinatorAttention>> {
        let graph = self.db().store().workflows().get(item)?;
        let Some(graph) = graph.filter(|g| !g.single_session()) else {
            return Ok(None);
        };
        match self.coordinator_progress(&graph)? {
            GraphProgress::NeedsAttention(attention) => Ok(Some(attention)),
            GraphProgress::Pending | GraphProgress::Settled => Ok(None),
        }
    }

    /// Cards the work has moved past, on every pass of the stall watch (so also right after a
    /// restart, when no unit start is going to tell): a parent's stop cards while its units run,
    /// queue, wait for you or are paused; the leftover questions and stop cards of a unit that failed
    /// or was cancelled. Cards about an effect nobody knows, a release, an approval or an acceptance
    /// stay.
    pub(crate) fn expire_obsolete_turn_cards(&mut self) -> Result<usize> {
        let mut n = self.receipts_tidy()?;
        for g in self.db().store().workflows().all_open()? {
            if g.single_session() {
                continue;
            }
            if self
                .db()
                .item(&g.item_id)?
                .is_some_and(|i| !i.archived && i.phase == ACTIVE)
                && matches!(self.coordinator_progress(&g)?, GraphProgress::Pending)
            {
                n += supersede_where(self.db(), &g.item_id, stop_card, "协作工作在等待或执行中")?;
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

    fn graph_facts(&self, g: &Workflow) -> Result<UnitWork> {
        let mut failed = Vec::new();
        let mut in_progress = false;
        let mut waiting_to_finish = false;
        let mut to_start = false;
        let held = held_units(g);
        let mut downstream = vec![];
        for u in &g.units {
            let title = u.spec.title.clone();
            if u.active() || self.workflow_busy(u)? {
                let open = match u.item_id.as_deref() {
                    Some(child) => self
                        .db()
                        .decisions_of_item(child)?
                        .iter()
                        .any(|d| d.status == "open"),
                    None => false,
                };
                let occupied = open || self.unit_turn(u)?;
                in_progress |= occupied;
                to_start |= !occupied;
                continue;
            }
            match u.status.as_str() {
                "pending" if u.attempt >= g.max_attempts => {
                    failed.push(steve_i18n::t!(
                        "core.now.unit_out_of_attempts",
                        title = title
                    ));
                }
                "pending" if held.contains(u.spec.id.as_str()) => downstream.push(title),
                "pending" => to_start = true,
                "failed" | "cancelled" if u.spec.required => failed.push(said(&title, u)),
                "succeeded"
                    if u.item_id.as_deref().is_some_and(|child| {
                        self.db().item(child).ok().flatten().is_some_and(|i| {
                            i.phase != CLOSED || i.outcome.as_deref() != Some("completed")
                        })
                    }) =>
                {
                    waiting_to_finish = true;
                }
                _ => {}
            }
        }
        Ok(UnitWork {
            in_progress,
            waiting_to_finish,
            failed,
            to_start,
            downstream,
        })
    }

    fn coordinator_progress(&self, g: &Workflow) -> Result<GraphProgress> {
        let needs = |failure| GraphProgress::NeedsAttention(CoordinatorAttention::Failed(failure));
        if let Some((by, why)) = g.paused_by() {
            return Ok(if by == steve_store::PAUSED_BY_AGENT {
                GraphProgress::NeedsAttention(CoordinatorAttention::PausedByAgent(steve_i18n::t!(
                    "core.now.workflow_halted",
                    why = js_head(why, 160)
                )))
            } else {
                GraphProgress::Pending
            });
        }
        if !self.workflow_current(g)? {
            return Ok(needs(steve_i18n::t!("core.now.workflow_replan")));
        }
        let UnitWork {
            in_progress,
            waiting_to_finish,
            to_start,
            mut failed,
            downstream,
        } = self.graph_facts(g)?;
        if failed.is_empty() && !downstream.is_empty() {
            failed.push(steve_i18n::t!("core.now.optional_failed"));
        }
        if in_progress
            || !self.cfg().dynamic_workflows
                && (to_start || waiting_to_finish || !failed.is_empty())
        {
            return Ok(GraphProgress::Pending);
        }
        if to_start && let Err(e) = self.rt().check_workflow_execution(&g.item_id) {
            return Ok(needs(steve_i18n::t!(
                "core.now.workflow_cannot_start",
                why = js_head(&e.to_string(), 160)
            )));
        }
        if to_start || waiting_to_finish {
            return Ok(GraphProgress::Pending);
        }
        if failed.is_empty() {
            return Ok(GraphProgress::Settled);
        }
        Ok(needs(if downstream.is_empty() {
            steve_i18n::t!("core.now.workflow_failed", names = names(&failed))
        } else {
            steve_i18n::t!(
                "core.now.workflow_failed_held",
                names = names(&failed),
                held = names(&downstream)
            )
        }))
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
