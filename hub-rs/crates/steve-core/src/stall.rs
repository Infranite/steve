//! Items that stopped without saying so: `run`, but their session has been idle for a while with no
//! turn, nothing queued and no open card (a cancelled turn, an agent that died). They get one card,
//! “它停下了：…”, with 让它继续 / 我来看看 / 放弃; it expires as soon as a turn starts again.

use std::time::Duration;

use serde_json::json;

use crate::error::Result;
use crate::lifecycle::settle;
use crate::repo::NewDecision;
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

    /// Every 30 s. Runs forever.
    pub async fn run_stall_watch(self) {
        let mut tick = tokio::time::interval(CHECK_EVERY);
        loop {
            tick.tick().await;
            let c = self.clone();
            match tokio::task::spawn_blocking(move || c.check_stalls()).await {
                Ok(Err(e)) => tracing::error!("stall check {e}"),
                Err(e) => tracing::error!("stall check task {e}"),
                Ok(Ok(_)) => {}
            }
            /* the Release builds Steve follows (network calls, outside the lock) */
            let c = self.clone();
            match tokio::task::spawn_blocking(move || c.check_builds()).await {
                Ok(Err(e)) => tracing::warn!("build check {e}"),
                Err(e) => tracing::warn!("build check task {e}"),
                Ok(Ok(_)) => {}
            }
        }
    }
}

impl Tx<'_> {
    pub(crate) fn check_stalls(&mut self, at: i64) -> Result<Vec<String>> {
        // a release in flight explains an idle item (and no turn may start anyway)
        if self.rt().is_holding() || self.st.releasing.is_some() {
            return Ok(vec![]);
        }
        let mut out = vec![];
        for ws in self.db().workspace_ids()? {
            for i in self.db().items_all(&ws)? {
                /* only work in progress stalls; a closing waits on its own gate */
                if i.status != "run" || i.archived || i.phase != crate::cycle::ACTIVE {
                    continue;
                }
                let Some(s) = self.active_session(&i.id)? else {
                    continue;
                };
                if self.rt().is_busy(&s.id)
                    || self.st.resumes.iter().any(|(_, ids)| ids.contains(&s.id))
                    || !self.db().queued(&s.id)?.is_empty()
                    || self
                        .db()
                        .decisions_of_item(&i.id)?
                        .iter()
                        .any(|d| d.status == "open")
                {
                    continue;
                }
                let last = self.db().last_event_ts(&s.id)?.unwrap_or(0).max(s.updated);
                if at - last < STALL_AFTER_MS {
                    continue;
                }
                let why = self
                    .db()
                    .last_system_text(&s.id)?
                    .filter(|t| !t.trim().is_empty())
                    .unwrap_or_else(|| "会话空闲，没有在运行，也没有交付或提问".into());
                let mut question = format!("它停下了：{}", js_head(&why, 200));
                /* Steve looks first, unless it was you who stopped it */
                if !why.starts_with("你中断了") {
                    let stop = crate::takeover::Stop::Idle {
                        why: question.clone(),
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
                settle(self.db(), &i.id)?;
                out.push(i.id);
            }
        }
        Ok(out)
    }

    pub(crate) fn expire_stall(&mut self, session_id: &str) -> Result<()> {
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
        if !open.is_empty() {
            settle(self.db(), &item)?;
        }
        Ok(())
    }
}
