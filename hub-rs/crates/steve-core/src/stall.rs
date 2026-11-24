//! Items that stopped without saying so: `run`, but their session has been idle for a while and their
//! work ([`crate::work`]) neither runs, queues, waits for you, is paused by you nor waits on
//! prerequisites (a cancelled turn, an agent that died, a coordinator left with a failed unit or
//! that paused its graph without asking you). They get one card,
//! “它停下了：…”, with 让它继续 / 我来看看 / 放弃; it expires as soon as a turn starts again.
//!
//! The same tick also runs the sweep over every open item ([`crate::sweep`]), which only records
//! what it would do: this watch and Steve's continues stay the only things that act.

use std::time::Duration;

use serde_json::json;

use crate::error::Result;
use crate::repo::NewDecision;
use crate::turn_wait::{CoordinatorAttention, stopped};
use crate::util::{js_head, now};
use crate::{Core, Tx};

/// How long an item may sit in `run` with nothing going on before it gets the card.
pub const STALL_AFTER_MS: i64 = 2 * 60_000;
/// The card's subject type, so a starting turn expires only this card.
pub const STALL: &str = "stall";
const CHECK_EVERY: Duration = Duration::from_secs(30);

impl Core {
    /// One pass: opens the card for every stalled item; returns their ids.
    pub fn check_stalls(&self) -> Result<Vec<String>> {
        self.tx().check_stalls(now())
    }

    /// A turn of the session started: its item's stall card no longer applies.
    pub fn expire_stall(&self, session_id: &str) -> Result<()> {
        self.tx().expire_stall(session_id)
    }

    /// Checks for stalled sessions every 30 seconds.
    pub async fn run_stall_watch(self) {
        self.stall_watch(CHECK_EVERY).await;
    }

    /// The stall watch every `every` (tests run it faster).
    pub(crate) async fn stall_watch(self, every: Duration) {
        let clock = self
            .modules()
            .map(|m| m.clock.clone())
            .unwrap_or_else(|| std::sync::Arc::new(steve_runtime::SystemClock));
        steve_runtime::every(
            "core.stall",
            clock,
            every,
            steve_runtime::FirstRun::Immediate,
            move || {
                let this = self.clone();
                async move {
                    let c = this.clone();
                    match tokio::task::spawn_blocking(move || c.check_stalls()).await {
                        Ok(Err(e)) => tracing::error!("stall check {e}"),
                        Err(e) => tracing::error!("stall check task {e}"),
                        Ok(Ok(_)) => {}
                    }
                }
            },
        )
        .await;
    }
}

impl Tx<'_> {
    pub(crate) fn check_stalls(&mut self, at: i64) -> Result<Vec<String>> {
        // cards the work has moved past go first, holds or not (crate::work)
        self.expire_obsolete_turn_cards()?;
        // a release in flight explains an idle item (and no turn may start anyway)
        if self.holding() || self.st.releasing.is_some() {
            return Ok(vec![]);
        }
        let mut out = vec![];
        /* quotas that are back: their agents go on */
        self.quota_wake(at)?;
        for ws in self.db().workspace_ids()? {
            /* only work in progress stalls; a closing waits on its own gate */
            for id in self.db().items_running(&ws)? {
                let Some(i) = self.db().item(&id)? else {
                    continue;
                };
                let Some(s) = self.active_session(&i.id)? else {
                    continue;
                };
                /* a unit's session belongs to its graph: the graph resumes it or reports it; its coordinator handles follow-up */
                if self.workflow_child(&i)? {
                    continue;
                }
                if self.turn_has_wait(&i, true)? {
                    continue;
                }
                let last = self.db().last_event_ts(&s.id)?.unwrap_or(0).max(s.updated);
                if at - last < STALL_AFTER_MS {
                    continue;
                }
                /* the cause from state, never the session's last system line */
                /* your Stop comes first: a failure the coordinator was told of and you then stopped
                 * is shown with its reason, and is yours to go on with (no continue spent) */
                let (cause, yours) = stopped(self.db().store().sessions().stop(&s.id)?.as_ref());
                let attention = self.coordinator_attention(&i.id)?;
                let failed = matches!(attention, Some(CoordinatorAttention::Failed(_)));
                let (why, yours) = match attention {
                    Some(CoordinatorAttention::Failed(why)) if yours => {
                        (format!("{why}；{cause}"), true)
                    }
                    Some(CoordinatorAttention::Failed(why)) => (why, false),
                    Some(CoordinatorAttention::PausedByAgent(why)) => (why, true),
                    None => (cause, yours),
                };
                let mut question = format!("它停下了：{}", js_head(&why, 200));
                /* Steve looks first, unless it was you who stopped it */
                if !yours {
                    let stop = if failed {
                        crate::takeover::Stop::Workflow {
                            why: question.clone(),
                        }
                    } else {
                        crate::takeover::Stop::Idle {
                            why: question.clone(),
                        }
                    };
                    match self.takeover(&i.id, &s.id, stop)? {
                        Some(q) => question = q,
                        None => continue,
                    }
                }
                let d = self.db().decision_create(NewDecision {
                    workspace_id: i.workspace_id.clone(),
                    kind: "fail".into(),
                    subject_type: STALL.into(),
                    subject_id: i.id.clone(),
                    item_id: Some(i.id.clone()),
                    session_id: Some(s.id.clone()),
                    question: question.clone(),
                    options: Some(vec!["continue".into(), "look".into(), "drop".into()]),
                    requested_by: "steve".into(),
                    ..Default::default()
                })?;
                self.mark_fail(&i.workspace_id, &i.id, &question, &d.id); // stats (ST3)
                self.db()
                    .timeline_add(&i.id, "steve", "sys", &question, Some(&s.id))?;
                out.push(i.id);
            }
        }
        Ok(out)
    }

    pub(crate) fn expire_stall(&mut self, session_id: &str) -> Result<()> {
        self.quota_turn_started(session_id);
        let Some(item) = self.db().session(session_id)?.and_then(|s| s.item_id) else {
            return Ok(());
        };
        let open: Vec<_> = self
            .db()
            .decisions_of_item(&item)?
            .into_iter()
            .filter(|d| d.status == "open" && d.subject_type == STALL)
            .collect();
        for d in &open {
            self.db().decision_resolve(
                &d.id,
                json!({"superseded": "它又开始工作了"}),
                "expired",
                None,
            )?;
        }
        /* a unit at work again: the coordinator's card about a stop (its own, or a unit's failure)
         * no longer describes the parent either */
        /* by_child also finds retired attempts: only the graph's current one counts */
        if let Some((parent, _)) = self.db().store().workflows().by_child(&item)?
            && let Some(unit) = self.db().store().workflows().get(&parent)?.and_then(|g| {
                g.units
                    .into_iter()
                    .find(|u| u.session_id.as_deref() == Some(session_id))
            })
        {
            crate::lifecycle::supersede_where(
                self.db(),
                &parent,
                crate::lifecycle::stop_card,
                &format!("协作工作 {} 又开始执行了", unit.spec.title),
            )?;
        }
        Ok(())
    }
}
