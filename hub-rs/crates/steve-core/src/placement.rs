//! Hard-constraint Bot eligibility and validation.
//!
//! Hub never ranks or scores Bots by keyword matching. Hub computes the hard-eligible candidate
//! set ([`eligible`]), validates Steve's or the owner's chosen Bot ([`validate`], [`check_bot`]),
//! and validates coordinator self-execution ([`validate_coordinator`]) when no executor Bot exists.
use std::{collections::BTreeSet, time::Instant};

use steve_node_api::HarnessChoice;
use steve_store::{
    Bot, Project,
    bot_dispatch::{self, Scope, Team},
};
use steve_types::{
    BotPlacement, CoordinatorDeferSource, Millis, PlacementBlocked, PlacementBlocker,
    PlacementExplanation, PlacementReason, PlacementRejection as Rejection,
};

use crate::{AgentRuntime, CoreError, Result, repo::Db};

pub(crate) const RULE_VERSION: u32 = 3;

pub(crate) struct DispatchRequest<'a> {
    pub workspace: &'a str,
    pub bot: Option<&'a str>,
    /// Optional runtime/harness override (for handoff switching); `None` uses each Bot's configured harness.
    pub harness: Option<&'a str>,
    /// Optional model override (for handoff model switching); `None` uses Bot/workspace default for the chosen runtime.
    pub model: Option<&'a str>,
    /// Optional reasoning effort override (for handoff execution-surface switching).
    pub effort: Option<steve_types::ReasoningEffort>,
    pub team: Option<&'a Team>,
    pub project: &'a Option<Option<String>>,
    pub required_specialties: &'a [String],
    pub node: Option<&'a str>,
    pub required_nodes: Option<&'a [String]>,
    /// Whether the workspace coordinator (`main = true`) is permitted as an explicit choice.
    /// Steve-driven selection (OFFER `SteveSelected`, async dispatch) sets this to `false`.
    pub allow_main: bool,
}

pub(crate) struct EligibleBot {
    pub bot: Bot,
    pub project: Option<Project>,
    pub choice: HarnessChoice,
    pub node: String,
    pub priority: u32,
    pub skipped: Vec<Rejection>,
}

pub(crate) struct EligibleCandidates {
    pub candidates: Vec<EligibleBot>,
    pub rejected: Vec<PlacementBlocker>,
    pub primary_rejection: Option<Rejection>,
}

impl EligibleBot {
    pub(crate) fn into_placement(
        self,
        required_node: Option<&str>,
        reason: PlacementReason,
        why: Option<String>,
        mut rejected: Vec<PlacementBlocker>,
        fallback: Option<Rejection>,
        deferred_by: Option<CoordinatorDeferSource>,
    ) -> (BotPlacement, Option<Project>) {
        let bot_id = self.bot.id;
        rejected.extend(self.skipped.into_iter().map(|r| PlacementBlocker {
            bot: Some(bot_id.clone().into()),
            reason: r,
        }));
        let reasoning_effort = self.choice.effective_effort();
        (
            BotPlacement {
                rule_version: RULE_VERSION,
                bot_revision: self.bot.revision,
                at: Millis(steve_store::now()),
                node: self.node.into(),
                required_node: required_node.map(Into::into),
                node_priority: self.priority,
                harness: self.choice.harness,
                model: self.choice.model,
                reasoning_effort,
                explanation: PlacementExplanation {
                    bot: bot_id.into(),
                    reason,
                    why: why.and_then(|s| {
                        let t = s.trim();
                        (!t.is_empty()).then(|| t.to_owned())
                    }),
                    rejected,
                    fallback,
                    deferred_by,
                },
            },
            self.project,
        )
    }
}

fn norm_specialty(tag: &str) -> Vec<String> {
    tag.split(|c: char| !c.is_alphanumeric())
        .filter(|part| !part.is_empty())
        .map(str::to_lowercase)
        .collect()
}

fn assess_bot(
    db: &Db,
    rt: &dyn AgentRuntime,
    req: &DispatchRequest<'_>,
    bot: Bot,
    mandatory: &[Vec<String>],
    default_model: &impl Fn(&str) -> Option<String>,
) -> Result<std::result::Result<EligibleBot, Vec<Rejection>>> {
    if !bot.main && req.team.is_some_and(|t| t.member(&bot.id).is_none()) {
        return Ok(Err(vec![Rejection::OutsideTeam]));
    }
    if !mandatory.iter().all(|tag| {
        bot.specialties
            .iter()
            .any(|have| norm_specialty(have) == *tag)
    }) {
        return Ok(Err(vec![Rejection::Specialty]));
    }
    if !bot.enabled {
        return Ok(Err(vec![Rejection::Bot]));
    }
    let default_project = match &req.project {
        Some(_) => None,
        None => db.bot_default_project(&bot.id, req.workspace)?,
    };
    let project = match &req.project {
        Some(explicit) => explicit.as_deref(),
        None => default_project.as_deref(),
    }
    .filter(|p| !p.is_empty());
    let found = db
        .project(project)?
        .filter(|p| p.workspace_id == req.workspace && !p.archived);
    if project.is_some() && found.is_none() {
        return Ok(Err(vec![Rejection::Project]));
    }
    let scope = db
        .store()
        .read(|r| bot_dispatch::scope_for(r, req.workspace, req.team, &bot.id))?;
    let Scope::Nodes(nodes) = scope else {
        return Ok(Err(vec![Rejection::OutsideTeam]));
    };
    let same_runtime = req
        .harness
        .is_none_or(|rt_id| rt_id == bot.harness.as_str());
    let runtime_id = req.harness.unwrap_or(&bot.harness);
    let model = req
        .model
        .map(str::to_owned)
        .or_else(|| same_runtime.then(|| bot.model.clone()).flatten())
        .or_else(|| default_model(runtime_id));
    let effort = req
        .effort
        .or_else(|| same_runtime.then_some(bot.reasoning_effort))
        .and_then(steve_types::ReasoningEffort::non_default);
    let choice = HarnessChoice {
        harness: runtime_id.to_string(),
        model,
        effort,
    };
    let mut skipped = Vec::new();
    for (priority, node) in nodes
        .into_iter()
        .enumerate()
        .filter(|(_, n)| req.node.is_none_or(|id| id == n))
    {
        if req
            .required_nodes
            .is_some_and(|nodes| !nodes.contains(&node))
        {
            skipped.push(Rejection::Tag);
            continue;
        }
        if let Some(reason) = rt.assess_placement_node(
            req.workspace,
            found.as_ref(),
            &node,
            &choice,
            &bot.perm_mode,
        )? {
            skipped.push(reason);
            continue;
        }
        return Ok(Ok(EligibleBot {
            bot,
            project: found,
            choice,
            node,
            priority: priority as u32,
            skipped,
        }));
    }
    if skipped.is_empty() {
        skipped.push(Rejection::NoBindings);
    }
    Ok(Err(skipped))
}

pub(crate) fn record_selection_metrics(
    started: Instant,
    result: &Result<(BotPlacement, Option<Project>)>,
) {
    thread_local! {
        static SELECTION_SECONDS: std::cell::OnceCell<metrics::Histogram> =
            const { std::cell::OnceCell::new() };
        static SELECTION_TOTAL: std::cell::RefCell<
            std::collections::HashMap<&'static str, metrics::Counter>,
        > = std::cell::RefCell::new(std::collections::HashMap::new());
        static BLOCKED_TOTAL: std::cell::RefCell<
            std::collections::HashMap<&'static str, metrics::Counter>,
        > = std::cell::RefCell::new(std::collections::HashMap::new());
    }
    let elapsed = started.elapsed().as_secs_f64();
    SELECTION_SECONDS.with(|cell| {
        cell.get_or_init(|| metrics::histogram!("steve_bot_selection_seconds"))
            .record(elapsed);
    });
    match result {
        Ok((placement, _)) => {
            let key = placement.explanation.reason.as_str();
            SELECTION_TOTAL.with(|cache| {
                cache
                    .borrow_mut()
                    .entry(key)
                    .or_insert_with(
                        || metrics::counter!("steve_bot_selection_total", "reason" => key),
                    )
                    .increment(1);
            });
        }
        Err(CoreError::Placement(blocked)) => {
            let reasons = blocked
                .rejected
                .iter()
                .map(|b| b.reason)
                .collect::<BTreeSet<_>>();
            BLOCKED_TOTAL.with(|cache| {
                let mut cache = cache.borrow_mut();
                for reason in reasons
                    .into_iter()
                    .chain(blocked.rejected.is_empty().then_some(blocked.reason))
                {
                    let key = reason.as_str();
                    cache
                        .entry(key)
                        .or_insert_with(
                            || metrics::counter!("steve_bot_selection_blocked_total", "reason" => key),
                        )
                        .increment(1);
                }
            });
        }
        Err(_) => {}
    }
}

/// Compute all hard-eligible non-main executor Bots for `req` without ranking or calling any model.
pub(crate) fn eligible(
    db: &Db,
    rt: &dyn AgentRuntime,
    req: &DispatchRequest<'_>,
    default_model: impl Fn(&str) -> Option<String>,
) -> Result<EligibleCandidates> {
    let mut bots = db.bots(req.workspace)?;
    bots.sort_unstable_by(|a, b| a.id.cmp(&b.id));
    let mandatory: Vec<_> = req
        .required_specialties
        .iter()
        .map(|tag| norm_specialty(tag))
        .collect();
    let mut candidates = Vec::new();
    let mut rejected = Vec::new();
    for bot in bots.into_iter().filter(|b| {
        !b.main
            && req.team.is_none_or(|t| t.member(&b.id).is_some())
            && req.harness.is_none_or(|rt_id| rt_id == b.harness.as_str())
    }) {
        let id = bot.id.clone();
        match assess_bot(db, rt, req, bot, &mandatory, &default_model)? {
            Ok(eligible_bot) => candidates.push(eligible_bot),
            Err(reasons) => rejected.extend(reasons.into_iter().map(|reason| PlacementBlocker {
                bot: Some(id.clone().into()),
                reason,
            })),
        }
    }
    let primary_rejection = rejected
        .iter()
        .map(|b| b.reason)
        .find(|reason| *reason != Rejection::Specialty)
        .or_else(|| rejected.first().map(|b| b.reason));
    Ok(EligibleCandidates {
        candidates,
        rejected,
        primary_rejection,
    })
}

/// Validate a specific Bot choice (`req.bot` must be `Some`) without emitting dispatch metrics
/// (used for OFFER card inspection and pre-validation).
pub(crate) fn check_bot(
    db: &Db,
    rt: &dyn AgentRuntime,
    req: &DispatchRequest<'_>,
    default_model: impl Fn(&str) -> Option<String>,
    reason: PlacementReason,
    why: Option<String>,
) -> Result<(BotPlacement, Option<Project>)> {
    let Some(bot_id) = req.bot else {
        return Err(CoreError::Placement(PlacementBlocked {
            reason: Rejection::NoCandidates,
            rejected: vec![],
        }));
    };
    let Some(bot) = db.bots(req.workspace)?.into_iter().find(|b| b.id == bot_id) else {
        return Err(CoreError::Placement(PlacementBlocked {
            reason: Rejection::Unauthorized,
            rejected: vec![],
        }));
    };
    if bot.main && !req.allow_main {
        return Err(CoreError::Placement(PlacementBlocked {
            reason: Rejection::OutsideTeam,
            rejected: vec![PlacementBlocker {
                bot: Some(bot.id.into()),
                reason: Rejection::OutsideTeam,
            }],
        }));
    }
    let mandatory: Vec<_> = req
        .required_specialties
        .iter()
        .map(|tag| norm_specialty(tag))
        .collect();
    match assess_bot(db, rt, req, bot, &mandatory, &default_model)? {
        Ok(eligible_bot) => {
            Ok(eligible_bot.into_placement(req.node, reason, why, vec![], None, None))
        }
        Err(reasons) => {
            let primary = reasons
                .iter()
                .copied()
                .find(|r| *r != Rejection::Specialty)
                .or_else(|| reasons.first().copied())
                .unwrap_or(Rejection::Bot);
            let rejected = reasons
                .into_iter()
                .map(|r| PlacementBlocker {
                    bot: Some(bot_id.to_owned().into()),
                    reason: r,
                })
                .collect();
            Err(CoreError::Placement(PlacementBlocked {
                reason: primary,
                rejected,
            }))
        }
    }
}

/// Validate a chosen Bot (`req.bot` must be `Some`) and record `steve_bot_selection_*` metrics.
pub(crate) fn validate(
    db: &Db,
    rt: &dyn AgentRuntime,
    req: &DispatchRequest<'_>,
    default_model: impl Fn(&str) -> Option<String>,
    reason: PlacementReason,
    why: Option<String>,
) -> Result<(BotPlacement, Option<Project>)> {
    let started = Instant::now();
    let result = check_bot(db, rt, req, default_model, reason, why);
    record_selection_metrics(started, &result);
    result
}

/// Validate the workspace coordinator (main bot) after no eligible executor Bot was found.
pub(crate) fn validate_coordinator(
    db: &Db,
    rt: &dyn AgentRuntime,
    req: &DispatchRequest<'_>,
    default_model: impl Fn(&str) -> Option<String>,
    blocked: PlacementBlocked,
    deferred_by: CoordinatorDeferSource,
) -> Result<(BotPlacement, Option<Project>)> {
    let started = Instant::now();
    let result = (|| {
        let main = db
            .bots(req.workspace)?
            .into_iter()
            .find(|b| b.main)
            .ok_or_else(|| CoreError::Placement(blocked.clone()))?;
        let main_id = main.id.clone();
        match assess_bot(db, rt, req, main, &[], &default_model)? {
            Ok(eligible_bot) => Ok(eligible_bot.into_placement(
                req.node,
                PlacementReason::CoordinatorDeferred,
                None,
                blocked.rejected,
                Some(blocked.reason),
                Some(deferred_by),
            )),
            Err(reasons) => {
                let mut rejected = blocked.rejected;
                rejected.extend(reasons.iter().copied().map(|reason| PlacementBlocker {
                    bot: Some(main_id.clone().into()),
                    reason,
                }));
                let reason = reasons.first().copied().unwrap_or(blocked.reason);
                Err(CoreError::Placement(PlacementBlocked { reason, rejected }))
            }
        }
    })();
    record_selection_metrics(started, &result);
    result
}

#[cfg(test)]
mod tests {
    use steve_store::{NewBot, WS};

    use super::*;
    use crate::testkit::Kit;

    #[test]
    fn eligible_and_validate_enforce_hard_constraints_without_ranking() {
        let k = Kit::new();
        let db = k.core.store();
        db.store()
            .bots()
            .seed(
                "review",
                false,
                &NewBot {
                    name: "Review".into(),
                    harness: "fixture".into(),
                    specialties: Some(vec!["code review".into()]),
                    ..Default::default()
                },
                WS,
            )
            .unwrap();
        db.store()
            .bots()
            .seed(
                "off",
                false,
                &NewBot {
                    name: "Off".into(),
                    harness: "fixture".into(),
                    enabled: Some(false),
                    ..Default::default()
                },
                WS,
            )
            .unwrap();
        let req = DispatchRequest {
            workspace: WS,
            bot: None,
            harness: None,
            model: None,
            effort: None,
            team: None,
            project: &Some(None),
            required_specialties: &[],
            node: None,
            required_nodes: None,
            allow_main: false,
        };
        let found = eligible(db, k.rt.as_ref(), &req, |_| None).unwrap();
        let ids: Vec<&str> = found.candidates.iter().map(|c| c.bot.id.as_str()).collect();
        assert_eq!(ids, vec!["dev", "review"]);
        assert!(found.rejected.iter().any(|b| {
            b.bot.as_ref().map(|id| id.as_str()) == Some("off") && b.reason == Rejection::Bot
        }));
        assert_eq!(found.primary_rejection, Some(Rejection::Bot));

        // Required specialties hard-filter candidates without keyword scoring.
        let filtered = eligible(
            db,
            k.rt.as_ref(),
            &DispatchRequest {
                required_specialties: &["code-review".into()],
                ..req
            },
            |_| None,
        )
        .unwrap();
        let filtered_ids: Vec<&str> = filtered
            .candidates
            .iter()
            .map(|c| c.bot.id.as_str())
            .collect();
        assert_eq!(filtered_ids, vec!["review"]);

        let (placed, _) = validate(
            db,
            k.rt.as_ref(),
            &DispatchRequest {
                bot: Some("review"),
                ..req
            },
            |_| None,
            PlacementReason::SteveSelected,
            Some("Best fit for review".into()),
        )
        .unwrap();
        assert_eq!(placed.explanation.bot.as_str(), "review");
        assert_eq!(placed.explanation.reason, PlacementReason::SteveSelected);
        assert_eq!(
            placed.explanation.why.as_deref(),
            Some("Best fit for review")
        );

        let err = validate(
            db,
            k.rt.as_ref(),
            &DispatchRequest {
                bot: Some("off"),
                ..req
            },
            |_| None,
            PlacementReason::SteveSelected,
            None,
        )
        .unwrap_err();
        assert!(matches!(
            err,
            CoreError::Placement(PlacementBlocked {
                reason: Rejection::Bot,
                ..
            })
        ));

        let (coord, _) = validate_coordinator(
            db,
            k.rt.as_ref(),
            &req,
            |_| None,
            PlacementBlocked {
                reason: Rejection::NoCandidates,
                rejected: vec![],
            },
            CoordinatorDeferSource::Setting,
        )
        .unwrap();
        assert_eq!(coord.explanation.bot.as_str(), "steve");
        assert_eq!(
            coord.explanation.reason,
            PlacementReason::CoordinatorDeferred
        );
    }

    #[test]
    fn validate_enforces_workspace_membership_before_main_bot_restriction() {
        let k = Kit::new();
        let db = k.core.store();
        steve_store::testkit::hub::mutations::seed_users(db.store()).unwrap();
        let ws2 = db
            .store()
            .workspaces()
            .create(
                &steve_store::NewWorkspace {
                    name: "Other".into(),
                    description: None,
                    item_prefix: None,
                },
                "u",
            )
            .unwrap();
        db.store()
            .bots()
            .seed(
                "steve2",
                true,
                &NewBot {
                    name: "Steve2".into(),
                    harness: "fixture".into(),
                    ..Default::default()
                },
                &ws2.id,
            )
            .unwrap();

        let base = DispatchRequest {
            workspace: WS,
            bot: None,
            harness: None,
            model: None,
            effort: None,
            team: None,
            project: &Some(None),
            required_specialties: &[],
            node: None,
            required_nodes: None,
            allow_main: false,
        };

        // In-workspace main bot rejected with OutsideTeam when allow_main=false.
        let main_err = validate(
            db,
            k.rt.as_ref(),
            &DispatchRequest {
                bot: Some("steve"),
                ..base
            },
            |_| None,
            PlacementReason::SteveSelected,
            None,
        )
        .unwrap_err();
        assert!(matches!(
            main_err,
            CoreError::Placement(PlacementBlocked {
                reason: Rejection::OutsideTeam,
                ..
            })
        ));

        // Cross-workspace main bot rejected with Unauthorized (workspace membership checked first).
        let other_err = validate(
            db,
            k.rt.as_ref(),
            &DispatchRequest {
                bot: Some("steve2"),
                ..base
            },
            |_| None,
            PlacementReason::SteveSelected,
            None,
        )
        .unwrap_err();
        assert!(matches!(
            other_err,
            CoreError::Placement(PlacementBlocked {
                reason: Rejection::Unauthorized,
                ..
            })
        ));

        // Explicit owner selection allows in-workspace main bot when allow_main=true.
        let (placed, _) = validate(
            db,
            k.rt.as_ref(),
            &DispatchRequest {
                bot: Some("steve"),
                allow_main: true,
                ..base
            },
            |_| None,
            PlacementReason::Explicit,
            None,
        )
        .unwrap();
        assert_eq!(placed.explanation.bot.as_str(), "steve");
    }

    #[test]
    fn cross_workspace_bot_ignores_other_workspaces_default_project() {
        let k = Kit::new();
        let db = k.core.store();
        steve_store::testkit::hub::mutations::seed_users(db.store()).unwrap();
        let ws2 = db
            .store()
            .workspaces()
            .create(
                &steve_store::NewWorkspace {
                    name: "Other".into(),
                    description: None,
                    item_prefix: None,
                },
                "u",
            )
            .unwrap();
        let prj1 = db
            .store()
            .projects()
            .create(
                WS,
                &steve_store::NewProject {
                    name: "MainRepo".into(),
                    path: "/main-repo".into(),
                    kind: "dir".into(),
                    device_id: "hub".into(),
                    ..Default::default()
                },
            )
            .unwrap();
        db.store()
            .bots()
            .seed(
                "cross",
                false,
                &NewBot {
                    name: "Cross".into(),
                    harness: "fixture".into(),
                    default_project: Some(prj1.id.clone()),
                    ..Default::default()
                },
                WS,
            )
            .unwrap();
        db.store()
            .bots()
            .set_workspaces("cross", &[WS.into(), ws2.id.clone()])
            .unwrap();

        let (placed_ws1, _) = validate(
            db,
            k.rt.as_ref(),
            &DispatchRequest {
                workspace: WS,
                bot: Some("cross"),
                harness: None,
                model: None,
                effort: None,
                team: None,
                project: &None,
                required_specialties: &[],
                node: None,
                required_nodes: None,
                allow_main: false,
            },
            |_| None,
            PlacementReason::SteveSelected,
            None,
        )
        .unwrap();
        assert_eq!(placed_ws1.explanation.bot.as_str(), "cross");
        assert_eq!(
            db.bot_default_project("cross", WS).unwrap().as_deref(),
            Some(prj1.id.as_str())
        );

        let (placed_ws2, _) = validate(
            db,
            k.rt.as_ref(),
            &DispatchRequest {
                workspace: &ws2.id,
                bot: Some("cross"),
                harness: None,
                model: None,
                effort: None,
                team: None,
                project: &None,
                required_specialties: &[],
                node: None,
                required_nodes: None,
                allow_main: false,
            },
            |_| None,
            PlacementReason::SteveSelected,
            None,
        )
        .unwrap();
        assert_eq!(placed_ws2.explanation.bot.as_str(), "cross");
        assert_eq!(db.bot_default_project("cross", &ws2.id).unwrap(), None);
    }
}
