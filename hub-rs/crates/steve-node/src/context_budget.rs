//! Deterministic turn context monitoring and declarative native-compaction launch wiring.
//!
//! No model calls and no harness-name branching: behavior follows the adapter's resolved
//! [`steve_harness_spec::AdapterProperties`] and the Hub's [`ContextBudget`].

use std::ffi::OsString;

use steve_harness_spec::AdapterProperties;
use steve_node_api::{
    CompactionMode, CompactionReason, ContextBudget, EventBody, ToolCategory, ToolPhase, ToolView,
};

/// Result of feeding one `usage_update` context snapshot into [`ContextMonitor`].
#[derive(Debug, Default, PartialEq)]
pub(crate) struct ContextObservation {
    /// Emitted when native compaction is confirmed or when a non-native harness hits its budget.
    pub compacted: Option<EventBody>,
    /// True once on a harness without declared native compaction when `context_used >= threshold`.
    pub stop_budget: bool,
}

#[derive(Debug, Clone, Copy)]
struct PendingDrop {
    before: u64,
    after: Option<u64>,
    limit: Option<u64>,
    explicit: bool,
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
            && tool.name.eq_ignore_ascii_case("compact")
            && tool.phase == ToolPhase::Succeeded
            && let Some(before) = self.last_used.or(self.peak_used).filter(|&n| n > 0)
        {
            self.pending_drop = Some(PendingDrop {
                before,
                after: None,
                limit: self.props.effective_context_limit(self.last_limit),
                explicit: true,
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
        let prev = self.last_used;
        self.last_used = Some(used);
        self.peak_used = Some(self.peak_used.map_or(used, |p| p.max(used)));

        if let Some(p) = self.pending_drop.take() {
            if p.explicit {
                if used < p.before {
                    out.compacted = Some(EventBody::ContextCompacted {
                        mode: CompactionMode::Native,
                        reason: CompactionReason::NativeCompaction,
                        before_tokens: p.before,
                        after_tokens: used,
                        limit_tokens: effective_limit.or(p.limit),
                    });
                } else if used <= p.before.saturating_mul(5) / 4 {
                    // Pre-compaction usage update arrived right after tool completion; keep waiting.
                    self.pending_drop = Some(p);
                }
            } else if used < p.before.saturating_mul(3) / 4 {
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
        } else if self.props.has_native_compaction()
            && let Some(prev) = prev
        {
            let min_drop = effective_limit.map_or(1_000, |l| (l / 10).max(1));
            if used <= prev / 2 && prev.saturating_sub(used) >= min_drop {
                self.pending_drop = Some(PendingDrop {
                    before: prev,
                    after: Some(used),
                    limit: effective_limit,
                    explicit: false,
                });
            }
        }

        if !self.props.has_native_compaction()
            && !self.budget_triggered
            && let Some(threshold) = self
                .props
                .turn_context_budget_tokens(self.budget, effective_limit)
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

    fn native_props() -> AdapterProperties {
        AdapterProperties {
            context_limit: Some(100_000),
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

    #[test]
    fn non_native_harness_stops_deterministically_at_budget_and_native_does_not() {
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

        let mut native = ContextMonitor::new(native_props(), None);
        let high = native.observe_usage(Some(95_000), Some(100_000));
        assert!(!high.stop_budget && high.compacted.is_none());
    }

    #[test]
    fn native_compaction_confirms_sustained_drop_and_ignores_oscillation() {
        // Dual-counter oscillation (689k -> 148k -> 691k) is ignored.
        let mut osc = ContextMonitor::new(native_props(), None);
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
        let mut codex = ContextMonitor::new(native_props(), None);
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
        let mut claude = ContextMonitor::new(native_props(), None);
        claude.observe_usage(Some(195_000), Some(200_000));
        claude.on_tool(&ToolView {
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
        });
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
