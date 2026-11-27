//! Deterministic Bot selection, independent of the router model and of session creation.
use std::{
    collections::{BTreeSet, HashMap},
    sync::LazyLock,
    time::Instant,
};

use steve_node_api::{HarnessChoice, PermissionMode};
use steve_store::{
    Bot, Project,
    bot_dispatch::{self, Scope, Team},
};
use steve_types::{
    BotPlacement, CoordinatorDeferSource, Millis, PlacementBlocked, PlacementBlocker,
    PlacementExplanation, PlacementReason, PlacementRejection as Rejection,
};

use crate::{AgentRuntime, CoreError, Result, repo::Db};

pub(crate) const RULE_VERSION: u32 = 2;

/// Separators splitting Bot role descriptions into matchable clauses while keeping hyphenated terms intact.
const ROLE_SEPARATORS: &[char] = &[
    ',', '.', ';', ':', '/', '|', '(', ')', '[', ']', '\n', '\r', '\u{ff0c}', '\u{3002}',
    '\u{ff1b}', '\u{3001}', '\u{ff1a}', '\u{00b7}', '\u{ff08}', '\u{ff09}', '\u{2014}',
];
/// Minimum character length for a single-token role clause to count as a match signal.
const MIN_SINGLE_TOKEN_CHARS: usize = 2;

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
    pub title: &'a str,
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

fn contains_seq(haystack: &[String], needle: &[String]) -> bool {
    !needle.is_empty() && haystack.windows(needle.len()).any(|part| part == needle)
}

fn is_meaningful_phrase(tokens: &[String]) -> bool {
    match tokens {
        [] => false,
        [only] => only.chars().count() >= MIN_SINGLE_TOKEN_CHARS,
        _ => true,
    }
}

#[derive(Default, Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum ProjectAffinity {
    #[default]
    None = 0,
    BotDefaultProject = 1,
    ProjectDefaultBot = 2,
}

#[derive(Default)]
struct Score {
    specialties: Vec<String>,
    description: Vec<String>,
    project: ProjectAffinity,
}

struct Candidate {
    bot: Bot,
    project: Option<Project>,
    choice: HarnessChoice,
    node: String,
    priority: u32,
    team_index: usize,
    joined_at: i64,
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
            (contains_seq(task, &normalized) && seen.insert(normalized)).then(|| tag.clone())
        })
        .collect();
    // An explicit task label can match the role description or Bot name, and role clauses separated
    // by common punctuation can match the task even if the Bot has no specialty tags.
    let role_words = words(&bot.role);
    let name_words = words(&bot.name);
    let mut description: Vec<String> = req
        .tags
        .iter()
        .filter_map(|tag| {
            let normalized = words(tag);
            (!normalized.is_empty()
                && (contains_seq(&role_words, &normalized)
                    || contains_seq(&name_words, &normalized))
                && seen.insert(normalized))
            .then(|| tag.clone())
        })
        .collect();
    description.extend(
        bot.role
            .split(ROLE_SEPARATORS)
            .map(str::trim)
            .filter_map(|clause| {
                let normalized = words(clause);
                (is_meaningful_phrase(&normalized)
                    && contains_seq(task, &normalized)
                    && seen.insert(normalized))
                .then(|| clause.to_owned())
            }),
    );
    let project = if req.project.is_some() {
        match project {
            Some(p) if p.default_bot.as_ref() == Some(&bot.id) => {
                ProjectAffinity::ProjectDefaultBot
            }
            Some(p) if bot.default_project.as_ref() == Some(&p.id) => {
                ProjectAffinity::BotDefaultProject
            }
            _ => ProjectAffinity::None,
        }
    } else {
        ProjectAffinity::None
    };
    Score {
        specialties,
        description,
        project,
    }
}

/// Deterministic candidate ranking key (higher tuple wins):
/// 1. Matched specialty count;
/// 2. Matched role/name/tag description count;
/// 3. Project affinity (`ProjectDefaultBot > BotDefaultProject > None`);
/// 4. Specialty fit (focused specialist when matched; generalist over unrelated specialist on zero match);
/// 5. Execution Node priority (lower binding index wins);
/// 6. Team member order (earlier in Team wins);
/// 7. Execution capability (non-`plan` outranks read-only `plan`);
/// 8. Workspace membership order (earlier `bot_workspaces.created` wins);
/// 9. Ascending `bot.id` as final stable tie-breaker.
fn rank(
    candidate: &Candidate,
) -> (
    usize,
    usize,
    ProjectAffinity,
    usize,
    std::cmp::Reverse<u32>,
    std::cmp::Reverse<usize>,
    bool,
    std::cmp::Reverse<i64>,
    std::cmp::Reverse<&str>,
) {
    let specialty_fit = if candidate.score.specialties.is_empty() {
        usize::from(candidate.bot.specialties.is_empty())
    } else {
        usize::MAX
            - candidate
                .bot
                .specialties
                .len()
                .saturating_sub(candidate.score.specialties.len())
    };
    let can_edit = PermissionMode::parse(&candidate.bot.perm_mode) != Some(PermissionMode::Plan);
    (
        candidate.score.specialties.len(),
        candidate.score.description.len(),
        candidate.score.project,
        specialty_fit,
        std::cmp::Reverse(candidate.priority),
        std::cmp::Reverse(candidate.team_index),
        can_edit,
        std::cmp::Reverse(candidate.joined_at),
        std::cmp::Reverse(candidate.bot.id.as_str()),
    )
}

fn task_words(req: &DispatchRequest<'_>) -> Vec<String> {
    words(
        &std::iter::once(req.title)
            .chain(std::iter::once(req.goal))
            .chain(
                req.tags
                    .iter()
                    .chain(req.required_specialties)
                    .map(String::as_str),
            )
            .collect::<Vec<_>>()
            .join("\n"),
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
    match &result {
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
    result
}

/// Preview deterministic Bot placement for an OFFER card without incrementing dispatch metrics.
pub(crate) fn preview(
    db: &Db,
    rt: &dyn AgentRuntime,
    request: DispatchRequest<'_>,
    default_model: impl Fn(&str) -> Option<String>,
) -> Result<(BotPlacement, Option<Project>)> {
    select_inner(db, rt, request, default_model)
}

/// Preview coordinator fallback placement for an OFFER card when workspace policy defers to Steve.
pub(crate) fn preview_coordinator(
    db: &Db,
    rt: &dyn AgentRuntime,
    req: DispatchRequest<'_>,
    default_model: impl Fn(&str) -> Option<String>,
    blocked: PlacementBlocked,
    deferred_by: CoordinatorDeferSource,
) -> Result<(BotPlacement, Option<Project>)> {
    select_coordinator(db, rt, req, default_model, blocked, deferred_by)
}

fn select_inner(
    db: &Db,
    rt: &dyn AgentRuntime,
    req: DispatchRequest<'_>,
    default_model: impl Fn(&str) -> Option<String>,
) -> Result<(BotPlacement, Option<Project>)> {
    let mut bots = db.bots(req.workspace)?;
    bots.sort_unstable_by(|a, b| a.id.cmp(&b.id));
    let join_order = db
        .store()
        .read(|r| bot_dispatch::workspace_join_order(r, req.workspace))?;
    let task = task_words(&req);
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
        let evaluated = candidate(
            db,
            rt,
            &req,
            bot,
            &task,
            &mandatory,
            &join_order,
            &default_model,
        )?;
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
    } else if best.score.project != ProjectAffinity::None {
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
    let task = task_words(&req);
    let join_order = HashMap::new();
    let evaluated = candidate(db, rt, &req, main, &task, &[], &join_order, &default_model)?;
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

#[allow(clippy::too_many_arguments)]
fn candidate(
    db: &Db,
    rt: &dyn AgentRuntime,
    req: &DispatchRequest<'_>,
    bot: Bot,
    task: &[String],
    mandatory: &[Vec<String>],
    join_order: &HashMap<String, i64>,
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
    let team_index = req
        .team
        .and_then(|t| t.members.iter().position(|m| m.bot == bot.id))
        .unwrap_or(usize::MAX);
    let joined_at = join_order.get(&bot.id).copied().unwrap_or(i64::MAX);
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
            team_index,
            joined_at,
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
    use steve_store::{
        BotPatch, NewBot, WS,
        bot_dispatch::{Member, Team},
    };

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
            "design"
        );
        db.store()
            .write(|w| steve_store::bot_admin::set_bindings(w, WS, "design", &["offline".into()]))
            .unwrap();
        k.rt.st()
            .placement_rejections
            .insert("offline".into(), Rejection::Offline);
        let blocked_preference = create("Code review", None, &[]).unwrap();
        assert_eq!(blocked_preference.bot.as_str(), "review");
        assert!(
            blocked_preference
                .rejected
                .iter()
                .any(|b| b.reason == Rejection::Offline)
        );
        db.store()
            .bots()
            .patch(
                "review",
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
            create("Code review", Some("design"), &[]),
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

    #[test]
    fn zero_signal_tie_prefers_generalist_over_higher_uid_specialist() {
        let k = Kit::new();
        let db = k.core.store().store();
        db.bots()
            .patch(
                "dev",
                &BotPatch {
                    enabled: Some(false),
                    ..Default::default()
                },
                "owner",
            )
            .unwrap();
        db.bots()
            .seed(
                "bot_7bb689d6507b4e72",
                false,
                &NewBot {
                    name: "开发".into(),
                    harness: "fixture".into(),
                    role: Some("负责日常编码实现".into()),
                    perm_mode: Some("auto".into()),
                    ..Default::default()
                },
                WS,
            )
            .unwrap();
        db.bots()
            .seed(
                "bot_d1b261984fef4cc9",
                false,
                &NewBot {
                    name: "文档".into(),
                    harness: "fixture".into(),
                    role: Some("写和整理文档、学习资料、调研笔记".into()),
                    specialties: Some(vec!["文档".into(), "调研".into()]),
                    perm_mode: Some("auto".into()),
                    ..Default::default()
                },
                WS,
            )
            .unwrap();

        let (placed, _) = select(
            k.core.store(),
            k.rt.as_ref(),
            DispatchRequest {
                workspace: WS,
                bot: None,
                harness: None,
                model: None,
                effort: None,
                team: None,
                project: &Some(None),
                title: "修复登录页样式对齐",
                goal: "把按钮间距调整成一致",
                tags: &[],
                required_specialties: &[],
                node: None,
                required_nodes: None,
            },
            |_| None,
        )
        .unwrap();
        assert_eq!(placed.explanation.bot.as_str(), "bot_7bb689d6507b4e72");
        assert_eq!(placed.explanation.reason, PlacementReason::StableOrder);
    }

    #[test]
    fn team_member_order_wins_zero_signal_ties_regardless_of_bot_id() {
        let k = Kit::new();
        let db = k.core.store().store();
        db.bots()
            .patch(
                "dev",
                &BotPatch {
                    enabled: Some(false),
                    ..Default::default()
                },
                "owner",
            )
            .unwrap();
        for id in ["bot_zzzz", "bot_aaaa"] {
            db.bots()
                .seed(
                    id,
                    false,
                    &NewBot {
                        name: id.into(),
                        harness: "fixture".into(),
                        perm_mode: Some("auto".into()),
                        ..Default::default()
                    },
                    WS,
                )
                .unwrap();
        }
        let team_z_first = Team {
            id: "t1".into(),
            name: "Z first".into(),
            members: vec![
                Member {
                    bot: "bot_zzzz".into(),
                    nodes: None,
                },
                Member {
                    bot: "bot_aaaa".into(),
                    nodes: None,
                },
            ],
        };
        let (placed_z, _) = select(
            k.core.store(),
            k.rt.as_ref(),
            DispatchRequest {
                workspace: WS,
                bot: None,
                harness: None,
                model: None,
                effort: None,
                team: Some(&team_z_first),
                project: &Some(None),
                title: "Task",
                goal: "Unmatched task",
                tags: &[],
                required_specialties: &[],
                node: None,
                required_nodes: None,
            },
            |_| None,
        )
        .unwrap();
        assert_eq!(placed_z.explanation.bot.as_str(), "bot_zzzz");

        let team_a_first = Team {
            id: "t2".into(),
            name: "A first".into(),
            members: vec![
                Member {
                    bot: "bot_aaaa".into(),
                    nodes: None,
                },
                Member {
                    bot: "bot_zzzz".into(),
                    nodes: None,
                },
            ],
        };
        let (placed_a, _) = select(
            k.core.store(),
            k.rt.as_ref(),
            DispatchRequest {
                workspace: WS,
                bot: None,
                harness: None,
                model: None,
                effort: None,
                team: Some(&team_a_first),
                project: &Some(None),
                title: "Task",
                goal: "Unmatched task",
                tags: &[],
                required_specialties: &[],
                node: None,
                required_nodes: None,
            },
            |_| None,
        )
        .unwrap();
        assert_eq!(placed_a.explanation.bot.as_str(), "bot_aaaa");
    }

    #[test]
    fn zero_signal_without_team_prefers_editable_earlier_joined_and_ascending_id() {
        let k = Kit::new();
        let db = k.core.store().store();
        db.bots()
            .patch(
                "dev",
                &BotPatch {
                    enabled: Some(false),
                    ..Default::default()
                },
                "owner",
            )
            .unwrap();
        db.bots()
            .seed(
                "bot_plan",
                false,
                &NewBot {
                    name: "Plan".into(),
                    harness: "fixture".into(),
                    perm_mode: Some("plan".into()),
                    ..Default::default()
                },
                WS,
            )
            .unwrap();
        db.bots()
            .seed(
                "bot_late",
                false,
                &NewBot {
                    name: "Late".into(),
                    harness: "fixture".into(),
                    perm_mode: Some("auto".into()),
                    ..Default::default()
                },
                WS,
            )
            .unwrap();
        db.bots()
            .seed(
                "bot_early",
                false,
                &NewBot {
                    name: "Early".into(),
                    harness: "fixture".into(),
                    perm_mode: Some("auto".into()),
                    ..Default::default()
                },
                WS,
            )
            .unwrap();
        for (bot, ts) in [("bot_plan", 100), ("bot_early", 200), ("bot_late", 300)] {
            steve_store::core_fixture::bot_workspace_created(db, WS, bot, ts);
        }

        let pick = || {
            select(
                k.core.store(),
                k.rt.as_ref(),
                DispatchRequest {
                    workspace: WS,
                    bot: None,
                    harness: None,
                    model: None,
                    effort: None,
                    team: None,
                    project: &Some(None),
                    title: "Generic",
                    goal: "Generic work",
                    tags: &[],
                    required_specialties: &[],
                    node: None,
                    required_nodes: None,
                },
                |_| None,
            )
            .unwrap()
            .0
            .explanation
            .bot
        };
        assert_eq!(pick().as_str(), "bot_early");

        // Same join time breaks ties by ascending bot.id ("bot_a_id" < "bot_late").
        db.bots()
            .seed(
                "bot_a_id",
                false,
                &NewBot {
                    name: "Alpha".into(),
                    harness: "fixture".into(),
                    perm_mode: Some("auto".into()),
                    ..Default::default()
                },
                WS,
            )
            .unwrap();
        for (bot, ts) in [("bot_a_id", 300), ("bot_early", 500)] {
            steve_store::core_fixture::bot_workspace_created(db, WS, bot, ts);
        }
        assert_eq!(pick().as_str(), "bot_a_id");

        // When only a read-only (`plan`) Bot remains, it is still selected rather than dropped.
        for id in ["bot_a_id", "bot_early", "bot_late"] {
            db.bots()
                .patch(
                    id,
                    &BotPatch {
                        enabled: Some(false),
                        ..Default::default()
                    },
                    "owner",
                )
                .unwrap();
        }
        assert_eq!(pick().as_str(), "bot_plan");
    }

    #[test]
    fn project_default_bot_outranks_bot_default_project() {
        let k = Kit::new();
        let db = k.core.store().store();
        db.bots()
            .patch(
                "dev",
                &BotPatch {
                    enabled: Some(false),
                    ..Default::default()
                },
                "owner",
            )
            .unwrap();
        let p = k
            .db()
            .project_create(WS, "App", "Main app", "/tmp/app", "dir", false, None, None)
            .unwrap();
        db.bots()
            .seed(
                "bot_proj_member",
                false,
                &NewBot {
                    name: "Member".into(),
                    harness: "fixture".into(),
                    default_project: Some(p.id.clone()),
                    ..Default::default()
                },
                WS,
            )
            .unwrap();
        db.bots()
            .seed(
                "bot_proj_default",
                false,
                &NewBot {
                    name: "Default".into(),
                    harness: "fixture".into(),
                    ..Default::default()
                },
                WS,
            )
            .unwrap();
        k.db()
            .project_patch(
                &p.id,
                &crate::patch! { default_bot: "bot_proj_default" },
                "owner",
            )
            .unwrap();

        let (placed, _) = select(
            k.core.store(),
            k.rt.as_ref(),
            DispatchRequest {
                workspace: WS,
                bot: None,
                harness: None,
                model: None,
                effort: None,
                team: None,
                project: &Some(Some(p.id)),
                title: "Do work",
                goal: "Handle task",
                tags: &[],
                required_specialties: &[],
                node: None,
                required_nodes: None,
            },
            |_| None,
        )
        .unwrap();
        assert_eq!(placed.explanation.bot.as_str(), "bot_proj_default");
        assert_eq!(
            placed.explanation.reason,
            PlacementReason::ProjectPreference
        );
    }

    #[test]
    fn description_matches_clauses_tags_name_and_title_while_ignoring_single_char_noise() {
        let k = Kit::new();
        let db = k.core.store().store();
        db.bots()
            .patch(
                "dev",
                &BotPatch {
                    enabled: Some(false),
                    ..Default::default()
                },
                "owner",
            )
            .unwrap();
        db.bots()
            .seed(
                "bot_noise",
                false,
                &NewBot {
                    name: "杂项".into(),
                    harness: "fixture".into(),
                    role: Some("写、修、改".into()),
                    ..Default::default()
                },
                WS,
            )
            .unwrap();
        db.bots()
            .seed(
                "bot_doc",
                false,
                &NewBot {
                    name: "文档".into(),
                    harness: "fixture".into(),
                    role: Some("写和整理文档、学习资料、调研笔记".into()),
                    ..Default::default()
                },
                WS,
            )
            .unwrap();
        db.bots()
            .seed(
                "bot_fe",
                false,
                &NewBot {
                    name: "前端".into(),
                    harness: "fixture".into(),
                    role: Some("界面交互 / React 组件：样式与无障碍".into()),
                    ..Default::default()
                },
                WS,
            )
            .unwrap();

        // Single-character clauses like "写" or "修" do not match `bot_noise`, whereas `req.tags`
        // matching `bot.name` ("文档") and role clause ("调研笔记") in title/goal match `bot_doc`.
        let (doc_pick, _) = select(
            k.core.store(),
            k.rt.as_ref(),
            DispatchRequest {
                workspace: WS,
                bot: None,
                harness: None,
                model: None,
                effort: None,
                team: None,
                project: &Some(None),
                title: "整理调研笔记",
                goal: "写一份总结",
                tags: &["文档".into()],
                required_specialties: &[],
                node: None,
                required_nodes: None,
            },
            |_| None,
        )
        .unwrap();
        assert_eq!(doc_pick.explanation.bot.as_str(), "bot_doc");
        assert_eq!(doc_pick.explanation.reason, PlacementReason::Description);
        assert!(doc_pick.explanation.matched.contains(&"文档".into()));
        assert!(doc_pick.explanation.matched.contains(&"调研笔记".into()));

        // Role separators (`/` and `：`) split clauses like "React 组件".
        let (fe_pick, _) = select(
            k.core.store(),
            k.rt.as_ref(),
            DispatchRequest {
                workspace: WS,
                bot: None,
                harness: None,
                model: None,
                effort: None,
                team: None,
                project: &Some(None),
                title: "优化 React 组件",
                goal: "修一下按钮焦点",
                tags: &[],
                required_specialties: &[],
                node: None,
                required_nodes: None,
            },
            |_| None,
        )
        .unwrap();
        assert_eq!(fe_pick.explanation.bot.as_str(), "bot_fe");
        assert_eq!(fe_pick.explanation.reason, PlacementReason::Description);
        assert_eq!(fe_pick.explanation.matched, ["React 组件"]);
    }

    #[test]
    fn specialty_precision_prefers_focused_bot_over_broad_bot() {
        let k = Kit::new();
        let db = k.core.store().store();
        db.bots()
            .patch(
                "dev",
                &BotPatch {
                    enabled: Some(false),
                    ..Default::default()
                },
                "owner",
            )
            .unwrap();
        db.bots()
            .seed(
                "bot_broad",
                false,
                &NewBot {
                    name: "Broad".into(),
                    harness: "fixture".into(),
                    specialties: Some(vec!["rust".into(), "sql".into(), "css".into()]),
                    ..Default::default()
                },
                WS,
            )
            .unwrap();
        db.bots()
            .seed(
                "bot_focused",
                false,
                &NewBot {
                    name: "Focused".into(),
                    harness: "fixture".into(),
                    specialties: Some(vec!["rust".into()]),
                    ..Default::default()
                },
                WS,
            )
            .unwrap();

        let (placed, _) = select(
            k.core.store(),
            k.rt.as_ref(),
            DispatchRequest {
                workspace: WS,
                bot: None,
                harness: None,
                model: None,
                effort: None,
                team: None,
                project: &Some(None),
                title: "Fix Rust lifetime",
                goal: "Update borrow handling",
                tags: &[],
                required_specialties: &[],
                node: None,
                required_nodes: None,
            },
            |_| None,
        )
        .unwrap();
        assert_eq!(placed.explanation.bot.as_str(), "bot_focused");
        assert_eq!(placed.explanation.reason, PlacementReason::Specialty);
    }
}
