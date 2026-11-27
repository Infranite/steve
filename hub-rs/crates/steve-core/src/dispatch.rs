//! Asynchronous Steve Bot-selection worker for multi-candidate automatic item dispatch (#626).
//!
//! When automatic item creation finds >= 2 hard-eligible non-main Bots, the item commits
//! unassigned with a durable row in `item_dispatch_jobs`. This worker runs a short Think job
//! with `ThinkTools::Dispatch` (exposing only `steve_bots_list` and `steve_nodes_list`) so Steve
//! inspects real Bot/Node state and chooses the executor, then Hub hard-validates the choice
//! before starting the item's session. Failures never silently swap Bots or fall back to scoring.
use std::time::{Duration, Instant};

use steve_store::bot_dispatch::{self, DispatchJob};
use steve_types::{
    CoordinatorDeferSource, DecisionStatus, NoExecutorPolicy, PlacementBlocked, PlacementBlocker,
    PlacementReason, PlacementRejection,
};

use crate::error::{CoreError, Result};
use crate::gateway::{
    DISPATCH_TIMEOUT, DispatchChoice, DispatchPromptInput, ThinkTools, Tier, dispatch_prompt,
    parse_dispatch_choice,
};
use crate::patch;
use crate::repo::BotExt;
use crate::util::js_head;
use crate::{Core, Tx};

/// Maximum concurrent Steve Bot-selection Think jobs across workspaces.
pub const MAX_CONCURRENT_DISPATCHES: usize = 4;

/// Safety sweep interval so pending dispatch jobs are picked up even if a wake signal raced.
const DISPATCH_SWEEP_INTERVAL: Duration = Duration::from_secs(5);

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
    Failed(String),
}

struct PreparedDispatch {
    prompt: String,
}

impl Core {
    /// Wake the background Bot-dispatch worker after enqueueing a pending `item_dispatch_jobs` row.
    pub(crate) fn wake_dispatch(&self) {
        self.0.dispatch_wake.notify_one();
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
                            this.run_dispatch_job(job).await;
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
        self.run_dispatch_job(job).await;
        Ok(true)
    }

    pub(crate) async fn run_dispatch_job(&self, job: DispatchJob) {
        let attempt_id = job.attempt_id.as_deref().unwrap_or("");
        let started = Instant::now();
        let prepared = {
            let mut tx = self.tx();
            tx.prepare_dispatch_job(&job, attempt_id)
        };
        let Some(prepared) = (match prepared {
            Ok(p) => p,
            Err(e) => {
                tracing::warn!(item = %job.item_id, "dispatch: prepare failed: {e}");
                return;
            }
        }) else {
            return;
        };

        let gw_res = crate::stats::tagged(
            "dispatch",
            &job.workspace_id,
            self.gw().call(
                &prepared.prompt,
                Tier::Smart,
                DISPATCH_TIMEOUT,
                &ThinkTools::Dispatch,
            ),
        )
        .await;

        let eval = match gw_res {
            Ok(printed) => match parse_dispatch_choice(&printed.text) {
                Some(choice) => DispatchEval::Selected(choice),
                None => DispatchEval::Failed(format!(
                    "模型返回内容无法解析为派发 JSON：{}",
                    js_head(printed.text.trim(), 160)
                )),
            },
            Err(e) => DispatchEval::Failed(js_head(&e.to_string(), 200).to_owned()),
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
        if item.is_closed() || item.archived || item.bot_id.is_some() || propose_open {
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
            if item.is_closed() || item.archived || item.bot_id.is_some() {
                return Ok(PostAction::Stale);
            }
            let (team, _goal, _tags, required_nodes) = Self::item_placement_inputs(w, &db, &item)?;
            let project_opt = Some(item.project_id.clone());

            match eval {
                DispatchEval::Failed(detail) => {
                    let blocked = PlacementBlocked {
                        reason: PlacementRejection::DispatchFailed,
                        rejected: vec![],
                    };
                    bot_dispatch::record_wait(w, ws, item_id, &blocked, None)?;
                    db.timeline_add(
                        item_id,
                        "steve",
                        "sys",
                        &format!("Steve 未能完成执行 Bot 选择：{detail}"),
                        None,
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
                        };
                        match crate::placement::validate_coordinator(
                            &db,
                            self.rt(),
                            &coord_req,
                            default_model,
                            blocked.clone(),
                            CoordinatorDeferSource::Setting,
                        ) {
                            Ok((placement, _)) => {
                                let main_id = placement.explanation.bot.as_str().to_owned();
                                bot_dispatch::record(w, ws, item_id, None, Ok(&placement))?;
                                db.item_patch(item_id, patch![bot_id: main_id.as_str()])?;
                                bot_dispatch::clear_wait(w, item_id)?;
                                Self::settle_wf_dispatching_child_in(w, item_id, &placement)?;
                                let display =
                                    db.bot(&main_id)?.map(|b| b.display()).unwrap_or(main_id);
                                db.timeline_add(
                                    item_id,
                                    "steve",
                                    "sys",
                                    &format!("无合适执行 Bot，改由 {display} 自己执行"),
                                    None,
                                )?;
                                return Ok(PostAction::Placed {
                                    outcome: "selected",
                                    deferred_by_setting: true,
                                });
                            }
                            Err(CoreError::Placement(coord_blocked)) => {
                                bot_dispatch::record_wait(w, ws, item_id, &coord_blocked, None)?;
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
                    bot_dispatch::record_wait(w, ws, item_id, &blocked, None)?;
                    let note = match why {
                        Some(w_text) => format!("Steve 判断当前没有合适的执行 Bot：{w_text}"),
                        None => "Steve 判断当前没有合适的执行 Bot，等待你指定".to_owned(),
                    };
                    db.timeline_add(item_id, "steve", "sys", &note, None)?;
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
                    let is_main = db.bot(&chosen)?.is_some_and(|b| b.main);
                    let validated = if is_main {
                        Err(CoreError::Placement(PlacementBlocked {
                            reason: PlacementRejection::OutsideTeam,
                            rejected: vec![PlacementBlocker {
                                bot: Some(chosen.clone().into()),
                                reason: PlacementRejection::OutsideTeam,
                            }],
                        }))
                    } else {
                        crate::placement::validate(
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
                            },
                            default_model,
                            PlacementReason::SteveSelected,
                            why.clone(),
                        )
                    };
                    match validated {
                        Ok((placement, _)) => {
                            let bot_id = placement.explanation.bot.as_str().to_owned();
                            bot_dispatch::record(w, ws, item_id, None, Ok(&placement))?;
                            db.item_patch(item_id, patch![bot_id: bot_id.as_str()])?;
                            bot_dispatch::clear_wait(w, item_id)?;
                            Self::settle_wf_dispatching_child_in(w, item_id, &placement)?;
                            let display = db.bot(&bot_id)?.map(|b| b.display()).unwrap_or(bot_id);
                            let line = match placement.explanation.why.as_deref() {
                                Some(w_text) => format!("Steve 选择由 {display} 执行：{w_text}"),
                                None => format!("Steve 选择由 {display} 执行"),
                            };
                            db.timeline_add(item_id, "steve", "sys", &line, None)?;
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
                            bot_dispatch::record_wait(w, ws, item_id, &wait_blocked, None)?;
                            db.timeline_add(
                                item_id,
                                "steve",
                                "sys",
                                &format!(
                                    "Steve 选择的 Bot（{chosen}）未通过校验（{}），等待你处理",
                                    blocked.reason.as_str()
                                ),
                                None,
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
                self.start_waiting_item(item_id)?;
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
}
