//! Deterministic Bot selection, independent of the router model and of session creation.
use std::{collections::BTreeSet, sync::LazyLock, time::Instant};

use steve_node_api::HarnessChoice;
use steve_store::{
    Bot, Project,
    bot_dispatch::{self, Scope, Team},
};
use steve_types::{
    BotPlacement, Millis, PlacementBlocked, PlacementBlocker, PlacementExplanation,
    PlacementReason, PlacementRejection as Rejection,
};

use crate::{AgentRuntime, CoreError, Result, repo::Db};

pub(crate) const RULE_VERSION: u32 = 1;

pub(crate) struct DispatchRequest<'a> {
    pub workspace: &'a str,
    pub bot: Option<&'a str>,
    pub team: Option<&'a Team>,
    pub project: &'a Option<Option<String>>,
    pub goal: &'a str,
    pub tags: &'a [String],
    pub required_specialties: &'a [String],
    pub node: Option<&'a str>,
    pub required_nodes: Option<&'a [String]>,
}

fn words(text: &str) -> Vec<String> {
    // Unicode word boundaries also split unspaced ideographs. Symbols and punctuation carry no rank.
    static WORD: LazyLock<regex::Regex> = LazyLock::new(|| {
        regex::Regex::new(r"(?u)\p{Han}|[^\W\p{Han}]+").expect("valid word pattern")
    });
    WORD.find_iter(text)
        .map(|m| m.as_str().to_lowercase())
        .collect()
}

#[derive(Default)]
struct Score {
    specialties: Vec<String>,
    description: Vec<String>,
    project: bool,
}

impl Score {
    fn evidence(&self) -> (usize, usize, bool) {
        (self.specialties.len(), self.description.len(), self.project)
    }
}

struct Candidate {
    bot: Bot,
    project: Option<Project>,
    choice: HarnessChoice,
    node: String,
    priority: u32,
    score: Score,
    skipped: Vec<Rejection>,
}

fn score(
    req: &DispatchRequest<'_>,
    bot: &Bot,
    project: Option<&Project>,
    task: &[String],
) -> Score {
    let mut seen = BTreeSet::new();
    let specialties = bot
        .specialties
        .iter()
        .filter_map(|tag| {
            let normalized = words(tag);
            (!normalized.is_empty()
                && task
                    .windows(normalized.len())
                    .any(|part| part == normalized)
                && seen.insert(normalized))
            .then(|| tag.clone())
        })
        .collect();
    // An explicit task label can match the description even if the Bot has no specialty tags.
    let description_words = words(&bot.role);
    let description = req
        .tags
        .iter()
        .map(String::as_str)
        .chain(
            bot.role
                .split([',', '.', ';', '\u{ff0c}', '\u{3002}', '\u{ff1b}', '\n'])
                .map(str::trim),
        )
        .filter_map(|tag| {
            let normalized = words(tag);
            (!normalized.is_empty()
                && description_words
                    .windows(normalized.len())
                    .any(|part| part == normalized)
                && task
                    .windows(normalized.len())
                    .any(|part| part == normalized)
                && seen.insert(normalized))
            .then(|| tag.to_owned())
        })
        .collect();
    let project = req.project.is_some()
        && project.is_some_and(|p| {
            p.default_bot.as_ref() == Some(&bot.id) || bot.default_project.as_ref() == Some(&p.id)
        });
    Score {
        specialties,
        description,
        project,
    }
}

fn rank(candidate: &Candidate) -> ((usize, usize, bool), std::cmp::Reverse<u32>, &str) {
    (
        candidate.score.evidence(),
        std::cmp::Reverse(candidate.priority),
        &candidate.bot.id,
    )
}

pub(crate) fn select(
    db: &Db,
    rt: &dyn AgentRuntime,
    request: DispatchRequest<'_>,
    model: impl Fn(&Bot) -> Option<String>,
) -> Result<(BotPlacement, Option<Project>)> {
    select_with_policy(db, rt, request, model, true)
}

/// Worker-only selection aligned with PR #544 (`reform/steve-coordinator`): when no explicit `bot`
/// is named, excludes the workspace main coordinator bot (`!b.main`) so unassigned work never
/// silently falls back to Steve unless the workspace setting `noExecutor = "steve"` or the owner
/// explicitly delegates to Steve on the decision card.
pub(crate) fn select_worker(
    db: &Db,
    rt: &dyn AgentRuntime,
    request: DispatchRequest<'_>,
    model: impl Fn(&Bot) -> Option<String>,
) -> Result<(BotPlacement, Option<Project>)> {
    select_with_policy(db, rt, request, model, false)
}

/// Explicit coordinator placement aligned with PR #544's `select_coordinator`: places work onto
/// the workspace main bot (`b.main`) when workspace setting `noExecutor = "steve"` or the owner
/// explicitly chose `"steve"` on a placement decision card.
pub(crate) fn select_coordinator_fallback(
    db: &Db,
    rt: &dyn AgentRuntime,
    req: DispatchRequest<'_>,
    model: impl Fn(&Bot) -> Option<String>,
    blocked: PlacementBlocked,
) -> Result<(BotPlacement, Option<Project>)> {
    let main = db
        .bots(req.workspace)?
        .into_iter()
        .find(|b| b.main)
        .ok_or_else(|| CoreError::Placement(blocked.clone()))?;
    let main_id = main.id.clone();
    let task = words(req.goal);
    let evaluated = candidate(db, rt, &req, main, &task, &[], &model, true)?;
    let best = match evaluated {
        Ok(candidate) => candidate,
        Err(reasons) => {
            let mut rejected = blocked.rejected;
            rejected.extend(reasons.iter().copied().map(|reason| PlacementBlocker {
                bot: Some(main_id.clone().into()),
                reason,
            }));
            let reason = reasons.first().copied().unwrap_or(blocked.reason);
            return Err(CoreError::Placement(PlacementBlocked { reason, rejected }));
        }
    };
    let mut rejected = blocked.rejected;
    rejected.extend(best.skipped.into_iter().map(|reason| PlacementBlocker {
        bot: Some(best.bot.id.clone().into()),
        reason,
    }));
    Ok((
        BotPlacement {
            rule_version: RULE_VERSION,
            bot_revision: best.bot.revision,
            at: Millis(steve_store::now()),
            node: best.node.into(),
            required_node: req.node.map(Into::into),
            node_priority: best.priority,
            harness: best.choice.harness,
            model: best.choice.model,
            reasoning_effort: best.choice.effort,
            explanation: PlacementExplanation {
                bot: best.bot.id.into(),
                reason: PlacementReason::SteveFallback,
                matched: vec![],
                rejected,
                fallback: Some(blocked.reason),
            },
        },
        best.project,
    ))
}

fn select_with_policy(
    db: &Db,
    rt: &dyn AgentRuntime,
    request: DispatchRequest<'_>,
    model: impl Fn(&Bot) -> Option<String>,
    allow_main_implicit: bool,
) -> Result<(BotPlacement, Option<Project>)> {
    let started = Instant::now();
    let result = select_inner(db, rt, request, model, allow_main_implicit);
    metrics::histogram!("steve_bot_selection_seconds").record(started.elapsed().as_secs_f64());
    if let Err(CoreError::Placement(blocked)) = &result {
        let reasons = blocked
            .rejected
            .iter()
            .map(|b| b.reason)
            .collect::<BTreeSet<_>>();
        for reason in reasons
            .into_iter()
            .chain(blocked.rejected.is_empty().then_some(blocked.reason))
        {
            metrics::counter!("steve_bot_selection_blocked_total", "reason" => reason.as_str())
                .increment(1);
        }
    }
    result
}

fn select_inner(
    db: &Db,
    rt: &dyn AgentRuntime,
    req: DispatchRequest<'_>,
    model: impl Fn(&Bot) -> Option<String>,
    allow_main_implicit: bool,
) -> Result<(BotPlacement, Option<Project>)> {
    let mut bots = db.bots(req.workspace)?;
    bots.sort_unstable_by(|a, b| a.id.cmp(&b.id));
    let task = words(
        &std::iter::once(req.goal)
            .chain(
                req.tags
                    .iter()
                    .chain(req.required_specialties)
                    .map(String::as_str),
            )
            .collect::<Vec<_>>()
            .join("\n"),
    );
    let mandatory: Vec<_> = req
        .required_specialties
        .iter()
        .map(|tag| words(tag))
        .collect();
    let mut eligible = Vec::new();
    let mut rejected = Vec::new();
    for bot in bots.into_iter().filter(|b| match req.bot {
        Some(id) => id == b.id,
        None => {
            (allow_main_implicit || !b.main) && req.team.is_none_or(|t| t.member(&b.id).is_some())
        }
    }) {
        let id = bot.id.clone();
        let evaluated = candidate(db, rt, &req, bot, &task, &mandatory, &model, false)?;
        match evaluated {
            Ok(candidate) => eligible.push(candidate),
            Err(reasons) => rejected.extend(reasons.into_iter().map(|reason| PlacementBlocker {
                bot: Some(id.clone().into()),
                reason,
            })),
        }
    }
    let primary = rejected
        .iter()
        .map(|b| b.reason)
        .find(|reason| *reason != Rejection::Specialty)
        .or_else(|| rejected.first().map(|b| b.reason));
    let Some(best) = eligible.into_iter().max_by(|a, b| rank(a).cmp(&rank(b))) else {
        return Err(CoreError::Placement(PlacementBlocked {
            reason: primary.unwrap_or(if req.bot.is_some() {
                Rejection::Unauthorized
            } else {
                Rejection::NoCandidates
            }),
            rejected,
        }));
    };
    let fallback = (req.bot.is_none() && best.bot.main && best.score.evidence() == (0, 0, false))
        .then_some(primary)
        .flatten();
    let reason = if req.bot.is_some() {
        PlacementReason::Explicit
    } else if !best.score.specialties.is_empty() {
        PlacementReason::Specialty
    } else if !best.score.description.is_empty() {
        PlacementReason::Description
    } else if best.score.project {
        PlacementReason::ProjectPreference
    } else if fallback.is_some() {
        PlacementReason::SteveFallback
    } else {
        PlacementReason::StableOrder
    };
    rejected.extend(best.skipped.into_iter().map(|reason| PlacementBlocker {
        bot: Some(best.bot.id.clone().into()),
        reason,
    }));
    Ok((
        BotPlacement {
            rule_version: RULE_VERSION,
            bot_revision: best.bot.revision,
            at: Millis(steve_store::now()),
            node: best.node.into(),
            required_node: req.node.map(Into::into),
            node_priority: best.priority,
            harness: best.choice.harness,
            model: best.choice.model,
            reasoning_effort: best.choice.effort,
            explanation: PlacementExplanation {
                bot: best.bot.id.into(),
                reason,
                matched: best
                    .score
                    .specialties
                    .into_iter()
                    .chain(best.score.description)
                    .collect(),
                rejected,
                fallback,
            },
        },
        best.project,
    ))
}

#[allow(clippy::too_many_arguments)]
fn candidate(
    db: &Db,
    rt: &dyn AgentRuntime,
    req: &DispatchRequest<'_>,
    bot: Bot,
    task: &[String],
    mandatory: &[Vec<String>],
    model: &impl Fn(&Bot) -> Option<String>,
    coordinator_bypass_team: bool,
) -> Result<std::result::Result<Candidate, Vec<Rejection>>> {
    if !(coordinator_bypass_team && bot.main)
        && req.team.is_some_and(|t| t.member(&bot.id).is_none())
    {
        return Ok(Err(vec![Rejection::OutsideTeam]));
    }
    if !mandatory
        .iter()
        .all(|tag| bot.specialties.iter().any(|have| words(have) == *tag))
    {
        return Ok(Err(vec![Rejection::Specialty]));
    }
    if !bot.enabled {
        return Ok(Err(vec![Rejection::Bot]));
    }
    let project = req
        .project
        .as_ref()
        .unwrap_or(&bot.default_project)
        .as_deref()
        .filter(|p| !p.is_empty());
    let found = db
        .project(project)?
        .filter(|p| p.workspace_id == req.workspace && !p.archived);
    if project.is_some() && found.is_none() {
        return Ok(Err(vec![Rejection::Project]));
    }

    let scope = db.store().read(|r| {
        bot_dispatch::scope_for(
            r,
            req.workspace,
            if coordinator_bypass_team && bot.main {
                None
            } else {
                req.team
            },
            &bot.id,
        )
    })?;
    let Scope::Nodes(nodes) = scope else {
        return Ok(Err(vec![Rejection::OutsideTeam]));
    };
    let choice = HarnessChoice {
        harness: bot.harness.clone(),
        model: model(&bot),
        effort: bot.reasoning_effort.clone(),
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
        let score = score(req, &bot, found.as_ref(), task);
        return Ok(Ok(Candidate {
            bot,
            project: found,
            choice,
            node,
            priority: priority as u32,
            score,
            skipped,
        }));
    }
    if skipped.is_empty() {
        skipped.push(Rejection::NoBindings);
    }
    Ok(Err(skipped))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CreateItem, testkit::Kit};
    use steve_store::{BotPatch, NewBot, WS};

    #[test]
    fn selection_only_ranks_eligible_bots_and_preserves_explicit_routes() {
        let k = Kit::new();
        let db = k.core.store();
        for (id, specialties) in [
            ("design", vec!["UI".into()]),
            ("review", vec!["code review".into()]),
        ] {
            db.store()
                .bots()
                .seed(
                    id,
                    false,
                    &NewBot {
                        name: id.into(),
                        harness: "fixture".into(),
                        specialties: Some(specialties),
                        ..Default::default()
                    },
                    WS,
                )
                .unwrap();
        }
        let create = |goal: &str, bot: Option<&str>, required: &[&str]| {
            let item = k.core.create_item(CreateItem {
                ws: WS.into(),
                title: "Placement fixture".into(),
                goal: goal.into(),
                bot: bot.map(str::to_owned),
                project: Some(None),
                required_specialties: required.iter().map(|s| (*s).to_owned()).collect(),
                propose: true,
                ..Default::default()
            })?;
            Ok::<_, CoreError>(
                db.store()
                    .read(|r| bot_dispatch::initial(r, &item.id))?
                    .unwrap()
                    .explanation,
            )
        };
        let review = create("CODE-REVIEW this change", None, &[]).unwrap();
        assert_eq!(review.bot.as_str(), "review");
        assert_eq!(review.reason, PlacementReason::Specialty);
        assert_eq!(review.matched, ["code review"]);
        assert_eq!(
            review,
            create("CODE-REVIEW this change", None, &[]).unwrap()
        );
        assert_eq!(
            create("Improve the UI", None, &[]).unwrap().bot.as_str(),
            "design"
        );
        assert_eq!(
            create("Improve the UI", Some("review"), &[])
                .unwrap()
                .reason,
            PlacementReason::Explicit
        );

        db.store()
            .bots()
            .patch(
                "design",
                &BotPatch {
                    specialties: Some(vec!["code review".into()]),
                    ..Default::default()
                },
                "owner",
            )
            .unwrap();
        assert_eq!(
            create("Code review", None, &[]).unwrap().bot.as_str(),
            "review"
        );
        db.store()
            .write(|w| steve_store::bot_admin::set_bindings(w, WS, "review", &["offline".into()]))
            .unwrap();
        k.rt.st()
            .placement_rejections
            .insert("offline".into(), Rejection::Offline);
        let blocked_preference = create("Code review", None, &[]).unwrap();
        assert_eq!(blocked_preference.bot.as_str(), "design");
        assert!(
            blocked_preference
                .rejected
                .iter()
                .any(|b| b.reason == Rejection::Offline)
        );
        db.store()
            .bots()
            .patch(
                "design",
                &BotPatch {
                    enabled: Some(false),
                    ..Default::default()
                },
                "owner",
            )
            .unwrap();
        let fallback = create("Code review", None, &[]).unwrap();
        assert_eq!(fallback.bot.as_str(), "steve");
        assert_eq!(fallback.reason, PlacementReason::SteveFallback);
        assert!(fallback.fallback.is_some());
        assert!(matches!(
            create("Code review", Some("review"), &[]),
            Err(CoreError::Placement(PlacementBlocked {
                reason: Rejection::Offline,
                ..
            }))
        ));
        assert!(matches!(
            create("External audit", None, &["security audit"]),
            Err(CoreError::Placement(PlacementBlocked {
                reason: Rejection::Specialty,
                ..
            }))
        ));
    }
}
