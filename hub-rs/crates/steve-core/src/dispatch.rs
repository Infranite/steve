//! Asynchronous Steve Bot-selection worker for multi-candidate automatic item dispatch (#626).
//!
//! When automatic item creation finds >= 2 hard-eligible non-main Bots, the item commits
//! unassigned with a durable row in `item_dispatch_jobs`. This worker runs a short Think job
//! with `ThinkTools::Dispatch` (exposing only `steve_bots_list` and `steve_nodes_list`) so Steve
//! inspects real Bot/Node state and chooses the executor, then Hub hard-validates the choice
//! before starting the item's session. Failures never silently swap Bots or fall back to scoring.
use std::sync::Arc;
use std::time::{Duration, Instant};

use steve_store::bot_dispatch::{self, DispatchJob};
use steve_types::{
    CoordinatorDeferSource, DecisionStatus, DispatchErrorCode, DispatchEvent, NoExecutorPolicy,
    PlacementBlocked, PlacementBlocker, PlacementReason, PlacementRejection,
};

use crate::error::{CoreError, Result};
use crate::gateway::{
    DISPATCH_TIMEOUT, DispatchChoice, DispatchPromptInput, ThinkTools, Tier, dispatch_prompt,
    parse_dispatch_choice,
};
use crate::patch;
use crate::util::js_head;
use crate::{Core, Tx};

/// Maximum concurrent Steve Bot-selection Think jobs across workspaces.
pub const MAX_CONCURRENT_DISPATCHES: usize = 4;

/// Safety sweep interval so pending dispatch jobs are picked up even if a wake signal raced.
const DISPATCH_SWEEP_INTERVAL: Duration = Duration::from_secs(5);

/// Maximum character length kept for failure detail strings persisted in `DispatchEvent::Failed`.
const MAX_DISPATCH_DETAIL_CHARS: usize = 200;

/// Maximum character length kept for unparseable model reply excerpts in `DispatchEvent::Failed`.
const MAX_INVALID_RESPONSE_EXCERPT_CHARS: usize = 160;

/// Persist a typed `DispatchEvent` in `timeline.text` under `kind = "dispatch"`.
/// User-visible localization happens strictly in the UI layer.
pub(crate) fn record_dispatch_timeline(
    db: &crate::repo::Db,
    item_id: &str,
    actor: &str,
    event: &DispatchEvent,
) -> Result<()> {
    let payload = serde_json::to_string(event)?;
    db.timeline_add(item_id, actor, "dispatch", &payload, None)?;
    Ok(())
}

fn record_job_metrics(started: Instant, outcome: &'static str) {
    metrics::histogram!("steve_dispatch_job_duration_seconds", "outcome" => outcome)
        .record(started.elapsed().as_secs_f64());
    metrics::counter!("steve_dispatch_job_total", "outcome" => outcome).increment(1);
}

fn record_invalid_choice(reason: PlacementRejection) {
    metrics::counter!(
        "steve_dispatch_invalid_choice_total",
        "source" => "job",
        "reason" => reason.as_str()
    )
    .increment(1);
}

enum DispatchEval {
    Selected(DispatchChoice),
    Failed {
        error: DispatchErrorCode,
        detail: Option<String>,
    },
}

struct PreparedDispatch {
    prompt: String,
}

struct CancelGuard {
    core: Core,
    item_id: String,
    attempt_id: String,
}

impl Drop for CancelGuard {
    fn drop(&mut self) {
        let mut cancels = self
            .core
            .0
            .dispatch_cancels
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if cancels
            .get(&self.item_id)
            .is_some_and(|(att, _)| att == &self.attempt_id)
        {
            cancels.remove(&self.item_id);
        }
    }
}

impl Core {
    /// Wake the background Bot-dispatch worker after enqueueing a pending `item_dispatch_jobs` row.
    pub(crate) fn wake_dispatch(&self) {
        self.0.dispatch_wake.notify_one();
    }

    fn register_dispatch_cancel(
        &self,
        item_id: &str,
        attempt_id: &str,
    ) -> (Arc<tokio::sync::Notify>, CancelGuard) {
        let notify = Arc::new(tokio::sync::Notify::new());
        let prev = {
            let mut cancels = self
                .0
                .dispatch_cancels
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            cancels.insert(item_id.to_owned(), (attempt_id.to_owned(), notify.clone()))
        };
        if let Some((_, old)) = prev {
            old.notify_one();
        }
        (
            notify,
            CancelGuard {
                core: self.clone(),
                item_id: item_id.to_owned(),
                attempt_id: attempt_id.to_owned(),
            },
        )
    }

    /// Abort any in-flight Steve dispatch Think job for `item_id` immediately.
    pub(crate) fn cancel_inflight_dispatch(&self, item_id: &str) {
        let entry = {
            let mut cancels = self
                .0
                .dispatch_cancels
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            cancels.remove(item_id)
        };
        if let Some((_, notify)) = entry {
            notify.notify_one();
        }
    }

    /// Background task processing durable `item_dispatch_jobs` with bounded concurrency.
    pub fn watch_dispatch(self) -> steve_runtime::Task {
        steve_runtime::spawn("core.dispatch", async move {
            let store = self.store().store().clone();
            match tokio::task::spawn_blocking(move || {
                store.write(bot_dispatch::reset_running_dispatches)
            })
            .await
            {
                Ok(Ok(n)) if n > 0 => {
                    tracing::info!(
                        reset = n,
                        "dispatch: reset stale running dispatch jobs on startup"
                    );
                }
                Ok(Ok(_)) => {}
                Ok(Err(e)) => tracing::warn!("dispatch: startup reset failed: {e}"),
                Err(e) => tracing::warn!("dispatch: startup reset task failed: {e}"),
            }

            loop {
                let Ok(permit) = self.0.dispatch_permits.clone().acquire_owned().await else {
                    break;
                };
                let store = self.store().store().clone();
                let claimed = tokio::task::spawn_blocking(move || {
                    let attempt_id = format!("dsp_{}", crate::util::token());
                    store.write(|w| bot_dispatch::claim_next_dispatch(w, &attempt_id))
                })
                .await;
                match claimed {
                    Ok(Ok(Some(job))) => {
                        let this = self.clone();
                        tokio::spawn(async move {
                            let _permit = permit;
                            this.run_dispatch_job_supervised(job).await;
                        });
                    }
                    Ok(Ok(None)) => {
                        drop(permit);
                        tokio::select! {
                            _ = self.0.dispatch_wake.notified() => {}
                            _ = tokio::time::sleep(DISPATCH_SWEEP_INTERVAL) => {}
                        }
                    }
                    Ok(Err(e)) => {
                        drop(permit);
                        tracing::warn!("dispatch: claim failed: {e}");
                        tokio::time::sleep(DISPATCH_SWEEP_INTERVAL).await;
                    }
                    Err(e) => {
                        drop(permit);
                        tracing::warn!("dispatch: claim task failed: {e}");
                        tokio::time::sleep(DISPATCH_SWEEP_INTERVAL).await;
                    }
                }
            }
        })
    }

    /// Claim and execute at most one pending dispatch job inline (used by deterministic tests).
    pub async fn drain_dispatch_once(&self) -> Result<bool> {
        let attempt_id = format!("dsp_{}", crate::util::token());
        let Some(job) = self
            .store()
            .store()
            .write(|w| bot_dispatch::claim_next_dispatch(w, &attempt_id))?
        else {
            return Ok(false);
        };
        self.run_dispatch_job_supervised(job).await;
        Ok(true)
    }

    /// Supervise one claimed dispatch job so even a task panic releases the `running` row,
    /// records `DispatchFailed`, and opens/updates the `no_executor` card.
    pub(crate) async fn run_dispatch_job_supervised(&self, job: DispatchJob) {
        let started = Instant::now();
        let job_for_task = job.clone();
        let this = self.clone();
        let handle = tokio::spawn(async move {
            this.run_dispatch_job(job_for_task).await;
        });
        if let Err(join_err) = handle.await {
            let attempt_id = job.attempt_id.as_deref().unwrap_or("");
            let detail = if join_err.is_panic() {
                let panic_msg = join_err
                    .into_panic()
                    .downcast::<String>()
                    .map(|s| *s)
                    .or_else(|p| p.downcast::<&'static str>().map(|s| (*s).to_owned()))
                    .unwrap_or_else(|_| "dispatch task panicked".to_owned());
                tracing::error!(item = %job.item_id, "dispatch: job panicked: {panic_msg}");
                Some(js_head(&panic_msg, MAX_DISPATCH_DETAIL_CHARS).to_owned())
            } else {
                tracing::warn!(item = %job.item_id, "dispatch: job task cancelled");
                Some("dispatch task cancelled".to_owned())
            };
            self.abort_dispatch_job(&job, attempt_id, DispatchErrorCode::InternalPanic, detail);
            record_job_metrics(started, "failed");
        }
    }

    fn abort_dispatch_job(
        &self,
        job: &DispatchJob,
        attempt_id: &str,
        error: DispatchErrorCode,
        detail: Option<String>,
    ) {
        let mut tx = self.tx();
        let db = tx.db().clone();
        let store = db.store().clone();
        let ws = job.workspace_id.as_str();
        let item_id = job.item_id.as_str();
        let constraints = job.constraints();
        let aborted = store.write::<_, CoreError>(|w| {
            if !bot_dispatch::finish_dispatch_if_matching(w, ws, item_id, attempt_id)? {
                return Ok(None);
            }
            let Some(item) = db.item(item_id)? else {
                return Ok(None);
            };
            if item.is_closed() || item.archived || item.bot_id.is_some() {
                return Ok(None);
            }
            let blocked = PlacementBlocked {
                reason: PlacementRejection::DispatchFailed,
                rejected: vec![],
            };
            bot_dispatch::record_wait(w, ws, item_id, &blocked, None, &constraints)?;
            record_dispatch_timeline(
                &db,
                item_id,
                "steve",
                &DispatchEvent::Failed {
                    error,
                    detail: detail.clone(),
                },
            )?;
            let paused_wf = Tx::block_wf_dispatching_child_in(
                w,
                item_id,
                PlacementRejection::DispatchFailed.as_str(),
            )?;
            Ok(Some(paused_wf))
        });
        match aborted {
            Ok(Some(paused_wf)) => {
                if paused_wf {
                    crate::workflow_run::update_active_runs_gauge(&store);
                }
                if let Err(e) = tx.open_or_update_no_executor_card(ws) {
                    tracing::warn!(
                        item = %item_id,
                        "dispatch: failed to open no_executor card after abort: {e}"
                    );
                }
            }
            Ok(None) => {}
            Err(e) => {
                tracing::error!(item = %item_id, "dispatch: abort cleanup failed: {e}");
            }
        }
    }

    pub(crate) async fn run_dispatch_job(&self, job: DispatchJob) {
        let attempt_id = job.attempt_id.as_deref().unwrap_or("");
        let started = Instant::now();
        let (cancel, _cancel_guard) = self.register_dispatch_cancel(&job.item_id, attempt_id);

        let prepared = {
            let mut tx = self.tx();
            tx.prepare_dispatch_job(&job, attempt_id)
        };
        let Some(prepared) = (match prepared {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(item = %job.item_id, "dispatch: prepare failed: {e}");
                self.abort_dispatch_job(
                    &job,
                    attempt_id,
                    DispatchErrorCode::PrepareFailed,
                    Some(js_head(&e.to_string(), MAX_DISPATCH_DETAIL_CHARS).to_owned()),
                );
                record_job_metrics(started, "failed");
                return;
            }
        }) else {
            return;
        };

        let gw_res = tokio::select! {
            biased;
            _ = cancel.notified() => {
                tracing::debug!(item = %job.item_id, attempt = attempt_id, "dispatch: cancelled in flight");
                record_job_metrics(started, "cancelled");
                return;
            }
            res = crate::stats::tagged(
                "dispatch",
                &job.workspace_id,
                self.gw().call(
                    &prepared.prompt,
                    Tier::Smart,
                    DISPATCH_TIMEOUT,
                    &ThinkTools::Dispatch,
                ),
            ) => res,
        };

        let eval = match gw_res {
            Ok(printed) => match parse_dispatch_choice(&printed.text) {
                Some(choice) => DispatchEval::Selected(choice),
                None => {
                    let excerpt = js_head(printed.text.trim(), MAX_INVALID_RESPONSE_EXCERPT_CHARS);
                    DispatchEval::Failed {
                        error: DispatchErrorCode::InvalidResponse,
                        detail: (!excerpt.is_empty()).then(|| excerpt.to_owned()),
                    }
                }
            },
            Err(e) => DispatchEval::Failed {
                error: DispatchErrorCode::ModelCallFailed,
                detail: Some(js_head(&e.to_string(), MAX_DISPATCH_DETAIL_CHARS).to_owned()),
            },
        };

        let outcome = {
            let mut tx = self.tx();
            tx.settle_dispatch_job(&job, attempt_id, eval)
        };
        match outcome {
            Ok(Some(label)) => record_job_metrics(started, label),
            Ok(None) => {}
            Err(e) => {
                tracing::warn!(item = %job.item_id, "dispatch: settle failed: {e}");
                self.abort_dispatch_job(
                    &job,
                    attempt_id,
                    DispatchErrorCode::SettleFailed,
                    Some(js_head(&e.to_string(), MAX_DISPATCH_DETAIL_CHARS).to_owned()),
                );
                record_job_metrics(started, "failed");
            }
        }
    }
}

impl Tx<'_> {
    fn prepare_dispatch_job(
        &mut self,
        job: &DispatchJob,
        attempt_id: &str,
    ) -> Result<Option<PreparedDispatch>> {
        let db = self.db().clone();
        let ws = job.workspace_id.as_str();
        let Some(item) = db.item(&job.item_id)? else {
            let _ = db.store().write(|w| {
                bot_dispatch::finish_dispatch_if_matching(w, ws, &job.item_id, attempt_id)
            });
            return Ok(None);
        };
        let propose_open = db
            .decisions_of_item(&item.id)?
            .into_iter()
            .any(|d| d.kind == "propose" && d.status == DecisionStatus::Open);
        if item.is_closed()
            || item.archived
            || item.bot_id.is_some()
            || propose_open
            || self.active_session(&item.id)?.is_some()
        {
            let _ = db.store().write(|w| {
                bot_dispatch::finish_dispatch_if_matching(w, ws, &job.item_id, attempt_id)
            });
            return Ok(None);
        }
        let (team, goal, tags, node_tag, project_name) = db.store().read::<_, CoreError>(|r| {
            let (team, goal, tags, _) = Self::item_placement_inputs(r, &db, &item)?;
            let node_tag = steve_store::nodes::item_tag(r, &item.id)?;
            let project_name = match item.project_id.as_deref() {
                Some(pid) => db.project(Some(pid))?.map(|p| p.name),
                None => None,
            };
            Ok((team, goal, tags, node_tag, project_name))
        })?;
        let prompt = dispatch_prompt(&DispatchPromptInput {
            title: &item.title,
            goal: &goal,
            tags: &tags,
            project_id: item.project_id.as_deref(),
            project_name: project_name.as_deref(),
            team: team.as_ref(),
            required_node: job.required_node.as_deref(),
            node_tag: node_tag.as_deref(),
            required_specialties: &job.required_specialties,
        });
        Ok(Some(PreparedDispatch { prompt }))
    }

    fn settle_dispatch_job(
        &mut self,
        job: &DispatchJob,
        attempt_id: &str,
        eval: DispatchEval,
    ) -> Result<Option<&'static str>> {
        let db = self.db().clone();
        let store = db.store().clone();
        let ws = job.workspace_id.as_str();
        let item_id = job.item_id.as_str();
        let constraints = job.constraints();
        let default_model = |rt_id: &str| self.c.models().agent_model(ws, rt_id);

        enum PostAction {
            Stale,
            Placed {
                outcome: &'static str,
                deferred_by_setting: bool,
            },
            Blocked {
                outcome: &'static str,
                paused_wf: bool,
            },
        }

        let action = store.write::<_, CoreError>(|w| {
            // Atomic fence: if the owner reassigned, archived, or closed the item while Steve was
            // thinking, the dispatch row was already removed or replaced, so discard late output.
            if !bot_dispatch::finish_dispatch_if_matching(w, ws, item_id, attempt_id)? {
                return Ok(PostAction::Stale);
            }
            let Some(item) = db.item(item_id)? else {
                return Ok(PostAction::Stale);
            };
            if item.is_closed()
                || item.archived
                || item.bot_id.is_some()
                || self.active_session(item_id)?.is_some()
            {
                return Ok(PostAction::Stale);
            }
            let (team, _goal, _tags, required_nodes) = Self::item_placement_inputs(w, &db, &item)?;
            let project_opt = if !job.project_explicit && item.project_id.is_none() {
                None
            } else {
                Some(item.project_id.clone())
            };

            match eval {
                DispatchEval::Failed { error, detail } => {
                    let blocked = PlacementBlocked {
                        reason: PlacementRejection::DispatchFailed,
                        rejected: vec![],
                    };
                    bot_dispatch::record_wait(w, ws, item_id, &blocked, None, &constraints)?;
                    record_dispatch_timeline(
                        &db,
                        item_id,
                        "steve",
                        &DispatchEvent::Failed { error, detail },
                    )?;
                    let paused_wf = Self::block_wf_dispatching_child_in(
                        w,
                        item_id,
                        PlacementRejection::DispatchFailed.as_str(),
                    )?;
                    Ok(PostAction::Blocked {
                        outcome: "failed",
                        paused_wf,
                    })
                }
                DispatchEval::Selected(DispatchChoice { bot: None, why }) => {
                    let blocked = PlacementBlocked {
                        reason: PlacementRejection::NoCandidates,
                        rejected: vec![],
                    };
                    let policy = bot_dispatch::no_executor_policy(w, ws)?;
                    metrics::counter!(
                        "steve_dispatch_no_executor_total",
                        "policy" => policy.as_str(),
                        "reason" => blocked.reason.as_str()
                    )
                    .increment(1);
                    if policy == NoExecutorPolicy::Steve {
                        let coord_req = crate::placement::DispatchRequest {
                            workspace: ws,
                            bot: None,
                            harness: None,
                            model: None,
                            effort: None,
                            team: team.as_ref(),
                            project: &project_opt,
                            required_specialties: &job.required_specialties,
                            node: job.required_node.as_deref(),
                            required_nodes: required_nodes.as_deref(),
                            allow_main: true,
                        };
                        match crate::placement::validate_coordinator(
                            &db,
                            self.rt(),
                            &coord_req,
                            default_model,
                            blocked.clone(),
                            CoordinatorDeferSource::Setting,
                        ) {
                            Ok((placement, resolved_project)) => {
                                let main_id = placement.explanation.bot.as_str().to_owned();
                                if bot_dispatch::initial(w, item_id)?.is_none() {
                                    bot_dispatch::record(w, ws, item_id, None, Ok(&placement))?;
                                }
                                if item.project_id.is_none()
                                    && let Some(ref proj) = resolved_project
                                {
                                    db.item_patch(
                                        item_id,
                                        patch![
                                            bot_id: main_id.as_str(),
                                            project_id: Some(proj.id.as_str())
                                        ],
                                    )?;
                                } else {
                                    db.item_patch(item_id, patch![bot_id: main_id.as_str()])?;
                                }
                                bot_dispatch::clear_wait(w, item_id)?;
                                Self::settle_wf_dispatching_child_in(w, item_id, &placement)?;
                                record_dispatch_timeline(
                                    &db,
                                    item_id,
                                    "steve",
                                    &DispatchEvent::CoordinatorDeferred {
                                        bot: main_id.into(),
                                    },
                                )?;
                                return Ok(PostAction::Placed {
                                    outcome: "selected",
                                    deferred_by_setting: true,
                                });
                            }
                            Err(CoreError::Placement(coord_blocked)) => {
                                bot_dispatch::record_wait(
                                    w,
                                    ws,
                                    item_id,
                                    &coord_blocked,
                                    None,
                                    &constraints,
                                )?;
                                let paused_wf = Self::block_wf_dispatching_child_in(
                                    w,
                                    item_id,
                                    coord_blocked.reason.as_str(),
                                )?;
                                return Ok(PostAction::Blocked {
                                    outcome: "failed",
                                    paused_wf,
                                });
                            }
                            Err(e) => return Err(e),
                        }
                    }
                    bot_dispatch::record_wait(w, ws, item_id, &blocked, None, &constraints)?;
                    record_dispatch_timeline(
                        &db,
                        item_id,
                        "steve",
                        &DispatchEvent::NoCandidates { why },
                    )?;
                    let paused_wf =
                        Self::block_wf_dispatching_child_in(w, item_id, blocked.reason.as_str())?;
                    Ok(PostAction::Blocked {
                        outcome: "failed",
                        paused_wf,
                    })
                }
                DispatchEval::Selected(DispatchChoice {
                    bot: Some(chosen),
                    why,
                }) => {
                    let validated = crate::placement::validate(
                        &db,
                        self.rt(),
                        &crate::placement::DispatchRequest {
                            workspace: ws,
                            bot: Some(&chosen),
                            harness: None,
                            model: None,
                            effort: None,
                            team: team.as_ref(),
                            project: &project_opt,
                            required_specialties: &job.required_specialties,
                            node: job.required_node.as_deref(),
                            required_nodes: required_nodes.as_deref(),
                            allow_main: false,
                        },
                        default_model,
                        PlacementReason::SteveSelected,
                        why.clone(),
                    );
                    match validated {
                        Ok((placement, resolved_project)) => {
                            let bot_id = placement.explanation.bot.as_str().to_owned();
                            if bot_dispatch::initial(w, item_id)?.is_none() {
                                bot_dispatch::record(w, ws, item_id, None, Ok(&placement))?;
                            }
                            if item.project_id.is_none()
                                && let Some(ref proj) = resolved_project
                            {
                                db.item_patch(
                                    item_id,
                                    patch![
                                        bot_id: bot_id.as_str(),
                                        project_id: Some(proj.id.as_str())
                                    ],
                                )?;
                            } else {
                                db.item_patch(item_id, patch![bot_id: bot_id.as_str()])?;
                            }
                            bot_dispatch::clear_wait(w, item_id)?;
                            Self::settle_wf_dispatching_child_in(w, item_id, &placement)?;
                            record_dispatch_timeline(
                                &db,
                                item_id,
                                "steve",
                                &DispatchEvent::Selected {
                                    bot: bot_id.into(),
                                    why: placement.explanation.why.clone(),
                                },
                            )?;
                            Ok(PostAction::Placed {
                                outcome: "selected",
                                deferred_by_setting: false,
                            })
                        }
                        Err(CoreError::Placement(blocked)) => {
                            record_invalid_choice(blocked.reason);
                            let wait_blocked = PlacementBlocked {
                                reason: PlacementRejection::InvalidChoice,
                                rejected: vec![PlacementBlocker {
                                    bot: Some(chosen.clone().into()),
                                    reason: blocked.reason,
                                }],
                            };
                            bot_dispatch::record_wait(
                                w,
                                ws,
                                item_id,
                                &wait_blocked,
                                None,
                                &constraints,
                            )?;
                            record_dispatch_timeline(
                                &db,
                                item_id,
                                "steve",
                                &DispatchEvent::InvalidChoice {
                                    bot: chosen.into(),
                                    reason: blocked.reason,
                                },
                            )?;
                            let paused_wf = Self::block_wf_dispatching_child_in(
                                w,
                                item_id,
                                PlacementRejection::InvalidChoice.as_str(),
                            )?;
                            Ok(PostAction::Blocked {
                                outcome: "invalid_choice",
                                paused_wf,
                            })
                        }
                        Err(e) => Err(e),
                    }
                }
            }
        })?;

        match action {
            PostAction::Stale => Ok(None),
            PostAction::Placed {
                outcome,
                deferred_by_setting,
            } => {
                if deferred_by_setting {
                    metrics::counter!(
                        "steve_coordinator_self_executions_total",
                        "source" => CoordinatorDeferSource::Setting.as_str()
                    )
                    .increment(1);
                }
                self.reconcile_no_executor_card(ws)?;
                if self.dependency_waiting(item_id)? {
                    self.check_blocked(item_id)?;
                } else {
                    self.start_waiting_item(item_id)?;
                }
                Ok(Some(outcome))
            }
            PostAction::Blocked { outcome, paused_wf } => {
                if paused_wf {
                    crate::workflow_run::update_active_runs_gauge(&store);
                }
                self.open_or_update_no_executor_card(ws)?;
                Ok(Some(outcome))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use steve_modules::Caller;
    use steve_store::{BotPatch, NewBot, Views, WS, bot_dispatch};
    use steve_types::{
        DecisionPrompt, DecisionStatus, ItemWait, NO_EXECUTOR, PlacementReason, PlacementRejection,
    };

    use crate::{CreateItem, DecisionCommand, testkit::Kit};

    fn seed_extra_bot(k: &Kit, id: &str, name: &str, enabled: bool) {
        k.db()
            .store()
            .bots()
            .seed(
                id,
                false,
                &NewBot {
                    name: name.into(),
                    harness: "fixture".into(),
                    enabled: Some(enabled),
                    ..Default::default()
                },
                WS,
            )
            .unwrap();
    }

    fn open_no_executor(k: &Kit) -> Vec<crate::repo::Decision> {
        k.db()
            .store()
            .decisions()
            .open(WS)
            .unwrap()
            .into_iter()
            .filter(|d| d.subject_type == NO_EXECUTOR && d.status == DecisionStatus::Open)
            .collect()
    }

    #[tokio::test]
    async fn single_eligible_and_explicit_bot_avoid_model_calls() {
        let k = Kit::new();
        k.gw.set(|_, _| panic!("must not call model for explicit or single-eligible dispatch"));

        // Only 1 non-main Bot (`dev`) exists -> placed immediately with OnlyEligible.
        let solo = k
            .core
            .create_item(CreateItem {
                ws: WS.into(),
                title: "Solo work".into(),
                goal: "Implement solo task".into(),
                project: Some(None),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(solo.bot_id.as_deref(), Some("dev"));
        let solo_fact = k
            .db()
            .store()
            .read(|r| bot_dispatch::initial(r, &solo.id))
            .unwrap()
            .unwrap();
        assert_eq!(solo_fact.explanation.reason, PlacementReason::OnlyEligible);
        assert!(k.gw.calls.lock().unwrap().is_empty());

        // Add a second Bot, then create with explicit Bot -> placed immediately with Explicit, 0 model calls.
        seed_extra_bot(&k, "review", "Review", true);
        let explicit = k
            .core
            .create_item(CreateItem {
                ws: WS.into(),
                title: "Explicit work".into(),
                goal: "Review PR".into(),
                bot: Some("review".into()),
                project: Some(None),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(explicit.bot_id.as_deref(), Some("review"));
        let exp_fact = k
            .db()
            .store()
            .read(|r| bot_dispatch::initial(r, &explicit.id))
            .unwrap()
            .unwrap();
        assert_eq!(exp_fact.explanation.reason, PlacementReason::Explicit);
        assert!(k.gw.calls.lock().unwrap().is_empty());
    }

    #[tokio::test]
    async fn multi_candidate_dispatch_runs_async_steve_choice_and_starts_session() {
        let k = Kit::new();
        seed_extra_bot(&k, "review", "Review", true);
        k.gw.set(|_, _| {
            Ok(
                r#"{"bot":"review","why":"Review Bot specializes in PR audits and is idle."}"#
                    .into(),
            )
        });

        let item = k
            .core
            .create_item(CreateItem {
                ws: WS.into(),
                title: "Audit auth changes".into(),
                goal: "Review session token validation".into(),
                project: Some(None),
                opening: Some("Please audit the auth diff.".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(item.bot_id, None);
        assert!(k.db().sessions_of_item(&item.id).unwrap().is_empty());

        // ItemView projects ItemWait::Dispatching while async dispatch is pending.
        let view = k
            .db()
            .store()
            .read(|r| r.item_views().of(WS, std::slice::from_ref(&item.id)))
            .unwrap()
            .pop()
            .unwrap();
        assert!(view.waits.contains(&ItemWait::Dispatching), "{view:?}");

        assert!(k.core.drain_dispatch_once().await.unwrap());
        assert_eq!(k.gw.calls.lock().unwrap().len(), 1);

        let updated = k.db().item(&item.id).unwrap().unwrap();
        assert_eq!(updated.bot_id.as_deref(), Some("review"));
        let fact = k
            .db()
            .store()
            .read(|r| bot_dispatch::initial(r, &item.id))
            .unwrap()
            .unwrap();
        assert_eq!(fact.explanation.bot.as_str(), "review");
        assert_eq!(fact.explanation.reason, PlacementReason::SteveSelected);
        assert_eq!(
            fact.explanation.why.as_deref(),
            Some("Review Bot specializes in PR audits and is idle.")
        );
        let sessions = k.db().sessions_of_item(&item.id).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].bot_id.as_deref(), Some("review"));

        let settled_view = k
            .db()
            .store()
            .read(|r| r.item_views().of(WS, std::slice::from_ref(&item.id)))
            .unwrap()
            .pop()
            .unwrap();
        assert!(!settled_view.waits.contains(&ItemWait::Dispatching));
    }

    #[tokio::test]
    async fn invalid_steve_choice_is_rejected_honestly_without_silent_swap() {
        let k = Kit::new();
        seed_extra_bot(&k, "review", "Review", true);
        seed_extra_bot(&k, "off", "Disabled", false);
        k.gw.set(|_, _| Ok(r#"{"bot":"off","why":"Picked disabled bot"}"#.into()));

        let item = k
            .core
            .create_item(CreateItem {
                ws: WS.into(),
                title: "Check error handling".into(),
                goal: "Investigate timeout".into(),
                project: Some(None),
                ..Default::default()
            })
            .unwrap();
        assert!(k.core.drain_dispatch_once().await.unwrap());

        let after = k.db().item(&item.id).unwrap().unwrap();
        assert_eq!(
            after.bot_id, None,
            "must never silently swap to another Bot"
        );
        assert!(k.db().sessions_of_item(&item.id).unwrap().is_empty());

        let cards = open_no_executor(&k);
        assert_eq!(cards.len(), 1);
        assert!(cards[0].options.contains(&"retry_dispatch".to_string()));
        let Some(DecisionPrompt::NoExecutor {
            reason, rejected, ..
        }) = &cards[0].question_template
        else {
            panic!("expected NoExecutor prompt");
        };
        assert_eq!(*reason, PlacementRejection::InvalidChoice);
        assert!(rejected.iter().any(|b| {
            b.bot.as_ref().map(|id| id.as_str()) == Some("off")
                && b.reason == PlacementRejection::Bot
        }));
    }

    #[tokio::test]
    async fn dispatch_failure_opens_card_and_retry_dispatch_recovers() {
        let k = Kit::new();
        steve_store::testkit::hub::mutations::seed_users(k.db().store()).unwrap();
        let owner = Caller {
            ws: WS.into(),
            me: "u".into(),
        };
        seed_extra_bot(&k, "review", "Review", true);
        k.gw.set(|_, _| Err(crate::CoreError::Msg("local node unavailable".into())));

        let item = k
            .core
            .create_item(CreateItem {
                ws: WS.into(),
                title: "Retry dispatch task".into(),
                goal: "Check retry path".into(),
                project: Some(None),
                ..Default::default()
            })
            .unwrap();
        assert!(k.core.drain_dispatch_once().await.unwrap());
        assert_eq!(k.db().item(&item.id).unwrap().unwrap().bot_id, None);
        let cards = open_no_executor(&k);
        assert_eq!(cards.len(), 1);
        let Some(DecisionPrompt::NoExecutor { reason, .. }) = &cards[0].question_template else {
            panic!("expected NoExecutor prompt");
        };
        assert_eq!(*reason, PlacementRejection::DispatchFailed);

        // Fix the model backend and trigger retry_dispatch from the no_executor card.
        k.gw.set(|_, _| Ok(r#"{"bot":"dev","why":"Dev Bot is ready"}"#.into()));
        let resolved = k
            .core
            .decision_command(
                &owner,
                &cards[0].id,
                Some(cards[0].revision),
                DecisionCommand::Resolve {
                    choice: "retry_dispatch".into(),
                    text: String::new(),
                },
            )
            .unwrap();
        assert_eq!(resolved.status, DecisionStatus::Resolved);
        assert!(k.core.drain_dispatch_once().await.unwrap());
        assert_eq!(
            k.db().item(&item.id).unwrap().unwrap().bot_id.as_deref(),
            Some("dev")
        );
        assert_eq!(k.db().sessions_of_item(&item.id).unwrap().len(), 1);
    }

    #[tokio::test]
    async fn owner_reassign_cancels_inflight_dispatch_so_stale_choice_is_discarded() {
        let k = Kit::new();
        seed_extra_bot(&k, "review", "Review", true);
        let item = k
            .core
            .create_item(CreateItem {
                ws: WS.into(),
                title: "Race test".into(),
                goal: "Owner reassigns before Steve finishes".into(),
                project: Some(None),
                ..Default::default()
            })
            .unwrap();
        // Claim job so it is in-flight (`running`).
        let job = k
            .db()
            .store()
            .write(|w| bot_dispatch::claim_next_dispatch(w, "dsp_inflight"))
            .unwrap()
            .expect("claimed");
        // Owner manually reassigns to `dev` while the job is in flight.
        k.core.reassign(&item.id, "dev", "u").unwrap();
        assert_eq!(
            k.db().item(&item.id).unwrap().unwrap().bot_id.as_deref(),
            Some("dev")
        );
        // Late model result picks `review`; settlement must discard it and keep `dev`.
        k.gw.set(|_, _| Ok(r#"{"bot":"review","why":"Late choice"}"#.into()));
        k.core.run_dispatch_job(job).await;
        assert_eq!(
            k.db().item(&item.id).unwrap().unwrap().bot_id.as_deref(),
            Some("dev")
        );
        // Also ensure disabling a bot leaves only 1 candidate on retry -> OnlyEligible immediately.
        k.db()
            .store()
            .bots()
            .patch(
                "review",
                &BotPatch {
                    enabled: Some(false),
                    ..Default::default()
                },
                "u",
            )
            .unwrap();
    }

    #[tokio::test]
    async fn dispatching_item_blocks_start_defers_supplement_and_ignores_resume_and_deps() {
        let k = Kit::new();
        seed_extra_bot(&k, "review", "Review", true);
        let item = k
            .core
            .create_item(CreateItem {
                ws: WS.into(),
                title: "Wait on Steve".into(),
                goal: "Do not fall back to main".into(),
                project: Some(None),
                opening: Some("initial brief".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(item.bot_id, None);
        assert!(
            k.db()
                .store()
                .read(|r| bot_dispatch::is_dispatching(r, &item.id))
                .unwrap()
        );

        // Unplaced reply during dispatch says Steve is selecting, not blocked.
        let reply = k.core.unplaced_reply(&item).expect("unplaced reply");
        assert!(
            reply.contains(&item.id)
                && reply == steve_i18n::t!("core.thread.dispatching_execution_bot", id = item.id),
            "{reply}"
        );

        // Direct start_session_with must fail with Pending and never start on main_bot.
        let err = k.core.tx().start_session(&item.id, None, None).unwrap_err();
        assert!(matches!(
            err,
            crate::CoreError::Placement(steve_types::PlacementBlocked {
                reason: PlacementRejection::Pending,
                ..
            })
        ));
        assert!(k.db().sessions_of_item(&item.id).unwrap().is_empty());

        // Supplement while dispatching must not open a session on main_bot, and must be preserved.
        k.core
            .supplement(
                &item.id,
                "extra owner hint",
                crate::SupplementMode::Auto,
                &[],
                "u",
            )
            .unwrap();
        assert!(k.db().sessions_of_item(&item.id).unwrap().is_empty());

        // Pause + Resume while dispatching must not open a session on main_bot.
        k.core
            .item_action(&item.id, crate::ItemAction::Pause, "u")
            .unwrap();
        k.core
            .item_action(&item.id, crate::ItemAction::Resume, "u")
            .unwrap();
        assert!(k.db().sessions_of_item(&item.id).unwrap().is_empty());

        // Now settle dispatch -> starts on review with both initial brief and supplement.
        k.gw.set(|_, _| Ok(r#"{"bot":"review","why":"best fit"}"#.into()));
        assert!(k.core.drain_dispatch_once().await.unwrap());
        let sessions = k.db().sessions_of_item(&item.id).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].bot_id.as_deref(), Some("review"));
        let prompt_text =
            k.rt.st()
                .prompts
                .iter()
                .map(|(_, text, _)| text.as_str())
                .collect::<Vec<_>>()
                .join("\n");
        assert!(prompt_text.contains("initial brief"), "{prompt_text}");
        assert!(prompt_text.contains("extra owner hint"), "{prompt_text}");
    }

    #[tokio::test]
    async fn dispatch_job_panic_releases_running_row_and_opens_card() {
        let k = Kit::new();
        seed_extra_bot(&k, "review", "Review", true);
        k.gw.set(|_, _| panic!("simulated gateway panic"));

        let item = k
            .core
            .create_item(CreateItem {
                ws: WS.into(),
                title: "Panic recovery".into(),
                goal: "Must never stay stuck dispatching".into(),
                project: Some(None),
                ..Default::default()
            })
            .unwrap();
        assert!(k.core.drain_dispatch_once().await.unwrap());
        assert!(
            !k.db()
                .store()
                .read(|r| bot_dispatch::is_dispatching(r, &item.id))
                .unwrap(),
            "running row must be released on panic"
        );
        let cards = open_no_executor(&k);
        assert_eq!(cards.len(), 1);
        let Some(DecisionPrompt::NoExecutor { reason, .. }) = &cards[0].question_template else {
            panic!("expected NoExecutor prompt");
        };
        assert_eq!(*reason, PlacementRejection::DispatchFailed);

        // Structured DispatchEvent recorded in timeline under kind = "dispatch" (no hardcoded Chinese).
        let tl = k.db().timeline(&item.id).unwrap();
        let dispatch_events: Vec<steve_types::DispatchEvent> = tl
            .iter()
            .filter(|e| e.kind == "dispatch")
            .map(|e| serde_json::from_str(&e.text).expect("valid DispatchEvent JSON"))
            .collect();
        assert!(
            dispatch_events
                .iter()
                .any(|ev| matches!(ev, steve_types::DispatchEvent::Queued))
        );
        assert!(dispatch_events.iter().any(|ev| matches!(
            ev,
            steve_types::DispatchEvent::Failed {
                error: steve_types::DispatchErrorCode::InternalPanic,
                ..
            }
        )));
    }

    #[tokio::test]
    async fn async_dispatch_and_retry_preserve_constraints_and_backfill_default_project() {
        let k = Kit::new();
        steve_store::testkit::hub::mutations::seed_users(k.db().store()).unwrap();
        let owner = Caller {
            ws: WS.into(),
            me: "u".into(),
        };
        let dir = tempfile::tempdir().unwrap();
        let proj = k
            .db()
            .store()
            .projects()
            .create(
                WS,
                &steve_store::NewProject {
                    name: "Backend".into(),
                    description: "backend".into(),
                    device_id: "hub".into(),
                    path: dir.path().display().to_string(),
                    kind: "dir".into(),
                    worktree: false,
                    default_bot: None,
                    release: None,
                },
            )
            .unwrap();
        k.db()
            .store()
            .bots()
            .patch(
                "dev",
                &BotPatch {
                    specialties: Some(vec!["rust".into()]),
                    ..Default::default()
                },
                "u",
            )
            .unwrap();
        k.db()
            .store()
            .bots()
            .seed(
                "review",
                false,
                &NewBot {
                    name: "Review".into(),
                    harness: "fixture".into(),
                    specialties: Some(vec!["rust".into()]),
                    default_project: Some(proj.id.clone()),
                    ..Default::default()
                },
                WS,
            )
            .unwrap();

        // First attempt fails -> constraints must persist in item_placement_waits.
        k.gw.set(|_, _| Err(crate::CoreError::Msg("temporary failure".into())));
        let item = k
            .core
            .create_item(CreateItem {
                ws: WS.into(),
                title: "Rust migration".into(),
                goal: "Check constraint preservation".into(),
                project: None,
                node: Some("local".into()),
                required_specialties: vec!["rust".into()],
                ..Default::default()
            })
            .unwrap();
        assert_eq!(item.project_id, None);
        assert!(k.core.drain_dispatch_once().await.unwrap());

        let waits = k
            .db()
            .store()
            .read(|r| bot_dispatch::waiting_items(r, WS))
            .unwrap();
        assert_eq!(waits.len(), 1);
        assert_eq!(waits[0].constraints.required_node.as_deref(), Some("local"));
        assert_eq!(waits[0].constraints.required_specialties, vec!["rust"]);
        assert!(!waits[0].constraints.project_explicit);

        // Retry dispatch -> re-enqueues with the exact same constraints and backfills default_project.
        let card = open_no_executor(&k).pop().unwrap();
        k.gw.set(|_, _| Ok(r#"{"bot":"review","why":"owns backend"}"#.into()));
        k.core
            .decision_command(
                &owner,
                &card.id,
                Some(card.revision),
                DecisionCommand::Resolve {
                    choice: "retry_dispatch".into(),
                    text: String::new(),
                },
            )
            .unwrap();

        let job = k
            .db()
            .store()
            .write(|w| bot_dispatch::claim_next_dispatch(w, "dsp_retry_check"))
            .unwrap()
            .expect("re-enqueued dispatch job");
        assert_eq!(job.required_node.as_deref(), Some("local"));
        assert_eq!(job.required_specialties, vec!["rust"]);
        assert!(!job.project_explicit);

        k.core.run_dispatch_job_supervised(job).await;
        let settled = k.db().item(&item.id).unwrap().unwrap();
        assert_eq!(settled.bot_id.as_deref(), Some("review"));
        assert_eq!(
            settled.project_id.as_deref(),
            Some(proj.id.as_str()),
            "omitted project must backfill chosen Bot's default_project"
        );
    }

    #[tokio::test]
    async fn inflight_dispatch_is_cancelled_immediately_on_reassign() {
        use async_trait::async_trait;
        use std::sync::{
            Arc, Mutex, RwLock,
            atomic::{AtomicBool, Ordering},
        };
        use std::time::Duration;
        use tokio::sync::Notify;

        struct HangingGw {
            started: Arc<Notify>,
            dropped: Arc<AtomicBool>,
        }
        struct DropFlag(Arc<AtomicBool>);
        impl Drop for DropFlag {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        #[async_trait]
        impl crate::gateway::ModelGateway for HangingGw {
            async fn call(
                &self,
                _: &str,
                _: crate::gateway::Tier,
                _: Duration,
                _: &crate::gateway::ThinkTools,
            ) -> crate::Result<crate::gateway::Printed> {
                let _flag = DropFlag(self.dropped.clone());
                self.started.notify_one();
                std::future::pending::<()>().await;
                unreachable!()
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let store =
            steve_store::Store::open(dir.path().join("steve.sqlite3"), steve_store::NoPublisher)
                .unwrap();
        store.ensure_workspace().unwrap();
        let db = crate::repo::Db::new(store);
        let rt = Arc::new(crate::testkit::FakeRuntime {
            store: db.clone(),
            s: Mutex::new(Default::default()),
            flushed: Notify::new(),
            stop_gate: RwLock::new(()),
        });
        let started = Arc::new(Notify::new());
        let dropped = Arc::new(AtomicBool::new(false));
        let gw = Arc::new(HangingGw {
            started: started.clone(),
            dropped: dropped.clone(),
        });
        let core = crate::Core::new(db.clone(), rt, gw, crate::CoreConfig::for_home(dir.path()));
        for (id, name) in [("dev", "Dev"), ("review", "Review")] {
            db.store()
                .bots()
                .seed(
                    id,
                    false,
                    &NewBot {
                        name: name.into(),
                        harness: "fixture".into(),
                        ..Default::default()
                    },
                    WS,
                )
                .unwrap();
        }

        let item = core
            .create_item(CreateItem {
                ws: WS.into(),
                title: "Cancel in flight".into(),
                goal: "Owner takes over immediately".into(),
                project: Some(None),
                ..Default::default()
            })
            .unwrap();
        let job = db
            .store()
            .write(|w| bot_dispatch::claim_next_dispatch(w, "dsp_hang"))
            .unwrap()
            .expect("claimed");
        let core_bg = core.clone();
        let task = tokio::spawn(async move {
            core_bg.run_dispatch_job_supervised(job).await;
        });
        started.notified().await;
        core.reassign(&item.id, "dev", "u").unwrap();
        tokio::time::timeout(Duration::from_secs(2), task)
            .await
            .expect("in-flight dispatch task must cancel immediately")
            .unwrap();
        assert!(dropped.load(Ordering::SeqCst));
    }

    #[test]
    fn offer_meta_rejects_main_bot_choice() {
        let k = Kit::new();
        let meta = k
            .core
            .offer_meta(
                WS,
                &crate::gateway::Offer {
                    title: "Fix bug".into(),
                    goal: "Fix the bug".into(),
                    bot: Some("steve".into()),
                    why: Some("picked coordinator".into()),
                },
                None,
                &[],
            )
            .unwrap();
        assert_eq!(meta["bot"], "steve");
        assert_eq!(meta["rejected"], "outside_team");
        assert!(meta.get("reason").is_none());
    }

    #[tokio::test]
    async fn deferred_supplement_survives_dependency_wait_and_stores_raw_text() {
        let k = Kit::new();
        seed_extra_bot(&k, "review", "Review", true);
        let prereq = k
            .core
            .create_item(CreateItem {
                ws: WS.into(),
                title: "Prereq".into(),
                goal: "Finish first".into(),
                bot: Some("dev".into()),
                project: Some(None),
                ..Default::default()
            })
            .unwrap();
        let dependent = k
            .core
            .create_item(CreateItem {
                ws: WS.into(),
                title: "Blocked by prereq".into(),
                goal: "Run after prereq with deferred supplement".into(),
                project: Some(None),
                deps: vec![prereq.id.clone()],
                opening: Some("initial dep brief".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(dependent.bot_id, None);
        assert!(
            k.db()
                .store()
                .read(|r| bot_dispatch::is_dispatching(r, &dependent.id))
                .unwrap()
        );

        let supp_file = crate::repo::FileRef {
            id: "fil_dep_supp".into(),
            name: "spec.md".into(),
            mime: "text/markdown".into(),
            size: 12,
        };
        k.core
            .supplement(
                &dependent.id,
                "extra dep hint",
                crate::SupplementMode::Auto,
                std::slice::from_ref(&supp_file),
                "u",
            )
            .unwrap();

        // Internal deferred_supplement entry stores raw text without hardcoded Chinese prefix.
        let tl = k.db().timeline(&dependent.id).unwrap();
        let deferred: Vec<_> = tl
            .iter()
            .filter(|e| e.kind == format!("deferred_supplement:{}", dependent.cycle))
            .collect();
        assert_eq!(deferred.len(), 1);
        assert_eq!(deferred[0].text, "extra dep hint");
        assert_eq!(deferred[0].files.len(), 1);
        assert_eq!(deferred[0].files[0].id, "fil_dep_supp");

        // Dispatch settles onto `review`, but prerequisite is still open so no session starts yet.
        k.gw.set(|_, _| Ok(r#"{"bot":"review","why":"best fit"}"#.into()));
        assert!(k.core.drain_dispatch_once().await.unwrap());
        assert_eq!(
            k.db()
                .item(&dependent.id)
                .unwrap()
                .unwrap()
                .bot_id
                .as_deref(),
            Some("review")
        );
        assert!(k.db().sessions_of_item(&dependent.id).unwrap().is_empty());

        // Complete the prerequisite -> lifecycle_tick runs after_cycle_closed -> check_blocked
        // starts `review` with both initial brief and deferred supplement text + attachment.
        let prereq_sessions = k.db().sessions_of_item(&prereq.id).unwrap();
        assert_eq!(prereq_sessions.len(), 1);
        k.rt.end_turn(&k.core, &prereq_sessions[0].id, "end_turn", None);
        let closed = k.finish(&prereq.id).unwrap();
        assert_eq!(closed.phase.as_str(), "closed");
        k.core.lifecycle_tick().unwrap();
        let sessions = k.db().sessions_of_item(&dependent.id).unwrap();
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].bot_id.as_deref(), Some("review"));
        let joined =
            k.rt.st()
                .prompts
                .iter()
                .filter(|(sid, _, _)| sid == &sessions[0].id)
                .map(|(_, text, _)| text.as_str())
                .collect::<Vec<_>>()
                .join("\n");
        assert!(joined.contains("initial dep brief"), "{joined}");
        assert!(joined.contains("补充信息：extra dep hint"), "{joined}");
        let dep_files: Vec<String> =
            k.rt.st()
                .files
                .iter()
                .filter(|(sid, _)| sid == &sessions[0].id)
                .flat_map(|(_, files)| files.iter().map(|f| f.id.clone()))
                .collect();
        assert!(
            dep_files.iter().any(|id| id == "fil_dep_supp"),
            "deferred supplement attachment must reach check_blocked session: {dep_files:?}"
        );
    }
}
