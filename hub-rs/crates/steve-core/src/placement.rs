//! Deterministic Bot selection, independent of the router model and of session creation.
use std::{collections::BTreeSet, sync::LazyLock, time::Instant};

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

pub(crate) const RULE_VERSION: u32 = 1;

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
    default_model: impl Fn(&str) -> Option<String>,
) -> Result<(BotPlacement, Option<Project>)> {
    let started = Instant::now();
    let result = select_inner(db, rt, request, default_model);
    thread_local! {
        static SELECTION_SECONDS: std::cell::OnceCell<metrics::Histogram> =
            const { std::cell::OnceCell::new() };
        static BLOCKED_TOTAL: std::cell::RefCell<
            std::collections::HashMap<&'static str, metrics::Counter>,
        > = std::cell::RefCell::new(std::collections::HashMap::new());
    }
    let elapsed = started.elapsed().as_secs_f64();
    SELECTION_SECONDS.with(|cell| {
        cell.get_or_init(|| metrics::histogram!("steve_bot_selection_seconds"))
            .record(elapsed);
    });
    if let Err(CoreError::Placement(blocked)) = &result {
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
    result
}

fn select_inner(
    db: &Db,
    rt: &dyn AgentRuntime,
    req: DispatchRequest<'_>,
    default_model: impl Fn(&str) -> Option<String>,
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
            !b.main
                && req.team.is_none_or(|t| t.member(&b.id).is_some())
                && req.harness.is_none_or(|rt_id| rt_id == b.harness.as_str())
        }
    }) {
        let id = bot.id.clone();
        let evaluated = candidate(db, rt, &req, bot, &task, &mandatory, &default_model)?;
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
    let reason = if req.bot.is_some() {
        PlacementReason::Explicit
    } else if !best.score.specialties.is_empty() {
        PlacementReason::Specialty
    } else if !best.score.description.is_empty() {
        PlacementReason::Description
    } else if best.score.project {
        PlacementReason::ProjectPreference
    } else {
        PlacementReason::StableOrder
    };
    rejected.extend(best.skipped.into_iter().map(|reason| PlacementBlocker {
        bot: Some(best.bot.id.clone().into()),
        reason,
    }));
    let reasoning_effort = best.choice.effective_effort();
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
            reasoning_effort,
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
                fallback: None,
                deferred_by: None,
            },
        },
        best.project,
    ))
}

/// Place work onto the workspace coordinator (main bot) after automatic selection found no
/// eligible executor and either the owner chose "later" on a decision card or workspace settings
/// direct Steve to handle unassigned work itself.
pub(crate) fn select_coordinator(
    db: &Db,
    rt: &dyn AgentRuntime,
    req: DispatchRequest<'_>,
    default_model: impl Fn(&str) -> Option<String>,
    blocked: PlacementBlocked,
    deferred_by: CoordinatorDeferSource,
) -> Result<(BotPlacement, Option<Project>)> {
    let main = db
        .bots(req.workspace)?
        .into_iter()
        .find(|b| b.main)
        .ok_or_else(|| CoreError::Placement(blocked.clone()))?;
    let main_id = main.id.clone();
    let task = words(req.goal);
    let evaluated = candidate(db, rt, &req, main, &task, &[], &default_model)?;
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
    let reasoning_effort = best.choice.effective_effort();
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
            reasoning_effort,
            explanation: PlacementExplanation {
                bot: best.bot.id.into(),
                reason: PlacementReason::CoordinatorDeferred,
                matched: vec![],
                rejected,
                fallback: Some(blocked.reason),
                deferred_by: Some(deferred_by),
            },
        },
        best.project,
    ))
}

fn candidate(
    db: &Db,
    rt: &dyn AgentRuntime,
    req: &DispatchRequest<'_>,
    bot: Bot,
    task: &[String],
    mandatory: &[Vec<String>],
    default_model: &impl Fn(&str) -> Option<String>,
) -> Result<std::result::Result<Candidate, Vec<Rejection>>> {
    if !bot.main && req.team.is_some_and(|t| t.member(&bot.id).is_none()) {
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
        // Disable `dev` too so no non-main executor remains; main bot ("steve") is never an
        // automatic candidate, so automatic placement leaves the item unassigned and opens a card.
        db.store()
            .bots()
            .patch(
                "dev",
                &BotPatch {
                    enabled: Some(false),
                    ..Default::default()
                },
                "owner",
            )
            .unwrap();
        let unassigned = k
            .core
            .create_item(CreateItem {
                ws: WS.into(),
                title: "Placement fixture".into(),
                goal: "Code review".into(),
                project: Some(None),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(unassigned.bot_id, None);
        assert!(
            db.store()
                .read(|r| bot_dispatch::initial(r, &unassigned.id))
                .unwrap()
                .is_none()
        );
        // Explicit selection of Steve remains allowed even when no ordinary Bot is eligible.
        let explicit_steve = create("Code review", Some("steve"), &[]).unwrap();
        assert_eq!(explicit_steve.bot.as_str(), "steve");
        assert_eq!(explicit_steve.reason, PlacementReason::Explicit);
        assert!(matches!(
            create("Code review", Some("review"), &[]),
            Err(CoreError::Placement(PlacementBlocked {
                reason: Rejection::Offline,
                ..
            }))
        ));
        let specialty_wait = k
            .core
            .create_item(CreateItem {
                ws: WS.into(),
                title: "Audit".into(),
                goal: "External audit".into(),
                project: Some(None),
                required_specialties: vec!["security audit".into()],
                ..Default::default()
            })
            .unwrap();
        assert_eq!(specialty_wait.bot_id, None);
        let blocked = db
            .store()
            .read(|r| bot_dispatch::creation_blocked(r, WS, &specialty_wait.id))
            .unwrap()
            .unwrap();
        assert_eq!(blocked.reason, Rejection::Specialty);
    }
}
