//! Deterministic turn context monitoring and declarative native-compaction launch wiring.
//!
//! No model calls and no harness-name branching: behavior follows the adapter's resolved
//! [`steve_harness_spec::AdapterProperties`] and the Hub's [`ContextBudget`].

use std::ffi::OsString;

use steve_harness_spec::AdapterProperties;
use steve_node_api::{
    CompactionMode, CompactionReason, ContextBudget, EventBody, ToolCategory, ToolPhase, ToolView,
};

/// Denominator for integer percentage thresholds in [`ContextMonitor`].
const PERCENT_DENOMINATOR: u64 = 100;

/// ACP `ToolCategory::Think` tool name emitted by harnesses that expose native compaction as an
/// explicit tool call (observed on Claude Code in `CONTEXT-BUDGET.md` §1.3).
const EXPLICIT_COMPACT_TOOL_NAME: &str = "compact";

/// Upper bound (% of `before`) for tolerating one trailing pre-compaction `usage_update` arriving
/// immediately after an explicit `compact` tool completes.
///
/// Protocol & empirical basis (`CONTEXT-BUDGET.md` §1.3): in ACP, `tool_call_update(completed)`
/// for `compact` can precede one trailing `usage_update` from the pre-compaction prompt step
/// (observed climbing slightly at trigger time, e.g. 199k -> 201k before dropping to 10k–18k).
/// Keeping the pending explicit drop alive up to 125% covers that single trailing reading while
/// dropping it if context continues climbing far past the pre-compact watermark.
const EXPLICIT_COMPACT_STALE_CEILING_PCT: u64 = 125;

/// Sustained confirmation ceiling (% of `before`) for an implicit native compaction drop.
///
/// Empirical basis (`CONTEXT-BUDGET.md` §1.3): 82 production turns exhibited alternating
/// dual-counter `usage_update` oscillation (e.g. 689k -> 148k -> 691k, bouncing back to ~100% of
/// `before`), whereas real implicit native compactions (Codex 247k–250k -> 19k–22k and
/// 789k–792k -> 28k–33k) stay near 4%–10% of `before` on the subsequent update. Requiring the
/// follow-up reading to stay strictly below 75% rejects oscillation bounces while leaving headroom
/// for new tool output added in the first post-compaction step.
const IMPLICIT_DROP_CONFIRM_CEILING_PCT: u64 = 75;

/// Maximum retained context (% of `prev`) to open an implicit native compaction candidate.
///
/// Empirical basis (`CONTEXT-BUDGET.md` §1.3 & §2.3): real native compactions observed in
/// production cut context by >85% (Claude Code 165k–200k -> 10k–18k; Codex 247k–250k -> 19k–22k
/// and 789k–792k -> 28k–33k). Requiring `used <= 50%` of `prev` filters routine prompt/cache
/// fluctuations without missing real compactions.
const IMPLICIT_DROP_MAX_RETAINED_PCT: u64 = 50;

/// Minimum absolute token drop (% of the active declared/configured budget scale, falling back to
/// `effective_context_limit`) required to open an implicit compaction candidate.
///
/// Derived from declared/configured context scale so 1M-window models constrained to a smaller
/// turn budget (e.g. 160k) still detect self-compaction relative to their active budget, while
/// ignoring tiny early-turn fluctuations well below the 22.5k–30.4k first-turn P50 baseline in
/// `CONTEXT-BUDGET.md` §1.4.
const IMPLICIT_DROP_MIN_WINDOW_PCT: u64 = 10;

/// Fallback minimum absolute token drop when neither the adapter declaration nor `usage_update`
/// provides a context limit or budget.
///
/// Empirical basis (`CONTEXT-BUDGET.md` §1.4): real turns start at P50 22.5k–30.4k tokens and
/// compact down to 10k–33k; drops smaller than 1,000 tokens without a known window are noise.
const IMPLICIT_DROP_MIN_TOKENS_FALLBACK: u64 = 1_000;

/// Result of feeding one `usage_update` context snapshot into [`ContextMonitor`].
#[derive(Debug, Default, PartialEq)]
pub(crate) struct ContextObservation {
    /// Emitted when native compaction is confirmed or when a budget-enforced harness hits its
    /// turn context budget.
    pub compacted: Option<EventBody>,
    /// True once when [`AdapterProperties::enforces_turn_context_budget`] holds and
    /// `context_used >= threshold` without native compaction intervening.
    pub stop_budget: bool,
}

#[derive(Debug, Clone, Copy)]
struct PendingDrop {
    before: u64,
    after: Option<u64>,
    limit: Option<u64>,
    explicit: bool,
    stale_seen: bool,
}

/// Per-attempt context tracker on the Node.
pub(crate) struct ContextMonitor {
    props: AdapterProperties,
    budget: Option<ContextBudget>,
    peak_used: Option<u64>,
    last_used: Option<u64>,
    last_limit: Option<u64>,
    pending_drop: Option<PendingDrop>,
    budget_triggered: bool,
}

impl ContextMonitor {
    pub(crate) fn new(props: AdapterProperties, budget: Option<ContextBudget>) -> Self {
        Self {
            props,
            budget,
            peak_used: None,
            last_used: None,
            last_limit: None,
            pending_drop: None,
            budget_triggered: false,
        }
    }

    pub(crate) fn peak_used(&self) -> Option<u64> {
        self.peak_used.filter(|&n| n > 0)
    }

    /// Observe a tool state transition. Harnesses that expose native compaction as a `compact`
    /// think tool emit this before the post-compaction `usage_update` arrives.
    pub(crate) fn on_tool(&mut self, tool: &ToolView) {
        if tool.category == ToolCategory::Think
            && tool.name.eq_ignore_ascii_case(EXPLICIT_COMPACT_TOOL_NAME)
            && tool.phase == ToolPhase::Succeeded
            && let Some(before) = self.last_used.or(self.peak_used).filter(|&n| n > 0)
        {
            self.pending_drop = Some(PendingDrop {
                before,
                after: None,
                limit: self.props.effective_context_limit(self.last_limit),
                explicit: true,
                stale_seen: false,
            });
        }
    }

    /// Observe main-loop `context_used` and `context_limit` from a `usage_update`.
    pub(crate) fn observe_usage(
        &mut self,
        used: Option<u64>,
        limit: Option<u64>,
    ) -> ContextObservation {
        let mut out = ContextObservation::default();
        if let Some(l) = limit.filter(|&n| n > 0) {
            self.last_limit = Some(l);
        }
        let Some(used) = used.filter(|&n| n > 0) else {
            return out;
        };
        let effective_limit = self.props.effective_context_limit(self.last_limit);
        let budget_threshold = self
            .props
            .turn_context_budget_tokens(self.budget, self.last_limit);
        let prev = self.last_used;
        self.last_used = Some(used);
        self.peak_used = Some(self.peak_used.map_or(used, |p| p.max(used)));

        if let Some(mut p) = self.pending_drop.take() {
            if p.explicit {
                let stale_ceiling = p.before.saturating_mul(EXPLICIT_COMPACT_STALE_CEILING_PCT)
                    / PERCENT_DENOMINATOR;
                if used < p.before {
                    out.compacted = Some(EventBody::ContextCompacted {
                        mode: CompactionMode::Native,
                        reason: CompactionReason::NativeCompaction,
                        before_tokens: p.before,
                        after_tokens: used,
                        limit_tokens: effective_limit.or(p.limit),
                    });
                } else if !p.stale_seen && used <= stale_ceiling {
                    // One trailing pre-compaction usage update arrived right after tool
                    // completion; wait for the post-compaction reading once.
                    p.stale_seen = true;
                    self.pending_drop = Some(p);
                }
            } else {
                let confirm_ceiling = p.before.saturating_mul(IMPLICIT_DROP_CONFIRM_CEILING_PCT)
                    / PERCENT_DENOMINATOR;
                if used < confirm_ceiling {
                    // Drop held across a subsequent observation: confirmed native compaction.
                    let after = p.after.unwrap_or(used).min(used);
                    out.compacted = Some(EventBody::ContextCompacted {
                        mode: CompactionMode::Native,
                        reason: CompactionReason::NativeCompaction,
                        before_tokens: p.before,
                        after_tokens: after,
                        limit_tokens: effective_limit.or(p.limit),
                    });
                }
                // Otherwise the reading bounced back to the high mark (dual-counter oscillation): ignore.
            }
        } else if self.props.has_native_compaction()
            && let Some(prev) = prev
        {
            let min_drop = budget_threshold.or(effective_limit).map_or(
                IMPLICIT_DROP_MIN_TOKENS_FALLBACK,
                |scale| {
                    (scale.saturating_mul(IMPLICIT_DROP_MIN_WINDOW_PCT) / PERCENT_DENOMINATOR)
                        .max(1)
                },
            );
            let max_retained =
                prev.saturating_mul(IMPLICIT_DROP_MAX_RETAINED_PCT) / PERCENT_DENOMINATOR;
            if used <= max_retained && prev.saturating_sub(used) >= min_drop {
                self.pending_drop = Some(PendingDrop {
                    before: prev,
                    after: Some(used),
                    limit: effective_limit,
                    explicit: false,
                    stale_seen: false,
                });
            }
        }

        let awaiting_explicit_compact = self.pending_drop.is_some_and(|p| p.explicit);
        if self.props.enforces_turn_context_budget()
            && !self.budget_triggered
            && !awaiting_explicit_compact
            && out.compacted.is_none()
            && let Some(threshold) = budget_threshold
            && used >= threshold
        {
            self.budget_triggered = true;
            out.compacted = Some(EventBody::ContextCompacted {
                mode: CompactionMode::Handoff,
                reason: CompactionReason::BudgetReached,
                before_tokens: used,
                after_tokens: 0,
                limit_tokens: effective_limit,
            });
            out.stop_budget = true;
        }

        out
    }

    /// Flush any unbounced implicit native compaction drop at attempt end.
    pub(crate) fn finish(&mut self) -> Option<EventBody> {
        let p = self.pending_drop.take()?;
        if !p.explicit
            && let Some(after) = p.after
            && after < p.before
        {
            Some(EventBody::ContextCompacted {
                mode: CompactionMode::Native,
                reason: CompactionReason::NativeCompaction,
                before_tokens: p.before,
                after_tokens: after,
                limit_tokens: p.limit,
            })
        } else {
            None
        }
    }
}

/// Append declarative native-compaction launch args without overriding explicit args.
pub(crate) fn apply_native_compaction_args(
    props: &AdapterProperties,
    budget: Option<ContextBudget>,
    args: &mut Vec<OsString>,
) {
    let launch = props.native_compaction_launch(budget);
    if !launch.args.is_empty() && !args_already_override(args, &launch.args) {
        args.extend(launch.args.into_iter().map(OsString::from));
    }
}

/// Append declarative native-compaction launch env without overriding explicit env vars.
pub(crate) fn apply_native_compaction_env(
    props: &AdapterProperties,
    budget: Option<ContextBudget>,
    spawn_env: &mut Vec<(OsString, OsString)>,
) {
    let launch = props.native_compaction_launch(budget);
    for (k, v) in launch.env {
        if !spawn_env.iter().any(|(ek, _)| ek == k.as_str()) {
            spawn_env.push((k.into(), v.into()));
        }
    }
}

fn args_already_override(existing: &[OsString], candidate: &[String]) -> bool {
    for arg in candidate {
        if let Some((key, _)) = arg.split_once('=') {
            let prefix = format!("{key}=");
            if existing
                .iter()
                .filter_map(|a| a.to_str())
                .any(|a| a.starts_with(&prefix))
            {
                return true;
            }
        }
    }
    if candidate.len() == 1 {
        return existing.iter().any(|a| a == candidate[0].as_str());
    }
    existing
        .windows(candidate.len())
        .any(|w| w.iter().zip(candidate.iter()).all(|(a, b)| a == b.as_str()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use steve_harness_spec::NativeCompaction;
    use steve_node_api::{CallId, ToolOrigin};

    fn configurable_native_props() -> AdapterProperties {
        AdapterProperties {
            context_limit: Some(100_000),
            context_budget_pct: Some(80),
            native_compaction: Some(NativeCompaction::EnvPercent {
                var: "COMPACT_PCT".into(),
                window_var: Some("COMPACT_WINDOW".into()),
            }),
            ..Default::default()
        }
    }

    fn builtin_native_props() -> AdapterProperties {
        AdapterProperties {
            context_limit: Some(200_000),
            context_budget_pct: Some(80),
            native_compaction: Some(NativeCompaction::Builtin),
            ..Default::default()
        }
    }

    fn non_native_props() -> AdapterProperties {
        AdapterProperties {
            context_limit: Some(100_000),
            context_budget_pct: Some(80),
            native_compaction: None,
            ..Default::default()
        }
    }

    fn compact_tool() -> ToolView {
        ToolView {
            id: CallId::new("c1").unwrap(),
            parent: None,
            origin: ToolOrigin::Harness,
            name: "compact".into(),
            category: ToolCategory::Think,
            title: "compact".into(),
            description: None,
            target: None,
            phase: ToolPhase::Succeeded,
            action: None,
            evidence: None,
            result: vec![],
            error: None,
            duration_ms: None,
        }
    }

    #[test]
    fn non_native_harness_stops_deterministically_at_budget_and_configurable_native_does_not() {
        let mut non_native = ContextMonitor::new(non_native_props(), None);
        let below = non_native.observe_usage(Some(79_999), Some(100_000));
        assert!(!below.stop_budget && below.compacted.is_none());
        let hit = non_native.observe_usage(Some(80_000), Some(100_000));
        assert!(hit.stop_budget);
        assert_eq!(
            hit.compacted,
            Some(EventBody::ContextCompacted {
                mode: CompactionMode::Handoff,
                reason: CompactionReason::BudgetReached,
                before_tokens: 80_000,
                after_tokens: 0,
                limit_tokens: Some(100_000),
            })
        );
        // Trigger fires once per attempt.
        let again = non_native.observe_usage(Some(90_000), Some(100_000));
        assert!(!again.stop_budget && again.compacted.is_none());
        assert_eq!(non_native.peak_used(), Some(90_000));

        // Configurable native compaction (env_percent/env_ratio/env_tokens/cli_args) delegates to
        // the harness and does not stop on budget.
        for compaction in [
            NativeCompaction::EnvPercent {
                var: "PCT".into(),
                window_var: Some("WIN".into()),
            },
            NativeCompaction::EnvRatio {
                var: "RATIO".into(),
            },
            NativeCompaction::EnvTokens {
                var: "TOKENS".into(),
            },
            NativeCompaction::CliArgs {
                args: vec!["--limit={tokens}".into()],
            },
        ] {
            let props = AdapterProperties {
                context_limit: Some(100_000),
                context_budget_pct: Some(80),
                native_compaction: Some(compaction),
                ..Default::default()
            };
            let mut native = ContextMonitor::new(props, None);
            let high = native.observe_usage(Some(95_000), Some(100_000));
            assert!(!high.stop_budget && high.compacted.is_none());
        }

        // 1M-window model on a non-native harness is constrained to declared context_limit * pct
        // (100k * 80% = 80k) by default, and can be raised explicitly via budget.max_tokens.
        let mut one_m_default = ContextMonitor::new(non_native_props(), None);
        assert!(
            !one_m_default
                .observe_usage(Some(79_999), Some(1_000_000))
                .stop_budget
        );
        let one_m_hit = one_m_default.observe_usage(Some(80_000), Some(1_000_000));
        assert!(one_m_hit.stop_budget);

        let mut one_m_raised = ContextMonitor::new(
            non_native_props(),
            Some(ContextBudget {
                ratio_pct: None,
                max_tokens: Some(400_000),
            }),
        );
        assert!(
            !one_m_raised
                .observe_usage(Some(250_000), Some(1_000_000))
                .stop_budget
        );
        assert!(
            one_m_raised
                .observe_usage(Some(400_000), Some(1_000_000))
                .stop_budget
        );
    }

    #[test]
    fn builtin_native_1m_window_enforces_budget_unless_harness_self_compacts() {
        // 1. Builtin native compaction on a 1M window does not accept a launch threshold: when it
        //    climbs past the resolved default budget (200k * 80% = 160k) without compacting, Steve
        //    stops the turn for ContextBudget handoff.
        let mut uncompacted = ContextMonitor::new(builtin_native_props(), None);
        assert!(
            !uncompacted
                .observe_usage(Some(159_999), Some(1_000_000))
                .stop_budget
        );
        let hit = uncompacted.observe_usage(Some(160_000), Some(1_000_000));
        assert!(hit.stop_budget);
        assert_eq!(
            hit.compacted,
            Some(EventBody::ContextCompacted {
                mode: CompactionMode::Handoff,
                reason: CompactionReason::BudgetReached,
                before_tokens: 160_000,
                after_tokens: 0,
                limit_tokens: Some(1_000_000),
            })
        );

        // 2. When a builtin 1M harness self-compacts implicitly before hitting budget (including a
        //    mid-turn drop <100k that still exceeds 10% of the active 160k budget), Steve records
        //    native compaction and does not interrupt the turn.
        let mut self_compacted = ContextMonitor::new(builtin_native_props(), None);
        assert!(
            !self_compacted
                .observe_usage(Some(100_000), Some(1_000_000))
                .stop_budget
        );
        let dropped = self_compacted.observe_usage(Some(15_000), Some(1_000_000));
        assert!(!dropped.stop_budget && dropped.compacted.is_none());
        let confirmed = self_compacted.observe_usage(Some(20_000), Some(1_000_000));
        assert!(!confirmed.stop_budget);
        assert_eq!(
            confirmed.compacted,
            Some(EventBody::ContextCompacted {
                mode: CompactionMode::Native,
                reason: CompactionReason::NativeCompaction,
                before_tokens: 100_000,
                after_tokens: 15_000,
                limit_tokens: Some(1_000_000),
            })
        );

        // 3. Explicit `compact` tool with one trailing pre-compaction reading slightly above
        //    budget waits for the post-compaction drop instead of falsely cancelling.
        let mut explicit = ContextMonitor::new(builtin_native_props(), None);
        assert!(
            !explicit
                .observe_usage(Some(155_000), Some(1_000_000))
                .stop_budget
        );
        explicit.on_tool(&compact_tool());
        let stale = explicit.observe_usage(Some(162_000), Some(1_000_000));
        assert!(!stale.stop_budget && stale.compacted.is_none());
        let post = explicit.observe_usage(Some(18_000), Some(1_000_000));
        assert!(!post.stop_budget);
        assert_eq!(
            post.compacted,
            Some(EventBody::ContextCompacted {
                mode: CompactionMode::Native,
                reason: CompactionReason::NativeCompaction,
                before_tokens: 155_000,
                after_tokens: 18_000,
                limit_tokens: Some(1_000_000),
            })
        );

        // 4. If an explicit `compact` tool fails to actually shrink context on the follow-up
        //    reading, the budget backstop still fires rather than waiting forever.
        let mut noop_compact = ContextMonitor::new(builtin_native_props(), None);
        noop_compact.observe_usage(Some(155_000), Some(1_000_000));
        noop_compact.on_tool(&compact_tool());
        assert!(
            !noop_compact
                .observe_usage(Some(162_000), Some(1_000_000))
                .stop_budget
        );
        let still_high = noop_compact.observe_usage(Some(165_000), Some(1_000_000));
        assert!(still_high.stop_budget);

        // 5. Explicit workspace max_tokens raises the builtin 1M threshold up to reported_limit.
        let mut raised = ContextMonitor::new(
            builtin_native_props(),
            Some(ContextBudget {
                ratio_pct: None,
                max_tokens: Some(400_000),
            }),
        );
        assert!(
            !raised
                .observe_usage(Some(250_000), Some(1_000_000))
                .stop_budget
        );
        assert!(
            raised
                .observe_usage(Some(400_000), Some(1_000_000))
                .stop_budget
        );
    }

    #[test]
    fn native_compaction_confirms_sustained_drop_and_ignores_oscillation() {
        // Dual-counter oscillation (689k -> 148k -> 691k) is ignored.
        let mut osc = ContextMonitor::new(configurable_native_props(), None);
        assert!(
            osc.observe_usage(Some(689_000), Some(200_000))
                .compacted
                .is_none()
        );
        assert!(
            osc.observe_usage(Some(148_000), Some(200_000))
                .compacted
                .is_none()
        );
        assert!(
            osc.observe_usage(Some(691_000), Some(200_000))
                .compacted
                .is_none()
        );
        assert!(osc.finish().is_none());

        // Real sustained drop (249k -> 21k -> 24k) is confirmed.
        let mut codex = ContextMonitor::new(configurable_native_props(), None);
        assert!(
            codex
                .observe_usage(Some(249_000), Some(258_400))
                .compacted
                .is_none()
        );
        assert!(
            codex
                .observe_usage(Some(21_000), Some(258_400))
                .compacted
                .is_none()
        );
        let confirmed = codex.observe_usage(Some(24_000), Some(258_400));
        assert_eq!(
            confirmed.compacted,
            Some(EventBody::ContextCompacted {
                mode: CompactionMode::Native,
                reason: CompactionReason::NativeCompaction,
                before_tokens: 249_000,
                after_tokens: 21_000,
                limit_tokens: Some(258_400),
            })
        );

        // Explicit `compact` tool confirms on the very next drop.
        let mut claude = ContextMonitor::new(configurable_native_props(), None);
        claude.observe_usage(Some(195_000), Some(200_000));
        claude.on_tool(&compact_tool());
        let dropped = claude.observe_usage(Some(14_000), Some(200_000));
        assert_eq!(
            dropped.compacted,
            Some(EventBody::ContextCompacted {
                mode: CompactionMode::Native,
                reason: CompactionReason::NativeCompaction,
                before_tokens: 195_000,
                after_tokens: 14_000,
                limit_tokens: Some(200_000),
            })
        );
    }

    #[test]
    fn launch_wiring_respects_explicit_overrides() {
        let props = AdapterProperties {
            context_limit: Some(258_400),
            context_budget_pct: Some(80),
            native_compaction: Some(NativeCompaction::CliArgs {
                args: vec![
                    "-c".into(),
                    "model_auto_compact_token_limit={tokens}".into(),
                ],
            }),
            ..Default::default()
        };
        let mut args = vec![
            OsString::from("-c"),
            OsString::from("model_auto_compact_token_limit=120000"),
        ];
        let mut env = vec![];
        apply_native_compaction_args(&props, None, &mut args);
        apply_native_compaction_env(&props, None, &mut env);
        assert_eq!(
            args.len(),
            2,
            "explicit CLI override must not be duplicated"
        );

        let mut fresh_args = vec![];
        apply_native_compaction_args(&props, None, &mut fresh_args);
        assert_eq!(
            fresh_args,
            vec![
                OsString::from("-c"),
                OsString::from("model_auto_compact_token_limit=206720")
            ]
        );
    }
}
